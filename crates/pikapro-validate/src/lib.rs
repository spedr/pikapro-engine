//! # pikapro-validate
//!
//! Team validation for pikapro: a port of Pokémon Showdown's team
//! validator (`sim/team-validator.ts`) for the formats the presets are
//! played in, checked differentially against Showdown's own validator
//! (see docs/CONFORMANCE.md).
//!
//! | pikapro preset | validates as |
//! |---|---|
//! | `championsvgc` | `gen9championsvgc2026regmc` ([Gen 9 Champions] VGC 2026 Reg M-C) |
//! | `championsbss` | `gen9championsbssregmc` ([Gen 9 Champions] BSS Reg M-C) |
//! | `gen9vgc` | `gen9vgc2025regi` ([Gen 9] VGC 2025 Reg I) |
//! | `gen9singles`, `gen9doubles` | `gen9anythinggoes@@@Species Clause` ([Gen 9] Anything Goes with Species Clause: Smogon's legality floor, no tier bans) |
//!
//! ```
//! use pikapro::{Investment, PokemonSet, Team};
//! use pikapro_data::gen9::{abilities, moves, species};
//! use pikapro_data::{Nature, Stats};
//! use pikapro_validate::{ValidationFormat, Validator};
//!
//! let validator = Validator::new(ValidationFormat::gen9_vgc_2025_reg_i());
//! let set = PokemonSet::new(species::GARCHOMP)
//!     .with_level(50)
//!     .with_ability(abilities::ROUGH_SKIN)
//!     .with_nature(Nature::Jolly)
//!     .with_investment(Investment::Evs { evs: Stats::new(4, 252, 0, 0, 0, 252), ivs: Stats::splat(31) })
//!     .with_moves(&[moves::EARTHQUAKE, moves::SPORE]);
//! let team = Team::new(vec![set; 1]);
//! let problems = validator.validate(&team).unwrap_err();
//! let messages = validator.messages(&problems);
//! assert_eq!(messages[0], "You must bring at least 4 Pokémon (your team has 1).");
//! assert_eq!(messages[1], "Garchomp can't learn Spore.");
//! ```
//!
//! Problems are typed ([`Problem`], [`ProblemKind`]); [`Validator::messages`]
//! renders them in Showdown's words.
//!
//! Showdown's validator also mutates the sets it checks, and a Showdown
//! server plays the team it leaves: [`Validator::check`] returns that team
//! ([`Checked::team`]; [`validate_preset`] too). A set that lists a
//! battle-only forme (`Floette-Mega @ Floettite`) starts as the forme it
//! changes from (Floette-Eternal with Flower Veil), which Mega Evolves in
//! battle. The engine plays the team it is given, as Showdown's simulator
//! does: give it this one.
//!
//! Showdown validates two things pikapro's [`PokemonSet`] does not
//! carry: the nickname (Nickname Clause, nickname length, nicknames that
//! name another species) and shininess (events, Pokémon GO). They are
//! passed alongside the team as [`SetInfo`], which can also override a
//! set's gender; without it every set is unnamed and not shiny, and keeps
//! its own gender (events that fix one, Battle Bond Greninja).

#![warn(missing_docs)]

// The case files the tests and `validate_fuzz` share: a test fixture
// format, not part of the validator's API.
#[doc(hidden)]
pub mod cases;
pub(crate) mod learn;
mod nickname;
pub mod problem;
pub mod rules;
mod set;
pub(crate) mod sources;

use pikapro::{Format, PokemonSet, Team, TeamError};
use pikapro_data::Dex;
use pikapro_data::legality::Legality;

pub use nickname::{Nickname, nickname};
pub use problem::{Problem, ProblemKind};
pub use rules::{SetRule, TeamRule, ValidationFormat};

use crate::rules::TeamRule as Rule;
use pikapro_data::gen9::species;

/// A set's gender (pikapro's). Showdown's validator only takes `M` and
/// `F`; `Genderless` (Showdown's `N`) counts as unset.
pub use pikapro::Gender;

