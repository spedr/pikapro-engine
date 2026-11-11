//! Battle state: plain data, no behaviour beyond small accessors.
//!
//! Everything here is `Copy`/`Clone` and heap-free, so cloning a
//! [`BattleState`] is a cheap snapshot (a few KB memcpy). That is what tree
//! search, rollouts and replay buffers rely on.
//!
//! Hidden information is *not* separated here: the state is omniscient. A
//! per-player view is derived at the protocol/observation boundary.

use pikapro_data::{AbilityId, ItemId, MoveId, MoveTarget, Nature, SpeciesId, Stat, Stats, Type, TypeSet};

use crate::format::Gimmicks;
use crate::util::StackVec;

/// Most Pokémon a side's team holds.
pub const MAX_TEAM: usize = 6;
/// Most active slots per side (2 in doubles, 1 in singles).
pub const MAX_ACTIVE: usize = 2;
/// Most move slots a Pokémon has.
pub const MAX_MOVES: usize = 4;

/// One of the two players.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum SideId {
    /// Player 1 (Showdown's `p1`).
    #[default]
    P1 = 0,
    /// Player 2 (Showdown's `p2`).
    P2 = 1,
}

impl SideId {
    /// Both sides, `P1` first.
    pub const BOTH: [SideId; 2] = [SideId::P1, SideId::P2];

    /// The side as an index into [`BattleState::sides`]: 0 or 1.
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The opposing side.
    #[inline]
    pub const fn foe(self) -> SideId {
        match self {
            SideId::P1 => SideId::P2,
            SideId::P2 => SideId::P1,
        }
    }
}

/// An active position on the field: a side and a slot (`0` = left/"a").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Position {
    /// The side the slot is on.
    pub side: SideId,
    /// The active slot, `0..MAX_ACTIVE`.
    pub slot: u8,
}

impl Position {
    /// The position of `slot` on `side`.
    pub const fn new(side: SideId, slot: u8) -> Position {
        Position { side, slot }
    }
}

/// Non-volatile status condition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Status {
    /// No status.
    #[default]
    None,
    /// Burned.
    Burn,
    /// Frozen; `turns` is the Champions mod's thaw countdown (unused, 0,
    /// under Scarlet/Violet mechanics).
    Freeze {
        /// Champions only: turns until a certain thaw (starts at 3, counted
        /// down before each move); 0 under Scarlet/Violet mechanics.
        turns: u8,
    },
    /// Paralyzed.
    Paralysis,
    /// Poisoned (regular poison).
    Poison,
    /// Badly poisoned; the counter drives n/16 residual damage.
    Toxic {
        /// 0 on infliction and on each switch-in; each end-of-turn residual
        /// raises it (at most 15), then deals `counter/16` of max HP.
        counter: u8,
    },
    /// Asleep; turns remaining.
    Sleep {
        /// Sleep turns left: drawn when it falls asleep, counted down before
        /// each move (twice with Early Bird); it wakes at 0.
        turns: u8,
    },
}

/// Stat stages, each in `-6..=6`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Boosts {
    /// Attack stage.
    pub atk: i8,
    /// Defense stage.
    pub def: i8,
    /// Special Attack stage.
    pub spa: i8,
    /// Special Defense stage.
    pub spd: i8,
    /// Speed stage.
    pub spe: i8,
    /// Accuracy stage.
    pub accuracy: i8,
    /// Evasion stage.
    pub evasion: i8,
}

impl Boosts {
    /// The stage of `id`.
    #[inline]
    pub fn get(&self, id: pikapro_data::BoostId) -> i8 {
        match id {
            pikapro_data::BoostId::Atk => self.atk,
            pikapro_data::BoostId::Def => self.def,
            pikapro_data::BoostId::SpA => self.spa,
            pikapro_data::BoostId::SpD => self.spd,
            pikapro_data::BoostId::Spe => self.spe,
            pikapro_data::BoostId::Accuracy => self.accuracy,
            pikapro_data::BoostId::Evasion => self.evasion,
        }
    }

    /// Stage for a battle stat. HP has no stage.
    #[inline]
    pub fn stat(&self, stat: Stat) -> i8 {
        match stat {
            Stat::Hp => 0,
            Stat::Atk => self.atk,
            Stat::Def => self.def,
            Stat::SpA => self.spa,
            Stat::SpD => self.spd,
            Stat::Spe => self.spe,
        }
    }
}

/// Volatile conditions: cleared on switch-out. A bit set, plus the order the
/// volatiles were added in (the key order of Showdown's `pokemon.volatiles`),
/// which decides how their handlers are collected and so how speed ties
/// between them are shuffled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Volatiles {
    bits: u128,
    /// Bit indices of the present volatiles, oldest first.
    order: StackVec<u8, ORDER_CAPACITY>,
}

/// Room in `Volatiles::order` for every defined volatile at once. Showdown
/// has no limit, and each flag is in the list at most once, so a list as
/// long as there are flags can't fill (the assert below keeps it so when a
/// flag is added). 79 fills the padding the `u128` already costs, so
/// `Volatiles` is 96 bytes.
const ORDER_CAPACITY: usize = 79;

const _: () = assert!(Volatiles::ALL.count_ones() as usize <= ORDER_CAPACITY);

/// Volatile flags, grouped in bit ranges by the code that handles them
/// (see the note after the last range).
impl Volatiles {
    /// Protect or Detect this turn (duration 1).
    pub const PROTECT: u128 = 1 << 0;
    /// Flinched: can't move this turn (duration 1).
    pub const FLINCH: u128 = 1 << 1;
    /// Counter: [`VolatileCounters::confusion`].
    pub const CONFUSION: u128 = 1 << 2;
    /// Counter: [`VolatileCounters::must_recharge`].
    pub const MUST_RECHARGE: u128 = 1 << 3;
    /// Counter: [`VolatileCounters::taunt`].
    pub const TAUNT: u128 = 1 << 4;
    /// Counters: [`VolatileCounters::encore`] and `encore_move`.
    pub const ENCORE: u128 = 1 << 5;
    /// Counters: [`VolatileCounters::disable`] and `disable_move`.
    pub const DISABLE: u128 = 1 << 6;
    /// Showdown's `stall` (consecutive protection). Counter:
    /// [`PokemonState::stall_turns`].
    pub const STALL: u128 = 1 << 7;

    // Multi-turn moves (`resolve/multiturn.rs`): bits 8..=15.
    /// Showdown's `twoturnmove`: charging a two-turn move. Counters:
    /// [`VolatileCounters::two_turn`], `charge_move`.
    pub const TWO_TURN_MOVE: u128 = 1 << 8;
    /// The charging move's own volatile (`fly`, `dig`, `solarbeam`, ...;
    /// the move is `charge_move`). Counters: `charging` (its duration, 0 for
    /// moves without a condition), `charge_target_loc`.
    pub const CHARGING: u128 = 1 << 9;
    /// Showdown's `lockedmove` (Outrage, Petal Dance, Thrash, Raging Fury).
    /// Counters: `locked_move`, `locked_true_duration`, `locked_move_id`.
    pub const LOCKED_MOVE: u128 = 1 << 10;
    /// Uproar. Counter: `uproar`.
    pub const UPROAR: u128 = 1 << 11;
    /// Rollout. Counters: `rollout`, `rollout_hits`.
    pub const ROLLOUT: u128 = 1 << 12;
    /// A priority-charge volatile (`focuspunch`, `beakblast`,
    /// `chillyreception`; which one is `priority_charge`). Duration 1.
    pub const PRIORITY_CHARGE: u128 = 1 << 13;
    /// Counter's `counter` (duration 1). Counters:
    /// [`VolatileCounters::counter_slot`] and `counter_damage`.
    pub const COUNTER: u128 = 1 << 14;
    /// Mirror Coat's `mirrorcoat` (duration 1). Counters:
    /// [`VolatileCounters::mirror_coat_slot`] and `mirror_coat_damage`. A
    /// Pokémon can hold both: Instruct runs only the `beforeTurnMove` of
    /// the target's last move, so one about to use Mirror Coat whose last
    /// move was Counter (or the reverse) gets both, in that order.
    pub const MIRROR_COAT: u128 = 1 << 15;
    /// Volatiles that lock the Pokémon into a move (Showdown's `LockMove`).
    pub const LOCKING: u128 = Self::TWO_TURN_MOVE | Self::LOCKED_MOVE | Self::UPROAR | Self::ROLLOUT;

    // Substitute, trapping and residual volatiles (`resolve/conditions.rs`):
    // bits 16..=35.
    /// Counter: [`VolatileCounters::substitute_hp_halves`].
    pub const SUBSTITUTE: u128 = 1 << 16;
    /// Bind, Wrap, Fire Spin, ... Counters: [`VolatileCounters::partially_trapped`]
    /// and `partially_trapped_move` / `partially_trapped_by`.
    pub const PARTIALLY_TRAPPED: u128 = 1 << 17;
    /// Block, Mean Look, Spirit Shackle, Jaw Lock. Counter:
    /// [`VolatileCounters::trapped_by`] (Showdown's linked `trapper`).
    pub const TRAPPED: u128 = 1 << 18;
    /// No Retreat: the Pokémon can't switch out.
    pub const NO_RETREAT: u128 = 1 << 19;
    /// Counter: [`VolatileCounters::leech_seed_slot`].
    pub const LEECH_SEED: u128 = 1 << 20;
    /// Counter: [`VolatileCounters::yawn`].
    pub const YAWN: u128 = 1 << 21;
    /// Ghost-type Curse: loses 1/4 of max HP at the end of each turn.
    pub const CURSE: u128 = 1 << 22;
    /// Counter: [`VolatileCounters::perish_song`].
    pub const PERISH_SONG: u128 = 1 << 23;
    /// Destiny Bond: a foe whose move knocks this Pokémon out faints too.
    pub const DESTINY_BOND: u128 = 1 << 24;
    /// Counter: [`VolatileCounters::attract_by`].
    pub const ATTRACT: u128 = 1 << 25;
    /// Torment: can't use the same move twice in a row.
    pub const TORMENT: u128 = 1 << 26;
    /// Salt Cure: loses 1/8 of max HP each turn (1/4 for Water and Steel types).
    pub const SALT_CURE: u128 = 1 << 27;
    /// Counters: [`VolatileCounters::syrup_bomb`] and `syrup_bomb_by`.
    pub const SYRUP_BOMB: u128 = 1 << 28;
    /// Counter: [`VolatileCounters::throat_chop`].
    pub const THROAT_CHOP: u128 = 1 << 29;
    /// Counter: [`VolatileCounters::heal_block`].
    pub const HEAL_BLOCK: u128 = 1 << 30;
    /// Imprison: foes can't use the moves this Pokémon knows.
    pub const IMPRISON: u128 = 1 << 31;
    /// Glaive Rush's aftermath: moves against it can't miss and deal double
    /// damage until it moves again.
    pub const GLAIVE_RUSH: u128 = 1 << 32;
    /// Endure: survives this turn's hits with at least 1 HP (duration 1).
    pub const ENDURE: u128 = 1 << 33;
    /// Smack Down or Thousand Arrows: grounded until it switches out.
    pub const SMACK_DOWN: u128 = 1 << 34;
    /// Counter: [`VolatileCounters::magnet_rise`].
    pub const MAGNET_RISE: u128 = 1 << 35;

