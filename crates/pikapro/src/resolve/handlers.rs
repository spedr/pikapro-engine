//! Showdown's event-handler ordering without its dynamic dispatch.
//!
//! Showdown collects every handler for an event (`findEventHandlers`),
//! attaches ordering data (`resolvePriority`) and speed-sorts them, which
//! draws random numbers on ties. Here each mechanics module contributes
//! [`Handler`]s (a function pointer plus the same ordering data) for the
//! events it takes part in, and the event runners below sort and run them.
//!
//! Adding a handler for an existing event means adding it in the module's
//! `*_handlers` collector with Showdown's `on<Event>Order` /
//! `on<Event>Priority` / `on<Event>SubOrder` values. A new event needs a
//! runner here and a hook at Showdown's call site.

use core::cmp::Ordering;

use pikapro_data::{Event, EventPrefix, MoveId};

pub(crate) use super::events::{Callback, Effect};
use super::queue::speed_sort;
use super::{events, faint_messages, field, multiturn, power, protect, status, volatiles};
use crate::battle::Battle;
use crate::state::{Position, SideId};

/// Where an effect lives (Showdown's `effectHolder`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Holder {
    // First: the default is all zeros (handler lists start zeroed).
    #[default]
    Field,
    Pokemon(Position),
    Side(SideId),
    /// A slot condition of the Pokémon in this position (collected with
    /// its handlers, after its volatiles). Unlike the Pokémon's own
    /// effects, it still runs while that Pokémon is fainted.
    Slot(Position),
    /// A side condition's per-Pokémon handler (Showdown's `customHolder`:
    /// hazards on switch-in, Fire Pledge's damage), collected after the
    /// Pokémon's own handlers.
    SideOn(Position),
    /// A field effect's per-Pokémon handler (Grassy Terrain's healing),
    /// collected after the side's.
    FieldOn(Position),
}

impl Holder {
    /// The Pokémon whose fainting stops this handler (Showdown skips a
    /// handler whose `effectHolder.fainted`, except for slot conditions).
    fn fainted_pokemon(self) -> Option<Position> {
        match self {
            Holder::Pokemon(pos) | Holder::SideOn(pos) | Holder::FieldOn(pos) => Some(pos),
            Holder::Side(_) | Holder::Field | Holder::Slot(_) => None,
        }
    }
}

/// Runs the handler. For events that can be stopped (`BeforeMove`),
/// returning false stops the event. For `Residual`, returning false means
/// the handler's effect ran out or was gone, so Showdown `continue`s past
/// the `faintMessages` that follows every other handler.
pub(crate) type HandlerFn = fn(&mut Battle, &Handler) -> bool;

