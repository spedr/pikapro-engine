//! Protection and hit-blocking effects.
//!
//! Showdown references: `protect`, `stall` and the other protection moves in
//! `data/moves.ts` (`detect`, `spikyshield`, `kingsshield`, `banefulbunker`,
//! `silktrap`, `burningbulwark`, `obstruct`); `checkMoveBypassesProtect` in
//! `sim/battle.ts`; `hitStepTryHitEvent` / `hitStepTryImmunity` /
//! `hitStepBreakProtect` in `sim/battle-actions.ts`.
//!
//! Contract (called by the move pipeline in `moves.rs`):
//! - [`handles`]: moves whose effect this module implements itself.
//! - [`prepare_hit`]: Showdown's `onPrepareHit` (`PrepareHit` step).
//! - [`on_hit`]: the move's effect when it lands on the user.
//! - [`try_hit`]: the `TryHit` hit step for one target (blocking).
//! - [`try_immunity`]: the `TryImmunity` hit step (powder vs Grass).
//! - [`break_protect`]: the `BreakProtect` hit step (Feint).
//! - [`residual_handlers`]: end-of-turn handlers for these volatiles.
//!
//! Endure is a registry volatile (`conditions.rs`), Wide Guard and Quick
//! Guard side conditions (`field.rs`), Prankster's Dark immunity an
//! ability (`abilities/misc.rs`). Max Guard, Crafty Shield and Mat Block
//! are Past in Gen 9 and Champions.

use pikapro_data::gen9::moves as ids;
use pikapro_data::{BoostId, MoveCategory, MoveData, MoveFlags, MoveId, StatusId, Type};

use super::handlers::{Callback, Handler, Handlers, Holder, sub_order, vol};
use super::targeting::Targets;
use super::{DamageSource, actor, apply_damage, boosts, field, status};
use crate::battle::Battle;
use crate::event::BattleEvent;
use crate::state::{Position, SideId, Volatiles};

/// Every protection volatile.
const PROTECTIONS: u128 = Volatiles::PROTECT
    | Volatiles::SPIKY_SHIELD
    | Volatiles::KINGS_SHIELD
    | Volatiles::BANEFUL_BUNKER
    | Volatiles::SILK_TRAP
    | Volatiles::BURNING_BULWARK
    | Volatiles::OBSTRUCT;

/// Protections that block status moves too (`checkMoveBypassesProtect`'s
/// `blockStatus`).
const BLOCKS_STATUS: u128 = Volatiles::PROTECT | Volatiles::SPIKY_SHIELD | Volatiles::BANEFUL_BUNKER;

/// The volatile a protection move sets (Detect sets Protect's).
fn volatile_of(move_id: MoveId) -> Option<u128> {
    Some(match move_id {
        ids::PROTECT | ids::DETECT => Volatiles::PROTECT,
        ids::SPIKY_SHIELD => Volatiles::SPIKY_SHIELD,
        ids::KINGS_SHIELD => Volatiles::KINGS_SHIELD,
        ids::BANEFUL_BUNKER => Volatiles::BANEFUL_BUNKER,
        ids::SILK_TRAP => Volatiles::SILK_TRAP,
        ids::BURNING_BULWARK => Volatiles::BURNING_BULWARK,
        ids::OBSTRUCT => Volatiles::OBSTRUCT,
        _ => return None,
    })
}

/// Moves this module implements (the rest of their data is not used).
pub(crate) fn handles(move_id: MoveId) -> bool {
    volatile_of(move_id).is_some()
}

/// A protection move's `onPrepareHit`: fails without a draw if nobody acts
/// after the user; otherwise, while the `stall` volatile is up, succeeds
/// with chance `1 / 3^n`, capped at 1/729 (`stall`'s `onStallMove`). True
/// for moves this module does not handle.
#[inline]
pub(crate) fn prepare_hit(battle: &mut Battle, user: Position, move_id: MoveId) -> bool {
    if !handles(move_id) {
        return true;
    }
    battle.queue.will_act() && stall_move(battle, user)
}

/// `runEvent('StallMove')`: while the `stall` volatile is up, succeeds with
/// chance `1 / 3^n`, capped at 1/729 (`stall`'s `onStallMove`); a failure
/// ends it.
pub(crate) fn stall_move(battle: &mut Battle, pos: Position) -> bool {
    let Some(&mon) = battle.state.at(pos) else { return true };
    if mon.stall_turns == 0 {
        return true;
    }
    let denominator = 3u32.pow(mon.protect_counter.min(6) as u32);
    let success = battle.rng.chance(1, denominator);
    if !success {
        let m = battle.state.at_mut(pos).expect("checked above");
        m.stall_turns = 0;
        m.protect_counter = 0;
        m.volatiles.clear(Volatiles::STALL);
    }
    success
}

