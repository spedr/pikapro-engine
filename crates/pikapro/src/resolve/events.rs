//! Showdown's event system: `runEvent`, `singleEvent` and `priorityEvent`
//! (`sim/battle.ts`), for abilities, items and the conditions whose
//! handlers sort together with theirs.
//!
//! # How an event runs
//!
//! [`run_event`] is Showdown's `runEvent(event, target, source, effect,
//! relay)`:
//!
//! 1. **Collect** (`findEventHandlers`): the target's own handlers
//!    (`on<Event>`: status, volatiles in the order they were added,
//!    ability, item, slot conditions), then for each ally including itself
//!    `onAlly<Event>` and `onAny<Event>`, for each foe `onFoe<Event>` and
//!    `onAny<Event>`, then the source's `onSource<Event>`, then the side
//!    conditions (`on<Event>` of the target's side, `onFoe<Event>` of the
//!    other, `onAny<Event>` of both) and the field's `on<Event>`.
//! 2. **Sort** (`resolvePriority` + `speedSort`): order, priority, holder
//!    speed, sub-order (status 0, volatile 2, slot 3, side 4, field 5,
//!    ability 7, item 8), `effectOrder`; ties are shuffled with the battle
//!    RNG. `TryHit`, `DamagingHit` and `Invulnerability` sort left to right
//!    instead, and [`priority_event`] by `compareRedirectOrder` (no draws).
//! 3. **Run**: each handler gets the relay value ([`Relay`]) and may
//!    return a new one; a falsy result stops the event. Suppressed handlers
//!    are skipped here, not when collecting (so they still take part in the
//!    sort and its ties): Mold Breaker (`breakable` abilities), Gastro
//!    Acid / Neutralizing Gas (`ignoringAbility`), Klutz / Magic Room
//!    (`ignoringItem`), Air Lock (weather handlers).
//! 4. **Modifier**: a numeric result is multiplied by the chained
//!    `chainModify` modifier ([`EventCtx::chain_modify`]), as
//!    `runEvent` does at its end.
//!
//! # Where handlers come from
//!
//! - Abilities and items: their generated [`pikapro_data::EventHandler`]s
//!   (with Showdown's `on<Callback>Order` / `Priority` / `SubOrder`), run by
//!   `abilities::run` and `items::run`, which dispatch to the module that
//!   implements each one.
//! - Conditions: the registries below ([`status_spec`], [`volatile_spec`],
//!   [`side_spec`], [`field_spec`]) name each condition's handler for an
//!   event and the function that implements it (in the condition's own
//!   module). The volatiles of bit 64 and up are looked up by bit range in
//!   the ability and item modules.
//!
//! Ported events: every modifier event (`BasePower`, `Modify*`,
//! `WeatherModifyDamage`, `ModifySTAB`, `Effectiveness`, ...), and the
//! events where conditions and abilities or items meet (`BeforeMove`,
//! `SetStatus`, `TryAddVolatile`, `TryBoost`, `TryHit`, `DamagingHit`,
//! `Damage`, `Update`, `DisableMove`, `TrapPokemon`, ...). `Residual` and
//! `SwitchIn` are Showdown's `fieldEvent` (`handlers.rs`).

use core::cmp::Ordering;

use pikapro_data::{
    AbilityId, BoostId, Dex, Event, EventHandler, EventPrefix, ItemId, MoveData, MoveId, Stat, StatBoosts, StatusId,
    Type,
};

use super::handlers::{Handler, HandlerFn, Holder, sub_order, vol};
use super::queue::speed_sort;
use super::{abilities, conditions, field, items, manipulation, multiturn, order, status};
use crate::battle::Battle;
use crate::state::{
    PokemonState, Position, PseudoWeather, SideCondition, SideId, SlotCondition, Status, Terrain, Volatiles, Weather,
};
use crate::util::StackVec;

// ---------------------------------------------------------------------------
// Relay values and effects.

/// Showdown's relay variable: what `runEvent` passes from handler to
/// handler and returns. A handler returns `None` for `undefined` (keep the
/// value) or `Some` to replace it; a falsy value ([`Relay::is_truthy`])
/// stops the event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Relay {
    /// `true`, also the relay of an event run without one.
    True,
    /// `false`: stopped, with the failure reported by the caller.
    False,
    /// `null`: stopped silently (the handler logged its own message).
    Null,
    /// A number: damage, base power, a stat, accuracy, a priority, a crit
    /// ratio, a type-effectiveness exponent, a STAB modifier in 4096ths.
    /// 0 is falsy.
    Num(i32),
    /// Stat-stage changes (`ChangeBoost`, `TryBoost`, `AfterEachBoost`,
    /// `AfterBoost`) or stages (`ModifyBoost`). An object: always truthy.
    Boosts(Boosts),
    /// A status (`SetStatus`, `AfterSetStatus`).
    Status(StatusId),
    /// An item (`TakeItem`, `UseItem`, `TryEatItem`, `EatItem`, ...).
    Item(ItemId),
    /// An ability (`SetAbility`).
    Ability(AbilityId),
    /// A condition by Showdown ID (`TryAddVolatile`, `Immunity` for a
    /// status or `sandstorm`, ...).
    Id(&'static str),
    /// `ModifySecondaries`: the move's secondaries that still apply (bit
    /// `i` for the `i`-th one rolled, `secondaries::ordered`: with Fling,
    /// the pushed flinch is bit 0).
    Secondaries(u8),
    /// A Pokémon (`RedirectTarget`'s target).
    Pokemon(Position),
}

impl Relay {
    /// JavaScript truthiness.
    #[inline(always)]
    pub(crate) fn is_truthy(self) -> bool {
        match self {
            Relay::True
            | Relay::Boosts(_)
            | Relay::Status(_)
            | Relay::Item(_)
            | Relay::Ability(_)
            | Relay::Secondaries(_)
            | Relay::Pokemon(_) => true,
            Relay::Id(id) => !id.is_empty(),
            Relay::Num(n) => n != 0,
            Relay::False | Relay::Null => false,
        }
    }

    /// The number, if the relay is one.
    #[inline]
    pub(crate) fn num(self) -> Option<i32> {
        match self {
            Relay::Num(n) => Some(n),
            _ => None,
        }
    }

    /// From a Rust boolean.
    #[inline]
    pub(crate) fn from_bool(b: bool) -> Relay {
        if b { Relay::True } else { Relay::False }
    }
}

/// A boosts object as Showdown passes it between `boost`'s events: the
/// changes, and which stats it names (after `getCappedBoost` a stat can be
/// named with 0: "won't go any higher"). Handlers change or delete
/// entries (Clear Body deletes the drops).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Boosts {
    pub changes: StatBoosts,
    /// Bit `BoostId as u8` for each stat the object names.
    pub keys: u8,
}

impl Boosts {
    /// The object for `changes` (naming its non-zero stats).
    pub(crate) fn new(changes: StatBoosts) -> Boosts {
        let mut keys = 0;
        for (id, _) in changes.iter() {
            keys |= 1 << id as u8;
        }
        Boosts { changes, keys }
    }

    /// The named stats and their changes, in the object's key order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (BoostId, i8)> + '_ {
        let keys = self.keys;
        self.changes.order.iter().filter(move |&&id| keys & (1 << id as u8) != 0).map(|&id| (id, self.changes.get(id)))
    }

    /// The change for `id`, if the object names it.
    #[inline]
    pub(crate) fn get(&self, id: BoostId) -> Option<i8> {
        (self.keys & (1 << id as u8) != 0).then(|| self.changes.get(id))
    }

    /// Sets the change for `id` (naming it; a new key goes last).
    pub(crate) fn set(&mut self, id: BoostId, change: i8) {
        if self.keys & (1 << id as u8) == 0 {
            let named = |x: BoostId| self.keys & (1 << x as u8) != 0;
            let mut order = [id; 7];
            let mut n = 0;
            for &x in self.changes.order.iter().filter(|&&x| named(x)) {
                order[n] = x;
                n += 1;
            }
            order[n] = id;
            n += 1;
            for &x in self.changes.order.iter().filter(|&&x| !named(x) && x != id) {
                order[n] = x;
                n += 1;
            }
            self.changes.order = order;
            self.keys |= 1 << id as u8;
        }
        self.changes.stages[id as usize] = change;
    }

    /// `delete boost[id]`.
    pub(crate) fn delete(&mut self, id: BoostId) {
        self.keys &= !(1 << id as u8);
        self.changes.stages[id as usize] = 0;
    }
}

/// An effect: what an event is about (Showdown's `sourceEffect`) or what a
/// handler belongs to (`handler.effect`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Effect {
    #[default]
    None,
    Move(MoveId),
    Ability(AbilityId),
    Item(ItemId),
    /// A non-volatile status.
    Status(StatusId),
    /// A volatile condition, by the bit index of its [`Volatiles`] flag.
    Volatile(u8),
    Side(SideCondition),
    Slot(SlotCondition),
    Weather(Weather),
    Terrain(Terrain),
    PseudoWeather(PseudoWeather),
    /// Another Showdown condition (`recoil`, `drain`, `confused`, ...).
    Condition(Condition),
}

/// Showdown conditions that are an event's effect without being a status,
/// volatile, side, slot or field condition of the engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Condition {
    /// Any other (a condition's own handler, `boost` from a condition).
    #[default]
    Other,
    /// A move's recoil (`recoil`).
    Recoil,
    /// Struggle's recoil (`strugglerecoil`).
    StruggleRecoil,
    /// Draining (`drain`).
    Drain,
    /// Hitting itself in confusion (`confused`).
    Confused,
    /// Synchronize passing its status on: Showdown's `{status, id:
    /// 'synchronize'}`, a status effect that is not the ability itself
    /// (plain `-status`, failures announced).
    Synchronize,
}

impl Effect {
    /// The volatile condition with `flag`.
    #[inline]
    pub(crate) fn volatile(flag: u128) -> Effect {
        Effect::Volatile(flag.trailing_zeros() as u8)
    }

    /// `effect.effectType === 'Move'`: a move, or hitting itself in
    /// confusion (Showdown's confusion damage is a pseudo-move).
    #[inline]
    pub(crate) fn is_move(self) -> bool {
        matches!(self, Effect::Move(_) | Effect::Condition(Condition::Confused))
    }

    /// `effect.effectType === 'Ability'`.
    #[inline]
    pub(crate) fn is_ability(self) -> bool {
        matches!(self, Effect::Ability(_))
    }

    /// The move, if the effect is one.
    #[inline]
    pub(crate) fn move_id(self) -> Option<MoveId> {
        match self {
            Effect::Move(m) => Some(m),
            _ => None,
        }
    }

    /// The status of a [`Status`] (its condition).
    pub(crate) fn of_status(status: Status) -> Effect {
        match status::kind(status) {
            Some(s) => Effect::Status(s),
            None => Effect::None,
        }
    }
}

/// What an event is run on (Showdown's `runEvent` target).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Pokemon(Position),
    /// A side (`SideConditionStart`): every active Pokémon's handlers are
    /// collected too.
    Side(SideId),
    /// The battle (`runEvent` without a target).
    Field,
}

enum MoveArg<'m> {
    None,
    Ref(&'m MoveData),
    Mut(&'m mut MoveData),
}

/// A running event: Showdown's `this.event` plus the arguments its
/// handlers get. Handlers read the relay, source and effect here, return
/// a new relay (or `None`), and chain modifiers with
/// [`chain_modify`](EventCtx::chain_modify).
pub(crate) struct EventCtx<'m> {
    pub event: Event,
    /// The prefix of the callback being run (set per handler).
    pub prefix: EventPrefix,
    /// The event's target (Showdown's `event.target`); for an event run on
    /// several Pokémon, the one the running handler was collected for.
    pub target: Target,
    pub source: Option<Position>,
    /// Showdown's `sourceEffect` (`event.effect`).
    pub effect: Effect,
    pub relay: Relay,
    /// Showdown's `event.modifier`, 4096-based.
    pub modifier: u32,
    /// The type an `Effectiveness` handler is asked about (Showdown passes
    /// it as the event's source).
    pub ty: Option<Type>,
    mv: MoveArg<'m>,
}

impl<'m> EventCtx<'m> {
    pub(crate) fn new(event: Event, target: Target, source: Option<Position>, effect: Effect, relay: Relay) -> Self {
        EventCtx {
            event,
            prefix: EventPrefix::On,
            target,
            source,
            effect,
            relay,
            modifier: 4096,
            ty: None,
            mv: MoveArg::None,
        }
    }

    /// The same event with the active move's data (the move `effect` is).
    pub(crate) fn with_move(mut self, data: &'m MoveData) -> Self {
        self.mv = MoveArg::Ref(data);
        self
    }

    /// The same event with the active move's data, which handlers may
    /// change (`ModifyType`, `ModifyMove`).
    pub(crate) fn with_move_mut(mut self, data: &'m mut MoveData) -> Self {
        self.mv = MoveArg::Mut(data);
        self
    }

    /// The target Pokémon (`event.target`), if it is one.
    #[inline]
    pub(crate) fn target_pos(&self) -> Option<Position> {
        match self.target {
            Target::Pokemon(p) => Some(p),
            _ => None,
        }
    }

    /// The active move's data, if the event is about a move.
    #[inline]
    pub(crate) fn move_data(&self) -> Option<&MoveData> {
        match &self.mv {
            MoveArg::None => None,
            MoveArg::Ref(d) => Some(d),
            MoveArg::Mut(d) => Some(d),
        }
    }

    /// The active move's data to change (`ModifyType`, `ModifyMove`).
    #[inline]
    pub(crate) fn move_mut(&mut self) -> Option<&mut MoveData> {
        match &mut self.mv {
            MoveArg::Mut(d) => Some(d),
            _ => None,
        }
    }

    /// The relay as a number (0 if it is not one).
    #[inline]
    pub(crate) fn value(&self) -> i32 {
        self.relay.num().unwrap_or(0)
    }

    /// Showdown's `chainModify(numerator, denominator)`. Returns `None`
    /// (what `return this.chainModify(...)` returns) for convenience.
    #[inline]
    pub(crate) fn chain_modify(&mut self, numerator: u32, denominator: u32) -> Option<Relay> {
        self.chain(numerator * 4096 / denominator);
        None
    }

    /// `chainModify([modifier, 4096])`: chains a 4096-based modifier.
    #[inline]
    pub(crate) fn chain(&mut self, modifier: u32) {
        self.modifier = (self.modifier * modifier + 2048) >> 12;
    }

    /// Showdown's `finalModify(value)`: the modifier so far applied, then
    /// reset.
    #[inline]
    pub(crate) fn final_modify(&mut self, value: u32) -> u32 {
        let v = crate::stats::modify(value, self.modifier);
        self.modifier = 4096;
        v
    }
}

/// The fields of Showdown's `ActiveMove` that abilities and items read or
/// set, for the move being used (`Battle::move_state`). Reset whenever a
/// move becomes the active move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct MoveState {
    // Shared by many abilities and items.
    /// `move.ignoreAbility` (Mold Breaker, Teravolt, Turboblaze): the
    /// targets' `breakable` abilities are ignored
    /// (`abilities::suppressing_ability`).
    pub ignore_ability: bool,
    /// `move.infiltrates` (Infiltrator): screens, Substitute, Mist and
    /// Safeguard are bypassed.
    pub infiltrates: bool,
    /// Showdown's `battle.activeTarget`: the target being processed
    /// (`getSpreadDamage`, `hitStepAccuracy`), which Unaware reads.
    pub active_target: Option<Position>,
    /// `target.getMoveHitData(move)` for each target (index `2 * side +
    /// slot`): whether the hit was critical and its type effectiveness.
    pub hit: [MoveHitData; 4],
    /// `move.hasSheerForce` (set by Sheer Force's `onModifyMove`): with the
    /// ability, the move's `AfterMoveSecondary(Self)` effects are skipped.
    pub has_sheer_force: bool,
    /// `move.ignoreAccuracy` (the user's accuracy stage is not used).
    pub ignore_accuracy: bool,
    /// `move.ignoreEvasion` (Keen Eye, Mind's Eye; the target's evasion
    /// stage is not used).
    pub ignore_evasion: bool,
    /// How many times the chances of the move's secondaries were doubled
    /// (Serene Grace's `onModifyMove`; the rainbow is separate).
    pub chance_doublings: u8,
    // Switch-in and forme abilities (`abilities/switchin.rs`).
    /// Showdown's `move.hit`: the hit being dealt (1 for the first, set by
    /// `hitStepMoveHitLoop`; 0 before). The Champions Disguise and Tera
    /// Shell keep their effect for a move's later hits.
    pub hit_number: u8,
    /// `move.ruinedAtk` / `ruinedDef` / `ruinedSpA` / `ruinedSpD`: the
    /// Ruin ability holder whose drop applies to this move.
    pub ruined: [Option<Position>; 4],
    /// `move.auraBooster` (Dark Aura, Fairy Aura) and `move.hasAuraBreak`.
    pub aura_booster: Option<Position>,
    pub has_aura_break: bool,
    /// The active move's category after `ModifyMove` (Shell Side Arm,
    /// Photon Geyser, Tera Blast): Ice Face's `onDamage` reads it.
    pub category: Option<pikapro_data::MoveCategory>,
    /// The active move's type after its `ModifyType` and `ModifyMove`
    /// (Revelation Dance, Judgment, the -ate abilities): `AfterMove` reads
    /// it (Charge ends after an Electric move).
    pub ty: Option<Type>,
    // Damage, stat and type modifiers (`abilities/damage.rs`).
    /// `move.typeChangerBoosted`: the -ate ability or Normalize that
    /// changed the move's type (its `onBasePower` boosts the move).
    pub type_changer: Option<AbilityId>,
    /// `move.ignoreImmunity.Fighting` / `.Normal` (Scrappy, Mind's Eye):
    /// Normal and Fighting moves hit Ghost types.
    pub ignore_immunity_fighting_normal: bool,
    /// A 10% flinch secondary was pushed onto the move's secondaries
    /// (Stench; King's Rock and Razor Fang do the same:
    /// `move.secondaries.push({chance: 10, volatileStatus: 'flinch'})`).
    pub added_flinch: bool,
    /// `move.spreadHit`: the move hits more than one target (set by
    /// `trySpreadMoveHit` before `PrepareHit`).
    pub spread_hit: bool,
    /// `move.multihitType === 'parentalbond'` (Parental Bond's
    /// `onPrepareHit`, which also sets `move.multihit = 2`): the second hit
    /// deals 1/4.
    pub parental_bond: bool,
    /// `getMoveHitData(move).bypassProtect` for each target (bit `2 * side
    /// + slot`): Piercing Drill or the Champions mod's Unseen Fist
    /// (`bypass_ability`) went through its protection, for 1/4 of the
    /// damage (`-zbroken`).
    pub bypass_protect: u8,
    /// The ability that set `bypass_protect`.
    pub bypass_ability: Option<AbilityId>,

    // Reactive abilities (`abilities/reactive.rs`).
    /// `move.hasBounced`: the move was reflected by Magic Bounce (it is
    /// not reflected again).
    pub has_bounced: bool,
    /// `move.isExternal`: a move Dancer copied.
    pub is_external: bool,
    /// `move.totalDamage`: the damage of the hits so far, every target
    /// together (Innards Out, Berserk, Anger Shell).
    pub total_damage: u32,
    /// `target.getLastAttackedBy().damage` after the hit loop, by target
    /// (index `2 * side + slot`): the last hit's damage (Berserk, Anger
    /// Shell).
    pub last_damage: [u16; 4],
    /// `move.accuracy = true` (Flash Fire's `onTryHit`): the move no
    /// longer checks accuracy.
    pub accuracy_true: bool,
    // `abilities/misc.rs`.
    /// `move.tracksTarget` set by Stalwart's and Propeller Tail's
    /// `onModifyMove`: the move is not redirected (`getMoveTargets`).
    pub tracks_target: bool,
    // Held items (`items/held.rs`).
    /// `delete move.multiaccuracy`: Loaded Dice's `onModifyMove`, Skill
    /// Link's: the hits after the first need no accuracy
    /// check.
    pub no_multiaccuracy: bool,
    /// `move.totalDamage` of the move it names (Shell Bell), summed over
    /// the hits and targets in `hitStepMoveHitLoop`.
    pub total_damage_of: (MoveId, u32),
    // Consumed items and the item moves (`items/consumed.rs`, `itemmoves.rs`).
    /// The item Fling throws (its `onPrepareHit` set the move's power and
    /// `onHit` from it).
    pub fling: Option<ItemId>,
    /// A `smartTarget` move (Dragon Darts) at two targets: the last hit's
    /// target, which is Showdown's `move.hitTargets` (Magician).
    pub smart_hit_target: Option<Position>,
    // Terastallization and Beat Up.
    /// `move.stellarBoosted`: a Pokémon Terastallized into Stellar boosted
    /// this move, so its later hits and targets are boosted too.
    pub stellar_boosted: bool,
    /// `move.allies` (Beat Up): the `set.species` of each ally that attacks,
    /// fixed by its `onModifyMove` and shifted off one per hit, so a status
    /// inflicted mid-move changes nothing.
    pub beat_up: StackVec<pikapro_data::SpeciesId, 6>,
}

