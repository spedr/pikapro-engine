//! Structured battle events.
//!
//! The engine emits [`BattleEvent`]s; it never formats text. `pikapro-protocol`
//! renders them as Showdown protocol lines, and observation encoders can
//! consume them directly. Events are self-contained (they carry the species
//! and HP they refer to) so they can be rendered after the state has moved on.
//!
//! Events carry *full* information (exact HP). Hiding information from a
//! player (HP as a percentage, unrevealed sets) is the renderer's job.

use pikapro_data::{AbilityId, BoostId, ItemId, MoveId, SpeciesId, Type, TypeSet};

use crate::state::{Position, SideCondition, SideId, Status, Terrain, Weather};
use crate::team::Gender;

/// A Pokémon named in an event: where it is and what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Actor {
    /// Its side and team position (the field slot while it is active).
    pub pos: Position,
    /// The species it shows: its own, or under Illusion its disguise's.
    pub species: SpeciesId,
    /// Whether it still occupies `pos`. A fainted Pokémon that has been
    /// removed from the field is named without a slot (`p2: Garchomp`).
    pub active: bool,
    /// The [`PokemonState::id`](crate::PokemonState::id) of the
    /// Pokémon whose name is shown: its own, or under Illusion its
    /// disguise's (Showdown's `pokemon.toString()` uses
    /// `illusion.fullname`). Renderers look nicknames up by it.
    pub id: u8,
}

/// What dealt damage: the `-damage` line's `[from] ...`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DamageSource {
    /// A move's direct damage: no `[from]`.
    Move,
    /// Burn or poison damage (`[from] brn`, `[from] psn`; toxic is `psn`
    /// too).
    Status(Status),
    /// Struggle's recoil (`[from] recoil`).
    Recoil,
    /// A move's `recoil` (`[from] Recoil`, Double-Edge).
    MoveRecoil,
    /// Hitting itself in confusion (`[from] confusion`).
    Confusion,
    /// Weather chip damage (`[from] Sandstorm`).
    Weather(Weather),
    /// A move's lingering effect held by `of` (Spiky Shield):
    /// `[from] <move>|[of] <of>`.
    MoveEffect {
        /// The move whose effect dealt the damage.
        move_id: MoveId,
        /// The Pokémon holding the effect (the Spiky Shield user).
        of: Actor,
    },
    /// A move's own effect on its user, named by the move's ID (`[from]
    /// highjumpkick`): crash damage, Steel Beam's recoil.
    MoveCondition(MoveId),
    /// An ability or item (`[from] item: Life Orb`, `[from] ability:
    /// Rough Skin|[of] <source>`).
    Effect(Cause),
    /// Damage whose effect is a species (`[from] pokemon:
    /// Mimikyu-Busted`: a busted Disguise).
    Species(SpeciesId),
}

/// A volatile condition that is announced in the log (`-start`, `-end`,
/// `-activate`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Volatile {
    /// Confusion (`confusion`).
    Confusion,
    /// Taunt (`move: Taunt`).
    Taunt,
    /// Encore (`Encore`).
    Encore,
    /// Disable (`Disable`).
    Disable,
}

/// A volatile from a stat, HP or type manipulating move that is announced
/// in the log (`-start`, `-end`) or heals (`-heal|...|[from] <name>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ManipulationVolatile {
    /// Aqua Ring (`Aqua Ring`).
    AquaRing,
    /// Ingrain (`move: Ingrain`).
    Ingrain,
    /// Charge (`Charge`).
    Charge,
    /// Focus Energy (`move: Focus Energy`).
    FocusEnergy,
    /// Dragon Cheer (`move: Dragon Cheer`).
    DragonCheer,
    /// Tar Shot (`Tar Shot`).
    TarShot,
    /// Stockpile, with its layer count (`-start|...|stockpile2`).
    Stockpile(u8),
    /// Power Trick (`Power Trick`).
    PowerTrick,
    /// Power Shift (`Power Shift`).
    PowerShift,
}

/// Why a Pokémon can't move (`|cant|`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CantReason {
    /// Flinched (`flinch`).
    Flinch,
    /// Recharging after Hyper Beam and the like.
    Recharge,
    /// Taunted, trying to use this status move.
    Taunt(MoveId),
    /// Trying to use this disabled move.
    Disable(MoveId),
}

/// What made a Pokémon use a move (the `|move|` line's `[from] ...`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MoveFrom {
    /// Locked into it (Outrage, Uproar, the second turn of Fly, ...):
    /// `[from] lockedmove`.
    LockedMove,
    /// Used on behalf of this move (`[from] move: <move>`): a called move
    /// (Sleep Talk, Copycat, Metronome), a Round moved up by Round, the
    /// ally's pledge a combination was made with.
    Move(MoveId),
    /// Reflected or copied by this ability (`[from] ability: Magic
    /// Bounce`; `[from] ability: Dancer`).
    Ability(AbilityId),
}

/// What restored HP (rendered as `[from] ...`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HealSource {
    /// The move's own healing (Recover): no `[from]`.
    Move,
    /// Draining damage from `of` (Giga Drain).
    Drain {
        /// The Pokémon drained (`[from] drain|[of] <of>`).
        of: Actor,
    },
    /// End-of-turn terrain healing (`[from] Grassy Terrain`).
    Terrain(Terrain),
    /// An ability or item (`[from] item: Leftovers`, `[from] ability:
    /// Water Absorb|[of] <source>`).
    Effect(Cause),
}

/// A field-wide effect started by `-fieldstart` and ended by `-fieldend`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldEffect {
    /// A terrain (`move: Electric Terrain`, ...).
    Terrain(Terrain),
    /// Trick Room (`move: Trick Room`).
    TrickRoom,
    /// Gravity (`move: Gravity`).
    Gravity,
    /// Magic Room (`move: Magic Room`).
    MagicRoom,
    /// Wonder Room (`move: Wonder Room`).
    WonderRoom,
}

/// A Substitute, trapping or residual volatile, or a field effect of the
/// same kind (Fairy Lock), named in its events
/// ([`BattleEvent::EffectStart`] and the other `Effect*` events). The
/// renderer knows how Showdown spells each one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VolatileEffect {
    /// Substitute.
    Substitute,
    /// Trapped by this move (Bind, Wrap, Fire Spin, ...).
    PartiallyTrapped(MoveId),
    /// Can't switch out (Block, Mean Look, Spirit Shackle, Jaw Lock).
    Trapped,
    /// No Retreat.
    NoRetreat,
    /// Leech Seed.
    LeechSeed,
    /// Yawn's drowsiness.
    Yawn,
    /// Ghost-type Curse.
    Curse,
    /// Perish Song.
    PerishSong,
    /// Destiny Bond.
    DestinyBond,
    /// Infatuation (Attract, Cute Charm).
    Attract,
    /// Torment.
    Torment,
    /// Salt Cure.
    SaltCure,
    /// Syrup Bomb's Speed drops.
    SyrupBomb,
    /// Throat Chop's sound-move block.
    ThroatChop,
    /// Heal Block.
    HealBlock,
    /// Imprison.
    Imprison,
    /// Glaive Rush's vulnerability after use.
    GlaiveRush,
    /// Endure.
    Endure,
    /// Smack Down's grounding.
    SmackDown,
    /// Magnet Rise.
    MagnetRise,
    /// Fairy Lock (field-wide).
    FairyLock,
    /// Octolock (Champions): `-start|<target>|move: Octolock|[of]
    /// <source>`, `-end|<target>|Octolock|[partiallytrapped]|[silent]`.
    Octolock,
}

/// An ability, item or move as a line names it (Showdown's
/// `effect.fullname`: `ability: Intimidate`, `item: Leftovers`, `move:
/// Knock Off`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EffectName {
    /// An ability (`ability: <name>`).
    Ability(AbilityId),
    /// An item (`item: <name>`).
    Item(ItemId),
    /// A move (`move: <name>`).
    Move(MoveId),
}

/// Showdown's `[from] <effect>` suffix, with `[of] <pokemon>` when
/// another Pokémon is involved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cause {
    /// The ability, item or move responsible (`[from] <effect>`).
    pub effect: EffectName,
    /// The other Pokémon involved (`[of] <of>`), usually the effect's
    /// holder or user.
    pub of: Option<Actor>,
}

impl Cause {
    /// `[from] ability: X`.
    pub fn ability(id: AbilityId) -> Cause {
        Cause { effect: EffectName::Ability(id), of: None }
    }

    /// `[from] item: X`.
    pub fn item(id: ItemId) -> Cause {
        Cause { effect: EffectName::Item(id), of: None }
    }

