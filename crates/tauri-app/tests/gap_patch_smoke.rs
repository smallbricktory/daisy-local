//! Opt-in smoke test for the gap-patch whisper I/O against a REAL session.
//! Inert in CI (whisper inference + a GGML model can't run in the sandbox); run
//! explicitly on a machine that has both:
//!
//!   DAISY_GAP_SMOKE_SESSION=/path/to/sessions/daisy-XXXX \
//!   DAISY_GAP_SMOKE_MODEL=/path/to/ggml-base.en.bin \
//!   cargo test -p tauri-app --test gap_patch_smoke -- --ignored --nocapture
//!
//! It copies the session to a tempdir (never mutates the original), runs
//! patch_gaps_on_disk over the known incident gap, and asserts speech was
//! recovered into the system track.

use std::path::{Path, PathBuf};
use tauri_app_core::commands::gap_patch::patch_gaps_on_disk;
use transcript::model::{SessionTranscript, Track};
use transcript::promote::ChunkSpan;

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

#[test]
#[ignore = "needs a real session + GGML model; set DAISY_GAP_SMOKE_SESSION / DAISY_GAP_SMOKE_MODEL"]
fn recovers_real_gap_speech() {
    let (Some(session), Some(model)) = (
        std::env::var_os("DAISY_GAP_SMOKE_SESSION"),
        std::env::var_os("DAISY_GAP_SMOKE_MODEL"),
    ) else {
        eprintln!("skipping: set DAISY_GAP_SMOKE_SESSION and DAISY_GAP_SMOKE_MODEL");
        return;
    };
    let src = PathBuf::from(session);
    let model = PathBuf::from(model);

    let tmp = std::env::temp_dir().join("daisy_gap_smoke");
    let _ = std::fs::remove_dir_all(&tmp);
    copy_dir(&src, &tmp);

    // The gap: 326.8s–342.1s, entirely within chunk 1 ([0, 343s)).
    let gaps = vec![(326_800u32, 342_100u32)];
    let spans = vec![ChunkSpan {
        index: 1,
        start_ms: 0,
        mic_track: Track::MicAec,
        mic_wav: "chunks/0001/mic_aec.wav".into(),
        system_wav: "chunks/0001/system.wav".into(),
    }];

    let recovered = patch_gaps_on_disk(&tmp, &model, &gaps, &spans).expect("patch ran");
    eprintln!("recovered {recovered} segment(s)");
    assert!(recovered > 0, "expected to recover far-end speech from the gap");

    // Confirm recovered text landed in the system track around the gap window.
    let tj: SessionTranscript =
        serde_json::from_slice(&std::fs::read(tmp.join("transcript.json")).unwrap()).unwrap();
    let c1 = tj.chunks.iter().find(|c| c.chunk_index == 1).unwrap();
    let sys = c1.tracks.iter().find(|t| t.track == Track::System).unwrap();
    let in_gap: String = sys
        .segments
        .iter()
        .filter(|s| s.start_ms >= 320_000 && s.start_ms <= 345_000)
        .map(|s| s.text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!("system text in gap window: {in_gap}");
    assert!(!in_gap.trim().is_empty(), "system track gap window should now have text");
}
