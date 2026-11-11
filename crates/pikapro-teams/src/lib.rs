//! # pikapro-teams
//!
//! Random teams for pikapro, for training and evaluation data. Deterministic
//! from a seed. Teams pass Showdown's validator, except in the random
//! battles, whose teams are Showdown's generator's ([`random_battle`]).
//!
//! Two [`Mode`]s:
//!
//! - [`Mode::Uniform`]: every legal choice drawn uniformly. A Pokédex
//!   number the format allows, one of its formes (a Mega or a Crowned
//!   hero is a forme of its number, registered as its base species holding
//!   the item), an ability among the species' legal ones, an item (the
//!   forme's required one for Arceus, Ogerpon, the Origin formes, Megas,
//!   ...; else any legal item but Mega Stones and the items of the
//!   number's other formes), up to four moves from its learnset
//!   (prevolutions, events and Sketch as the validator allows; each drawn
//!   move is kept if the set stays legal), a nature, EVs (steps of 4, at
//!   most 508) or Stat Points (at most 66) spread at random, IVs that are
//!   31 three times in four (else 0-30), a Tera type (Stellar included) and
//!   a gender by the species' ratio.
//! - [`Mode::Sets`]: competitive-style sets from Showdown's random-battle
//!   data (see [`sets`]): a Pokédex number, a forme, then one of the sets
//!   Showdown's random-battle generator made for it (moves, ability, item,
//!   Tera type), with a spread chosen by its role ([`Spreads`]). In
//!   `gen9randombattle` and `gen9randomdoublesbattle`, Showdown's
//!   random-battle generator itself, exactly (below).
//!
//! Except for the random battles' [`Mode::Sets`] teams, every set is checked with `pikapro-validate` (an exact port of Showdown's
//! validator) and redrawn if it fails; the team rules (Species Clause by
//! Pokédex number, Item Clause, restricted legendaries) are kept while
//! drawing. [`Redraws`] counts what forced each redraw. A set is kept as
//! the validator leaves it, which is what Showdown's server plays
//! (`pikapro_validate::Checked::team`: Keldeo with Secret Sword is
//! Keldeo-Resolute), so the engine plays a generated team as Showdown
//! would.
//!
//! | preset | validated as | [`Mode::Sets`] data |
//! |---|---|---|
//! | `championsvgc` | `gen9championsvgc2026regmc` | Champions random battles |
//! | `championsbss` | `gen9championsbssregmc` | Champions random battles |
//! | `gen9vgc` | `gen9vgc2025regi` | Gen 9 random doubles |
//! | `gen9doubles` | `gen9anythinggoes@@@Species Clause` (Smogon's legality floor) | Gen 9 random doubles |
//! | `gen9singles` | `gen9anythinggoes@@@Species Clause` (Smogon's legality floor) | Gen 9 random battles |
//! | `gen9ou` | `gen9ou` ([Gen 9] OU) | Gen 9 random battles |
//! | `gen9randombattle` | `gen9anythinggoes@@@Species Clause` ([`Mode::Uniform`] only) | Showdown's generator |
//! | `gen9randomdoublesbattle` | `gen9anythinggoes@@@Species Clause` ([`Mode::Uniform`] only) | Showdown's generator |
//!
//! In `gen9ou` the tier's bans are the validator's like any other rule: a
//! Pokémon of the Uber or AG tier is never drawn in [`Mode::Uniform`], nor
//! a banned ability, item or move (Baton Pass, an OHKO or sleep move,
//! ...); in [`Mode::Sets`], a random-battle set the tier doesn't allow (an
//! Uber, a set with Spore or Tera Blast) is redrawn.
//!
//! The random battles' [`Mode::Sets`] teams are Showdown's
//! (`[Gen 9] Random Battle`, `[Gen 9] Random Doubles Battle`): the team
//! for a seed is the team Showdown's generator gives for Showdown's seed
//! `gen5,<seed in 16 hex digits>` ([`TeamGenerator::seed`]), and
//! [`TeamGenerator::generate_from`] takes any of Showdown's seeds. They
//! aren't validated, as Showdown validates no generated team, take no
//! [`Options`], and can be shiny ([`Generated::shiny`]).
//!
//! Sets are level 50 in the VGC presets and 100 in the custom games and
//! in `gen9ou` ([`Options::level`]); teams have six Pokémon
//! ([`Options::team_size`]).
//! One generator serves many threads; build it once per preset and mode
//! (a fraction of a second for a uniform one, whose species' legal moves
//! are then worked out on first use) and draw a team per seed.
//!
//! ```
//! use pikapro::Format;
//! use pikapro_teams::{Mode, TeamGenerator};
//!
//! let generator = TeamGenerator::new(&Format::champions_vgc(), Mode::Uniform).unwrap();
//! let team = generator.team(2_300_000).unwrap();
//! assert_eq!(team.sets.len(), 6);
//! assert!(generator.validator().validate(&team).is_ok());
//! assert_eq!(team, generator.team(2_300_000).unwrap());
//! ```