    // Stat, HP and type manipulation (`resolve/manipulation.rs`): bits
    // 36..=49.
    /// Aqua Ring: heals 1/16 at the end of each turn.
    pub const AQUA_RING: u128 = 1 << 36;
    /// Ingrain: heals 1/16 at the end of each turn, traps, grounds.
    pub const INGRAIN: u128 = 1 << 37;
    /// Charge: the next Electric move has double power.
    pub const CHARGE: u128 = 1 << 38;
    /// Focus Energy: crit ratio +2.
    pub const FOCUS_ENERGY: u128 = 1 << 39;
    /// Dragon Cheer: crit ratio +1 (+2 for Dragon types). Data:
    /// [`VolatileCounters::dragon_cheer_dragon`].
    pub const DRAGON_CHEER: u128 = 1 << 40;
    /// Roost: no Flying type until the end of the turn.
    pub const ROOST: u128 = 1 << 41;
    /// Tar Shot: Fire moves are one step more effective.
    pub const TAR_SHOT: u128 = 1 << 42;
    /// Minimize: `minimize` moves always hit and deal double damage.
    pub const MINIMIZE: u128 = 1 << 43;
    /// Defense Curl (Rollout and Ice Ball double their power).
    pub const DEFENSE_CURL: u128 = 1 << 44;
    /// Stockpile. Data: [`VolatileCounters::stockpile_layers`] and the
    /// stages it added.
    pub const STOCKPILE: u128 = 1 << 45;
    /// Power Trick: Attack and Defense swapped.
    pub const POWER_TRICK: u128 = 1 << 46;
    /// Power Shift: Attack and Defense swapped (Showdown's Gen 9 version).
    pub const POWER_SHIFT: u128 = 1 << 47;
    /// Electrify: this turn's move is Electric.
    pub const ELECTRIFY: u128 = 1 << 48;
    /// Lock-On: the next move against [`VolatileCounters::lock_on`] hits.
    pub const LOCK_ON: u128 = 1 << 49;

    // Action order, targeting and move calling: bits 50..=55.
    /// Helping Hand (duration 1). Counter:
    /// [`VolatileCounters::helping_hand`].
    pub const HELPING_HAND: u128 = 1 << 50;
    /// Follow Me (duration 1). Counter: [`VolatileCounters::redirect_order`].
    pub const FOLLOW_ME: u128 = 1 << 51;
    /// Rage Powder (duration 1). Counter:
    /// [`VolatileCounters::redirect_order`].
    pub const RAGE_POWDER: u128 = 1 << 52;
    /// Ally Switch's stall-like counter (duration 2). Counters:
    /// [`VolatileCounters::ally_switch`] and `ally_switch_counter`.
    pub const ALLY_SWITCH: u128 = 1 << 53;
    /// Transformed (Showdown's `transformed` flag, not a volatile there).
    /// Data: [`VolatileCounters::transform`].
    pub const TRANSFORMED: u128 = 1 << 54;
    /// A move slot replaced by Mimic until switching out. Data:
    /// [`VolatileCounters::mimic_slot`] and `mimic_original`.
    pub const MIMIC: u128 = 1 << 55;

    // Damage formula: bits 56..=57.
    /// Fury Cutter's growing power. Counters:
    /// [`VolatileCounters::fury_cutter`] and `fury_cutter_duration`.
    pub const FURY_CUTTER: u128 = 1 << 56;

    // Protection variants (`resolve/protect.rs`): bits 58..=63.
    /// Obstruct: blocks damaging moves; contact lowers the attacker's Defense by 2.
    pub const OBSTRUCT: u128 = 1 << 58;
    /// Burning Bulwark: blocks damaging moves; contact burns the attacker.
    pub const BURNING_BULWARK: u128 = 1 << 59;
    /// Silk Trap: blocks damaging moves; contact lowers the attacker's Speed by 1.
    pub const SILK_TRAP: u128 = 1 << 60;
    /// Baneful Bunker: blocks moves; contact poisons the attacker.
    pub const BANEFUL_BUNKER: u128 = 1 << 61;
    /// King's Shield: blocks damaging moves; contact lowers the attacker's
    /// Attack by 1.
    pub const KINGS_SHIELD: u128 = 1 << 62;
    /// Spiky Shield: blocks moves; contact costs the attacker 1/8 of its max HP.
    pub const SPIKY_SHIELD: u128 = 1 << 63;

    // Abilities and items: bits 64..=67.
    /// Showdown's `choicelock` (a Choice item locked the Pokémon into
    /// [`VolatileCounters::choice_lock`]).
    pub const CHOICE_LOCK: u128 = 1 << 64;
    /// Gastro Acid's volatile: the ability is suppressed
    /// (`abilities::ignoring_ability`).
    pub const GASTRO_ACID: u128 = 1 << 65;
    /// Commander's `commanding` (Tatsugiri inside Dondozo): its ability is
    /// not suppressed by Neutralizing Gas.
    pub const COMMANDING: u128 = 1 << 66;

    // Switch-in, field, forme and ability-suppressing abilities
    // (`resolve/abilities/switchin.rs`): bits 68..=79.
    /// Zen Mode's `zenmode` (Darmanitan in its Zen forme).
    pub const ZEN_MODE: u128 = 1 << 68;
    /// Protosynthesis's volatile. Data: [`VolatileCounters::paradox_stat`]
    /// and `paradox_booster`.
    pub const PROTOSYNTHESIS: u128 = 1 << 69;
    /// Quark Drive's volatile (same data as Protosynthesis's).
    pub const QUARK_DRIVE: u128 = 1 << 70;
    /// Truant's `truant`: the next move is loafed.
    pub const TRUANT: u128 = 1 << 71;
    /// Commander's `commanded` (Dondozo with Tatsugiri inside). Data:
    /// [`VolatileCounters::commanded_by`].
    pub const COMMANDED: u128 = 1 << 72;

    // Damage, stat and type modifying abilities
    // (`resolve/abilities/damage.rs`): bits 80..=87, unused.

    // Immunities, contact and reactive effects, status and boost protection
    // (`resolve/abilities/reactive.rs`): bits 88..=99.
    /// Flash Fire's `flashfire` volatile: the holder's Fire moves get 1.5x
    /// Attack and Special Attack while it keeps the ability.
    pub const FLASH_FIRE: u128 = 1 << 88;

    // Other abilities (`resolve/abilities/misc.rs`): bits 100..=107,
    // unused.

    // Held items that stay held (`resolve/items/held.rs`): bits 108..=111.
    /// The Metronome item's volatile: consecutive uses of one move boost
    /// it. Data: [`VolatileCounters::metronome_move`] and
    /// `metronome_count`.
    pub const METRONOME: u128 = 1 << 108;

    // Berries, consumed items, item and ability transfer
    // (`resolve/items/consumed.rs`): bits 112..=123.
    /// Unburden's volatile: Speed doubles while the Pokémon holds no item.
    pub const UNBURDEN: u128 = 1 << 112;
    /// Micle Berry's volatile: the next move's accuracy is 1.2x. Counter:
    /// [`VolatileCounters::micle_berry`].
    pub const MICLE_BERRY: u128 = 1 << 113;
    /// Fling's volatile: the flung item is gone at the next `Update`.
    pub const FLING: u128 = 1 << 114;
    /// The `gem` volatile: a Gem boosts this move's power. Counter:
    /// [`VolatileCounters::gem`].
    pub const GEM: u128 = 1 << 115;

    // Champions (`resolve/conditions.rs`): bit 124.
    /// Octolock's `octolock`: traps, and lowers Defense and Special Defense
    /// at the end of each turn. Counter: [`VolatileCounters::octolock_by`].
    pub const OCTOLOCK: u128 = 1 << 124;

    // Bits 125..=127 are free. A section's handlers are found by its bit
    // range (`resolve::events::volatile_owner`): a new range goes there too.

    /// Every flag above (a test checks it against this file).
    const ALL: u128 = Self::PROTECT
        | Self::FLINCH
        | Self::CONFUSION
        | Self::MUST_RECHARGE
        | Self::TAUNT
        | Self::ENCORE
        | Self::DISABLE
        | Self::STALL
        | Self::TWO_TURN_MOVE
        | Self::CHARGING
        | Self::LOCKED_MOVE
        | Self::UPROAR
        | Self::ROLLOUT
        | Self::PRIORITY_CHARGE
        | Self::COUNTER
        | Self::MIRROR_COAT
        | Self::SUBSTITUTE
        | Self::PARTIALLY_TRAPPED
        | Self::TRAPPED
        | Self::NO_RETREAT
        | Self::LEECH_SEED
        | Self::YAWN
        | Self::CURSE
        | Self::PERISH_SONG
        | Self::DESTINY_BOND
        | Self::ATTRACT
        | Self::TORMENT
        | Self::SALT_CURE
        | Self::SYRUP_BOMB
        | Self::THROAT_CHOP
        | Self::HEAL_BLOCK
        | Self::IMPRISON
        | Self::GLAIVE_RUSH
        | Self::ENDURE
        | Self::SMACK_DOWN
        | Self::MAGNET_RISE
        | Self::AQUA_RING
        | Self::INGRAIN
        | Self::CHARGE
        | Self::FOCUS_ENERGY
        | Self::DRAGON_CHEER
        | Self::ROOST
        | Self::TAR_SHOT
        | Self::MINIMIZE
        | Self::DEFENSE_CURL
        | Self::STOCKPILE
        | Self::POWER_TRICK
        | Self::POWER_SHIFT
        | Self::ELECTRIFY
        | Self::LOCK_ON
        | Self::HELPING_HAND
        | Self::FOLLOW_ME
        | Self::RAGE_POWDER
        | Self::ALLY_SWITCH
        | Self::TRANSFORMED
        | Self::MIMIC
        | Self::FURY_CUTTER
        | Self::OBSTRUCT
        | Self::BURNING_BULWARK
        | Self::SILK_TRAP
        | Self::BANEFUL_BUNKER
        | Self::KINGS_SHIELD
        | Self::SPIKY_SHIELD
        | Self::CHOICE_LOCK
        | Self::GASTRO_ACID
        | Self::COMMANDING
        | Self::ZEN_MODE
        | Self::PROTOSYNTHESIS
        | Self::QUARK_DRIVE
        | Self::TRUANT
        | Self::COMMANDED
        | Self::FLASH_FIRE
        | Self::METRONOME
        | Self::UNBURDEN
        | Self::MICLE_BERRY
        | Self::FLING
        | Self::GEM
        | Self::OCTOLOCK;

