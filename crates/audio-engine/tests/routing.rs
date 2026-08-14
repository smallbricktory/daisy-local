use audio_engine::routing::{detect_routing, OutputClass};

#[test]
#[ignore = "live system probe; run with --ignored"]
fn detect_routing_returns_a_classification() {
    let r = detect_routing().unwrap();
    eprintln!("default sink: {}", r.default_sink_name);
    eprintln!("description : {}", r.default_sink_description);
    eprintln!("output class: {:?}", r.output_class);
    assert!(!r.default_sink_name.is_empty(), "should detect a default sink");
    // No specific class is asserted; it depends on the machine.
    let _ = matches!(r.output_class, OutputClass::Speaker | OutputClass::Headphone | OutputClass::Unknown);
}
