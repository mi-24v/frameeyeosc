use std::{
    collections::BTreeSet,
    fs,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamState {
    Detected,
    Waiting,
    Ambiguous,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamObservation {
    pub state: StreamState,
    pub peer: Option<SocketAddr>,
}
impl Default for StreamObservation {
    fn default() -> Self {
        Self {
            state: StreamState::Waiting,
            peer: None,
        }
    }
}
fn parse_address(value: &str, ipv6: bool) -> Option<SocketAddr> {
    let (host, port) = value.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    if port == 0 {
        return None;
    }
    let ip = if ipv6 {
        if host.len() != 32 {
            return None;
        }
        let mut bytes = [0; 16];
        for index in 0..4 {
            bytes[index * 4..index * 4 + 4].copy_from_slice(
                &u32::from_str_radix(&host[index * 8..index * 8 + 8], 16)
                    .ok()?
                    .to_le_bytes(),
            );
        }
        let address = Ipv6Addr::from(bytes);
        if address.is_unicast_link_local() {
            return None;
        }
        if let Some(ipv4) = address.to_ipv4_mapped() {
            IpAddr::V4(ipv4)
        } else {
            IpAddr::V6(address)
        }
    } else {
        IpAddr::V4(Ipv4Addr::from(
            u32::from_str_radix(host, 16).ok()?.to_le_bytes(),
        ))
    };
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return None;
    }
    Some(SocketAddr::new(ip, port))
}
fn parse_row(row: &str, ipv6: bool, owned: &[String]) -> Option<SocketAddr> {
    let columns: Vec<_> = row.split_whitespace().collect();
    if columns.len() < 10 || columns[3] != "01" || !owned.iter().any(|inode| inode == columns[9]) {
        return None;
    }
    parse_address(columns[2], ipv6)
}
pub(crate) fn detect_stream(proc_root: &Path) -> StreamObservation {
    detect_stream_with_observation(proc_root, |_| {})
}
fn detect_stream_with_observation(
    proc_root: &Path,
    mut after_tables: impl FnMut(&Path),
) -> StreamObservation {
    let Ok(entries) = fs::read_dir(proc_root) else {
        return StreamObservation::default();
    };
    let mut peers = BTreeSet::new();
    let mut raced = false;
    for entry in entries {
        let Ok(entry) = entry else {
            raced = true;
            continue;
        };
        if !entry
            .file_name()
            .to_string_lossy()
            .chars()
            .all(|c| c.is_ascii_digit())
        {
            continue;
        }
        let path = entry.path();
        let Ok(exe) = fs::read_link(path.join("exe")) else {
            continue;
        };
        if exe
            .file_name()
            .is_none_or(|name| name.to_string_lossy().trim_end_matches(" (deleted)") != "vrlink")
        {
            continue;
        }
        let before = fs::read_link(path.join("ns/net"));
        let Ok(namespace) = before else {
            raced = true;
            continue;
        };
        let Ok(fds) = fs::read_dir(path.join("fd")) else {
            raced = true;
            continue;
        };
        let mut owned = Vec::new();
        for fd in fds {
            let Ok(fd) = fd else {
                raced = true;
                continue;
            };
            let link = match fs::read_link(fd.path()) {
                Ok(link) => link,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    raced = true;
                    continue;
                }
            };
            let text = link.to_string_lossy();
            if let Some(inode) = text
                .strip_prefix("socket:[")
                .and_then(|s| s.strip_suffix(']'))
            {
                owned.push(inode.to_string());
            }
        }
        let mut process_peers = Vec::new();
        for (name, ipv6) in [("udp", false), ("udp6", true)] {
            let table = match fs::read_to_string(path.join("net").join(name)) {
                Ok(table) => table,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    raced = true;
                    continue;
                }
            };
            process_peers.extend(table.lines().filter_map(|row| {
                let peer = parse_row(row, ipv6, &owned)?;
                Some((peer, row.split_whitespace().nth(9)?.to_owned()))
            }));
        }
        after_tables(&path);
        if fs::read_link(path.join("exe")).ok() != Some(exe)
            || fs::read_link(path.join("ns/net")).ok() != Some(namespace)
        {
            raced = true;
            continue;
        }
        // Verify ownership again so a disappearing socket cannot leave a stale destination.
        let Ok(fds) = fs::read_dir(path.join("fd")) else {
            raced = true;
            continue;
        };
        let current: BTreeSet<_> = fds
            .filter_map(Result::ok)
            .filter_map(|fd| fs::read_link(fd.path()).ok())
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        if process_peers
            .iter()
            .any(|(_, inode)| !current.contains(&format!("socket:[{inode}]")))
        {
            raced = true;
            continue;
        }
        peers.extend(process_peers.into_iter().map(|(peer, _)| peer));
    }
    if raced {
        return StreamObservation::default();
    }
    let ips: BTreeSet<_> = peers.iter().map(SocketAddr::ip).collect();
    if ips.len() > 1 {
        StreamObservation {
            state: StreamState::Ambiguous,
            peer: None,
        }
    } else if let Some(peer) = peers.first() {
        StreamObservation {
            state: StreamState::Detected,
            peer: Some(*peer),
        }
    } else {
        StreamObservation::default()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_peer_and_owned_inode() {
        let row = include_str!("../docs/research/fixtures/steamlink-adapter-v4/udp-streaming.txt")
            .lines()
            .nth(1)
            .unwrap();
        assert_eq!(
            parse_row(row, false, &["159857".to_string()])
                .unwrap()
                .to_string(),
            "192.0.2.20:10400"
        );
        assert!(parse_row(row, false, &["1".to_string()]).is_none());
    }
    #[test]
    fn ipv4_mapped_loopback_is_not_a_stream_peer() {
        assert!(parse_address("0000000000000000FFFF00000100007F:28A0", true).is_none());
        assert!(parse_address("0000000000000000FFFF000000000000:28A0", true).is_none());
    }
    #[test]
    fn ipv6_scope_and_loopback() {
        assert!(parse_address("0100007F:28A0", false).is_none());
        assert!(parse_address("00000000:28A0", false).is_none());
        assert!(parse_address("000080FE000000000000000001000000:28A0", true).is_none());
        assert!(parse_address("B80D0120000000000000000001000000:28A0", true).is_some());
    }
}
#[cfg(test)]
pub(crate) mod detector_tests {
    use super::*;
    use crate::input::fixtures::Fixture;
    use std::os::unix::fs::symlink;
    pub(crate) fn process(root: &Path, pid: &str, inode: &str, row: &str) {
        let path = root.join(pid);
        fs::create_dir_all(path.join("fd")).unwrap();
        fs::create_dir_all(path.join("ns")).unwrap();
        fs::create_dir_all(path.join("net")).unwrap();
        symlink(
            "/opt/steamvr/tools/vrlink/bin/linuxarm64/vrlink",
            path.join("exe"),
        )
        .unwrap();
        symlink("net:[4026531833]", path.join("ns/net")).unwrap();
        symlink(format!("socket:[{inode}]"), path.join("fd/80")).unwrap();
        fs::write(path.join("net/udp"), row).unwrap();
        fs::write(path.join("net/udp6"), "").unwrap();
    }
    #[test]
    fn unrelated_socket_churn_keeps_peer() {
        let fixture = Fixture::new(4);
        let row = include_str!("../docs/research/fixtures/steamlink-adapter-v4/udp-streaming.txt");
        process(&fixture.dir, "1", "159857", row);
        symlink("socket:[99]", fixture.dir.join("1/fd/81")).unwrap();
        let observation = detect_stream_with_observation(&fixture.dir, |path| {
            fs::remove_file(path.join("fd/81")).unwrap();
        });
        assert_eq!(observation.state, StreamState::Detected);
        let observation = detect_stream_with_observation(&fixture.dir, |path| {
            fs::remove_file(path.join("fd/80")).unwrap();
        });
        assert_eq!(observation.state, StreamState::Waiting);
    }
    #[test]
    fn ipv4_stream_survives_absent_udp6_and_deleted_executable() {
        let fixture = Fixture::new(4);
        let row = include_str!("../docs/research/fixtures/steamlink-adapter-v4/udp-streaming.txt");
        process(&fixture.dir, "1", "159857", row);
        fs::remove_file(fixture.dir.join("1/net/udp6")).unwrap();
        assert_eq!(detect_stream(&fixture.dir).state, StreamState::Detected);
        fs::remove_file(fixture.dir.join("1/exe")).unwrap();
        symlink("/opt/vrlink (deleted)", fixture.dir.join("1/exe")).unwrap();
        assert_eq!(detect_stream(&fixture.dir).state, StreamState::Detected);
    }
    #[test]
    fn stream_stop_restart_and_other_process() {
        let fixture = Fixture::new(4);
        let row = include_str!("../docs/research/fixtures/steamlink-adapter-v4/udp-streaming.txt");
        process(&fixture.dir, "9272", "159857", row);
        let found = detect_stream(&fixture.dir);
        assert_eq!(found.peer.unwrap().to_string(), "192.0.2.20:10400");
        fs::remove_dir_all(fixture.dir.join("9272")).unwrap();
        assert_eq!(detect_stream(&fixture.dir).state, StreamState::Waiting);
        let row = include_str!("../docs/research/fixtures/steamlink-adapter-v4/udp-restarted.txt");
        process(&fixture.dir, "9683", "160449", row);
        assert_eq!(detect_stream(&fixture.dir).peer, found.peer);
        fs::remove_file(fixture.dir.join("9683/exe")).unwrap();
        symlink("/opt/steam", fixture.dir.join("9683/exe")).unwrap();
        assert_eq!(detect_stream(&fixture.dir).state, StreamState::Waiting);
    }
    #[test]
    fn ambiguous_ip_and_proc_race_wait() {
        let fixture = Fixture::new(4);
        let row = include_str!("../docs/research/fixtures/steamlink-adapter-v4/udp-streaming.txt");
        process(&fixture.dir, "1", "159857", row);
        process(
            &fixture.dir,
            "2",
            "159857",
            &row.replace("140200C0", "150200C0"),
        );
        assert_eq!(detect_stream(&fixture.dir).state, StreamState::Ambiguous);
        fs::remove_file(fixture.dir.join("2/ns/net")).unwrap();
        assert_eq!(detect_stream(&fixture.dir).state, StreamState::Waiting);
    }
}
