//! Executing a move: Showdown's `runMove` → `useMove` → `trySpreadMoveHit`
//! → `hitStepMoveHitLoop` → `spreadMoveHit` pipeline
//! (`sim/battle-actions.ts`).
//!
//! This file is the shared skeleton. Each step calls into the module that
//! owns the mechanic (`protect`, `effects`, `status`, `volatiles`, `boosts`,
//! `secondaries`, `field`, `switching`), at the point where Showdown does.

use pikapro_data::gen9::moves as ids;
use pikapro_data::{Event, MoveCategory, MoveData, MoveId, MoveTarget, Stat, Type};

use super::events::{self, Effect};
use super::targeting::{self, Loc, Targets};
use super::{
    DamageSource, abilities, actor, apply_damage, boosts, check_win, conditions, each_update, effects, faint_messages,
    field, handlers, itemmoves, manipulation, multiturn, order, pledges, power, protect, queue, secondaries, slots,
    status, switching, volatiles,
};
use crate::battle::Battle;
use crate::event::{BattleEvent, MoveAttr, MoveFrom};
use crate::state::{MoveResult, Position, Status, Volatiles};
use crate::stats::modify;

/// Gen 7+ critical-hit denominators by crit ratio (Showdown's `critMult`);
/// ratio 0 never crits.
const CRIT_MULT: [u32; 5] = [0, 24, 8, 2, 1];

/// Showdown's `runMove`: aim, `BeforeMove`, pay PP, use the move, then
/// report faints and check for a winner. `options` carries the queued
/// action's `originalTarget` and `sourceEffect`.
pub(super) fn run_move(
    battle: &mut Battle,
    user: Position,
    move_id: MoveId,
    target_loc: Loc,
    options: order::RunMoveOptions,
) {
    let combo = pledges::waiting_for(battle, user);
    let locked = battle.order.locked;
    run_move_inner(battle, user, move_id, target_loc, options);
    battle.order.locked = locked;
    pledges::forget(battle, user.side, combo);
}

fn run_move_inner(
    battle: &mut Battle,
    user: Position,
    move_id: MoveId,
    target_loc: Loc,
    options: order::RunMoveOptions,
) {
    let mon = battle.state.at_mut(user).expect("run_move for an active Pokémon");
    mon.active_move_actions = mon.active_move_actions.saturating_add(1);
    // Prankster's `move.pranksterBoosted`.
    abilities::misc::begin_move_action(battle, user, move_id);
    let (_, target_type) = volatiles::queued_move(battle, move_id);
    let mut target = order::get_target(battle, user, move_id, target_type, target_loc, options.original_target);
    // `OverrideAction` (Encore). The new move keeps the chosen one's priority.
    // Not for a move Dancer copies (`externalMove`).
    let external = options.external;
    let mut move_id = move_id;
    let mut priority = None;
    if move_id != ids::STRUGGLE
        && !external
        && let Some((_, new_move)) = volatiles::override_action(battle, user, move_id)
    {
        priority = Some(volatiles::queued_move(battle, move_id).0);
        move_id = new_move;
        target = targeting::get_random_target(battle, user, battle.format.dex.move_(move_id).target);
    }
    battle.active_move = Some((user, move_id));
    // Gen 6+: `action.move.priority` is the action's (after `ModifyPriority`),
    // which an encored move keeps.
    let priority = options.priority.or(priority);
    order::set_active_move(battle, move_id, options.source_effect, priority);
    battle.order.source_ignores_ability = options.source_ignores_ability;
    // `setActiveMove(move, pokemon, target)`: `activeTarget` (Unaware).
    battle.move_state.active_target = target.or(Some(user));
    pledges::begin_move(battle, user);
    // `mustrecharge` stops `recharge` here; once a Dancer copy used it up,
    // `recharge` goes on and has no PP (`cant|...|nopp|recharge`).
    let will_try_move = handlers::run_before_move(battle, user, target, move_id);
    if !will_try_move.is_truthy() {
        // `MoveAborted`: Charge, Destiny Bond ends, a charging move is
        // abandoned.
        // Their handlers are volatiles of the same Pokémon with no
        // priority, so `speedSort` shuffles them; they don't interact, so
        // only the draws matter.
        let tied = battle.state.at(user).map_or(0, |m| {
            [Volatiles::CHARGE, Volatiles::DESTINY_BOND, Volatiles::TWO_TURN_MOVE]
                .iter()
                .filter(|&&f| m.volatiles.has(f))
                .count()
        });
        let mut list = [(); 3];
        queue::speed_sort(&mut list[..tied], &mut battle.rng, |_, _| core::cmp::Ordering::Equal);
        manipulation::move_aborted(battle, user, move_id);
        conditions::move_aborted(battle, user);
        multiturn::move_aborted(battle, user);
        // `pokemon.moveThisTurnResult = willTryMove`: `false` counts as a
        // failed move (Stomping Tantrum, Temper Flare), `null` doesn't.
        // Only `mustrecharge` returns `null`, whatever the move is: under
        // Encore, `OverrideAction` has made the queued `recharge` the
        // encored move by now.
        let result = if will_try_move == events::Relay::Null { MoveResult::Null } else { MoveResult::Failed };
        power::set_move_result(battle, user, result);
        battle.active_move = None;
        return;
    }
    // `if (move.flags['cantusetwice'] && pokemon.lastMove?.id === move.id)
    // pokemon.addVolatile(move.id)` (no condition: only `TryAddVolatile`'s
    // draws), removed after `AfterMove` with a hint.
    let twice_id = match move_id {
        ids::BLOOD_MOON => Some("bloodmoon"),
        ids::GIGATON_HAMMER => Some("gigatonhammer"),
        _ => None,
    }
    .filter(|_| battle.state.at(user).is_some_and(|m| m.last_move == Some(move_id)));
    if let Some(id) = twice_id {
        events::try_add_volatile(battle, user, id);
    }
    // `beforeMoveCallback` (Focus Punch that lost its focus).
    if multiturn::before_move_callback(battle, user, move_id) {
        power::set_move_result(battle, user, MoveResult::Failed);
        battle.active_move = None;
        return;
    }
    // `LockMove`: a locked move costs no PP and is used
    // `[from] lockedmove`.
    let locked = !external && multiturn::locked_move(battle, user).is_some();
    battle.order.locked = locked.then(|| (user.side, battle.state.at(user).expect("user is active").id));
    // `deductPP` by move: without PP left (Spite, Eerie
    // Spell) the move is not used.
    if !external && !locked && !order::deduct_pp(battle, user, move_id) {
        // BattleActions.runMove records a failed action on its nopp
        // return; next turn Stomping Tantrum/Temper Flare read it.
        power::set_move_result(battle, user, MoveResult::Failed);
        battle.active_move = None;
        return;
    }
    let typed;
    let data = match modify_type(battle, user, move_id) {
        Some(ty) => {
            typed = MoveData { ty, ..*battle.format.dex.move_(move_id) };
            &typed
        }
        None => battle.format.dex.move_(move_id),
    };
    let mon = battle.state.at_mut(user).expect("run_move for an active Pokémon");
    // `moveUsed`, which a Dancer's copy skips; its lock is dropped after
    // (`noLock`).
    let no_lock = external && !mon.volatiles.has(Volatiles::LOCKED_MOVE);
    if !external {
        mon.last_move = Some(move_id);
        mon.last_move_target_loc = target_loc;
    }
    power::begin_use_move(battle, user);
    let user_ref = super::queue::MonRef { side: user.side, id: battle.state.at(user).expect("user is active").id };
    let from = if external {
        Some(MoveFrom::Ability(pikapro_data::gen9::abilities::DANCER))
    } else {
        locked.then_some(MoveFrom::LockedMove)
    };
    let did_something = use_move(battle, user, move_id, data, target, from);
    // The Pokémon may have moved (Ally Switch): the rest follows it.
    let user = super::position_of_mon(battle, user_ref).unwrap_or(user);
    // `if (this.battle.activeMove) move = this.battle.activeMove`: a move
    // called by this one (Metronome, Copycat, Sleep Talk, Magic Bounce) is the move
    // from here on (`lastSuccessfulMoveThisTurn`, `AfterMove`), with its
    // `ActiveMove`'s fields: its type after `ModifyType` (Galvanize,
    // Liquid Voice: Charge's `onAfterMove`) and `hasSheerForce` (Sparkling
    // Aria's).
    let called = battle.order.called_move.take();
    let retyped;
    let (move_id, data) = match battle.active_move {
        Some((_, m)) if (m != move_id || called.is_some()) && (m.0 as usize) < battle.format.dex.moves.len() => {
            let base = battle.format.dex.move_(m);
            match called {
                Some(state) => {
                    battle.move_state.ty = state.ty;
                    battle.move_state.has_sheer_force = state.has_sheer_force;
                    battle.move_state.ignore_ability = state.ignore_ability;
                    match state.ty {
                        Some(ty) if ty != base.ty => {
                            retyped = MoveData { ty, ..*base };
                            (m, &retyped)
                        }
                        _ => (m, base),
                    }
                }
                None => (m, base),
            }
        }
        // The `ActiveMove`'s type after `useMove` changed it (Revelation
        // Dance's `onModifyType`, the -ate abilities).
        _ => match battle.move_state.ty {
            Some(ty) if ty != data.ty => {
                retyped = MoveData { ty, ..*data };
                (move_id, &retyped)
            }
            _ => (move_id, data),
        },
    };
    power::end_use_move(battle, user, move_id, did_something);
    // `singleEvent('AfterMove', move)`: Spit Up, Sparkling Aria, Beak Blast.
    manipulation::after_move(battle, user, move_id);
    multiturn::after_move(battle, user, move_id);
    // `runEvent('AfterMove')`: the user's Charge and `lockedmove`, and the
    // abilities' and items' (`onAnyAfterMove`: White Herb, Mirror Herb,
    // Opportunist, ...), in one sort. A user whose faint was reported
    // (Memento) is no longer active, but the event still runs through its
    // target.
    events::move_event(battle, Event::AfterMove, user, target, move_id, data, events::Relay::True);
    // `if (move.flags['cantusetwice'] && pokemon.removeVolatile(move.id))`:
    // the volatile is still there unless the user left the field.
    let on_field = super::position_of_mon(battle, user_ref)
        .is_some_and(|p| battle.state.side(p.side).active[p.slot as usize].is_some());
    if twice_id.is_some() && on_field {
        battle.hint(crate::event::Hint::CantUseTwice(move_id));
    }
    // Dancers copy a dance move, then a copy's lock ends.
    if !external {
        abilities::misc::dancer(battle, user, did_something);
    }
    if no_lock && let Some(m) = battle.state.at_mut(user) {
        m.volatiles.clear(Volatiles::LOCKED_MOVE);
    }
    // Showdown's Battle#runAction calls clearActiveMove once after the
    // entire runMove/Dancer chain. External runMove calls leave the active
    // move for that outer action; a final aborted Dancer clears it without
    // recording any of the earlier successful copies (Copycat sees the
    // previous action's move).
    if !external {
        order::record_last_move(battle);
    }
    // It comes after the drags (`dragIn`): a Mold Breaker Dragon Tail's
    // target lands on the hazards without its Levitate.
    let dragging = super::switching::any_in_slots(battle, |m| m.force_switch_flag);
    if !external && !dragging {
        battle.active_move = None;
    }
    if !faint_messages(battle, true) {
        check_win(battle, None);
    }
}

