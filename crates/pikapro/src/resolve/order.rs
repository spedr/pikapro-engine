//! Action order, targeting and move calling.
//!
//! Showdown references: `prioritizeAction` and `willMove` in
//! `sim/battle-queue.ts`; `getTarget` (`tracksTarget`, `smartTarget`) and
//! `swapPosition` in `sim/battle.ts`; `getMoveTargets` (`RedirectTarget`,
//! `getSmartTargets`), `deductPP`, `transformInto` and `copyVolatileFrom` in
//! `sim/pokemon.ts`; `useMove` and `hitStepMoveHitLoop` in
//! `sim/battle-actions.ts`; and the moves After You, Quash, Instruct,
//! Round, Ally Switch, Helping Hand, Follow Me, Rage Powder, Dragon Darts,
//! Snipe Shot, Sucker Punch, Thunderclap, Upper Hand, Last Resort,
//! Metronome, Copycat, Sleep Talk, Snore, Baton Pass, Pollen Puff,
//! Transform, Sketch, Mimic and Spite in `data/moves.ts`.
//!
//! Contract (hooks called by the shared pipeline at Showdown's call sites):
//! - queue: [`set_original_target`] (`resolveAction`), [`get_target`]
//!   (`getActionSpeed`, `runMove`), [`RunMoveOptions`] (a move action's
//!   `originalTarget` and `sourceEffect`).
//! - `runMove`: [`set_active_move`], [`deduct_pp`] (`cant|...|nopp`),
//!   [`record_last_move`] (`battle.lastMove`).
//! - `useMove`: [`move_line`] (`[from]` and `null` targets),
//!   [`call_move`] (moves called by Metronome, Copycat, Sleep Talk).
//! - `getMoveTargets`: [`redirect_and_smart_targets`].
//! - hit steps: [`on_try`], [`prepare_hit`], [`try_hit`], [`on_hit`],
//!   [`smart_target`], [`after_hit_step`], [`smart_target_missed`],
//!   [`hit_targets`], [`sleep_usable`], [`deals_damage`], [`has_self`].
//! - damage: [`base_power_callback`] (Round), [`base_power_modifier`]
//!   (Helping Hand).
//! - volatiles: [`add_volatile`], [`residual_handlers`].
//! - switching: [`passed_volatiles`] / [`copy_volatiles`] (Baton Pass).
//!
//! State: volatile bits 50–55 and their
//! [`VolatileCounters`](crate::state::VolatileCounters) fields,
//! [`MoveRecord::used_moves`](crate::state::MoveRecord::used_moves), and
//! [`OrderState`] on the battle.

use pikapro_data::gen9::{moves as ids, species};
use pikapro_data::{
    AbilityFlags, Dex, MoveCategory, MoveData, MoveFlags, MoveId, MoveTarget, Nonstandard, Stat, TypeSet,
};

use super::events::is_active;
use super::handlers::{Callback, Handler, Handlers, Holder, sub_order, vol};
use super::moves::{self, Outcome};
use super::queue::{self, Action, ActionKind, MonRef};
use super::targeting::{self, Loc, Targets};
use super::{HealSource, actor, apply_heal, position_of_mon, switching};
use crate::battle::Battle;
use crate::event::{Actor, BattleEvent, FailNote, MoveFrom};
use crate::state::{Boosts, MoveSlot, PokemonState, Position, SideId, Status, TransformBase, Volatiles};
use crate::stats::calc_stat;
use crate::util::StackVec;

/// Battle-wide state of this module.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct OrderState {
    /// Showdown's `battle.lastMove`: the last move used by anyone, recorded
    /// when a move action ends (Copycat).
    pub last_move: Option<MoveId>,
    /// The active move's `sourceEffect`: the move that called it (Sleep
    /// Talk, Metronome, Copycat) or moved it up (Round).
    pub source_effect: Option<MoveId>,
    /// The active move's `smartTarget` (Dragon Darts), switched off by a
    /// redirection or a failed hit step.
    pub smart_target: bool,
    /// Showdown's `effectOrder` counter, for the ability states it orders
    /// (`VolatileCounters::ability_order`).
    pub effect_order: u16,
    /// The active move's priority when it is not its own: a called move
    /// has its caller's (`move.priority = activeMove.priority` in
    /// `useMove`), a move forced by Encore the chosen move's (`runMove`).
    pub priority_override: Option<i8>,
    /// A Pokémon revived in its own slot (Revival Blessing) while its
    /// `instaswitch` calls it back: Showdown's `oldActive` is the Pokémon
    /// switching in, whose `isActive` is still false, so its switch-out
    /// messages name it `p1: X`.
    pub revived_switching: Option<Position>,
    /// A Pokémon revived in its own slot while its `instaswitch` is
    /// resolved (`addChoice`'s `resolveAction`, which computes its speed)
    /// and before it is queued: not `isActive`, as while it waits
    /// (`slots::off_field`, `slots::revived_waiting`).
    pub revived_resolving: Option<Position>,
    /// Revival Blessing has revived a Pokémon in its slot this battle (its
    /// `instaswitch` makes a Pokémon off the field while it stands there:
    /// `slots::off_field`). Until then that never needs checking.
    pub revived_in_slot: bool,
    /// The waiting pledge's `ignoreAbility` (its user's Mold Breaker),
    /// which the action it moved up takes (`move.ignoreAbility =
    /// sourceEffect.ignoreAbility` in `useMoveInner`).
    pub pledge_ignores_ability: bool,
    /// The `ignoreAbility` of the move that moved the active move's action
    /// up ([`Action::source_ignores_ability`]).
    pub source_ignores_ability: bool,
    /// The Pokémon (side, `PokemonState::id`) whose move action is running a
    /// locked move: `runMove` makes its `sourceEffect` the `lockedmove`
    /// condition, whatever the action's own was. Decided once, when the
    /// action starts (a two-turn move it starts later doesn't change it).
    pub locked: Option<(SideId, u8)>,
    /// Per side and slot, the `PokemonState::id` of a fainted Pokémon that
    /// Showdown's `side.active[slot]` still holds after a stale switch took
    /// it to the other slot: it came in early (an Eject Pack switch) and
    /// fainted, and the switch queued for it at the start of the turn
    /// brought it in again (`switchIn` checks only `isActive`, and updates
    /// only the slot it comes into). What reads `side.active` with the
    /// fainted Pokémon (`runSwitch`'s `getAllActive(true)`, `fieldEvent`)
    /// then sees it in both slots ([`super::field_index`]), and its slot
    /// holds no live Pokémon (targeting). It lasts until a switch into that
    /// slot. Showdown throws at the end of that turn's forced switches
    /// (`Invalid switch position`, or a `TypeError` once the fainted
    /// Pokémon is on the bench), so nothing after the turn is compared.
    pub stale_active: [[Option<u8>; crate::state::MAX_ACTIVE]; 2],
    /// The slots of `stale_active` that are set: bit `2 * side + slot`.
    pub stale_slots: u8,
    /// The move a Metronome, Copycat, Sleep Talk or Magic Bounce called, as its
    /// `useMove` left it ([`CalledMove`]). Taken by `run_move` after
    /// `useMove`.
    pub called_move: Option<CalledMove>,
    /// `move.hitTargets` as Magician's `onAfterMoveSecondarySelf` left it:
    /// its `speedSort` sorts the array in place, and the move's
    /// `AfterMove` (Sparkling Aria's cures) goes through it in that order.
    /// Packed in nibbles of `1 + side * 2 + slot`: the Magician holder in
    /// the lowest, then the (at most three) targets in order; 0 when there
    /// is none. Set for a Sparkling Aria only, taken by its `AfterMove`
    /// (`manipulation::after_move`).
    pub hit_order: u16,
}

/// The fields of a called move's `ActiveMove` that `AfterMove` reads. It
/// is `battle.activeMove` from then on, which the caller's `runMove` takes
/// as the move for `AfterMove` (`if (this.battle.activeMove) move =
/// this.battle.activeMove`), while the rest of the caller's own hit has the
/// caller's fields ([`call_move`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CalledMove {
    /// Its type after `ModifyType` (Galvanize, Liquid Voice): Charge's
    /// `onAfterMove`.
    pub ty: Option<pikapro_data::Type>,
    /// `hasSheerForce`: Sparkling Aria's `onAfterMove`.
    pub has_sheer_force: bool,
    /// The global `battle.activeMove.ignoreAbility`. Suppression checks
    /// read this even while an outer move resumes with its own fields.
    pub ignore_ability: bool,
}

/// What `runAction` passes to `runMove` from a move action.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RunMoveOptions {
    /// Showdown's `originalTarget`.
    pub original_target: Option<MonRef>,
    /// Showdown's `action.sourceEffect` (set by `prioritizeAction`).
    pub source_effect: Option<MoveId>,
    /// Its `ignoreAbility` ([`Action::source_ignores_ability`]).
    pub source_ignores_ability: bool,
    /// `externalMove`, a move copied by Dancer: no
    /// `OverrideAction`, no PP, no lock and no `moveUsed`.
    pub external: bool,
    /// The action's priority, which Gen 6+ gives the
    /// move (`action.move.priority`, after `ModifyPriority`: Prankster,
    /// Gale Wings, Triage, Grassy Glide).
    pub priority: Option<i8>,
}

impl RunMoveOptions {
    pub fn of(action: &Action) -> RunMoveOptions {
        RunMoveOptions {
            original_target: action.original_target,
            source_effect: action.source_effect,
            source_ignores_ability: action.source_ignores_ability,
            external: false,
            priority: Some(action.priority),
        }
    }
}

