// Runs a WeSpeaker encode on a session WAV and prints the result.
fn main() {
    let wav = std::env::args().nth(1).unwrap();
    let mut r = hound::WavReader::open(&wav).unwrap();
    let sr = r.spec().sample_rate;
    let pcm: Vec<i16> = r.samples::<i16>().take((sr as usize) * 6).map(|s| s.unwrap()).collect();
    println!("read {} samples @ {sr}", pcm.len());
    let dir = voiceprints::model_dir();
    println!("model dir: {dir:?} exists={}", dir.exists());
    let mut enc = voiceprints::Encoder::load().unwrap();
    // take a 4s window starting 1s in
    let w = &pcm[(sr as usize)..(sr as usize) * 5];
    match enc.encode_pcm(w) {
        Ok(v) => println!("ENCODE OK dim={} norm={:.4} first5={:?}", v.len(),
            v.iter().map(|x| x * x).sum::<f32>().sqrt(), &v[..5]),
        Err(e) => println!("ENCODE ERR: {e}"),
    }
}