/// What Showdown's set has that [`PokemonSet`] doesn't: nickname and
/// shininess (pikapro-protocol's `SetExtras`). `gender` overrides the set's
/// own [`PokemonSet::gender`] when given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetInfo {
    /// The nickname (`set.name`), if it has one.
    pub nickname: Option<String>,
    /// The gender, overriding the set's when given.
    pub gender: Option<Gender>,
    /// Whether it is shiny.
    pub shiny: bool,
}

impl SetInfo {
    /// The info of `set` with its nickname as written (empty: none) and
    /// shininess, e.g. from pikapro-protocol's `TeamSheet`:
    /// `sheet.team.sets.iter().zip(&sheet.extras).map(|(s, e)| SetInfo::new(s, &e.name, e.shiny))`.
    pub fn new(set: &PokemonSet, nickname: &str, shiny: bool) -> SetInfo {
        SetInfo { nickname: (!nickname.is_empty()).then(|| nickname.to_string()), gender: set.gender, shiny }
    }
}

/// Why a team can't be used in a preset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// The engine can't build it ([`Team::validate`]).
    Structure(TeamError),
    /// Showdown's validator rejects it.
    Problems(Vec<Problem>),
}

impl core::fmt::Display for Invalid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Invalid::Structure(e) => e.fmt(f),
            Invalid::Problems(p) if p.len() == 1 => f.write_str("the validator found 1 problem"),
            Invalid::Problems(p) => write!(f, "the validator found {} problems", p.len()),
        }
    }
}

impl std::error::Error for Invalid {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Invalid::Structure(e) => Some(e),
            Invalid::Problems(_) => None,
        }
    }
}

/// Validates teams for one [`ValidationFormat`].
#[derive(Clone, Debug)]
pub struct Validator {
    pub(crate) format: ValidationFormat,
    pub(crate) dex: &'static Dex,
    pub(crate) legality: &'static Legality,
}

impl Validator {
    /// A validator for `format`.
    pub fn new(format: ValidationFormat) -> Validator {
        Validator { dex: format.dex, legality: format.legality, format }
    }

    /// The validation format.
    pub fn format(&self) -> &ValidationFormat {
        &self.format
    }

    /// Validates `team` with no [`SetInfo`].
    pub fn validate(&self, team: &Team) -> Result<(), Vec<Problem>> {
        self.validate_with(team, &[])
    }

    /// Validates `team`; `info[i]` describes set `i` (missing entries are
    /// the default).
    pub fn validate_with(&self, team: &Team, info: &[SetInfo]) -> Result<(), Vec<Problem>> {
        let problems = self.check(team, info).problems;
        if problems.is_empty() { Ok(()) } else { Err(problems) }
    }

    /// Validates `team` as [`Validator::validate_with`] does, and returns the
    /// team and names validation leaves the sets, which Showdown's battle
    /// then plays (see [`Checked::team`]).
    pub fn check(&self, team: &Team, info: &[SetInfo]) -> Checked {
        let (problems, results) = self.team_problems(team, info);
        let team = if results.len() == team.sets.len() {
            Team::new(results.iter().map(|r| r.played.clone()).collect())
        } else {
            let default = SetInfo::default();
            Team::new(team.sets.iter().enumerate().map(|(i, s)| given(s, info.get(i).unwrap_or(&default))).collect())
        };
        Checked { problems, names: results.into_iter().map(|r| r.name).collect(), team }
    }

    /// `team` as Showdown's validator leaves it ([`Checked::team`]), whether
    /// it is valid or not: what a Showdown server plays after accepting it.
    pub fn played(&self, team: &Team) -> Team {
        self.check(team, &[]).team
    }

    /// The problems of one set on its own (Showdown's `validateSet`, without
    /// the team rules), typed. Empty if the set is legal.
    pub fn set_problems(&self, set: &PokemonSet, info: &SetInfo) -> Vec<ProblemKind> {
        self.check_set(set, info).0
    }

