//! One rule at a time: synthetic events, applied to a fresh tracker.

use pikapro::event::{
    AbilityNote, Actor, CantReason, Cause, DamageSource, EffectName, FieldEffect, HealSource, ItemEndNote, LineArg,
    ManipulationVolatile, MoveAnim, MoveAttr, MoveFrom, Volatile, VolatileEffect,
};
use pikapro::state::{SideCondition, Terrain, Weather};
use pikapro::{BattleEvent as E, Format, Gender, Position, SideId, Status};
use pikapro_data::gen9::{abilities as a, items as i, moves as m, species as s};
use pikapro_data::{BoostId, ItemId, SpeciesId, Type, TypeSet};

use crate::*;

const P1: SideId = SideId::P1;
const P2: SideId = SideId::P2;

fn mon(side: SideId, slot: u8, id: u8, species: SpeciesId) -> Actor {
    Actor { pos: Position::new(side, slot), species, active: true, id }
}

fn switch(actor: Actor, hp: u16, max_hp: u16) -> E {
    switch_with(actor, hp, max_hp, Status::None)
}

/// A switch after the previous Pokémon left by `from` (Baton Pass).
fn switch_from(actor: Actor, from: pikapro_data::MoveId) -> E {
    match switch(actor, 100, 100) {
        E::Switch { actor, team_index, level, hp, max_hp, status, tera, shown, gender, .. } => {
            E::Switch { actor, team_index, level, hp, max_hp, status, tera, from: Some(from), shown, gender }
        }
        e => e,
    }
}

fn switch_with(actor: Actor, hp: u16, max_hp: u16, status: Status) -> E {
    E::Switch {
        actor,
        team_index: actor.id,
        level: 50,
        hp,
        max_hp,
        status,
        tera: None,
        from: None,
        shown: None,
        gender: Gender::Male,
    }
}

fn mv(user: Actor, move_id: pikapro_data::MoveId, target: Option<Actor>) -> E {
    E::Move { user, move_id, target, from: None }
}

/// A doubles tracker (no team preview: rows in order of appearance) with
/// Incineroar and Rillaboom on p1, Gholdengo and Amoonguss on p2.
fn doubles() -> (PublicTracker, [Actor; 4]) {
    let mut t = PublicTracker::for_format(&Format::gen9_doubles());
    let a = [
        mon(P1, 0, 0, s::INCINEROAR),
        mon(P1, 1, 1, s::RILLABOOM),
        mon(P2, 0, 0, s::GHOLDENGO),
        mon(P2, 1, 1, s::AMOONGUSS),
    ];
    for x in a {
        t.apply(&switch(x, 200, 200));
    }
    t.apply(&E::Turn { turn: 1 });
    (t, a)
}

fn row(t: &PublicTracker, x: Actor) -> &MonView {
    t.side(x.pos.side).mon(t.row_of_actor(&x).unwrap()).unwrap()
}

fn slot(t: &PublicTracker, x: Actor) -> &ActiveView {
    &t.side(x.pos.side).slots[x.pos.slot as usize]
}

#[test]
fn switch_ins_fill_rows_in_order_of_appearance() {
    let (mut t, [inc, _, gho, _]) = doubles();
    assert_eq!(t.side(P1).mons.len(), 2);
    assert_eq!(t.side(P1).reveal_order.as_ref(), [0, 1]);
    // A third Pokémon (id 5) takes row 2, whatever its id.
    let third = mon(P1, 0, 5, s::GARCHOMP);
    t.apply(&switch(third, 150, 200));
    assert_eq!(t.row_of_actor(&third), Some(2));
    assert!(!row(&t, inc).is_active());
    assert!(row(&t, third).is_active());
    assert_eq!(row(&t, third).hp, HpView { hp: 75, max: 100, hint: pikapro_protocol::HpHint::None });
    // Only one ability: known at once.
    assert_eq!(row(&t, gho).ability, AbilityState::Known(a::GOOD_AS_GOLD));
}

#[test]
fn stat_stages_follow_every_boost_line() {
    let (mut t, [inc, ril, gho, _]) = doubles();
    t.apply(&E::Boost { target: inc, stat: BoostId::Atk, amount: 2 });
    t.apply(&E::Unboost { target: inc, stat: BoostId::Spe, amount: 1 });
    t.apply(&E::Boost { target: inc, stat: BoostId::Atk, amount: 6 });
    assert_eq!(slot(&t, inc).boost(BoostId::Atk), 6, "clamped");
    t.apply(&E::InvertBoost { target: inc });
    assert_eq!((slot(&t, inc).boost(BoostId::Atk), slot(&t, inc).boost(BoostId::Spe)), (-6, 1));
    t.apply(&E::ClearNegativeBoost { target: inc, silent: true });
    assert_eq!(slot(&t, inc).boost(BoostId::Atk), 0);
    t.apply(&E::SetBoost { target: ril, stat: BoostId::Atk, stage: 6, move_id: m::BELLY_DRUM });
    t.apply(&E::SwapBoost { source: inc, target: ril, move_id: m::POWER_SWAP });
    assert_eq!((slot(&t, inc).boost(BoostId::Atk), slot(&t, ril).boost(BoostId::Atk)), (6, 0));
    t.apply(&E::CopyBoost { source: gho, target: inc });
    assert_eq!(slot(&t, gho).boosts, slot(&t, inc).boosts);
    t.apply(&E::ClearAllBoost);
    assert!(t.sides.iter().all(|s| s.slots.iter().all(|v| v.boosts == [0; 7])));
}

#[test]
fn a_sleep_from_rest_is_marked() {
    let (mut t, [inc, _, gho, _]) = doubles();
    let slp = Status::Sleep { turns: 3 };
    t.apply(&E::StatusStart { target: inc, status: slp, from_move: Some(m::REST) });
    t.apply(&E::StatusStart { target: gho, status: slp, from_move: None });
    assert!(row(&t, inc).flags.has(MonFlags::SLEEP_FROM_REST));
    assert!(!row(&t, gho).flags.has(MonFlags::SLEEP_FROM_REST));
    t.apply(&E::CureStatus { target: inc, status: slp, from_move: None });
    assert!(!row(&t, inc).flags.has(MonFlags::SLEEP_FROM_REST));
}

#[test]
fn sleep_and_toxic_turns_are_counted_from_lines() {
    let (mut t, [inc, _, gho, _]) = doubles();
    let slp = Status::Sleep { turns: 3 };
    t.apply(&E::StatusStart { target: inc, status: slp, from_move: None });
    t.apply(&E::StatusCant { actor: inc, status: slp });
    t.apply(&mv(inc, m::SLEEP_TALK, None));
    t.apply(&E::Move {
        user: inc,
        move_id: m::FLAMETHROWER,
        target: Some(gho),
        from: Some(MoveFrom::Move(m::SLEEP_TALK)),
    });
    // One attempt: the `cant|slp`; Sleep Talk and the move it calls are
    // no more.
    assert_eq!(row(&t, inc).status_turns, 1, "cant, then Sleep Talk (the moves are no attempt)");
    assert!(row(&t, inc).revealed(m::SLEEP_TALK).is_some());
    // D: Sleep Talk calls one of its own moves: revealed, with no use.
    assert_eq!(row(&t, inc).revealed(m::FLAMETHROWER).map(|r| r.uses), Some(0));
    let tox = Status::Toxic { counter: 1 };
    t.apply(&E::StatusStart { target: gho, status: tox, from_move: None });
    for hp in [180, 150] {
        t.apply(&E::Damage { target: gho, hp, max_hp: 200, status: tox, source: DamageSource::Status(tox) });
    }
    assert_eq!(row(&t, gho).status_turns, 2);
    // Switching out and back in restarts the counter.
    t.apply(&switch(mon(P2, 0, 4, s::GARCHOMP), 100, 100));
    t.apply(&switch_with(gho, 150, 200, tox));
    assert_eq!(row(&t, gho).status_turns, 0);
}

#[test]
fn moves_count_only_from_own_slots() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    t.apply(&mv(inc, m::FAKE_OUT, Some(gho)));
    t.apply(&mv(inc, m::FAKE_OUT, Some(gho)));
    t.apply(&E::Move { user: inc, move_id: m::OUTRAGE, target: Some(gho), from: Some(MoveFrom::LockedMove) });
    assert!(slot(&t, inc).volatiles.has(PublicVolatiles::LOCKED_MOVE));
    assert_eq!(slot(&t, inc).locked_move, Some(m::OUTRAGE));
    t.apply(&mv(inc, m::STRUGGLE, Some(gho)));
    assert!(!slot(&t, inc).volatiles.has(PublicVolatiles::LOCKED_MOVE));
    let r = row(&t, inc);
    assert_eq!(r.revealed(m::FAKE_OUT).map(|x| x.uses), Some(2));
    assert!(r.revealed(m::OUTRAGE).is_none() && r.revealed(m::STRUGGLE).is_none());
    // A move it was stopped from using is still its own.
    t.apply(&E::Cant { actor: ril, reason: CantReason::Disable(m::WOOD_HAMMER) });
    assert_eq!(row(&t, ril).revealed(m::WOOD_HAMMER).map(|x| x.uses), Some(0));
    // Pressure, once known, costs a second PP per use.
    t.apply(&E::AbilityShow { actor: amo, ability: a::PRESSURE, note: AbilityNote::None, cause: None });
    t.apply(&mv(ril, m::WOOD_HAMMER, Some(amo)));
    assert_eq!(row(&t, ril).revealed(m::WOOD_HAMMER).map(|x| (x.uses, x.pp_spent)), Some((1, 2)));
    // Mimic's copy and a transformed Pokémon's moves are not its own.
    t.apply(&E::MimicStart { actor: gho, move_id: m::PROTECT });
    t.apply(&mv(gho, m::PROTECT, None));
    assert!(row(&t, gho).revealed(m::PROTECT).is_none());
    t.apply(&E::Transform { actor: gho, target: inc });
    assert!(!slot(&t, gho).volatiles.has(PublicVolatiles::MIMIC));
    t.apply(&E::Transform { actor: amo, target: inc });
    t.apply(&mv(amo, m::FAKE_OUT, Some(inc)));
    assert!(row(&t, amo).revealed(m::FAKE_OUT).is_none());
    assert!(slot(&t, amo).copied_moves.contains(&m::FAKE_OUT));
    // D: nor is a transformed caller whose called move Pressure charges
    // (view-conformance, gen9singles 4,534,923: Imposter, Copycat).
    t.apply(&E::AbilityShow { actor: inc, ability: a::PRESSURE, note: AbilityNote::None, cause: None });
    t.apply(&mv(amo, m::COPYCAT, Some(amo)));
    t.apply(&E::Move { user: amo, move_id: m::SUPERSONIC, target: Some(inc), from: Some(MoveFrom::Move(m::COPYCAT)) });
    assert!(row(&t, amo).revealed(m::COPYCAT).is_none());
}

/// D: `transformInto` drops the user's own Focus Energy and Dragon Cheer
/// without a line, and copies the target's with `[silent]` starts
/// (view-conformance, gen9singles 4,550,127, Imposter target).
#[test]
fn transform_replaces_crit_volatiles() {
    let (mut t, [inc, _, gho, amo]) = doubles();
    t.apply(&E::ManipulationStart { actor: inc, volatile: ManipulationVolatile::FocusEnergy, silent: false });
    t.apply(&E::ManipulationStart { actor: amo, volatile: ManipulationVolatile::FocusEnergy, silent: false });
    assert!(slot(&t, inc).volatiles.has(PublicVolatiles::FOCUS_ENERGY));
    t.apply(&E::Transform { actor: inc, target: gho });
    assert!(!slot(&t, inc).volatiles.has(PublicVolatiles::FOCUS_ENERGY));
    t.apply(&E::Transform { actor: gho, target: amo });
    t.apply(&E::ManipulationStart { actor: gho, volatile: ManipulationVolatile::FocusEnergy, silent: true });
    assert!(slot(&t, gho).volatiles.has(PublicVolatiles::FOCUS_ENERGY));
}

/// D: the details show Greninja-Bond and Rockruff-Dusk as their base
/// species; the view must not know the forme (it would give Battle Bond
/// and Own Tempo away) (view-conformance, gen9singles 4,550,064).
#[test]
fn hidden_formes_show_as_the_details_do() {
    let mut t = PublicTracker::for_format(&Format::gen9_singles());
    let g = mon(P1, 0, 0, s::GRENINJA_BOND);
    t.apply(&switch(g, 100, 100));
    let r = row(&t, g);
    assert_eq!((r.species, r.details_species, r.ability), (s::GRENINJA, s::GRENINJA, AbilityState::Unknown));
    let r2 = mon(P2, 0, 0, s::ROCKRUFF_DUSK);
    t.apply(&switch(r2, 100, 100));
    assert_eq!(row(&t, r2).species, s::ROCKRUFF);
}

/// D: Mega Evolution (`detailschange`) resets a type change, as
/// `setSpecies` does (view-conformance, championsvgc 4,550,050: Magic
/// Powder, then Charizard-Mega-X).
#[test]
fn a_permanent_forme_change_resets_types() {
    let (mut t, [inc, ..]) = doubles();
    t.apply(&E::TypeChange { target: inc, types: TypeSet::mono(Type::Psychic), silent: false });
    assert_eq!(t.types(P1, 0), Some(TypeSet::mono(Type::Psychic)));
    t.apply(&E::DetailsChange {
        actor: inc,
        species: s::CHARIZARD_MEGA_X,
        level: 50,
        tera: None,
        silent: false,
        gender: Gender::Male,
    });
    assert_eq!(t.types(P1, 0), Some(TypeSet::dual(Type::Fire, Type::Dragon)));
}

/// D: an item given away by Trick can come back without a line (a Sticky
/// Barb moves to an itemless attacker on contact): its effect shows it
/// held again (view-conformance, gen9doubles 4,551,808).
#[test]
fn a_traded_item_shows_again() {
    let (mut t, [inc, ..]) = doubles();
    t.apply(&E::ItemShow { actor: inc, item: i::STICKY_BARB, cause: None });
    t.apply(&E::ItemEnd {
        actor: inc,
        item: i::STICKY_BARB,
        note: ItemEndNote::Silent,
        from: Some(EffectName::Move(m::TRICK)),
        of: None,
    });
    assert!(matches!(row(&t, inc).item, ItemState::Gone { .. }));
    t.apply(&E::Damage {
        target: inc,
        hp: 150,
        max_hp: 200,
        status: Status::None,
        source: DamageSource::Effect(Cause::item(i::STICKY_BARB)),
    });
    assert_eq!(row(&t, inc).item, ItemState::Known(i::STICKY_BARB));
}

