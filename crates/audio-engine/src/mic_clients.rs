// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Which other processes hold the microphone open right now.

/// Another process capturing the microphone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicClient {
    pub pid: u32,
    pub name: String,
}

/// Whether this build on this machine can answer [`mic_clients`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicClientsSupport {
    Yes,
    No(&'static str),
}

/// Whether the microphone-client question can be answered here.
pub fn mic_clients_support() -> MicClientsSupport {
    #[cfg(target_os = "macos")]
    {
        crate::macos::mic_clients_support()
    }
    #[cfg(target_os = "windows")]
    {
        MicClientsSupport::Yes
    }
    #[cfg(target_os = "linux")]
    {
        if crate::topology_linux::pipewire_up() {
            MicClientsSupport::Yes
        } else {
            MicClientsSupport::No("needs PipeWire")
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        MicClientsSupport::No("not supported on this system")
    }
}

/// Other processes holding the microphone open. Our own process and every
/// process it started, directly or through further descendants, are
/// excluded. Empty where the question cannot be answered.
pub fn mic_clients() -> Vec<MicClient> {
    let clients = os_mic_clients();
    if clients.is_empty() {
        return clients;
    }
    let own = std::process::id();
    let info = process_table();
    clients
        .into_iter()
        .filter(|c| !in_process_tree(c.pid, own, &info))
        .collect()
}

fn os_mic_clients() -> Vec<MicClient> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::mic_clients()
    }
    #[cfg(target_os = "windows")]
    {
        crate::wasapi::sessions::mic_clients()
    }
    #[cfg(target_os = "linux")]
    {
        crate::topology_linux::mic_clients()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Vec::new()
    }
}

/// A process's parent pid and start time. Start times compare only with
/// other start times from the same OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcInfo {
    pub ppid: u32,
    pub started: u64,
}

/// Looks up [`ProcInfo`] for any pid on this machine.
fn process_table() -> impl Fn(u32) -> Option<ProcInfo> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::proc_info
    }
    #[cfg(target_os = "windows")]
    {
        crate::wasapi::sessions::process_table()
    }
    #[cfg(target_os = "linux")]
    {
        |pid: u32| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            proc_info_from_stat(&stat)
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        |_: u32| None
    }
}

/// Whether `pid` is `root` or a descendant of it. A parent link counts only
/// when the parent started no later than the child, so a recycled parent
/// pid is not an ancestor. A pid with no readable info ends the walk.
pub fn in_process_tree(pid: u32, root: u32, info: impl Fn(u32) -> Option<ProcInfo>) -> bool {
    let mut cur = pid;
    for _ in 0..64 {
        if cur == root {
            return true;
        }
        let Some(child) = info(cur) else { return false };
        if child.ppid == 0 || child.ppid == cur {
            return false;
        }
        let Some(parent) = info(child.ppid) else { return false };
        if parent.started > child.started {
            return false;
        }
        cur = child.ppid;
    }
    false
}

/// Parent pid and start time (clock ticks since boot) from the contents of
/// Linux `/proc/<pid>/stat`.
#[cfg(any(target_os = "linux", test))]
fn proc_info_from_stat(stat: &str) -> Option<ProcInfo> {
    let (_, rest) = stat.rsplit_once(')')?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some(ProcInfo {
        ppid: fields.get(1)?.parse().ok()?,
        started: fields.get(19)?.parse().ok()?,
    })
}

