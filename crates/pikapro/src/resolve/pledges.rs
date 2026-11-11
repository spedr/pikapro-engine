//! The pledge moves and their combinations.
//!
//! Showdown references (`data/moves.ts`): `firepledge`, `grasspledge`,
//! `waterpledge` (`onPrepareHit`, `onModifyMove`, `basePowerCallback`, and
//! the side conditions they leave: the sea of fire, the swamp and the
//! rainbow); `queue.prioritizeAction` (`sim/battle-queue.ts`).
//!
//! When a Pokémon uses a pledge while an ally still has a different pledge
//! queued, it waits (`-waiting`) and the ally's action moves to the front
//! of the queue with the first pledge as its `sourceEffect`. The ally's
//! pledge then becomes the combination: 150 base power (`-combine`), STAB
//! whatever its type, and a side condition. The waiting pledge is kept in
//! `SideConditions::pledge_combo` until the ally's move is over.

use pikapro_data::gen9::moves as ids;
use pikapro_data::{MoveId, Type};

use super::field::condition_damage;
use super::handlers::{Callback, Handler, Handlers, Holder};
use super::manipulation::has_type;
use super::queue::ActionKind;
use super::{actor, field};
use crate::battle::Battle;
use crate::event::BattleEvent;
use crate::state::{Position, SideCondition};

fn is_pledge(move_id: MoveId) -> bool {
    matches!(move_id, ids::FIRE_PLEDGE | ids::GRASS_PLEDGE | ids::WATER_PLEDGE)
}

/// A pledge's `onPrepareHit`: with an ally's different pledge still queued
/// (`action.pokemon.isActive`, not fainted), that action goes first
/// (`prioritizeAction`: order 3, this move as its `sourceEffect`) and this
/// one waits, ending silently (`null`). False when it waits.
#[inline(always)]
pub(crate) fn prepare_hit(battle: &mut Battle, user: Position, move_id: MoveId) -> bool {
    !is_pledge(move_id) || pledge_prepare_hit(battle, user, move_id)
}

/// [`prepare_hit`] for a pledge.
fn pledge_prepare_hit(battle: &mut Battle, user: Position, move_id: MoveId) -> bool {
    let found = battle.queue.as_slice().iter().copied().find(|action| {
        let ActionKind::Move { move_id: other, .. } = action.kind else { return false };
        let Some(mon) = action.mon else { return false };
        let active =
            super::position_of_mon(battle, mon).and_then(|p| battle.state.at(p)).is_some_and(|m| !m.is_fainted());
        active && mon.side == user.side && is_pledge(other) && other != move_id
    });
    let Some(action) = found else { return true };
    let mon = action.mon.expect("found a move action");
    let mut removed = false;
    battle.queue.retain(|a| {
        let this = !removed && *a == action;
        removed |= this;
        !this
    });
    battle.queue.insert(0, super::queue::Action { order: 3, ..action });
    battle.state.side_mut(user.side).conditions.pledge_combo = Some((mon.id, move_id));
    battle.order.pledge_ignores_ability = battle.move_state.ignore_ability;
    let ally = super::position_of_mon(battle, mon).expect("checked above");
    let (user_actor, ally_actor) = (actor(battle, user), actor(battle, ally));
    battle.emit(BattleEvent::PledgeWaiting { user: user_actor, ally: ally_actor });
    false
}

/// The pledge waiting for this Pokémon: its action's `sourceEffect`, which
/// the `|move|` line names (`[from] move: <pledge>`) and whose
/// `ignoreAbility` the move takes, whatever move it is (Encore can change
/// it). Only for the action's own move: a move called inside it (Sleep
/// Talk's, Metronome's) has the caller as its `sourceEffect`, and a locked
/// move (Instruct's Outrage, whose lock goes on into the action the pledge
/// moved up) `lockedmove`: a pledge used locked doesn't combine.
pub(crate) fn source(battle: &Battle, user: Position) -> Option<MoveId> {
    if battle.order.source_effect.is_some() || super::order::is_locked(battle, user) {
        return None;
    }
    let id = battle.state.at(user)?.id;
    let (ally, source) = battle.state.side(user.side).conditions.pledge_combo?;
    (ally == id).then_some(source)
}

/// The pledge this Pokémon's current pledge combines with: a different
/// pledge as its `sourceEffect`.
pub(crate) fn combo(battle: &Battle, user: Position, move_id: MoveId) -> Option<MoveId> {
    if !is_pledge(move_id) {
        return None;
    }
    source(battle, user).filter(|&source| source != move_id)
}

#[inline]
fn current_combo(battle: &Battle, user: Position) -> Option<(MoveId, MoveId)> {
    let (u, move_id) = battle.active_move?;
    if u != user {
        return None;
    }
    combo(battle, user, move_id).map(|source| (move_id, source))
}