/// Showdown's `moveHitData` for one target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct MoveHitData {
    pub crit: bool,
    pub type_mod: i8,
}

impl MoveState {
    /// `target.getMoveHitData(move)`.
    #[inline]
    pub(crate) fn hit_data(&self, target: Position) -> MoveHitData {
        self.hit[target.side.index() * 2 + (target.slot as usize & 1)]
    }

    #[inline]
    pub(crate) fn hit_data_mut(&mut self, target: Position) -> &mut MoveHitData {
        &mut self.hit[target.side.index() * 2 + (target.slot as usize & 1)]
    }
}

// ---------------------------------------------------------------------------
// Handlers.

/// A condition's handler with relay semantics.
pub(crate) type RelayFn = fn(&mut Battle, &mut EventCtx<'_>, &Handler) -> Option<Relay>;

/// What a [`Handler`] runs.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum Callback {
    /// Nothing: a handler that only takes part in the sort.
    #[default]
    Sort,
    /// A condition's plain handler (for `Residual`, `SwitchIn`,
    /// `BeforeMove`, ...: true means continue).
    Plain(HandlerFn),
    /// A condition's relay handler.
    Relay(RelayFn),
    /// The ability or item `Handler::effect` names, collected with
    /// `prefix`. `start`: Showdown runs its `onStart` for this `SwitchIn`.
    /// `state`: the `effectOrder` of the `abilityState` / `itemState` it
    /// was collected with (`fieldEvent` skips a handler whose state has
    /// since been replaced).
    Held { prefix: EventPrefix, start: bool, state: u16 },
}

/// A condition's handler for one event: its ordering data and callback
/// (Showdown's `on<Event>Order` / `Priority` / `SubOrder`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Spec {
    pub order: Option<u8>,
    pub priority: i32,
    /// `None`: the default for the effect type.
    pub sub_order: Option<u8>,
    pub callback: Callback,
}

impl Spec {
    /// A relay handler with default ordering.
    pub(crate) const fn relay(run: RelayFn) -> Spec {
        Spec { order: None, priority: 0, sub_order: None, callback: Callback::Relay(run) }
    }

    /// A plain handler (`true`: continue, `false`: stop with `false`).
    pub(crate) const fn plain(run: HandlerFn) -> Spec {
        Spec { order: None, priority: 0, sub_order: None, callback: Callback::Plain(run) }
    }

    pub(crate) const fn priority(self, priority: i32) -> Spec {
        Spec { priority, ..self }
    }
}

/// The handlers of one event (at most a few per Pokémon, side and field).
pub(crate) type EventHandlers = super::handlers::SpillList<32>;

/// Where collected handlers go: a list, or [`Count`] (most events have no
/// handler or one, which needs no list).
pub(crate) trait Sink {
    fn add(&mut self, handler: Handler);
}

impl<const N: usize> Sink for StackVec<Handler, N> {
    #[inline]
    fn add(&mut self, handler: Handler) {
        self.push(handler);
    }
}

impl<const N: usize, const I: usize> Sink for super::handlers::SpillList<N, I> {
    #[inline]
    fn add(&mut self, handler: Handler) {
        self.push(handler);
    }
}

/// Counts handlers, keeping the first.
#[derive(Default)]
pub(crate) struct Count {
    pub n: usize,
    pub first: Handler,
}

impl Sink for Count {
    #[inline]
    fn add(&mut self, handler: Handler) {
        if self.n == 0 {
            self.first = handler;
        }
        self.n += 1;
    }
}

/// Showdown's `effectTypeOrder` default sub-order for an ability's handler
/// (Poison Touch and Perish Body 6, Stall 9, others 7).
fn ability_sub_order(id: AbilityId) -> u8 {
    use pikapro_data::gen9::abilities as a;
    match id {
        a::POISON_TOUCH | a::PERISH_BODY => 6,
        a::STALL => 9,
        _ => sub_order::ABILITY,
    }
}

fn held_handler(h: &EventHandler, holder: Holder, speed: i32, sub: u8, effect: Effect, state: u16) -> Handler {
    // `resolvePriority` sets `effectOrder` only for `SwitchIn` and
    // `RedirectTarget` handlers.
    let effect_order = if matches!(h.event, Event::SwitchIn | Event::RedirectTarget) { state as u32 } else { 0 };
    Handler {
        order: h.order,
        priority: h.priority as i32,
        speed,
        sub_order: if h.sub_order != 0 { h.sub_order } else { sub },
        effect_order,
        holder,
        callback: Callback::Held { prefix: h.prefix, start: h.via_start, state },
        effect,
        ..Handler::default()
    }
}

fn spec_handler(spec: Spec, holder: Holder, speed: i32, default_sub: u8, effect: Effect) -> Handler {
    Handler {
        order: spec.order,
        priority: spec.priority,
        speed,
        sub_order: spec.sub_order.unwrap_or(default_sub),
        holder,
        callback: spec.callback,
        effect,
        ..Handler::default()
    }
}

/// The handlers of the Pokémon in `pos` for `event` with `prefix`
/// (Showdown's `findPokemonEventHandlers`): its status, volatiles in the
/// order they were added, ability, item, then its slot's conditions.
pub(crate) fn pokemon_handlers(
    battle: &Battle,
    pos: Position,
    event: Event,
    prefix: EventPrefix,
    index: u8,
    sink: &mut impl Sink,
) {
    let side = battle.state.side(pos.side);
    let Some(mon) = side.team.get(pos.slot as usize) else { return };
    let holder = Holder::Pokemon(pos);
    let speed = mon.speed;
    if let Some(spec) = status_spec(mon.status, event, prefix) {
        let h = spec_handler(spec, holder, speed, sub_order::STATUS, Effect::of_status(mon.status));
        sink.add(Handler { index, ..h });
    }
    if mon.volatiles.bits() != 0 {
        for flag in mon.volatiles.in_order() {
            if let Some(spec) = volatile_spec(mon, flag, event, prefix) {
                let h = spec_handler(spec, holder, speed, sub_order::CONDITION, Effect::volatile(flag));
                sink.add(Handler { volatile_bit: vol(flag), index, ..h });
            }
        }
    }
    held_handlers(battle, mon, event, prefix, holder, speed, index, sink);
    // `side.slotConditions[pokemon.position]` (none off the field).
    let slot_conditions = side.conditions.slots.get(pos.slot as usize).map_or(&[][..], |s| &s.order[..]);
    for &condition in slot_conditions {
        if let Some(spec) = slot_spec(condition, event, prefix) {
            let h = spec_handler(spec, holder, speed, sub_order::SLOT_CONDITION, Effect::Slot(condition));
            sink.add(Handler { index, ..h });
        }
    }
}

/// The ability's and item's handlers of `mon`, held by `holder`.
#[expect(clippy::too_many_arguments, reason = "the collection context")]
pub(crate) fn held_handlers(
    battle: &Battle,
    mon: &PokemonState,
    event: Event,
    prefix: EventPrefix,
    holder: Holder,
    speed: i32,
    index: u8,
    sink: &mut impl Sink,
) {
    let dex = battle.format.dex;
    let ability = dex.ability(mon.ability);
    if has(ability.events.0, event)
        && let Some(h) = pikapro_data::find_handler(ability.handlers, event, prefix)
    {
        let (sub, state) = (ability_sub_order(mon.ability), mon.volatile_counters.ability_order);
        // `resolvePriority`: Magic Bounce's `onAllyTryHitSide` sorts by
        // `pokemon.getStat('spe', true, true)` rather than `pokemon.speed`.
        let bounce = mon.ability == pikapro_data::gen9::abilities::MAGIC_BOUNCE
            && event == Event::TryHitSide
            && prefix == EventPrefix::Ally;
        let speed = if bounce { i32::from(mon.stats[Stat::Spe]).min(10000) } else { speed };
        let h = held_handler(h, holder, speed, sub, Effect::Ability(mon.ability), state);
        sink.add(Handler { index, ..h });
    }
    if let Some(item) = mon.item {
        let data = dex.item(item);
        if has(data.events.0, event)
            && let Some(h) = pikapro_data::find_handler(data.handlers, event, prefix)
        {
            let state = mon.volatile_counters.item_order;
            let h = held_handler(h, holder, speed, sub_order::ITEM, Effect::Item(item), state);
            sink.add(Handler { index, ..h });
        }
    }
}

/// The side conditions' handlers of `side` (Showdown's
/// `findSideEventHandlers`), in the order they started. `custom` is the
/// Pokémon they are collected for by `fieldEvent` (it gives them its
/// speed); otherwise the side holds them (speed 0).
fn side_handlers(
    battle: &Battle,
    side: SideId,
    event: Event,
    prefix: EventPrefix,
    custom: Option<Position>,
    index: u8,
    sink: &mut impl Sink,
) {
    let conditions = &battle.state.side(side).conditions;
    for &condition in conditions.order.iter() {
        let Some(spec) = side_spec(condition, event, prefix) else { continue };
        let (holder, speed) = match custom {
            Some(pos) => (Holder::SideOn(pos), battle.state.side(pos.side).team[pos.slot as usize].speed),
            None => (Holder::Side(side), 0),
        };
        let h = spec_handler(spec, holder, speed, sub_order::SIDE_CONDITION, Effect::Side(condition));
        sink.add(Handler { index, ..h });
    }
}

/// The field's handlers (Showdown's `findFieldEventHandlers`):
/// pseudo-weathers in the order they started, the weather, the terrain.
fn field_handlers(battle: &Battle, event: Event, custom: Option<Position>, index: u8, sink: &mut impl Sink) {
    let f = &battle.state.field;
    let (holder, speed) = match custom {
        Some(pos) => (Holder::FieldOn(pos), battle.state.side(pos.side).team[pos.slot as usize].speed),
        None => (Holder::Field, 0),
    };
    // Rules with battle callbacks are installed as pseudo-weathers at
    // construction, before any move starts a field effect.
    if event == Event::SetStatus && battle.format.sleep_clause_mod {
        let h = spec_handler(
            Spec::relay(status::sleep_clause_set_status),
            holder,
            speed,
            sub_order::FIELD_CONDITION,
            Effect::None,
        );
        sink.add(Handler { index, ..h });
    }
    for &pw in f.pseudo_order.iter() {
        if let Some(spec) = field_spec(FieldKind::Pseudo(pw), event) {
            // A pseudo-weather's state has no `target` until `runEvent`
            // has run one of its handlers: until then it sorts as a
            // volatile would (2), then as a field condition (5).
            let sub =
                if field::pseudo_targeted(battle, pw) { sub_order::FIELD_CONDITION } else { sub_order::CONDITION };
            let h = spec_handler(spec, holder, speed, sub, Effect::PseudoWeather(pw));
            sink.add(Handler { index, ..h });
        }
    }
    if let Some(w) = f.weather
        && let Some(spec) = field_spec(FieldKind::Weather(w), event)
    {
        let h = spec_handler(spec, holder, speed, sub_order::FIELD_CONDITION, Effect::Weather(w));
        sink.add(Handler { index, ..h });
    }
    if let Some(t) = f.terrain {
        // A terrain's `effectType` is `Terrain`, which has no default
        // sub-order (0).
        if let Some(spec) = field_spec(FieldKind::Terrain(t), event) {
            let h = spec_handler(spec, holder, speed, 0, Effect::Terrain(t));
            sink.add(Handler { index, ..h });
        }
    }
}

/// `effect.secondaries`: the effect is a move with secondary effects. The
/// active move's are after its `ModifyMove`s and `onPrepareHit` (Sheer
/// Force deleted them, Stench and King's Rock added a flinch, Fling
/// created an array); any other move's are its own.
pub(crate) fn effect_has_secondaries(battle: &Battle, effect: Effect) -> bool {
    let Effect::Move(move_id) = effect else { return false };
    let dex = battle.format.dex;
    let own = (move_id.0 as usize) < dex.moves.len() && !dex.move_(move_id).secondaries.is_empty();
    if battle.active_move.is_some_and(|(_, m)| m == move_id) {
        // Fling creates `secondaries = []` even for an item without a
        // secondary. An empty JS array is truthy. Berries and the herbs
        // instead install `onHit`, leaving secondaries alone.
        let fling = move_id == pikapro_data::gen9::moves::FLING
            && battle.move_state.fling.is_some_and(|item| {
                use pikapro_data::gen9::items as i;
                !dex.item(item).is_berry && !matches!(item, i::MENTAL_HERB | i::WHITE_HERB)
            });
        (own && !battle.move_state.has_sheer_force) || battle.move_state.added_flinch || fling
    } else {
        own
    }
}

/// Showdown's `isActive`: in a slot, its faint not reported yet.
#[inline]
pub(crate) fn is_active(battle: &Battle, pos: Position) -> bool {
    battle.state.side(pos.side).active.get(pos.slot as usize).is_some_and(|a| a.is_some())
}

/// Whether the Pokémon in `slot` of `side` has HP left and is active
/// (Showdown's `side.allies()` keeps these).
#[inline]
fn ally_at(battle: &Battle, side: SideId, slot: usize) -> bool {
    let s = battle.state.side(side);
    s.active[slot].is_some() && s.team[slot].hp > 0
}

/// Events whose handlers are collected without prefixes.
const fn prefixed(event: Event) -> bool {
    !matches!(event, Event::Update | Event::Weather | Event::WeatherChange | Event::TerrainChange)
}

/// Showdown's `findEventHandlers(target, event, source)`. `masks` (from
/// [`Masks::new`]) lets it skip what has no handler.
pub(crate) fn collect(
    battle: &Battle,
    masks: &Masks,
    event: Event,
    target: Target,
    source: Option<Position>,
    index: u8,
    sink: &mut impl Sink,
) {
    let active_n = battle.format.active_per_side();
    // Other Pokémon's (and the source's) handlers only when one of them has
    // a prefixed handler for the event.
    let prefixed = prefixed(event);
    let mon_prefixed = prefixed && masks.prefixed(event);
    let mon = |pos: Position| masks.mon(battle, active_n, pos, event);
    let mut side_target = None;
    let mut bubble_down = false;
    match target {
        Target::Pokemon(t) => {
            let active = |p: Position| is_active(battle, p) && !masks.is_inactive(p);
            if active(t) || source.is_some_and(active) {
                if mon(t) {
                    pokemon_handlers(battle, t, event, EventPrefix::On, index, sink);
                }
                if mon_prefixed {
                    // `alliesAndSelf()`, then `foes()`.
                    for side in [t.side, t.side.foe()] {
                        let prefix = if side == t.side { EventPrefix::Ally } else { EventPrefix::Foe };
                        for slot in 0..battle.format.active_per_side() {
                            let pos = Position::new(side, slot as u8);
                            if mon(pos) && ally_at(battle, side, slot) {
                                pokemon_handlers(battle, pos, event, prefix, index, sink);
                                pokemon_handlers(battle, pos, event, EventPrefix::Any, index, sink);
                            }
                        }
                    }
                }
                side_target = Some(t.side);
            }
        }
        Target::Side(side) => {
            side_target = Some(side);
            bubble_down = true;
        }
        Target::Field => {}
    }
    if let Some(s) = source.filter(|&s| mon_prefixed && mon(s)) {
        pokemon_handlers(battle, s, event, EventPrefix::Source, index, sink);
    }
    if let Some(target_side) = side_target {
        for side in SideId::BOTH {
            if bubble_down {
                let s = battle.state.side(side);
                for slot in 0..battle.format.active_per_side().min(s.team.len()) {
                    let pos = Position::new(side, slot as u8);
                    if !mon(pos) {
                        continue;
                    }
                    if side == target_side {
                        pokemon_handlers(battle, pos, event, EventPrefix::On, index, sink);
                    } else if mon_prefixed {
                        pokemon_handlers(battle, pos, event, EventPrefix::Foe, index, sink);
                    }
                    if mon_prefixed {
                        pokemon_handlers(battle, pos, event, EventPrefix::Any, index, sink);
                    }
                }
            }
            if !has(masks.sides[side.index()], event) {
                continue;
            }
            if side == target_side {
                side_handlers(battle, side, event, EventPrefix::On, None, index, sink);
            } else if prefixed {
                side_handlers(battle, side, event, EventPrefix::Foe, None, index, sink);
            }
            if prefixed {
                side_handlers(battle, side, event, EventPrefix::Any, None, index, sink);
            }
        }
    }
    if has(masks.field, event) {
        field_handlers(battle, event, None, index, sink);
    }
}

// ---------------------------------------------------------------------------
// Sorting.

/// Showdown's `compareLeftToRightOrder` (`TryHit`, `DamagingHit`,
/// `Invulnerability`): order, priority, then the target's index; a stable
/// sort, so ties keep the collection order and nothing is drawn.
fn sort_left_to_right(list: &mut [Handler]) {
    let key = |h: &Handler| (h.order.map_or(u64::MAX, u64::from), -(h.priority as i64), h.index);
    stable_sort(list, |a, b| key(a).cmp(&key(b)));
}

