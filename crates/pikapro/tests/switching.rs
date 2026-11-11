//! Self-switching moves, forced switches and the protection family.

mod common;

use common::{mon, move_1, play_random, singles};
use pikapro::data::MoveId;
use pikapro::data::gen9::{moves as m, species as s};
use pikapro::event::DamageSource;
use pikapro::{Battle, BattleEvent, BattleOptions, Choice, Format, Progress, Request, Seed, SideId, SlotAction, Team};

#[test]
fn u_turn_asks_for_a_switch_mid_turn_then_resumes() {
    // A fast U-turn user, then a slow foe that attacks whoever is in.
    let mut battle = singles(
        vec![mon(s::JOLTEON, 100, &[m::U_TURN]), mon(s::SNORLAX, 100, &[m::TACKLE])],
        vec![mon(s::SNORLAX, 50, &[m::TACKLE])],
        1,
    );
    battle.submit(SideId::P1, move_1()).unwrap();
    assert_eq!(battle.submit(SideId::P2, move_1()), Ok(Progress::Resolved));

    // Only P1 decides, in the middle of turn 1.
    assert_eq!(battle.turn(), 1);
    assert_eq!(battle.request(SideId::P1), Request::ForceSwitch { slots: [true, false] });
    assert_eq!(battle.request(SideId::P2), Request::Wait);
    assert_eq!(battle.legal_actions(SideId::P1).slot(0), &[SlotAction::Switch(1)]);
    assert!(!battle.events().iter().any(|e| matches!(e, BattleEvent::Move { move_id: m::TACKLE, .. })));

    assert_eq!(battle.submit(SideId::P1, Choice::single(SlotAction::Switch(1))), Ok(Progress::Resolved));
    assert_eq!(battle.turn(), 2);
    let events = battle.events();
    let switch = events.iter().position(|e| matches!(e, BattleEvent::Switch { from: Some(m::U_TURN), .. }));
    let tackle = events.iter().position(|e| matches!(e, BattleEvent::Move { move_id: m::TACKLE, .. }));
    assert!(switch.unwrap() < tackle.unwrap(), "the foe's move comes after the replacement");
    let p1 = battle.state().side(SideId::P1);
    assert_eq!(p1.team[0].species, s::SNORLAX);
    assert!(p1.team[0].hp < p1.team[0].max_hp(), "the replacement took the hit");
}

#[test]
fn u_turn_without_a_replacement_just_attacks() {
    let mut battle = singles(vec![mon(s::JOLTEON, 100, &[m::U_TURN])], vec![mon(s::SNORLAX, 50, &[m::TACKLE])], 1);
    battle.submit(SideId::P1, move_1()).unwrap();
    battle.submit(SideId::P2, move_1()).unwrap();
    assert_eq!(battle.turn(), 2);
    assert_eq!(battle.request(SideId::P1), Request::Move);
    assert_eq!(battle.state().side(SideId::P1).team[0].switch_flag, pikapro::state::SwitchFlag::None);
}

#[test]
fn roar_drags_in_a_random_foe_or_fails() {
    let mut battle = singles(
        vec![mon(s::SNORLAX, 50, &[m::ROAR])],
        vec![mon(s::PIKACHU, 50, &[m::GROWL]), mon(s::GARCHOMP, 50, &[m::GROWL])],
        1,
    );
    battle.submit(SideId::P1, move_1()).unwrap();
    battle.submit(SideId::P2, move_1()).unwrap();
    assert_eq!(battle.turn(), 2);
    let dragged =
        battle.events().iter().any(|e| matches!(e, BattleEvent::Drag { actor, .. } if actor.species == s::GARCHOMP));
    assert!(dragged);
    assert_eq!(battle.state().side(SideId::P2).team[0].species, s::GARCHOMP);

    // One foe left: nothing to drag in, so Roar fails.
    let mut battle = singles(vec![mon(s::SNORLAX, 50, &[m::ROAR])], vec![mon(s::PIKACHU, 50, &[m::GROWL])], 1);
    battle.submit(SideId::P1, move_1()).unwrap();
    battle.submit(SideId::P2, move_1()).unwrap();
    assert!(battle.events().iter().any(|e| matches!(e, BattleEvent::Fail { user } if user.pos.side == SideId::P1)));
    assert!(!battle.events().iter().any(|e| matches!(e, BattleEvent::Drag { .. })));
}

