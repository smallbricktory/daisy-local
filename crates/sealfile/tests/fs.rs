// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! `sealfile::fs`: files under a session folder are sealed on write and
//! opened on read; everything else passes through to `syncsafe`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use sealfile::fs;
use sealfile::keyring;
use sealfile::ContentSecret;

/// The keyring is process-wide; these tests take turns.
fn alone() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A profile with a `sessions/<id>` folder and an unlocked keyring.
fn session(root: &Path, id: &str) -> PathBuf {
    keyring::lock();
    keyring::unlock(ContentSecret::generate().0);
    let dir = root.join("sessions").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fs_round_trips_session_file_sealed_on_disk() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-1");
    let manifest = dir.join("manifest.json");
    let summary = dir.join("summary.md");
    let nested = dir.join("chunks").join("0001").join("notes.json");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();

    fs::write(&manifest, br#"{"id":"s-1"}"#).unwrap();
    fs::write_atomic(&summary, "# Summary").unwrap();
    fs::write_atomic_synced(&nested, b"[]").unwrap();

    for (path, plain) in [(&manifest, &br#"{"id":"s-1"}"#[..]), (&summary, b"# Summary"), (&nested, b"[]")] {
        let on_disk = std::fs::read(path).unwrap();
        assert_eq!(&on_disk[..4], b"DSS1", "{}", path.display());
        assert!(!on_disk.windows(plain.len()).any(|w| w == plain), "plaintext on disk: {}", path.display());
        assert_eq!(fs::read(path).unwrap(), plain);
    }
    assert_eq!(fs::read_to_string(&summary).unwrap(), "# Summary");
    assert!(dir.join(keyring::KEY_FILE).is_file(), "the first write creates the session key");
}

#[test]
fn fs_seals_each_file_to_its_path() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-2");
    fs::write(dir.join("a.json"), b"1").unwrap();
    std::fs::copy(dir.join("a.json"), dir.join("b.json")).unwrap();
    let err = fs::read(dir.join("b.json")).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn fs_reads_legacy_plaintext() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-3");
    std::fs::write(dir.join("manifest.json"), b"{}").unwrap();
    assert_eq!(fs::read(dir.join("manifest.json")).unwrap(), b"{}");
    assert_eq!(fs::read_to_string(dir.join("manifest.json")).unwrap(), "{}");
    assert!(!dir.join(keyring::KEY_FILE).exists(), "reading plaintext needs no key");
}

#[test]
fn fs_passes_non_session_paths_through() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    session(td.path(), "s-4");
    keyring::lock();
    let settings = td.path().join("settings.json");
    fs::write_atomic(&settings, b"{}").unwrap();
    assert_eq!(std::fs::read(&settings).unwrap(), b"{}");
    assert_eq!(fs::read(&settings).unwrap(), b"{}");
    // A file directly in `sessions/` belongs to no session.
    let listing = td.path().join("sessions").join("index.json");
    fs::write(&listing, b"[]").unwrap();
    assert_eq!(std::fs::read(&listing).unwrap(), b"[]");
    // The session key file itself stays as written.
    assert!(fs::session_of(&td.path().join("sessions/s-4/key.json")).is_some());
}

#[test]
fn fs_session_of_names_the_folder_and_relative_path() {
    let root = Path::new("/p");
    let (dir, rel) = fs::session_of(&root.join("sessions/s-5/chunks/0001/a.json")).unwrap();
    assert_eq!(dir, root.join("sessions/s-5"));
    assert_eq!(rel, "chunks/0001/a.json");
    let (dir, rel) = fs::session_of(&root.join("sessions/.s-5.importing/manifest.json")).unwrap();
    assert_eq!(dir, root.join("sessions/.s-5.importing"));
    assert_eq!(rel, "manifest.json");
    assert!(fs::session_of(&root.join("sessions/s-5")).is_none());
    assert!(fs::session_of(&root.join("sessions/.trash/manifest.json")).is_none());
    assert!(fs::session_of(&root.join("settings.json")).is_none());
    assert!(fs::session_of(&root.join("sync.mac-1.inbox/sessions/f9310070/daisy-1/manifest.json")).is_none());
    let (dir, rel) = fs::session_of(&root.join("inbox/sessions/s-6/notes.md")).unwrap();
    assert_eq!((dir, rel.as_str()), (root.join("inbox/sessions/s-6"), "notes.md"));
}

