//! Moves that switch Pokémon out, and the switch requests they cause.
//!
//! Showdown references (`sim/battle-actions.ts`, `sim/battle.ts`):
//! `selfSwitch` (U-turn, Volt Switch, Flip Turn, Parting Shot, Teleport)
//! sets `switchFlag` in `runMoveEffects`, and `runAction`'s end-of-action
//! processing turns switch flags into a switch request for the sides
//! concerned (`makeRequest('switch')`); the turn resumes once they choose.
//! `forceSwitch` (Roar, Whirlwind, Dragon Tail, Circle Throw) sets
//! `forceSwitchFlag`, and `runAction` then calls `dragIn`, which picks the
//! replacement with `getRandomSwitchable` (a draw).
//!
//! Contract:
//! - [`handles`], [`on_try`], [`on_hit`]: status moves implemented here
//!   (Teleport, Parting Shot).
//! - [`force_switch_possible`]: `runMoveEffects`' `forceSwitch` check.
//! - [`force_switch`]: Showdown's `forceSwitch` after secondaries.
//! - [`self_switch`]: `runMoveEffects`' `selfSwitch`: flag the user.
//! - [`run_drags`], [`check_fainted`], [`make_switch_request`]: `runAction`'s
//!   end-of-action steps.
//! - [`take_switch_flag`]: `resolveAction` consuming the flag of a Pokémon
//!   that switches out.
//!
//! Elsewhere: Baton Pass and Shed Tail's `copyVolatileFrom` (`order.rs`,
//! `mod.rs`), Chilly Reception (`multiturn.rs`), Revival Blessing
//! (`slots.rs`), the `DragOut` handlers (Ingrain, Suction Cups, Guard Dog,
//! Commander: registries), Emergency Exit (`abilities/`) and Eject Button,
//! Eject Pack, Red Card (`items/`).

use pikapro_data::gen9::moves as ids;
use pikapro_data::{MoveData, MoveId, StatBoosts};

use super::queue::MonRef;
use super::targeting::Targets;
use super::{boosts, switch_in};
use crate::battle::Battle;
use crate::request::Request;
use crate::state::{MAX_ACTIVE, MAX_TEAM, Position, SideId, Status, SwitchFlag, Volatiles};
use crate::util::StackVec;

/// Status moves this module implements (their callbacks are ported here).
pub(crate) fn handles(move_id: MoveId) -> bool {
    move_id == ids::TELEPORT || move_id == ids::PARTING_SHOT
}

/// The move's `onTry`. Teleport fails when nothing can come in.
pub(crate) fn on_try(battle: &Battle, user: Position, move_id: MoveId) -> bool {
    move_id != ids::TELEPORT || can_switch(battle, user.side) > 0
}

/// The move's `onHit` on `target`. Returns false if the move loses its
/// `selfSwitch` (Parting Shot that lowered nothing).
pub(crate) fn on_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    if move_id != ids::PARTING_SHOT {
        return true;
    }
    // Mirror Armor keeps the switch (`!target.hasAbility('mirrorarmor')`).
    boosts::boost(battle, target, StatBoosts::new([-1, 0, -1, 0, 0, 0, 0]), Some(user), false, false)
        || super::abilities::has_ability(battle, target, pikapro_data::gen9::abilities::MIRROR_ARMOR)
}

/// Showdown's `canSwitch`: how many benched Pokémon could come in.
pub(crate) fn can_switch(battle: &Battle, side: SideId) -> usize {
    // `switchable(battle, side).len()`, counted without the list.
    let team = &battle.state.side(side).team;
    let past = team.iter().skip(battle.format.active_per_side()).filter(|m| !m.is_fainted()).count();
    if battle.order.stale_slots == 0 { past } else { past + stuck(battle, side).count_ones() as usize }
}

/// Showdown's `possibleSwitches`: team positions past the active slots that
/// hold a Pokémon that has not fainted.
fn switchable(battle: &Battle, side: SideId) -> StackVec<u8, MAX_TEAM> {
    let team = &battle.state.side(side).team;
    let past = battle.format.active_per_side()..team.len();
    let mut list: StackVec<u8, MAX_TEAM> = past.filter(|&i| !team[i].is_fainted()).map(|i| i as u8).collect();
    if battle.order.stale_slots != 0 {
        let stuck = stuck(battle, side);
        list = (0..team.len() as u8).filter(|&i| stuck & 1 << i != 0 || list.contains(&i)).collect();
    }
    list
}

