//! Data-table row types and the [`Dex`] that owns a full set of tables.
//!
//! The rows mirror Pokémon Showdown's data (`data/pokedex.ts`,
//! `data/moves.ts`, ...) and are generated from it by `tools/datagen`.
//! Anything Showdown expresses as code (event callbacks) or as a field this
//! schema does not represent is listed by name in
//! [`MoveData::unmodelled`], so the engine can tell exactly which moves are
//! fully described by data.

use crate::{AbilityId, BoostId, ItemId, MoveId, SpeciesId, Stats, TeraType, Type, TypeSet};

/// Why an entry is not part of a dex's standard game (Showdown's
/// `isNonstandard`). `None` on a row means it is standard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Nonstandard {
    /// From an earlier game, not obtainable in this one.
    Past,
    /// Not (yet) in this game, e.g. Champions-only Megas in the Gen 9 dex.
    Future,
    /// In the game's data but not obtainable.
    Unobtainable,
}

/// A fixed gender, or the male share of a gendered species.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GenderRule {
    /// Always male.
    Male,
    /// Always female.
    Female,
    /// No gender.
    Genderless,
    /// Randomly male or female; `male_per_mille` of 1000 are male.
    Ratio {
        /// Males per thousand.
        male_per_mille: u16,
    },
}

/// A species' ability slots (Showdown's `abilities: {0, 1, H, S}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AbilitySlots {
    /// Slot `0`.
    pub primary: AbilityId,
    /// Slot `1`.
    pub secondary: Option<AbilityId>,
    /// Slot `H`: the hidden ability.
    pub hidden: Option<AbilityId>,
    /// Slot `S`: an event-only ability.
    pub special: Option<AbilityId>,
}

/// A species or forme (Showdown's `Species`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpeciesData {
    /// Showdown's name (`"Charizard-Mega-X"`).
    pub name: &'static str,
    /// National Dex number.
    pub num: i16,
    /// Its types.
    pub types: TypeSet,
    /// Base stats.
    pub base_stats: Stats<u8>,
    /// Ability slots.
    pub abilities: AbilitySlots,
    /// Weight in hectograms (0.1 kg).
    pub weight_hg: u16,
    /// Gender or gender ratio.
    pub gender: GenderRule,
    /// The base species; equals this species' own ID for base formes.
    pub base_species: SpeciesId,
    /// Forme name (`"Mega"`, `"Alola"`, ...), empty for base formes.
    pub forme: &'static str,
    /// Items required to use this forme (Mega Stones, plates, masks).
    pub required_items: &'static [ItemId],
    /// Only exists in battle (Megas, Zacian-Crowned, ...).
    pub battle_only: bool,
    /// The only Tera type this species can have (Ogerpon, Terapagos).
    pub required_tera_type: Option<TeraType>,
    /// Fixed maximum HP (Shedinja: 1).
    pub max_hp: Option<u16>,
    /// Showdown's `nfe`: it can still evolve into a species of its dex
    /// (Eviolite).
    pub nfe: bool,
    /// Showdown's `tags` include `"Paradox"` (Booster Energy can't be taken
    /// from it).
    pub paradox: bool,
    /// Showdown's `isNonstandard` (`None`: standard in this dex).
    pub nonstandard: Option<Nonstandard>,
}

/// A move's damage category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MoveCategory {
    /// Uses Attack and Defense.
    Physical,
    /// Uses Special Attack and Special Defense.
    Special,
    /// Deals no direct damage.
    Status,
}

/// Who a move may target, mirroring Showdown's `MoveTarget` strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MoveTarget {
    /// `normal`: one adjacent Pokémon of the user's choice.
    Normal,
    /// `adjacentFoe`
    AdjacentFoe,
    /// `adjacentAlly`
    AdjacentAlly,
    /// `adjacentAllyOrSelf`
    AdjacentAllyOrSelf,
    /// `any`: any other Pokémon, including non-adjacent (only differs in triples).
    Any,
    /// `allAdjacentFoes`: spread move hitting all adjacent foes.
    AllAdjacentFoes,
    /// `allAdjacent`: spread move hitting all adjacent Pokémon, allies included.
    AllAdjacent,
    /// `self`
    User,
    /// `allies`
    Allies,
    /// `allySide`
    AllySide,
    /// `foeSide`
    FoeSide,
    /// `all`: the whole field (weather, terrain, Trick Room).
    All,
    /// `randomNormal`
    RandomNormal,
    /// `allyTeam`
    AllyTeam,
    /// `scripted` (Counter, Metal Burst, ...)
    Scripted,
}

