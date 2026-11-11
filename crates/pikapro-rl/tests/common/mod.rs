//! Helpers shared by the integration tests (`mod common;` in each).

/// The protocol lines of a recorded Champions VGC channel fixture (a
/// Commander battle) of pikapro-conformance.
pub fn commander_channel() -> Vec<String> {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../pikapro-conformance/fixtures/channel/championsvgc-commander-5121013.txt"
    ))
    .unwrap();
    text.lines().filter_map(|l| l.strip_prefix("line ")).map(str::to_string).collect()
}
