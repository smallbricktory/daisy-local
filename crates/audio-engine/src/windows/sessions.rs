// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Processes holding a capture endpoint's audio session open.

use crate::mic_clients::{MicClient, ProcInfo};
use std::collections::HashMap;
use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, FILETIME, MAX_PATH, S_OK};
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, eConsole, AudioSessionStateActive, IAudioSessionControl2,
    IAudioSessionManager2, IMMDevice,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Other processes with an active session on a capture endpoint. Runs on a
/// short-lived thread that initializes COM itself.
pub(crate) fn mic_clients() -> Vec<MicClient> {
    std::thread::spawn(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let clients = collect();
            CoUninitialize();
            clients
        }
    })
    .join()
    .unwrap_or_default()
}

unsafe fn collect() -> Vec<MicClient> {
    let own = std::process::id();
    let mut out: Vec<MicClient> = Vec::new();
    let Ok(enumerator) = super::create_enumerator() else {
        return out;
    };
    for role in [eConsole, eCommunications] {
        let Ok(device) = enumerator.GetDefaultAudioEndpoint(eCapture, role) else {
            continue;
        };
        for pid in session_pids(&device) {
            if pid == 0 || pid == own || out.iter().any(|c| c.pid == pid) {
                continue;
            }
            out.push(MicClient { pid, name: process_name(pid) });
        }
    }
    out
}

unsafe fn session_pids(device: &IMMDevice) -> Vec<u32> {
    let mut pids = Vec::new();
    let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
        return pids;
    };
    let Ok(sessions) = manager.GetSessionEnumerator() else {
        return pids;
    };
    let Ok(count) = sessions.GetCount() else {
        return pids;
    };
    for i in 0..count {
        let Ok(control) = sessions.GetSession(i) else { continue };
        if control.GetState().ok() != Some(AudioSessionStateActive) {
            continue;
        }
        let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
            continue;
        };
        if control2.IsSystemSoundsSession() == S_OK {
            continue;
        }
        if let Ok(pid) = control2.GetProcessId() {
            pids.push(pid);
        }
    }
    pids
}

/// Parent pids from one process snapshot, with start times (FILETIME ticks)
/// read on demand.
pub(crate) fn process_table() -> impl Fn(u32) -> Option<ProcInfo> {
    let parents = unsafe { parent_pids() };
    move |pid| {
        let ppid = *parents.get(&pid)?;
        let started = unsafe { start_time(pid) }?;
        Some(ProcInfo { ppid, started })
    }
}

unsafe fn parent_pids() -> HashMap<u32, u32> {
    let mut out = HashMap::new();
    let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
        return out;
    };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    if Process32FirstW(snapshot, &mut entry).is_ok() {
        loop {
            out.insert(entry.th32ProcessID, entry.th32ParentProcessID);
            if Process32NextW(snapshot, &mut entry).is_err() {
                break;
            }
        }
    }
    let _ = CloseHandle(snapshot);
    out
}

unsafe fn start_time(pid: u32) -> Option<u64> {
    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut created = FILETIME::default();
    let (mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default());
    let read = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
    let _ = CloseHandle(handle);
    read.ok()?;
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

/// Executable file name of a pid, or `pid <n>` when it is unreadable.
unsafe fn process_name(pid: u32) -> String {
    let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
        return format!("pid {pid}");
    };
    let mut buf = [0u16; MAX_PATH as usize];
    let mut len = buf.len() as u32;
    let read = QueryFullProcessImageNameW(
        handle,
        PROCESS_NAME_WIN32,
        windows::core::PWSTR(buf.as_mut_ptr()),
        &mut len,
    );
    let _ = CloseHandle(handle);
    if read.is_err() {
        return format!("pid {pid}");
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit(['\\', '/'])
        .next()
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("pid {pid}"))
}
