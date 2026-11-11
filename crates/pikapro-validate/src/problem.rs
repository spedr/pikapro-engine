//! Typed validation problems and their rendering in Showdown's words.
//!
//! Each [`ProblemKind`] is one message of Showdown's team validator (a few
//! render as two lines, as Showdown returns them). Names that Showdown
//! derives from the set (its nickname, the species it was given, the forme
//! the validator moved it to) are resolved by the validator and carried as
//! text; everything else is an ID.

use pikapro_data::legality::EventGender;
use pikapro_data::{AbilityId, Dex, ItemId, MoveId, Nature, SpeciesId, Stat, Type, to_id};

use crate::rules::{Reason, SetRule, Tag};

/// One problem found in a team.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// The set it concerns (registration index), `None` for team problems.
    pub set: Option<usize>,
    /// What the problem is.
    pub kind: ProblemKind,
}

/// A thing a tag rule can reject.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Thing {
    /// A species, named `set.name` or `set.name (Species)`.
    Species(SpeciesId),
    /// An item, named `<set>'s item <item>`.
    Item(ItemId),
    /// A move, named `<set>'s move <move>`.
    Move(MoveId),
}

/// Why a move can't be learned (`checkCanLearn`'s `cantLearnReason` and
/// its other results).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CantLearn {
    /// ` can't learn any moves at all.`
    NoLearnset,
    /// ` can't learn <move>.`
    Move(MoveId),
    /// `'s move <move> can't be Sketched.`
    CantSketch(MoveId),
    /// `'s move <move> is incompatible with <moves>.`
    IncompatibleWith(MoveId, Vec<MoveId>),
    /// `'s move <move> can't be transferred from Gen <from> to <to>.`
    Transfer(MoveId, u8, u8),
    /// `'s move <move> is from a <prevo> that can't be transferred to USUM to evolve into <species>.`
    Usum(MoveId, SpeciesId, SpeciesId),
    /// `'s move <move> can't be transferred from Gen 3 to 4 because it's an HM move.` (and Gen 4 to 5)
    Hm(MoveId, u8, u8),
    /// `'s move <move> is learned at level <level>.`
    Level(MoveId, u16),
    /// ` can't Sketch <move> and <other> because it can only Sketch 1 move.`
    OneSketch(MoveId, MoveId),
    /// ` can't simultaneously transfer Defog and Whirlpool from Gen 4 to 5.`
    DefogWhirlpool,
    /// `'s moves <moves> are incompatible.`
    Incompatible(Vec<MoveId>),
}

/// The problems `validateForme` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormeProblem {
    /// `Necrozma-Ultra must start the battle holding Ultranecrozium Z.`
    NecrozmaUltraItem,
    /// `Necrozma-Ultra must start the battle as Necrozma-Dusk-Mane or Necrozma-Dawn-Wings ...`
    NecrozmaUltraStart,
    /// `<species> must start the battle as Zygarde or Zygarde-10% with Power Construct...`
    ZygardeStart(SpeciesId),
    /// `Zygarde holding Zygardite can only Mega Evolve with the Power Construct ability.`
    ZygarditeAbility,
    /// `<species> transforms in-battle with <ability>, please fix its ability.`
    TransformAbility(SpeciesId, AbilityId),
    /// `... with <item>, please fix its item.` (Showdown's `requiredItem`,
    /// `undefined` when the species has only `requiredItems`).
    TransformItem(SpeciesId, Option<ItemId>),
    /// `... with <move>, please fix its moves.`
    TransformMove(SpeciesId, MoveId),
    /// `<name> needs to hold <items>.` (Arceus, Silvally) and the revert line.
    NeedsItemArceus {
        /// The set's name in the message.
        name: String,
        /// The species.
        species: SpeciesId,
    },
    /// `<name> needs to hold <items> to be in its <forme> forme.` and the revert line.
    NeedsItem {
        /// The set's name in the message.
        name: String,
        /// The species.
        species: SpeciesId,
        /// The forme it reverts to.
        revert: String,
    },
    /// `<name> needs to know the move <move> to be in its <forme> forme.` and the revert line.
    NeedsMove {
        /// The set's name in the message.
        name: String,
        /// The species.
        species: SpeciesId,
        /// The move.
        move_: MoveId,
        /// The forme it reverts to.
        revert: String,
    },
    /// `In Generation 9, Hoopa cannot run Hyperspace Fury ...`
    HoopaFury,
    /// `In Generation 9, Hoopa-Unbound cannot run Hyperspace Hole ...`
    HoopaUnboundHole,
}

/// `validateEvent`'s `because` argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Because {
    /// ` to be underleveled`
    Underleveled,
    /// ` because it has a move only available`
    MoveOnlyAvailable,
    /// ` to be`
    ToBe,
}

/// Where an event comes from (`validateEvent`'s `from`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum From {
    /// `from an event`
    AnEvent,
    /// `from its event` / `from its event #<n>`
    ItsEvent(Option<usize>),
    /// `from Gen 1-2 Virtual Console transfer`
    VirtualConsole,
    /// `from Gen 7 Let's Go! HOME transfer`
    LetsGo,
    /// `from Gen 5 Dream World`
    DreamWorld,
}