    /// [`Validator::set_problems`], and the set as validation leaves it,
    /// which Showdown's battle plays (see [`Checked::team`]).
    pub fn check_set(&self, set: &PokemonSet, info: &SetInfo) -> (Vec<ProblemKind>, PokemonSet) {
        if let Some((error, id)) = unknown_id(self.dex, set) {
            return (vec![ProblemKind::UnknownId { error, id }], given(set, info));
        }
        let r = self.validate_set(set, info);
        (r.problems, r.played)
    }

    /// Showdown's message(s) for each problem, in order.
    pub fn messages(&self, problems: &[Problem]) -> Vec<String> {
        problems.iter().flat_map(|p| p.lines(self.dex)).collect()
    }

    /// `baseValidateTeam`: the problems, and what `validateSet` left of
    /// each set (none if the team is too large to look at its sets).
    fn team_problems(&self, team: &Team, info: &[SetInfo]) -> (Vec<Problem>, Vec<set::SetResult>) {
        let f = &self.format;
        let mut problems = Vec::new();
        let len = team.sets.len();
        if len < f.min_team_size as usize {
            problems.push(Problem { set: None, kind: ProblemKind::TeamTooSmall { min: f.min_team_size, len } });
        }
        if len > f.max_team_size as usize {
            let problem = Problem { set: None, kind: ProblemKind::TeamTooLarge { max: f.max_team_size, len } };
            return (vec![problem], Vec::new());
        }
        let default = SetInfo::default();
        let mut results = Vec::with_capacity(len);
        let mut unknown = Vec::new();
        for (i, set) in team.sets.iter().enumerate() {
            let info = info.get(i).unwrap_or(&default);
            // An ID outside the dex: reported, and left out of the rest.
            if let Some((error, id)) = unknown_id(self.dex, set) {
                problems.push(Problem { set: Some(i), kind: ProblemKind::UnknownId { error, id } });
                let name = pikapro_data::get_name(info.nickname.as_deref().unwrap_or(""));
                results.push(set::SetResult { problems: Vec::new(), played: given(set, info), name });
                unknown.push(i);
                continue;
            }
            let r = self.validate_set(set, info);
            problems.extend(r.problems.iter().cloned().map(|kind| Problem { set: Some(i), kind }));
            results.push(r);
        }
        // (Complex team bans: none in these formats.)
        let known: Vec<set::SetResult>;
        let checked = if unknown.is_empty() {
            &results[..]
        } else {
            known = results.iter().enumerate().filter(|(i, _)| !unknown.contains(i)).map(|(_, r)| r.clone()).collect();
            &known[..]
        };
        for rule in f.team_rules {
            let kinds = self.team_rule(*rule, checked);
            problems.extend(kinds.into_iter().map(|kind| Problem { set: None, kind }));
        }
        (problems, results)
    }