/// The type the move's own `ModifyType` gives it before `useMove` (Raging
/// Bull, pledge combos), if it changes it: the caller then copies the
/// dex's data with it, and otherwise uses the dex's data as it is. The rest
/// of its own `onModifyType` and `runEvent('ModifyType')` run in
/// [`use_move`], in Showdown's order.
pub(super) fn modify_type(battle: &Battle, user: Position, move_id: MoveId) -> Option<Type> {
    let ty = field::modify_type(battle, user, move_id)?;
    (ty != battle.format.dex.move_(move_id).ty).then_some(ty)
}

/// The move's data with the type [`modify_type`] gives it (tests).
#[cfg(test)]
pub(super) fn typed_data(battle: &Battle, user: Position, move_id: MoveId) -> MoveData {
    let data = *battle.format.dex.move_(move_id);
    MoveData { ty: modify_type(battle, user, move_id).unwrap_or(data.ty), ..data }
}

/// `runEvent('ModifyType', pokemon, target, move, move)`, after the move's
/// own `ModifyType` and `ModifyMove`: the abilities' and items' (the -ate
/// abilities and Liquid Voice at priority -1, Normalize at 1), then
/// Electrify's (priority -2, so always last; Ion Deluge is not in the Gen 9
/// dex). Electrify after Weather Ball's own Fire in sun
/// makes it Electric.
pub(super) fn modify_type_event(
    battle: &mut Battle,
    user: Position,
    target: Option<Position>,
    move_id: MoveId,
    data: &mut MoveData,
) {
    events::modify_move_event(battle, Event::ModifyType, user, target, move_id, data);
    manipulation::modify_type(battle, user, move_id, data);
}

/// Showdown's `useMoveInner`. Returns whether the move did something.
pub(super) fn use_move(
    battle: &mut Battle,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
    target: Option<Position>,
    from: Option<MoveFrom>,
) -> bool {
    // setActiveMove replaces the global move. A previous nested call's
    // snapshot no longer applies while this new useMove is running.
    battle.order.called_move = None;
    // A new `ActiveMove`: its ability and item fields start over, with the
    // move's own `ignoreAbility` (Sunsteel Strike, Photon Geyser), unless
    // another effect called it (`move.ignoreAbility =
    // sourceEffect.ignoreAbility`).
    // A move called by another (Metronome, Copycat, Sleep Talk) inherits
    // the caller's active `ignoreAbility` (`sourceEffect` is the calling
    // `ActiveMove`): set by Mold Breaker, or by Mycelium Might for the
    // status move that called a damaging one.
    let caller_ignores = battle.move_state.ignore_ability;
    battle.move_state = events::MoveState::default();
    // A move reflected by Magic Bounce (`[from] ability: Magic Bounce`) has
    // `hasBounced`. A move called by an ability (Magic Bounce, Dancer) takes
    // the ability's `ignoreAbility` (none).
    let bounced = from == Some(MoveFrom::Ability(pikapro_data::gen9::abilities::MAGIC_BOUNCE));
    battle.move_state.has_bounced = bounced;
    battle.move_state.is_external = from == Some(MoveFrom::Ability(pikapro_data::gen9::abilities::DANCER));
    battle.move_state.ignore_ability = match battle.order.source_effect {
        Some(caller) if from.is_none() && order::calls_moves(battle.format.dex, caller) => caller_ignores,
        // A move whose action another moved up (Round) has that one's.
        Some(_) if from.is_none() => battle.order.source_ignores_ability,
        // The action a waiting pledge moved up has it as its `sourceEffect`
        // (a locked move's is `lockedmove`).
        _ if from.is_none() && pledges::source(battle, user).is_some() => battle.order.pledge_ignores_ability,
        _ => {
            data.unmodelled.contains(&"ignoreAbility")
                && battle.order.source_effect.is_none()
                && !matches!(from, Some(MoveFrom::LockedMove | MoveFrom::Ability(_)))
        }
    };
    // `ModifyTarget` (Metal Burst, Comeuppance).
    let target = multiturn::modify_target(battle, user, move_id).or(target);
    let target = if matches!(data.target, MoveTarget::User | MoveTarget::Allies) { Some(user) } else { target };
    // `setActiveMove(move, pokemon, target)`: `activeTarget` (Unaware).
    battle.move_state.active_target = target.or(Some(user));
    // The move's own `ModifyMove` (Curse's target depends on the user's
    // type); a changed target type redraws the target.
    let modified;
    let (data, retarget) = match conditions::modify_move(battle, user, move_id, data, target) {
        Some(m) => {
            modified = m;
            (&modified, true)
        }
        None => (data, false),
    };
    let mut target = target;
    if retarget {
        target = targeting::get_random_target(battle, user, data.target);
    }
    if move_id == ids::STRUGGLE {
        // Struggle's `onModifyMove`.
        battle.emit(BattleEvent::StruggleStart { actor: actor(battle, user) });
    }
    // The move's own `onModifyType` / `onModifyMove`, on a copy of its data
    // (Showdown's `ActiveMove`), which the rest of the pipeline uses.
    let mut active = *data;
    let target = power::modify_move(battle, user, move_id, &mut active, target);
    // The move's own `onModifyMove` (Rollout).
    multiturn::modify_move(battle, user, move_id, target);
    // Then `runEvent('ModifyType')`, whose -ate abilities set this active
    // move's `typeChangerBoosted`.
    modify_type_event(battle, user, target, move_id, &mut active);
    // The active move has its type from here on, also when `ModifyMove`
    // stops it (Throat Chop on a called sound move): `AfterMove` reads it.
    battle.move_state.ty = Some(active.ty);
    // `runEvent('ModifyMove')`: its handler sort, then the handlers: the
    // user's status and volatiles, its ability and item (Choice lock, Mold
    // Breaker, Sheer Force, ...), then the field's (speed 0). They change
    // the same active move.
    // `pokemon.lastMoveUsed = move` came first, and holds the active move
    // with its modified type even when `ModifyMove` stops it (Gravity,
    // Throat Chop, Heal Block): Conversion 2 reads that type.
    if !conditions::modify_move_event(battle, user, target, move_id, &mut active) {
        set_last_move_used(battle, user, move_id, active.ty);
        return modify_move_failed(battle, user);
    }
    let data = &active;
    // The active move's category (Ice Face).
    battle.move_state.category = Some(data.category);
    battle.move_state.ty = Some(data.ty);
    // Gravity's `ModifyMove` stops moves it disables.
    if !field::modify_move(battle, user, move_id, data) {
        set_last_move_used(battle, user, move_id, data.ty);
        return modify_move_failed(battle, user);
    }
    // A target type changed from the move's own (`baseTarget`, e.g. by
    // Curse's `onModifyMove` above) re-aims the move again after the events.
    let target = power::retarget(battle, user, move_id, data, target);
    // `lastMoveUsed` is the active move, with its modified type.
    set_last_move_used(battle, user, move_id, data.ty);
    if let Some(m) = battle.state.at_mut(user) {
        // `lastMove` is this active move unless another effect called it.
        if matches!(from, None | Some(MoveFrom::LockedMove)) && m.last_move == Some(move_id) {
            m.last_move_target = Some(data.target);
        }
    }
    if battle.state.at(user).is_none_or(|m| m.is_fainted()) {
        return false;
    }
    // The `|move|` line: `null` without a target; `[from]` for a locked
    // move, the action a waiting pledge moved up or a called move. (`from`
    // counts for the PP below too.) Its actors are only read for the log:
    // without one, they aren't made, and `emit`'s other work, the RNG
    // trace's flush, is done alone (`emit_without_log`).
    let from = from.or_else(|| pledges::source(battle, user).map(MoveFrom::Move));
    let user_actor = battle.log.is_enabled().then(|| actor(battle, user));
    match user_actor {
        Some(user_actor) => {
            let shown = target.map(|t| actor(battle, t));
            let line = order::move_line(battle, user_actor, move_id, shown, from);
            battle.emit(line);
        }
        None => battle.emit_without_log(),
    }
    let no_target = |battle: &mut Battle| {
        battle.attr_last_move(MoveAttr::NoTarget);
        match user_actor {
            Some(user) => battle.emit(BattleEvent::NoTarget { user }),
            None => battle.emit_without_log(),
        }
    };
    let Some(target) = target else {
        no_target(battle);
        return false;
    };
    let (targets, retarget) = targeting::get_move_targets(battle, user, data, target);
    if let Some(t) = retarget {
        let t = actor(battle, t);
        battle.flush_rng_trace();
        battle.log.retarget_last_move(t);
    }
    // `DeductPP`: each pressure target (Pressure) can cost extra PP.
    deduct_pressure_pp(battle, user, move_id, data, &targets, from);
    // `TryMove`: the move's own `onTryMove` (the charging turn of two-turn
    // moves, ...), then `runEvent('TryMove')`.
    if !power::try_move(battle, user, move_id) {
        return false;
    }
    let defender = targets.last().copied().unwrap_or(target);
    // Pollen Puff's `onTryMove` (Heal Block).
    if !conditions::pollen_puff_try_move(battle, user, move_id, defender) {
        return false;
    }
    if !multiturn::try_move(battle, user, move_id, data, defender) {
        return false;
    }
    let tried = events::move_event(battle, Event::TryMove, user, Some(defender), move_id, data, events::Relay::True);
    if !tried.is_truthy() {
        // `useMoveInner` returns the `null` (Heavy Rain, Extreme Sun), which
        // becomes `moveThisTurnResult`: not a failure (Stomping Tantrum).
        if tried == events::Relay::Null {
            power::set_move_result(battle, user, MoveResult::Null);
        }
        return false;
    }
    effects::self_destruct(battle, user, data);
    let last_target = targets.last().copied();
    let hit =
        if matches!(data.target, MoveTarget::All | MoveTarget::FoeSide | MoveTarget::AllySide | MoveTarget::AllyTeam) {
            try_move_hit(battle, &targets, user, move_id, data)
        } else {
            if targets.is_empty() {
                no_target(battle);
                return false;
            }
            try_spread_move_hit(battle, targets, user, move_id, data)
        };
    // `selfBoost`, `MoveFail`, `AfterMoveSecondarySelf`.
    power::after_use_move(battle, user, move_id, data, last_target, hit)
}

/// `pokemon.lastMoveUsed`: the active move and its type (Conversion 2).
fn set_last_move_used(battle: &mut Battle, user: Position, move_id: MoveId, ty: Type) {
    if let Some(m) = battle.state.at_mut(user) {
        m.volatile_counters.last_move_used = Some((move_id, ty));
    }
}

