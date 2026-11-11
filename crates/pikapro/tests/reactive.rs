//! Reactive abilities: immunities, contact effects, status and boost
//! protection. Conformance fuzzing checks them against Showdown; these pin
//! a few behaviours down without Node.

mod common;

use pikapro::data::gen9::{abilities as a, moves as m, species as s};
use pikapro::data::{AbilityId, MoveId, SpeciesId};
use pikapro::event::{BattleEvent, Cause, MoveFrom};
use pikapro::{Battle, BattleOptions, Format, PokemonSet, Seed, SideId, Team};

fn set(species: SpeciesId, ability: AbilityId, moves: &[MoveId]) -> PokemonSet {
    common::mon(species, 50, moves).with_ability(ability)
}

fn singles(p1: PokemonSet, p2: PokemonSet) -> Battle {
    let format = Format { turn_limit: 5, ..Format::gen9_singles() };
    Battle::new(format, [Team::new(vec![p1]), Team::new(vec![p2])], BattleOptions::new(Seed::fast(7))).unwrap()
}

/// Both sides use their first move.
fn turn(battle: &mut Battle) {
    for side in SideId::BOTH {
        battle.submit(side, common::move_1()).unwrap();
    }
}

#[test]
fn volt_absorb_heals_instead_of_taking_damage() {
    let mut b = singles(set(s::PIKACHU, a::STATIC, &[m::THUNDERBOLT]), set(s::LANTURN, a::VOLT_ABSORB, &[m::SPLASH]));
    turn(&mut b);
    let lanturn = &b.state().side(SideId::P2).team[0];
    assert_eq!(lanturn.hp, lanturn.max_hp());
    let immune = BattleEvent::ImmuneFrom {
        target: pikapro::event::Actor {
            pos: pikapro::Position::new(SideId::P2, 0),
            species: s::LANTURN,
            active: true,
            id: 0,
        },
        detail: None,
        cause: Cause::ability(a::VOLT_ABSORB),
    };
    // At full HP the heal does nothing: `-immune|...|[from] ability: Volt Absorb`.
    assert!(b.events().contains(&immune));
}

#[test]
fn magic_bounce_reflects_taunt() {
    let mut b = singles(set(s::ESPEON, a::MAGIC_BOUNCE, &[m::SPLASH]), set(s::GRIMMSNARL, a::FRISK, &[m::TAUNT]));
    turn(&mut b);
    let reflected = b.events().iter().any(|e| {
        matches!(e, BattleEvent::Move { move_id, from: Some(MoveFrom::Ability(ab)), .. }
            if *move_id == m::TAUNT && *ab == a::MAGIC_BOUNCE)
    });
    assert!(reflected, "{:?}", b.events());
    assert!(b.state().side(SideId::P2).team[0].volatiles.has(pikapro::state::Volatiles::TAUNT));
    assert!(!b.state().side(SideId::P1).team[0].volatiles.has(pikapro::state::Volatiles::TAUNT));
}

#[test]
fn wonder_guard_stops_moves_that_are_not_super_effective() {
    let mut b =
        singles(set(s::GARCHOMP, a::ROUGH_SKIN, &[m::EARTHQUAKE]), set(s::SHEDINJA, a::WONDER_GUARD, &[m::SPLASH]));
    turn(&mut b);
    let shedinja = &b.state().side(SideId::P2).team[0];
    assert_eq!(shedinja.hp, 1);
    assert!(b.events().iter().any(|e| matches!(e, BattleEvent::ImmuneFrom { cause, .. }
        if *cause == Cause::ability(a::WONDER_GUARD))));
}

#[test]
fn mummy_spreads_on_contact() {
    let mut b = singles(set(s::GARCHOMP, a::ROUGH_SKIN, &[m::CRUNCH]), set(s::COFAGRIGUS, a::MUMMY, &[m::SPLASH]));
    turn(&mut b);
    assert_eq!(b.state().side(SideId::P1).team[0].ability, a::MUMMY);
    assert!(b.events().iter().any(|e| matches!(e, BattleEvent::AbilityReplaced { ability, old, .. }
        if *ability == a::MUMMY && *old == a::ROUGH_SKIN)));
}

#[test]
fn contrary_reverses_stat_changes() {
    let mut b =
        singles(set(s::SERPERIOR, a::CONTRARY, &[m::LEAF_STORM]), set(s::BLISSEY, a::NATURAL_CURE, &[m::SPLASH]));
    turn(&mut b);
    assert_eq!(b.state().side(SideId::P1).team[0].boosts.spa, 2);
}

#[test]
fn sturdy_survives_a_knockout_from_full_hp() {
    let mut b = singles(set(s::GARCHOMP, a::ROUGH_SKIN, &[m::EARTHQUAKE]), set(s::MAGNEZONE, a::STURDY, &[m::SPLASH]));
    turn(&mut b);
    let magnezone = &b.state().side(SideId::P2).team[0];
    assert_eq!(magnezone.hp, 1);
    assert!(b.events().iter().any(|e| matches!(e, BattleEvent::AbilityShow { ability, .. } if *ability == a::STURDY)));
}
