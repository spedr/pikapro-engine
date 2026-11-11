//! Multi-turn moves.
//!
//! Showdown references (`data/moves.ts`, `data/conditions.ts`,
//! `sim/battle-actions.ts`, `sim/battle-queue.ts`, `sim/pokemon.ts`):
//!
//! - Two-turn moves: the moves' `onTryMove` and the `twoturnmove`
//!   condition (Fly, Dig, Dive, Bounce, Phantom Force, Shadow Force, Sky
//!   Attack, Solar Beam, Solar Blade, Meteor Beam, Electro Shot, Freeze
//!   Shock, Ice Burn); the semi-invulnerable moves' conditions
//!   (`onInvulnerability`, `onSourceModifyDamage`, `onSourceBasePower`,
//!   `onImmunity`) and `hitStepInvulnerabilityEvent`.
//! - Locked moves: `lockedmove` (Outrage, Petal Dance, Thrash, Raging Fury),
//!   Uproar and Rollout; `Pokemon.getLockedMove`, `Side.chooseMove` and
//!   `Pokemon.getMoveRequestData` for what a locked Pokémon may choose.
//! - Start-of-turn callbacks: `priorityChargeCallback` (Focus Punch, Beak
//!   Blast, Chilly Reception) and `beforeTurnCallback` (Counter, Mirror
//!   Coat), which `BattleQueue.resolveAction` turns into their own actions.
//! - Future Sight and Doom Desire: the `futuremove` slot condition.
//! - Damage taken: `Pokemon.gotAttacked` / `getLastDamagedBy` (Metal Burst,
//!   Comeuppance) and the `counter` / `mirrorcoat` conditions.
//!
//! State: presence bits in [`Volatiles`] (bits 8..=15), their data in
//! [`VolatileCounters`](crate::state::VolatileCounters), the last foe damage
//! in [`MoveRecord::damaged_by`](crate::state::MoveRecord), pending future
//! attacks in [`SideState::future_moves`](crate::state::SideState).
//!
//! Items and abilities join through the event system: Power Herb's
//! `ChargeMove`, No Guard's `Invulnerability`, the user's
//! `effectiveWeather` (Utility Umbrella, Air Lock), ...

use pikapro_data::gen9::moves as ids;
use pikapro_data::{MoveCategory, MoveData, MoveFlags, MoveId, StatBoosts, StatusId, Type};

use super::events::{EventCtx, Relay};
use super::handlers::{Callback, Handler, Handlers, Holder, sub_order, vol};
use super::queue::{Action, ActionKind, MonRef};
use super::targeting::{self, Loc};
use super::{
    DamageSource, actor, apply_damage, boosts, check_win, each_update, faint_messages, position_of_mon, status,
    volatiles,
};
use crate::battle::Battle;
use crate::event::{Actor, BattleEvent};
use crate::state::{
    DamagedBy, FutureMove, MAX_ACTIVE, PokemonState, Position, SideId, SlotCondition, Status, Volatiles, Weather,
};

/// Stands for the `lockedmove` condition as the effect adding confusion
/// when a lock-in ends (put in `battle.active_move` for the call), which
/// Showdown announces with `[fatigue]`.
pub(crate) const LOCKED_MOVE_EFFECT: MoveId = MoveId(u16::MAX - 1);

/// Status moves this module implements beyond their data.
pub(crate) fn handles(move_id: MoveId) -> bool {
    move_id == ids::CHILLY_RECEPTION
}

fn mon(battle: &Battle, pos: Position) -> Option<&PokemonState> {
    battle.state.at(pos)
}

fn mon_mut(battle: &mut Battle, pos: Position) -> &mut PokemonState {
    battle.state.at_mut(pos).expect("volatile holder is active")
}

/// `attrLastMove('[still]')`: the `|move|` line loses its target.
fn still(battle: &mut Battle) {
    battle.flush_rng_trace();
    battle.attr_last_move(crate::event::MoveAttr::Still);
}

/// `addMove('-anim', attacker, move.name, defender)`.
pub(crate) fn anim(battle: &mut Battle, user: Position, move_id: MoveId, defender: Position) {
    if battle.log.is_enabled() {
        let (user, target) = (actor(battle, user), actor(battle, defender));
        battle.emit(BattleEvent::Anim { user, move_id, target });
    }
}

// ---------------------------------------------------------------------
// Two-turn moves.

/// Moves with a charging turn (`onTryMove` adding `twoturnmove`).
fn is_two_turn(move_id: MoveId) -> bool {
    matches!(
        move_id,
        ids::FLY
            | ids::DIG
            | ids::DIVE
            | ids::BOUNCE
            | ids::PHANTOM_FORCE
            | ids::SHADOW_FORCE
            | ids::SKY_ATTACK
            | ids::SOLAR_BEAM
            | ids::SOLAR_BLADE
            | ids::METEOR_BEAM
            | ids::ELECTRO_SHOT
            | ids::FREEZE_SHOCK
            | ids::ICE_BURN
    )
}

/// Two-turn moves whose own volatile makes the user semi-invulnerable (the
/// moves with a `condition`, which lasts 2 turns).
fn semi_invulnerable_move(move_id: MoveId) -> bool {
    matches!(move_id, ids::FLY | ids::DIG | ids::DIVE | ids::BOUNCE | ids::PHANTOM_FORCE | ids::SHADOW_FORCE)
}

/// The move whose own volatile the Pokémon at `pos` has while charging.
fn charging(battle: &Battle, pos: Position) -> Option<MoveId> {
    mon(battle, pos).filter(|m| m.volatiles.has(Volatiles::CHARGING)).map(|m| m.volatile_counters.charge_move)
}

/// Showdown's `pokemon.isSemiInvulnerable()`.
pub(crate) fn is_semi_invulnerable(battle: &Battle, pos: Position) -> bool {
    charging(battle, pos).is_some_and(semi_invulnerable_move)
}

/// The move's `onTryMove` (`singleEvent('TryMove')`, after the `|move|`
/// line). A two-turn move whose volatile is up attacks; otherwise it
/// announces the charge (`-prepare`) and, unless the weather lets it skip
/// the charge, starts `twoturnmove` and stops. Returns whether the move
/// goes on.
pub(crate) fn try_move(
    battle: &mut Battle,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
    defender: Position,
) -> bool {
    if !is_two_turn(move_id) {
        return true;
    }
    // `attacker.removeVolatile(move.id)`: the second turn.
    if charging(battle, user) == Some(move_id) {
        mon_mut(battle, user).volatiles.clear(Volatiles::CHARGING);
        return true;
    }
    // Dive's Gulp Missile forme change.
    if move_id == ids::DIVE {
        super::abilities::switchin::gulp_missile_dive(battle, user);
    }
    let a = actor(battle, user);
    battle.emit(BattleEvent::Prepare { user: a, move_id });
    // The user's `effectiveWeather()` (Air Lock, Utility Umbrella;
    // Solar Beam's and Solar Blade's `effectiveWeather(undefined, true)`
    // is Mega Sol's sun, which it announces).
    let weather = if matches!(move_id, ids::SOLAR_BEAM | ids::SOLAR_BLADE) {
        super::mega::pokemon_weather_shown(battle, user, move_id)
    } else {
        super::mega::pokemon_weather(battle, user, super::mega::WeatherAsker::Move(move_id))
    };
    if matches!(move_id, ids::SOLAR_BEAM | ids::SOLAR_BLADE)
        && matches!(weather, Some(Weather::Sun | Weather::HarshSun))
    {
        // `attrLastMove('[still]')`, then the attack is shown by `-anim`.
        still(battle);
        anim(battle, user, move_id, defender);
        return true;
    }
    if matches!(move_id, ids::METEOR_BEAM | ids::ELECTRO_SHOT) {
        boosts::boost(battle, user, StatBoosts::new([0, 0, 1, 0, 0, 0, 0]), Some(user), false, false);
    }
    if move_id == ids::ELECTRO_SHOT && matches!(weather, Some(Weather::Rain | Weather::HeavyRain)) {
        still(battle);
        anim(battle, user, move_id, defender);
        return true;
    }
    // `runEvent('ChargeMove')`: Power Herb skips the charging turn.
    let relay = super::events::Relay::True;
    if !super::events::move_event(battle, pikapro_data::Event::ChargeMove, user, Some(defender), move_id, data, relay)
        .is_truthy()
    {
        return true;
    }
    add_two_turn_move(battle, user, move_id, data, defender);
    // Every charging `onTryMove` returns `null`, not `false`. If the
    // charge is interrupted (Gravity, Smack Down, switching), the next
    // turn's Stomping Tantrum / Temper Flare must not double its power.
    super::power::set_move_result(battle, user, crate::state::MoveResult::Null);
    false
}