    /// Whether any volatile in `mask` is present.
    #[inline]
    pub fn has(self, mask: u128) -> bool {
        self.bits & mask != 0
    }

    /// The present volatiles as a bit set.
    #[inline]
    pub fn bits(self) -> u128 {
        self.bits
    }

    /// Adds the volatiles in `mask` (usually a single flag). Each one not
    /// yet present goes to the end of the order, lowest bit first;
    /// re-adding a present one keeps its place, like Showdown's
    /// `onRestart`. A bit no flag uses is ignored, so the order list can't
    /// fill.
    #[inline]
    pub fn set(&mut self, mask: u128) {
        let mut new = mask & Self::ALL & !self.bits;
        self.bits |= new;
        while new != 0 {
            self.order.push(new.trailing_zeros() as u8);
            new &= new - 1;
        }
    }

    /// Adds one volatile: [`set`](Self::set) for the engine, which adds
    /// one defined flag at a time, in Showdown's `addVolatile` order. A
    /// mask of several would be ordered by bit instead, so a debug build
    /// refuses one.
    #[inline]
    pub(crate) fn add(&mut self, flag: u128) {
        debug_assert!(flag.is_power_of_two() && flag & Self::ALL != 0, "one defined volatile at a time");
        self.set(flag);
    }

    /// Removes every volatile in `mask`.
    pub fn clear(&mut self, mask: u128) {
        if self.bits & mask != 0 {
            self.bits &= !mask;
            self.order.retain(|&i| mask & (1 << i) == 0);
        }
    }

    /// The present volatiles (single flags), oldest first.
    pub fn in_order(&self) -> impl Iterator<Item = u128> + '_ {
        self.order.iter().map(|&i| 1 << i)
    }
}

/// Turn counters and data of the volatiles in [`Volatiles`] (Showdown's
/// per-volatile `effectState`). Meaningful only while the volatile's bit is
/// set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct VolatileCounters {
    /// Confusion's `time`: checks left, counted down before each move.
    pub confusion: u8,
    /// `mustrecharge`'s `duration`.
    pub must_recharge: u8,
    /// Taunt's `duration`.
    pub taunt: u8,
    /// Encore's `duration`.
    pub encore: u8,
    /// The move Encore locks the Pokémon into.
    pub encore_move: MoveId,
    /// Disable's `duration`.
    pub disable: u8,
    /// The disabled move.
    pub disable_move: MoveId,
    // Damage formula. Besides Fury Cutter's counters, these
    // are the per-Pokémon fields Showdown's `clearVolatile` resets, so they
    // live here and are reset with the rest on switch-out and faint.
    /// Fury Cutter's `multiplier` (1, 2 or 4).
    pub fury_cutter: u8,
    /// Fury Cutter's `duration`.
    pub fury_cutter_duration: u8,
    /// Showdown's `hurtThisTurn` is truthy: the Pokémon took damage this
    /// turn and survived. Also reset at the start of each turn after the
    /// first (`endTurn`).
    pub hurt_this_turn: bool,
    /// Showdown's `statsLoweredThisTurn`. Also reset at the start of each
    /// turn after the first.
    pub stats_lowered_this_turn: bool,
    /// Showdown's `attackedBy` entries of this turn with damage: bit
    /// `6 * side + id` is set when that Pokémon's move damaged this one.
    /// Cleared at the start of each turn.
    pub damaged_by: u16,
    /// The opposite of Showdown's `newlySwitched`: set at the start of each
    /// turn.
    pub settled: bool,
    /// Showdown's `moveThisTurnResult`; moves to `move_last_turn` at the
    /// start of each turn.
    pub move_this_turn: MoveResult,
    /// Showdown's `moveLastTurnResult`.
    pub move_last_turn: MoveResult,
    /// `timesAttacked` under the Champions mod, whose `clearVolatile`
    /// resets it (see [`MoveRecord::times_attacked`]).
    pub times_attacked: u8,
    // Multi-turn moves.
    /// `twoturnmove`'s `duration`.
    pub two_turn: u8,
    /// The two-turn move being charged (`twoturnmove`'s `move`, and the ID
    /// of the move's own volatile).
    pub charge_move: MoveId,
    /// The charging move's own volatile's `duration` (0: none).
    pub charging: u8,
    /// The charging move's `targetLoc`, which the locked-in turn uses.
    pub charge_target_loc: i8,
    /// `lockedmove`'s `duration`.
    pub locked_move: u8,
    /// `lockedmove`'s `trueDuration`.
    pub locked_true_duration: u8,
    /// The move `lockedmove` locks the Pokémon into.
    pub locked_move_id: MoveId,
    /// Uproar's `duration`.
    pub uproar: u8,
    /// Rollout's `duration`.
    pub rollout: u8,
    /// Rollout's `hitCount` (and `contactHitCount`, always equal for it).
    pub rollout_hits: u8,
    /// Which priority-charge volatile [`Volatiles::PRIORITY_CHARGE`] is.
    pub priority_charge: MoveId,
    /// Focus Punch's `lostFocus`.
    pub lost_focus: bool,
    /// [`Volatiles::COUNTER`]'s `slot`: the slot of the last foe that hit
    /// with a physical move.
    pub counter_slot: Option<Position>,
    /// [`Volatiles::COUNTER`]'s `damage`: twice the damage that foe dealt.
    pub counter_damage: u16,
    /// [`Volatiles::MIRROR_COAT`]'s `slot`: the slot of the last foe that
    /// hit with a special move.
    pub mirror_coat_slot: Option<Position>,
    /// [`Volatiles::MIRROR_COAT`]'s `damage`: twice the damage that foe
    /// dealt.
    pub mirror_coat_damage: u16,
    // Substitute, trapping, residual volatiles.
    /// The substitute's HP in half points (Showdown's `effectState.hp`,
    /// doubled). Metal Burst and Comeuppance deal `damage * 1.5` to a
    /// substitute without truncating it, so it can hold x.5 HP. 0 when the
    /// Pokémon has no substitute.
    pub substitute_hp_halves: u16,
    /// `partiallytrapped`'s `duration`.
    pub partially_trapped: u8,
    /// The move that trapped it (Showdown's `sourceEffect`).
    pub partially_trapped_move: MoveId,
    /// Who trapped it: side and stable [`PokemonState::id`].
    pub partially_trapped_by: (SideId, u8),
    /// Who trapped it with Block, Mean Look, ... (side and stable id). The
    /// volatile ends silently when that Pokémon leaves the field.
    pub trapped_by: (SideId, u8),
    /// The slot the seeder was in (Showdown's `sourceSlot`): whoever stands
    /// there gets the HP.
    pub leech_seed_slot: Position,
    /// Yawn's `duration`.
    pub yawn: u8,
    /// Yawn's `effectState.source`: who set it (its side is Sleep Clause
    /// Mod's business). Set with [`VolatileCounters::yawn`].
    pub yawn_source: Position,
    /// Perish Song's `duration`.
    pub perish_song: u8,
    /// Who it is attracted to (side and stable id).
    pub attract_by: (SideId, u8),
    /// Syrup Bomb's `duration`.
    pub syrup_bomb: u8,
    /// Who used Syrup Bomb (side and stable id).
    pub syrup_bomb_by: (SideId, u8),
    /// Who used Octolock (side and stable id: its `effectState.source`).
    pub octolock_by: (SideId, u8),
    /// Throat Chop's `duration`.
    pub throat_chop: u8,
    /// Heal Block's `duration`.
    pub heal_block: u8,
    /// Magnet Rise's `duration`.
    pub magnet_rise: u8,
    /// Set when a move knocks out a Pokémon under Destiny Bond: the user of
    /// that move (side and stable id), who faints with it.
    pub destiny_bond_ko_by: Option<(SideId, u8)>,
    // Stat, HP and type manipulation.
    /// Showdown's `pokemon.types` once a move changed them (Soak, Reflect
    /// Type, ...); `None` means the species' types. `PokemonState::types`
    /// is derived from it (see `resolve/manipulation.rs`).
    pub types: Option<TypeSet>,
    /// Showdown's `addedType` (Forest's Curse, Trick-or-Treat).
    pub added_type: Option<Type>,
    /// Showdown's `apparentType` when it lags behind the real types (Reflect
    /// Type on a foe); the end of the turn reveals them.
    pub apparent_types: Option<TypeSet>,
    /// Showdown's `knownType` is false: Reflect Type copied types others
    /// can't see. `setType`, Terastallization and being attacked (at the end
    /// of the turn) make them known again.
    pub type_unknown: bool,
    /// Who is in Showdown's `attackedBy` (bits as
    /// [`VolatileCounters::damaged_by`]): Pokémon whose moves hit this one,
    /// kept while they stay on the field.
    pub attacked_by: u16,
    /// Showdown's `lastMoveUsed`: the last move this Pokémon used (called
    /// moves included) with its type after `ModifyType` (Conversion 2).
    pub last_move_used: Option<(MoveId, Type)>,
    /// Stockpile's `layers`.
    pub stockpile_layers: u8,
    /// Stockpile's `def`: minus the Defense stages it actually added.
    pub stockpile_def: i8,
    /// Stockpile's `spd`: minus the Special Defense stages it actually added.
    pub stockpile_spd: i8,
    /// Dragon Cheer's `hasDragonType`.
    pub dragon_cheer_dragon: bool,
    /// Lock-On's target (side and [`PokemonState::id`]).
    pub lock_on: Option<(SideId, u8)>,
    /// Lock-On's `duration`.
    pub lock_on_turns: u8,
    /// Sparkling Aria's volatile: added by its secondary, removed after the
    /// move (it has no handlers, so it needs no place in the volatile order).
    pub sparkling_aria: bool,
    /// Showdown's `statsRaisedThisTurn`: reset when the Pokémon switches out
    /// and at the start of each turn after the first.
    pub stats_raised_this_turn: bool,
    // Action order, targeting, move calling.
    /// Helping Hands received this turn (the power multiplier is `1.5^n`).
    pub helping_hand: u8,
    /// Follow Me / Rage Powder's `effectOrder`: later redirections sort
    /// after earlier ones of the same priority and speed.
    pub redirect_order: u8,
    /// Ally Switch's `duration`.
    pub ally_switch: u8,
    /// Ally Switch's success denominator for its next use (`counter`).
    pub ally_switch_counter: u16,
    /// The Pokémon's own form while transformed, restored on switching out.
    pub transform: Option<TransformBase>,
    /// The index of the move slot Mimic replaced.
    pub mimic_slot: u8,
    /// What the slot Mimic replaced held; restored on switching out.
    pub mimic_original: MoveSlot,
    /// Transformed into a Terastallized Pokémon: its shown type is the Tera
    /// type until the end of the turn reveals the real ones (`apparentType`).
    pub apparent_type_stale: bool,
    /// Terastallized since switching in, so its shown type (`apparentType`)
    /// is its Tera type, which Transform copies.
    pub tera_apparent: bool,
    /// The `effectOrder` of the Pokémon's `abilityState` (set when it
    /// switches in or transforms), which breaks speed ties when redirection
    /// handlers are sorted (`compareRedirectOrder`).
    pub ability_order: u16,
    // Abilities and items.
    /// The `effectOrder` of the Pokémon's `itemState` (0 without an item);
    /// with `ability_order`, it identifies the state a handler was collected
    /// with, as Showdown compares state objects.
    pub item_order: u16,
    /// `choicelock`'s `move` ([`Volatiles::CHOICE_LOCK`]).
    pub choice_lock: MoveId,
    /// Showdown's `abilityState`: renewed on switching in and by
    /// `setAbility`. Kept when the Pokémon leaves the field (Showdown's
    /// `clearVolatile` keeps it), so a benched or fainted Pokémon still has
    /// its last one, with `ability_order`, until it switches in again.
    pub ability_state: AbilityState,
    /// Showdown's `itemState`: renewed on switching in and cleared when the
    /// item changes or is used up. Kept when the Pokémon leaves the field
    /// (Showdown's `clearVolatile` keeps it), so a benched or fainted
    /// Pokémon still has its last one, with `item_order`, until it switches
    /// in again.
    pub item_state: ItemState,
    /// Showdown's `hurtThisTurn` as a number: the HP left right after the
    /// last damage this turn (Emergency Exit compares it with the HP before
    /// the hit; [`VolatileCounters::hurt_this_turn`] is its truthiness).
    pub hurt_hp: u16,
    /// `partiallytrapped`'s `boundDivisor` (8, or 6 with Binding Band; 0
    /// before it is set).
    pub bound_divisor: u8,
    // Held items that stay held.
    /// The Metronome item's volatile's `lastMove` ([`Volatiles::METRONOME`]).
    pub metronome_move: Option<MoveId>,
    /// The Metronome item's volatile's `numConsecutive`.
    pub metronome_count: u8,
    // Switch-in, field, forme and ability-suppressing abilities.
    /// Protosynthesis's / Quark Drive's `bestStat` (0 Attack, 1 Defense, 2
    /// Special Attack, 3 Special Defense, 4 Speed).
    pub paradox_stat: u8,
    /// Protosynthesis's / Quark Drive's `fromBooster`.
    pub paradox_booster: bool,
    /// `commanded`'s `source`: the stable id of the Tatsugiri inside.
    pub commanded_by: Option<u8>,
    // Berries, consumed items, item and ability transfer.
    /// Turns left on Micle Berry's volatile ([`Volatiles::MICLE_BERRY`]).
    pub micle_berry: u8,
    /// Turns left on the `gem` volatile ([`Volatiles::GEM`]).
    pub gem: u8,
}

