//! Volatile conditions.
//!
//! Showdown references: `flinch`, `confusion`, `mustrecharge` in
//! `data/conditions.ts`; the `taunt`, `encore` and `disable` move conditions
//! and Fake Out's `onTry` in `data/moves.ts`; `Pokemon.addVolatile` /
//! `removeVolatile` in `sim/pokemon.ts`; the `cantusetwice` flag in
//! `BattleActions.runMove` and `Battle.endTurn`.
//!
//! Contract:
//! - [`add_volatile`]: add a volatile by Showdown condition ID (the IDs in
//!   `MoveData::volatile_status`, `Secondary::volatile_status`,
//!   `MoveData::self_volatile`). Returns whether it was added, or `None`
//!   when a `TryAddVolatile` handler interrupted it silently (`null`).
//! - [`before_move_handlers`]: `onBeforeMove` (mustrecharge 11, flinch 8,
//!   disable 7, taunt 5, confusion 3, by `onBeforeMovePriority`).
//! - [`residual_handlers`]: durations and `onResidual`.
//! - [`disable_moves`]: `endTurn`'s `DisableMove` pass, which decides the
//!   moves the next request offers (taunt, encore, disable, cantusetwice).
//! - [`try_move`], [`try_hit`], [`override_action`]: move callbacks
//!   (Fake Out's `onTry`, Disable's `onTryHit`, Encore's `onOverrideAction`).
//! - [`RECHARGE`]: the move a recharging Pokémon is locked into.
//! - [`handles`]: status moves implemented here beyond their data.
//!
//! State: presence bits in [`Volatiles`], turn counters in
//! [`VolatileCounters`](crate::state::VolatileCounters).
//!
//! Substitute, trapping, Leech Seed and the other residual volatiles live in
//! `conditions.rs`; the functions here route to it.

use pikapro_data::gen9::moves as ids;
use pikapro_data::{MoveCategory, MoveFlags, MoveId, MoveTarget, Stat};

use super::handlers::{Callback, Handler, Handlers, Holder, sub_order, vol};
use super::queue::{self, Action, ActionKind, speed_sort};
use super::{DamageSource, actor, apply_damage, moves};
use super::{conditions, field};

use crate::battle::Battle;
use crate::event::{BattleEvent, CantReason, Volatile};
use crate::format::Mechanics;
use crate::state::{Position, SideId, Volatiles};

/// Showdown's `recharge` pseudo-move: what a Pokémon with `mustrecharge` is
/// locked into (`onLockMove: 'recharge'`). It has no data; the request
/// offers it as the only move and `mustrecharge`'s `onBeforeMove` stops it.
pub(crate) const RECHARGE: MoveId = MoveId(u16::MAX);

/// Status moves whose effect this module implements beyond their data
/// (their `condition` / `onTryHit`).
pub(crate) fn handles(move_id: MoveId) -> bool {
    matches!(move_id, ids::TAUNT | ids::ENCORE | ids::DISABLE) || conditions::handles(move_id)
}

/// Priority and target type of a queued move. [`RECHARGE`] has priority 0
/// and no target type, which Showdown's targeting treats like a spread
/// target: never a valid chosen location, so `getTarget` falls back to
/// `getRandomTarget` (a random foe in doubles).
pub(crate) fn queued_move(battle: &Battle, move_id: MoveId) -> (i8, MoveTarget) {
    if move_id == RECHARGE {
        return (0, MoveTarget::AllAdjacentFoes);
    }
    let data = battle.format.dex.move_(move_id);
    (data.priority, data.target)
}

/// Whether the active Pokémon in `slot` of `side` is locked into recharging.
pub(crate) fn must_recharge(battle: &Battle, side: SideId, slot: usize) -> bool {
    battle.state.side(side).active(slot).is_some_and(|m| m.volatiles.has(Volatiles::MUST_RECHARGE))
}

/// Showdown's `addVolatile` for the volatiles implemented here. Returns
/// whether it was added; an unknown ID is never added. `is_secondary` is
/// true for secondary effects (Showdown's `addVolatile` does not
/// distinguish them; callers use it for failure messages).
pub(crate) fn add_volatile(
    battle: &mut Battle,
    target: Position,
    id: &'static str,
    source: Option<Position>,
    is_secondary: bool,
) -> Option<bool> {
    let effect = super::field::current_effect(battle);
    add_volatile_from(battle, target, id, source, is_secondary, effect)
}

