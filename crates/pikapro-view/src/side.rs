//! What the public log says about a side ([`PublicSide`]), its active
//! slots ([`ActiveView`]) and the field ([`PublicField`]).

use pikapro::state::{SideCondition, Terrain, Weather};
use pikapro::util::StackVec;
use pikapro::{MAX_ACTIVE, MAX_MOVES, MAX_TEAM, SideId};
use pikapro_data::{BoostId, MoveId, SpeciesId, Type, TypeSet};

use crate::mon::{HpView, ItemState, MonView, StatusKind};

/// Side conditions, indexed by [`SideCondition`]'s discriminant
/// ([`SideCondition::ALL`]).
pub const SIDE_CONDITIONS: usize = SideCondition::ALL.len();

/// Marks an unused entry of [`PublicSide`]'s id-to-row map.
pub(crate) const NO_ROW: u8 = u8::MAX;

/// A public volatile condition set: bits named by the constants below.
/// Each bit is set by the event that announces the condition and cleared
/// by the one that ends it, or when the Pokémon leaves the field (except
/// what Baton Pass and Shed Tail pass on). Only conditions the log shows
/// are here; the engine's own bookkeeping volatiles (stall counters,
/// Choice lock) never are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PublicVolatiles(pub u64);

impl PublicVolatiles {
    // Announced with `-start` / `-end` (or `-activate`).
    pub const SUBSTITUTE: u64 = 1 << 0;
    pub const CONFUSION: u64 = 1 << 1;
    pub const TAUNT: u64 = 1 << 2;
    pub const ENCORE: u64 = 1 << 3;
    pub const DISABLE: u64 = 1 << 4;
    pub const LEECH_SEED: u64 = 1 << 5;
    pub const YAWN: u64 = 1 << 6;
    pub const PERISH_SONG: u64 = 1 << 7;
    pub const CURSE: u64 = 1 << 8;
    pub const ATTRACT: u64 = 1 << 9;
    pub const TORMENT: u64 = 1 << 10;
    pub const SALT_CURE: u64 = 1 << 11;
    pub const SYRUP_BOMB: u64 = 1 << 12;
    pub const THROAT_CHOP: u64 = 1 << 13;
    pub const HEAL_BLOCK: u64 = 1 << 14;
    pub const IMPRISON: u64 = 1 << 15;
    pub const NO_RETREAT: u64 = 1 << 16;
    /// Can't switch out (Block, Mean Look, Jaw Lock, ...), or Octolocked
    /// ([`ActiveView::octolocked`]: `-start|<pokemon>|move: Octolock`
    /// until its `-end`, which comes at the residuals after its user left;
    /// it traps while the user is on the field, and lowers Defense and
    /// Special Defense each turn).
    pub const TRAPPED: u64 = 1 << 17;
    /// Bound by a partial trap (Bind, Fire Spin, ...).
    pub const PARTIALLY_TRAPPED: u64 = 1 << 18;
    pub const SMACK_DOWN: u64 = 1 << 19;
    pub const MAGNET_RISE: u64 = 1 << 20;
    pub const AQUA_RING: u64 = 1 << 21;
    pub const INGRAIN: u64 = 1 << 22;
    pub const CHARGE: u64 = 1 << 23;
    pub const FOCUS_ENERGY: u64 = 1 << 24;
    pub const DRAGON_CHEER: u64 = 1 << 25;
    pub const TAR_SHOT: u64 = 1 << 26;
    /// Stockpiled (layers in [`ObservedCounters::stockpile`]).
    pub const STOCKPILE: u64 = 1 << 27;
    pub const POWER_TRICK: u64 = 1 << 28;
    pub const POWER_SHIFT: u64 = 1 << 29;
    pub const UPROAR: u64 = 1 << 30;
    pub const GLAIVE_RUSH: u64 = 1 << 31;
    /// Its types were changed (`typechange`: [`ActiveView::types`]).
    pub const TYPE_CHANGE: u64 = 1 << 32;
    /// A type was added (`typeadd`: [`ActiveView::added_type`]).
    pub const TYPE_ADD: u64 = 1 << 33;
    /// Transformed (`-transform`: [`ActiveView::transformed`]).
    pub const TRANSFORMED: u64 = 1 << 34;
    pub const MIMIC: u64 = 1 << 35;
    /// Ability suppressed (`-endability`: Gastro Acid).
    pub const ABILITY_SUPPRESSED: u64 = 1 << 36;
    /// `-start|ability: Flash Fire`.
    pub const FLASH_FIRE: u64 = 1 << 37;
    /// `-activate|ability: Protosynthesis` (or Quark Drive) is up.
    pub const PARADOX_BOOST: u64 = 1 << 38;
    /// Commanding its ally Dondozo (Commander).
    pub const COMMANDING: u64 = 1 << 39;
    // Inferred from move lines, not `-start`.
    /// Must recharge next turn (`-mustrecharge`).
    pub const MUST_RECHARGE: u64 = 1 << 40;
    /// Charging a two-turn move (`-prepare`; [`ActiveView::locked_move`]).
    pub const CHARGING: u64 = 1 << 41;
    /// Its last move was a lock-in's continuation (a `[from] lockedmove`
    /// line: Outrage, Rollout, Uproar, ...; [`ActiveView::locked_move`],
    /// [`ObservedCounters::locked_turns`]). Cleared by a move of its own
    /// choice, a `cant`, fatigue confusion, the end of Uproar or falling
    /// asleep. A lock can also end without a line (a Rollout whose target
    /// fainted, an Outrage cut short), so this is what its last move was,
    /// not a promise of the next.
    pub const LOCKED_MOVE: u64 = 1 << 42;
    /// Focus Punch or Beak Blast charging this turn (`-singleturn`).
    pub const FOCUSING: u64 = 1 << 43;
    // This turn only (cleared at `|turn|`).
    /// Protected this turn (`-singleturn|Protect` and the like).
    pub const PROTECT: u64 = 1 << 44;
    pub const ENDURE: u64 = 1 << 45;
    pub const ROOST: u64 = 1 << 46;
    pub const HELPING_HAND: u64 = 1 << 47;
    /// The center of attention (Follow Me, Rage Powder).
    pub const CENTER_OF_ATTENTION: u64 = 1 << 48;
    pub const ELECTRIFY: u64 = 1 << 49;
    pub const DESTINY_BOND: u64 = 1 << 50;
    pub const LOCK_ON: u64 = 1 << 51;
    // Inferred from a `|move|` line from its own move slot, as Showdown's
    // client does: the move adds the volatile whether or not its boost
    // takes (no line announces it).
    /// Used Minimize (Stomp and the like hit it harder).
    pub const MINIMIZE: u64 = 1 << 52;
    /// Used Defense Curl (Rollout and Ice Ball hit harder).
    pub const DEFENSE_CURL: u64 = 1 << 53;
    // Abilities' conditions.
    /// `-start|<pokemon>|ability: Slow Start` until `-end`.
    pub const SLOW_START: u64 = 1 << 54;
    /// Its Neutralizing Gas is up (`-ability|<pokemon>|Neutralizing Gas`
    /// until `-end|<pokemon>|ability: Neutralizing Gas` or it leaves).
    pub const NEUTRALIZING_GAS: u64 = 1 << 55;
    /// A Dondozo with its Tatsugiri inside (`-activate|<tatsugiri>|ability:
    /// Commander|[of] <dondozo>`). The last bit.
    pub const COMMANDED: u64 = 1 << 56;