/// `addVolatile('stall')`: a new one (after `TryAddVolatile`, whose ties
/// draw) starts the counter; `onRestart` triples it, up to 729. It lasts
/// until the next turn ends.
pub(crate) fn add_stall(battle: &mut Battle, pos: Position) {
    if battle.state.at(pos).is_some_and(|m| !m.is_fainted() && !m.volatiles.has(Volatiles::STALL)) {
        super::events::try_add_volatile(battle, pos, "stall");
    }
    let Some(m) = battle.state.at_mut(pos).filter(|m| !m.is_fainted()) else { return };
    m.protect_counter = if m.volatiles.has(Volatiles::STALL) { (m.protect_counter + 1).min(6) } else { 1 };
    m.stall_turns = 2;
    m.volatiles.add(Volatiles::STALL);
}

/// A protection move's hit: its volatile (`-singleturn`), then `stall`
/// (started, or restarted with the counter tripled).
pub(crate) fn on_hit(battle: &mut Battle, user: Position, move_id: MoveId) {
    let volatile = volatile_of(move_id).expect("a protection move");
    // The protection (`addVolatile` fails silently if it is already up,
    // e.g. used again through Instruct), then `stall` from its `onHit`
    // (restarted if up). A new volatile first runs `runStatusImmunity` and
    // `TryAddVolatile`, whose tied handlers (two Veil abilities) draw.
    let started = !battle.state.at(user).expect("user is active").volatiles.has(volatile);
    if started {
        super::events::try_add_volatile(battle, user, "protect");
        battle.state.at_mut(user).expect("user is active").volatiles.add(volatile);
        let actor = actor(battle, user);
        battle.emit(BattleEvent::ProtectStart { actor, move_id });
    }
    add_stall(battle, user);
}

/// `TryHit` for one target: false blocks the move (and logs why). The
/// protections' `onTryHit`: `-activate`, then the contact penalty.
pub(crate) fn try_hit(battle: &mut Battle, target: Position, user: Position, data: &MoveData) -> bool {
    let defender = battle.state.at(target).expect("target is active");
    let protection = defender.volatiles.bits() & PROTECTIONS;
    if protection == 0 {
        return true;
    }
    // `checkMoveBypassesProtect`: `runEvent('HitProtect')` can let the
    // move through.
    let blocks_status = protection & BLOCKS_STATUS != 0;
    if !data.flags.contains(MoveFlags::PROTECT) || data.category == MoveCategory::Status && !blocks_status {
        return true;
    }
    if let Some((_, move_id)) = super::active_dex_move(battle) {
        let relay = super::events::Relay::True;
        let event = pikapro_data::Event::HitProtect;
        if !super::events::move_event(battle, event, user, Some(target), move_id, data, relay).is_truthy() {
            return true;
        }
    }
    // A `smartTarget` move (Dragon Darts) is blocked
    // silently, and loses `smartTarget`.
    if !super::order::smart_target_missed(battle) {
        battle.emit(BattleEvent::Protected { target: actor(battle, target) });
    }
    super::multiturn::protected_locked_move(battle, user);
    // `checkMoveMakesContact` (Protective Pads).
    if super::abilities::makes_contact(battle, data, user, target, None) {
        contact_penalty(battle, protection, target, user);
    }
    false
}

/// What touching the protection does to the attacker.
fn contact_penalty(battle: &mut Battle, protection: u128, target: Position, user: Position) {
    // The boosts' effect is the protection move
    // (`this.dex.getActiveMove("King's Shield")`), not the attacker's.
    match protection {
        Volatiles::SPIKY_SHIELD => {
            let Some(max_hp) = battle.state.at(user).map(|m| m.max_hp()) else { return };
            let of = actor(battle, target);
            let source = DamageSource::MoveEffect { move_id: ids::SPIKY_SHIELD, of };
            apply_damage(battle, user, super::hp_fraction(u32::from(max_hp), 1, 8), source);
        }
        Volatiles::KINGS_SHIELD => {
            let effect = super::events::Effect::Move(ids::KINGS_SHIELD);
            boosts::boost_from(battle, user, boosts::one_boost(BoostId::Atk, -1), Some(target), effect, false, false);
        }
        Volatiles::SILK_TRAP => {
            let effect = super::events::Effect::Move(ids::SILK_TRAP);
            boosts::boost_from(battle, user, boosts::one_boost(BoostId::Spe, -1), Some(target), effect, false, false);
        }
        Volatiles::OBSTRUCT => {
            let effect = super::events::Effect::Move(ids::OBSTRUCT);
            boosts::boost_from(battle, user, boosts::one_boost(BoostId::Def, -2), Some(target), effect, false, false);
        }
        // `source.trySetStatus(status, target)`: the effect is
        // `battle.effect`, the protection's condition (not a move: Flower
        // Veil and Safeguard block it silently), with no status of its own,
        // so failures are silent (as for a secondary).
        Volatiles::BANEFUL_BUNKER | Volatiles::BURNING_BULWARK => {
            let status = if protection == Volatiles::BANEFUL_BUNKER { StatusId::Poison } else { StatusId::Burn };
            let effect = super::events::Effect::volatile(protection);
            status::set_status_from(battle, user, status, Some(target), effect, true);
        }
        _ => {}
    }
}

