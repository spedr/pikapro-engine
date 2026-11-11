//! Turn records and the protect chain in doubles: Ally
//! Switch moves the acting record's pending target with the swap, guards
//! and Psychic Terrain record `Protected`, `[spread]` makes a spread
//! target, HP paid for the user's own move is not a foe's damage, and a
//! second Wide Guard in a turn keeps its user's chain going as Showdown's
//! `stall` does.

use pikapro::event::{Actor, DamageSource, MoveAttr, SpreadSlots};
use pikapro::state::{SideCondition, Terrain};
use pikapro::{BattleEvent as E, Format, Gender, Position, SideId, Status};
use pikapro_data::SpeciesId;
use pikapro_data::gen9::{moves as m, species as s};
use pikapro_view::*;

const P1: SideId = SideId::P1;
const P2: SideId = SideId::P2;

fn mon(side: SideId, slot: u8, id: u8, species: SpeciesId) -> Actor {
    Actor { pos: Position::new(side, slot), species, active: true, id }
}

fn switch(actor: Actor) -> E {
    E::Switch {
        actor,
        team_index: actor.id,
        level: 50,
        hp: 200,
        max_hp: 200,
        status: Status::None,
        tera: None,
        from: None,
        shown: None,
        gender: Gender::Male,
    }
}

fn mv(user: Actor, move_id: pikapro_data::MoveId, target: Option<Actor>) -> E {
    E::Move { user, move_id, target, from: None }
}

fn damage(target: Actor, hp: u16) -> E {
    E::Damage { target, hp, max_hp: 200, status: Status::None, source: DamageSource::Move }
}

/// Doubles, turn 1: Incineroar and Rillaboom against Gholdengo and
/// Amoonguss, the turn's actions begun.
fn doubles() -> (PublicTracker, [Actor; 4]) {
    let mut t = PublicTracker::for_format(&Format::gen9_doubles());
    let a = [
        mon(P1, 0, 0, s::INCINEROAR),
        mon(P1, 1, 1, s::RILLABOOM),
        mon(P2, 0, 0, s::GHOLDENGO),
        mon(P2, 1, 1, s::AMOONGUSS),
    ];
    for x in a {
        t.apply(&switch(x));
    }
    t.apply(&E::Turn { turn: 1 });
    t.apply(&E::Blank);
    t.apply(&E::Timestamp);
    (t, a)
}

fn end_turn(t: &mut PublicTracker, turn: u16) {
    t.apply(&E::Blank);
    t.apply(&E::Upkeep);
    t.apply(&E::Turn { turn });
}

/// Rillaboom (slot 1) protects, then Incineroar (slot 0) uses Ally Switch:
/// each record keeps its own move's target (as its line named it) and
/// follows its Pokémon; a foe's move after the swap aims at the new slot.
#[test]
fn ally_switch_after_protect() {
    let (mut t, [inc, ril, gho, _]) = doubles();
    t.apply(&mv(ril, m::PROTECT, Some(ril)));
    t.apply(&E::ProtectStart { actor: ril, move_id: m::PROTECT });
    t.apply(&mv(inc, m::ALLY_SWITCH, Some(inc)));
    t.apply(&E::Swap { actor: inc, slot: 1, move_id: m::ALLY_SWITCH });
    let inc_now = mon(P1, 1, 0, s::INCINEROAR);
    t.apply(&mv(gho, m::SHADOW_BALL, Some(inc_now)));
    t.apply(&damage(inc_now, 100));
    end_turn(&mut t, 2);
    let r = t.side(P1).last_turn;
    // Rillaboom, now in slot 0.
    assert_eq!(r[0].move_id, Some(m::PROTECT));
    assert_eq!(r[0].target, TurnTarget::Slot { side: P1, slot: 1 }, "{:?}", r[0]);
    assert_eq!(r[0].order, 1);
    // Incineroar, now in slot 1.
    assert_eq!(r[1].move_id, Some(m::ALLY_SWITCH));
    assert_eq!(r[1].target, TurnTarget::Slot { side: P1, slot: 0 }, "{:?}", r[1]);
    assert_eq!((r[1].order, r[1].taken, r[1].taken_by), (2, 50, taken_by::MOVE));
    let g = t.side(P2).last_turn[0];
    assert_eq!((g.target, g.dealt), (TurnTarget::Slot { side: P1, slot: 1 }, [0, 50]));
}