#[test]
fn dragon_tail_drags_after_the_damage() {
    let mut hits = 0;
    for seed in 0..10 {
        let teams = [
            Team::new(vec![mon(s::GARCHOMP, 100, &[m::DRAGON_TAIL])]),
            Team::new(vec![mon(s::SNORLAX, 100, &[m::GROWL]), mon(s::BLISSEY, 100, &[m::GROWL])]),
        ];
        let mut battle = Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(seed))).unwrap();
        battle.submit(SideId::P1, move_1()).unwrap();
        battle.submit(SideId::P2, move_1()).unwrap();
        let events = battle.events();
        let Some(damage) = events.iter().position(|e| matches!(e, BattleEvent::Damage { .. })) else { continue };
        let drag = events.iter().position(|e| matches!(e, BattleEvent::Drag { .. })).expect("hit, then dragged");
        assert!(damage < drag);
        assert_eq!(battle.state().side(SideId::P2).team[0].species, s::BLISSEY);
        hits += 1;
    }
    assert!(hits > 0);
}

#[test]
fn spiky_shield_blocks_and_hurts_contact() {
    let mut battle =
        singles(vec![mon(s::JOLTEON, 50, &[m::TACKLE])], vec![mon(s::CHESNAUGHT, 50, &[m::SPIKY_SHIELD])], 1);
    battle.submit(SideId::P1, move_1()).unwrap();
    battle.submit(SideId::P2, move_1()).unwrap();
    let jolteon = battle.state().side(SideId::P1).team[0];
    let chesnaught = battle.state().side(SideId::P2).team[0];
    assert_eq!(chesnaught.hp, chesnaught.max_hp());
    assert_eq!(jolteon.hp, jolteon.max_hp() - jolteon.max_hp() / 8);
    assert!(battle.events().iter().any(|e| matches!(
        e,
        BattleEvent::Damage { source: DamageSource::MoveEffect { move_id: m::SPIKY_SHIELD, .. }, .. }
    )));
}

#[test]
fn feint_breaks_protection() {
    let mut battle = singles(vec![mon(s::JOLTEON, 50, &[m::FEINT])], vec![mon(s::SNORLAX, 50, &[m::DETECT])], 1);
    battle.submit(SideId::P1, move_1()).unwrap();
    battle.submit(SideId::P2, move_1()).unwrap();
    let events = battle.events();
    assert!(events.iter().any(|e| matches!(e, BattleEvent::ProtectStart { move_id: m::DETECT, .. })));
    assert!(events.iter().any(|e| matches!(e, BattleEvent::ProtectionBroken { move_id: m::FEINT, .. })));
    let snorlax = battle.state().side(SideId::P2).team[0];
    assert!(snorlax.hp < snorlax.max_hp());
}

#[test]
fn powder_moves_do_not_affect_grass_types() {
    let mut battle = singles(vec![mon(s::JOLTEON, 50, &[m::SPORE])], vec![mon(s::VENUSAUR, 50, &[m::GROWL])], 1);
    battle.submit(SideId::P1, move_1()).unwrap();
    battle.submit(SideId::P2, move_1()).unwrap();
    assert!(
        battle.events().iter().any(|e| matches!(e, BattleEvent::Immune { target } if target.species == s::VENUSAUR))
    );
}

#[test]
fn teleport_fails_without_a_replacement() {
    let mut battle = singles(vec![mon(s::ABRA, 50, &[m::TELEPORT])], vec![mon(s::SNORLAX, 50, &[m::GROWL])], 1);
    battle.submit(SideId::P1, move_1()).unwrap();
    battle.submit(SideId::P2, move_1()).unwrap();
    assert!(battle.events().iter().any(|e| matches!(e, BattleEvent::Fail { .. })));
    assert_eq!(battle.request(SideId::P1), Request::Move);
}

#[test]
fn random_battles_with_switching_moves_terminate() {
    let pool = [m::U_TURN, m::VOLT_SWITCH, m::ROAR, m::DRAGON_TAIL, m::SPIKY_SHIELD, m::TELEPORT];
    for format in [Format::gen9_singles(), Format::gen9_doubles()] {
        for seed in 0..50 {
            let team = |offset: usize| {
                let species = [s::JOLTEON, s::SNORLAX, s::GARCHOMP, s::BLISSEY, s::CHESNAUGHT, s::ABRA];
                let sets =
                    (0..4).map(|i| mon(species[(i + offset) % 6], 50, &[pool[(i + offset) % 6], m::TACKLE])).collect();
                Team::new(sets)
            };
            let teams = [team(seed as usize), team(seed as usize + 3)];
            let mut battle = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(seed))).unwrap();
            play_random(&mut battle, seed);
        }
    }
}

