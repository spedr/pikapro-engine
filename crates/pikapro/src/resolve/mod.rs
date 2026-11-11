//! Turn resolution.
//!
//! This is a port of Pokémon Showdown's control flow (`sim/battle.ts`,
//! `sim/battle-queue.ts`, `sim/battle-actions.ts`), structured so that, with
//! a Showdown-compatible seed, logs and random draws can be compared line by
//! line with the real simulator (see `docs/CONFORMANCE.md`). When in doubt,
//! match Showdown.
//!
//! The flow mirrors Showdown's:
//!
//! - `resolve` is `commitChoices`: refresh stored speeds, turn every
//!   choice into queued actions, sort, run the turn loop.
//! - `turn_loop` queues the turn's `BeforeTurn` and `Residual` actions, and
//!   `run_queue` shifts actions off the queue and runs them until a request
//!   is pending or the queue is empty (end of turn).
//! - `run_action` runs one action, then Showdown's end-of-action
//!   processing: faints, `Update` events, forced-switch requests and the
//!   Gen 8+ dynamic speed re-sort.
//!
//! What is implemented is what `pikapro-conformance`'s pools admit
//! (`species_pool`, `move_pool`, `IMPLEMENTED_ABILITIES`,
//! `IMPLEMENTED_ITEMS`): the fuzzer checks all of it against Showdown.
//!
//! The module is internal: the damage formula, for one, is the move
//! pipeline's (`moves.rs`, `roll_damage_inner`), whose steps are
//! interleaved with the events that modify them, and has no pure form.

mod boosts;
mod effects;
pub(crate) mod endless;
mod field;
mod handlers;
mod moves;
mod protect;
mod queue;
mod secondaries;
mod status;
mod switching;
mod targeting;
mod volatiles;

// The damage formula's callbacks.
mod power;

// Multi-turn moves.
mod multiturn;

// Substitute, trapping, residual volatiles.
mod conditions;
pub(crate) use conditions::{is_trapped, request_target};
pub(crate) use volatiles::{request_shows_usable, request_shows_usable_moves};
// `LegalActions::check` takes what `chooseMove` does.
pub(crate) use targeting::valid_target_loc_in;

// Hazards, side, field and slot conditions.
mod hazards;
mod pledges;
mod slots;

// Stat, HP and type manipulation.
pub(crate) mod manipulation;

// Action order, targeting, move calling.
mod order;

// Abilities and items: the event framework and the dispatch modules.
pub(crate) mod abilities;
pub(crate) mod events;
mod forme;
mod itemmoves;
pub(crate) use itemmoves::own_disable_move;
pub(crate) mod items;
pub(crate) use events::MoveState;

pub(crate) mod mega;
pub(crate) mod tera;
#[cfg(test)]
mod typing_tests;
// What the request data (`request.rs`) reads.
pub(crate) use conditions::is_visibly_trapped;
pub(crate) use field::{gravity, grounded};
pub(crate) use multiturn::locked_move;

pub(crate) use order::OrderState;
pub(crate) use queue::Queue;

use pikapro_data::gen9::moves as move_ids;
use pikapro_data::{MoveId, Stat};

use crate::battle::Battle;
use crate::choice::{Choice, SlotAction};
use crate::event::{Actor, BattleEvent};
pub(crate) use crate::event::{DamageSource, HealSource};
use crate::format::Mechanics;
use crate::request::Request;
use crate::result::{BattleResult, EndReason};
use crate::state::{MAX_ACTIVE, Position, SideId};
use crate::util::StackVec;
use queue::{Action, ActionKind, MonRef};

/// Queues the `start` action. With team preview the battle then waits for
/// team choices; otherwise leads are sent out right away (Showdown's
/// `start()`).
pub(crate) fn start(battle: &mut Battle) {
    queue::add_choice(battle, Action::new(ActionKind::Start, None));
    battle.mid_turn = true;
    if battle.requests.iter().all(|r| *r == Request::Wait) {
        turn_loop(battle);
    }
}

/// Showdown's `commitChoices`: every side's choice becomes queued actions,
/// which are sorted ahead of anything already queued; then the turn loop
/// resumes.
pub(crate) fn resolve(battle: &mut Battle, requests: [Request; 2], choices: [Option<Choice>; 2]) {
    let fresh = update_speed(battle);
    with_fresh_speeds(battle, fresh, |battle| add_choices(battle, requests, choices));
    turn_loop(battle);
}

/// The choices become queued actions, sorted ahead of what is left of the
/// queue (`commitChoices` between `updateSpeed` and `turnLoop`).
fn add_choices(battle: &mut Battle, requests: [Request; 2], choices: [Option<Choice>; 2]) {
    // What is left of the queue (after a mid-turn switch request), set
    // aside; usually nothing, which needs no copy of the whole queue.
    let old = if battle.queue.is_empty() {
        battle.queue.clear();
        None
    } else {
        Some(core::mem::take(&mut battle.queue))
    };
    for side in SideId::BOTH {
        let Some(choice) = choices[side.index()] else { continue };
        match (requests[side.index()], choice) {
            (Request::TeamPreview { bring }, Choice::Team(order)) => {
                for (index, &position) in order.iter().take(bring as usize).enumerate() {
                    let mon = mon_ref(battle, side, position as usize);
                    let kind = ActionKind::Team { index: index as u8 };
                    let action = Action { priority: -(index as i8), ..Action::new(kind, Some(mon)) };
                    queue::add_choice(battle, action);
                }
            }
            (Request::Move | Request::ForceSwitch { .. }, Choice::Actions(actions)) => {
                let forced = matches!(requests[side.index()], Request::ForceSwitch { .. });
                for (slot, action) in actions.iter().enumerate() {
                    add_slot_action(battle, side, slot, *action, forced);
                }
            }
            _ => unreachable!("choice validated against its request"),
        }
    }
    queue::sort(battle);
    if let Some(old) = &old {
        for &action in old.as_slice() {
            battle.queue.push(action);
        }
    }
}

/// Runs `f`, which resolves actions (`resolveAction`, `getActionSpeed`)
/// right after `update_speed` or `update_speed_of` returned `fresh`, with
/// those slots' stored speeds standing for computing them again
/// (`Battle::fresh_speeds`). Resolving actions changes nothing a speed
/// depends on, except a `FractionalPriority` handler (Custap Berry eats
/// itself, which can start Unburden): with one in the battle, every speed
/// is computed. Nor with a Pokémon revived in its slot, whose event masks
/// depend on the queue, which `commitChoices` empties meanwhile.
///
/// This holds only while every `ModifySpe`, `ModifyBoost` and
/// `ModifyPriority` handler stays pure (reads state, draws nothing): a
/// handler with a side effect there must turn this off, like
/// `FractionalPriority`. Debug builds recompute each stored speed and
/// assert it matches.
fn with_fresh_speeds<R>(battle: &mut Battle, fresh: u8, f: impl FnOnce(&mut Battle) -> R) -> R {
    battle.fresh_speeds =
        if events::filtered(battle, pikapro_data::Event::FractionalPriority) && !battle.order.revived_in_slot {
            fresh
        } else {
            0
        };
    let result = f(battle);
    battle.fresh_speeds = 0;
    result
}