impl MoveTarget {
    /// Whether the player must pick a target for this move in a multi-battle.
    pub const fn requires_choice(self) -> bool {
        matches!(
            self,
            MoveTarget::Normal
                | MoveTarget::AdjacentFoe
                | MoveTarget::AdjacentAlly
                | MoveTarget::AdjacentAllyOrSelf
                | MoveTarget::Any
        )
    }
}

/// Bit flags mirroring Showdown's `move.flags`, one per flag name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MoveFlags(pub u64);

macro_rules! move_flags {
    ($($(#[$doc:meta])* $name:ident = $bit:literal, $id:literal;)*) => {
        impl MoveFlags {
            $($(#[$doc])* pub const $name: MoveFlags = MoveFlags(1 << $bit);)*

            /// Every flag with its Showdown name.
            pub const ALL: &'static [(&'static str, MoveFlags)] = &[$(($id, MoveFlags::$name)),*];
        }
    };
}

move_flags! {
    /// Animates on an ally (`allyanim`).
    ALLY_ANIM = 0, "allyanim";
    /// Ignores Substitute.
    BYPASS_SUB = 1, "bypasssub";
    /// A biting move (Strong Jaw).
    BITE = 2, "bite";
    /// A ball or bomb move (Bulletproof).
    BULLET = 3, "bullet";
    /// Can't be used twice in a row (Gigaton Hammer).
    CANT_USE_TWICE = 4, "cantusetwice";
    /// Two-turn move.
    CHARGE = 5, "charge";
    /// Makes contact.
    CONTACT = 6, "contact";
    /// A dance move (Dancer).
    DANCE = 7, "dance";
    /// Thaws the user.
    DEFROST = 8, "defrost";
    /// Can target any Pokémon in triples.
    DISTANCE = 9, "distance";
    /// Copycat can't call it.
    FAIL_COPYCAT = 10, "failcopycat";
    /// Encore can't lock it.
    FAIL_ENCORE = 11, "failencore";
    /// Instruct can't repeat it.
    FAIL_INSTRUCT = 12, "failinstruct";
    /// Me First can't copy it.
    FAIL_ME_FIRST = 13, "failmefirst";
    /// Mimic can't copy it.
    FAIL_MIMIC = 14, "failmimic";
    /// Hits two turns later (Future Sight, Doom Desire).
    FUTURE_MOVE = 15, "futuremove";
    /// Unusable under Gravity.
    GRAVITY = 16, "gravity";
    /// Blocked by Heal Block.
    HEAL = 17, "heal";
    /// Callable by Metronome.
    METRONOME = 18, "metronome";
    /// Double damage against Minimize.
    MINIMIZE = 19, "minimize";
    /// Copyable by Mirror Move.
    MIRROR = 20, "mirror";
    /// Pressure deducts PP even without a target on its side.
    MUST_PRESSURE = 21, "mustpressure";
    /// Assist can't call it.
    NO_ASSIST = 22, "noassist";
    /// Can't be used or selected in a Sky Battle.
    NON_SKY = 23, "nonsky";
    /// Parental Bond doesn't make it hit twice.
    NO_PARENTAL_BOND = 24, "noparentalbond";
    /// Sketch can't copy it.
    NO_SKETCH = 25, "nosketch";
    /// Sleep Talk can't call it.
    NO_SLEEP_TALK = 26, "nosleeptalk";
    /// A Pledge that combines with another.
    PLEDGE_COMBO = 27, "pledgecombo";
    /// A powder move (Grass types and Overcoat are immune).
    POWDER = 28, "powder";
    /// Blocked by Protect and similar.
    PROTECT = 29, "protect";
    /// A pulse move (Mega Launcher).
    PULSE = 30, "pulse";
    /// A punching move (Iron Fist).
    PUNCH = 31, "punch";
    /// The user must recharge next turn.
    RECHARGE = 32, "recharge";
    /// Reflected by Magic Coat / Magic Bounce.
    REFLECTABLE = 33, "reflectable";
    /// A slicing move (Sharpness).
    SLICING = 34, "slicing";
    /// Snatch can steal it.
    SNATCH = 35, "snatch";
    /// A sound move (Soundproof, Throat Chop).
    SOUND = 36, "sound";
    /// A wind move (Wind Rider, Wind Power).
    WIND = 37, "wind";
}

impl MoveFlags {
    /// No flag.
    pub const NONE: MoveFlags = MoveFlags(0);

    /// The union of `flags` (usable in `const` tables).
    pub const fn of(flags: &[MoveFlags]) -> MoveFlags {
        let mut bits = 0;
        let mut i = 0;
        while i < flags.len() {
            bits |= flags[i].0;
            i += 1;
        }
        MoveFlags(bits)
    }

    /// Whether every flag of `other` is set.
    #[inline]
    pub const fn contains(self, other: MoveFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// The flags of both.
    #[inline]
    pub const fn union(self, other: MoveFlags) -> MoveFlags {
        MoveFlags(self.0 | other.0)
    }
}

impl core::ops::BitOr for MoveFlags {
    type Output = MoveFlags;
    fn bitor(self, rhs: MoveFlags) -> MoveFlags {
        self.union(rhs)
    }
}

/// A non-volatile status a move can inflict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatusId {
    /// `brn`.
    Burn,
    /// `par`.
    Paralysis,
    /// `slp`.
    Sleep,
    /// `frz`.
    Freeze,
    /// `psn`.
    Poison,
    /// `tox`: bad poison.
    Toxic,
}

/// Stat-stage changes, and the order Showdown lists them in.
///
/// Showdown applies (and announces) a boosts object in its key order, which
/// is not always atk, def, spa, ... (Shell Smash lowers def and spd first).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StatBoosts {
    /// Stages by [`BoostId`]: atk, def, spa, spd, spe, accuracy, evasion.
    pub stages: [i8; 7],
    /// Every [`BoostId`] once, in Showdown's key order.
    pub order: [BoostId; 7],
}

impl StatBoosts {
    /// Changes listed in [`BoostId::ALL`] order.
    pub const fn new(stages: [i8; 7]) -> StatBoosts {
        StatBoosts { stages, order: BoostId::ALL }
    }

    /// Changes listed in `order` (indices into [`BoostId::ALL`]).
    pub const fn ordered(stages: [i8; 7], order: [u8; 7]) -> StatBoosts {
        let mut ids = BoostId::ALL;
        let mut i = 0;
        while i < 7 {
            ids[i] = BoostId::ALL[order[i] as usize];
            i += 1;
        }
        StatBoosts { stages, order: ids }
    }

    /// The stage change for `id`.
    #[inline]
    pub const fn get(&self, id: BoostId) -> i8 {
        self.stages[id as usize]
    }

    /// The non-zero changes, in Showdown's order.
    pub fn iter(&self) -> impl Iterator<Item = (BoostId, i8)> + '_ {
        self.order.iter().map(|&id| (id, self.stages[id as usize])).filter(|&(_, n)| n != 0)
    }
}

