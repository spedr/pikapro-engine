//! Genders, fixed when the battle is created as in Showdown's
//! `Pokemon` constructor, and the mechanics that read them.

mod common;

use pikapro::data::gen9::{moves as m, species as s};
use pikapro::data::{MoveId, SpeciesId};
use pikapro::event::VolatileEffect;
use pikapro::{Battle, BattleEvent, BattleOptions, Choice, Format, Gender, PokemonSet, Seed, SideId, Team};

fn set(species: SpeciesId, moves: &[MoveId]) -> PokemonSet {
    common::mon(species, 50, moves)
}

fn singles(p1: Vec<PokemonSet>, p2: Vec<PokemonSet>, seed: Seed) -> Battle {
    let options = BattleOptions::new(seed).with_rng_trace();
    Battle::new(Format::gen9_singles(), [Team::new(p1), Team::new(p2)], options).unwrap()
}

fn genders(battle: &Battle, side: SideId) -> Vec<Gender> {
    battle.state().side(side).team.iter().map(|p| p.gender).collect()
}

fn draws_before_start(battle: &Battle) -> usize {
    // (The `Player` events come between the draws.)
    battle
        .events()
        .iter()
        .take_while(|e| !matches!(e, BattleEvent::Poke { .. }))
        .filter(|e| matches!(e, BattleEvent::Rng { .. }))
        .count()
}

#[test]
fn set_and_species_genders_draw_nothing() {
    let p1 = vec![
        set(s::GARCHOMP, &[m::TACKLE]).with_gender(Gender::Female),
        // Showdown takes a set's gender even when the species can't have it.
        set(s::CHANSEY, &[m::TACKLE]).with_gender(Gender::Male),
        set(s::GARCHOMP, &[m::TACKLE]).with_gender(Gender::Genderless),
    ];
    let p2 = vec![set(s::TAUROS, &[m::TACKLE]), set(s::GHOLDENGO, &[m::TACKLE]), set(s::BLISSEY, &[m::TACKLE])];
    let battle = singles(p1, p2, Seed::ShowdownGen5([1, 2, 3, 4]));
    assert_eq!(genders(&battle, SideId::P1), [Gender::Female, Gender::Male, Gender::Genderless]);
    assert_eq!(genders(&battle, SideId::P2), [Gender::Male, Gender::Genderless, Gender::Female]);
    assert_eq!(draws_before_start(&battle), 0);
}

#[test]
fn unset_genders_are_drawn_in_team_order() {
    let p1 = vec![set(s::GARCHOMP, &[m::TACKLE]), set(s::GHOLDENGO, &[m::TACKLE]), set(s::PIKACHU, &[m::TACKLE])];
    let p2 = vec![set(s::EEVEE, &[m::TACKLE])];
    let battle = singles(p1, p2, Seed::ShowdownGen5([1, 2, 3, 4]));
    // Showdown's `sample(['M', 'F'])`: one `random(2)` per gendered set,
    // p1's then p2's, before anything else.
    let draws: Vec<_> = battle
        .events()
        .iter()
        .take_while(|e| !matches!(e, BattleEvent::Poke { .. }))
        .filter(|e| !matches!(e, BattleEvent::Player { .. }))
        .map(|e| match *e {
            BattleEvent::Rng { bound, result } => (bound, result),
            _ => panic!("{e:?}"),
        })
        .collect();
    assert_eq!(draws.len(), 3);
    assert!(draws.iter().all(|&(bound, _)| bound == 2));
    let g = |r: u32| if r == 0 { Gender::Male } else { Gender::Female };
    assert_eq!(genders(&battle, SideId::P1), [g(draws[0].1), Gender::Genderless, g(draws[1].1)]);
    assert_eq!(genders(&battle, SideId::P2), [g(draws[2].1)]);
}

fn attract(p1_gender: Gender, p2_gender: Gender) -> bool {
    let p1 = vec![set(s::GARCHOMP, &[m::ATTRACT]).with_gender(p1_gender)];
    let p2 = vec![set(s::GARCHOMP, &[m::SPLASH]).with_gender(p2_gender)];
    let mut battle = singles(p1, p2, Seed::fast(1));
    let act = common::move_action(0);
    battle.submit(SideId::P1, Choice::single(act)).unwrap();
    battle.submit(SideId::P2, Choice::single(act)).unwrap();
    battle.events().iter().any(|e| matches!(e, BattleEvent::EffectStart { effect: VolatileEffect::Attract, .. }))
}

#[test]
fn attract_needs_opposite_genders() {
    assert!(attract(Gender::Male, Gender::Female));
    assert!(attract(Gender::Female, Gender::Male));
    assert!(!attract(Gender::Male, Gender::Male));
    assert!(!attract(Gender::Female, Gender::Genderless));
}