/// Volatiles Baton Pass does not pass: Showdown's `noCopy` conditions
/// (Transform and Mimic are not volatiles there, and are not copied
/// either). Every Gen 9 one is here but `trapper` (no state here) and
/// `dynamax`.
const NO_COPY: u128 = Volatiles::ENCORE
    | Volatiles::DISABLE
    | Volatiles::TRANSFORMED
    | Volatiles::MIMIC
    // Showdown's `noCopy` conditions implemented in other modules:
    // `conditions.rs` (Attract, Destiny Bond, Glaive Rush, Imprison, Salt
    // Cure, Smack Down, Syrup Bomb, Torment, Yawn, `trapped`),
    // `manipulation.rs` (Defense Curl, Lock-On, Minimize, Stockpile),
    // `multiturn.rs` (Counter and Mirror Coat).
    | Volatiles::ATTRACT
    | Volatiles::DESTINY_BOND
    | Volatiles::GLAIVE_RUSH
    | Volatiles::IMPRISON
    | Volatiles::SALT_CURE
    | Volatiles::SMACK_DOWN
    | Volatiles::SYRUP_BOMB
    | Volatiles::TORMENT
    | Volatiles::YAWN
    | Volatiles::TRAPPED
    | Volatiles::DEFENSE_CURL
    | Volatiles::LOCK_ON
    | Volatiles::MINIMIZE
    | Volatiles::STOCKPILE
    | Volatiles::COUNTER
    | Volatiles::MIRROR_COAT
    // `choicelock`, and Commander's, Flash Fire's, Protosynthesis's and
    // Quark Drive's (the last three end with their ability first).
    | Volatiles::CHOICE_LOCK
    | Volatiles::COMMANDING
    | Volatiles::COMMANDED
    | Volatiles::FLASH_FIRE
    | Volatiles::PROTOSYNTHESIS
    | Volatiles::QUARK_DRIVE;

/// Follow Me and Rage Powder.
const REDIRECTION: u128 = Volatiles::FOLLOW_ME | Volatiles::RAGE_POWDER;

/// `move.tracksTarget` in the format's dex: Snipe Shot, and Champions'
/// Curse (Pursuit's and Sky Drop's are Past).
fn tracks_target(battle: &Battle, move_id: MoveId) -> bool {
    battle.format.dex.moves.get(move_id.0 as usize).is_some_and(data_tracks_target)
}

/// `move.tracksTarget`, from the data's `unmodelled` list.
fn data_tracks_target(data: &MoveData) -> bool {
    data.unmodelled.contains(&"tracksTarget")
}

/// `smartTarget` moves.
fn smart_target_move(move_id: MoveId) -> bool {
    move_id == ids::DRAGON_DARTS
}

/// `sleepUsable` moves.
fn sleep_usable_move(move_id: MoveId) -> bool {
    move_id == ids::SLEEP_TALK || move_id == ids::SNORE
}

fn mon_ref(battle: &Battle, pos: Position) -> MonRef {
    MonRef { side: pos.side, id: battle.state.side(pos.side).team[pos.slot as usize].id }
}

// ---------------------------------------------------------------------
// Queue and targeting.

/// `resolveAction`: `action.originalTarget = pokemon.getAtLoc(targetLoc)`.
pub(crate) fn set_original_target(battle: &Battle, action: &mut Action) {
    let (ActionKind::Move { target_loc, .. }, Some(mon)) = (action.kind, action.mon) else { return };
    let Some(user) = position_of_mon(battle, mon) else { return };
    action.original_target = targeting::at_loc(battle, user, target_loc).map(|p| mon_ref(battle, p));
}

/// Showdown's `getTarget` with its move-specific rules: a `tracksTarget`
/// move (Snipe Shot, Champions' Curse) follows its original target while it is active; a
/// `smartTarget` move (Dragon Darts) takes whatever stands at the location,
/// or a random target if that is empty or fainted.
pub(crate) fn get_target(
    battle: &mut Battle,
    user: Position,
    move_id: MoveId,
    target_type: MoveTarget,
    loc: Loc,
    original_target: Option<MonRef>,
) -> Option<Position> {
    // Stalwart and Propeller Tail track targets too. (The original target
    // first: without one, as in `getActionSpeed`, the checks, which only
    // read, need not run.)
    if let Some(pos) = original_target.and_then(|t| position_of_mon(battle, t)).filter(|&p| is_active(battle, p))
        && (tracks_target(battle, move_id) || super::abilities::misc::tracks_target(battle, user))
    {
        return Some(pos);
    }
    // A future move (Future Sight) aimed at the user's own slot targets the
    // user (`getTarget`'s self-location rule).
    let self_loc = -(user.slot as Loc + 1);
    // (`recharge` is not a dex move.)
    let future =
        || battle.format.dex.moves.get(move_id.0 as usize).is_some_and(|m| m.flags.contains(MoveFlags::FUTURE_MOVE));
    if loc == self_loc && future() {
        return Some(user);
    }
    if smart_target_move(move_id) {
        let current = targeting::at_loc(battle, user, loc).filter(|&p| is_active(battle, p));
        return current.or_else(|| targeting::get_random_target(battle, user, target_type));
    }
    targeting::get_target(battle, user, target_type, loc)
}

/// Showdown's `priorityEvent('RedirectTarget')` in `getMoveTargets` (the
/// foes' Follow Me and Rage Powder), then `getSmartTargets` for a
/// `smartTarget` move. Returns the target and, for a smart move hitting
/// two Pokémon, the second one.
///
/// Redirection handlers are sorted without drawing (`fastExit` uses
/// `compareRedirectOrder`): priority (1 for both), then the holder's
/// speed, ties staying in collection order (foes left to right).
pub(crate) fn redirect_and_smart_targets(
    battle: &mut Battle,
    user: Position,
    data: &MoveData,
    target: Position,
) -> (Position, Option<Position>) {
    let mut target = target;
    // Stalwart's and Propeller Tail's `tracksTarget`.
    let tracks = data_tracks_target(data) || battle.move_state.tracks_target;
    // `Pokemon.getMoveTargets`: every non-`tracksTarget` move is
    // redirected, a two-turn move on its charge turn too.
    if battle.format.active_per_side() > 1 && !tracks {
        // (Nor for Sky Drop targets: Sky Drop is Past in Gen 9 and Champions.)
        // `priorityEvent('RedirectTarget')`: the first handler that returns
        // a target wins, by priority: Follow Me and Rage Powder (1), the
        // abilities (0), then Counter's and Mirror Coat's own (-1).
        let mut redirected_by_ability = false;
        if let Some(redirected) = redirect_target(battle, user, data) {
            target = redirected;
            battle.order.smart_target = false;
        } else if let Some((_, move_id)) = super::active_dex_move(battle) {
            // The abilities' `onAnyRedirectTarget` (Lightning Rod, Storm
            // Drain; priority 0, after Follow Me's 1).
            let mut ev = super::events::EventCtx::new(
                pikapro_data::Event::RedirectTarget,
                super::events::Target::Pokemon(user),
                Some(user),
                super::events::Effect::Move(move_id),
                super::events::Relay::Pokemon(target),
            )
            .with_move(data);
            // (A handler that returns the same target also stops the
            // event; no handler that could, can meet Counter's.)
            if let super::events::Relay::Pokemon(t) = super::events::priority_event(battle, &mut ev) {
                redirected_by_ability = t != target;
                target = t;
            }
            if !redirected_by_ability && let Some(t) = super::multiturn::redirect_target(battle, user) {
                target = t;
            }
        }
    }
    if !battle.order.smart_target {
        return (target, None);
    }
    // `getSmartTargets`: the target's first adjacent ally.
    let side = battle.state.side(target.side);
    let second = (0..battle.format.active_per_side().min(side.team.len()))
        .map(|slot| Position::new(target.side, slot as u8))
        .find(|&p| p != target && is_active(battle, p) && is_active(battle, target));
    match second {
        Some(second) if second != user && battle.state.side(second.side).team[second.slot as usize].hp > 0 => {
            if battle.state.side(target.side).team[target.slot as usize].hp == 0 {
                battle.order.smart_target = false;
                (second, None)
            } else {
                (target, Some(second))
            }
        }
        _ => {
            battle.order.smart_target = false;
            (target, None)
        }
    }
}

/// The first foe whose Follow Me / Rage Powder takes the move.
fn redirect_target(battle: &mut Battle, user: Position, data: &MoveData) -> Option<Position> {
    let foe = user.side.foe();
    let mut holders = StackVec::<(Position, i32, u128), 2>::new();
    for slot in 0..battle.format.active_per_side() {
        let pos = Position::new(foe, slot as u8);
        let Some(mon) = battle.state.at(pos).filter(|m| !m.is_fainted()) else { continue };
        let v = mon.volatiles.bits() & REDIRECTION;
        if v != 0 {
            holders.push((pos, mon.speed, v));
        }
    }
    // `compareRedirectOrder`: faster holders first, then the one whose
    // ability state is older (switched in earlier).
    let order = |p: Position| battle.state.at(p).map_or(0, |m| m.volatile_counters.ability_order);
    if holders.len() == 2 && (holders[1].1, order(holders[0].0)) > (holders[0].1, order(holders[1].0)) {
        holders.swap(0, 1);
    }
    for &(pos, _, v) in holders.iter() {
        let valid = targeting::valid_target_loc(battle, targeting::loc_of(user, pos), user, data.target);
        if v & Volatiles::FOLLOW_ME != 0 && valid {
            return Some(pos);
        }
        // Rage Powder: `source.runStatusImmunity('powder')` (Grass types,
        // then the `Immunity` event: Overcoat, Safety Goggles).
        if v & Volatiles::RAGE_POWDER != 0 {
            let grass =
                battle.state.at(user).is_some_and(|m| super::manipulation::has_type(m, pikapro_data::Type::Grass));
            let powder_immune = grass || super::events::immunity_event(battle, user, "powder") != Some(true);
            if !powder_immune && valid {
                return Some(pos);
            }
        }
    }
    None
}

