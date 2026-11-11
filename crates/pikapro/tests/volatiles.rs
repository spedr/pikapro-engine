//! Volatile conditions: recharge, cantusetwice, confusion, Taunt, First Impression.

mod common;

use pikapro::data::MoveId;
use pikapro::data::gen9::{moves as m, species as s};
use pikapro::event::{CantReason, DamageSource, Volatile};
use pikapro::{Battle, BattleEvent, Choice, SideId, SlotAction};

fn singles(p1: &[MoveId], p2: &[MoveId], seed: u64) -> Battle {
    common::singles(vec![common::mon(s::SNORLAX, 50, p1)], vec![common::mon(s::BLISSEY, 100, p2)], seed)
}

fn use_move(battle: &mut Battle, side: SideId, index: u8) {
    let choice = Choice::Actions([common::move_action(index), SlotAction::Pass]);
    battle.submit(side, choice).unwrap();
}

fn moves_offered(battle: &Battle, side: SideId) -> Vec<SlotAction> {
    battle.legal_actions(side).slot(0).to_vec()
}

#[test]
fn recharge_turn_offers_only_recharge() {
    let mut battle = singles(&[m::HYPER_BEAM, m::TACKLE], &[m::TACKLE], 1);
    // Hyper Beam can miss; keep firing until it hits.
    for _ in 0..10 {
        use_move(&mut battle, SideId::P1, 0);
        use_move(&mut battle, SideId::P2, 0);
        if battle.events().iter().any(|e| matches!(e, BattleEvent::MustRecharge { .. })) {
            break;
        }
    }
    let only = common::move_action(0);
    assert_eq!(moves_offered(&battle, SideId::P1), [only]);
    use_move(&mut battle, SideId::P1, 0);
    use_move(&mut battle, SideId::P2, 0);
    assert!(
        battle.events().iter().any(|e| matches!(e, BattleEvent::Cant { reason: CantReason::Recharge, .. })),
        "recharge turn logged"
    );
    assert!(moves_offered(&battle, SideId::P1).len() > 1, "free again after recharging");
}

#[test]
fn gigaton_hammer_is_disabled_the_turn_after() {
    let mut battle = singles(&[m::GIGATON_HAMMER, m::TACKLE], &[m::TACKLE], 2);
    use_move(&mut battle, SideId::P1, 0);
    use_move(&mut battle, SideId::P2, 0);
    let offered = moves_offered(&battle, SideId::P1);
    assert!(!offered.iter().any(|a| matches!(a, SlotAction::Move { index: 0, .. })));
    use_move(&mut battle, SideId::P1, 1);
    use_move(&mut battle, SideId::P2, 0);
    let offered = moves_offered(&battle, SideId::P1);
    assert!(offered.iter().any(|a| matches!(a, SlotAction::Move { index: 0, .. })));
}

#[test]
fn confusion_starts_and_hurts() {
    let mut hurt = false;
    for seed in 0..30 {
        let mut battle = singles(&[m::TACKLE], &[m::CONFUSE_RAY], seed);
        for _ in 0..6 {
            if battle.is_over() {
                break;
            }
            use_move(&mut battle, SideId::P1, 0);
            use_move(&mut battle, SideId::P2, 0);
        }
        let events = battle.events();
        assert!(events.iter().any(|e| matches!(e, BattleEvent::VolatileStart { volatile: Volatile::Confusion, .. })));
        hurt |= events.iter().any(|e| matches!(e, BattleEvent::Damage { source: DamageSource::Confusion, .. }));
    }
    assert!(hurt, "a confused Pokémon eventually hits itself");
}

#[test]
fn taunt_disables_status_moves() {
    let mut battle = singles(&[m::TACKLE, m::CONFUSE_RAY], &[m::TAUNT], 3);
    use_move(&mut battle, SideId::P1, 0);
    use_move(&mut battle, SideId::P2, 0);
    let taunted =
        battle.events().iter().any(|e| matches!(e, BattleEvent::VolatileStart { volatile: Volatile::Taunt, .. }));
    assert!(taunted);
    let offered = moves_offered(&battle, SideId::P1);
    assert!(!offered.iter().any(|a| matches!(a, SlotAction::Move { index: 1, .. })));
}

#[test]
fn first_impression_only_works_first() {
    let mut battle = singles(&[m::FIRST_IMPRESSION], &[m::TACKLE], 4);
    for _ in 0..2 {
        use_move(&mut battle, SideId::P1, 0);
        use_move(&mut battle, SideId::P2, 0);
    }
    let fails = battle.events().iter().filter(|e| matches!(e, BattleEvent::Fail { .. })).count();
    assert_eq!(fails, 1, "the second First Impression fails");
}