/// [`add_volatile`] with its `sourceEffect` named: an ability's volatile
/// (Cursed Body's Disable, Cute Charm's Attract, Poison Puppeteer's
/// confusion) is announced `[from] ability: X|[of] <source>`, and
/// `TryAddVolatile` handlers see the effect (Aroma Veil only announces
/// blocking a move's).
pub(crate) fn add_volatile_from(
    battle: &mut Battle,
    target: Position,
    id: &'static str,
    source: Option<Position>,
    _is_secondary: bool,
    effect: super::events::Effect,
) -> Option<bool> {
    // The volatiles other modules start: a new one
    // first goes through `runStatusImmunity` and `TryAddVolatile`, whose
    // handlers (abilities) can stop it or draw on a speed tie.
    use super::manipulation::OwnVolatile;
    let delegated = match id {
        "helpinghand" => Some(OwnVolatile::Flag(Volatiles::HELPING_HAND)),
        "followme" => Some(OwnVolatile::Flag(Volatiles::FOLLOW_ME)),
        "ragepowder" => Some(OwnVolatile::Flag(Volatiles::RAGE_POWDER)),
        "lockedmove" => Some(OwnVolatile::Flag(Volatiles::LOCKED_MOVE)),
        "uproar" => Some(OwnVolatile::Flag(Volatiles::UPROAR)),
        _ => super::manipulation::volatile_of(id),
    };
    if let (Some(volatile), Some(mon)) = (delegated, battle.state.at(target))
        && !mon.is_fainted()
        && !volatile.present(mon)
    {
        if super::events::immunity_event(battle, target, id) != Some(true) {
            return Some(false);
        }
        let mut ev = super::events::EventCtx::new(
            pikapro_data::Event::TryAddVolatile,
            super::events::Target::Pokemon(target),
            Some(source.unwrap_or(target)),
            effect,
            super::events::Relay::Id(id),
        );
        match super::events::run_event(battle, &mut ev, None) {
            super::events::Relay::Null => return None,
            r if !r.is_truthy() => return Some(false),
            _ => {}
        }
    }
    // Helping Hand, Follow Me, Rage Powder (`order.rs`).
    if let Some(added) = super::order::add_volatile(battle, target, id, source) {
        return added;
    }
    // Gastro Acid (`itemmoves.rs`).
    if let Some(added) = super::itemmoves::add_volatile(battle, target, id, source) {
        return added;
    }
    let Some(mon) = battle.state.at(target) else { return Some(false) };
    // `if (!this.hp) return false`.
    if mon.is_fainted() {
        return Some(false);
    }
    // Volatiles implemented in `manipulation.rs`.
    if super::manipulation::volatile_of(id).is_some() {
        return super::manipulation::add_volatile(battle, target, id);
    }
    // `lockedmove`, `uproar` (`multiturn.rs`).
    if let Some(added) = super::multiturn::add_volatile(battle, target, id) {
        return Some(added);
    }
    let mon = battle.state.at(target).expect("checked above");
    let flag = match id {
        "flinch" => Volatiles::FLINCH,
        "confusion" => Volatiles::CONFUSION,
        "mustrecharge" => Volatiles::MUST_RECHARGE,
        "taunt" => Volatiles::TAUNT,
        "encore" => Volatiles::ENCORE,
        "disable" => Volatiles::DISABLE,
        // The volatiles of `conditions.rs`.
        _ => match conditions::flag_of(id) {
            Some(flag) => flag,
            None => return Some(false),
        },
    };
    // `if (linkedStatus && source && !source.hp) return false`: every
    // `trapped` is linked to its trapper (Jaw Lock that knocked out its
    // target, Mean Look, ...).
    if flag == Volatiles::TRAPPED && battle.state.at(source.unwrap_or(target)).is_none_or(|m| m.is_fainted()) {
        return Some(false);
    }
    // Already present: `onRestart`, which none of the volatiles above has.
    if mon.volatiles.has(flag) {
        return conditions::restart(battle, target, flag);
    }
    let lock = (mon.last_move, mon.moves);
    // `runStatusImmunity(id)`: the type (Ghosts can't be trapped), then
    // the `Immunity` event (Own Tempo, Oblivious, ...).
    if conditions::status_immune(battle, target, flag)
        || super::events::immunity_event(battle, target, id) != Some(true)
    {
        return Some(false);
    }
    // `TryAddVolatile`: Pokémon, side and terrain handlers share a sort.
    // Focus Punch's focus keeps it from flinching.
    let mut ev = super::events::EventCtx::new(
        pikapro_data::Event::TryAddVolatile,
        super::events::Target::Pokemon(target),
        Some(source.unwrap_or(target)),
        effect,
        super::events::Relay::Id(id),
    );
    match super::events::run_event(battle, &mut ev, None) {
        super::events::Relay::Null => return None,
        r if !r.is_truthy() => return Some(false),
        _ => {}
    }
    // An ability's volatile says so (`[from] ability: X|[of] <source>`).
    let from_ability = match effect {
        super::events::Effect::Ability(a) => {
            let of = actor(battle, source.unwrap_or(target));
            Some(crate::event::Cause::ability(a).of(of))
        }
        _ => None,
    };
    let started = match flag {
        Volatiles::FLINCH => true,
        Volatiles::CONFUSION => {
            // `onStart`: announce, then draw the number of checks.
            let a = actor(battle, target);
            // The end of a lock-in (Outrage) says `[fatigue]`.
            let fatigue = battle.active_move.is_some_and(|(_, m)| m == super::multiturn::LOCKED_MOVE_EFFECT);
            battle.emit(match from_ability {
                _ if fatigue => BattleEvent::FatigueConfusion { actor: a },
                Some(cause) => BattleEvent::StartFrom { actor: a, what: "confusion", arg: None, cause },
                None => BattleEvent::VolatileStart { actor: a, volatile: Volatile::Confusion, move_id: None },
            });
            // `sourceEffect.id === 'axekick'`: the effect, not the move
            // being used (a confusion berry eaten during Axe Kick).
            let axe_kick = effect == super::events::Effect::Move(ids::AXE_KICK);
            let min = if axe_kick { 3 } else { 2 };
            let time = battle.rng.random_range(min, 6) as u8;
            mon_mut(battle, target).volatile_counters.confusion = time;
            true
        }
        Volatiles::MUST_RECHARGE => {
            mon_mut(battle, target).volatile_counters.must_recharge = 2;
            let a = actor(battle, target);
            battle.emit(BattleEvent::MustRecharge { actor: a });
            true
        }
        Volatiles::TAUNT => {
            // `onStart`: a Pokémon that has been in for a turn and has
            // already moved keeps the taunt one more turn.
            let active_turns = mon_mut(battle, target).active_turns;
            let extend = active_turns > 0 && !queue::will_move(battle, target);
            mon_mut(battle, target).volatile_counters.taunt = if extend { 4 } else { 3 };
            let a = actor(battle, target);
            battle.emit(BattleEvent::VolatileStart { actor: a, volatile: Volatile::Taunt, move_id: None });
            true
        }
        Volatiles::ENCORE => {
            // `onStart`: fails without a last move that has PP left and
            // can be encored.
            let (last_move, moves) = lock;
            let encored = last_move.filter(|&m| {
                !battle.format.dex.move_(m).flags.contains(MoveFlags::FAIL_ENCORE)
                    && moves.iter().any(|s| s.id == m && s.pp > 0)
            });
            match encored {
                None => false,
                Some(m) => {
                    let counters = &mut mon_mut(battle, target).volatile_counters;
                    counters.encore = 3;
                    counters.encore_move = m;
                    let a = actor(battle, target);
                    battle.emit(BattleEvent::VolatileStart { actor: a, volatile: Volatile::Encore, move_id: None });
                    let queued = queue::will_move_action(battle, target).map(|(_, a)| a);
                    match queued {
                        None => mon_mut(battle, target).volatile_counters.encore = 4,
                        // Champions: a different queued move is replaced
                        // by the encored one right away.
                        Some(action) if battle.format.mechanics == Mechanics::Champions => {
                            change_to_encored_move(battle, target, action, m);
                        }
                        Some(_) => {}
                    }
                    true
                }
            }
        }
        Volatiles::DISABLE => {
            // `onStart`: one turn shorter if the target still moves this
            // turn or is using its own move (not a Dancer copy:
            // `!activeMove.isExternal`); fails without a last move, or if
            // it is out of PP.
            let user_is_target = battle.active_move.is_some_and(|(u, _)| u == target) && !battle.move_state.is_external;
            let duration = if queue::will_move(battle, target) || user_is_target { 4 } else { 5 };
            let (last_move, moves) = lock;
            let disabled = last_move.filter(|&m| moves.iter().all(|s| s.id != m || s.pp > 0));
            match disabled {
                None => false,
                Some(m) => {
                    let counters = &mut mon_mut(battle, target).volatile_counters;
                    counters.disable = duration;
                    counters.disable_move = m;
                    let a = actor(battle, target);
                    let event = match from_ability {
                        Some(cause) => {
                            let arg = Some(crate::event::LineArg::Move(m));
                            BattleEvent::StartFrom { actor: a, what: "Disable", arg, cause }
                        }
                        None => BattleEvent::VolatileStart { actor: a, volatile: Volatile::Disable, move_id: Some(m) },
                    };
                    battle.emit(event);
                    true
                }
            }
        }
        _ => conditions::start(battle, target, flag, source, from_ability)?,
    };
    if started {
        mon_mut(battle, target).volatiles.add(flag);
    }
    Some(started)
}