/// `initEffectState` for the Pokémon's `abilityState` (switching in,
/// Transform's `setAbility`): the next `effectOrder`.
pub(crate) fn new_ability_state(battle: &mut Battle, pos: Position) {
    battle.order.effect_order += 1;
    let order = battle.order.effect_order;
    if let Some(mon) = battle.state.at_mut(pos) {
        mon.volatile_counters.ability_order = order;
    }
}

/// Showdown's `swapPosition` (Ally Switch): `|swap|`, then the two
/// Pokémon trade team positions.
fn swap_position(battle: &mut Battle, user: Position, slot: u8, move_id: MoveId) {
    let a = actor(battle, user);
    battle.emit(BattleEvent::Swap { actor: a, slot, move_id });
    battle.state.side_mut(user.side).team.swap(user.slot as usize, slot as usize);
    // The move goes on from the user's new position.
    if let Some((pos, m)) = battle.active_move
        && pos == user
    {
        battle.active_move = Some((Position::new(user.side, slot), m));
    }
    // `runEvent('Swap', target, pokemon)`, then `('Swap', pokemon, target)`:
    // Healing Wish and Lunar Dance.
    super::slots::swap_event(battle, user);
    super::slots::swap_event(battle, Position::new(user.side, slot));
}

/// Showdown's `queue.prioritizeAction`: the action at `index` goes to the
/// front with order 3.
fn prioritize(battle: &mut Battle, index: usize, action: Action, source_effect: Option<MoveId>) {
    let mut i = 0;
    battle.queue.retain(|_| {
        let keep = i != index;
        i += 1;
        keep
    });
    // The active move is `sourceEffect`.
    let source_ignores_ability = source_effect.is_some() && battle.move_state.ignore_ability;
    battle.queue.insert(0, Action { order: 3, source_effect, source_ignores_ability, ..action });
}

// ---------------------------------------------------------------------
// runMove / useMove.

/// Whether the move action of the Pokémon at `user` runs a locked move
/// ([`OrderState::locked`]).
pub(crate) fn is_locked(battle: &Battle, user: Position) -> bool {
    battle.order.locked.is_some_and(|(side, id)| side == user.side && battle.state.at(user).is_some_and(|m| m.id == id))
}

/// The move about to be used becomes the active move.
/// `priority` is the chosen move's priority if Encore replaced it.
pub(crate) fn set_active_move(
    battle: &mut Battle,
    move_id: MoveId,
    source_effect: Option<MoveId>,
    priority: Option<i8>,
) {
    battle.order.priority_override = priority;
    battle.order.source_effect = source_effect;
    battle.order.smart_target = smart_target_move(move_id);
}

/// `runMove`'s `deductPP`: pays 1 PP for the move (by ID, like
/// `getMoveData`) and marks its slot used. Without PP left (Spite), logs
/// `cant|...|nopp` and returns false. Struggle costs nothing.
pub(crate) fn deduct_pp(battle: &mut Battle, user: Position, move_id: MoveId) -> bool {
    if move_id == ids::STRUGGLE {
        // Struggle only spends PP when it is in a move slot.
        deduct_pp_of(battle, user, move_id, 1);
        return true;
    }
    if deduct_pp_of(battle, user, move_id, 1) > 0 {
        return true;
    }
    let a = actor(battle, user);
    // `recharge` (and any other stand-in for a locked move) is no dex move:
    // the line names it without the dex.
    let move_id = (move_id.index() < battle.format.dex.moves.len()).then_some(move_id);
    battle.emit(BattleEvent::CantNoPp { actor: a, move_id });
    false
}

/// Showdown's `pokemon.deductPP(move, amount)` for extra PP (Pressure).
pub(crate) fn deduct_extra_pp(battle: &mut Battle, pos: Position, move_id: MoveId, amount: u8) {
    deduct_pp_of(battle, pos, move_id, amount);
}

/// Showdown's `pokemon.deductPP(move, amount)`: marks the slot used and
/// removes up to `amount` PP. Returns the PP removed.
fn deduct_pp_of(battle: &mut Battle, pos: Position, move_id: MoveId, amount: u8) -> u8 {
    let Some(mon) = battle.state.at_mut(pos) else { return 0 };
    let Some(index) = mon.moves.iter().position(|s| s.id == move_id) else { return 0 };
    mon.record.used_moves |= 1 << index;
    let slot = &mut mon.moves[index];
    let removed = amount.min(slot.pp);
    slot.pp -= removed;
    removed
}

/// `clearActiveMove()` at the end of a move action: `battle.lastMove`
/// becomes the active move (the called move, for Sleep Talk and the like).
pub(crate) fn record_last_move(battle: &mut Battle) {
    if let Some((_, move_id)) = battle.active_move {
        battle.order.last_move = Some(move_id);
    }
}

/// The `|move|` line: `[from] move: X` for a move with a source effect, and
/// Showdown's `null` target when there is none at all.
pub(crate) fn move_line(
    battle: &Battle,
    user: Actor,
    move_id: MoveId,
    target: Option<Actor>,
    from: Option<MoveFrom>,
) -> BattleEvent {
    let from = from.or(battle.order.source_effect.map(MoveFrom::Move));
    match target {
        None => BattleEvent::MoveNull { user, move_id, from },
        target => BattleEvent::Move { user, move_id, target, from },
    }
}

/// Showdown's `useMove(move, pokemon)` from a move's `onHit`: the called
/// move gets a random target (`getRandomTarget`, which draws in doubles)
/// and becomes the active move, with the caller as its source effect. No
/// PP is paid.
fn call_move(battle: &mut Battle, user: Position, move_id: MoveId, caller: MoveId) {
    let dex_data = *battle.format.dex.move_(move_id);
    // `useMoveInner`: `ModifyTarget` (Metal Burst, Comeuppance) first; only
    // without a target is a random one drawn.
    let target = super::multiturn::modify_target(battle, user, move_id)
        .or_else(|| targeting::get_random_target(battle, user, dex_data.target));
    // `move.priority = activeMove.priority`.
    let priority = active_priority(battle, battle.format.dex.move_(caller));
    battle.active_move = Some((user, move_id));
    battle.order.source_effect = Some(caller);
    battle.order.smart_target = smart_target_move(move_id);
    battle.order.priority_override = Some(priority);
    // The calling move's `ActiveMove` fields (its `hasSheerForce`, a pushed
    // flinch, ...) are its own: the called move gets new ones, and the
    // caller's are back after it. `activeTarget` is the battle's.
    let caller_state = battle.move_state;
    // The called move's own `ModifyType` (`use_move` runs the rest and
    // `runEvent('ModifyType')`: the -ate abilities, Electrify).
    let typed;
    let data = match moves::modify_type(battle, user, move_id) {
        Some(ty) => {
            typed = MoveData { ty, ..*battle.format.dex.move_(move_id) };
            &typed
        }
        None => battle.format.dex.move_(move_id),
    };
    // The `[from]` comes from `sourceEffect` (`move_line`).
    // `useMove`: a called move's result becomes `moveThisTurnResult` unless
    // it set one itself (Stomping Tantrum after a failed call).
    let before = battle.state.at(user).map(|m| m.volatile_counters.move_this_turn);
    let did = moves::use_move(battle, user, move_id, data, target, None);
    let called = &battle.move_state;
    // A nested call (including Magic Bounce) may have replaced the
    // battle's activeMove again. Keep that innermost move's fields for
    // BattleActions.runMove's AfterMove, not the intermediate caller's.
    battle.order.called_move.get_or_insert(CalledMove {
        ty: called.ty,
        has_sheer_force: called.has_sheer_force,
        ignore_ability: called.ignore_ability,
    });
    battle.move_state = super::MoveState { active_target: battle.move_state.active_target, ..caller_state };
    if let Some(m) = battle.state.at_mut(user)
        && Some(m.volatile_counters.move_this_turn) == before
    {
        m.volatile_counters.move_this_turn =
            if did { crate::state::MoveResult::Succeeded } else { crate::state::MoveResult::Failed };
    }
}

// ---------------------------------------------------------------------
// Hit steps.

/// The moves that call another (`callsMove` in the data: Metronome,
/// Copycat and Sleep Talk through [`call_move`]; Assist, Me First, Mirror
/// Move and Nature Power are Past): the called move's `sourceEffect` is
/// their `ActiveMove`.
pub(crate) fn calls_moves(dex: &pikapro_data::Dex, move_id: MoveId) -> bool {
    dex.moves.get(move_id.0 as usize).is_some_and(|m| m.unmodelled.contains(&"callsMove"))
}

/// Status moves whose effect this module implements beyond their data.
pub(crate) fn handles(move_id: MoveId) -> bool {
    matches!(
        move_id,
        ids::AFTER_YOU
            | ids::QUASH
            | ids::INSTRUCT
            | ids::ALLY_SWITCH
            | ids::HELPING_HAND
            | ids::FOLLOW_ME
            | ids::RAGE_POWDER
            | ids::METRONOME
            | ids::COPYCAT
            | ids::SLEEP_TALK
            | ids::BATON_PASS
            | ids::TRANSFORM
            | ids::SKETCH
            | ids::MIMIC
            | ids::SPITE
    )
}