fn add_slot_action(battle: &mut Battle, side: SideId, slot: usize, action: SlotAction, forced: bool) {
    // A Pokémon locked into a move (Outrage, the second turn
    // of Fly, ...) uses it (`chooseMove` with `getLockedMove`).
    if let (SlotAction::Move { .. }, Some((index, move_id, target_loc))) =
        (action, multiturn::locked_action(battle, side, slot))
    {
        let kind = ActionKind::Move { slot: index, move_id, target_loc, gimmick: None };
        let mon = mon_ref(battle, side, slot);
        queue::add_choice(battle, Action::new(kind, Some(mon)));
        return;
    }
    let kind = match action {
        SlotAction::Pass => return,
        // Revival Blessing's switch request picks a fainted Pokémon
        // (`chooseSwitch` clears the user's `switchFlag` at once).
        SlotAction::Switch(position) if forced && slots::reviving(battle, Position::new(side, slot as u8)) => {
            battle.state.side_mut(side).team[slot].switch_flag = crate::state::SwitchFlag::None;
            ActionKind::RevivalBlessing { target: mon_ref(battle, side, position as usize) }
        }
        SlotAction::Switch(position) => {
            let target = mon_ref(battle, side, position as usize);
            if forced { ActionKind::InstaSwitch { target, from: None } } else { ActionKind::Switch { target } }
        }
        // A recharging Pokémon's only option is Showdown's `recharge`
        // locked move, aimed where its last move was.
        SlotAction::Move { .. } if volatiles::must_recharge(battle, side, slot) => {
            let target_loc = battle.state.side(side).team[slot].last_move_target_loc;
            ActionKind::Move { slot: 0, move_id: volatiles::RECHARGE, target_loc, gimmick: None }
        }
        SlotAction::Move { index, target, gimmick } => {
            let user = &battle.state.side(side).team[slot];
            // With no usable move, any move choice means Struggle.
            let usable = user.has_usable_move();
            let move_id = if usable { user.moves[index as usize].id } else { move_ids::STRUGGLE };
            // Struggle ignores the target a choice names (a move only
            // Imprison disables still shows one).
            let target_loc = if move_id == move_ids::STRUGGLE { 0 } else { targeting::loc_of_choice(target) };
            // `chooseMove`'s `!moves.length` branch pushes Struggle without
            // the choice's `mega` / `terastallize` (a request showing moves
            // only Imprison disables offers them).
            let gimmick = if usable { gimmick } else { None };
            ActionKind::Move { slot: index, move_id, target_loc, gimmick }
        }
    };
    let mon = mon_ref(battle, side, slot);
    queue::add_choice(battle, Action::new(kind, Some(mon)));
}

/// Showdown's `turnLoop`.
fn turn_loop(battle: &mut Battle) {
    // `add(''); add('t:', ...)`.
    battle.emit(BattleEvent::Blank);
    battle.emit(BattleEvent::Timestamp);
    if !battle.mid_turn {
        queue::insert_choice(battle, Action::new(ActionKind::BeforeTurn, None), false);
        queue::add_choice(battle, Action::new(ActionKind::Residual, None));
        battle.mid_turn = true;
    }
    run_queue(battle);
}

/// The rest of `turnLoop`: runs the queued actions, then ends the turn.
///
/// Never inlined, so that `run_action` is inlined here: it would not be
/// inlined into a caller that inlined this one.
#[inline(never)]
pub(crate) fn run_queue(battle: &mut Battle) {
    while let Some(action) = battle.queue.shift() {
        run_action(battle, action);
        if battle.is_over() || battle.requests.iter().any(|r| *r != Request::Wait) {
            return;
        }
    }
    next_turn(battle);
    battle.mid_turn = false;
    battle.queue.clear();
}

/// Showdown's `runAction`.
///
/// Always inlined into [`run_queue`], its one caller: left to the
/// inliner, whether this large function is inlined flips with unrelated
/// changes elsewhere, and a call cost about 1% of a battle's
/// instructions (registers spilled around it).
#[inline(always)]
fn run_action(battle: &mut Battle, action: Action) {
    let mon = action.mon;
    let original_hp = mon.and_then(|m| active_position(battle, m)).and_then(|p| battle.state.at(p)).map_or(0, |m| m.hp);
    let mut residual_hp: Option<StackVec<(MonRef, u16), 4>> = None;
    match action.kind {
        ActionKind::Team { index } => {
            let mon = mon.expect("team action has a Pokémon");
            let side = battle.state.side_mut(mon.side);
            let position = side.position_of(mon.id).expect("picked Pokémon is on the team");
            side.team.swap(index as usize, position);
            return;
        }
        ActionKind::Start => {
            // `teamsize` of each side (the picked Pokémon), then `start`.
            for side in SideId::BOTH {
                let len = battle.state.side(side).team.len();
                let size = battle.format.team_preview.brought(len);
                battle.emit(BattleEvent::TeamSize { side, size: size as u8 });
            }
            battle.emit(BattleEvent::Start);
            // `singleEvent('BattleStart')` of each Pokémon's species
            // condition (Zacian and Zamazenta crowned by their items).
            items::held::battle_start(battle);
            for side in SideId::BOTH {
                // Only the picked Pokémon take part.
                if let Some(bring) = battle.format.team_preview.picked() {
                    battle.state.side_mut(side).team.truncate(bring as usize);
                }
                let n = battle.state.side(side).team.len().min(battle.format.active_per_side());
                for slot in 0..n {
                    let incoming = mon_ref(battle, side, slot);
                    switch_in(battle, incoming, slot, None, false);
                }
            }
            battle.mid_turn = true;
        }
        ActionKind::Move { move_id, target_loc, .. } => {
            // A Pokémon that fainted or left the field loses its move, and
            // Showdown skips the end-of-action processing entirely.
            let Some(user) = mon.and_then(|m| active_position(battle, m)) else { return };
            moves::run_move(battle, user, move_id, target_loc, order::RunMoveOptions::of(&action));
        }
        ActionKind::Terastallize => {
            if let Some(pos) = mon.and_then(|m| active_position(battle, m)) {
                tera::terastallize(battle, pos);
            }
        }
        // `runMegaEvo`.
        ActionKind::MegaEvo => {
            if let Some(pos) = mon.and_then(|m| active_position(battle, m)) {
                mega::run_mega_evo(battle, pos);
            }
        }
        // Counter's and Focus Punch's start-of-turn effects.
        // Like a move, nothing at all happens if the Pokémon is gone.
        ActionKind::BeforeTurnMove { move_id, target_loc } => {
            let Some(user) = mon.and_then(|m| active_position(battle, m)) else { return };
            if !multiturn::before_turn_move(battle, user, move_id, target_loc) {
                return;
            }
        }
        ActionKind::PriorityChargeMove { move_id, .. } => {
            let Some(user) = mon.and_then(|m| active_position(battle, m)) else { return };
            multiturn::priority_charge_move(battle, user, move_id);
        }
        ActionKind::Switch { target } | ActionKind::InstaSwitch { target, .. } => {
            let outgoing = mon.expect("switch action has a Pokémon");
            let slot = battle.state.side(outgoing.side).position_of(outgoing.id).expect("outgoing Pokémon exists");
            // `singleEvent('CheckShow', naturalcure, null, pokemon)` for a
            // statused Pokémon choosing to switch (whatever its ability).
            let pos = Position::new(outgoing.side, slot as u8);
            if matches!(action.kind, ActionKind::Switch { .. })
                && battle.state.at(pos).is_some_and(|m| m.status != crate::state::Status::None)
            {
                let natural_cure = pikapro_data::gen9::abilities::NATURAL_CURE;
                let relay = events::Relay::True;
                events::single_event_ability(
                    battle,
                    pikapro_data::Event::CheckShow,
                    pos,
                    natural_cure,
                    None,
                    events::Effect::None,
                    relay,
                );
            }
            let from = match action.kind {
                ActionKind::InstaSwitch { from, .. } => from,
                _ => None,
            };
            switch_in(battle, target, slot, from, false);
        }
        ActionKind::RunSwitch => run_switch(battle, mon),
        ActionKind::RevivalBlessing { target } => {
            slots::revive(battle, mon.expect("revival action has a Pokémon"), target);
        }
        // `eachEvent('BeforeTurn')` (no ability or item handles it).
        ActionKind::BeforeTurn => each_event(battle, None, events::Effect::None),
        ActionKind::Residual => {
            // `add('')`.
            battle.emit(BattleEvent::Blank);
            update_speed(battle);
            // `residualPokemon`: each active Pokémon's HP before, for
            // Emergency Exit.
            let mut before = StackVec::<(MonRef, u16), 4>::new();
            for side in SideId::BOTH {
                for slot in 0..battle.format.active_per_side() {
                    if let Some(m) = battle.state.at(Position::new(side, slot as u8)) {
                        before.push((MonRef { side, id: m.id }, m.hp));
                    }
                }
            }
            handlers::run_residual(battle);
            if !battle.is_over() {
                battle.emit(BattleEvent::Upkeep);
            }
            residual_hp = Some(before);
        }
    }

    // Pokémon hit by Roar / Dragon Tail are dragged out now, then
    // `clearActiveMove()`.
    switching::run_drags(battle);
    battle.active_move = None;
    if faint_messages(battle, true) {
        return;
    }
    if battle.queue.is_empty() {
        switching::check_fainted(battle);
    } else if matches!(battle.queue.peek().map(|a| a.kind), Some(ActionKind::InstaSwitch { .. })) {
        return;
    }
    if action.kind != ActionKind::Start {
        each_update(battle);
        // Emergency Exit and Wimp Out after the end-of-turn damage.
        for &(m, hp) in residual_hp.iter().flat_map(|r| r.iter()) {
            if let Some(pos) = active_position(battle, m) {
                moves::emergency_exit(battle, pos, None, hp);
            }
        }
    }
    // After hazards on switching in.
    if action.kind == ActionKind::RunSwitch
        && let Some(pos) = mon.and_then(|m| active_position(battle, m))
    {
        moves::emergency_exit(battle, pos, None, original_hp);
    }
    // Switch-flagged Pokémon (fainted, or after U-turn) are replaced now:
    // `makeRequest('switch')` pauses the turn until the sides choose.
    if switching::make_switch_request(battle) {
        return;
    }
    // Gen 8+: speeds are refreshed and the queue re-sorted before each move.
    if matches!(battle.queue.peek().map(|a| a.kind), Some(ActionKind::Move { .. })) {
        // Speed and priority handlers only compute: one set of event masks
        // serves them all.
        let any = Position::new(SideId::P1, 0);
        events::with_cached_masks(battle, events::Target::Pokemon(any), None, |battle| {
            // Nothing changes the state in here: each queued action of a
            // Pokémon whose speed `update_speed` computed without sorting
            // handlers takes the speed it stored.
            let fresh = update_speed(battle);
            with_fresh_speeds(battle, fresh, |battle| {
                for i in 0..battle.queue.as_slice().len() {
                    let mut queued = battle.queue.as_slice()[i];
                    if queued.mon.is_some() {
                        queue::get_action_speed(battle, &mut queued);
                        battle.queue.as_mut_slice()[i] = queued;
                    }
                }
            });
        });
        queue::sort(battle);
    }
}

