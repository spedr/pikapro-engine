//! Substitute, trapping and residual volatiles.

mod common;

use common::{mon as set, move_action as use_move, singles};
use pikapro::data::gen9::{moves as m, species as s};
use pikapro::event::VolatileEffect;
use pikapro::state::Volatiles;
use pikapro::{Battle, BattleEvent, BattleOptions, Choice, Format, PokemonSet, Seed, SideId, SlotAction, Team};

fn act(battle: &mut Battle, side: SideId, action: SlotAction) {
    battle.submit(side, Choice::Actions([action, SlotAction::Pass])).unwrap();
}

fn can_switch(battle: &Battle, side: SideId) -> bool {
    battle.legal_actions(side).slot(0).iter().any(|a| matches!(a, SlotAction::Switch(_)))
}

fn started(battle: &Battle, effect: VolatileEffect) -> bool {
    battle.events().iter().any(|e| matches!(e, BattleEvent::EffectStart { effect: e, .. } if *e == effect))
}

#[test]
fn mean_look_traps_until_the_trapper_leaves() {
    let p1 = vec![set(s::UMBREON, 50, &[m::MEAN_LOOK, m::TACKLE]), set(s::SNORLAX, 50, &[m::TACKLE])];
    let p2 = vec![set(s::BLISSEY, 50, &[m::GROWL]), set(s::CHANSEY, 50, &[m::GROWL])];
    let mut battle = singles(p1, p2, 1);
    assert!(can_switch(&battle, SideId::P2));
    act(&mut battle, SideId::P1, use_move(0));
    act(&mut battle, SideId::P2, use_move(0));
    assert!(started(&battle, VolatileEffect::Trapped));
    assert!(!can_switch(&battle, SideId::P2), "trapped by Mean Look");
    // The trapper leaving frees it.
    act(&mut battle, SideId::P1, SlotAction::Switch(1));
    act(&mut battle, SideId::P2, use_move(0));
    assert!(can_switch(&battle, SideId::P2));
}

#[test]
fn ghost_types_cant_be_trapped() {
    let p1 = vec![set(s::UMBREON, 50, &[m::MEAN_LOOK]), set(s::SNORLAX, 50, &[m::TACKLE])];
    let p2 = vec![set(s::GENGAR, 50, &[m::GROWL]), set(s::CHANSEY, 50, &[m::GROWL])];
    let mut battle = singles(p1, p2, 1);
    act(&mut battle, SideId::P1, use_move(0));
    act(&mut battle, SideId::P2, use_move(0));
    assert!(!started(&battle, VolatileEffect::Trapped));
    assert!(can_switch(&battle, SideId::P2));
}

#[test]
fn substitute_costs_a_quarter_and_takes_the_hits() {
    let p1 = vec![set(s::SNORLAX, 50, &[m::SUBSTITUTE, m::TACKLE])];
    let p2 = vec![set(s::BLISSEY, 50, &[m::TACKLE])];
    let mut battle = singles(p1, p2, 1);
    let max = battle.state().side(SideId::P1).team[0].max_hp();
    // Blissey is faster: hit, then the substitute.
    act(&mut battle, SideId::P1, use_move(0));
    act(&mut battle, SideId::P2, use_move(0));
    let hp = battle.state().side(SideId::P1).team[0].hp;
    act(&mut battle, SideId::P1, use_move(1));
    act(&mut battle, SideId::P2, use_move(0));
    let mon = &battle.state().side(SideId::P1).team[0];
    assert!(mon.volatiles.has(Volatiles::SUBSTITUTE));
    assert!(hp < max && mon.hp == hp, "the substitute took the second Tackle");
    assert!(
        battle
            .events()
            .iter()
            .any(|e| matches!(e, BattleEvent::EffectActivate { effect: VolatileEffect::Substitute, .. }))
    );
    // Too weak for another one.
    assert!(mon.volatile_counters.substitute_hp_halves < 2 * (max / 4));
}

/// The substitute's HP in half points, if it has one.
fn substitute(battle: &Battle, side: SideId) -> Option<u16> {
    let mon = &battle.state().side(side).team[0];
    let halves = mon.volatile_counters.substitute_hp_halves;
    assert_eq!(halves > 0, mon.volatiles.has(Volatiles::SUBSTITUTE), "no HP without a substitute");
    mon.volatiles.has(Volatiles::SUBSTITUTE).then_some(halves)
}