/// The move's `onTry` (`singleEvent('Try')` in `trySpreadMoveHit`). False
/// fails the move (`-fail`).
pub(crate) fn on_try(battle: &mut Battle, user: Position, target: Position, move_id: MoveId) -> bool {
    match move_id {
        // Comatose counts as asleep.
        ids::SLEEP_TALK | ids::SNORE => {
            battle.state.at(user).is_some_and(|m| matches!(m.status, Status::Sleep { .. }))
                || super::abilities::has_ability(battle, user, pikapro_data::gen9::abilities::COMATOSE)
        }
        ids::FOLLOW_ME | ids::RAGE_POWDER => battle.format.active_per_side() > 1,
        ids::SUCKER_PUNCH | ids::THUNDERCLAP => {
            let recharging = battle.state.at(target).is_some_and(|m| m.volatiles.has(Volatiles::MUST_RECHARGE));
            match queue::will_move_action(battle, target).map(|(_, a)| a.kind) {
                Some(ActionKind::Move { move_id: m, .. }) if m != super::volatiles::RECHARGE => {
                    let status = battle.format.dex.move_(m).category == MoveCategory::Status;
                    !(recharging || status && m != ids::ME_FIRST)
                }
                _ => false,
            }
        }
        ids::UPPER_HAND => match queue::will_move_action(battle, target) {
            // `move.priority` is the action's priority as `getActionSpeed`
            // computed it.
            Some((_, Action { kind: ActionKind::Move { move_id: m, .. }, priority, .. }))
                if m != super::volatiles::RECHARGE =>
            {
                priority > 0 && battle.format.dex.move_(m).category != MoveCategory::Status
            }
            _ => false,
        },
        ids::LAST_RESORT => {
            let Some(mon) = battle.state.at(user) else { return false };
            if mon.moves.len() < 2 {
                return false;
            }
            let mut has_last_resort = false;
            for (i, slot) in mon.moves.iter().enumerate() {
                if slot.id == ids::LAST_RESORT {
                    has_last_resort = true;
                } else if mon.record.used_moves & (1 << i) == 0 {
                    return false;
                }
            }
            has_last_resort
        }
        ids::ROUND => {
            // The first Round still queued moves up, with Round as its
            // source effect (it doubles its power).
            let next = battle.queue.as_slice().iter().enumerate().find_map(|(i, a)| match a.kind {
                ActionKind::Move { move_id: ids::ROUND, .. } if a.mon.is_some() => Some((i, *a)),
                _ => None,
            });
            if let Some((i, action)) = next {
                prioritize(battle, i, action, Some(ids::ROUND));
            }
            true
        }
        _ => true,
    }
}

/// The move's `onPrepareHit`. Ally Switch adds its stall-like volatile,
/// which fails with chance `1 - 1/counter` on consecutive uses.
#[inline(always)]
pub(crate) fn prepare_hit(battle: &mut Battle, user: Position, move_id: MoveId) -> bool {
    move_id != ids::ALLY_SWITCH || ally_switch_prepare_hit(battle, user)
}

/// [`prepare_hit`] for Ally Switch.
fn ally_switch_prepare_hit(battle: &mut Battle, user: Position) -> bool {
    let Some(mon) = battle.state.at(user) else { return false };
    if !mon.volatiles.has(Volatiles::ALLY_SWITCH) {
        // `TryAddVolatile` (its ties draw), then `onStart`.
        super::events::try_add_volatile(battle, user, "allyswitch");
        let mon = battle.state.at_mut(user).expect("checked above");
        mon.volatiles.add(Volatiles::ALLY_SWITCH);
        mon.volatile_counters.ally_switch = 2;
        mon.volatile_counters.ally_switch_counter = 3;
        return true;
    }
    // `onRestart`.
    let counter = mon.volatile_counters.ally_switch_counter.max(1);
    if !battle.rng.chance(1, counter as u32) {
        battle.state.at_mut(user).expect("checked above").volatiles.clear(Volatiles::ALLY_SWITCH);
        return false;
    }
    let counters = &mut battle.state.at_mut(user).expect("checked above").volatile_counters;
    if counters.ally_switch_counter < 729 {
        counters.ally_switch_counter *= 3;
    }
    counters.ally_switch = 2;
    true
}

/// The move's own `onTryHit` (a `singleEvent` in `spreadMoveHit`). False
/// fails the hit with `-fail`. Helping Hand fails on an ally that has
/// already moved, unless it just switched in.
pub(crate) fn try_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    if move_id != ids::HELPING_HAND {
        return true;
    }
    // `newlySwitched` is true from switching in until the end of the turn.
    let newly_switched = battle.state.at(target).is_some_and(|m| m.active_turns == 0);
    if !newly_switched && !queue::will_move(battle, target) {
        moves::fail_still(battle, user);
        return false;
    }
    true
}

/// Moves whose `onHit` is ported here.
pub(crate) fn handles_hit(move_id: MoveId) -> bool {
    matches!(
        move_id,
        ids::AFTER_YOU
            | ids::QUASH
            | ids::INSTRUCT
            | ids::ALLY_SWITCH
            | ids::METRONOME
            | ids::COPYCAT
            | ids::SLEEP_TALK
            | ids::BATON_PASS
            | ids::POLLEN_PUFF
            | ids::TRANSFORM
            | ids::SKETCH
            | ids::MIMIC
            | ids::SPITE
    )
}

/// The move's `onHit` on `target`, as `runMoveEffects` combines it.
pub(crate) fn on_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> Outcome {
    let fail = Outcome::Failed;
    let ok = Outcome::Succeeded;
    match move_id {
        ids::AFTER_YOU => {
            if battle.format.active_per_side() == 1 {
                return fail;
            }
            let Some((i, action)) = queue::will_move_action(battle, target) else { return fail };
            prioritize(battle, i, action, None);
            activate(battle, target, move_id, None, 0);
            ok
        }
        ids::QUASH => {
            if battle.format.active_per_side() == 1 {
                return fail;
            }
            let Some((i, _)) = queue::will_move_action(battle, target) else { return fail };
            battle.queue.as_mut_slice()[i].order = 201;
            activate(battle, target, move_id, None, 0);
            ok
        }
        ids::INSTRUCT => instruct(battle, target, user),
        ids::ALLY_SWITCH => ally_switch(battle, user),
        ids::METRONOME => {
            let Some(called) = metronome_move(battle) else { return fail };
            call_move(battle, user, called, move_id);
            ok
        }
        ids::COPYCAT => {
            let Some(last) = battle.order.last_move else { return ok };
            if battle.format.dex.move_(last).flags.contains(MoveFlags::FAIL_COPYCAT) {
                return fail;
            }
            call_move(battle, user, last, move_id);
            ok
        }
        ids::SLEEP_TALK => {
            let Some(mon) = battle.state.at(user) else { return fail };
            let dex = battle.format.dex;
            let choices: StackVec<MoveId, 4> = mon
                .moves
                .iter()
                .map(|s| s.id)
                .filter(|&m| {
                    !dex.move_(m).flags.contains(MoveFlags::NO_SLEEP_TALK)
                        && !dex.move_(m).flags.contains(MoveFlags::CHARGE)
                })
                .collect();
            if choices.is_empty() {
                return fail;
            }
            let called = *battle.rng.sample(&choices);
            call_move(battle, user, called, move_id);
            ok
        }
        ids::BATON_PASS => {
            let commanded = battle.state.at(target).is_some_and(|m| m.volatiles.has(Volatiles::COMMANDED));
            if switching::can_switch(battle, user.side) == 0 || commanded {
                moves::fail_still(battle, target);
                return Outcome::NotFail;
            }
            // `self.onHit`.
            if let Some(mon) = battle.state.at_mut(user) {
                mon.skip_before_switch_out = true;
            }
            ok
        }
        ids::POLLEN_PUFF => {
            if target.side != user.side {
                return ok;
            }
            // Heal Block stopped it earlier (`onTryMove`,
            // `conditions::pollen_puff_try_move`).
            let amount = battle.state.at(target).map_or(0, |m| m.max_hp() / 2);
            if apply_heal(battle, target, amount as u32, HealSource::Move) == 0 {
                return Outcome::NotFail;
            }
            ok
        }
        ids::TRANSFORM => Outcome::from_bool(transform_into(battle, user, target, None)),
        ids::SKETCH => sketch(battle, target, user),
        ids::MIMIC => mimic(battle, target, user),
        ids::SPITE => {
            let Some(last) = battle.state.at(target).and_then(|m| m.last_move) else { return fail };
            let removed = deduct_pp_of(battle, target, last, 4);
            if removed == 0 {
                return fail;
            }
            activate(battle, target, move_id, Some(last), removed);
            ok
        }
        _ => ok,
    }
}

fn activate(battle: &mut Battle, target: Position, move_id: MoveId, about: Option<MoveId>, amount: u8) {
    let target = actor(battle, target);
    battle.emit(BattleEvent::ActivateMove { target, move_id, about, amount });
}