impl Default for StatBoosts {
    fn default() -> StatBoosts {
        StatBoosts::new([0; 7])
    }
}

/// A ratio such as drain or recoil (`[1, 2]` = half).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fraction {
    /// The numerator.
    pub numerator: u8,
    /// The denominator.
    pub denominator: u8,
}

/// A chance-based effect of a damaging move (Showdown's `secondaries`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Secondary {
    /// Percent chance.
    pub chance: u8,
    /// Status inflicted on the target.
    pub status: Option<StatusId>,
    /// Showdown condition ID, e.g. `"flinch"`, `"confusion"`.
    pub volatile_status: Option<&'static str>,
    /// Stat changes for the target.
    pub boosts: Option<StatBoosts>,
    /// Stat changes for the user.
    pub self_boosts: Option<StatBoosts>,
    /// Showdown fields/callbacks of this secondary that are not represented.
    pub unmodelled: &'static [&'static str],
}

/// How a move switches the user out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SelfSwitch {
    /// U-turn, Volt Switch, ...
    Switch,
    /// Baton Pass: passes stat changes and some volatiles.
    CopyVolatile,
    /// Shed Tail: passes a Substitute.
    ShedTail,
}

/// A move (Showdown's `Move`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveData {
    /// Showdown's name.
    pub name: &'static str,
    /// Showdown's `num`.
    pub num: i16,
    /// Its type.
    pub ty: Type,
    /// Physical, special or status.
    pub category: MoveCategory,
    /// Base power (0 for status moves and moves that compute their own).
    pub base_power: u8,
    /// `None` means the move never misses (Showdown's `accuracy: true`).
    pub accuracy: Option<u8>,
    /// Base PP, before PP Ups.
    pub pp: u8,
    /// Priority bracket.
    pub priority: i8,
    /// Who it may target.
    pub target: MoveTarget,
    /// Showdown's `flags`.
    pub flags: MoveFlags,
    /// Critical-hit stage; 1 is normal.
    pub crit_ratio: u8,
    /// Number of hits, `(min, max)`.
    pub multihit: Option<(u8, u8)>,
    /// HP the user drains, as a fraction of the damage dealt.
    pub drain: Option<Fraction>,
    /// Recoil, as a fraction of the damage dealt.
    pub recoil: Option<Fraction>,
    /// Healing as a fraction of the user's max HP.
    pub heal: Option<Fraction>,
    /// Status inflicted on the target (Will-O-Wisp, ...).
    pub status: Option<StatusId>,
    /// Volatile condition applied to the target (Showdown condition ID).
    pub volatile_status: Option<&'static str>,
    /// Stat changes for the target (Growl) or, for self-targeting moves, the user.
    pub boosts: Option<StatBoosts>,
    /// Stat changes the user always gets (Close Combat, Make It Rain).
    pub self_boosts: Option<StatBoosts>,
    /// Volatile condition the user gets (e.g. `"mustrecharge"`).
    pub self_volatile: Option<&'static str>,
    /// Chance-based effects (Showdown's `secondaries`).
    pub secondaries: &'static [Secondary],
    /// How it switches the user out, if it does.
    pub self_switch: Option<SelfSwitch>,
    /// Forces the target out (Roar, Dragon Tail).
    pub force_switch: bool,
    /// A one-hit KO move (Fissure, Sheer Cold).
    pub ohko: bool,
    /// The user faints (Explosion).
    pub self_destruct: bool,
    /// The side condition it starts (Showdown condition ID).
    pub side_condition: Option<&'static str>,
    /// The slot condition it starts (Wish, Healing Wish).
    pub slot_condition: Option<&'static str>,
    /// The weather it sets.
    pub weather: Option<&'static str>,
    /// The terrain it sets.
    pub terrain: Option<&'static str>,
    /// The pseudo-weather it starts (Trick Room, Gravity).
    pub pseudo_weather: Option<&'static str>,
    /// Showdown's `isNonstandard` (`None`: standard in this dex).
    pub nonstandard: Option<Nonstandard>,
    /// Showdown fields and callbacks of this move that the schema does not
    /// represent (e.g. `"onHit"`, `"basePowerCallback"`,
    /// `"overrideDefensiveStat"`). Empty means the row fully describes the
    /// move.
    pub unmodelled: &'static [&'static str],
}