fn mon_ref(battle: &Battle, side: SideId, position: usize) -> MonRef {
    MonRef { side, id: battle.state.side(side).team[position].id }
}

/// The team position of the Pokémon Showdown's `side.active[slot]` holds,
/// fainted included: `slot`, or, after a stale switch, the position of the
/// fainted Pokémon it moved to the other slot
/// ([`OrderState::stale_active`]) while it stands there. (Once a switch
/// into that slot takes it to the bench, Showdown throws: what is left of
/// the battle is pikapro's own, and the slot is `slot` again.)
#[inline(always)]
pub(crate) fn field_index(battle: &Battle, side: SideId, slot: usize) -> usize {
    if battle.order.stale_slots == 0 {
        return slot;
    }
    stale_index(battle, side, slot)
}

/// [`field_index`] for a slot a stale switch left.
#[cold]
#[inline(never)]
fn stale_index(battle: &Battle, side: SideId, slot: usize) -> usize {
    let Some(id) = battle.order.stale_active[side.index()][slot] else { return slot };
    battle.state.side(side).position_of(id).filter(|&p| p < battle.format.active_per_side()).unwrap_or(slot)
}

/// Where a Pokémon stands on the field (fainted or not), if it is in a slot.
///
/// Only the slots' team positions (0, and 1 in doubles) are looked at: an
/// id is on one team position, and one past the slots is off the field.
#[inline]
pub(crate) fn position_of_mon(battle: &Battle, mon: MonRef) -> Option<Position> {
    const _: () = assert!(MAX_ACTIVE == 2);
    let team = &battle.state.side(mon.side).team;
    let position = if team.first().is_some_and(|m| m.id == mon.id) {
        0
    } else if battle.format.active_per_side() > 1 && team.get(1).is_some_and(|m| m.id == mon.id) {
        1
    } else {
        return None;
    };
    Some(Position::new(mon.side, position))
}

/// Where a Pokémon stands if it is active and has not fainted.
fn active_position(battle: &Battle, mon: MonRef) -> Option<Position> {
    position_of_mon(battle, mon).filter(|&p| battle.state.at(p).is_some_and(|m| !m.is_fainted()))
}

/// The Pokémon at `pos` for event purposes. Team position `slot` always
/// holds the Pokémon that last occupied that slot, so this also names a
/// Pokémon that has just fainted.
///
/// # Panics
/// If the slot has never been filled.
pub(crate) fn actor(battle: &Battle, pos: Position) -> Actor {
    let side = battle.state.side(pos.side);
    let filled = side.active.get(pos.slot as usize).is_some_and(|a| a.is_some());
    // (A slot a stale switch left, or the slot it took its fainted
    // Pokémon to, is unfilled.)
    if !filled && battle.order.stale_slots != 0 && (pos.slot as usize) < MAX_ACTIVE {
        return stale_actor(battle, pos);
    }
    let mon = side.team.get(pos.slot as usize).expect("event for a slot that was never filled");
    // Illusion shows its disguise. A team position past the active slots
    // names a Pokémon off the field (a Future Sight user).
    let species = abilities::switchin::seen_species(side, mon);
    let id = abilities::switchin::seen_id(side, mon);
    let active = filled && !slots::off_field(battle, pos);
    Actor { pos, species, active, id }
}

/// [`actor`] after a stale switch ([`OrderState::stale_active`]): a slot
/// it left names the fainted Pokémon it moved, where it stands, and that
/// Pokémon is `isActive` there.
#[cold]
#[inline(never)]
fn stale_actor(battle: &Battle, pos: Position) -> Actor {
    let pos = Position::new(pos.side, field_index(battle, pos.side, pos.slot as usize) as u8);
    let side = battle.state.side(pos.side);
    let mon = side.team.get(pos.slot as usize).expect("event for a slot that was never filled");
    let species = abilities::switchin::seen_species(side, mon);
    let id = abilities::switchin::seen_id(side, mon);
    let active = side.active.get(pos.slot as usize).is_some_and(|a| a.is_some()) && !slots::off_field(battle, pos)
        || (pos.slot as usize) < battle.format.active_per_side()
            && battle.order.stale_active[pos.side.index()].contains(&Some(mon.id));
    Actor { pos, species, active, id }
}

/// [`actor`] when the event log is on (the only reader of an event's
/// actors), for [`Battle::emit_for`].
#[inline]
pub(crate) fn log_actor(battle: &Battle, pos: Position) -> Option<Actor> {
    battle.log.is_enabled().then(|| actor(battle, pos))
}

/// Showdown's `pokemon.getActionSpeed()`: boosted, modified Speed
/// (`getStat('spe')`: `ModifyBoost`, then `ModifySpe`, whose handler sorts
/// can draw), inverted under Trick Room. Events only find handlers for an
/// active Pokémon, so a fainted one's queued move uses its unmodified
/// Speed.
pub(crate) fn mon_action_speed(battle: &mut Battle, mon: MonRef) -> i32 {
    let Some(index) = battle.state.side(mon.side).position_of(mon.id) else { return 1 };
    let speed = events::get_stat(battle, Position::new(mon.side, index as u8), Stat::Spe, false, false);
    action_speed(battle, speed)
}

/// `getActionSpeed` from the Speed stat: Trick Room and the cartridge's
/// 13-bit wrap (custom games replace `trunc` with `Math.trunc`).
fn action_speed(battle: &Battle, speed: u32) -> i32 {
    let trick_room = battle.state.field.trick_room_turns > 0;
    match battle.format.mechanics {
        // `10000 - speed`, truncated to 13 bits: very fast Pokémon wrap
        // around and move first even under Trick Room.
        Mechanics::ScarletViolet => {
            let speed = if trick_room { 10000 - speed as i32 } else { speed as i32 };
            if battle.format.cartridge_overflow { speed & 0x1FFF } else { speed }
        }
        // The `champions` mod removes the underflow and the truncation.
        Mechanics::Champions if trick_room => -(speed as i32),
        Mechanics::Champions => speed as i32,
    }
}

