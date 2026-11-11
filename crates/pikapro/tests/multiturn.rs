//! Multi-turn moves: charging, semi-invulnerability, locked
//! moves, start-of-turn callbacks and future attacks.

mod common;

use common::{mon, move_1, move_action, singles, turn};
use pikapro::data::MoveId;
use pikapro::data::gen9::{moves as m, species as s};
use pikapro::event::MoveFrom;
use pikapro::state::Volatiles;
use pikapro::{Battle, BattleEvent, Choice, SideId, SlotAction};

fn moves_used(battle: &Battle) -> Vec<(MoveId, Option<MoveFrom>)> {
    battle
        .events()
        .iter()
        .filter_map(|e| match *e {
            BattleEvent::Move { move_id, from, .. } => Some((move_id, from)),
            _ => None,
        })
        .collect()
}

#[test]
fn fly_charges_then_strikes_locked_in() {
    let mut battle = singles(
        vec![mon(s::CORVIKNIGHT, 50, &[m::FLY, m::TACKLE]), mon(s::SNORLAX, 50, &[m::TACKLE])],
        vec![mon(s::SNORLAX, 50, &[m::TACKLE])],
        1,
    );
    turn(&mut battle, move_1(), move_1());
    assert!(battle.events().iter().any(|e| matches!(e, BattleEvent::Prepare { move_id: m::FLY, .. })));
    // Flying high: the foe's Tackle misses.
    assert!(battle.events().iter().any(|e| matches!(e, BattleEvent::Miss { .. })));
    let flyer = &battle.state().side(SideId::P1).team[0];
    assert!(flyer.volatiles.has(Volatiles::TWO_TURN_MOVE | Volatiles::CHARGING));
    let pp = flyer.moves[0].pp;
    // Locked in: "move 1" only, no switching.
    assert_eq!(battle.legal_actions(SideId::P1).slot(0), &[move_action(0)]);
    turn(&mut battle, move_1(), move_1());
    assert_eq!(moves_used(&battle)[0], (m::FLY, Some(MoveFrom::LockedMove)));
    let flyer = &battle.state().side(SideId::P1).team[0];
    assert_eq!(flyer.moves[0].pp, pp, "the second turn costs no PP");
    assert!(!flyer.volatiles.has(Volatiles::LOCKING));
    assert!(battle.legal_actions(SideId::P1).slot(0).len() > 1);
}

#[test]
fn outrage_locks_in_then_confuses() {
    let mut battle =
        singles(vec![mon(s::DRAGONITE, 50, &[m::OUTRAGE, m::TACKLE])], vec![mon(s::BLISSEY, 100, &[m::SPLASH])], 3);
    turn(&mut battle, move_1(), move_1());
    let mut locked_turns = 0;
    while battle.state().side(SideId::P1).team[0].volatiles.has(Volatiles::LOCKED_MOVE) {
        assert_eq!(battle.legal_actions(SideId::P1).slot(0).len(), 1);
        turn(&mut battle, move_1(), move_1());
        assert!(moves_used(&battle).contains(&(m::OUTRAGE, Some(MoveFrom::LockedMove))));
        locked_turns += 1;
    }
    assert!((1..=2).contains(&locked_turns), "2 or 3 turns in all");
    assert!(battle.events().iter().any(|e| matches!(e, BattleEvent::FatigueConfusion { .. })));
}

#[test]
fn future_sight_lands_two_turns_later_even_after_switching() {
    let mut battle = singles(
        vec![mon(s::ALAKAZAM, 50, &[m::FUTURE_SIGHT]), mon(s::SNORLAX, 50, &[m::SPLASH])],
        vec![mon(s::BLISSEY, 50, &[m::SPLASH])],
        5,
    );
    turn(&mut battle, move_1(), move_1());
    assert!(battle.events().iter().any(|e| matches!(e, BattleEvent::FutureMoveStart { move_id: m::FUTURE_SIGHT, .. })));
    // Again: the slot already awaits an attack.
    turn(&mut battle, move_1(), move_1());
    assert!(battle.events().iter().any(|e| matches!(e, BattleEvent::Fail { .. })));
    // The user leaves; the attack still comes at the end of turn 3.
    turn(&mut battle, Choice::single(SlotAction::Switch(1)), move_1());
    let events = battle.events();
    let end = events.iter().position(|e| matches!(e, BattleEvent::FutureMoveEnd { .. })).expect("it lands");
    assert!(matches!(
        events[end + 1..].iter().find(|e| !matches!(e, BattleEvent::Rng { .. })),
        Some(BattleEvent::Damage { .. }) | Some(BattleEvent::Crit { .. })
    ));
}