impl MoveData {
    /// Whether the move is fully described by its data: no callbacks, no
    /// unrepresented fields, no custom secondaries.
    pub fn is_data_only(&self) -> bool {
        self.unmodelled.is_empty() && self.secondaries.iter().all(|s| s.unmodelled.is_empty())
    }
}

/// Showdown ability flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AbilityFlags(pub u8);

impl AbilityFlags {
    /// Ignored by Mold Breaker and similar.
    pub const BREAKABLE: AbilityFlags = AbilityFlags(1 << 0);
    /// Can't be suppressed or changed (Gastro Acid, Worry Seed, ...).
    pub const CANT_SUPPRESS: AbilityFlags = AbilityFlags(1 << 1);
    /// Role Play can't copy it.
    pub const FAIL_ROLE_PLAY: AbilityFlags = AbilityFlags(1 << 2);
    /// Skill Swap can't swap it.
    pub const FAIL_SKILL_SWAP: AbilityFlags = AbilityFlags(1 << 3);
    /// Entrainment can't give it.
    pub const NO_ENTRAIN: AbilityFlags = AbilityFlags(1 << 4);
    /// Receiver and Power of Alchemy can't copy it.
    pub const NO_RECEIVER: AbilityFlags = AbilityFlags(1 << 5);
    /// Trace can't copy it.
    pub const NO_TRACE: AbilityFlags = AbilityFlags(1 << 6);
    /// Doesn't work on a transformed Pokémon.
    pub const NO_TRANSFORM: AbilityFlags = AbilityFlags(1 << 7);