/// Instruct's `onHit`: the target's last move is resolved again (as a new
/// move action, with the drawing that entails) and put at the front of
/// the queue.
fn instruct(battle: &mut Battle, target: Position, user: Position) -> Outcome {
    let Some(mon) = battle.state.at(target) else { return Outcome::Failed };
    let Some(last) = mon.last_move else { return Outcome::Failed };
    let data = battle.format.dex.move_(last);
    let slot = mon.moves.iter().position(|s| s.id == last);
    // A target charging Focus Punch or Beak Blast fails it
    // (`volatiles['focuspunch' | 'beakblast' | 'shelltrap']`; Chilly
    // Reception's charge doesn't).
    let charging = mon.volatiles.has(Volatiles::PRIORITY_CHARGE)
        && matches!(mon.volatile_counters.priority_charge, ids::FOCUS_PUNCH | ids::BEAK_BLAST | ids::SHELL_TRAP);
    if data.flags.contains(MoveFlags::FAIL_INSTRUCT)
        || charging
        || data.flags.contains(MoveFlags::CHARGE)
        || data.flags.contains(MoveFlags::RECHARGE)
        || slot.is_some_and(|i| mon.moves[i].pp == 0)
    {
        return Outcome::Failed;
    }
    let target_loc = mon.last_move_target_loc;
    let (t, of) = (actor(battle, target), actor(battle, user));
    battle.emit(BattleEvent::MoveSingleTurn { target: t, move_id: ids::INSTRUCT, of: Some(of) });
    let kind = ActionKind::Move { slot: slot.unwrap_or(0) as u8, move_id: last, target_loc, gimmick: None };
    let action = Action::new(kind, Some(mon_ref(battle, target)));
    let action = queue::resolve_action(battle, action, false)[0];
    battle.queue.insert(0, Action { order: 3, source_effect: None, ..action });
    Outcome::Succeeded
}

/// Ally Switch's `onHit`: swap places with the ally, or fail with
/// `-fail|...|move: Ally Switch` (`NOT_FAIL`).
fn ally_switch(battle: &mut Battle, user: Position) -> Outcome {
    let new_slot = if user.slot == 0 { battle.format.active_per_side() as u8 - 1 } else { 0 };
    let ok = battle.format.active_per_side() == 2
        && new_slot != user.slot
        && is_active(battle, Position::new(user.side, new_slot));
    if !ok {
        let a = actor(battle, user);
        battle.emit(BattleEvent::MoveFail { user: a, move_id: ids::ALLY_SWITCH, note: FailNote::None });
        battle.flush_rng_trace();
        battle.attr_last_move(crate::event::MoveAttr::Still);
        return Outcome::NotFail;
    }
    swap_position(battle, user, new_slot, ids::ALLY_SWITCH);
    Outcome::Succeeded
}

/// Metronome's `onHit` picks from `this.dex.moves.all().filter(move =>
/// (!move.isNonstandard || move.isNonstandard === 'Unobtainable') &&
/// move.flags['metronome'])`, sorted by `num` (a stable sort), with
/// `this.sample(moves)`: one draw, even from a single move. The list is
/// built once per dex at compile time ([`METRONOME_GEN9`],
/// [`METRONOME_CHAMPIONS`]).
fn metronome_move(battle: &mut Battle) -> Option<MoveId> {
    let dex = battle.format.dex;
    let table = if core::ptr::eq(dex, &pikapro_data::gen9::DEX) {
        &METRONOME_GEN9
    } else if core::ptr::eq(dex, &pikapro_data::champions::DEX) {
        &METRONOME_CHAMPIONS
    } else {
        // A dex of the caller's own: its list, built on the spot.
        let list = MetronomeMoves::build(dex.moves);
        return (list.len > 0).then(|| *battle.rng.sample(list.as_slice()));
    };
    (table.len > 0).then(|| *battle.rng.sample(table.as_slice()))
}

/// Metronome's callable moves in one dex, in its order.
struct MetronomeMoves {
    ids: [MoveId; METRONOME_MAX],
    len: usize,
}

const METRONOME_MAX: usize = pikapro_data::gen9::DEX.moves.len();

impl MetronomeMoves {
    fn as_slice(&self) -> &[MoveId] {
        &self.ids[..self.len]
    }

    /// Showdown's filter, then a stable counting sort by `num` (every
    /// `num` Metronome can call is below 1024).
    const fn build(moves: &[MoveData]) -> MetronomeMoves {
        const NUMS: usize = 1024;
        const fn callable(m: &MoveData) -> bool {
            m.flags.contains(MoveFlags::METRONOME) && matches!(m.nonstandard, None | Some(Nonstandard::Unobtainable))
        }
        assert!(moves.len() <= METRONOME_MAX);
        // `start[num]`: where the moves of that number go.
        let mut start = [0u16; NUMS + 1];
        let mut i = 0;
        while i < moves.len() {
            if callable(&moves[i]) {
                assert!(moves[i].num >= 0 && (moves[i].num as usize) < NUMS);
                start[moves[i].num as usize + 1] += 1;
            }
            i += 1;
        }
        let mut n = 0;
        while n < NUMS {
            start[n + 1] += start[n];
            n += 1;
        }
        let len = start[NUMS] as usize;
        let mut ids = [MoveId(0); METRONOME_MAX];
        i = 0;
        while i < moves.len() {
            if callable(&moves[i]) {
                let num = moves[i].num as usize;
                ids[start[num] as usize] = MoveId(i as u16);
                start[num] += 1;
            }
            i += 1;
        }
        MetronomeMoves { ids, len }
    }
}

/// Metronome's list in the Gen 9 dex (581 moves).
static METRONOME_GEN9: MetronomeMoves = MetronomeMoves::build(pikapro_data::gen9::DEX.moves);
/// Metronome's list in the Champions dex (447 moves; Metronome itself is
/// `Past` there).
static METRONOME_CHAMPIONS: MetronomeMoves = MetronomeMoves::build(pikapro_data::champions::DEX.moves);

/// Sketch's `onHit`: the user permanently learns the target's last move in
/// Sketch's slot.
fn sketch(battle: &mut Battle, target: Position, user: Position) -> Outcome {
    let Some(last) = battle.state.at(target).and_then(|m| m.last_move) else { return Outcome::Failed };
    let copied_target = copied_target(battle, target);
    let data = *battle.format.dex.move_(last);
    let Some(mon) = battle.state.at_mut(user) else { return Outcome::Failed };
    if mon.volatiles.has(Volatiles::TRANSFORMED) || mon.moves.iter().any(|s| s.id == last) {
        return Outcome::Failed;
    }
    if data.flags.contains(MoveFlags::NO_SKETCH) {
        return Outcome::Failed;
    }
    let Some(index) = mon.moves.iter().position(|s| s.id == ids::SKETCH) else { return Outcome::Failed };
    let target = copied_target.filter(|&t| t != data.target);
    mon.moves[index] = MoveSlot { id: last, pp: data.pp, max_pp: data.pp, disabled: false, target };
    mon.record.used_moves &= !(1 << index);
    activate(battle, user, ids::SKETCH, Some(last), 0);
    Outcome::Succeeded
}

/// `target.lastMove.target`, which Sketch and Mimic copy into the slot.
fn copied_target(battle: &Battle, target: Position) -> Option<pikapro_data::MoveTarget> {
    battle.state.at(target).and_then(|m| m.last_move_target)
}

/// Mimic's `onHit`: Mimic's slot holds the target's last move until the
/// user switches out.
fn mimic(battle: &mut Battle, target: Position, user: Position) -> Outcome {
    let Some(last) = battle.state.at(target).and_then(|m| m.last_move) else { return Outcome::Failed };
    let copied_target = copied_target(battle, target);
    let data = *battle.format.dex.move_(last);
    let Some(mon) = battle.state.at_mut(user) else { return Outcome::Failed };
    if mon.volatiles.has(Volatiles::TRANSFORMED)
        || data.flags.contains(MoveFlags::FAIL_MIMIC)
        || mon.moves.iter().any(|s| s.id == last)
    {
        return Outcome::Failed;
    }
    let Some(index) = mon.moves.iter().position(|s| s.id == ids::MIMIC) else { return Outcome::Failed };
    mon.volatiles.add(Volatiles::MIMIC);
    mon.volatile_counters.mimic_slot = index as u8;
    mon.volatile_counters.mimic_original = mon.moves[index];
    let target = copied_target.filter(|&t| t != data.target);
    mon.moves[index] = MoveSlot { id: last, pp: data.pp, max_pp: data.pp, disabled: false, target };
    mon.record.used_moves &= !(1 << index);
    let a = actor(battle, user);
    battle.emit(BattleEvent::MimicStart { actor: a, move_id: last });
    Outcome::Succeeded
}