/// Showdown's `removeVolatile`: removes the volatile and runs its `onEnd`
/// (the `-end` message). Returns whether it was present.
pub(crate) fn remove_volatile(battle: &mut Battle, pos: Position, flag: u128) -> bool {
    let Some(mon) = battle.state.at_mut(pos) else { return false };
    if mon.is_fainted() || !mon.volatiles.has(flag) {
        return false;
    }
    mon.volatiles.clear(flag);
    let volatile = match flag {
        Volatiles::CONFUSION => Some(Volatile::Confusion),
        Volatiles::TAUNT => Some(Volatile::Taunt),
        Volatiles::ENCORE => Some(Volatile::Encore),
        Volatiles::DISABLE => Some(Volatile::Disable),
        _ => None,
    };
    if let Some(volatile) = volatile {
        let a = actor(battle, pos);
        battle.emit(BattleEvent::VolatileEnd { actor: a, volatile });
    }
    true
}

fn mon_mut(battle: &mut Battle, pos: Position) -> &mut crate::state::PokemonState {
    battle.state.at_mut(pos).expect("volatile holder is active")
}

/// The Champions mod's Encore `onStart`: `queue.changeAction` to the
/// encored move (untargeted, so re-aimed at random), keeping the action's
/// `order` (201 after Quash) and its priority adjusted by the difference in
/// move priorities (`data/mods/champions/moves.js`, `encore.condition.onStart`).
fn change_to_encored_move(battle: &mut Battle, pos: Position, action: Action, encored: MoveId) {
    let ActionKind::Move { move_id, .. } = action.kind else { return };
    // Not while it holds Mental Herb (which is about to end the Encore).
    if move_id == encored || super::items::has_item(battle, pos, pikapro_data::gen9::items::MENTAL_HERB) {
        return;
    }
    let priority = action.priority - queued_move(battle, move_id).0 + queued_move(battle, encored).0;
    let mon = action.mon.expect("move actions have a Pokémon");
    // `cancelMove`, then `insertChoice`.
    battle.queue.retain(|a| !(matches!(a.kind, ActionKind::Move { .. }) && a.mon == Some(mon)));
    let slot = battle.state.at(pos).and_then(|m| m.moves.iter().position(|s| s.id == encored)).unwrap_or(0) as u8;
    let kind = ActionKind::Move { slot, move_id: encored, target_loc: 0, gimmick: None };
    // `order: action.order`, which `resolveAction` keeps.
    queue::insert_choice(battle, Action { order: action.order, ..Action::new(kind, Some(mon)) }, false);
    if let Some((i, _)) = queue::will_move_action(battle, pos) {
        battle.queue.as_mut_slice()[i].priority = priority;
    }
}

