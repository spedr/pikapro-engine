//! Stat stages, self drops and stat-changing secondaries.

mod common;

use common::play_random;
use pikapro::data::BoostId;
use pikapro::data::gen9::{moves as m, species as s};
use pikapro::{Battle, BattleEvent, BattleOptions, Format, PokemonSet, Seed, Team};

/// The events of each use of `move_id`, up to the next move or upkeep.
fn after_each_use(battle: &Battle, move_id: pikapro::data::MoveId) -> Vec<Vec<BattleEvent>> {
    let events = battle.events();
    let mut uses = Vec::new();
    for (i, e) in events.iter().enumerate() {
        if matches!(e, BattleEvent::Move { move_id: id, .. } if *id == move_id) {
            let rest =
                events[i + 1..].iter().take_while(|e| !matches!(e, BattleEvent::Move { .. } | BattleEvent::Upkeep));
            uses.push(rest.copied().collect());
        }
    }
    uses
}

#[test]
fn shell_smash_follows_showdown_key_order_and_caps() {
    let format = Format { turn_limit: 6, ..Format::gen9_singles() };
    let p1 = Team::new(vec![PokemonSet::new(s::CLOYSTER).with_moves(&[m::SHELL_SMASH])]);
    let p2 = Team::new(vec![PokemonSet::new(s::PIKACHU).with_moves(&[m::PROTECT])]);
    let mut battle = Battle::new(format, [p1, p2], BattleOptions::new(Seed::fast(1))).unwrap();
    play_random(&mut battle, 1);
    let uses = after_each_use(&battle, m::SHELL_SMASH);
    assert!(uses.len() >= 4);
    let stages = |events: &[BattleEvent]| -> Vec<(bool, BoostId, u8)> {
        events
            .iter()
            .filter_map(|e| match *e {
                BattleEvent::Boost { stat, amount, .. } => Some((true, stat, amount)),
                BattleEvent::Unboost { stat, amount, .. } => Some((false, stat, amount)),
                _ => None,
            })
            .collect()
    };
    // def, spd, atk, spa, spe (Showdown's key order), not atk first.
    use BoostId::*;
    assert_eq!(stages(&uses[0]), [(false, Def, 1), (false, SpD, 1), (true, Atk, 2), (true, SpA, 2), (true, Spe, 2)]);
    // Third use: +4 -> +6; fourth: already at +6, announced with amount 0.
    assert_eq!(stages(&uses[3]), [(false, Def, 1), (false, SpD, 1), (true, Atk, 0), (true, SpA, 0), (true, Spe, 0)]);
    let mon = &battle.state().sides[0].team[0];
    assert_eq!(mon.boosts.atk, 6);
}

#[test]
fn spread_self_drops_apply_once() {
    // Make It Rain hits both foes but lowers the user's Sp. Atk once.
    let format = Format { turn_limit: 3, ..Format::gen9_doubles() };
    let mon = |sp, mv| PokemonSet::new(sp).with_moves(&[mv]);
    let p1 = Team::new(vec![
        mon(s::GHOLDENGO, m::MAKE_IT_RAIN),
        mon(s::PIKACHU, m::PROTECT),
        mon(s::GARCHOMP, m::PROTECT),
        mon(s::DRAGONITE, m::PROTECT),
    ]);
    let p2 = Team::new(vec![
        mon(s::BLISSEY, m::PROTECT),
        mon(s::CHANSEY, m::PROTECT),
        mon(s::SNORLAX, m::PROTECT),
        mon(s::MUNCHLAX, m::PROTECT),
    ]);
    let mut seen = 0;
    for seed in 0..10 {
        let mut battle =
            Battle::new(format.clone(), [p1.clone(), p2.clone()], BattleOptions::new(Seed::fast(seed))).unwrap();
        play_random(&mut battle, seed);
        for events in after_each_use(&battle, m::MAKE_IT_RAIN) {
            let hits = events.iter().filter(|e| matches!(e, BattleEvent::Damage { .. })).count();
            let drops = events.iter().filter(|e| matches!(e, BattleEvent::Unboost { stat: BoostId::SpA, .. })).count();
            if hits == 2 {
                seen += 1;
                assert_eq!(drops, 1, "seed {seed}: {events:?}");
            }
        }
    }
    assert!(seen > 0, "scenario never came up");
}