/// Showdown's `transformInto`: the user takes the target's species (for
/// stats, types, moves with 5 PP each, and stat stages), keeping its own
/// HP, name and ability slot. `effect` is the ability that made it
/// (Imposter: `-transform|...|[from] ability: Imposter`).
pub(crate) fn transform_into(
    battle: &mut Battle,
    user: Position,
    target: Position,
    effect: Option<pikapro_data::AbilityId>,
) -> bool {
    let dex = battle.format.dex;
    let (Some(source), Some(into)) = (battle.state.at(user).copied(), battle.state.at(target).copied()) else {
        return false;
    };
    let tera_species = |m: &PokemonState| {
        let base = dex.species(m.species).base_species;
        base == species::OGERPON || base == species::TERAPAGOS
    };
    // A substitute (Imposter; the move is blocked by it
    // before), Illusion.
    if into.is_fainted()
        || into.volatiles.has(Volatiles::SUBSTITUTE)
        || super::abilities::switchin::has_illusion(battle, user)
        || super::abilities::switchin::has_illusion(battle, target)
        || into.volatiles.has(Volatiles::TRANSFORMED)
        || source.volatiles.has(Volatiles::TRANSFORMED)
        || into.species == species::ETERNATUS_ETERNAMAX
        || tera_species(&into) && (source.terastallized || into.terastallized)
        // `this.terastallized === 'Stellar'`.
        || source.terastallized && source.tera_type == pikapro_data::Type::Stellar
    {
        return false;
    }
    // The Pokémon's own form, with a Mimic slot put back.
    let mut own_moves = source.moves;
    if source.volatiles.has(Volatiles::MIMIC) {
        let c = source.volatile_counters;
        own_moves[c.mimic_slot as usize] = c.mimic_original;
    }
    let base = TransformBase {
        species: into.species,
        own_species: source.species,
        moves: own_moves,
        stats: source.stats,
        types: source.types,
        base_types: source.base_types,
    };
    // `getTypes(true, true)` (Roost's `typeWas` for a roosting target): the
    // target's types before Terastallization and Roost, including ones a
    // move changed (Soak, Magic Powder).
    let types: TypeSet = into.volatile_counters.types.unwrap_or(into.base_types);
    let moves = into
        .moves
        .iter()
        .map(|s| {
            let pp = dex.move_(s.id).pp.min(5);
            MoveSlot { id: s.id, pp, max_pp: pp, disabled: false, target: s.target }
        })
        .collect();
    let mon = battle.state.at_mut(user).expect("checked above");
    mon.volatiles.clear(Volatiles::MIMIC);
    mon.volatiles.add(Volatiles::TRANSFORMED);
    mon.volatile_counters.transform = Some(base);
    for stat in [Stat::Atk, Stat::Def, Stat::SpA, Stat::SpD, Stat::Spe] {
        mon.stats[stat] = into.stats[stat];
    }
    // `setType(types, true)`: `pokemon.types` become the target's, replacing
    // any a forme change or a move set (Shields Down, Soak), which the
    // user's STAB (`getTypes(false, true)`) reads; a Terastallized user
    // keeps showing its Tera type.
    mon.base_types = types;
    mon.volatile_counters.types = Some(types);
    super::manipulation::refresh_types(mon);
    // `this.addedType = pokemon.addedType` (Forest's Curse, Trick-or-Treat).
    mon.volatile_counters.added_type = into.volatile_counters.added_type;
    // `knownType = isAlly && pokemon.knownType`, `apparentType =
    // pokemon.apparentType` (revealed at the end of the turn).
    let known = target.side == user.side && !into.volatile_counters.type_unknown;
    mon.volatile_counters.type_unknown = !known;
    // An apparent type an Illusion revealed is copied too.
    // (Showdown keeps one `apparentType`: a Tera type shown since switching
    // in is `tera_apparent` below.)
    let revealed = into.illusion_apparent.filter(|_| !into.volatile_counters.tera_apparent);
    mon.volatile_counters.apparent_types = into.volatile_counters.apparent_types.or(revealed);
    mon.illusion_apparent = None;
    // `this.timesAttacked = pokemon.timesAttacked` (Rage Fist, which keeps
    // it per mechanics).
    mon.record.times_attacked = into.record.times_attacked;
    mon.volatile_counters.times_attacked = into.volatile_counters.times_attacked;
    mon.moves = moves;
    mon.record.used_moves = 0;
    mon.boosts = into.boosts;
    // `apparentType` is copied: a target that Terastallized since switching
    // in shows its Tera type, which the end of the turn corrects.
    mon.volatile_counters.apparent_type_stale =
        into.volatile_counters.tera_apparent && !mon.terastallized && types != TypeSet::mono(into.tera_type);
    // `if (this.terastallized) { knownType = true; apparentType =
    // terastallized }` (Reflect Type and an Illusion disguised
    // as it see it).
    if mon.terastallized {
        mon.volatile_counters.tera_apparent = true;
        mon.volatile_counters.type_unknown = false;
    }
    // `setSpecies` stores the Speed of the target's species with the user's
    // own set, before the target's stats are copied.
    let base_speed = dex.species(into.species).base_stats[Stat::Spe];
    mon.speed = calc_stat(Stat::Spe, base_speed, mon.ivs[Stat::Spe], mon.evs[Stat::Spe], mon.level, mon.nature) as i32;
    // Dragon Cheer and Focus Energy, before `-transform`.
    super::manipulation::transform_copy(battle, user, &into);
    let (a, t) = (actor(battle, user), actor(battle, target));
    match effect {
        Some(ability) => {
            let cause = crate::event::Cause::ability(ability);
            battle.emit(BattleEvent::TransformFrom { actor: a, target: t, cause });
        }
        None => battle.emit(BattleEvent::Transform { actor: a, target: t }),
    }
    // `setAbility(pokemon.ability, this, null, true, true)`: the old
    // ability's `End`, a new `abilityState`, the new ability's `Start`
    // (Intimidate) unless it is the same.
    let kind = super::abilities::SetAbilityKind::Transform;
    super::abilities::set_ability(battle, user, into.ability, Some(user), None, kind);
    true
}

impl PokemonState {
    /// Ends Transform and Mimic (part of `clearVolatile`): the move slots,
    /// stats and types go back to the Pokémon's own, and the used-move
    /// marks reset.
    pub(crate) fn restore_base_form(&mut self) {
        if self.volatiles.has(Volatiles::TRANSFORMED) {
            if let Some(base) = self.volatile_counters.transform {
                self.moves = base.moves;
                self.stats = base.stats;
                self.base_types = base.base_types;
                // `setSpecies(baseSpecies)`; a Terastallized Pokémon shows
                // its Tera type.
                self.types = self.tera_types().unwrap_or(base.types);
            }
        } else if self.volatiles.has(Volatiles::MIMIC) {
            let c = self.volatile_counters;
            if let Some(slot) = self.moves.get_mut(c.mimic_slot as usize) {
                *slot = c.mimic_original;
            }
        }
        self.record.used_moves = 0;
    }
}

/// The active move's priority (`move.priority`): its action's (after
/// `ModifyPriority`); a called move has its caller's, an encored move the
/// chosen one's.
pub(crate) fn active_priority(battle: &Battle, data: &MoveData) -> i8 {
    battle.order.priority_override.unwrap_or(data.priority)
}

/// `smartTarget` of the active move.
pub(crate) fn smart_target(battle: &Battle) -> bool {
    battle.order.smart_target
}

/// After a hit step: a `false` result turns `smartTarget` off.
pub(crate) fn after_hit_step(battle: &mut Battle, failed: bool) {
    if failed {
        battle.order.smart_target = false;
    }
}

/// `hitStepAccuracy` missing a `smartTarget` move's target: `smartTarget`
/// goes off instead of the `-miss` message. Returns whether that happened.
pub(crate) fn smart_target_missed(battle: &mut Battle) -> bool {
    core::mem::take(&mut battle.order.smart_target)
}

/// The targets of hit number `hit`: a `smartTarget` move with two targets
/// hits one per hit. A move with a `smartTarget` field retargets its
/// `|move|` line to the first hit's target.
pub(crate) fn hit_targets(battle: &mut Battle, user: Position, move_id: MoveId, targets: &Targets, hit: u8) -> Targets {
    let hit_targets = if battle.order.smart_target && targets.len() > 1 {
        let mut one = Targets::new();
        one.push(targets[(hit as usize - 1).min(targets.len() - 1)]);
        one
    } else {
        *targets
    };
    if hit == 1 && smart_target_move(move_id) {
        let t = actor(battle, hit_targets[0]);
        battle.flush_rng_trace();
        battle.log.retarget_last_move(t);
    } else if hit > 1 && smart_target_move(move_id) {
        // `addMove('-anim', pokemon, move.name, target)`.
        super::multiturn::anim(battle, user, move_id, hit_targets[0]);
    }
    hit_targets
}

/// `move.sleepUsable || dex.moves.get(move.sourceEffect).sleepUsable`.
pub(crate) fn sleep_usable(battle: &Battle, move_id: MoveId) -> bool {
    sleep_usable_move(move_id) || battle.order.source_effect.is_some_and(sleep_usable_move)
}

/// Whether the hit deals damage (a number rather than `undefined`): not
/// for status moves, nor for Pollen Puff on an ally (its `onTryHit` sets
/// the base power to 0).
pub(crate) fn deals_damage(user: Position, target: Position, move_id: MoveId, data: &MoveData) -> bool {
    data.category != MoveCategory::Status && !(move_id == ids::POLLEN_PUFF && target.side == user.side)
}

/// Moves with a `self` effect the data does not show (Baton Pass's, Burn
/// Up's and Double Shock's `self.onHit`): a failure is then not reported
/// by `runMoveEffects`.
pub(crate) fn has_self(move_id: MoveId) -> bool {
    move_id == ids::BATON_PASS || super::manipulation::has_self_on_hit(move_id)
}

// ---------------------------------------------------------------------
// Damage.

/// Round's `basePowerCallback`: doubled when moved up by another Round.
pub(crate) fn base_power_callback(battle: &Battle, move_id: MoveId, base_power: u16) -> u16 {
    if move_id == ids::ROUND && battle.order.source_effect == Some(ids::ROUND) { base_power * 2 } else { base_power }
}

/// Helping Hand's `onBasePower` (priority 10): `chainModify(1.5^n)`.
pub(crate) fn helping_hand_base_power(
    battle: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
    h: &Handler,
) -> Option<super::events::Relay> {
    let Holder::Pokemon(pos) = h.holder else { return None };
    ev.chain(base_power_modifier(battle, pos));
    None
}

/// Helping Hand's multiplier as a 4096-based modifier (`1.5^n`, 4096 = none).
pub(crate) fn base_power_modifier(battle: &Battle, attacker: Position) -> u32 {
    let Some(mon) = battle.state.at(attacker).filter(|m| m.volatiles.has(Volatiles::HELPING_HAND)) else {
        return 4096;
    };
    // `trunc(multiplier * 4096)` for 1.5, 2.25, 3.375, ...
    let n = mon.volatile_counters.helping_hand.max(1) as u32;
    4096 * 3u32.pow(n) / 2u32.pow(n)
}

// ---------------------------------------------------------------------
// Volatiles.