/// Showdown's `battle.updateSpeed()`: refresh the stored speed of every
/// active Pokémon (`getAllActive()`).
///
/// Returns the active slots (bit `2 * side + slot`) whose speed was
/// computed without sorting a list of handlers (`Battle::handler_sorts`
/// unchanged: at most one `ModifyBoost` and one `ModifySpe` handler). Such
/// a computation drew nothing and read no stored speed, so while the state
/// doesn't change, computing it again gives the same speed and draws
/// nothing.
fn update_speed(battle: &mut Battle) -> u8 {
    // Without a `ModifyBoost` or `ModifySpe` handler on the field, the
    // speeds need no events.
    let events =
        events::any_active_handles(battle, &[pikapro_data::Event::ModifyBoost, pikapro_data::Event::ModifySpe]);
    // The speed handlers only compute (no effect changes), so the four
    // Pokémon share one set of event masks.
    if events && !battle.masks_cached.is_cached() {
        let any = Position::new(SideId::P1, 0);
        return events::with_cached_masks(battle, events::Target::Pokemon(any), None, update_speed);
    }
    let mut fresh = 0;
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            let pos = Position::new(side, slot as u8);
            let Some(m) = battle.state.at(pos) else { continue };
            let sorts = battle.handler_sorts;
            let speed = if events {
                mon_action_speed(battle, MonRef { side, id: m.id })
            } else {
                let raw = crate::stats::apply_stage(m.stats[Stat::Spe], m.boosts.spe.clamp(-6, 6)) as u32;
                action_speed(battle, raw.min(10000))
            };
            if battle.handler_sorts == sorts {
                fresh |= 1 << (side.index() * 2 + slot);
            }
            battle.state.at_mut(pos).expect("checked above").speed = speed;
        }
    }
    fresh
}

/// Showdown's `pokemon.updateSpeed()` for one Pokémon. Returns its slot's
/// bit if it is active and its speed was computed without sorting handlers
/// (as [`update_speed`]).
pub(crate) fn update_speed_of(battle: &mut Battle, mon: MonRef) -> u8 {
    let Some(position) = battle.state.side(mon.side).position_of(mon.id) else { return 0 };
    let sorts = battle.handler_sorts;
    let speed = mon_action_speed(battle, mon);
    battle.state.side_mut(mon.side).team[position].speed = speed;
    if battle.handler_sorts == sorts && position < battle.format.active_per_side() {
        1 << (mon.side.index() * 2 + position)
    } else {
        0
    }
}

/// Showdown's `eachEvent(event, effect)` (`BeforeTurn`, `WeatherChange`,
/// `TerrainChange`, `Update`): every active Pokémon, sorted by stored speed
/// (a sort that draws on ties), runs the event (`runEvent(event, pokemon,
/// null, effect)`).
pub(crate) fn each_event(battle: &mut Battle, event: Option<pikapro_data::Event>, effect: events::Effect) {
    let mut order = [(Position::default(), 0); 2 * MAX_ACTIVE];
    let mut n = 0;
    let active = battle.format.active_per_side();
    for side in SideId::BOTH {
        let s = battle.state.side(side);
        for slot in 0..active {
            if let Some(mon) = s.active(slot) {
                order[n] = (Position::new(side, slot as u8), mon.speed);
                n += 1;
            }
        }
    }
    // `BeforeTurn` has no handlers yet: only its sort's draws (on ties)
    // matter.
    let Some(event) = event else {
        queue::speed_sort_by_speed_draws(&order[..n], &mut battle.rng);
        return;
    };
    events::each_event_run(battle, event, effect, &mut order[..n]);
}

/// Showdown's `eachEvent('Update')`: Attract and Syrup Bomb ending, items
/// and abilities that react to the state (Sitrus Berry, ...).
pub(crate) fn each_update(battle: &mut Battle) {
    each_event(battle, Some(pikapro_data::Event::Update), events::Effect::None);
}

/// `runEvent(event, pokemon)` with no source, effect or relay.
pub(crate) fn run_mon_event(battle: &mut Battle, event: pikapro_data::Event, pos: Position) -> events::Relay {
    run_mon_event_from(battle, event, pos, None)
}

/// `runEvent(event, pokemon, source)` with no effect or relay.
pub(crate) fn run_mon_event_from(
    battle: &mut Battle,
    event: pikapro_data::Event,
    pos: Position,
    source: Option<Position>,
) -> events::Relay {
    let mut ev =
        events::EventCtx::new(event, events::Target::Pokemon(pos), source, events::Effect::None, events::Relay::True);
    events::run_event(battle, &mut ev, None)
}