#[test]
fn fs_seals_in_a_registered_profile_named_inbox() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    for profile in [td.path().join("inbox"), td.path().join("work.inbox")] {
        syncsafe::register_sessions_dir(profile.join("sessions"));
        let dir = session(&profile, "s-7");
        fs::write(dir.join("notes.md"), b"agenda").unwrap();
        assert_eq!(&std::fs::read(dir.join("notes.md")).unwrap()[..4], b"DSS1", "{}", profile.display());
        assert_eq!(fs::read(dir.join("notes.md")).unwrap(), b"agenda");
    }
}

#[test]
fn fs_locked_read_errors_but_pinned_session_reads() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let open = session(td.path(), "s-6");
    let pinned = td.path().join("sessions").join("s-7");
    std::fs::create_dir_all(&pinned).unwrap();
    fs::write(open.join("manifest.json"), b"open").unwrap();
    let pin = keyring::pin(&pinned);
    fs::write(pinned.join("manifest.json"), b"pinned").unwrap();

    keyring::lock();
    let err = fs::read(open.join("manifest.json")).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(matches!(
        err.get_ref().and_then(|e| e.downcast_ref::<sealfile::SealError>()),
        Some(sealfile::SealError::Locked)
    ));
    let err = fs::write(open.join("summary.md"), b"x").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(!open.join("summary.md").exists(), "a locked write leaves no plaintext");

    assert_eq!(fs::read(pinned.join("manifest.json")).unwrap(), b"pinned");
    fs::write(pinned.join("summary.md"), b"later").unwrap();
    assert_eq!(fs::read(pinned.join("summary.md")).unwrap(), b"later");
    drop(pin);
    assert_eq!(fs::read(pinned.join("summary.md")).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn fs_write_into_a_missing_session_is_not_found() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    session(td.path(), "s-8");
    let gone = td.path().join("sessions").join("s-9");
    let err = fs::write(gone.join("summary.md"), b"x").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert!(!gone.exists(), "a write never recreates a deleted session");
}

#[test]
fn fs_plaintext_equals_compares_plaintext() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-10");
    let path = dir.join("manifest.json");
    assert!(!fs::plaintext_equals(&path, b"{}"));
    fs::write(&path, b"{}").unwrap();
    assert!(fs::plaintext_equals(&path, b"{}"));
    assert!(!fs::plaintext_equals(&path, b"{ }"));
    std::fs::write(&path, b"{}").unwrap();
    assert!(fs::plaintext_equals(&path, b"{}"));
}

