//! Team *input*: what a player registers before the battle.
//!
//! These types describe sets as a player builds them. The battle converts
//! them into [`PokemonState`](crate::PokemonState) once, at construction;
//! nothing here is touched during simulation.

use core::fmt;

use pikapro_data::{AbilityId, ItemId, MoveId, Nature, SpeciesId, Stat, Stats, Type};

use crate::format::{Format, Gimmick, StatSystem};
use crate::state::{MAX_MOVES, MAX_TEAM};
use crate::stats::stat_points_to_evs;
use crate::util::StackVec;

/// How a set distributes stat investment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Investment {
    /// Scarlet/Violet style EVs and IVs.
    Evs {
        /// Effort values: at most 252 each and 510 in all.
        evs: Stats<u8>,
        /// Individual values: at most 31 each.
        ivs: Stats<u8>,
    },
    /// Champions Stat Points (at most 32 each and 66 in all). IVs are fixed
    /// at 31.
    StatPoints(Stats<u8>),
}

impl Investment {
    /// The `(evs, ivs)` pair fed to the stat formula.
    pub fn effective(&self) -> (Stats<u8>, Stats<u8>) {
        match *self {
            Investment::Evs { evs, ivs } => (evs, ivs),
            Investment::StatPoints(sp) => {
                let mut evs = Stats::splat(0);
                for s in Stat::ALL {
                    evs[s] = stat_points_to_evs(sp[s]);
                }
                (evs, Stats::splat(31))
            }
        }
    }
}

impl Default for Investment {
    fn default() -> Self {
        Investment::Evs { evs: Stats::splat(0), ivs: Stats::splat(31) }
    }
}

/// A Pokémon's gender (Showdown's `pokemon.gender`: `M`, `F`, or empty for
/// a genderless Pokémon).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Gender {
    /// `M`.
    Male,
    /// `F`.
    Female,
    /// No gender (Showdown's `N`, or an empty gender).
    #[default]
    Genderless,
}

impl Gender {
    /// A set's letter for it (`set.gender`): `M`, `F` or `N`.
    pub fn letter(self) -> &'static str {
        match self {
            Gender::Male => "M",
            Gender::Female => "F",
            Gender::Genderless => "N",
        }
    }

    /// The gender a set's letter gives (`M`, `F` or `N`); anything else
    /// means none to Showdown's `Pokemon` constructor.
    pub fn from_letter(letter: &str) -> Option<Gender> {
        match letter {
            "M" => Some(Gender::Male),
            "F" => Some(Gender::Female),
            "N" => Some(Gender::Genderless),
            _ => None,
        }
    }
}

/// One Pokémon as registered by a player.
///
/// IDs come from the format's dex: the constants in
/// `pikapro_data::gen9::{species, moves, abilities, items}` (the Champions
/// dex shares them), or its lookups by name (`dex.species_by_name`, ...).
/// `pikapro-protocol` reads Showdown's export and packed formats into sets.
///
/// ```
/// use pikapro::{Investment, PokemonSet, Team};
/// use pikapro_data::gen9::{abilities, items, moves, species};
/// use pikapro_data::{Nature, Stats};
///
/// let garchomp = PokemonSet::new(species::GARCHOMP)
///     .with_level(50)
///     .with_ability(abilities::ROUGH_SKIN)
///     .with_item(items::CHOICE_SCARF)
///     .with_nature(Nature::Jolly)
///     .with_investment(Investment::Evs { evs: Stats::new(4, 252, 0, 0, 0, 252), ivs: Stats::splat(31) })
///     .with_moves(&[moves::EARTHQUAKE, moves::DRAGON_CLAW, moves::ROCK_SLIDE, moves::PROTECT]);
/// let team = Team::new(vec![garchomp]);
/// assert!(team.validate(&pikapro::Format::gen9_singles()).is_ok());
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PokemonSet {
    /// The species (or forme).
    pub species: SpeciesId,
    /// 1 to 100; the format's [`LevelRule`](crate::LevelRule) may change it
    /// in battle.
    pub level: u8,
    /// [`AbilityId::NONE`] for none.
    pub ability: AbilityId,
    /// The held item.
    pub item: Option<ItemId>,
    /// The nature.
    pub nature: Nature,
    /// EVs and IVs, or Stat Points: the format's
    /// [`StatSystem`] decides which.
    pub investment: Investment,
    /// One to four distinct moves.
    pub moves: StackVec<MoveId, MAX_MOVES>,
    /// Defaults to the species' primary type when `None`, as in Showdown's
    /// simulator (its validator fills in the forced Tera type of Ogerpon's
    /// masks and Terapagos). [`Type::Stellar`] is allowed.
    pub tera_type: Option<Type>,
    /// The set's gender (Showdown's `set.gender`). `None` takes the
    /// species' fixed gender, or one drawn at random when the battle is
    /// created, as Showdown's `Pokemon` constructor does. `Some(Genderless)`
    /// is Showdown's `N`. A gender the species can't have is not rejected
    /// here (Showdown's simulator takes it too; its validator doesn't).
    pub gender: Option<Gender>,
}