#![warn(missing_docs)]

mod competitive;
mod feasible;
mod pools;
pub mod random_battle;
pub mod sets;
mod spread;
mod uniform;

use std::collections::BTreeMap;
use std::fmt;

use pikapro::{BattleRng, Format, GameType, Investment, Mechanics, PokemonSet, Seed, Team};
use pikapro_data::{AbilityId, ItemId, MoveId, SpeciesId};
use pikapro_validate::rules::TeamRule;
use pikapro_validate::{ProblemKind, ValidationFormat, Validator};

use crate::competitive::SetsPool;
use crate::pools::Pools;
use crate::random_battle::RandomBattle;
use crate::uniform::UniformPool;

/// How sets are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Uniformly among the legal choices.
    Uniform,
    /// Competitive-style sets from Showdown's random-battle data.
    Sets,
}

impl Mode {
    /// Both modes.
    pub const ALL: [Mode; 2] = [Mode::Uniform, Mode::Sets];

    /// Its name (`"uniform"`, `"sets"`).
    pub const fn name(self) -> &'static str {
        match self {
            Mode::Uniform => "uniform",
            Mode::Sets => "sets",
        }
    }

    /// The mode named `name`.
    pub fn from_name(name: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.name() == name)
    }
}

/// How [`Mode::Sets`] invests a set's EVs or Stat Points, IVs and nature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Spreads {
    /// By its role and moves: 252 / 252 / 4 into its attacking stat and
    /// Speed (HP under Trick Room) with a matching nature, or HP and the
    /// defences for supports (32 / 32 / 2 and 32 / 17 / 17 Stat Points);
    /// 0 Attack IVs when no move uses Attack, 0 Speed IVs under Trick Room.
    #[default]
    Competitive,
    /// Showdown's random-battle spread: 85 EVs (11 Stat Points) in every
    /// stat except Attack when no move uses it and Speed under Trick Room,
    /// a neutral nature (Serious).
    Showdown,
}

/// Only these species, moves, abilities or items (e.g. what the engine
/// implements); `None` allows everything legal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Restrict {
    /// The species allowed.
    pub species: Option<Vec<SpeciesId>>,
    /// The moves allowed.
    pub moves: Option<Vec<MoveId>>,
    /// The abilities allowed.
    pub abilities: Option<Vec<AbilityId>>,
    /// The items allowed.
    pub items: Option<Vec<ItemId>>,
}

/// Generator options.
///
/// [`TeamGenerator::with_options`] rejects options no team can satisfy: a
/// level the validation format doesn't allow, or restrictions that leave
/// too few Pokédex numbers for a team under Species Clause, Item Clause and
/// the restricted-legendary limit. In [`Mode::Uniform`] a set holds no item
/// when every allowed item is taken (Item Clause) or would make it another
/// forme; `items: Some(vec![])` gives item-less sets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// The sets' level. `None`: 50 for the VGC presets, 100 for the custom
    /// games, `gen9ou` and the random battles' [`Mode::Uniform`]. A format
    /// with `Adjust Level` plays every set at that level (the validated
    /// team carries it).
    pub level: Option<u8>,
    /// Pokémon per team (at most 6). `None`: 6.
    pub team_size: Option<usize>,
    /// How [`Mode::Sets`] invests its sets.
    pub spreads: Spreads,
    /// What the sets may use.
    pub restrict: Restrict,
}