/// A requirement of an event the set doesn't meet (`validateEvent`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventProblem {
    /// `must be at least level <n>`.
    Level(u8),
    /// `must be shiny` (true) / `must not be shiny` (false).
    Shiny(bool),
    /// `'s gender must be <M|F>`.
    Gender(EventGender),
    /// `must have <n> <stat> IVs`.
    Iv(Stat, u8),
    /// `can only have Hidden Power <type>`.
    HiddenPower(Type),
    /// `must have at least <n> perfect IVs`.
    PerfectIvs(u8),
    /// `can't use Hidden Power Fighting because it must have at least three perfect IVs`.
    HiddenPowerFighting,
    /// `can only use Hidden Power Dark/Dragon/Electric/Steel/Ice because it must have at least 5 perfect IVs`.
    HiddenPowerFivePerfect,
    /// `must not have moves only learnable in gen <n>`.
    MovesOnlyGen(u8),
    /// `<name> can only have one Dream World move.`
    OneDreamWorld,
}

/// A reason a set can't come from Pokémon GO (`validatePokemonGo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoProblem {
    /// `<name> is not obtainable from Pokemon GO.`
    NotObtainable(String),
    /// `<name> must be at least level <n> to be from Pokemon GO.`
    Level(String, u8),
    /// `<name> must have at least <n> IV(s) in non-Speed stats to be from Pokemon GO.`
    MinIvs(String, u8),
    /// `<name> must have odd non-Speed IVs to be from Pokemon GO.`
    OddIvs(String),
    /// `<name>'s Atk and Sp. Atk IVs must match to be from Pokemon GO.`
    AtkSpa(String),
    /// `<name>'s Def and Sp. Def IVs must match to be from Pokemon GO.`
    DefSpd(String),
    /// `<name>'s move(s) <moves> is/are incompatible with a Pokemon GO origin.`
    IncompatibleMoves(String, Vec<MoveId>),
    /// `<name> has moves which are incompatible with a Pokemon GO origin.`
    Incompatible(String),
}

/// The line introducing a list of [`GoProblem`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GoIntro {
    /// `It failed to validate as a Pokemon from Pokemon GO because:`
    Failed,
    /// `Additionally, it failed to validate as a Pokemon from Pokemon GO because:`
    Additionally,
    /// `<name> is only obtainable from Pokemon GO, and failed to validate because:`
    OnlyFromGo,
}