/// A PipeWire `Stream/Input/Audio` node's props as a mic client. A node that
/// captures a sink's monitor, a node with no usable `application.process.id`,
/// and our own process are not clients.
pub fn mic_client_from_stream_props(
    capture_sink: Option<&str>,
    process_id: Option<&str>,
    app_name: Option<&str>,
    own_pid: u32,
) -> Option<MicClient> {
    if capture_sink.is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1") {
        return None;
    }
    let pid: u32 = process_id?.trim().parse().ok()?;
    if pid == own_pid {
        return None;
    }
    let name = app_name.map(str::trim).filter(|n| !n.is_empty());
    Some(MicClient {
        pid,
        name: name.map(str::to_string).unwrap_or_else(|| format!("pid {pid}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &[(u32, u32, u64)]) -> impl Fn(u32) -> Option<ProcInfo> + '_ {
        move |pid| {
            rows.iter()
                .find(|r| r.0 == pid)
                .map(|&(_, ppid, started)| ProcInfo { ppid, started })
        }
    }

    // pid, ppid, started. 100 is the app; 200 its webview host; 300 the
    // webview's audio process; 900 an unrelated meeting app.
    const TREE: &[(u32, u32, u64)] = &[
        (1, 0, 0),
        (50, 1, 5),
        (100, 50, 10),
        (200, 100, 11),
        (300, 200, 12),
        (900, 50, 20),
    ];

    #[test]
    fn the_app_itself_is_in_its_tree() {
        assert!(in_process_tree(100, 100, table(TREE)));
    }

    #[test]
    fn a_grandchild_audio_process_is_in_the_app_tree() {
        assert!(in_process_tree(200, 100, table(TREE)));
        assert!(in_process_tree(300, 100, table(TREE)));
    }

    #[test]
    fn a_sibling_and_an_ancestor_are_not_in_the_app_tree() {
        assert!(!in_process_tree(900, 100, table(TREE)));
        assert!(!in_process_tree(50, 100, table(TREE)));
        assert!(!in_process_tree(1, 100, table(TREE)));
    }

    #[test]
    fn a_recycled_parent_pid_is_not_an_ancestor() {
        // 700 names 100 as its parent but started before 100 did.
        let rows = [(50, 1, 5), (100, 50, 10), (700, 100, 8)];
        assert!(!in_process_tree(700, 100, table(&rows)));
    }

    #[test]
    fn an_unreadable_process_is_not_in_the_tree() {
        assert!(!in_process_tree(4242, 100, table(TREE)));
    }

    #[test]
    fn a_parent_loop_ends_the_walk() {
        let rows = [(100, 50, 10), (5, 6, 1), (6, 5, 1)];
        assert!(!in_process_tree(5, 100, table(&rows)));
    }

    #[test]
    fn linux_stat_gives_parent_and_start_time() {
        let stat = "4242 (Web Content (x)) S 4200 4242 4200 0 -1 4194560 100 0 0 0 \
                    5 3 0 0 20 0 12 0 987654 123456789 1000 18446744073709551615";
        assert_eq!(
            proc_info_from_stat(stat),
            Some(ProcInfo { ppid: 4200, started: 987654 })
        );
        assert_eq!(proc_info_from_stat("garbage"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_process_we_spawn_is_in_our_tree_and_launchd_or_init_is_not() {
        let own = std::process::id();
        let info = process_table();
        let mut child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        let in_tree = in_process_tree(child.id(), own, &info);
        let _ = child.kill();
        let _ = child.wait();
        assert!(in_tree);
        assert!(in_process_tree(own, own, &info));
        assert!(!in_process_tree(1, own, &info));
    }

    #[test]
    fn a_monitor_capture_is_not_a_mic_client() {
        assert_eq!(
            mic_client_from_stream_props(Some("true"), Some("4242"), Some("OBS"), 7),
            None
        );
    }

    #[test]
    fn our_own_capture_is_not_a_mic_client() {
        assert_eq!(mic_client_from_stream_props(None, Some("7"), Some("Daisy"), 7), None);
    }

    #[test]
    fn a_stream_without_a_process_id_is_not_a_mic_client() {
        assert_eq!(mic_client_from_stream_props(None, None, Some("Zoom"), 7), None);
        assert_eq!(mic_client_from_stream_props(None, Some("abc"), Some("Zoom"), 7), None);
    }

    #[test]
    fn a_named_capture_stream_is_a_mic_client() {
        assert_eq!(
            mic_client_from_stream_props(Some("false"), Some("4242"), Some("Zoom"), 7),
            Some(MicClient { pid: 4242, name: "Zoom".into() })
        );
    }

    #[test]
    fn an_unnamed_capture_stream_reads_as_its_pid() {
        assert_eq!(
            mic_client_from_stream_props(None, Some("4242"), Some("   "), 7),
            Some(MicClient { pid: 4242, name: "pid 4242".into() })
        );
        assert_eq!(
            mic_client_from_stream_props(None, Some("4242"), None, 7),
            Some(MicClient { pid: 4242, name: "pid 4242".into() })
        );
    }
}
