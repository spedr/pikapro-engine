//! Replays the golden view fixtures (`fixtures/view/*.txt`): battles that
//! passed the player-view checks, with the view's facts at each decision
//! record. Each battle's checks must still pass and every recorded fact
//! must still hold (the view may learn more, never lose or change a fact).
//!
//! A failure means the view regressed. Investigate with
//! `cargo view-conformance --replay crates/pikapro-conformance/fixtures/view/<file>`.

use pikapro_conformance::fixture;
use pikapro_conformance::view::golden;

#[test]
fn golden_views_hold() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/view");
    // Fixtures are never deleted: the manifest lists every one.
    let paths = fixture::check_manifest(&dir, "txt").unwrap_or_else(|e| panic!("{e}"));
    let mut failures = Vec::new();
    for path in &paths {
        let text = std::fs::read_to_string(path).expect("readable fixture");
        let f = golden::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if let Err(e) = golden::replay(&f) {
            failures.push(format!("{}:\n{e}", path.file_name().unwrap().to_string_lossy()));
        }
    }
    assert!(failures.is_empty(), "{} of {} view fixtures fail:\n{}", failures.len(), paths.len(), failures.join("\n"));
}