#[test]
fn fs_reseal_to_moves_a_file_into_another_session() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let from = session(td.path(), "s-11");
    let to = td.path().join("sessions").join("s-12");
    std::fs::create_dir_all(to.join("chunks/0003")).unwrap();
    std::fs::create_dir_all(from.join("chunks/0003")).unwrap();
    let plain: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    fs::write(from.join("chunks/0003/transcript.json"), &plain).unwrap();

    // A copy under the destination path does not open there.
    std::fs::copy(from.join("chunks/0003/transcript.json"), to.join("chunks/0003/transcript.json")).unwrap();
    assert!(fs::read(to.join("chunks/0003/transcript.json")).is_err());

    fs::reseal_to(&from.join("chunks/0003/transcript.json"), &to.join("chunks/0003/transcript.json")).unwrap();
    assert_eq!(fs::read(to.join("chunks/0003/transcript.json")).unwrap(), plain);
    assert_eq!(fs::read(from.join("chunks/0003/transcript.json")).unwrap(), plain, "the source stays");
    assert!(to.join(keyring::KEY_FILE).is_file());

    std::fs::write(from.join("plain.json"), b"{}").unwrap();
    let err = fs::reseal_to(&from.join("plain.json"), &to.join("plain.json")).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn fs_records_append_sealed_and_read_back() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-13");
    let log = dir.join("metrics.jsonl");
    fs::append_record(&log, br#"{"i":1}"#).unwrap();
    let mut w = fs::RecordLog::open(&log).unwrap();
    w.append(br#"{"i":2}"#).unwrap();
    drop(w);
    let on_disk = std::fs::read(&log).unwrap();
    assert!(on_disk.starts_with(b"DSR1 1\n"));
    assert!(!on_disk.windows(5).any(|w| w == br#""i":"#), "plaintext on disk");
    assert_eq!(fs::read_records(&log).unwrap(), vec![br#"{"i":1}"#.to_vec(), br#"{"i":2}"#.to_vec()]);
    assert_eq!(fs::read(&log).unwrap(), b"{\"i\":1}\n{\"i\":2}\n");
}

#[test]
fn fs_records_torn_first_line_starts_the_log_again_sealed() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-15");
    for (i, torn) in [&b"D"[..], b"DSR", b"DSR1 "].into_iter().enumerate() {
        let log = dir.join(format!("metrics-{i}.jsonl"));
        std::fs::write(&log, torn).unwrap();
        fs::append_record(&log, br#"{"i":1}"#).unwrap();
        let on_disk = std::fs::read(&log).unwrap();
        assert!(on_disk.starts_with(b"DSR1 1\n"), "{:?}", String::from_utf8_lossy(&on_disk));
        assert!(!on_disk.windows(5).any(|w| w == br#""i":"#), "plaintext on disk");
        assert_eq!(fs::read_records(&log).unwrap(), vec![br#"{"i":1}"#.to_vec()]);
    }
}

#[test]
fn fs_records_keep_a_plaintext_log_plaintext() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-14");
    let log = dir.join("live_transcript.jsonl");
    std::fs::write(&log, b"{\"a\":1}\n\n{\"a\":2").unwrap();
    fs::append_record(&log, b"{\"a\":3}").unwrap();
    assert_eq!(std::fs::read(&log).unwrap(), b"{\"a\":1}\n\n{\"a\":2\n{\"a\":3}\n", "no mixed files");
    assert_eq!(fs::read_records(&log).unwrap().len(), 3);
}

#[test]
fn fs_records_rewrite_and_continue_a_log_written_whole() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-15");
    let log = dir.join("live_transcript.jsonl");
    fs::write(&log, b"{\"a\":1}\n{\"a\":2}\n").unwrap();
    assert_eq!(fs::read_records(&log).unwrap().len(), 2);
    fs::append_record(&log, b"{\"a\":3}").unwrap();
    assert!(std::fs::read(&log).unwrap().starts_with(b"DSR1 1\n"));
    assert_eq!(fs::read_records(&log).unwrap().len(), 3);

    fs::write_records(&log, &[b"{\"b\":1}"]).unwrap();
    assert!(std::fs::read(&log).unwrap().starts_with(b"DSR1 1\n"));
    assert_eq!(fs::read_records(&log).unwrap(), vec![b"{\"b\":1}".to_vec()]);
}

#[test]
fn fs_records_locked_append_fails_and_writes_nothing() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-16");
    let log = dir.join("telemetry.jsonl");
    fs::append_record(&log, b"{}").unwrap();
    let before = std::fs::read(&log).unwrap();
    keyring::lock();
    assert_eq!(fs::append_record(&log, b"{\"x\":1}").unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(std::fs::read(&log).unwrap(), before);
}

#[test]
fn fs_records_outside_a_session_are_plain_lines() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let log = td.path().join("x.jsonl");
    fs::append_record(&log, b"{}").unwrap();
    fs::append_record(&log, b"[]").unwrap();
    assert_eq!(std::fs::read(&log).unwrap(), b"{}\n[]\n");
}

fn pcm16() -> sealfile::stream::StreamHeader {
    sealfile::stream::StreamHeader::pcm(sealfile::stream::PcmFormat { sample_rate: 16_000, channels: 1, bits_per_sample: 16 })
}

fn le(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn hound_samples<R: io::Read>(r: R) -> Vec<i16> {
    hound::WavReader::new(r).unwrap().samples::<i16>().map(|s| s.unwrap()).collect()
}

#[test]
fn fs_streams_seal_session_audio_and_read_back_as_wav() {
    use io::{Read as _, Write as _};
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-17");
    std::fs::create_dir_all(dir.join("chunks/0001")).unwrap();
    let path = dir.join("chunks/0001/mic.wav");
    let samples: Vec<i16> = (0..20_000).map(|i| (i % 2_000) as i16 - 1_000).collect();
    let mut w = fs::create_stream(&path, pcm16()).unwrap();
    assert!(w.is_sealed());
    w.write_all(&le(&samples)).unwrap();
    w.finish().unwrap();
    assert_eq!(&std::fs::read(&path).unwrap()[..4], b"DSS1");

    assert_eq!(hound_samples(fs::open_wav(&path).unwrap()), samples);
    let mut r = fs::open_stream(&path).unwrap();
    assert_eq!(r.len(), 40_000);
    assert_eq!(r.header().pcm.unwrap().sample_rate, 16_000);
    let mut bytes = Vec::new();
    r.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, le(&samples));

    // Appending continues the same stream.
    let mut w = fs::append_stream(&path, pcm16()).unwrap();
    assert_eq!(w.position(), 40_000);
    w.write_all(&le(&[7; 100])).unwrap();
    w.finish().unwrap();
    let got = hound_samples(fs::open_wav(&path).unwrap());
    assert_eq!(got.len(), 20_100);
    assert_eq!(&got[20_000..], &[7; 100]);
}

#[test]
fn fs_streams_outside_a_session_are_plain_wav_files() {
    use io::{Read as _, Write as _};
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let path = td.path().join("probe.wav");
    let mut w = fs::create_stream(&path, pcm16()).unwrap();
    assert!(!w.is_sealed());
    w.write_all(&le(&[1, 2, 3])).unwrap();
    w.flush().unwrap();
    assert_eq!(hound::WavReader::open(&path).unwrap().duration(), 3, "the header counts what was flushed");
    w.write_all(&le(&[4])).unwrap();
    w.finish().unwrap();
    assert_eq!(hound_samples(std::fs::File::open(&path).unwrap()), [1, 2, 3, 4]);

    let mut w = fs::append_stream(&path, pcm16()).unwrap();
    assert_eq!(w.position(), 8);
    w.write_all(&le(&[5])).unwrap();
    w.finish().unwrap();
    assert_eq!(hound_samples(std::fs::File::open(&path).unwrap()), [1, 2, 3, 4, 5]);

    let mut r = fs::open_stream(&path).unwrap();
    assert!(!r.is_sealed());
    assert_eq!(r.header().pcm.unwrap().channels, 1);
    let mut bytes = Vec::new();
    r.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, le(&[1, 2, 3, 4, 5]));
}

#[test]
fn fs_open_wav_passes_legacy_session_wavs_through() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-18");
    let path = dir.join("system.wav");
    let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for s in [0.5f32, -0.5, 0.25, -0.25] {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
    let r = hound::WavReader::new(fs::open_wav(&path).unwrap()).unwrap();
    assert_eq!(r.spec(), spec);
    assert_eq!(r.into_samples::<f32>().map(|s| s.unwrap()).collect::<Vec<_>>(), [0.5, -0.5, 0.25, -0.25]);
    assert_eq!(fs::open_stream(&path).unwrap().len(), 16);
    assert!(!dir.join(keyring::KEY_FILE).exists(), "reading plaintext needs no key");
}

#[test]
fn fs_streams_need_the_session_key() {
    use io::Write as _;
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-19");
    let path = dir.join("meeting.opus");
    let mut w = fs::create_stream(&path, sealfile::stream::StreamHeader::opaque()).unwrap();
    w.write_all(b"OggS").unwrap();
    w.finish().unwrap();
    keyring::lock();
    assert_eq!(fs::open_stream(&path).err().unwrap().kind(), io::ErrorKind::PermissionDenied);
    let created = fs::create_stream(dir.join("other.opus"), sealfile::stream::StreamHeader::opaque());
    assert_eq!(created.err().unwrap().kind(), io::ErrorKind::PermissionDenied);
    assert!(!dir.join("other.opus").exists(), "a locked write leaves no plaintext");
}

/// A WAV file as hound writes it.
fn hound_wav(spec: hound::WavSpec, samples: &[i16]) -> Vec<u8> {
    let mut out = io::Cursor::new(Vec::new());
    let mut w = hound::WavWriter::new(&mut out, spec).unwrap();
    for s in samples {
        w.write_sample(*s).unwrap();
    }
    w.finalize().unwrap();
    out.into_inner()
}

#[test]
fn canonical_pcm_wav_reads_back_byte_identical_through_wav_view() {
    use io::{Read as _, Write as _};
    use sealfile::wav_view::{canonical_pcm, wav_view};
    let key = sealfile::SessionKey::random();
    let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let samples: Vec<i16> = (0..20_000).map(|i| (i % 3_000) as i16 - 1_500).collect();
    let file = hound_wav(spec, &samples);

    let format = canonical_pcm(&mut io::Cursor::new(&file)).unwrap().expect("hound's 16-bit mono file is canonical");
    assert_eq!(format, sealfile::stream::PcmFormat { sample_rate: 16_000, channels: 1, bits_per_sample: 16 });
    let mut w = sealfile::stream::StreamWriter::new(
        io::Cursor::new(Vec::new()),
        &key,
        "chunks/0001/mic.wav",
        sealfile::stream::StreamHeader::pcm(format),
    )
    .unwrap();
    w.write_all(&file[44..]).unwrap();
    let sealed = w.finish().unwrap().into_inner();
    let r = sealfile::stream::StreamReader::open(io::Cursor::new(sealed), &key, "chunks/0001/mic.wav").unwrap();
    let mut back = Vec::new();
    wav_view(r).read_to_end(&mut back).unwrap();
    assert_eq!(back, file);

    // Anything a 44-byte PCM header does not describe exactly is not.
    let float = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut out = io::Cursor::new(Vec::new());
    let mut fw = hound::WavWriter::new(&mut out, float).unwrap();
    fw.write_sample(0.5f32).unwrap();
    fw.finalize().unwrap();
    assert_eq!(canonical_pcm(&mut io::Cursor::new(out.into_inner())).unwrap(), None, "float");
    let mut trailing = file.clone();
    trailing.extend_from_slice(b"LIST\x04\0\0\0abcd");
    assert_eq!(canonical_pcm(&mut io::Cursor::new(trailing)).unwrap(), None, "a chunk after the samples");
    let mut stale = file.clone();
    stale[40..44].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(canonical_pcm(&mut io::Cursor::new(stale)).unwrap(), None, "a header a crash left unpatched");
    assert_eq!(canonical_pcm(&mut io::Cursor::new(b"OggS".to_vec())).unwrap(), None, "not a WAV file");
}

#[test]
fn wav_view_passes_a_file_sealed_whole_through() {
    use io::{Read as _, Write as _};
    let key = sealfile::SessionKey::random();
    let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut file = hound_wav(spec, &[1, -1, 2, -2]);
    file.extend_from_slice(b"LIST\x04\0\0\0abcd");
    let mut w = sealfile::stream::StreamWriter::new(
        io::Cursor::new(Vec::new()),
        &key,
        "system.wav",
        sealfile::stream::StreamHeader::opaque(),
    )
    .unwrap();
    w.write_all(&file).unwrap();
    let sealed = w.finish().unwrap().into_inner();
    let r = sealfile::stream::StreamReader::open(io::Cursor::new(sealed.clone()), &key, "system.wav").unwrap();
    let mut back = Vec::new();
    sealfile::wav_view::wav_view(r).read_to_end(&mut back).unwrap();
    assert_eq!(back, file);
    let r = sealfile::stream::StreamReader::open(io::Cursor::new(sealed), &key, "system.wav").unwrap();
    assert_eq!(hound_samples(sealfile::wav_view::wav_view(r)), [1, -1, 2, -2]);
}

#[test]
fn a_session_write_waits_for_the_session_lock() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-30");
    let other = session_dir_beside(&dir, "s-31");
    let lock = fs::lock_session(&dir);
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            fs::write(dir.join("notes.md"), "after the lock").unwrap();
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        // Another session is not held up.
        fs::write(other.join("notes.md"), "elsewhere").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!done.load(std::sync::atomic::Ordering::SeqCst), "the write waits while the lock is held");
        drop(lock);
    });
    assert!(done.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(fs::read(dir.join("notes.md")).unwrap(), b"after the lock");
}

/// A second session folder beside `dir`, under the same unlocked keyring.
fn session_dir_beside(dir: &Path, id: &str) -> PathBuf {
    let other = dir.parent().unwrap().join(id);
    std::fs::create_dir_all(&other).unwrap();
    other
}

#[test]
fn a_plaintext_log_or_wav_open_for_appending_is_held_open() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session(td.path(), "s-32");
    let log = dir.join("telemetry.jsonl");
    std::fs::write(&log, b"{\"old\":1}\n").unwrap();
    let wav = dir.join("mic.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    std::fs::write(&wav, hound_wav(spec, &[1, 2, 3])).unwrap();
    let sealed = dir.join("live_transcript.jsonl");

    let mut l = fs::RecordLog::open(&log).unwrap();
    let w = fs::append_stream(&wav, pcm16()).unwrap();
    let s = fs::RecordLog::open(&sealed).unwrap();
    assert!(fs::is_held_open(&log));
    assert!(fs::is_held_open(&wav));
    assert!(!fs::is_held_open(&sealed), "a sealed log is not");
    l.append(b"{\"new\":2}").unwrap();
    drop((l, w, s));
    assert!(!fs::is_held_open(&log));
    assert!(!fs::is_held_open(&wav));
    assert_eq!(fs::read_records(&log).unwrap(), [b"{\"old\":1}".to_vec(), b"{\"new\":2}".to_vec()]);
}