impl PokemonSet {
    /// A level-100 set with no moves, no item, neutral nature, 0 EVs and 31 IVs.
    pub fn new(species: SpeciesId) -> PokemonSet {
        PokemonSet {
            species,
            level: 100,
            ability: AbilityId::NONE,
            item: None,
            nature: Nature::default(),
            investment: Investment::default(),
            moves: StackVec::new(),
            tera_type: None,
            gender: None,
        }
    }

    /// The set's moves, in order (a set needs one to four; [`Team::validate`]
    /// rejects none or a repeated one).
    ///
    /// # Panics
    ///
    /// If `moves` has more than four ([`MAX_MOVES`]).
    pub fn with_moves(mut self, moves: &[MoveId]) -> Self {
        assert!(moves.len() <= crate::MAX_MOVES, "a set has at most {} moves, got {}", crate::MAX_MOVES, moves.len());
        self.moves = moves.iter().copied().collect();
        self
    }

    /// The set's level.
    pub fn with_level(mut self, level: u8) -> Self {
        self.level = level;
        self
    }

    /// The held item.
    pub fn with_item(mut self, item: ItemId) -> Self {
        self.item = Some(item);
        self
    }

    /// The ability.
    pub fn with_ability(mut self, ability: AbilityId) -> Self {
        self.ability = ability;
        self
    }

    /// The nature.
    pub fn with_nature(mut self, nature: Nature) -> Self {
        self.nature = nature;
        self
    }

    /// EVs and IVs, or Stat Points.
    pub fn with_investment(mut self, investment: Investment) -> Self {
        self.investment = investment;
        self
    }

    /// The Tera type.
    pub fn with_tera_type(mut self, ty: Type) -> Self {
        self.tera_type = Some(ty);
        self
    }

    /// The gender (otherwise the species' fixed one, or a random draw).
    pub fn with_gender(mut self, gender: Gender) -> Self {
        self.gender = Some(gender);
        self
    }
}

/// A registered team, in registration order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Team {
    /// The sets, in registration order (team preview's indices).
    pub sets: Vec<PokemonSet>,
}

impl Team {
    /// A team of `sets`, in that order.
    pub fn new(sets: Vec<PokemonSet>) -> Team {
        Team { sets }
    }

    /// Checks the team against the format's structural rules.
    ///
    /// This is *not* a legality checker (learnsets, event-only abilities,
    /// species bans: that is `pikapro-validate`); it only enforces what the
    /// engine needs to be sound plus the format's clauses.
    ///
    /// A team needs at least one Pokémon whatever the format's
    /// [`team_size`](Format::team_size) says: Showdown can't play an empty
    /// side. A format whose [`team_preview`](Format::team_preview) brings
    /// none (`Pick(0)`; Showdown's `Picked Team Size` is a positive integer)
    /// has no valid team: every team is [`TeamError::Size`] with `max: 0`.
    pub fn validate(&self, format: &Format) -> Result<(), TeamError> {
        let (min, max) = format.team_size;
        let (min, max) = (min.max(1), if format.team_preview == crate::TeamPreview::Pick(0) { 0 } else { max });
        let n = self.sets.len();
        if n < min as usize || n > max as usize || n > MAX_TEAM {
            return Err(TeamError::Size { len: n, min, max });
        }
        // The National Dex numbers so far: Showdown's
        // `speciesclause.onValidateTeam` keys on them, so two formes of one
        // species (Rotom-Wash and Rotom-Heat) break it.
        let mut nums = [0i16; MAX_TEAM];
        for (i, set) in self.sets.iter().enumerate() {
            set.check(format).map_err(|kind| TeamError::Set { index: i, kind })?;
            if format.clauses.species {
                let num = format.dex.species(set.species).num;
                // Not `contains`: it calls the out-of-line `slice_contains`,
                // which costs `Battle::new` 2 x 200 instructions more than
                // these at most five comparisons (`cargo bench-insns`,
                // construct/*vgc).
                #[allow(clippy::manual_contains)]
                if nums[..i].iter().any(|&n| n == num) {
                    return Err(TeamError::SpeciesClause { index: i });
                }
                nums[i] = num;
            }
            if format.clauses.item && set.item.is_some() && self.sets[..i].iter().any(|s| s.item == set.item) {
                return Err(TeamError::ItemClause { index: i });
            }
        }
        Ok(())
    }
}