/// Every problem Showdown's validator reports for the supported formats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProblemKind {
    /// Not one of Showdown's (its sets hold names): the set has an ID the
    /// format's dex doesn't (a hand-built [`pikapro::PokemonSet`]; sets
    /// read from text always have known IDs). The set isn't checked further.
    /// `error` is [`SetError::UnknownSpecies`](pikapro::SetError),
    /// `UnknownAbility`, `UnknownItem` or `UnknownMove`.
    UnknownId {
        /// Which kind of ID is unknown.
        error: pikapro::SetError,
        /// The unknown ID's number.
        id: u16,
    },

    // ---- team size
    /// `You must bring at least <min> Pokémon (your team has <len>).`
    TeamTooSmall {
        /// The format's minimum team size.
        min: u8,
        /// The team's size.
        len: usize,
    },
    /// `You may only bring up to <max> Pokémon (your team has <len>).`
    TeamTooLarge {
        /// The format's maximum team size.
        max: u8,
        /// The team's size.
        len: usize,
    },

    // ---- set basics
    /// Two lines: the nickname and its length in UTF-16 units.
    NicknameTooLong {
        /// The species.
        species: SpeciesId,
        /// The nickname.
        nickname: String,
        /// Its length in UTF-16 units.
        len: usize,
    },
    /// `<name> (level <level>) is below the minimum level of <min>`
    LevelBelowMin {
        /// The set's name in the message.
        name: String,
        /// The set's level.
        level: u16,
        /// The format's minimum level.
        min: u16,
    },
    /// `<name> (level <level>) is above the maximum level of <max>`
    LevelAboveMax {
        /// The set's name in the message.
        name: String,
        /// The set's level.
        level: u16,
        /// The format's maximum level.
        max: u16,
    },
    /// A problem of `validateForme`.
    Forme(FormeProblem),
    /// `<base species> has multiple copies of <move>.`
    DuplicateMove {
        /// The base species.
        base: SpeciesId,
        /// The move.
        move_: MoveId,
    },
    /// `Battle Bond Greninja must be male.`
    BattleBondMale,
    /// `<species>'s Terastal type needs to be <type>.`
    TeraTypeRequired {
        /// The species.
        species: SpeciesId,
        /// The required Tera type.
        required: Type,
    },
    /// `<species> is <reason>.`
    SpeciesBanned {
        /// The species.
        species: SpeciesId,
        /// Why it is banned.
        reason: Reason,
    },
    /// `<move> is banned by <rule>.`: a rule's `onValidateSet` (OHKO
    /// Clause, Sleep Moves Clause).
    MoveBannedByClause {
        /// The move.
        move_: MoveId,
        /// The rule.
        rule: SetRule,
    },
    /// `<thing> is tagged <tag>, which is <reason>.`
    Tagged {
        /// The set's name in the message.
        name: String,
        /// What is rejected.
        thing: Thing,
        /// The tag.
        tag: Tag,
        /// Why the tag is banned (`None`: banned).
        reason: Option<Reason>,
    },
    /// `<thing> is not obtainable without hacking or glitches[ in Gen <n>].`
    Unobtainable {
        /// The set's name in the message.
        name: String,
        /// What is rejected.
        thing: Thing,
        /// The generation the message names, if any.
        in_gen: Option<u8>,
    },
    /// `<thing> does not exist in Gen <n>.`
    DoesNotExist {
        /// The set's name in the message.
        name: String,
        /// What is rejected.
        thing: Thing,
        /// The generation.
        generation: u8,
    },
    /// `<set>'s item <item> is <reason>.`
    ItemBanned {
        /// The set's name in the message.
        name: String,
        /// The item.
        item: ItemId,
        /// Why it is banned.
        reason: Reason,
    },
    /// `<set> not holding an item is <reason>.`
    NoItemBanned {
        /// The set's name in the message.
        name: String,
        /// Why it is banned.
        reason: Reason,
    },
    /// `<set>'s move <move> is <reason>.`
    MoveBanned {
        /// The set's name in the message.
        name: String,
        /// The move.
        move_: MoveId,
        /// Why it is banned.
        reason: Reason,
    },
    /// `<set>'s nature <nature> is <reason>.`
    NatureBanned {
        /// The set's name in the message.
        name: String,
        /// The nature.
        nature: Nature,
        /// Why it is banned.
        reason: Reason,
    },
    /// `<set>'s ability <ability> is <reason>.`
    AbilityBanned {
        /// The set's name in the message.
        name: String,
        /// The ability.
        ability: AbilityId,
        /// Why it is banned.
        reason: Reason,
    },
    /// `<set>'s ability <ability> is tagged <nonstandard>, which is <reason>.`
    AbilityTagged {
        /// The set's name in the message.
        name: String,
        /// The ability.
        ability: AbilityId,
        /// The tag.
        tag: Tag,
        /// Why it is banned.
        reason: Reason,
    },
    /// `<set>'s ability <ability> does not exist in Gen <n>.`
    AbilityDoesNotExist {
        /// The set's name in the message.
        name: String,
        /// The ability.
        ability: AbilityId,
        /// The generation.
        generation: u8,
    },
    /// `<name> needs to have an ability.`
    NeedsAbility {
        /// The set's name in the message.
        name: String,
    },
    /// `<name> can't have <ability>.`
    CantHaveAbility {
        /// The set's name in the message.
        name: String,
        /// The ability.
        ability: AbilityId,
    },
    /// `<name> has no moves (it must have at least one to be usable).`
    NoMoves {
        /// The set's name in the message.
        name: String,
    },

    // ---- stats (`validateStats`)
    /// `<name>'s IVs are not maxed out, but this format requires all IVs to be 31.`
    IvsNotMaxed {
        /// The set's name in the message.
        name: String,
    },
    /// `<name>'s Hidden Power type <a> is incompatible with Hidden Power <b>` (no period).
    HiddenPowerTypes {
        /// The set's name in the message.
        name: String,
        /// The Hidden Power type.
        hp: Type,
        /// The type of the Hidden Power move.
        move_type: Type,
    },
    /// `<name> must not have Hidden Power Fighting because it starts with 3 perfect IVs ...`
    HiddenPowerFightingLegendary {
        /// The set's name in the message.
        name: String,
    },
    /// `<name> must have at least three perfect IVs because it's a legendary in Gen 6 or later.`
    LegendaryIvs {
        /// The set's name in the message.
        name: String,
        /// The generation the format requires (`Min Source Gen`), if any.
        this_gen_only: Option<u8>,
    },
    /// `<name> has Hidden Power <hp>, but its IVs are for Hidden Power <iv_hp>.`
    HiddenPowerIvs {
        /// The set's name in the message.
        name: String,
        /// The Hidden Power type.
        hp: Type,
        /// The Hidden Power type its IVs give.
        iv_hp: Type,
    },
    /// `<name> has Hidden Power <hp>, but its IVs don't allow this even with (Bottle Cap) Hyper Training.`
    HiddenPowerBottleCap {
        /// The set's name in the message.
        name: String,
        /// The Hidden Power type.
        hp: Type,
    },
    /// `<name> has more than 32 Stat Points in <stat>.` / `more than 255 EVs`
    StatTooHigh {
        /// The set's name in the message.
        name: String,
        /// The stat.
        stat: Stat,
        /// Stat Points (true) or EVs (false).
        stat_points: bool,
    },
    /// `<name> has exactly 0 Stat Points - did you forget to invest it? ...`
    ZeroStatPoints {
        /// The set's name in the message.
        name: String,
    },
    /// `<name> has exactly 0 EVs - did you forget to EV it? ...`
    ZeroEvs {
        /// The set's name in the message.
        name: String,
    },
    /// `<name> has exactly <n> EVs, but this format does not restrict you to 510 EVs ...`
    Exactly510 {
        /// The set's name in the message.
        name: String,
        /// The EV total.
        total: u32,
    },
    /// `<name> is level 50, but this format allows level <n> Pokémon. ...`
    Level50 {
        /// The set's name in the message.
        name: String,
        /// The format's maximum level.
        max_level: u16,
    },
    /// `<name> has EVs/Stat Points, which is not allowed by this format.`
    InvestmentNotAllowed {
        /// The set's name in the message.
        name: String,
        /// Stat Points (true) or EVs (false).
        stat_points: bool,
    },
    /// `<name> has <n> total EVs/Stat Points, which is more than this format's limit of <limit>.`
    OverLimit {
        /// The set's name in the message.
        name: String,
        /// The EV or Stat Point total.
        total: u32,
        /// The format's limit.
        limit: u16,
        /// Stat Points (true) or EVs (false).
        stat_points: bool,
    },

    // ---- moves (`validateMoves`, `checkCanLearn`)
    /// A move it can't learn: `<name>` and the reason's text.
    CantLearn {
        /// The set's name in the message.
        name: String,
        /// Why it can't learn the move.
        why: CantLearn,
    },
    /// `<name> needs to know <move> to evolve, so it can only know 3 other moves from <prevo>.`
    EvoMoveCarry {
        /// The set's name in the message.
        name: String,
        /// The move it needs to evolve (`None`: a Fairy-type move).
        evo_move: Option<MoveId>,
        /// The pre-evolution its other moves come from.
        prevo: Option<SpeciesId>,
    },
    /// `<name>'s event/egg moves are from an evolution, and are incompatible with its moves from <baby>.`
    BabyOnlyEvolution {
        /// The set's name in the message.
        name: String,
        /// The baby species its other moves come from.
        baby: SpeciesId,
    },
    /// `<name> has moves from before Gen <n>, which are incompatible with its moves from <baby>.`
    BabyOnlyGen {
        /// The set's name in the message.
        name: String,
        /// The baby species its other moves come from.
        baby: SpeciesId,
        /// The generation.
        generation: u8,
    },

    // ---- sources and events
    /// `<name> must be at least level <level> to be evolved.`
    Underleveled {
        /// The set's name in the message.
        name: String,
        /// The level it must reach.
        level: u8,
    },
    /// A requirement of an event (`validateEvent` with a reason).
    Event {
        /// The set's name in the message.
        name: String,
        /// The requirement it doesn't meet.
        problem: EventProblem,
        /// The message's `because` part, if any.
        because: Option<Because>,
        /// Where the event comes from.
        from: From,
    },
    /// `<name> is from a Gen 2 or 3 egg, which cannot be obtained at levels below 5.`
    EggBelowLevel5 {
        /// The set's name in the message.
        name: String,
    },
    /// Two lines: the egg moves (by Showdown ID) and the bug-report line.
    EggMoveCombination {
        /// The set's name in the message.
        name: String,
        /// The egg moves.
        moves: Vec<MoveId>,
    },
    /// `<name> has an event-exclusive move that it doesn't qualify for ...:`
    EventExclusive {
        /// The set's name in the message.
        name: String,
    },
    /// `<species> is only obtainable from an event - it needs to match its event:` (or `events`).
    EventOnly {
        /// The species.
        species: SpeciesId,
        /// Whether it has several events.
        several: bool,
    },
    /// The line introducing the Pokémon GO problems that follow.
    GoIntro {
        /// Which introduction line.
        intro: GoIntro,
        /// The set's name in the message.
        name: String,
    },
    /// A reason the set can't come from Pokémon GO.
    Go(GoProblem),
    /// `<name> must not be nicknamed a different Pokémon species than what it actually is.`
    NicknameSpecies {
        /// The set's name in the message.
        name: String,
    },

    // ---- team rules
    /// Two lines each.
    OneKyurem,
    /// `You cannot have more than one Necrozma-Dusk-Mane` and the N-Solarizer line.
    OneNecrozmaDuskMane,
    /// `You cannot have more than one Necrozma-Dawn-Wings` and the N-Lunarizer line.
    OneNecrozmaDawnWings,
    /// `You cannot have more than one Calyrex-Ice/Calyrex-Shadow.` and the Reins of Unity line.
    OneCalyrex,
    /// `You are limited to one of each Pokémon by Species Clause.` and `(You have more than one <base>)`.
    SpeciesClause {
        /// The base species.
        base: SpeciesId,
    },
    /// `Your Pokémon must have different nicknames.` and `(You have more than one <nickname>)`.
    NicknameClause {
        /// The nickname.
        nickname: String,
    },
    /// `You are limited to <limit> of each item by Item Clause.` and `(You have more than <limit> <item>)`.
    ItemClause {
        /// How many of each item are allowed.
        limit: u8,
        /// The item.
        item: ItemId,
    },
    /// `You can only use one restricted Pokémon (you have: <species>)` (or `up to two`).
    Restricted {
        /// How many restricted Pokémon are allowed.
        limit: u8,
        /// The restricted Pokémon on the team.
        species: Vec<SpeciesId>,
    },
}

