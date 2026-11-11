//! Showdown's `BattleQueue` (`sim/battle-queue.ts`) and action ordering.
//!
//! Everything a battle does between two requests is an [`Action`] in the
//! queue: team preview picks, sending out leads, switches, switch-in
//! effects (`RunSwitch`), gimmicks, moves and the end of turn. Actions are
//! resolved, sorted and inserted exactly as Showdown does it, because that is
//! where many of its random draws happen: tie shuffles in `speedSort`, tie
//! placement in `insertChoice`, and the `getRandomTarget` calls made while
//! resolving move actions.

use core::cmp::Ordering;

use pikapro_data::MoveId;

use super::targeting::{self, Loc};
use super::{mon_action_speed, multiturn, order, position_of_mon, switching, update_speed_of, volatiles};
use crate::battle::Battle;
use crate::format::Gimmick;
use crate::rng::BattleRng;
use crate::state::{Position, SideId};
use crate::util::StackVec;

/// A Pokémon by stable identity. Showdown actions hold the Pokémon object,
/// so an action follows its Pokémon even if team positions change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct MonRef {
    pub side: SideId,
    pub id: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum ActionKind {
    /// Team preview pick: `index` is the position in the chosen order.
    Team {
        index: u8,
    },
    /// Send out the leads.
    Start,
    /// A switch answering a forced-switch request. `from` is the move that
    /// flagged the outgoing Pokémon (U-turn), for the `[from]` annotation.
    InstaSwitch {
        target: MonRef,
        from: Option<MoveId>,
    },
    /// Start-of-turn effects.
    BeforeTurn,
    /// Revival Blessing's choice: revive the fainted `target`.
    RevivalBlessing {
        target: MonRef,
    },
    /// Showdown's `beforeTurnMove`, the `beforeTurnCallback`
    /// of a chosen move (Counter, Mirror Coat).
    BeforeTurnMove {
        move_id: MoveId,
        target_loc: Loc,
    },
    /// Showdown's `priorityChargeMove`, the
    /// `priorityChargeCallback` of a chosen move (Focus Punch, Beak Blast,
    /// Chilly Reception).
    PriorityChargeMove {
        move_id: MoveId,
        target_loc: Loc,
    },
    /// Switch-in effects for a Pokémon that just came in.
    RunSwitch,
    Switch {
        target: MonRef,
    },
    Terastallize,
    /// Showdown's `megaEvo` (`runMegaEvo`).
    MegaEvo,
    Move {
        slot: u8,
        move_id: MoveId,
        target_loc: Loc,
        gimmick: Option<Gimmick>,
    },
    /// End-of-turn effects.
    #[default]
    Residual,
}