/// Showdown's `tryMoveHit`, for moves that target the field or a side:
/// `Try` / `PrepareHit`, then `moveHit` on the first target, whose side
/// (or the field) the effect applies to.
fn try_move_hit(battle: &mut Battle, targets: &Targets, user: Position, move_id: MoveId, data: &MoveData) -> bool {
    if !field::handles(move_id)
        && !manipulation::handles(move_id)
        && !conditions::handles(move_id)
        && !multiturn::handles(move_id)
        && !itemmoves::handles(move_id)
        && !data.is_data_only()
    {
        // Every field and side move standard in Gen 9 or Champions is
        // implemented; this stops the Past ones (Crafty Shield, Mat Block).
        battle.emit(BattleEvent::Fail { user: actor(battle, user) });
        return false;
    }
    let Some(&target) = targets.first() else {
        battle.emit(BattleEvent::NoTarget { user: actor(battle, user) });
        return false;
    };
    if !field::try_move(battle, move_id) || !protect::prepare_hit(battle, user, move_id) {
        fail_still(battle, user);
        return false;
    }
    // `runEvent('PrepareHit')` (Protean, Libero), then `TryHitSide` for
    // side moves (Magic Bounce).
    if !prepare_hit_event(battle, user, target, move_id, data) {
        return false;
    }
    if matches!(data.target, MoveTarget::FoeSide | MoveTarget::AllySide | MoveTarget::AllyTeam) {
        match events::move_event(battle, Event::TryHitSide, target, Some(user), move_id, data, events::Relay::True) {
            events::Relay::False => {
                fail_still(battle, user);
                return false;
            }
            r if !r.is_truthy() => return false,
            _ => {}
        }
    }
    // `runEvent('TryHitField')` for a field move: no condition, ability or
    // item handles it in Gen 9 (nor the moves' own `onTryHitField`), so it
    // never stops the move nor draws. The guards are `TryHit` handlers.
    // The `onHit` of ally-team moves (Heal Bell).
    if let Some(did) = manipulation::on_hit(battle, target, user, move_id) {
        if did == Outcome::Failed {
            fail_still(battle, user);
        }
        return did == Outcome::Succeeded;
    }
    // `runMoveEffects`: a move that does nothing fails.
    // Then the move's `onHitField` (Perish Song).
    let effects = field::run_field_effects(battle, target, user, data);
    let mut did = matches!(effects, Outcome::Undefined | Outcome::Succeeded)
        && conditions::hit_field(battle, user, move_id)
        && itemmoves::hit_field(battle, target, user, move_id);
    // `selfSwitch` (Chilly Reception) succeeds if something
    // can come in, and the user switches out even if the weather failed.
    let self_switch = data.self_switch.is_some();
    let commanded = battle.state.at(user).is_some_and(|m| m.volatiles.has(Volatiles::COMMANDED));
    if self_switch && switching::can_switch(battle, user.side) > 0 && !commanded {
        did = true;
    }
    if !did {
        // `didAnything === null` (a weather a primal weather stopped, which
        // said so): no `-fail`. With `selfSwitch` it became `false`.
        if effects != Outcome::Null || self_switch {
            fail_still(battle, user);
        }
        return false;
    }
    if self_switch {
        switching::self_switch(battle, user, move_id);
    }
    true
}

/// Showdown's `trySpreadMoveHit`: `PrepareHit`, then each hit step over all
/// remaining targets before the next step.
fn try_spread_move_hit(
    battle: &mut Battle,
    mut targets: Targets,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
) -> bool {
    // `spreadHit` is decided before any target is filtered out
    // (never for `smartTarget` moves).
    let spread = targets.len() > 1 && !order::smart_target(battle);

    // Try / PrepareHit.
    // The move's `onTry`.
    match manipulation::on_try(battle, user, move_id) {
        Outcome::Undefined => {}
        Outcome::Failed => {
            fail_still(battle, user);
            return false;
        }
        _ => return false,
    }
    // The move's own `onTry`, which announces its failure.
    if !power::on_try(battle, user, move_id) {
        return false;
    }
    // Try / PrepareHit. The multi-turn moves' `onTry`s can also end the
    // move successfully (Future Sight).
    match multiturn::on_try(battle, user, targets[0], move_id) {
        multiturn::TryResult::Continue => {}
        multiturn::TryResult::Fail => {
            fail_still(battle, user);
            return false;
        }
        multiturn::TryResult::Stop => return true,
    }
    if !volatiles::try_move(battle, user, move_id)
        || !switching::on_try(battle, user, move_id)
        || !field::try_move(battle, move_id)
        || !order::on_try(battle, user, targets[0], move_id)
        || !itemmoves::on_try(battle, user, targets[0], move_id)
        || !protect::prepare_hit(battle, user, move_id)
        || !conditions::prepare_hit(battle, user, move_id)
        || !order::prepare_hit(battle, user, move_id)
    {
        fail_still(battle, user);
        return false;
    }
    // Fling's `onPrepareHit` sets the move's power and
    // effects from the item.
    let flung;
    let data = match itemmoves::prepare_hit(battle, user, move_id, data) {
        Ok(Some(d)) => {
            flung = d;
            &flung
        }
        Ok(None) => data,
        Err(()) => {
            fail_still(battle, user);
            return false;
        }
    };
    // A pledge waiting for its ally's ends without failing (`null`).
    if !field::prepare_hit(battle, user, move_id) {
        return false;
    }
    // `runEvent('PrepareHit')` (Protean, Libero; Parental Bond, which
    // needs `move.spreadHit`).
    battle.move_state.spread_hit = spread;
    // The `[anim]` of the moves' own `onPrepareHit`.
    anim_attr(battle, user, targets[0], move_id, data);
    if !prepare_hit_event(battle, user, targets[0], move_id, data) {
        return false;
    }
    // Parental Bond's `move.multihit = 2`.
    let parental_bond;
    let data = if battle.move_state.parental_bond {
        parental_bond = MoveData { multihit: Some((2, 2)), ..*data };
        &parental_bond
    } else {
        data
    };
    // Status moves go through the pipeline when their data describes them
    // fully or a module implements them.
    if data.category == MoveCategory::Status
        && !protect::handles(move_id)
        && !status::handles(move_id)
        && !volatiles::handles(move_id)
        && !switching::handles(move_id)
        && !manipulation::handles(move_id)
        && !power::handles(move_id)
        && !field::handles(move_id)
        && !order::handles(move_id)
        && !itemmoves::handles(move_id)
        && !data.is_data_only()
    {
        // Every status move standard in Gen 9 or Champions is implemented;
        // this stops the Past ones.
        battle.emit(BattleEvent::Fail { user: actor(battle, user) });
        return false;
    }

    // Every step's `false` is a failure (`atLeastOneFailure`); only
    // protection's `NOT_FAIL` is not.
    let initial = targets.len();
    // hitStepInvulnerabilityEvent (Fly, Dig, ...).
    let miss_attr = !battle.move_state.spread_hit;
    retain_targets(battle, &mut targets, |battle, t| multiturn::invulnerability(battle, t, user, move_id, miss_attr));
    order::after_hit_step(battle, targets.len() < initial);
    // hitStepTryHitEvent. `TryHit` handlers run left to right by priority
    // over all targets: Psychic Terrain (4) before Protect (3).
    let before = targets.len();
    // Wide Guard's and Quick Guard's `NOT_FAIL` counts as protection.
    let mut guarded = 0;
    retain_targets(battle, &mut targets, |battle, t| {
        if !field::guard_try_hit(battle, t, data) {
            guarded += 1;
            return false;
        }
        field::try_hit(battle, t, user, move_id, data)
    });
    // Protect's `NOT_FAIL` is not a failure for `smartTarget`; a terrain's
    // `null` is.
    order::after_hit_step(battle, targets.len() + guarded < before);
    let before_protect = targets.len();
    retain_targets(battle, &mut targets, |battle, t| protect::try_hit(battle, t, user, data));
    // Protection returns `NOT_FAIL`, which is not a failure.
    let protected = before_protect - targets.len() + guarded;
    // The abilities' and items' `TryHit` (Volt Absorb, Magic Bounce,
    // Wonder Guard, ...: priority 1 and below, after protection's 3).
    try_hit_event(battle, &mut targets, user, move_id, data);
    // hitStepTypeImmunity. Status moves ignore immunity unless they say
    // otherwise (Thunder Wave's `ignoreImmunity: false`); Struggle is
    // typeless ("???") and hits everything.
    if !ignores_type_immunity(battle, move_id, data) {
        let before = targets.len();
        retain_targets(battle, &mut targets, |battle, t| {
            // `runImmunity(move, !move.smartTarget)` (added types; Ground
            // checks `isGrounded`: Levitate, Smack Down, Magnet Rise,
            // Ingrain, Air Balloon, ...).
            let message = !order::smart_target(battle);
            events::run_immunity(battle, t, data.ty, message)
        });
        order::after_hit_step(battle, targets.len() < before);
    }
    // hitStepTryImmunity.
    let before = targets.len();
    retain_targets(battle, &mut targets, |battle, t| {
        protect::try_immunity(battle, t, user, data)
            && power::try_immunity(battle, t, user, move_id)
            && conditions::try_immunity(battle, t, user, move_id)
            // The move's own `TryImmunity`, then Dark types are immune to
            // Prankster moves.
            && itemmoves::try_immunity(battle, t, user, move_id)
            && abilities::misc::prankster_immunity(battle, t, user, data.status)
    });
    order::after_hit_step(battle, targets.len() < before);
    // hitStepAccuracy.
    retain_targets(battle, &mut targets, |battle, t| effects::hit_step_accuracy(battle, user, t, move_id, data));
    if targets.is_empty() {
        attr_spread(battle, spread, &targets);
        // `move.hitTargets` for `AfterMove`.
        manipulation::record_hit_targets(battle, user, move_id, &[]);
        // Without `atLeastOneFailure` (only protection stopped the move),
        // `moveThisTurnResult` is `null`.
        if protected == initial {
            power::set_move_result(battle, user, MoveResult::Null);
        }
        return false;
    }
    // hitStepBreakProtect.
    protect::break_protect(battle, &targets, move_id);
    if move_id == ids::SPECTRAL_THIEF {
        boosts::steal_positive_boosts(battle, user, targets[0], move_id);
    }
    let hit = move_hit_loop(battle, &targets, user, move_id, data, spread);
    // `move.hitTargets` for `AfterMove`.
    let hit_targets: &[Position] = if hit { &targets } else { &[] };
    manipulation::record_hit_targets(battle, user, move_id, hit_targets);
    attr_spread(battle, spread, hit_targets);
    hit
}

/// The end of `trySpreadMoveHit`, `attrLastMove('[spread] ' +
/// hitSlot)` for a spread move, with the targets left after the hit steps.
#[inline(always)]
fn attr_spread(battle: &mut Battle, spread: bool, targets: &[Position]) {
    if spread && battle.log.is_enabled() {
        logged_attr_spread(battle, targets);
    }
}

