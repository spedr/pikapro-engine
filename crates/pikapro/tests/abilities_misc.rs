//! Residual, priority, accuracy, KO and miscellaneous abilities.
//! Conformance fuzzing checks them against Showdown; these tests pin a few
//! behaviours down, including abilities of species that no Gen 9 format
//! allows (Emergency Exit), which fuzzing never meets.

mod common;

use common::{move_1, singles, turn};
use pikapro::data::gen9::{abilities as a, moves as m, species as s};
use pikapro::data::{AbilityId, MoveId, SpeciesId};
use pikapro::state::Status;
use pikapro::{BattleEvent, PokemonSet, Request, SideId, SlotAction};

fn mon(species: SpeciesId, level: u8, ability: AbilityId, moves: &[MoveId]) -> PokemonSet {
    common::mon(species, level, moves).with_ability(ability)
}

/// Prankster raises a status move's priority, and Dark types are immune to
/// it (`-immune`).
#[test]
fn prankster_moves_fail_against_dark_types() {
    let mut battle = singles(
        vec![mon(s::WHIMSICOTT, 50, a::PRANKSTER, &[m::THUNDER_WAVE])],
        vec![mon(s::UMBREON, 50, a::INNER_FOCUS, &[m::SPLASH])],
        1,
    );
    turn(&mut battle, move_1(), move_1());
    assert!(battle.events().iter().any(|e| matches!(e, BattleEvent::Immune { .. })));
    assert_eq!(battle.state().side(SideId::P2).team[0].status, Status::None);
}

/// Speed Boost raises Speed at the end of each turn; Magic Guard ignores
/// poison damage.
#[test]
fn speed_boost_and_magic_guard() {
    let mut battle = singles(
        vec![mon(s::BLAZIKEN, 50, a::SPEED_BOOST, &[m::TOXIC])],
        vec![mon(s::CLEFABLE, 50, a::MAGIC_GUARD, &[m::SPLASH])],
        1,
    );
    turn(&mut battle, move_1(), move_1());
    let state = battle.state();
    assert_eq!(state.side(SideId::P1).team[0].boosts.spe, 1);
    let clefable = &state.side(SideId::P2).team[0];
    assert!(matches!(clefable.status, Status::Toxic { .. }) || clefable.status == Status::None);
    assert_eq!(clefable.hp, clefable.max_hp(), "Magic Guard: no poison damage");
}

/// Shadow Tag traps the foe from the end of the turn on.
#[test]
fn shadow_tag_traps() {
    let mut battle = singles(
        vec![mon(s::WOBBUFFET, 50, a::SHADOW_TAG, &[m::SPLASH])],
        vec![mon(s::SNORLAX, 50, a::THICK_FAT, &[m::SPLASH]), mon(s::SNORLAX, 50, a::THICK_FAT, &[m::SPLASH])],
        1,
    );
    turn(&mut battle, move_1(), move_1());
    assert!(!battle.legal_actions(SideId::P2).slot(0).iter().any(|a| matches!(a, SlotAction::Switch(_))));
}

/// Emergency Exit: dropping to half HP or less asks for a switch at once.
#[test]
fn emergency_exit_switches_out() {
    let mut battle = singles(
        vec![mon(s::GOLISOPOD, 50, a::EMERGENCY_EXIT, &[m::SPLASH]), mon(s::SNORLAX, 50, a::THICK_FAT, &[m::SPLASH])],
        vec![mon(s::SNORLAX, 100, a::THICK_FAT, &[m::SEISMIC_TOSS])],
        1,
    );
    turn(&mut battle, move_1(), move_1());
    assert!(battle.events().iter().any(|e| matches!(
        e,
        BattleEvent::ActivateEffect { effect: pikapro::event::EffectName::Ability(a::EMERGENCY_EXIT), .. }
    )));
    assert!(matches!(battle.request(SideId::P1), Request::ForceSwitch { .. }));
}