/// Showdown's `abilityState` beyond its `effectOrder`
/// ([`VolatileCounters::ability_order`]): each ability's own data. Reset
/// when the Pokémon switches in and when its ability changes
/// (`set_ability`), not when it leaves the field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AbilityState {
    // Abilities and items.
    /// `abilityState.ending`: the ability is being removed (Neutralizing
    /// Gas stops suppressing others while it ends).
    pub ending: bool,
    // Switch-in, field, forme and ability-suppressing abilities.
    /// Unnerve's and As One's `unnerved`: its `-ability` was shown (the
    /// foes can't eat berries).
    pub unnerved: bool,
    /// Supreme Overlord's `fallen` (0: unset).
    pub fallen: u8,
    /// Disguise's and Ice Face's `busted`.
    pub busted: bool,
    /// Trace's `seek`.
    pub seek: bool,
    /// Embody Aspect's `embodied`.
    pub embodied: bool,
    /// Tera Shell's `resisted`: the move's later hits are resisted too.
    pub resisted: bool,
    /// Slow Start's `counter` (turns left; 0: unset).
    pub slow_start: u8,
    // Damage, stat and type modifiers.
    /// Gorilla Tactics' `abilityState.choiceLock`: the move it locked the
    /// Pokémon into.
    pub choice_lock: Option<MoveId>,
    // Immunities, contact and reactive effects, status and boost protection.
    /// Berserk's `checkedBerserk` and Anger Shell's `checkedAngerShell`
    /// (`None` until set): whether a healing berry may be eaten now, false
    /// between a move's damage and its `AfterMoveSecondary`.
    pub checked: Option<bool>,
    // Residual, priority, accuracy, KO and miscellaneous.
    /// Protean's and Libero's `effectState.protean` / `libero`: the type
    /// already changed since the Pokémon switched in.
    pub type_changed: bool,
    /// Opportunist's `effectState.boosts`: the foes' stat raises it will
    /// copy (`None`: no foe boost event since the last copy), in the
    /// order their stats were first named.
    pub opportunist: Option<pikapro_data::StatBoosts>,
    // Ability transfer and item abilities (Cud Chew, Harvest, ...).
    /// Gluttony's `abilityState.gluttony` (set by its `onStart` and
    /// `onDamage`): the pinch berries are eaten at half HP.
    pub gluttony: bool,
    /// Ripen's `abilityState.berryWeaken`: the berry just eaten was a
    /// resist berry, whose halving Ripen doubles.
    pub berry_weaken: bool,
    /// Cud Chew's `effectState.berry`: the berry to eat again.
    pub cud_chew_berry: Option<ItemId>,
    /// Cud Chew's `effectState.counter`.
    pub cud_chew_counter: u8,
}

/// Showdown's `itemState` beyond its `effectOrder`
/// ([`VolatileCounters::item_order`]): each item's own data. Reset when the
/// Pokémon switches in and when its item changes or is used up
/// (`set_item`, `take_item`, `use_item`, `eat_item`), not when it leaves
/// the field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ItemState {
    // Held items that stay held.
    /// Utility Umbrella's `inactive`: its `End` ran, so its next `Update`
    /// announces the weather change.
    pub umbrella_inactive: bool,
    // Berries, consumed items, item and ability transfer.
    /// White Herb's `effectState.boosts`: the stats (bit per `BoostId`)
    /// its `onUse` resets.
    pub white_herb: u8,
    /// Eject Pack's `effectState.eject`: a stat was lowered.
    pub eject: bool,
    /// Mirror Herb's `effectState.ready`.
    pub mirror_herb_ready: bool,
    /// Mirror Herb's `effectState.boosts`, in the order their stats were
    /// first raised (`mirror_herb_keys`: bit per `BoostId`).
    pub mirror_herb_boosts: pikapro_data::StatBoosts,
    /// The stats `mirror_herb_boosts` names (bit per `BoostId`), which
    /// Showdown's boost objects keep even at 0.
    pub mirror_herb_keys: u8,
    /// Booster Energy's `effectState.started` (its `onStart` ran).
    pub booster_started: bool,
}

/// What Transform replaces and switching out restores (Showdown's
/// `baseMoveSlots` and `setSpecies(baseSpecies)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TransformBase {
    /// The species it transformed into (weight, species checks).
    pub species: SpeciesId,
    /// Its own species when it transformed, possibly a temporary forme
    /// (Minior-Meteor): Showdown's `baseStoredStats`, which Transform's
    /// `setSpecies` leaves alone, come from it (requests' `stats`).
    pub own_species: SpeciesId,
    /// Its own move slots (`baseMoveSlots`), restored on switching out.
    pub moves: StackVec<MoveSlot, MAX_MOVES>,
    /// Its own stats when it transformed, restored on switching out.
    pub stats: Stats<u16>,
    /// Its types when it transformed, restored on switching out (unless
    /// Terastallized).
    pub types: TypeSet,
    /// Its own `base_types` when it transformed, restored on switching out.
    pub base_types: TypeSet,
}

/// What happened to and by a Pokémon that later effects read (Showdown's
/// `timesAttacked`, `attackedBy`, `move.hitTargets`, `moveSlot.used`).
/// Each field documents when it resets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MoveRecord {
    // Damage formula.
    /// Showdown's `timesAttacked`: hits taken from other Pokémon's damaging
    /// moves (Rage Fist). Never reset under Scarlet/Violet mechanics; the
    /// Champions mod resets it on switch-out, which
    /// [`VolatileCounters::times_attacked`] tracks.
    pub times_attacked: u8,
    // Multi-turn moves.
    /// The foes whose moves damaged this Pokémon, each with its latest hit,
    /// oldest first (Showdown's `attackedBy` as `getLastDamagedBy(true)`
    /// sees it). Reset when the Pokémon switches or faints
    /// (`clearVolatile`), though the hit that knocked it out is recorded
    /// after that (`gotAttacked` follows `faintMessages`) and stays until it
    /// is back on the field; at the end of each turn the entries stop being
    /// `this_turn`, and those of foes that have left the field are dropped.
    pub damaged_by: StackVec<DamagedBy, MAX_TEAM>,

    // Stat, HP and type manipulation.
    /// Showdown's `move.hitTargets` for the move being used, as a bit set
    /// of `side * 2 + slot`: the targets left after the hit steps. Set by
    /// the move pipeline, taken by the move's `AfterMove` (`None`: the move
    /// never got to its hit steps).
    pub hit_targets: Option<u8>,
    // Action order, targeting, move calling.
    /// Move slots (bit per index) whose PP was deducted since the Pokémon
    /// switched in (Showdown's `moveSlot.used`, read by Last Resort).
    /// Reset on switching in and when the move slots are replaced.
    pub used_moves: u8,
}

/// A move's outcome as Showdown's `moveThisTurnResult`
/// records it (`undefined`, `null`, `false` or `true`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MoveResult {
    /// No move this turn.
    #[default]
    Undefined,
    /// The move did nothing without failing (blocked by Protect, a
    /// `TryMove` that returned `null`, recharging).
    Null,
    /// The move failed (missed, immune, stopped before moving, ...).
    Failed,
    /// The move succeeded.
    Succeeded,
}

/// One of Showdown's `attackedBy` entries with numeric damage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DamagedBy {
    /// The attacker's side.
    pub side: SideId,
    /// The attacker's stable [`PokemonState::id`].
    pub id: u8,
    /// The slot it attacked from (`source.getSlot()`), or `None` if it was
    /// not on the field (a Future Sight user that had switched out).
    pub slot: Option<Position>,
    /// HP lost to the last hit.
    pub damage: u16,
    /// Dealt this turn (`thisTurn`).
    pub this_turn: bool,
}