/// [`attr_spread`] for a spread move, with the log on.
fn logged_attr_spread(battle: &mut Battle, targets: &[Position]) {
    let mut slots = crate::event::SpreadSlots::default();
    for &t in targets.iter().take(slots.slots.len()) {
        slots.slots[slots.len as usize] = t;
        slots.len += 1;
    }
    battle.attr_last_move(MoveAttr::Spread(slots));
}

/// The `attrLastMove('[anim] ...')` of Ivy Cudgel's, Shell Side
/// Arm's and Tera Blast's `onPrepareHit`.
#[inline(always)]
fn anim_attr(battle: &mut Battle, user: Position, target: Position, move_id: MoveId, data: &MoveData) {
    if battle.log.is_enabled() && matches!(move_id, ids::IVY_CUDGEL | ids::SHELL_SIDE_ARM | ids::TERA_BLAST) {
        logged_anim_attr(battle, user, target, move_id, data);
    }
}

/// [`anim_attr`] for one of those moves, with the log on.
fn logged_anim_attr(battle: &mut Battle, user: Position, target: Position, move_id: MoveId, data: &MoveData) {
    use crate::event::MoveAnim;
    let anim = match move_id {
        ids::IVY_CUDGEL if data.ty != pikapro_data::Type::Grass => MoveAnim::IvyCudgel(data.ty),
        ids::SHELL_SIDE_ARM if user.side != target.side => MoveAnim::ShellSideArm(data.category),
        ids::TERA_BLAST => match battle.state.at(user) {
            Some(m) if m.terastallized => MoveAnim::TeraBlast(m.tera_type),
            _ => return,
        },
        _ => return,
    };
    battle.attr_last_move(MoveAttr::Anim(anim));
}

/// `if (!move.spreadHit) attrLastMove('[miss]')` before `-miss`.
pub(super) fn attr_miss(battle: &mut Battle) {
    if !battle.move_state.spread_hit {
        battle.attr_last_move(MoveAttr::Miss);
    }
}

/// Keeps the targets for which `step` returns true (a hit step's
/// `targets.filter(...)`), running `step` on every target in order.
fn retain_targets(battle: &mut Battle, targets: &mut Targets, mut step: impl FnMut(&mut Battle, Position) -> bool) {
    // In place (`StackVec::retain` runs `keep` on every element, in order).
    targets.retain(|&t| step(battle, t));
}

/// Showdown's `hitStepMoveHitLoop`: each hit runs `spreadMoveHit`, then
/// `Update` events; after the last hit, faints, `-hitcount`, recoil and
/// `Update` again. Returns whether any target was hit (Showdown filters the
/// targets by the returned damage).
fn move_hit_loop(
    battle: &mut Battle,
    targets: &Targets,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
    spread: bool,
) -> bool {
    let hits = effects::hit_count(battle, user, data);
    let mut total_damage = 0u32;
    // `targetsCopy` and `moveDamage`: the last `spreadMoveHit`'s targets
    // and damage, that hit's even if it failed on every target.
    let mut last_hit = power::LastHit::default();
    // `damage`: the last hit that didn't fail on every target (the loop
    // breaks before a failed one updates it).
    let mut damage = power::LastHit::default();
    // With `smartTarget` at two targets, `moveDamage` collects every hit's
    // (one target each) for `gotAttacked`.
    let mut smart_hits = power::LastHit::default();
    let mut hit = 1;
    // `move.hit`: the last hit started.
    let mut started = 0;
    // `move.sleepUsable || dex.moves.get(move.sourceEffect).sleepUsable`.
    let sleep_usable = order::sleep_usable(battle, move_id);
    while hit <= hits {
        let asleep = battle.state.at(user).is_some_and(|m| matches!(m.status, Status::Sleep { .. }));
        if hit > 1 && asleep && !sleep_usable {
            break;
        }
        if targets.iter().all(|&t| battle.state.at(t).is_none_or(|m| m.is_fainted())) {
            break;
        }
        started = hit;
        // `smartTarget` (Dragon Darts): each hit on its own target.
        let hit_targets = order::hit_targets(battle, user, move_id, targets, hit);
        if !effects::multi_accuracy(battle, user, hit_targets[0], move_id, data, hit) {
            // `targetsCopy = targets.slice(0)` came before the accuracy
            // check (`hitStepMoveHitLoop`), and `moveDamage` is still the
            // last hit's: every target gets `gotAttacked` and the
            // `AfterMoveSecondary` effects, one behind a substitute that
            // took the last hit too, with damage 0 (Eject Button, Rage
            // Fist's count, Comeuppance).
            if !(order::smart_target(battle) && targets.len() > 1) {
                // `gotAttacked`'s entry for a target behind the substitute,
                // a number that `getLastDamagedBy` finds (Comeuppance and
                // Metal Burst: `0 * 1.5 || 1`); the pipeline records the
                // others with their hits.
                let user_ref = super::queue::MonRef {
                    side: user.side,
                    id: battle.state.side(user.side).team[user.slot as usize].id,
                };
                for &t in last_hit.substitute.iter() {
                    if t.side != user.side
                        && let Some(m) = battle.state.at_mut(t)
                        && !m.is_fainted()
                    {
                        multiturn::remember_damage(m, user_ref, Some(user), 0);
                    }
                }
                last_hit = last_hit.all_targets(targets);
            }
            break;
        }
        // `move.hit = hit` (Disguise, Tera Shell).
        battle.move_state.hit_number = hit;
        let Some(dealt) = spread_move_hit(battle, &hit_targets, user, move_id, data, spread, hit, &mut last_hit) else {
            break;
        };
        damage = last_hit;
        // A `smartTarget` move's `hitTargets` is its last
        // hit's target (Magician).
        if order::smart_target(battle) && targets.len() > 1 {
            battle.move_state.smart_hit_target = Some(hit_targets[0]);
        }
        total_damage += dealt;
        // `move.totalDamage` (Innards Out, Berserk).
        battle.move_state.total_damage = total_damage;
        if order::smart_target(battle) && targets.len() > 1 {
            for &entry in last_hit.hits.iter() {
                smart_hits.hits.push(entry);
            }
        }
        each_update(battle);
        hit += 1;
        if targets.len() == 1 && battle.state.at(user).is_none_or(|m| m.is_fainted()) {
            break;
        }
    }
    // `move.totalDamage` (Shell Bell).
    battle.move_state.total_damage_of = (move_id, total_damage);
    // `if (hit === 1) return damage.fill(false)`: nothing else happens.
    if hit == 1 {
        return false;
    }
    // `faintMessages(false, false, !pokemon.hp)`: faints are reported
    // before recoil; the win is only checked here if the user is down (or
    // a faint caused another, as Destiny Bond does).
    let user_fainted = battle.state.at(user).is_none_or(|m| m.is_fainted());
    // Showdown carries on even if this ended the battle (a user fainted by
    // Explosion): the `Update` sort below still draws.
    faint_messages(battle, user_fainted);
    // The Champions mod doesn't announce a Parental Bond move's hits when
    // it stopped after the first (`move.hit === 1`).
    let bond_once = battle.move_state.parental_bond
        && started == 1
        && battle.format.mechanics == crate::format::Mechanics::Champions;
    // The move's data without its hit count, copied only then.
    let single;
    let counted = if bond_once {
        single = MoveData { multihit: None, ..*data };
        &single
    } else {
        data
    };
    effects::after_hit_loop(battle, user, targets[0], move_id, counted, hit - 1, total_damage);
    // `gotAttacked` / `timesAttacked`: a `smartTarget` move at two targets
    // (Dragon Darts) counts one attack per target hit.
    let attacks = if order::smart_target(battle) && targets.len() > 1 {
        power::record_attacks(battle, user, &smart_hits, 1);
        &smart_hits
    } else {
        power::record_attacks(battle, user, &last_hit, hit - 1);
        &last_hit
    };
    // `getLastAttackedBy().damage` (Berserk, Anger Shell).
    for &(t, dealt) in attacks.hits.iter() {
        battle.move_state.last_damage[t.side.index() * 2 + (t.slot as usize & 1)] = dealt.unwrap_or(0);
    }
    each_update(battle);
    // afterMoveSecondaryEvent: the move's own, then `runEvent` on the
    // targets (frz's `thawsTarget`; Color Change, Pickpocket, Red Card,
    // Eject Button, ...), unless Sheer Force dropped the secondaries.
    if !sheer_force(battle, user) {
        // The last hit's targets (`hitStepMoveHitLoop`:
        // `targetsCopy.filter(val => !!val)`): not those whose substitute
        // took it (all of them for a `smartTarget` move), except after a
        // multi-accuracy move's later hit missed (`targetsCopy` was reset to
        // every target before the accuracy check), and none if that
        // hit failed on every target (a multi-hit move's second hit on a
        // Mimikyu whose Disguise took the first: busted, it's a Ghost
        // again; its Eject Button stays). A target the move
        // knocked out stays: `faintMessages` has made it inactive, but
        // `findEventHandlers` still collects its handlers (the user is
        // active) and `checkFainted` has not cleared its status, so a
        // fainted frozen target's `frz` handler takes part in the
        // `speedSort` and its ties.
        let hit: Targets =
            if order::smart_target(battle) { *targets } else { last_hit.hits.iter().map(|&(t, _)| t).collect() };
        let mut relays = [events::Relay::True; 4];
        let n = hit.len();
        events::run_event_multi(
            battle,
            Event::AfterMoveSecondary,
            &hit,
            Some(user),
            Effect::Move(move_id),
            &mut relays[..n],
            Some(data),
        );
    }
    // Emergency Exit and Wimp Out of targets that fell below half HP: every
    // target of the hit loop, its `damage[i]` 0 if its last hit dealt none
    // (a substitute took it, or it failed), `move.totalDamage` if it is the
    // only one. A `smartTarget` move at two targets keeps only the last
    // started hit's entry (`damage = [damage[hit - 1]]`), a number only if
    // that hit ran.
    if !sheer_force(battle, user) {
        let smart = order::smart_target(battle) && targets.len() > 1;
        let checked: Targets = match (smart, targets.get(started as usize - 1)) {
            (false, _) => *targets,
            (true, Some(&t)) if started + 1 == hit => [t].into_iter().collect(),
            (true, _) => Targets::new(),
        };
        for &t in checked.iter() {
            let dealt = if targets.len() == 1 {
                total_damage as u16
            } else {
                damage.hits.iter().find(|&&(p, _)| p == t).and_then(|&(_, d)| d).unwrap_or(0)
            };
            let Some(m) = battle.state.at(t).filter(|m| m.hp > 0) else { continue };
            let before = m.volatile_counters.hurt_hp.saturating_add(dealt);
            if (m.hp as u32) * 2 <= m.max_hp() as u32 && (before as u32) * 2 > m.max_hp() as u32 {
                super::run_mon_event_from(battle, Event::EmergencyExit, t, Some(user));
            }
        }
    }
    true
}