/// `TryImmunity` for one target: false makes the target immune. Gen 6+:
/// powder moves don't affect Grass types (`dex.getImmunity('powder')`).
pub(crate) fn try_immunity(battle: &mut Battle, target: Position, user: Position, data: &MoveData) -> bool {
    // `getTypes()` includes an added type (Forest's Curse).
    let grass = battle.state.at(target).is_some_and(|m| super::manipulation::has_type(m, Type::Grass));
    // (Overcoat and Safety Goggles stop powder moves earlier, in `TryHit`;
    // `dex.getImmunity('powder')` here is the type's alone.)
    if data.flags.contains(MoveFlags::POWDER) && target != user && grass {
        battle.emit(BattleEvent::Immune { target: actor(battle, target) });
        return false;
    }
    true
}

/// Moves with `breaksProtect`.
fn breaks_protect(move_id: MoveId) -> bool {
    matches!(move_id, ids::FEINT | ids::HYPERSPACE_FURY | ids::HYPERSPACE_HOLE | ids::PHANTOM_FORCE | ids::SHADOW_FORCE)
}

/// Showdown's `hitStepBreakProtect`: a `breaksProtect` move removes the
/// targets' protections (and their `stall` counter).
#[inline(always)]
pub(crate) fn break_protect(battle: &mut Battle, targets: &Targets, move_id: MoveId) {
    if breaks_protect(move_id) {
        break_protections(battle, targets, move_id);
    }
}

/// [`break_protect`] for a move that breaks protections.
fn break_protections(battle: &mut Battle, targets: &Targets, move_id: MoveId) {
    for &t in targets.iter() {
        let Some(mon) = battle.state.at(t) else { continue };
        let protected = mon.volatiles.has(PROTECTIONS);
        // Gen 6+: the target side's guards (Wide Guard, Quick Guard) too.
        let guarded = field::break_guards(battle, t.side);
        if !protected && !guarded {
            continue;
        }
        let mon = battle.state.at_mut(t).expect("checked above");
        // Gen 6+: `stall` goes too.
        mon.volatiles.clear(PROTECTIONS | Volatiles::STALL);
        mon.stall_turns = 0;
        mon.protect_counter = 0;
        let target = actor(battle, t);
        battle.emit(BattleEvent::ProtectionBroken { target, move_id });
    }
}

/// Protections (duration 1) and `stall` (duration 2) count down at end of
/// turn.
pub(crate) fn residual_handlers(battle: &Battle, handlers: &mut Handlers) {
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            let pos = Position::new(side, slot as u8);
            let Some(mon) = battle.state.at(pos) else { continue };
            // Neither below: nothing to collect.
            if !mon.volatiles.has(PROTECTIONS | Volatiles::STALL) {
                continue;
            }
            let base = Handler {
                speed: mon.speed,
                sub_order: sub_order::CONDITION,
                holder: Holder::Pokemon(pos),
                ..Handler::default()
            };
            let protection = mon.volatiles.bits() & PROTECTIONS;
            if protection != 0 {
                // One handler ends every protection (`end_protect`); it is
                // keyed on the lowest one, should two ever be up at once.
                handlers.push(Handler {
                    volatile_bit: vol(protection & protection.wrapping_neg()),
                    callback: Callback::Plain(end_protect),
                    ..base
                });
            }
            if mon.volatiles.has(Volatiles::STALL) {
                handlers.push(Handler {
                    volatile_bit: vol(Volatiles::STALL),
                    callback: Callback::Plain(tick_stall),
                    ..base
                });
            }
        }
    }
}

/// Duration 1: always runs out, so no `faintMessages` after it.
fn end_protect(battle: &mut Battle, handler: &Handler) -> bool {
    if let Holder::Pokemon(pos) = handler.holder
        && let Some(mon) = battle.state.at_mut(pos)
    {
        mon.volatiles.clear(PROTECTIONS);
    }
    false
}

fn tick_stall(battle: &mut Battle, handler: &Handler) -> bool {
    if let Holder::Pokemon(pos) = handler.holder
        && let Some(mon) = battle.state.at_mut(pos)
        && mon.stall_turns > 0
    {
        mon.stall_turns -= 1;
        if mon.stall_turns == 0 {
            mon.protect_counter = 0;
            mon.volatiles.clear(Volatiles::STALL);
            // Ran out: no `faintMessages` after it.
            return false;
        }
    }
    true
}