/// The team positions of the active slots that hold a healthy Pokémon off
/// the field, as bits: only a Pokémon a stale switch benched there
/// (`OrderState::stale_active`). Showdown never counts one
/// (`side.pokemon.slice(side.active.length)`) and throws at the end of
/// that turn; pikapro lets it come in again, or its side could never lose.
#[cold]
#[inline(never)]
fn stuck(battle: &Battle, side: SideId) -> u8 {
    let s = battle.state.side(side);
    let slots = battle.format.active_per_side().min(s.team.len());
    (0..slots).filter(|&i| s.active[i].is_none() && !s.team[i].is_fainted()).fold(0, |bits, i| bits | 1 << i)
}

/// `runMoveEffects`' `forceSwitch` result: whether anything can replace the
/// target. A status move (Roar) that cannot switch anyone fails.
pub(crate) fn force_switch_possible(battle: &Battle, target: Position) -> bool {
    can_switch(battle, target.side) > 0
}

/// Showdown's `forceSwitch`: flags each target that can be dragged out.
/// Returns the targets whose `DragOut` failed a status move (`damage[i] =
/// false`).
pub(crate) fn force_switch(battle: &mut Battle, targets: &Targets, user: Position, data: &MoveData) -> Targets {
    let mut failed = Targets::new();
    for &t in targets.iter() {
        let target_up = battle.state.at(t).is_some_and(|m| m.hp > 0);
        let user_up = battle.state.at(user).is_some_and(|m| m.hp > 0);
        if target_up && user_up && can_switch(battle, t.side) > 0 {
            // `runEvent('DragOut', target, source, move)`: Ingrain, Suction
            // Cups and Guard Dog (`null`: no failure); `false` fails a status move.
            let move_id = super::active_dex_move(battle).map(|(_, m)| m);
            let relay = match move_id {
                Some(m) => super::events::move_event(
                    battle,
                    pikapro_data::Event::DragOut,
                    t,
                    Some(user),
                    m,
                    data,
                    super::events::Relay::True,
                ),
                None => super::run_mon_event_from(battle, pikapro_data::Event::DragOut, t, Some(user)),
            };
            if relay.is_truthy() {
                battle.state.at_mut(t).expect("checked above").force_switch_flag = true;
            } else if relay == super::events::Relay::False && data.category == pikapro_data::MoveCategory::Status {
                super::moves::fail_still(battle, user);
                failed.push(t);
            }
        }
    }
    failed
}

/// `runMoveEffects`' `selfSwitch`: after a move that did something, a user
/// still standing is flagged to switch out. The flag is set even if nothing
/// can come in; `make_switch_request` then clears it.
pub(crate) fn self_switch(battle: &mut Battle, user: Position, move_id: MoveId) {
    // Not while `commanded` (Commander).
    if let Some(mon) = battle.state.at_mut(user).filter(|m| m.hp > 0 && !m.volatiles.has(Volatiles::COMMANDED)) {
        mon.switch_flag = SwitchFlag::Move(move_id);
    }
}

/// The start of `runAction`'s end-of-action processing: every active
/// Pokémon with `forceSwitchFlag` is dragged out (`dragIn`) if it still has
/// HP.
pub(crate) fn run_drags(battle: &mut Battle) {
    // Usually no Pokémon is flagged (after every action): one pass over
    // both sides' filled slots says so.
    if !any_in_slots(battle, |m| m.force_switch_flag) {
        return;
    }
    for side in SideId::BOTH {
        for slot in 0..filled_slots(battle, side) {
            let mon = &mut battle.state.side_mut(side).team[slot];
            if !mon.force_switch_flag {
                continue;
            }
            mon.force_switch_flag = false;
            if mon.hp > 0 {
                drag_in(battle, side, slot);
            }
        }
    }
}

/// Showdown's `dragIn`: a random benched Pokémon (`getRandomSwitchable`,
/// which draws even with one candidate) replaces the one in `slot`.
fn drag_in(battle: &mut Battle, side: SideId, slot: usize) -> bool {
    let candidates = switchable(battle, side);
    if candidates.is_empty() {
        return false;
    }
    let position = *battle.rng.sample(&candidates) as usize;
    let s = battle.state.side(side);
    if s.team[slot].hp == 0 {
        return false;
    }
    // `runEvent('DragOut', oldActive)` again, without a source: an ability
    // gained since the hit (Wandering Spirit's Suction Cups) keeps it in.
    let out = Position::new(side, slot as u8);
    if !super::run_mon_event(battle, pikapro_data::Event::DragOut, out).is_truthy() {
        return false;
    }
    let s = battle.state.side(side);
    let incoming = MonRef { side, id: s.team[position].id };
    switch_in(battle, incoming, slot, None, true);
    true
}