    #[inline]
    pub const fn has(self, mask: u64) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub fn set(&mut self, mask: u64) {
        self.0 |= mask;
    }

    #[inline]
    pub fn clear(&mut self, mask: u64) {
        self.0 &= !mask;
    }
}

/// Counters a player can keep from the log for the active Pokémon's
/// volatiles. All are *observed* counts, never the engine's hidden ones
/// (a confusion's or Taunt's real remaining turns are unknown to both
/// players). A duration (`*_turns`) counts the residuals (`|upkeep|`
/// lines) since its volatile started, as the engine counts a `duration`
/// down, so it agrees with the field's and the sides' durations at every
/// decision, a replacement between the residuals and the next `|turn|`
/// included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ObservedCounters {
    /// Turns since confusion started (`|upkeep|` lines seen while
    /// confused).
    pub confusion_turns: u8,
    /// Turns since Taunt started.
    pub taunt_turns: u8,
    /// Turns since Encore started.
    pub encore_turns: u8,
    /// Turns since Disable started.
    pub disable_turns: u8,
    /// Turns since Yawn started (it puts the Pokémon to sleep at the end
    /// of the next turn).
    pub yawn_turns: u8,
    /// The last Perish count shown (`perish3` .. `perish0`); 0 without
    /// [`PublicVolatiles::PERISH_SONG`].
    pub perish: u8,
    /// Stockpile layers (`stockpile1` .. `stockpile3`).
    pub stockpile: u8,
    /// Consecutive turns ending with a successful protection (Protect,
    /// Detect, Endure, Spiky Shield, Wide Guard, ... in a row), which lower
    /// the next one's chance: Showdown's `stall` counter, at most 6 (its
    /// `counterMax`, 3^6). It ends in the residuals of a turn without one,
    /// at once when the next one's roll fails, or when Feint breaks it.
    pub protect_chain: u8,
    /// Turns since Heal Block started.
    pub heal_block_turns: u8,
    /// Turns since Throat Chop started.
    pub throat_chop_turns: u8,
    /// Turns since Magnet Rise started.
    pub magnet_rise_turns: u8,
    /// Turns since the partial trap (Bind, Fire Spin, ...) started.
    pub partial_trap_turns: u8,
    /// Turns since Uproar started.
    pub uproar_turns: u8,
    /// Turns since Slow Start started.
    pub slow_start_turns: u8,
    /// Turns since Syrup Bomb started.
    pub syrup_bomb_turns: u8,
    /// Turns it has been locked into [`ActiveView::locked_move`] by a
    /// rampage (Outrage, Petal Dance, ...): its `[from] lockedmove` lines.
    pub locked_turns: u8,
    /// Fainted allies Supreme Overlord counted (`-start|...|fallen<n>`).
    pub fallen: u8,
    /// Upkeeps since Lock-On or Mind Reader took aim (it ends at the
    /// second).
    pub lock_on_turns: u8,
    /// Confusion's checks since it started (`-activate|<pokemon>|confusion`:
    /// one per attempt to move, Dancer's copies and Instruct's too). What
    /// ends confusion is its checks, not turns: it lasts 2 to 5 of them
    /// (3 to 5 after Axe Kick; `confusion.onStart`, `time`), the check that
    /// ends it showing `-end` instead.
    pub confusion_checks: u8,
    /// Taunt lasts 4 turns, not 3: it started on a Pokémon on the field
    /// since an earlier turn with no move still to come this turn
    /// (`taunt.condition.onStart`: `if (target.activeTurns &&
    /// !this.queue.willMove(target)) this.effectState.duration++`). Read
    /// from the turn as it went when it started.
    pub taunt_long: bool,
    /// Encore lasts 4 turns, not 3: no move still to come this turn when
    /// it started (`encore.condition.onStart`: `if (!this.queue.willMove(
    /// target)) this.effectState.duration++`; Champions' override keeps it).
    pub encore_long: bool,
    /// Disable lasts 4 turns, not 5: a move still to come this turn, or
    /// the Pokémon moving now (Cursed Body), when it started
    /// (`disable.condition.onStart`: `if (this.queue.willMove(pokemon) ||
    /// pokemon === this.activePokemon && this.activeMove &&
    /// !this.activeMove.isExternal) this.effectState.duration--`).
    pub disable_short: bool,
}