/// Why a generator can't be built, or a team drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a preset (`gen9singles`, `gen9doubles`, `gen9vgc`, `gen9ou`,
    /// `gen9randombattle`, `gen9randomdoublesbattle`, `championsvgc`,
    /// `championsbss`).
    UnknownFormat(String),
    /// Fewer legal Pokédex numbers than the team size (with [`Restrict`]).
    TooFewSpecies {
        /// The legal Pokédex numbers.
        available: usize,
        /// The team size asked for.
        team_size: usize,
    },
    /// The team size is 0, above 6, or below the format's minimum.
    TeamSize(usize),
    /// A level outside `min..=max`, the validation format's range.
    Level {
        /// The level asked for.
        level: u8,
        /// The lowest level allowed.
        min: u8,
        /// The highest level allowed.
        max: u8,
    },
    /// Species Clause, Item Clause and the restricted limit together allow
    /// at most `available` of the Pokémon [`Restrict`] leaves in a team.
    TeamRules {
        /// The most Pokémon a team can have.
        available: usize,
        /// The team size asked for.
        team_size: usize,
    },
    /// No legal team after `draws` draws (every set drawn was rejected).
    NoTeam {
        /// The draws made.
        draws: u32,
    },
    /// Options for a random battle's [`Mode::Sets`]: Showdown's generator
    /// takes none.
    RandomBattleOptions,
    /// Showdown's random-battle generator throws for this seed (its
    /// message): it can't build a team.
    Generator(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnknownFormat(id) => write!(f, "no team rules for format {id:?}"),
            Error::TooFewSpecies { available, team_size } => {
                write!(f, "only {available} legal species for teams of {team_size}")
            }
            Error::TeamSize(n) => write!(f, "can't build teams of {n}"),
            Error::Level { level, min, max } => write!(f, "level {level} is outside {min} to {max}"),
            Error::TeamRules { available, team_size } => write!(
                f,
                "Species Clause, Item Clause and the restricted limit allow only {available} of these Pokémon \
                 in a team, not {team_size}"
            ),
            Error::NoTeam { draws } => write!(f, "no legal team after {draws} draws"),
            Error::RandomBattleOptions => {
                write!(f, "a random battle's sets are Showdown's generator's, which takes no options")
            }
            Error::Generator(message) => write!(f, "Showdown's random-battle generator throws: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// What forced redraws while building teams, by rule: the team rules
/// (`Species Clause`, `Item Clause`, `Restricted`), `Forme item` (an item
/// that belongs to another forme), and the validator's problem kinds for
/// sets that failed (`CantLearn:Incompatible`, `Event:Gender`, ...).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Redraws {
    counts: BTreeMap<String, u64>,
}

impl Redraws {
    pub(crate) fn add(&mut self, rule: &str) {
        match self.counts.get_mut(rule) {
            Some(n) => *n += 1,
            None => {
                self.counts.insert(rule.to_owned(), 1);
            }
        }
    }

    pub(crate) fn add_problems(&mut self, problems: &[ProblemKind]) {
        for p in problems {
            self.add(&problem_name(p));
        }
    }

    /// Adds another count in.
    pub fn merge(&mut self, other: &Redraws) {
        for (k, n) in &other.counts {
            *self.counts.entry(k.clone()).or_default() += n;
        }
    }

    /// `(rule, redraws)`, by rule name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, u64)> {
        self.counts.iter().map(|(k, &n)| (k.as_str(), n))
    }

    /// Every redraw.
    pub fn total(&self) -> u64 {
        self.counts.values().sum()
    }
}

