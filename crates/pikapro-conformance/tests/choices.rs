//! Choice strings against Pokémon Showdown, a few battles per format
//! (`cargo conformance --choices N` runs more). Skipped (with a message)
//! when Node or the oracle's dependencies are missing.

use pikapro::Format;
use pikapro_conformance::choices::check_choices;
use pikapro_conformance::oracle_available;

#[test]
fn choices_match_showdown() {
    if !oracle_available() {
        eprintln!("skipped: the Showdown oracle is not installed");
        return;
    }
    let mut failures = Vec::new();
    for format in Format::presets() {
        for seed in 1_720_000..1_720_002 {
            if let Err(e) = check_choices(format, seed, 6, 5) {
                failures.push(format!("{} seed {seed}: {e}", format.id));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