/// Where a transformed Pokémon took its form from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TransformView {
    /// The side and row of the Pokémon it transformed into.
    pub side: SideId,
    pub row: u8,
    /// That Pokémon's species at the time (`-transform` names it).
    pub species: SpeciesId,
}

/// What the log says about one active slot: the Pokémon in it (its row)
/// and everything that lasts only while it stays in (boosts, volatiles).
/// Reset when a new Pokémon enters the slot, except what Baton Pass or
/// Shed Tail passes on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ActiveView {
    /// The row (index into [`PublicSide::mons`]) of the Pokémon in this
    /// slot, `None` before the slot was first filled. A fainted Pokémon
    /// keeps its slot until it is replaced.
    pub row: Option<u8>,
    /// Stat stages by [`BoostId`] (`atk`, `def`, `spa`, `spd`, `spe`,
    /// `accuracy`, `evasion`), each in `-6..=6`, from `-boost`, `-unboost`,
    /// `-setboost`, `-clearboost`, `-copyboost`, `-swapboost`,
    /// `-invertboost` and their `[from]` forms. An amount of 0 (at the cap)
    /// changes nothing.
    pub boosts: [i8; 7],
    pub volatiles: PublicVolatiles,
    pub counters: ObservedCounters,
    /// Its types while changed (`-start|typechange`, Soak, Reflect Type,
    /// Protean, Color Change, ...). `None`: its species' types, or its
    /// Tera type once Terastallized.
    pub types: Option<TypeSet>,
    /// The type added by Forest's Curse or Trick-or-Treat (`typeadd`).
    pub added_type: Option<Type>,
    /// Transformed (Transform, Imposter): into whom.
    pub transformed: Option<TransformView>,
    /// The move Disable disabled.
    pub disabled_move: Option<MoveId>,
    /// The move Encore locked it into.
    pub encore_move: Option<MoveId>,
    /// The move it is locked into or charging ([`PublicVolatiles::LOCKED_MOVE`],
    /// [`PublicVolatiles::CHARGING`]).
    pub locked_move: Option<MoveId>,
    /// The last `|move|` line from its own move slots or a lock-in (not a
    /// move it called: after Sleep Talk it is Sleep Talk), close to what
    /// Encore and Disable read (Showdown's `lastMove`, which some failed
    /// moves also clear).
    pub last_move: Option<MoveId>,
    /// Turns it has been on the field: 0 on the turn it entered, then +1
    /// at each `|turn|` (Fake Out works while 0 or 1 depending on when it
    /// entered: see `moved_this_turn`).
    pub turns_active: u8,
    /// It used a move, or was stopped from using one (`cant`), this turn.
    pub moved_this_turn: bool,
    /// The move a Choice item locks it into: its last move as
    /// [`ActiveView::last_move`] has it but for Struggle, on this stint and
    /// since it last received an item (`choicelock.effectState.move`,
    /// Showdown's `data/conditions.js`: Struggle doesn't restart a lock,
    /// `addVolatile` keeps the one there, and `onBeforeMove` lets it
    /// through, so a Choice-locked Pokémon out of PP stays locked; a lock
    /// Struggle starts itself is gone by the next turn, `onDisableMove`).
    pub choice_move: Option<MoveId>,
    /// It received the item it holds from another Pokémon (Trick, Thief,
    /// ...: [`MonView::own_item`](crate::MonView::own_item)) on this stint
    /// and used no move since: a Choice item's `onStart` ended any Choice
    /// lock, and only its next move locks it (`choicescarf.onStart`,
    /// `onModifyMove` and `choicelock` in Showdown's `data/items.js` and
    /// `data/conditions.js`).
    pub item_received: bool,
    /// Moves it has while transformed: the revealed moves of the Pokémon it
    /// transformed into, as they were, and the ones it used since. They
    /// are not its own ([`MonView::moves`] leaves them out).
    pub copied_moves: StackVec<MoveId, MAX_MOVES>,
    /// The move Mimic copied into a move slot ([`PublicVolatiles::MIMIC`]).
    pub mimic_move: Option<MoveId>,
    /// The stat Protosynthesis or Quark Drive boosts
    /// ([`PublicVolatiles::PARADOX_BOOST`]).
    pub boosted_stat: Option<BoostId>,
    /// Its ability changed since it came in (Skill Swap, Trace, Mummy,
    /// Transform, ...): [`MonView::ability`] is not its own
    /// ([`MonView::base_ability`]) until it leaves the field.
    pub ability_changed: bool,
    /// Who keeps it from switching out ([`PublicVolatiles::TRAPPED`]): the
    /// active slot of the Block, Mean Look or Jaw Lock user, which frees it
    /// by leaving the field.
    pub trapped_by: Option<(SideId, u8)>,
    /// Speed Swap exchanged its Speed stat with another Pokémon's
    /// (`-activate|<user>|move: Speed Swap|[of] <target>`, both marked): its
    /// Speed stat isn't its own until it leaves the field (Baton Pass doesn't
    /// pass it).
    pub speed_swapped: bool,
    /// Guard Split or Power Split averaged its stored stats with another
    /// Pokémon's (`-activate|<user>|move: Guard Split|[of] <target>`, both
    /// marked; Showdown's `guardsplit` / `powersplit` `onHit`): its Defense
    /// and Special Defense, or Attack and Special Attack, aren't its own
    /// until it leaves the field (Baton Pass doesn't pass them).
    pub stats_split: bool,
    /// Octolocked (`-start|<pokemon>|move: Octolock` until its `-end`),
    /// which also sets [`PublicVolatiles::TRAPPED`]: it holds the
    /// Pokémon in whoever else released it, and Baton Pass passes it.
    pub octolocked: bool,
    /// The slot its Leech Seed drains into ([`PublicVolatiles::LEECH_SEED`]):
    /// the seeder's side and active slot when it was seeded (the user of
    /// the `|move|` line, a Magic Bounce user for a move it sent back), as
    /// Showdown's `sourceSlot` keeps it: whoever stands there now gets the
    /// HP, and Ally Switch doesn't move it.
    pub leech_seed_slot: Option<(SideId, u8)>,
    /// Who holds it in a partial trap ([`PublicVolatiles::PARTIALLY_TRAPPED`]),
    /// and with which move: the `[of]` Pokémon of the trap's `-activate`
    /// line, as its side and row (Showdown's `effectState.source`: the
    /// trap ends at a residual once it isn't on the field).
    pub partial_trap: Option<(SideId, u8, MoveId)>,
    /// It used a protection move this turn and the log has not shown it
    /// working yet (for [`ObservedCounters::protect_chain`]). The
    /// tracker's bookkeeping, not a fact.
    pub(crate) protect_pending: bool,
    /// Its protection worked this turn (the tracker's bookkeeping).
    pub(crate) protected: bool,
    /// The HP last shown for the Pokémon in this slot. It equals its
    /// record's ([`MonView::hp`]) unless an Illusion shows the same
    /// Pokémon in both slots, when the record mixes the two.
    pub hp: HpView,
    /// The status last shown for the Pokémon in this slot (see
    /// [`ActiveView::hp`]).
    pub status: StatusKind,
}