/// A pending Future Sight or Doom Desire (Showdown's
/// `futuremove` slot condition).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FutureMove {
    /// Future Sight or Doom Desire.
    pub move_id: MoveId,
    /// The user's side (`source`).
    pub source_side: SideId,
    /// The user's stable [`PokemonState::id`].
    pub source_id: u8,
    /// `endingTurn`: the attack lands at the end of the turn after it.
    pub ending_turn: u16,
}

/// Showdown's `switchFlag`: an active Pokémon that has to leave its slot at
/// the next switch request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SwitchFlag {
    /// No switch pending.
    #[default]
    None,
    /// Fainted, to be replaced (`checkFainted` at the end of the queue).
    /// Showdown's `switchFlag = true`, as for [`SwitchFlag::True`]; nothing
    /// tells the two apart ([`SwitchFlag::is_true`]). Emergency Exit and
    /// Wimp Out set this one too.
    Fainted,
    /// Switching out after using this move (U-turn, Volt Switch, ...); the
    /// replacement's switch-in is logged `[from]` it.
    Move(MoveId),
    /// Showdown's `switchFlag = true` set by an item (Eject Button, Eject
    /// Pack): switching out, with no `[from]`. Emergency Exit's is set as
    /// [`SwitchFlag::Fainted`], the same `true`.
    True,
}

impl SwitchFlag {
    /// Showdown's `switchFlag === true`: set by an effect or a faint, not
    /// by a move.
    #[inline]
    pub fn is_true(self) -> bool {
        matches!(self, SwitchFlag::True | SwitchFlag::Fainted)
    }
}

/// One move slot of a Pokémon (Showdown's `moveSlot`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MoveSlot {
    /// The move in the slot.
    pub id: MoveId,
    /// PP left.
    pub pp: u8,
    /// The slot's maximum PP.
    pub max_pp: u8,
    /// Can't be chosen: recomputed each turn by the `DisableMove` handlers
    /// (Disable, Taunt, Gravity, Imprison, a Choice lock, ...).
    pub disabled: bool,
    /// Showdown's `moveSlot.target` when it isn't the move's own: Sketch
    /// and Mimic copy the target of the move they copy as it was used
    /// (Curse from a non-Ghost: `self`), and Transform copies the slot's.
    /// Requests and choice validation read it.
    pub target: Option<MoveTarget>,
}

/// Showdown's staleness values, which its Endless Battle Clause reads
/// ([`Format::endless_battle_clause`](crate::Format::endless_battle_clause)):
/// whether a Pokémon restored PP or HP in a way that could go on for ever.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Staleness {
    /// Not stale.
    #[default]
    None = 0,
    /// By its own side's doing (it ate a restorative berry it got back).
    Internal = 1,
    /// By a foe's doing (a foe healed it, ate or flung its Leppa Berry, or
    /// gave it an ability).
    External = 2,
}

impl Staleness {
    const fn from_bits(bits: u8) -> Staleness {
        match bits & 3 {
            1 => Staleness::Internal,
            2 => Staleness::External,
            _ => Staleness::None,
        }
    }
}

/// A Pokémon's Endless Battle Clause bookkeeping, packed in a byte:
/// Showdown's `staleness`, `volatileStaleness` and `pendingStaleness`, and
/// what `maybeTriggerEndlessBattleClause` reads of its set (`pokemon.set`).
/// The staleness is kept in every format, as in Showdown; only a format
/// with the clause reads it, and only there are the set's two flags
/// worked out when the battle starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EndlessState(u8);

impl EndlessState {
    const STALENESS: u8 = 0;
    const VOLATILE: u8 = 2;
    const PENDING: u8 = 4;
    const SET_BERRY: u8 = 1 << 6;
    const SET_CYCLE: u8 = 1 << 7;

    /// A Pokémon that isn't stale, with what its set says: whether its item
    /// is a restorative berry, and whether it can get a berry back
    /// (Harvest, Pickup or Recycle).
    pub const fn of_set(berry: bool, cycle: bool) -> EndlessState {
        EndlessState(if berry { Self::SET_BERRY } else { 0 } | if cycle { Self::SET_CYCLE } else { 0 })
    }

    const fn get(self, shift: u8) -> Staleness {
        Staleness::from_bits(self.0 >> shift)
    }

    fn set(&mut self, shift: u8, value: Staleness) {
        self.0 = self.0 & !(3 << shift) | (value as u8) << shift;
    }

    /// Showdown's `pokemon.staleness`: it lasts the whole battle.
    pub const fn staleness(self) -> Staleness {
        self.get(Self::STALENESS)
    }

    /// Showdown's `pokemon.volatileStaleness`: gone when the Pokémon
    /// leaves the field (`clearVolatile`).
    pub const fn volatile_staleness(self) -> Staleness {
        self.get(Self::VOLATILE)
    }

    /// Showdown's `pokemon.pendingStaleness`: what eating the held item
    /// will make of `staleness`, if it is a restorative berry (set when
    /// the Pokémon is given an item, dropped when the item is taken).
    pub const fn pending_staleness(self) -> Staleness {
        self.get(Self::PENDING)
    }

    /// Showdown's `pokemon.volatileStaleness || pokemon.staleness`.
    pub const fn stale(self) -> Staleness {
        match self.volatile_staleness() {
            Staleness::None => self.staleness(),
            volatile => volatile,
        }
    }

    /// Whether the set's item is a restorative berry
    /// (`RESTORATIVE_BERRIES.has(toID(pokemon.set.item))`). False in a
    /// format without the clause.
    pub const fn set_berry(self) -> bool {
        self.0 & Self::SET_BERRY != 0
    }

    /// Whether the set can get a berry back: its ability is Harvest or
    /// Pickup, or it has Recycle. False in a format without the clause.
    pub const fn set_cycle(self) -> bool {
        self.0 & Self::SET_CYCLE != 0
    }

    pub(crate) fn set_staleness(&mut self, value: Staleness) {
        self.set(Self::STALENESS, value);
    }

    pub(crate) fn set_volatile_staleness(&mut self, value: Staleness) {
        self.set(Self::VOLATILE, value);
    }

    pub(crate) fn set_pending_staleness(&mut self, value: Staleness) {
        self.set(Self::PENDING, value);
    }
}