/// `addVolatile('twoturnmove', defender)`: its `onStart` adds the move's own
/// volatile, remembers where the move is aimed and clears the `|move|`
/// line's target.
fn add_two_turn_move(battle: &mut Battle, user: Position, move_id: MoveId, data: &MoveData, defender: Position) {
    // No `onRestart`: nothing happens if it is already up.
    if mon_mut(battle, user).volatiles.has(Volatiles::TWO_TURN_MOVE) {
        return;
    }
    // `TryAddVolatile` for `twoturnmove`, then (its `onStart`) for the
    // move's own volatile: nothing stops them, but ties draw.
    super::events::try_add_volatile(battle, user, "twoturnmove");
    super::events::try_add_volatile(battle, user, "charging");
    // `lastMoveTargetLoc` is not updated for a move with a `sourceEffect`
    // (called by another, used while locked into another move, or the
    // action a waiting pledge moved up, which Encore changed): that one is
    // aimed at `defender`,
    // or, if it has fainted (a Copycat after the foe fainted), at
    // `sample(attacker.foes(true))`: every foe in a slot, fainted or not.
    let called = (battle.order.source_effect.is_some()
        || super::order::is_locked(battle, user)
        || super::pledges::source(battle, user).is_some())
        && battle.format.dex.move_(move_id).target != pikapro_data::MoveTarget::User;
    let called_loc = called.then(|| {
        let mut defender = defender;
        if battle.state.at(defender).is_none() {
            let foe = user.side.foe();
            let filled = battle.format.active_per_side().min(battle.state.side(foe).team.len());
            let foes: crate::util::StackVec<Position, MAX_ACTIVE> =
                (0..filled as u8).map(|slot| Position::new(foe, slot)).collect();
            defender = *battle.rng.sample(&foes);
        }
        super::targeting::loc_of(user, defender)
    });
    let m = mon_mut(battle, user);
    m.volatiles.add(Volatiles::TWO_TURN_MOVE);
    let c = &mut m.volatile_counters;
    c.two_turn = 2;
    c.charge_move = move_id;
    // The move's own volatile.
    c.charging = if semi_invulnerable_move(move_id) { 2 } else { 0 };
    c.charge_target_loc = called_loc.unwrap_or(m.last_move_target_loc);
    m.volatiles.add(Volatiles::CHARGING);
    still(battle);
    // `runEvent('PrepareHit', attacker, defender, effect)`, `effect` being
    // the active move, with its modified type (Protean, Libero: Electrify
    // makes a charging Meteor Beam Electric).
    let relay = super::events::Relay::True;
    super::events::move_event(battle, pikapro_data::Event::PrepareHit, user, Some(defender), move_id, data, relay);
}

/// `removeVolatile('twoturnmove')`, whose `onEnd` removes the move's own
/// volatile too. Nothing happens to a Pokémon at 0 HP.
fn remove_two_turn_move(battle: &mut Battle, pos: Position) {
    let Some(m) = battle.state.at_mut(pos) else { return };
    if m.is_fainted() || !m.volatiles.has(Volatiles::TWO_TURN_MOVE) {
        return;
    }
    m.volatiles.clear(Volatiles::TWO_TURN_MOVE | Volatiles::CHARGING);
}

/// Smack Down knocking a Fly or Bounce user down:
/// `removeVolatile('fly' | 'bounce')` (no message), `queue.cancelMove`,
/// `removeVolatile('twoturnmove')`. Returns whether it was in the air.
pub(crate) fn knock_down(battle: &mut Battle, pos: Position) -> bool {
    if !matches!(charging(battle, pos), Some(ids::FLY | ids::BOUNCE)) {
        return false;
    }
    let id = mon(battle, pos).expect("checked above").id;
    let user = MonRef { side: pos.side, id };
    battle.queue.retain(|a| !(a.mon == Some(user) && matches!(a.kind, super::queue::ActionKind::Move { .. })));
    remove_two_turn_move(battle, pos);
    true
}

/// `runEvent('MoveAborted')`: a Pokémon that could not move stops charging
/// (`twoturnmove`'s `onMoveAborted`).
pub(crate) fn move_aborted(battle: &mut Battle, user: Position) {
    remove_two_turn_move(battle, user);
}

/// `hitStepInvulnerabilityEvent` for one target: a semi-invulnerable
/// target is missed (`-miss`) except by the moves that reach it. Returns
/// whether the target can be hit. `miss_attr`: a shown miss also adds
/// `[miss]` to the move line (`trySpreadMoveHit` without `spreadHit`;
/// not the field moves' `onHitField`, Perish Song and Teatime).
pub(crate) fn invulnerability(
    battle: &mut Battle,
    target: Position,
    user: Position,
    move_id: MoveId,
    miss_attr: bool,
) -> bool {
    invulnerability_from(battle, target, user, None, move_id, miss_attr, false)
}

/// The field moves' `runEvent('Invulnerability', pokemon, source, move) ===
/// false` (Perish Song's and Teatime's `onHitField`), with `-miss` but no
/// `[miss]`. Unlike `hitStepInvulnerabilityEvent`, the event runs for a
/// commanding Tatsugiri too: its `commanding` volatile's `onInvulnerability:
/// false` (priority 0, `data/conditions.js`: "for moves that manually call
/// this event like Perish Song") comes after No Guard's
/// `onAnyInvulnerability` and Lock-On's `onSourceInvulnerability` (priority
/// 1), whose `0` ends the event: the move reaches it.
pub(crate) fn field_invulnerability(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    invulnerability_from(battle, target, user, None, move_id, false, true)
}

/// [`invulnerability`] (or, with `field`, [`field_invulnerability`]) for a
/// user that may be off the field (a future attack's: `user` is then its
/// team position), named `user_actor`.
fn invulnerability_from(
    battle: &mut Battle,
    target: Position,
    user: Position,
    user_actor: Option<Actor>,
    move_id: MoveId,
    miss_attr: bool,
    field: bool,
) -> bool {
    if move_id == ids::HELPING_HAND {
        return true;
    }
    // Commander's `commanding`: never hit. `hitStepInvulnerabilityEvent`
    // misses it without running the event (so neither No Guard nor Lock-On
    // reaches it); the field moves run it (see `field_invulnerability`).
    let commanding = mon(battle, target).is_some_and(|m| m.volatiles.has(Volatiles::COMMANDING));
    let skip_event = commanding && !field;
    // Gen 8+: Toxic from a Poison type always hits.
    if !commanding
        && move_id == ids::TOXIC
        && mon(battle, user).is_some_and(|m| super::manipulation::has_type(m, Type::Poison))
    {
        return true;
    }
    // `runEvent('Invulnerability')` of abilities (No Guard's
    // `onAnyInvulnerability`, priority 1, returns 0: hit). Lock-On's
    // `onSourceInvulnerability` (priority 1, 0 too) and the charging moves'
    // `onInvulnerability` (priority 0, `false`) follow. Showdown sorts them
    // in one list, left to right without drawing, by priority: the same
    // order, and the two priority-1 handlers give the same result.
    if !skip_event {
        // The dex's data (`&'static`: no copy). For a commanding target the
        // event ends on its volatile's `false`.
        let data = battle.format.dex.move_(move_id);
        let relay = super::events::move_event(
            battle,
            pikapro_data::Event::Invulnerability,
            target,
            Some(user),
            move_id,
            data,
            super::events::Relay::True,
        );
        if relay != super::events::Relay::True && relay != super::events::Relay::False {
            return true;
        }
    }
    // Lock-On's `onSourceInvulnerability` (priority 1):
    // `0`, which reaches the locked target.
    let locked = battle.state.at(target).map(|t| (target.side, t.id));
    if !skip_event
        && mon(battle, user).is_some_and(|m| {
            m.volatiles.has(Volatiles::LOCK_ON)
                && m.volatile_counters.lock_on.is_some()
                && m.volatile_counters.lock_on == locked
        })
    {
        return true;
    }
    let charge = if commanding { Some(ids::FLY) } else { charging(battle, target) };
    let Some(charge) = charge.filter(|&m| commanding || semi_invulnerable_move(m)) else { return true };
    let reaches: &[MoveId] = match charge {
        ids::FLY | ids::BOUNCE => &[
            ids::GUST,
            ids::TWISTER,
            ids::SKY_UPPERCUT,
            ids::THUNDER,
            ids::HURRICANE,
            ids::SMACK_DOWN,
            ids::THOUSAND_ARROWS,
        ],
        ids::DIG => &[ids::EARTHQUAKE, ids::MAGNITUDE],
        ids::DIVE => &[ids::SURF, ids::WHIRLPOOL],
        _ => &[],
    };
    if !commanding && reaches.contains(&move_id) {
        return true;
    }
    // A `smartTarget` move (Dragon Darts) misses its first
    // target silently and turns `smartTarget` off, so a second invulnerable
    // target's miss is shown (`hitStepInvulnerabilityEvent`).
    if !super::order::smart_target_missed(battle) {
        let user_actor = user_actor.unwrap_or_else(|| actor(battle, user));
        let target_actor = actor(battle, target);
        if miss_attr {
            battle.attr_last_move(crate::event::MoveAttr::Miss);
        }
        battle.emit(BattleEvent::Miss { user: user_actor, target: target_actor });
    }
    false
}