impl ActiveView {
    /// The stage of `stat`.
    #[inline]
    pub fn boost(&self, stat: BoostId) -> i8 {
        self.boosts[stat as usize]
    }
}

/// What a slot's Pokémon did as its action in a turn ([`TurnRecord::action`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TurnAction {
    /// Nothing (yet): no action shown.
    #[default]
    None = 0,
    /// Used a move (`|move|` from its own move slots or a lock-in; a move
    /// it called is [`TurnRecord::move_id`], with the caller in
    /// [`TurnRecord::called_by`]).
    Move = 1,
    /// Switched in ([`TurnRecord::forced`] if the switch wasn't its slot's
    /// chosen action).
    Switch = 2,
    /// Couldn't move (`cant`, or hurt itself in confusion:
    /// [`TurnRecord::cant`]).
    Cant = 3,
}

/// How a move turned out, as the log showed it ([`TurnRecord::outcome`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum MoveOutcome {
    /// No move.
    #[default]
    None = 0,
    /// It damaged a target, or no line said otherwise.
    Hit = 1,
    /// `-miss`.
    Miss = 2,
    /// `-fail`, `-notarget`, or stopped after its `|move|` line (Damp).
    Fail = 3,
    /// Blocked by a protection: `-activate|<target>|move: Protect` (and
    /// the other protections), `move: Wide Guard`, `move: Quick Guard`,
    /// or Psychic Terrain stopping a priority move (`move: Psychic
    /// Terrain`).
    Protected = 4,
    /// `-immune` (a type or an ability).
    Immune = 5,
}