/// `trick.onHit`: `-activate|<user>|move: Trick|[of] <target>`, then the
/// target's `-item` (the user's item) and the user's (the target's). Each
/// holds the other's now; its own item is the one the other got.
fn trick_lines(t: &mut PublicTracker, user: Actor, target: Actor, mine: Option<ItemId>, yours: Option<ItemId>) {
    trick_lines_as(t, user, target, mine, yours, false)
}

/// [`trick_lines`] with the `-activate` line as the engine emits it, or as
/// the channel reads it back (`channel`: `MoveActivate`).
fn trick_lines_as(
    t: &mut PublicTracker,
    user: Actor,
    target: Actor,
    mine: Option<ItemId>,
    yours: Option<ItemId>,
    channel: bool,
) {
    t.apply(&mv(user, m::TRICK, Some(target)));
    t.apply(&if channel {
        E::MoveActivate { actor: user, move_id: m::TRICK, of: Some(target) }
    } else {
        E::ActivateEffect { actor: user, effect: EffectName::Move(m::TRICK), args: [None, None], of: Some(target) }
    });
    let from = Some(EffectName::Move(m::TRICK));
    for (who, gets, loses) in [(target, mine, yours), (user, yours, mine)] {
        match gets {
            Some(it) => t.apply(&E::ItemShow { actor: who, item: it, cause: Some(Cause::move_(m::TRICK)) }),
            None => {
                t.apply(&E::ItemEnd { actor: who, item: loses.unwrap(), note: ItemEndNote::Silent, from, of: None })
            }
        }
    }
}

/// Trick swaps two items (`trick.onHit`): each Pokémon's record keeps its
/// own item, the one the other got (`OwnItem::Moved`), and what it holds
/// now; the one that got a Choice item is locked only from its next move.
#[test]
fn trick_keeps_each_ones_own_item() {
    for channel in [false, true] {
        trick_keeps_own_items(channel);
    }
}

fn trick_keeps_own_items(channel: bool) {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&mv(inc, m::FAKE_OUT, Some(gho)));
    trick_lines_as(&mut t, gho, inc, Some(i::CHOICE_SCARF), Some(i::LEFTOVERS), channel);
    assert_eq!(row(&t, gho).item, ItemState::Known(i::LEFTOVERS));
    assert_eq!(row(&t, gho).own_item, OwnItem::Moved(Some(i::CHOICE_SCARF)));
    assert_eq!(row(&t, inc).item, ItemState::Known(i::CHOICE_SCARF));
    assert_eq!(row(&t, inc).own_item, OwnItem::Moved(Some(i::LEFTOVERS)));
    assert_eq!(row(&t, inc).own_item.of(row(&t, inc).item), Some(i::LEFTOVERS));
    assert!(slot(&t, inc).item_received && slot(&t, gho).item_received);
    t.apply(&E::Turn { turn: 2 });
    t.apply(&mv(inc, m::FLARE_BLITZ, Some(gho)));
    assert!(!slot(&t, inc).item_received, "its move after the trade locks it");
    assert_eq!(slot(&t, inc).choice_move, Some(m::FLARE_BLITZ));
    // Tricked back: each holds its own again, which stays known.
    trick_lines_as(&mut t, inc, gho, Some(i::CHOICE_SCARF), Some(i::LEFTOVERS), channel);
    assert_eq!(slot(&t, inc).choice_move, None, "a lock starts again from its next move");
    assert_eq!(row(&t, gho).item, ItemState::Known(i::CHOICE_SCARF));
    assert_eq!(row(&t, gho).own_item, OwnItem::Moved(Some(i::CHOICE_SCARF)));
    assert_eq!(row(&t, inc).own_item, OwnItem::Moved(Some(i::LEFTOVERS)));
}