/// Solar Beam's and Solar Blade's own `onBasePower`: halved in rain,
/// sand and snow (the user's `effectiveWeather`; Mega Sol's sun counts).
/// A 4096-based modifier.
pub(crate) fn own_base_power(battle: &Battle, user: Position, move_id: MoveId) -> u32 {
    match move_id {
        ids::SOLAR_BEAM | ids::SOLAR_BLADE
            if matches!(
                super::mega::pokemon_weather(battle, user, super::mega::WeatherAsker::Move(move_id)),
                Some(Weather::Rain | Weather::HeavyRain | Weather::Sand | Weather::Snow)
            ) =>
        {
            2048
        }
        _ => 4096,
    }
}

/// Bounce's `onSourceBasePower`: Gust and Twister double against it.
pub(crate) fn bounce_source_base_power(
    _: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
    _: &Handler,
) -> Option<super::events::Relay> {
    if matches!(ev.effect.move_id(), Some(ids::GUST | ids::TWISTER)) {
        return ev.chain_modify(2, 1);
    }
    None
}

/// Whether the charging move's volatile has `onSourceModifyDamage` (Fly,
/// Dig, Dive).
pub(crate) fn has_source_modify_damage(charge_move: MoveId) -> bool {
    matches!(charge_move, ids::FLY | ids::DIG | ids::DIVE)
}

/// Fly's, Dig's and Dive's `onSourceModifyDamage`: the moves that reach
/// the Pokémon deal double damage.
pub(crate) fn semi_invulnerable_source_damage(
    battle: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
    h: &Handler,
) -> Option<super::events::Relay> {
    let Holder::Pokemon(pos) = h.holder else { return None };
    let charge = battle.state.side(pos.side).team[pos.slot as usize].volatile_counters.charge_move;
    match (charge, ev.effect.move_id()?) {
        (ids::FLY, ids::GUST | ids::TWISTER)
        | (ids::DIG, ids::EARTHQUAKE | ids::MAGNITUDE)
        | (ids::DIVE, ids::SURF | ids::WHIRLPOOL) => ev.chain_modify(2, 1),
        _ => None,
    }
}

/// Dig's and Dive's `onImmunity`: no sandstorm or hail damage underground
/// or underwater.
pub(crate) fn dig_immunity(
    _: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
    _: &Handler,
) -> Option<super::events::Relay> {
    matches!(ev.relay, super::events::Relay::Id("sandstorm" | "hail")).then_some(super::events::Relay::False)
}

// ---------------------------------------------------------------------
// Locked moves.

/// Showdown's `getLockedMove`: the move one of the Pokémon's volatiles
/// locks it into (`onLockMove`). `priorityEvent` sorts one Pokémon's
/// handlers without changing their order, so with two (a two-turn move
/// charged while Instruct's Raging Fury locks it) the volatile added first
/// answers.
fn locked_move_of(m: &PokemonState) -> Option<MoveId> {
    let (v, c) = (m.volatiles, &m.volatile_counters);
    if !v.has(Volatiles::LOCKING) {
        return None;
    }
    v.in_order().find_map(|flag| match flag {
        Volatiles::TWO_TURN_MOVE => Some(c.charge_move),
        Volatiles::LOCKED_MOVE => Some(c.locked_move_id),
        Volatiles::UPROAR => Some(ids::UPROAR),
        Volatiles::ROLLOUT => Some(ids::ROLLOUT),
        _ => None,
    })
}

/// The move the Pokémon at `pos` is locked into, if any.
pub(crate) fn locked_move(battle: &Battle, pos: Position) -> Option<MoveId> {
    mon(battle, pos).and_then(locked_move_of)
}

/// `chooseMove` for a locked Pokémon: the move slot, the locked move and
/// its target (`volatiles[move].targetLoc || lastMoveTargetLoc`).
pub(crate) fn locked_action(battle: &Battle, side: SideId, slot: usize) -> Option<(u8, MoveId, Loc)> {
    let m = battle.state.side(side).active(slot)?;
    let move_id = locked_move_of(m)?;
    let c = &m.volatile_counters;
    let charged = m.volatiles.has(Volatiles::CHARGING) && c.charge_move == move_id;
    let target_loc = if charged && c.charge_target_loc != 0 { c.charge_target_loc } else { m.last_move_target_loc };
    let index = m.moves.iter().position(|s| s.id == move_id).unwrap_or(0) as u8;
    Some((index, move_id, target_loc))
}

/// `addVolatile` for the volatiles moves apply by ID (`self.volatileStatus`):
/// `lockedmove` and `uproar`. `None` for any other ID. The caller has
/// checked that the Pokémon is active and has HP.
pub(crate) fn add_volatile(battle: &mut Battle, target: Position, id: &str) -> Option<bool> {
    let flag = match id {
        "lockedmove" => Volatiles::LOCKED_MOVE,
        "uproar" => Volatiles::UPROAR,
        _ => return None,
    };
    let m = mon_mut(battle, target);
    if m.volatiles.has(flag) {
        // `lockedmove`'s `onRestart`: another turn while the true duration
        // lasts. Uproar has no `onRestart`.
        if flag == Volatiles::LOCKED_MOVE {
            let c = &mut m.volatile_counters;
            if c.locked_true_duration >= 2 {
                c.locked_move = 2;
            }
            return Some(true);
        }
        return Some(false);
    }
    // `Pokemon.addVolatile`'s `runStatusImmunity(id)` and
    // `runEvent('TryAddVolatile')` (whose tied handlers draw) ran once
    // already, in the caller `volatiles::add_volatile_from`, with the real
    // source and effect: they don't run again here.
    m.volatiles.add(flag);
    if flag == Volatiles::LOCKED_MOVE {
        // `onStart`: the true duration is drawn; the move is the one
        // being used (`effect.id`).
        let true_duration = battle.rng.random_range(2, 4) as u8;
        let move_id = battle.active_move.map_or(ids::OUTRAGE, |(_, m)| m);
        let c = &mut mon_mut(battle, target).volatile_counters;
        c.locked_move = 2;
        c.locked_true_duration = true_duration;
        c.locked_move_id = move_id;
    } else {
        mon_mut(battle, target).volatile_counters.uproar = 3;
        let a = actor(battle, target);
        battle.emit(BattleEvent::UproarStart { actor: a, upkeep: false });
    }
    Some(true)
}

/// `removeVolatile('lockedmove')` and its `onEnd`: a lock-in that ran its
/// course confuses the Pokémon (`[fatigue]`).
fn remove_locked_move(battle: &mut Battle, pos: Position) {
    let Some(m) = battle.state.at_mut(pos) else { return };
    if m.is_fainted() || !m.volatiles.has(Volatiles::LOCKED_MOVE) {
        return;
    }
    m.volatiles.clear(Volatiles::LOCKED_MOVE);
    if m.volatile_counters.locked_true_duration > 1 {
        return;
    }
    // The effect adding confusion is the condition, not a move: Misty
    // Terrain stops it silently, and it is announced with `[fatigue]`.
    let saved = battle.active_move;
    battle.active_move = Some((pos, LOCKED_MOVE_EFFECT));
    volatiles::add_volatile(battle, pos, "confusion", None, true);
    battle.active_move = saved;
}

/// The protections' `onTryHit` (Protect and its variants, Wide Guard,
/// Quick Guard): a `lockedmove` started this turn (`duration === 2`; a
/// Dancer's Petal Dance) is deleted, without its `onEnd`.
pub(crate) fn protected_locked_move(battle: &mut Battle, source: Position) {
    let Some(m) = battle.state.at_mut(source) else { return };
    if m.volatiles.has(Volatiles::LOCKED_MOVE) && m.volatile_counters.locked_move == 2 {
        m.volatiles.clear(Volatiles::LOCKED_MOVE);
    }
}

/// Rollout's `onModifyMove` (before the `|move|` line): starts the
/// `rollout` volatile unless it is up or the user is asleep.
pub(crate) fn modify_move(battle: &mut Battle, user: Position, move_id: MoveId, target: Option<Position>) {
    let Some(target) = target.filter(|_| move_id == ids::ROLLOUT) else { return };
    // With a `sourceEffect` (called by another move, used while locked into
    // another move, or the action a waiting pledge moved up), it re-aims
    // `lastMoveTargetLoc`, which the locked turns use.
    let called = battle.order.source_effect.is_some()
        || super::order::is_locked(battle, user)
        || super::pledges::source(battle, user).is_some();
    let m = mon_mut(battle, user);
    if m.volatiles.has(Volatiles::ROLLOUT) || matches!(m.status, Status::Sleep { .. }) {
        return;
    }
    // `addVolatile('rollout')`: `TryAddVolatile`'s ties draw.
    super::events::try_add_volatile(battle, user, "rollout");
    let m = mon_mut(battle, user);
    m.volatiles.add(Volatiles::ROLLOUT);
    m.volatile_counters.rollout = 1;
    m.volatile_counters.rollout_hits = 0;
    if called {
        m.last_move_target_loc = super::targeting::loc_of(user, target);
    }
}