/// Why a Pokémon couldn't move ([`TurnRecord::cant`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum CantKind {
    #[default]
    None = 0,
    Flinch = 1,
    Recharge = 2,
    Paralysis = 3,
    Sleep = 4,
    Freeze = 5,
    /// Hurt itself in its confusion.
    Confusion = 6,
    Attract = 7,
    Taunt = 8,
    Disable = 9,
    /// Anything else (Gravity, Heal Block, Throat Chop, Imprison, Truant,
    /// no PP, a lost Focus Punch, ...).
    Other = 10,
}

/// Whom a move was aimed at ([`TurnRecord::target`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TurnTarget {
    /// No target shown: the `|move|` line names none, or `[still]` hides
    /// it.
    #[default]
    None,
    /// The slot the `|move|` line names as the target, as it names it (the
    /// user itself for a move on itself). Field and side moves name one
    /// too: Tailwind or Wide Guard its user, Sticky Web a foe.
    Slot { side: SideId, slot: u8 },
    /// Every adjacent foe, or every adjacent Pokémon: a spread move in
    /// doubles (`allAdjacentFoes`, `allAdjacent`), or one the line shows
    /// hitting several, foes among them (`[spread]`: Expanding Force in
    /// Psychic Terrain; not Howl, whose `[spread]` names the user and its
    /// ally).
    Spread,
}

/// Bits of [`TurnRecord::taken_by`]: what the HP a Pokémon lost in a turn
/// came from.
pub mod taken_by {
    /// Another Pokémon's move (a foe's, an ally's spread move, Future
    /// Sight).
    pub const MOVE: u8 = 1 << 0;
    /// A condition: a status, the weather, an entry hazard, a volatile
    /// (Leech Seed, Salt Cure, a partial trap, Curse, ...).
    pub const RESIDUAL: u8 = 1 << 1;
    /// Its own move's cost: recoil, crash damage, Steel Beam, Struggle,
    /// and the HP it pays for its move (Substitute, Belly Drum, a Ghost's
    /// Curse, Fillet Away, Clangorous Soul, Pain Split).
    pub const RECOIL: u8 = 1 << 2;
    /// An item (Life Orb, Rocky Helmet, Sticky Barb, Black Sludge, ...).
    pub const ITEM: u8 = 1 << 3;
    /// Anything else: an ability (Rough Skin, Solar Power, ...), its own
    /// confusion, a protection (Spiky Shield), a busted Disguise.
    pub const OTHER: u8 = 1 << 4;
}