/// The move's `onTry` (Fake Out, First Impression: only on the first move
/// action since switching in). False fails the move.
pub(crate) fn try_move(battle: &mut Battle, user: Position, move_id: MoveId) -> bool {
    if move_id == ids::FAKE_OUT || move_id == ids::FIRST_IMPRESSION {
        let first = battle.state.at(user).is_some_and(|m| m.active_move_actions <= 1);
        if !first {
            battle.hint(crate::event::Hint::FirstTurnOnly(move_id));
        }
        return first;
    }
    conditions::try_move(battle, user, move_id)
}

/// The move's own `onTryHit`, run by `spreadMoveHit` on its first target
/// (Disable fails against a Pokémon that has not moved or last used
/// Struggle). On failure logs `-fail` and the hit does nothing.
pub(crate) fn try_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    if move_id == ids::DISABLE {
        let last = battle.state.at(target).and_then(|m| m.last_move);
        if last.is_none_or(|m| m == ids::STRUGGLE) {
            moves::fail_still(battle, user);
            return false;
        }
    }
    if conditions::handles(move_id) && battle.state.at(target).is_some() {
        return conditions::try_hit(battle, target, user, move_id);
    }
    true
}

/// Encore's `onOverrideAction`: a Pokémon encored after choosing another
/// move uses the encored move instead. Returns its move slot and ID.
pub(crate) fn override_action(battle: &Battle, user: Position, move_id: MoveId) -> Option<(u8, MoveId)> {
    let mon = battle.state.at(user)?;
    if !mon.volatiles.has(Volatiles::ENCORE) {
        return None;
    }
    let encored = mon.volatile_counters.encore_move;
    if encored == move_id {
        return None;
    }
    let slot = mon.moves.iter().position(|s| s.id == encored)?;
    Some((slot as u8, encored))
}