/// Rollout's `basePowerCallback`: doubles with each hit in a row, and each
/// hit keeps the lock another turn (5 hits at most).
pub(crate) fn base_power(battle: &mut Battle, user: Position, move_id: MoveId, base_power: u16) -> u16 {
    if move_id != ids::ROLLOUT {
        return base_power;
    }
    let m = mon_mut(battle, user);
    let mut bp = base_power;
    if m.volatiles.has(Volatiles::ROLLOUT) {
        let c = &mut m.volatile_counters;
        if c.rollout_hits > 0 {
            bp <<= c.rollout_hits;
        }
        if !matches!(m.status, Status::Sleep { .. }) {
            c.rollout_hits += 1;
            if c.rollout_hits < 5 {
                c.rollout = 2;
            }
        }
    }
    // Defense Curl doubles it.
    if m.volatiles.has(Volatiles::DEFENSE_CURL) {
        bp *= 2;
    }
    bp
}

/// Uproar's `onTryHit`: every sleeping active Pokémon wakes up, pairing the
/// target's side and the other side slot by slot.
pub(crate) fn try_hit(battle: &mut Battle, target: Position, move_id: MoveId) {
    if move_id != ids::UPROAR {
        return;
    }
    for slot in 0..battle.format.active_per_side() as u8 {
        for side in [target.side, target.side.foe()] {
            let pos = Position::new(side, slot);
            if mon(battle, pos).is_some_and(|m| matches!(m.status, Status::Sleep { .. })) {
                status::cure_status(battle, pos);
            }
        }
    }
}

/// Uproar's `onAnySetStatus`: nobody falls asleep during an uproar
/// (`-fail|<pokemon>|slp|[from] Uproar`, with `[msg]` for the holder
/// itself). A handler of the event, sorted by the holder's speed with the
/// abilities' `SetStatus` handlers (Sweet Veil, ...); `null` stops it.
pub(crate) fn uproar_set_status(
    battle: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
    h: &super::handlers::Handler,
) -> Option<super::events::Relay> {
    if ev.relay != super::events::Relay::Status(StatusId::Sleep) {
        return None;
    }
    let target = ev.target_pos()?;
    let super::handlers::Holder::Pokemon(holder) = h.holder else { return None };
    let a = actor(battle, target);
    battle.emit(BattleEvent::UproarPreventsSleep { target: a, own: holder == target });
    Some(super::events::Relay::Null)
}

// ---------------------------------------------------------------------
// Start-of-turn callbacks.

/// `resolveAction`: a move with a `beforeTurnCallback` (Counter, Mirror
/// Coat) gets a `beforeTurnMove` action aimed like the move.
pub(crate) fn before_turn_action(action: &Action) -> Option<Action> {
    let ActionKind::Move { move_id, target_loc, .. } = action.kind else { return None };
    matches!(move_id, ids::COUNTER | ids::MIRROR_COAT)
        .then(|| Action::new(ActionKind::BeforeTurnMove { move_id, target_loc }, action.mon))
}

/// `resolveAction`: a move with a `priorityChargeCallback` (Focus Punch,
/// Beak Blast, Chilly Reception) gets a `priorityChargeMove` action.
pub(crate) fn priority_charge_action(action: &Action) -> Option<Action> {
    let ActionKind::Move { move_id, .. } = action.kind else { return None };
    matches!(move_id, ids::FOCUS_PUNCH | ids::BEAK_BLAST | ids::CHILLY_RECEPTION)
        .then(|| Action::new(ActionKind::PriorityChargeMove { move_id, target_loc: 0 }, action.mon))
}

/// `resolveAction` for those actions: without a target location, a random
/// target is picked for the move (`getRandomTarget`, which can draw).
pub(crate) fn resolve_target_loc(battle: &mut Battle, action: &mut Action) {
    let (ActionKind::BeforeTurnMove { move_id, target_loc: 0 }
    | ActionKind::PriorityChargeMove { move_id, target_loc: 0 }) = action.kind
    else {
        return;
    };
    let Some(user) = action.mon.and_then(|m| position_of_mon(battle, m)) else { return };
    let target_type = battle.format.dex.move_(move_id).target;
    let Some(target) = targeting::get_random_target(battle, user, target_type) else { return };
    let loc = targeting::loc_of(user, target);
    if let ActionKind::BeforeTurnMove { target_loc, .. } | ActionKind::PriorityChargeMove { target_loc, .. } =
        &mut action.kind
    {
        *target_loc = loc;
    }
}

/// `runAction('beforeTurnMove')`: the move's `beforeTurnCallback` starts
/// Counter's or Mirror Coat's volatile. Returns false if the move has no
/// target (Showdown then skips the rest of the action).
pub(crate) fn before_turn_move(battle: &mut Battle, user: Position, move_id: MoveId, target_loc: Loc) -> bool {
    let target_type = battle.format.dex.move_(move_id).target;
    if targeting::get_target(battle, user, target_type, target_loc).is_none() {
        return false;
    }
    let (flag, id) =
        if move_id == ids::COUNTER { (Volatiles::COUNTER, "counter") } else { (Volatiles::MIRROR_COAT, "mirrorcoat") };
    // `addVolatile`: nothing if it is up (no `onRestart`), else
    // `TryAddVolatile`'s ties draw. Instruct's `beforeTurnMove` can add
    // one while the other is up.
    if !mon_mut(battle, user).volatiles.has(flag) {
        super::events::try_add_volatile(battle, user, id);
        let m = mon_mut(battle, user);
        m.volatiles.add(flag);
        let c = &mut m.volatile_counters;
        if flag == Volatiles::COUNTER {
            (c.counter_slot, c.counter_damage) = (None, 0);
        } else {
            (c.mirror_coat_slot, c.mirror_coat_damage) = (None, 0);
        }
    }
    true
}

/// The `slot` and `damage` of `move_id`'s volatile (Counter's or Mirror
/// Coat's), if it is up.
fn counter_state(m: &PokemonState, move_id: MoveId) -> Option<(Option<Position>, u16)> {
    let c = &m.volatile_counters;
    match move_id {
        ids::COUNTER if m.volatiles.has(Volatiles::COUNTER) => Some((c.counter_slot, c.counter_damage)),
        ids::MIRROR_COAT if m.volatiles.has(Volatiles::MIRROR_COAT) => Some((c.mirror_coat_slot, c.mirror_coat_damage)),
        _ => None,
    }
}

/// `runAction('priorityChargeMove')`: the move's `priorityChargeCallback`
/// starts its volatile (Focus Punch and Beak Blast announce it).
pub(crate) fn priority_charge_move(battle: &mut Battle, user: Position, move_id: MoveId) {
    if mon_mut(battle, user).volatiles.has(Volatiles::PRIORITY_CHARGE) {
        return;
    }
    // `addVolatile`: `TryAddVolatile`'s ties draw.
    super::events::try_add_volatile(battle, user, "prioritycharge");
    let m = mon_mut(battle, user);
    m.volatiles.add(Volatiles::PRIORITY_CHARGE);
    m.volatile_counters.priority_charge = move_id;
    m.volatile_counters.lost_focus = false;
    if matches!(move_id, ids::FOCUS_PUNCH | ids::BEAK_BLAST) {
        let a = actor(battle, user);
        battle.emit(BattleEvent::ChargeStart { actor: a, move_id });
    }
}

fn priority_charge(battle: &Battle, pos: Position) -> Option<MoveId> {
    mon(battle, pos)
        .filter(|m| m.volatiles.has(Volatiles::PRIORITY_CHARGE))
        .map(|m| m.volatile_counters.priority_charge)
}

/// Focus Punch's `beforeMoveCallback`: hit before it could strike, it
/// fails (`cant`). Returns true if the move stops here.
pub(crate) fn before_move_callback(battle: &mut Battle, user: Position, move_id: MoveId) -> bool {
    if move_id != ids::FOCUS_PUNCH || priority_charge(battle, user) != Some(ids::FOCUS_PUNCH) {
        return false;
    }
    if !mon(battle, user).is_some_and(|m| m.volatile_counters.lost_focus) {
        return false;
    }
    let a = actor(battle, user);
    battle.emit(BattleEvent::LostFocus { actor: a });
    true
}

/// The `focuspunch` volatile's `onTryAddVolatile`: a focused Pokémon does
/// not flinch. Collected even for other volatiles, before the ability
/// and item, so their ties are sorted before any handler can stop it.
pub(crate) fn focus_punch_try_add_volatile(_: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    (ev.relay == Relay::Id("flinch")).then_some(Relay::Null)
}

/// `runEvent('Hit')` on a target of a move: a hit (not by a status move)
/// breaks Focus Punch's focus; contact with a Pokémon charging Beak Blast
/// burns the attacker. `user` is `None` for an attacker off the field.
#[inline(always)]
pub(crate) fn on_hit(battle: &mut Battle, target: Position, user: Option<Position>, data: &MoveData) {
    if let Some(charge) = priority_charge(battle, target) {
        charged_on_hit(battle, target, user, data, charge);
    }
}