/// Showdown's `effectTypeOrder`: the default `subOrder` by effect type.
pub(crate) mod sub_order {
    /// Non-volatile status (`effectType: 'Status'` has no entry, so 0).
    pub const STATUS: u8 = 0;
    /// Volatile conditions.
    pub const CONDITION: u8 = 2;
    pub const SLOT_CONDITION: u8 = 3;
    pub const SIDE_CONDITION: u8 = 4;
    /// Weather, terrain and pseudo-weather.
    pub const FIELD_CONDITION: u8 = 5;
    pub const ABILITY: u8 = 7;
    pub const ITEM: u8 = 8;
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Handler {
    /// `on<Event>Order`; `None` sorts after every ordered handler
    /// (Showdown's `false`).
    pub order: Option<u8>,
    /// `on<Event>Priority`; higher runs first.
    pub priority: i32,
    /// The holder's stored speed (`effectHolder.speed`); higher runs first.
    pub speed: i32,
    /// `on<Event>SubOrder`, or the effect type's default ([`sub_order`]).
    pub sub_order: u8,
    /// `effectOrder` (only switch-in and redirection events use it).
    pub effect_order: u32,
    pub holder: Holder,
    /// The volatile this handler belongs to ([`vol`] of its
    /// [`Volatiles`](crate::state::Volatiles) flag: its bit index plus
    /// one), or 0; [`Handler::volatile`] gives the flag. A byte rather
    /// than the `u128` flag keeps a handler 8-byte aligned and small: the
    /// lists are copied and set up for every event.
    /// A Pokémon's handlers are collected status first, then volatiles in
    /// the order they were added.
    pub volatile_bit: u8,
    /// Free for the owning module (e.g. which of several effects).
    pub arg: u8,
    /// What the handler runs (nothing, a plain or relay handler, an
    /// ability's or an item's callback). See `events.rs`.
    pub callback: Callback,
    /// The effect the handler belongs to (Showdown's
    /// `handler.effect`), for the handlers `events.rs` collects.
    pub effect: Effect,
    /// For an event run on several targets (`TryHit`,
    /// `DamagingHit`, ...), which target it was collected for (Showdown's
    /// `handler.index`). The residual's terrain handler keeps the turns
    /// the terrain had left when collected.
    pub index: u8,
}

impl Default for Handler {
    #[inline]
    fn default() -> Self {
        Handler::EMPTY
    }
}

impl Handler {
    /// [`Handler::default`], as a constant.
    pub(crate) const EMPTY: Handler = Handler {
        order: None,
        priority: 0,
        speed: 0,
        sub_order: 0,
        effect_order: 0,
        holder: Holder::Field,
        volatile_bit: 0,
        arg: 0,
        callback: Callback::Sort,
        effect: Effect::None,
        index: 0,
    };

    /// The [`Volatiles`](crate::state::Volatiles) flag this handler
    /// belongs to, or 0.
    #[inline]
    pub fn volatile(&self) -> u128 {
        match self.volatile_bit {
            0 => 0,
            b => 1 << (b - 1),
        }
    }
}

/// [`Handler::volatile_bit`] for the volatile `flag` (one bit), or 0 for
/// none.
#[inline]
pub(crate) const fn vol(flag: u128) -> u8 {
    if flag == 0 {
        return 0;
    }
    assert!(flag.is_power_of_two(), "a handler belongs to one volatile");
    flag.trailing_zeros() as u8 + 1
}

const _: () = assert!(size_of::<Handler>() <= 48 && align_of::<Handler>() == 8);

impl Handler {
    /// Showdown's `comparePriority`.
    pub fn compare(&self, other: &Handler) -> Ordering {
        let order = |h: &Handler| h.order.map_or(u64::MAX, u64::from);
        order(self)
            .cmp(&order(other))
            .then(other.priority.cmp(&self.priority))
            .then(other.speed.cmp(&self.speed))
            .then(self.sub_order.cmp(&other.sub_order))
            .then(self.effect_order.cmp(&other.effect_order))
    }
}

/// The handlers of a `fieldEvent` (`Residual`, `SwitchIn`): every active
/// Pokémon's, the sides' and the field's. Room for 16 before it spills: a
/// turn's residual handlers are often more than a few.
pub(crate) type Handlers = SpillList<{ super::queue::SORT_CAPACITY }, 16>;

/// A list of at most `N` handlers that starts with room for `I` and moves
/// to its full capacity only when it outgrows them: an event usually
/// collects a few handlers, and setting up 32 or 64 (1.5 or 3 KiB) cost
/// more than collecting them; so did setting up 8 for every event, most of
/// which collect fewer than 4. Derefs to a slice.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpillList<const N: usize, const I: usize = 4> {
    len: u8,
    inline: [Handler; I],
    /// All the handlers, once there are more than `I`.
    spilled: Option<[Handler; N]>,
}

impl<const N: usize, const I: usize> SpillList<N, I> {
    #[inline(always)]
    pub(crate) fn new() -> Self {
        const { assert!(I <= N && N <= u8::MAX as usize) };
        SpillList { len: 0, inline: [Handler::EMPTY; I], spilled: None }
    }