/// What one active slot did in a turn, as the log showed it: its action,
/// the move's target and outcome, the damage it dealt and took. Kept per
/// slot and side from one `|turn|` to the next ([`PublicSide::this_turn`]),
/// then kept as [`PublicSide::last_turn`] for the whole next turn. A
/// Pokémon entering the slot starts a new record (and clears the last
/// turn's, which was another Pokémon's); Ally Switch swaps them with the
/// Pokémon. Illusion doesn't matter: records are the slot's.
///
/// Only the slot's own action counts as its move: a Dancer copy, a move
/// Magic Bounce sent back, or a second move in the turn (Instruct) sets
/// nothing here. HP is in percent points of the Pokémon's maximum as the
/// public record shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct TurnRecord {
    pub action: TurnAction,
    /// Its place among the turn's actions (1 for the first), counting
    /// both sides' moves, `cant`s and chosen switches; 0 without an
    /// action, or for a forced switch-in.
    pub order: u8,
    /// A switch-in that wasn't the slot's chosen action: a lead, a
    /// replacement for a fainted Pokémon, one after a U-turn, Baton Pass,
    /// Eject Button, Emergency Exit or Revival Blessing, or a drag (Roar,
    /// Dragon Tail, Red Card).
    pub forced: bool,
    /// The move it used (the called one after Metronome, Sleep Talk, ...).
    pub move_id: Option<MoveId>,
    /// The move that called [`TurnRecord::move_id`].
    pub called_by: Option<MoveId>,
    pub target: TurnTarget,
    pub outcome: MoveOutcome,
    /// A critical hit on any target.
    pub crit: bool,
    /// Per slot of the user's foes: the effectiveness the log announced
    /// against the Pokémon there: `-supereffective` +1, `-resisted` -1
    /// (Champions logs the level: ±2 for 4× and ¼×), `-immune` -3; 0 if
    /// none was announced.
    pub effect: [i8; MAX_ACTIVE],
    /// Hits of a multi-hit move (`-hitcount`), else 1 if it damaged
    /// something, else 0.
    pub hits: u8,
    /// Per slot of the user's foes: the HP its move took from the Pokémon
    /// there.
    pub dealt: [u8; MAX_ACTIVE],
    /// The HP it lost in the turn (heals don't count), whatever the cause.
    pub taken: u8,
    /// What it lost HP to ([`taken_by`] bits).
    pub taken_by: u8,
    pub cant: CantKind,
}

impl TurnRecord {
    /// It took an action of its own: a move, a `cant` or a chosen switch.
    pub fn acted(&self) -> bool {
        match self.action {
            TurnAction::None => false,
            TurnAction::Switch => !self.forced,
            TurnAction::Move | TurnAction::Cant => true,
        }
    }
}

/// A slot's pending healing wishes (Showdown's slot conditions), from the
/// `|move|` line that made them until the line that resolves them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SlotConditions {
    /// Wish: the upkeeps since it was made (it heals at the end of the
    /// turn after, then ends, silently if the Pokémon is at full HP).
    pub wish: Option<u8>,
    /// Healing Wish waits for a Pokémon that needs it to come in.
    pub healing_wish: bool,
    /// Lunar Dance, the same (PP too).
    pub lunar_dance: bool,
}

impl SlotConditions {
    /// Upkeeps after which a Wish has resolved.
    pub const WISH_TURNS: u8 = 2;

    /// The ends of turn left until the Wish resolves (this turn's
    /// included), if one is pending.
    pub fn wish_turns_left(&self) -> Option<u8> {
        self.wish.map(|n| Self::WISH_TURNS.saturating_sub(n))
    }
}

/// A side condition as the log shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ConditionView {
    /// 0: not up. Entry hazards: layers (Spikes 1..=3, Toxic Spikes 1..=2,
    /// Stealth Rock and Sticky Web 1). Other conditions: 1 while up.
    pub layers: u8,
    /// End-of-turn upkeeps since it started (0 on the turn it was set).
    /// The remaining duration is not shown: a Light Clay would change it.
    pub turns: u8,
}