impl ActionKind {
    /// Showdown's action `order`; lower runs first.
    pub fn order(self) -> u16 {
        match self {
            ActionKind::Team { .. } => 1,
            ActionKind::Start => 2,
            ActionKind::InstaSwitch { .. } => 3,
            ActionKind::BeforeTurn => 4,
            ActionKind::RevivalBlessing { .. } => 6,
            ActionKind::BeforeTurnMove { .. } => 5,
            ActionKind::RunSwitch => 101,
            ActionKind::Switch { .. } => 103,
            ActionKind::MegaEvo => 104,
            ActionKind::Terastallize => 106,
            ActionKind::PriorityChargeMove { .. } => 107,
            ActionKind::Move { .. } => 200,
            ActionKind::Residual => 300,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Action {
    pub kind: ActionKind,
    /// The acting Pokémon, if any.
    pub mon: Option<MonRef>,
    pub order: u16,
    pub priority: i8,
    pub speed: i32,
    /// The Pokémon at the move's target location when the action was
    /// resolved (Showdown's `originalTarget`, which
    /// `tracksTarget` moves follow).
    pub original_target: Option<MonRef>,
    /// The move that moved this action up (Showdown's
    /// `action.sourceEffect` from `prioritizeAction`: Round).
    pub source_effect: Option<MoveId>,
    /// That move's `ignoreAbility` (a Mold Breaker's Round), which this
    /// action's move takes (`move.ignoreAbility = sourceEffect.ignoreAbility`).
    pub source_ignores_ability: bool,
    /// Showdown's `fractionalPriority` in tenths (Quick Claw and
    /// Custap Berry +1, Lagging Tail, Full Incense, Stall and Mycelium
    /// Might -1), added to `priority` when sorting.
    pub fractional_priority: i8,
}

impl Action {
    pub fn new(kind: ActionKind, mon: Option<MonRef>) -> Action {
        Action { kind, mon, ..Action::default() }
    }

    /// Showdown's `comparePriority` for actions: order ascending, then
    /// priority and speed descending. `Equal` means tied.
    pub fn compare(&self, other: &Action) -> Ordering {
        let priority = |a: &Action| a.priority as i16 * 10 + a.fractional_priority as i16;
        self.order.cmp(&other.order).then(priority(other).cmp(&priority(self))).then(other.speed.cmp(&self.speed))
    }

    /// A key that orders as [`Action::compare`]: `a.sort_key() <
    /// b.sort_key()` exactly when `a.compare(&b)` is `Less`, equal exactly
    /// when it is `Equal`. The order in the top 16 bits, then the
    /// priority in tenths (`priority * 10 + fractional_priority`, within
    /// ±1,407) descending, then the speed descending.
    #[inline]
    pub fn sort_key(&self) -> u64 {
        let priority = self.priority as i32 * 10 + self.fractional_priority as i32;
        let priority = (i16::MAX as i32 - priority) as u64 & 0xffff;
        let speed = !((self.speed as u32) ^ 0x8000_0000) as u64;
        (self.order as u64) << 48 | priority << 32 | speed
    }
}

/// Upper bound on queued actions: 12 team picks + start, or a turn's
/// before-turn, 4 x (gimmick + move), residual and 4 switch-ins.
pub(crate) const QUEUE_CAPACITY: usize = 24;

/// The queued actions, front first. Shifting advances a head index instead
/// of moving the rest of the list.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Queue {
    items: StackVec<Action, QUEUE_CAPACITY>,
    head: u8,
}

/// Compares the queued actions only (not those already shifted), as
/// [`Hash`](core::hash::Hash) hashes them.
impl PartialEq for Queue {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for Queue {}

/// Hashes the queued actions only (not those already shifted).
impl core::hash::Hash for Queue {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl Queue {
    /// The queued actions, front first.
    #[inline]
    pub fn as_slice(&self) -> &[Action] {
        self.items.tail(self.head as usize)
    }

    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [Action] {
        self.items.tail_mut(self.head as usize)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.head as usize >= self.items.len()
    }

    pub fn shift(&mut self) -> Option<Action> {
        let action = *self.items.get(self.head as usize)?;
        self.head += 1;
        if self.head as usize == self.items.len() {
            self.clear();
        }
        Some(action)
    }

    #[inline]
    pub fn peek(&self) -> Option<&Action> {
        self.items.get(self.head as usize)
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.head = 0;
    }

    pub fn push(&mut self, action: Action) {
        self.make_room();
        self.items.push(action);
    }

    /// Inserts at `index` counted from the front of the queue.
    pub fn insert(&mut self, index: usize, action: Action) {
        self.make_room();
        self.items.insert(self.head as usize + index, action);
    }

    pub fn retain(&mut self, keep: impl FnMut(&Action) -> bool) {
        self.compact();
        self.items.retain(keep);
    }

    /// Showdown's `willAct`: is a move or switch still to come this turn?
    pub fn will_act(&self) -> bool {
        self.as_slice().iter().any(|a| {
            matches!(a.kind, ActionKind::Move { .. } | ActionKind::Switch { .. } | ActionKind::InstaSwitch { .. })
        })
    }

    fn make_room(&mut self) {
        if self.items.is_full() {
            self.compact();
        }
    }

    /// Drops the already-shifted prefix.
    fn compact(&mut self) {
        let head = self.head as usize;
        if head > 0 {
            let len = self.items.len();
            self.items.copy_within(head..len, 0);
            self.items.truncate(len - head);
            self.head = 0;
        }
    }
}

/// The longest list [`speed_sort`] accepts: the action queue or an event's
/// handlers (residual handlers from every mechanic across four Pokémon).
pub(crate) const SORT_CAPACITY: usize = 64;

/// Showdown's `queue.willMove(pokemon)` (`sim/battle-queue.ts`) for the
/// Pokémon at `pos`: its queued move action and the action's index in the
/// queue. `None` once it has fainted (`pokemon.fainted`, set by
/// `faintMessages`, which also empties its slot).
pub(crate) fn will_move_action(battle: &Battle, pos: Position) -> Option<(usize, Action)> {
    let mon = MonRef { side: pos.side, id: battle.state.at(pos)?.id };
    battle
        .queue
        .as_slice()
        .iter()
        .enumerate()
        .find(|(_, a)| matches!(a.kind, ActionKind::Move { .. }) && a.mon == Some(mon))
        .map(|(i, a)| (i, *a))
}

/// Whether the Pokémon at `pos` has a move action still queued
/// ([`will_move_action`]).
pub(crate) fn will_move(battle: &Battle, pos: Position) -> bool {
    will_move_action(battle, pos).is_some()
}

/// Showdown's `speedSort`: a selection sort that shuffles each group of tied
/// elements with the battle RNG, drawing exactly as Showdown does.
pub(crate) fn speed_sort<T>(list: &mut [T], rng: &mut BattleRng, compare: impl Fn(&T, &T) -> Ordering) {
    let len = list.len();
    assert!(len <= SORT_CAPACITY, "speed_sort: {len} elements exceed {SORT_CAPACITY}");
    // Most lists are a few active Pokémon or handlers: their indices fit
    // in a small array.
    if len <= 8 {
        speed_sort_in::<T, 8>(list, rng, compare);
    } else {
        speed_sort_in::<T, SORT_CAPACITY>(list, rng, compare);
    }
}

/// [`speed_sort`] of at most `N` elements.
#[inline(always)]
fn speed_sort_in<T, const N: usize>(list: &mut [T], rng: &mut BattleRng, compare: impl Fn(&T, &T) -> Ordering) {
    let len = list.len();
    // Indices of the best elements left, in list order: `next[..n]`.
    let mut next = [0u8; N];
    let mut sorted = 0;
    while sorted + 1 < len {
        next[0] = sorted as u8;
        let mut n = 1;
        for i in sorted + 1..len {
            match compare(&list[next[0] as usize], &list[i]) {
                Ordering::Less => {}
                Ordering::Greater => {
                    next[0] = i as u8;
                    n = 1;
                }
                Ordering::Equal => {
                    next[n] = i as u8;
                    n += 1;
                }
            }
        }
        for (k, &index) in next[..n].iter().enumerate() {
            if index as usize != sorted + k {
                list.swap(sorted + k, index as usize);
            }
        }
        if n > 1 {
            rng.shuffle(&mut list[sorted..sorted + n]);
        }
        sorted += n;
    }
}

/// [`speed_sort`] of Pokémon by stored speed, fastest first
/// (`speedSort(pokemon)`, whose comparison is `b.speed - a.speed`): `list`
/// holds each Pokémon's position and speed, at most four. The same
/// selection sort as [`speed_sort_in`] with `compare(a, b)` being
/// `b.1.cmp(&a.1)`, written out for the speeds: the same comparisons, swaps
/// and draws. Two Pokémon (singles' `eachEvent`) are sorted inline: the
/// slower first is swapped, a tie shuffled.
#[inline(always)]
pub(crate) fn speed_sort_by_speed(list: &mut [(Position, i32)], rng: &mut BattleRng) {
    match list {
        [] | [_] => {}
        [a, b] => {
            if b.1 > a.1 {
                core::mem::swap(a, b);
            } else if b.1 == a.1 {
                rng.shuffle(list);
            }
        }
        _ => speed_sort_by_speed_long(list, rng),
    }
}

/// The draws of [`speed_sort_by_speed`] on `list`, for a caller that
/// never reads the order (an `eachEvent` that runs no handler): the
/// selection sort shuffles each group of equal speeds, fastest group
/// first, and a shuffle's draws (`random(m)`, `random(m - 1)`, ...,
/// `random(2)` for a group of `m`) depend only on the group's size.
/// Distinct speeds draw nothing.
#[inline(always)]
pub(crate) fn speed_sort_by_speed_draws(list: &[(Position, i32)], rng: &mut BattleRng) {
    match list {
        [] | [_] => {}
        [a, b] => {
            if a.1 == b.1 {
                rng.shuffle(&mut [(); 2]);
            }
        }
        _ => speed_sort_by_speed_draws_long(list, rng),
    }
}

/// [`speed_sort_by_speed_draws`] of three or four Pokémon. Their groups
/// of equal speeds follow from the number of equal pairs: none, one pair
/// (one group of two), two pairs (two groups of two: four Pokémon), three
/// (a group of three) or six (all four). With at most one group, or two
/// of the same size, the groups' order doesn't change the draws.
#[inline(never)]
fn speed_sort_by_speed_draws_long(list: &[(Position, i32)], rng: &mut BattleRng) {
    let len = list.len();
    assert!((3..=4).contains(&len), "speed_sort_by_speed_draws: {len} Pokémon");
    #[cfg(debug_assertions)]
    let mut check_rng = rng.clone();
    let eq = |a: &(Position, i32), b: &(Position, i32)| usize::from(a.1 == b.1);
    let pairs = match list {
        [a, b, c] => eq(a, b) + eq(a, c) + eq(b, c),
        [a, b, c, d] => eq(a, b) + eq(a, c) + eq(a, d) + eq(b, c) + eq(b, d) + eq(c, d),
        _ => unreachable!("checked above"),
    };
    match pairs {
        0 => {}
        1 => rng.shuffle(&mut [(); 2]),
        2 => {
            rng.shuffle(&mut [(); 2]);
            rng.shuffle(&mut [(); 2]);
        }
        3 => rng.shuffle(&mut [(); 3]),
        _ => rng.shuffle(&mut [(); 4]),
    }
    #[cfg(debug_assertions)]
    {
        let mut copy = [(Position::default(), 0); 4];
        copy[..len].copy_from_slice(list);
        speed_sort(&mut copy[..len], &mut check_rng, |a, b| b.1.cmp(&a.1));
        debug_assert!(check_rng == *rng, "speed_sort_by_speed_draws draws as speed_sort");
    }
}

/// [`speed_sort_by_speed`] of three or four Pokémon.
#[inline(never)]
fn speed_sort_by_speed_long(list: &mut [(Position, i32)], rng: &mut BattleRng) {
    let len = list.len();
    assert!(len <= 4, "speed_sort_by_speed: {len} Pokémon");
    #[cfg(debug_assertions)]
    let (mut check, mut check_rng) = {
        let mut copy = [(Position::default(), 0); 4];
        copy[..len].copy_from_slice(list);
        (copy, rng.clone())
    };
    // Distinct speeds (the usual case) have one order fastest first, and
    // the selection sort draws nothing for them (every group is one
    // Pokémon): each Pokémon goes to its rank, the number of faster ones.
    // With two equal speeds the list goes to the selection sort untouched.
    let distinct = match list {
        [a, b, c] => sort_distinct_speeds([a, b, c]),
        [a, b, c, d] => sort_distinct_speeds([a, b, c, d]),
        _ => false,
    };
    if !distinct {
        speed_sort_by_speed_ties(list, rng);
    }
    #[cfg(debug_assertions)]
    {
        speed_sort(&mut check[..len], &mut check_rng, |a, b| b.1.cmp(&a.1));
        debug_assert_eq!(&check[..len], &*list, "speed_sort_by_speed sorts as speed_sort");
        debug_assert!(check_rng == *rng, "speed_sort_by_speed draws as speed_sort");
    }
}

/// Puts `N` Pokémon of distinct speeds in order, fastest first: each at its
/// rank (the number of faster ones), without branching on the speeds.
/// Returns `false`, the list untouched, when two speeds are equal.
#[inline(always)]
fn sort_distinct_speeds<const N: usize>(list: [&mut (Position, i32); N]) -> bool {
    let speeds: [i32; N] = core::array::from_fn(|i| list[i].1);
    let mut rank = [0usize; N];
    let mut tie = false;
    for i in 0..N {
        for j in i + 1..N {
            let (a, b) = (speeds[i], speeds[j]);
            tie |= a == b;
            rank[i] += usize::from(b > a);
            rank[j] += usize::from(a > b);
        }
    }
    if tie {
        return false;
    }
    let mut sorted = [(Position::default(), 0); N];
    for (entry, &r) in list.iter().zip(&rank) {
        // A rank is at most `N - 1`.
        sorted[r.min(N - 1)] = **entry;
    }
    for (entry, s) in list.into_iter().zip(sorted) {
        *entry = s;
    }
    true
}

/// [`speed_sort_by_speed_long`] with two equal speeds: the selection sort.
fn speed_sort_by_speed_ties(list: &mut [(Position, i32)], rng: &mut BattleRng) {
    let len = list.len();
    let mut sorted = 0;
    while sorted + 1 < len {
        // The fastest left and those tied with it, in list order.
        let mut next = [0u8; 4];
        next[0] = sorted as u8;
        let mut best = list[sorted].1;
        let mut n = 1;
        for (i, &(_, speed)) in list.iter().enumerate().skip(sorted + 1) {
            if speed > best {
                best = speed;
                next[0] = i as u8;
                n = 1;
            } else if speed == best {
                next[n] = i as u8;
                n += 1;
            }
        }
        for (k, &index) in next[..n].iter().enumerate() {
            if index as usize != sorted + k {
                list.swap(sorted + k, index as usize);
            }
        }
        if n > 1 {
            rng.shuffle(&mut list[sorted..sorted + n]);
        }
        sorted += n;
    }
}

/// Showdown's `resolveAction`: fills in order, expands a gimmick into its
/// own action (unless `mid_turn`), resolves a move's target location
/// (`getRandomTarget` when none was chosen) and computes priority and speed.
/// The actions in queue order: the extra ones, then `action`.
pub(crate) fn resolve_action(battle: &mut Battle, action: Action, mid_turn: bool) -> StackVec<Action, 4> {
    let mut extra = None;
    let action = resolve_action_parts(battle, action, mid_turn, &mut extra);
    let mut actions = extra.unwrap_or_default();
    actions.push(action);
    actions
}

/// [`resolve_action`] in parts: returns the resolved action, and leaves
/// the extra actions that go before it (in queue order) in `extra`, which
/// is only made when there are some (most actions have none).
fn resolve_action_parts(
    battle: &mut Battle,
    mut action: Action,
    mid_turn: bool,
    extra: &mut Option<StackVec<Action, 4>>,
) -> Action {
    // Showdown `unshift`s each extra action (with its own extra ones), so
    // the last one resolved comes first.
    let unshift = |extra: &mut Option<StackVec<Action, 4>>, resolved: StackVec<Action, 4>| {
        let actions = extra.get_or_insert_default();
        for &a in resolved.iter().rev() {
            actions.insert(0, a);
        }
    };
    // `if (!action.order)`: an action that brings its own order keeps it
    // (the Champions Encore's `changeAction` passes the Quashed 201).
    if action.order == 0 {
        action.order = action.kind.order();
    }
    if !mid_turn {
        // `beforeTurnMove` is resolved first.
        if let Some(before) = multiturn::before_turn_action(&action) {
            unshift(extra, resolve_action(battle, before, mid_turn));
        }
        // `megaEvo` (Sky Drop, which would stop it, is not in Gen 9).
        if let ActionKind::Move { gimmick: Some(Gimmick::MegaEvolve), .. } = action.kind {
            let mega = Action::new(ActionKind::MegaEvo, action.mon);
            unshift(extra, resolve_action(battle, mega, mid_turn));
        }
        if let ActionKind::Move { gimmick: Some(Gimmick::Terastallize), .. } = action.kind {
            let mon = action.mon.and_then(|m| mon_state(battle, m));
            if mon.is_some_and(|m| !m.terastallized) {
                let tera = Action::new(ActionKind::Terastallize, action.mon);
                unshift(extra, resolve_action(battle, tera, mid_turn));
            }
        }
        // `priorityChargeMove`, resolved (and unshifted) last.
        if let Some(charge) = multiturn::priority_charge_action(&action) {
            unshift(extra, resolve_action(battle, charge, mid_turn));
        }
        // `runEvent('FractionalPriority', pokemon, null, move, 0)` (Quick
        // Claw's roll, Custap Berry, Lagging Tail, ...), in tenths.
        if let (ActionKind::Move { move_id, .. }, Some(mon)) = (action.kind, action.mon) {
            let known = (move_id.0 as usize) < battle.format.dex.moves.len();
            if let Some(user) = position_of_mon(battle, mon) {
                let event = pikapro_data::Event::FractionalPriority;
                let relay = super::events::Relay::Num(0);
                let result = if known {
                    let data = battle.format.dex.move_(move_id);
                    super::events::move_event(battle, event, user, None, move_id, data, relay)
                } else {
                    // `recharge` is not in the dex: Showdown's move has no
                    // data (no category), but the event still runs (Quick
                    // Claw rolls).
                    let target = super::events::Target::Pokemon(user);
                    let effect = super::events::Effect::None;
                    let mut ev = super::events::EventCtx::new(event, target, None, effect, relay);
                    super::events::run_event(battle, &mut ev, None)
                };
                if let super::events::Relay::Num(n) = result {
                    action.fractional_priority = n.clamp(-10, 10) as i8;
                }
            }
        }
        if let (ActionKind::Switch { .. } | ActionKind::InstaSwitch { .. }, Some(mon)) = (action.kind, action.mon) {
            // The outgoing Pokémon's `switchFlag` is consumed; a move's flag
            // becomes the switch's `sourceEffect`.
            let flag = switching::take_switch_flag(battle, mon);
            if let ActionKind::InstaSwitch { from, .. } = &mut action.kind {
                *from = flag;
            }
        }
    }
    // The extra actions of Counter, Focus Punch, ... carry
    // the move and resolve a target the same way.
    multiturn::resolve_target_loc(battle, &mut action);
    // Champions (`BattleQueue.resolveAction`): a non-Ghost's Curse
    // aims at its user (`targetLoc = getLocOf(pokemon)`), so it draws no
    // random target, and its `originalTarget` is the user (`tracksTarget`).
    if battle.format.mechanics == crate::format::Mechanics::Champions
        && let (ActionKind::Move { move_id: pikapro_data::gen9::moves::CURSE, target_loc, .. }, Some(mon)) =
            (&mut action.kind, action.mon)
        && let Some(user) = position_of_mon(battle, mon)
    {
        let ghost = battle.state.at(user).is_some_and(|m| super::manipulation::has_type(m, pikapro_data::Type::Ghost));
        if !ghost {
            *target_loc = targeting::loc_of(user, user);
        }
    }
    if let (ActionKind::Move { slot, move_id, target_loc: 0, gimmick }, Some(mon)) = (action.kind, action.mon)
        && let Some(user) = position_of_mon(battle, mon)
    {
        let target = targeting::get_random_target(battle, user, volatiles::queued_move(battle, move_id).1);
        if let Some(t) = target {
            let target_loc = targeting::loc_of(user, t);
            action.kind = ActionKind::Move { slot, move_id, target_loc, gimmick };
        }
    }
    // `action.originalTarget = pokemon.getAtLoc(targetLoc)`.
    order::set_original_target(battle, &mut action);
    get_action_speed(battle, &mut action);
    action
}

/// Showdown's `getActionSpeed`: a move's priority (after resolving its
/// target, which can draw) and the acting Pokémon's current action speed.
///
/// A Pokémon whose slot is in `Battle::fresh_speeds` takes the speed it
/// stored: computing it again gives the same speed and draws nothing.
pub(crate) fn get_action_speed(battle: &mut Battle, action: &mut Action) {
    if let (ActionKind::Move { move_id, target_loc, .. }, Some(mon)) = (action.kind, action.mon) {
        let (mut priority, target_type) = volatiles::queued_move(battle, move_id);
        if let Some(user) = position_of_mon(battle, mon) {
            let target = order::get_target(battle, user, move_id, target_type, target_loc, None);
            // The move's own `onModifyPriority` (Grassy Glide), then
            // `runEvent('ModifyPriority', pokemon, target, move, priority)`
            // (Prankster, Gale Wings, Triage).
            priority += super::power::modify_priority(battle, user, move_id);
            if (move_id.0 as usize) < battle.format.dex.moves.len() {
                // The dex's data (`&'static`: no copy).
                let data = battle.format.dex.move_(move_id);
                let event = pikapro_data::Event::ModifyPriority;
                let relay = super::events::Relay::Num(priority as i32);
                if let super::events::Relay::Num(n) =
                    super::events::move_event(battle, event, user, target, move_id, data, relay)
                {
                    priority = n as i8;
                }
            }
        }
        action.priority = priority;
    }
    action.speed = match action.mon {
        Some(m) => match position_of_mon(battle, m) {
            Some(pos) if battle.fresh_speeds & (1 << (pos.side.index() * 2 + pos.slot as usize)) != 0 => {
                let stored = battle.state.side(pos.side).team[pos.slot as usize].speed;
                // The speed handlers only compute: computing it again (in
                // tests) must give the same speed, sorting and drawing
                // nothing.
                #[cfg(debug_assertions)]
                {
                    let (rng, sorts) = (battle.rng.clone(), battle.handler_sorts);
                    let again = mon_action_speed(battle, m);
                    assert!(
                        again == stored && battle.handler_sorts == sorts && battle.rng == rng,
                        "a fresh speed differs from its recomputation"
                    );
                }
                stored
            }
            _ => mon_action_speed(battle, m),
        },
        None => 1,
    };
}

/// Showdown's `addChoice`: resolve and append.
pub(crate) fn add_choice(battle: &mut Battle, action: Action) {
    let mut extra = None;
    let action = resolve_action_parts(battle, action, false, &mut extra);
    for &a in extra.iter().flat_map(|e| e.iter()) {
        battle.queue.push(a);
    }
    battle.queue.push(action);
}

/// Showdown's `insertChoice`: resolve and insert where the action would
/// have been sorted, without re-sorting the rest. Among tied actions the
/// position is random (a draw).
pub(crate) fn insert_choice(battle: &mut Battle, action: Action, mid_turn: bool) {
    let fresh = match action.mon {
        Some(mon) => update_speed_of(battle, mon),
        None => 0,
    };
    let (extra, action) = super::with_fresh_speeds(battle, fresh, |battle| {
        let mut extra = None;
        let action = resolve_action_parts(battle, action, mid_turn, &mut extra);
        (extra, action)
    });
    // The first of the actions in queue order.
    let head = extra.as_ref().and_then(|e| e.first()).unwrap_or(&action);
    let mut first = None;
    let mut last = None;
    for (i, current) in battle.queue.as_slice().iter().enumerate() {
        let compared = head.compare(current);
        if compared != Ordering::Greater && first.is_none() {
            first = Some(i);
        }
        if compared == Ordering::Less {
            last = Some(i);
            break;
        }
    }
    let index = match first {
        None => battle.queue.as_slice().len(),
        Some(first) => {
            let last = last.unwrap_or(battle.queue.as_slice().len());
            if first == last { first } else { battle.rng.random_range(first as u32, last as u32 + 1) as usize }
        }
    };
    for (k, a) in extra.iter().flat_map(|e| e.iter()).chain([&action]).enumerate() {
        battle.queue.insert(index + k, *a);
    }
}

/// Showdown's `queue.sort()`.
///
/// Actions that never compare `Equal` have one order, and the selection
/// sort draws nothing for them (every group is one action): any sort that
/// finds it is the same sort. Up to eight actions (a turn's queue) are
/// sorted by a key that orders as [`Action::compare`] does
/// ([`Action::sort_key`]: equal keys exactly when `Equal`): an insertion
/// sort of the keys, which leaves equal keys next to each other; with
/// none, the actions are put in that order (nothing moves when they
/// already are), and with any, the list goes to `speed_sort` untouched.
/// Debug builds check every such sort against `speed_sort`.
pub(crate) fn sort(battle: &mut Battle) {
    let list = battle.queue.as_mut_slice();
    let len = list.len();
    if !(2..=8).contains(&len) {
        speed_sort(list, &mut battle.rng, Action::compare);
        return;
    }
    #[cfg(debug_assertions)]
    let (check, check_rng) = {
        let mut copy = [Action::default(); 8];
        copy[..len].copy_from_slice(list);
        let mut rng = battle.rng.clone();
        speed_sort(&mut copy[..len], &mut rng, Action::compare);
        (copy, rng)
    };
    let mut keys = [(0u64, 0u8); 8];
    for (i, a) in list.iter().enumerate() {
        keys[i] = (a.sort_key(), i as u8);
    }
    let mut tie = false;
    let mut moved = false;
    for i in 1..len {
        let entry = keys[i];
        let mut j = i;
        while j > 0 && keys[j - 1].0 > entry.0 {
            keys[j] = keys[j - 1];
            j -= 1;
        }
        if j > 0 && keys[j - 1].0 == entry.0 {
            tie = true;
            break;
        }
        moved |= j != i;
        keys[j] = entry;
    }
    if tie {
        speed_sort(list, &mut battle.rng, Action::compare);
    } else if moved {
        let mut copy = [Action::default(); 8];
        copy[..len].copy_from_slice(list);
        for (action, &(_, i)) in list.iter_mut().zip(&keys[..len]) {
            *action = copy[i as usize];
        }
    }
    #[cfg(debug_assertions)]
    {
        assert_eq!(&check[..len], battle.queue.as_slice(), "queue::sort sorts as speed_sort");
        assert!(check_rng == battle.rng, "queue::sort draws as speed_sort");
    }
}

fn mon_state(battle: &Battle, mon: MonRef) -> Option<&crate::state::PokemonState> {
    let side = battle.state.side(mon.side);
    side.position_of(mon.id).map(|i| &side.team[i])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Seed;

    #[test]
    fn speed_sort_orders_and_only_draws_on_ties() {
        let mut rng = BattleRng::new(Seed::Fast(1));
        let mut untouched = rng.clone();
        let mut v = [3, 1, 2];
        speed_sort(&mut v, &mut rng, |a: &i32, b: &i32| b.cmp(a));
        assert_eq!(v, [3, 2, 1]);
        assert_eq!(rng.random(1000), untouched.random(1000), "no ties, no draws");

        let mut ties = [(5, 'a'), (5, 'b'), (1, 'c')];
        speed_sort(&mut ties, &mut rng, |a, b| b.0.cmp(&a.0));
        assert_eq!(ties[2], (1, 'c'));
    }

    /// [`speed_sort_by_speed`] orders and draws exactly as `speed_sort`
    /// by speed, over every list of up to four of four speeds (all
    /// distinct, or with ties).
    #[test]
    fn speed_sort_by_speed_is_speed_sort() {
        for len in 0..=4u32 {
            for code in 0..4u32.pow(len) {
                let mut list = [(Position::default(), 0); 4];
                for (i, entry) in list.iter_mut().enumerate().take(len as usize) {
                    let speed = [i32::MIN, 80, 81, i32::MAX][(code / 4u32.pow(i as u32) % 4) as usize];
                    *entry = (Position::new(SideId::BOTH[i / 2], (i % 2) as u8), speed);
                }
                for seed in 0..20 {
                    let (mut a, mut b) = (BattleRng::new(Seed::Fast(seed)), BattleRng::new(Seed::Fast(seed)));
                    let (mut x, mut y) = (list, list);
                    speed_sort(&mut x[..len as usize], &mut a, |p, q| q.1.cmp(&p.1));
                    let mut c = BattleRng::new(Seed::Fast(seed));
                    speed_sort_by_speed(&mut y[..len as usize], &mut b);
                    speed_sort_by_speed_draws(&list[..len as usize], &mut c);
                    assert_eq!(x, y);
                    let next = a.random(1 << 16);
                    assert_eq!(next, b.random(1 << 16), "the same draws");
                    assert_eq!(next, c.random(1 << 16), "the same draws without the order");
                }
            }
        }
    }

    /// [`Action::sort_key`] orders every pair of actions as
    /// [`Action::compare`] does, ties included, at the ends of each field.
    #[test]
    fn action_sort_key_orders_as_compare() {
        let orders = [0u16, 1, 103, 200, 300, u16::MAX];
        let priorities = [i8::MIN, -7, -1, 0, 1, 5, i8::MAX];
        let fractions = [i8::MIN, -1, 0, 1, i8::MAX];
        let speeds = [i32::MIN, -1, 0, 1, 80, 81, i32::MAX];
        let mut actions = Vec::new();
        for &order in &orders {
            for &priority in &priorities {
                for &fractional_priority in &fractions {
                    for &speed in &speeds {
                        actions.push(Action { order, priority, fractional_priority, speed, ..Action::default() });
                    }
                }
            }
        }
        for a in &actions {
            for b in &actions {
                assert_eq!(a.sort_key().cmp(&b.sort_key()), a.compare(b), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn queue_shift_insert_retain_and_compaction() {
        let action = |speed| Action { speed, ..Action::default() };
        let speeds = |q: &Queue| q.as_slice().iter().map(|a| a.speed).collect::<Vec<_>>();
        let mut q = Queue::default();
        for s in 0..4 {
            q.push(action(s));
        }
        assert_eq!(q.shift().map(|a| a.speed), Some(0));
        assert_eq!(speeds(&q), [1, 2, 3]);
        q.insert(1, action(9));
        assert_eq!(speeds(&q), [1, 9, 2, 3]);
        q.retain(|a| a.speed != 2);
        assert_eq!(speeds(&q), [1, 9, 3]);
        // Filling past capacity reclaims the shifted prefix.
        while q.shift().is_some() {}
        assert!(q.is_empty());
        for round in 0..3 {
            for s in 0..QUEUE_CAPACITY as i32 - 1 {
                q.push(action(s));
            }
            q.shift();
            q.push(action(-1));
            q.push(action(-2));
            assert_eq!(q.as_slice().len(), QUEUE_CAPACITY, "round {round}");
            q.clear();
        }
    }

    #[test]
    fn action_order_follows_showdown() {
        let order = |k: ActionKind| Action { order: k.order(), ..Action::new(k, None) };
        let switch = order(ActionKind::Switch { target: MonRef::default() });
        let tera = order(ActionKind::Terastallize);
        assert_eq!(switch.compare(&tera), Ordering::Less);
        let fast = Action { speed: 200, ..tera };
        assert_eq!(fast.compare(&tera), Ordering::Less);
        let high_priority = Action { priority: 1, order: 200, ..Action::default() };
        let quick = Action { speed: 999, order: 200, ..Action::default() };
        assert_eq!(high_priority.compare(&quick), Ordering::Less);
    }
}