    /// `[from] move: X`.
    pub fn move_(id: MoveId) -> Cause {
        Cause { effect: EffectName::Move(id), of: None }
    }

    /// With `[of] <of>`.
    pub fn of(self, of: Actor) -> Cause {
        Cause { of: Some(of), ..self }
    }
}

/// An extra argument of a line (`-activate|p|item: Leppa Berry|<move>`,
/// `-block|p|ability: X|<move>`, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LineArg {
    /// A move's name.
    Move(MoveId),
    /// An item's name.
    Item(ItemId),
    /// An ability's name.
    Ability(AbilityId),
    /// A species' name.
    Species(SpeciesId),
    /// A type's name.
    Type(Type),
    /// A Pokémon (`p1a: <name>`).
    Actor(Actor),
    /// A literal (`[consumed]`, `[silent]`, `confusion`, `unboost`, ...).
    Text(&'static str),
}

/// What an `-ability` line says after the ability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AbilityNote {
    /// Nothing.
    None,
    /// `|boost`: the ability changes stats next (Intimidate).
    Boost,
    /// The ability it replaced (`setAbility`: `-ability|p|NEW|OLD|[from]
    /// ...`).
    Replaced(AbilityId),
}

/// How an item ended (`-enditem`'s annotations).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemEndNote {
    /// No annotation.
    None,
    /// `[eat]`: a berry was eaten.
    Eat,
    /// `[weaken]`: a resist berry took effect.
    Weaken,
    /// `[from] gem` without the move (a log from a server that doesn't name
    /// it); the engine writes [`ItemEndNote::GemMove`].
    Gem,
    /// `[silent]` (before the cause: Trick, Thief).
    Silent,
    /// `[from] stealeat|[move] <move>` (Bug Bite, Pluck).
    StealEat(MoveId),
    /// `[from] gem|[move] <move>`: a Gem used up by the move it powers
    /// (`Pokemon.useItem`).
    GemMove(MoveId),
}

/// The annotation on a [`BattleEvent::MoveFail`] line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FailNote {
    /// No annotation.
    None,
    /// `[forme]`: the wrong forme used the move (Hoopa's Hyperspace Fury).
    Forme,
    /// `[weak]`: the user has too little HP (Substitute, Shed Tail).
    Weak,
}

/// An annotation Showdown's `attrLastMove` appends to the last `|move|`
/// line (see [`BattleEvent::MoveAttr`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MoveAttr {
    /// `[still]`: no animation; the line's target is blanked too.
    Still,
    /// `[miss]`: a single-target move missed.
    Miss,
    /// `[notarget]`: the move had no target.
    NoTarget,
    /// `[spread] p2a,p2b`: a spread move and the slots it hit, in order
    /// (`[spread] ` alone when it hit nothing).
    Spread(SpreadSlots),
    /// `[anim] <animation>`: the move is animated as another.
    Anim(MoveAnim),
}

/// The slots a spread move hit (`hitSlot`), in the order of its targets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SpreadSlots {
    /// The slots hit, in target order; only the first `len` are used.
    pub slots: [Position; 3],
    /// How many entries of `slots` are used (0 to 3).
    pub len: u8,
}

impl SpreadSlots {
    /// The slots hit, in target order.
    pub fn as_slice(&self) -> &[Position] {
        &self.slots[..self.len as usize]
    }
}

/// The `[anim]` annotations Showdown's moves write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MoveAnim {
    /// `[anim] Fickle Beam All Out`.
    FickleBeamAllOut,
    /// `[anim] Ivy Cudgel <type>`.
    IvyCudgel(Type),
    /// `[anim] Shell Side Arm <category>`.
    ShellSideArm(pikapro_data::MoveCategory),
    /// `[anim] Tera Blast <type>`.
    TeraBlast(Type),
}

/// A `|-hint|` line: Showdown explaining a mechanic (`battle.hint`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hint {
    /// `Only a Pokemon whose form is <forms> can use this move.` (Aura
    /// Wheel, Dark Void, Hyperspace Fury).
    OnlyForme(MoveId),
    /// `<move> only works on your first turn out.` (Fake Out, First
    /// Impression, Mat Block).
    FirstTurnOnly(MoveId),
    /// `Psychic Terrain doesn't affect airborne Pokémon.`
    PsychicTerrainAirborne,
    /// `<move>'s BP doubled on grounded target.` (Rising Voltage).
    GroundedDoubled(MoveId),
    /// `If a Terastallized Pokemon uses Roost, it remains Flying-type.`
    TeraRoost,
    /// `<move> uses [Sp. ]Def boosts when Wonder Room is active.` (Body
    /// Press: `special` when it would have used Defense).
    WonderRoomDefense {
        /// The move the hint names (Body Press).
        move_id: MoveId,
        /// Whether the hint says `Sp. Def` rather than `Def`.
        special: bool,
    },
    /// `Since gen 7, Dark is immune to Prankster moves.`
    DarkPrankster,
    /// `<Physical|Special> Shell Side Arm`.
    ShellSideArm(pikapro_data::MoveCategory),
    /// `In Gen 8+, Future attacks will never resolve when used on the 255th
    /// turn or later.`
    FutureMoveTurnLimit,
    /// `<move> did not hit because the target is fainted.` / `... is the
    /// user.`
    FutureMoveMissed {
        /// The foreseen move (Future Sight, Doom Desire).
        move_id: MoveId,
        /// Whether the target had fainted (`false`: the target is the user).
        fainted: bool,
    },
    /// `In Gen 8+, Wish will never resolve when used on the <turn>th turn.`
    WishTurnLimit(u16),
    /// `Some effects can force a Pokemon to use <move> again in a row.`
    CantUseTwice(MoveId),
    /// `Transform Mimicry changes you to your original un-transformed types.`
    TransformMimicry,
    /// `A switch failed because the Pokémon trying to switch in is already
    /// in.`
    SwitchAlreadyIn,
    /// `A Pokemon can't switch between when it runs out of HP and when it
    /// faints`
    SwitchAtZeroHp,
    /// `Skill Swap does not announce the abilities of the Pokémon when used
    /// between allies.` (`Battle.skillSwap`).
    SkillSwapAllies,
    /// `In Gen 7, Stockpile keeps track of how many times it successfully
    /// altered each stat individually.` (Stockpile ends after a layer that
    /// didn't change both defenses.)
    StockpileTracking,
    /// `Sleep Clause Mod prevents players from putting more than one of
    /// their opponent's Pokémon to sleep at a time`.
    SleepClauseMod,
    /// `Illusion Level Mod is active, so this Pokémon's true level was
    /// hidden.` (once per battle).
    IllusionLevelMod,
}

/// A message line (`|message|`, `|-message|`, `|bigerror|`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Message {
    /// `|-message|(<cured> of <side>'s pokemon was/were cured by Natural
    /// Cure.)`
    NaturalCure {
        /// The side whose Pokémon were cured (named by its player).
        side: SideId,
        /// How many of its Pokémon were cured.
        cured: u8,
    },
    /// `|message|It is turn <limit>. You have hit the turn limit!`: the
    /// battle ends in a tie ([`Format::turn_limit`](crate::Format), 1000
    /// in every preset and in Showdown).
    TurnLimit {
        /// The format's turn limit.
        limit: u16,
    },
    /// `|bigerror|You will auto-tie if the battle doesn't end in <n> turns
    /// (on turn <limit>).`
    AutoTieWarning {
        /// Turns until the tie.
        turns_left: u16,
        /// The format's turn limit.
        limit: u16,
    },
    /// `|-message|<player>'s team started with the rudimentary means to
    /// perform restorative berry-cycling and thus loses.`: the Endless
    /// Battle Clause ends the battle, won by the other side.
    BerryCycling {
        /// The side that loses (named by its player).
        loser: SideId,
    },
    /// `|-message|Each side's team started with the rudimentary means to
    /// perform restorative berry-cycling.`: the Endless Battle Clause ends
    /// the battle in a tie.
    BerryCyclingBoth,
    /// `|-message|Sleep Clause Mod activated.`
    SleepClauseMod,
}