    /// The `onValidateTeam` hook of `rule`, on the sets as `validateSet`
    /// left them.
    fn team_rule(&self, rule: Rule, sets: &[set::SetResult]) -> Vec<ProblemKind> {
        match rule {
            Rule::Obtainable => {
                let pairs: [(&[pikapro_data::SpeciesId], ProblemKind); 4] = [
                    (&[species::KYUREM_WHITE, species::KYUREM_BLACK], ProblemKind::OneKyurem),
                    (&[species::NECROZMA_DUSK_MANE], ProblemKind::OneNecrozmaDuskMane),
                    (&[species::NECROZMA_DAWN_WINGS], ProblemKind::OneNecrozmaDawnWings),
                    (&[species::CALYREX_ICE, species::CALYREX_SHADOW], ProblemKind::OneCalyrex),
                ];
                let mut counts = [0; 4];
                for s in sets {
                    for (i, (group, problem)) in pairs.iter().enumerate() {
                        if group.contains(&s.played.species) {
                            if counts[i] > 0 {
                                return vec![problem.clone()];
                            }
                            counts[i] += 1;
                        }
                    }
                }
                Vec::new()
            }
            Rule::SpeciesClause => {
                let mut nums = Vec::new();
                for s in sets {
                    let data = self.dex.species(s.played.species);
                    if nums.contains(&data.num) {
                        return vec![ProblemKind::SpeciesClause { base: data.base_species }];
                    }
                    nums.push(data.num);
                }
                Vec::new()
            }
            Rule::NicknameClause => {
                let mut names: Vec<&str> = Vec::new();
                for s in sets {
                    let base = self.dex.species(self.dex.species(s.played.species).base_species).name;
                    if s.name.is_empty() || s.name == base {
                        continue;
                    }
                    if names.contains(&s.name.as_str()) {
                        return vec![ProblemKind::NicknameClause { nickname: s.name.clone() }];
                    }
                    names.push(&s.name);
                }
                Vec::new()
            }
            Rule::ItemClause(limit) => {
                // A multiset in first-seen order.
                let mut counts: Vec<(pikapro_data::ItemId, usize)> = Vec::new();
                for item in sets.iter().filter_map(|s| s.played.item) {
                    match counts.iter_mut().find(|(i, _)| *i == item) {
                        Some((_, n)) => *n += 1,
                        None => counts.push((item, 1)),
                    }
                }
                counts
                    .iter()
                    .find(|&&(_, n)| n > limit as usize)
                    .map(|&(item, _)| vec![ProblemKind::ItemClause { limit, item }])
                    .unwrap_or_default()
            }
            Rule::LimitRestricted(limit) => {
                let restricted: Vec<_> =
                    sets.iter().map(|s| s.played.species).filter(|&s| self.format.is_restricted_species(s)).collect();
                if restricted.len() > limit as usize {
                    vec![ProblemKind::Restricted { limit, species: restricted }]
                } else {
                    Vec::new()
                }
            }
        }
    }
}

/// A set validation doesn't look at, with its [`SetInfo`]'s gender.
fn given(set: &PokemonSet, info: &SetInfo) -> PokemonSet {
    PokemonSet { gender: info.gender.or(set.gender), ..set.clone() }
}

/// The first of `set`'s IDs outside `dex` ([`Team::validate`]'s order),
/// with its value.
fn unknown_id(dex: &Dex, set: &PokemonSet) -> Option<(pikapro::SetError, u16)> {
    use pikapro::SetError as E;
    if set.species.index() >= dex.species.len() {
        return Some((E::UnknownSpecies, set.species.0));
    }
    if set.ability.index() >= dex.abilities.len() {
        return Some((E::UnknownAbility, set.ability.0));
    }
    if let Some(item) = set.item.filter(|i| i.index() >= dex.items.len()) {
        return Some((E::UnknownItem, item.0));
    }
    set.moves.iter().find(|m| m.index() >= dex.moves.len()).map(|m| (E::UnknownMove, m.0))
}

/// The engine's error for an ID outside the preset's dex, set by set.
fn unknown_id_error(format: &Format, team: &Team) -> Option<TeamError> {
    team.sets
        .iter()
        .enumerate()
        .find_map(|(index, set)| unknown_id(format.dex, set).map(|(kind, _)| TeamError::Set { index, kind }))
}

/// What [`Validator::check`] finds: Showdown's validator's problems and
/// the sets' names as it leaves them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Checked {
    /// Every problem, in Showdown's order (none: the team is valid).
    pub problems: Vec<Problem>,
    /// Each set's `set.name` after validation: its nickname cleaned by
    /// `Dex.getName` ([`pikapro_data::get_name`]), or its base species' name
    /// when it has none or the nickname names its own species. Empty when
    /// the team is too large for its sets to be looked at.
    pub names: Vec<String>,
    /// The team as validation leaves it, which Showdown's battle plays
    /// (its server validates a team and starts the battle with the sets the
    /// validator mutated): each set's species, ability, moves, level and
    /// Tera type as `validateSet` leaves them (the level set by `Adjust
    /// Level` or `Adjust Level Down`, a forced Tera type filled in), its
    /// other fields as given. A set that lists a battle-only forme plays
    /// the forme it changes from (`validateForme`: `set.species =
    /// species.battleOnly`), so
    /// `Floette-Mega @ Floettite` starts as Floette-Eternal and Mega Evolves
    /// in battle; a Mega's ability on it becomes the base forme's first
    /// (Fairy Aura: Flower Veil); Terapagos's formes start as Terapagos
    /// with Tera Shift; Behemoth Blade and Behemoth Bash become Iron Head; a
    /// forme an item forces (Ogerpon's masks) or a move (Keldeo-Resolute)
    /// is set. Only meaningful for a valid team (without problems): an
    /// invalid one is never played. The given team when it is too large for
    /// its sets to be looked at.
    pub team: Team,
}