/// Showdown's `compareRedirectOrder` (`priorityEvent`): priority, speed,
/// then the holders' `abilityState.effectOrder`; stable, no draws.
fn sort_redirect(battle: &Battle, list: &mut [Handler]) {
    let ability_order = |h: &Handler| match h.holder {
        Holder::Pokemon(p) => {
            battle.state.side(p.side).team.get(p.slot as usize).map(|m| m.volatile_counters.ability_order)
        }
        _ => None,
    };
    stable_sort(list, |a, b| {
        b.priority.cmp(&a.priority).then(b.speed.cmp(&a.speed)).then(match (ability_order(a), ability_order(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            _ => Ordering::Equal,
        })
    });
}

fn stable_sort(list: &mut [Handler], cmp: impl Fn(&Handler, &Handler) -> Ordering) {
    for i in 1..list.len() {
        let mut j = i;
        while j > 0 && cmp(&list[j - 1], &list[j]) == Ordering::Greater {
            list.swap(j - 1, j);
            j -= 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Running.

/// How [`run_handlers`] ends the event.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Exit {
    /// `runEvent`: a falsy relay stops it.
    Falsy,
    /// `priorityEvent` (`fastExit`): any returned value stops it.
    First,
}

/// Whether `handler` is skipped when its turn comes (`runEvent`'s checks):
/// a status that changed, a suppressed ability or item, a weather handler
/// under Air Lock.
fn suppressed(battle: &Battle, event: Event, handler: &Handler) -> bool {
    let pos = match handler.holder {
        Holder::Pokemon(p) => Some(p),
        _ => None,
    };
    match handler.effect {
        Effect::Status(s) => {
            // `effect.effectType === 'Status' && effectHolder.status !== effect.id`.
            let mon = pos.and_then(|p| battle.state.side(p.side).team.get(p.slot as usize));
            if mon.is_none_or(|m| status::kind(m.status) != Some(s)) {
                return true;
            }
        }
        Effect::Ability(id) => {
            let Some(pos) = pos else { return false };
            if battle.format.dex.ability(id).flags.contains(pikapro_data::AbilityFlags::BREAKABLE)
                && abilities::suppressing_ability(battle, pos)
            {
                return true;
            }
            if event != Event::End && abilities::ignoring_ability(battle, pos) {
                return true;
            }
        }
        Effect::Item(_) => {
            if let Some(pos) = pos
                && !matches!(event, Event::Start | Event::SwitchIn | Event::TakeItem)
                && items::ignoring_item(battle, pos)
            {
                return true;
            }
        }
        _ => {}
    }
    let weather = matches!(handler.effect, Effect::Weather(_)) || event == Event::Weather;
    weather && !matches!(event, Event::Residual | Event::End) && abilities::suppressing_weather(battle)
}

/// Runs one handler.
pub(crate) fn call(battle: &mut Battle, ev: &mut EventCtx<'_>, handler: &Handler) -> Option<Relay> {
    battle.handler_calls = battle.handler_calls.wrapping_add(1);
    match handler.callback {
        Callback::Sort => None,
        Callback::Plain(run) => {
            if run(battle, handler) {
                None
            } else {
                Some(Relay::False)
            }
        }
        Callback::Relay(run) => run(battle, ev, handler),
        Callback::Held { prefix, start, .. } => {
            let Holder::Pokemon(holder) = handler.holder else { return None };
            ev.prefix = prefix;
            let event = ev.event;
            if start {
                // `getCallback`: the `SwitchIn` handler is `onStart`.
                ev.event = Event::Start;
            }
            let result = match handler.effect {
                Effect::Ability(id) => abilities::run(battle, ev, holder, id),
                Effect::Item(id) => items::run(battle, ev, holder, id),
                _ => None,
            };
            ev.event = event;
            ev.prefix = EventPrefix::On;
            result
        }
    }
}

/// Runs sorted handlers on a single-target event.
fn run_handlers(battle: &mut Battle, ev: &mut EventCtx<'_>, list: &[Handler], exit: Exit) {
    for handler in list {
        if suppressed(battle, ev.event, handler) {
            continue;
        }
        if let Some(r) = call(battle, ev, handler) {
            ev.relay = r;
            if !r.is_truthy() || exit == Exit::First {
                break;
            }
        }
    }
    // `runEvent`'s end: a non-negative whole number is modified.
    if let Relay::Num(n) = ev.relay
        && n >= 0
    {
        ev.relay = Relay::Num(crate::stats::modify(n as u32, ev.modifier) as i32);
    }
}

/// The move's or effect's own handler for `onEffect` (`runEvent`'s
/// `onEffect`: the move's `onBasePower`, the effect's `onDamage`), held by
/// the event's target.
fn own_handler(battle: &Battle, ev: &EventCtx<'_>, spec: Spec) -> Handler {
    let (holder, speed) = match ev.target {
        Target::Pokemon(p) => (Holder::Pokemon(p), battle.state.side(p.side).team[p.slot as usize].speed),
        Target::Side(s) => (Holder::Side(s), 0),
        Target::Field => (Holder::Field, 0),
    };
    // A move's `effectType` has no default sub-order.
    spec_handler(spec, holder, speed, 0, ev.effect)
}

/// Showdown's `runEvent`. `own`: the effect's own handler (`onEffect`).
/// Returns the final relay (also left in `ev.relay`).
///
/// Inlined, the event filter's test (as [`move_event`]'s).
#[inline(always)]
pub(crate) fn run_event(battle: &mut Battle, ev: &mut EventCtx<'_>, own: Option<Spec>) -> Relay {
    if own.is_none()
        && (filtered(battle, ev.event)
            || matches!(ev.target, Target::Pokemon(t) if quiet(battle, ev.event, t, ev.source)))
    {
        return ev.relay;
    }
    run_event_unfiltered(battle, ev, own)
}

/// [`run_event`] past the event filter.
#[inline(never)]
fn run_event_unfiltered(battle: &mut Battle, ev: &mut EventCtx<'_>, own: Option<Spec>) -> Relay {
    let at = Masks::locate_event(battle, ev.event, ev.target, ev.source);
    if own.is_none() && !at.get(battle).relevant(battle.format.active_per_side(), ev.target, ev.event) {
        return ev.relay;
    }
    run_located(battle, ev, own, &at)
}

/// [`run_event`] once its masks are located (and, without `own`, found
/// relevant).
fn run_located(battle: &mut Battle, ev: &mut EventCtx<'_>, own: Option<Spec>, at: &MasksAt) -> Relay {
    // Most events have no handler or one: count first, and only build and
    // sort a list for more.
    let mut count = Count::default();
    collect(battle, at.get(battle), ev.event, ev.target, ev.source, 0, &mut count);
    match (count.n, own) {
        (0, None) => return ev.relay,
        (0, Some(spec)) => {
            let h = own_handler(battle, ev, spec);
            run_handlers(battle, ev, core::slice::from_ref(&h), Exit::Falsy);
            return ev.relay;
        }
        (1, None) => {
            let h = count.first;
            run_handlers(battle, ev, core::slice::from_ref(&h), Exit::Falsy);
            return ev.relay;
        }
        _ => {}
    }
    battle.handler_sorts = battle.handler_sorts.wrapping_add(1);
    let mut list = EventHandlers::new();
    if let Some(spec) = own {
        list.push(own_handler(battle, ev, spec));
    }
    collect(battle, at.get(battle), ev.event, ev.target, ev.source, 0, &mut list);
    if matches!(ev.event, Event::TryHit | Event::DamagingHit | Event::Invulnerability) {
        sort_left_to_right(&mut list);
    } else {
        speed_sort(&mut list, &mut battle.rng, Handler::compare);
    }
    run_handlers(battle, ev, &list, Exit::Falsy);
    ev.relay
}

/// `eachEvent`'s `speedSort` of the active Pokémon (`order`: each one's
/// position and stored speed), then its `runEvent(event, pokemon, null,
/// effect)` on each, as [`run_event`] on each. The masks are located once,
/// and again only after a handler ran: until one does, nothing changes.
/// When no Pokémon could find a handler, the order is never read: only
/// the sort's draws (on ties) are made.
pub(crate) fn each_event_run(battle: &mut Battle, event: Event, effect: Effect, order: &mut [(Position, i32)]) {
    if filtered(battle, event) {
        super::queue::speed_sort_by_speed_draws(order, &mut battle.rng);
        return;
    }
    // A Pokémon revived in its slot: each target's own masks.
    if battle.order.revived_in_slot {
        super::queue::speed_sort_by_speed(order, &mut battle.rng);
        for &(pos, _) in order.iter() {
            let mut ev = EventCtx::new(event, Target::Pokemon(pos), None, effect, Relay::True);
            run_event(battle, &mut ev, None);
        }
        return;
    }
    let active = battle.format.active_per_side();
    let mut at = Masks::locate(battle, Target::Field, None);
    // The Pokémon (by `2 * side + slot`) that could find a handler now.
    let mut relevant = 0u8;
    let masks = at.get(battle);
    if masks.any(event) {
        for &(pos, _) in order.iter() {
            if masks.relevant(active, Target::Pokemon(pos), event)
                && !(event == Event::Update && update_idle(battle, masks, pos))
            {
                relevant |= 1 << (pos.side.index() * 2 + pos.slot as usize);
            }
        }
    }
    // None could: no handler runs.
    if relevant == 0 {
        super::queue::speed_sort_by_speed_draws(order, &mut battle.rng);
        return;
    }
    super::queue::speed_sort_by_speed(order, &mut battle.rng);
    let mut calls = battle.handler_calls;
    // `relevant` holds until a handler runs.
    let mut first = true;
    for &(pos, _) in order.iter() {
        if battle.handler_calls != calls {
            at = Masks::locate(battle, Target::Pokemon(pos), None);
            calls = battle.handler_calls;
            first = false;
        }
        let runs = if first {
            relevant & 1 << (pos.side.index() * 2 + pos.slot as usize) != 0
        } else {
            at.get(battle).relevant(active, Target::Pokemon(pos), event)
        };
        if !runs {
            continue;
        }
        let mut ev = EventCtx::new(event, Target::Pokemon(pos), None, effect, Relay::True);
        run_located(battle, &mut ev, None, &at);
    }
}

/// `eachEvent('Update')` on the Pokémon at `pos` would run one handler,
/// its item's, which would do nothing now ([`items::update_idle`]): the
/// event can be skipped. `Update` is not prefixed, so the handlers it
/// collects are the Pokémon's own (status, volatiles, ability, item and
/// slot conditions), its side's and the field's; with one handler there
/// is no sort, no draw, and a handler that does nothing changes nothing
/// (`call` only counts it in `Battle::handler_calls`, which says when
/// masks must be looked up again). Checked against the collected
/// handlers in debug builds.
fn update_idle(battle: &Battle, masks: &Masks, pos: Position) -> bool {
    const _: () = assert!(!prefixed(Event::Update));
    let side = battle.state.side(pos.side);
    let Some(mon) = side.team.get(pos.slot as usize) else { return false };
    let Some(item) = mon.item else { return false };
    if mon.volatiles.bits() & VOLATILES_WITH_UPDATE != 0 {
        return false;
    }
    let mut others = battle.format.dex.ability(mon.ability).events.0
        | status_events(mon.status)
        | masks.sides[pos.side.index()]
        | masks.field;
    for &c in side.conditions.slots.get(pos.slot as usize).map_or(&[][..], |s| &s.order[..]) {
        others |= slot_events(c);
    }
    if has(others, Event::Update) || !items::update_idle(battle, pos, item) {
        return false;
    }
    #[cfg(debug_assertions)]
    {
        let mut count = Count::default();
        collect(battle, masks, Event::Update, Target::Pokemon(pos), None, 0, &mut count);
        assert!(
            count.n == 0 || count.n == 1 && count.first.effect == Effect::Item(item),
            "Update on {pos:?}: {} handlers, not only its item's",
            count.n
        );
    }
    true
}

/// The volatiles with an `Update` handler.
const VOLATILES_WITH_UPDATE: u128 = {
    let mut m = 0;
    let mut bits = VOLATILES_WITH_HANDLERS;
    while bits != 0 {
        let flag = bits & bits.wrapping_neg();
        if has(volatile_events(flag), Event::Update) {
            m |= flag;
        }
        bits &= bits - 1;
    }
    m
};

/// Showdown's `priorityEvent`: like [`run_event`], but sorted without
/// draws and stopped by the first handler that returns anything.
///
/// Inlined, the event filter's test (as [`move_event`]'s).
#[inline(always)]
pub(crate) fn priority_event(battle: &mut Battle, ev: &mut EventCtx<'_>) -> Relay {
    if filtered(battle, ev.event) || matches!(ev.target, Target::Pokemon(t) if quiet(battle, ev.event, t, ev.source)) {
        return ev.relay;
    }
    priority_event_unfiltered(battle, ev)
}

/// [`priority_event`] past the event filter.
#[inline(never)]
fn priority_event_unfiltered(battle: &mut Battle, ev: &mut EventCtx<'_>) -> Relay {
    let at = Masks::locate(battle, ev.target, ev.source);
    let masks = at.get(battle);
    if !masks.relevant(battle.format.active_per_side(), ev.target, ev.event) {
        return ev.relay;
    }
    let mut count = Count::default();
    collect(battle, masks, ev.event, ev.target, ev.source, 0, &mut count);
    match count.n {
        0 => return ev.relay,
        1 => {
            let h = count.first;
            run_handlers(battle, ev, core::slice::from_ref(&h), Exit::First);
            return ev.relay;
        }
        _ => {}
    }
    let mut list = EventHandlers::new();
    collect(battle, at.get(battle), ev.event, ev.target, ev.source, 0, &mut list);
    sort_redirect(battle, &mut list);
    run_handlers(battle, ev, &list, Exit::First);
    ev.relay
}

/// A modifier event on a Pokémon (`runEvent` with a number): the value
/// after every handler and the chained modifier. Inlined, so that
/// [`run_event`]'s test of the event folds at the call sites (most of
/// the damage calculation's events have no handler).
#[expect(clippy::too_many_arguments, reason = "mirrors runEvent's arguments")]
#[inline(always)]
pub(crate) fn run_modify(
    battle: &mut Battle,
    event: Event,
    target: Position,
    source: Option<Position>,
    effect: Effect,
    value: u32,
    mv: Option<&MoveData>,
    own: Option<Spec>,
) -> u32 {
    let mut ev = EventCtx::new(event, Target::Pokemon(target), source, effect, Relay::Num(value as i32));
    if let Some(data) = mv {
        ev = ev.with_move(data);
    }
    match run_event(battle, &mut ev, own) {
        Relay::Num(n) => n.max(0) as u32,
        // A handler stopped the event with `false` / `null`: 0.
        _ => 0,
    }
}

/// A boolean event on a Pokémon (`runEvent` without a relay): the final
/// relay, truthy when no handler stopped it.
pub(crate) fn run_check(
    battle: &mut Battle,
    event: Event,
    target: Position,
    source: Option<Position>,
    effect: Effect,
    mv: Option<&MoveData>,
) -> Relay {
    let mut ev = EventCtx::new(event, Target::Pokemon(target), source, effect, Relay::True);
    if let Some(data) = mv {
        ev = ev.with_move(data);
    }
    run_event(battle, &mut ev, None)
}

/// Showdown's `runEvent` on several targets at once (`TryHit`,
/// `DamagingHit`, `AfterMoveSecondary`): each target's handlers, sorted
/// together; each handler sees its own target's relay, and a falsy result
/// stops only that target (the event ends once every target is stopped).
///
/// Inlined, the event filter's test (as [`move_event`]'s).
#[inline(always)]
pub(crate) fn run_event_multi(
    battle: &mut Battle,
    event: Event,
    targets: &[Position],
    source: Option<Position>,
    effect: Effect,
    relays: &mut [Relay],
    mv: Option<&MoveData>,
) {
    if filtered(battle, event) {
        return;
    }
    // A loop, not `all`: its closure was compiled out of line, where the
    // event is no constant and `quiet` doesn't fold.
    let mut i = 0;
    while i < targets.len() {
        if !quiet(battle, event, targets[i], source) {
            run_event_multi_unfiltered(battle, event, targets, source, effect, relays, mv);
            return;
        }
        i += 1;
    }
}

/// [`run_event_multi`] past the event filter.
#[inline(never)]
fn run_event_multi_unfiltered(
    battle: &mut Battle,
    event: Event,
    targets: &[Position],
    source: Option<Position>,
    effect: Effect,
    relays: &mut [Relay],
    mv: Option<&MoveData>,
) {
    let first = targets.first().copied().map_or(Target::Field, Target::Pokemon);
    let at = Masks::locate(battle, first, source);
    let masks = at.get(battle);
    if !masks.any(event) {
        return;
    }
    let mut count = Count::default();
    for (i, &t) in targets.iter().enumerate() {
        collect(battle, masks, event, Target::Pokemon(t), source, i as u8, &mut count);
    }
    if count.n == 0 {
        return;
    }
    let mut list = EventHandlers::new();
    for (i, &t) in targets.iter().enumerate() {
        collect(battle, masks, event, Target::Pokemon(t), source, i as u8, &mut list);
    }
    if list.len() > 1 {
        if matches!(event, Event::TryHit | Event::DamagingHit | Event::Invulnerability) {
            sort_left_to_right(&mut list);
        } else {
            speed_sort(&mut list, &mut battle.rng, Handler::compare);
        }
    }
    let mut ev = EventCtx::new(event, first, source, effect, Relay::True);
    if let Some(data) = mv {
        ev = ev.with_move(data);
    }
    for handler in list.iter() {
        let i = handler.index as usize;
        let current = relays[i];
        if !current.is_truthy() && (current != Relay::Num(0) || event != Event::DamagingHit) {
            continue;
        }
        ev.target = Target::Pokemon(targets[i]);
        ev.relay = current;
        if suppressed(battle, event, handler) {
            continue;
        }
        if let Some(r) = call(battle, &mut ev, handler) {
            // Showdown only records a falsy result per target.
            if !r.is_truthy() {
                relays[i] = r;
                if relays.iter().all(|r| !r.is_truthy()) {
                    break;
                }
            }
        }
    }
}

/// `addVolatile`'s `runStatusImmunity(id)` and `runEvent('TryAddVolatile')`
/// for a volatile whose handlers no Gen 9 effect stops (a protection,
/// `stall`, `twoturnmove` and the charging move's own): their sort still
/// draws on ties (two Veil abilities).
pub(crate) fn try_add_volatile(battle: &mut Battle, pos: Position, id: &'static str) {
    let _ = immunity_event(battle, pos, id);
    let mut ev = EventCtx::new(
        pikapro_data::Event::TryAddVolatile,
        Target::Pokemon(pos),
        Some(pos),
        Effect::None,
        Relay::Id(id),
    );
    run_event(battle, &mut ev, None);
}

/// Showdown's `pokemon.addVolatile(id)` for a volatile of the ability and
/// item modules (a flag of bit 64 and up): fails if the Pokémon has no
/// HP or already has it (no `onRestart`), or if `runStatusImmunity(id)` or
/// the `TryAddVolatile` event stops it. On success the flag is set and
/// the caller runs the volatile's `onStart`.
pub(crate) fn add_volatile_flag(
    battle: &mut Battle,
    pos: Position,
    flag: u128,
    id: &'static str,
    source: Option<Position>,
    effect: Effect,
) -> bool {
    let Some(mon) = battle.state.at(pos) else { return false };
    if mon.is_fainted() || mon.volatiles.has(flag) {
        return false;
    }
    if immunity_event(battle, pos, id) != Some(true) {
        return false;
    }
    let mut ev =
        EventCtx::new(Event::TryAddVolatile, Target::Pokemon(pos), source.or(Some(pos)), effect, Relay::Id(id));
    if !run_event(battle, &mut ev, None).is_truthy() {
        return false;
    }
    if let Some(mon) = battle.state.at_mut(pos) {
        mon.volatiles.add(flag);
    }
    true
}

/// Showdown's `pokemon.removeVolatile(id)` for a volatile without an
/// `onEnd` (a flag of bit 64 and up). Returns whether it was removed:
/// `if (!this.hp) return false` first, so a Pokémon knocked out whose faint
/// is still queued keeps it (a caller that logs an `-end` stays silent).
/// Showdown's `delete pokemon.volatiles[id]` has no such check
/// ([`delete_volatile_flag`]).
pub(crate) fn remove_volatile_flag(battle: &mut Battle, pos: Position, flag: u128) -> bool {
    match battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) {
        Some(mon) if mon.hp > 0 && mon.volatiles.has(flag) => {
            mon.volatiles.clear(flag);
            true
        }
        _ => false,
    }
}

/// Showdown's `delete pokemon.volatiles[id]`: removes the flag whatever the
/// Pokémon's HP (Protosynthesis', Quark Drive's and Zen Mode's `onEnd`).
pub(crate) fn delete_volatile_flag(battle: &mut Battle, pos: Position, flag: u128) -> bool {
    match battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) {
        Some(mon) if mon.volatiles.has(flag) => {
            mon.volatiles.clear(flag);
            true
        }
        _ => false,
    }
}

/// Showdown's `pokemon.disableMove(moveid)`: the move can't be chosen (from
/// a `DisableMove` handler).
pub(crate) fn disable_move(battle: &mut Battle, pos: Position, move_id: MoveId) {
    if let Some(mon) = battle.state.at_mut(pos) {
        for (i, slot) in mon.moves.iter_mut().enumerate() {
            if slot.id == move_id {
                slot.disabled = true;
                mon.effect_disabled |= 1 << i;
            }
        }
    }
}

/// Showdown's `pokemon.tryTrap()` (from a `TrapPokemon` handler: Shadow
/// Tag, Arena Trap, Magnet Pull): Ghost types can't be trapped
/// (`runStatusImmunity('trapped')`). Returns whether it is trapped.
pub(crate) fn try_trap(battle: &mut Battle, pos: Position) -> bool {
    let Some(mon) = battle.state.at(pos) else { return false };
    if manipulation::has_type(mon, Type::Ghost) || immunity_event(battle, pos, "trapped") != Some(true) {
        return false;
    }
    if let Some(mon) = battle.state.at_mut(pos) {
        mon.trapped = true;
    }
    true
}

/// `pokemon.trapped = pokemon.maybeTrapped = false` from a `TrapPokemon`
/// handler (Shed Shell, priority -10): nothing traps the Pokémon this turn.
pub(crate) fn release_trap(battle: &mut Battle, pos: Position) {
    if let Some(mon) = battle.state.at_mut(pos) {
        mon.trapped = false;
        mon.trap_released = true;
    }
}

/// `runEvent(event, target, source, move, relay)` about the active move
/// `move_id` (its data after `ModifyType` / `ModifyMove`).
///
/// Inlined, the event filter's test: most of these events nothing in the
/// battle handles (about 60%), and the rest of the function saves and
/// restores registers before it can tell.
#[inline(always)]
pub(crate) fn move_event(
    battle: &mut Battle,
    event: Event,
    target: Position,
    source: Option<Position>,
    move_id: MoveId,
    data: &MoveData,
    relay: Relay,
) -> Relay {
    if filtered(battle, event) || quiet(battle, event, target, source) {
        return relay;
    }
    move_event_unfiltered(battle, event, target, source, move_id, data, relay)
}

/// [`move_event`] past the event filter.
#[inline(never)]
fn move_event_unfiltered(
    battle: &mut Battle,
    event: Event,
    target: Position,
    source: Option<Position>,
    move_id: MoveId,
    data: &MoveData,
    relay: Relay,
) -> Relay {
    let at = Masks::locate_event(battle, event, Target::Pokemon(target), source);
    if !at.get(battle).relevant(battle.format.active_per_side(), Target::Pokemon(target), event) {
        return relay;
    }
    let mut ev = EventCtx::new(event, Target::Pokemon(target), source, Effect::Move(move_id), relay).with_move(data);
    run_located(battle, &mut ev, None, &at)
}

/// `runEvent('ModifyType' | 'ModifyMove', pokemon, target, move, move)`:
/// handlers change the active move's data.
///
/// Inlined, the event filter's test (as [`move_event`]'s).
#[inline(always)]
pub(crate) fn modify_move_event(
    battle: &mut Battle,
    event: Event,
    user: Position,
    target: Option<Position>,
    move_id: MoveId,
    data: &mut MoveData,
) {
    if filtered(battle, event) || quiet(battle, event, user, target) {
        return;
    }
    modify_move_event_unfiltered(battle, event, user, target, move_id, data);
}

/// [`modify_move_event`] past its test.
fn modify_move_event_unfiltered(
    battle: &mut Battle,
    event: Event,
    user: Position,
    target: Option<Position>,
    move_id: MoveId,
    data: &mut MoveData,
) {
    let at = Masks::locate_event(battle, event, Target::Pokemon(user), target);
    if !at.get(battle).relevant(battle.format.active_per_side(), Target::Pokemon(user), event) {
        return;
    }
    let mut ev =
        EventCtx::new(event, Target::Pokemon(user), target, Effect::Move(move_id), Relay::True).with_move_mut(data);
    run_located(battle, &mut ev, None, &at);
}

/// Adds this framework's handlers for `event` (abilities, items, registry
/// conditions; `findEventHandlers`) to a list that `handlers.rs` or a
/// mechanics module collected, which then sorts and runs them together
/// ([`run_list`]).
///
/// Inlined, the event filter's test (as [`move_event`]'s).
#[inline(always)]
pub(crate) fn collect_into(
    battle: &mut Battle,
    event: Event,
    target: Target,
    source: Option<Position>,
    list: &mut impl Sink,
) {
    if filtered(battle, event) || matches!(target, Target::Pokemon(t) if quiet(battle, event, t, source)) {
        return;
    }
    collect_into_unfiltered(battle, event, target, source, list);
}

/// [`collect_into`] past the event filter.
#[inline(never)]
fn collect_into_unfiltered(
    battle: &mut Battle,
    event: Event,
    target: Target,
    source: Option<Position>,
    list: &mut impl Sink,
) {
    let at = Masks::locate_event(battle, event, target, source);
    let masks = at.get(battle);
    if masks.relevant(battle.format.active_per_side(), target, event) {
        collect(battle, masks, event, target, source, 0, list);
    }
}

/// Runs a sorted list of handlers, the mechanics modules' (`Callback::Plain`
/// or `Sort` with no effect) and the event system's, as `runEvent` does.
/// Returns the relay.
pub(crate) fn run_list(battle: &mut Battle, ev: &mut EventCtx<'_>, list: &[Handler]) -> Relay {
    run_handlers(battle, ev, list, Exit::Falsy);
    ev.relay
}

// ---------------------------------------------------------------------------
// Immunities (Showdown's `pokemon.runImmunity` / `runStatusImmunity`).

/// Showdown's `pokemon.runImmunity(type, message)` for a move's type: the
/// `NegateImmunity` event (Ring Target, Foresight), then `isGrounded` for
/// Ground (`null` for Levitate) or the type chart. With `message`, an
/// immune Pokémon gets `-immune` (`[from] ability: Levitate`).
pub(crate) fn run_immunity(battle: &mut Battle, pos: Position, ty: Type, message: bool) -> bool {
    // `if (!type || type === '???') return true` (a typeless Revelation
    // Dance), before anything else.
    if ty == Type::Unknown {
        return true;
    }
    if battle.state.at(pos).is_none_or(|m| m.is_fainted()) {
        return false;
    }
    let negate = if filtered(battle, Event::NegateImmunity) {
        false
    } else {
        let at = Masks::locate(battle, Target::Pokemon(pos), None);
        if at.get(battle).relevant(battle.format.active_per_side(), Target::Pokemon(pos), Event::NegateImmunity) {
            let mut ev = EventCtx::new(Event::NegateImmunity, Target::Pokemon(pos), None, Effect::None, Relay::True);
            ev.ty = Some(ty);
            !run_located(battle, &mut ev, None, &at).is_truthy()
        } else {
            false
        }
    };
    let not_immune = if ty == Type::Ground {
        field::grounded(battle, pos, negate)
    } else {
        Some(negate || !manipulation::type_immune(battle, pos, ty))
    };
    if not_immune == Some(true) {
        return true;
    }
    if message {
        let target = super::actor(battle, pos);
        // Levitate, else Eelevate.
        use pikapro_data::gen9::abilities::{EELEVATE, LEVITATE};
        let levitate = [LEVITATE, EELEVATE].into_iter().find(|&a| abilities::has_ability(battle, pos, a));
        if let (None, Some(ability)) = (not_immune, levitate) {
            let cause = crate::event::Cause::ability(ability);
            battle.emit(crate::event::BattleEvent::ImmuneFrom { target, detail: None, cause });
        } else {
            battle.emit(crate::event::BattleEvent::Immune { target });
        }
    }
    false
}

/// `runEvent('Immunity', pokemon, null, null, id)` (Showdown's
/// `runStatusImmunity` after the type check): whether nothing makes the
/// Pokémon immune to the status or effect `id` (`slp`, `par`, `frz`,
/// `sandstorm`, `powder`, `confusion`, `attract`, ...). `None` for
/// Showdown's `null` (the handler announced it).
pub(crate) fn immunity_event(battle: &mut Battle, pos: Position, id: &'static str) -> Option<bool> {
    if filtered(battle, Event::Immunity) || quiet(battle, Event::Immunity, pos, None) {
        return Some(true);
    }
    let at = Masks::locate(battle, Target::Pokemon(pos), None);
    if !at.get(battle).relevant(battle.format.active_per_side(), Target::Pokemon(pos), Event::Immunity) {
        return Some(true);
    }
    let mut ev = EventCtx::new(Event::Immunity, Target::Pokemon(pos), None, Effect::None, Relay::Id(id));
    match run_located(battle, &mut ev, None, &at) {
        Relay::Null => None,
        r => Some(r.is_truthy()),
    }
}

/// Showdown's `singleEvent(event, ability, abilityState, target, source,
/// sourceEffect, relay)` for the ability of the Pokémon at `holder`
/// (`Start` and `End` from `setAbility`, switching and fainting, ...).
/// `ability` is the one to run (the Pokémon's current or old one).
pub(crate) fn single_event_ability(
    battle: &mut Battle,
    event: Event,
    holder: Position,
    ability: AbilityId,
    source: Option<Position>,
    effect: Effect,
    relay: Relay,
) -> Relay {
    let data = battle.format.dex.ability(ability);
    if !has(data.events.0, event) || data.handler(event, EventPrefix::On).is_none() {
        return relay;
    }
    // `singleEvent`'s suppression checks.
    if event == Event::SwitchIn
        && data.flags.contains(pikapro_data::AbilityFlags::BREAKABLE)
        && abilities::suppressing_ability(battle, holder)
    {
        return relay;
    }
    if event != Event::End && abilities::ignoring_ability(battle, holder) {
        return relay;
    }
    let mut ev = EventCtx::new(event, Target::Pokemon(holder), source, effect, relay);
    abilities::run(battle, &mut ev, holder, ability).unwrap_or(relay)
}

/// Showdown's `singleEvent` for an item of the Pokémon at `holder`
/// (`Start`, `End`, `Eat`, `Use`, ...).
pub(crate) fn single_event_item(
    battle: &mut Battle,
    event: Event,
    holder: Position,
    item: ItemId,
    source: Option<Position>,
    effect: Effect,
    relay: Relay,
) -> Relay {
    let data = battle.format.dex.item(item);
    if !has(data.events.0, event) || data.handler(event, EventPrefix::On).is_none() {
        return relay;
    }
    if !matches!(event, Event::Start | Event::TakeItem | Event::SetAbility) && items::ignoring_item(battle, holder) {
        return relay;
    }
    let mut ev = EventCtx::new(event, Target::Pokemon(holder), source, effect, relay);
    items::run(battle, &mut ev, holder, item).unwrap_or(relay)
}

/// The ability's and item's `End` (`singleEvent('End', ...)`) of the
/// Pokémon at `pos` leaving the field (switching out, fainting).
pub(crate) fn end_held(battle: &mut Battle, pos: Position) {
    let Some(mon) = battle.state.side(pos.side).team.get(pos.slot as usize) else { return };
    let (ability, item) = (mon.ability, mon.item);
    single_event_ability(battle, Event::End, pos, ability, None, Effect::None, Relay::True);
    if let Some(item) = item {
        single_event_item(battle, Event::End, pos, item, None, Effect::None, Relay::True);
    }
}

// ---------------------------------------------------------------------------
// `fieldEvent` (`Residual`, `SwitchIn`): the handlers of the Pokémon this
// framework knows (abilities, items, registry conditions) join the ones
// `handlers.rs` collects, and run through [`run_field_handler`].

/// The handlers `fieldEvent` collects from the Pokémon at `pos`
/// (`findPokemonEventHandlers(pokemon, 'on<Prefix><Event>')`): its
/// registry status and volatiles, ability, item and slot conditions, with
/// `speed` as their speed (`SwitchIn` lowers it by the Pokémon's place in
/// `speedOrder`).
pub(crate) fn field_event_handlers(
    battle: &Battle,
    event: Event,
    pos: Position,
    prefix: EventPrefix,
    speed: i32,
    handlers: &mut super::handlers::Handlers,
) {
    if filtered(battle, event) {
        return;
    }
    let Some(mon) = battle.state.side(pos.side).team.get(pos.slot as usize) else { return };
    if mon_quiet(battle, mon, event) {
        debug_assert!(!has(mon_events(battle.format.dex, mon), event));
        return;
    }
    let start = handlers.len();
    pokemon_handlers(battle, pos, event, prefix, 0, handlers);
    for h in handlers[start..].iter_mut() {
        h.speed = speed;
    }
}

/// Runs one handler of a `fieldEvent` (`Residual`, `SwitchIn`):
/// Showdown's loop body. A handler whose state is gone (its ability or item
/// changed, its volatile ended) is skipped without the `faintMessages`
/// that follows every other one: false. Suppressed handlers
/// (`singleEvent`'s checks) don't run but count as run.
pub(crate) fn run_field_handler(battle: &mut Battle, event: Event, handler: &Handler) -> bool {
    let run_plain = |battle: &mut Battle| match handler.callback {
        Callback::Plain(run) => run(battle, handler),
        _ => true,
    };
    if matches!(handler.callback, Callback::Sort | Callback::Plain(_)) && handler.effect == Effect::None {
        return run_plain(battle);
    }
    let Holder::Pokemon(pos) = handler.holder else { return run_plain(battle) };
    let Some(mon) = battle.state.side(pos.side).team.get(pos.slot as usize) else { return false };
    let current = match (handler.callback, handler.effect) {
        (Callback::Held { state, .. }, Effect::Ability(a)) => {
            mon.ability == a && mon.volatile_counters.ability_order == state
        }
        (Callback::Held { state, .. }, Effect::Item(i)) => {
            mon.item == Some(i) && mon.volatile_counters.item_order == state
        }
        (_, Effect::Status(s)) => status::kind(mon.status) == Some(s),
        _ => handler.volatile() == 0 || mon.volatiles.has(handler.volatile()),
    };
    if !current {
        return false;
    }
    if single_suppressed(battle, event, handler.effect, pos) {
        return true;
    }
    let mut ev = EventCtx::new(event, Target::Pokemon(pos), None, Effect::None, Relay::True);
    let result = call(battle, &mut ev, handler);
    // A registry volatile whose `duration` ran out (a plain handler
    // returning false) ends without `faintMessages`, as for the others.
    !(matches!(handler.callback, Callback::Plain(_)) && result == Some(Relay::False))
}

/// `singleEvent`'s suppression checks for `effect` held by the Pokémon at
/// `pos`: a Mold Breaker move stops a breakable ability's `SwitchIn`;
/// Klutz, Embargo and Magic Room stop an item's (except `Start`,
/// `TakeItem`, `SetAbility`); Gastro Acid and Neutralizing Gas an
/// ability's (except `End`); Air Lock a weather's.
fn single_suppressed(battle: &Battle, event: Event, effect: Effect, pos: Position) -> bool {
    match effect {
        Effect::Ability(id) => {
            (event == Event::SwitchIn
                && battle.format.dex.ability(id).flags.contains(pikapro_data::AbilityFlags::BREAKABLE)
                && abilities::suppressing_ability(battle, pos))
                || (event != Event::End && abilities::ignoring_ability(battle, pos))
        }
        Effect::Item(_) => {
            !matches!(event, Event::Start | Event::TakeItem | Event::SetAbility) && items::ignoring_item(battle, pos)
        }
        Effect::Weather(_) => abilities::suppressing_weather(battle),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Stats (Showdown's `pokemon.getStat` / `calculateStat`).

/// The `Modify<Stat>` event of `stat`.
pub(crate) fn modify_stat_event(stat: Stat) -> Event {
    match stat {
        Stat::Atk => Event::ModifyAtk,
        Stat::Def => Event::ModifyDef,
        Stat::SpA => Event::ModifySpA,
        Stat::SpD => Event::ModifySpD,
        Stat::Hp | Stat::Spe => Event::ModifySpe,
    }
}

/// The boost of `stat` (HP has none: Speed's stands in).
fn boost_id(stat: Stat) -> BoostId {
    match stat {
        Stat::Atk => BoostId::Atk,
        Stat::Def => BoostId::Def,
        Stat::SpA => BoostId::SpA,
        Stat::SpD => BoostId::SpD,
        Stat::Hp | Stat::Spe => BoostId::Spe,
    }
}

/// The stage of `stat` in a boosts object.
fn stage_of(boosts: &Boosts, stat: Stat) -> i8 {
    boosts.get(boost_id(stat)).unwrap_or(0)
}

/// `runEvent('ModifyBoost', pokemon, null, null, boosts)`: the stages the
/// Pokémon at `pos` uses (Unaware ignores some).
pub(crate) fn modify_boost(battle: &mut Battle, pos: Position, boosts: Boosts) -> Boosts {
    let mut ev = EventCtx::new(Event::ModifyBoost, Target::Pokemon(pos), None, Effect::None, Relay::Boosts(boosts));
    match run_event(battle, &mut ev, None) {
        Relay::Boosts(b) => b,
        _ => boosts,
    }
}

/// One stage of the Pokémon at `pos` after `runEvent('ModifyBoost')`
/// (Unaware), from its current stages `b`.
pub(crate) fn boost_stage(battle: &mut Battle, pos: Position, b: crate::state::Boosts, stat: BoostId) -> i8 {
    if filtered(battle, Event::ModifyBoost) {
        return b.get(stat);
    }
    let at = Masks::locate(battle, Target::Pokemon(pos), None);
    if !at.get(battle).relevant(battle.format.active_per_side(), Target::Pokemon(pos), Event::ModifyBoost) {
        return b.get(stat);
    }
    // `modify_boost`, with the masks located.
    let boosts = all_boosts(b);
    let mut ev = EventCtx::new(Event::ModifyBoost, Target::Pokemon(pos), None, Effect::None, Relay::Boosts(boosts));
    let modified = match run_located(battle, &mut ev, None, &at) {
        Relay::Boosts(m) => m,
        _ => boosts,
    };
    modified.get(stat).unwrap_or(0)
}

/// All seven stages of a Pokémon as a boosts object.
pub(crate) fn all_boosts(b: crate::state::Boosts) -> Boosts {
    let changes = StatBoosts::new([b.atk, b.def, b.spa, b.spd, b.spe, b.accuracy, b.evasion]);
    Boosts { changes, keys: 0x7F }
}

/// Showdown's `pokemon.getStat(stat, unboosted, unmodified)` for the
/// Pokémon at `pos`: the stored stat, with its stage (after
/// `ModifyBoost`) unless `unboosted`, and `Modify<Stat>` unless
/// `unmodified`; Speed is capped at 10000.
pub(crate) fn get_stat(battle: &mut Battle, pos: Position, stat: Stat, unboosted: bool, unmodified: bool) -> u32 {
    if battle.masks_cached == Cached::No
        && !unmodified
        && !(filtered(battle, Event::ModifyBoost) && filtered(battle, modify_stat_event(stat)))
    {
        // As `with_cached_masks`, set up only when one of the events is
        // handled on the field.
        let at = Masks::locate(battle, Target::Pokemon(pos), None);
        let masks = at.get(battle);
        let events = stat_events(masks, battle.format.active_per_side(), pos, modify_stat_event(stat));
        if events == (false, false) {
            return stat_value(battle, pos, stat, unboosted, false, events);
        }
        cache(battle, at);
        let value = stat_value(battle, pos, stat, unboosted, false, events);
        battle.masks_cached = Cached::No;
        return value;
    }
    get_stat_inner(battle, pos, stat, unboosted, unmodified)
}

fn get_stat_inner(battle: &mut Battle, pos: Position, stat: Stat, unboosted: bool, unmodified: bool) -> u32 {
    // One check for both events (the common case: neither is handled).
    let stat_ev = modify_stat_event(stat);
    let events = if unmodified || filtered(battle, Event::ModifyBoost) && filtered(battle, stat_ev) {
        (false, false)
    } else {
        let at = Masks::locate(battle, Target::Pokemon(pos), None);
        let masks = at.get(battle);
        stat_events(masks, battle.format.active_per_side(), pos, stat_ev)
    };
    stat_value(battle, pos, stat, unboosted, unmodified, events)
}

/// Whether `ModifyBoost` and `stat_ev` on the Pokémon at `pos` could find a
/// handler: exactly when [`run_event`] would collect them, so a `false`
/// skips an event that would run no handler.
#[inline]
fn stat_events(masks: &Masks, active: usize, pos: Position, stat_ev: Event) -> (bool, bool) {
    let target = Target::Pokemon(pos);
    (masks.relevant(active, target, Event::ModifyBoost), masks.relevant(active, target, stat_ev))
}

/// [`get_stat`] once it is known which of `ModifyBoost` and
/// `Modify<Stat>` could be handled (`events`).
fn stat_value(
    battle: &mut Battle,
    pos: Position,
    stat: Stat,
    unboosted: bool,
    unmodified: bool,
    (boost_event, stat_event): (bool, bool),
) -> u32 {
    let (stored, boosts) = {
        let mon = &battle.state.side(pos.side).team[pos.slot as usize];
        (mon.stats[stat], mon.boosts)
    };
    let mut value = stored as u32;
    // Under Wonder Room an unmodified stat reads the other defence's stage.
    let named = if unmodified { field::defensive_stat(battle, stat) } else { stat };
    if !unboosted {
        let stage = if boost_event {
            stage_of(&modify_boost(battle, pos, all_boosts(boosts)), named)
        } else {
            boosts.stat(named)
        };
        value = crate::stats::apply_stage(value as u16, stage.clamp(-6, 6)) as u32;
    }
    if stat_event {
        value = run_modify(battle, modify_stat_event(stat), pos, None, Effect::None, value, None, None);
    }
    if stat == Stat::Spe { value.min(10000) } else { value }
}

/// Showdown's `pokemon.calculateStat(stat, boost, 1, statUser)` (the
/// damage formula's attack and defense): the stored stat (Wonder Room
/// swaps Defense and Special Defense), with the stage `boost` after
/// `ModifyBoost` on `stat_user`.
pub(crate) fn calculate_stat(battle: &mut Battle, pos: Position, stat: Stat, boost: i8, stat_user: Position) -> u32 {
    let value = battle.state.side(pos.side).team[pos.slot as usize].stats[field::defensive_stat(battle, stat)];
    // No `ModifyBoost` handler (no Unaware): the stage as it is, without
    // building the boosts object `run_event` would hand back unchanged.
    if filtered(battle, Event::ModifyBoost) || quiet(battle, Event::ModifyBoost, stat_user, None) {
        return crate::stats::apply_stage(value, boost.clamp(-6, 6)) as u32;
    }
    let mut one = Boosts { changes: StatBoosts::default(), keys: 0 };
    let id = boost_id(stat);
    one.set(id, boost);
    let stage = modify_boost(battle, stat_user, one).get(id).unwrap_or(0);
    crate::stats::apply_stage(value, stage.clamp(-6, 6)) as u32
}

// ---------------------------------------------------------------------------
// The fast path: whether anything on the field could handle an event.

/// What could handle events right now, as event masks: each slot's
/// Pokémon (ability, item, status, volatiles, slot conditions), each
/// side's conditions, the field's effects. A cheap superset of what
/// [`collect`] finds, computed once per event so that most events skip
/// collecting, and collecting skips what has nothing.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Masks {
    /// By `2 * side + slot`.
    mons: [u128; 4],
    sides: [u128; 2],
    field: u128,
    /// The events some active Pokémon handles with an `Ally`, `Foe`, `Any`
    /// or `Source` handler (on another Pokémon's event): without the bit,
    /// [`collect`] looks only at the target's own handlers.
    prefixed: u128,
}

/// A slot's mask has the [`INACTIVE`] bit when its Pokémon is not
/// `isActive` though it stands there (revived by Revival Blessing, waiting
/// for its switch): as the target, nothing of a Pokémon's or a side's is
/// collected (`findEventHandlers` needs `target.isActive ||
/// source?.isActive`), but as another Pokémon's ally or foe its handlers
/// are (`side.allies()` keeps any Pokémon with HP), and its ability's and
/// item's are skipped when their turn comes (`ignoringAbility`,
/// `ignoringItem`: `suppressed`), after the sort has drawn for them. No
/// event has this bit.
const INACTIVE: u128 = 1 << 127;
const _: () = assert!(pikapro_data::Event::ALL.len() < 127);

/// Which masks `Battle::masks_cache` stands for ([`with_cached_masks`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cached {
    /// None: outside a stretch of cached events.
    No,
    /// The memo's (`Battle::masks_memo`), which nothing changes until the
    /// stretch ends: every memo update starts from [`Masks::locate`],
    /// which finds the cache first.
    Memo,
    /// `Battle::masks_cache` itself.
    Own,
}

impl Cached {
    /// Inside a stretch of cached events.
    #[inline(always)]
    pub(crate) fn is_cached(self) -> bool {
        self != Cached::No
    }
}

/// Where the masks of an event are ([`Masks::locate`]). Most events only
/// test a bit or two, so the masks are read where they are kept rather
/// than copied out.
#[derive(Clone, Copy, Debug)]
pub(crate) enum MasksAt {
    /// `Battle::masks_cache` (inside [`with_cached_masks`]).
    Cache,
    /// `Battle::masks_memo`.
    Memo,
    /// Computed for this event only: a Pokémon off the field (a Future
    /// Sight user) or waiting for its switch after Revival Blessing.
    Own(Masks),
}

impl Masks {
    /// No events (`Battle::masks_cache` outside a stretch of cached events).
    pub(crate) const NONE: Masks = Masks { mons: [0; 4], sides: [0; 2], field: 0, prefixed: 0 };
}

impl MasksAt {
    /// The masks.
    #[inline]
    pub(crate) fn get<'a>(&'a self, battle: &'a Battle) -> &'a Masks {
        match self {
            MasksAt::Cache => match battle.masks_cached {
                Cached::Memo => &battle.masks_memo.masks,
                _ => &battle.masks_cache,
            },
            MasksAt::Memo => &battle.masks_memo.masks,
            MasksAt::Own(m) => m,
        }
    }
}

impl Masks {
    /// The masks now: the cached ones inside [`with_cached_masks`], else
    /// the battle's memo while nothing they come from changed
    /// ([`MasksMemo`]), else computed and memoised.
    #[inline]
    pub(crate) fn of(battle: &mut Battle, target: Target, source: Option<Position>) -> Masks {
        let at = Masks::locate(battle, target, source);
        *at.get(battle)
    }

    /// Makes the masks for an event on `target` from `source` current (see
    /// [`Masks::of`]) and says where they are.
    ///
    /// Inlined: every event starts here, and the common path (the cache,
    /// or a valid memo and no Pokémon off the field) is a few comparisons.
    #[inline(always)]
    pub(crate) fn locate(battle: &mut Battle, target: Target, source: Option<Position>) -> MasksAt {
        if battle.masks_cached.is_cached() {
            return MasksAt::Cache;
        }
        let active = battle.format.active_per_side();
        let off = |pos: Position| pos.slot as usize >= active;
        if battle.order.revived_in_slot || matches!(target, Target::Pokemon(t) if off(t)) || source.is_some_and(off) {
            return Masks::locate_rare(battle, target, source);
        }
        if !battle.masks_memo.valid(battle) {
            Masks::memoise(battle);
        }
        MasksAt::Memo
    }

    /// [`Masks::locate`] for `event`. For an event no Pokémon in the battle
    /// could handle with a prefixed handler (outside
    /// `Battle::prefixed_filter`) on a Pokémon in its slot, from one in its
    /// slot or none, the memo is used once the target's own inputs and
    /// the conditions are unchanged ([`MasksMemo::valid_for`]): the other
    /// Pokémon's masks, which may be stale, are not read for it.
    /// [`Masks::relevant`] and [`collect`] read the target's mask, the
    /// sides' and the field's, and the other Pokémon's (and `prefixed`)
    /// only for an event in `prefixed` (`mon_prefixed`, the prefixed part
    /// of `relevant`), which such an event never is: `prefixed` is always
    /// computed from abilities, items and volatiles whose prefixed events
    /// are in `Battle::prefixed_filter`, which only grows.
    #[inline(always)]
    pub(crate) fn locate_event(battle: &mut Battle, event: Event, target: Target, source: Option<Position>) -> MasksAt {
        if let Target::Pokemon(t) = target
            && !has(battle.prefixed_filter, event)
            && !battle.masks_cached.is_cached()
            && !battle.order.revived_in_slot
        {
            let active = battle.format.active_per_side();
            if (t.slot as usize) < active
                && source.is_none_or(|s| (s.slot as usize) < active)
                && battle.masks_memo.valid_for(battle, t)
            {
                #[cfg(debug_assertions)]
                Masks::check_partly_valid(battle, event, t, source);
                return MasksAt::Memo;
            }
        }
        Masks::locate(battle, target, source)
    }

    /// Debug builds: the memo found valid for `event` on `target`
    /// ([`Masks::locate_event`]) says what fresh masks say, and collects
    /// the same handlers.
    #[cfg(debug_assertions)]
    fn check_partly_valid(battle: &Battle, event: Event, target: Position, source: Option<Position>) {
        let memo = MasksAt::Memo.get(battle);
        let fresh = Masks::new(battle, Target::Field, None);
        let active = battle.format.active_per_side();
        let t = Target::Pokemon(target);
        assert!(!has(fresh.prefixed, event) && !has(memo.prefixed, event), "{event:?} is prefixed");
        assert_eq!(memo.relevant(active, t, event), fresh.relevant(active, t, event), "{event:?}: relevance");
        let (mut a, mut b) = (Count::default(), Count::default());
        collect(battle, memo, event, t, source, 0, &mut a);
        collect(battle, &fresh, event, t, source, 0, &mut b);
        assert_eq!(a.n, b.n, "{event:?}: handlers");
    }

    /// [`Masks::locate`] with a Pokémon revived in its slot, or a target or
    /// source off the field.
    #[cold]
    #[inline(never)]
    fn locate_rare(battle: &mut Battle, target: Target, source: Option<Position>) -> MasksAt {
        // A Pokémon revived in its slot and waiting for its switch depends
        // on the queue: no memo then.
        if super::slots::revived_waiting(battle) {
            return MasksAt::Own(Masks::new(battle, target, source));
        }
        if !battle.masks_memo.valid(battle) {
            Masks::memoise(battle);
        }
        let off_field = Masks::off_field(battle, target, source);
        if off_field != 0 {
            let mut m = *MasksAt::Memo.get(battle);
            m.prefixed |= off_field;
            return MasksAt::Own(m);
        }
        MasksAt::Memo
    }

    /// Computes the field's masks into `Battle::masks_memo`. When only some
    /// active Pokémon's inputs changed (the conditions, weather and
    /// terrain did not), only theirs are computed again: each slot's mask
    /// and prefixed events depend on nothing else ([`slot_masks`]), and
    /// `prefixed` is the union of the slots'. Debug builds check the
    /// result against masks computed from scratch.
    #[cold]
    #[inline(never)]
    fn memoise(battle: &mut Battle) {
        // A stretch of cached events may read the memo's masks ([`Cached::Memo`]).
        debug_assert!(!battle.masks_cached.is_cached(), "the masks memo updated in a cached stretch");
        if !battle.order.revived_in_slot && battle.masks_memo.conditions_valid(battle) {
            let memo = &mut battle.masks_memo;
            // Updated in place (the memo is a few hundred bytes): the
            // state and the dex are other fields of the battle.
            let (dex, active) = (battle.format.dex, battle.format.active_per_side());
            let mut prefixed = 0;
            for side in SideId::BOTH {
                let s = battle.state.side(side);
                for slot in 0..active.min(s.team.len()) {
                    let i = side.index() * 2 + slot;
                    if !memo.mons[i].matches(&s.team[slot]) {
                        memo.mons[i] = MonKey::of(&s.team[slot]);
                        (memo.masks.mons[i], memo.mon_prefixed[i]) = slot_masks(dex, s, slot);
                    }
                    prefixed |= memo.mon_prefixed[i];
                }
            }
            memo.masks.prefixed = prefixed;
            #[cfg(debug_assertions)]
            {
                let fresh = Masks::new(battle, Target::Field, None);
                let m = MasksAt::Memo.get(battle);
                assert!(
                    m.mons == fresh.mons
                        && m.sides == fresh.sides
                        && m.field == fresh.field
                        && m.prefixed == fresh.prefixed,
                    "the masks memo updated in part differs from fresh masks"
                );
            }
            return;
        }
        let mut mon_prefixed = [0; 4];
        let m = Masks::new_into(battle, Target::Field, None, &mut mon_prefixed);
        battle.masks_memo = MasksMemo::new(battle, m, mon_prefixed);
    }

    /// The events of a target or source off the field (a Future Sight
    /// user), a superset, which join `prefixed` (that `collect` then looks
    /// at).
    #[inline]
    fn off_field(battle: &Battle, target: Target, source: Option<Position>) -> u128 {
        let active = battle.format.active_per_side();
        let events = |pos: Position| {
            if (pos.slot as usize) < active {
                return 0;
            }
            battle.state.side(pos.side).team.get(pos.slot as usize).map_or(0, |mon| mon_events(battle.format.dex, mon))
        };
        let mut m = 0;
        if let Target::Pokemon(t) = target {
            m |= events(t);
        }
        if let Some(s) = source {
            m |= events(s);
        }
        m
    }

    /// A target or source off the field: see [`Masks::off_field`].
    #[inline]
    fn add_off_field(&mut self, battle: &Battle, target: Target, source: Option<Position>) {
        self.prefixed |= Masks::off_field(battle, target, source);
    }

    pub(crate) fn new(battle: &Battle, target: Target, source: Option<Position>) -> Masks {
        Masks::new_into(battle, target, source, &mut [0; 4])
    }

    /// [`Masks::new`], with each slot's prefixed events (by `2 * side +
    /// slot`, those of slots it computes) in `mon_prefixed`.
    #[inline(always)]
    fn new_into(battle: &Battle, target: Target, source: Option<Position>, mon_prefixed: &mut [u128; 4]) -> Masks {
        let mut m = Masks::default();
        if battle.format.sleep_clause_mod {
            m.field |= Event::SetStatus.bit();
        }
        let f = &battle.state.field;
        if let Some(w) = f.weather {
            m.field |= field_events(FieldKind::Weather(w));
        }
        if let Some(t) = f.terrain {
            m.field |= field_events(FieldKind::Terrain(t));
        }
        for &pw in f.pseudo_order.iter() {
            m.field |= field_events(FieldKind::Pseudo(pw));
        }
        let active = battle.format.active_per_side();
        // A Pokémon revived in its slot waits for its `instaswitch`.
        let revived = super::slots::revived_waiting(battle);
        for side in SideId::BOTH {
            let s = battle.state.side(side);
            for &c in s.conditions.order.iter() {
                m.sides[side.index()] |= side_events(c);
            }
            for slot in 0..active.min(s.team.len()) {
                let i = side.index() * 2 + slot;
                let (mask, prefixed) = slot_masks(battle.format.dex, s, slot);
                let inactive = revived && super::slots::off_field(battle, Position::new(side, slot as u8));
                m.mons[i] = if inactive { mask | INACTIVE } else { mask };
                m.prefixed |= prefixed;
                mon_prefixed[i] = prefixed;
            }
        }
        m.add_off_field(battle, target, source);
        m
    }

    /// The Pokémon at `pos` stands in its slot but is not `isActive`.
    #[inline]
    fn is_inactive(&self, pos: Position) -> bool {
        (pos.slot as usize) < 2 && self.mons[pos.side.index() * 2 + pos.slot as usize] & INACTIVE != 0
    }

    /// Whether anything could handle `event`.
    #[inline]
    pub(crate) fn any(&self, event: Event) -> bool {
        let all = self.mons.iter().fold(self.sides[0] | self.sides[1] | self.field | self.prefixed, |a, &m| a | m);
        has(all, event)
    }

    /// Whether anything could handle `event` run on `target` (in a format
    /// with `active` Pokémon per side): the target's own handlers, its
    /// side's and the field's, and (for a prefixed event) the other
    /// Pokémon's prefixed handlers and the foe side's.
    #[inline]
    pub(crate) fn relevant(&self, active: usize, target: Target, event: Event) -> bool {
        let Target::Pokemon(t) = target else { return self.any(event) };
        let pre = prefixed(event);
        // An off-field target's events are in `prefixed`.
        let own =
            if (t.slot as usize) < active { self.mons[t.side.index() * 2 + t.slot as usize] } else { self.prefixed };
        let mut m = own | self.sides[t.side.index()] | self.field;
        if pre {
            m |= self.prefixed | self.sides[t.side.foe().index()];
        }
        has(m, event)
    }

    /// Whether the Pokémon at `pos`, active in a format with `active`
    /// Pokémon per side, could handle `event` (an off-field one is looked
    /// at directly).
    #[inline]
    fn mon(&self, battle: &Battle, active: usize, pos: Position, event: Event) -> bool {
        if (pos.slot as usize) < active {
            return has(self.mons[pos.side.index() * 2 + pos.slot as usize], event);
        }
        battle
            .state
            .side(pos.side)
            .team
            .get(pos.slot as usize)
            .is_some_and(|m| has(mon_events(battle.format.dex, m), event))
    }

    /// Whether some active Pokémon has an `Ally`, `Foe`, `Any` or `Source`
    /// handler for `event`.
    #[inline]
    fn prefixed(&self, event: Event) -> bool {
        has(self.prefixed, event)
    }
}

/// The last [`Masks`] computed for the field (no Pokémon off it), with
/// what they came from: [`Battle::conditions_version`] (side, slot and
/// pseudo-weather conditions, the weather and terrain), and for each
/// active slot its Pokémon's ability, item, status and volatiles that may
/// have handlers. While all of that is unchanged, so are the masks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MasksMemo {
    /// `Battle::conditions_version`, widened: [`MasksMemo::EMPTY`]'s is
    /// none of them.
    version: u64,
    /// What the version stands for (debug builds: checked to be unchanged
    /// while the version is).
    #[cfg(debug_assertions)]
    conditions: Option<ConditionLists>,
    /// Per slot, by `2 * side + slot`.
    mons: [MonKey; 4],
    /// Per slot, its Pokémon's prefixed events ([`slot_masks`]):
    /// `masks.prefixed` is their union.
    mon_prefixed: [u128; 4],
    masks: Masks,
}

/// What a Pokémon's masks come from ([`mon_events`],
/// [`mon_events_prefixed`]): its ability and item, its status's kind (the
/// handlers don't depend on a counter), its volatiles that may have
/// handlers (in halves: every volatile of bit 64 and up may). The ability,
/// item and status kind are packed in one word ([`MonKey::pack`]): a match
/// is three comparisons, and the memo's four keys take 24 bytes each.
#[derive(Clone, Copy, Debug)]
struct MonKey {
    /// [`MonKey::pack`].
    packed: u64,
    volatiles_low: u64,
    volatiles_high: u64,
}

const _: () = assert!(VOLATILES_WITH_HANDLERS >> 64 == u64::MAX as u128);

impl MonKey {
    const NONE: MonKey =
        MonKey { packed: MonKey::pack_of(AbilityId(0), None, &Status::None), volatiles_low: 0, volatiles_high: 0 };

    /// The ability (bits 0..16), the item (16..32; `u16::MAX` for none)
    /// and the status's kind (32..40): equal exactly when all three are.
    #[inline(always)]
    const fn pack_of(ability: AbilityId, item: Option<pikapro_data::ItemId>, status: &Status) -> u64 {
        let item = match item {
            Some(i) => i.0,
            None => u16::MAX,
        };
        let status: u64 = match status {
            Status::None => 0,
            Status::Burn => 1,
            Status::Freeze { .. } => 2,
            Status::Paralysis => 3,
            Status::Poison => 4,
            Status::Toxic { .. } => 5,
            Status::Sleep { .. } => 6,
        };
        ability.0 as u64 | (item as u64) << 16 | status << 32
    }

    #[inline(always)]
    fn pack(m: &PokemonState) -> u64 {
        MonKey::pack_of(m.ability, m.item, &m.status)
    }

    #[inline]
    fn of(m: &PokemonState) -> MonKey {
        let v = m.volatiles.bits();
        MonKey {
            packed: MonKey::pack(m),
            volatiles_low: v as u64 & VOLATILES_WITH_HANDLERS as u64,
            volatiles_high: (v >> 64) as u64,
        }
    }

    /// `MonKey::of(m) == self`, field by field (cheaper than building it).
    #[inline(always)]
    fn matches(&self, m: &PokemonState) -> bool {
        let v = m.volatiles.bits();
        self.volatiles_high == (v >> 64) as u64
            && self.volatiles_low == v as u64 & VOLATILES_WITH_HANDLERS as u64
            && self.packed == MonKey::pack(m)
    }
}

impl MasksMemo {
    /// No memo yet: never valid (its version is no `u32`).
    pub(crate) const EMPTY: MasksMemo = MasksMemo {
        version: u64::MAX,
        #[cfg(debug_assertions)]
        conditions: None,
        mons: [MonKey::NONE; 4],
        mon_prefixed: [0; 4],
        masks: Masks::NONE,
    };

    fn new(battle: &Battle, masks: Masks, mon_prefixed: [u128; 4]) -> MasksMemo {
        let mut mons = [MonKey::NONE; 4];
        let active = battle.format.active_per_side();
        for side in SideId::BOTH {
            let s = battle.state.side(side);
            for slot in 0..active.min(s.team.len()) {
                mons[side.index() * 2 + slot] = MonKey::of(&s.team[slot]);
            }
        }
        MasksMemo {
            version: u64::from(battle.conditions_version),
            #[cfg(debug_assertions)]
            conditions: Some(ConditionLists::of(battle)),
            mons,
            mon_prefixed,
            masks,
        }
    }

    /// Whether the conditions, the weather and terrain, and the inputs of
    /// the Pokémon in `pos` (an active slot) are unchanged: its mask, the
    /// sides' and the field's are current (see [`Masks::locate_event`]).
    #[inline(always)]
    fn valid_for(&self, battle: &Battle, pos: Position) -> bool {
        if !self.conditions_valid(battle) {
            return false;
        }
        let key = &self.mons[pos.side.index() * 2 + pos.slot as usize];
        battle.state.side(pos.side).team.get(pos.slot as usize).is_none_or(|m| key.matches(m))
    }

    /// Whether the conditions, the weather and terrain are unchanged: one
    /// version ([`Battle::conditions_changed`]).
    #[inline(always)]
    fn conditions_valid(&self, battle: &Battle) -> bool {
        let valid = self.version == u64::from(battle.conditions_version);
        #[cfg(debug_assertions)]
        if valid {
            let now = Some(ConditionLists::of(battle));
            assert!(
                self.conditions == now,
                "conditions changed without Battle::conditions_changed: {:?} then {now:?}",
                self.conditions
            );
        }
        valid
    }

    #[inline(always)]
    fn valid(&self, battle: &Battle) -> bool {
        if !self.conditions_valid(battle) {
            return false;
        }
        // The active slots (1 or 2 per side) of each side's team.
        let doubles = battle.format.active_per_side() > 1;
        for (side, keys) in SideId::BOTH.into_iter().zip(self.mons.chunks_exact(2)) {
            let team = &battle.state.side(side).team;
            if let Some(m) = team.first()
                && !keys[0].matches(m)
            {
                return false;
            }
            if doubles
                && let Some(m) = team.get(1)
                && !keys[1].matches(m)
            {
                return false;
            }
        }
        true
    }
}

/// What [`Battle::conditions_version`] stands for: the side, slot and
/// pseudo-weather conditions present (in order), the weather and the
/// terrain. Debug builds only, to check that every change of them calls
/// [`Battle::conditions_changed`].
#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct ConditionLists {
    weather: Option<Weather>,
    terrain: Option<crate::state::Terrain>,
    pseudo_weathers: crate::util::StackVec<crate::state::PseudoWeather, 6>,
    /// Per side.
    sides: [crate::util::StackVec<crate::state::SideCondition, 16>; 2],
    /// Per side, per slot.
    slots: [[crate::util::StackVec<crate::state::SlotCondition, 5>; crate::state::MAX_ACTIVE]; 2],
}