/// `addVolatile` for Helping Hand, Follow Me and Rage Powder; `None` for
/// other IDs.
pub(crate) fn add_volatile(
    battle: &mut Battle,
    target: Position,
    id: &str,
    source: Option<Position>,
) -> Option<Option<bool>> {
    let (flag, move_id) = match id {
        "helpinghand" => (Volatiles::HELPING_HAND, ids::HELPING_HAND),
        "followme" => (Volatiles::FOLLOW_ME, ids::FOLLOW_ME),
        "ragepowder" => (Volatiles::RAGE_POWDER, ids::RAGE_POWDER),
        _ => return None,
    };
    // `if (!this.hp) return false`.
    let Some(mon) = battle.state.at(target).filter(|m| !m.is_fainted()) else { return Some(Some(false)) };
    let present = mon.volatiles.has(flag);
    // Only Helping Hand has `onRestart` (the multiplier grows).
    if present && flag != Volatiles::HELPING_HAND {
        return Some(Some(false));
    }
    let order = if flag == Volatiles::HELPING_HAND {
        0
    } else {
        // `effectOrder`: after every redirection already on the field.
        let mut order = 0;
        for side in SideId::BOTH {
            for slot in 0..battle.format.active_per_side() {
                if let Some(m) = battle.state.at(Position::new(side, slot as u8))
                    && m.volatiles.has(REDIRECTION)
                {
                    order = order.max(m.volatile_counters.redirect_order + 1);
                }
            }
        }
        order
    };
    let mon = battle.state.at_mut(target).expect("checked above");
    mon.volatiles.add(flag);
    if flag == Volatiles::HELPING_HAND {
        mon.volatile_counters.helping_hand = if present { mon.volatile_counters.helping_hand + 1 } else { 1 };
    } else {
        mon.volatile_counters.redirect_order = order;
    }
    let of = match (flag, source) {
        (Volatiles::HELPING_HAND, Some(s)) => Some(actor(battle, s)),
        _ => None,
    };
    let t = actor(battle, target);
    battle.emit(BattleEvent::MoveSingleTurn { target: t, move_id, of });
    Some(Some(true))
}

/// End-of-turn handlers: this module's volatiles have durations (1 for
/// Helping Hand, Follow Me and Rage Powder; 2 for Ally Switch) and no
/// `onResidual`, so they only count down, but they take part in the sort.
pub(crate) fn residual_handlers(battle: &Battle, handlers: &mut Handlers) {
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            let pos = Position::new(side, slot as u8);
            let Some(mon) = battle.state.at(pos) else { continue };
            const FLAGS: [u128; 4] =
                [Volatiles::HELPING_HAND, Volatiles::FOLLOW_ME, Volatiles::RAGE_POWDER, Volatiles::ALLY_SWITCH];
            // None of them: nothing to collect.
            if !mon.volatiles.has(FLAGS[0] | FLAGS[1] | FLAGS[2] | FLAGS[3]) {
                continue;
            }
            let base = Handler {
                speed: mon.speed,
                sub_order: sub_order::CONDITION,
                holder: Holder::Pokemon(pos),
                callback: Callback::Plain(tick),
                ..Handler::default()
            };
            for flag in FLAGS {
                if mon.volatiles.has(flag) {
                    handlers.push(Handler { volatile_bit: vol(flag), ..base });
                }
            }
        }
    }
}

/// Counts a volatile's duration down; it ends silently at 0.
fn tick(battle: &mut Battle, handler: &Handler) -> bool {
    let Holder::Pokemon(pos) = handler.holder else { return true };
    let Some(mon) = battle.state.at_mut(pos) else { return true };
    if !mon.volatiles.has(handler.volatile()) {
        return true;
    }
    if handler.volatile() == Volatiles::ALLY_SWITCH {
        let turns = &mut mon.volatile_counters.ally_switch;
        *turns = turns.saturating_sub(1);
        if *turns > 0 {
            return true;
        }
    }
    mon.volatiles.clear(handler.volatile());
    // Ran out: no `faintMessages` after it (`run_residual`).
    false
}

/// `endTurn`: a Pokémon whose shown type differs from its real types (a
/// Pokémon transformed into a Terastallized one) reveals them.
pub(crate) fn end_turn(battle: &mut Battle, pos: Position) {
    let Some(mon) = battle.state.at_mut(pos) else { return };
    // An Illusion shows another Pokémon's types
    // (`manipulation::end_turn` checks those).
    if !mon.volatile_counters.apparent_type_stale || mon.terastallized || mon.illusion.is_some() {
        return;
    }
    mon.volatile_counters.apparent_type_stale = false;
    let (types, added) = (mon.types, mon.volatile_counters.added_type);
    let a = actor(battle, pos);
    battle.emit(BattleEvent::TypeChangeSilent { actor: a, types });
    // `if (pokemon.addedType) this.add('-start', pokemon, 'typeadd',
    // pokemon.addedType, '[silent]')`.
    if let Some(ty) = added {
        battle.emit(BattleEvent::TypeAdd { target: a, ty, move_id: None });
    }
}

// ---------------------------------------------------------------------
// Baton Pass.

/// What Baton Pass passes on (`copyVolatileFrom(pokemon, 'copyvolatile')`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Passed {
    boosts: Boosts,
    volatiles: Volatiles,
    counters: crate::state::VolatileCounters,
    protect_counter: u8,
    stall_turns: u8,
}

/// The outgoing Pokémon's stat stages and copyable volatiles, if it leaves
/// through Baton Pass (`from` is the move that switched it out).
pub(crate) fn passed_volatiles(mon: &PokemonState, from: Option<MoveId>) -> Option<Passed> {
    if from != Some(ids::BATON_PASS) {
        return None;
    }
    let mut volatiles = mon.volatiles;
    volatiles.clear(NO_COPY);
    Some(Passed {
        boosts: mon.boosts,
        volatiles,
        counters: mon.volatile_counters,
        protect_counter: mon.protect_counter,
        stall_turns: mon.stall_turns,
    })
}

/// Gives the incoming Pokémon what Baton Pass passed (after its own
/// volatiles were cleared). Volatiles keep their order and state.
pub(crate) fn copy_volatiles(mon: &mut PokemonState, passed: Passed, dex: &Dex) {
    mon.boosts = passed.boosts;
    mon.volatiles = passed.volatiles;
    // `VolatileCounters` also holds Pokémon properties that switching out
    // resets but that are not volatiles; the incoming Pokémon keeps its own.
    let own = mon.volatile_counters;
    let mut c = passed.counters;
    // `hurtThisTurn`, `statsLoweredThisTurn`, `attackedBy`
    // damage, `moveThisTurnResult`, `timesAttacked`.
    c.hurt_this_turn = own.hurt_this_turn;
    c.hurt_hp = own.hurt_hp;
    c.stats_lowered_this_turn = own.stats_lowered_this_turn;
    c.damaged_by = own.damaged_by;
    c.settled = own.settled;
    c.move_this_turn = own.move_this_turn;
    c.move_last_turn = own.move_last_turn;
    c.times_attacked = own.times_attacked;
    // Types, `knownType`, `attackedBy`, `statsRaisedThisTurn`.
    c.types = own.types;
    c.added_type = own.added_type;
    c.apparent_types = own.apparent_types;
    c.type_unknown = own.type_unknown;
    c.attacked_by = own.attacked_by;
    c.last_move_used = own.last_move_used;
    c.stats_raised_this_turn = own.stats_raised_this_turn;
    // Transform and Mimic are not copied; `apparentType` and the ability
    // order are the Pokémon's own.
    c.transform = own.transform;
    c.mimic_slot = own.mimic_slot;
    c.mimic_original = own.mimic_original;
    c.apparent_type_stale = own.apparent_type_stale;
    c.tera_apparent = own.tera_apparent;
    c.ability_order = own.ability_order;
    // The ability and item states are the Pokémon's own, and
    // `choicelock` is `noCopy`.
    c.item_order = own.item_order;
    c.ability_state = own.ability_state;
    c.item_state = own.item_state;
    c.choice_lock = own.choice_lock;
    mon.volatile_counters = c;
    // `singleEvent('Copy')`: Power Trick and Power Shift
    // swap the receiver's stored Attack and Defense.
    for flag in [Volatiles::POWER_TRICK, Volatiles::POWER_SHIFT] {
        if mon.volatiles.has(flag) {
            let (atk, def) = (mon.stats[Stat::Atk], mon.stats[Stat::Def]);
            mon.stats[Stat::Atk] = def;
            mon.stats[Stat::Def] = atk;
        }
    }
    // Showdown's Gastro Acid `condition.onCopy` (data/moves.ts), called
    // by Pokemon#copyVolatileFrom: an unsuppressible recipient removes
    // the copied acid. Keeping it would suppress a later changed ability.
    if dex.ability(mon.ability).flags.contains(AbilityFlags::CANT_SUPPRESS) {
        mon.volatiles.clear(Volatiles::GASTRO_ACID);
    }
    if passed.volatiles.has(Volatiles::STALL) {
        mon.protect_counter = passed.protect_counter;
        mon.stall_turns = passed.stall_turns;
    }
}

#[cfg(test)]
mod tests {
    use pikapro_data::gen9::moves as m;

    use super::*;
    use crate::choice::{Choice, SlotAction};
    use crate::team::{PokemonSet, Team};
    use crate::{BattleOptions, Format, Seed};

    fn battle(format: Format, p1: PokemonSet, p2: PokemonSet) -> Battle {
        let teams = [Team::new(vec![p1]), Team::new(vec![p2])];
        Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap()
    }

    fn set(species: pikapro_data::SpeciesId, moves: &[MoveId]) -> PokemonSet {
        PokemonSet::new(species).with_level(50).with_moves(moves)
    }