/// The damage the foreseen attack dealt this turn.
fn future_damage(battle: &Battle) -> u16 {
    let mut hp = None;
    let mut landed = false;
    for e in battle.events() {
        match *e {
            BattleEvent::FutureMoveEnd { .. } => landed = true,
            BattleEvent::Damage { target, hp: after, .. } if target.pos.side == SideId::P2 => {
                if landed {
                    return hp.expect("hit earlier this turn") - after;
                }
                hp = Some(after);
            }
            _ => {}
        }
    }
    panic!("no future attack landed");
}

/// A Gem used on the turn the user's Future Sight lands still boosts it:
/// the `gem` volatile (`onBasePower`, duration 1, no residual order) ends
/// after `futuremove`'s residual (order 3), and the hit's `BasePower` event
/// runs the user's volatiles.
#[test]
fn a_gem_used_the_same_turn_boosts_future_sight() {
    for seed in 1..=8 {
        let damage = |gem: bool| {
            let mut alakazam = mon(s::ALAKAZAM, 50, &[m::FUTURE_SIGHT, m::SWIFT, m::SPLASH]);
            if gem {
                alakazam = alakazam.with_item(pikapro::data::gen9::items::NORMAL_GEM);
            }
            let mut battle = singles(vec![alakazam], vec![mon(s::BLISSEY, 100, &[m::SPLASH])], seed);
            let mv = |index| Choice::single(move_action(index));
            turn(&mut battle, mv(0), move_1());
            turn(&mut battle, mv(2), move_1());
            // Swift uses the Gem, then Future Sight lands.
            turn(&mut battle, mv(1), move_1());
            assert!(battle.state().side(SideId::P1).team[0].item.is_none());
            future_damage(&battle)
        };
        let (boosted, plain) = (damage(true), damage(false));
        assert!(boosted > plain, "seed {seed}: {boosted} vs {plain}");
    }
}

#[test]
fn focus_punch_loses_focus_when_hit() {
    let mut battle =
        singles(vec![mon(s::MACHAMP, 50, &[m::FOCUS_PUNCH])], vec![mon(s::JOLTEON, 50, &[m::QUICK_ATTACK])], 7);
    turn(&mut battle, move_1(), move_1());
    let events = battle.events();
    let charge = events.iter().position(|e| matches!(e, BattleEvent::ChargeStart { move_id: m::FOCUS_PUNCH, .. }));
    let hit = events.iter().position(|e| matches!(e, BattleEvent::Move { move_id: m::QUICK_ATTACK, .. }));
    assert!(charge.unwrap() < hit.unwrap(), "the charge is announced before any move");
    assert!(events.iter().any(|e| matches!(e, BattleEvent::LostFocus { .. })));
    assert!(moves_used(&battle).iter().all(|&(id, _)| id != m::FOCUS_PUNCH));
}

/// A recharge turn doesn't count as a failed move (Stomping Tantrum,
/// Temper Flare): `mustrecharge`'s `onBeforeMove` returns `null`, and
/// `runMove` keeps that as the turn's result. Under Encore too, where
/// `OverrideAction` has turned the queued `recharge` into the encored
/// move by the time `BeforeMove` runs (conformance, gen9ou 40,814,993).
#[test]
fn a_recharge_turn_under_encore_is_no_failed_move() {
    use pikapro::state::MoveResult;
    let last_result = |encore: bool| {
        // Giga Impact can miss: the first seed where it hits.
        for seed in 1..40 {
            let mut battle = singles(
                vec![mon(s::SNORLAX, 50, &[m::GIGA_IMPACT, m::STOMPING_TANTRUM])],
                vec![mon(s::SHUCKLE, 100, &[m::ENCORE, m::SPLASH])],
                seed,
            );
            let foe = Choice::single(move_action(if encore { 0 } else { 1 }));
            turn(&mut battle, move_1(), foe);
            if !battle.events().iter().any(|e| matches!(e, BattleEvent::MustRecharge { .. })) {
                continue;
            }
            let user = &battle.state().side(SideId::P1).team[0];
            assert_eq!(user.volatiles.has(Volatiles::ENCORE), encore);
            turn(&mut battle, move_1(), Choice::single(move_action(1)));
            assert!(battle.events().iter().any(|e| matches!(e, BattleEvent::Cant { .. })), "seed {seed}");
            return battle.state().side(SideId::P1).team[0].volatile_counters.move_last_turn;
        }
        panic!("Giga Impact never hit");
    };
    assert_eq!(last_result(false), MoveResult::Null);
    assert_eq!(last_result(true), MoveResult::Null);
}