/// `endTurn`'s move disabling: clears every active Pokémon's disabled
/// moves, then runs `DisableMove` (taunt, encore, disable, and
/// `conditions.rs`' torment, heal block, throat chop and the foes'
/// imprison), the moves' own `onDisableMove` (Champions' Fake Out and First
/// Impression) and disables a `cantusetwice` move used last. Then, per Pokémon as
/// Showdown's `endTurn` does, the `TrapPokemon` event.
pub(crate) fn disable_moves(battle: &mut Battle, pos: Position) {
    const OWN: u128 = Volatiles::TAUNT | Volatiles::ENCORE | Volatiles::DISABLE | conditions::DISABLE_MOVE;
    let Some(mon) = battle.state.at(pos) else { return };
    // `runEvent('DisableMove')` speed-sorts its handlers: the
    // Pokémon's own tie, so two or more draw a shuffle, and a foe's
    // Imprison ties with them at equal speed.
    // The foes' Imprison is collected before the user's registry handlers
    // here, not after them as `findEventHandlers` does: `DisableMove`
    // handlers only disable moves, so their order changes neither the
    // result nor the number of draws.
    let mut handlers = super::handlers::MonHandlers::new();
    for v in mon.volatiles.in_order().filter(|&v| v & OWN != 0) {
        let (speed, holder) = (mon.speed, Holder::Pokemon(pos));
        handlers.push(Handler {
            volatile_bit: vol(v),
            speed,
            sub_order: sub_order::CONDITION,
            holder,
            ..Handler::default()
        });
    }
    conditions::disable_move_handlers(battle, pos, &mut handlers);
    // The abilities', items' and registry volatiles' (`choicelock`)
    // `onDisableMove`, sorted with them.
    let event = pikapro_data::Event::DisableMove;
    super::events::collect_into(battle, event, super::events::Target::Pokemon(pos), None, &mut handlers);
    speed_sort(&mut handlers, &mut battle.rng, Handler::compare);
    let mut disabled = [false; crate::state::MAX_MOVES];
    let count = battle.state.at(pos).expect("checked above").moves.len();
    // Both only read: a foe's Imprison is looked for once.
    let imprison = conditions::foe_imprisons(battle, pos);
    let visible = visibly_disabled_moves(battle, pos);
    for (i, d) in disabled.iter_mut().enumerate().take(count) {
        *d = visible & (1 << i) != 0 || imprison && conditions::imprisoned(battle, pos, i);
    }
    let mon = battle.state.at_mut(pos).expect("checked above");
    for (m, &d) in mon.moves.iter_mut().zip(&disabled) {
        m.disabled = d;
    }
    mon.effect_disabled = 0;
    // The event framework's handlers disable more moves
    // (`events::disable_move`); the others only took part in the sort.
    let mut ev = super::events::EventCtx::new(
        pikapro_data::Event::DisableMove,
        super::events::Target::Pokemon(pos),
        None,
        super::events::Effect::None,
        super::events::Relay::True,
    );
    super::events::run_list(battle, &mut ev, &handlers);
}

/// Whether a `DisableMove` handler other than Imprison disables the
/// Pokémon's move `index` (Showdown's `disableMove(id)`, which the request
/// shows; Imprison's is hidden from the last active Pokémon's request).
fn visibly_disabled(battle: &Battle, pos: Position, index: usize) -> bool {
    let mon = battle.state.at(pos).expect("disabling moves of an active Pokémon");
    if !mon.volatiles.has(VISIBLE_DISABLES) && !field::gravity(battle) {
        let disabled = own_or_twice_disabled(battle, mon, mon.moves[index].id);
        debug_assert_eq!(disabled, visibly_disabled_full(battle, pos, index), "visibly_disabled's short path");
        return disabled;
    }
    visibly_disabled_full(battle, pos, index)
}

/// Without Taunt, Encore, Disable, Torment, Heal Block, Throat Chop
/// (`conditions::disabled_by`) and Gravity, the terms of
/// [`visibly_disabled_full`] that need them are false: what is left is
/// the move's own disable and `cantusetwice` ([`own_or_twice_disabled`]).
const VISIBLE_DISABLES: u128 = Volatiles::TAUNT
    | Volatiles::ENCORE
    | Volatiles::DISABLE
    | Volatiles::TORMENT
    | Volatiles::HEAL_BLOCK
    | Volatiles::THROAT_CHOP;

/// [`visibly_disabled`]'s terms left without [`VISIBLE_DISABLES`] and
/// Gravity.
#[inline(always)]
fn own_or_twice_disabled(battle: &Battle, mon: &crate::state::PokemonState, id: MoveId) -> bool {
    super::itemmoves::own_disable_move(&battle.format, mon, id)
        || mon.last_move == Some(id) && battle.format.dex.move_(id).flags.contains(MoveFlags::CANT_USE_TWICE)
}