#[cfg(debug_assertions)]
impl ConditionLists {
    fn of(battle: &Battle) -> ConditionLists {
        let f = &battle.state.field;
        let side = |s: SideId| &battle.state.side(s).conditions;
        ConditionLists {
            weather: f.weather,
            terrain: f.terrain,
            pseudo_weathers: f.pseudo_order,
            sides: SideId::BOTH.map(|s| side(s).order),
            slots: SideId::BOTH.map(|s| side(s).slots.map(|c| c.order)),
        }
    }
}

/// The events the conditions' registries handle (every status, volatile,
/// slot and side condition, weather, terrain and pseudo-weather), whatever
/// their state: [`filtered`] adds them to `Battle::event_filter`.
const CONDITION_EVENTS: u128 = STATUS_EVENTS | VOLATILE_EVENTS | SIDE_EVENTS | SLOT_EVENTS | FIELD_EVENTS;

/// Every status but `None`, for the unions below and the registry test.
/// A new status fails to compile in [`listed_status`] until it is listed
/// in both.
const STATUSES: [Status; 6] = [
    Status::Burn,
    Status::Paralysis,
    Status::Poison,
    Status::Toxic { counter: 0 },
    Status::Sleep { turns: 0 },
    Status::Freeze { turns: 0 },
];