const STAT_NAMES: [&str; 6] = ["HP", "Attack", "Defense", "Special Attack", "Special Defense", "Speed"];

impl Problem {
    /// Showdown's message(s) for this problem, as its validator returns them.
    pub fn lines(&self, dex: &Dex) -> Vec<String> {
        self.kind.lines(dex)
    }
}

impl ProblemKind {
    /// Showdown's message(s) for this problem.
    pub fn lines(&self, dex: &Dex) -> Vec<String> {
        let sp = |id: SpeciesId| dex.species(id).name;
        let mv = |id: MoveId| dex.move_(id).name;
        let it = |id: ItemId| dex.item(id).name;
        let ab = |id: AbilityId| dex.ability(id).name;
        let thing = |name: &str, t: Thing| match t {
            Thing::Species(s) if name == sp(s) => name.to_owned(),
            Thing::Species(s) => format!("{name} ({})", sp(s)),
            Thing::Item(i) => format!("{name}'s item {}", it(i)),
            Thing::Move(m) => format!("{name}'s move {}", mv(m)),
        };
        let one = |s: String| vec![s];
        match self {
            ProblemKind::UnknownId { error, id } => {
                let what = match error {
                    pikapro::SetError::UnknownSpecies => "species",
                    pikapro::SetError::UnknownAbility => "ability",
                    pikapro::SetError::UnknownItem => "item",
                    _ => "move",
                };
                one(format!("The {what} with ID {id} does not exist in this format's dex."))
            }
            ProblemKind::TeamTooSmall { min, len } => {
                one(format!("You must bring at least {min} Pokémon (your team has {len})."))
            }
            ProblemKind::TeamTooLarge { max, len } => {
                one(format!("You may only bring up to {max} Pokémon (your team has {len})."))
            }
            ProblemKind::NicknameTooLong { species, nickname, len } => vec![
                format!("{}'s nickname \"{nickname}\" is too long.", sp(*species)),
                format!(
                    "(It's {len} characters long, but should be 18 or less. Some characters, like emojis, may count as more than one.)"
                ),
            ],
            ProblemKind::LevelBelowMin { name, level, min } => {
                one(format!("{name} (level {level}) is below the minimum level of {min}"))
            }
            ProblemKind::LevelAboveMax { name, level, max } => {
                one(format!("{name} (level {level}) is above the maximum level of {max}"))
            }
            ProblemKind::Forme(f) => forme_lines(f, dex),
            ProblemKind::DuplicateMove { base, move_ } => {
                one(format!("{} has multiple copies of {}.", sp(*base), mv(*move_)))
            }
            ProblemKind::BattleBondMale => one("Battle Bond Greninja must be male.".to_owned()),
            ProblemKind::TeraTypeRequired { species, required } => {
                one(format!("{}'s Terastal type needs to be {}.", sp(*species), required.name()))
            }
            ProblemKind::SpeciesBanned { species, reason } => one(format!("{} is {}.", sp(*species), reason.text())),
            ProblemKind::MoveBannedByClause { move_, rule } => {
                one(format!("{} is banned by {}.", mv(*move_), rule.name()))
            }
            ProblemKind::Tagged { name, thing: t, tag, reason } => one(format!(
                "{} is tagged {}, which is {}.",
                thing(name, *t),
                tag.name(),
                reason.map_or_else(|| "banned".to_owned(), Reason::text)
            )),
            ProblemKind::Unobtainable { name, thing: t, in_gen } => one(format!(
                "{} is not obtainable without hacking or glitches{}.",
                thing(name, *t),
                in_gen.map_or_else(String::new, |g| format!(" in Gen {g}"))
            )),
            ProblemKind::DoesNotExist { name, thing: t, generation } => {
                one(format!("{} does not exist in Gen {generation}.", thing(name, *t)))
            }
            ProblemKind::ItemBanned { name, item, reason } => {
                one(format!("{name}'s item {} is {}.", it(*item), reason.text()))
            }
            ProblemKind::NoItemBanned { name, reason } => {
                one(format!("{name} not holding an item is {}.", reason.text()))
            }
            ProblemKind::MoveBanned { name, move_, reason } => {
                one(format!("{name}'s move {} is {}.", mv(*move_), reason.text()))
            }
            ProblemKind::NatureBanned { name, nature, reason } => {
                one(format!("{name}'s nature {} is {}.", nature.name(), reason.text()))
            }
            ProblemKind::AbilityBanned { name, ability, reason } => {
                one(format!("{name}'s ability {} is {}.", ab(*ability), reason.text()))
            }
            ProblemKind::AbilityTagged { name, ability, tag, reason } => {
                one(format!("{name}'s ability {} is tagged {}, which is {}.", ab(*ability), tag.name(), reason.text()))
            }
            ProblemKind::AbilityDoesNotExist { name, ability, generation } => {
                one(format!("{name}'s ability {} does not exist in Gen {generation}.", ab(*ability)))
            }
            ProblemKind::NeedsAbility { name } => one(format!("{name} needs to have an ability.")),
            ProblemKind::CantHaveAbility { name, ability } => one(format!("{name} can't have {}.", ab(*ability))),
            ProblemKind::NoMoves { name } => {
                one(format!("{name} has no moves (it must have at least one to be usable)."))
            }
            ProblemKind::IvsNotMaxed { name } => {
                one(format!("{name}'s IVs are not maxed out, but this format requires all IVs to be 31."))
            }
            ProblemKind::HiddenPowerTypes { name, hp, move_type } => one(format!(
                "{name}'s Hidden Power type {} is incompatible with Hidden Power {}",
                hp.name(),
                move_type.name()
            )),
            ProblemKind::HiddenPowerFightingLegendary { name } => one(format!(
                "{name} must not have Hidden Power Fighting because it starts with 3 perfect IVs because it's a Gen 6+ legendary."
            )),
            ProblemKind::LegendaryIvs { name, this_gen_only } => one(format!(
                "{name} must have at least three perfect IVs because it's a legendary{}.",
                this_gen_only.map_or_else(
                    || " in Gen 6 or later".to_owned(),
                    |g| format!(" and this format requires Gen {g} Pokémon")
                )
            )),
            ProblemKind::HiddenPowerIvs { name, hp, iv_hp } => one(format!(
                "{name} has Hidden Power {}, but its IVs are for Hidden Power {}.",
                hp.name(),
                iv_hp.name()
            )),
            ProblemKind::HiddenPowerBottleCap { name, hp } => one(format!(
                "{name} has Hidden Power {}, but its IVs don't allow this even with (Bottle Cap) Hyper Training.",
                hp.name()
            )),
            ProblemKind::StatTooHigh { name, stat, stat_points } => one(if *stat_points {
                format!("{name} has more than 32 Stat Points in {}.", STAT_NAMES[*stat as usize])
            } else {
                format!("{name} has more than 255 EVs in {}.", STAT_NAMES[*stat as usize])
            }),
            ProblemKind::ZeroStatPoints { name } => one(format!(
                "{name} has exactly 0 Stat Points - did you forget to invest it? (If this was intentional, change your Nature to a different neutral Nature, which won't change its stats but will tell us that it wasn't a mistake)."
            )),
            ProblemKind::ZeroEvs { name } => one(format!(
                "{name} has exactly 0 EVs - did you forget to EV it? (If this was intentional, add exactly 1 to one of your EVs, which won't change its stats but will tell us that it wasn't a mistake)."
            )),
            ProblemKind::Exactly510 { name, total } => one(format!(
                "{name} has exactly {total} EVs, but this format does not restrict you to 510 EVs (If this was intentional, add exactly 1 to one of your EVs, which won't change its stats but will tell us that it wasn't a mistake)."
            )),
            ProblemKind::Level50 { name, max_level } => one(format!(
                "{name} is level 50, but this format allows level {max_level} Pokémon. (If this was intentional, add exactly 1 to one of your EVs, which won't change its stats but will tell us that it wasn't a mistake)."
            )),
            ProblemKind::InvestmentNotAllowed { name, stat_points } => one(format!(
                "{name} has {}, which is not allowed by this format.",
                if *stat_points { "Stat Points" } else { "EVs" }
            )),
            ProblemKind::OverLimit { name, total, limit, stat_points } => one(format!(
                "{name} has {total} total {}, which is more than this format's limit of {limit}.",
                if *stat_points { "Stat Points" } else { "EVs" }
            )),
            ProblemKind::CantLearn { name, why } => one(format!("{name}{}", cant_learn(why, dex))),
            ProblemKind::EvoMoveCarry { name, evo_move, prevo } => one(format!(
                "{name} needs to know {} to evolve, so it can only know 3 other moves from {}.",
                evo_move.map_or("a Fairy-type move", mv),
                prevo.map_or("", sp)
            )),
            ProblemKind::BabyOnlyEvolution { name, baby } => one(format!(
                "{name}'s event/egg moves are from an evolution, and are incompatible with its moves from {}.",
                sp(*baby)
            )),
            ProblemKind::BabyOnlyGen { name, baby, generation } => one(format!(
                "{name} has moves from before Gen {generation}, which are incompatible with its moves from {}.",
                sp(*baby)
            )),
            ProblemKind::Underleveled { name, level } => {
                one(format!("{name} must be at least level {level} to be evolved."))
            }
            ProblemKind::Event { name, problem, because, from } => one(event_line(name, *problem, *because, *from)),
            ProblemKind::EggBelowLevel5 { name } => {
                one(format!("{name} is from a Gen 2 or 3 egg, which cannot be obtained at levels below 5."))
            }
            ProblemKind::EggMoveCombination { name, moves } => vec![
                format!(
                    "{name} can't get its egg move combination ({}) from any possible father.",
                    moves.iter().map(|&m| to_id(mv(m))).collect::<Vec<_>>().join(", ")
                ),
                "(Is this incorrect? If so, post the chainbreeding instructions in Bug Reports)".to_owned(),
            ],
            ProblemKind::EventExclusive { name } => one(format!(
                "{name} has an event-exclusive move that it doesn't qualify for (only one of several ways to get the move will be listed):"
            )),
            ProblemKind::EventOnly { species, several } => one(if *several {
                format!("{} is only obtainable from events - it needs to match one of its events:", sp(*species))
            } else {
                format!("{} is only obtainable from an event - it needs to match its event:", sp(*species))
            }),
            ProblemKind::GoIntro { intro, name } => one(match intro {
                GoIntro::Failed => "It failed to validate as a Pokemon from Pokemon GO because:".to_owned(),
                GoIntro::Additionally => {
                    "Additionally, it failed to validate as a Pokemon from Pokemon GO because:".to_owned()
                }
                GoIntro::OnlyFromGo => {
                    format!("{name} is only obtainable from Pokemon GO, and failed to validate because:")
                }
            }),
            ProblemKind::Go(g) => one(go_line(g, dex)),
            ProblemKind::NicknameSpecies { name } => {
                one(format!("{name} must not be nicknamed a different Pokémon species than what it actually is."))
            }
            ProblemKind::OneKyurem => vec![
                "You cannot have more than one Kyurem-Black/Kyurem-White.".to_owned(),
                "(It's untradeable and you can only make one with the DNA Splicers.)".to_owned(),
            ],
            ProblemKind::OneNecrozmaDuskMane => vec![
                "You cannot have more than one Necrozma-Dusk-Mane".to_owned(),
                "(It's untradeable and you can only make one with the N-Solarizer.)".to_owned(),
            ],
            ProblemKind::OneNecrozmaDawnWings => vec![
                "You cannot have more than one Necrozma-Dawn-Wings".to_owned(),
                "(It's untradeable and you can only make one with the N-Lunarizer.)".to_owned(),
            ],
            ProblemKind::OneCalyrex => vec![
                "You cannot have more than one Calyrex-Ice/Calyrex-Shadow.".to_owned(),
                "(It's untradeable and you can only make one with the Reins of Unity.)".to_owned(),
            ],
            ProblemKind::SpeciesClause { base } => vec![
                "You are limited to one of each Pokémon by Species Clause.".to_owned(),
                format!("(You have more than one {})", sp(*base)),
            ],
            ProblemKind::NicknameClause { nickname } => vec![
                "Your Pokémon must have different nicknames.".to_owned(),
                format!("(You have more than one {nickname})"),
            ],
            ProblemKind::ItemClause { limit, item } => vec![
                format!("You are limited to {limit} of each item by Item Clause."),
                format!("(You have more than {limit} {})", it(*item)),
            ],
            ProblemKind::Restricted { limit, species } => {
                let names: Vec<&str> = species.iter().map(|&s| sp(s)).collect();
                one(if *limit == 1 {
                    format!("You can only use one restricted Pokémon (you have: {})", names.join(", "))
                } else {
                    format!("You can only use up to two restricted Pokémon (you have: {})", names.join(", "))
                })
            }
        }
    }
}

