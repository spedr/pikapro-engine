//! Same inputs, same battle: the core reproducibility guarantee.

mod common;

use common::{new_battle, play_random};
use pikapro::{Battle, Format, Seed};

fn run(format: Format, seed: u64, policy: u64) -> Battle {
    let mut battle = new_battle(format, seed);
    play_random(&mut battle, policy);
    battle
}

#[test]
fn identical_inputs_give_identical_battles() {
    for format in Format::presets() {
        for seed in 0..20 {
            let a = run(format.clone(), seed, seed + 1000);
            let b = run(format.clone(), seed, seed + 1000);
            assert_eq!(a.events(), b.events(), "{} seed {seed}: event logs differ", format.id);
            assert_eq!(a.state(), b.state(), "{} seed {seed}: states differ", format.id);
            assert_eq!(a.result(), b.result());
        }
    }
}

#[test]
fn different_seeds_diverge() {
    let format = Format::gen9_doubles();
    let a = run(format.clone(), 1, 7);
    let b = run(format, 2, 7);
    assert_ne!(a.events(), b.events());
}

#[test]
fn clone_is_an_independent_snapshot() {
    let original = new_battle(Format::gen9_vgc(), 5);
    let mut copy = original.clone();
    play_random(&mut copy, 1);
    assert!(copy.is_over());
    assert_eq!(original.state(), new_battle(Format::gen9_vgc(), 5).state());

    // Two clones fed the same choices stay identical.
    let (mut a, mut b) = (original.clone(), original);
    play_random(&mut a, 3);
    play_random(&mut b, 3);
    assert_eq!(a.events(), b.events());
}

#[test]
fn fork_changes_future_randomness_only() {
    let base = new_battle(Format::gen9_doubles(), 11);
    let forked = base.fork(Seed::fast(12345));
    assert_eq!(base.state(), forked.state());
    let (mut a, mut b) = (base, forked);
    play_random(&mut a, 9);
    play_random(&mut b, 9);
    assert_ne!(a.events(), b.events());
}

#[test]
fn event_logging_does_not_change_the_simulation() {
    use pikapro::BattleOptions;
    use pikapro::testing::sample_team;
    let format = Format::gen9_vgc();
    let teams = || [sample_team(&format), sample_team(&format)];
    let mut logged = Battle::new(format.clone(), teams(), BattleOptions::new(Seed::fast(3))).unwrap();
    let mut silent = Battle::new(format.clone(), teams(), BattleOptions::new(Seed::fast(3)).without_events()).unwrap();
    play_random(&mut logged, 4);
    play_random(&mut silent, 4);
    assert_eq!(logged.state(), silent.state());
    assert!(silent.events().is_empty());
}

#[test]
fn showdown_rng_backend_is_deterministic() {
    use pikapro::BattleOptions;
    use pikapro::testing::sample_team;
    let format = Format::gen9_singles();
    let make = || {
        let teams = [sample_team(&format), sample_team(&format)];
        let opts = BattleOptions::new(Seed::ShowdownGen5([1, 2, 3, 4]));
        let mut b = Battle::new(format.clone(), teams, opts).unwrap();
        play_random(&mut b, 1);
        b
    };
    assert_eq!(make().events(), make().events());
}