/// Ally Switch first, then the ally (now in the other slot) moves: its
/// record is the one in its new slot.
#[test]
fn ally_switch_then_the_ally_moves() {
    let (mut t, [inc, ril, gho, _]) = doubles();
    t.apply(&mv(inc, m::ALLY_SWITCH, Some(inc)));
    t.apply(&E::Swap { actor: inc, slot: 1, move_id: m::ALLY_SWITCH });
    let ril_now = mon(P1, 0, 1, s::RILLABOOM);
    let _ = ril;
    t.apply(&mv(ril_now, m::WOOD_HAMMER, Some(gho)));
    t.apply(&damage(gho, 150));
    end_turn(&mut t, 2);
    let r = t.side(P1).last_turn;
    assert_eq!((r[0].move_id, r[0].order), (Some(m::WOOD_HAMMER), 2));
    assert_eq!((r[0].target, r[0].dealt), (TurnTarget::Slot { side: P2, slot: 0 }, [25, 0]));
    assert_eq!((r[1].move_id, r[1].order), (Some(m::ALLY_SWITCH), 1));
    assert_eq!(r[1].target, TurnTarget::Slot { side: P1, slot: 0 });
}

/// A spread move Wide Guard blocks for both targets.
#[test]
fn wide_guard_block_is_protected() {
    let (mut t, [inc, ril, gho, _]) = doubles();
    t.apply(&mv(inc, m::WIDE_GUARD, Some(inc)));
    t.apply(&E::GuardStart { actor: inc, condition: SideCondition::WideGuard });
    t.apply(&mv(gho, m::MAKE_IT_RAIN, Some(inc)));
    t.apply(&E::MoveActivate { actor: inc, move_id: m::WIDE_GUARD, of: None });
    t.apply(&E::MoveActivate { actor: ril, move_id: m::WIDE_GUARD, of: None });
    end_turn(&mut t, 2);
    let r = t.side(P2).last_turn[0];
    assert_eq!((r.outcome, r.target, r.hits), (MoveOutcome::Protected, TurnTarget::Spread, 0), "{r:?}");
}

/// Quick Guard, and Psychic Terrain stopping a priority move.
#[test]
fn quick_guard_and_psychic_terrain_blocks_are_protected() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    t.apply(&mv(ril, m::QUICK_GUARD, Some(ril)));
    t.apply(&E::GuardStart { actor: ril, condition: SideCondition::QuickGuard });
    t.apply(&mv(gho, m::FAKE_OUT, Some(inc)));
    t.apply(&E::MoveActivate { actor: inc, move_id: m::QUICK_GUARD, of: None });
    t.apply(&mv(inc, m::FAKE_OUT, Some(amo)));
    t.apply(&E::TerrainActivate { target: amo, terrain: Terrain::Psychic });
    end_turn(&mut t, 2);
    assert_eq!(t.side(P2).last_turn[0].outcome, MoveOutcome::Protected);
    assert_eq!(t.side(P1).last_turn[0].outcome, MoveOutcome::Protected);
    assert_eq!(t.side(P1).last_turn[0].target, TurnTarget::Slot { side: P2, slot: 1 });
}

/// Expanding Force in Psychic Terrain hits both foes (`[spread]`).
#[test]
fn spread_in_battle() {
    let (mut t, [inc, _, gho, amo]) = doubles();
    t.apply(&mv(inc, m::EXPANDING_FORCE, Some(gho)));
    let spread = SpreadSlots { slots: [Position::new(P2, 0), Position::new(P2, 1), Position::new(P2, 0)], len: 2 };
    t.apply(&E::MoveAttr { attr: MoveAttr::Spread(spread) });
    t.apply(&damage(gho, 150));
    t.apply(&damage(amo, 150));
    end_turn(&mut t, 2);
    let r = t.side(P1).last_turn[0];
    assert_eq!((r.target, r.dealt), (TurnTarget::Spread, [25, 25]), "{r:?}");
}

/// The HP Substitute costs its user is its own move's cost, not a move's
/// damage; a foe's hit on it afterwards is.
#[test]
fn hp_paid_for_the_own_move() {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&mv(inc, m::SUBSTITUTE, Some(inc)));
    t.apply(&damage(inc, 150));
    end_turn(&mut t, 2);
    let r = t.side(P1).last_turn[0];
    assert_eq!((r.taken, r.taken_by, r.dealt), (25, taken_by::RECOIL, [0, 0]), "{r:?}");
    t.apply(&E::Blank);
    t.apply(&E::Timestamp);
    t.apply(&mv(gho, m::SHADOW_BALL, Some(inc)));
    t.apply(&damage(inc, 100));
    end_turn(&mut t, 3);
    let r = t.side(P1).last_turn[0];
    assert_eq!((r.taken, r.taken_by), (25, taken_by::MOVE), "{r:?}");
}

