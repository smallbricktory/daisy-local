// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Runs the file operations a recording start and a model load make, one
//! step at a time, against a directory or file the caller names, and prints
//! OK or the raw OS error for each step.
//!
//!   cargo run -p audio-engine --example fs_probe -- session <scratch-dir>
//!   cargo run -p audio-engine --example fs_probe -- model <file>
//!
//! `session` builds `<scratch-dir>/probe-<pid>` and inside it two chunk
//! directories: `mixed` joined as `chunks/0001` and `native` joined one component at a time. In each it
//! opens mic.wav then system.wav with the recorder's `WavFrameWriter`, in the
//! capture thread's order, writes one second to both, closes them, re-creates
//! system.wav over the existing file, then writes and replaces a manifest the
//! way `Session` does. It removes `probe-<pid>` at the end, on failure too.
//!
//! `model` opens, stats, reads and (on Windows) memory-maps one file, through
//! the path as given and through its `\\?\` verbatim form.
//!
//! Point it at scratch space only, never at a live profile.

use audio_engine::wav::{WavFrameWriter, FRAME_SAMPLES};
use std::io::Read;
use std::path::{Path, PathBuf};

fn report<T>(step: &str, r: std::io::Result<T>) -> Option<T> {
    match r {
        Ok(v) => {
            println!("OK   {step}");
            Some(v)
        }
        Err(e) => {
            println!(
                "ERR  {step}: {e} (kind={:?}, raw_os_error={:?})",
                e.kind(),
                e.raw_os_error()
            );
            None
        }
    }
}

fn wav_err(e: audio_engine::error::Error) -> std::io::Error {
    match e {
        audio_engine::error::Error::Wav(hound::Error::IoError(io)) => io,
        other => std::io::Error::other(other.to_string()),
    }
}

fn open_writer(path: &Path) -> std::io::Result<WavFrameWriter> {
    WavFrameWriter::create(path, 16_000).map_err(wav_err)
}

fn one_second(w: &mut WavFrameWriter) -> std::io::Result<()> {
    let frame = [0i16; FRAME_SAMPLES];
    for _ in 0..50 {
        w.write_frame(&frame).map_err(wav_err)?;
    }
    Ok(())
}

fn size_of(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(m) => format!("{} bytes", m.len()),
        Err(e) => format!("stat failed: {e} (raw_os_error={:?})", e.raw_os_error()),
    }
}

fn chunk_steps(label: &str, dir: &Path) {
    println!("--- {label}: {}", dir.display());
    if report(&format!("{label}: create_dir_all"), std::fs::create_dir_all(dir)).is_none() {
        return;
    }
    let mic = dir.join("mic.wav");
    let system = dir.join("system.wav");
    let mut mic_w = report(&format!("{label}: create mic.wav"), open_writer(&mic));
    let mut sys_w = report(&format!("{label}: create system.wav"), open_writer(&system));
    println!("     after create: mic.wav {}, system.wav {}", size_of(&mic), size_of(&system));
    if let Some(w) = mic_w.as_mut() {
        report(&format!("{label}: write 1 s to mic.wav"), one_second(w));
    }
    if let Some(w) = sys_w.as_mut() {
        report(&format!("{label}: write 1 s to system.wav"), one_second(w));
    }
    if let Some(w) = mic_w {
        report(&format!("{label}: close mic.wav"), w.close().map_err(wav_err));
    }
    if let Some(w) = sys_w {
        report(&format!("{label}: close system.wav"), w.close().map_err(wav_err));
    }
    println!("     after close: mic.wav {}, system.wav {}", size_of(&mic), size_of(&system));
    // The next chunk rotation, or a retried start, re-creates over a file that exists.
    if let Some(w) = report(&format!("{label}: re-create existing system.wav"), open_writer(&system)) {
        report(&format!("{label}: close re-created system.wav"), w.close().map_err(wav_err));
    }
}

