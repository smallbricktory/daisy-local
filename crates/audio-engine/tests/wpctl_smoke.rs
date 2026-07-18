//! Smoke test for the wpctl wrapper. The parser is unit-tested against a
//! sample of the typical `wpctl status` output shape; routing changes
//! require live audio streams and are tested manually.

use audio_engine::virtual_sink::parse_wpctl_status_streams;

const SAMPLE_OUTPUT: &str = r"
PipeWire 'pipewire-0' [1.0.5, manos@daisy, cookie:42]
 ├─ Audio
 │   ├─ Devices:
 │   │       58. Built-in Audio                       [vol: 1.00]
 │   ├─ Sinks:
 │   │   *  82. Built-in Speakers                     [vol: 0.50]
 │   │      91. daisy-capture                         [vol: 1.00]
 │   ├─ Sink endpoints:
 │   ├─ Sources:
 │   │   *  83. Built-in Microphone                   [vol: 0.80]
 │   │      92. daisy-capture                         [vol: 1.00]
 │   └─ Streams:
 │       ├─ Output:
 │       │     105. Microsoft Edge                    [Built-in Speakers]
 │       │     106. Spotify                            [Built-in Speakers]
 │       └─ Input:
 │             107. Microsoft Edge                    [Built-in Microphone]
 │
";

#[test]
fn parser_extracts_output_streams() {
    let streams = parse_wpctl_status_streams(SAMPLE_OUTPUT);
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].id, 105);
    assert_eq!(streams[0].app_name, "Microsoft Edge");
    assert_eq!(streams[0].current_sink, "Built-in Speakers");
    assert_eq!(streams[1].id, 106);
    assert_eq!(streams[1].app_name, "Spotify");
}

#[test]
fn parser_skips_input_streams_section() {
    // The Input section also has stream entries with the same shape; they
    // do not appear as outputs.
    let streams = parse_wpctl_status_streams(SAMPLE_OUTPUT);
    assert!(
        !streams.iter().any(|s| s.id == 107),
        "input-section streams should not appear in output list"
    );
}

#[test]
fn parser_handles_empty_input() {
    let streams = parse_wpctl_status_streams("");
    assert!(streams.is_empty());
}

// PipeWire 1.4.7+ (incl. 1.6.x) drops the trailing `[Sink]` and instead prints
// each stream's destination on indented port-link rows. The sink node is the
// token left of `:` on the first link row. Input streams stay under "Input:"
// and must not leak into the output list.
const SAMPLE_OUTPUT_1_6: &str = r"
PipeWire 'pipewire-0' [1.6.2, dev@host, cookie:1]
 └─ Streams:
        Output:
            105. Microsoft Edge
                output_FR > Speaker:playback_FR
                output_FL > Speaker:playback_FL
            106. Spotify
                output_FR > daisy-capture:playback_FR
                output_FL > daisy-capture:playback_FL
        Input:
            107. Microsoft Edge
                input_FL > Built-in Microphone:capture_FL
";

#[test]
fn parser_extracts_streams_pipewire_1_6_link_rows() {
    let streams = parse_wpctl_status_streams(SAMPLE_OUTPUT_1_6);
    assert_eq!(streams.len(), 2, "two output streams in the 1.6 sample");
    assert_eq!(streams[0].id, 105);
    assert_eq!(streams[0].app_name, "Microsoft Edge");
    assert_eq!(streams[0].current_sink, "Speaker");
    assert_eq!(streams[1].id, 106);
    assert_eq!(streams[1].app_name, "Spotify");
    assert_eq!(streams[1].current_sink, "daisy-capture");
}

#[test]
fn parser_skips_input_section_in_1_6_format() {
    let streams = parse_wpctl_status_streams(SAMPLE_OUTPUT_1_6);
    assert!(
        !streams.iter().any(|s| s.id == 107),
        "input-section streams should not appear in output list"
    );
}