/// Showdown's `switchIn`: the incoming Pokémon takes the slot's team
/// position and the outgoing one takes the incoming's old position, so the
/// active Pokémon always occupy positions `0..active` (see
/// [`SideState`](crate::state::SideState)). Queues the incoming Pokémon's
/// switch-in effects (`runSwitch`), or runs them at once when it was
/// dragged in (`drag`). `from` is the move that made the previous Pokémon
/// switch out (U-turn).
pub(crate) fn switch_in(battle: &mut Battle, incoming: MonRef, slot: usize, from: Option<MoveId>, drag: bool) {
    // "A switch failed because the Pokémon trying to switch in is already
    // in": a queued switch whose Pokémon came in meanwhile (after an Eject
    // Pack or Eject Button switch). A Pokémon revived in its own slot
    // (Revival Blessing) is not active yet and comes in there.
    if let Some(p) = position_of_mon(battle, incoming)
        && p.slot as usize != slot
        && battle.state.side(incoming.side).active[p.slot as usize].is_some()
    {
        battle.hint(crate::event::Hint::SwitchAlreadyIn);
        return;
    }
    let pos = Position::new(incoming.side, slot as u8);
    let mut carried = None;
    // What Baton Pass hands over (`copyVolatileFrom`).
    let mut passed = None;
    let mut copy_volatile = false;
    if let Some((out, skip)) =
        battle.state.at(pos).filter(|m| !m.is_fainted()).map(|m| (m.id, m.skip_before_switch_out))
    {
        // A Pokémon revived in this slot calls itself back, off the field.
        if out == incoming.id {
            battle.order.revived_switching = Some(pos);
        }
        if !skip && !drag {
            // `runEvent('BeforeSwitchOut')` (no Gen 9 ability or item
            // handles it), then `eachEvent('Update')` (Gen 5+).
            each_update(battle);
        }
        // `beingCalledBack` (Slow Start's and Illusion's `End`).
        let outgoing = &mut battle.state.side_mut(pos.side).team[slot];
        outgoing.being_called_back = true;
        // `clearVolatile`'s `setSpecies` resets `apparentType`.
        outgoing.illusion_apparent = None;
        // `runEvent('SwitchOut')` (Regenerator, Natural Cure), then the
        // ability's and item's `End`.
        run_mon_event(battle, pikapro_data::Event::SwitchOut, pos);
        events::end_held(battle, pos);
        let out = MonRef { side: incoming.side, id: out };
        battle.queue.retain(|a| a.mon != Some(out));
        // Linked volatiles end; Shed Tail's substitute is passed on
        // (`copyVolatileFrom`).
        carried = conditions::switch_out(battle, pos, from);
        let mon = battle.state.at_mut(pos).expect("checked above");
        mon.skip_before_switch_out = false;
        mon.used_item_this_turn = false;
        passed = order::passed_volatiles(mon, from);
        copy_volatile = matches!(from, Some(move_ids::BATON_PASS | move_ids::SHED_TAIL));
        mon.clear_volatile_state();
        // Off the field, `runEvent('Type')` collects no handlers: an Arceus's
        // Multitype no longer applies (a Future Sight it used has its
        // forme's types, before Terastallization too).
        mon.base_types = mon.species_types;
        mon.types = mon.tera_types().unwrap_or(mon.species_types);
        battle.order.revived_switching = None;
    }
    // `if (oldActive) oldActive.usedItemThisTurn = false`, a fainted one's
    // too: revived (Revival Blessing) and back on a later turn, its berry
    // is no longer Pickup's.
    // (A fainted Pokémon has left `active` but is still `team[slot]`.)
    if let Some(old) = battle.state.side_mut(pos.side).team.get_mut(slot) {
        old.used_item_this_turn = false;
    }
    let side = battle.state.side_mut(incoming.side);
    let position = side.position_of(incoming.id).expect("incoming Pokémon is on the team");
    side.team.swap(slot, position);
    // BattleActions.switchIn keeps battle.activePokemon pointing to the
    // same Pokémon as its position changes. A Red Card can drag the move's
    // user out before clearActiveMove; Mold Breaker must then see that user
    // on the bench, rather than suppressing abilities through its replacement.
    if let Some((user, _)) = &mut battle.active_move
        && user.side == incoming.side
    {
        if user.slot as usize == slot {
            user.slot = position as u8;
        } else if user.slot as usize == position {
            user.slot = slot as u8;
        }
    }
    // A fainted Pokémon a stale switch brings in (it came in early, after
    // an Eject Pack, and fainted on entry): `switchIn` checks only
    // `isActive`, so it comes in with `0 fnt`, but it stays `fainted`:
    // `fieldEvent` skips its handlers (Imposter), `getAllActive` and
    // `allies` leave it out. The slot is left unfilled. The slot it
    // fainted in still holds it (`side.active[pos] = pokemon` sets only
    // the new one).
    if side.team[slot].is_fainted() {
        side.active[slot] = None;
        stale_switch_in(battle, incoming, slot, position);
    } else {
        side.active[slot] = Some(slot as u8);
    }
    if battle.order.stale_slots != 0 {
        battle.order.stale_active[incoming.side.index()][slot] = None;
        battle.order.stale_slots &= !(1 << (incoming.side.index() * 2 + slot));
    }
    let side = battle.state.side_mut(incoming.side);
    let mon = &mut side.team[slot];
    // Showdown's `switchIn` doesn't `clearVolatile` the incoming Pokémon
    // (it was cleared when it left). What the move that knocked it out
    // wrote after `faintMessages` cleared it stays: `hitStepMoveHitLoop`'s
    // `gotAttacked` and `timesAttacked` (a Pokémon revived by Revival
    // Blessing comes back with them: Rage Fist's count in Champions, a
    // `thisTurn` hit for Metal Burst and Avalanche).
    let attacked = (mon.volatile_counters.times_attacked, mon.volatile_counters.attacked_by);
    let (damaged_bits, damaged_by) = (mon.volatile_counters.damaged_by, mon.record.damaged_by);
    mon.clear_volatile_state();
    (mon.volatile_counters.times_attacked, mon.volatile_counters.attacked_by) = attacked;
    (mon.volatile_counters.damaged_by, mon.record.damaged_by) = (damaged_bits, damaged_by);
    // `switchIn`: `activeMoveActions = 0`.
    mon.active_move_actions = 0;
    // On the field again, Multitype's `onType` applies.
    // Its `apparentType` is still the forme's (`setSpecies`).
    if let Some(types) = abilities::switchin::multitype_types_of(&battle.format, mon) {
        mon.base_types = types;
        mon.types = mon.tera_types().unwrap_or(types);
        if types != mon.species_types && mon.illusion_apparent.is_none() {
            mon.volatile_counters.apparent_types = Some(mon.species_types);
        }
    }
    // Baton Pass and Shed Tail: `copyVolatileFrom` starts with
    // `clearVolatile`, whose `setSpecies` resets `apparentType`: an
    // Illusion's earlier reveal of this (Terastallized) Pokémon's types no
    // longer stands.
    if copy_volatile {
        mon.illusion_apparent = None;
    }
    if let Some(passed) = passed {
        order::copy_volatiles(mon, passed, battle.format.dex);
    }
    status::switch_in(mon);
    conditions::switched_in(battle, pos, carried);
    let mon = &battle.state.side(incoming.side).team[slot];
    let (level, hp, max_hp, status) = (mon.level, mon.hp, mon.max_hp(), mon.status);
    let tera = mon.terastallized.then_some(mon.tera_type);
    let team_index = position as u8;
    // A new `abilityState` and `itemState` (`clearVolatile` kept the old
    // ones).
    abilities::new_ability_state(battle, pos);
    items::new_item_state(battle, pos);
    // `runEvent('BeforeSwitchIn')` (Illusion).
    run_mon_event(battle, pikapro_data::Event::BeforeSwitchIn, pos);
    // Illusion shows its disguise.
    let side = battle.state.side(incoming.side);
    let species = abilities::switchin::seen_species(side, &side.team[slot]);
    let id = abilities::switchin::seen_id(side, &side.team[slot]);
    let shown = abilities::switchin::seen_details(side, &side.team[slot]);
    // `illusion.getUpdatedDetails()` shows the disguise's gender.
    let me = &side.team[slot];
    let gender = me.illusion.and_then(|id| side.team.iter().find(|m| m.id == id)).unwrap_or(me).gender;
    // `getFullDetails`: under Illusion Level Mod the details show the
    // disguise's level (`this.illusion.level`), not the Pokémon's.
    let level = match me.illusion.and_then(|id| side.team.iter().find(|m| m.id == id)) {
        Some(disguise) if battle.format.illusion_level_mod => disguise.level,
        _ => level,
    };
    let actor = Actor { pos, species, active: true, id };
    if drag {
        battle.emit(BattleEvent::Drag { actor, team_index, level, hp, max_hp, status, tera, shown, gender });
        // Gen 5+: a dragged-in Pokémon's switch-in effects run at once.
        run_switch(battle, Some(incoming));
    } else {
        battle.emit(BattleEvent::Switch { actor, team_index, level, hp, max_hp, status, tera, from, shown, gender });
        queue::insert_choice(battle, Action::new(ActionKind::RunSwitch, Some(incoming)), false);
    }
}

/// A stale switch brought `incoming`, fainted, from team `position` into
/// `slot`: the slot it fainted in still holds it
/// ([`OrderState::stale_active`]).
#[cold]
#[inline(never)]
fn stale_switch_in(battle: &mut Battle, incoming: MonRef, slot: usize, position: usize) {
    if position < battle.format.active_per_side() && position != slot {
        battle.order.stale_active[incoming.side.index()][position] = Some(incoming.id);
        battle.order.stale_slots |= 1 << (incoming.side.index() * 2 + position);
    }
}

/// Showdown's `runSwitch`: takes every queued switch-in at once, orders all
/// active Pokémon (fainted included) by stored speed, and runs `SwitchIn`.
/// Also called directly for a Pokémon dragged in.
fn run_switch(battle: &mut Battle, first: Option<MonRef>) {
    let mut switchers = StackVec::<Position, 4>::new();
    if let Some(pos) = first.and_then(|m| position_of_mon(battle, m)) {
        switchers.push(pos);
    }
    while matches!(battle.queue.peek().map(|a| a.kind), Some(ActionKind::RunSwitch)) {
        let action = battle.queue.shift().expect("peeked");
        if let Some(pos) = action.mon.and_then(|m| position_of_mon(battle, m)) {
            switchers.push(pos);
        }
    }
    // `speedSort(getAllActive(true))`, kept as `speedOrder`
    // (`getFieldPositionValue`: where each Pokémon stands, which for one a
    // stale switch moved is the slot it moved to).
    let mut order = StackVec::<(Position, i32), 4>::new();
    for side in SideId::BOTH {
        let s = battle.state.side(side);
        for slot in 0..battle.format.active_per_side().min(s.team.len()) {
            let index = field_index(battle, side, slot);
            order.push((Position::new(side, index as u8), s.team[index].speed));
        }
    }
    queue::speed_sort_by_speed(&mut order, &mut battle.rng);
    let order: StackVec<Position, 4> = order.iter().map(|&(pos, _)| pos).collect();
    // `fieldEvent('SwitchIn', switchersIn)`.
    handlers::run_switch_in(battle, &switchers, &order);
}

/// A queued faint (Showdown's `faintQueue` entry): the Pokémon, and the
/// source and effect of what knocked it out (Moxie's `onSourceAfterFaint`
/// reads them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct FaintData {
    pub pos: Position,
    pub source: Option<Position>,
    pub effect: events::Effect,
}

impl From<Position> for FaintData {
    fn from(pos: Position) -> FaintData {
        FaintData { pos, source: None, effect: events::Effect::None }
    }
}