    /// Every flag with its Showdown name.
    pub const ALL: &'static [(&'static str, AbilityFlags)] = &[
        ("breakable", AbilityFlags::BREAKABLE),
        ("cantsuppress", AbilityFlags::CANT_SUPPRESS),
        ("failroleplay", AbilityFlags::FAIL_ROLE_PLAY),
        ("failskillswap", AbilityFlags::FAIL_SKILL_SWAP),
        ("noentrain", AbilityFlags::NO_ENTRAIN),
        ("noreceiver", AbilityFlags::NO_RECEIVER),
        ("notrace", AbilityFlags::NO_TRACE),
        ("notransform", AbilityFlags::NO_TRANSFORM),
    ];

    /// Whether every flag of `other` is set.
    #[inline]
    pub const fn contains(self, other: AbilityFlags) -> bool {
        self.0 & other.0 == other.0
    }
}

/// A Showdown event that abilities and items handle: the `<Event>` of an
/// `on<Prefix><Event>` callback (`onBasePower`, `onSourceModifyDamage`,
/// `onAnySwitchIn`, ...). The generator fails on an event missing here, so
/// the list always covers every ability and item.
///
/// Values are stable indices below 128 (see [`EventSet`]). Each variant is
/// the Showdown event of its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum Event {
    Accuracy,
    AfterBoost,
    AfterEachBoost,
    AfterFaint,
    AfterMega,
    AfterMove,
    AfterMoveSecondary,
    AfterMoveSecondarySelf,
    AfterSetStatus,
    AfterSubDamage,
    AfterTerastallization,
    AfterUseItem,
    Attract,
    BasePower,
    BeforeMove,
    BeforeSwitchIn,
    ChangeBoost,
    ChargeMove,
    CheckShow,
    CriticalHit,
    Damage,
    DamagingHit,
    DeductPP,
    DisableMove,
    DragOut,
    Eat,
    EatItem,
    Effectiveness,
    EmergencyExit,
    End,
    Faint,
    Flinch,
    FractionalPriority,
    Hit,
    HitProtect,
    Immunity,
    Invulnerability,
    MaybeTrapPokemon,
    ModifyAccuracy,
    ModifyAtk,
    ModifyBoost,
    ModifyCritRatio,
    ModifyDamage,
    ModifyDef,
    ModifyMove,
    ModifyPriority,
    ModifySTAB,
    ModifySecondaries,
    ModifySpA,
    ModifySpD,
    ModifySpe,
    ModifyType,
    ModifyWeight,
    NegateImmunity,
    PrepareHit,
    PseudoWeatherChange,
    RedirectTarget,
    Residual,
    SetAbility,
    SetStatus,
    SetWeather,
    SideConditionStart,
    Start,
    SwitchIn,
    SwitchOut,
    TakeItem,
    TerrainChange,
    TrapPokemon,
    TryAddVolatile,
    TryBoost,
    TryEatItem,
    TryHeal,
    TryHit,
    TryHitSide,
    TryMove,
    TryPrimaryHit,
    Update,
    Use,
    UseItem,
    Weather,
    WeatherChange,
    WeatherModifyDamage,
}