/// A problem kind's name: its variant, and the variant of what it wraps
/// (`CantLearn:Incompatible`, `Event:Iv`, `Forme:NeedsItem`).
pub fn problem_name(k: &ProblemKind) -> String {
    fn head(debug: String) -> String {
        let end = debug.find([' ', '{', '(']).unwrap_or(debug.len());
        debug[..end].to_owned()
    }
    let inner = match k {
        ProblemKind::CantLearn { why, .. } => Some(head(format!("{why:?}"))),
        ProblemKind::Event { problem, .. } => Some(head(format!("{problem:?}"))),
        ProblemKind::Forme(f) => Some(head(format!("{f:?}"))),
        ProblemKind::Go(g) => Some(head(format!("{g:?}"))),
        _ => None,
    };
    let outer = head(format!("{k:?}"));
    match inner {
        Some(i) => format!("{outer}:{i}"),
        None => outer,
    }
}

/// A generated team and what forced redraws while building it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generated {
    /// The team, as the validator leaves it, except for random-battle
    /// [`Mode::Sets`] teams, which bypass validation.
    pub team: Team,
    /// Which sets are shiny, by set (the random battles' generator draws
    /// it; never in other teams).
    pub shiny: Vec<bool>,
    /// The redraws, by rule.
    pub redraws: Redraws,
}

/// Builds random teams for one format and mode. Teams pass validation
/// except for random-battle [`Mode::Sets`] teams (see the crate docs).
///
/// Cheap to share between threads (`Send + Sync`); each species' legal
/// moves are worked out on first use and cached, which never changes what
/// a seed gives.
#[derive(Debug)]
pub struct TeamGenerator {
    format: Format,
    mode: Mode,
    options: Options,
    pub(crate) validator: Validator,
    pub(crate) level: u8,
    team_size: usize,
    pub(crate) item_clause: bool,
    restricted_limit: Option<u8>,
    pub(crate) pools: Pools,
    uniform: Option<UniformPool>,
    sets: Option<SetsPool>,
    /// A random battle's [`Mode::Sets`]: Showdown's generator.
    random: Option<RandomBattle>,
}

impl TeamGenerator {
    /// Showdown's `validateSet` rejects a level-50 Pokémon in a format that
    /// allows higher levels and doesn't adjust them (the custom games) when
    /// its EV total is a multiple of 4, taking it for a VGC set brought by
    /// mistake; one more EV "tells us that it wasn't". This adds it.
    pub(crate) fn mark_level_50(&self, investment: Investment) -> Investment {
        let f = self.validator.format();
        let Investment::Evs { mut evs, ivs } = investment else { return investment };
        let total: u32 = evs.0.iter().map(|&e| e as u32).sum();
        let limit = f.ev_limit.map_or(u32::MAX, u32::from);
        if self.level != 50 || f.adjust_level.is_some() || f.max_level == 50 || limit == 0 || !total.is_multiple_of(4) {
            return investment;
        }
        if total < limit
            && let Some(e) = evs.0.iter_mut().find(|e| **e < 252)
        {
            *e += 1;
        }
        Investment::Evs { evs, ivs }
    }

    /// A generator for a pikapro preset with the default [`Options`].
    pub fn new(format: &Format, mode: Mode) -> Result<TeamGenerator, Error> {
        TeamGenerator::with_options(format, mode, Options::default())
    }