    /// Appends `handler`. Panics if full (as `StackVec::push`).
    #[inline]
    pub(crate) fn push(&mut self, handler: Handler) {
        let len = self.len as usize;
        match &mut self.spilled {
            Some(all) => {
                assert!(len < N, "SpillList capacity {N} exceeded");
                all[len] = handler;
            }
            None if len < I => self.inline[len] = handler,
            None => self.spill(handler),
        }
        self.len += 1;
    }

    /// Moves to the full capacity and appends `handler`.
    #[cold]
    #[inline(never)]
    fn spill(&mut self, handler: Handler) {
        let mut all = [Handler::EMPTY; N];
        all[..I].copy_from_slice(&self.inline);
        all[I] = handler;
        self.spilled = Some(all);
    }
}

impl<const N: usize, const I: usize> core::ops::Deref for SpillList<N, I> {
    type Target = [Handler];
    #[inline]
    fn deref(&self) -> &[Handler] {
        let len = self.len as usize;
        match &self.spilled {
            Some(all) => &all[..len],
            None => &self.inline[..len],
        }
    }
}

impl<const N: usize, const I: usize> core::ops::DerefMut for SpillList<N, I> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [Handler] {
        let len = self.len as usize;
        match &mut self.spilled {
            Some(all) => &mut all[..len],
            None => &mut self.inline[..len],
        }
    }
}

/// The handlers of an event on one Pokémon (`BeforeMove`, `ModifyMove`,
/// `DisableMove`, `TrapPokemon`): a smaller list.
pub(crate) type MonHandlers = SpillList<32>;