    fn use_first_move(b: &mut Battle) {
        for side in SideId::BOTH {
            let action = SlotAction::Move { index: 0, target: None, gimmick: None };
            b.submit(side, Choice::single(action)).unwrap();
        }
    }

    /// `deduct_pp` names a dex move without PP, and `None` for the
    /// stand-ins that are no dex move (`recharge`, the locked-move effect).
    #[test]
    fn no_pp_names_only_dex_moves() {
        let mut b = battle(Format::gen9_singles(), set(species::MEW, &[m::TACKLE]), set(species::MEW, &[m::TACKLE]));
        let pos = Position::new(SideId::P1, 0);
        b.state.at_mut(pos).unwrap().moves[0].pp = 0;
        let last = |b: &Battle| match b.events().last() {
            Some(BattleEvent::CantNoPp { move_id, .. }) => *move_id,
            other => panic!("{other:?}"),
        };
        assert!(!deduct_pp(&mut b, pos, m::TACKLE));
        assert_eq!(last(&b), Some(m::TACKLE));
        for stand_in in [super::super::volatiles::RECHARGE, super::super::multiturn::LOCKED_MOVE_EFFECT] {
            assert!(!deduct_pp(&mut b, pos, stand_in));
            assert_eq!(last(&b), None);
        }
    }

    #[test]
    fn metronome_draws_from_showdowns_list() {
        let b = battle(Format::gen9_singles(), set(species::MEW, &[m::METRONOME]), set(species::MEW, &[m::TACKLE]));
        let dex = b.format.dex;
        let count = (0..dex.moves.len() as u16)
            .filter(|&i| {
                let d = dex.move_(MoveId(i));
                d.flags.contains(MoveFlags::METRONOME)
                    && matches!(d.nonstandard, None | Some(Nonstandard::Unobtainable))
            })
            .count();
        // `Dex.moves.all()` filtered as Metronome's `onHit` does.
        assert_eq!(count, 581);
        // The tables, sorted by `num` as Showdown sorts them (its lists
        // start and end with these; every `num` is distinct).
        let gen9 = METRONOME_GEN9.as_slice();
        assert_eq!(gen9.len(), 581);
        assert_eq!(gen9[..3], [m::POUND, m::MEGA_PUNCH, m::PAY_DAY]);
        assert_eq!(gen9[578..], [m::PSYCHIC_NOISE, m::UPPER_HAND, m::MALIGNANT_CHAIN]);
        let champions = METRONOME_CHAMPIONS.as_slice();
        assert_eq!(champions.len(), 447);
        assert_eq!(champions[..3], [m::POUND, m::FIRE_PUNCH, m::ICE_PUNCH]);
        assert_eq!(champions[444..], [m::SUPERCELL_SLAM, m::PSYCHIC_NOISE, m::UPPER_HAND]);
        for (table, dex) in [(gen9, &pikapro_data::gen9::DEX), (champions, &pikapro_data::champions::DEX)] {
            assert!(table.windows(2).all(|w| dex.move_(w[0]).num < dex.move_(w[1]).num));
            assert_eq!(MetronomeMoves::build(dex.moves).as_slice(), table);
        }
    }

    /// Every move that reaches [`call_move`] has `callsMove` in both dexes,
    /// which [`calls_moves`] reads.
    #[test]
    fn move_callers_have_calls_move() {
        for dex in [&pikapro_data::gen9::DEX, &pikapro_data::champions::DEX] {
            for id in [m::METRONOME, m::COPYCAT, m::SLEEP_TALK] {
                assert!(calls_moves(dex, id), "{id:?}");
            }
            assert!(!calls_moves(dex, m::TACKLE));
        }
    }

    #[test]
    fn helping_hand_chains_one_and_a_half_per_use() {
        let mut b = battle(Format::gen9_singles(), set(species::MEW, &[m::TACKLE]), set(species::MEW, &[m::TACKLE]));
        let pos = Position::new(SideId::P1, 0);
        assert_eq!(base_power_modifier(&b, pos), 4096);
        let mon = b.state.at_mut(pos).unwrap();
        mon.volatiles.add(Volatiles::HELPING_HAND);
        mon.volatile_counters.helping_hand = 1;
        assert_eq!(base_power_modifier(&b, pos), 6144);
        b.state.at_mut(pos).unwrap().volatile_counters.helping_hand = 2;
        assert_eq!(base_power_modifier(&b, pos), 9216);
    }

    #[test]
    fn prioritized_action_goes_first_with_order_three() {
        let mut b = battle(Format::gen9_singles(), set(species::MEW, &[m::TACKLE]), set(species::MEW, &[m::TACKLE]));
        b.queue.clear();
        for speed in [30, 20, 10] {
            b.queue.push(Action { order: 200, speed, ..Action::default() });
        }
        let last = b.queue.as_slice()[2];
        prioritize(&mut b, 2, last, Some(ids::ROUND));
        let q = b.queue.as_slice();
        assert_eq!((q[0].speed, q[0].order, q[0].source_effect), (10, 3, Some(ids::ROUND)));
        assert_eq!(q.iter().map(|a| a.speed).collect::<Vec<_>>(), [10, 30, 20]);
    }

    #[test]
    fn transform_copies_moves_with_five_pp_until_switching_out() {
        let ditto = set(species::DITTO, &[m::TRANSFORM]);
        let mut b = battle(Format::gen9_singles(), ditto, set(species::SNORLAX, &[m::TACKLE, m::REST]));
        use_first_move(&mut b);
        let mon = b.state.at(Position::new(SideId::P1, 0)).unwrap();
        assert!(mon.volatiles.has(Volatiles::TRANSFORMED));
        let moves: Vec<_> = mon.moves.iter().map(|s| (s.id, s.pp)).collect();
        assert_eq!(moves, [(m::TACKLE, 5), (m::REST, 5)]);
        assert_eq!(mon.types, b.format.dex.species(species::SNORLAX).types);
        assert!(b.events().iter().any(|e| matches!(e, BattleEvent::Transform { .. })));

        let mut mon = *mon;
        mon.clear_volatile_state();
        assert_eq!(mon.moves[0].id, m::TRANSFORM);
        assert_eq!(mon.types, b.format.dex.species(species::DITTO).types);
        assert_eq!(mon.stats, b.state.side(SideId::P1).team[0].volatile_counters.transform.unwrap().stats);
    }

    /// `clearVolatile` deletes only `abilityState.started` and
    /// `itemState.started`: the rest waits for `switchIn`'s new states (a
    /// fainted Neutralizing Gas revived in its slot still has `ending`).
    #[test]
    fn clearing_volatiles_keeps_the_ability_and_item_states() {
        let mut mon = crate::state::PokemonState::default();
        let c = &mut mon.volatile_counters;
        (c.ability_order, c.item_order, c.confusion) = (7, 8, 2);
        c.ability_state.ending = true;
        c.item_state.booster_started = true;
        c.item_state.eject = true;
        mon.clear_volatile_state();
        let c = &mon.volatile_counters;
        assert_eq!((c.ability_order, c.item_order, c.confusion), (7, 8, 0));
        assert!(c.ability_state.ending && c.item_state.eject);
        assert!(!c.item_state.booster_started);
    }

    #[test]
    fn mimic_slot_returns_on_switching_out() {
        let mut b = battle(
            Format::gen9_singles(),
            set(species::MEW, &[m::TACKLE, m::MIMIC]),
            set(species::SNORLAX, &[m::REST]),
        );
        let user = Position::new(SideId::P1, 0);
        let target = Position::new(SideId::P2, 0);
        b.state.at_mut(target).unwrap().last_move = Some(m::REST);
        assert!(mimic(&mut b, target, user) == Outcome::Succeeded);
        let mut mon = *b.state.at(user).unwrap();
        assert_eq!(mon.moves[1].id, m::REST);
        mon.clear_volatile_state();
        assert_eq!(mon.moves[1].id, m::MIMIC);
    }

    #[test]
    fn baton_pass_gastro_acid_copy_respects_unsuppressible_abilities() {
        use pikapro_data::gen9::abilities as a;

        for (ability, keeps_acid) in [(a::DISGUISE, false), (a::ZERO_TO_HERO, false), (a::INTIMIDATE, true)] {
            let mut b = battle(
                Format::gen9_singles(),
                set(species::MEW, &[m::BATON_PASS]).with_ability(a::SYNCHRONIZE),
                set(species::MIMIKYU, &[m::TRANSFORM]).with_ability(ability),
            );
            let source = Position::new(SideId::P1, 0);
            let recipient = Position::new(SideId::P2, 0);
            b.state.at_mut(source).unwrap().volatiles.add(Volatiles::GASTRO_ACID);
            let passed = passed_volatiles(b.state.at(source).unwrap(), Some(m::BATON_PASS)).unwrap();
            copy_volatiles(b.state.at_mut(recipient).unwrap(), passed, b.format.dex);
            assert_eq!(b.state.at(recipient).unwrap().volatiles.has(Volatiles::GASTRO_ACID), keeps_acid);

            // Transform replaces the unsuppressible ability with one that
            // Gastro Acid could suppress, exposing an incorrectly kept copy.
            assert!(transform_into(&mut b, recipient, source, None));
            assert_eq!(super::super::abilities::ignoring_ability(&b, recipient), keeps_acid);
        }
    }

    #[test]
    fn ally_moves_can_aim_at_an_empty_slot() {
        // Singles: no target to choose, and Showdown's `null` target.
        let mut b =
            battle(Format::gen9_singles(), set(species::MEW, &[m::HELPING_HAND]), set(species::MEW, &[m::TACKLE]));
        use_first_move(&mut b);
        assert!(b.events().iter().any(|e| matches!(e, BattleEvent::MoveNull { move_id: m::HELPING_HAND, .. })));
    }
}