/// [`STATUSES`]' position of a status (`None` is not listed): an
/// exhaustive match, so a new variant must be given one to compile.
const fn listed_status(s: Status) -> Option<usize> {
    match s {
        Status::None => None,
        Status::Burn => Some(0),
        Status::Paralysis => Some(1),
        Status::Poison => Some(2),
        Status::Toxic { .. } => Some(3),
        Status::Sleep { .. } => Some(4),
        Status::Freeze { .. } => Some(5),
    }
}

/// Every slot condition. A new one fails to compile in
/// [`listed_slot_condition`] until it is listed in both.
const SLOT_CONDITIONS: [SlotCondition; 5] = [
    SlotCondition::Wish,
    SlotCondition::HealingWish,
    SlotCondition::LunarDance,
    SlotCondition::RevivalBlessing,
    SlotCondition::FutureMove,
];

/// [`SLOT_CONDITIONS`]' position of a slot condition (exhaustive).
const fn listed_slot_condition(c: SlotCondition) -> usize {
    match c {
        SlotCondition::Wish => 0,
        SlotCondition::HealingWish => 1,
        SlotCondition::LunarDance => 2,
        SlotCondition::RevivalBlessing => 3,
        SlotCondition::FutureMove => 4,
    }
}

/// Every weather, terrain and pseudo-weather. A new one fails to compile
/// in [`listed_field_kind`] until it is listed in both.
const FIELD_KINDS: [FieldKind; 17] = {
    use PseudoWeather as P;
    use Weather as W;
    [
        FieldKind::Weather(W::Sun),
        FieldKind::Weather(W::Rain),
        FieldKind::Weather(W::Sand),
        FieldKind::Weather(W::Snow),
        FieldKind::Weather(W::HarshSun),
        FieldKind::Weather(W::HeavyRain),
        FieldKind::Weather(W::StrongWinds),
        FieldKind::Terrain(Terrain::Electric),
        FieldKind::Terrain(Terrain::Grassy),
        FieldKind::Terrain(Terrain::Misty),
        FieldKind::Terrain(Terrain::Psychic),
        FieldKind::Pseudo(P::TrickRoom),
        FieldKind::Pseudo(P::Gravity),
        FieldKind::Pseudo(P::MagicRoom),
        FieldKind::Pseudo(P::WonderRoom),
        FieldKind::Pseudo(P::FairyLock),
        FieldKind::Pseudo(P::EchoedVoice),
    ]
};

