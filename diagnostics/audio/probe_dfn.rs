// Reports which DFN model tars load under the pinned tract version.
use df::tract::{DfParams, DfTract, RuntimeParams};
fn main() {
    let dir = std::env::args().nth(1).unwrap();
    for name in [
        "DeepFilterNet3_onnx.tar.gz",
        "DeepFilterNet3_ll_onnx.tar.gz",
        "DeepFilterNet2_onnx.tar.gz",
        "DeepFilterNet2_onnx_ll.tar.gz",
    ] {
        let p = std::path::PathBuf::from(&dir).join(name);
        let r = DfParams::new(p).and_then(|dp| DfTract::new(dp, &RuntimeParams::default()));
        match r {
            Ok(m) => println!("{name}: OK (sr={})", m.sr),
            Err(e) => println!("{name}: ERR {e:#}"),
        }
    }
}