/// [`visibly_disabled`], every term.
fn visibly_disabled_full(battle: &Battle, pos: Position, index: usize) -> bool {
    let mon = battle.state.at(pos).expect("disabling moves of an active Pokémon");
    let m = mon.moves[index];
    let (v, counters) = (mon.volatiles, mon.volatile_counters);
    let data = battle.format.dex.move_(m.id);
    let encore = v.has(Volatiles::ENCORE) && mon.moves.iter().any(|s| s.id == counters.encore_move);
    v.has(Volatiles::TAUNT) && data.category == MoveCategory::Status && m.id != ids::ME_FIRST
        || encore && m.id != counters.encore_move
        || v.has(Volatiles::DISABLE) && m.id == counters.disable_move
        || conditions::disabled_by(battle, pos, m.id)
        // Belch, Stuff Cheeks; the Champions mod's Fake Out, First Impression.
        || super::itemmoves::own_disable_move(&battle.format, mon, m.id)
        || data.flags.contains(MoveFlags::CANT_USE_TWICE) && mon.last_move == Some(m.id)
        // Gravity's `onDisableMove` (`field::end_turn`).
        || field::gravity(battle) && data.flags.contains(MoveFlags::GRAVITY)
}

/// Whether the move request shows move `index` as usable (`getMoves` with
/// `restrictData` for the last active Pokémon): PP left and not disabled,
/// except by Imprison. A choice of it still means Struggle when every move
/// is disabled.
pub(crate) fn request_shows_usable(battle: &Battle, pos: Position, index: usize) -> bool {
    let mon = battle.state.at(pos).expect("a request for an active Pokémon");
    mon.moves[index].pp > 0 && !visibly_disabled(battle, pos, index) && mon.effect_disabled & (1 << index) == 0
}

/// [`request_shows_usable`] of every move of the Pokémon at `pos`, bit `i`
/// for move `i` (each move request: one per active slot): the Pokémon, its
/// volatiles and Gravity are looked at once, not per move.
pub(crate) fn request_shows_usable_moves(battle: &Battle, pos: Position) -> u8 {
    let mon = battle.state.at(pos).expect("a request for an active Pokémon");
    let disabled = visibly_disabled_moves(battle, pos);
    let mut usable = 0;
    for (i, m) in mon.moves.iter().enumerate() {
        if m.pp > 0 && disabled & (1 << i) == 0 && mon.effect_disabled & (1 << i) == 0 {
            usable |= 1 << i;
        }
        debug_assert_eq!(usable & (1 << i) != 0, request_shows_usable(battle, pos, i), "request_shows_usable_moves");
    }
    usable
}

/// [`visibly_disabled`] of every move of the Pokémon at `pos`, bit `i` for
/// move `i`: the Pokémon, its volatiles and Gravity are looked at once.
fn visibly_disabled_moves(battle: &Battle, pos: Position) -> u8 {
    let mon = battle.state.at(pos).expect("disabling moves of an active Pokémon");
    let short = !mon.volatiles.has(VISIBLE_DISABLES) && !field::gravity(battle);
    let mut disabled = 0;
    for (i, m) in mon.moves.iter().enumerate() {
        let d = if short { own_or_twice_disabled(battle, mon, m.id) } else { visibly_disabled_full(battle, pos, i) };
        disabled |= u8::from(d) << i;
        debug_assert_eq!(d, visibly_disabled(battle, pos, i), "visibly_disabled_moves");
    }
    disabled
}

/// `onBeforeMove` handlers of the user's volatiles.
pub(crate) fn before_move_handlers<const N: usize>(
    battle: &Battle,
    user: Position,
    handlers: &mut super::handlers::SpillList<N>,
) {
    let Some(mon) = battle.state.at(user) else { return };
    let base = Handler {
        speed: mon.speed,
        sub_order: sub_order::CONDITION,
        holder: Holder::Pokemon(user),
        ..Handler::default()
    };
    let v = mon.volatiles;
    if v.has(Volatiles::MUST_RECHARGE) {
        handlers.push(Handler {
            volatile_bit: vol(Volatiles::MUST_RECHARGE),
            priority: 11,
            callback: Callback::Relay(recharge_before_move),
            ..base
        });
    }
    if v.has(Volatiles::FLINCH) {
        handlers.push(Handler {
            volatile_bit: vol(Volatiles::FLINCH),
            priority: 8,
            callback: Callback::Plain(flinch_before_move),
            ..base
        });
    }
    if v.has(Volatiles::DISABLE) {
        handlers.push(Handler {
            volatile_bit: vol(Volatiles::DISABLE),
            priority: 7,
            callback: Callback::Plain(disable_before_move),
            ..base
        });
    }
    if v.has(Volatiles::TAUNT) {
        handlers.push(Handler {
            volatile_bit: vol(Volatiles::TAUNT),
            priority: 5,
            callback: Callback::Plain(taunt_before_move),
            ..base
        });
    }
    if v.has(Volatiles::CONFUSION) {
        handlers.push(Handler {
            volatile_bit: vol(Volatiles::CONFUSION),
            priority: 3,
            callback: Callback::Plain(confusion_before_move),
            ..base
        });
    }
    conditions::before_move_handlers(battle, user, handlers);
}