/// `move.hasSheerForce && pokemon.hasAbility('sheerforce')`: the move's
/// secondaries were dropped, and with them its `AfterMoveSecondary`
/// effects (Life Orb's recoil, Emergency Exit, ...).
pub(super) fn sheer_force(battle: &Battle, user: Position) -> bool {
    battle.move_state.has_sheer_force
        && abilities::has_ability(battle, user, pikapro_data::gen9::abilities::SHEER_FORCE)
}

/// `runEvent('EmergencyExit', pokemon, source)` when the Pokémon at `pos`
/// has fallen to half its max HP or below from above `hp_before`, and still
/// has HP (`spreadMoveHit`, the residual, `runSwitch`: `pokemon.hp && ...`).
pub(super) fn emergency_exit(battle: &mut Battle, pos: Position, source: Option<Position>, hp_before: u16) {
    if battle.state.at(pos).is_some_and(|m| m.hp > 0) {
        emergency_exit_at_any_hp(battle, pos, source, hp_before);
    }
}

/// [`emergency_exit`] without the HP check: `applyRecoilDamage` and
/// `useMoveInner`'s `MoveFail` / `AfterMoveSecondarySelf` checks only
/// `pokemon.hp <= pokemon.maxhp / 2 && originalHp > pokemon.maxhp / 2`, so
/// recoil or crash damage that knocks the user out from above half HP
/// triggers it too (the user stays active until `faintMessages`).
pub(super) fn emergency_exit_at_any_hp(battle: &mut Battle, pos: Position, source: Option<Position>, hp_before: u16) {
    let Some(m) = battle.state.at(pos) else { return };
    let max = m.max_hp() as u32;
    if (m.hp as u32) * 2 <= max && (hp_before as u32) * 2 > max {
        super::run_mon_event_from(battle, Event::EmergencyExit, pos, source.or(Some(pos)));
    }
}

/// Showdown's `runEvent('PrepareHit', pokemon, target, move)` after the
/// move's own `Try` / `PrepareHit`: `false` fails the move (`-fail`).
fn prepare_hit_event(battle: &mut Battle, user: Position, target: Position, move_id: MoveId, data: &MoveData) -> bool {
    match events::move_event(battle, Event::PrepareHit, user, Some(target), move_id, data, events::Relay::True) {
        events::Relay::False => {
            fail_still(battle, user);
            false
        }
        r => r.is_truthy(),
    }
}

/// The abilities' and items' part of `hitStepTryHitEvent`
/// (`runEvent('TryHit', targets, pokemon, move)`): targets whose handler
/// returned a falsy value are dropped; `-fail` if none is left `true` and
/// one returned `false`.
pub(super) fn try_hit_event(
    battle: &mut Battle,
    targets: &mut Targets,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
) {
    if targets.is_empty() {
        return;
    }
    let mut relays = [events::Relay::True; 4];
    let n = targets.len();
    events::run_event_multi(
        battle,
        Event::TryHit,
        targets,
        Some(user),
        Effect::Move(move_id),
        &mut relays[..n],
        Some(data),
    );
    let relays = &relays[..n];
    if relays.iter().all(|&r| r == events::Relay::True) {
        return;
    }
    let failed = relays.contains(&events::Relay::False);
    if failed && !relays.contains(&events::Relay::True) {
        fail_still(battle, user);
    }
    let mut kept = Targets::new();
    for (i, &t) in targets.iter().enumerate() {
        if relays[i].is_truthy() || relays[i] == events::Relay::Num(0) {
            kept.push(t);
        }
    }
    *targets = kept;
    // `hitResults[i] || false` (all but `NOT_FAIL`, which no `TryHit`
    // handler here returns) after the `-fail` check: a `null` is a failure
    // too.
    let failure = relays.iter().any(|&r| !r.is_truthy());
    order::after_hit_step(battle, failure);
}

/// `useMoveInner`'s `DeductPP` loop: each of the move's pressure targets
/// (Pressure's `onDeductPP`) can cost extra PP, unless the move was called
/// by a non-move effect (a locked move).
fn deduct_pressure_pp(
    battle: &mut Battle,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
    targets: &Targets,
    from: Option<MoveFrom>,
) {
    // A move copied by Dancer (an ability, which has no PP) costs none.
    if matches!(from, Some(MoveFrom::LockedMove | MoveFrom::Ability(_))) {
        return;
    }
    let mut pressure = Targets::new();
    // `getMoveTargets`' early returns (a single target that fainted
    // or no foe to retarget to) give no `pressureTargets`, even for a
    // `mustpressure` move (Tera Blast at a fainted ally). Only the requests
    // show the PP.
    let single = !matches!(
        data.target,
        MoveTarget::All
            | MoveTarget::FoeSide
            | MoveTarget::AllySide
            | MoveTarget::AllyTeam
            | MoveTarget::AllAdjacent
            | MoveTarget::AllAdjacentFoes
            | MoveTarget::Allies
    );
    if single && targets.is_empty() {
        return;
    }
    if data.flags.contains(pikapro_data::MoveFlags::MUST_PRESSURE) {
        for slot in 0..battle.format.active_per_side() {
            let foe = Position::new(user.side.foe(), slot as u8);
            if battle.state.at(foe).is_some_and(|m| !m.is_fainted()) {
                pressure.push(foe);
            }
        }
    } else if data.target != MoveTarget::FoeSide {
        pressure = *targets;
    }
    let mut extra = 0;
    for &t in pressure.iter() {
        if let events::Relay::Num(n) =
            events::move_event(battle, Event::DeductPP, t, Some(user), move_id, data, events::Relay::True)
        {
            extra += n.max(0) as u8;
        }
    }
    if extra > 0 {
        // `deductPP(callerMoveForPressure || move)`: the calling move
        // (Metronome, Copycat, ...), or the first Pledge of a combination
        // (its `sourceEffect`), which the user usually doesn't know.
        let combo = match from {
            Some(MoveFrom::Move(m)) => Some(m),
            _ => None,
        };
        let caller = battle.order.source_effect.or(combo).unwrap_or(move_id);
        order::deduct_extra_pp(battle, user, caller, extra);
    }
}

/// Showdown's `spreadMoveHit` for one hit: damage for every target
/// (`getSpreadDamage`), then apply it all (`spreadDamage`, with drain), then
/// `runMoveEffects`, `selfDrops`, `secondaries` and `forceSwitch` on the
/// targets that did not fail. Returns the damage dealt, or `None` if the hit
/// failed on every target. `last_hit` records the targets and their damage
/// (the `[damage, targets]` it returns, Showdown's `moveDamage` and
/// `targetsCopy`): a failed move's own `TryHit` keeps every target, with
/// no damage; any other failure leaves none.
#[expect(clippy::too_many_arguments, reason = "mirrors spreadMoveHit's inputs")]
fn spread_move_hit(
    battle: &mut Battle,
    targets: &Targets,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
    spread: bool,
    hit: u8,
    last_hit: &mut power::LastHit,
) -> Option<u32> {
    let mut total = 0;
    let targets_in_order = *targets;
    // The move's own `TryHit` (a `singleEvent` on the first target):
    // `return [[false], targets]`.
    let tried = volatiles::try_hit(battle, targets[0], user, move_id)
        && field::move_try_hit(battle, targets[0], user, move_id)
        && order::try_hit(battle, targets[0], user, move_id)
        // The move's own `onTryHit`.
        && manipulation::try_hit(battle, targets[0], user, move_id)
        && itemmoves::try_hit(battle, targets[0], user, move_id);
    if !tried {
        last_hit.hits.clear();
        last_hit.substitute.clear();
        for &t in targets.iter() {
            last_hit.hits.push((t, None));
        }
        return None;
    }
    // Showdown's Psycho Shift `onTryHit` (data/moves.ts) sets move.status
    // from the user. Keep it on a local ActiveMove so primary status,
    // immunity and selfDrops follow the normal spreadMoveHit pipeline.
    let psycho_shift;
    let data = if move_id == ids::PSYCHO_SHIFT {
        let status = battle.state.at(user).and_then(|m| status::kind(m.status));
        psycho_shift = MoveData { status, ..*data };
        &psycho_shift
    } else {
        data
    };
    // Uproar's `onTryHit` wakes everyone up.
    multiturn::try_hit(battle, targets[0], move_id);
    // `tryPrimaryHitEvent`: a substitute takes the hit instead.
    let typeless = move_id == ids::STRUGGLE;
    // A substitute's `onTryPrimaryHit` calls `getDamage`: `false` (immune)
    // fails like `undefined`.
    let primary = conditions::try_primary_hit(battle, targets, user, move_id, data, |battle, t| {
        if !damage_immunity(battle, t, move_id, data) {
            return None;
        }
        get_damage(battle, user, t, move_id, data, spread, typeless, hit)
    });
    let mut targets = primary.hit;
    // The damage dealt to each target (a number in Showdown).
    let mut dealt_to = [None::<u16>; 4];
    // Damage is `undefined` for status moves and Pollen Puff on an ally.
    let damaging = !targets.is_empty() && order::deals_damage(user, targets[0], move_id, data);
    if damaging {
        // getSpreadDamage. `None` is Showdown's `undefined` (no damage).
        let mut damages = [None::<u32>; 4];
        let mut immune = Targets::new();
        for (i, &t) in targets.iter().enumerate() {
            // `this.battle.activeTarget = target` (Unaware).
            battle.move_state.active_target = Some(t);
            if !damage_immunity(battle, t, move_id, data) {
                // `damage calculation interrupted`: `damage[i] = false`.
                immune.push(t);
                continue;
            }
            // `spreadDamage` floors it (`clampIntRange`).
            damages[i] = get_damage(battle, user, t, move_id, data, spread, typeless, hit).map(|h| h / 2);
        }
        if !immune.is_empty() {
            // `targets[i] = false` for them. With nothing else hit,
            // `runMoveEffects` combines only `false`s: `-fail`, `[still]`
            // (unless the move has `self` effects or makes its user faint),
            // and the hit loop stops.
            if immune.len() == targets.len() {
                let has_self = data.self_boosts.is_some() || data.self_volatile.is_some() || order::has_self(move_id);
                if !has_self && !data.self_destruct {
                    fail_still(battle, user);
                }
                last_hit.hits.clear();
                last_hit.substitute.clear();
                return None;
            }
            let mut j = 0;
            for i in 0..targets.len() {
                if !immune.contains(&targets[i]) {
                    damages[j] = damages[i];
                    j += 1;
                }
            }
            targets.retain(|t| !immune.contains(t));
        }
        // spreadDamage.
        for (i, &t) in targets.iter().enumerate() {
            let Some(amount) = damages[i] else { continue };
            // `runEvent('Damage')` (Endure, the move's own `onDamage`,
            // Focus Sash, ...) runs in `apply_damage`.
            let dealt = apply_damage(battle, t, amount, DamageSource::Move);
            dealt_to[i] = Some(dealt);
            total += dealt as u32;
            effects::drain(battle, user, t, dealt, data);
            // `DamagingHit` (Counter) and `gotAttacked`.
            multiturn::damaged(battle, t, user, data, dealt);
        }
    }
    let all = targets;
    let (kept, not_fail) = run_move_effects(battle, &targets, user, move_id, data, damaging);
    last_hit.hits.clear();
    for &t in kept.iter() {
        let i = all.iter().position(|&a| a == t).expect("kept targets come from the hit's targets");
        last_hit.hits.push((t, dealt_to[i]));
    }
    last_hit.substitute.clear();
    for &t in primary.substitute.iter() {
        last_hit.substitute.push(t);
    }
    // Targets whose substitute took the hit still get `selfDrops` and roll
    // their secondaries, in target order.
    let mut effect_targets = Targets::new();
    for &t in targets_in_order.iter() {
        if kept.contains(&t) || primary.substitute.contains(&t) {
            effect_targets.push(t);
        }
    }
    let targets = &kept;
    // A substitute that took the hit counts as the move doing something, so
    // U-turn and the like still switch the user out.
    if !primary.substitute.is_empty() && data.self_switch.is_some() && data.category != MoveCategory::Status {
        switching::self_switch(battle, user, move_id);
    }
    secondaries::self_drops(battle, &effect_targets, user, move_id, data);
    // `spreadMoveHit`'s `secondaries` step, after `selfDrops`.
    for &t in effect_targets.iter() {
        if primary.substitute.contains(&t) {
            secondaries::substitute_secondaries(battle, user, data);
        } else {
            secondaries::secondaries(battle, &[t].into_iter().collect(), user, data);
        }
    }
    // A target whose `DragOut` failed the move counts as not hit
    // (`damage[i] = false`: the hit loop stops without `Update`s).
    let mut drag_failed = 0;
    if data.force_switch {
        drag_failed = switching::force_switch(battle, targets, user, data).len();
    }
    // `runEvent('DamagingHit', damagedTargets, pokemon, move, damage)`,
    // left to right (frz thaws; Static, Rough Skin, Rocky Helmet, ...).
    if damaging {
        let mut damaged = Targets::new();
        let mut relays = [events::Relay::Num(0); 4];
        for &t in targets.iter() {
            let i = all.iter().position(|&a| a == t).expect("kept targets come from the hit's targets");
            if let Some(d) = dealt_to[i] {
                relays[damaged.len()] = events::Relay::Num(d as i32);
                damaged.push(t);
            }
        }
        let user_hp = battle.state.at(user).map_or(0, |m| m.hp);
        let n = damaged.len();
        events::run_event_multi(
            battle,
            Event::DamagingHit,
            &damaged,
            Some(user),
            Effect::Move(move_id),
            &mut relays[..n],
            Some(data),
        );
        // The move's `onAfterHit`, on each damaged target, while the user
        // has HP (the Champions mod's `spreadMoveHit` runs it even after the
        // user fainted: Rough Skin during Mortal Spin).
        let champions = battle.format.mechanics == crate::format::Mechanics::Champions;
        let user_mon = battle.state.side(user.side).team.get(user.slot as usize);
        if user_mon.is_some_and(|m| m.hp > 0 || champions) {
            for &t in damaged.iter() {
                field::after_hit(battle, t, user, move_id);
                itemmoves::after_hit(battle, t, user, move_id);
            }
        }
        // The user fell below half HP (Rough Skin, Rocky Helmet).
        emergency_exit(battle, user, None, user_hp);
    }
    // A `NOT_FAIL` result or a substitute that took the hit
    // or blocked a status move is not a failure, so the hit counts.
    let counts = targets.len() > drag_failed || not_fail || !primary.substitute.is_empty() || primary.blocked;
    if !counts {
        // Every target is `false`.
        last_hit.hits.clear();
    }
    counts.then_some(total)
}