fn forme_lines(f: &FormeProblem, dex: &Dex) -> Vec<String> {
    let sp = |id: SpeciesId| dex.species(id).name;
    let one = |s: String| vec![s];
    match f {
        FormeProblem::NecrozmaUltraItem => one("Necrozma-Ultra must start the battle holding Ultranecrozium Z.".to_owned()),
        FormeProblem::NecrozmaUltraStart => one("Necrozma-Ultra must start the battle as Necrozma-Dusk-Mane or Necrozma-Dawn-Wings holding Ultranecrozium Z. Please specify which Necrozma it should start as.".to_owned()),
        FormeProblem::ZygardeStart(s) => one(format!(
            "{} must start the battle as Zygarde or Zygarde-10% with Power Construct. Please specify which Zygarde it should start as.",
            sp(*s)
        )),
        FormeProblem::ZygarditeAbility => {
            one("Zygarde holding Zygardite can only Mega Evolve with the Power Construct ability.".to_owned())
        }
        FormeProblem::TransformAbility(s, a) => one(format!(
            "{} transforms in-battle with {}, please fix its ability.",
            sp(*s),
            dex.ability(*a).name
        )),
        FormeProblem::TransformItem(s, i) => one(format!(
            "{} transforms in-battle with {}, please fix its item.",
            sp(*s),
            i.map_or("undefined", |i| dex.item(i).name)
        )),
        FormeProblem::TransformMove(s, m) => {
            one(format!("{} transforms in-battle with {}, please fix its moves.", sp(*s), dex.move_(*m).name))
        }
        FormeProblem::NeedsItemArceus { name, species } => vec![
            format!("{name} needs to hold {}.", items_or(dex, *species)),
            "(It will revert to its Normal forme if you remove the item or give it a different item.)".to_owned(),
        ],
        FormeProblem::NeedsItem { name, species, revert } => vec![
            format!("{name} needs to hold {} to be in its {} forme.", items_or(dex, *species), dex.species(*species).forme),
            format!("(It will revert to its {revert} forme if you remove the item or give it a different item.)"),
        ],
        FormeProblem::NeedsMove { name, species, move_, revert } => vec![
            format!(
                "{name} needs to know the move {} to be in its {} forme.",
                dex.move_(*move_).name,
                dex.species(*species).forme
            ),
            format!("(It will revert to its {revert} forme if it forgets the move.)"),
        ],
        FormeProblem::HoopaFury => one("In Generation 9, Hoopa cannot run Hyperspace Fury because it gets replaced with Hyperspace Hole upon changing forme.".to_owned()),
        FormeProblem::HoopaUnboundHole => one("In Generation 9, Hoopa-Unbound cannot run Hyperspace Hole because it gets replaced with Hyperspace Fury upon changing forme.".to_owned()),
    }
}