/// What the public log says about one side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PublicSide {
    /// One record per Pokémon seen, by row. With team preview the rows are
    /// the `|poke|` order (the registration order); without it, the order
    /// of first appearance, so the true team order doesn't leak. A row
    /// never moves once assigned.
    pub mons: StackVec<MonView, MAX_TEAM>,
    /// The active slots.
    pub slots: [ActiveView; MAX_ACTIVE],
    /// Side conditions, by [`SideCondition`] discriminant.
    pub conditions: [ConditionView; SIDE_CONDITIONS],
    /// Pokémon it plays with (`|teamsize|`: the brought ones with team
    /// preview), 0 before the battle starts.
    pub team_size: u8,
    /// Its Pokémon that have fainted (`|faint|` lines, minus revivals).
    pub fainted: u8,
    /// It has Terastallized a Pokémon this battle.
    pub tera_used: bool,
    /// It has Mega Evolved a Pokémon this battle.
    pub mega_used: bool,
    /// Rows in the order they first entered the field.
    pub reveal_order: StackVec<u8, MAX_TEAM>,
    /// Each slot's record as it was just before its Pokémon entered, taken
    /// before the switch-in line updates it (the disguise's own last-seen
    /// state under Illusion). When an Illusion breaks (`|replace|`), what
    /// the disguise's record learnt since then belongs to the Pokémon
    /// behind it: the tracker moves it there and restores the disguise's
    /// record from here, as poke-env's `end_illusion` does.
    pub entered: [MonView; MAX_ACTIVE],
    /// Whether the slot's Pokémon was new to [`PublicSide::reveal_order`]
    /// when it entered (undone with the rest when an Illusion breaks).
    pub entered_first: [bool; MAX_ACTIVE],
    /// Future Sight or Doom Desire aimed at each of this side's slots
    /// (`-start|<user>|move: Future Sight`, the target from its `|move|`
    /// line), until it lands (`-end`).
    pub future: [FutureView; MAX_ACTIVE],
    /// Wish, Healing Wish and Lunar Dance waiting in each slot.
    pub slot_conditions: [SlotConditions; MAX_ACTIVE],
    /// What each slot did in the previous turn (the record of
    /// [`PublicSide::this_turn`] at the last `|turn|`).
    pub last_turn: [TurnRecord; MAX_ACTIVE],
    /// What each slot has done so far in the turn being played (at a
    /// decision between turns: nothing but the switch-ins since the last
    /// `|turn|`, if any).
    pub this_turn: [TurnRecord; MAX_ACTIVE],
    /// Row of each Pokémon by the `id` events name it with
    /// ([`pikapro::event::Actor::id`]: its shown identity, the disguise's
    /// under Illusion); [`NO_ROW`] if not seen. Private: for a foe it
    /// would expose the registration order.
    pub(crate) row_of_id: [u8; MAX_TEAM],
    /// Who started Reflect, Light Screen and Aurora Veil (in that order)
    /// on this side, as the log names them: the row of the move's user, on
    /// this side ([`PublicSide::screen_setter`]). Its item sets the
    /// duration (Light Clay), so a reader that hypothesises items can
    /// derive it. `None` when the log didn't say, after Court Change, and
    /// while the screen is down.
    pub screen_setters: [Option<u8>; 3],
    /// Whether each screen's setter (in [`PublicSide::screen_setters`]'
    /// order) had lost its item when it set the screen (the log showed
    /// `-enditem` before): Light Clay lost then didn't extend it, though the
    /// item shows the same (`Gone`) later.
    pub screen_setter_lost: [bool; 3],
    /// Each screen's setter's item (in [`PublicSide::screen_setters`]'
    /// order) as the log showed it when the setter set the screen: an item
    /// it lost, traded or received since doesn't change the duration
    /// (Light Clay is read as the screen starts).
    pub screen_setter_items: [ItemState; 3],
}

/// The side conditions [`PublicSide::screen_setters`] follows.
pub const SCREENS: [SideCondition; 3] = [SideCondition::Reflect, SideCondition::LightScreen, SideCondition::AuroraVeil];

impl Default for PublicSide {
    fn default() -> PublicSide {
        PublicSide {
            mons: StackVec::new(),
            slots: [ActiveView::default(); MAX_ACTIVE],
            conditions: [ConditionView::default(); SIDE_CONDITIONS],
            team_size: 0,
            fainted: 0,
            tera_used: false,
            mega_used: false,
            reveal_order: StackVec::new(),
            entered: [MonView::default(); MAX_ACTIVE],
            entered_first: [false; MAX_ACTIVE],
            future: [FutureView::default(); MAX_ACTIVE],
            slot_conditions: [SlotConditions::default(); MAX_ACTIVE],
            last_turn: [TurnRecord::default(); MAX_ACTIVE],
            this_turn: [TurnRecord::default(); MAX_ACTIVE],
            row_of_id: [NO_ROW; MAX_TEAM],
            screen_setters: [None; 3],
            screen_setter_lost: [false; 3],
            screen_setter_items: [ItemState::Unknown; 3],
        }
    }
}

impl PublicSide {
    /// The record in `row`.
    pub fn mon(&self, row: u8) -> Option<&MonView> {
        self.mons.get(row as usize)
    }

    /// The Pokémon in active slot `slot`, with its row.
    pub fn active(&self, slot: usize) -> Option<(u8, &MonView)> {
        let row = self.slots.get(slot)?.row?;
        Some((row, self.mons.get(row as usize)?))
    }

    /// The condition `c`.
    pub fn condition(&self, c: SideCondition) -> ConditionView {
        self.conditions[c as usize]
    }

    /// The row (on this side) of the Pokémon that started screen `c`
    /// ([`SCREENS`]), if the log named it; `None` for other conditions.
    pub fn screen_setter(&self, c: SideCondition) -> Option<u8> {
        SCREENS.iter().position(|&s| s == c).and_then(|i| self.screen_setters[i])
    }