/// Whether `runImmunity(move)` skips the type check: status moves (bar
/// Thunder Wave's `ignoreImmunity: false`), Struggle's typeless `???`, and
/// `move.ignoreImmunity[type]` (Scrappy, Mind's Eye: Normal and Fighting).
fn ignores_type_immunity(battle: &Battle, move_id: MoveId, data: &MoveData) -> bool {
    data.category == MoveCategory::Status && move_id != ids::THUNDER_WAVE
        || move_id == ids::STRUGGLE
        || battle.move_state.ignore_immunity_fighting_normal
            && matches!(data.ty, pikapro_data::Type::Normal | pikapro_data::Type::Fighting)
}

/// `getDamage`'s first step, `target.runImmunity(move, true)`: every hit
/// checks the target's types again, which a forme change between hits can
/// have reset (Disguise busting a Soaked Mimikyu). False is `getDamage`
/// returning `false`, after `-immune`.
fn damage_immunity(battle: &mut Battle, target: Position, move_id: MoveId, data: &MoveData) -> bool {
    ignores_type_immunity(battle, move_id, data) || events::run_immunity(battle, target, data.ty, true)
}

/// Showdown's `getDamage` for one target, in half points: fixed damage, the
/// move's `damageCallback` (Metal Burst's `damage * 1.5` isn't truncated:
/// a substitute takes all of it, a Pokémon's `spreadDamage` floors it), or
/// the damage formula. `None` is `undefined`.
#[expect(clippy::too_many_arguments, reason = "mirrors getDamage's inputs")]
fn get_damage(
    battle: &mut Battle,
    user: Position,
    target: Position,
    move_id: MoveId,
    data: &MoveData,
    spread: bool,
    typeless: bool,
    hit: u8,
) -> Option<u32> {
    // `damageCallback` (Counter, Metal Burst, Endeavor, ...), then fixed
    // damage.
    if let Some(halves) = multiturn::damage_callback(battle, user, move_id) {
        return Some(halves);
    }
    if let Some(d) = power::damage_callback(battle, user, target, move_id) {
        return Some(2 * d);
    }
    if let Some(d) = effects::fixed_damage(battle, user, target, move_id, data) {
        return Some(2 * d);
    }
    roll_damage(battle, user, target, move_id, data, spread, typeless, hit).map(|d| 2 * d)
}

/// Showdown's `runMoveEffects`: a move's primary effects on each target, in
/// Showdown's order. Returns the targets it did not fail on, and whether
/// some target's result was `NOT_FAIL` (Heal Pulse at full HP, Ally Switch,
/// Pollen Puff on a healthy ally), which drops the target but does not fail
/// the hit. `damaging` is whether the targets took damage (a number) this
/// hit.
fn run_move_effects(
    battle: &mut Battle,
    targets: &Targets,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
    damaging: bool,
) -> (Targets, bool) {
    let mut kept = Targets::new();
    let mut not_fail = false;
    let mut did_anything = Outcome::Undefined;
    // Whether the move still switches its user out (Parting Shot's `onHit`
    // can cancel it).
    let mut self_switch = data.self_switch.is_some();
    for &t in targets.iter() {
        // `didSomething`, combined over the effects that ran.
        let mut did = Outcome::Undefined;
        // Clangorous Soul's `onTryHit` deleted its boosts.
        if let Some(b) = data.boosts.filter(|_| !manipulation::skips_data_boosts(move_id))
            && battle.state.at(t).is_some_and(|m| !m.is_fainted())
        {
            // `boost` returns `null` when no stage changed.
            did = did.max(Outcome::from_null(boosts::boost(battle, t, b, Some(user), false, false)));
        }
        if data.heal.is_some() && battle.state.at(t).is_some() {
            // A failed heal logs `-fail|target|heal` and skips the rest.
            if !effects::heal_move(battle, t, data) {
                did_anything = did_anything.max(Outcome::Null);
                continue;
            }
            did = Outcome::Succeeded;
        }
        if let Some(s) = data.status {
            // A primary status that fails drops the target (`continue`).
            if !status::try_set_status(battle, t, s, Some(user), false) {
                did_anything = did_anything.max(Outcome::Null);
                continue;
            }
            did = Outcome::Succeeded;
        }
        // Curse's `onTryHit` deletes `volatileStatus` for a user that is not
        // a Ghost type by then (Protean can make it one after `ModifyMove`).
        let curse_fails = move_id == ids::CURSE
            && !battle.state.at(user).is_some_and(|m| manipulation::has_type(m, pikapro_data::Type::Ghost));
        // No Retreat's `onTry` deletes it for a trapped user, which only
        // gets the boosts.
        let no_retreat_trapped = move_id == ids::NO_RETREAT
            && battle.state.at(user).is_some_and(|m| m.volatiles.has(crate::state::Volatiles::TRAPPED));
        if let Some(v) = data.volatile_status.filter(|_| !curse_fails && !no_retreat_trapped) {
            if protect::handles(move_id) {
                protect::on_hit(battle, user, move_id);
                did = Outcome::Succeeded;
            } else {
                let added = volatiles::add_volatile(battle, t, v, Some(user), false);
                did = did.max(added.map_or(Outcome::Null, Outcome::from_bool));
            }
        }
        if let Some(c) = data.side_condition {
            did = did.max(Outcome::from_bool(field::add_side_condition(battle, t.side, c, user)));
        }
        if let Some(c) = data.slot_condition {
            did = did.max(Outcome::from_bool(field::add_slot_condition(battle, t, c, user)));
        }
        if let Some(w) = data.weather {
            did = did.max(field::set_weather(battle, w, user));
        }
        if let Some(w) = data.terrain {
            did = did.max(Outcome::from_bool(field::set_terrain(battle, w, user)));
        }
        if let Some(w) = data.pseudo_weather {
            did = did.max(Outcome::from_bool(field::add_pseudo_weather(battle, w, user)));
        }
        if data.force_switch {
            did = did.max(Outcome::from_bool(switching::force_switch_possible(battle, t)));
        }
        // The move's `onHit` (Parting Shot's can cancel the switch). It
        // returns nothing, which `singleEvent` turns into `true`. Teleport
        // has none (`if (moveData.onHit)`), so a `commanded` user fails.
        if move_id == ids::PARTING_SHOT {
            self_switch &= switching::on_hit(battle, t, user, move_id);
            did = Outcome::Succeeded;
        }
        // The move's `onHit`, then `runEvent('Hit')` (Focus Punch, Beak
        // Blast), then `selfdestruct: 'ifHit'` (Memento; Healing Wish, Lunar
        // Dance). Shell Side Arm's `onHit` hint.
        if move_id == ids::SHELL_SIDE_ARM && t.side != user.side {
            battle.hint(crate::event::Hint::ShellSideArm(data.category));
        }
        if let Some(result) = manipulation::on_hit(battle, t, user, move_id) {
            did = did.max(result);
        }
        if conditions::has_on_hit(move_id) {
            did = did.max(Outcome::from_bool(conditions::on_hit(battle, t, user, move_id)));
        }
        if let Some(result) = field::on_hit(battle, t, user, move_id) {
            did = did.max(Outcome::from_bool(result));
        }
        if order::handles_hit(move_id) {
            did = did.max(order::on_hit(battle, t, user, move_id));
        }
        if let Some(result) = itemmoves::on_hit(battle, t, user, move_id) {
            did = did.max(result);
        }
        multiturn::on_hit(battle, t, Some(user), data);
        // The abilities' and items' `Hit` (Anger Point, ...).
        events::move_event(battle, Event::Hit, t, Some(user), move_id, data, events::Relay::True);
        manipulation::self_destruct_if_hit(battle, user, data);
        slots::self_destruct_if_hit(battle, user, move_id);
        if self_switch {
            // Not while `commanded` (Commander).
            let commanded = battle.state.at(user).is_some_and(|m| m.volatiles.has(Volatiles::COMMANDED));
            let can = switching::can_switch(battle, user.side) > 0 && !commanded;
            did = if can { Outcome::Succeeded } else { did.max(Outcome::Failed) };
        }
        // `if (didSomething === undefined) didSomething = true`.
        if did == Outcome::Undefined {
            did = Outcome::Succeeded;
        }
        // Damage (a number) outranks any effect result.
        if damaging || did == Outcome::Succeeded {
            kept.push(t);
        }
        // Damage (a number) outranks `NOT_FAIL`.
        not_fail |= !damaging && did == Outcome::NotFail;
        did_anything = did_anything.max(did);
    }
    // A status move that did nothing fails loudly when some target failed
    // with `false` rather than `null`, unless it has `self` effects or
    // makes the user faint. Damage is a number, so a damaging move always
    // did something and switches its user out (U-turn).
    let did_nothing = !damaging && did_anything != Outcome::Succeeded;
    let has_self = data.self_boosts.is_some() || data.self_volatile.is_some() || order::has_self(move_id);
    if did_nothing && !has_self && !data.self_destruct {
        if did_anything == Outcome::Failed {
            fail_still(battle, user);
        }
    } else if self_switch {
        switching::self_switch(battle, user, move_id);
    }
    (kept, not_fail)
}