/// Something that happened in a battle, in log order. Most variants render
/// as one Showdown protocol line (`pikapro-protocol`'s renderer), which the
/// variant's doc names.
///
/// Fields that recur mean the same thing everywhere: `hp` / `max_hp` /
/// `status` are the named Pokémon's exact HP after the event, its max HP
/// and its non-volatile status (an HP report, `<hp>/<max_hp> <status>`);
/// `of` is the other Pokémon credited (`[of] <of>`); `cause` is the
/// `[from] <effect>|[of] <pokemon>` suffix; `silent` adds `[silent]`.
///
/// Announced volatiles come in three families: [`Volatile`] (confusion,
/// Taunt, Encore, Disable: `VolatileStart` and the like),
/// [`ManipulationVolatile`] (Aqua Ring, Charge, Stockpile, ...:
/// `ManipulationStart` and the like) and [`VolatileEffect`] (Substitute,
/// trapping, Leech Seed, Perish Song, ...: `EffectStart` and the like).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BattleEvent {
    /// A Pokémon on a side's registered team (sent before team preview).
    Poke {
        /// The side whose team it is on.
        side: SideId,
        /// Its species.
        species: SpeciesId,
        /// Its level.
        level: u8,
        /// Its gender, shown in the details (`Garchomp, L50, M`).
        gender: Gender,
    },
    /// Team preview begins (`|teampreview|<bring>`, or `|teampreview`
    /// without a picked team size).
    TeamPreview {
        /// How many Pokémon each player brings to the battle; `None` for
        /// the whole team ([`TeamPreview::All`](crate::TeamPreview::All)).
        bring: Option<u8>,
    },
    /// The battle starts (`|start`).
    Start,
    /// A new turn begins (`|turn|<turn>`).
    Turn {
        /// The turn number, from 1.
        turn: u16,
    },
    /// A Pokémon switched in (`|switch|<actor>|<details>|<hp>`).
    /// `team_index` is the incoming Pokémon's position before the switch.
    Switch {
        /// The Pokémon switching in, at the slot it enters.
        actor: Actor,
        /// Its team position before the switch.
        team_index: u8,
        /// Its level, shown in the details.
        level: u8,
        /// Its current HP.
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Shown after the HP (`118/135 slp`), like every HP report.
        status: Status,
        /// Its Tera type once it has Terastallized (`, tera:<type>` in the
        /// details).
        tera: Option<Type>,
        /// The move that switched the previous Pokémon out (U-turn), logged
        /// as `[from]`.
        from: Option<MoveId>,
        /// The species its details show when it is not
        /// `actor.species` (an Illusion disguised as a transformed
        /// teammate shows that teammate's name and current species).
        shown: Option<SpeciesId>,
        /// The gender its details show (an Illusion's disguise's).
        gender: Gender,
    },
    /// A Pokémon dragged in by Roar / Dragon Tail (Showdown's `drag`).
    /// Fields as in [`BattleEvent::Switch`].
    Drag {
        /// The Pokémon dragged in, at the slot it enters.
        actor: Actor,
        /// Its team position before the switch.
        team_index: u8,
        /// Its level, shown in the details.
        level: u8,
        /// Its current HP.
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Its status, shown after the HP.
        status: Status,
        /// Its Tera type once it has Terastallized (`, tera:<type>` in the
        /// details).
        tera: Option<Type>,
        /// The species its details show when it is not `actor.species`
        /// (Illusion).
        shown: Option<SpeciesId>,
        /// The gender its details show (an Illusion's disguise's).
        gender: Gender,
    },
    /// A Pokémon used a move (`|move|<user>|<move>|<target>`).
    Move {
        /// The Pokémon using the move.
        user: Actor,
        /// The move used.
        move_id: MoveId,
        /// The target the line names; `None` leaves it blank (no target,
        /// or the move failed before hitting).
        target: Option<Actor>,
        /// What made the Pokémon use the move (`[from] ...`), if anything.
        from: Option<MoveFrom>,
    },
    /// A Pokémon took damage (`-damage|<target>|<hp>|[from] ...`).
    Damage {
        /// The Pokémon damaged.
        target: Actor,
        /// Its HP after the damage (0: it is fainting, `0 fnt`).
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Its status, shown after the HP.
        status: Status,
        /// What dealt the damage (`[from] ...`).
        source: DamageSource,
    },
    /// A Pokémon restored HP (`-heal|<target>|<hp>|[from] ...`).
    Heal {
        /// The Pokémon healed.
        target: Actor,
        /// Its HP after the healing.
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Its status, shown after the HP.
        status: Status,
        /// What restored the HP (`[from] ...`).
        source: HealSource,
    },
    /// A critical hit (`-crit|<target>`).
    Crit {
        /// The Pokémon hit.
        target: Actor,
    },
    /// A super-effective hit.
    SuperEffective {
        /// The Pokémon hit (`-supereffective|<target>`).
        target: Actor,
        /// 1 for 2x, 2 for 4x (written as `|<level>` under Champions
        /// mechanics only).
        level: u8,
    },
    /// A resisted hit.
    Resisted {
        /// The Pokémon hit (`-resisted|<target>`).
        target: Actor,
        /// 1 for 0.5x, 2 for 0.25x (written as `|<level>` under Champions
        /// mechanics only).
        level: u8,
    },
    /// A move did not affect the target (`-immune|<target>`).
    Immune {
        /// The Pokémon unaffected.
        target: Actor,
    },
    /// An OHKO move cannot affect the target (`-immune|...|[ohko]`): higher
    /// level, or an Ice type against Sheer Cold.
    OhkoImmune {
        /// The Pokémon unaffected.
        target: Actor,
    },
    /// How many times a multi-hit move hit.
    HitCount {
        /// The move's first target (`-hitcount|<target>|<hits>`).
        target: Actor,
        /// The number of hits.
        hits: u8,
    },
    /// An OHKO move knocked out its target.
    Ohko,
    /// A move missed (`-miss|<user>|<target>`).
    Miss {
        /// The Pokémon using the move.
        user: Actor,
        /// The Pokémon it missed.
        target: Actor,
    },
    /// A stat stage rose by `amount` (`-boost`). An amount of 0 means it
    /// could not go any higher.
    Boost {
        /// The Pokémon whose stage rose.
        target: Actor,
        /// The stat.
        stat: BoostId,
        /// Stages gained.
        amount: u8,
    },
    /// A stat stage fell by `amount` (`-unboost`). An amount of 0 means it
    /// could not go any lower.
    Unboost {
        /// The Pokémon whose stage fell.
        target: Actor,
        /// The stat.
        stat: BoostId,
        /// Stages lost.
        amount: u8,
    },
    /// A move failed (including "not implemented yet").
    Fail {
        /// The Pokémon whose move failed (`-fail|<user>`).
        user: Actor,
    },
    /// A healing move failed because the target is at full HP
    /// (`-fail|...|heal`).
    HealFail {
        /// The Pokémon already at full HP.
        target: Actor,
    },
    /// A move had no valid target left.
    NoTarget {
        /// The Pokémon whose move failed (`-fail|<user>`).
        user: Actor,
    },
    /// The Pokémon has no usable moves and struggles (logged before the move).
    StruggleStart {
        /// The Pokémon struggling (`-activate|<actor>|move: Struggle`).
        actor: Actor,
    },
    /// The Pokémon protected itself this turn with `move_id` (Protect,
    /// Spiky Shield, ...).
    ProtectStart {
        /// The Pokémon protecting itself.
        actor: Actor,
        /// The protecting move.
        move_id: MoveId,
    },
    /// An attack was blocked by protection.
    Protected {
        /// The protected Pokémon (`-activate|<target>|move: Protect`).
        target: Actor,
    },
    /// `move_id` (Feint) broke the target's protection.
    ProtectionBroken {
        /// The Pokémon whose protection broke.
        target: Actor,
        /// The move that broke it.
        move_id: MoveId,
    },
    /// A Pokémon Terastallized (`-terastallize|<actor>|<type>`).
    Terastallize {
        /// The Pokémon Terastallizing.
        actor: Actor,
        /// Its Tera type.
        ty: Type,
    },
    /// A non-volatile status was inflicted (`-status`). `from_move` is set
    /// when Showdown names the move (`[from] move: X`, sleep only).
    StatusStart {
        /// The Pokémon given the status.
        target: Actor,
        /// The status.
        status: Status,
        /// The move named with `[from] move: <move>` (Rest), if any.
        from_move: Option<MoveId>,
    },
    /// A status was cured (`-curestatus`): `[msg]`, or `[from] move: X` for
    /// a frozen Pokémon thawed by its own `defrost` move.
    CureStatus {
        /// The Pokémon cured.
        target: Actor,
        /// The status it had.
        status: Status,
        /// The thawing move (`[from] move: <move>`); `None` writes `[msg]`.
        from_move: Option<MoveId>,
    },
    /// The Pokémon's status stops it from moving (`cant|...|par`).
    StatusCant {
        /// The Pokémon that can't move.
        actor: Actor,
        /// The status stopping it.
        status: Status,
    },
    /// A status move failed because the target already has that status
    /// (`-fail|<target>|<status>`).
    StatusFail {
        /// The Pokémon that already has the status.
        target: Actor,
        /// The status.
        status: Status,
    },
    /// The Pokémon can't move this turn.
    Cant {
        /// The Pokémon that can't move (`cant|<actor>|<reason>`).
        actor: Actor,
        /// Why it can't.
        reason: CantReason,
    },
    /// A volatile condition started. `move_id` is the move it concerns
    /// (the disabled move).
    VolatileStart {
        /// The Pokémon given the condition (`-start|<actor>|<volatile>`).
        actor: Actor,
        /// The condition.
        volatile: Volatile,
        /// The move it concerns, written after the condition (Disable's
        /// disabled move).
        move_id: Option<MoveId>,
    },
    /// A volatile condition ended.
    VolatileEnd {
        /// The Pokémon whose condition ended (`-end|<actor>|<volatile>`).
        actor: Actor,
        /// The condition.
        volatile: Volatile,
    },
    /// A volatile condition took effect (confusion before a move).
    VolatileActivate {
        /// The Pokémon affected (`-activate|<actor>|<volatile>`).
        actor: Actor,
        /// The condition.
        volatile: Volatile,
    },
    /// The Pokémon must recharge next turn.
    MustRecharge {
        /// The Pokémon that must recharge (`-mustrecharge|<actor>`).
        actor: Actor,
    },
    /// A Pokémon fainted (`faint|<actor>`).
    Faint {
        /// The Pokémon that fainted.
        actor: Actor,
    },
    /// The weather started (`upkeep` false), continues at end of turn
    /// (`upkeep` true), or ended (`None`).
    Weather {
        /// The weather now in place (`-weather|<weather>`); `None` when it
        /// ended (`-weather|none`).
        weather: Option<Weather>,
        /// Whether this is the end-of-turn report (`[upkeep]`).
        upkeep: bool,
    },
    /// A terrain or pseudo-weather started; `of` is the Pokémon credited
    /// (Trick Room names its user).
    FieldStart {
        /// The effect started (`-fieldstart|move: <effect>`).
        effect: FieldEffect,
        /// The Pokémon credited (`[of] <of>`), if any.
        of: Option<Actor>,
    },
    /// A terrain or pseudo-weather ended (`-fieldend|move: <effect>`).
    FieldEnd {
        /// The effect that ended.
        effect: FieldEffect,
    },
    /// A side condition started (`-sidestart|<side>|<condition>`).
    SideStart {
        /// The side it is on.
        side: SideId,
        /// The condition.
        condition: SideCondition,
    },
    /// A side condition ended (`-sideend|<side>|<condition>`).
    SideEnd {
        /// The side it was on.
        side: SideId,
        /// The condition.
        condition: SideCondition,
    },
    /// A terrain protected `target` (Psychic Terrain blocking a priority
    /// move; Misty and Electric Terrain blocking status).
    TerrainActivate {
        /// The Pokémon protected (`-activate|<target>|move: <terrain>`).
        target: Actor,
        /// The terrain.
        terrain: Terrain,
    },
    /// A move failed with its own message (`-fail|<user>|move: <move>`):
    /// Aura Wheel, Hyperspace Fury, Double Shock, Burn Up, Dark Void,
    /// Substitute, Shed Tail, Heal Block, Ally Switch.
    MoveFail {
        /// The Pokémon whose move failed.
        user: Actor,
        /// The move.
        move_id: MoveId,
        /// The line's annotation.
        note: FailNote,
    },

    // Multi-turn moves.
    /// A move is being prepared (`-prepare`): a two-turn move's charging
    /// turn, or Chilly Reception's announcement (with `[premajor]`).
    Prepare {
        /// The Pokémon preparing the move (`-prepare|<user>|<move>`).
        user: Actor,
        /// The move.
        move_id: MoveId,
    },
    /// Focus Punch or Beak Blast starts charging
    /// (`-singleturn|<actor>|move: Focus Punch`).
    ChargeStart {
        /// The Pokémon charging.
        actor: Actor,
        /// The move charging (Focus Punch or Beak Blast).
        move_id: MoveId,
    },
    /// Focus Punch lost its focus (`cant|<actor>|Focus Punch|Focus Punch`).
    LostFocus {
        /// The Pokémon that lost its focus.
        actor: Actor,
    },
    /// A lock-in (Outrage, ...) ended in confusion
    /// (`-start|<actor>|confusion|[fatigue]`).
    FatigueConfusion {
        /// The Pokémon confused by fatigue.
        actor: Actor,
    },
    /// Uproar started (`-start|<actor>|Uproar`) or goes on at the end of the
    /// turn (`upkeep`: `-start|<actor>|Uproar|[upkeep]`).
    UproarStart {
        /// The Pokémon causing the uproar.
        actor: Actor,
        /// Whether this is the end-of-turn report (`[upkeep]`).
        upkeep: bool,
    },
    /// Uproar ended (`-end|<actor>|Uproar`).
    UproarEnd {
        /// The Pokémon whose uproar ended.
        actor: Actor,
    },
    /// An uproar kept `target` awake (`-fail|<target>|slp|[from] Uproar`,
    /// plus `[msg]` when `target` is the one causing it).
    UproarPreventsSleep {
        /// The Pokémon kept awake.
        target: Actor,
        /// Whether `target` is the one causing the uproar (`[msg]`).
        own: bool,
    },
    /// Future Sight or Doom Desire was foreseen
    /// (`-start|<user>|move: Future Sight`, `-start|<user>|Doom Desire`).
    FutureMoveStart {
        /// The Pokémon that used the move.
        user: Actor,
        /// Future Sight or Doom Desire.
        move_id: MoveId,
    },
    /// A foreseen attack lands on `target` (`-end|<target>|move: Future Sight`).
    FutureMoveEnd {
        /// The Pokémon the attack lands on.
        target: Actor,
        /// Future Sight or Doom Desire.
        move_id: MoveId,
    },

    // Substitute, trapping and residual volatiles ([`VolatileEffect`]).
    /// A volatile started (`-start`, or `-activate` /
    /// `-singlemove` / `-singleturn` for the effects Showdown announces that
    /// way). `of` is the Pokémon credited (`[of]`); `from` is the move that
    /// created it when that is not the effect's own move (a substitute from
    /// Shed Tail).
    EffectStart {
        /// The Pokémon the effect is on.
        actor: Actor,
        /// The effect.
        effect: VolatileEffect,
        /// The Pokémon credited (`[of] <of>`), if any.
        of: Option<Actor>,
        /// The move that created it (`[from] move: <move>`), when not the
        /// effect's own.
        from: Option<MoveId>,
    },
    /// A volatile ended (`-end`). `silent` marks the endings
    /// Showdown flags `[silent]` beyond the effect's usual form (a partial
    /// trap whose trapper left).
    EffectEnd {
        /// The Pokémon the effect was on.
        actor: Actor,
        /// The effect.
        effect: VolatileEffect,
        /// Whether to add `[silent]` beyond the effect's usual form.
        silent: bool,
    },
    /// A volatile removed by another move
    /// (`-end|<actor>|<effect>|[from] move: <move>|[of] <of>`: Rapid Spin
    /// freeing its user from Leech Seed).
    EffectEndFrom {
        /// The Pokémon the effect was on.
        actor: Actor,
        /// The effect removed.
        effect: VolatileEffect,
        /// The move that removed it.
        move_id: MoveId,
        /// The move's user.
        of: Actor,
    },
    /// A volatile took effect (`-activate`): a substitute took
    /// a hit, Attract before a move, Destiny Bond, Endure.
    EffectActivate {
        /// The Pokémon the effect is on (`-activate|<actor>|move: <effect>`).
        actor: Actor,
        /// The effect.
        effect: VolatileEffect,
        /// The Pokémon credited (`[of] <of>`), if any.
        of: Option<Actor>,
    },
    /// A volatile stops the Pokémon's move (`cant`), naming the
    /// move when Showdown does.
    EffectCant {
        /// The Pokémon that can't move (`cant|<actor>|<effect>|<move>`).
        actor: Actor,
        /// The effect stopping it.
        effect: VolatileEffect,
        /// The move it was stopped from using, when the line names it.
        move_id: Option<MoveId>,
    },
    /// Damage from a volatile (`-damage|...|[from] ...`).
    EffectDamage {
        /// The Pokémon damaged.
        target: Actor,
        /// Its HP after the damage.
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Its status, shown after the HP.
        status: Status,
        /// The effect dealing the damage (`[from] <effect>`).
        effect: VolatileEffect,
        /// The Pokémon credited (`[of] <of>`), if any.
        of: Option<Actor>,
    },
    /// Healing from a volatile (Leech Seed's `-heal|...|[silent]`).
    EffectHeal {
        /// The Pokémon healed.
        target: Actor,
        /// Its HP after the healing.
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Its status, shown after the HP.
        status: Status,
        /// The effect the healing comes from (not written on the line).
        effect: VolatileEffect,
    },
    /// Perish Song's count (`-start|<actor>|perish<count>`); the first one
    /// is `[silent]`.
    PerishCount {
        /// The Pokémon counting down.
        actor: Actor,
        /// The count shown (3 down to 0; it faints at 0).
        count: u8,
        /// Whether to add `[silent]`.
        silent: bool,
    },
    /// A field-wide move took effect (`-fieldactivate`: Fairy Lock, Perish
    /// Song).
    FieldActivate {
        /// The effect (`-fieldactivate|move: <effect>`).
        effect: VolatileEffect,
    },

    // Hazards, side, field and slot conditions.
    /// Wide Guard or Quick Guard protects `actor`'s side this turn
    /// (`-singleturn|<actor>|Wide Guard`).
    GuardStart {
        /// The Pokémon that used the guard.
        actor: Actor,
        /// Wide Guard or Quick Guard.
        condition: SideCondition,
    },
    /// An entry hazard was cleared: `-sideend|<side>|<hazard>`, with
    /// `[from] move: <from>` and `[of] <of>` when a move (Rapid Spin,
    /// Defog) did it.
    HazardEnd {
        /// The side the hazard was on.
        side: SideId,
        /// The hazard.
        condition: SideCondition,
        /// The move that cleared it, if any.
        from: Option<MoveId>,
        /// The Pokémon that used that move, if any.
        of: Option<Actor>,
    },
    /// A grounded Poison type absorbed the Toxic Spikes on its side.
    ToxicSpikesAbsorbed {
        /// The side the Toxic Spikes were on
        /// (`-sideend|<side>|move: Toxic Spikes|[of] <of>`).
        side: SideId,
        /// The Poison type that absorbed them.
        of: Actor,
    },
    /// Damage from a side condition (`[from] Stealth Rock`, `Spikes`,
    /// `Fire Pledge`).
    ConditionDamage {
        /// The Pokémon damaged.
        target: Actor,
        /// Its HP after the damage.
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Its status, shown after the HP.
        status: Status,
        /// The side condition dealing the damage.
        condition: SideCondition,
    },
    /// Healing from a slot condition or Revival Blessing
    /// (`-heal|<target>|<hp>|[from] move: <move>`).
    MoveHeal {
        /// The Pokémon healed.
        target: Actor,
        /// Its HP after the healing.
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Its status, shown after the HP.
        status: Status,
        /// The move the healing comes from.
        move_id: MoveId,
    },
    /// Wish came true (`-heal|...|[from] move: Wish|[wisher] <name>`).
    WishHeal {
        /// The Pokémon healed.
        target: Actor,
        /// Its HP after the healing.
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Its status, shown after the HP.
        status: Status,
        /// The species of the Pokémon that used Wish (on the target's
        /// side).
        wisher: SpeciesId,
        /// The wisher's `PokemonState::id` (`[wisher]` shows its
        /// name).
        wisher_id: u8,
    },
    /// Haze (`-clearallboost`).
    ClearAllBoost,
    /// Court Change (`-swapsideconditions`).
    SwapSideConditions,
    /// Gravity stops a move (`cant|<actor>|move: Gravity|<move>`).
    GravityCant {
        /// The Pokémon that can't move.
        actor: Actor,
        /// The move Gravity stopped.
        move_id: MoveId,
    },
    /// Magic Room ended; `of` started it (`-fieldend|move: Magic Room|[of] <of>`).
    MagicRoomEnd {
        /// The Pokémon credited with Magic Room.
        of: Actor,
    },
    /// A pledge waits for the ally's pledge (`-waiting|<user>|<ally>`).
    PledgeWaiting {
        /// The Pokémon whose pledge waits.
        user: Actor,
        /// The ally whose pledge it waits for.
        ally: Actor,
    },
    /// Two pledges combine (`-combine`).
    PledgeCombine,

    // Stat, HP and type manipulation.
    /// A stat stage was set outright by `move_id` (`-setboost`, Belly Drum).
    SetBoost {
        /// The Pokémon whose stage was set.
        target: Actor,
        /// The stat.
        stat: BoostId,
        /// The stage it was set to (-6 to 6).
        stage: i8,
        /// The move that set it (`[from] move: <move>`).
        move_id: MoveId,
    },
    /// HP set by Pain Split (`-sethp`); `silent` for the target's line.
    SetHp {
        /// The Pokémon whose HP was set.
        target: Actor,
        /// Its new HP.
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Its status, shown after the HP.
        status: Status,
        /// Whether to add `[silent]` (the Pain Split target's line).
        silent: bool,
    },
    /// `source` copied `target`'s stat stages (`-copyboost`, Psych Up).
    CopyBoost {
        /// The Pokémon that copied the stages (Psych Up's user).
        source: Actor,
        /// The Pokémon whose stages were copied.
        target: Actor,
    },
    /// `source` and `target` swapped the stat stages `move_id` swaps
    /// (`-swapboost`: Heart Swap, Guard Swap, Power Swap).
    SwapBoost {
        /// The move's user.
        source: Actor,
        /// The move's target.
        target: Actor,
        /// Heart Swap, Guard Swap or Power Swap.
        move_id: MoveId,
    },
    /// The target's stat stages were inverted (`-invertboost`, Topsy-Turvy).
    InvertBoost {
        /// The Pokémon whose stages were inverted.
        target: Actor,
    },
    /// The target's stat stages were reset (`-clearboost`, Clear Smog).
    ClearBoost {
        /// The Pokémon whose stages were reset.
        target: Actor,
    },
    /// A move took effect: `-activate|<actor>|move: <move>`, with `[of]`
    /// when another Pokémon is involved (Guard Split, Lock-On, Heal Bell,
    /// Celebrate, Ingrain resisting Roar, Fickle Beam's doubled power,
    /// Safeguard, Mist, the guards, Sticky Web, Gravity, Tidy Up, Court
    /// Change).
    MoveActivate {
        /// The Pokémon the line names first.
        actor: Actor,
        /// The move that took effect.
        move_id: MoveId,
        /// The other Pokémon involved (`[of] <of>`), if any.
        of: Option<Actor>,
    },
    /// Eerie Spell took `amount` PP from `move_id`.
    PpDrain {
        /// The Pokémon that lost PP
        /// (`-activate|<target>|move: Eerie Spell|<move>|<amount>`).
        target: Actor,
        /// Its move that lost PP.
        move_id: MoveId,
        /// The PP removed.
        amount: u8,
    },
    /// The target's types became `types` (`-start|...|typechange`);
    /// `silent` when the end of the turn reveals types that changed
    /// unseen (after Reflect Type).
    TypeChange {
        /// The Pokémon whose types changed.
        target: Actor,
        /// Its new types.
        types: pikapro_data::TypeSet,
        /// Whether to add `[silent]`.
        silent: bool,
    },
    /// Reflect Type: `target` copied the types of `of`.
    TypeCopy {
        /// The Pokémon whose types changed (Reflect Type's user).
        target: Actor,
        /// The Pokémon whose types it copied.
        of: Actor,
    },
    /// `move_id` added the type `ty` (`-start|...|typeadd`); without a
    /// move, revealed at the end of the turn (`[silent]`).
    TypeAdd {
        /// The Pokémon given the type.
        target: Actor,
        /// The type added.
        ty: Type,
        /// The move that added it (Trick-or-Treat, Forest's Curse); `None`
        /// writes `[silent]`.
        move_id: Option<MoveId>,
    },
    /// A [`ManipulationVolatile`] started (`-start`); `silent` when copied (Psych Up).
    ManipulationStart {
        /// The Pokémon given the volatile.
        actor: Actor,
        /// The volatile.
        volatile: ManipulationVolatile,
        /// Whether to add `[silent]`.
        silent: bool,
    },
    /// A [`ManipulationVolatile`] ended (`-end`); `silent` adds `[silent]` (Charge).
    ManipulationEnd {
        /// The Pokémon whose volatile ended.
        actor: Actor,
        /// The volatile.
        volatile: ManipulationVolatile,
        /// Whether to add `[silent]`.
        silent: bool,
    },
    /// An effect lasting the rest of the turn (`-singleturn|...|move: X`:
    /// Roost, Electrify).
    SingleTurn {
        /// The Pokémon the effect is on.
        actor: Actor,
        /// The move whose effect it is.
        move_id: MoveId,
    },
    /// Splash did nothing (`-nothing`).
    Nothing,
    /// HP restored by Rest (`from: None`, logged `[silent]`) or at the end
    /// of the turn by a volatile (`[from] Aqua Ring`).
    ManipulationHeal {
        /// The Pokémon healed.
        target: Actor,
        /// Its HP after the healing.
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Its status, shown after the HP.
        status: Status,
        /// The volatile that healed it (`[from] <volatile>`); `None` for
        /// Rest.
        from: Option<ManipulationVolatile>,
    },

    // Action order, targeting and move calling.
    /// A move used with no target at all (Showdown's `getTarget` returned
    /// `null`: an ally-only move without an ally, Dragon Cheer alone),
    /// logged `|move|...|null`, with `from` as in [`BattleEvent::Move`].
    MoveNull {
        /// The Pokémon using the move.
        user: Actor,
        /// The move used.
        move_id: MoveId,
        /// What made the Pokémon use the move (`[from] ...`), if anything.
        from: Option<MoveFrom>,
    },
    /// The Pokémon has no PP left for the move it was about to use
    /// (`cant|...|nopp|<move>`, `runMove`).
    CantNoPp {
        /// The Pokémon that can't move.
        actor: Actor,
        /// The move without PP. `None` is `recharge`, which is not a dex
        /// move: a Pokémon whose `mustrecharge` a Dancer copy used up is
        /// still locked into it and has no PP for it.
        move_id: Option<MoveId>,
    },
    /// Ally Switch moved `actor` to `slot` (`|swap|`).
    Swap {
        /// The Pokémon moving, at its slot before the swap.
        actor: Actor,
        /// The slot it moves to (`|swap|<actor>|<slot>`).
        slot: u8,
        /// The move that swapped it (`[from] move: <move>`).
        move_id: MoveId,
    },
    /// A move's effect on `target` naming another move (`-activate|...|move:
    /// X|<about>|<amount>`): After You, Quash, Sketch (`about` = the sketched
    /// move), Spite (`about` = the move, `amount` = PP removed).
    ActivateMove {
        /// The Pokémon the effect is on.
        target: Actor,
        /// The move taking effect.
        move_id: MoveId,
        /// The other move the line names, if any.
        about: Option<MoveId>,
        /// The PP removed (Spite); 0 writes nothing.
        amount: u8,
    },
    /// A one-turn effect of `move_id` started on `target` (`-singleturn`):
    /// Helping Hand and Instruct (`of` their user), Follow Me, Rage Powder.
    MoveSingleTurn {
        /// The Pokémon the effect is on.
        target: Actor,
        /// The move whose effect it is.
        move_id: MoveId,
        /// The move's user (`[of] <of>`), if the line names it.
        of: Option<Actor>,
    },
    /// `actor` transformed into `target` (`-transform`).
    Transform {
        /// The Pokémon transforming.
        actor: Actor,
        /// The Pokémon it transformed into.
        target: Actor,
    },
    /// Mimic copied `move_id` (`-start|...|Mimic|<move>`).
    MimicStart {
        /// The Pokémon that used Mimic.
        actor: Actor,
        /// The move copied.
        move_id: MoveId,
    },
    /// The end of the turn reveals the Pokémon's real types
    /// (`-start|...|typechange|<types>|[silent]`).
    TypeChangeSilent {
        /// The Pokémon whose types are revealed.
        actor: Actor,
        /// Its types.
        types: TypeSet,
    },
    // Abilities and items: general events first, then specific ones.
    /// An ability is revealed or announced (`-ability|<actor>|<ability>`,
    /// with its [`AbilityNote`] and cause).
    AbilityShow {
        /// The Pokémon whose ability it is.
        actor: Actor,
        /// The ability.
        ability: AbilityId,
        /// What the line says after the ability.
        note: AbilityNote,
        /// The `[from] ...|[of] ...` suffix, if any.
        cause: Option<Cause>,
    },
    /// An item is revealed or gained (`-item|<actor>|<item>|[from] ...|[of]
    /// ...`).
    ItemShow {
        /// The Pokémon holding the item.
        actor: Actor,
        /// The item.
        item: ItemId,
        /// The `[from] ...|[of] ...` suffix, if any.
        cause: Option<Cause>,
    },
    /// An item is used up, eaten or lost (`-enditem|<actor>|<item>`, with
    /// its [`ItemEndNote`], `[from] <from>` and `[of] <of>`: Red Card names
    /// its target with `[of]` alone).
    ItemEnd {
        /// The Pokémon that held the item.
        actor: Actor,
        /// The item.
        item: ItemId,
        /// The line's annotation.
        note: ItemEndNote,
        /// The effect that removed it (`[from] <from>`), if named.
        from: Option<EffectName>,
        /// The other Pokémon involved (`[of] <of>`), if any.
        of: Option<Actor>,
    },
    /// `-enditem|<actor>||[from] move: Fling`, with no item named: Fling's
    /// end-of-move cleanup found the item already gone. A Blunder Policy
    /// flung at a foe and missing is used up by the miss, and Showdown's
    /// Fling then announces the loss of the nothing that is left.
    FlingNoItem {
        /// The Pokémon that used Fling.
        actor: Actor,
    },
    /// A stat change caused by an ability or item: `-boost|...|[from]
    /// item: X` (`unboost`: `-unboost`).
    BoostFrom {
        /// The Pokémon whose stage changed.
        target: Actor,
        /// The stat.
        stat: BoostId,
        /// Stages gained or lost.
        amount: u8,
        /// Whether the stage fell (`-unboost`) rather than rose.
        unboost: bool,
        /// The ability or item responsible.
        cause: Cause,
    },
    /// A status inflicted by an ability or item (`-status|<target>|brn|[from]
    /// ability: Flame Body|[of] <source>`, `[from] item: Flame Orb`).
    StatusFrom {
        /// The Pokémon given the status.
        target: Actor,
        /// The status.
        status: Status,
        /// The ability or item responsible.
        cause: Cause,
    },
    /// A status cured by an ability (`-curestatus|<target>|<status>|[from]
    /// ability: Natural Cure`, `[silent]`).
    CureStatusFrom {
        /// The Pokémon cured.
        target: Actor,
        /// The status it had.
        status: Status,
        /// The ability or item responsible.
        cause: Cause,
        /// Whether to add `[silent]`.
        silent: bool,
    },
    /// An ability or item made the target immune (`-immune|<target>|[from]
    /// ability: Levitate`; `detail` is an extra argument, such as
    /// `confusion`).
    ImmuneFrom {
        /// The Pokémon unaffected.
        target: Actor,
        /// An extra argument after the target, if any.
        detail: Option<LineArg>,
        /// The ability or item responsible.
        cause: Cause,
    },
    /// An ability or item stopped something (`-fail|<target>|unboost|[from]
    /// ability: Clear Body|[of] <target>`).
    FailFrom {
        /// The Pokémon named first (`-fail|<target>`).
        target: Actor,
        /// What failed, right after the target (`unboost`, a move, ...).
        what: LineArg,
        /// A further argument, if any.
        detail: Option<LineArg>,
        /// The ability or item responsible.
        cause: Cause,
    },
    /// Weather started by an ability (`-weather|SunnyDay|[from] ability:
    /// Drought|[of] <actor>`).
    WeatherFrom {
        /// The weather started.
        weather: Weather,
        /// The ability responsible and its holder.
        cause: Cause,
    },
    /// A terrain or pseudo-weather started by an ability
    /// (`-fieldstart|move: Electric Terrain|[from] ability: Electric
    /// Surge|[of] <actor>`).
    FieldStartFrom {
        /// The effect started.
        effect: FieldEffect,
        /// The ability responsible and its holder.
        cause: Cause,
    },
    /// An ability or item takes effect (`-activate|<actor>|ability: X`, with
    /// up to two extra arguments and `[of]`).
    ActivateEffect {
        /// The Pokémon named on the line.
        actor: Actor,
        /// The ability or item taking effect.
        effect: EffectName,
        /// Up to two extra arguments after the effect, in order.
        args: [Option<LineArg>; 2],
        /// The other Pokémon involved (`[of] <of>`), if any.
        of: Option<Actor>,
    },
    /// An ability's or item's effect starts or ends on a Pokémon
    /// (`-start|<actor>|ability: Flash Fire`, `-end|<actor>|ability:
    /// Neutralizing Gas`), with an extra argument, a cause and `[silent]`.
    EffectStartEnd {
        /// The Pokémon the effect is on.
        actor: Actor,
        /// Whether it starts (`-start`) or ends (`-end`).
        start: bool,
        /// With `effect`, an extra argument after it (`LineArg::Text("")`
        /// writes none); without, the name of what starts or ends.
        what: LineArg,
        /// The ability or item written after the Pokémon, if any.
        effect: Option<EffectName>,
        /// The `[from] ...|[of] ...` suffix, if any.
        cause: Option<Cause>,
        /// Whether to add `[silent]`.
        silent: bool,
    },
    /// An ability or item blocks something (`-block|<actor>|ability: X`,
    /// with an extra argument and `[of]`).
    Block {
        /// The Pokémon whose ability or item blocks.
        actor: Actor,
        /// The ability or item.
        effect: EffectName,
        /// An extra argument (usually the blocked move), if any.
        arg: Option<LineArg>,
        /// The other Pokémon involved (`[of] <of>`), if any.
        of: Option<Actor>,
    },
    /// An ability stops a move (`cant|<actor>|ability: Truant`, `cant|<holder>|ability:
    /// Dazzling|<move>|[of] <user>`).
    CantFrom {
        /// The Pokémon named on the line: the one that can't move, or the
        /// holder of the blocking ability.
        actor: Actor,
        /// The ability responsible.
        effect: EffectName,
        /// The move stopped, if the line names it.
        move_id: Option<MoveId>,
        /// The move's user (`[of] <of>`), when `actor` is the holder.
        of: Option<Actor>,
    },
    /// A forme change (`-formechange|<actor>|<species>|[msg]|[from] ability:
    /// X`).
    FormeChange {
        /// The Pokémon changing forme.
        actor: Actor,
        /// Its new forme.
        species: SpeciesId,
        /// Whether to write `[msg]` (otherwise an empty argument).
        msg: bool,
        /// The `[from] ...|[of] ...` suffix, if any.
        cause: Option<Cause>,
    },
    /// A permanent forme change (`detailschange|<actor>|<details>`).
    DetailsChange {
        /// The Pokémon changing forme.
        actor: Actor,
        /// The species its new details show.
        species: SpeciesId,
        /// Its level, shown in the details.
        level: u8,
        /// Its Tera type once it has Terastallized (`, tera:<type>`).
        tera: Option<Type>,
        /// Whether to add `[silent]`.
        silent: bool,
        /// The gender its details show.
        gender: Gender,
    },
    /// HP changed without a heal or damage (`-heal|<actor>|<hp>|[silent]`
    /// after a forme change raised max HP).
    HealSilent {
        /// The Pokémon whose HP changed.
        target: Actor,
        /// Its HP after the change.
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Its status, shown after the HP.
        status: Status,
    },
    /// A weather takes effect (`-fieldactivate|Delta Stream`).
    WeatherActivate {
        /// The weather (a primal weather's name is its ability's).
        weather: Weather,
    },
    /// Negative stages cleared (`-clearnegativeboost|<actor>`, `[silent]`).
    ClearNegativeBoost {
        /// The Pokémon whose negative stages were cleared.
        target: Actor,
        /// Whether to add `[silent]`.
        silent: bool,
    },
    /// Spectral Thief takes the target's positive stat stages before damage.
    ClearPositiveBoost {
        /// The Pokemon losing its positive stages.
        target: Actor,
        /// The Pokemon receiving them.
        source: Actor,
        /// The move taking them.
        move_id: MoveId,
    },
    // Specific ability events.
    /// An ability cleared the target's stat stages (`-clearboost|<target>|[from]
    /// ability: Curious Medicine|[of] <of>`).
    ClearBoostFrom {
        /// The Pokémon whose stages were reset.
        target: Actor,
        /// The ability responsible and its holder.
        cause: Cause,
    },
    /// An ability copied `target`'s stat stages to `source`
    /// (`-copyboost|<source>|<target>|[from] ability: Costar`).
    CopyBoostFrom {
        /// The Pokémon that copied the stages (the ability's holder).
        source: Actor,
        /// The Pokémon whose stages were copied.
        target: Actor,
        /// The ability responsible.
        cause: Cause,
    },
    /// An ability changed the target's types (`-start|<target>|typechange|<types>|[from]
    /// ability: Mimicry`).
    TypeChangeFrom {
        /// The Pokémon whose types changed.
        target: Actor,
        /// Its new types.
        types: TypeSet,
        /// The ability responsible.
        cause: Cause,
    },
    /// An ability transformed `actor` into `target` (`-transform|<actor>|<target>|[from]
    /// ability: Imposter`).
    TransformFrom {
        /// The Pokémon transforming.
        actor: Actor,
        /// The Pokémon it transformed into.
        target: Actor,
        /// The ability responsible.
        cause: Cause,
    },
    /// A primal weather stopped a weather (`-fail|<source>|<move>|[from] <weather id>`,
    /// or `-ability|<source>|<ability>|[from] <weather id>|[fail]`).
    WeatherFail {
        /// The Pokémon whose move or ability failed to set the weather.
        source: Actor,
        /// That move or ability.
        effect: EffectName,
        /// The primal weather in place (written as its ID, `desolateland`).
        weather: Weather,
    },
    /// Heavy Rain stopped a damaging Fire move, or Extreme Sun a Water one
    /// (`-fail|<user>|<move>|[from] Primordial Sea` or `[from] Desolate
    /// Land`, with `[still]` on the move line): the weathers' `onTryMove`.
    WeatherMoveFail {
        /// The Pokémon whose move failed.
        user: Actor,
        /// The move.
        move_id: MoveId,
        /// [`Weather::HeavyRain`] or [`Weather::HarshSun`].
        weather: Weather,
    },
    /// A forme change announced silently (`-formechange|<actor>|<species>|[silent]|[from]
    /// ability: Zen Mode`).
    FormeChangeSilent {
        /// The Pokémon changing forme.
        actor: Actor,
        /// Its new forme.
        species: SpeciesId,
        /// The ability responsible.
        cause: Cause,
    },
    /// Orichalcum Pulse set the sun (`-activate|<actor>|Orichalcum Pulse|[source]`).
    ActivateSource {
        /// The ability's holder.
        actor: Actor,
        /// The ability (written by name, without `ability: `).
        ability: AbilityId,
    },
    /// An ability announced by another name (`-ability|<actor>|As One`).
    AbilityNamed {
        /// The ability's holder.
        actor: Actor,
        /// The name written.
        name: &'static str,
    },
    /// Healing announced after the fact (`-heal|<target>|<hp>|[from]
    /// ability: Regenerator|[silent]`, the Champions Regenerator).
    HealFromSilent {
        /// The Pokémon healed.
        target: Actor,
        /// Its HP after the healing.
        hp: u16,
        /// Its max HP.
        max_hp: u16,
        /// Its status, shown after the HP.
        status: Status,
        /// The ability responsible.
        cause: Cause,
    },

    /// A move went through the target's protection for part of its damage
    /// (`-zbroken|<target>`: Piercing Drill, the Champions mod's Unseen
    /// Fist).
    ZBroken {
        /// The protected Pokémon hit.
        target: Actor,
    },

    /// A volatile or type change an ability started (`-start|<actor>|<what>|<arg>|[from]
    /// ability: X|[of] <source>`: Cursed Body's `Disable|<move>`, Cute
    /// Charm's `Attract`, Electromorphosis's `Charge|<move>`, Color Change's
    /// `typechange|<type>`, Poison Puppeteer's `confusion`).
    StartFrom {
        /// The Pokémon the volatile starts on.
        actor: Actor,
        /// The volatile's name as written.
        what: &'static str,
        /// An extra argument after it, if any.
        arg: Option<LineArg>,
        /// The ability responsible and its holder.
        cause: Cause,
    },
    /// A stat stage set by an ability (`-setboost|<target>|atk|12|[from]
    /// ability: Anger Point`).
    SetBoostFrom {
        /// The Pokémon whose stage was set.
        target: Actor,
        /// The stat.
        stat: BoostId,
        /// The number written after the stat (Anger Point writes 12).
        stage: i8,
        /// The ability responsible.
        cause: Cause,
    },
    /// `battle.skillSwap` (Wandering Spirit): `-activate|<source>|Skill
    /// Swap|<target's ability>|<source's ability>|[of] <target>`, the
    /// abilities left empty between allies (`None`).
    SkillSwapActivate {
        /// The Pokémon starting the swap.
        source: Actor,
        /// The target's and the source's abilities before the swap, in that
        /// order; `None` between allies.
        abilities: Option<(AbilityId, AbilityId)>,
        /// The other Pokémon in the swap (the target).
        of: Actor,
    },
    /// An ability made a move fail, with nothing else to say
    /// (`-fail|<target>|[from] ability: Insomnia|[of] <target>`: Rest).
    FailBy {
        /// The Pokémon named on the line.
        target: Actor,
        /// The ability responsible.
        cause: Cause,
    },
    /// Mummy or Lingering Aroma replaced an ability (`-activate|<holder>|ability:
    /// Mummy|<target>|[ability] <old ability>`).
    AbilityReplaced {
        /// The Pokémon with Mummy or Lingering Aroma.
        holder: Actor,
        /// Mummy or Lingering Aroma.
        ability: AbilityId,
        /// The Pokémon whose ability was replaced.
        target: Actor,
        /// Its ability before the replacement.
        old: AbilityId,
    },

    // Ability suppression and transfer.
    /// Gastro Acid suppressed the Pokémon's ability (`-endability`).
    AbilityEnd {
        /// The Pokémon whose ability is suppressed.
        actor: Actor,
    },
    /// Skill Swap swapped `source`'s and `target`'s abilities
    /// (`-activate|<source>|Skill Swap|<target's>|<source's>|[of] <target>`;
    /// between allies the abilities are not shown).
    SkillSwap {
        /// Skill Swap's user.
        source: Actor,
        /// Skill Swap's target.
        target: Actor,
        /// The target's and the source's abilities before the swap, in that
        /// order; `None` between allies.
        abilities: Option<(AbilityId, AbilityId)>,
    },
    /// A field-wide move took effect (`-fieldactivate|move: Teatime`).
    MoveFieldActivate {
        /// The move.
        move_id: MoveId,
    },

    // Mega Evolution.
    /// A Pokémon Mega Evolved (`-mega|<actor>|<species>|<item>`): the
    /// species the opponent sees (the Mega forme's base species) and the
    /// Mega forme's required item (its Mega Stone).
    MegaEvolve {
        /// The Pokémon Mega Evolving.
        actor: Actor,
        /// The Mega forme's base species (`Charizard`).
        species: SpeciesId,
        /// Its Mega Stone; `None` leaves the argument empty.
        item: Option<ItemId>,
    },
    // Move annotations and other log lines.
    /// Showdown's `attrLastMove`: an annotation for the last `|move|` line
    /// (drained events are not amended, as Showdown's sent lines are not).
    MoveAttr {
        /// The annotation.
        attr: MoveAttr,
    },
    /// An animation line (`|-anim|<user>|<move>|<target>`, Showdown's
    /// `addMove('-anim', ...)`): a charge skipped by the weather or Power
    /// Herb, a `smartTarget` move's later hits. Like `|move|`, it becomes
    /// the line [`BattleEvent::MoveAttr`] amends (`[still]` removes it).
    Anim {
        /// The Pokémon using the move.
        user: Actor,
        /// The move animated.
        move_id: MoveId,
        /// The move's target.
        target: Actor,
    },
    /// A player joined (Showdown's `setPlayer`: `|player|<side>|...`), after
    /// the draws for its team's genders. p2's is where Showdown's `start`
    /// writes the rest of the header (`|gen|`, `|tier|`, the rules).
    Player {
        /// The player's side.
        side: SideId,
    },
    /// `|-hint|<text>`.
    Hint {
        /// The hint.
        hint: Hint,
    },
    /// A message line.
    Message {
        /// The message.
        message: Message,
    },
    /// An Illusion broke: `|replace|<actor>|<details>` (the Pokémon's own
    /// name and details).
    Replace {
        /// The Pokémon whose Illusion broke.
        actor: Actor,
        /// Its own species.
        species: SpeciesId,
        /// Its level, shown in the details.
        level: u8,
        /// The Pokémon's own gender.
        gender: crate::team::Gender,
    },
    /// A blank line (`|`, Showdown's `add('')`): the start of the turn loop,
    /// of the residual phase, and before `|win|` / `|tie|`.
    Blank,
    /// A timestamp line (`|t:|`) at the start of the turn loop. Core has no
    /// clock; the renderer supplies the time.
    Timestamp,
    /// `|teamsize|<side>|<size>` as the battle starts.
    TeamSize {
        /// The side.
        side: SideId,
        /// How many Pokémon it has in the battle.
        size: u8,
    },
    /// End-of-turn bookkeeping begins.
    Upkeep,
    /// A side won (`|win|<player name>`).
    Win {
        /// The winning side.
        side: SideId,
    },
    /// The battle ended in a tie (`|tie`).
    Tie,
    /// A random draw of `random(bound)` (bound 0 = raw 32-bit draw). Only
    /// emitted with [`BattleOptions::with_rng_trace`](crate::BattleOptions::with_rng_trace).
    Rng {
        /// The bound passed to `random(bound)`; 0 for a raw 32-bit draw.
        bound: u32,
        /// The value drawn.
        result: u32,
    },
}