    /// Whether screen `c`'s setter had lost its item when it set it
    /// ([`PublicSide::screen_setter_lost`]); false for other conditions.
    pub fn screen_setter_lost(&self, c: SideCondition) -> bool {
        SCREENS.iter().position(|&s| s == c).is_some_and(|i| self.screen_setter_lost[i])
    }

    /// Screen `c`'s setter's item as the log showed it when it set the
    /// screen ([`PublicSide::screen_setter_items`]); `Unknown` for other
    /// conditions.
    pub fn screen_setter_item(&self, c: SideCondition) -> ItemState {
        SCREENS.iter().position(|&s| s == c).map_or(ItemState::Unknown, |i| self.screen_setter_items[i])
    }

    /// The row events name with `id`, if seen.
    pub(crate) fn row_of(&self, id: u8) -> Option<u8> {
        self.row_of_id.get(id as usize).copied().filter(|&r| r != NO_ROW)
    }

    /// The `id` events name the Pokémon in `row` with (the inverse of
    /// [`PublicSide::row_of`]).
    pub(crate) fn id_of(&self, row: u8) -> Option<u8> {
        self.row_of_id.iter().position(|&r| r == row).map(|id| id as u8)
    }

    /// Its Pokémon still standing as far as the log shows: `team_size`
    /// minus the fainted ones.
    pub fn standing(&self) -> u8 {
        self.team_size.saturating_sub(self.fainted)
    }
}

/// A foreseen attack (Future Sight, Doom Desire) aimed at a slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct FutureView {
    /// The move, `None` when nothing is foreseen.
    pub move_id: Option<MoveId>,
    /// End-of-turn upkeeps since it was foreseen (it lands at the end of
    /// the second turn after the one it was used on).
    pub turns: u8,
}

impl FutureView {
    /// Upkeeps after which a foreseen attack has landed.
    pub const TURNS: u8 = 3;

    /// The ends of turn left until it lands (this turn's included), if one
    /// is foreseen. The duration is fixed, so this is public.
    pub fn turns_left(&self) -> Option<u8> {
        self.move_id.map(|_| Self::TURNS.saturating_sub(self.turns))
    }
}

/// The field as the log shows it. Durations are turns *elapsed* (the end
/// of turn upkeeps seen since it started), never turns remaining, which
/// would reveal Damp Rock, Light Clay or Terrain Extender.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PublicField {
    pub weather: Option<Weather>,
    /// Upkeeps (`-weather|X|[upkeep]`) since the weather started.
    pub weather_turns: u8,
    /// Who started the weather, as the log names them: the side and row of
    /// the ability's holder (`[from] ability: X|[of] <holder>`) or of the
    /// move's user. Its item sets the duration (Damp Rock and the like).
    pub weather_setter: Option<(SideId, u8)>,
    /// Whether the weather's setter had lost its item when it started the
    /// weather (an extender lost by then didn't extend it).
    pub weather_setter_lost: bool,
    /// The weather's setter's item as the log showed it when it started
    /// the weather (an extender traded away since still extended it).
    pub weather_setter_item: ItemState,
    pub terrain: Option<Terrain>,
    /// Upkeeps since the terrain started.
    pub terrain_turns: u8,
    /// Who started the terrain, as for [`PublicField::weather_setter`]
    /// (Terrain Extender).
    pub terrain_setter: Option<(SideId, u8)>,
    /// Whether the terrain's setter had lost its item when it started the
    /// terrain, as for [`PublicField::weather_setter_lost`].
    pub terrain_setter_lost: bool,
    /// The terrain's setter's item when it started the terrain, as for
    /// [`PublicField::weather_setter_item`].
    pub terrain_setter_item: ItemState,
    /// Trick Room: `Some(turns elapsed)` while up.
    pub trick_room: Option<u8>,
    pub gravity: Option<u8>,
    pub magic_room: Option<u8>,
    pub wonder_room: Option<u8>,
    /// Fairy Lock this turn or the next (`-fieldactivate|move: Fairy Lock`).
    pub fairy_lock: bool,
    /// Upkeeps since Fairy Lock started (it ends at the second).
    pub fairy_lock_turns: u8,
}

impl PublicField {
    /// Turns elapsed on the pseudo-weather `effect`, `None` if it is not up
    /// (terrains are in [`PublicField::terrain`]).
    pub fn pseudo_weather(&self, effect: pikapro::event::FieldEffect) -> Option<u8> {
        use pikapro::event::FieldEffect;
        match effect {
            FieldEffect::Terrain(t) => (self.terrain == Some(t)).then_some(self.terrain_turns),
            FieldEffect::TrickRoom => self.trick_room,
            FieldEffect::Gravity => self.gravity,
            FieldEffect::MagicRoom => self.magic_room,
            FieldEffect::WonderRoom => self.wonder_room,
        }
    }
}