/// Trick with nothing to give: the target's `-enditem` (`[silent]`) and the
/// user's `-item`. The target held its own item; the user, which had eaten
/// its berry, got another's.
#[test]
fn trick_from_an_empty_hand() {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&E::ItemEnd { actor: gho, item: i::SITRUS_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    trick_lines(&mut t, gho, inc, None, Some(i::LEFTOVERS));
    assert_eq!(row(&t, inc).item, ItemState::Gone { item: Some(i::LEFTOVERS), how: ItemLoss::Traded });
    assert_eq!(row(&t, inc).own_item, OwnItem::Held);
    assert_eq!(row(&t, inc).own_item.of(row(&t, inc).item), Some(i::LEFTOVERS));
    assert_eq!(row(&t, gho).item, ItemState::Known(i::LEFTOVERS));
    assert_eq!(row(&t, gho).own_item, OwnItem::Moved(Some(i::SITRUS_BERRY)));
}

/// Thief (`thief.onAfterHit`: the target's `-enditem ... [of] <thief>`, the
/// thief's `-item ... [of] <target>`), Symbiosis (`-activate|<giver>|
/// ability: Symbiosis|<item>|[of] <ally>`) and Pickup: the receiver's item
/// isn't its own.
#[test]
fn stolen_and_passed_items_are_not_the_receivers_own() {
    let (mut t, [inc, ril, gho, _]) = doubles();
    t.apply(&mv(inc, m::THIEF, Some(gho)));
    let thief = Cause { effect: EffectName::Move(m::THIEF), of: Some(gho) };
    t.apply(&E::ItemEnd {
        actor: gho,
        item: i::LIFE_ORB,
        note: ItemEndNote::Silent,
        from: Some(EffectName::Move(m::THIEF)),
        of: Some(inc),
    });
    t.apply(&E::ItemShow { actor: inc, item: i::LIFE_ORB, cause: Some(thief) });
    assert_eq!(row(&t, inc).own_item, OwnItem::Moved(None));
    assert_eq!(row(&t, inc).own_item.of(row(&t, inc).item), None);
    assert_eq!(row(&t, gho).own_item.of(row(&t, gho).item), Some(i::LIFE_ORB));
    // Symbiosis: Rillaboom ate its berry, its ally passes it a Leftovers.
    t.apply(&E::ItemEnd { actor: ril, item: i::SITRUS_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    t.apply(&E::ActivateEffect {
        actor: inc,
        effect: EffectName::Ability(a::SYMBIOSIS),
        args: [Some(LineArg::Item(i::LEFTOVERS)), None],
        of: Some(ril),
    });
    assert_eq!(row(&t, ril).item, ItemState::Known(i::LEFTOVERS));
    assert_eq!(row(&t, ril).own_item, OwnItem::Moved(Some(i::SITRUS_BERRY)));
    // Pickup: another's used item.
    let (mut t2, [inc2, ..]) = doubles();
    t2.apply(&E::ItemShow { actor: inc2, item: i::WHITE_HERB, cause: Some(Cause::ability(a::PICKUP)) });
    assert_eq!(row(&t2, inc2).own_item, OwnItem::Moved(None));
}

/// D: a physical Shell Side Arm makes contact: a Sticky Barb moves
/// to its itemless user without a line (view-conformance, gen9singles
/// 4,532,463).
#[test]
fn physical_shell_side_arm_takes_a_sticky_barb() {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&E::ItemShow { actor: inc, item: i::STICKY_BARB, cause: None });
    t.apply(&E::ItemEnd { actor: gho, item: i::SITRUS_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    t.apply(&mv(gho, m::SHELL_SIDE_ARM, Some(inc)));
    t.apply(&E::MoveAttr { attr: MoveAttr::Anim(MoveAnim::ShellSideArm(pikapro_data::MoveCategory::Physical)) });
    t.apply(&E::Damage { target: inc, hp: 0, max_hp: 200, status: Status::None, source: DamageSource::Move });
    assert!(matches!(row(&t, inc).item, ItemState::Gone { .. }));
    assert_eq!(row(&t, gho).item, ItemState::Known(i::STICKY_BARB));
    // Against an ally, its category isn't shown: the barb is in doubt.
    let (mut t2, [inc2, ril2, ..]) = doubles();
    t2.apply(&E::ItemShow { actor: inc2, item: i::STICKY_BARB, cause: None });
    t2.apply(&E::ItemEnd { actor: ril2, item: i::SITRUS_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    t2.apply(&mv(ril2, m::SHELL_SIDE_ARM, Some(inc2)));
    t2.apply(&E::Damage { target: inc2, hp: 100, max_hp: 200, status: Status::None, source: DamageSource::Move });
    assert_eq!((row(&t2, inc2).item, row(&t2, ril2).item), (ItemState::Unknown, ItemState::Unknown));
    // So does an attacker that may have Long Reach (gen9singles 4,528,672).
    let (mut t3, [inc3, ..]) = doubles();
    let owl = mon(P2, 0, 5, s::DECIDUEYE);
    t3.apply(&switch(owl, 100, 100));
    t3.apply(&E::ItemShow { actor: inc3, item: i::STICKY_BARB, cause: None });
    t3.apply(&E::ItemEnd { actor: owl, item: i::SITRUS_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    t3.apply(&mv(owl, m::POUND, Some(inc3)));
    t3.apply(&E::Damage { target: inc3, hp: 0, max_hp: 200, status: Status::None, source: DamageSource::Move });
    assert_eq!(row(&t3, inc3).item, ItemState::Unknown);
    // D: not under Magic Room (gen9vgc 4,590,700).
    t.apply(&E::FieldStart { effect: FieldEffect::MagicRoom, of: Some(inc) });
    t.apply(&E::ItemEnd { actor: inc, item: i::SITRUS_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    t.apply(&mv(inc, m::U_TURN, Some(gho)));
    t.apply(&E::Damage { target: gho, hp: 100, max_hp: 200, status: Status::None, source: DamageSource::Move });
    assert_eq!(row(&t, gho).item, ItemState::Known(i::STICKY_BARB));
}

/// D: a charge or a recharge lasts two residuals; Commander skipping the
/// turn it locks lets it end silently (view-conformance, gen9doubles
/// 4,530,703 and 4,530,773).
#[test]
fn skipped_charge_and_recharge_expire() {
    let (mut t, [inc, ril, gho, _]) = doubles();
    t.apply(&E::Prepare { user: inc, move_id: m::SKY_ATTACK });
    t.apply(&mv(ril, m::HYPER_BEAM, Some(gho)));
    t.apply(&E::MustRecharge { actor: ril });
    t.apply(&E::Upkeep);
    let locked = PublicVolatiles::CHARGING | PublicVolatiles::MUST_RECHARGE;
    assert!(slot(&t, inc).volatiles.has(PublicVolatiles::CHARGING));
    assert!(slot(&t, ril).volatiles.has(PublicVolatiles::MUST_RECHARGE));
    t.apply(&E::Upkeep);
    assert!(!slot(&t, inc).volatiles.has(locked) && !slot(&t, ril).volatiles.has(locked));
    assert_eq!(slot(&t, inc).locked_move, None);
}

/// D: a White Herb Covet stole and used at once is gone from both
/// (view-conformance, championsvgc 4,530,480).
#[test]
fn stolen_and_used_at_once() {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&E::ItemShow { actor: inc, item: i::WHITE_HERB, cause: None });
    t.apply(&mv(gho, m::COVET, Some(inc)));
    t.apply(&E::ItemEnd { actor: gho, item: i::WHITE_HERB, note: ItemEndNote::None, from: None, of: None });
    let covet = Cause { effect: EffectName::Move(m::COVET), of: Some(inc) };
    t.apply(&E::ItemShow { actor: gho, item: i::WHITE_HERB, cause: Some(covet) });
    assert!(matches!(row(&t, gho).item, ItemState::Gone { .. }));
    assert!(matches!(row(&t, inc).item, ItemState::Gone { .. }));
}

/// D: Thief's victim's `-enditem` comes between the thief's using up the
/// White Herb it stole and its `-item` (view-conformance, championsvgc
/// 10,440,338).
#[test]
fn stolen_by_thief_and_used_at_once() {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&E::ItemShow { actor: inc, item: i::WHITE_HERB, cause: None });
    t.apply(&mv(gho, m::THIEF, Some(inc)));
    t.apply(&E::ItemEnd { actor: gho, item: i::WHITE_HERB, note: ItemEndNote::None, from: None, of: None });
    let from = Some(EffectName::Move(m::THIEF));
    t.apply(&E::ItemEnd { actor: inc, item: i::WHITE_HERB, note: ItemEndNote::Silent, from, of: Some(gho) });
    let thief = Cause { effect: EffectName::Move(m::THIEF), of: Some(inc) };
    t.apply(&E::ItemShow { actor: gho, item: i::WHITE_HERB, cause: Some(thief) });
    assert!(matches!(row(&t, gho).item, ItemState::Gone { .. }));
    assert!(matches!(row(&t, inc).item, ItemState::Gone { how: ItemLoss::Stolen, .. }));
}

/// D: a berry's effect after Symbiosis passed a new item keeps the new
/// one (view-conformance, gen9vgc 4,527,620).
#[test]
fn berry_effect_after_symbiosis() {
    let (mut t, [inc, ril, ..]) = doubles();
    t.apply(&E::ItemEnd { actor: inc, item: i::ENIGMA_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    let symbiosis = Cause { effect: EffectName::Ability(a::SYMBIOSIS), of: Some(ril) };
    t.apply(&E::ItemShow { actor: inc, item: i::ABSORB_BULB, cause: Some(symbiosis) });
    t.apply(&E::Heal {
        target: inc,
        hp: 150,
        max_hp: 200,
        status: Status::None,
        source: HealSource::Effect(Cause::item(i::ENIGMA_BERRY)),
    });
    assert_eq!(row(&t, inc).item, ItemState::Known(i::ABSORB_BULB));
    // A Custap Berry's `[consumed]` after it (gen9doubles 4,529,156).
    t.apply(&E::ItemEnd { actor: inc, item: i::CUSTAP_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    t.apply(&E::ItemShow { actor: inc, item: i::MOON_BALL, cause: Some(symbiosis) });
    t.apply(&E::ActivateEffect {
        actor: inc,
        effect: EffectName::Item(i::CUSTAP_BERRY),
        args: [Some(LineArg::Text("[consumed]")), None],
        of: None,
    });
    assert_eq!(row(&t, inc).item, ItemState::Known(i::MOON_BALL));
}

/// D: Bug Bite eats a berry, but the user's own Life Orb shows itself
/// (view-conformance, championsvgc 4,560,074).
#[test]
fn bug_bite_user_shows_its_life_orb() {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&mv(inc, m::BUG_BITE, Some(gho)));
    t.apply(&E::Damage {
        target: inc,
        hp: 180,
        max_hp: 200,
        status: Status::None,
        source: DamageSource::Effect(Cause::item(i::LIFE_ORB)),
    });
    assert_eq!(row(&t, inc).item, ItemState::Known(i::LIFE_ORB));
}

/// D: Cud Chew's second bite of a berry leaves the held item alone
/// (view-conformance, championsvgc 4,523,658).
#[test]
fn cud_chew_eats_again_not_the_held_item() {
    let (mut t, [inc, ..]) = doubles();
    t.apply(&E::ItemShow { actor: inc, item: i::SHARP_BEAK, cause: None });
    t.apply(&E::ActivateEffect { actor: inc, effect: EffectName::Ability(a::CUD_CHEW), args: [None, None], of: None });
    t.apply(&E::ItemEnd { actor: inc, item: i::HABAN_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    assert_eq!(row(&t, inc).item, ItemState::Known(i::SHARP_BEAK));
    // Nor does the berry's effect (championsvgc 4,543,710).
    t.apply(&E::ActivateEffect { actor: inc, effect: EffectName::Ability(a::CUD_CHEW), args: [None, None], of: None });
    t.apply(&E::ItemEnd { actor: inc, item: i::SITRUS_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    t.apply(&E::Heal {
        target: inc,
        hp: 150,
        max_hp: 200,
        status: Status::None,
        source: HealSource::Effect(Cause::item(i::SITRUS_BERRY)),
    });
    assert_eq!(row(&t, inc).item, ItemState::Known(i::SHARP_BEAK));
}

/// D: breaking protection (Feint, Phantom Force: `-activate|...|[broken]`)
/// also ends the side's Wide Guard and Quick Guard (view-conformance,
/// championsvgc 4,580,228); a resist berry's `[weaken]` after Symbiosis
/// passed an item leaves the new item (4,582,176).
#[test]
fn broken_guards_and_weakening_berries() {
    let (mut t, [inc, _, gho, amo]) = doubles();
    t.apply(&E::SideStart { side: P2, condition: SideCondition::WideGuard });
    assert_eq!(t.side(P2).condition(SideCondition::WideGuard).layers, 1);
    t.apply(&E::ProtectionBroken { target: gho, move_id: m::FEINT });
    assert_eq!(t.side(P2).condition(SideCondition::WideGuard).layers, 0);

    t.apply(&E::ItemEnd { actor: amo, item: i::CHILAN_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    t.apply(&E::ItemShow { actor: amo, item: i::SMOOTH_ROCK, cause: None });
    t.apply(&E::ItemEnd { actor: amo, item: i::CHILAN_BERRY, note: ItemEndNote::Weaken, from: None, of: None });
    assert_eq!(row(&t, amo).item, ItemState::Known(i::SMOOTH_ROCK));
    let _ = inc;
}

/// D: a dance Dancer copies and Imprison stops (`cant|<mon>|move:
/// Imprison|<move>`) is not the dancer's own move; a revival takes a faint
/// back even when the faint showed on another record (an Illusion user
/// fainting in disguise) (view-conformance, gen9singles 4,580,054 and
/// 4,593,354).
#[test]
fn dancer_copies_and_revivals() {
    let (mut t, [inc, _, gho, amo]) = doubles();
    t.apply(&E::ActivateEffect { actor: gho, effect: EffectName::Ability(a::DANCER), args: [None, None], of: None });
    t.apply(&E::EffectCant { actor: gho, effect: VolatileEffect::Imprison, move_id: Some(m::QUIVER_DANCE) });
    assert!(row(&t, gho).revealed(m::QUIVER_DANCE).is_none());
    t.apply(&E::EffectCant { actor: gho, effect: VolatileEffect::Imprison, move_id: Some(m::SHADOW_BALL) });
    assert!(row(&t, gho).revealed(m::SHADOW_BALL).is_some());
    // A move Magic Bounce sent back, stopped by Heal Block, isn't its own.
    t.apply(&mv(inc, m::STRENGTH_SAP, Some(gho)));
    t.apply(&E::EffectCant { actor: gho, effect: VolatileEffect::HealBlock, move_id: Some(m::STRENGTH_SAP) });
    assert!(row(&t, gho).revealed(m::STRENGTH_SAP).is_none());
    // Nor a move Mimic copied, named by Eerie Spell.
    t.apply(&E::MimicStart { actor: amo, move_id: m::TAUNT });
    t.apply(&E::PpDrain { target: amo, move_id: m::TAUNT, amount: 3 });
    assert!(row(&t, amo).revealed(m::TAUNT).is_none());

    t.apply(&E::Faint { actor: inc });
    assert_eq!(t.side(P1).fainted, 1);
    let mut benched = amo;
    benched.pos = Position::new(P1, 1);
    benched.id = 1;
    t.apply(&E::MoveHeal {
        target: Actor { active: false, ..benched },
        hp: 50,
        max_hp: 100,
        status: Status::None,
        move_id: m::REVIVAL_BLESSING,
    });
    assert_eq!(t.side(P1).fainted, 0);
}

/// D: a fainted Tatsugiri leaves its Dondozo `commanded` (only Dondozo's
/// own exit removes it); Tatsugiri commands until the next action after
/// its Dondozo fainted (view-conformance, gen9vgc 4,550,023 and
/// 4,591,813).
#[test]
fn commander_outlives_tatsugiri_not_dondozo() {
    let (mut t, [tatsugiri, dondozo, ..]) = doubles();
    let link = |t: &mut PublicTracker| {
        t.sides[0].slots[0].volatiles.set(PublicVolatiles::COMMANDING);
        t.sides[0].slots[1].volatiles.set(PublicVolatiles::COMMANDED);
    };
    link(&mut t);
    t.apply(&E::Faint { actor: tatsugiri });
    assert!(t.side(P1).slots[1].volatiles.has(PublicVolatiles::COMMANDED));
    let (mut t, [tatsugiri, dondozo2, ..]) = doubles();
    let _ = (tatsugiri, dondozo);
    link(&mut t);
    // Fainted in an action: its `Update` ends the command at once.
    t.apply(&E::Faint { actor: dondozo2 });
    assert!(!t.side(P1).slots[0].volatiles.has(PublicVolatiles::COMMANDING));
    // In the residuals too; but when the battle ends with the faint, no
    // `Update` runs (gen9vgc 4,591,813).
    let (mut t, [_, dondozo3, ..]) = doubles();
    link(&mut t);
    t.apply(&E::Blank);
    t.apply(&E::Faint { actor: dondozo3 });
    assert!(!t.side(P1).slots[0].volatiles.has(PublicVolatiles::COMMANDING));
    t.apply(&E::Blank);
    t.apply(&E::Win { side: P2 });
    assert!(t.side(P1).slots[0].volatiles.has(PublicVolatiles::COMMANDING));
    // But the weather's residual runs one (gen9doubles 4,533,923).
    let (mut t, [_, dondozo5, ..]) = doubles();
    link(&mut t);
    t.apply(&E::Blank);
    let sand = DamageSource::Weather(Weather::Sand);
    t.apply(&E::Damage { target: dondozo5, hp: 0, max_hp: 200, status: Status::None, source: sand });
    t.apply(&E::Faint { actor: dondozo5 });
    t.apply(&E::Blank);
    t.apply(&E::Win { side: P2 });
    assert!(!t.side(P1).slots[0].volatiles.has(PublicVolatiles::COMMANDING));
    // A Tatsugiri that lost Commander (Doodle) commands on (gen9vgc
    // 4,533,643).
    let (mut t, [_, dondozo4, ..]) = doubles();
    link(&mut t);
    t.sides[0].mons[0].ability = AbilityState::Known(a::PICKUP);
    t.apply(&E::Faint { actor: dondozo4 });
    assert!(t.side(P1).slots[0].volatiles.has(PublicVolatiles::COMMANDING));
    // Switched out (a switch queued before Tatsugiri came in).
    let (mut t, _) = doubles();
    link(&mut t);
    t.apply(&switch(mon(P1, 1, 3, s::SNORLAX), 100, 100));
    assert!(!t.side(P1).slots[0].volatiles.has(PublicVolatiles::COMMANDING));
}

#[test]
fn transform_copies_form_types_stages_and_moves() {
    let (mut t, [inc, _, gho, amo]) = doubles();
    t.apply(&mv(gho, m::MAKE_IT_RAIN, None));
    t.apply(&E::Boost { target: gho, stat: BoostId::SpA, amount: 2 });
    t.apply(&E::TransformFrom { actor: inc, target: gho, cause: Cause::ability(a::IMPOSTER) });
    let v = slot(&t, inc);
    assert!(v.volatiles.has(PublicVolatiles::TRANSFORMED));
    assert_eq!(v.transformed.map(|x| x.species), Some(s::GHOLDENGO));
    assert_eq!(v.boost(BoostId::SpA), 2);
    assert_eq!(v.copied_moves.as_ref(), [m::MAKE_IT_RAIN]);
    assert_eq!(t.types(P1, 0), Some(TypeSet::dual(Type::Steel, Type::Ghost)));
    let r = row(&t, inc);
    assert_eq!(r.base_ability, AbilityState::Known(a::IMPOSTER));
    assert_eq!(r.ability, AbilityState::Known(a::GOOD_AS_GOLD), "a transformed Pokémon has its target's ability");
    assert_eq!(r.species, s::INCINEROAR, "its record keeps its own species");
    // Leaving undoes it.
    t.apply(&switch(mon(P1, 0, 3, s::GARCHOMP), 100, 100));
    assert_eq!(row(&t, inc).ability, AbilityState::Known(a::IMPOSTER));
    let _ = amo;
}

#[test]
fn items_are_known_then_gone_with_how() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    t.apply(&E::Heal {
        target: inc,
        hp: 200,
        max_hp: 200,
        status: Status::None,
        source: HealSource::Effect(Cause::item(i::LEFTOVERS)),
    });
    assert_eq!(row(&t, inc).item, ItemState::Known(i::LEFTOVERS));
    // Rocky Helmet names its holder with `[of]`.
    t.apply(&E::Damage {
        target: gho,
        hp: 180,
        max_hp: 200,
        status: Status::None,
        source: DamageSource::Effect(Cause::item(i::ROCKY_HELMET).of(ril)),
    });
    assert_eq!(row(&t, ril).item, ItemState::Known(i::ROCKY_HELMET));
    assert_eq!(row(&t, gho).item, ItemState::Unknown);
    t.apply(&E::ItemEnd {
        actor: ril,
        item: i::ROCKY_HELMET,
        note: ItemEndNote::None,
        from: Some(EffectName::Move(m::KNOCK_OFF)),
        of: Some(gho),
    });
    assert_eq!(row(&t, ril).item, ItemState::Gone { item: Some(i::ROCKY_HELMET), how: ItemLoss::KnockedOff });
    // A berry's effect after its `-enditem` doesn't bring it back.
    t.apply(&E::ItemEnd { actor: amo, item: i::SITRUS_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    t.apply(&E::Heal {
        target: amo,
        hp: 150,
        max_hp: 200,
        status: Status::None,
        source: HealSource::Effect(Cause::item(i::SITRUS_BERRY)),
    });
    assert_eq!(row(&t, amo).item, ItemState::Gone { item: Some(i::SITRUS_BERRY), how: ItemLoss::Eaten });
    // Frisk: the item is the named Pokémon's, the ability the frisker's.
    t.apply(&E::ItemShow { actor: gho, item: i::CHOICE_SPECS, cause: Some(Cause::ability(a::FRISK).of(inc)) });
    assert_eq!(row(&t, gho).item, ItemState::Known(i::CHOICE_SPECS));
    assert_eq!(row(&t, inc).ability, AbilityState::Known(a::FRISK));
    // Thief: the item moves.
    t.apply(&E::ItemEnd {
        actor: gho,
        item: i::CHOICE_SPECS,
        note: ItemEndNote::Silent,
        from: Some(EffectName::Move(m::THIEF)),
        of: Some(inc),
    });
    t.apply(&E::ItemShow { actor: inc, item: i::CHOICE_SPECS, cause: Some(Cause::move_(m::THIEF).of(gho)) });
    assert_eq!(row(&t, inc).item, ItemState::Known(i::CHOICE_SPECS));
    assert_eq!(row(&t, gho).item, ItemState::Gone { item: Some(i::CHOICE_SPECS), how: ItemLoss::Stolen });
}

#[test]
fn trick_swaps_items() {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&mv(inc, m::TRICK, Some(gho)));
    t.apply(&E::ActivateEffect { actor: inc, effect: EffectName::Move(m::TRICK), args: [None, None], of: Some(gho) });
    t.apply(&E::ItemShow { actor: gho, item: i::CHOICE_SCARF, cause: Some(Cause::move_(m::TRICK)) });
    t.apply(&E::ItemEnd {
        actor: inc,
        item: i::FLAME_ORB,
        note: ItemEndNote::Silent,
        from: Some(EffectName::Move(m::TRICK)),
        of: None,
    });
    assert_eq!(row(&t, gho).item, ItemState::Known(i::CHOICE_SCARF));
    assert_eq!(row(&t, inc).item, ItemState::Gone { item: Some(i::FLAME_ORB), how: ItemLoss::Traded });
}

#[test]
fn abilities_known_own_and_changed() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    // Intimidate announces itself.
    t.apply(&E::AbilityShow { actor: inc, ability: a::INTIMIDATE, note: AbilityNote::Boost, cause: None });
    assert_eq!(
        (row(&t, inc).ability, row(&t, inc).base_ability),
        (AbilityState::Known(a::INTIMIDATE), AbilityState::Known(a::INTIMIDATE))
    );
    // Skill Swap between foes shows both.
    t.apply(&E::SkillSwap { source: ril, target: amo, abilities: Some((a::REGENERATOR, a::GRASSY_SURGE)) });
    assert_eq!(row(&t, ril).ability, AbilityState::Known(a::REGENERATOR));
    assert_eq!(row(&t, ril).base_ability, AbilityState::Known(a::GRASSY_SURGE));
    assert_eq!(row(&t, amo).base_ability, AbilityState::Known(a::REGENERATOR));
    // Its own ability acting late isn't its ability now.
    t.apply(&E::StatusFrom { target: gho, status: Status::Poison, cause: Cause::ability(a::GRASSY_SURGE).of(ril) });
    assert_eq!(row(&t, ril).ability, AbilityState::Known(a::REGENERATOR));
    // Between allies nothing is shown: what was known swaps.
    t.apply(&E::SkillSwap { source: inc, target: ril, abilities: None });
    assert_eq!(row(&t, ril).ability, AbilityState::Known(a::INTIMIDATE));
    assert_eq!(row(&t, inc).ability, AbilityState::Known(a::REGENERATOR));
    assert_eq!(row(&t, inc).base_ability, AbilityState::Known(a::INTIMIDATE));
    // Mummy.
    t.apply(&E::AbilityReplaced { holder: gho, ability: a::MUMMY, target: amo, old: a::GRASSY_SURGE });
    assert_eq!(row(&t, amo).ability, AbilityState::Known(a::MUMMY));
    // Leaving the field restores the own ability.
    t.apply(&switch(mon(P1, 0, 3, s::GARCHOMP), 100, 100));
    assert_eq!(row(&t, inc).ability, AbilityState::Known(a::INTIMIDATE));
}

#[test]
fn tera_and_displayed_types() {
    let (mut t, [inc, ril, gho, _]) = doubles();
    assert_eq!(t.types(P1, 0), Some(TypeSet::dual(Type::Fire, Type::Dark)));
    t.apply(&E::TypeChange { target: ril, types: TypeSet::mono(Type::Water), silent: false });
    assert_eq!(t.types(P1, 1), Some(TypeSet::mono(Type::Water)));
    t.apply(&E::TypeAdd { target: ril, ty: Type::Ghost, move_id: Some(m::TRICK_OR_TREAT) });
    assert_eq!(slot(&t, ril).added_type, Some(Type::Ghost));
    t.apply(&E::Terastallize { actor: inc, ty: Type::Grass });
    assert!(t.side(P1).tera_used);
    assert_eq!(t.types(P1, 0), Some(TypeSet::mono(Type::Grass)));
    assert_eq!(row(&t, inc).tera_type, Some(Type::Grass));
    // Stellar keeps its types.
    t.apply(&E::Terastallize { actor: gho, ty: Type::Stellar });
    assert_eq!(t.types(P2, 0), Some(TypeSet::dual(Type::Steel, Type::Ghost)));
}

/// D: Double Shock announces its type change on a Terastallized user,
/// whose `setType` fails: Transform copies the types it kept
/// (view-conformance, gen9vgc 11,511,965).
#[test]
fn a_terastallized_type_change_keeps_the_types() {
    let (mut t, [inc, ril, ..]) = doubles();
    t.apply(&E::Terastallize { actor: inc, ty: Type::Electric });
    let cause = Cause { effect: EffectName::Move(m::DOUBLE_SHOCK), of: None };
    t.apply(&E::TypeChangeFrom { target: inc, types: TypeSet::mono(Type::Electric), cause });
    assert_eq!(t.types(P1, 0), Some(TypeSet::mono(Type::Electric)));
    t.apply(&E::Transform { actor: ril, target: inc });
    assert_eq!(t.types(P1, 1), Some(TypeSet::dual(Type::Fire, Type::Dark)));
}

/// D: Reflect Type copies the target's types without `???` (Double
/// Shock's), and Normal when only an added type is left (view-conformance,
/// championsvgc 12,420,023).
#[test]
fn reflect_type_drops_the_typeless() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    let shock = Cause { effect: EffectName::Move(m::DOUBLE_SHOCK), of: None };
    t.apply(&E::TypeChangeFrom { target: gho, types: TypeSet::dual(Type::Unknown, Type::Ghost), cause: shock });
    t.apply(&E::TypeCopy { target: amo, of: gho });
    assert_eq!(t.types(P2, 1), Some(TypeSet::mono(Type::Ghost)));
    t.apply(&E::TypeChangeFrom { target: ril, types: TypeSet::mono(Type::Unknown), cause: shock });
    t.apply(&E::TypeAdd { target: ril, ty: Type::Grass, move_id: Some(m::FORESTS_CURSE) });
    t.apply(&E::TypeCopy { target: inc, of: ril });
    assert_eq!(t.types(P1, 0), Some(TypeSet::mono(Type::Normal)));
    assert_eq!(slot(&t, inc).added_type, Some(Type::Grass));
}

/// D: Mimicry's reset (`-end|<mon>|typechange|[silent]`, after
/// `setType`) ends Trick-or-Treat's added type too (view-conformance,
/// championsvgc 11,501,101).
#[test]
fn mimicry_reset_ends_an_added_type() {
    let (mut t, [_, ril, ..]) = doubles();
    t.apply(&E::TypeChange { target: ril, types: TypeSet::mono(Type::Grass), silent: false });
    t.apply(&E::TypeAdd { target: ril, ty: Type::Ghost, move_id: Some(m::TRICK_OR_TREAT) });
    t.apply(&E::EffectStartEnd {
        actor: ril,
        start: false,
        what: LineArg::Text("typechange"),
        effect: None,
        cause: None,
        silent: true,
    });
    assert_eq!(slot(&t, ril).added_type, None);
    assert!(!slot(&t, ril).volatiles.has(PublicVolatiles::TYPE_ADD | PublicVolatiles::TYPE_CHANGE));
    assert_eq!(t.types(P1, 1), Some(TypeSet::mono(Type::Grass)));
}

#[test]
fn roost_removes_flying_for_the_turn() {
    let mut t = PublicTracker::for_format(&Format::gen9_singles());
    let x = mon(P1, 0, 0, s::CORVIKNIGHT);
    t.apply(&switch(x, 100, 100));
    t.apply(&E::SingleTurn { actor: x, move_id: m::ROOST });
    assert_eq!(t.types(P1, 0), Some(TypeSet::mono(Type::Steel)));
    t.apply(&E::Upkeep);
    assert_eq!(t.types(P1, 0), Some(TypeSet::dual(Type::Flying, Type::Steel)));
}

/// Taunt, Encore and Disable keep whether their target still had a move to
/// come when they started (Showdown's `queue.willMove`), which sets their
/// duration: Taunt and Encore one turn longer without (Taunt only for a
/// Pokémon on the field since an earlier turn), Disable one turn shorter
/// with, or on the Pokémon moving (`taunt`, `encore`, `disable.condition.
/// onStart` in `data/moves.js`).
#[test]
fn taunt_encore_and_disable_keep_whether_their_target_had_moved() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    // Gholdengo moves, then Incineroar Taunts it: none to come, so 4 turns.
    t.apply(&mv(gho, m::SHADOW_BALL, Some(inc)));
    t.apply(&mv(inc, m::TAUNT, Some(gho)));
    t.apply(&E::VolatileStart { actor: gho, volatile: Volatile::Taunt, move_id: None });
    assert!(slot(&t, gho).counters.taunt_long);
    // Amoonguss hasn't moved: Encore 3 turns, Disable 4.
    t.apply(&mv(ril, m::ENCORE, Some(amo)));
    t.apply(&E::VolatileStart { actor: amo, volatile: Volatile::Encore, move_id: None });
    assert!(!slot(&t, amo).counters.encore_long);
    t.apply(&E::VolatileStart { actor: amo, volatile: Volatile::Disable, move_id: Some(m::POLLEN_PUFF) });
    assert!(slot(&t, amo).counters.disable_short);
    // Gholdengo has moved and isn't moving: Disable 5 turns, Encore 4.
    t.apply(&E::VolatileStart { actor: gho, volatile: Volatile::Disable, move_id: Some(m::SHADOW_BALL) });
    assert!(!slot(&t, gho).counters.disable_short);
    t.apply(&E::VolatileStart { actor: gho, volatile: Volatile::Encore, move_id: None });
    assert!(slot(&t, gho).counters.encore_long);
    // The Pokémon moving now (Cursed Body's Disable): one turn short.
    t.apply(&mv(amo, m::POLLEN_PUFF, Some(gho)));
    t.apply(&E::VolatileStart { actor: amo, volatile: Volatile::Disable, move_id: Some(m::POLLEN_PUFF) });
    assert!(slot(&t, amo).counters.disable_short);
    // Next turn, a Taunt before Rillaboom moves: 3 turns.
    t.apply(&E::Upkeep);
    t.apply(&E::Turn { turn: 2 });
    t.apply(&E::VolatileStart { actor: ril, volatile: Volatile::Taunt, move_id: None });
    assert!(!slot(&t, ril).counters.taunt_long);
}

/// Instruct queues a move action of its own for its target
/// (`instruct.onHit`: `queue.prioritizeAction`): (a) Instructed before its
/// own action, the target still has that action queued (`queue.willMove`),
/// so a Taunt or Encore afterwards lasts 3 turns, not 4; (b) Instructed
/// after it moved, the target is the one moving (`activePokemon`) while its
/// repeated move hits a Cursed Body holder: Disable one turn short.
#[test]
fn an_instructed_move_isnt_the_targets_action_but_it_is_moving() {
    // (a) Rillaboom Instructs Incineroar, which Fake Outs; then Gholdengo
    // Taunts and Encores Incineroar, whose own action is still to come.
    let (mut t, [inc, ril, gho, amo]) = doubles();
    t.apply(&mv(ril, m::INSTRUCT, Some(inc)));
    t.apply(&E::MoveSingleTurn { target: inc, move_id: m::INSTRUCT, of: Some(ril) });
    t.apply(&mv(inc, m::FAKE_OUT, Some(amo)));
    assert_eq!(slot(&t, inc).counters, ObservedCounters::default());
    t.apply(&mv(gho, m::TAUNT, Some(inc)));
    t.apply(&E::VolatileStart { actor: inc, volatile: Volatile::Taunt, move_id: None });
    t.apply(&E::VolatileStart { actor: inc, volatile: Volatile::Encore, move_id: None });
    assert!(!slot(&t, inc).counters.taunt_long && !slot(&t, inc).counters.encore_long);
    // Its own action then is its record's.
    t.apply(&mv(inc, m::FAKE_OUT, Some(gho)));
    assert_eq!(t.side(P1).this_turn[0].move_id, Some(m::FAKE_OUT));
    assert_eq!(t.side(P1).this_turn[0].order, 3);
    // (b) Amoonguss moves, then is Instructed, and its repeated Pollen Puff
    // hits Gholdengo's Cursed Body (as if Gholdengo had it).
    t.apply(&mv(amo, m::POLLEN_PUFF, Some(ril)));
    t.apply(&mv(gho, m::INSTRUCT, Some(amo)));
    t.apply(&E::MoveSingleTurn { target: amo, move_id: m::INSTRUCT, of: Some(gho) });
    t.apply(&mv(amo, m::POLLEN_PUFF, Some(ril)));
    t.apply(&E::StartFrom {
        actor: amo,
        what: "Disable",
        arg: Some(LineArg::Move(m::POLLEN_PUFF)),
        cause: Cause::ability(a::CURSED_BODY).of(ril),
    });
    assert!(slot(&t, amo).counters.disable_short, "moving now: 4 turns");
}

/// A Taunt Magic Bounce sends back starts on its user, which has used its
/// action (it is that action's move): 4 turns (`queue.willMove` null). The
/// reflected move is no move of the bouncer's own.
#[test]
fn a_taunt_magic_bounce_sends_back_lasts_four_turns() {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&mv(inc, m::TAUNT, Some(gho)));
    t.apply(&E::Move {
        user: gho,
        move_id: m::TAUNT,
        target: Some(inc),
        from: Some(MoveFrom::Ability(a::MAGIC_BOUNCE)),
    });
    t.apply(&E::VolatileStart { actor: inc, volatile: Volatile::Taunt, move_id: None });
    assert!(slot(&t, inc).counters.taunt_long);
    assert_eq!(t.side(P2).this_turn[0].action, TurnAction::None, "Gholdengo's own action is still to come");
}

/// The move a Choice item locks its holder into (`ActiveView::choice_move`)
/// is its last move but Struggle: a locked Pokémon out of PP Struggles and
/// stays locked (`choicelock`, Showdown's `data/conditions.js`).
#[test]
fn struggle_keeps_the_choice_move() {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&mv(inc, m::FLARE_BLITZ, Some(gho)));
    t.apply(&E::Upkeep);
    t.apply(&E::Turn { turn: 2 });
    t.apply(&mv(inc, m::STRUGGLE, Some(gho)));
    assert_eq!(slot(&t, inc).last_move, Some(m::STRUGGLE));
    assert_eq!(slot(&t, inc).choice_move, Some(m::FLARE_BLITZ));
}

/// A weather or terrain set by a Pokémon that had lost its item already:
/// its rock or Terrain Extender didn't extend it (the view keeps that it
/// was lost then: `weather_setter_lost`, `terrain_setter_lost`); one lost
/// after doesn't change the flag.
#[test]
fn a_weather_or_terrain_set_after_its_setter_lost_its_item_is_flagged() {
    let (mut t, [inc, ril, ..]) = doubles();
    t.apply(&E::WeatherFrom { weather: Weather::Rain, cause: Cause::ability(a::DRIZZLE).of(inc) });
    assert!(!t.field.weather_setter_lost);
    let lose = |who| E::ItemEnd {
        actor: who,
        item: i::DAMP_ROCK,
        note: ItemEndNote::None,
        from: Some(EffectName::Move(m::KNOCK_OFF)),
        of: None,
    };
    t.apply(&lose(inc));
    assert!(!t.field.weather_setter_lost, "lost after it set the rain");
    assert_eq!(t.field.weather_setter_item, ItemState::Unknown, "its item as the rain started");
    t.apply(&mv(inc, m::RAIN_DANCE, None));
    t.apply(&E::Weather { weather: Some(Weather::Rain), upkeep: false });
    assert!(t.field.weather_setter_lost, "set after the loss");
    assert!(matches!(t.field.weather_setter_item, ItemState::Gone { item: Some(i::DAMP_ROCK), .. }));
    t.apply(&lose(ril));
    t.apply(&E::FieldStartFrom {
        effect: FieldEffect::Terrain(Terrain::Grassy),
        cause: Cause::ability(a::GRASSY_SURGE).of(ril),
    });
    assert!(t.field.terrain_setter_lost);
    // A screen next turn (its user's action then).
    t.apply(&E::Upkeep);
    t.apply(&E::Turn { turn: 2 });
    t.apply(&mv(inc, m::REFLECT, None));
    t.apply(&E::SideStart { side: P1, condition: SideCondition::Reflect });
    assert!(t.side(P1).screen_setter_lost(SideCondition::Reflect));
    assert!(matches!(t.side(P1).screen_setter_item(SideCondition::Reflect), ItemState::Gone { .. }));
}

#[test]
fn volatiles_counters_and_links() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    t.apply(&E::VolatileStart { actor: gho, volatile: Volatile::Taunt, move_id: None });
    t.apply(&mv(ril, m::FAKE_OUT, Some(amo)));
    t.apply(&E::VolatileStart { actor: amo, volatile: Volatile::Encore, move_id: None });
    // Durations count the residuals (`|upkeep|`), not `|turn|` lines.
    t.apply(&E::Upkeep);
    t.apply(&E::Turn { turn: 2 });
    t.apply(&E::Turn { turn: 3 });
    assert_eq!(slot(&t, gho).counters.taunt_turns, 1);
    t.apply(&E::Upkeep);
    assert_eq!(slot(&t, gho).counters.taunt_turns, 2);
    t.apply(&E::VolatileEnd { actor: gho, volatile: Volatile::Taunt });
    assert!(!slot(&t, gho).volatiles.has(PublicVolatiles::TAUNT));
    // Mean Look's trap ends when its user leaves.
    t.apply(&mv(inc, m::MEAN_LOOK, Some(gho)));
    t.apply(&E::EffectStart { actor: gho, effect: VolatileEffect::Trapped, of: None, from: None });
    assert!(slot(&t, gho).volatiles.has(PublicVolatiles::TRAPPED));
    t.apply(&switch(mon(P1, 0, 3, s::GARCHOMP), 100, 100));
    assert!(!slot(&t, gho).volatiles.has(PublicVolatiles::TRAPPED));
    // Stockpile layers, Perish counts.
    t.apply(&E::ManipulationStart { actor: ril, volatile: ManipulationVolatile::Stockpile(2), silent: false });
    assert_eq!(slot(&t, ril).counters.stockpile, 2);
    t.apply(&E::PerishCount { actor: ril, count: 3, silent: true });
    assert_eq!(slot(&t, ril).counters.perish, 3);
    // Cursed Body's Disable names the move.
    t.apply(&E::StartFrom {
        actor: ril,
        what: "Disable",
        arg: Some(LineArg::Move(m::FAKE_OUT)),
        cause: Cause::ability(a::CURSED_BODY).of(amo),
    });
    assert_eq!(slot(&t, ril).disabled_move, Some(m::FAKE_OUT));
    assert_eq!(row(&t, amo).ability, AbilityState::Known(a::CURSED_BODY));
}

/// Octolock traps its target until its `-end` (at the residuals after its
/// user left), whoever else trapped it and left; Baton Pass passes it.
#[test]
fn octolock_traps_until_it_ends() {
    let (mut t, [inc, ril, gho, _]) = doubles();
    let trapped = |t: &PublicTracker| slot(t, gho).volatiles.has(PublicVolatiles::TRAPPED);
    t.apply(&E::EffectStart { actor: gho, effect: VolatileEffect::Octolock, of: Some(ril), from: None });
    assert!(trapped(&t) && slot(&t, gho).octolocked);
    // Its user leaving doesn't end it: the residual's `-end` does.
    t.apply(&switch(mon(P1, 1, 4, s::GARCHOMP), 100, 100));
    assert!(trapped(&t));
    t.apply(&E::EffectEnd { actor: gho, effect: VolatileEffect::Octolock, silent: true });
    assert!(!trapped(&t) && !slot(&t, gho).octolocked);
    // Under a Mean Look whose user leaves, it still holds.
    t.apply(&mv(inc, m::MEAN_LOOK, Some(gho)));
    t.apply(&E::EffectStart { actor: gho, effect: VolatileEffect::Trapped, of: None, from: None });
    t.apply(&E::EffectStart { actor: gho, effect: VolatileEffect::Octolock, of: Some(inc), from: None });
    t.apply(&switch(mon(P1, 0, 3, s::GHOLDENGO), 100, 100));
    assert!(trapped(&t), "Octolock holds it");
    // Baton Pass passes it.
    t.apply(&switch_from(mon(P2, 0, 5, s::GARCHOMP), m::BATON_PASS));
    assert!(trapped(&t) && slot(&t, gho).octolocked);
}

/// Leech Seed keeps the seeder's slot (Showdown's `sourceSlot`: the `|move|`
/// line's user, a Magic Bounce user for a move it sent back), a partial
/// trap its `[of]` source and move; Baton Pass passes both, their ends
/// clear them.
#[test]
fn leech_seed_and_partial_traps_keep_their_source() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    t.apply(&mv(amo, m::LEECH_SEED, Some(inc)));
    t.apply(&E::EffectStart { actor: inc, effect: VolatileEffect::LeechSeed, of: None, from: None });
    assert_eq!(slot(&t, inc).leech_seed_slot, Some((P2, 1)));
    // Sent back by Magic Bounce: the bouncer seeds its user.
    t.apply(&mv(inc, m::LEECH_SEED, Some(gho)));
    t.apply(&E::Move {
        user: gho,
        move_id: m::LEECH_SEED,
        target: Some(ril),
        from: Some(MoveFrom::Ability(a::MAGIC_BOUNCE)),
    });
    t.apply(&E::EffectStart { actor: ril, effect: VolatileEffect::LeechSeed, of: None, from: None });
    assert_eq!(slot(&t, ril).leech_seed_slot, Some((P2, 0)));
    // Fire Spin from Gholdengo (`-activate|...|move: Fire Spin|[of] ...`).
    t.apply(&mv(gho, m::FIRE_SPIN, Some(ril)));
    let fire_spin = VolatileEffect::PartiallyTrapped(m::FIRE_SPIN);
    t.apply(&E::EffectStart { actor: ril, effect: fire_spin, of: Some(gho), from: None });
    let gholdengo = t.row_of_actor(&gho).unwrap();
    assert_eq!(slot(&t, ril).partial_trap, Some((P2, gholdengo, m::FIRE_SPIN)));
    // Its source leaving doesn't end it (a residual does, silently).
    t.apply(&switch(mon(P2, 0, 4, s::GARCHOMP), 100, 100));
    assert_eq!(slot(&t, ril).partial_trap, Some((P2, gholdengo, m::FIRE_SPIN)));
    // Baton Pass passes both, with their sources.
    let gar = mon(P1, 1, 3, s::GARCHOMP);
    t.apply(&switch_from(gar, m::BATON_PASS));
    assert_eq!(slot(&t, gar).leech_seed_slot, Some((P2, 0)));
    assert_eq!(slot(&t, gar).partial_trap, Some((P2, gholdengo, m::FIRE_SPIN)));
    t.apply(&E::EffectEnd { actor: gar, effect: fire_spin, silent: true });
    t.apply(&E::EffectEnd { actor: gar, effect: VolatileEffect::LeechSeed, silent: false });
    assert_eq!((slot(&t, gar).leech_seed_slot, slot(&t, gar).partial_trap), (None, None));
    assert!(!slot(&t, gar).volatiles.has(PublicVolatiles::LEECH_SEED | PublicVolatiles::PARTIALLY_TRAPPED));
}

#[test]
fn protect_chains_across_turns() {
    let (mut t, [inc, ..]) = doubles();
    for turn in 2..5 {
        t.apply(&mv(inc, m::PROTECT, None));
        t.apply(&E::ProtectStart { actor: inc, move_id: m::PROTECT });
        t.apply(&E::Upkeep);
        t.apply(&E::Turn { turn });
    }
    assert_eq!(slot(&t, inc).counters.protect_chain, 3);
    // A protection that fails ends the chain at once.
    t.apply(&mv(inc, m::PROTECT, None));
    t.apply(&E::Fail { user: inc });
    assert_eq!(slot(&t, inc).counters.protect_chain, 0);
    t.apply(&E::Upkeep);
    t.apply(&E::Turn { turn: 5 });
    // A turn without one ends it at the residuals.
    t.apply(&mv(inc, m::PROTECT, None));
    t.apply(&E::ProtectStart { actor: inc, move_id: m::PROTECT });
    t.apply(&E::Upkeep);
    t.apply(&E::Turn { turn: 6 });
    t.apply(&mv(inc, m::FAKE_OUT, None));
    assert_eq!(slot(&t, inc).counters.protect_chain, 1);
    t.apply(&E::Upkeep);
    assert_eq!(slot(&t, inc).counters.protect_chain, 0);
}

#[test]
fn field_and_side_conditions_record_their_setter() {
    let (mut t, [inc, ril, gho, _]) = doubles();
    let at = |t: &PublicTracker, x: Actor| Some((x.pos.side, t.row_of_actor(&x).unwrap()));
    // An ability's `[of]` holder.
    t.apply(&E::WeatherFrom { weather: Weather::Sun, cause: Cause::ability(a::DROUGHT).of(gho) });
    assert_eq!(t.field.weather_setter, at(&t, gho));
    // A move's user.
    t.apply(&mv(inc, m::RAIN_DANCE, None));
    t.apply(&E::Weather { weather: Some(Weather::Rain), upkeep: false });
    assert_eq!(t.field.weather_setter, at(&t, inc));
    t.apply(&mv(ril, m::REFLECT, None));
    t.apply(&E::SideStart { side: P1, condition: SideCondition::Reflect });
    assert_eq!(t.side(P1).screen_setter(SideCondition::Reflect), at(&t, ril).map(|(_, r)| r));
    t.apply(&mv(gho, m::GRASSY_TERRAIN, None));
    t.apply(&E::FieldStart { effect: FieldEffect::Terrain(Terrain::Grassy), of: None });
    assert_eq!(t.field.terrain_setter, at(&t, gho));
    // Ended: no setter.
    t.apply(&E::Weather { weather: None, upkeep: false });
    t.apply(&E::FieldEnd { effect: FieldEffect::Terrain(Terrain::Grassy) });
    assert_eq!((t.field.weather_setter, t.field.terrain_setter), (None, None));
}

#[test]
fn field_and_sides_count_turns_elapsed() {
    let (mut t, [inc, ..]) = doubles();
    t.apply(&E::WeatherFrom { weather: Weather::Sun, cause: Cause::ability(a::DROUGHT).of(inc) });
    assert_eq!(row(&t, inc).ability, AbilityState::Known(a::DROUGHT));
    t.apply(&E::FieldStart { effect: FieldEffect::Terrain(Terrain::Grassy), of: None });
    t.apply(&E::FieldStart { effect: FieldEffect::TrickRoom, of: Some(inc) });
    t.apply(&E::SideStart { side: P2, condition: SideCondition::Spikes });
    t.apply(&E::SideStart { side: P2, condition: SideCondition::Spikes });
    t.apply(&E::SideStart { side: P1, condition: SideCondition::Reflect });
    for _ in 0..2 {
        t.apply(&E::Weather { weather: Some(Weather::Sun), upkeep: true });
        t.apply(&E::Upkeep);
    }
    let f = t.field;
    assert_eq!(
        (f.weather, f.weather_turns, f.terrain, f.terrain_turns, f.trick_room),
        (Some(Weather::Sun), 2, Some(Terrain::Grassy), 2, Some(2))
    );
    assert_eq!(t.side(P2).condition(SideCondition::Spikes), ConditionView { layers: 2, turns: 2 });
    t.apply(&E::SwapSideConditions);
    assert_eq!(t.side(P2).condition(SideCondition::Reflect).layers, 1);
    assert_eq!(t.side(P1).condition(SideCondition::Spikes).layers, 2);
    t.apply(&E::FieldEnd { effect: FieldEffect::TrickRoom });
    assert_eq!(t.field.trick_room, None);
}

#[test]
fn illusion_breaking_moves_facts_to_the_pokemon_behind() {
    let mut t = PublicTracker::for_format(&Format::gen9_singles());
    let lead = mon(P1, 0, 0, s::INCINEROAR);
    let foe = mon(P2, 0, 0, s::GARCHOMP);
    t.apply(&switch(lead, 200, 200));
    t.apply(&switch(foe, 200, 200));
    t.apply(&E::Faint { actor: lead });
    // Zoroark (id 1) comes in as Rillaboom (id 2), never seen before.
    let disguise = mon(P1, 0, 2, s::RILLABOOM);
    t.apply(&switch(disguise, 150, 150));
    t.apply(&mv(disguise, m::NIGHT_DAZE, Some(foe)));
    t.apply(&E::Damage { target: foe, hp: 120, max_hp: 200, status: Status::None, source: DamageSource::Move });
    t.apply(&E::Damage {
        target: disguise,
        hp: 90,
        max_hp: 150,
        status: Status::None,
        source: DamageSource::Effect(Cause::item(i::LIFE_ORB)),
    });
    assert_eq!(t.side(P1).reveal_order.as_ref(), [0, 1]);
    let zoroark = mon(P1, 0, 1, s::ZOROARK);
    t.apply(&E::Replace { actor: zoroark, species: s::ZOROARK, level: 50, gender: Gender::Male });
    t.apply(&E::EffectStartEnd {
        actor: zoroark,
        start: false,
        what: LineArg::Text("Illusion"),
        effect: None,
        cause: None,
        silent: false,
    });
    let side = t.side(P1);
    // The disguise's record is as before: never seen.
    let r = side.mon(1).unwrap();
    assert!(!r.is_revealed() && r.moves.is_empty() && r.item == ItemState::Unknown && !r.hp.is_known());
    // Zoroark's has what it did.
    let z = row(&t, zoroark);
    assert_eq!(z.species, s::ZOROARK);
    assert!(z.is_active() && z.revealed(m::NIGHT_DAZE).is_some());
    assert_eq!(z.item, ItemState::Known(i::LIFE_ORB));
    assert_eq!(z.hp.hp, 60);
    assert_eq!(z.base_ability, AbilityState::Known(a::ILLUSION));
    assert_eq!(t.side(P1).active(0).map(|(r, _)| r), t.row_of_actor(&zoroark));
    assert_eq!(t.side(P1).reveal_order.as_ref(), [0, 2]);
}

/// D: in doubles the disguise can be the active ally; when its record was
/// created by the disguise, breaking the Illusion leaves it the ally's
/// (view-conformance, gen9doubles 4,526,630).
#[test]
fn illusion_as_the_active_ally() {
    let (mut t, [_, _, _, amo]) = doubles();
    t.apply(&E::Faint { actor: amo });
    // Zoroark (id 2) comes in as Arcanine (id 3), then Arcanine itself.
    t.apply(&switch(mon(P2, 1, 3, s::ARCANINE_HISUI), 133, 133));
    t.apply(&switch(mon(P2, 0, 3, s::ARCANINE_HISUI), 192, 192));
    let zoroark = mon(P2, 1, 2, s::ZOROARK_HISUI);
    t.apply(&E::Replace { actor: zoroark, species: s::ZOROARK_HISUI, level: 50, gender: Gender::Male });
    let (row, arcanine) = t.side(P2).active(0).expect("filled");
    assert_eq!((arcanine.species, arcanine.level), (s::ARCANINE_HISUI, 50));
    assert!(arcanine.is_active() && Some(row) != t.row_of_actor(&zoroark));
}

/// D: an ability a line changed on the active ally an Illusion showed
/// (Entrainment, Mummy) stays the ally's when the Illusion breaks
/// (view-conformance, championsvgc 12,950,584).
#[test]
fn illusion_as_the_active_ally_keeps_its_changed_ability() {
    let (mut t, [inc, _, _, amo]) = doubles();
    t.apply(&E::Faint { actor: amo });
    t.apply(&switch(mon(P2, 1, 3, s::ARCANINE_HISUI), 133, 133));
    let arcanine = mon(P2, 0, 3, s::ARCANINE_HISUI);
    t.apply(&switch(arcanine, 192, 192));
    t.apply(&E::AbilityReplaced { holder: inc, ability: a::MUMMY, target: arcanine, old: a::ROCK_HEAD });
    let zoroark = mon(P2, 1, 2, s::ZOROARK_HISUI);
    t.apply(&E::Replace { actor: zoroark, species: s::ZOROARK_HISUI, level: 50, gender: Gender::Male });
    let (_, ally) = t.side(P2).active(0).expect("filled");
    assert_eq!((ally.ability, ally.base_ability), (AbilityState::Known(a::MUMMY), AbilityState::Known(a::ROCK_HEAD)));
    assert_ne!(row(&t, zoroark).ability, AbilityState::Known(a::MUMMY));
}

/// D: a move a Metronome called that Gravity stops isn't the caller's;
/// nor is a charged move it called, released without `[from]lockedmove`
/// (view-conformance, gen9singles 4,535,016, gen9doubles 4,535,387).
#[test]
fn called_moves_stopped_or_released() {
    let (mut t, [inc, ril, gho, _]) = doubles();
    t.apply(&mv(inc, m::METRONOME, Some(inc)));
    t.apply(&E::GravityCant { actor: inc, move_id: m::BOUNCE });
    assert!(row(&t, inc).revealed(m::BOUNCE).is_none());
    t.apply(&E::Blank);
    t.apply(&mv(ril, m::METRONOME, Some(ril)));
    t.apply(&E::Move { user: ril, move_id: m::DIG, target: None, from: Some(MoveFrom::Move(m::METRONOME)) });
    t.apply(&E::Prepare { user: ril, move_id: m::DIG });
    t.apply(&E::Blank);
    t.apply(&mv(ril, m::DIG, Some(gho)));
    assert!(row(&t, ril).revealed(m::DIG).is_none());
    assert!(row(&t, inc).revealed(m::METRONOME).is_some());
    // Nor a lock-in move it called, continued without `[from]lockedmove`
    // (gen9singles 4,536,897).
    t.apply(&E::Blank);
    t.apply(&mv(inc, m::METRONOME, Some(inc)));
    t.apply(&E::Move { user: inc, move_id: m::OUTRAGE, target: Some(gho), from: Some(MoveFrom::Move(m::METRONOME)) });
    t.apply(&E::Blank);
    t.apply(&E::MoveNull { user: inc, move_id: m::OUTRAGE, from: None });
    t.apply(&E::Cant { actor: inc, reason: CantReason::Disable(m::OUTRAGE) });
    assert!(row(&t, inc).revealed(m::OUTRAGE).is_none());
    // A Dancer copy confusion stopped ends with its action (gen9singles
    // 4,536,953).
    t.apply(&E::ActivateEffect { actor: gho, effect: EffectName::Ability(a::DANCER), args: [None, None], of: None });
    t.apply(&E::Blank);
    let other = mon(P2, 0, 4, s::ARCANINE);
    t.apply(&switch(other, 100, 100));
    t.apply(&mv(other, m::FLAMETHROWER, Some(inc)));
    assert_eq!(row(&t, other).ability, AbilityState::Unknown);
}

#[test]
fn team_preview_rows_and_wildcards() {
    let format = Format::gen9_vgc();
    let mut t = PublicTracker::for_format(&format);
    t.apply(&E::Poke { side: P1, species: s::URSHIFU_RAPID_STRIKE, level: 50, gender: Gender::Male });
    t.apply(&E::Poke { side: P1, species: s::INCINEROAR, level: 50, gender: Gender::Male });
    let r = t.side(P1).mon(0).unwrap();
    assert!(r.flags.has(MonFlags::FORME_HIDDEN));
    assert_eq!(r.species, s::URSHIFU);
    t.apply(&switch(mon(P1, 0, 0, s::URSHIFU_RAPID_STRIKE), 100, 100));
    let r = t.side(P1).mon(0).unwrap();
    assert!(!r.flags.has(MonFlags::FORME_HIDDEN));
    assert_eq!(r.species, s::URSHIFU_RAPID_STRIKE);
    // Without team preview `|poke|` isn't shown: nothing is known.
    let mut t = PublicTracker::for_format(&Format::gen9_singles());
    t.apply(&E::Poke { side: P1, species: s::INCINEROAR, level: 50, gender: Gender::Male });
    assert!(t.side(P1).mons.is_empty());
}

#[test]
fn open_team_sheets_fill_rows() {
    let format = Format::gen9_vgc();
    let mut t = PublicTracker::for_format(&format);
    t.apply(&E::Poke { side: P2, species: s::URSHIFU, level: 50, gender: Gender::Male });
    let mut sheet = OpenSheet::default();
    sheet.mons.push(SheetMon {
        species: s::URSHIFU,
        level: 50,
        gender: Gender::Male,
        item: None,
        ability: a::UNSEEN_FIST,
        moves: [m::WICKED_BLOW, m::PROTECT].into_iter().collect(),
        tera_type: Type::Dark,
        nature: None,
    });
    t.set_open_sheet(P2, sheet);
    let r = t.side(P2).mon(0).unwrap();
    assert_eq!(r.item, ItemState::NoItem);
    assert_eq!(r.base_ability, AbilityState::Known(a::UNSEEN_FIST));
    assert_eq!(r.tera_type, Some(Type::Dark));
    assert_eq!(r.revealed(m::WICKED_BLOW).map(|x| x.uses), Some(0));
    assert_eq!(t.ots[1], Some(sheet));
}

#[test]
fn faints_and_revival_blessing() {
    let (mut t, [inc, ril, ..]) = doubles();
    t.apply(&E::Faint { actor: inc });
    assert_eq!((t.side(P1).fainted, t.side(P1).standing()), (1, 0));
    assert!(row(&t, inc).is_fainted());
    let benched = Actor { active: false, ..inc };
    t.apply(&E::MoveHeal { target: benched, hp: 100, max_hp: 200, status: Status::None, move_id: m::REVIVAL_BLESSING });
    assert_eq!(t.side(P1).fainted, 0);
    assert!(!row(&t, inc).is_fainted());
    let _ = ril;
}

#[test]
fn ally_switch_swaps_slots() {
    let (mut t, [inc, ril, ..]) = doubles();
    t.apply(&E::Boost { target: inc, stat: BoostId::Atk, amount: 1 });
    t.apply(&E::Swap { actor: inc, slot: 1, move_id: m::ALLY_SWITCH });
    assert_eq!(t.side(P1).slots[1].row, t.row_of_actor(&inc));
    assert_eq!(t.side(P1).slots[1].boost(BoostId::Atk), 1);
    assert_eq!(t.side(P1).slots[0].row, t.row_of_actor(&ril));
}

/// D: a move Magic Bounce or Magic Coat sends back skips its user's
/// `onBeforeMove`, which ends Glaive Rush's drawback (view-conformance,
/// championsvgc 10,431,851).
#[test]
fn a_reflected_move_keeps_glaive_rush() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    t.apply(&mv(gho, m::GLAIVE_RUSH, Some(inc)));
    t.apply(&E::EffectStart { actor: gho, effect: VolatileEffect::GlaiveRush, of: None, from: None });
    t.apply(&mv(amo, m::GASTRO_ACID, Some(gho)));
    let bounce = Some(MoveFrom::Ability(a::MAGIC_BOUNCE));
    t.apply(&E::Move { user: gho, move_id: m::GASTRO_ACID, target: Some(amo), from: bounce });
    assert!(slot(&t, gho).volatiles.has(PublicVolatiles::GLAIVE_RUSH));
    t.apply(&E::Move { user: gho, move_id: m::TAUNT, target: Some(ril), from: Some(MoveFrom::Move(m::MAGIC_COAT)) });
    assert!(slot(&t, gho).volatiles.has(PublicVolatiles::GLAIVE_RUSH));
    t.apply(&mv(gho, m::GLAIVE_RUSH, Some(inc)));
    assert!(!slot(&t, gho).volatiles.has(PublicVolatiles::GLAIVE_RUSH));
}

/// D: a skipped recharge's residuals follow the Pokémon through Ally
/// Switch (view-conformance, gen9doubles 10,590,607: a commanding
/// Tatsugiri).
#[test]
fn skipped_recharge_expires_after_ally_switch() {
    let (mut t, [inc, ril, gho, _]) = doubles();
    t.apply(&mv(inc, m::HYPER_BEAM, Some(gho)));
    t.apply(&E::MustRecharge { actor: inc });
    t.apply(&E::Upkeep);
    t.apply(&E::Turn { turn: 2 });
    t.apply(&mv(ril, m::ALLY_SWITCH, Some(ril)));
    t.apply(&E::Swap { actor: ril, slot: 0, move_id: m::ALLY_SWITCH });
    assert!(t.side(P1).slots[1].volatiles.has(PublicVolatiles::MUST_RECHARGE));
    t.apply(&E::Upkeep);
    assert!(!t.side(P1).slots[1].volatiles.has(PublicVolatiles::MUST_RECHARGE));
}

#[test]
fn a_line_that_shows_nothing_changes_nothing() {
    let (mut t, [inc, ..]) = doubles();
    let before = t;
    t.apply(&E::Rng { bound: 16, result: 3 });
    t.apply(&E::Poke { side: P1, species: s::INCINEROAR, level: 50, gender: Gender::Male });
    t.apply(&E::MoveAttr { attr: pikapro::event::MoveAttr::Still });
    assert_eq!(t, before);
    let _ = inc;
}

#[test]
fn baton_pass_and_shed_tail_pass_on_what_they_pass() {
    let mut t = PublicTracker::for_format(&Format::gen9_singles());
    let x = mon(P1, 0, 0, s::INCINEROAR);
    t.apply(&switch(x, 100, 100));
    t.apply(&E::Boost { target: x, stat: BoostId::Atk, amount: 2 });
    t.apply(&E::EffectStart { actor: x, effect: VolatileEffect::Substitute, of: None, from: None });
    t.apply(&E::VolatileStart { actor: x, volatile: Volatile::Encore, move_id: None });
    let y = mon(P1, 0, 1, s::RILLABOOM);
    t.apply(&switch_from(y, m::BATON_PASS));
    let v = &t.side(P1).slots[0];
    assert_eq!(v.boost(BoostId::Atk), 2);
    assert!(v.volatiles.has(PublicVolatiles::SUBSTITUTE));
    assert!(!v.volatiles.has(PublicVolatiles::ENCORE), "Encore is not passed");
    let z = mon(P1, 0, 2, s::GARCHOMP);
    t.apply(&switch_from(z, m::SHED_TAIL));
    let v = &t.side(P1).slots[0];
    assert_eq!(v.boost(BoostId::Atk), 0, "Shed Tail passes the substitute only");
    assert!(v.volatiles.has(PublicVolatiles::SUBSTITUTE));
    t.apply(&switch(x, 100, 100));
    assert!(t.side(P1).slots[0].volatiles.0 == 0);
}

#[test]
fn future_sight_is_aimed_at_a_slot() {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&mv(inc, m::FUTURE_SIGHT, Some(gho)));
    t.apply(&E::FutureMoveStart { user: inc, move_id: m::FUTURE_SIGHT });
    assert_eq!(t.side(P2).future[0], FutureView { move_id: Some(m::FUTURE_SIGHT), turns: 0 });
    t.apply(&E::Upkeep);
    t.apply(&E::Upkeep);
    assert_eq!(t.side(P2).future[0].turns, 2);
    t.apply(&E::FutureMoveEnd { target: gho, move_id: m::FUTURE_SIGHT });
    assert_eq!(t.side(P2).future[0], FutureView::default());
}

#[test]
fn sticky_barb_moves_unseen_on_contact() {
    let (mut t, [inc, _, gho, amo]) = doubles();
    t.apply(&E::Damage {
        target: gho,
        hp: 180,
        max_hp: 200,
        status: Status::None,
        source: DamageSource::Effect(Cause::item(i::STICKY_BARB)),
    });
    assert_eq!(row(&t, gho).item, ItemState::Known(i::STICKY_BARB));
    // A contact move from a Pokémon that may hold nothing: now in doubt.
    t.apply(&mv(inc, m::FLARE_BLITZ, Some(gho)));
    t.apply(&E::Damage { target: gho, hp: 100, max_hp: 200, status: Status::None, source: DamageSource::Move });
    assert_eq!(row(&t, gho).item, ItemState::Unknown);
    let _ = amo;
}

/// Without team preview, a player's own Pokémon get rows as they appear;
/// the ones not seen yet have none, and ids stay the stable index.
#[test]
fn own_rows_and_positions_without_team_preview() {
    let format = Format::gen9_singles();
    let teams = [pikapro::testing::sample_team(&format), pikapro::testing::sample_team(&format)];
    let mut b =
        pikapro::Battle::new(format.clone(), teams, pikapro::BattleOptions::new(pikapro::Seed::fast(4_240_900)))
            .unwrap();
    let mut t = PublicTracker::for_format(&format);
    t.apply_all(b.drain_events().as_slice());
    // P1 switches to its fourth Pokémon.
    let switch = pikapro::Choice::single(pikapro::SlotAction::Switch(3));
    b.submit(P1, switch).unwrap();
    let foe = b.legal_actions(P2).slot(0)[0];
    b.submit(P2, pikapro::Choice::single(foe)).unwrap();
    t.apply_all(b.drain_events().as_slice());
    let own = OwnOverlay::from_battle(&b, P1);
    let view = PlayerView::new(P1, &t, &own);
    assert_eq!(view.own_row(0), Some(0), "the lead appeared first");
    assert_eq!(view.own_row(3), Some(1), "id 3 appeared second");
    assert_eq!(view.own_row(1), None, "not seen yet");
    assert_eq!(view.own_id_of_row(1).and_then(|id| view.own_position(id)), Some(0), "row 1 is on the field");
    assert_eq!(own.mons[0].id, 3);
    assert_eq!(view.own_position(0), Some(3), "the lead went to position 3");
}

/// A sheet shown before the `|poke|` lines fills each row as it appears;
/// Zacian holding a Rusted Sword shows crowned, with Behemoth Blade.
#[test]
fn open_team_sheets_before_preview_and_crowned_formes() {
    let format = Format::gen9_vgc();
    let set = |species, item, moves: &[pikapro_data::MoveId]| {
        pikapro::PokemonSet::new(species).with_level(50).with_item(item).with_moves(moves)
    };
    let team = pikapro::Team::new(vec![
        set(s::ZACIAN, i::RUSTED_SWORD, &[m::IRON_HEAD, m::PROTECT]),
        set(s::INCINEROAR, i::SITRUS_BERRY, &[m::FAKE_OUT]).with_ability(a::INTIMIDATE),
    ]);
    let sheet = OpenSheet::from_team(&team, &format);
    assert_eq!(sheet.mons[0].species, s::ZACIAN_CROWNED);
    assert_eq!(sheet.mons[0].moves.as_ref(), [m::BEHEMOTH_BLADE, m::PROTECT]);
    assert_eq!(sheet.mons[0].nature, None, "only Champions shows the nature");
    let mut t = PublicTracker::for_format(&format);
    t.set_open_sheet(P1, sheet);
    t.apply(&E::Poke { side: P1, species: s::ZACIAN_CROWNED, level: 50, gender: Gender::Genderless });
    t.apply(&E::Poke { side: P1, species: s::INCINEROAR, level: 50, gender: Gender::Male });
    let r = t.side(P1).mon(1).unwrap();
    assert_eq!(r.item, ItemState::Known(i::SITRUS_BERRY));
    assert_eq!(r.base_ability, AbilityState::Known(a::INTIMIDATE));
    assert!(r.revealed(m::FAKE_OUT).is_some());
}

/// A sheet shows the level the battle plays (a Champions set registered at
/// 100 shows 50, as Showdown's validator leaves it) and, from the battle,
/// the gender it drew, as Showdown's `|showteam|` does.
#[test]
fn open_team_sheets_show_the_battle_level_and_drawn_gender() {
    use pikapro::{Battle, BattleOptions, Seed};
    let format = Format::champions_vgc();
    let team = pikapro::testing::sample_team(&format);
    let mut team100 = team.clone();
    for set in &mut team100.sets {
        set.level = 100;
        set.gender = None;
    }
    let sheet = OpenSheet::from_team(&team100, &format);
    assert!(sheet.mons.iter().all(|m| m.level == 50), "{sheet:?}");
    let b = Battle::new(format.clone(), [team100.clone(), team], BattleOptions::new(Seed::fast(7_200_200))).unwrap();
    let sheet = OpenSheet::from_battle(&team100, &b, P1);
    for mon in b.state().side(P1).team.iter() {
        assert_eq!(sheet.mons[usize::from(mon.id)].gender, mon.gender);
    }
    assert!(sheet.mons.iter().any(|m| m.gender != Gender::Genderless), "a drawn gender shows");
}

/// `OwnTeam::from_team` crowns Zacian as the battle's start does: the
/// Crowned forme's species and stats, Behemoth Blade's PP in Iron Head's
/// slot.
#[test]
fn own_team_crowns_as_the_battle_does() {
    use pikapro::{Battle, BattleOptions, Seed};
    let format = Format::gen9_doubles();
    let set = |species, item, moves: &[pikapro_data::MoveId]| {
        pikapro::PokemonSet::new(species).with_level(50).with_item(item).with_moves(moves)
    };
    let team = pikapro::Team::new(vec![
        set(s::ZACIAN, i::RUSTED_SWORD, &[m::IRON_HEAD, m::PROTECT]),
        set(s::INCINEROAR, i::SITRUS_BERRY, &[m::FAKE_OUT]).with_ability(a::INTIMIDATE),
    ]);
    let own = OwnTeam::from_team(&team, &format);
    let b = Battle::new(format.clone(), [team.clone(), team], BattleOptions::new(Seed::fast(7_200_100))).unwrap();
    let zacian = b.state().side(P1).team.iter().find(|p| p.id == 0).unwrap();
    let t = own.get(0).unwrap();
    assert_eq!((t.species, zacian.base_species), (s::ZACIAN_CROWNED, s::ZACIAN_CROWNED));
    assert_eq!(t.moves.as_ref(), [m::BEHEMOTH_BLADE, m::PROTECT]);
    assert_eq!(t.ability, zacian.base_ability);
    let st = zacian.set_stats;
    let stats = [pikapro_data::Stat::Hp, pikapro_data::Stat::Atk, pikapro_data::Stat::Def].map(|x| st[x]);
    assert_eq!(t.stats[..3], stats);
    for (k, slot) in zacian.moves.iter().enumerate() {
        assert_eq!((t.moves[k], t.max_pp[k], t.pp[k]), (slot.id, slot.max_pp, slot.pp));
    }
}

/// Without the team, an active Pokémon whose request shows a transformed
/// move list (an Imposter lead) doesn't give its set's moves: a later
/// request does.
#[test]
fn own_team_skips_copied_moves() {
    let copied = |id| OwnMove { id, pp: 5, max_pp: 5, target: pikapro_data::MoveTarget::Normal, disabled: false };
    let ditto = OwnMon {
        id: 0,
        species: s::DITTO,
        level: 50,
        max_hp: 100,
        hp: 100,
        active: true,
        moves: [m::FLARE_BLITZ, m::KNOCK_OFF].into_iter().collect(),
        ..OwnMon::default()
    };
    let mut own = OwnOverlay { request: OwnRequest::Move, ..OwnOverlay::default() };
    own.mons.push(ditto);
    let moves = OwnMoves::Moves([copied(m::FLARE_BLITZ), copied(m::KNOCK_OFF)].into_iter().collect());
    own.active[0] = Some(OwnActive { moves, ..OwnActive::default() });
    let mut team = OwnTeam::default();
    team.observe(&own);
    let t = team.get(0).unwrap();
    assert_eq!((t.species, t.level, t.moves.len(), t.pp_known), (s::DITTO, 50, 0, false));
    // Benched later: its own move.
    own.mons[0] = OwnMon { active: false, moves: [m::TRANSFORM].into_iter().collect(), ..ditto };
    own.active[0] = None;
    team.observe(&own);
    assert_eq!(team.get(0).unwrap().moves.as_ref(), [m::TRANSFORM]);
}

#[test]
fn the_overlay_reveals_what_refusals_taught() {
    let mut own = OwnOverlay::default();
    own.known_trapped[1] = true;
    own.known_disabled[0] = 0b10;
    let r = own.revealed();
    assert_eq!((r.trapped, r.disabled), ([false, true], [0b10, 0]));
}

#[test]
fn a_series_is_seen_from_the_player_side() {
    let t = PublicTracker::for_format(&Format::champions_vgc());
    let own = OwnOverlay { side: P2, ..OwnOverlay::default() };
    let previous = [t];
    let view = PlayerView::new(P2, &t, &own);
    assert!(view.series.is_none() && view.series_score() == [0, 0]);
    let view = view.with_series(SeriesView::new(3, 2, [1, 0], 0, &previous));
    assert_eq!(view.series_score(), [0, 1], "the player's wins first");
    assert_eq!(view.series.unwrap().previous.len(), 1);
}

/// Showdown's Team Preview rule shows `Urshifu-*` for either forme
/// (`onTeamPreview`, without Species Reveal Clause), so only the types
/// every forme has are known until a switch-in; the Champions mod hides
/// only Xerneas', Zacian's and Zamazenta's formes (all `Past` there).
#[test]
fn wildcard_formes_share_only_their_common_types() {
    let format = Format::gen9_vgc();
    let dex = format.dex;
    let mut t = PublicTracker::for_format(&format);
    t.apply(&E::Poke { side: P2, species: s::URSHIFU_RAPID_STRIKE, level: 50, gender: Gender::Male });
    let r = *t.side(P2).mon(0).unwrap();
    assert!(r.flags.has(MonFlags::FORME_HIDDEN));
    assert_eq!(wildcard_types(dex, r.species), ([Some(Type::Fighting), None], false));
    assert_eq!(wildcard_types(dex, s::ZACIAN), ([Some(Type::Fairy), None], false), "Zacian-Crowned is Fairy/Steel");
    assert_eq!(wildcard_types(dex, s::GRENINJA), ([Some(Type::Water), Some(Type::Dark)], true));
    assert_eq!(wildcard_types(dex, s::DUDUNSPARCE), ([Some(Type::Normal), None], true));
    assert_eq!(wildcard_types(dex, s::SILVALLY), ([None, None], false));
    // Its switch-in shows the forme.
    t.apply(&switch(mon(P2, 0, 0, s::URSHIFU_RAPID_STRIKE), 100, 100));
    assert!(!t.side(P2).mon(0).unwrap().flags.has(MonFlags::FORME_HIDDEN));
    assert_eq!(t.types(P2, 0), Some(TypeSet::dual(Type::Fighting, Type::Water)));
    // Champions shows Urshifu's forme.
    let champions = Format::champions_vgc();
    let mut t = PublicTracker::for_format(&champions);
    t.apply(&E::Poke { side: P2, species: s::URSHIFU_RAPID_STRIKE, level: 50, gender: Gender::Male });
    assert!(!t.side(P2).mon(0).unwrap().flags.has(MonFlags::FORME_HIDDEN));
}

/// The start of a turn's actions (`|`, `|t:|`).
fn actions(t: &mut PublicTracker) {
    t.apply(&E::Blank);
    t.apply(&E::Timestamp);
}

/// The end of a turn: residuals, `|upkeep`, then `|turn|`.
fn end_turn(t: &mut PublicTracker, turn: u16) {
    t.apply(&E::Blank);
    t.apply(&E::Upkeep);
    t.apply(&E::Turn { turn });
}

fn damage(target: Actor, hp: u16, source: DamageSource) -> E {
    E::Damage { target, hp, max_hp: 200, status: Status::None, source }
}

#[test]
fn turn_records_order_actions_and_outcomes() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    // The leads came in before the first turn: forced switch-ins.
    let lead = t.side(P1).last_turn[0];
    assert_eq!((lead.action, lead.forced, lead.order, lead.acted()), (TurnAction::Switch, true, 0, false));
    actions(&mut t);
    // A chosen switch comes first.
    let garchomp = mon(P2, 1, 2, s::GARCHOMP);
    t.apply(&switch(garchomp, 200, 200));
    t.apply(&mv(inc, m::FLAMETHROWER, Some(gho)));
    t.apply(&E::SuperEffective { target: gho, level: 1 });
    t.apply(&E::Crit { target: gho });
    t.apply(&damage(gho, 50, DamageSource::Move));
    t.apply(&damage(inc, 180, DamageSource::Effect(Cause::item(i::LIFE_ORB))));
    t.apply(&E::StatusCant { actor: ril, status: Status::Paralysis });
    t.apply(&mv(gho, m::SHADOW_BALL, Some(ril)));
    t.apply(&E::Protected { target: ril });
    end_turn(&mut t, 2);
    let [p1, p2] = [t.side(P1).last_turn, t.side(P2).last_turn];
    assert_eq!((p2[1].action, p2[1].forced, p2[1].order), (TurnAction::Switch, false, 1));
    let f = p1[0];
    assert_eq!(
        (f.action, f.order, f.move_id, f.outcome),
        (TurnAction::Move, 2, Some(m::FLAMETHROWER), MoveOutcome::Hit)
    );
    assert_eq!(f.target, TurnTarget::Slot { side: P2, slot: 0 });
    assert_eq!((f.crit, f.effect, f.dealt, f.hits), (true, [1, 0], [75, 0], 1));
    assert_eq!((f.taken, f.taken_by), (10, taken_by::ITEM));
    assert_eq!((p1[1].action, p1[1].order, p1[1].cant), (TurnAction::Cant, 3, CantKind::Paralysis));
    let g = p2[0];
    assert_eq!((g.order, g.outcome, g.dealt), (4, MoveOutcome::Protected, [0, 0]));
    assert_eq!((g.taken, g.taken_by), (75, taken_by::MOVE));
    // The next turn starts empty.
    assert_eq!(t.side(P1).this_turn, [TurnRecord::default(); 2]);
    let _ = amo;
}

#[test]
fn turn_records_follow_called_moves() {
    let (mut t, [inc, _, gho, amo]) = doubles();
    actions(&mut t);
    t.apply(&mv(inc, m::METRONOME, Some(inc)));
    t.apply(&E::Move {
        user: inc,
        move_id: m::THUNDERBOLT,
        target: Some(amo),
        from: Some(MoveFrom::Move(m::METRONOME)),
    });
    t.apply(&E::Resisted { target: amo, level: 1 });
    t.apply(&damage(amo, 180, DamageSource::Move));
    // A Dancer's copy isn't its action.
    t.apply(&E::Move {
        user: gho,
        move_id: m::FIERY_DANCE,
        target: Some(inc),
        from: Some(MoveFrom::Ability(a::DANCER)),
    });
    t.apply(&damage(inc, 150, DamageSource::Move));
    end_turn(&mut t, 2);
    let r = t.side(P1).last_turn[0];
    assert_eq!((r.move_id, r.called_by, r.order), (Some(m::THUNDERBOLT), Some(m::METRONOME), 1));
    assert_eq!((r.target, r.effect, r.dealt), (TurnTarget::Slot { side: P2, slot: 1 }, [0, -1], [0, 10]));
    assert_eq!((r.taken, r.taken_by), (25, taken_by::MOVE));
    assert_eq!(t.side(P2).last_turn[0], TurnRecord::default(), "Gholdengo didn't act");
}

#[test]
fn turn_records_of_spread_moves() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    actions(&mut t);
    t.apply(&mv(ril, m::EARTHQUAKE, Some(gho)));
    t.apply(&E::Immune { target: amo });
    t.apply(&E::SuperEffective { target: gho, level: 1 });
    t.apply(&damage(gho, 20, DamageSource::Move));
    t.apply(&damage(inc, 150, DamageSource::Move));
    end_turn(&mut t, 2);
    let r = t.side(P1).last_turn[1];
    assert_eq!((r.target, r.outcome, r.effect, r.dealt), (TurnTarget::Spread, MoveOutcome::Hit, [1, -3], [90, 0]));
    assert_eq!(t.side(P1).last_turn[0].taken, 25, "the ally took it, but it isn't dealt to a foe");
    // A move that missed everything.
    actions(&mut t);
    t.apply(&mv(ril, m::ROCK_SLIDE, Some(gho)));
    t.apply(&E::Miss { user: ril, target: gho });
    t.apply(&E::Miss { user: ril, target: amo });
    end_turn(&mut t, 3);
    assert_eq!(t.side(P1).last_turn[1].outcome, MoveOutcome::Miss);
}

#[test]
fn forced_switch_ins_start_a_record() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    actions(&mut t);
    // U-turn, then its replacement comes in mid-turn.
    t.apply(&mv(inc, m::U_TURN, Some(gho)));
    t.apply(&damage(gho, 150, DamageSource::Move));
    actions(&mut t);
    let chomp = mon(P1, 0, 2, s::GARCHOMP);
    t.apply(&switch(chomp, 200, 200));
    // Eject Button sends out one that hadn't moved.
    t.apply(&mv(gho, m::SHADOW_BALL, Some(ril)));
    t.apply(&damage(ril, 100, DamageSource::Move));
    t.apply(&E::ItemEnd { actor: ril, item: i::EJECT_BUTTON, note: ItemEndNote::None, from: None, of: None });
    actions(&mut t);
    t.apply(&switch(mon(P1, 1, 3, s::ARCANINE), 200, 200));
    // Roar drags one in.
    t.apply(&mv(amo, m::ROAR, Some(chomp)));
    t.apply(&E::Drag {
        actor: mon(P1, 0, 4, s::SNORLAX),
        team_index: 4,
        level: 50,
        hp: 200,
        max_hp: 200,
        status: Status::None,
        tera: None,
        shown: None,
        gender: Gender::Male,
    });
    // A faint, replaced after the residuals.
    t.apply(&E::Faint { actor: gho });
    t.apply(&E::Blank);
    t.apply(&E::Upkeep);
    actions(&mut t);
    t.apply(&switch(mon(P2, 0, 4, s::TYRANITAR), 200, 200));
    t.apply(&E::Turn { turn: 2 });
    for (side, slot) in [(P1, 0), (P1, 1), (P2, 0)] {
        let r = t.side(side).last_turn[slot];
        assert_eq!((r.action, r.forced, r.order), (TurnAction::Switch, true, 0), "{side:?} {slot}");
        assert!(r.move_id.is_none() && !r.acted());
    }
    let a = t.side(P2).last_turn[1];
    assert_eq!((a.action, a.move_id, a.order), (TurnAction::Move, Some(m::ROAR), 3));
}

#[test]
fn turn_records_stay_with_the_slot_through_illusion() {
    let mut t = PublicTracker::for_format(&Format::gen9_singles());
    let foe = mon(P2, 0, 0, s::GARCHOMP);
    let disguise = mon(P1, 0, 2, s::RILLABOOM);
    t.apply(&switch(disguise, 150, 150));
    t.apply(&switch(foe, 200, 200));
    t.apply(&E::Turn { turn: 1 });
    actions(&mut t);
    t.apply(&mv(disguise, m::NIGHT_DAZE, Some(foe)));
    t.apply(&damage(foe, 120, DamageSource::Move));
    let zoroark = mon(P1, 0, 1, s::ZOROARK);
    t.apply(&E::Replace { actor: zoroark, species: s::ZOROARK, level: 50, gender: Gender::Male });
    end_turn(&mut t, 2);
    let r = t.side(P1).last_turn[0];
    assert_eq!((r.move_id, r.order, r.dealt), (Some(m::NIGHT_DAZE), 1, [40, 0]));
}

/// A sleeping Pokémon's Sleep Talk is its action (its `cant|slp` comes
/// first), and the move it called is its record's move.
#[test]
fn sleep_talk_is_the_action_of_a_sleeping_pokemon() {
    let (mut t, [inc, _, gho, _]) = doubles();
    actions(&mut t);
    let slp = Status::Sleep { turns: 2 };
    t.apply(&E::StatusCant { actor: inc, status: slp });
    t.apply(&mv(inc, m::SLEEP_TALK, Some(inc)));
    t.apply(&E::Move {
        user: inc,
        move_id: m::FLAMETHROWER,
        target: Some(gho),
        from: Some(MoveFrom::Move(m::SLEEP_TALK)),
    });
    end_turn(&mut t, 2);
    let r = t.side(P1).last_turn[0];
    assert_eq!((r.action, r.order, r.cant), (TurnAction::Move, 1, CantKind::None));
    assert_eq!((r.move_id, r.called_by), (Some(m::FLAMETHROWER), Some(m::SLEEP_TALK)));
}

/// Wish, Healing Wish and Lunar Dance wait in their user's slot.
#[test]
fn healing_wishes_wait_in_their_slot() {
    let (mut t, [inc, ril, ..]) = doubles();
    actions(&mut t);
    t.apply(&mv(inc, m::WISH, Some(inc)));
    let wish = |t: &PublicTracker| t.side(P1).slot_conditions[0];
    assert_eq!((wish(&t).wish, wish(&t).wish_turns_left()), (Some(0), Some(2)));
    end_turn(&mut t, 2);
    assert_eq!(wish(&t).wish_turns_left(), Some(1));
    // A second one fails and changes nothing.
    actions(&mut t);
    t.apply(&mv(inc, m::WISH, Some(inc)));
    t.apply(&E::Fail { user: inc });
    assert_eq!(wish(&t).wish, Some(1));
    // It comes true, healing its slot's Pokémon.
    t.apply(&E::Blank);
    t.apply(&E::WishHeal {
        target: inc,
        hp: 200,
        max_hp: 200,
        status: Status::None,
        wisher: s::INCINEROAR,
        wisher_id: 0,
    });
    assert_eq!(wish(&t).wish, None);
    t.apply(&E::Upkeep);
    t.apply(&E::Turn { turn: 3 });
    // At full HP it ends silently, at the second residual.
    actions(&mut t);
    t.apply(&mv(inc, m::WISH, Some(inc)));
    end_turn(&mut t, 4);
    assert!(wish(&t).wish.is_some());
    end_turn(&mut t, 5);
    assert_eq!(wish(&t).wish, None);
    // Healing Wish: failed (nothing to switch in), then Lunar Dance made.
    actions(&mut t);
    t.apply(&mv(ril, m::HEALING_WISH, Some(ril)));
    t.apply(&E::Fail { user: ril });
    assert!(!t.side(P1).slot_conditions[1].healing_wish);
    actions(&mut t);
    t.apply(&mv(ril, m::LUNAR_DANCE, Some(ril)));
    t.apply(&E::Faint { actor: ril });
    assert!(t.side(P1).slot_conditions[1].lunar_dance);
    let next = mon(P1, 1, 3, s::ARCANINE);
    t.apply(&switch(next, 100, 200));
    t.apply(&E::MoveHeal { target: next, hp: 200, max_hp: 200, status: Status::None, move_id: m::LUNAR_DANCE });
    assert_eq!(t.side(P1).slot_conditions[1], SlotConditions::default());
}

/// Confusion's checks (what ends it) and a
/// frozen Pokémon's attempts to move (Champions thaws it by the third).
#[test]
fn confusion_checks_and_freeze_attempts_are_counted() {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&E::VolatileStart { actor: gho, volatile: Volatile::Confusion, move_id: None });
    t.apply(&E::Upkeep);
    t.apply(&E::Turn { turn: 2 });
    let c = slot(&t, gho).counters;
    assert_eq!((c.confusion_turns, c.confusion_checks), (1, 0), "confused after it moved: no check");
    t.apply(&E::VolatileActivate { actor: gho, volatile: Volatile::Confusion });
    assert_eq!(slot(&t, gho).counters.confusion_checks, 1);
    let frz = Status::Freeze { turns: 3 };
    t.apply(&E::StatusStart { target: inc, status: frz, from_move: None });
    t.apply(&E::StatusCant { actor: inc, status: frz });
    t.apply(&E::StatusCant { actor: inc, status: frz });
    assert_eq!(row(&t, inc).status_turns, 2);
}

/// PP spent stop at the move's maximum, and a Leppa Berry or
/// Lunar Dance gives them back.
#[test]
fn pp_spent_are_capped_and_restored() {
    let (mut t, [inc, _, gho, _]) = doubles();
    let spent = |t: &PublicTracker| row(t, inc).revealed(m::FAKE_OUT).map(|r| r.pp_spent);
    for _ in 0..20 {
        t.apply(&mv(inc, m::FAKE_OUT, Some(gho)));
    }
    assert_eq!(spent(&t), Some(16), "Fake Out has 16 PP at most");
    t.apply(&E::ActivateEffect {
        actor: inc,
        effect: EffectName::Item(i::LEPPA_BERRY),
        args: [Some(LineArg::Move(m::FAKE_OUT)), Some(LineArg::Text("[consumed]"))],
        of: None,
    });
    assert_eq!(spent(&t), Some(6));
    t.apply(&E::MoveHeal { target: inc, hp: 200, max_hp: 200, status: Status::None, move_id: m::LUNAR_DANCE });
    assert_eq!(spent(&t), Some(0));
}

/// A Dancer copy that confusion stopped before its `|move|` line isn't
/// taken for the dancer's own move after.
#[test]
fn a_dance_stopped_by_confusion_leaves_the_next_move_its_own() {
    let (mut t, [inc, _, gho, _]) = doubles();
    t.apply(&E::ActivateEffect { actor: inc, effect: EffectName::Ability(a::DANCER), args: [None, None], of: None });
    t.apply(&E::VolatileActivate { actor: inc, volatile: Volatile::Confusion });
    t.apply(&E::Damage { target: inc, hp: 180, max_hp: 200, status: Status::None, source: DamageSource::Confusion });
    t.apply(&mv(inc, m::FLAMETHROWER, Some(gho)));
    assert_eq!(row(&t, inc).revealed(m::FLAMETHROWER).map(|r| (r.uses, r.pp_spent)), Some((1, 1)));
    assert_eq!(slot(&t, inc).last_move, Some(m::FLAMETHROWER));
}

#[test]
fn guard_and_power_split_mark_both_until_they_leave() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    t.apply(&E::MoveActivate { actor: inc, move_id: m::GUARD_SPLIT, of: Some(gho) });
    assert!(slot(&t, inc).stats_split && slot(&t, gho).stats_split);
    assert!(!slot(&t, ril).stats_split && !slot(&t, amo).stats_split);
    t.apply(&E::MoveActivate { actor: amo, move_id: m::POWER_SPLIT, of: Some(ril) });
    assert!(slot(&t, ril).stats_split && slot(&t, amo).stats_split);
    // Switching out ends it; Baton Pass doesn't pass it.
    t.apply(&switch(mon(P1, 0, 5, s::GARCHOMP), 200, 200));
    assert!(!t.side(P1).slots[0].stats_split);
    t.apply(&switch_from(mon(P2, 0, 4, s::GARCHOMP), m::BATON_PASS));
    assert!(!t.side(P2).slots[0].stats_split);
    assert!(slot(&t, amo).stats_split);
}

/// Symbiosis passing the item its ally just ate (two of the same berry,
/// without Item Clause): the line names it, so the ally holds it again
/// (view-conformance, gen9doubles 36,922,718).
#[test]
fn symbiosis_passes_the_item_just_eaten() {
    let (mut t, [inc, ril, _, _]) = doubles();
    t.apply(&E::ItemEnd { actor: ril, item: i::SITRUS_BERRY, note: ItemEndNote::Eat, from: None, of: None });
    t.apply(&E::ActivateEffect {
        actor: inc,
        effect: EffectName::Ability(a::SYMBIOSIS),
        args: [Some(LineArg::Item(i::SITRUS_BERRY)), None],
        of: Some(ril),
    });
    assert_eq!(row(&t, ril).item, ItemState::Known(i::SITRUS_BERRY));
}

/// A Jaw Lock whose line lost its target to `[still]` (a `-fail` later in
/// the move): its user is trapped by the Pokémon whose `trapped` line
/// follows, never by itself, and freed when that one faints
/// (view-conformance, gen9vgc 36,930,591).
#[test]
fn jaw_lock_without_a_line_target_traps_by_its_partner() {
    let (mut t, [inc, ril, _, _]) = doubles();
    let trapped = |t: &PublicTracker, x| slot(t, x).volatiles.has(PublicVolatiles::TRAPPED);
    t.apply(&mv(inc, m::JAW_LOCK, None));
    t.apply(&E::Damage { target: ril, hp: 100, max_hp: 200, status: Status::None, source: DamageSource::Move });
    t.apply(&E::EffectStart { actor: inc, effect: VolatileEffect::Trapped, of: None, from: None });
    t.apply(&E::EffectStart { actor: ril, effect: VolatileEffect::Trapped, of: None, from: None });
    assert_eq!(slot(&t, inc).trapped_by, Some((P1, 1)));
    assert_eq!(slot(&t, ril).trapped_by, Some((P1, 0)));
    t.apply(&E::Faint { actor: ril });
    assert!(!trapped(&t, inc), "its partner fainted");
}

/// A Jaw Lock whose line lost its target, and whose target shows no
/// `trapped` line (already trapped, a Ghost, or knocked out): the view
/// can't tell whose leaving frees the user, so it doesn't mark it trapped.
/// A user already trapped shows no line and keeps its trapper.
#[test]
fn jaw_lock_without_any_partner_line_leaves_its_user_untrapped() {
    let (mut t, [inc, ril, gho, amo]) = doubles();
    let trapped = |t: &PublicTracker, x| slot(t, x).volatiles.has(PublicVolatiles::TRAPPED);
    t.apply(&mv(inc, m::JAW_LOCK, None));
    t.apply(&E::EffectStart { actor: inc, effect: VolatileEffect::Trapped, of: None, from: None });
    assert!(!trapped(&t, inc));
    // The next move's `trapped` line isn't the partner's.
    t.apply(&mv(amo, m::JAW_LOCK, None));
    t.apply(&E::EffectStart { actor: ril, effect: VolatileEffect::Trapped, of: None, from: None });
    assert!(!trapped(&t, inc));
    // Mean Look on Incineroar, then its Jaw Lock: only the target's line.
    t.apply(&mv(gho, m::MEAN_LOOK, Some(inc)));
    t.apply(&E::EffectStart { actor: inc, effect: VolatileEffect::Trapped, of: None, from: None });
    t.apply(&mv(inc, m::JAW_LOCK, None));
    t.apply(&E::EffectStart { actor: amo, effect: VolatileEffect::Trapped, of: None, from: None });
    assert_eq!(slot(&t, inc).trapped_by, Some((P2, 0)), "still Gholdengo's");
}

/// In singles the only other Pokémon is the partner of a Jaw Lock whose
/// line lost its target, and its leaving frees the user.
#[test]
fn jaw_lock_without_a_line_target_in_singles_traps_by_the_foe() {
    let mut t = PublicTracker::for_format(&Format::gen9_singles());
    let (inc, gho) = (mon(P1, 0, 0, s::INCINEROAR), mon(P2, 0, 0, s::GHOLDENGO));
    t.apply(&switch(inc, 200, 200));
    t.apply(&switch(gho, 200, 200));
    t.apply(&E::Turn { turn: 1 });
    t.apply(&mv(inc, m::JAW_LOCK, None));
    t.apply(&E::EffectStart { actor: inc, effect: VolatileEffect::Trapped, of: None, from: None });
    assert_eq!(slot(&t, inc).trapped_by, Some((P2, 0)));
    t.apply(&E::Faint { actor: gho });
    assert!(!slot(&t, inc).volatiles.has(PublicVolatiles::TRAPPED), "its partner fainted");
}