fn holder(handler: &Handler) -> Position {
    match handler.holder {
        Holder::Pokemon(pos) => pos,
        _ => unreachable!("volatile handlers are held by a Pokémon"),
    }
}

fn active_move_id(battle: &Battle) -> MoveId {
    battle.active_move.expect("BeforeMove runs for the active move").1
}

/// `mustrecharge`: `cant|recharge`, then the volatile ends silently.
/// `return null`, where every other `BeforeMove` handler that stops a move
/// returns `false`: the Pokémon had nothing else to choose, so the turn
/// doesn't count as a failed move (`run_move`).
fn recharge_before_move(
    battle: &mut Battle,
    _: &mut super::events::EventCtx<'_>,
    handler: &Handler,
) -> Option<super::events::Relay> {
    let pos = holder(handler);
    let a = actor(battle, pos);
    battle.emit(BattleEvent::Cant { actor: a, reason: CantReason::Recharge });
    remove_volatile(battle, pos, Volatiles::MUST_RECHARGE);
    // `removeVolatile('truant')`.
    super::events::remove_volatile_flag(battle, pos, Volatiles::TRUANT);
    Some(super::events::Relay::Null)
}

/// `flinch`: `cant|flinch`, then `runEvent('Flinch')` (Steadfast). The
/// volatile stays until the end of the turn.
fn flinch_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    let a = actor(battle, pos);
    battle.emit(BattleEvent::Cant { actor: a, reason: CantReason::Flinch });
    super::run_mon_event(battle, pikapro_data::Event::Flinch, pos);
    false
}

/// Disable: the disabled move can't be used. Champions' `disable` condition
/// (`onBeforeMove`, `data/mods/champions/moves.ts`) lets a
/// `cantusetwice` move (Gigaton Hammer) through; `onDisableMove` still
/// disables it in the request.
fn disable_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    let move_id = active_move_id(battle);
    let exempt = battle.format.mechanics == crate::format::Mechanics::Champions
        && battle.format.dex.moves.get(move_id.0 as usize).is_some_and(|m| m.flags.contains(MoveFlags::CANT_USE_TWICE));
    let disabled = !exempt && battle.state.at(pos).is_some_and(|m| m.volatile_counters.disable_move == move_id);
    if disabled {
        let a = actor(battle, pos);
        battle.emit(BattleEvent::Cant { actor: a, reason: CantReason::Disable(move_id) });
    }
    !disabled
}

/// Taunt: status moves (except Me First) can't be used.
fn taunt_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    let move_id = active_move_id(battle);
    if move_id == RECHARGE
        || move_id == ids::ME_FIRST
        || battle.format.dex.move_(move_id).category != MoveCategory::Status
    {
        return true;
    }
    let a = actor(battle, holder(handler));
    battle.emit(BattleEvent::Cant { actor: a, reason: CantReason::Taunt(move_id) });
    false
}

/// Confusion: count down (ending it at zero), otherwise `-activate` and a
/// 33% chance to hit itself instead of moving.
fn confusion_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    let mon = mon_mut(battle, pos);
    mon.volatile_counters.confusion = mon.volatile_counters.confusion.saturating_sub(1);
    if mon.volatile_counters.confusion == 0 {
        remove_volatile(battle, pos, Volatiles::CONFUSION);
        return true;
    }
    let a = actor(battle, pos);
    battle.emit(BattleEvent::VolatileActivate { actor: a, volatile: Volatile::Confusion });
    if !battle.rng.chance(33, 100) {
        return true;
    }
    // `this.activeTarget = pokemon` (Unaware then leaves its stages alone).
    battle.move_state.active_target = Some(pos);
    let damage = confusion_damage(battle, pos, 40);
    apply_damage(battle, pos, damage, DamageSource::Confusion);
    false
}