/// The end of Showdown's `pokemon.faint()` for a Pokémon left with no HP:
/// `this.switchFlag = false` (a U-turn user knocked out by Life Orb isn't
/// switched out mid-turn), then its entry in the faint queue, which
/// `faintMessages` reports. An Emergency Exit that runs after it (recoil)
/// sets the flag again.
pub(crate) fn queue_faint(battle: &mut Battle, data: FaintData) {
    if let Some(mon) = battle.state.side_mut(data.pos.side).team.get_mut(data.pos.slot as usize) {
        mon.switch_flag = crate::state::SwitchFlag::None;
    }
    battle.faint_queue.push(data);
}

/// Showdown's `pokemon.faint()` for a Pokémon on the field that still has
/// HP (`selfdestruct`, Perish Song, Destiny Bond): HP to 0 and a queued
/// faint ([`queue_faint`]).
pub(crate) fn faint(battle: &mut Battle, pos: Position) {
    let Some(mon) = battle.state.at_mut(pos).filter(|m| !m.is_fainted()) else { return };
    mon.hp = 0;
    queue_faint(battle, pos.into());
}

/// The move being used and its user, unless the move is a stand-in that
/// is no dex move (`recharge`, the locked-move effect).
pub(crate) fn active_dex_move(battle: &Battle) -> Option<(Position, MoveId)> {
    battle.active_move.filter(|&(_, m)| (m.0 as usize) < battle.format.dex.moves.len())
}

/// The active move's user and the move as an effect (none without a dex
/// move, [`active_dex_move`]).
fn active_move_effect(battle: &Battle) -> (Option<Position>, events::Effect) {
    match active_dex_move(battle) {
        Some((user, m)) => (Some(user), events::Effect::Move(m)),
        None => (None, events::Effect::None),
    }
}

/// `effect ||= this.effect` where the effect running is a move or a
/// condition: the move being used ([`active_dex_move`]), else the
/// condition (whose messages are the same for every condition).
pub(crate) fn move_or_condition_effect(battle: &Battle) -> events::Effect {
    match active_dex_move(battle) {
        Some((_, m)) => events::Effect::Move(m),
        None => events::Effect::Condition(events::Condition::Other),
    }
}

/// The source and effect Showdown passes for damage the engine names by
/// its message: a move's damage comes from its user (the active move), a
/// status's and weather's from nothing, recoil and confusion from the
/// Pokémon itself.
fn damage_effect(battle: &Battle, pos: Position, msg: DamageSource) -> (Option<Position>, events::Effect) {
    use events::{Condition, Effect};
    match msg {
        DamageSource::Move => active_move_effect(battle),
        DamageSource::Status(s) => (None, Effect::of_status(s)),
        DamageSource::Weather(w) => (None, Effect::Weather(w)),
        DamageSource::Recoil => (Some(pos), Effect::Condition(Condition::StruggleRecoil)),
        DamageSource::MoveRecoil => (Some(pos), Effect::Condition(Condition::Recoil)),
        DamageSource::Confusion => (Some(pos), Effect::Condition(Condition::Confused)),
        DamageSource::MoveEffect { of, .. } => (Some(of.pos), Effect::Condition(Condition::Other)),
        DamageSource::MoveCondition(_) => (Some(pos), Effect::Condition(Condition::Other)),
        DamageSource::Effect(_) => (None, Effect::Condition(Condition::Other)),
        DamageSource::Species(_) => (Some(pos), Effect::Condition(Condition::Other)),
    }
}

/// Showdown's `damage` / `spreadDamage` for one target, for the engine's
/// own damage (named by its message; see [`spread_damage`]). Returns the HP
/// actually removed (0 if the `Damage` event stopped it).
pub(crate) fn apply_damage(battle: &mut Battle, pos: Position, amount: u32, msg: DamageSource) -> u16 {
    let (source, effect) = damage_effect(battle, pos, msg);
    spread_damage(battle, pos, amount, source, effect, msg).unwrap_or(0)
}

/// Showdown's `battle.damage(amount, target, source, effect)` for an
/// ability's or item's damage: the line reads `[from] ability: X` /
/// `[from] item: X`, with `[of] <source>` when the source is another
/// Pokémon or the effect an ability. Returns the HP removed, `None` if the
/// `Damage` event stopped it (Magic Guard).
pub(crate) fn damage_by(
    battle: &mut Battle,
    pos: Position,
    amount: u32,
    source: Option<Position>,
    effect: events::Effect,
) -> Option<u16> {
    let msg = match effect_cause(battle, pos, source, effect) {
        Some(cause) => DamageSource::Effect(cause),
        None => DamageSource::Move,
    };
    spread_damage(battle, pos, amount, source, effect, msg)
}

/// The `[from] <effect>|[of] <source>` of an ability's or item's damage or
/// healing (`spreadDamage`'s and `heal`'s default messages).
fn effect_cause(
    battle: &Battle,
    pos: Position,
    source: Option<Position>,
    effect: events::Effect,
) -> Option<crate::event::Cause> {
    use crate::event::Cause;
    let cause = match effect {
        events::Effect::Ability(a) => Cause::ability(a),
        events::Effect::Item(i) => Cause::item(i),
        events::Effect::Move(m) => Cause::move_(m),
        _ => return None,
    };
    Some(match source {
        Some(s) if s != pos || effect.is_ability() => cause.of(actor(battle, s)),
        _ => cause,
    })
}

/// `spreadDamage`'s `runEvent('Damage', target, source, effect, amount)`
/// for damage that the caller applies and logs itself (a volatile's or a
/// hazard's): the amount (at least 1) after Magic Guard and the like, or
/// `None` if a handler stopped it.
pub(crate) fn damage_event(
    battle: &mut Battle,
    pos: Position,
    amount: u32,
    source: Option<Position>,
    effect: events::Effect,
) -> Option<u32> {
    let amount = if amount != 0 { amount.max(1) } else { 0 };
    if events::filtered(battle, pikapro_data::Event::Damage) {
        return Some(amount);
    }
    let masks = events::Masks::of(battle, events::Target::Pokemon(pos), source);
    if !masks.relevant(battle.format.active_per_side(), events::Target::Pokemon(pos), pikapro_data::Event::Damage) {
        return Some(amount);
    }
    let mut ev = events::EventCtx::new(
        pikapro_data::Event::Damage,
        events::Target::Pokemon(pos),
        source,
        effect,
        events::Relay::Num(amount as i32),
    );
    match events::run_event(battle, &mut ev, None) {
        events::Relay::Num(n) => Some(if n > 0 { n as u32 } else { 0 }),
        events::Relay::True => Some(amount),
        _ => None,
    }
}

/// An amount of `hp * num / den` as Showdown passes it, unrounded, to
/// `damage`, `directDamage`, `heal` or `sethp` (`pokemon.baseMaxhp / 8`,
/// `move.totalDamage / 8`): floored, but a nonzero fraction is 1 HP at
/// least. `spreadDamage`'s `if (targetDamage !== 0) targetDamage =
/// clampIntRange(targetDamage, 1)`, `directDamage`'s `clampIntRange(damage,
/// 1)` after `if (!damage) return 0`, `heal`'s `if (damage && damage <= 1)
/// damage = 1` before `trunc`, and `sethp`'s `trunc` then `if (d < 1) d = 1`.
/// So a 1 HP Shedinja takes 1 from Rough Skin's or Sticky Barb's eighth,
/// where `max_hp / 8` would floor to 0 first.
///
/// The one helper for these amounts: every fractional damage or healing
/// passes through it. A site Showdown floors itself (`Math.floor`, Pollen
/// Puff's heal, a substitute's HP) or rounds (`Math.round`: Struggle's
/// recoil, Steel Beam, Mind Blown), and `Pokemon.heal` (Regenerator, which
/// truncates without the clamp), don't.
pub(crate) fn hp_fraction(max_hp: u32, num: u32, den: u32) -> u32 {
    let n = max_hp * num;
    if n == 0 { 0 } else { (n / den).max(1) }
}