/// [`on_hit`] on a target charging `charge`.
fn charged_on_hit(battle: &mut Battle, target: Position, user: Option<Position>, data: &MoveData, charge: MoveId) {
    match charge {
        ids::FOCUS_PUNCH if data.category != MoveCategory::Status => {
            mon_mut(battle, target).volatile_counters.lost_focus = true;
        }
        // `checkMoveMakesContact` (Protective Pads).
        ids::BEAK_BLAST if data.flags.contains(MoveFlags::CONTACT) => {
            if let Some(user) = user
                && super::abilities::makes_contact(battle, data, user, target, None)
            {
                // `source.trySetStatus('brn', target)` from the
                // `beakblast` condition: the effect is the condition, not
                // a move (Flower Veil blocks it without `-block`).
                let effect = super::events::Effect::volatile(Volatiles::PRIORITY_CHARGE);
                status::set_status_from(battle, user, StatusId::Burn, Some(target), effect, true);
            }
        }
        _ => {}
    }
}

/// `AfterMove`: the move's own `onAfterMove` (Beak Blast's charge ends),
/// then the user's volatiles' (`lockedmove` ends after its last turn).
pub(crate) fn after_move(battle: &mut Battle, user: Position, move_id: MoveId) {
    // `removeVolatile` does nothing to a Pokémon at 0 HP.
    let Some(m) = battle.state.at_mut(user).filter(|m| !m.is_fainted()) else { return };
    if move_id == ids::BEAK_BLAST
        && m.volatiles.has(Volatiles::PRIORITY_CHARGE)
        && m.volatile_counters.priority_charge == ids::BEAK_BLAST
    {
        m.volatiles.clear(Volatiles::PRIORITY_CHARGE);
    }
    // (Rollout's `rolloutstorage` needs a Max Move: no Dynamax in Gen 9 or
    // Champions.)
}

/// `lockedmove`'s `onAfterMove` (a handler of `runEvent('AfterMove')`,
/// sorted with the abilities' and items'): the lock ends after its last
/// turn.
pub(crate) fn locked_move_after_move(
    battle: &mut Battle,
    _: &mut super::events::EventCtx<'_>,
    h: &Handler,
) -> Option<super::events::Relay> {
    let Holder::Pokemon(pos) = h.holder else { return None };
    if battle
        .state
        .at(pos)
        .is_some_and(|m| m.volatiles.has(Volatiles::LOCKED_MOVE) && m.volatile_counters.locked_move == 1)
    {
        remove_locked_move(battle, pos);
    }
    None
}

/// `onBeforeMove` handlers: Chilly Reception announces itself (priority 100).
pub(crate) fn before_move_handlers<const N: usize>(
    battle: &Battle,
    user: Position,
    handlers: &mut super::handlers::SpillList<N>,
) {
    let Some(m) = mon(battle, user) else { return };
    if m.volatiles.has(Volatiles::PRIORITY_CHARGE) && m.volatile_counters.priority_charge == ids::CHILLY_RECEPTION {
        handlers.push(Handler {
            volatile_bit: vol(Volatiles::PRIORITY_CHARGE),
            priority: 100,
            speed: m.speed,
            sub_order: sub_order::CONDITION,
            holder: Holder::Pokemon(user),
            callback: Callback::Plain(chilly_reception_before_move),
            ..Handler::default()
        });
    }
}

fn chilly_reception_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    let Holder::Pokemon(pos) = handler.holder else { return true };
    if battle.active_move.is_some_and(|(_, m)| m == ids::CHILLY_RECEPTION) {
        let a = actor(battle, pos);
        battle.emit(BattleEvent::Prepare { user: a, move_id: ids::CHILLY_RECEPTION });
    }
    true
}

// ---------------------------------------------------------------------
// Counter, Mirror Coat, Metal Burst, Comeuppance.

/// A hit dealt `dealt` damage to `target` (`spreadDamage`, then
/// `DamagingHit` and `gotAttacked` after the hit loop): remembers the last
/// foe that damaged it, and Counter / Mirror Coat remember a hit of their
/// category. `source` is the attacker, `source_pos` its slot if it is on
/// the field.
fn record_damage(
    battle: &mut Battle,
    target: Position,
    source: MonRef,
    source_pos: Option<Position>,
    category: MoveCategory,
    dealt: u16,
) {
    if source_pos == Some(target) || source.side == target.side {
        return;
    }
    let Some(m) = battle.state.at_mut(target) else { return };
    remember_damage(m, source, source_pos, dealt);
    // `counter` / `mirrorcoat`'s `onDamagingHit`. `DamagingHit` handlers
    // sort left to right without drawing, and this one only records, so it
    // runs here rather than in that sort. (A future attack from a Pokémon
    // off the field records a slot with no one in it, which nothing can
    // read: the volatile ends in the same residual, and `endTurn` forgets
    // the attack for Metal Burst.)
    let c = &mut m.volatile_counters;
    if category == MoveCategory::Physical && m.volatiles.has(Volatiles::COUNTER) {
        (c.counter_slot, c.counter_damage) = (source_pos, 2 * dealt);
    }
    if category == MoveCategory::Special && m.volatiles.has(Volatiles::MIRROR_COAT) {
        (c.mirror_coat_slot, c.mirror_coat_damage) = (source_pos, 2 * dealt);
    }
}

/// `gotAttacked`'s `attackedBy` entry as `getLastDamagedBy(true)` reads it
/// (only foes matter, and only each foe's latest entry: they leave the
/// list together). `source` damaged `m` by `dealt` from `source_pos`.
pub(crate) fn remember_damage(m: &mut PokemonState, source: MonRef, source_pos: Option<Position>, dealt: u16) {
    let entries = &mut m.record.damaged_by;
    entries.retain(|d| (d.side, d.id) != (source.side, source.id));
    entries.push(DamagedBy { side: source.side, id: source.id, slot: source_pos, damage: dealt, this_turn: true });
}

/// The move pipeline's hook after a hit's damage to `target`.
pub(crate) fn damaged(battle: &mut Battle, target: Position, user: Position, data: &MoveData, dealt: u16) {
    let Some(id) = mon(battle, user).map(|m| m.id) else { return };
    let source = MonRef { side: user.side, id };
    record_damage(battle, target, source, Some(user), data.category, dealt);
}

/// `endTurn`: this turn's damage becomes last turn's; damage from a
/// Pokémon that has left the field is forgotten.
pub(crate) fn end_turn(battle: &mut Battle) {
    let active_per_side = battle.format.active_per_side();
    // Each side's entries are changed in place, its foe's side read: p1's,
    // then p2's.
    let end = |own: &mut crate::state::SideState, foe: &crate::state::SideState| {
        for slot in 0..active_per_side.min(own.team.len()) {
            let entries = &mut own.team[slot].record.damaged_by;
            if entries.is_empty() {
                continue;
            }
            entries.retain(|d| foe.position_of(d.id).is_some_and(|p| p < active_per_side && foe.active[p].is_some()));
            entries.iter_mut().for_each(|d| d.this_turn = false);
        }
    };
    let [p1, p2] = &mut battle.state.sides;
    end(p1, p2);
    end(p2, p1);
}

/// Metal Burst's and Comeuppance's `onModifyTarget`: aimed at the slot of
/// the last foe that damaged the user.
pub(crate) fn modify_target(battle: &Battle, user: Position, move_id: MoveId) -> Option<Position> {
    if !matches!(move_id, ids::METAL_BURST | ids::COMEUPPANCE) {
        return None;
    }
    mon(battle, user)?.record.damaged_by.last()?.slot
}

/// Counter's and Mirror Coat's `onRedirectTarget` (doubles): aimed at the
/// slot of the foe that hit the user. `battle.active_move` is the move.
pub(crate) fn redirect_target(battle: &Battle, user: Position) -> Option<Position> {
    let (_, move_id) = battle.active_move?;
    counter_state(mon(battle, user)?, move_id)?.0
}

/// What a move's `onTry` decided.
pub(crate) enum TryResult {
    Continue,
    /// `false`: `-fail`.
    Fail,
    /// `NOT_FAIL`: the move stops here and counts as successful.
    Stop,
}

/// The move's `onTry` (Showdown's `singleEvent('Try')`, before the hit
/// steps).
pub(crate) fn on_try(battle: &mut Battle, user: Position, target: Position, move_id: MoveId) -> TryResult {
    let Some(m) = mon(battle, user) else { return TryResult::Continue };
    let ok = match move_id {
        ids::COUNTER | ids::MIRROR_COAT => counter_state(m, move_id).is_some_and(|(slot, _)| slot.is_some()),
        ids::METAL_BURST | ids::COMEUPPANCE => m.record.damaged_by.last().is_some_and(|d| d.this_turn),
        ids::FUTURE_SIGHT | ids::DOOM_DESIRE => {
            if !add_future_move(battle, target, user, move_id) {
                return TryResult::Fail;
            }
            let a = actor(battle, user);
            battle.emit(BattleEvent::FutureMoveStart { user: a, move_id });
            return TryResult::Stop;
        }
        _ => true,
    };
    if ok { TryResult::Continue } else { TryResult::Fail }
}