/// The event buffer, which can be switched off for training runs.
#[derive(Clone, Debug, Default)]
pub struct EventLog {
    enabled: bool,
    events: Vec<BattleEvent>,
    /// The events before this one came before the resolution going on:
    /// Showdown has sent them (it sends its log whenever it waits for a
    /// choice), so its `attrLastMove` and `retargetLastMove` change them
    /// for no one. A `u32` (clamped), in the padding after `enabled`.
    sent: u32,
}

impl EventLog {
    pub(crate) fn new(enabled: bool) -> EventLog {
        EventLog { enabled, events: Vec::new(), sent: 0 }
    }

    /// A resolution begins: the events so far are sent.
    #[inline]
    pub(crate) fn mark_sent(&mut self) {
        self.sent = self.events.len().min(u32::MAX as usize) as u32;
    }

    pub(crate) fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    #[inline]
    pub(crate) fn push(&mut self, event: BattleEvent) {
        if self.enabled {
            self.events.push(event);
        }
    }

    /// Whether events are recorded (off for training runs).
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The buffered events, oldest first.
    pub fn events(&self) -> &[BattleEvent] {
        &self.events
    }

    /// Clears the target of the most recent `Move` event, as Showdown does
    /// when a move fails before hitting (the `|move|` line loses its target).
    /// Not a line already sent: a future move failing in the residuals
    /// leaves the `|move|` line of a Revival Blessing whose choice came
    /// between as it was.
    pub(crate) fn clear_last_move_target(&mut self) {
        if let Some(BattleEvent::Move { target, .. }) = self.unsent().iter_mut().rev().find(|e| is_move_line(e)) {
            *target = None;
        }
    }