/// An effect's result in `runMoveEffects`, ordered as Showdown's
/// `combineResults` ranks them: `undefined` < `NOT_FAIL` (`""`) < `null` <
/// `false` < `true`, so combining results is `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Outcome {
    Undefined,
    /// Showdown's `NOT_FAIL`: failed, but silently and without failing the
    /// move (Heal Pulse at full HP, Ally Switch, Baton Pass, Pollen Puff on
    /// a healthy ally).
    NotFail,
    Null,
    Failed,
    Succeeded,
}

impl Outcome {
    pub(super) fn from_bool(ok: bool) -> Self {
        if ok { Outcome::Succeeded } else { Outcome::Failed }
    }

    /// For effects that return `null` rather than `false` on failure.
    fn from_null(ok: bool) -> Self {
        if ok { Outcome::Succeeded } else { Outcome::Null }
    }
}

/// Showdown's `getDamage` for one target, from the base power on, then
/// `modifyDamage`: `basePowerCallback`, `ModifyCritRatio`, the crit roll and
/// `CriticalHit`, `BasePower` (with the move's own `onBasePower`), the
/// attacking and defending stats (`ModifyBoost`, `Modify<Stat>`), then the
/// spread modifier, `WeatherModifyDamage`, the crit, the damage roll,
/// `ModifySTAB`, type effectiveness (`Effectiveness`), burn and
/// `ModifyDamage`. Every event's handlers (conditions, abilities, items)
/// sort together, which can draw. `None` is Showdown's `undefined`: no
/// damage at all (a base power of 0, Present's healing).
///
/// `pos` is the user's position, or its team position when it has left the
/// field (a Future Sight user): its handlers are then collected but do
/// nothing, as in Showdown.
#[expect(clippy::too_many_arguments, reason = "mirrors getDamage's inputs")]
pub(super) fn roll_damage(
    battle: &mut Battle,
    pos: Position,
    target: Position,
    move_id: MoveId,
    data: &MoveData,
    spread: bool,
    typeless: bool,
    hit: u8,
) -> Option<u32> {
    // The damage calculation's handlers add or remove no effects: its
    // events share one set of masks.
    events::with_cached_masks(battle, events::Target::Pokemon(target), Some(pos), |battle| {
        roll_damage_inner(battle, pos, target, move_id, data, spread, typeless, hit)
    })
}

#[expect(clippy::too_many_arguments, reason = "mirrors getDamage's inputs")]
fn roll_damage_inner(
    battle: &mut Battle,
    pos: Position,
    target: Position,
    move_id: MoveId,
    data: &MoveData,
    spread: bool,
    typeless: bool,
    hit: u8,
) -> Option<u32> {
    let effect = Effect::Move(move_id);
    // `basePowerCallback` (power.rs; a combined pledge's `-combine`; Spit Up
    // in manipulation.rs; then effects.rs), before the crit roll.
    let base_power = match power::base_power_callback(battle, pos, target, move_id, data, hit) {
        Some(bp) => bp,
        None => match pledges::base_power(battle, pos) {
            Some(bp) => bp as u32,
            None => {
                let attacker = &battle.state.side(pos.side).team[pos.slot as usize];
                match manipulation::base_power(attacker, move_id) {
                    Some(bp) => bp as u32,
                    None => {
                        // Rollout's and Round's `basePowerCallback`.
                        let bp = effects::base_power(attacker, move_id, data, hit);
                        let bp = multiturn::base_power(battle, pos, move_id, bp);
                        order::base_power_callback(battle, move_id, bp) as u32
                    }
                }
            }
        },
    };
    if base_power == 0 {
        return None;
    }
    // `runEvent('ModifyCritRatio', source, target, move, move.critRatio)`.
    let ratio = events::run_modify(
        battle,
        Event::ModifyCritRatio,
        pos,
        Some(target),
        effect,
        data.crit_ratio as u32,
        Some(data),
        None,
    );
    let crit_ratio = ratio.min(4) as usize;
    let mut crit = power::will_crit(move_id) || (crit_ratio > 0 && battle.rng.chance(1, CRIT_MULT[crit_ratio]));
    if crit {
        // `runEvent('CriticalHit', target, null, move)`: Battle Armor and
        // Shell Armor stop it.
        crit = events::run_check(battle, Event::CriticalHit, target, None, effect, Some(data)).is_truthy();
    }
    battle.move_state.hit_data_mut(target).crit = crit;
    // `runEvent('BasePower', source, target, move, basePower, true)`: the
    // move's own `onBasePower` sorts with the rest (Helping Hand 10, Charge
    // 9, the terrains 6, then priority 0). 0 deals 0 damage.
    let own = events::Spec::relay(power::own_base_power);
    let base_power =
        events::run_modify(battle, Event::BasePower, pos, Some(target), effect, base_power, Some(data), Some(own));
    if base_power == 0 {
        return Some(0);
    }
    // What the calculation reads of the attacker and the defender, read
    // here (not the whole Pokémon copied).
    let attacker = &battle.state.side(pos.side).team[pos.slot as usize];
    let defender = &battle.state.side(target.side).team[target.slot as usize];
    let move_ty = data.ty;
    let attacker_level = attacker.level;
    let defender_boosts = defender.boosts;
    let offender_boosts = if power::uses_target_attack(move_id) { defender.boosts } else { attacker.boosts };
    let attacker_has_type =
        manipulation::has_type(attacker, move_ty) || manipulation::pre_tera_has_type(attacker, move_ty);
    let tera_stab =
        attacker.terastallized && attacker.tera_type == move_ty && manipulation::pre_tera_has_type(attacker, move_ty);
    let attacker_burned = attacker.status == Status::Burn;
    // Terastallized Pokémon use weak moves of their Tera type at 60 BP or
    // more (Stellar: of a type whose boost is unused, even Struggle's
    // `???`): not priority or multi-hit moves (by their dex data), nor
    // variable-power moves whose base power is 0 or 150.
    let mut base_power = base_power.max(1);
    let dex_move = battle.format.dex.move_(move_id);
    let variable = (data.base_power == 0 || data.base_power == 150) && power::has_base_power_callback(dex_move);
    if super::tera::power_floor_type(attacker, (!typeless).then_some(move_ty))
        && base_power < 60
        && dex_move.priority <= 0
        && dex_move.multihit.is_none()
        && !variable
    {
        base_power = 60;
    }
    // `overrideOffensivePokemon` (Foul Play) uses the target's stat and stages.
    let offender_pos = if power::uses_target_attack(move_id) { target } else { pos };
    let (atk_stat, def_stat) = match data.category {
        MoveCategory::Physical => (Stat::Atk, Stat::Def),
        _ => (Stat::SpA, Stat::SpD),
    };
    // `overrideOffensiveStat` (Body Press), `overrideDefensiveStat` (Psyshock).
    // Under Wonder Room Body Press uses the Special Defense
    // (Wonder Room's `onModifyMove` flips `overrideOffensiveStat`).
    let offensive_stat = match power::offensive_stat(move_id) {
        Some(Stat::Def) if battle.state.field.wonder_room_turns > 0 => Stat::SpD,
        Some(stat) => stat,
        None => atk_stat,
    };
    let defensive_stat = power::defensive_stat(move_id).unwrap_or(def_stat);
    let mut atk_stage = offender_boosts.stat(offensive_stat);
    let mut def_stage = defender_boosts.stat(defensive_stat);
    if crit {
        atk_stage = atk_stage.max(0);
        def_stage = def_stage.min(0);
    }
    if power::ignores_defensive(move_id) {
        def_stage = 0;
    }
    // `calculateStat` (with `ModifyBoost` on the stat's user), then
    // `Modify<Stat>`: `ModifyAtk` / `ModifySpA` on the user (even for Foul
    // Play and Body Press), `Modify<defensive stat>` on the target.
    let attack = events::calculate_stat(battle, offender_pos, offensive_stat, atk_stage, pos);
    let defense = events::calculate_stat(battle, target, defensive_stat, def_stage, target);
    // (Each stat's event a constant at its call: most have no handler,
    // which `run_modify` then tells in a few instructions.)
    let attack = match atk_stat {
        Stat::Atk => events::run_modify(battle, Event::ModifyAtk, pos, Some(target), effect, attack, Some(data), None),
        _ => events::run_modify(battle, Event::ModifySpA, pos, Some(target), effect, attack, Some(data), None),
    };
    let defense = match defensive_stat {
        Stat::Def => events::run_modify(battle, Event::ModifyDef, target, Some(pos), effect, defense, Some(data), None),
        Stat::SpD => events::run_modify(battle, Event::ModifySpD, target, Some(pos), effect, defense, Some(data), None),
        stat => {
            let event = events::modify_stat_event(stat);
            events::run_modify(battle, event, target, Some(pos), effect, defense, Some(data), None)
        }
    };
    let level_factor = 2 * attacker_level as u32 / 5 + 2;
    let base_damage = level_factor * base_power * attack / defense.max(1) / 50;
    // `modifyDamage`.
    let mut d = base_damage + 2;
    if spread {
        d = modify(d, 3072);
    } else if battle.move_state.parental_bond && hit > 1 {
        // Parental Bond's second hit.
        d = modify(d, 1024);
    }
    // `priorityEvent('WeatherModifyDamage', source, target, move, damage)`.
    let mut ev = events::EventCtx::new(
        Event::WeatherModifyDamage,
        events::Target::Pokemon(pos),
        Some(target),
        effect,
        events::Relay::Num(d as i32),
    )
    .with_move(data);
    d = events::priority_event(battle, &mut ev).num().unwrap_or(0).max(0) as u32;
    if crit {
        // Showdown truncates here rather than rounding.
        d = d * 3 / 2;
    }
    d = d * (100 - battle.rng.random(16)) / 100;
    // STAB. Types changed by moves (Soak, Forest's Curse, Roost) count.
    // `if (type !== '???')`: neither Struggle nor a typeless Revelation
    // Dance (after Burn Up or Double Shock) gets STAB or a Stellar boost.
    if !typeless && move_ty != Type::Unknown {
        let is_stab = pledges::force_stab(battle, pos) || attacker_has_type;
        // Terastallized into Stellar, the Stellar boost replaces STAB
        // and `ModifySTAB`.
        let stab = match super::tera::stellar_stab(battle, pos, move_ty, is_stab) {
            Some(stab) => stab,
            None => {
                let stab = if tera_stab {
                    8192
                } else if is_stab {
                    6144
                } else {
                    4096
                };
                // `runEvent('ModifySTAB', pokemon, target, move, stab)`, in 4096ths.
                events::run_modify(battle, Event::ModifySTAB, pos, Some(target), effect, stab, Some(data), None)
            }
        };
        d = modify(d, stab);
    }
    // `target.runEffectiveness(move)`, with the `Effectiveness` event per type.
    let type_mod = if typeless { 0 } else { power::run_effectiveness(battle, target, move_id, data).clamp(-6, 6) };
    battle.move_state.hit_data_mut(target).type_mod = type_mod;
    let target_actor = super::log_actor(battle, target);
    if type_mod > 0 {
        battle.emit_for(target_actor, |target| BattleEvent::SuperEffective { target, level: type_mod.min(2) as u8 });
        d <<= type_mod;
    }
    if type_mod < 0 {
        battle.emit_for(target_actor, |target| BattleEvent::Resisted { target, level: (-type_mod).min(2) as u8 });
        d >>= -type_mod;
    }
    if crit {
        battle.emit_for(target_actor, |target| BattleEvent::Crit { target });
    }
    // Burn halves physical damage, except with Guts or (Gen 6+) Facade.
    let burned = attacker_burned
        && data.category == MoveCategory::Physical
        && !abilities::has_ability(battle, pos, pikapro_data::gen9::abilities::GUTS)
        && !power::ignores_burn(move_id);
    if burned {
        d = modify(d, 2048);
    }
    // `runEvent('ModifyDamage', pokemon, target, move, damage)`: the
    // attacker's (Life Orb), the defender's `onSourceModifyDamage`
    // (Multiscale, Minimize, Glaive Rush, Fly and co.), every screen on
    // either side (`onAnyModifyDamage`), ...
    d = events::run_modify(battle, Event::ModifyDamage, pos, Some(target), effect, d, Some(data), None);
    // `bypassProtect` (Piercing Drill, the Champions mod's Unseen Fist): a
    // quarter of the damage through protection.
    if battle.move_state.bypass_protect & 1 << (target.side.index() * 2 + (target.slot as usize & 1)) != 0 {
        d = modify(d, 1024);
        if let Some(ability) = battle.move_state.bypass_ability {
            let actor = actor(battle, pos);
            let note = crate::event::AbilityNote::None;
            battle.emit(BattleEvent::AbilityShow { actor, ability, note, cause: None });
        }
        battle.emit_for(target_actor, |target| BattleEvent::ZBroken { target });
    }
    // `tr(baseDamage, 16)`, which custom games skip (`Math.trunc`).
    Some(if d == 0 {
        1
    } else if battle.format.cartridge_overflow {
        d % 65536
    } else {
        d
    })
}