/// Metal Burst returns `damage * 1.5` untruncated (Showdown's
/// `damageCallback`), and a substitute loses all of it: two 1.5-point hits
/// break a 3 HP substitute, where truncated ones would leave it at 1.
#[test]
fn metal_burst_leaves_a_substitute_at_half_a_point() {
    // Level 1: Seismic Toss deals 1, and the Raticate's substitute has 3 HP.
    let p1 = vec![set(s::SHUCKLE, 1, &[m::METAL_BURST, m::SPLASH])];
    let p2 = vec![set(s::RATICATE, 1, &[m::SUBSTITUTE, m::SEISMIC_TOSS])];
    let mut battle = singles(p1, p2, 1);
    act(&mut battle, SideId::P1, use_move(1));
    act(&mut battle, SideId::P2, use_move(0));
    assert_eq!(substitute(&battle, SideId::P2), Some(6));
    // The faster Raticate hits first, then Metal Burst: 1.5 HP left.
    act(&mut battle, SideId::P1, use_move(0));
    act(&mut battle, SideId::P2, use_move(1));
    assert_eq!(substitute(&battle, SideId::P2), Some(3));
    act(&mut battle, SideId::P1, use_move(0));
    act(&mut battle, SideId::P2, use_move(1));
    assert_eq!(substitute(&battle, SideId::P2), None, "the second 1.5 breaks it");
    let ended =
        battle.events().iter().any(|e| matches!(e, BattleEvent::EffectEnd { effect: VolatileEffect::Substitute, .. }));
    assert!(ended);
}

/// The hit that breaks a substitute left at x.5 HP deals x.5: its recoil is
/// `Math.round(10.5 * 33 / 100)` = 3, not `Math.round(11 * 33 / 100)` = 4.
#[test]
fn recoil_from_breaking_a_half_point_substitute() {
    // Blissey: 589 HP (IV 27), a 147 HP substitute; Seismic Toss deals 91,
    // so Comeuppance deals 136.5 and leaves 10.5.
    let mut ivs = pikapro::data::Stats::splat(31);
    ivs[pikapro::data::Stat::Hp] = 27;
    let blissey = set(s::BLISSEY, 91, &[m::SUBSTITUTE, m::SEISMIC_TOSS, m::SPLASH])
        .with_investment(pikapro::Investment::Evs { evs: pikapro::data::Stats::splat(0), ivs });
    let p1 = vec![set(s::SNORLAX, 50, &[m::COMEUPPANCE, m::DOUBLE_EDGE, m::SPLASH])];
    let mut battle = singles(p1, vec![blissey], 1);
    act(&mut battle, SideId::P1, use_move(2));
    act(&mut battle, SideId::P2, use_move(0));
    assert_eq!(substitute(&battle, SideId::P2), Some(294));
    act(&mut battle, SideId::P1, use_move(0));
    act(&mut battle, SideId::P2, use_move(1));
    assert_eq!(substitute(&battle, SideId::P2), Some(21));
    let hp = battle.state().side(SideId::P1).team[0].hp;
    act(&mut battle, SideId::P1, use_move(1));
    act(&mut battle, SideId::P2, use_move(2));
    assert_eq!(substitute(&battle, SideId::P2), None);
    assert_eq!(hp - battle.state().side(SideId::P1).team[0].hp, 3, "recoil from 10.5 damage");
}

#[test]
fn perish_song_ends_both_after_three_turns() {
    let p1 = vec![set(s::MISMAGIUS, 50, &[m::PERISH_SONG, m::GROWL])];
    let p2 = vec![set(s::BLISSEY, 50, &[m::GROWL])];
    let mut battle = singles(p1, p2, 1);
    act(&mut battle, SideId::P1, use_move(0));
    act(&mut battle, SideId::P2, use_move(0));
    for _ in 0..3 {
        assert!(!battle.is_over());
        act(&mut battle, SideId::P1, use_move(1));
        act(&mut battle, SideId::P2, use_move(0));
    }
    let counts: Vec<u8> = battle
        .events()
        .iter()
        .filter_map(|e| match e {
            BattleEvent::PerishCount { count, silent: false, .. } => Some(*count),
            _ => None,
        })
        .collect();
    assert_eq!(counts, [3, 3, 2, 2, 1, 1, 0, 0]);
    assert!(battle.is_over());
}

#[test]
fn destiny_bond_takes_the_attacker_down() {
    let p1 = vec![set(s::MISMAGIUS, 40, &[m::DESTINY_BOND]), set(s::SNORLAX, 50, &[m::TACKLE])];
    let p2 = vec![set(s::SNORLAX, 100, &[m::CRUNCH]), set(s::BLISSEY, 50, &[m::GROWL])];
    let mut battle = singles(p1, p2, 1);
    act(&mut battle, SideId::P1, use_move(0));
    act(&mut battle, SideId::P2, use_move(0));
    let faints = battle.events().iter().filter(|e| matches!(e, BattleEvent::Faint { .. })).count();
    assert_eq!(faints, 2);
    assert!(
        battle
            .events()
            .iter()
            .any(|e| matches!(e, BattleEvent::EffectActivate { effect: VolatileEffect::DestinyBond, .. }))
    );
}

