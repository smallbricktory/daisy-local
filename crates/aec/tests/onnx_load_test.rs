use aec::constants::model_dir;
use aec::onnx::{Stage1Session, Stage2Session};

#[test]
fn stage1_loads_when_model_present() {
    let dir = model_dir();
    let path = dir.join("model_256_1.onnx");
    if !path.exists() {
        eprintln!("skipping: {} absent", path.display());
        return;
    }
    let session = Stage1Session::load(&path).expect("load stage1");
    assert_eq!(session.input_count(), 3);
    assert_eq!(session.output_count(), 2);
}

#[test]
fn stage2_loads_when_model_present() {
    let dir = model_dir();
    let path = dir.join("model_256_2.onnx");
    if !path.exists() {
        eprintln!("skipping: {} absent", path.display());
        return;
    }
    let session = Stage2Session::load(&path).expect("load stage2");
    assert_eq!(session.input_count(), 3);
    assert_eq!(session.output_count(), 2);
}

#[test]
fn missing_model_returns_descriptive_error() {
    let session = Stage1Session::load(std::path::Path::new("/nonexistent/model.onnx"));
    let err = session.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("model") || msg.contains("not found") || msg.contains("nonexistent"),
        "expected helpful message, got: {msg}"
    );
}