/// Forgets a waiting pledge once another Pokémon of the side moves (the
/// `sourceEffect` belongs to the ally's action only).
pub(crate) fn begin_move(battle: &mut Battle, user: Position) {
    let Some(id) = battle.state.at(user).map(|m| m.id) else { return };
    let conditions = &mut battle.state.side_mut(user.side).conditions;
    if conditions.pledge_combo.is_some_and(|(ally, _)| ally != id) {
        conditions.pledge_combo = None;
    }
}

/// The pledge waiting for the Pokémon at `user` as it starts a move: that
/// move is the prioritized action whose `sourceEffect` the pledge is.
pub(crate) fn waiting_for(battle: &Battle, user: Position) -> Option<(u8, MoveId)> {
    let id = battle.state.at(user)?.id;
    battle.state.side(user.side).conditions.pledge_combo.filter(|&(ally, _)| ally == id)
}

/// After that action's `runMove`, the combination is forgotten, so a later
/// pledge of the same Pokémon (Instruct) is one of its own. A pledge that
/// waited during the move (for another ally) stays.
pub(crate) fn forget(battle: &mut Battle, side: crate::state::SideId, combo: Option<(u8, MoveId)>) {
    let conditions = &mut battle.state.side_mut(side).conditions;
    if combo.is_some() && conditions.pledge_combo == combo {
        conditions.pledge_combo = None;
    }
}

/// The combination's type (`onModifyMove`): Fire with Grass is Fire, Grass
/// with Water is Grass, Water with Fire is Water.
pub(crate) fn modify_move(battle: &Battle, user: Position) -> Option<Type> {
    let (move_id, source) = current_combo(battle, user)?;
    Some(match (move_id, source) {
        (ids::FIRE_PLEDGE | ids::GRASS_PLEDGE, ids::GRASS_PLEDGE | ids::FIRE_PLEDGE) => Type::Fire,
        (ids::GRASS_PLEDGE | ids::WATER_PLEDGE, ids::WATER_PLEDGE | ids::GRASS_PLEDGE) => Type::Grass,
        _ => Type::Water,
    })
}

/// The combination's `basePowerCallback`: `-combine` and 150 base power.
pub(crate) fn base_power(battle: &mut Battle, user: Position) -> Option<u16> {
    current_combo(battle, user)?;
    battle.emit(BattleEvent::PledgeCombine);
    Some(150)
}

/// `forceSTAB` of a combination.
pub(crate) fn force_stab(battle: &Battle, user: Position) -> bool {
    current_combo(battle, user).is_some()
}

/// The combination's side condition, after the hit: the sea of fire or
/// the swamp on the target's side (`move.sideCondition`), the rainbow on
/// the user's (`move.self.sideCondition`).
pub(crate) fn on_hit(battle: &mut Battle, target: Position, user: Position) {
    let Some((move_id, source)) = current_combo(battle, user) else { return };
    let pair = |a, b| (move_id == a && source == b) || (move_id == b && source == a);
    if pair(ids::FIRE_PLEDGE, ids::GRASS_PLEDGE) {
        field::add_side_condition(battle, target.side, "firepledge", user);
    } else if pair(ids::GRASS_PLEDGE, ids::WATER_PLEDGE) {
        field::add_side_condition(battle, target.side, "grasspledge", user);
    } else {
        field::add_side_condition(battle, user.side, "waterpledge", user);
    }
}

/// The rainbow's `onModifyMove`: secondary effects of the side's moves are
/// twice as likely, except a flinch when the user has Serene Grace.
pub(crate) fn secondary_chance(battle: &Battle, user: Position, chance: u32, flinch: bool) -> u32 {
    let serene_grace =
        flinch && super::abilities::has_ability(battle, user, pikapro_data::gen9::abilities::SERENE_GRACE);
    if battle.state.side(user.side).conditions.water_pledge > 0 && !serene_grace { chance * 2 } else { chance }
}

/// The sea of fire's per-Pokémon `onResidual` (order 5, sub-order 1).
pub(crate) fn residual_handlers(battle: &Battle, pos: Position, speed: i32, handlers: &mut Handlers) {
    if battle.state.side(pos.side).conditions.fire_pledge > 0 {
        handlers.push(Handler {
            order: Some(5),
            sub_order: 1,
            speed,
            holder: Holder::SideOn(pos),
            callback: Callback::Plain(sea_of_fire),
            ..Handler::default()
        });
    }
}

/// The sea of fire burns non-Fire types for 1/8 of their max HP.
fn sea_of_fire(battle: &mut Battle, handler: &Handler) -> bool {
    let Holder::SideOn(pos) = handler.holder else { return true };
    if battle.state.side(pos.side).conditions.fire_pledge == 0 {
        return true;
    }
    let Some(mon) = battle.state.at(pos).filter(|m| !m.is_fainted() && !has_type(m, Type::Fire)) else {
        return true;
    };
    let amount = super::hp_fraction(mon.max_hp() as u32, 1, 8);
    condition_damage(battle, pos, amount, SideCondition::FirePledge);
    true
}