/// Showdown's `spreadDamage` for one target: nothing for a Pokémon without
/// HP (0) or off the field (`None`); the `Damage` event (all but Struggle's
/// recoil: Endure, False Swipe, Focus Sash, Magic Guard, ...) can change
/// the amount or stop the damage (`None`); then `pokemon.damage` (a faint
/// is queued at 0 HP with its source and effect) and the `-damage` line.
pub(crate) fn spread_damage(
    battle: &mut Battle,
    pos: Position,
    amount: u32,
    source: Option<Position>,
    effect: events::Effect,
    msg: DamageSource,
) -> Option<u16> {
    // As the event shows it before the `Damage` event; made only for the
    // log (see `emit_without_log`).
    let target = battle.log.is_enabled().then(|| actor(battle, pos));
    let mon = battle.state.at(pos)?;
    if mon.is_fainted() {
        return Some(0);
    }
    // `!target.isActive`: revived in its slot, not switched in yet.
    if slots::off_field(battle, pos) {
        return None;
    }
    let mut amount = if amount != 0 { amount.max(1) } else { 0 };
    // `effect.effectType === 'Weather' && !target.runStatusImmunity(effect.id)`.
    if let events::Effect::Weather(w) = effect
        && field::weather_immune(battle, pos, w)
    {
        return Some(0);
    }
    if effect != events::Effect::Condition(events::Condition::StruggleRecoil) {
        let mut ev = events::EventCtx::new(
            pikapro_data::Event::Damage,
            events::Target::Pokemon(pos),
            source,
            effect,
            events::Relay::Num(amount as i32),
        );
        // The move's own `onDamage` (False Swipe, priority -20).
        let own = effect.move_id().and_then(power::own_damage_spec);
        match events::run_event(battle, &mut ev, own) {
            events::Relay::Num(n) => amount = n.max(0) as u32,
            events::Relay::True => {}
            _ => return None,
        }
        if amount != 0 {
            amount = amount.max(1);
        }
    }
    let mon = battle.state.at_mut(pos)?;
    if mon.is_fainted() {
        return Some(0);
    }
    let dealt = amount.min(mon.hp as u32) as u16;
    mon.hp -= dealt;
    power::on_damage_dealt(mon, dealt, msg);
    let (hp, max_hp, status, fainted) = (mon.hp, mon.max_hp(), mon.status, mon.is_fainted());
    if fainted {
        queue_faint(battle, FaintData { pos, source, effect });
    }
    match target {
        Some(target) => battle.emit(BattleEvent::Damage { target, hp, max_hp, status, source: msg }),
        None => battle.emit_without_log(),
    }
    if fainted && msg == DamageSource::Move {
        conditions::on_move_ko(battle, pos);
    }
    Some(dealt)
}

/// Showdown's `directDamage` (Belly Drum's and Substitute's HP cost, a
/// Ghost type's Curse): a plain `-damage` with no `Damage` event, and it
/// doesn't count as `hurtThisTurn`. Nothing happens for 0.
pub(crate) fn direct_damage(battle: &mut Battle, pos: Position, amount: u32) {
    let target = actor(battle, pos);
    let Some(m) = battle.state.at_mut(pos) else { return };
    if m.is_fainted() || amount == 0 {
        return;
    }
    let dealt = amount.min(m.hp as u32) as u16;
    m.hp -= dealt;
    let (hp, max_hp, status, fainted) = (m.hp, m.max_hp(), m.status, m.is_fainted());
    battle.emit(BattleEvent::Damage { target, hp, max_hp, status, source: DamageSource::Move });
    if fainted {
        queue_faint(battle, pos.into());
    }
}

/// Showdown's `heal` for the engine's own healing (named by its message:
/// a move's healing, draining, Grassy Terrain). Returns the HP actually
/// restored (nothing is logged for 0).
pub(crate) fn apply_heal(battle: &mut Battle, pos: Position, amount: u32, msg: HealSource) -> u16 {
    use events::{Condition, Effect};
    let (source, effect) = match msg {
        HealSource::Move => active_move_effect(battle),
        HealSource::Drain { of } => (Some(of.pos), Effect::Condition(Condition::Drain)),
        HealSource::Terrain(t) => (None, Effect::Terrain(t)),
        HealSource::Effect(_) => (None, Effect::Condition(Condition::Other)),
    };
    heal_inner(battle, pos, amount, source, effect, msg).unwrap_or(0)
}

/// Showdown's `battle.heal` of a move's `heal` (Recover): `None` when
/// `TryHeal` stopped it.
pub(crate) fn heal_move(battle: &mut Battle, pos: Position, amount: u32) -> Option<u16> {
    let (source, effect) = active_move_effect(battle);
    heal_inner(battle, pos, amount, source, effect, HealSource::Move)
}

/// Showdown's `battle.heal(amount, target, source, effect)` for an
/// ability's or item's healing (`[from] item: Leftovers`, `[from] ability:
/// Water Absorb|[of] <source>`). Returns the HP restored; `None` when
/// nothing was healed (Showdown's falsy result: `TryHeal` stopped it, or
/// the Pokémon is at full HP).
pub(crate) fn heal_by(
    battle: &mut Battle,
    pos: Position,
    amount: u32,
    source: Option<Position>,
    effect: events::Effect,
) -> Option<u16> {
    // `heal` names the source only when it is another Pokémon (unlike
    // `spreadDamage`, even for an ability).
    let msg = match effect_cause(battle, pos, source.filter(|&s| s != pos), effect) {
        Some(cause) => HealSource::Effect(cause),
        None => HealSource::Move,
    };
    heal_inner(battle, pos, amount, source, effect, msg)
}

/// `runEvent('TryHeal', target, source, effect, amount)` (Heal Block,
/// Big Root, Liquid Ooze): the amount to heal, or `None` if stopped.
pub(crate) fn try_heal(
    battle: &mut Battle,
    pos: Position,
    amount: u32,
    source: Option<Position>,
    effect: events::Effect,
) -> Option<u32> {
    let mut ev = events::EventCtx::new(
        pikapro_data::Event::TryHeal,
        events::Target::Pokemon(pos),
        source,
        effect,
        events::Relay::Num(amount as i32),
    );
    match events::run_event(battle, &mut ev, None) {
        events::Relay::Num(n) if n > 0 => Some(n as u32),
        events::Relay::True => Some(amount),
        _ => None,
    }
}

/// Showdown's `heal`: at least 1 HP; `TryHeal` (Heal Block, Big Root,
/// Liquid Ooze) can change or stop it; nothing for a fainted, inactive or
/// full-HP Pokémon; then the `-heal` line.
fn heal_inner(
    battle: &mut Battle,
    pos: Position,
    amount: u32,
    source: Option<Position>,
    effect: events::Effect,
    msg: HealSource,
) -> Option<u16> {
    // Made only for the log (see `emit_without_log`).
    let target = battle.log.is_enabled().then(|| actor(battle, pos));
    let amount = if amount != 0 { amount.max(1) } else { 0 };
    let amount = try_heal(battle, pos, amount, source, effect)?;
    // `!target.isActive`: revived in its slot, not switched in yet.
    if slots::off_field(battle, pos) {
        return None;
    }
    let mon = battle.state.at_mut(pos)?;
    if mon.is_fainted() || mon.hp >= mon.max_hp() {
        return None;
    }
    let healed = amount.min((mon.max_hp() - mon.hp) as u32) as u16;
    mon.hp += healed;
    let (hp, max_hp, status) = (mon.hp, mon.max_hp(), mon.status);
    match target {
        Some(target) => battle.emit(BattleEvent::Heal { target, hp, max_hp, status, source: msg }),
        None => battle.emit_without_log(),
    }
    // `runEvent('Heal')`: no Gen 9 ability or item handles it.
    Some(healed)
}

/// Showdown's `faintMessages`: reports queued faints and empties their
/// slots, then (with `check_win_after`) checks for a winner. Returns true
/// if the battle has ended.
///
/// Inlined, the tests that almost always answer (after every action and
/// every hit): the battle is over, or no faint is queued (then nothing
/// happens and it returns false).
#[inline(always)]
pub(crate) fn faint_messages(battle: &mut Battle, check_win_after: bool) -> bool {
    if battle.is_over() {
        return true;
    }
    if battle.faint_queue.is_empty() {
        return false;
    }
    queued_faint_messages(battle, check_win_after)
}