/// One Pokémon's in-battle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PokemonState {
    /// Stable identity: the Pokémon's index in its registered team. Unlike
    /// its position in [`SideState::team`], this never changes.
    pub id: u8,
    /// The current species or forme (forme changes, Transform); switching
    /// out restores `base_species`.
    pub species: SpeciesId,
    /// Level, 1..=100.
    pub level: u8,
    /// The species' own types; STAB and Tera checks need them after a type change.
    pub base_types: TypeSet,
    /// The forme's types as the species data gives them (Showdown's
    /// `apparentType` after `setSpecies`). They differ from `base_types`
    /// only for an Arceus whose Multitype follows its Plate.
    pub species_types: TypeSet,
    /// Current types (Terastallization, Soak, ... change these).
    pub types: TypeSet,
    /// The Tera type (possibly Stellar).
    pub tera_type: Type,
    /// Whether it has Terastallized.
    pub terastallized: bool,
    /// Showdown's `stellarBoostedTypes`: the move types (bit `1 <<
    /// type`) whose once-per-type Stellar boost this Pokémon has used.
    /// Never reset.
    pub stellar_boosted: u32,
    /// Showdown's `canTerastallize` is `null` from the start (its
    /// item is a Z-Crystal or a Mega Stone it can use): it can never
    /// Terastallize.
    pub tera_blocked: bool,
    /// The current ability (Showdown's `ability`); switching out restores
    /// `base_ability`.
    pub ability: AbilityId,
    /// The held item; `None` when it holds none.
    pub item: Option<ItemId>,
    /// Computed stats; `stats[Hp]` is max HP.
    pub stats: Stats<u16>,
    /// The stats from the set for the Pokémon's own species (Showdown's
    /// `baseStoredStats`): `setSpecies` on switching out restores them,
    /// undoing Power Trick, Guard Split, Transform and the like.
    pub set_stats: Stats<u16>,
    /// The set's effective EVs, for recomputing stats for another species
    /// (Transform's `setSpecies`), with `ivs` and `nature`.
    pub evs: Stats<u8>,
    /// The set's IVs (see `evs`).
    pub ivs: Stats<u8>,
    /// The set's nature (see `evs`).
    pub nature: Nature,
    /// Current HP; 0 once fainted. Max HP is `stats[Hp]`.
    pub hp: u16,
    /// Non-volatile status condition.
    pub status: Status,
    /// Showdown's `statusState.source?.isAlly(this)`, for a sleep: whether
    /// the sleep's source is on this Pokémon's side (Rest, an ally's move
    /// or Yawn), which Sleep Clause Mod doesn't count. Meaningless unless
    /// the status is `Sleep`.
    pub sleep_source_ally: bool,
    /// Stat stages; `stats` does not include them.
    pub boosts: Boosts,
    /// Move slots in set order (Transform and Mimic replace them until it
    /// switches out).
    pub moves: StackVec<MoveSlot, MAX_MOVES>,
    /// Volatile conditions present; cleared on switching out.
    pub volatiles: Volatiles,
    /// Durations and data of the volatiles in `volatiles`.
    pub volatile_counters: VolatileCounters,
    /// What happened to and by this Pokémon that later effects read.
    pub record: MoveRecord,
    /// Consecutive successful protection moves; the next one succeeds with
    /// chance `1 / 3^n`, capped at 1/729 (Showdown's `stall` counter).
    pub protect_counter: u8,
    /// Turns left on Showdown's `stall` volatile (0 = absent). It is set to 2
    /// on each successful protection and counts down at end of turn.
    pub stall_turns: u8,
    /// Turns spent active since the last switch-in.
    pub active_turns: u16,
    /// Showdown's `lastMove`: the last move it used; `None` since switching in.
    pub last_move: Option<MoveId>,
    /// `lastMove.target`: the target type `last_move` was used with, after
    /// its `ModifyMove` (Curse from a non-Ghost: `self`). Sketch and Mimic
    /// copy it into the slot.
    pub last_move_target: Option<MoveTarget>,
    /// The target location `last_move` was chosen with (Showdown's
    /// `lastMoveTargetLoc`); locked moves such as recharging reuse it.
    pub last_move_target_loc: i8,
    /// Move actions taken since the last switch-in, including ones that did
    /// not get to move (Showdown's `activeMoveActions`; Fake Out checks it).
    pub active_move_actions: u8,
    /// Showdown's stored `pokemon.speed`: the action speed as of the last
    /// `updateSpeed`. Speed-ordered event handling (`eachEvent`, switch-in
    /// ordering, residual handlers) sorts by this, not by a fresh value.
    pub speed: i32,
    /// Showdown's `switchFlag`.
    pub switch_flag: SwitchFlag,
    /// Showdown's `forceSwitchFlag`: hit by Roar / Dragon Tail; dragged out
    /// at the end of the current action.
    pub force_switch_flag: bool,
    /// Showdown's `skipBeforeSwitchOutEventFlag`: `BeforeSwitchOut` already
    /// ran when the switch request was made.
    pub skip_before_switch_out: bool,
    // Abilities and items.
    /// Showdown's `baseSpecies`: the set's species, or the forme a permanent
    /// forme change made (Mega Evolution, Shaymin thawing into Land Forme,
    /// Mimikyu's busted Disguise). `species` returns to it on switching out.
    pub base_species: SpeciesId,
    /// Showdown's `baseAbility`: `ability` returns to it on switching out.
    pub base_ability: AbilityId,
    /// Showdown's `lastItem`: the last item it used or ate (Recycle,
    /// Harvest, Pickup).
    pub last_item: Option<ItemId>,
    /// Showdown's `usedItemThisTurn`. Reset at the start of each turn after
    /// the first and when the Pokémon switches out.
    pub used_item_this_turn: bool,
    /// Showdown's `ateBerry` (Belch, Cud Chew). Never reset.
    pub ate_berry: bool,
    /// Showdown's `trapped` as set by `tryTrap` from an ability's
    /// `TrapPokemon` handler (Shadow Tag, Arena Trap, Magnet Pull) at the
    /// end of the turn. The volatiles that trap are read directly.
    pub trapped: bool,
    /// Shed Shell cleared `trapped` this turn: nothing traps the Pokémon.
    pub trap_released: bool,
    /// The move slots an ability's, item's or volatile's `DisableMove`
    /// handler disabled this turn (bit per slot; `disableMove` without
    /// `isHidden`, so the request shows them disabled).
    pub effect_disabled: u8,
    // Switch-in, field, forme and ability-suppressing abilities:
    // once-per-battle flags and the like.
    /// Showdown's `swordBoost` (Intrepid Sword): the boost happened once this
    /// battle.
    pub sword_boost: bool,
    /// Showdown's `shieldBoost` (Dauntless Shield): the boost happened once
    /// this battle.
    pub shield_boost: bool,
    /// Showdown's `syrupTriggered` (Supersweet Syrup).
    pub syrup_triggered: bool,
    /// Showdown's `showCure` (Natural Cure): `None` is `undefined`.
    pub show_cure: Option<bool>,
    /// Showdown's `heroMessageDisplayed` (Zero to Hero).
    pub hero_message_displayed: bool,
    /// Showdown's `bondTriggered` (Battle Bond).
    pub bond_triggered: bool,
    /// Showdown's `illusion`: the stable id of the teammate Illusion
    /// disguises this Pokémon as (it keeps it on the bench).
    pub illusion: Option<u8>,
    /// Showdown's `apparentType` once the end of the turn revealed it for
    /// an Illusion disguised as this Pokémon (`None`: its species' types,
    /// as `setSpecies` leaves it). Reset when it leaves the field.
    pub illusion_apparent: Option<TypeSet>,
    /// Showdown's `beingCalledBack`: switching out (its ability's `End`
    /// knows). Reset by `clearVolatile`.
    pub being_called_back: bool,
    /// Showdown's `set.species`: the species the team named (Shields Down's
    /// Core, Beat Up's power, `formeRegression`).
    pub set_species: SpeciesId,
    /// Showdown's `set.ability`, which `formeRegression` restores
    /// (Ogerpon's and Terapagos's Tera formes changed it).
    pub set_ability: AbilityId,
    /// Showdown's `formeRegression`, with the species `baseSpecies` goes
    /// back to on fainting (the set's: a terastallized Hangry Morpeko,
    /// Zygarde-Complete).
    pub forme_regression: Option<SpeciesId>,

    // Residual, priority, accuracy, KO and miscellaneous.
    /// Showdown's `move.pranksterBoosted` for the move this Pokémon is
    /// using (set when its move action starts, cleared by the next one):
    /// Prankster raised the chosen status move's priority, so Dark-type
    /// foes are immune to it (and to a move it calls).
    pub prankster_boosted: bool,

    // Mega Evolution.
    /// Showdown's `canMegaEvo`: the Mega forme this Pokémon can become,
    /// computed once when the battle starts (`actions.canMegaEvo`: its
    /// Mega Stone, with the Gen 9 base-species fallback). Cleared for the whole
    /// side once one of its Pokémon Mega Evolves (Showdown's `false`, which
    /// [`SideState::gimmicks_used`] also records).
    pub can_mega_evo: Option<SpeciesId>,
    /// The Endless Battle Clause's bookkeeping (Showdown's `staleness`,
    /// `volatileStaleness` and `pendingStaleness`).
    pub endless: EndlessState,
    // Teams, choices, gender.
    /// Showdown's `pokemon.gender`, fixed when the battle is created: the
    /// set's, else the species', else drawn at random. Forme changes and
    /// Transform keep it.
    pub gender: crate::team::Gender,
}

impl Default for PokemonState {
    fn default() -> Self {
        PokemonState {
            id: 0,
            species: SpeciesId(0),
            level: 1,
            base_types: TypeSet::mono(Type::Normal),
            species_types: TypeSet::mono(Type::Normal),
            types: TypeSet::mono(Type::Normal),
            tera_type: Type::Normal,
            terastallized: false,
            stellar_boosted: 0,
            tera_blocked: false,
            ability: AbilityId::NONE,
            item: None,
            stats: Stats::splat(1),
            set_stats: Stats::splat(1),
            evs: Stats::splat(0),
            ivs: Stats::splat(0),
            nature: Nature::default(),
            hp: 0,
            status: Status::None,
            sleep_source_ally: false,
            boosts: Boosts::default(),
            moves: StackVec::new(),
            volatiles: Volatiles::default(),
            volatile_counters: VolatileCounters::default(),
            record: MoveRecord::default(),
            protect_counter: 0,
            stall_turns: 0,
            active_turns: 0,
            last_move: None,
            last_move_target: None,
            last_move_target_loc: 0,
            active_move_actions: 0,
            speed: 0,
            switch_flag: SwitchFlag::None,
            force_switch_flag: false,
            skip_before_switch_out: false,
            base_species: SpeciesId(0),
            base_ability: AbilityId::NONE,
            last_item: None,
            used_item_this_turn: false,
            ate_berry: false,
            trapped: false,
            trap_released: false,
            effect_disabled: 0,
            prankster_boosted: false,
            sword_boost: false,
            shield_boost: false,
            syrup_triggered: false,
            show_cure: None,
            hero_message_displayed: false,
            bond_triggered: false,
            illusion: None,
            illusion_apparent: None,
            being_called_back: false,
            set_species: SpeciesId(0),
            forme_regression: None,
            can_mega_evo: None,
            endless: EndlessState(0),
            set_ability: AbilityId::NONE,
            gender: crate::team::Gender::Genderless,
        }
    }
}

impl PokemonState {
    /// What Showdown's `getTypes()` answers for a Terastallized
    /// Pokémon: its Tera type, unless it Terastallized into Stellar (which
    /// keeps its own types). `None` if it has not Terastallized.
    #[inline]
    pub fn tera_types(&self) -> Option<TypeSet> {
        (self.terastallized && self.tera_type != Type::Stellar).then_some(TypeSet::mono(self.tera_type))
    }

    /// Max HP (`stats[Hp]`).
    #[inline]
    pub fn max_hp(&self) -> u16 {
        self.stats[Stat::Hp]
    }

    /// Whether HP is 0.
    #[inline]
    pub fn is_fainted(&self) -> bool {
        self.hp == 0
    }

    /// Whether any move can be selected. When none can, the Pokémon
    /// struggles (legality and resolution must agree on this).
    #[inline]
    pub fn has_usable_move(&self) -> bool {
        self.moves.iter().any(|m| m.pp > 0 && !m.disabled)
    }

    /// Resets everything that does not survive switching out (Showdown's
    /// `clearVolatile`, whose `setSpecies` also resets the stored speed to
    /// the raw Speed stat).
    pub(crate) fn clear_volatile_state(&mut self) {
        // Transform and Mimic end (`moveSlots = baseMoveSlots`).
        self.restore_base_form();
        // `ability = baseAbility`, `setSpecies(baseSpecies)`: a forme that
        // was not permanent ends, the stats go back to the set's (`set_stats`,
        // the base species': undoing Power Trick, Guard Split, ...), then the
        // types reset (Soak, Roost, ...).
        self.ability = self.base_ability;
        self.species = self.base_species;
        self.stats = self.set_stats;
        self.types = self.tera_types().unwrap_or(self.base_types);
        self.boosts = Boosts::default();
        self.volatiles = Volatiles::default();
        // `abilityState` and `itemState` survive, with their
        // `effectOrder`: `clearVolatile` only does `delete
        // abilityState.started` (no Gen 9 ability reads it) and `delete
        // itemState.started` (Booster Energy's). `switchIn` renews both
        // (`new_ability_state`, `new_item_state`). Until then a fainted
        // Neutralizing Gas keeps `ending`, so if Revival Blessing brings it
        // back in its slot, the `End` its instaswitch runs shows `-end`
        // without a second speed sort.
        let kept = &self.volatile_counters;
        let (ability_state, ability_order) = (kept.ability_state, kept.ability_order);
        let (mut item_state, item_order) = (kept.item_state, kept.item_order);
        item_state.booster_started = false;
        self.volatile_counters = VolatileCounters::default();
        let c = &mut self.volatile_counters;
        (c.ability_state, c.ability_order) = (ability_state, ability_order);
        (c.item_state, c.item_order) = (item_state, item_order);
        // `setSpecies` shows the forme's own types again (`apparentType`):
        // an Arceus of another type reveals it at the end of a turn.
        // (An Illusion's earlier reveal, `illusion_apparent`, stands.)
        if self.species_types != self.base_types && self.illusion_apparent.is_none() {
            self.volatile_counters.apparent_types = Some(self.species_types);
        }
        self.protect_counter = 0;
        self.stall_turns = 0;
        self.active_turns = 0;
        self.last_move = None;
        self.last_move_target = None;
        self.speed = self.stats[Stat::Spe] as i32;
        self.last_move_target_loc = 0;
        // (`activeMoveActions` is reset by `switchIn`, not here: a fainted
        // Pokémon keeps it, which its request shows.)
        // `clearVolatile(includeSwitchFlags = true)`.
        self.switch_flag = SwitchFlag::None;
        self.force_switch_flag = false;
        self.trapped = false;
        self.trap_released = false;
        self.effect_disabled = 0;
        // `beingCalledBack = false`.
        self.being_called_back = false;
        // `volatileStaleness = undefined`.
        self.endless.set_volatile_staleness(Staleness::None);
        // `attackedBy = []`.
        self.record.damaged_by.clear();
    }
}