impl PokemonSet {
    /// The per-set checks of [`Team::validate`] (known IDs, level, moves,
    /// the stat system's ranges and a forced Tera type; the clauses are the
    /// team's), for one set (`validate`).
    pub(crate) fn check(&self, format: &Format) -> Result<(), SetError> {
        let (dex, set) = (format.dex, self);
        if set.species.index() >= dex.species.len() {
            return Err(SetError::UnknownSpecies);
        }
        if set.ability.index() >= dex.abilities.len() {
            return Err(SetError::UnknownAbility);
        }
        if set.item.is_some_and(|it| it.index() >= dex.items.len()) {
            return Err(SetError::UnknownItem);
        }
        if set.level == 0 || set.level > 100 {
            return Err(SetError::Level);
        }
        if set.moves.is_empty() {
            return Err(SetError::NoMoves);
        }
        for (j, m) in set.moves.iter().enumerate() {
            if m.index() >= dex.moves.len() {
                return Err(SetError::UnknownMove);
            }
            if set.moves[..j].contains(m) {
                return Err(SetError::DuplicateMove);
            }
        }
        match (format.stat_system, set.investment) {
            (StatSystem::EffortValues, Investment::Evs { evs, ivs }) => {
                let total: u32 = evs.0.iter().map(|&e| e as u32).sum();
                if evs.0.iter().any(|&e| e > 252) || total > 510 || ivs.0.iter().any(|&v| v > 31) {
                    return Err(SetError::Investment);
                }
            }
            (StatSystem::StatPoints, Investment::StatPoints(sp)) => {
                let total: u32 = sp.0.iter().map(|&e| e as u32).sum();
                if sp.0.iter().any(|&e| e > 32) || total > 66 {
                    return Err(SetError::Investment);
                }
            }
            _ => return Err(SetError::WrongStatSystem),
        }
        // `requiredTeraType` (Ogerpon, Terapagos), as Showdown's
        // validator checks it in Gen 9 formats with Terastallization.
        let required = dex.species(set.species).required_tera_type;
        if format.gimmicks.contains(Gimmick::Terastallize)
            && set.tera_type.zip(required).is_some_and(|(t, r)| t != r.ty())
        {
            return Err(SetError::TeraType);
        }
        Ok(())
    }
}

/// What is wrong with one set ([`TeamError::Set`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SetError {
    /// The species ID isn't in the format's dex.
    UnknownSpecies,
    /// The ability ID isn't in the format's dex.
    UnknownAbility,
    /// The item ID isn't in the format's dex.
    UnknownItem,
    /// A move ID isn't in the format's dex.
    UnknownMove,
    /// The level isn't 1 to 100.
    Level,
    /// The set has no moves.
    NoMoves,
    /// The set has a move twice.
    DuplicateMove,
    /// EVs, IVs or Stat Points out of range.
    Investment,
    /// EVs given to a Stat Point format or vice versa.
    WrongStatSystem,
    /// A Tera type other than the species' forced one.
    TeraType,
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SetError::UnknownSpecies => "unknown species",
            SetError::UnknownAbility => "unknown ability",
            SetError::UnknownItem => "unknown item",
            SetError::UnknownMove => "unknown move",
            SetError::Level => "level must be 1 to 100",
            SetError::NoMoves => "no moves",
            SetError::DuplicateMove => "duplicate move",
            SetError::Investment => "EVs, IVs or Stat Points out of range",
            SetError::WrongStatSystem => "wrong stat system (EVs for Stat Points or the reverse)",
            SetError::TeraType => "Tera type other than the species' forced one",
        })
    }
}

impl std::error::Error for SetError {}

/// Why [`Team::validate`] (and so [`Battle::new`](crate::Battle::new))
/// rejects a team. Indices are registration order, from 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TeamError {
    /// The team has `len` Pokémon; the format takes `min..=max`.
    Size {
        /// The team's size.
        len: usize,
        /// The fewest the format takes (at least 1).
        min: u8,
        /// The most the format takes (0: none, see [`Team::validate`]).
        max: u8,
    },
    /// Set `index` is malformed.
    Set {
        /// The set.
        index: usize,
        /// What is wrong with it.
        kind: SetError,
    },
    /// Set `index` repeats an earlier set's species (by National Dex
    /// number, so any forme counts).
    SpeciesClause {
        /// The set.
        index: usize,
    },
    /// Set `index` holds an earlier set's item.
    ItemClause {
        /// The set.
        index: usize,
    },
}

impl fmt::Display for TeamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TeamError::Size { len, min, max } => {
                write!(f, "team has {len} Pokémon; format requires {min}..={max}")
            }
            TeamError::Set { index, kind } => write!(f, "set {}: {kind}", index + 1),
            TeamError::SpeciesClause { index } => {
                write!(f, "set {}: duplicate species (species clause)", index + 1)
            }
            TeamError::ItemClause { index } => {
                write!(f, "set {}: duplicate item (item clause)", index + 1)
            }
        }
    }
}

impl std::error::Error for TeamError {}