fn session(scratch: &Path) {
    let root = scratch.join(format!("probe-{}", std::process::id()));
    println!("probe root: {}", root.display());
    if report("create probe root", std::fs::create_dir_all(&root)).is_some() {
        chunk_steps("mixed", &root.join("mixed").join("chunks/0001"));
        chunk_steps("native", &root.join("native").join("chunks").join("0001"));
        let manifest = root.join("manifest.json");
        report("manifest: first write (write tmp, fsync, rename)", syncsafe::write_atomic_synced(&manifest, b"{\"v\":1}"));
        report("manifest: replace existing (write tmp, fsync, rename over)", syncsafe::write_atomic_synced(&manifest, b"{\"v\":2}"));
        report("manifest: read back", std::fs::read_to_string(&manifest));
        report("heartbeat: fs::write", std::fs::write(root.join("heartbeat"), b"1"));
    }
    report("remove probe root", std::fs::remove_dir_all(&root));
    println!("probe root exists after cleanup: {}", root.exists());
}

fn verbatim(path: &Path) -> Option<PathBuf> {
    let s = path.to_str()?;
    if s.starts_with(r"\\?\") || !cfg!(windows) {
        return None;
    }
    let abs = std::path::absolute(path).ok()?;
    Some(PathBuf::from(format!(r"\\?\{}", abs.display())))
}

fn model_steps(label: &str, path: &Path) {
    println!("--- {label}: {}", path.display());
    println!("     exists()={} is_file()={}", path.exists(), path.is_file());
    report(&format!("{label}: fs::metadata"), std::fs::metadata(path).map(|m| m.len()));
    if let Some(mut f) = report(&format!("{label}: File::open"), std::fs::File::open(path)) {
        report(&format!("{label}: File::metadata"), f.metadata().map(|m| m.len()));
        let mut head = vec![0u8; 64 * 1024];
        report(&format!("{label}: read first 64 KiB"), f.read(&mut head));
    }
    report(&format!("{label}: fs::read (whole file)"), std::fs::read(path).map(|b| b.len()));
    #[cfg(windows)]
    win::steps(label, path);
}

fn model(path: &Path) {
    model_steps("as given", path);
    if let Some(v) = verbatim(path) {
        model_steps("verbatim", &v);
    }
}

#[cfg(windows)]
mod win {
    use super::report;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileAttributesExW, GetFileExInfoStandard, FILE_ATTRIBUTE_NORMAL,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        WIN32_FILE_ATTRIBUTE_DATA,
    };
    use windows::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, PAGE_READONLY,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }

    fn io<T>(r: windows::core::Result<T>) -> std::io::Result<T> {
        r.map_err(|e| std::io::Error::from_raw_os_error((e.code().0 & 0xFFFF) as i32))
    }

    pub fn steps(label: &str, path: &Path) {
        let w = wide(path);
        let mut data = WIN32_FILE_ATTRIBUTE_DATA::default();
        // What std::filesystem::file_size (onnxruntime's external-data loader) calls.
        report(
            &format!("{label}: GetFileAttributesExW"),
            io(unsafe {
                GetFileAttributesExW(PCWSTR(w.as_ptr()), GetFileExInfoStandard, &mut data as *mut _ as *mut _)
            })
            .map(|_| ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64),
        );
        let Some(h) = report(
            &format!("{label}: CreateFileW(GENERIC_READ, share all)"),
            io(unsafe {
                CreateFileW(
                    PCWSTR(w.as_ptr()),
                    GENERIC_READ.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            }),
        ) else {
            return;
        };
        if let Some(map) = report(
            &format!("{label}: CreateFileMappingW(PAGE_READONLY)"),
            io(unsafe { CreateFileMappingW(h, None, PAGE_READONLY, 0, 0, PCWSTR::null()) }),
        ) {
            let view = unsafe { MapViewOfFile(map, FILE_MAP_READ, 0, 0, 0) };
            if view.Value.is_null() {
                report::<()>(&format!("{label}: MapViewOfFile"), Err(std::io::Error::last_os_error()));
            } else {
                let first = unsafe { *(view.Value as *const u8) };
                report(&format!("{label}: MapViewOfFile (first byte {first:#04x})"), Ok(()));
                let _ = unsafe { UnmapViewOfFile(view) };
            }
            let _ = unsafe { CloseHandle(map) };
        }
        let _ = unsafe { CloseHandle(h) };
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match (args.get(1).map(String::as_str), args.get(2)) {
        (Some("session"), Some(dir)) => session(Path::new(dir)),
        (Some("model"), Some(file)) => model(Path::new(file)),
        _ => {
            eprintln!("usage: fs_probe session <scratch-dir> | fs_probe model <file>");
            std::process::exit(2);
        }
    }
}
