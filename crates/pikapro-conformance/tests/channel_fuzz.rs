//! A small round of the structured channel fuzzer
//! (`pikapro_conformance::channel_fuzz`): legal transforms of player
//! channels keep the view sound and equal to the plain session's, illegal
//! ones neither panic nor leave the tracker inconsistent, and a series'
//! games record as the environment's. `PIKAPRO_CHANNEL_FUZZ=N` runs `N`
//! battles per format and source instead of one (the `channel_fuzz`
//! binary runs any number).

use pikapro_conformance::channel_fuzz::{FuzzOptions, run};

#[test]
fn channels_survive_the_fuzzer() {
    let battles = std::env::var("PIKAPRO_CHANNEL_FUZZ").ok().and_then(|n| n.parse().ok()).unwrap_or(1);
    let stats = run(&FuzzOptions::new(battles, 8_400_500), &mut |_, _| {});
    assert!(stats.games > 0);
    assert!(stats.clean(), "{}", stats.report(20));
}

/// Two fainted Pokémon named alike (Alcremie-Matcha-Cream and
/// Alcremie-Mint-Cream, both "Alcremie"), one revived by `|-heal|p2:
/// Alcremie|...|[from] move: Revival Blessing`: no client can tell which,
/// so soundness is excused from there, and the channel is still fed to its
/// end, where the series records its game (championsvgc 10,825,245).
#[test]
fn a_revived_one_of_two_alike_is_excused() {
    let mut opts = FuzzOptions::new(1, 10_825_245);
    opts.formats = vec![pikapro::Format::champions_vgc()];
    opts.sources = vec![pikapro_conformance::view::Source::Target(pikapro_conformance::view::Target::RevivalBlessing)];
    let stats = run(&opts, &mut |_, _| {});
    assert!(stats.games > 0);
    assert!(stats.clean(), "{}", stats.report(20));
}