#[test]
fn leech_seed_drains_every_turn() {
    let p1 = vec![set(s::VENUSAUR, 50, &[m::LEECH_SEED])];
    let p2 = vec![set(s::SNORLAX, 50, &[m::GROWL])];
    let mut battle = singles(p1, p2, 1);
    // Leech Seed can miss: try a few times.
    for _ in 0..5 {
        act(&mut battle, SideId::P1, use_move(0));
        act(&mut battle, SideId::P2, use_move(0));
        if started(&battle, VolatileEffect::LeechSeed) {
            break;
        }
    }
    let max = battle.state().side(SideId::P2).team[0].max_hp();
    let drained = battle
        .events()
        .iter()
        .filter(|e| matches!(e, BattleEvent::EffectDamage { effect: VolatileEffect::LeechSeed, .. }))
        .count();
    assert!(drained >= 1);
    assert!(battle.state().side(SideId::P2).team[0].hp <= max - max / 8);
}

fn doubles(format: Format, p1: Vec<PokemonSet>, p2: Vec<PokemonSet>, seed: u64) -> Battle {
    let teams = [Team::new(p1), Team::new(p2)];
    let mut battle = Battle::new(format, teams, BattleOptions::new(Seed::fast(seed))).unwrap();
    for side in [SideId::P1, SideId::P2] {
        battle.submit(side, Choice::team(&[0, 1, 2, 3])).unwrap();
    }
    battle
}

/// A Curse user that turns Ghost type after choosing it (Mega Evolution,
/// Terastallization), against a Gholdengo using Protect and a Pikachu.
/// Returns which foe got the curse (`None`: no one) for each seed.
fn curse_after_a_type_change(format: Format, absol: PokemonSet, gimmick: pikapro::Gimmick) -> Vec<Option<usize>> {
    let investment = match format.stat_system {
        pikapro::StatSystem::StatPoints => pikapro::Investment::StatPoints(pikapro::data::Stats::splat(0)),
        pikapro::StatSystem::EffortValues => pikapro::Investment::default(),
    };
    let team = |lead: PokemonSet| -> Vec<PokemonSet> {
        let mut team = vec![lead];
        team.extend([s::PIKACHU, s::SNORLAX, s::EEVEE, s::VULPIX, s::DITTO].map(|sp| set(sp, 50, &[m::SPLASH])));
        team.into_iter().map(|set| set.with_investment(investment)).collect()
    };
    let gholdengo = set(s::GHOLDENGO, 50, &[m::PROTECT]).with_ability(pikapro::data::gen9::abilities::GOOD_AS_GOLD);
    let (p1, p2) = (team(absol), team(gholdengo));
    (1..=16)
        .map(|seed| {
            let mut battle = doubles(format.clone(), p1.clone(), p2.clone(), seed);
            let curse = SlotAction::Move { index: 0, target: None, gimmick: Some(gimmick) };
            battle.submit(SideId::P1, Choice::doubles(curse, use_move(0))).unwrap();
            battle.submit(SideId::P2, Choice::doubles(use_move(0), use_move(0))).unwrap();
            let team = &battle.state().side(SideId::P2).team;
            let cursed: Vec<usize> = (0..2).filter(|&i| team[i].volatiles.has(Volatiles::CURSE)).collect();
            assert!(cursed.len() <= 1);
            cursed.first().copied()
        })
        .collect()
}

/// Champions: a non-Ghost's Curse aims at its user when it is queued
/// (`resolveAction`, `tracksTarget`). Mega Evolved into Absol-Mega-Z (Dark /
/// Ghost) it is a Ghost type's Curse by the time it runs, and `onHit` curses
/// a random foe directly (`getRandomTarget`, `addVolatile`): no `TryHit`
/// against the foe, so neither Protect nor Good as Gold stops it.
#[test]
fn champions_mega_ghost_curse_bypasses_good_as_gold() {
    let absol = set(s::ABSOL, 50, &[m::CURSE]).with_item(pikapro::data::gen9::items::ABSOLITE_Z);
    let cursed = curse_after_a_type_change(Format::champions_vgc(), absol, pikapro::Gimmick::MegaEvolve);
    assert!(cursed.iter().all(Option::is_some), "a foe is always cursed: {cursed:?}");
    assert!(cursed.contains(&Some(0)), "Gholdengo, through Protect and Good as Gold: {cursed:?}");
    assert!(cursed.contains(&Some(1)), "or Pikachu: {cursed:?}");
}

/// Gen 9: the Curse is aimed at a random foe when it is queued, and a Ghost
/// type's Curse at Gholdengo goes through `TryHit`: Protect or Good as Gold
/// blocks it.
#[test]
fn gen9_tera_ghost_curse_is_blocked_by_gholdengo() {
    let absol = set(s::ABSOL, 50, &[m::CURSE]).with_tera_type(pikapro::data::Type::Ghost);
    let cursed = curse_after_a_type_change(Format::gen9_vgc(), absol, pikapro::Gimmick::Terastallize);
    assert!(!cursed.contains(&Some(0)), "Gholdengo is never cursed: {cursed:?}");
    assert!(cursed.contains(&None) && cursed.contains(&Some(1)), "{cursed:?}");
}
