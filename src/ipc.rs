//! IPC v1 implemented subset: state and transactional settings only.
use crate::{runtime::Runtime, settings::SettingsStore, target::StreamState};
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
const LINE_LIMIT: usize = 64 * 1024;
const QUEUE_LIMIT: usize = 1024 * 1024;
const CACHE_LIMIT: usize = 4096;
const CACHE_BYTES: usize = 16 * 1024 * 1024;
const CLIENT_LIMIT: usize = 8;
const IO_BUDGET: usize = 16 * 1024;
const REQUEST_BUDGET: usize = 8;

// Build Value only after every map has checked its keys. Deserializing directly
// into Value would silently replace duplicate keys, including nested duplicates.
struct StrictValue(Value);
impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}
struct StrictVisitor;
impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("finite JSON with unique object keys")
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(v)))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(v.into()))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(v.into()))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
        // Value equality treats both signed floating zeros as equal. Preserve
        // that equivalence when caching canonical serialized parsed requests.
        let v = if v == 0.0 { 0.0 } else { v };
        serde_json::Number::from_f64(v)
            .map(|n| StrictValue(Value::Number(n)))
            .ok_or_else(|| E::custom("nonfinite number"))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(v.into())))
    }
    fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(v)))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(StrictValue(v)) = access.next_element()? {
            values.push(v);
        }
        Ok(StrictValue(Value::Array(values)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        while let Some((key, StrictValue(v))) = access.next_entry::<String, StrictValue>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("duplicate object key"));
            }
            values.insert(key, v);
        }
        Ok(StrictValue(Value::Object(values)))
    }
}
pub(crate) fn parse_json(bytes: &[u8]) -> serde_json::Result<Value> {
    serde_json::from_slice::<StrictValue>(bytes).map(|v| v.0)
}
pub(crate) fn unique_id() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
struct Cached {
    request: Box<[u8]>,
    response: Box<[u8]>,
}
struct Client {
    stream: UnixStream,
    id: String,
    subscribed: bool,
    input: Vec<u8>,
    output: VecDeque<Box<[u8]>>,
    written: usize,
    queued: usize,
    cache: HashMap<Box<str>, Cached>,
    cache_bytes: usize,
    dead: bool,
    eof: bool,
}
impl Client {
    fn queue(&mut self, message: &Value) {
        let mut bytes = serde_json::to_vec(message).expect("JSON values serialize");
        bytes.push(b'\n');
        self.queue_bytes(bytes.into_boxed_slice());
    }
    fn queue_bytes(&mut self, bytes: Box<[u8]>) {
        if self.queued + bytes.len() > QUEUE_LIMIT {
            self.dead = true;
            return;
        }
        self.queued += bytes.len();
        self.output.push_back(bytes);
    }
    fn receive(&mut self) -> io::Result<()> {
        if self.eof {
            return Ok(());
        }
        let mut remaining = IO_BUDGET;
        let mut buffer = [0; 4096];
        while remaining > 0 && self.input.len() < LINE_LIMIT + IO_BUDGET {
            let length = remaining.min(buffer.len());
            match self.stream.read(&mut buffer[..length]) {
                Ok(0) => {
                    self.eof = true;
                    self.subscribed = false;
                    break;
                }
                Ok(n) => {
                    remaining -= n;
                    self.input.extend_from_slice(&buffer[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                Err(e) => return Err(e),
            }
        }
        // All complete lines and the incomplete tail are independently bounded.
        if self
            .input
            .split(|b| *b == b'\n')
            .any(|line| line.len() > LINE_LIMIT)
        {
            self.dead = true;
        }
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        let mut budget = IO_BUDGET;
        while budget > 0 {
            let Some(front) = self.output.front() else {
                break;
            };
            let end = front.len().min(self.written + budget);
            match self.stream.write(&front[self.written..end]) {
                Ok(0) => {
                    self.dead = true;
                    break;
                }
                Ok(n) => {
                    budget -= n;
                    self.queued -= n;
                    self.written += n;
                    if self.written == front.len() {
                        self.output.pop_front();
                        self.written = 0;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}
pub(crate) struct IpcServer {
    listener: UnixListener,
    _lock: File,
    path: PathBuf,
    identity: (u64, u64),
    instance_id: String,
    next_connection: u64,
    clients: Vec<Client>,
    pub(crate) store: SettingsStore,
    last_state: Option<Value>,
    cursor: usize,
    last_accept_error: Option<Instant>,
}
fn reject_symlink_ancestors(path: &Path) -> io::Result<()> {
    let mut ancestor = Some(path);
    while let Some(p) = ancestor {
        if !p.as_os_str().is_empty() {
            match fs::symlink_metadata(p) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(io::Error::other("symlink in control socket path"));
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        ancestor = p.parent();
    }
    Ok(())
}
fn private_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::other("control socket needs a private parent directory"))?;
    // Check existing ancestors before creating anything through the path.
    reject_symlink_ancestors(parent)?;
    match fs::symlink_metadata(parent) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(parent)?;
        }
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    reject_symlink_ancestors(parent)?;
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(io::Error::other(
            "control socket parent must be owned by this user with mode 0700",
        ));
    }
    Ok(())
}
impl IpcServer {
    pub(crate) fn bind(path: PathBuf, store: SettingsStore) -> io::Result<Self> {
        private_parent(&path)?;
        let lock_path = path.with_file_name(format!(
            "{}.lock",
            path.file_name()
                .ok_or_else(|| io::Error::other("invalid socket path"))?
                .to_string_lossy()
        ));
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&lock_path)?;
        let meta = lock.metadata()?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o777 != 0o600
        {
            return Err(io::Error::other("invalid control socket lock"));
        }
        // SAFETY: flock receives a live file descriptor and valid operation flags.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if !meta.file_type().is_socket() || meta.uid() != unsafe { libc::geteuid() } {
                    return Err(io::Error::other(
                        "refusing to replace non-socket or foreign socket",
                    ));
                }
                match UnixStream::connect(&path) {
                    Ok(_) => {
                        return Err(io::Error::other(
                            "control socket already has a live instance",
                        ));
                    }
                    Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                        fs::remove_file(&path)?
                    }
                    Err(e) => return Err(e),
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let instance_id = unique_id()?;
        let listener = UnixListener::bind(&path)?;
        if let Err(e) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .and_then(|()| listener.set_nonblocking(true))
        {
            let _ = fs::remove_file(&path);
            return Err(e);
        }
        let meta = fs::symlink_metadata(&path)?;
        Ok(Self {
            listener,
            _lock: lock,
            path,
            identity: (meta.dev(), meta.ino()),
            instance_id,
            next_connection: 0,
            clients: Vec::new(),
            store,
            last_state: None,
            cursor: 0,
            last_accept_error: None,
        })
    }
    fn state(&self, runtime: &Runtime, connection: &str) -> Value {
        let snapshot = runtime.snapshot();
        let output = match snapshot.output {
            "sending" => json!({"state":"sending","reason":null}),
            reason @ ("input_unavailable" | "abi_mismatch" | "no_target") => {
                json!({"state":"waiting","reason":reason})
            }
            reason => json!({"state":"inactive","reason":reason}),
        };
        json!({"instance_id":self.instance_id,"connection_id":connection,
            "settings":{"current":snapshot.settings.values(),"saved":self.store.known_saved(),"sources":self.store.sources},
            "tracking":match snapshot.tracking {Some(true)=>"active",Some(false)=>"inactive",None=>"unknown"},
            "stream":{"state":match snapshot.stream.state {StreamState::Detected=>"connected",StreamState::Waiting|StreamState::Ambiguous=>"unknown"},"peer":snapshot.stream.peer.map(|p|p.to_string())},
            "osc_target":snapshot.osc_target.map(|p|p.to_string()),"output":output,"active_profile":null,"calibration":null})
    }
    fn patch_result(&self, runtime: &Runtime, save: &str) -> Value {
        let state = self.state(runtime, "");
        json!({"current":state["settings"]["current"],"sources":self.store.sources,"osc_target":state["osc_target"],"output":state["output"],"save":save})
    }
    fn publish(&mut self, runtime: &Runtime, cause: Value) {
        let state = self.state(runtime, "");
        if self.last_state.as_ref() == Some(&state) {
            return;
        }
        self.last_state = Some(state.clone());
        for client in &mut self.clients {
            if client.subscribed && !client.dead {
                let mut data = state.clone();
                data["connection_id"] = json!(client.id);
                client.queue(&json!({"v":1,"type":"event","event":"state.changed","data":data,"caused_by":cause}));
            }
        }
    }
    fn handle(&mut self, index: usize, runtime: &mut Runtime, value: Value) {
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let response_id = id.as_ref().map_or(Value::Null, |s| json!(s));
        let error = |code: &str, message: &str, result: Value| json!({"v":1,"type":"response","id":response_id,"ok":false,"result":result,"error":{"code":code,"message":message}});
        if id.is_none() {
            self.clients[index].queue(&error(
                "invalid_request",
                "id must be a nonempty string",
                json!({}),
            ));
            return;
        }
        if value["type"] != "request" || !(value["v"].is_u64() || value["v"].is_i64()) {
            self.clients[index].queue(&error(
                "invalid_request",
                "expected a request with an integer version",
                json!({}),
            ));
            return;
        }
        if value["v"] != 1 {
            self.clients[index].queue(&error(
                "unsupported_version",
                "only IPC v1 is supported",
                json!({}),
            ));
            return;
        }
        let valid = value.as_object().is_some_and(|map| {
            map.len() == 5
                && ["v", "type", "id", "method", "params"]
                    .iter()
                    .all(|k| map.contains_key(*k))
        }) && value["type"] == "request"
            && value["method"].is_string()
            && value["params"].is_object();
        if !valid || !value["v"].is_u64() {
            self.clients[index].queue(&error(
                "invalid_request",
                "expected a v1 request object",
                json!({}),
            ));
            return;
        }
        let id = id.unwrap();
        // Default serde_json maps are BTreeMaps: serializing this parsed value
        // yields deterministic object order, including objects nested in arrays.
        let canonical =
            serde_json::to_vec(&json!({"method":value.get("method"),"params":value.get("params")}))
                .unwrap()
                .into_boxed_slice();
        if let Some(cached) = self.clients[index].cache.get(id.as_str()) {
            if cached.request == canonical {
                let bytes = cached.response.clone();
                self.clients[index].queue_bytes(bytes);
            } else {
                self.clients[index].queue(&error(
                    "id_conflict",
                    "id was already used for a different request",
                    json!({}),
                ));
            }
            return;
        }
        if self.clients[index].cache.len() >= CACHE_LIMIT {
            self.clients[index].queue(&error(
                "request_limit",
                "connection request limit reached",
                json!({}),
            ));
            return;
        }
        // Reserve a conservative serialized response budget before any mutation.
        // A patch can add at most one line's strings to current and saved values.
        let response_budget =
            serde_json::to_vec(&self.state(runtime, "")).unwrap().len() + 2 * LINE_LIMIT + 4096;
        let reserve = canonical.len() + response_budget + id.len();
        if self.clients[index].cache_bytes.saturating_add(reserve) > CACHE_BYTES {
            self.clients[index].dead = true;
            return;
        }
        let mut cause = None;
        let response = {
            let params = value["params"].as_object().unwrap();
            match value["method"].as_str().unwrap() {
                "state.get" if params.is_empty() => {
                    let result = self.state(runtime, &self.clients[index].id);
                    self.clients[index].subscribed = !self.clients[index].eof;
                    json!({"v":1,"type":"response","id":id,"ok":true,"result":result,"error":null})
                }
                "state.get" => error(
                    "invalid_params",
                    "state.get requires empty params",
                    json!({}),
                ),
                "settings.patch" => {
                    let before = self.patch_result(runtime, "not_requested");
                    if params.len() != 2
                        || !params.get("patch").is_some_and(Value::is_object)
                        || !params.get("save").is_some_and(Value::is_boolean)
                    {
                        error(
                            "invalid_params",
                            "settings.patch requires patch object and save bool",
                            before,
                        )
                    } else {
                        let patch = params["patch"].as_object().unwrap();
                        match runtime.snapshot().settings.patched(patch) {
                            Err(e) => error("invalid_settings", &e.to_string(), before),
                            Ok(candidate) => match runtime.prepare_settings(&candidate) {
                                Err(e) => {
                                    runtime.revalidate_input();
                                    error(
                                        "prepare_failed",
                                        &e.to_string(),
                                        self.patch_result(runtime, "not_requested"),
                                    )
                                }
                                Ok(prepared) => {
                                    let save = params["save"].as_bool().unwrap();
                                    let saved = if save {
                                        self.store.save_patch(patch)
                                    } else {
                                        Ok(())
                                    };
                                    match saved {
                                        Err(e) => {
                                            runtime.revalidate_input();
                                            error(
                                                "save_failed",
                                                &e.to_string(),
                                                self.patch_result(runtime, "failed"),
                                            )
                                        }
                                        Ok(()) => {
                                            runtime.apply_settings(prepared);
                                            self.store.panel_sources(patch);
                                            cause = Some(
                                                json!({"connection_id":self.clients[index].id,"request_id":id}),
                                            );
                                            json!({"v":1,"type":"response","id":id,"ok":true,"result":self.patch_result(runtime,if save{"saved"}else{"not_requested"}),"error":null})
                                        }
                                    }
                                }
                            },
                        }
                    }
                }
                _ => error(
                    "unknown_method",
                    "method is unavailable in this build",
                    json!({}),
                ),
            }
        };
        let mut response_bytes = serde_json::to_vec(&response).unwrap();
        response_bytes.push(b'\n');
        let response_bytes = response_bytes.into_boxed_slice();
        let key = id.into_boxed_str();
        let bytes = canonical.len() + response_bytes.len() + key.len();
        self.clients[index].cache_bytes += bytes;
        if self.clients[index].cache_bytes > CACHE_BYTES {
            self.clients[index].dead = true;
            self.publish(runtime, cause.unwrap_or(Value::Null));
            return;
        }
        self.clients[index].queue_bytes(response_bytes.clone());
        self.clients[index].cache.insert(
            key,
            Cached {
                request: canonical,
                response: response_bytes,
            },
        );

        self.publish(runtime, cause.unwrap_or(Value::Null));
    }
    fn report_accept_error(&mut self, error: &io::Error) {
        let now = Instant::now();
        if self
            .last_accept_error
            .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(1))
        {
            self.last_accept_error = Some(now);
            eprintln!("IPC client acceptance unavailable: {error}");
        }
    }
    pub(crate) fn poll(&mut self, runtime: &mut Runtime) -> io::Result<()> {
        self.poll_with_accept(
            runtime,
            |listener| listener.accept().map(|(s, _)| s),
            |stream| stream.set_nonblocking(true),
        )
    }
    fn poll_with_accept(
        &mut self,
        runtime: &mut Runtime,
        mut accept: impl FnMut(&UnixListener) -> io::Result<UnixStream>,
        mut configure: impl FnMut(&UnixStream) -> io::Result<()>,
    ) -> io::Result<()> {
        // Tick changes are observed before requests, so nearby operations cannot
        // steal the cause of a spontaneous tracking/stream transition.
        self.publish(runtime, Value::Null);
        for _ in 0..4 {
            match accept(&self.listener) {
                Ok(stream) => {
                    if self.clients.len() >= CLIENT_LIMIT {
                        continue;
                    }
                    if let Err(error) = configure(&stream) {
                        self.report_accept_error(&error);
                        break;
                    }
                    self.next_connection += 1;
                    self.clients.push(Client {
                        stream,
                        id: format!("p{}", self.next_connection),
                        subscribed: false,
                        input: Vec::new(),
                        output: VecDeque::new(),
                        written: 0,
                        queued: 0,
                        cache: HashMap::new(),
                        cache_bytes: 0,
                        dead: false,
                        eof: false,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                Err(e) => {
                    if matches!(
                        e.raw_os_error(),
                        Some(libc::EBADF | libc::ENOTSOCK | libc::EINVAL)
                    ) {
                        return Err(e);
                    }
                    self.report_accept_error(&e);
                    break;
                }
            }
        }
        let count = self.clients.len();
        let mut requests = REQUEST_BUDGET;
        for offset in 0..count {
            let index = (self.cursor + offset) % count;
            if self.clients[index].dead {
                continue;
            }
            if self.clients[index].receive().is_err() {
                self.clients[index].dead = true;
                continue;
            }
            // One line per client per turn, with rotating first client fairness.
            if requests > 0
                && !self.clients[index].dead
                && let Some(end) = self.clients[index].input.iter().position(|b| *b == b'\n')
            {
                requests -= 1;
                let line: Vec<_> = self.clients[index].input.drain(..=end).collect();
                match parse_json(&line[..end]) {
                    Ok(value) => self.handle(index, runtime, value),
                    Err(_) => self.clients[index].dead = true,
                }
            }
            if self.clients[index].eof && !self.clients[index].input.contains(&b'\n') {
                self.clients[index].input.clear();
                self.clients[index].subscribed = false;
            }
            if !self.clients[index].dead && self.clients[index].flush().is_err() {
                self.clients[index].dead = true;
            }
            if self.clients[index].eof
                && self.clients[index].input.is_empty()
                && self.clients[index].output.is_empty()
            {
                self.clients[index].dead = true;
            }
        }
        if count > 0 {
            self.cursor = (self.cursor + 1) % count;
        }
        self.clients.retain(|c| !c.dead);
        Ok(())
    }
}
impl Drop for IpcServer {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| (m.dev(), m.ino()) == self.identity && m.file_type().is_socket())
        {
            let _ = fs::remove_file(&self.path);
        }
        // Retain the lock inode; removing it allows a second inode to be locked
        // while a contender still owns the first descriptor.
    }
}
#[cfg(test)]
#[path = "../tests/runtime_ipc/mod.rs"]
mod tests;