/// `damageCallback`, in half points: Counter and Mirror Coat return twice
/// the damage taken, Metal Burst and Comeuppance 1.5 times (at least 1),
/// which Showdown doesn't truncate (`lastDamagedBy.damage * 1.5 || 1`).
pub(crate) fn damage_callback(battle: &Battle, user: Position, move_id: MoveId) -> Option<u32> {
    let m = mon(battle, user)?;
    match move_id {
        ids::COUNTER | ids::MIRROR_COAT => {
            Some(counter_state(m, move_id).map_or(0, |(_, damage)| 2 * (damage as u32).max(1)))
        }
        ids::METAL_BURST | ids::COMEUPPANCE => {
            Some(m.record.damaged_by.last().map_or(0, |d| (3 * d.damage as u32).max(2)))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------
// Future Sight and Doom Desire.

/// `side.addSlotCondition(target, 'futuremove')`: fails if the slot already
/// awaits an attack.
fn add_future_move(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    let Some(source_id) = battle.state.side(user.side).team.get(user.slot as usize).map(|m| m.id) else {
        return false;
    };
    let turn = battle.state.turn;
    let slot = &mut battle.state.side_mut(target.side).future_moves[target.slot as usize];
    if slot.is_some() {
        return false;
    }
    // `endingTurn = turn - 1 + 2`.
    *slot = Some(FutureMove { move_id, source_side: user.side, source_id, ending_turn: turn + 1 });
    // `slotConditions[position]['futuremove']`: after the slot's older
    // conditions (Wish), which ties with it shuffle around.
    battle.state.side_mut(target.side).conditions.slots[target.slot as usize].order.push(SlotCondition::FutureMove);
    battle.conditions_changed();
    // `futuremove`'s `onStart` hint.
    if turn + 1 >= 254 {
        battle.hint(crate::event::Hint::FutureMoveTurnLimit);
    }
    true
}

// ---------------------------------------------------------------------
// End of turn.

/// `fieldEvent('Residual')` handlers: the durations of this module's
/// volatiles, `lockedmove`'s, Uproar's and Rollout's `onResidual`, and the
/// `futuremove` slot condition (order 3; it runs even for a fainted
/// Pokémon's slot).
pub(crate) fn residual_handlers(battle: &Battle, handlers: &mut Handlers) {
    for side in SideId::BOTH {
        let s = battle.state.side(side);
        for slot in 0..battle.format.active_per_side().min(s.team.len()) {
            let pos = Position::new(side, slot as u8);
            // Every volatile below: with none of them, nothing to collect.
            const ANY: u128 = Volatiles::TWO_TURN_MOVE
                | Volatiles::CHARGING
                | Volatiles::LOCKED_MOVE
                | Volatiles::ROLLOUT
                | Volatiles::PRIORITY_CHARGE
                | Volatiles::COUNTER
                | Volatiles::MIRROR_COAT
                | Volatiles::UPROAR;
            if let Some(m) = battle.state.at(pos)
                && m.volatiles.has(ANY)
            {
                let base = Handler {
                    speed: m.speed,
                    sub_order: sub_order::CONDITION,
                    holder: Holder::Pokemon(pos),
                    ..Handler::default()
                };
                let (v, c) = (m.volatiles, &m.volatile_counters);
                let mut push = |flag: u128, run: fn(&mut Battle, &Handler) -> bool| {
                    if v.has(flag) {
                        handlers.push(Handler { volatile_bit: vol(flag), callback: Callback::Plain(run), ..base });
                    }
                };
                push(Volatiles::TWO_TURN_MOVE, tick_two_turn_move);
                if c.charging > 0 {
                    push(Volatiles::CHARGING, tick_charging);
                }
                push(Volatiles::LOCKED_MOVE, residual_locked_move);
                push(Volatiles::ROLLOUT, residual_rollout);
                push(Volatiles::PRIORITY_CHARGE, end_one_turn);
                push(Volatiles::COUNTER, end_one_turn);
                push(Volatiles::MIRROR_COAT, end_one_turn);
                if v.has(Volatiles::UPROAR) {
                    let uproar = Handler { order: Some(28), sub_order: 1, ..base };
                    handlers.push(Handler {
                        volatile_bit: vol(Volatiles::UPROAR),
                        callback: Callback::Plain(residual_uproar),
                        ..uproar
                    });
                }
            }
            if s.future_moves[slot].is_some() {
                handlers.push(Handler {
                    effect: super::events::Effect::Slot(SlotCondition::FutureMove),
                    order: Some(3),
                    speed: s.team[slot].speed,
                    sub_order: sub_order::SLOT_CONDITION,
                    holder: Holder::Slot(pos),
                    callback: Callback::Plain(residual_future_move),
                    ..Handler::default()
                });
            }
        }
    }
}

fn holder(handler: &Handler) -> Position {
    match handler.holder {
        Holder::Pokemon(pos) | Holder::Slot(pos) => pos,
        _ => unreachable!("held by a Pokémon or its slot"),
    }
}

/// Counts down the duration of `handler`'s volatile: `Some(true)` if it is
/// still up (its `onResidual` runs), `Some(false)` if it has just ended
/// (the caller removes it), `None` if it was already gone.
fn tick(battle: &mut Battle, handler: &Handler, counter: fn(&mut PokemonState) -> &mut u8) -> Option<bool> {
    let m = battle.state.at_mut(holder(handler))?;
    if !m.volatiles.has(handler.volatile()) {
        return None;
    }
    let turns = counter(m);
    *turns = turns.saturating_sub(1);
    Some(*turns > 0)
}

fn tick_two_turn_move(battle: &mut Battle, handler: &Handler) -> bool {
    if tick(battle, handler, |m| &mut m.volatile_counters.two_turn) == Some(false) {
        remove_two_turn_move(battle, holder(handler));
        // Ran out: no `faintMessages` after it.
        return false;
    }
    true
}

fn tick_charging(battle: &mut Battle, handler: &Handler) -> bool {
    if tick(battle, handler, |m| &mut m.volatile_counters.charging) == Some(false) {
        mon_mut(battle, holder(handler)).volatiles.clear(Volatiles::CHARGING);
        return false;
    }
    true
}

/// Priority-charge and Counter volatiles last until the end of the turn.
/// A one-turn volatile's duration runs out (no `faintMessages` after it).
fn end_one_turn(battle: &mut Battle, handler: &Handler) -> bool {
    if let Some(m) = battle.state.at_mut(holder(handler)) {
        m.volatiles.clear(handler.volatile());
    }
    false
}

/// `lockedmove`: count down (ending it at 0), then `onResidual`: a sleeping
/// Pokémon stops (silently); the true duration goes down.
fn residual_locked_move(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    match tick(battle, handler, |m| &mut m.volatile_counters.locked_move) {
        Some(false) => {
            remove_locked_move(battle, pos);
            return false;
        }
        Some(true) => {
            let m = mon_mut(battle, pos);
            if matches!(m.status, Status::Sleep { .. }) {
                m.volatiles.clear(Volatiles::LOCKED_MOVE);
            }
            let c = &mut m.volatile_counters;
            c.locked_true_duration = c.locked_true_duration.saturating_sub(1);
        }
        None => {}
    }
    true
}

/// Uproar: count down (ending it at 0 with `-end`), then `onResidual`:
/// `-start|...|Uproar|[upkeep]` (after silently ending it if the user last
/// struggled).
fn residual_uproar(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    match tick(battle, handler, |m| &mut m.volatile_counters.uproar) {
        Some(false) => {
            // `removeVolatile` does nothing on a Pokémon at 0 HP (knocked
            // out by an earlier handler, its faint not yet reported).
            if mon_mut(battle, pos).hp > 0 {
                mon_mut(battle, pos).volatiles.clear(Volatiles::UPROAR);
                let a = actor(battle, pos);
                battle.emit(BattleEvent::UproarEnd { actor: a });
            }
            return false;
        }
        Some(true) => {
            // Throat Chop ends it.
            if mon_mut(battle, pos).volatiles.has(Volatiles::THROAT_CHOP) {
                mon_mut(battle, pos).volatiles.clear(Volatiles::UPROAR);
                let a = actor(battle, pos);
                battle.emit(BattleEvent::UproarEnd { actor: a });
                return true;
            }
            let m = mon_mut(battle, pos);
            if m.last_move == Some(ids::STRUGGLE) {
                m.volatiles.clear(Volatiles::UPROAR);
            }
            let a = actor(battle, pos);
            battle.emit(BattleEvent::UproarStart { actor: a, upkeep: true });
        }
        None => {}
    }
    true
}

/// Rollout: count down (ending it at 0), then `onResidual`: it ends
/// silently if the user last struggled.
fn residual_rollout(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    match tick(battle, handler, |m| &mut m.volatile_counters.rollout) {
        Some(false) => {
            mon_mut(battle, pos).volatiles.clear(Volatiles::ROLLOUT);
            return false;
        }
        Some(true) => {
            let m = mon_mut(battle, pos);
            if m.last_move == Some(ids::STRUGGLE) {
                m.volatiles.clear(Volatiles::ROLLOUT);
            }
        }
        None => {}
    }
    true
}

/// `futuremove`'s `onResidual`: two turns after it was used, the condition
/// ends and its `onEnd` makes the attack.
fn residual_future_move(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    let Some(fm) = battle.state.side(pos.side).future_moves[pos.slot as usize] else { return true };
    // `getOverflowedTurnCount() < endingTurn` (`endingTurn` never wraps).
    if super::slots::overflowed_turn(battle) < fm.ending_turn {
        return true;
    }
    // `removeSlotCondition`.
    let side = battle.state.side_mut(pos.side);
    side.future_moves[pos.slot as usize] = None;
    side.conditions.slots[pos.slot as usize].order.retain(|&c| c != SlotCondition::FutureMove);
    battle.conditions_changed();
    future_move_end(battle, pos, fm);
    true
}

/// `futuremove`'s `onEnd`: the foreseen attack hits whoever is in the slot
/// now, unless it fainted or is the user itself.
fn future_move_end(battle: &mut Battle, pos: Position, fm: FutureMove) {
    let side = battle.state.side(pos.side);
    // `this.hint(... did not hit because the target is ...)`.
    let fainted = side.active[pos.slot as usize].is_none();
    if fainted || pos.side == fm.source_side && side.team[pos.slot as usize].id == fm.source_id {
        battle.hint(crate::event::Hint::FutureMoveMissed { move_id: fm.move_id, fainted });
        return;
    }
    let target = actor(battle, pos);
    battle.emit(BattleEvent::FutureMoveEnd { target, move_id: fm.move_id });
    // `removeVolatile('Protect')`, `removeVolatile('Endure')`.
    mon_mut(battle, pos).volatiles.clear(Volatiles::PROTECT | Volatiles::ENDURE);
    future_move_hit(battle, pos, fm);
    // `futuremove`'s `onEnd`, after `trySpreadMoveHit` whatever it did: the
    // user's Life Orb, if it is on the field.
    let source_pos = future_source(battle, fm).1;
    if let Some(user) = source_pos
        && super::items::has_item(battle, user, pikapro_data::gen9::items::LIFE_ORB)
        && !battle.is_over()
    {
        let event = pikapro_data::Event::AfterMoveSecondarySelf;
        let orb = pikapro_data::gen9::items::LIFE_ORB;
        let effect = super::events::Effect::Item(orb);
        super::events::single_event_item(battle, event, user, orb, Some(pos), effect, super::events::Relay::True);
    }
    battle.active_move = None;
    check_win(battle, None);
}

/// The source of a future attack, which may have left the field: its state,
/// its slot if it is on the field, and how the log names it.
fn future_source(battle: &Battle, fm: FutureMove) -> (PokemonState, Option<Position>, Actor) {
    let side = battle.state.side(fm.source_side);
    let index = side.position_of(fm.source_id).expect("the user is on its team");
    let source = side.team[index];
    let on_field = index < battle.format.active_per_side() && side.active[index].is_some();
    let pos = Position::new(fm.source_side, index as u8);
    if on_field {
        (source, Some(pos), actor(battle, pos))
    } else {
        // A benched Pokémon keeps its Illusion.
        let species = super::abilities::switchin::seen_species(side, &source);
        let id = super::abilities::switchin::seen_id(side, &source);
        let a = Actor { pos: Position::new(fm.source_side, 0), species, active: false, id };
        (source, None, a)
    }
}

/// `trySpreadMoveHit([target], source, hitMove)` for a future attack: the
/// move made from the stored data (Special, no `protect` flag, no longer
/// ignoring type immunity) by a user that may have left the field.
fn future_move_hit(battle: &mut Battle, target: Position, fm: FutureMove) {
    let mut data = *battle.format.dex.move_(fm.move_id);
    let (source, source_pos, source_actor) = future_source(battle, fm);
    battle.active_move = Some((source_pos.unwrap_or(target), fm.move_id));
    // A new active move; the user's Infiltrator (and Normalize) still apply
    // if it is on the field.
    battle.move_state = Default::default();
    if let Some(pos) = source_pos {
        battle.move_state.infiltrates =
            super::abilities::has_ability(battle, pos, pikapro_data::gen9::abilities::INFILTRATOR);
        if super::abilities::has_ability(battle, pos, pikapro_data::gen9::abilities::NORMALIZE) {
            data.ty = Type::Normal;
        }
    }
    // The user's position, or its team position once it has left the
    // field (its handlers are then collected but do nothing).
    let index = battle.state.side(fm.source_side).position_of(fm.source_id).expect("the user is on its team");
    let damage_pos = source_pos.unwrap_or(Position::new(fm.source_side, index as u8));
    let target_actor = actor(battle, target);
    // hitStepInvulnerabilityEvent, with the user on the field or not: a
    // commanding Tatsugiri is missed, the `Invulnerability` handlers (the
    // charging moves', No Guard's) run, and the miss marks the last
    // `|move|` line, whoever used it.
    if !invulnerability_from(battle, target, damage_pos, Some(source_actor), fm.move_id, true, false) {
        return;
    }
    // hitStepTryHitEvent: protection does not stop a move without the
    // `protect` flag, and the move has no priority for Psychic Terrain;
    // the abilities' and items' `TryHit` (Wonder Guard, ...) still apply.
    let mut targets: super::targeting::Targets = [target].into_iter().collect();
    super::moves::try_hit_event(battle, &mut targets, damage_pos, fm.move_id, &data);
    if targets.is_empty() {
        return;
    }
    // hitStepTypeImmunity: `runImmunity` with added types and Levitate.
    let defender = *battle.state.at(target).expect("target is active");
    if !super::events::run_immunity(battle, target, data.ty, true) {
        return;
    }
    // hitStepAccuracy. The user's stages are 0 once it has switched out.
    // The `Accuracy` event: a Glaive Rush user can't be missed.
    // With the user on the field, it is Showdown's `hitStepAccuracy` with
    // `data.source` (Lock-On, the user's ability and item, ...); an
    // off-field user's ability and item are ignored (`!isActive`).
    if let Some(src) = source_pos
        && !super::effects::hit_step_accuracy(battle, src, target, fm.move_id, &data)
    {
        return;
    }
    // A Glaive Rush user is hit all the same (its `onAccuracy` returns
    // true), after `ModifyAccuracy` and the evasion's `ModifyBoost`, whose
    // ties draw (`hitStepAccuracy`).
    let acc = data.accuracy.filter(|_| source_pos.is_none());
    if let Some(acc) = acc {
        // An off-field user's `ModifyAccuracy` and
        // `Accuracy` events still collect the target's and the field's
        // handlers (No Guard, Sand Veil, Gravity, ...).
        use super::events::Relay;
        battle.move_state.active_target = Some(target);
        let event = pikapro_data::Event::ModifyAccuracy;
        let relay = Relay::Num(acc as i32);
        let modified = super::events::move_event(battle, event, target, Some(damage_pos), fm.move_id, &data, relay);
        let acc = match modified {
            Relay::Num(acc) => {
                // `hitStepAccuracy`: `runEvent('ModifyBoost', pokemon)` for
                // the user collects nothing off the field (`findEventHandlers`
                // needs it or a source active), but the target's
                // `runEvent('ModifyBoost', target)` for the evasion runs its
                // side's handlers (Unaware), whose ties draw.
                let evasion =
                    super::events::boost_stage(battle, target, defender.boosts, pikapro_data::BoostId::Evasion);
                let boost = (source.boosts.accuracy.clamp(-6, 6) - evasion.clamp(-6, 6)).clamp(-6, 6);
                let acc = acc.max(0) as u32;
                let acc = if boost >= 0 { acc * (3 + boost as u32) / 3 } else { acc * 3 / (3 + (-boost) as u32) };
                let event = pikapro_data::Event::Accuracy;
                let relay = Relay::Num(acc as i32);
                match super::events::move_event(battle, event, target, Some(damage_pos), fm.move_id, &data, relay) {
                    Relay::Num(n) => Some(n.max(0) as u32),
                    Relay::True => None,
                    _ => Some(0),
                }
            }
            _ => None,
        };
        if acc.is_some_and(|acc| !battle.rng.chance(acc, 100)) {
            battle.attr_last_move(crate::event::MoveAttr::Miss);
            battle.emit(BattleEvent::Miss { user: source_actor, target: target_actor });
            return;
        }
    }
    // hitStepMoveHitLoop: one hit (`move.hit = 1`: Tera Shell forgets an
    // earlier move's resisted hit). `tryPrimaryHitEvent` first: a
    // substitute takes it instead. An off-field user is
    // never the target (a slot past the active ones stands for it).
    battle.move_state.hit_number = 1;
    let user = source_pos.unwrap_or(Position::new(SideId::P1, MAX_ACTIVE as u8));
    let targets: super::targeting::Targets = [target].into_iter().collect();
    let primary = super::conditions::try_primary_hit(battle, &targets, user, fm.move_id, &data, |battle, t| {
        // In half points.
        super::moves::roll_damage(battle, damage_pos, t, fm.move_id, &data, false, false, 1).map(|d| 2 * d)
    });
    // What `gotAttacked` gets, after `faintMessages`.
    let mut attacked = None;
    if !primary.hit.is_empty() {
        let amount =
            super::moves::roll_damage(battle, damage_pos, target, fm.move_id, &data, false, false, 1).unwrap_or(0);
        // A Pokémon revived in its slot, not switched in yet: `spreadDamage`
        // fails (`!target.isActive`), so `runMoveEffects` did nothing and
        // logs `-fail` for the move's user (`[still]` on the last `|move|`
        // line), and the hit loop ends there.
        if super::slots::off_field(battle, target) {
            battle.attr_last_move(crate::event::MoveAttr::Still);
            battle.emit(BattleEvent::Fail { user: source_actor });
            return;
        }
        let dealt = apply_damage(battle, target, amount, DamageSource::Move);
        // `runMoveEffects`: nothing but the `Hit` event (the target's
        // conditions, then its abilities' and items': Enigma Berry).
        on_hit(battle, target, source_pos, &data);
        let hit = pikapro_data::Event::Hit;
        super::events::move_event(battle, hit, target, Some(damage_pos), fm.move_id, &data, super::events::Relay::True);
        // `DamagingHit` (a Special move: Mirror Coat) and `gotAttacked`.
        let source_ref = MonRef { side: fm.source_side, id: fm.source_id };
        record_damage(battle, target, source_ref, source_pos, data.category, dealt);
        attacked = Some(dealt);
        // `runEvent('DamagingHit')` of the abilities and items (Stamina,
        // Weak Armor, ...).
        // The source is the user even off the field (Spicy Spray burns it).
        let mut relays = [super::events::Relay::Num(dealt as i32)];
        let effect = super::events::Effect::Move(fm.move_id);
        let event = pikapro_data::Event::DamagingHit;
        super::events::run_event_multi(battle, event, &[target], Some(damage_pos), effect, &mut relays, Some(&data));
        // `move.totalDamage` after the hit and
        // `getLastAttackedBy().damage` (Berserk, Anger Shell).
        battle.move_state.total_damage = dealt as u32;
        battle.move_state.last_damage[target.side.index() * 2 + (target.slot as usize & 1)] = dealt;
    }
    each_update(battle);
    // `faintMessages(false, false, !pokemon.hp)`: Showdown carries on with
    // the hit loop even if this ended the battle (the `Update` sort below
    // still draws). `pokemon.hp` is the user's HP now, not before the
    // hit: an Innards Out that knocked it out makes this check for a
    // winner, and with both sides out the side of the last faint wins
    // (`checkWin(faintData)`), before `futuremove`'s `onEnd` calls
    // `checkWin()` without faint data.
    let side = battle.state.side(fm.source_side);
    let user_fainted = side.position_of(fm.source_id).is_some_and(|i| side.team[i].is_fainted());
    faint_messages(battle, user_fainted);
    // `gotAttacked` and `timesAttacked` for Rage Fist, after
    // `faintMessages`: a target the hit knocked out keeps them.
    if let Some(dealt) = attacked {
        let source_ref = MonRef { side: fm.source_side, id: fm.source_id };
        super::power::got_attacked(battle, target, source_ref, source_pos, Some(dealt), 1);
    }
    each_update(battle);
    // `afterMoveSecondaryEvent` (its source the user, even off the field:
    // Berserk), unless a substitute took the hit; then Emergency Exit's
    // check, even then (`damage` is 0, a number). A target the hit knocked
    // out is still among `targetsCopy` (see `move_hit_loop`).
    if primary.hit.is_empty() && primary.substitute.is_empty() {
        return;
    }
    if !primary.hit.is_empty() {
        let mut relays = [super::events::Relay::True];
        let effect = super::events::Effect::Move(fm.move_id);
        let event = pikapro_data::Event::AfterMoveSecondary;
        super::events::run_event_multi(battle, event, &[target], Some(damage_pos), effect, &mut relays, Some(&data));
    }
    // Emergency Exit and Wimp Out (`runEvent('EmergencyExit', target,
    // pokemon)`): the target fell to half its max HP or below this hit (an
    // off-field user has no Sheer Force).
    let total = battle.move_state.total_damage as u16;
    if let Some(m) = battle.state.at(target).filter(|m| m.hp > 0) {
        let before = m.volatile_counters.hurt_hp.saturating_add(total);
        if (m.hp as u32) * 2 <= m.max_hp() as u32 && (before as u32) * 2 > m.max_hp() as u32 {
            super::run_mon_event_from(battle, pikapro_data::Event::EmergencyExit, target, Some(damage_pos));
        }
    }
}

#[cfg(test)]
mod tests {
    use pikapro_data::gen9::{abilities as a, moves as m, species as s};

    use super::*;
    use crate::state::SwitchFlag;
    use crate::{BattleOptions, Choice, Format, PokemonSet, Seed, SlotAction, Team};

    fn set(
        species: pikapro_data::SpeciesId,
        level: u8,
        ability: pikapro_data::AbilityId,
        moves: &[MoveId],
    ) -> PokemonSet {
        PokemonSet::new(species).with_level(level).with_ability(ability).with_moves(moves)
    }

    fn both_move(b: &mut Battle) {
        let first = Choice::single(SlotAction::Move { index: 0, target: None, gimmick: None });
        for side in SideId::BOTH {
            b.submit(side, first).unwrap();
        }
    }

    #[test]
    fn interrupted_charges_do_not_power_up_failure_moves() {
        use crate::format::TeamPreview;
        use crate::resolve::{moves, power};
        use crate::state::MoveResult;

        let charges = [
            m::BOUNCE,
            m::FLY,
            m::DIG,
            m::DIVE,
            m::PHANTOM_FORCE,
            m::SHADOW_FORCE,
            m::SKY_ATTACK,
            m::SOLAR_BEAM,
            m::SOLAR_BLADE,
            m::METEOR_BEAM,
            m::ELECTRO_SHOT,
            m::FREEZE_SHOCK,
            m::ICE_BURN,
        ];
        for preset in Format::presets() {
            for (i, charge) in charges.into_iter().enumerate() {
                let mut format = preset.clone();
                format.team_preview = TeamPreview::None;
                format.team_size = (1, 6);
                let mut p1 = Team::new(vec![
                    set(s::MEW, 50, a::NO_ABILITY, &[charge, m::TEMPER_FLARE, m::STOMPING_TANTRUM]),
                    set(s::BLISSEY, 50, a::NO_ABILITY, &[m::SPLASH]),
                ]);
                let mut p2 = Team::new(vec![
                    set(s::SNORLAX, 50, a::NO_ABILITY, &[m::SPLASH]),
                    set(s::WOBBUFFET, 50, a::NO_ABILITY, &[m::SPLASH]),
                ]);
                if format.stat_system == crate::format::StatSystem::StatPoints {
                    for team in [&mut p1, &mut p2] {
                        for set in &mut team.sets {
                            set.investment = crate::Investment::StatPoints(pikapro_data::Stats::splat(0));
                        }
                    }
                }
                let mut b =
                    Battle::new(format, [p1, p2], BattleOptions::new(Seed::fast(57_000_000 + i as u64))).unwrap();
                let user = Position::new(SideId::P1, 0);
                let target = Position::new(SideId::P2, 0);
                moves::run_move(&mut b, user, charge, 1, super::super::order::RunMoveOptions::default());
                assert_eq!(
                    mon(&b, user).unwrap().volatile_counters.move_this_turn,
                    MoveResult::Null,
                    "{} {charge:?}",
                    preset.id
                );
                // Fly/Bounce can be interrupted by Gravity or Smack Down;
                // the other charges can be cancelled by switching out.
                if !knock_down(&mut b, user) {
                    move_aborted(&mut b, user);
                }
                assert!(!mon(&b, user).unwrap().volatiles.has(Volatiles::TWO_TURN_MOVE));
                power::end_turn(&mut b);
                for attack in [m::TEMPER_FLARE, m::STOMPING_TANTRUM] {
                    let data = *b.format.dex.move_(attack);
                    assert_eq!(power::base_power_callback(&mut b, user, target, attack, &data, 1), Some(75));
                }
            }
        }
    }

    /// A future attack runs the hit loop's `EmergencyExit` check, so
    /// Emergency Exit switches out a Pokémon it takes below half its HP.
    #[test]
    fn future_sight_triggers_emergency_exit() {
        let p1 = Team::new(vec![set(s::ALAKAZAM, 5, a::SYNCHRONIZE, &[m::FUTURE_SIGHT])]);
        let p2 = Team::new(vec![
            set(s::GOLISOPOD, 50, a::EMERGENCY_EXIT, &[m::SPLASH]),
            set(s::BLISSEY, 50, a::NATURAL_CURE, &[m::SPLASH]),
        ]);
        let mut b = Battle::new(Format::gen9_singles(), [p1, p2], BattleOptions::new(Seed::fast(3))).unwrap();
        let pos = Position::new(SideId::P2, 0);
        let half = b.state.at(pos).unwrap().max_hp() / 2;
        b.state.at_mut(pos).unwrap().hp = half + 1;
        // Future Sight lands at the end of turn 3.
        for _ in 0..3 {
            both_move(&mut b);
        }
        let golisopod = b.state.side(SideId::P2).team[0];
        assert!(golisopod.hp <= half && golisopod.hp > 0, "the attack took it below half: {}", golisopod.hp);
        assert_eq!(golisopod.switch_flag, SwitchFlag::Fainted, "Emergency Exit's `switchFlag = true`");
        assert!(matches!(b.request(SideId::P2), crate::Request::ForceSwitch { .. }));
    }
}