/// [`FIELD_KINDS`]' position of a field effect (exhaustive).
const fn listed_field_kind(kind: FieldKind) -> usize {
    use PseudoWeather as P;
    use Weather as W;
    match kind {
        FieldKind::Weather(w) => match w {
            W::Sun => 0,
            W::Rain => 1,
            W::Sand => 2,
            W::Snow => 3,
            W::HarshSun => 4,
            W::HeavyRain => 5,
            W::StrongWinds => 6,
        },
        FieldKind::Terrain(t) => match t {
            Terrain::Electric => 7,
            Terrain::Grassy => 8,
            Terrain::Misty => 9,
            Terrain::Psychic => 10,
        },
        FieldKind::Pseudo(p) => match p {
            P::TrickRoom => 11,
            P::Gravity => 12,
            P::MagicRoom => 13,
            P::WonderRoom => 14,
            P::FairyLock => 15,
            P::EchoedVoice => 16,
        },
    }
}

// The unions below also check that each list agrees with its match: entry
// `i` is the variant numbered `i` (so no variant is listed twice and none
// of those numbered is missing).

/// The events some status handles.
const STATUS_EVENTS: u128 = {
    let mut m = 0;
    let mut i = 0;
    while i < STATUSES.len() {
        assert!(matches!(listed_status(STATUSES[i]), Some(p) if p == i));
        m |= status_events(STATUSES[i]);
        i += 1;
    }
    m
};

/// The events some volatile handles.
const VOLATILE_EVENTS: u128 = {
    let mut m = 0;
    let mut bit = 0;
    while bit < 128 {
        m |= volatile_events(1u128 << bit);
        bit += 1;
    }
    m
};

/// The events some side condition handles.
const SIDE_EVENTS: u128 = {
    let mut m = 0;
    let mut i = 0;
    while i < SideCondition::ALL.len() {
        m |= side_events(SideCondition::ALL[i]);
        i += 1;
    }
    m
};

/// The events some slot condition handles.
const SLOT_EVENTS: u128 = {
    let mut m = 0;
    let mut i = 0;
    while i < SLOT_CONDITIONS.len() {
        assert!(listed_slot_condition(SLOT_CONDITIONS[i]) == i);
        m |= slot_events(SLOT_CONDITIONS[i]);
        i += 1;
    }
    m
};

/// The events some weather, terrain or pseudo-weather handles.
const FIELD_EVENTS: u128 = {
    let mut m = 0;
    let mut i = 0;
    while i < FIELD_KINDS.len() {
        assert!(listed_field_kind(FIELD_KINDS[i]) == i);
        m |= field_events(FIELD_KINDS[i]);
        i += 1;
    }
    m
};

/// By event: the volatiles that may have a prefixed handler for it, as
/// [`mon_events_prefixed`] finds them (below bit 64 exactly; of bit 64
/// and up, those with a handler for an event of
/// [`HIGH_VOLATILE_PREFIXED_EVENTS`], a superset).
const VOLATILE_PREFIXED_SOURCES: [u128; 128] = {
    let mut table = [0; 128];
    let mut bits = VOLATILES_WITH_HANDLERS;
    while bits != 0 {
        let flag = bits & bits.wrapping_neg();
        let mut events = if flag.trailing_zeros() < 64 {
            volatile_events_prefixed(flag)
        } else {
            volatile_events(flag) & HIGH_VOLATILE_PREFIXED_EVENTS
        };
        while events != 0 {
            table[events.trailing_zeros() as usize] |= flag;
            events &= events - 1;
        }
        bits &= bits - 1;
    }
    table
};

/// By event: the volatiles with a handler for it (any prefix), as
/// [`mon_events`] finds them.
const VOLATILE_SOURCES: [u128; 128] = {
    let mut table = [0; 128];
    let mut bits = VOLATILES_WITH_HANDLERS;
    while bits != 0 {
        let flag = bits & bits.wrapping_neg();
        let mut events = volatile_events(flag);
        while events != 0 {
            table[events.trailing_zeros() as usize] |= flag;
            events &= events - 1;
        }
        bits &= bits - 1;
    }
    table
};

/// `Battle::event_filter` and `Battle::prefixed_filter` for a new battle,
/// in one pass over both teams: the events of every ability and item on
/// either team (base abilities and the set's ability, which
/// `faint_forme_regression` restores, included), and for the prefixed
/// filter the events of every volatile's `Ally`, `Foe`, `Any` and `Source`
/// handlers too (the conditions' events are added by [`filtered`]).
pub(crate) fn filters(battle: &Battle) -> (u128, u128) {
    let dex = battle.format.dex;
    let mut all = 0;
    let mut prefixed = VOLATILE_PREFIXED_EVENTS;
    for side in SideId::BOTH {
        for mon in battle.state.side(side).team.iter() {
            for ability in [mon.ability, mon.base_ability, mon.set_ability] {
                let data = dex.ability(ability);
                all |= data.events.0;
                prefixed |= data.events_prefixed.0;
            }
            if let Some(item) = mon.item {
                let data = dex.item(item);
                all |= data.events.0;
                prefixed |= data.events_prefixed.0;
            }
        }
    }
    (all, prefixed)
}

/// The events of the volatiles' prefixed handlers: what
/// [`mon_events_prefixed`] can find in a Pokémon's volatiles (those of
/// [`volatile_events_prefixed`] below bit 64, and
/// [`HIGH_VOLATILE_PREFIXED_EVENTS`]).
const VOLATILE_PREFIXED_EVENTS: u128 = {
    let mut m = HIGH_VOLATILE_PREFIXED_EVENTS;
    let mut bit = 0;
    while bit < 64 {
        m |= volatile_events_prefixed(1u128 << bit);
        bit += 1;
    }
    m
};

/// The events the volatiles of bit 64 and up have a prefixed handler for:
/// exactly the union of [`module_volatile_events_prefixed`] over them,
/// which `event_masks_cover_the_registries` checks. [`quiet`] relies on
/// it (through [`VOLATILE_PREFIXED_SOURCES`]): keep it in step with the
/// modules' registries.
const HIGH_VOLATILE_PREFIXED_EVENTS: u128 = events(&[Event::Accuracy]);

/// An ability or item joins the battle (`set_ability`, `set_item`, a forme
/// change): its events join `Battle::event_filter` (and its prefixed
/// handlers' `Battle::prefixed_filter`). Anything that gives a Pokémon an
/// ability or item that was not in the battle must call this
/// (`abilities::set_ability` and the item helpers do): [`quiet`] trusts
/// `event_filter` for every event, the conditions' included.
pub(crate) fn note_held(battle: &mut Battle, ability: Option<AbilityId>, item: Option<ItemId>) {
    let dex = battle.format.dex;
    if let Some(a) = ability {
        battle.event_filter |= dex.ability(a).events.0;
        battle.prefixed_filter |= dex.ability(a).events_prefixed.0;
    }
    if let Some(i) = item {
        battle.event_filter |= dex.item(i).events.0;
        battle.prefixed_filter |= dex.item(i).events_prefixed.0;
    }
}