    /// Replaces the target of the most recent `Move` event (Showdown's
    /// `retargetLastMove`), unless it was sent.
    pub(crate) fn retarget_last_move(&mut self, new_target: Actor) {
        if let Some(BattleEvent::Move { target, .. }) = self.unsent().iter_mut().rev().find(|e| is_move_line(e)) {
            *target = Some(new_target);
        }
    }

    /// The events of the resolution going on.
    fn unsent(&mut self) -> &mut [BattleEvent] {
        let from = (self.sent as usize).min(self.events.len());
        &mut self.events[from..]
    }

    /// Removes and returns all buffered events.
    pub fn drain(&mut self) -> std::vec::Drain<'_, BattleEvent> {
        self.sent = 0;
        self.events.drain(..)
    }
}

/// Whether `event` is a `|move|` line (Showdown's `lastMoveLine`).
fn is_move_line(event: &BattleEvent) -> bool {
    matches!(event, BattleEvent::Move { .. } | BattleEvent::MoveNull { .. } | BattleEvent::Anim { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Position, SideId};
    use pikapro_data::gen9::{moves, species};

    fn actor(side: SideId) -> Actor {
        Actor { pos: Position::new(side, 0), species: species::MEW, active: true, id: 0 }
    }

    fn move_line(move_id: MoveId) -> BattleEvent {
        BattleEvent::Move { user: actor(SideId::P1), move_id, target: Some(actor(SideId::P2)), from: None }
    }

    fn targets(log: &EventLog) -> Vec<Option<Actor>> {
        log.events()
            .iter()
            .filter_map(|e| match e {
                BattleEvent::Move { target, .. } => Some(*target),
                _ => None,
            })
            .collect()
    }

    /// `attrLastMove` and `retargetLastMove` don't change a line sent
    /// before the resolution going on, and `drain` starts the log anew.
    #[test]
    fn sent_move_lines_keep_their_targets() {
        let mut log = EventLog::new(true);
        let (foe, ally) = (Some(actor(SideId::P2)), Some(actor(SideId::P1)));
        log.push(move_line(moves::REVIVAL_BLESSING));
        log.mark_sent();
        log.clear_last_move_target();
        log.retarget_last_move(actor(SideId::P1));
        assert_eq!(targets(&log), [foe]);
        log.push(move_line(moves::FUTURE_SIGHT));
        log.retarget_last_move(actor(SideId::P1));
        assert_eq!(targets(&log), [foe, ally]);
        log.clear_last_move_target();
        assert_eq!(targets(&log), [foe, None]);
        assert_eq!(log.drain().count(), 2);
        // After `drain`, nothing counts as sent.
        log.push(move_line(moves::TACKLE));
        log.clear_last_move_target();
        assert_eq!(targets(&log), [None]);
    }
}