impl Event {
    /// Every event, by value.
    pub const ALL: [Event; 82] = [
        Event::Accuracy,
        Event::AfterBoost,
        Event::AfterEachBoost,
        Event::AfterFaint,
        Event::AfterMega,
        Event::AfterMove,
        Event::AfterMoveSecondary,
        Event::AfterMoveSecondarySelf,
        Event::AfterSetStatus,
        Event::AfterSubDamage,
        Event::AfterTerastallization,
        Event::AfterUseItem,
        Event::Attract,
        Event::BasePower,
        Event::BeforeMove,
        Event::BeforeSwitchIn,
        Event::ChangeBoost,
        Event::ChargeMove,
        Event::CheckShow,
        Event::CriticalHit,
        Event::Damage,
        Event::DamagingHit,
        Event::DeductPP,
        Event::DisableMove,
        Event::DragOut,
        Event::Eat,
        Event::EatItem,
        Event::Effectiveness,
        Event::EmergencyExit,
        Event::End,
        Event::Faint,
        Event::Flinch,
        Event::FractionalPriority,
        Event::Hit,
        Event::HitProtect,
        Event::Immunity,
        Event::Invulnerability,
        Event::MaybeTrapPokemon,
        Event::ModifyAccuracy,
        Event::ModifyAtk,
        Event::ModifyBoost,
        Event::ModifyCritRatio,
        Event::ModifyDamage,
        Event::ModifyDef,
        Event::ModifyMove,
        Event::ModifyPriority,
        Event::ModifySTAB,
        Event::ModifySecondaries,
        Event::ModifySpA,
        Event::ModifySpD,
        Event::ModifySpe,
        Event::ModifyType,
        Event::ModifyWeight,
        Event::NegateImmunity,
        Event::PrepareHit,
        Event::PseudoWeatherChange,
        Event::RedirectTarget,
        Event::Residual,
        Event::SetAbility,
        Event::SetStatus,
        Event::SetWeather,
        Event::SideConditionStart,
        Event::Start,
        Event::SwitchIn,
        Event::SwitchOut,
        Event::TakeItem,
        Event::TerrainChange,
        Event::TrapPokemon,
        Event::TryAddVolatile,
        Event::TryBoost,
        Event::TryEatItem,
        Event::TryHeal,
        Event::TryHit,
        Event::TryHitSide,
        Event::TryMove,
        Event::TryPrimaryHit,
        Event::Update,
        Event::Use,
        Event::UseItem,
        Event::Weather,
        Event::WeatherChange,
        Event::WeatherModifyDamage,
    ];

    /// This event's bit in an [`EventSet`].
    #[inline]
    pub const fn bit(self) -> u128 {
        1 << self as u8
    }
}

/// How a handler is collected (the `<Prefix>` of `on<Prefix><Event>`):
/// Showdown's `findEventHandlers` looks for `on<Event>` on the event's
/// target, `onAlly<Event>` / `onAny<Event>` on its side, `onFoe<Event>` /
/// `onAny<Event>` on the other side and `onSource<Event>` on the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum EventPrefix {
    /// `on<Event>`: on the event's target.
    On,
    /// `onSource<Event>`: on the event's source.
    Source,
    /// `onAlly<Event>`: on the target's allies (itself included).
    Ally,
    /// `onFoe<Event>`: on the target's foes.
    Foe,
    /// `onAny<Event>`: on any Pokémon.
    Any,
}

/// A set of [`Event`]s (bit `event as u8`): the events an ability or item
/// has at least one handler for, so the engine skips it in one test.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EventSet(pub u128);

impl EventSet {
    /// No event.
    pub const EMPTY: EventSet = EventSet(0);

    /// The set of `events` (usable in `const` tables).
    pub const fn of(events: &[Event]) -> EventSet {
        let mut bits = 0;
        let mut i = 0;
        while i < events.len() {
            bits |= events[i].bit();
            i += 1;
        }
        EventSet(bits)
    }

    /// Whether `event` is in the set.
    #[inline]
    pub const fn contains(self, event: Event) -> bool {
        self.0 & event.bit() != 0
    }
}

/// One `on<Prefix><Event>` callback of an ability or item, with the
/// ordering data Showdown's `resolvePriority` reads for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EventHandler {
    /// The event.
    pub event: Event,
    /// How it is collected.
    pub prefix: EventPrefix,
    /// `on<Callback>Order`; `None` is Showdown's `false` (after every
    /// ordered handler).
    pub order: Option<u8>,
    /// `on<Callback>Priority` (higher runs first).
    pub priority: i8,
    /// `on<Callback>SubOrder`; 0 means the effect type's default (7 for
    /// abilities, 8 for items).
    pub sub_order: u8,
    /// A `SwitchIn` handler that runs `onStart`: Showdown's `getCallback`
    /// falls back to `onStart` for an ability or item with neither
    /// `onSwitchIn` nor `onAnySwitchIn` (its ordering comes from
    /// `onSwitchInPriority`).
    pub via_start: bool,
}