/// Checks a team for a pikapro preset: Showdown's rules if the preset
/// has a [`ValidationFormat`] (they report every problem), then the
/// engine's structural checks ([`Team::validate`]: what a battle needs, such
/// as the stat system matching the format). An ID outside the preset's dex
/// (a hand-built set) is [`Invalid::Structure`] before anything else.
///
/// Returns the team to play: the team as Showdown's validator leaves it
/// ([`Checked::team`]; a listed battle-only forme such as `Floette-Mega`
/// becomes the forme it changes from in battle), or `team` itself for a
/// preset without a [`ValidationFormat`]. The engine plays whatever team it
/// is given, as Showdown's simulator does: give it this one. The rules are
/// looked up by [`Format::id`]: a custom format with a preset's id is
/// validated as that preset.
pub fn validate_preset(format: &Format, team: &Team, info: &[SetInfo]) -> Result<Team, Invalid> {
    if let Some(e) = unknown_id_error(format, team) {
        return Err(Invalid::Structure(e));
    }
    let played = match ValidationFormat::for_preset(format.id) {
        Some(v) => {
            let checked = Validator::new(v).check(team, info);
            if !checked.problems.is_empty() {
                return Err(Invalid::Problems(checked.problems));
            }
            checked.team
        }
        None => team.clone(),
    };
    played.validate(format).map_err(Invalid::Structure)?;
    Ok(played)
}

/// [`validate_preset`], in words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PresetCheck {
    /// Showdown's validator's messages or, if it has none, the engine's
    /// structural error. Empty: the team can play the preset.
    pub messages: Vec<String>,
    /// The sets' names as Showdown's validator leaves them (see
    /// [`Checked::names`]); for a preset without a [`ValidationFormat`],
    /// each nickname through `getName`, or the base species' name.
    pub names: Vec<String>,
    /// The team to play once `messages` is empty: as Showdown's validator
    /// leaves it ([`Checked::team`]), or as given for a preset without a
    /// [`ValidationFormat`] (or with an ID outside its dex).
    pub team: Team,
}

/// [`validate_preset`] with the problems as Showdown's messages, and the
/// names to play the sets by.
pub fn check_preset(format: &Format, team: &Team, info: &[SetInfo]) -> PresetCheck {
    let (mut messages, names, played) = match ValidationFormat::for_preset(format.id) {
        // An ID outside the preset's dex: only the engine's error (below),
        // and the sets' nicknames as names.
        _ if unknown_id_error(format, team).is_some() => {
            let names = (0..team.sets.len())
                .map(|i| pikapro_data::get_name(info.get(i).and_then(|i| i.nickname.as_deref()).unwrap_or("")))
                .collect();
            (Vec::new(), names, team.clone())
        }
        Some(v) => {
            let validator = Validator::new(v);
            let checked = validator.check(team, info);
            (validator.messages(&checked.problems), checked.names, checked.team)
        }
        None => {
            let dex = format.dex;
            let names = team
                .sets
                .iter()
                .enumerate()
                .map(|(i, set)| {
                    let given = info.get(i).and_then(|i| i.nickname.as_deref()).unwrap_or("");
                    nickname(dex, set.species, set.species, given).name
                })
                .collect();
            (Vec::new(), names, team.clone())
        }
    };
    if messages.is_empty()
        && let Err(e) = played.validate(format)
    {
        messages.push(e.to_string());
    }
    PresetCheck { messages, names, team: played }
}

/// The README's examples, run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
