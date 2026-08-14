use audio_engine::list_sources;

/// CI runners may have no audio. Asserts only that the call returns Ok and
/// yields a Vec (possibly empty), and that any returned sources have
/// non-empty names.
#[test]
fn list_sources_does_not_crash() {
    let result = list_sources();
    if let Err(e) = &result {
        // Without a running PipeWire daemon this fails at Context::connect;
        // the test skips in that case.
        let msg = format!("{e}");
        if msg.contains("connect") || msg.contains("PipeWire") {
            eprintln!("skipping: PipeWire not available ({e})");
            return;
        }
    }
    let sources = result.unwrap();
    eprintln!("found {} sources", sources.len());
    for s in &sources {
        assert!(!s.node_name.is_empty(), "node_name empty for source id={}", s.id);
        assert!(!s.description.is_empty(), "description empty for source id={}", s.id);
    }
}