impl EventHandler {
    /// A handler without ordering data.
    pub const fn new(event: Event, prefix: EventPrefix) -> EventHandler {
        EventHandler { event, prefix, order: None, priority: 0, sub_order: 0, via_start: false }
    }

    /// With `on<Callback>Order`.
    pub const fn order(self, order: u8) -> EventHandler {
        EventHandler { order: Some(order), ..self }
    }

    /// With `on<Callback>Priority`.
    pub const fn priority(self, priority: i8) -> EventHandler {
        EventHandler { priority, ..self }
    }

    /// With `on<Callback>SubOrder`.
    pub const fn sub_order(self, sub_order: u8) -> EventHandler {
        EventHandler { sub_order, ..self }
    }

    /// The `SwitchIn` handler Showdown runs `onStart` for.
    pub const fn via_start(self) -> EventHandler {
        EventHandler { via_start: true, ..self }
    }
}

/// The handler for `event` collected with `prefix`, if `handlers` has one.
#[inline]
pub fn find_handler(handlers: &[EventHandler], event: Event, prefix: EventPrefix) -> Option<&EventHandler> {
    handlers.iter().find(|h| h.event == event && h.prefix == prefix)
}

/// An ability (Showdown's `Ability`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbilityData {
    /// Showdown's name.
    pub name: &'static str,
    /// Showdown's `num`.
    pub num: i16,
    /// Showdown's `flags`.
    pub flags: AbilityFlags,
    /// Showdown's `isNonstandard` (`None`: standard in this dex).
    pub nonstandard: Option<Nonstandard>,
    /// Its event callbacks, in Showdown's key order.
    pub handlers: &'static [EventHandler],
    /// The events of `handlers`.
    pub events: EventSet,
    /// The events of its `Ally` / `Foe` / `Any` / `Source` handlers (it
    /// handles them on another Pokémon's event).
    pub events_prefixed: EventSet,
    /// Showdown's `suppressWeather` (Air Lock, Cloud Nine).
    pub suppress_weather: bool,
}

impl AbilityData {
    /// The handler for `event` collected with `prefix`, if any.
    #[inline]
    pub fn handler(&self, event: Event, prefix: EventPrefix) -> Option<&EventHandler> {
        if !self.events.contains(event) {
            return None;
        }
        find_handler(self.handlers, event, prefix)
    }
}

/// An item (Showdown's `Item`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemData {
    /// Showdown's name.
    pub name: &'static str,
    /// Showdown's `num`.
    pub num: i16,
    /// Fling base power, if the item can be flung.
    pub fling_power: Option<u8>,
    /// Showdown's `isBerry`.
    pub is_berry: bool,
    /// Showdown's `isGem`.
    pub is_gem: bool,
    /// Showdown's `isChoice` (Choice Band, Specs, Scarf).
    pub is_choice: bool,
    /// For a Mega Stone, the `(species, mega forme)` pairs it works for
    /// (Magearnite has two).
    pub mega: &'static [(SpeciesId, SpeciesId)],
    /// Showdown's `isNonstandard` (`None`: standard in this dex).
    pub nonstandard: Option<Nonstandard>,
    /// Its event callbacks, in Showdown's key order.
    pub handlers: &'static [EventHandler],
    /// The events of `handlers`.
    pub events: EventSet,
    /// The events of its `Ally` / `Foe` / `Any` / `Source` handlers (it
    /// handles them on another Pokémon's event).
    pub events_prefixed: EventSet,
    /// Showdown's `ignoreKlutz` (the Power items, Ability Shield).
    pub ignore_klutz: bool,
    /// Showdown's `isPrimalOrb` (never ignored).
    pub is_primal_orb: bool,
    /// A Poké Ball (no battle effect).
    pub is_pokeball: bool,
    /// Showdown's `boosts`: what `useItem` applies (Weakness Policy,
    /// the terrain seeds, ...).
    pub boosts: Option<StatBoosts>,
    /// Showdown's `onPlate`: the type a Plate gives Judgment and Arceus.
    pub plate: Option<Type>,
    /// A Z-Crystal (Showdown's `zMove`).
    pub is_z_crystal: bool,
}

