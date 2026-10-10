use super::*;
use std::os::unix::fs::PermissionsExt;
struct Fixture {
    root: PathBuf,
    server: IpcServer,
    runtime: Runtime,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("frameeyeosc-ipc-{}", unique_id().unwrap()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let (store, settings) =
            SettingsStore::load(root.join("settings.json"), &Map::new(), false).unwrap();
        let runtime =
            Runtime::with_paths(settings, &root.join("missing-input"), &root.join("proc")).unwrap();
        let server = IpcServer::bind(root.join("control.sock"), store).unwrap();
        Self {
            root,
            server,
            runtime,
        }
    }
    fn client(&self) -> UnixStream {
        let c = UnixStream::connect(self.root.join("control.sock")).unwrap();
        c.set_nonblocking(true).unwrap();
        c
    }
    fn pump(&mut self) {
        for _ in 0..5 {
            self.server.poll(&mut self.runtime).unwrap();
        }
    }
    fn request(&mut self, c: &mut UnixStream, request: Value) -> Vec<Value> {
        c.write_all(format!("{request}\n").as_bytes()).unwrap();
        self.pump();
        read(c)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn read(c: &mut UnixStream) -> Vec<Value> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        match c.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&buffer[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("{e}"),
        }
    }
    bytes
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
        .map(|b| serde_json::from_slice(b).unwrap())
        .collect()
}
fn request(id: &str, method: &str, params: Value) -> Value {
    json!({"v":1,"type":"request","id":id,"method":method,"params":params})
}
#[test]
fn strict_recursive_json_rejects_duplicates_and_nonfinite() {
    for line in [
        r#"{"x":1,"x":2}"#,
        r#"{"a":[{"x":1,"\u0078":2}]}"#,
        r#"{"x":1e999}"#,
        r#"{"x":NaN}"#,
        r#"{} {}"#,
    ] {
        assert!(parse_json(line.as_bytes()).is_err(), "{line}");
    }
}
#[test]
fn snapshot_subscription_origin_and_retry() {
    let mut f = Fixture::new();
    let mut a = f.client();
    let mut b = f.client();
    let first = f.request(&mut a, request("s", "state.get", json!({})));
    let a_id = first[0]["result"]["connection_id"].clone();
    let patch = request(
        "p",
        "settings.patch",
        json!({"patch":{"eyelid_output_cap":0.7},"save":false}),
    );
    let applied = f.request(&mut a, patch.clone());
    assert_eq!(applied.len(), 2);
    assert_eq!(applied[0]["result"]["save"], "not_requested");
    assert_eq!(
        applied[1]["caused_by"],
        json!({"connection_id":a_id,"request_id":"p"})
    );
    assert!(read(&mut b).is_empty());
    assert_eq!(f.request(&mut a, patch), vec![applied[0].clone()]);
    let second = f.request(&mut b, request("s", "state.get", json!({})));
    let b_id = second[0]["result"]["connection_id"].clone();
    assert_ne!(a_id, b_id);
    let changed = f.request(
        &mut a,
        request(
            "p2",
            "settings.patch",
            json!({"patch":{"prefix":"/New"},"save":true}),
        ),
    );
    assert!(changed[0]["ok"].as_bool().unwrap());
    let event = read(&mut b);
    assert_eq!(event[0]["data"]["connection_id"], b_id);
    assert_eq!(event[0]["caused_by"]["connection_id"], a_id);
    assert_eq!(f.server.store.saved["prefix"], "/New");
    assert!(!f.server.store.saved.contains_key("eyelid_output_cap"));
}
#[test]
fn errors_rollback_and_cache_conflict() {
    let mut f = Fixture::new();
    let mut c = f.client();
    let r = f.request(
        &mut c,
        request(
            "p",
            "settings.patch",
            json!({"patch":{"shm_version":"5","prefix":"/Wrong"},"save":true}),
        ),
    );
    assert_eq!(r[0]["ok"], false);
    assert_eq!(r[0]["result"]["current"]["prefix"], "/FT");
    assert_eq!(r[0]["result"]["save"], "not_requested");
    assert!(f.server.store.saved.is_empty());
    let conflict = f.request(&mut c, request("p", "state.get", json!({})));
    assert_eq!(conflict[0]["error"]["code"], "id_conflict");
    let unsupported = f.request(&mut c, request("cal", "calibration.start", json!({})));
    assert_eq!(unsupported[0]["error"]["code"], "unknown_method");
    let auto = f.request(
        &mut c,
        request(
            "auto",
            "settings.patch",
            json!({"patch":{"target_mode":"auto"},"save":false}),
        ),
    );
    assert_eq!(auto[0]["ok"], true);
    assert!(auto[0]["result"]["osc_target"].is_null());
}
#[test]
fn socket_security_and_exclusion() {
    let f = Fixture::new();
    let socket = f.root.join("control.sock");
    assert_eq!(
        fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let (store, _) = SettingsStore::load(f.root.join("other.json"), &Map::new(), false).unwrap();
    assert!(IpcServer::bind(socket, store).is_err());
    let public = f.root.join("public");
    fs::create_dir(&public).unwrap();
    fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
    let (store, _) = SettingsStore::load(f.root.join("other.json"), &Map::new(), false).unwrap();
    assert!(IpcServer::bind(public.join("control.sock"), store).is_err());
    assert_eq!(
        fs::metadata(public).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn abi_failure_reports_revalidated_old_input() {
    use crate::{
        input::{ShmVersion, fixtures::Fixture as InputFixture},
        settings::Settings,
    };
    let input = InputFixture::new(4);
    let mut f = Fixture::new();
    f.runtime = Runtime::with_paths(
        Settings {
            shm_version: ShmVersion::V4,
            ..Settings::default()
        },
        &input.path,
        &input.dir,
    )
    .unwrap();
    input.update(1, 1.0, [0.7; 2]);
    f.runtime.tick().unwrap();
    assert_eq!(f.runtime.snapshot().output, "sending");
    input.write(0, &9u32.to_le_bytes());
    let mut c = f.client();
    let response = f.request(
        &mut c,
        request(
            "abi",
            "settings.patch",
            json!({"patch":{"shm_version":"5"},"save":true}),
        ),
    );
    assert_eq!(response[0]["ok"], false);
    assert_eq!(
        response[0]["result"]["output"],
        json!({"state":"waiting","reason":"abi_mismatch"})
    );
    assert_eq!(f.runtime.snapshot().settings.shm_version, ShmVersion::V4);
    assert!(f.server.store.saved.is_empty());
}
#[test]
fn strict_numbers_before_float_narrowing() {
    use crate::settings::Settings;
    for patch in [
        json!({"eyelid_output_cap":1.0000000001}),
        json!({"one_euro_beta":-1e-50}),
        json!({"eyelid_output_cap":-1e-50}),
    ] {
        assert!(
            Settings::default()
                .patched(patch.as_object().unwrap())
                .is_err(),
            "{patch}"
        );
    }
}

#[test]
fn fragmented_lines_and_canonical_key_order() {
    let mut f = Fixture::new();
    let mut c = f.client();
    c.write_all(b"{\"v\":1,\"type\":\"request\",\"id\":\"s\",")
        .unwrap();
    f.pump();
    assert!(read(&mut c).is_empty());
    c.write_all(b"\"method\":\"state.get\",\"params\":{}}\n{\"v\":1,\"type\":\"request\",\"id\":\"p\",\"method\":\"settings.patch\",\"params\":{\"patch\":{\"prefix\":\"/X\",\"one_euro\":false},\"save\":false}}\n").unwrap();
    f.pump();
    let replies = read(&mut c);
    assert_eq!(replies.len(), 3);
    assert_eq!(replies[0]["id"], "s");
    assert_eq!(replies[1]["id"], "p");
    assert_eq!(replies[2]["type"], "event");
    c.write_all(b"{\"params\":{\"save\":false,\"patch\":{\"one_euro\":false,\"prefix\":\"/X\"}},\"method\":\"settings.patch\",\"id\":\"p\",\"type\":\"request\",\"v\":1}\n").unwrap();
    f.pump();
    assert_eq!(read(&mut c), vec![replies[1].clone()]);
}
#[test]
fn malformed_wire_disconnects_without_partial_apply() {
    for wire in [
        b"{bad}\n".to_vec(),
        b"{\"v\":1,\"v\":1}\n".to_vec(),
        b"{\"x\":{\"a\":1,\"a\":2}}\n".to_vec(),
        b"{\"x\":1e999}\n".to_vec(),
        vec![b'x'; LINE_LIMIT + 1],
        vec![255, b'\n'],
    ] {
        let mut f = Fixture::new();
        let mut c = f.client();
        c.write_all(&wire).unwrap();
        for _ in 0..10 {
            f.pump();
        }
        let mut buffer = [0];
        let ended = match c.read(&mut buffer) {
            Ok(0) => true,
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => true,
            _ => false,
        };
        assert!(ended, "wire len {}", wire.len());
        assert_eq!(f.runtime.snapshot().settings.prefix, "/FT");
    }
}
#[test]
fn strict_request_envelopes_and_params() {
    let mut f = Fixture::new();
    let mut c = f.client();
    for (value, code) in [
        (
            json!({"v":2,"type":"request","id":"v","method":"state.get","params":{}}),
            "unsupported_version",
        ),
        (
            json!({"v":1,"type":"request","id":"","method":"state.get","params":{}}),
            "invalid_request",
        ),
        (
            json!({"v":1,"type":"request","id":null,"method":"state.get","params":{}}),
            "invalid_request",
        ),
        (
            request("extra", "state.get", json!({"extra":0})),
            "invalid_params",
        ),
        (
            request(
                "missing",
                "settings.patch",
                json!({"patch":{"prefix":"/X"}}),
            ),
            "invalid_params",
        ),
        (
            request("wrongbool", "settings.patch", json!({"patch":{},"save":1})),
            "invalid_params",
        ),
        (
            request(
                "extra_patch",
                "settings.patch",
                json!({"patch":{},"save":false,"other":1}),
            ),
            "invalid_params",
        ),
        (
            request(
                "margin",
                "settings.patch",
                json!({"patch":{"eye_widening_margin_left":0.05},"save":true}),
            ),
            "invalid_settings",
        ),
    ] {
        let result = f.request(&mut c, value.clone());
        assert_eq!(result[0]["error"]["code"], code);
        assert!(result[0]["result"].is_object());
        assert_eq!(result[0]["v"], 1);
        if value["id"].is_null() || value["id"] == "" {
            assert!(result[0]["id"].is_null());
        }
    }
}
#[test]
fn request_limit_keeps_first_id_retryable() {
    let mut f = Fixture::new();
    let mut c = f.client();
    let first = f.request(&mut c, request("0", "unavailable", json!({})))[0].clone();
    for id in 1..CACHE_LIMIT {
        let reply = f.request(&mut c, request(&id.to_string(), "unavailable", json!({})));
        assert_eq!(reply[0]["error"]["code"], "unknown_method");
    }
    let limited = f.request(
        &mut c,
        request(
            "new",
            "settings.patch",
            json!({"patch":{"prefix":"/Wrong"},"save":false}),
        ),
    );
    assert_eq!(limited[0]["error"]["code"], "request_limit");
    assert_eq!(
        f.request(&mut c, request("0", "unavailable", json!({}))),
        vec![first]
    );
    assert_eq!(f.runtime.snapshot().settings.prefix, "/FT");
}
#[test]
fn atomic_save_retains_unknown_and_failure_retains_old_state() {
    let mut f = Fixture::new();
    let config = f.root.join("settings.json");
    fs::write(&config, b"{\"future\":{\"nested\":true},\"one_euro\":true}").unwrap();
    let (store, current) = SettingsStore::load(
        config.clone(),
        json!({"prefix":"/Cli"}).as_object().unwrap(),
        false,
    )
    .unwrap();
    f.server.store = store;
    f.runtime =
        Runtime::with_paths(current, &f.root.join("missing"), &f.root.join("proc")).unwrap();
    let mut c = f.client();
    let response = f.request(
        &mut c,
        request(
            "save",
            "settings.patch",
            json!({"patch":{"eyelid_output_cap":0.3},"save":true}),
        ),
    );
    assert_eq!(response[0]["result"]["save"], "saved");
    let saved: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(
        saved,
        json!({"future":{"nested":true},"one_euro":true,"eyelid_output_cap":0.3})
    );
    assert_eq!(fs::metadata(&config).unwrap().mode() & 0o777, 0o600);
    fs::remove_file(&config).unwrap();
    fs::create_dir(&config).unwrap();
    let response = f.request(
        &mut c,
        request(
            "fail",
            "settings.patch",
            json!({"patch":{"prefix":"/Wrong"},"save":true}),
        ),
    );
    assert_eq!(response[0]["error"]["code"], "save_failed");
    assert_eq!(response[0]["result"]["save"], "failed");
    assert_eq!(response[0]["result"]["current"]["prefix"], "/Cli");
    assert_eq!(f.server.store.saved, *saved.as_object().unwrap());
    assert!(config.is_dir());
    assert!(
        !fs::read_dir(&f.root).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp"))
    );
}
#[test]
fn invalid_known_saved_setting_is_startup_error_even_if_cli_overrides() {
    let f = Fixture::new();
    let config = f.root.join("bad.json");
    fs::write(&config, b"{\"one_euro\":1}").unwrap();
    assert!(
        SettingsStore::load(
            config,
            json!({"one_euro":false}).as_object().unwrap(),
            false
        )
        .is_err()
    );
}
#[test]
fn stale_socket_and_symlink_regular_foreign_paths() {
    let f = Fixture::new();
    let bind = |path: PathBuf| {
        let (store, _) =
            SettingsStore::load(f.root.join("other.json"), &Map::new(), false).unwrap();
        IpcServer::bind(path, store)
    };
    let stale = f.root.join("stale.sock");
    drop(UnixListener::bind(&stale).unwrap());
    assert!(bind(stale).is_ok());
    let live = f.root.join("live.sock");
    let listener = UnixListener::bind(&live).unwrap();
    assert!(bind(live.clone()).is_err());
    assert!(live.exists());
    drop(listener);
    let regular = f.root.join("regular");
    fs::write(&regular, b"keep").unwrap();
    assert!(bind(regular.clone()).is_err());
    assert_eq!(fs::read(&regular).unwrap(), b"keep");
    let link = f.root.join("link");
    std::os::unix::fs::symlink(&regular, &link).unwrap();
    assert!(bind(link.clone()).is_err());
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let parent_link = f.root.join("parent-link");
    std::os::unix::fs::symlink(&f.root, &parent_link).unwrap();
    assert!(bind(parent_link.join("nested.sock")).is_err());
    // /root provides an existing foreign-owned private parent on ordinary Linux
    // CI without needing chown privileges or touching its contents.
    if let Ok(meta) = fs::metadata("/root")
        && meta.uid() != unsafe { libc::geteuid() }
        && meta.mode() & 0o777 == 0o700
    {
        assert!(bind(PathBuf::from("/root/frameeyeosc-test.sock")).is_err());
    }
    // Only root can arrange foreign socket ownership; run when supported.
    if unsafe { libc::geteuid() } == 0 {
        use std::ffi::CString;
        let foreign = f.root.join("foreign.sock");
        drop(UnixListener::bind(&foreign).unwrap());
        let path = CString::new(foreign.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::chown(path.as_ptr(), 65534, 65534) }, 0);
        assert!(bind(foreign.clone()).is_err());
        assert!(foreign.exists());
    }
}
#[test]
fn sequential_connections_and_spontaneous_cause() {
    let mut f = Fixture::new();
    let mut a = f.client();
    let mut b = f.client();
    let a_state = f.request(&mut a, request("s", "state.get", json!({})))[0].clone();
    let b_state = f.request(&mut b, request("s", "state.get", json!({})))[0].clone();
    a.write_all(
        format!(
            "{}\n",
            request(
                "a",
                "settings.patch",
                json!({"patch":{"prefix":"/A"},"save":false})
            )
        )
        .as_bytes(),
    )
    .unwrap();
    b.write_all(
        format!(
            "{}\n",
            request(
                "b",
                "settings.patch",
                json!({"patch":{"prefix":"/B"},"save":false})
            )
        )
        .as_bytes(),
    )
    .unwrap();
    f.pump();
    let a_messages = read(&mut a);
    let b_messages = read(&mut b);
    let a_events: Vec<_> = a_messages.iter().filter(|v| v["type"] == "event").collect();
    let b_events: Vec<_> = b_messages.iter().filter(|v| v["type"] == "event").collect();
    assert_eq!(a_events.len(), 2);
    assert_eq!(b_events.len(), 2);
    for (a, b) in a_events.iter().zip(b_events.iter()) {
        assert_eq!(a["data"]["settings"], b["data"]["settings"]);
        assert_eq!(a["caused_by"], b["caused_by"]);
        assert_eq!(
            a["data"]["connection_id"],
            a_state["result"]["connection_id"]
        );
        assert_eq!(
            b["data"]["connection_id"],
            b_state["result"]["connection_id"]
        );
    }
    assert_eq!(
        a_events[1]["data"]["settings"]["current"]["prefix"],
        f.runtime.snapshot().settings.prefix
    );
    f.runtime.tick().unwrap();
    f.pump();
    let spontaneous = read(&mut a);
    assert_eq!(spontaneous[0]["caused_by"], Value::Null);
    assert_eq!(spontaneous[0]["data"]["tracking"], "inactive");
    drop(b);
    f.pump();
    let mut reconnect = f.client();
    let r = f.request(&mut reconnect, request("s", "state.get", json!({})));
    assert_ne!(
        r[0]["result"]["connection_id"],
        b_state["result"]["connection_id"]
    );
}
#[test]
fn ipc_patch_cap_reaches_real_udp_and_failed_abi_keeps_valid_source() {
    use crate::{input::fixtures::Fixture as InputFixture, settings::Settings};
    use std::{net::UdpSocket, time::Duration};
    let input = InputFixture::new(4);
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut f = Fixture::new();
    f.runtime = Runtime::with_paths(
        Settings {
            target_port: receiver.local_addr().unwrap().port(),
            ..Settings::default()
        },
        &input.path,
        &input.dir,
    )
    .unwrap();
    let mut c = f.client();
    let patch = f.request(
        &mut c,
        request(
            "cap",
            "settings.patch",
            json!({"patch":{"eyelid_output_cap":0.7,"one_euro":true},"save":false}),
        ),
    );
    assert_eq!(patch[0]["result"]["sources"]["eyelid_output_cap"], "panel");
    input.update(1, 1.0, [1.0; 2]);
    f.runtime.tick().unwrap();
    let mut buf = [0; 1024];
    let mut lids = 0;
    for _ in 0..9 {
        let n = receiver.recv(&mut buf).unwrap();
        if buf[..n].windows(6).any(|w| w == b"EyeLid") {
            assert_eq!(f32::from_be_bytes(buf[n - 4..n].try_into().unwrap()), 0.7);
            lids += 1;
        }
    }
    assert_eq!(lids, 2);
    let failed = f.request(
        &mut c,
        request(
            "abi",
            "settings.patch",
            json!({"patch":{"shm_version":"5"},"save":true}),
        ),
    );
    assert_eq!(failed[0]["ok"], false);
    assert_eq!(
        failed[0]["result"]["output"],
        json!({"state":"sending","reason":null})
    );
    input.update(2, 1.01, [1.0; 2]);
    f.runtime.tick().unwrap();
    for _ in 0..9 {
        receiver.recv(&mut buf).unwrap();
    }
    assert_eq!(f.runtime.snapshot().output, "sending");
}
#[test]
fn client_limits_and_bounded_queue() {
    let mut f = Fixture::new();
    let mut clients: Vec<_> = (0..CLIENT_LIMIT + 1).map(|_| f.client()).collect();
    f.pump();
    assert_eq!(f.server.clients.len(), CLIENT_LIMIT);
    let mut buffer = [0];
    assert_eq!(clients[CLIENT_LIMIT].read(&mut buffer).unwrap(), 0);
    let id = f.server.clients[0].id.clone();
    for _ in 0..20 {
        f.server.clients[0].queue(&json!({"payload":"x".repeat(LINE_LIMIT)}));
    }
    assert!(f.server.clients[0].dead);
    assert!(f.server.clients[0].queued <= QUEUE_LIMIT);
    f.pump();
    assert!(!f.server.clients.iter().any(|c| c.id == id));
}
#[test]
fn reused_id_still_checks_envelope_before_cache() {
    let mut f = Fixture::new();
    let mut c = f.client();
    let valid = request("id", "state.get", json!({}));
    assert_eq!(f.request(&mut c, valid.clone())[0]["ok"], true);
    for (key, value, code) in [
        ("v", json!(2), "unsupported_version"),
        ("type", json!("event"), "invalid_request"),
        ("extra", json!(0), "invalid_request"),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        let response = f.request(&mut c, invalid);
        assert_eq!(response[0]["ok"], false);
        assert_eq!(response[0]["error"]["code"], code);
    }
}
#[test]
fn cache_memory_limit_precedes_mutation() {
    let mut f = Fixture::new();
    let mut a = f.client();
    let mut b = f.client();
    f.request(&mut a, request("s", "state.get", json!({})));
    f.request(&mut b, request("s", "state.get", json!({})));
    f.server.clients[0].cache_bytes = CACHE_BYTES - 1;
    a.write_all(
        format!(
            "{}\n",
            request(
                "overflow",
                "settings.patch",
                json!({"patch":{"prefix":"/Wrong"},"save":true})
            )
        )
        .as_bytes(),
    )
    .unwrap();
    f.pump();
    assert_eq!(f.runtime.snapshot().settings.prefix, "/FT");
    assert!(f.server.store.saved.is_empty());
    assert!(read(&mut b).is_empty());
}
#[test]
fn frozen_wire_contract_matches_real_socket() {
    let lines: Vec<Value> = include_str!("../fixtures/ipc-v1-subset.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut f = Fixture::new();
    let mut c = f.client();
    let mut index = 0;
    while index < lines.len() {
        assert_eq!(lines[index]["type"], "request");
        let request = lines[index].clone();
        index += 1;
        let start = index;
        while index < lines.len() && lines[index]["type"] != "request" {
            index += 1;
        }
        let mut actual = f.request(&mut c, request);
        for message in &mut actual {
            let snapshot = if message["type"] == "event" {
                Some(&mut message["data"])
            } else if message["result"].get("instance_id").is_some() {
                Some(&mut message["result"])
            } else {
                None
            };
            if let Some(snapshot) = snapshot {
                snapshot["instance_id"] = json!("i-fixture");
            }
        }
        assert_eq!(actual, lines[start..index]);
    }
}
#[test]
fn save_failure_revalidates_old_mapping_before_reply() {
    use crate::{input::fixtures::Fixture as InputFixture, settings::Settings};
    let input = InputFixture::new(4);
    let mut f = Fixture::new();
    let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    f.runtime = Runtime::with_paths(
        Settings {
            target_port: receiver.local_addr().unwrap().port(),
            ..Settings::default()
        },
        &input.path,
        &input.dir,
    )
    .unwrap();
    input.update(1, 1.0, [0.7; 2]);
    f.runtime.tick().unwrap();
    input.write(0, &9u32.to_le_bytes());
    fs::create_dir(f.root.join("settings.json")).unwrap();
    let mut c = f.client();
    let reply = f.request(
        &mut c,
        request(
            "fail",
            "settings.patch",
            json!({"patch":{"prefix":"/Wrong"},"save":true}),
        ),
    );
    assert_eq!(reply[0]["error"]["code"], "save_failed");
    assert_eq!(
        reply[0]["result"]["output"],
        json!({"state":"waiting","reason":"abi_mismatch"})
    );
    assert_eq!(reply[0]["result"]["current"]["prefix"], "/FT");
    assert!(f.server.store.saved.is_empty());
}
fn assert_tracking_packet(receiver: &std::net::UdpSocket, active: bool, prefix: &str) {
    let mut bytes = [0; 4096];
    let n = receiver
        .recv(&mut bytes)
        .expect("tracking packet must arrive");
    let flag: &[u8] = if active { b",T\0" } else { b",F\0" };
    assert!(bytes[..n].windows(3).any(|w| w == flag));
    let address = format!("/avatar/parameters{prefix}/frame/EyeTrackingActive");
    assert!(
        bytes[..n]
            .windows(address.len())
            .any(|w| w == address.as_bytes())
    );
    if active {
        // Consume the eight value packets emitted after the tracking flag.
        for _ in 0..8 {
            receiver.recv(&mut bytes).unwrap();
        }
    }
}
fn rejected_transaction_preserves_normal_inactive_notification(save_failure: bool) {
    use crate::{input::fixtures::Fixture as InputFixture, settings::Settings};
    {
        let input = InputFixture::new(4);
        let mut f = Fixture::new();
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(std::time::Duration::from_millis(100)))
            .unwrap();
        f.runtime = Runtime::with_paths(
            Settings {
                target_port: receiver.local_addr().unwrap().port(),
                ..Settings::default()
            },
            &input.path,
            &input.dir,
        )
        .unwrap();
        input.update(1, 1.0, [0.7; 2]);
        f.runtime.tick().unwrap();
        assert_tracking_packet(&receiver, true, "/FT");
        if save_failure {
            input.write(0, &9u32.to_le_bytes());
            fs::create_dir(f.root.join("settings.json")).unwrap();
        } else {
            fs::remove_file(&input.path).unwrap();
        }
        let mut c = f.client();
        let patch = if save_failure {
            json!({"prefix":"/Wrong"})
        } else {
            json!({"shm_version":"5","prefix":"/Wrong"})
        };
        let reply = f.request(
            &mut c,
            request("fail", "settings.patch", json!({"patch":patch,"save":true})),
        );
        assert_eq!(
            reply[0]["error"]["code"],
            if save_failure {
                "save_failed"
            } else {
                "prepare_failed"
            }
        );
        f.runtime.tick().unwrap();
        assert_tracking_packet(&receiver, false, "/FT");
        receiver.set_nonblocking(true).unwrap();
        f.runtime.tick().unwrap();
        assert_eq!(
            receiver.recv(&mut [0; 4096]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
#[test]
fn failed_save_preserves_normal_inactive_notification() {
    rejected_transaction_preserves_normal_inactive_notification(true);
}
#[test]
fn failed_prepare_preserves_normal_inactive_notification() {
    rejected_transaction_preserves_normal_inactive_notification(false);
}
#[test]
fn abi_switch_then_producer_stop_notifies_current_destination_once() {
    use crate::{input::fixtures::Fixture as InputFixture, settings::Settings};
    let input = InputFixture::new(4);
    let mut f = Fixture::new();
    let old = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let new = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    old.set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();
    new.set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();
    f.runtime = Runtime::with_paths(
        Settings {
            target_port: old.local_addr().unwrap().port(),
            ..Settings::default()
        },
        &input.path,
        &input.dir,
    )
    .unwrap();
    input.update(1, 1.0, [0.7; 2]);
    f.runtime.tick().unwrap();
    assert_tracking_packet(&old, true, "/FT");
    let mut c = f.client();
    let reply = f.request(
        &mut c,
        request(
            "switch",
            "settings.patch",
            json!({"patch":{"shm_version":"4","prefix":"/New",
            "target_port":new.local_addr().unwrap().port()},"save":false}),
        ),
    );
    assert_eq!(reply[0]["ok"], true);
    std::thread::sleep(std::time::Duration::from_millis(1050));
    f.runtime.tick().unwrap();
    assert_tracking_packet(&new, false, "/New");
    old.set_nonblocking(true).unwrap();
    new.set_nonblocking(true).unwrap();
    f.runtime.tick().unwrap();
    for receiver in [&old, &new] {
        assert_eq!(
            receiver.recv(&mut [0; 4096]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
#[test]
fn symlink_ancestor_does_not_create_redirected_parent() {
    let f = Fixture::new();
    let outside = f.root.join("outside");
    fs::create_dir(&outside).unwrap();
    let link = f.root.join("redirect");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let (store, _) = SettingsStore::load(f.root.join("other.json"), &Map::new(), false).unwrap();
    assert!(IpcServer::bind(link.join("new/control.sock"), store).is_err());
    assert!(!outside.join("new").exists());
}
#[test]
fn failed_save_preserves_existing_file_bytes() {
    // Root bypasses directory write bits. Non-root CI can exercise the actual
    // filesystem error without mocking the writer or injecting production hooks.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let mut f = Fixture::new();
    let directory = f.root.join("readonly");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("settings.json");
    let old = b"{\"prefix\":\"/Old\",\"future\":42}\n";
    fs::write(&path, old).unwrap();
    let (store, current) = SettingsStore::load(path.clone(), &Map::new(), false).unwrap();
    f.server.store = store;
    f.runtime =
        Runtime::with_paths(current, &f.root.join("missing"), &f.root.join("proc")).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
    let mut c = f.client();
    let reply = f.request(
        &mut c,
        request(
            "fail",
            "settings.patch",
            json!({"patch":{"prefix":"/Wrong"},"save":true}),
        ),
    );
    assert_eq!(reply[0]["result"]["save"], "failed");
    assert_eq!(reply[0]["result"]["current"]["prefix"], "/Old");
    assert_eq!(fs::read(path).unwrap(), old);
    assert_eq!(f.server.store.saved["future"], 42);
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
}
#[test]
fn decimal_current_saved_and_patch_values_match() {
    let mut f = Fixture::new();
    let mut c = f.client();
    let patch = json!({"eyelid_output_cap":0.7,"one_euro_beta":0.1});
    let reply = f.request(
        &mut c,
        request(
            "decimal",
            "settings.patch",
            json!({"patch":patch,"save":true}),
        ),
    );
    assert_eq!(reply[0]["result"]["current"]["eyelid_output_cap"], 0.7);
    let state = f.request(&mut c, request("state", "state.get", json!({})));
    for key in ["eyelid_output_cap", "one_euro_beta"] {
        assert_eq!(state[0]["result"]["settings"]["current"][key], patch[key]);
        assert_eq!(state[0]["result"]["settings"]["saved"][key], patch[key]);
    }
}
#[test]
fn cached_payloads_are_compact_bytes_not_value_trees() {
    assert_eq!(
        std::mem::size_of::<Cached>(),
        2 * std::mem::size_of::<Box<[u8]>>()
    );
}
#[test]
fn future_envelope_reports_version_before_v1_shape() {
    let mut f = Fixture::new();
    let mut c = f.client();
    for value in [
        json!({"v":2,"type":"request","id":"future1","method":"state.get","params":{},"trace":1}),
        json!({"v":2,"type":"request","id":"future2","method":"m"}),
    ] {
        let reply = f.request(&mut c, value);
        assert_eq!(reply[0]["error"]["code"], "unsupported_version");
        assert_eq!(reply[0]["v"], 1);
    }
}
#[test]
fn half_closed_requests_receive_all_complete_responses_then_close() {
    let mut f = Fixture::new();
    let mut c = f.client();
    let bytes = format!(
        "{}\n{}\n{{incomplete",
        request("s", "state.get", json!({})),
        request(
            "p",
            "settings.patch",
            json!({"patch":{"prefix":"/Half"},"save":false})
        )
    );
    c.write_all(bytes.as_bytes()).unwrap();
    c.shutdown(std::net::Shutdown::Write).unwrap();
    f.pump();
    let replies = read(&mut c);
    assert_eq!(replies.len(), 2, "half-close must not receive events");
    let responses: Vec<_> = replies.iter().filter(|v| v["type"] == "response").collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["id"], "s");
    assert_eq!(responses[1]["id"], "p");
    assert_eq!(responses[1]["result"]["current"]["prefix"], "/Half");
    assert!(f.server.clients.is_empty());
}
#[test]
fn unknown_saved_keys_are_disk_only_and_config_read_is_bounded() {
    let mut f = Fixture::new();
    let config = f.root.join("settings.json");
    fs::write(
        &config,
        b"{\"future\":{\"private\":true},\"prefix\":\"/Saved\"}",
    )
    .unwrap();
    let (store, current) = SettingsStore::load(config.clone(), &Map::new(), false).unwrap();
    f.server.store = store;
    f.runtime =
        Runtime::with_paths(current, &f.root.join("missing"), &f.root.join("proc")).unwrap();
    let mut c = f.client();
    let reply = f.request(&mut c, request("s", "state.get", json!({})));
    assert_eq!(
        reply[0]["result"]["settings"]["saved"],
        json!({"prefix":"/Saved"})
    );
    assert!(f.server.store.saved.contains_key("future"));
    fs::write(
        &config,
        serde_json::to_vec(&json!({"future":"x".repeat(64*1024)})).unwrap(),
    )
    .unwrap();
    assert!(SettingsStore::load(config, &Map::new(), false).is_err());
}
#[test]
fn oversized_save_fails_before_replacement_or_application() {
    let mut f = Fixture::new();
    let config = f.root.join("settings.json");
    let old = serde_json::to_vec(&json!({"future":"x".repeat(64_000)})).unwrap();
    fs::write(&config, &old).unwrap();
    let (store, _) = SettingsStore::load(config.clone(), &Map::new(), false).unwrap();
    f.server.store = store;
    let mut c = f.client();
    let reply = f.request(
        &mut c,
        request(
            "p",
            "settings.patch",
            json!({"patch":{"prefix":format!("/{}","x".repeat(2000))},"save":true}),
        ),
    );
    assert_eq!(reply[0]["error"]["code"], "save_failed");
    assert_eq!(f.runtime.snapshot().settings.prefix, "/FT");
    assert_eq!(fs::read(config).unwrap(), old);
}
#[test]
fn transient_accept_and_stream_setup_failures_preserve_runtime_and_clients() {
    use crate::{input::fixtures::Fixture as InputFixture, settings::Settings};
    let input = InputFixture::new(4);
    let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver
        .set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();
    let mut f = Fixture::new();
    f.runtime = Runtime::with_paths(
        Settings {
            target_port: receiver.local_addr().unwrap().port(),
            ..Settings::default()
        },
        &input.path,
        &input.dir,
    )
    .unwrap();
    let mut c = f.client();
    f.pump();
    c.write_all(format!("{}\n", request("s", "state.get", json!({}))).as_bytes())
        .unwrap();
    assert!(
        f.server
            .poll_with_accept(
                &mut f.runtime,
                |_| Err(io::Error::from_raw_os_error(libc::EMFILE)),
                |s| s.set_nonblocking(true)
            )
            .is_ok()
    );
    assert_eq!(read(&mut c)[0]["ok"], true);
    let first_error = f.server.last_accept_error;
    let _second = f.client();
    assert!(
        f.server
            .poll_with_accept(
                &mut f.runtime,
                |l| l.accept().map(|(s, _)| s),
                |_| Err(io::Error::other("setup failure"))
            )
            .is_ok()
    );
    assert_eq!(f.server.clients.len(), 1);
    assert_eq!(f.server.last_accept_error, first_error);
    for errno in [libc::EBADF, libc::ENOTSOCK, libc::EINVAL] {
        let error = f
            .server
            .poll_with_accept(
                &mut f.runtime,
                |_| Err(io::Error::from_raw_os_error(errno)),
                |s| s.set_nonblocking(true),
            )
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(errno));
    }
    input.update(1, 1.0, [0.7; 2]);
    f.runtime.tick().unwrap();
    let mut buf = [0; 1024];
    for _ in 0..9 {
        receiver.recv(&mut buf).unwrap();
    }
    assert_eq!(f.runtime.snapshot().output, "sending");
}
#[test]
fn created_parent_and_lock_have_private_modes() {
    if std::env::var_os("FRAMEEYEOSC_PERMISSION_CHILD").is_none() {
        let full_name = concat!(
            module_path!(),
            "::created_parent_and_lock_have_private_modes"
        );
        // libtest names omit the crate prefix printed by module_path!().
        let name = full_name.split_once("::").unwrap().1;
        let output = std::process::Command::new("sh")
            .args([
                "-c",
                "umask 022; exec \"$1\" --exact \"$2\" --nocapture",
                "sh",
            ])
            .arg(std::env::current_exe().unwrap())
            .arg(name)
            .env("FRAMEEYEOSC_PERMISSION_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        return;
    }
    let f = Fixture::new();
    let parent = f.root.join("new");
    let (store, _) = SettingsStore::load(f.root.join("other.json"), &Map::new(), false).unwrap();
    let _server = IpcServer::bind(parent.join("control.sock"), store).unwrap();
    assert_eq!(fs::metadata(&parent).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        fs::metadata(parent.join("control.sock.lock"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(parent.join("control.sock")).unwrap().mode() & 0o777,
        0o600
    );
}
#[test]
fn ipc_filter_enable_and_each_coefficient_reset_real_udp_history() {
    use crate::{input::fixtures::Fixture as InputFixture, settings::Settings};
    use std::net::UdpSocket;
    fn lids(receiver: &UdpSocket) -> [f32; 2] {
        let mut buf = [0; 1024];
        let mut lids = Vec::new();
        for _ in 0..9 {
            let n = receiver.recv(&mut buf).unwrap();
            if buf[..n].windows(6).any(|w| w == b"EyeLid") {
                lids.push(f32::from_be_bytes(buf[n - 4..n].try_into().unwrap()));
            }
        }
        lids.try_into().unwrap()
    }
    for patch in [
        json!({"one_euro_min_cutoff":0.6}),
        json!({"one_euro_beta":2.0}),
        json!({"one_euro_d_cutoff":2.0}),
        json!({"one_euro":false}),
    ] {
        let input = InputFixture::new(4);
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut f = Fixture::new();
        f.runtime = Runtime::with_paths(
            Settings {
                one_euro: true,
                target_port: receiver.local_addr().unwrap().port(),
                ..Settings::default()
            },
            &input.path,
            &input.dir,
        )
        .unwrap();
        let mut c = f.client();
        input.update(1, 1.0, [1.0; 2]);
        f.runtime.tick().unwrap();
        assert_eq!(lids(&receiver), [1.0; 2]);
        input.update(2, 1.01, [0.0; 2]);
        f.runtime.tick().unwrap();
        assert!(lids(&receiver).iter().all(|v| *v > 0.0));
        assert_eq!(
            f.request(
                &mut c,
                request(
                    "reset",
                    "settings.patch",
                    json!({"patch":patch,"save":false})
                )
            )[0]["ok"],
            true
        );
        input.update(3, 1.02, [0.2; 2]);
        f.runtime.tick().unwrap();
        assert_eq!(lids(&receiver), [0.2; 2]);
        if patch.get("one_euro").is_some() {
            assert_eq!(
                f.request(
                    &mut c,
                    request(
                        "enable",
                        "settings.patch",
                        json!({"patch":{"one_euro":true},"save":false})
                    )
                )[0]["ok"],
                true
            );
            input.update(4, 1.03, [0.0; 2]);
            f.runtime.tick().unwrap();
            assert_eq!(lids(&receiver), [0.0; 2]);
        }
    }
}
#[test]
fn large_array_cache_accounts_exact_retained_byte_payloads() {
    let mut f = Fixture::new();
    let mut c = f.client();
    let params = json!({"array":vec![0;30_000]});
    for id in 0..3 {
        let reply = f.request(
            &mut c,
            request(&format!("big{id}"), "unavailable", params.clone()),
        );
        assert_eq!(reply[0]["error"]["code"], "unknown_method");
    }
    let client = &f.server.clients[0];
    let retained: usize = client
        .cache
        .iter()
        .map(|(id, c)| id.len() + c.request.len() + c.response.len())
        .sum();
    assert_eq!(client.cache_bytes, retained);
    assert!(retained < 200_000);
    let before = client.cache_bytes;
    f.request(&mut c, request("big0", "unavailable", params));
    assert_eq!(f.server.clients[0].cache_bytes, before);
}
#[test]
fn canonical_retry_preserves_parsed_signed_zero_equality() {
    assert_eq!(json!(-0.0), json!(0.0));
    let mut f = Fixture::new();
    let mut c = f.client();
    let first = f.request(
        &mut c,
        request("zero", "unavailable", json!({"nested":[-0.0]})),
    );
    assert_eq!(
        f.request(
            &mut c,
            request("zero", "unavailable", json!({"nested":[0.0]}))
        ),
        first
    );
}
#[test]
fn known_saved_numeric_representations_match_current_without_changing_port_type() {
    let mut f = Fixture::new();
    let mut c = f.client();
    let reply=f.request(&mut c,request("integer","settings.patch",json!({"patch":{"one_euro_beta":3,"eyelid_output_cap":1,"target_port":9001},"save":true})));
    assert_eq!(reply[0]["ok"], true);
    let state = f.request(&mut c, request("s", "state.get", json!({})));
    for key in ["one_euro_beta", "eyelid_output_cap", "target_port"] {
        assert_eq!(
            state[0]["result"]["settings"]["current"][key],
            state[0]["result"]["settings"]["saved"][key]
        );
    }
    assert!(f.server.store.saved["one_euro_beta"].is_f64());
    assert!(f.server.store.saved["target_port"].is_u64());
    let config = f.root.join("handwritten.json");
    fs::write(&config,b"{\"eyelid_output_cap\":1,\"one_euro_beta\":3,\"target_port\":9000,\"future\":9007199254740993}").unwrap();
    let (store, current) = SettingsStore::load(config, &Map::new(), false).unwrap();
    assert_eq!(
        store.known_saved()["eyelid_output_cap"],
        current.values()["eyelid_output_cap"]
    );
    assert_eq!(
        store.known_saved()["one_euro_beta"],
        current.values()["one_euro_beta"]
    );
    assert!(store.known_saved()["target_port"].is_u64());
    assert_eq!(store.saved["future"].as_u64(), Some(9007199254740993));
}

#[test]
fn half_close_ends_preexisting_subscription_without_losing_patch_response() {
    let mut f = Fixture::new();
    let mut c = f.client();
    let initial = f.request(&mut c, request("s", "state.get", json!({})));
    assert_eq!(initial.len(), 1);
    assert!(f.server.clients[0].subscribed);
    c.write_all(
        format!(
            "{}\n",
            request(
                "p",
                "settings.patch",
                json!({"patch":{"prefix":"/End"},"save":false})
            )
        )
        .as_bytes(),
    )
    .unwrap();
    c.shutdown(std::net::Shutdown::Write).unwrap();
    f.pump();
    let replies = read(&mut c);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["type"], "response");
    assert_eq!(replies[0]["id"], "p");
    assert_eq!(replies[0]["ok"], true);
    assert!(f.server.clients.is_empty());
}