/// Whether `event`'s bit is in `mask` (as `mask & event.bit() != 0`, in
/// fewer instructions: one 64-bit half, then the bit).
#[inline(always)]
const fn has(mask: u128, event: Event) -> bool {
    let i = event as u32;
    let half = if i < 64 { mask as u64 } else { (mask >> 64) as u64 };
    (half >> (i & 63)) & 1 != 0
}

/// Nothing in the battle handles `event`: no condition's registry, and no
/// ability or item that has been in the battle (`Battle::event_filter`).
#[inline]
pub(crate) fn filtered(battle: &Battle, event: Event) -> bool {
    !has(battle.event_filter | CONDITION_EVENTS, event)
}

/// Whether the Pokémon `mon` has no handler of its own for `event` (its
/// ability, item, status and volatiles: `!has(mon_events(mon), event)`).
/// Its ability and item are looked at only when one in the battle handles
/// `event` (`Battle::event_filter`, a superset of theirs).
#[inline(always)]
fn mon_quiet(battle: &Battle, mon: &PokemonState, event: Event) -> bool {
    if mon.volatiles.bits() & VOLATILE_SOURCES[event as usize] != 0
        || has(STATUS_EVENTS, event) && has(status_events(mon.status), event)
    {
        return false;
    }
    if has(battle.event_filter, event) {
        let dex = battle.format.dex;
        if has(dex.ability(mon.ability).events.0, event) || mon.item.is_some_and(|i| has(dex.item(i).events.0, event)) {
            return false;
        }
    }
    true
}

/// Whether no active Pokémon could have an `Ally`, `Foe`, `Any` or
/// `Source` handler for `event` (fresh [`Masks`]' `prefixed` lacks it):
/// none of their volatiles of [`VOLATILE_PREFIXED_SOURCES`], and their
/// abilities and items only when one in the battle handles `event`.
#[inline(always)]
fn prefixed_quiet(battle: &Battle, event: Event) -> bool {
    let active = battle.format.active_per_side();
    let held = has(battle.event_filter, event);
    let dex = battle.format.dex;
    for side in SideId::BOTH {
        let team = &battle.state.side(side).team;
        for mon in team.iter().take(active) {
            if mon.volatiles.bits() & VOLATILE_PREFIXED_SOURCES[event as usize] != 0 {
                return false;
            }
            if held
                && (has(dex.ability(mon.ability).events_prefixed.0, event)
                    || mon.item.is_some_and(|i| has(dex.item(i).events_prefixed.0, event)))
            {
                return false;
            }
        }
    }
    true
}

/// No handler for `event` run on the Pokémon at `target` from `source`
/// (`findEventHandlers` finds none), told from the state at once: fresh
/// [`Masks`]' `relevant` says no. The event's possible sources are
/// constants once it is: the volatiles of [`VOLATILE_SOURCES`], a status,
/// a slot or side condition or field effect only when one handles it at
/// all (the sides' and field's masks are the memo's while the conditions
/// are unchanged). When an ability or item in the battle handles it, the
/// target's own handlers are read from the memo's mask for its slot while
/// its inputs are unchanged ([`MasksMemo::valid_for`]) rather than from
/// the dex, which takes fewer instructions but is slower (dependent
/// loads).
/// For a prefixed event that some Pokémon in the battle could handle with
/// a prefixed handler (`Battle::prefixed_filter`), the active Pokémon's
/// prefixed handlers too ([`prefixed_quiet`]). It can tell when the target
/// and the source are on the field and no Pokémon is revived in its slot:
/// else false, and the caller's generic path decides. Debug builds check
/// it against fresh masks.
#[inline(always)]
pub(crate) fn quiet(battle: &Battle, event: Event, target: Position, source: Option<Position>) -> bool {
    let active = battle.format.active_per_side();
    if battle.order.revived_in_slot
        || target.slot as usize >= active
        || source.is_some_and(|s| s.slot as usize >= active)
    {
        return false;
    }
    if prefixed(event) && has(battle.prefixed_filter, event) && !prefixed_quiet(battle, event) {
        return false;
    }
    let side = battle.state.side(target.side);
    if has(battle.event_filter, event) {
        // Its ability or item may handle it: the memo's mask for its slot.
        let memo = &battle.masks_memo;
        if !memo.valid_for(battle, target)
            || has(memo.masks.mons[target.side.index() * 2 + target.slot as usize], event)
        {
            return false;
        }
    } else if let Some(mon) = side.team.get(target.slot as usize) {
        if !mon_quiet(battle, mon, event) {
            return false;
        }
        if has(SLOT_EVENTS, event)
            && side.conditions.slots[target.slot as usize].order.iter().any(|&c| has(slot_events(c), event))
        {
            return false;
        }
    }
    if has(SIDE_EVENTS | FIELD_EVENTS, event) {
        let memo = &battle.masks_memo;
        if !memo.conditions_valid(battle) {
            return false;
        }
        let mut m = memo.masks.sides[target.side.index()] | memo.masks.field;
        if prefixed(event) {
            m |= memo.masks.sides[target.side.foe().index()];
        }
        if has(m, event) {
            return false;
        }
    }
    #[cfg(debug_assertions)]
    {
        let t = Target::Pokemon(target);
        let fresh = Masks::new(battle, t, source);
        assert!(!fresh.relevant(active, t, event), "{event:?} on {target:?} found quiet, but fresh masks say relevant");
        let mut count = Count::default();
        collect(battle, &fresh, event, t, source, 0, &mut count);
        assert_eq!(count.n, 0, "{event:?} on {target:?} found quiet, but has handlers");
    }
    true
}

/// Whether an active Pokémon, a side or the field could handle one of
/// `list` (`eachEvent`'s and `updateSpeed`'s check before running an event
/// on every active Pokémon).
pub(crate) fn any_active_handles(battle: &mut Battle, list: &[Event]) -> bool {
    if list.iter().all(|&e| filtered(battle, e)) {
        return false;
    }
    let at = Masks::locate(battle, Target::Field, None);
    let masks = at.get(battle);
    list.iter().any(|&e| masks.any(e))
}

/// Runs `f` with the event masks computed once for all its events (for
/// the Pokémon at `target` and `source`, or any active one): for a stretch
/// of events whose handlers add or remove nothing that has handlers (a
/// damage calculation, a stat).
pub(crate) fn with_cached_masks<R>(
    battle: &mut Battle,
    target: Target,
    source: Option<Position>,
    f: impl FnOnce(&mut Battle) -> R,
) -> R {
    if !battle.masks_cached.is_cached() {
        let at = Masks::locate(battle, target, source);
        cache(battle, at);
        let result = f(battle);
        battle.masks_cached = Cached::No;
        return result;
    }
    let previous = (battle.masks_cache, battle.masks_cached);
    battle.masks_cache = Masks::new(battle, target, source);
    battle.masks_cached = Cached::Own;
    let result = f(battle);
    (battle.masks_cache, battle.masks_cached) = previous;
    result
}

/// Starts a stretch of cached events with the masks `at` (located outside
/// one): the memo's are read where they are, others copied in.
#[inline]
fn cache(battle: &mut Battle, at: MasksAt) {
    battle.masks_cached = match at {
        MasksAt::Memo => Cached::Memo,
        MasksAt::Own(m) => {
            battle.masks_cache = m;
            Cached::Own
        }
        MasksAt::Cache => unreachable!("masks located outside a cached stretch"),
    };
}

/// The mask of the Pokémon in `slot` of `side` (its own events and its
/// slot conditions') and its prefixed events: what [`Masks::new`] takes
/// from that slot.
#[inline]
fn slot_masks(dex: &Dex, side: &crate::state::SideState, slot: usize) -> (u128, u128) {
    let mon = &side.team[slot];
    let mut mask = mon_events(dex, mon);
    for &c in side.conditions.slots[slot].order.iter() {
        mask |= slot_events(c);
    }
    (mask, mon_events_prefixed(dex, mon))
}

/// The events a Pokémon's ability, item, status and volatiles handle.
fn mon_events(dex: &Dex, mon: &PokemonState) -> u128 {
    let mut events = dex.ability(mon.ability).events.0 | status_events(mon.status);
    if let Some(item) = mon.item {
        events |= dex.item(item).events.0;
    }
    let mut v = mon.volatiles.bits() & VOLATILES_WITH_HANDLERS;
    while v != 0 {
        let flag = v & v.wrapping_neg();
        events |= volatile_events(flag);
        v &= v - 1;
    }
    events
}

/// The events of `mon`'s `Ally`, `Foe`, `Any` and `Source` handlers (a
/// superset: every event of the volatiles of bit 64 and up).
fn mon_events_prefixed(dex: &Dex, mon: &PokemonState) -> u128 {
    let mut events = dex.ability(mon.ability).events_prefixed.0;
    if let Some(item) = mon.item {
        events |= dex.item(item).events_prefixed.0;
    }
    let mut v = mon.volatiles.bits() & VOLATILES_WITH_HANDLERS;
    while v != 0 {
        let flag = v & v.wrapping_neg();
        events |= if volatile_owner(flag) == VolatileOwner::Here {
            volatile_events_prefixed(flag)
        } else {
            module_volatile_events_prefixed(mon, flag)
        };
        v &= v - 1;
    }
    events
}

// `Event::ALL[i]` is the event with value `i`.
const _: () = {
    let mut i = 0;
    while i < Event::ALL.len() {
        assert!(Event::ALL[i] as usize == i);
        i += 1;
    }
};

/// The events a volatile of bit 64 and up has a prefixed (not
/// `On`) handler for: its events ([`volatile_events`]) for which its
/// registry's `volatile_spec` returns one. Their registries don't depend on
/// the Pokémon, so this is exact. (Computed when the masks are, which is
/// rare: a superset such as all its events would make every Pokémon's
/// event with an `On` handler of it, say Protosynthesis's `ModifySpe`,
/// look worth collecting for.)
fn module_volatile_events_prefixed(mon: &PokemonState, flag: u128) -> u128 {
    let all = volatile_events(flag);
    let mut m = 0;
    let mut bits = all;
    while bits != 0 {
        let event = Event::ALL[bits.trailing_zeros() as usize];
        let prefixes = [EventPrefix::Source, EventPrefix::Ally, EventPrefix::Foe, EventPrefix::Any];
        if prefixes.into_iter().any(|p| volatile_spec(mon, flag, event, p).is_some()) {
            m |= event.bit();
        }
        bits &= bits - 1;
    }
    m
}

/// The events [`volatile_spec`] has a prefixed (not `On`) handler for (a
/// volatile below bit 64).
const fn volatile_events_prefixed(flag: u128) -> u128 {
    match flag {
        Volatiles::CHARGING => const { events(&[Event::BasePower, Event::ModifyDamage]) },
        Volatiles::GLAIVE_RUSH | Volatiles::MINIMIZE => const { events(&[Event::ModifyDamage]) },
        Volatiles::LOCK_ON => const { events(&[Event::Accuracy]) },
        Volatiles::UPROAR => const { events(&[Event::SetStatus]) },
        Volatiles::IMPRISON => const { events(&[Event::BeforeMove]) },
        _ => 0,
    }
}

/// The volatiles that may have handlers: those [`volatile_events`] names,
/// and every volatile of bits 64 and up. Keep it in step with
/// `volatile_events` (a test checks them).
const VOLATILES_WITH_HANDLERS: u128 = Volatiles::CHARGING
    | Volatiles::PRIORITY_CHARGE
    | Volatiles::GLAIVE_RUSH
    | Volatiles::ATTRACT
    | Volatiles::SYRUP_BOMB
    | Volatiles::CHARGE
    | Volatiles::FOCUS_ENERGY
    | Volatiles::DRAGON_CHEER
    | Volatiles::MINIMIZE
    | Volatiles::TAR_SHOT
    | Volatiles::HELPING_HAND
    | Volatiles::ENDURE
    | Volatiles::HEAL_BLOCK
    | Volatiles::DESTINY_BOND
    | Volatiles::INGRAIN
    | Volatiles::MAGNET_RISE
    | Volatiles::LOCK_ON
    | Volatiles::UPROAR
    | Volatiles::LOCKED_MOVE
    | Volatiles::IMPRISON
    | !(u64::MAX as u128);

const fn events(list: &[Event]) -> u128 {
    pikapro_data::EventSet::of(list).0
}

/// The events [`status_spec`] has a handler for (any prefix).
const fn status_events(s: Status) -> u128 {
    match s {
        Status::Paralysis => const { events(&[Event::ModifySpe]) },
        Status::Freeze { .. } => const { events(&[Event::DamagingHit, Event::AfterMoveSecondary]) },
        _ => 0,
    }
}

/// Whose registry holds a volatile's handlers ([`volatile_spec`],
/// [`volatile_events`]), by the bit ranges of [`Volatiles`]' sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VolatileOwner {
    /// Bits 0..=63: this file's own match.
    Here,
    /// `items/held.rs`: `choicelock` (bit 64), 108..=111.
    Held,
    /// `items/consumed.rs`: Gastro Acid (65), 112..=123.
    Consumed,
    /// `abilities/switchin.rs`: `commanding` (66), 68..=79.
    Switchin,
    /// `abilities/reactive.rs`: 88..=99.
    Reactive,
    /// No registry here: bit 67 (unused), 80..=87 and 100..=107 (no
    /// volatile of `abilities/damage.rs` or `abilities/misc.rs` has
    /// handlers), Octolock (124, whose trapping and residual
    /// `conditions.rs` handles itself) and the free bits 125..=127. A
    /// volatile given one of these bits gets no handlers until its range is
    /// added above.
    Elsewhere,
}

/// The registry of the volatile `flag` (a single bit).
#[inline]
const fn volatile_owner(flag: u128) -> VolatileOwner {
    match flag.trailing_zeros() {
        0..=63 => VolatileOwner::Here,
        64 | 108..=111 => VolatileOwner::Held,
        65 | 112..=123 => VolatileOwner::Consumed,
        66 | 68..=79 => VolatileOwner::Switchin,
        88..=99 => VolatileOwner::Reactive,
        _ => VolatileOwner::Elsewhere,
    }
}

/// The events [`volatile_spec`] has a handler for (any prefix, any state of
/// the volatile).
pub(crate) const fn volatile_events(flag: u128) -> u128 {
    match volatile_owner(flag) {
        VolatileOwner::Here => {}
        VolatileOwner::Held => return items::held::volatile_events(flag),
        VolatileOwner::Consumed => return items::consumed::volatile_events(flag),
        VolatileOwner::Switchin => return abilities::switchin::volatile_events(flag),
        VolatileOwner::Reactive => return abilities::reactive::volatile_events(flag),
        VolatileOwner::Elsewhere => return 0,
    }
    match flag {
        Volatiles::CHARGING => const { events(&[Event::BasePower, Event::ModifyDamage, Event::Immunity]) },
        Volatiles::MAGNET_RISE => const { events(&[Event::Immunity]) },
        Volatiles::GLAIVE_RUSH => const { events(&[Event::ModifyDamage, Event::Accuracy]) },
        Volatiles::ATTRACT | Volatiles::SYRUP_BOMB => const { events(&[Event::Update]) },
        Volatiles::ENDURE => const { events(&[Event::Damage]) },
        Volatiles::HEAL_BLOCK => const { events(&[Event::TryHeal]) },
        Volatiles::DESTINY_BOND => const { events(&[Event::Faint]) },
        Volatiles::INGRAIN => const { events(&[Event::DragOut]) },
        Volatiles::CHARGE => const { events(&[Event::BasePower, Event::AfterMove]) },
        Volatiles::LOCKED_MOVE => const { events(&[Event::AfterMove]) },
        Volatiles::FOCUS_ENERGY | Volatiles::DRAGON_CHEER => const { events(&[Event::ModifyCritRatio]) },
        Volatiles::MINIMIZE => const { events(&[Event::ModifyDamage, Event::Accuracy]) },
        Volatiles::LOCK_ON => const { events(&[Event::Accuracy]) },
        Volatiles::UPROAR => const { events(&[Event::SetStatus]) },
        Volatiles::PRIORITY_CHARGE => const { events(&[Event::TryAddVolatile]) },
        Volatiles::TAR_SHOT => const { events(&[Event::Effectiveness]) },
        Volatiles::HELPING_HAND => const { events(&[Event::BasePower]) },
        Volatiles::IMPRISON => const { events(&[Event::BeforeMove]) },
        _ => 0,
    }
}

/// The events [`slot_spec`] has a handler for.
const fn slot_events(_condition: SlotCondition) -> u128 {
    0
}

/// The events [`side_spec`] has a handler for (any prefix).
const fn side_events(condition: SideCondition) -> u128 {
    match condition {
        SideCondition::Safeguard => const { events(&[Event::SetStatus, Event::TryAddVolatile]) },
        SideCondition::Mist => const { events(&[Event::TryBoost]) },
        SideCondition::Reflect | SideCondition::LightScreen | SideCondition::AuroraVeil => {
            const { events(&[Event::ModifyDamage]) }
        }
        SideCondition::Tailwind | SideCondition::GrassPledge => const { events(&[Event::ModifySpe]) },
        _ => 0,
    }
}

/// The events [`field_spec`] has a handler for.
const fn field_events(kind: FieldKind) -> u128 {
    match kind {
        FieldKind::Weather(Weather::Sand) => const { events(&[Event::ModifySpD, Event::Weather]) },
        FieldKind::Weather(Weather::Snow) => const { events(&[Event::ModifyDef]) },
        FieldKind::Weather(Weather::Sun) => const { events(&[Event::WeatherModifyDamage, Event::Immunity]) },
        FieldKind::Weather(Weather::HarshSun) => {
            const { events(&[Event::WeatherModifyDamage, Event::Immunity, Event::TryMove]) }
        }
        FieldKind::Weather(Weather::Rain) => const { events(&[Event::WeatherModifyDamage]) },
        FieldKind::Weather(Weather::HeavyRain) => const { events(&[Event::WeatherModifyDamage, Event::TryMove]) },
        FieldKind::Weather(Weather::StrongWinds) => const { events(&[Event::Effectiveness]) },
        FieldKind::Terrain(Terrain::Misty | Terrain::Electric) => {
            const { events(&[Event::BasePower, Event::SetStatus, Event::TryAddVolatile]) }
        }
        FieldKind::Terrain(_) => const { events(&[Event::BasePower]) },
        FieldKind::Pseudo(PseudoWeather::Gravity) => const { events(&[Event::ModifyAccuracy]) },
        FieldKind::Pseudo(_) => 0,
    }
}

// ---------------------------------------------------------------------------
// The condition registries: each condition's handler for each ported event.
//
// Keep each `*_events` function above in step with its registry (a test
// checks every condition, event and prefix).

/// A field effect with handlers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Pseudo(PseudoWeather),
    Weather(Weather),
    Terrain(Terrain),
}