/// A side condition (the counters in [`SideConditions`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SideCondition {
    /// Reflect: weakens physical moves against the side.
    #[default]
    Reflect,
    /// Light Screen: weakens special moves against the side.
    LightScreen,
    /// Aurora Veil: weakens physical and special moves against the side.
    AuroraVeil,
    /// Tailwind: doubles the side's Speed.
    Tailwind,
    // Hazards, side, field and slot conditions.
    /// Safeguard: blocks non-volatile status and confusion from foes.
    Safeguard,
    /// Mist: blocks stat drops from foes.
    Mist,
    /// Wide Guard: blocks spread moves this turn.
    WideGuard,
    /// Quick Guard: blocks priority moves this turn.
    QuickGuard,
    /// Stealth Rock: Rock-type damage on switching in.
    StealthRock,
    /// Spikes: damage to grounded Pokémon switching in (up to 3 layers).
    Spikes,
    /// Toxic Spikes: poisons grounded Pokémon switching in (2 layers: badly).
    ToxicSpikes,
    /// Sticky Web: lowers the Speed of grounded Pokémon switching in.
    StickyWeb,
    /// Sea of fire (Fire Pledge's combination): damages non-Fire types each turn.
    FirePledge,
    /// Swamp (Grass Pledge's combination): quarters the side's Speed.
    GrassPledge,
    /// Rainbow (Water Pledge's combination): doubles the side's secondary
    /// effect chances.
    WaterPledge,
}

impl SideCondition {
    /// Every side condition, indexed by its discriminant.
    pub const ALL: [SideCondition; 15] = [
        SideCondition::Reflect,
        SideCondition::LightScreen,
        SideCondition::AuroraVeil,
        SideCondition::Tailwind,
        SideCondition::Safeguard,
        SideCondition::Mist,
        SideCondition::WideGuard,
        SideCondition::QuickGuard,
        SideCondition::StealthRock,
        SideCondition::Spikes,
        SideCondition::ToxicSpikes,
        SideCondition::StickyWeb,
        SideCondition::FirePledge,
        SideCondition::GrassPledge,
        SideCondition::WaterPledge,
    ];

    /// Entry hazards have layers instead of a duration.
    #[inline]
    pub fn is_hazard(self) -> bool {
        matches!(
            self,
            SideCondition::StealthRock | SideCondition::Spikes | SideCondition::ToxicSpikes | SideCondition::StickyWeb
        )
    }
}

/// Side conditions. Each field is turns remaining, or for entry hazards the
/// number of layers (`0` = inactive).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SideConditions {
    /// Tailwind turns left.
    pub tailwind: u8,
    /// Reflect turns left.
    pub reflect: u8,
    /// Light Screen turns left.
    pub light_screen: u8,
    /// Aurora Veil turns left.
    pub aurora_veil: u8,
    // Hazards, side, field and slot conditions.
    /// Safeguard turns left.
    pub safeguard: u8,
    /// Mist turns left.
    pub mist: u8,
    /// Wide Guard: 1 while up, this turn only.
    pub wide_guard: u8,
    /// Quick Guard: 1 while up, this turn only.
    pub quick_guard: u8,
    /// 1 while Stealth Rock is up.
    pub stealth_rock: u8,
    /// Layers, up to 3.
    pub spikes: u8,
    /// Layers, up to 2.
    pub toxic_spikes: u8,
    /// 1 while Sticky Web is up.
    pub sticky_web: u8,
    /// Sea of fire turns left (the pledge combinations are Fire, Grass
    /// and Water Pledge's side conditions).
    pub fire_pledge: u8,
    /// Swamp turns left.
    pub grass_pledge: u8,
    /// Rainbow turns left.
    pub water_pledge: u8,
    /// The active conditions in the order they started. Showdown iterates
    /// `side.sideConditions` in insertion order when it collects end-of-turn
    /// handlers, which decides how speed ties between them are shuffled.
    pub order: StackVec<SideCondition, 16>,
    /// Slot conditions of each active slot (Showdown's `side.slotConditions`).
    pub slots: [SlotConditions; MAX_ACTIVE],
    /// A pledge waiting for this side's Pokémon with stable id `.0` to
    /// combine with it: Showdown's `prioritizeAction` sets that Pokémon's
    /// queued action's `sourceEffect` to the pledge `.1`.
    pub pledge_combo: Option<(u8, MoveId)>,
}

impl SideConditions {
    /// Turns remaining on `condition`, or its layers (`0` = inactive).
    #[inline]
    pub fn turns(&self, condition: SideCondition) -> u8 {
        match condition {
            SideCondition::Reflect => self.reflect,
            SideCondition::LightScreen => self.light_screen,
            SideCondition::AuroraVeil => self.aurora_veil,
            SideCondition::Tailwind => self.tailwind,
            SideCondition::Safeguard => self.safeguard,
            SideCondition::Mist => self.mist,
            SideCondition::WideGuard => self.wide_guard,
            SideCondition::QuickGuard => self.quick_guard,
            SideCondition::StealthRock => self.stealth_rock,
            SideCondition::Spikes => self.spikes,
            SideCondition::ToxicSpikes => self.toxic_spikes,
            SideCondition::StickyWeb => self.sticky_web,
            SideCondition::FirePledge => self.fire_pledge,
            SideCondition::GrassPledge => self.grass_pledge,
            SideCondition::WaterPledge => self.water_pledge,
        }
    }

    /// The counter of `condition`, mutably.
    #[inline]
    pub fn turns_mut(&mut self, condition: SideCondition) -> &mut u8 {
        match condition {
            SideCondition::Reflect => &mut self.reflect,
            SideCondition::LightScreen => &mut self.light_screen,
            SideCondition::AuroraVeil => &mut self.aurora_veil,
            SideCondition::Tailwind => &mut self.tailwind,
            SideCondition::Safeguard => &mut self.safeguard,
            SideCondition::Mist => &mut self.mist,
            SideCondition::WideGuard => &mut self.wide_guard,
            SideCondition::QuickGuard => &mut self.quick_guard,
            SideCondition::StealthRock => &mut self.stealth_rock,
            SideCondition::Spikes => &mut self.spikes,
            SideCondition::ToxicSpikes => &mut self.toxic_spikes,
            SideCondition::StickyWeb => &mut self.sticky_web,
            SideCondition::FirePledge => &mut self.fire_pledge,
            SideCondition::GrassPledge => &mut self.grass_pledge,
            SideCondition::WaterPledge => &mut self.water_pledge,
        }
    }
}

/// A slot condition: an effect on an active position that outlasts the
/// Pokémon in it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SlotCondition {
    /// Wish: heals the slot at the end of the next turn (data in
    /// [`SlotConditions::wish`]).
    #[default]
    Wish,
    /// Healing Wish: fully heals and cures the next Pokémon in the slot that needs it.
    HealingWish,
    /// Lunar Dance: like Healing Wish, and restores PP too.
    LunarDance,
    /// Revival Blessing: the slot waits for the choice of a fainted Pokémon
    /// to revive (duration 1).
    RevivalBlessing,
    /// Future Sight or Doom Desire (`futuremove`). Its attack is kept in
    /// [`SideState::future_moves`]; the entry here only records when it was
    /// added among the slot's other conditions (Showdown's
    /// `side.slotConditions[position]` keeps insertion order, which decides
    /// how their handlers are collected and so how ties shuffle).
    FutureMove,
}

/// A pending Wish.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Wish {
    /// Showdown's `getOverflowedTurnCount()` when it was made: the turn
    /// before (it never actually overflows, see `resolve/slots.rs`).
    pub starting_turn: u16,
    /// Half the wisher's max HP, rounded down.
    pub hp: u16,
    /// Who made it (`[wisher]`).
    pub wisher: SpeciesId,
    /// The wisher's `PokemonState::id` (`[wisher]` shows its name).
    pub wisher_id: u8,
}

/// The slot conditions of one active slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SlotConditions {
    /// The pending Wish (meaningful while `order` holds `Wish`).
    pub wish: Wish,
    /// The present conditions, oldest first (a future move included).
    pub order: StackVec<SlotCondition, 5>,
    /// Conditions (bit `1 << condition`) whose Showdown state has a
    /// Pokémon `target` because a `runEvent` handler of theirs ran
    /// (Healing Wish's `onSwap` after Ally Switch). `fieldEvent` then skips
    /// their handlers, as it looks for the state in the Pokémon's volatiles.
    pub targeted: u8,
}

impl SlotConditions {
    /// Whether `condition` is present.
    #[inline]
    pub fn has(&self, condition: SlotCondition) -> bool {
        self.order.contains(&condition)
    }
}

/// One player's side of the field.
///
/// `team` follows Showdown's `side.pokemon` ordering exactly: after team
/// preview it holds only the brought Pokémon in the chosen order, and every
/// switch swaps the incoming and outgoing Pokémon's positions. So the
/// Pokémon in slot `i` is always `team[i]`, and a switch choice's index is a
/// position in the *current* order, as in Showdown's `switch N`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SideState {
    /// The side's Pokémon, in Showdown's `side.pokemon` order (see above).
    pub team: StackVec<PokemonState, MAX_TEAM>,
    /// `Some(slot)` while slot `slot` holds a non-fainted Pokémon (always at
    /// `team[slot]`); `None` once it faints or if it was never filled.
    pub active: [Option<u8>; MAX_ACTIVE],
    /// Side conditions, and the slot conditions of each active slot.
    pub conditions: SideConditions,
    /// Gimmicks this side has already used this battle.
    pub gimmicks_used: Gimmicks,
    /// Showdown's `totalFainted` (capped at 100; Last
    /// Respects).
    pub total_fainted: u8,
    /// Showdown's `faintedThisTurn` is set.
    pub fainted_this_turn: bool,
    /// Showdown's `faintedLastTurn` is set (Retaliate).
    pub fainted_last_turn: bool,
    /// The `futuremove` slot condition of each active slot.
    pub future_moves: [Option<FutureMove>; MAX_ACTIVE],
}

impl SideState {
    /// Pokémon in `team` that have not fainted.
    pub fn pokemon_left(&self) -> usize {
        self.team.iter().filter(|p| !p.is_fainted()).count()
    }