    /// A generator for a pikapro preset, picked by [`Format::id`]
    /// (see [`Options`] for what it rejects).
    pub fn with_options(format: &Format, mode: Mode, options: Options) -> Result<TeamGenerator, Error> {
        let vformat =
            ValidationFormat::for_preset(format.id).ok_or_else(|| Error::UnknownFormat(format.id.to_owned()))?;
        // The VGC rulesets adjust levels to 50; Anything Goes and OU keep 100.
        let level = options.level.unwrap_or(vformat.adjust_level.map_or(100, |l| l as u8));
        // The engine takes levels 1 to 100 too.
        let (min, max) = (vformat.min_level.clamp(1, 100) as u8, vformat.max_level.clamp(1, 100) as u8);
        if !(min..=max).contains(&level) {
            return Err(Error::Level { level, min, max });
        }
        let team_size = options.team_size.unwrap_or(6);
        if team_size == 0
            || team_size > 6
            || team_size < vformat.min_team_size as usize
            || team_size < format.team_size.0 as usize
        {
            return Err(Error::TeamSize(team_size));
        }
        let random = (mode == Mode::Sets && format.random_teams).then(|| RandomBattle::for_format(format.id)).flatten();
        if random.is_some() && options != Options::default() {
            return Err(Error::RandomBattleOptions);
        }
        let item_clause = vformat.team_rules.iter().any(|r| matches!(r, TeamRule::ItemClause(_)));
        let restricted_limit = vformat.team_rules.iter().find_map(|r| match r {
            TeamRule::LimitRestricted(n) => Some(*n),
            _ => None,
        });
        let validator = Validator::new(vformat);
        let pools = Pools::new(format, &options.restrict);
        let mut g = TeamGenerator {
            format: format.clone(),
            mode,
            options,
            validator,
            level,
            team_size,
            item_clause,
            restricted_limit,
            pools,
            uniform: None,
            sets: None,
            random,
        };
        if random.is_some() {
            return Ok(g);
        }
        let numbers = match mode {
            Mode::Uniform => {
                let pool = UniformPool::new(&g);
                let n = pool.number_items(&g);
                g.uniform = Some(pool);
                n
            }
            Mode::Sets => {
                let pool = SetsPool::new(&g, sets_table(format));
                let n = pool.number_items(&g);
                g.sets = Some(pool);
                n
            }
        };
        if numbers.len() < team_size {
            return Err(Error::TooFewSpecies { available: numbers.len(), team_size });
        }
        let available = feasible::max_team(&numbers, g.item_clause, g.restricted_limit, team_size);
        if available < team_size {
            return Err(Error::TeamRules { available, team_size });
        }
        Ok(g)
    }

    /// The format.
    pub fn format(&self) -> &Format {
        &self.format
    }

    /// The mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The options.
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// The supplied-team validator for this preset. Generated teams pass it
    /// except for random-battle [`Mode::Sets`] teams, which bypass validation
    /// as in Showdown and may fail this validator.
    pub fn validator(&self) -> &Validator {
        &self.validator
    }

    /// The Showdown validation format for supplied teams. Generated
    /// random-battle [`Mode::Sets`] teams bypass it (see [`Self::validator`]).
    pub fn validation_format(&self) -> &ValidationFormat {
        self.validator.format()
    }

    /// Pokédex numbers the generator draws from (0 for Showdown's
    /// random-battle generator).
    pub fn species_numbers(&self) -> usize {
        match (&self.uniform, &self.sets) {
            (Some(u), _) => u.numbers(),
            (_, Some(s)) => s.numbers(),
            _ => 0,
        }
    }

    /// Showdown's random-battle generator, when this is a random battle's
    /// [`Mode::Sets`].
    pub fn random_battle(&self) -> Option<RandomBattle> {
        self.random
    }

    /// The seed [`team`](Self::team) draws `seed`'s team from: the fast
    /// backend's, or for Showdown's random-battle generator Showdown's
    /// Gen 5 seed `gen5,<seed in 16 hex digits>`
    /// ([`random_battle::gen5_seed`]), so Showdown gives the same team for
    /// it.
    pub fn seed(&self, seed: u64) -> Seed {
        if self.random.is_some() { random_battle::gen5_seed(seed) } else { Seed::fast(seed) }
    }

    /// The team for `seed` (drawn from [`seed`](Self::seed)'s stream).
    ///
    /// An error ([`Error::NoTeam`]) only if every draw's set is rejected,
    /// which the checks of [`with_options`](Self::with_options) leave to
    /// restrictions whose species can't have a legal set (no legal move
    /// among [`Restrict::moves`], ...); [`Error::Generator`] where
    /// Showdown's random-battle generator throws.
    pub fn team(&self, seed: u64) -> Result<Team, Error> {
        Ok(self.generate(seed)?.team)
    }