/// A status's handler for `event` with `prefix` (`data/conditions.ts`).
pub(crate) fn status_spec(s: Status, event: Event, prefix: EventPrefix) -> Option<Spec> {
    if prefix != EventPrefix::On {
        return None;
    }
    let spec = match (s, event) {
        (Status::Paralysis, Event::ModifySpe) => Spec::relay(status::par_modify_spe).priority(-101),
        (Status::Freeze { .. }, Event::DamagingHit) => Spec::relay(status::frz_damaging_hit),
        (Status::Freeze { .. }, Event::AfterMoveSecondary) => Spec::relay(status::frz_after_move_secondary),
        _ => return None,
    };
    Some(spec)
}

/// A volatile's handler for `event` with `prefix`. The volatiles of bits 64
/// and up are looked up in the ability and item modules.
pub(crate) fn volatile_spec(mon: &PokemonState, flag: u128, event: Event, prefix: EventPrefix) -> Option<Spec> {
    match volatile_owner(flag) {
        VolatileOwner::Here => {}
        VolatileOwner::Held => return items::held::volatile_spec(flag, event, prefix),
        VolatileOwner::Consumed => return items::consumed::volatile_spec(flag, event, prefix),
        VolatileOwner::Switchin => return abilities::switchin::volatile_spec(flag, event, prefix),
        VolatileOwner::Reactive => return abilities::reactive::volatile_spec(flag, event, prefix),
        VolatileOwner::Elsewhere => return None,
    }
    use EventPrefix::{Any, Foe, On, Source};
    let spec = match (flag, event, prefix) {
        // The semi-invulnerable moves' `CHARGING` (`multiturn.rs`).
        (Volatiles::CHARGING, Event::BasePower, Source)
            if mon.volatile_counters.charge_move == pikapro_data::gen9::moves::BOUNCE =>
        {
            Spec::relay(multiturn::bounce_source_base_power)
        }
        (Volatiles::CHARGING, Event::ModifyDamage, Source)
            if multiturn::has_source_modify_damage(mon.volatile_counters.charge_move) =>
        {
            Spec::relay(multiturn::semi_invulnerable_source_damage)
        }
        (Volatiles::CHARGING, Event::Immunity, On)
            if matches!(
                mon.volatile_counters.charge_move,
                pikapro_data::gen9::moves::DIG | pikapro_data::gen9::moves::DIVE
            ) =>
        {
            Spec::relay(multiturn::dig_immunity)
        }
        (Volatiles::MAGNET_RISE, Event::Immunity, On) => Spec::relay(conditions::magnet_rise_immunity),
        (Volatiles::GLAIVE_RUSH, Event::ModifyDamage, Source) => Spec::relay(conditions::glaive_rush_source_damage),
        (Volatiles::GLAIVE_RUSH, Event::Accuracy, On) => Spec::relay(conditions::glaive_rush_accuracy),
        (Volatiles::ATTRACT | Volatiles::SYRUP_BOMB, Event::Update, On) => Spec::relay(conditions::source_left_update),
        (Volatiles::ENDURE, Event::Damage, On) => Spec::relay(conditions::endure_damage).priority(-10),
        (Volatiles::HEAL_BLOCK, Event::TryHeal, On) => Spec::relay(conditions::heal_block_try_heal),
        (Volatiles::IMPRISON, Event::BeforeMove, Foe) => Spec::plain(conditions::imprison_before_move).priority(4),
        (Volatiles::DESTINY_BOND, Event::Faint, On) => Spec::plain(conditions::destiny_bond_faint),
        (Volatiles::INGRAIN, Event::DragOut, On) => Spec::relay(manipulation::ingrain_drag_out),
        (Volatiles::CHARGE, Event::BasePower, On) => Spec::relay(manipulation::charge_base_power).priority(9),
        (Volatiles::CHARGE, Event::AfterMove, On) => Spec::relay(manipulation::charge_after_move),
        (Volatiles::LOCKED_MOVE, Event::AfterMove, On) => Spec::relay(multiturn::locked_move_after_move),
        (Volatiles::FOCUS_ENERGY | Volatiles::DRAGON_CHEER, Event::ModifyCritRatio, On) => {
            Spec::relay(manipulation::crit_ratio_handler)
        }
        (Volatiles::MINIMIZE, Event::ModifyDamage, Source) => Spec::relay(manipulation::minimize_source_damage),
        (Volatiles::MINIMIZE, Event::Accuracy, On) => Spec::relay(manipulation::minimize_accuracy),
        (Volatiles::LOCK_ON, Event::Accuracy, Source) => Spec::relay(manipulation::lock_on_accuracy),
        (Volatiles::UPROAR, Event::SetStatus, Any) => Spec::relay(multiturn::uproar_set_status),
        (Volatiles::PRIORITY_CHARGE, Event::TryAddVolatile, On)
            if mon.volatile_counters.priority_charge == pikapro_data::gen9::moves::FOCUS_PUNCH =>
        {
            Spec::relay(multiturn::focus_punch_try_add_volatile)
        }
        (Volatiles::TAR_SHOT, Event::Effectiveness, On) => {
            Spec::relay(manipulation::tar_shot_effectiveness).priority(-2)
        }
        (Volatiles::HELPING_HAND, Event::BasePower, On) => Spec::relay(order::helping_hand_base_power).priority(10),
        _ => return None,
    };
    Some(spec)
}

/// A slot condition's handler for `event` with `prefix` (none of the
/// slot conditions handles a ported event; `SwitchIn` and `Residual` run
/// through `fieldEvent`).
pub(crate) fn slot_spec(_condition: SlotCondition, _event: Event, _prefix: EventPrefix) -> Option<Spec> {
    None
}

/// A side condition's handler for `event` with `prefix`.
pub(crate) fn side_spec(condition: SideCondition, event: Event, prefix: EventPrefix) -> Option<Spec> {
    use EventPrefix::On;
    let spec = match (condition, event, prefix) {
        (SideCondition::Safeguard, Event::SetStatus | Event::TryAddVolatile, On) => {
            Spec::relay(field::safeguard_status)
        }
        (SideCondition::Mist, Event::TryBoost, On) => Spec::relay(field::mist_try_boost),
        (
            SideCondition::Reflect | SideCondition::LightScreen | SideCondition::AuroraVeil,
            Event::ModifyDamage,
            EventPrefix::Any,
        ) => Spec::relay(field::screen_modify_damage),
        (SideCondition::Tailwind | SideCondition::GrassPledge, Event::ModifySpe, On) => {
            Spec::relay(field::side_modify_spe)
        }
        _ => return None,
    };
    Some(spec)
}

/// A field effect's handler for `event` (always `on<Event>`).
pub(crate) fn field_spec(kind: FieldKind, event: Event) -> Option<Spec> {
    let spec = match (kind, event) {
        (FieldKind::Terrain(Terrain::Misty | Terrain::Electric), Event::SetStatus | Event::TryAddVolatile) => {
            Spec::relay(field::terrain_status)
        }
        (FieldKind::Weather(Weather::Sand), Event::ModifySpD) => Spec::relay(field::sand_modify_spd).priority(10),
        (FieldKind::Weather(Weather::Snow), Event::ModifyDef) => Spec::relay(field::snow_modify_def).priority(10),
        (
            FieldKind::Weather(Weather::Sun | Weather::Rain | Weather::HarshSun | Weather::HeavyRain),
            Event::WeatherModifyDamage,
        ) => Spec::relay(field::weather_modify_damage),
        (FieldKind::Weather(Weather::StrongWinds), Event::Effectiveness) => {
            Spec::relay(field::delta_stream_effectiveness).priority(-1)
        }
        (FieldKind::Terrain(_), Event::BasePower) => Spec::relay(field::terrain_base_power).priority(6),
        (FieldKind::Weather(Weather::Sun | Weather::HarshSun), Event::Immunity) => Spec::relay(field::sun_immunity),
        (FieldKind::Pseudo(PseudoWeather::Gravity), Event::ModifyAccuracy) => {
            Spec::relay(field::gravity_modify_accuracy)
        }
        (FieldKind::Weather(Weather::Sand), Event::Weather) => Spec::relay(field::sand_weather),
        (FieldKind::Weather(Weather::HarshSun | Weather::HeavyRain), Event::TryMove) => {
            Spec::relay(field::primal_weather_try_move).priority(1)
        }
        _ => return None,
    };
    Some(spec)
}

#[cfg(test)]
mod status_order_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BattleOptions, Format, Seed};

    const PREFIXES: [EventPrefix; 5] =
        [EventPrefix::On, EventPrefix::Source, EventPrefix::Ally, EventPrefix::Foe, EventPrefix::Any];

    /// Every registry entry is in its `*_events` mask, so `event_filter`
    /// and the [`Masks`] never skip a handler.
    #[test]
    fn event_masks_cover_the_registries() {
        use pikapro_data::gen9::moves as m;
        let mut statuses = STATUSES;
        statuses[4] = Status::Sleep { turns: 1 };
        // The events of the prefixed handlers of volatiles of bit 64 and up.
        let mut high_prefixed = 0;
        // States a volatile's handlers depend on (the charging move).
        let mons: Vec<PokemonState> = [m::FLY, m::DIG, m::DIVE, m::BOUNCE, m::PHANTOM_FORCE, m::SOLAR_BEAM]
            .iter()
            .map(|&mv| {
                let mut mon = PokemonState::default();
                mon.volatile_counters.charge_move = mv;
                mon
            })
            .collect();
        for event in Event::ALL {
            for prefix in PREFIXES {
                for s in statuses {
                    if status_spec(s, event, prefix).is_some() {
                        assert!(status_events(s) & event.bit() != 0, "{s:?} {event:?}");
                    }
                }
                for bit in 0..128 {
                    let flag = 1u128 << bit;
                    for mon in &mons {
                        if volatile_spec(mon, flag, event, prefix).is_some() {
                            assert!(volatile_events(flag) & event.bit() != 0, "volatile bit {bit} {event:?}");
                            if prefix != EventPrefix::On {
                                let prefixed = if bit >= 64 {
                                    high_prefixed |= event.bit();
                                    module_volatile_events_prefixed(mon, flag)
                                } else {
                                    volatile_events_prefixed(flag)
                                };
                                assert!(prefixed & event.bit() != 0, "prefixed volatile bit {bit} {event:?}");
                                // `Battle::prefixed_filter` starts from it.
                                assert!(
                                    VOLATILE_PREFIXED_EVENTS & event.bit() != 0,
                                    "VOLATILE_PREFIXED_EVENTS: volatile bit {bit} {event:?}"
                                );
                            }
                        }
                        if volatile_events(flag) != 0 {
                            assert!(VOLATILES_WITH_HANDLERS & flag != 0, "volatile bit {bit}");
                        }
                    }
                }
                for c in SideCondition::ALL {
                    if side_spec(c, event, prefix).is_some() {
                        assert!(side_events(c) & event.bit() != 0, "{c:?} {event:?}");
                    }
                }
                for c in SLOT_CONDITIONS {
                    if slot_spec(c, event, prefix).is_some() {
                        assert!(slot_events(c) & event.bit() != 0, "{c:?} {event:?}");
                    }
                }
            }
            for kind in FIELD_KINDS {
                if field_spec(kind, event).is_some() {
                    assert!(field_events(kind) & event.bit() != 0, "{kind:?} {event:?}");
                }
            }
        }
        // `quiet` trusts it exactly: a prefixed handler of a volatile of bit
        // 64 and up for an event outside it would be skipped.
        assert_eq!(high_prefixed, HIGH_VOLATILE_PREFIXED_EVENTS, "HIGH_VOLATILE_PREFIXED_EVENTS");
    }

    fn singles(p1: crate::PokemonSet, p2: crate::PokemonSet) -> Battle {
        let format = Format::gen9_singles();
        let team = |set: crate::PokemonSet| crate::Team::new(vec![set]);
        Battle::new(format, [team(p1), team(p2)], BattleOptions::new(Seed::fast(1))).unwrap()
    }

    fn set(species: pikapro_data::SpeciesId, ability: AbilityId, item: Option<ItemId>) -> crate::PokemonSet {
        use pikapro_data::gen9::moves as m;
        let s = crate::PokemonSet::new(species).with_level(50).with_ability(ability).with_moves(&[m::TACKLE]);
        match item {
            Some(i) => s.with_item(i),
            None => s,
        }
    }

    const P1: Position = Position::new(SideId::P1, 0);
    const P2: Position = Position::new(SideId::P2, 0);

    #[test]
    fn a_dragged_move_user_no_longer_suppresses_abilities() {
        use pikapro_data::gen9::{abilities as a, items as i, moves as m, species as s};
        let teams = [
            crate::Team::new(vec![set(s::CARBINK, a::CLEAR_BODY, Some(i::RED_CARD))]),
            crate::Team::new(vec![set(s::HAWLUCHA, a::MOLD_BREAKER, None), set(s::SHINX, a::INTIMIDATE, None)]),
        ];
        let mut b = Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(50_000_000))).unwrap();
        b.active_move = Some((P2, m::TACKLE));
        b.move_state.ignore_ability = true;
        assert!(abilities::suppressing_ability(&b, P1));
        let original = super::super::queue::MonRef { side: SideId::P2, id: b.state.at(P2).unwrap().id };
        let incoming = super::super::queue::MonRef { side: SideId::P2, id: b.state.side(SideId::P2).team[1].id };
        super::super::switch_in(&mut b, incoming, 0, None, true);
        assert_eq!(b.active_move, Some((Position::new(SideId::P2, 1), m::TACKLE)));
        assert!(!abilities::suppressing_ability(&b, P1));
        assert_eq!(b.state.at(P1).unwrap().boosts.atk, 0, "Clear Body must block the incoming Intimidate");
        // Follow the original object if a later switch brings it back
        // before clearActiveMove, rather than permanently clearing the move.
        super::super::switch_in(&mut b, original, 0, None, true);
        assert_eq!(b.active_move, Some((P2, m::TACKLE)));
        assert!(abilities::suppressing_ability(&b, P1));
    }

    /// `chainModify` rounds each step (`(a * b + 2048) >> 12`) and the
    /// relay is modified once at the end, as `runEvent` does.
    #[test]
    fn chain_modify_rounds_like_showdown() {
        let mut ev = EventCtx::new(Event::BasePower, Target::Pokemon(P1), None, Effect::None, Relay::Num(60));
        ev.chain_modify(3, 2);
        ev.chain(5324);
        assert_eq!(ev.modifier, (6144 * 5324 + 2048) >> 12);
        assert_eq!(ev.final_modify(100), crate::stats::modify(100, (6144 * 5324 + 2048) >> 12));
        assert_eq!(ev.modifier, 4096);
    }

    /// Huge Power doubles Attack (`onModifyAtk`); Choice Scarf multiplies
    /// Speed by 1.5 (`onModifySpe`), through `getStat`.
    #[test]
    fn stat_modifiers_run_through_get_stat() {
        use pikapro_data::gen9::{abilities as a, items as i, species as s};
        let mut b =
            singles(set(s::AZUMARILL, a::HUGE_POWER, Some(i::CHOICE_SCARF)), set(s::AZUMARILL, a::THICK_FAT, None));
        let (atk, spe) = {
            let m = b.state.at(P1).unwrap();
            (m.stats[Stat::Atk] as u32, m.stats[Stat::Spe] as u32)
        };
        assert_eq!(get_stat(&mut b, P1, Stat::Atk, false, false), atk * 2);
        assert_eq!(get_stat(&mut b, P1, Stat::Spe, false, false), crate::stats::modify(spe, 6144));
        // Unmodified: no handlers.
        assert_eq!(get_stat(&mut b, P1, Stat::Atk, false, true), atk);
        // Thick Fat only modifies a Fire or Ice move's attack: no change
        // without one.
        let foe_atk = b.state.at(P2).unwrap().stats[Stat::Atk] as u32;
        assert_eq!(get_stat(&mut b, P2, Stat::Atk, false, false), foe_atk);
    }

    /// Technician boosts moves of 60 base power or less (after the
    /// modifiers so far), and not stronger ones.
    #[test]
    fn technician_checks_the_modified_base_power() {
        use pikapro_data::gen9::{abilities as a, moves as m, species as s};
        let mut b = singles(set(s::SCIZOR, a::TECHNICIAN, None), set(s::AZUMARILL, a::THICK_FAT, None));
        let data = *b.format.dex.move_(m::TACKLE);
        let bp = |b: &mut Battle, v| {
            run_modify(b, Event::BasePower, P1, Some(P2), Effect::Move(m::TACKLE), v, Some(&data), None)
        };
        assert_eq!(bp(&mut b, 40), 60);
        assert_eq!(bp(&mut b, 60), 90);
        assert_eq!(bp(&mut b, 80), 80);
    }

    /// Multiscale halves damage at full HP (`onSourceModifyDamage`), and a
    /// Mold Breaker move (`move.ignoreAbility`) ignores it.
    #[test]
    fn multiscale_is_breakable() {
        use pikapro_data::gen9::{abilities as a, moves as m, species as s};
        let mut b = singles(set(s::GARCHOMP, a::ROUGH_SKIN, None), set(s::DRAGONITE, a::MULTISCALE, None));
        let data = *b.format.dex.move_(m::TACKLE);
        b.active_move = Some((P1, m::TACKLE));
        let damage = |b: &mut Battle| {
            run_modify(b, Event::ModifyDamage, P1, Some(P2), Effect::Move(m::TACKLE), 100, Some(&data), None)
        };
        // `ModifyDamage` runs on the attacker; Multiscale is the target's
        // `onSourceModifyDamage`.
        assert_eq!(damage(&mut b), 50);
        b.move_state.ignore_ability = true;
        assert_eq!(damage(&mut b), 100);
        b.move_state.ignore_ability = false;
        b.state.at_mut(P2).unwrap().hp -= 1;
        assert_eq!(damage(&mut b), 100);
    }

    /// Focus Sash leaves its holder at 1 HP from full (a move's damage) and
    /// is used up; Sitrus Berry is eaten at half HP on `Update`.
    #[test]
    fn focus_sash_and_sitrus_berry() {
        use pikapro_data::gen9::{abilities as a, items as i, moves as m, species as s};
        let mut b =
            singles(set(s::GARCHOMP, a::ROUGH_SKIN, None), set(s::DRAGONITE, a::INNER_FOCUS, Some(i::FOCUS_SASH)));
        b.active_move = Some((P1, m::TACKLE));
        let max = b.state.at(P2).unwrap().max_hp();
        let dealt = crate::resolve::apply_damage(&mut b, P2, 1000, crate::resolve::DamageSource::Move);
        assert_eq!(dealt, max - 1);
        let m2 = b.state.at(P2).unwrap();
        assert_eq!((m2.hp, m2.item, m2.last_item), (1, None, Some(i::FOCUS_SASH)));

        let mut b =
            singles(set(s::GARCHOMP, a::ROUGH_SKIN, None), set(s::DRAGONITE, a::INNER_FOCUS, Some(i::SITRUS_BERRY)));
        let max = b.state.at(P2).unwrap().max_hp();
        b.state.at_mut(P2).unwrap().hp = max / 2;
        crate::resolve::each_update(&mut b);
        let m2 = b.state.at(P2).unwrap();
        assert_eq!(m2.hp, max / 2 + max / 4);
        assert!(m2.item.is_none() && m2.ate_berry);
    }
}