    /// The Pokémon in `slot`, or `None` if it is empty or not a slot of
    /// this format (an off-field user of Future Sight stands at one).
    ///
    /// For speed this trusts that a filled slot `slot` holds `Some(slot)`
    /// and that `slot` is below `team.len()`, as in every state a battle
    /// holds: it returns `team[slot]` without reading the position. In a
    /// hand-built state that breaks it, the result is unspecified (a
    /// release build may return a removed or default Pokémon; a debug
    /// build panics), never undefined behaviour.
    #[inline]
    pub fn active(&self, slot: usize) -> Option<&PokemonState> {
        // The active Pokémon in `slot` is `team[slot]` (switches swap team
        // positions), and the team only shrinks at the start, before
        // anyone is sent out.
        let filled = self.active.get(slot)?;
        debug_assert!(filled.is_none_or(|i| i as usize == slot), "slot {slot} holds team position {filled:?}");
        filled.map(|_| self.team.in_capacity(slot))
    }

    /// Like `active`, mutably.
    #[inline]
    pub fn active_mut(&mut self, slot: usize) -> Option<&mut PokemonState> {
        let filled = *self.active.get(slot)?;
        debug_assert!(filled.is_none_or(|i| i as usize == slot), "slot {slot} holds team position {filled:?}");
        filled.map(|_| self.team.in_capacity_mut(slot))
    }

    /// Whether team position `team_index` holds an active Pokémon.
    #[inline]
    pub fn is_active(&self, team_index: u8) -> bool {
        self.active.contains(&Some(team_index))
    }

    /// The current team position of the Pokémon with stable `id`.
    #[inline]
    pub fn position_of(&self, id: u8) -> Option<usize> {
        self.team.iter().position(|p| p.id == id)
    }

    /// Team indices that could be switched in: not active, not fainted.
    pub fn bench(&self) -> impl Iterator<Item = u8> + '_ {
        (0..self.team.len() as u8).filter(|&i| !self.is_active(i) && !self.team[i as usize].is_fainted())
    }
}

/// A weather condition (Showdown's `field.weather`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weather {
    /// Sunny Day's sun (`sunnyday`).
    Sun,
    /// Rain (`raindance`).
    Rain,
    /// Sandstorm (`sandstorm`).
    Sand,
    /// Snow (`snowscape`).
    Snow,
    /// Desolate Land's extremely harsh sunlight (`desolateland`).
    HarshSun,
    /// Primordial Sea's heavy rain (`primordialsea`).
    HeavyRain,
    /// Delta Stream's strong winds (`deltastream`).
    StrongWinds,
}

/// A terrain (Showdown's `field.terrain`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Terrain {
    /// Electric Terrain (`electricterrain`).
    Electric,
    /// Grassy Terrain (`grassyterrain`).
    Grassy,
    /// Misty Terrain (`mistyterrain`).
    Misty,
    /// Psychic Terrain (`psychicterrain`).
    Psychic,
}

/// A pseudo-weather (Showdown's `field.pseudoWeather`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PseudoWeather {
    /// Trick Room: turns in `FieldState::trick_room_turns`.
    #[default]
    TrickRoom,
    /// Gravity: turns in `FieldState::gravity_turns`.
    Gravity,
    /// Magic Room: turns in `FieldState::magic_room_turns`.
    MagicRoom,
    /// Wonder Room: turns in `FieldState::wonder_room_turns`.
    WonderRoom,
    /// Turns in `FieldState::fairy_lock_turns`.
    FairyLock,
    /// Turns in `FieldState::echoed_voice_turns`.
    EchoedVoice,
}

/// Field-wide conditions. Turn counters are turns remaining.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FieldState {
    /// The current weather, if any.
    pub weather: Option<Weather>,
    /// Weather turns left; 0 for the primal weathers, which have no duration.
    pub weather_turns: u8,
    /// The current terrain, if any.
    pub terrain: Option<Terrain>,
    /// Terrain turns left.
    pub terrain_turns: u8,
    /// Trick Room turns left (`0` = inactive).
    pub trick_room_turns: u8,
    /// Turns left on the Echoed Voice pseudo-weather
    /// (`0` = inactive).
    pub echoed_voice_turns: u8,
    /// The Echoed Voice pseudo-weather's `multiplier`.
    pub echoed_voice_multiplier: u8,
    /// Showdown's `lastSuccessfulMoveThisTurn` (Fusion Bolt,
    /// Fusion Flare).
    pub last_successful_move: Option<MoveId>,
    /// Fairy Lock's `duration` (`0` = inactive).
    pub fairy_lock_turns: u8,
    // Hazards, side, field and slot conditions.
    /// Gravity turns left (`0` = inactive).
    pub gravity_turns: u8,
    /// Magic Room turns left (`0` = inactive).
    pub magic_room_turns: u8,
    /// Wonder Room turns left (`0` = inactive).
    pub wonder_room_turns: u8,
    /// Who started Magic Room (side, stable id): its end names them.
    pub magic_room_source: Option<(SideId, u8)>,
    /// The present pseudo-weathers in the order they started (the key
    /// order of Showdown's `field.pseudoWeather`, which is how their
    /// handlers are collected).
    pub pseudo_order: StackVec<PseudoWeather, 6>,
    /// `weatherState.source` (side, stable id), which the
    /// primal weathers' `onEnd` checks.
    pub weather_source: Option<(SideId, u8)>,
    /// Pseudo-weathers (bit `1 << kind`) whose Showdown effect state has
    /// gained a `target`: that happens the first time `runEvent` runs one of
    /// their handlers, and until then their handlers sort with the
    /// sub-order of a volatile (2) instead of a field effect (5).
    pub pseudo_targeted: u8,
    /// Illusion Level Mod's hint has been given (it is `once`, Showdown's
    /// `battle.hints`).
    pub illusion_level_hint: bool,
}

/// The complete simulation state, minus the RNG and pending choices (which
/// live on [`Battle`](crate::Battle)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BattleState {
    /// `0` before the first turn starts.
    pub turn: u16,
    /// Both sides, indexed by [`SideId::index`].
    pub sides: [SideState; 2],
    /// Field-wide conditions.
    pub field: FieldState,
}

impl BattleState {
    /// One side's state.
    #[inline]
    pub fn side(&self, side: SideId) -> &SideState {
        &self.sides[side.index()]
    }

    /// One side's state, mutably.
    #[inline]
    pub fn side_mut(&mut self, side: SideId) -> &mut SideState {
        &mut self.sides[side.index()]
    }

    /// The active Pokémon at `pos`, or `None` if the slot is empty. See
    /// [`SideState::active`] for a hand-built state.
    #[inline]
    pub fn at(&self, pos: Position) -> Option<&PokemonState> {
        self.side(pos.side).active(pos.slot as usize)
    }

    /// Like `at`, mutably.
    #[inline]
    pub fn at_mut(&mut self, pos: Position) -> Option<&mut PokemonState> {
        self.side_mut(pos.side).active_mut(pos.slot as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::Volatiles;

    #[test]
    fn volatiles_keep_insertion_order() {
        let mut v = Volatiles::default();
        v.set(Volatiles::STALL);
        v.set(Volatiles::FLINCH);
        v.set(Volatiles::MUST_RECHARGE);
        // Re-adding keeps the place (`onRestart`).
        v.set(Volatiles::STALL);
        let order = |v: &Volatiles| v.in_order().collect::<Vec<_>>();
        assert_eq!(order(&v), [Volatiles::STALL, Volatiles::FLINCH, Volatiles::MUST_RECHARGE]);
        // Removed and added again: now the newest.
        v.clear(Volatiles::STALL | Volatiles::PROTECT);
        v.set(Volatiles::STALL);
        assert_eq!(order(&v), [Volatiles::FLINCH, Volatiles::MUST_RECHARGE, Volatiles::STALL]);
        assert!(v.has(Volatiles::FLINCH | Volatiles::TAUNT));
        assert!(!v.has(Volatiles::TAUNT));
    }

    /// A mask of several flags adds each one not yet present, lowest bit
    /// first, and keeps the present ones in place; undefined bits are
    /// ignored.
    #[test]
    fn volatiles_set_a_mask() {
        let mut v = Volatiles::default();
        v.set(Volatiles::FLINCH);
        v.set(Volatiles::TAUNT | Volatiles::FLINCH | Volatiles::PROTECT | !Volatiles::ALL);
        assert_eq!(v.bits(), Volatiles::TAUNT | Volatiles::FLINCH | Volatiles::PROTECT);
        let mut rest = [Volatiles::TAUNT, Volatiles::PROTECT];
        rest.sort_by_key(|f| f.trailing_zeros());
        let order = v.in_order().collect::<Vec<_>>();
        assert_eq!(order, [Volatiles::FLINCH, rest[0], rest[1]]);
    }

    /// The flags `impl Volatiles` defines, read from this file: every
    /// `pub const NAME: u128 = 1 << n`.
    fn defined_flags() -> Vec<u128> {
        let text = include_str!("state.rs");
        let block = text.split("impl Volatiles {").nth(1).unwrap().split("\n}\n").next().unwrap();
        let flags: Vec<u128> = block
            .lines()
            .filter_map(|line| line.trim().strip_prefix("pub const ")?.split_once(": u128 = 1 << "))
            .map(|(_, bit)| 1 << bit.trim_end_matches(';').parse::<u32>().unwrap())
            .collect();
        assert!(flags.len() > 70, "{}", flags.len());
        flags
    }

    /// `Volatiles::ALL` (which sizes the order list) is every defined flag.
    #[test]
    fn all_is_every_defined_flag() {
        let flags = defined_flags();
        assert_eq!(flags.iter().fold(0, |a, f| a | f), Volatiles::ALL);
        assert_eq!(Volatiles::ALL.count_ones() as usize, flags.len(), "two flags share a bit");
    }

    /// Every defined volatile on one Pokémon at once, in any order: no
    /// overflow, and the order kept. In the 96 bytes the `u128`'s alignment
    /// rounds `Volatiles` up to.
    #[test]
    fn volatiles_hold_every_flag() {
        let flags = defined_flags();
        let order = |v: &Volatiles| v.in_order().collect::<Vec<_>>();
        for rotation in [0, 1, 37, flags.len() - 1] {
            let mut want = flags.clone();
            want.rotate_left(rotation);
            want.reverse();
            let mut v = Volatiles::default();
            for &f in &want {
                v.set(f);
            }
            assert_eq!(v.bits(), Volatiles::ALL);
            assert_eq!(order(&v), want);
            // Taking one out and putting it back moves it to the end.
            let moved = want.remove(rotation % want.len());
            v.clear(moved);
            v.set(moved);
            want.push(moved);
            assert_eq!(order(&v), want);
        }
        assert_eq!(core::mem::size_of::<Volatiles>(), 96);
    }
}