/// [`faint_messages`] with faints queued.
#[inline(never)]
fn queued_faint_messages(battle: &mut Battle, check_win_after: bool) -> bool {
    let length = battle.faint_queue.len();
    let mut last = None;
    let mut check_win_after = check_win_after;
    while !battle.faint_queue.is_empty() {
        let left = battle.faint_queue.len();
        let data = battle.faint_queue.remove(0);
        let pos = data.pos;
        last = Some(data);
        // `!pokemon.fainted` (`runEvent('BeforeFaint')`: no Gen 9 ability
        // or item handles it).
        if !events::is_active(battle, pos) {
            continue;
        }
        let actor = actor(battle, pos);
        battle.emit(BattleEvent::Faint { actor });
        // `totalFainted`, then `runEvent('Faint')` (Destiny Bond, which can
        // queue another faint), then the ability's and item's `End`.
        power::on_faint(battle, pos.side);
        let mut ev = events::EventCtx::new(
            pikapro_data::Event::Faint,
            events::Target::Pokemon(pos),
            data.source,
            data.effect,
            events::Relay::True,
        );
        events::run_event(battle, &mut ev, None);
        events::end_held(battle, pos);
        battle.state.side_mut(pos.side).active[pos.slot as usize] = None;
        // `clearVolatile`: linked volatiles end. The status stays until
        // `checkFainted`.
        conditions::switch_out(battle, pos, None);
        let mon = &mut battle.state.side_mut(pos.side).team[pos.slot as usize];
        let transformed = mon.volatiles.has(crate::state::Volatiles::TRANSFORMED);
        // `clearVolatile(false)`: the `switchFlag` stays. `faint()` dropped
        // it, so only an Emergency Exit after the faint was queued (recoil
        // from above half HP) set it again: the side then gets a switch
        // request at the end of the action, as for a U-turn.
        let switch_flag = mon.switch_flag;
        mon.clear_volatile_state();
        mon.switch_flag = switch_flag;
        // `delete pokemon.terastallized`: its species' types are back (a
        // Future Sight it used earlier hits without Tera STAB).
        mon.terastallized = false;
        // `isActive = false`: off the field, `runEvent('Type')` collects
        // no handlers, so an Arceus's Multitype no longer applies (a Doom
        // Desire it used hits with its forme's types, as after a switch).
        mon.base_types = mon.species_types;
        mon.types = mon.species_types;
        // `formeRegression` (Morpeko, Zygarde).
        abilities::switchin::faint_forme_regression(battle, pos, transformed);
        // A faint that caused another one always checks for a winner.
        if battle.faint_queue.len() >= left {
            check_win_after = true;
        }
    }
    let Some(last) = last else { return false };
    if check_win_after && check_win(battle, Some(last.pos.side)) {
        return true;
    }
    // `runEvent('AfterFaint', ...)` for the last faint, with the number
    // of faints reported.
    let mut ev = events::EventCtx::new(
        pikapro_data::Event::AfterFaint,
        events::Target::Pokemon(last.pos),
        last.source,
        last.effect,
        events::Relay::Num(length as i32),
    );
    events::run_event(battle, &mut ev, None);
    false
}

/// Showdown's `checkWin`. If nobody has Pokémon left, the side whose Pokémon
/// fainted last wins (Gen 5+), or it's a tie when called without faint data;
/// otherwise a side with no foes left wins. Returns true if the battle ended.
pub(crate) fn check_win(battle: &mut Battle, last_fainted: Option<SideId>) -> bool {
    if battle.is_over() {
        return true;
    }
    // `pokemonLeft` counts a Pokémon down only once its faint is reported:
    // one knocked out whose faint is still queued is left.
    let left = SideId::BOTH.map(|s| {
        let pending = battle.faint_queue.iter().filter(|f| f.pos.side == s && events::is_active(battle, f.pos)).count();
        battle.state.side(s).pokemon_left() + pending
    });
    if left == [0, 0] {
        end(battle, last_fainted, EndReason::AllFainted);
        return true;
    }
    for side in SideId::BOTH {
        if left[side.foe().index()] == 0 {
            end(battle, Some(side), EndReason::AllFainted);
            return true;
        }
    }
    false
}

pub(crate) fn end(battle: &mut Battle, winner: Option<SideId>, reason: EndReason) {
    // `win` starts with `add('')`.
    battle.emit(BattleEvent::Blank);
    battle.emit(match winner {
        Some(side) => BattleEvent::Win { side },
        None => BattleEvent::Tie,
    });
    battle.requests = [Request::Wait; 2];
    battle.pending = [None; 2];
    battle.queue.clear();
    battle.result = Some(BattleResult { winner, reason, turns: battle.state.turn });
}

/// Showdown's `endTurn`.
fn next_turn(battle: &mut Battle) {
    battle.state.turn += 1;
    power::end_turn(battle);
    // Each active Pokémon in turn: `runEvent('DisableMove')`, then
    // `statsRaisedThisTurn` resets and changed types are revealed (a
    // Pokémon transformed into a Terastallized one), then
    // `runEvent('TrapPokemon')`. Their draws and messages interleave by
    // Pokémon.
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            let pos = Position::new(side, slot as u8);
            volatiles::disable_moves(battle, pos);
            manipulation::end_turn(battle, pos);
            order::end_turn(battle, pos);
            if battle.state.at(pos).is_some() {
                conditions::trap_pokemon_event(battle, pos);
            }
        }
    }
    // Pledge combos reset; Gravity disables moves.
    field::end_turn(battle);
    // `attackedBy` bookkeeping.
    multiturn::end_turn(battle);
    for side in &mut battle.state.sides {
        for slot in 0..MAX_ACTIVE {
            if let Some(mon) = side.active_mut(slot) {
                mon.active_turns = mon.active_turns.saturating_add(1);
            }
        }
    }
    // `maybeTriggerEndlessBattleClause`, after the rest of `endTurn` (its
    // `DisableMove` draws included): past the turn limit the battle is a
    // tie (with its message). The result counts the turns played.
    let limit = battle.format.turn_limit;
    if battle.state.turn > limit {
        battle.emit(BattleEvent::Message { message: crate::event::Message::TurnLimit { limit } });
        end(battle, None, EndReason::TurnLimit);
        if let Some(result) = &mut battle.result {
            result.turns = limit;
        }
        return;
    }
    // Its warnings as the limit nears. Showdown's, for its limit of
    // 1000 (`turn >= 500 && turn % 100 === 0 || turn >= 900 && turn % 10
    // === 0 || turn >= 990`), counted back from the format's limit.
    let turns_left = limit.saturating_sub(battle.state.turn);
    if turns_left <= 500 && turns_left.is_multiple_of(100)
        || turns_left <= 100 && turns_left.is_multiple_of(10)
        || turns_left <= 10
    {
        let message = crate::event::Message::AutoTieWarning { turns_left, limit };
        battle.emit(BattleEvent::Message { message });
    }
    // The Endless Battle Clause itself: no `|turn|` line and no request
    // if it ends the battle.
    if endless::check(battle) {
        return;
    }
    battle.emit(BattleEvent::Turn { turn: battle.state.turn });
    battle.requests = [Request::Move; 2];
}

#[cfg(test)]
mod hp_fraction_tests {
    use super::hp_fraction;

    #[test]
    fn a_nonzero_fraction_is_at_least_one() {
        // A 1 HP Shedinja: Sticky Barb's, Rough Skin's, Rocky Helmet's
        // and Life Orb's fractions all take 1.
        for den in [2, 3, 4, 6, 8, 10, 16, 24] {
            assert_eq!(hp_fraction(1, 1, den), 1);
        }
        // Clangorous Soul's `maxhp * 33 / 100` below 1 HP.
        assert_eq!(hp_fraction(3, 33, 100), 1);
        // Floored otherwise; 0 stays 0 (Shell Bell after no damage).
        assert_eq!(hp_fraction(175, 1, 8), 21);
        assert_eq!(hp_fraction(150, 33, 100), 49);
        assert_eq!(hp_fraction(0, 1, 8), 0);
    }
}