fn items_or(dex: &Dex, species: SpeciesId) -> String {
    dex.species(species).required_items.iter().map(|&i| dex.item(i).name).collect::<Vec<_>>().join(" or ")
}

fn cant_learn(why: &CantLearn, dex: &Dex) -> String {
    let mv = |id: MoveId| dex.move_(id).name;
    let list = |ms: &[MoveId]| ms.iter().map(|&m| mv(m)).collect::<Vec<_>>().join(", ");
    match why {
        CantLearn::NoLearnset => " can't learn any moves at all.".to_owned(),
        CantLearn::Move(m) => format!(" can't learn {}.", mv(*m)),
        CantLearn::CantSketch(m) => format!("'s move {} can't be Sketched.", mv(*m)),
        CantLearn::IncompatibleWith(m, ms) => format!("'s move {} is incompatible with {}.", mv(*m), list(ms)),
        CantLearn::Transfer(m, from, to) => format!("'s move {} can't be transferred from Gen {from} to {to}.", mv(*m)),
        CantLearn::Usum(m, prevo, species) => format!(
            "'s move {} is from a {} that can't be transferred to USUM to evolve into {}.",
            mv(*m),
            dex.species(*prevo).name,
            dex.species(*species).name
        ),
        CantLearn::Hm(m, from, to) => {
            format!("'s move {} can't be transferred from Gen {from} to {to} because it's an HM move.", mv(*m))
        }
        CantLearn::Level(m, level) => format!("'s move {} is learned at level {level}.", mv(*m)),
        CantLearn::OneSketch(m, other) => {
            format!(" can't Sketch {} and {} because it can only Sketch 1 move.", mv(*m), mv(*other))
        }
        CantLearn::DefogWhirlpool => " can't simultaneously transfer Defog and Whirlpool from Gen 4 to 5.".to_owned(),
        CantLearn::Incompatible(ms) => format!("'s moves {} are incompatible.", list(ms)),
    }
}