impl ItemData {
    /// The handler for `event` collected with `prefix`, if any.
    #[inline]
    pub fn handler(&self, event: Event, prefix: EventPrefix) -> Option<&EventHandler> {
        if !self.events.contains(event) {
            return None;
        }
        find_handler(self.handlers, event, prefix)
    }
}

/// A complete, immutable set of data tables for one mechanics family.
///
/// A `Dex` is always used as `&'static Dex`: static tables are free to share
/// across threads and battles, and a dex loaded at runtime (e.g. a mod) can be
/// leaked once with `Box::leak`. Dexes generated together (Gen 9 and
/// Champions) share one ID space, so ID constants work with either.
#[derive(Debug)]
pub struct Dex {
    /// `"gen9"` or `"champions"`.
    pub name: &'static str,
    /// The species, by [`SpeciesId`].
    pub species: &'static [SpeciesData],
    /// The moves, by [`MoveId`].
    pub moves: &'static [MoveData],
    /// The abilities, by [`AbilityId`].
    pub abilities: &'static [AbilityData],
    /// The items, by [`ItemId`].
    pub items: &'static [ItemData],
    /// The tables' rows by name, for the name lookups.
    pub names: &'static NameIndex,
}

/// Each table's row indices sorted by their names' Showdown ID
/// ([`to_id`](crate::to_id)), which the [`Dex`]'s name lookups
/// binary-search. Generated with the tables: IDs are unique, never empty and
/// at most [`MAX_ID_LEN`](crate::MAX_ID_LEN) long.
#[derive(Debug)]
pub struct NameIndex {
    /// The species' rows.
    pub species: &'static [u16],
    /// The moves' rows.
    pub moves: &'static [u16],
    /// The abilities' rows.
    pub abilities: &'static [u16],
    /// The items' rows.
    pub items: &'static [u16],
}

impl Dex {
    /// The species `id` (panics on an ID outside this dex).
    #[inline]
    pub fn species(&self, id: SpeciesId) -> &SpeciesData {
        &self.species[id.index()]
    }

    /// The move `id` (panics on an ID outside this dex).
    #[inline]
    pub fn move_(&self, id: MoveId) -> &MoveData {
        &self.moves[id.index()]
    }

    /// The ability `id` (panics on an ID outside this dex).
    #[inline]
    pub fn ability(&self, id: AbilityId) -> &AbilityData {
        &self.abilities[id.index()]
    }

    /// The item `id` (panics on an ID outside this dex).
    #[inline]
    pub fn item(&self, id: ItemId) -> &ItemData {
        &self.items[id.index()]
    }

    // Name lookups normalise the name once (giving up past the longest ID)
    // and binary-search the generated `NameIndex`: bounded work for any
    // input, however long.

    /// The species whose name has the same Showdown ID as `name`.
    pub fn species_by_name(&self, name: &str) -> Option<SpeciesId> {
        find(self.names.species, |i| self.species[i].name, name).map(SpeciesId)
    }

    /// The move whose name has the same Showdown ID as `name`.
    pub fn move_by_name(&self, name: &str) -> Option<MoveId> {
        find(self.names.moves, |i| self.moves[i].name, name).map(MoveId)
    }

    /// The ability whose name has the same Showdown ID as `name`.
    pub fn ability_by_name(&self, name: &str) -> Option<AbilityId> {
        find(self.names.abilities, |i| self.abilities[i].name, name).map(AbilityId)
    }

    /// The item whose name has the same Showdown ID as `name`.
    pub fn item_by_name(&self, name: &str) -> Option<ItemId> {
        find(self.names.items, |i| self.items[i].name, name).map(ItemId)
    }
}

fn find<'a>(order: &[u16], name_of: impl Fn(usize) -> &'a str, name: &str) -> Option<u16> {
    let id = crate::NameId::new(name)?;
    order.binary_search_by(|&i| id.cmp_name(name_of(i as usize)).reverse()).ok().map(|k| order[k])
}