/// A second Wide Guard in a turn succeeds with no line: its user's chain
/// goes on. One that `willAct` stops (`-fail`) ends it.
#[test]
fn second_wide_guard_counts_unless_it_fails() {
    let (mut t, [inc, ril, _, _]) = doubles();
    t.apply(&mv(inc, m::WIDE_GUARD, Some(inc)));
    t.apply(&E::GuardStart { actor: inc, condition: SideCondition::WideGuard });
    t.apply(&mv(ril, m::WIDE_GUARD, Some(ril)));
    end_turn(&mut t, 2);
    let chains = t.side(P1).slots.map(|a| a.counters.protect_chain);
    assert_eq!(&chains[..2], &[1, 1]);

    t.apply(&E::Blank);
    t.apply(&E::Timestamp);
    t.apply(&mv(inc, m::WIDE_GUARD, Some(inc)));
    t.apply(&E::GuardStart { actor: inc, condition: SideCondition::WideGuard });
    t.apply(&mv(ril, m::WIDE_GUARD, Some(ril)));
    t.apply(&E::Fail { user: ril });
    end_turn(&mut t, 3);
    let chains = t.side(P1).slots.map(|a| a.counters.protect_chain);
    assert_eq!(&chains[..2], &[2, 0]);
}

mod engine {
    use pikapro::{
        Battle, BattleOptions, Choice, Format, Investment, PokemonSet, Seed, SideId, SlotAction, Target, Team,
    };
    use pikapro_data::Stats;
    use pikapro_data::gen9::{abilities as a, moves as m, species as s};
    use pikapro_view::{PublicTracker, check::check_sound};

    fn set(
        species: pikapro_data::SpeciesId,
        ability: pikapro_data::AbilityId,
        moves: &[pikapro_data::MoveId],
    ) -> PokemonSet {
        PokemonSet::new(species)
            .with_level(50)
            .with_ability(ability)
            .with_moves(moves)
            .with_investment(Investment::Evs { evs: Stats::new(84, 84, 84, 84, 84, 84), ivs: Stats::splat(31) })
    }

    /// Two Wide Guard (or Quick Guard) users on one side, both using it in
    /// the same turn, for two turns: the view's chains and the engine's
    /// `stall` counters agree (`check_sound`). Against foes that attack
    /// (the second guard succeeds with no line), or that both protect
    /// first (nothing acts after the second guard: `willAct` fails it).
    #[test]
    fn two_guards_in_one_turn() {
        let format = Format::gen9_doubles();
        for (guard, foe_move) in [(m::WIDE_GUARD, 0), (m::QUICK_GUARD, 0), (m::WIDE_GUARD, 1)] {
            let p1 = Team::new(vec![
                set(s::HITMONTOP, a::INTIMIDATE, &[guard, m::FAKE_OUT]),
                set(s::AEGISLASH, a::STANCE_CHANGE, &[guard, m::KINGS_SHIELD]),
                set(s::SNORLAX, a::THICK_FAT, &[m::BODY_SLAM]),
            ]);
            let p2 = Team::new(vec![
                set(s::GARCHOMP, a::ROUGH_SKIN, &[m::EARTHQUAKE, m::PROTECT]),
                set(s::PIKACHU, a::STATIC, &[m::THUNDERBOLT, m::PROTECT]),
                set(s::SNORLAX, a::THICK_FAT, &[m::BODY_SLAM]),
            ]);
            for seed in 7_200_000..7_200_019u64 {
                let mut battle =
                    Battle::new(format.clone(), [p1.clone(), p2.clone()], BattleOptions::new(Seed::fast(seed)))
                        .unwrap();
                let mut t = PublicTracker::for_format(&format);
                for e in battle.drain_events() {
                    t.apply(&e);
                }
                for _ in 0..2 {
                    let eq = SlotAction::Move { index: foe_move, target: None, gimmick: None };
                    let target = (foe_move == 0).then_some(Target::Foe(0));
                    let tb = SlotAction::Move { index: foe_move, target, gimmick: None };
                    let foe = Choice::Actions([eq, tb]);
                    if battle.is_over()
                        || !battle.legal_actions(SideId::P1).is_legal(&wall())
                        || !battle.legal_actions(SideId::P2).is_legal(&foe)
                    {
                        break;
                    }
                    battle.submit(SideId::P1, wall()).unwrap();
                    battle.submit(SideId::P2, foe).unwrap();
                    for e in battle.drain_events() {
                        t.apply(&e);
                    }
                    if let Err(v) = check_sound(&t, &battle) {
                        panic!("{guard:?} seed {seed}: {v:?}");
                    }
                }
            }
        }
    }

    fn wall() -> Choice {
        let g = SlotAction::Move { index: 0, target: None, gimmick: None };
        Choice::Actions([g, g])
    }
}