/// `-fail` plus `[still]`, which clears the target from the `|move|` line.
pub(super) fn fail_still(battle: &mut Battle, user: Position) {
    battle.flush_rng_trace();
    battle.attr_last_move(crate::event::MoveAttr::Still);
    battle.emit(BattleEvent::Fail { user: actor(battle, user) });
}

/// `useMoveInner` after a `ModifyMove` handler returned `false` (Heal
/// Block, Throat Chop, Gravity): `move.target` of `false` differs from
/// `baseTarget`, so it calls `getRandomTarget(pokemon, false)`, whose
/// target-less move takes the `randomFoe()` path (a draw in doubles), then
/// `if (!move) return false`.
fn modify_move_failed(battle: &mut Battle, user: Position) -> bool {
    targeting::get_random_target(battle, user, MoveTarget::Normal);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BattleOptions, Format, PokemonSet, Seed, SideId, Team};
    use pikapro_data::gen9::{abilities as a, species as s};

    #[test]
    fn exhausted_queued_move_records_failure_without_replacing_last_move() {
        for previous in [MoveResult::Undefined, MoveResult::Succeeded] {
            let set = || PokemonSet::new(s::MEW).with_ability(a::SYNCHRONIZE).with_moves(&[ids::RECOVER]);
            let teams = [Team::new(vec![set()]), Team::new(vec![set()])];
            let mut b = Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(48_000_000))).unwrap();
            let user = Position::new(SideId::P1, 0);
            let mon = b.state.at_mut(user).unwrap();
            mon.moves[0].pp = 0;
            mon.last_move = Some(ids::SPLASH);
            mon.volatile_counters.move_this_turn = previous;
            run_move(&mut b, user, ids::RECOVER, 0, Default::default());
            assert_eq!(b.state.at(user).unwrap().volatile_counters.move_this_turn, MoveResult::Failed);
            assert_eq!(b.state.at(user).unwrap().last_move, Some(ids::SPLASH));
            assert!(b.active_move.is_none());
        }
    }

    #[test]
    fn reflected_move_type_reaches_after_move_through_nested_calls() {
        for caller in [ids::GROWL, ids::COPYCAT] {
            let set =
                |species, ability, moves: &[MoveId]| PokemonSet::new(species).with_ability(ability).with_moves(moves);
            let teams = [
                Team::new(vec![
                    set(s::MEW, a::SYNCHRONIZE, &[ids::CHARGE, ids::GROWL, ids::COPYCAT]),
                    set(s::BLISSEY, a::SERENE_GRACE, &[ids::SPLASH]),
                ]),
                Team::new(vec![
                    set(s::ESPEON, a::MAGIC_BOUNCE, &[ids::SPLASH]),
                    set(s::REGIELEKI, a::TRANSISTOR, &[ids::ELECTRIFY]),
                ]),
            ];
            let mut b = Battle::new(Format::gen9_doubles(), teams, BattleOptions::new(Seed::fast(48_000_000))).unwrap();
            let user = Position::new(SideId::P1, 0);
            let bouncer = Position::new(SideId::P2, 0);
            run_move(&mut b, user, ids::CHARGE, 0, Default::default());
            assert!(b.state.at(user).unwrap().volatiles.has(Volatiles::CHARGE));
            b.state.at_mut(bouncer).unwrap().volatiles.add(Volatiles::ELECTRIFY);
            b.order.last_move = Some(ids::GROWL);
            run_move(&mut b, user, caller, 0, Default::default());
            // BattleActions.runMove takes battle.activeMove after all
            // nested useMove calls, including a same-ID reflection.
            assert!(!b.state.at(user).unwrap().volatiles.has(Volatiles::CHARGE), "{caller:?}");
            assert_eq!(b.state.at(user).unwrap().last_move, Some(caller));
            assert_eq!(
                b.state.at(bouncer).unwrap().volatile_counters.last_move_used,
                Some((ids::GROWL, Type::Electric))
            );
        }
    }

    #[test]
    fn reflected_active_move_controls_suppression_on_remaining_targets() {
        use pikapro_data::gen9::items as i;

        for caller in [ids::GROWL, ids::COPYCAT] {
            for shield in [false, true] {
                let set = |species, ability, moves: &[MoveId]| {
                    PokemonSet::new(species).with_ability(ability).with_moves(moves)
                };
                let mut bouncer = set(s::ESPEON, a::MAGIC_BOUNCE, &[ids::SPLASH]);
                if shield {
                    bouncer = bouncer.with_item(i::ABILITY_SHIELD);
                }
                let teams = [
                    Team::new(vec![
                        set(s::MEW, a::MOLD_BREAKER, &[caller]),
                        set(s::BLISSEY, a::NO_ABILITY, &[ids::SPLASH]),
                    ]),
                    Team::new(vec![bouncer, set(s::METAGROSS, a::CLEAR_BODY, &[ids::SPLASH])]),
                ];
                let mut b =
                    Battle::new(Format::gen9_doubles(), teams, BattleOptions::new(Seed::fast(51_000_000))).unwrap();
                b.order.last_move = Some(ids::GROWL);
                run_move(&mut b, Position::new(SideId::P1, 0), caller, 0, Default::default());
                // Ability Shield lets Magic Bounce run despite Mold
                // Breaker. Its new ActiveMove does not ignore abilities,
                // including Clear Body on the original move's next target.
                assert_eq!(
                    b.state.at(Position::new(SideId::P2, 1)).unwrap().boosts.atk,
                    if shield { 0 } else { -1 },
                    "{caller:?}, shield={shield}"
                );
            }
        }
    }

    #[test]
    fn last_move_waits_for_the_entire_dancer_chain() {
        let set = |species| PokemonSet::new(species).with_ability(a::DANCER).with_moves(&[ids::QUIVER_DANCE]);
        for last_asleep in [false, true] {
            let teams = [
                Team::new(vec![set(s::ORICORIO), set(s::ORICORIO_SENSU)]),
                Team::new(vec![set(s::ORICORIO_POM_POM), set(s::ORICORIO_PAU)]),
            ];
            let mut b = Battle::new(Format::gen9_doubles(), teams, BattleOptions::new(Seed::fast(47_000_001))).unwrap();
            let last = Position::new(SideId::P2, 1);
            b.state.at_mut(last).unwrap().stats[Stat::Spe] = 500;
            if last_asleep {
                b.state.at_mut(last).unwrap().status = Status::Sleep { turns: 3 };
            }
            b.order.last_move = Some(ids::FEATHER_DANCE);
            run_move(&mut b, Position::new(SideId::P1, 0), ids::QUIVER_DANCE, 0, Default::default());
            // Showdown Battle#clearActiveMove runs once in runAction. If
            // the final external runMove aborts, clearActiveMove(true)
            // leaves the previously recorded move for a later Copycat.
            assert_eq!(b.order.last_move, Some(if last_asleep { ids::FEATHER_DANCE } else { ids::QUIVER_DANCE }));
        }
    }
}