fn event_line(name: &str, problem: EventProblem, because: Option<Because>, from: From) -> String {
    let because = match because {
        None => "",
        Some(Because::Underleveled) => " to be underleveled",
        Some(Because::MoveOnlyAvailable) => " because it has a move only available",
        Some(Because::ToBe) => " to be",
    };
    let from = match from {
        From::AnEvent => "from an event".to_owned(),
        From::ItsEvent(None) => "from its event".to_owned(),
        From::ItsEvent(Some(n)) => format!("from its event #{n}"),
        From::VirtualConsole => "from Gen 1-2 Virtual Console transfer".to_owned(),
        From::LetsGo => "from Gen 7 Let's Go! HOME transfer".to_owned(),
        From::DreamWorld => "from Gen 5 Dream World".to_owned(),
    };
    let etc = format!("{because} {from}");
    match problem {
        EventProblem::Level(l) => format!("{name} must be at least level {l}{etc}."),
        EventProblem::Shiny(true) => format!("{name} must be shiny{etc}."),
        EventProblem::Shiny(false) => format!("{name} must not be shiny{etc}."),
        EventProblem::Gender(g) => {
            format!("{name}'s gender must be {}{etc}.", if g == EventGender::Male { "M" } else { "F" })
        }
        EventProblem::Iv(stat, v) => format!("{name} must have {v} {} IVs{etc}.", STAT_NAMES[stat as usize]),
        EventProblem::HiddenPower(t) => format!("{name} can only have Hidden Power {}{etc}.", t.name()),
        EventProblem::PerfectIvs(n) => format!("{name} must have at least {n} perfect IVs{etc}."),
        EventProblem::HiddenPowerFighting => {
            format!("{name} can't use Hidden Power Fighting because it must have at least three perfect IVs{etc}.")
        }
        EventProblem::HiddenPowerFivePerfect => format!(
            "{name} can only use Hidden Power Dark/Dragon/Electric/Steel/Ice because it must have at least 5 perfect IVs{etc}."
        ),
        EventProblem::MovesOnlyGen(g) => format!("{name} must not have moves only learnable in gen {g}{etc}."),
        EventProblem::OneDreamWorld => format!("{name} can only have one Dream World move."),
    }
}