/// Showdown's `checkFainted` (when the queue has run dry): fainted active
/// Pokémon are flagged to be replaced and lose their status (it becomes
/// `fnt`). Until then a fainted Pokémon keeps its status, and its status
/// handlers still take part in handler sorts.
pub(crate) fn check_fainted(battle: &mut Battle) {
    for side in SideId::BOTH {
        for slot in 0..filled_slots(battle, side) {
            let s = battle.state.side_mut(side);
            if s.active[slot].is_none() {
                s.team[slot].switch_flag = SwitchFlag::Fainted;
                if s.team[slot].is_fainted() {
                    s.team[slot].status = Status::None;
                }
            }
        }
    }
}

/// The end of `runAction`: sides with switch-flagged Pokémon get a switch
/// request (`makeRequest('switch')`), which pauses the turn. Flags are
/// dropped on a side with nothing to switch in; a flagged Pokémon still on
/// the field runs `BeforeSwitchOut` now rather than when it leaves. Returns
/// true if a request was made.
pub(crate) fn make_switch_request(battle: &mut Battle) -> bool {
    // Usually no Pokémon is flagged (after every action): one pass over
    // both sides' filled slots says so.
    if !any_in_slots(battle, |m| m.switch_flag != SwitchFlag::None) {
        return false;
    }
    let mut any = false;
    for side in SideId::BOTH {
        let filled = filled_slots(battle, side);
        // Usually no Pokémon is flagged: nothing to do.
        if battle.state.side(side).team.iter().take(filled).all(|m| m.switch_flag == SwitchFlag::None) {
            continue;
        }
        let can_switch = can_switch(battle, side) > 0;
        // A slot waiting for Revival Blessing's choice keeps its flag even
        // with nothing to switch in, and doesn't switch out.
        let reviving: [bool; MAX_ACTIVE] = core::array::from_fn(|slot| {
            slot < filled && super::slots::reviving(battle, Position::new(side, slot as u8))
        });
        let s = battle.state.side_mut(side);
        let mut slots = [false; MAX_ACTIVE];
        for (slot, flag) in slots.iter_mut().enumerate().take(filled) {
            let mon = &mut s.team[slot];
            if mon.switch_flag == SwitchFlag::None {
                continue;
            }
            if !can_switch && !reviving[slot] {
                mon.switch_flag = SwitchFlag::None;
                continue;
            }
            *flag = true;
            if reviving[slot] {
                continue;
            }
            if mon.hp > 0 && !mon.skip_before_switch_out {
                // `runEvent('BeforeSwitchOut')`: only Dynamax and Pursuit
                // handle it, neither in Gen 9 or Champions; the
                // `faintMessages` after it has nothing left to report (the
                // action's were just run, and `Update` and `EmergencyExit`
                // faint no one).
                mon.skip_before_switch_out = true;
            }
        }
        if slots.iter().any(|&f| f) {
            battle.requests[side.index()] = Request::ForceSwitch { slots };
            any = true;
        }
    }
    any
}

/// `resolveAction` for a switch: the outgoing Pokémon's `switchFlag` is
/// cleared; a move's flag is returned as the switch's `sourceEffect`.
pub(crate) fn take_switch_flag(battle: &mut Battle, mon: MonRef) -> Option<MoveId> {
    let side = battle.state.side_mut(mon.side);
    let position = side.position_of(mon.id)?;
    match core::mem::take(&mut side.team[position].switch_flag) {
        SwitchFlag::Move(move_id) => Some(move_id),
        SwitchFlag::None | SwitchFlag::Fainted | SwitchFlag::True => None,
    }
}

/// Active slots that have ever been filled (Showdown's `side.active`,
/// fainted Pokémon included).
fn filled_slots(battle: &Battle, side: SideId) -> usize {
    battle.format.active_per_side().min(battle.state.side(side).team.len())
}

/// Whether `test` holds for a Pokémon in a slot, fainted or not (the team
/// positions below the slots per side, [`filled_slots`]), on either side.
/// (The two slots written out: as a loop over each side's slots, or a
/// `take` of the team's iterator, the test took several times as long.)
#[inline(always)]
pub(crate) fn any_in_slots(battle: &Battle, test: impl Fn(&crate::state::PokemonState) -> bool) -> bool {
    const _: () = assert!(crate::state::MAX_ACTIVE == 2);
    let doubles = battle.format.active_per_side() > 1;
    battle.state.sides.iter().any(|s| s.team.first().is_some_and(&test) || doubles && s.team.get(1).is_some_and(&test))
}