    /// The team for `seed`, with its shiny sets and what forced redraws.
    pub fn generate(&self, seed: u64) -> Result<Generated, Error> {
        self.generate_from(&mut BattleRng::new(self.seed(seed)))
    }

    /// A team drawn from `rng`, with its shiny sets and what forced
    /// redraws. For Showdown's random-battle generator, `rng` is the PRNG
    /// of the player's seed (`Seed::ShowdownGen5` or `Seed::Sodium`, as
    /// Showdown's seed strings give them).
    pub fn generate_from(&self, rng: &mut BattleRng) -> Result<Generated, Error> {
        let mut redraws = Redraws::default();
        if let Some(format) = self.random {
            let sets = random_battle::generate(format, rng).map_err(|e| Error::Generator(e.message))?;
            let shiny = sets.iter().map(|s| s.shiny).collect();
            return Ok(Generated { team: random_battle::to_team(&sets), shiny, redraws });
        }
        let team = self.team_from(rng, &mut redraws)?;
        let shiny = vec![false; team.sets.len()];
        Ok(Generated { team, shiny, redraws })
    }

    /// A team drawn from `rng`, adding to `redraws` (see [`team`](Self::team)
    /// for the error and [`generate_from`](Self::generate_from) for
    /// Showdown's random-battle generator, which forces no redraws).
    pub fn team_from(&self, rng: &mut BattleRng, redraws: &mut Redraws) -> Result<Team, Error> {
        if let Some(format) = self.random {
            let sets = random_battle::generate(format, rng).map_err(|e| Error::Generator(e.message))?;
            return Ok(random_battle::to_team(&sets));
        }
        const MAX_DRAWS: u32 = 100_000;
        let mut sets: Vec<PokemonSet> = Vec::with_capacity(self.team_size);
        let mut numbers: Vec<usize> = Vec::with_capacity(self.team_size);
        let mut restricted = 0;
        let mut draws = 0;
        while sets.len() < self.team_size {
            draws += 1;
            if draws >= MAX_DRAWS {
                return Err(Error::NoTeam { draws });
            }
            let used_items: Vec<ItemId> =
                if self.item_clause { sets.iter().filter_map(|s| s.item).collect() } else { Vec::new() };
            let drawn = match (&self.uniform, &self.sets) {
                (Some(pool), _) => pool.draw(self, rng, &numbers, &used_items, redraws),
                (_, Some(pool)) => pool.draw(self, rng, &numbers, &used_items, redraws),
                _ => unreachable!("a generator has a pool"),
            };
            let Some((number, set)) = drawn else { continue };
            if self.validator.format().is_restricted_species(set.species) {
                if self.restricted_limit.is_some_and(|limit| restricted >= limit) {
                    redraws.add("Restricted");
                    continue;
                }
                restricted += 1;
            }
            numbers.push(number);
            sets.push(set);
        }
        Ok(Team::new(sets))
    }
}

/// The [`Mode::Sets`] table for a format: Champions' random-battle sets
/// under Champions mechanics, else Gen 9's singles or doubles sets.
fn sets_table(format: &Format) -> &'static sets::SetTable {
    match (format.mechanics, format.game_type) {
        (Mechanics::Champions, _) => &sets::CHAMPIONS,
        (Mechanics::ScarletViolet, GameType::Singles) => &sets::GEN9,
        (Mechanics::ScarletViolet, GameType::Doubles) => &sets::GEN9_DOUBLES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn generator_is_send_and_sync() {
        assert_send_sync::<TeamGenerator>();
    }

    #[test]
    fn unknown_formats_are_errors() {
        let mut f = Format::gen9_vgc();
        f.id = "gen9ubers";
        assert_eq!(TeamGenerator::new(&f, Mode::Uniform).err(), Some(Error::UnknownFormat("gen9ubers".into())));
        let o = Options { team_size: Some(4), ..Options::default() };
        assert_eq!(
            TeamGenerator::with_options(&Format::champions_vgc(), Mode::Sets, o).err(),
            Some(Error::TeamSize(4))
        );
    }
}

/// The README's examples, run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