fn go_line(g: &GoProblem, dex: &Dex) -> String {
    match g {
        GoProblem::NotObtainable(name) => format!("{name} is not obtainable from Pokemon GO."),
        GoProblem::Level(name, l) => format!("{name} must be at least level {l} to be from Pokemon GO."),
        GoProblem::MinIvs(name, n) => format!(
            "{name} must have at least {n} {} in non-Speed stats to be from Pokemon GO.",
            if *n == 1 { "IV" } else { "IVs" }
        ),
        GoProblem::OddIvs(name) => format!("{name} must have odd non-Speed IVs to be from Pokemon GO."),
        GoProblem::AtkSpa(name) => format!("{name}'s Atk and Sp. Atk IVs must match to be from Pokemon GO."),
        GoProblem::DefSpd(name) => format!("{name}'s Def and Sp. Def IVs must match to be from Pokemon GO."),
        GoProblem::IncompatibleMoves(name, moves) => {
            let list: Vec<&str> = moves.iter().map(|&m| dex.move_(m).name).collect();
            let (s, verb) = if moves.len() == 1 { ("", "is") } else { ("s", "are") };
            format!("{name}'s move{s} {} {verb} incompatible with a Pokemon GO origin.", list.join(", "))
        }
        GoProblem::Incompatible(name) => format!("{name} has moves which are incompatible with a Pokemon GO origin."),
    }
}