impl<'a, const N: usize, const I: usize> IntoIterator for &'a SpillList<N, I> {
    type Item = &'a Handler;
    type IntoIter = core::slice::Iter<'a, Handler>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Showdown's `fieldEvent('Residual')`: collect, sort, then run each
/// handler, reporting faints after each one.
///
/// Handlers own their effect's duration: a handler for an effect with a
/// duration counts it down and ends the effect at zero instead of running
/// (Showdown decrements `duration` before calling `onResidual`). A handler
/// must also check that its effect is still present, since an earlier
/// handler may have removed it. Faints are reported after each handler,
/// except one that returns false (its effect ran out or was gone).
pub(crate) fn run_residual(battle: &mut Battle) {
    let mut handlers = Handlers::new();
    protect::residual_handlers(battle, &mut handlers);
    status::residual_handlers(battle, &mut handlers);
    volatiles::residual_handlers(battle, &mut handlers);
    field::residual_handlers(battle, &mut handlers);
    super::manipulation::residual_handlers(battle, &mut handlers);
    power::residual_handlers(battle, &mut handlers);
    multiturn::residual_handlers(battle, &mut handlers);
    super::order::residual_handlers(battle, &mut handlers);
    // Abilities, items and the event framework's conditions.
    for side in SideId::BOTH {
        let team = &battle.state.side(side).team;
        for slot in 0..battle.format.active_per_side().min(team.len()) {
            // (After a stale switch, the fainted Pokémon twice.)
            let pos = Position::new(side, super::field_index(battle, side, slot) as u8);
            let speed = team[pos.slot as usize].speed;
            events::field_event_handlers(battle, Event::Residual, pos, EventPrefix::On, speed, &mut handlers);
        }
    }
    sort_by_holder(battle, &mut handlers);
    speed_sort(&mut handlers, &mut battle.rng, Handler::compare);
    for handler in handlers.iter() {
        // Handlers on a fainted Pokémon are skipped (slot conditions' are
        // held by the slot).
        if let Some(pos) = handler.holder.fainted_pokemon()
            && battle.state.side(pos.side).active[pos.slot as usize].is_none()
        {
            continue;
        }
        // A handler whose duration ran out returns false: Showdown ends the
        // effect and `continue`s without `faintMessages`.
        let ran = events::run_field_handler(battle, Event::Residual, handler);
        if ran && faint_messages(battle, true) {
            return;
        }
    }
}

/// Showdown's `fieldEvent('SwitchIn', switchers)` from `runSwitch`: every
/// active Pokémon's `onAnySwitchIn` handlers, and the switch-in handlers of
/// the Pokémon that just came in (`switchers`): their abilities' and
/// items' (`onSwitchIn`, or `onStart`), slot conditions, then the entry
/// hazards on their side, sorted and run with faints reported after each.
/// `speed_order` is `runSwitch`'s speed-sorted list of the active Pokémon
/// (Showdown's `speedOrder`): each holder's speed is lowered by its place
/// in it over `2 * activePerHalf` (speeds are scaled by 4 to keep that
/// exact), which breaks speed ties without a draw.
pub(crate) fn run_switch_in(battle: &mut Battle, switchers: &[Position], speed_order: &[Position]) {
    let mut handlers = Handlers::new();
    let per_half = battle.format.active_per_side() as i32;
    for side in SideId::BOTH {
        let team_len = battle.state.side(side).team.len();
        for slot in 0..battle.format.active_per_side().min(team_len) {
            // `side.active[slot]`: after a stale switch, the fainted Pokémon
            // in the other slot as well (`OrderState::stale_active`), whose
            // handlers are collected twice.
            let pos = Position::new(side, super::field_index(battle, side, slot) as u8);
            let index = speed_order.iter().position(|&p| p == pos).unwrap_or(0) as i32;
            let speed = battle.state.side(side).team[pos.slot as usize].speed * 4 - index * 2 / per_half;
            events::field_event_handlers(battle, Event::SwitchIn, pos, EventPrefix::Any, speed, &mut handlers);
            if !switchers.contains(&pos) {
                continue;
            }
            events::field_event_handlers(battle, Event::SwitchIn, pos, EventPrefix::On, speed, &mut handlers);
            super::slots::switch_in_handlers(battle, pos, speed, &mut handlers);
            super::hazards::switch_in_handlers(battle, pos, speed, &mut handlers);
        }
    }
    speed_sort(&mut handlers, &mut battle.rng, Handler::compare);
    for handler in handlers.iter() {
        if let Some(pos) = handler.holder.fainted_pokemon()
            && battle.state.side(pos.side).active[pos.slot as usize].is_none()
        {
            continue;
        }
        let ran = events::run_field_handler(battle, Event::SwitchIn, handler);
        if ran && faint_messages(battle, true) {
            return;
        }
    }
}

/// Puts handlers in the order Showdown collects them: field effects, then
/// per side its side conditions followed by each active Pokémon's handlers
/// (`findPokemonEventHandlers`: status, then volatiles in the order they
/// were added, ability, item, then `side.slotConditions[position]` in the
/// order they were added: Wish, Future Sight, ...). `speedSort` swaps
/// elements as it goes, so this starting order decides how ties are
/// shuffled even between handlers that don't tie with each other.
fn sort_by_holder(battle: &Battle, handlers: &mut [Handler]) {
    let rank = |h: &Handler| {
        let holder = match h.holder {
            Holder::Field => 0,
            Holder::Side(side) => 1 + 3 * side.index(),
            Holder::Pokemon(pos) | Holder::Slot(pos) | Holder::SideOn(pos) | Holder::FieldOn(pos) => {
                2 + 3 * pos.side.index() + pos.slot as usize
            }
        };
        // Per Pokémon: status, volatiles in the order they were added,
        // ability, item, slot conditions, then the side's and the field's
        // handlers for it.
        let within = match h.holder {
            Holder::Pokemon(pos) if h.volatile() != 0 => battle
                .state
                .at(pos)
                .and_then(|m| m.volatiles.in_order().position(|v| v == h.volatile()))
                .map_or(usize::MAX - 6, |i| i + 1),
            Holder::Pokemon(_) if matches!(h.effect, Effect::Ability(_)) => usize::MAX - 5,
            Holder::Pokemon(_) if matches!(h.effect, Effect::Item(_)) => usize::MAX - 4,
            Holder::Pokemon(_) if matches!(h.effect, Effect::Slot(_)) => usize::MAX - 2,
            Holder::Slot(_) => usize::MAX - 2,
            Holder::SideOn(_) => usize::MAX - 1,
            Holder::FieldOn(_) => usize::MAX,
            _ => 0,
        };
        // Among a slot's conditions: the order they were added in.
        let slot = match (h.holder, h.effect) {
            (Holder::Pokemon(pos) | Holder::Slot(pos), Effect::Slot(c)) => battle
                .state
                .side(pos.side)
                .conditions
                .slots
                .get(pos.slot as usize)
                .and_then(|s| s.order.iter().position(|&o| o == c))
                .unwrap_or(0),
            _ => 0,
        };
        (holder, within, slot)
    };
    // Each handler's rank once (a volatile's is a search of the
    // volatiles), as one key that orders as the triple does.
    let key = |h: &Handler| {
        let (holder, within, slot) = rank(h);
        (holder as u128) << 80 | (within as u128) << 16 | slot as u128
    };
    if handlers.len() < 2 {
        return;
    }
    if handlers.len() <= 4 {
        insertion_sort_by_key::<4>(handlers, key);
    } else if handlers.len() <= 16 {
        insertion_sort_by_key::<16>(handlers, key);
    } else {
        insertion_sort_by_key::<{ super::queue::SORT_CAPACITY }>(handlers, key);
    }
}

/// Stable insertion sort of at most `N` handlers by `key`, each key made
/// once: the lists are short and must not allocate.
#[inline(always)]
fn insertion_sort_by_key<const N: usize>(handlers: &mut [Handler], key: impl Fn(&Handler) -> u128) {
    let mut keys = [0u128; N];
    for (k, h) in keys.iter_mut().zip(handlers.iter()) {
        *k = key(h);
    }
    for i in 1..handlers.len() {
        let mut j = i;
        while j > 0 && keys[j - 1] > keys[j] {
            handlers.swap(j - 1, j);
            keys.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// Showdown's `runEvent('BeforeMove', user, target, move)`. Returns false
/// if a handler stops the move (flinch, sleep, full paralysis, Truant,
/// a Choice lock, ...).
///
/// The move being used is in `battle.active_move`. The user's status and
/// volatiles (`status`, `volatiles` and `multiturn`'s handlers), then the event framework's (the
/// user's ability, item and registry volatiles, the foes' `onFoeBeforeMove`
/// and the target's `onSourceBeforeMove`), then the field's (Gravity), in
/// one sort.
///
/// Returns the event's relay: `True` if the move goes on, else what the
/// handler that stopped it returned (`False`, or `mustrecharge`'s `Null`).
pub(crate) fn run_before_move(
    battle: &mut Battle,
    user: Position,
    target: Option<Position>,
    move_id: MoveId,
) -> events::Relay {
    let mut handlers = MonHandlers::new();
    status::before_move_handlers(battle, user, &mut handlers);
    volatiles::before_move_handlers(battle, user, &mut handlers);
    multiturn::before_move_handlers(battle, user, &mut handlers);
    sort_by_holder(battle, &mut handlers);
    events::collect_into(battle, Event::BeforeMove, events::Target::Pokemon(user), target, &mut handlers);
    // `runEvent` collects the field's handlers (Gravity) after the Pokémon's.
    field::before_move_handlers(battle, &mut handlers);
    // No handler (most moves): nothing to sort or run, and `runEvent`'s
    // relay stays true.
    if handlers.is_empty() {
        return events::Relay::True;
    }
    speed_sort(&mut handlers, &mut battle.rng, Handler::compare);
    // `recharge` is not a dex move.
    // The dex's data (`&'static`: no copy).
    let dex = battle.format.dex;
    let data = dex.moves.get(move_id.0 as usize);
    let effect = if data.is_some() { Effect::Move(move_id) } else { Effect::None };
    let mut ev =
        events::EventCtx::new(Event::BeforeMove, events::Target::Pokemon(user), target, effect, events::Relay::True);
    if let Some(data) = data {
        ev = ev.with_move(data);
    }
    events::run_list(battle, &mut ev, &handlers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn showdown_compare_priority() {
        let h = |order, priority, speed, sub_order| Handler { order, priority, speed, sub_order, ..Handler::default() };
        // Ordered handlers come before unordered ones.
        assert_eq!(h(Some(10), 0, 0, 0).compare(&h(None, 5, 500, 0)), Ordering::Less);
        assert_eq!(h(Some(1), 0, 0, 0).compare(&h(Some(10), 0, 0, 0)), Ordering::Less);
        // Then priority and speed descending, then sub-order ascending.
        assert_eq!(h(None, 5, 0, 0).compare(&h(None, 1, 999, 0)), Ordering::Less);
        assert_eq!(h(None, 0, 200, 9).compare(&h(None, 0, 100, 0)), Ordering::Less);
        assert_eq!(h(None, 0, 100, 2).compare(&h(None, 0, 100, 4)), Ordering::Less);
        assert_eq!(h(None, 0, 100, 2).compare(&h(None, 0, 100, 2)), Ordering::Equal);
    }

    /// A [`MonHandlers`] (`SpillList<32>`, four inline) past its inline
    /// handlers: every length up to its capacity keeps the push order,
    /// and `speed_sort` over it orders and draws exactly as over a plain
    /// array of the same handlers.
    #[test]
    fn mon_handlers_spill_keeps_order_and_sorts_like_an_array() {
        use crate::rng::{BattleRng, Seed};
        // Speeds with ties (`i % 5`), so the sort shuffles and draws.
        let handler = |i: usize| Handler { speed: (i % 5) as i32, arg: i as u8, ..Handler::default() };
        for n in 1..=32 {
            let mut list = MonHandlers::new();
            let mut array = Vec::new();
            for i in 0..n {
                list.push(handler(i));
                array.push(handler(i));
                assert_eq!(list.len(), i + 1);
            }
            let args = |hs: &[Handler]| hs.iter().map(|h| h.arg).collect::<Vec<_>>();
            assert_eq!(args(&list), (0..n as u8).collect::<Vec<_>>(), "{n} handlers in push order");
            // A copy is independent of the original (the list is `Copy`).
            let copy = list;
            list[0].arg = u8::MAX;
            assert_eq!(copy[0].arg, 0);
            list[0].arg = 0;

            let (mut a, mut b) = (BattleRng::new(Seed::Fast(n as u64)), BattleRng::new(Seed::Fast(n as u64)));
            speed_sort(&mut list, &mut a, Handler::compare);
            speed_sort(&mut array, &mut b, Handler::compare);
            assert_eq!(args(&list), args(&array), "{n} handlers sorted");
            assert!(list.windows(2).all(|w| w[0].speed >= w[1].speed));
            assert_eq!(a.random(1 << 16), b.random(1 << 16), "{n} handlers: the same draws");
        }
    }

    /// [`Handlers`] (`SpillList<64, 16>`): every length up to its
    /// capacity, across its spill, keeps the push order.
    #[test]
    fn field_handlers_spill_keeps_order() {
        for n in 1..=64 {
            let mut list = Handlers::new();
            for i in 0..n {
                list.push(Handler { arg: i as u8, ..Handler::default() });
            }
            let args: Vec<u8> = list.iter().map(|h| h.arg).collect();
            assert_eq!(args, (0..n as u8).collect::<Vec<_>>(), "{n} handlers in push order");
        }
    }

    #[test]
    #[should_panic(expected = "SpillList capacity 32 exceeded")]
    fn mon_handlers_panic_past_their_capacity() {
        let mut list = MonHandlers::new();
        for _ in 0..33 {
            list.push(Handler::default());
        }
    }
}