/// An Eject Pack switch brings in the Pokémon the other slot's queued switch
/// is for, and it faints on entry. The queued switch still brings it in, as
/// Showdown's `switchIn` does (it checks only `isActive`), with 0 HP, and
/// nothing of it runs (`fieldEvent` skips a fainted holder: no Imposter).
/// Showdown throws at the end of that turn; pikapro lets the Pokémon the
/// stale switch benched, left at the other slot's team position, switch in
/// again, and the battle goes on.
#[test]
fn a_stale_switch_to_a_pokemon_that_fainted_after_an_eject_pack_switch() {
    use pikapro::data::gen9::{abilities as a, items};
    let set = |species, level, ability, moves: &[MoveId]| mon(species, level, moves).with_ability(ability);
    let p1 = vec![
        set(s::PIKACHU, 100, a::STATIC, &[m::THUNDERBOLT, m::PROTECT]).with_item(items::EJECT_PACK),
        set(s::SHUCKLE, 1, a::STURDY, &[m::SPLASH]),
        set(s::SHEDINJA, 100, a::IMPOSTER, &[m::SPLASH]),
    ];
    let p2 = vec![
        set(s::GLIMMORA, 1, a::TOXIC_DEBRIS, &[m::STEALTH_ROCK]),
        set(s::MAGIKARP, 1, a::SWIFT_SWIM, &[m::SPLASH]),
        set(s::QWILFISH, 1, a::INTIMIDATE, &[m::SPLASH]),
    ];
    let mut battle =
        Battle::new(Format::gen9_doubles(), [Team::new(p1), Team::new(p2)], BattleOptions::new(Seed::fast(1))).unwrap();
    let mv = common::move_action;
    // Turn 1: Stealth Rock. Turn 2: Shuckle's switch to Shedinja waits
    // behind Glimmora's switch to Qwilfish, whose Intimidate sets off
    // Pikachu's Eject Pack.
    battle.submit(SideId::P1, Choice::doubles(mv(1), mv(0))).unwrap();
    battle.submit(SideId::P2, Choice::doubles(mv(0), mv(0))).unwrap();
    battle.submit(SideId::P1, Choice::doubles(mv(1), SlotAction::Switch(2))).unwrap();
    battle.submit(SideId::P2, Choice::doubles(SlotAction::Switch(2), mv(0))).unwrap();
    assert_eq!(battle.request(SideId::P1), Request::ForceSwitch { slots: [true, false] });
    // Shedinja comes in for Pikachu and faints to Stealth Rock.
    battle.submit(SideId::P1, Choice::doubles(SlotAction::Switch(2), SlotAction::Pass)).unwrap();
    let events = battle.events();
    let stale = events
        .iter()
        .position(|e| matches!(e, BattleEvent::Switch { actor, hp: 0, .. } if actor.species == s::SHEDINJA))
        .expect("the stale switch brings Shedinja in with 0 HP");
    assert!(!events[stale..].iter().any(|e| matches!(e, BattleEvent::Transform { .. })), "no Imposter");
    // Shuckle, benched by the stale switch, is at slot 0's team position.
    let p1 = battle.state().side(SideId::P1);
    assert_eq!(p1.team[0].species, s::SHUCKLE);
    assert_eq!(p1.active, [None, None]);
    assert_eq!(battle.request(SideId::P1), Request::ForceSwitch { slots: [true, true] });
    let legal = battle.legal_actions(SideId::P1);
    assert_eq!(legal.slot(0), &[SlotAction::Switch(0), SlotAction::Switch(2)]);
    battle.submit(SideId::P1, Choice::doubles(SlotAction::Switch(0), SlotAction::Switch(2))).unwrap();
    let p1 = battle.state().side(SideId::P1);
    assert_eq!((p1.team[0].species, p1.team[1].species), (s::SHUCKLE, s::PIKACHU));
    assert_eq!(p1.active, [Some(0), Some(1)]);
    play_random(&mut battle, 1);
}