/// Showdown's `getConfusionDamage`: a typeless 40 BP physical hit on
/// itself, with stat stages but no other modifiers, and a damage roll.
fn confusion_damage(battle: &mut Battle, pos: Position, base_power: u32) -> u32 {
    let mon = *battle.state.at(pos).expect("confused Pokémon is active");
    // `calculateStat(stat, boost)`: Wonder Room swaps the defences, and the
    // stages go through `ModifyBoost` (Unaware).
    let attack = super::events::calculate_stat(battle, pos, Stat::Atk, mon.boosts.atk, pos);
    let defense = super::events::calculate_stat(battle, pos, Stat::Def, mon.boosts.def, pos);
    let level = mon.level as u32;
    let base = (2 * level / 5 + 2) * base_power * attack / defense / 50 + 2;
    // `tr(baseDamage, 16)` (not in custom games: `Math.trunc`), then the
    // `randomizer` roll.
    let base = if battle.format.cartridge_overflow { base % (1 << 16) } else { base };
    let damage = base * (100 - battle.rng.random(16)) / 100;
    damage.max(1)
}

/// End-of-turn handlers: volatiles with a duration count down (Showdown's
/// `fieldEvent('Residual')` includes every volatile with a `duration`).
pub(crate) fn residual_handlers(battle: &Battle, handlers: &mut Handlers) {
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            let pos = Position::new(side, slot as u8);
            let Some(mon) = battle.state.at(pos) else { continue };
            // None of the five below: nothing to collect.
            const ANY: u128 = Volatiles::FLINCH
                | Volatiles::MUST_RECHARGE
                | Volatiles::TAUNT
                | Volatiles::ENCORE
                | Volatiles::DISABLE;
            if !mon.volatiles.has(ANY) {
                continue;
            }
            let base = Handler {
                speed: mon.speed,
                sub_order: sub_order::CONDITION,
                holder: Holder::Pokemon(pos),
                ..Handler::default()
            };
            let v = mon.volatiles;
            if v.has(Volatiles::FLINCH) {
                handlers.push(Handler {
                    volatile_bit: vol(Volatiles::FLINCH),
                    callback: Callback::Plain(end_flinch),
                    ..base
                });
            }
            if v.has(Volatiles::MUST_RECHARGE) {
                handlers.push(Handler {
                    volatile_bit: vol(Volatiles::MUST_RECHARGE),
                    callback: Callback::Plain(tick_recharge),
                    ..base
                });
            }
            if v.has(Volatiles::TAUNT) {
                handlers.push(Handler {
                    volatile_bit: vol(Volatiles::TAUNT),
                    order: Some(15),
                    callback: Callback::Plain(tick_taunt),
                    ..base
                });
            }
            if v.has(Volatiles::ENCORE) {
                handlers.push(Handler {
                    volatile_bit: vol(Volatiles::ENCORE),
                    order: Some(16),
                    callback: Callback::Plain(tick_encore),
                    ..base
                });
            }
            if v.has(Volatiles::DISABLE) {
                handlers.push(Handler {
                    volatile_bit: vol(Volatiles::DISABLE),
                    order: Some(17),
                    callback: Callback::Plain(tick_disable),
                    ..base
                });
            }
        }
    }
    conditions::residual_handlers(battle, handlers);
}

/// Counts down a duration; at zero the volatile ends. Returns whether it
/// is still up (so `onResidual` runs and faints are reported after it).
fn tick(
    battle: &mut Battle,
    pos: Position,
    flag: u128,
    counter: fn(&mut crate::state::VolatileCounters) -> &mut u8,
) -> bool {
    let Some(mon) = battle.state.at_mut(pos) else { return false };
    if !mon.volatiles.has(flag) {
        return false;
    }
    let turns = counter(&mut mon.volatile_counters);
    *turns = turns.saturating_sub(1);
    if *turns == 0 {
        remove_volatile(battle, pos, flag);
        return false;
    }
    true
}

fn end_flinch(battle: &mut Battle, handler: &Handler) -> bool {
    // Duration 1: always ends (so no `faintMessages` after it).
    remove_volatile(battle, holder(handler), Volatiles::FLINCH);
    false
}

fn tick_recharge(battle: &mut Battle, handler: &Handler) -> bool {
    tick(battle, holder(handler), Volatiles::MUST_RECHARGE, |c| &mut c.must_recharge)
}

fn tick_taunt(battle: &mut Battle, handler: &Handler) -> bool {
    tick(battle, holder(handler), Volatiles::TAUNT, |c| &mut c.taunt)
}

/// Encore also ends once the encored move runs out of PP.
fn tick_encore(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    if !tick(battle, pos, Volatiles::ENCORE, |c| &mut c.encore) {
        return false;
    }
    let mon = battle.state.at(pos).expect("checked by tick");
    let encored = mon.volatile_counters.encore_move;
    if !mon.moves.iter().any(|s| s.id == encored && s.pp > 0) {
        remove_volatile(battle, pos, Volatiles::ENCORE);
    }
    true
}

fn tick_disable(battle: &mut Battle, handler: &Handler) -> bool {
    tick(battle, holder(handler), Volatiles::DISABLE, |c| &mut c.disable)
}
