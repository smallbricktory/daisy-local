//! Replay a session's raw chunk WAVs through the live-AEC path (zero-lag,
//! continuous LSTM state, exactly as the aec_bridge processes the stream)
//! and write the result for offline echo measurement.
//!
//! Usage: live_aec_replay <session-dir> <out.wav>

use aec::echo_canceller::AcousticEchoCanceller;

fn read_wav(p: &std::path::Path) -> (Vec<i16>, u32) {
    let mut r = hound::WavReader::open(p).expect("open wav");
    let sr = r.spec().sample_rate;
    (r.samples::<i16>().map(|s| s.unwrap()).collect(), sr)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("session dir"));
    let out = std::path::PathBuf::from(args.next().expect("out wav"));

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    let mut canceller = AcousticEchoCanceller::load(&aec::constants::model_dir()).expect("load model");
    const FRAME: usize = AcousticEchoCanceller::FRAME_SIZE;

    // Optional fixed reference advance (ms): models the acoustic delay so the
    // far signal lines up with its echo in the mic.
    let align_ms: usize = args.next().map(|a| a.parse().unwrap()).unwrap_or(0);

    let mut cleaned: Vec<i16> = Vec::new();
    let mut sample_rate = 16_000;
    for c in manifest["chunks"].as_array().unwrap() {
        let (mic, sr) = read_wav(&dir.join(c["mic_wav_relative"].as_str().unwrap()));
        let (sys_raw, _) = read_wav(&dir.join(c["system_wav_relative"].as_str().unwrap()));
        // Advance the reference: prepend silence to the mic side equivalently
        // by delaying mic, i.e. drop the first align samples of nothing —
        // implement as sys shifted right by align (sys[t-align] pairs mic[t]).
        let align = align_ms * sr as usize / 1000;
        let mut sys = vec![0i16; align];
        sys.extend(&sys_raw);
        sample_rate = sr;
        let n = mic.len().min(sys.len());
        let mut at = 0;
        while at + FRAME <= n {
            let clean = canceller
                .process(&mic[at..at + FRAME], &sys[at..at + FRAME])
                .expect("process");
            cleaned.extend(clean);
            at += FRAME;
        }
        cleaned.extend(&mic[at..n]); // trailing partial frame raw
    }

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&out, spec).unwrap();
    for s in &cleaned {
        w.write_sample(*s).unwrap();
    }
    w.finalize().unwrap();
    println!("wrote {} samples to {}", cleaned.len(), out.display());
}
