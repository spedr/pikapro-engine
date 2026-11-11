//! Fuzzing pikapro-teams' generators against Showdown's team validator.
//!
//! Every generated team must be accepted by Showdown's `TeamValidator`
//! (`tools/showdown-oracle/validator.mjs`) in the generator's validation
//! format, by pikapro-validate, by the engine's structural checks
//! (`Team::validate`) and by `Battle::new`. The report also says how the
//! teams cover the format (species, moves, items, abilities) and which
//! rules forced redraws while building them.

use std::collections::{BTreeMap, BTreeSet};

use pikapro::{Battle, BattleOptions, Format, Seed, Team};
use pikapro_data::{AbilityId, ItemId, MoveId, SpeciesId};
use pikapro_teams::{Generated, Redraws, TeamGenerator};
use pikapro_validate::SetInfo;
use pikapro_validate::cases::{Case, format_case, played_line};

use crate::OracleError;
use crate::validate::{Validated, run_showdown_validator_played, team_json};

/// The outcome of one generated team.
#[derive(Clone, Debug)]
pub struct TeamOutcome {
    pub seed: u64,
    pub generated: Generated,
    /// Showdown's problems (empty: accepted).
    pub showdown: Vec<String>,
    /// pikapro-validate's messages (empty: accepted).
    pub ours: Vec<String>,
    /// `Team::validate` or `Battle::new`'s error, if any.
    pub engine: Option<String>,
    /// The sets Showdown's validator changes (`played` lines, generated →
    /// Showdown's), which its server would play otherwise than generated:
    /// none for a team the engine plays as Showdown would.
    pub replayed: Vec<(String, String)>,
}

impl TeamOutcome {
    pub fn accepted(&self) -> bool {
        self.showdown.is_empty() && self.ours.is_empty() && self.engine.is_none() && self.replayed.is_empty()
    }
}

/// The sets' info as the validators see it (the set's own gender).
pub fn team_info(team: &Team) -> Vec<SetInfo> {
    team.sets.iter().map(|s| SetInfo { gender: s.gender, ..SetInfo::default() }).collect()
}

/// Generates the teams of `seeds` and checks each with both validators and
/// the engine.
pub fn check_teams(g: &TeamGenerator, seeds: std::ops::Range<u64>) -> Result<Vec<TeamOutcome>, OracleError> {
    let dex = g.validation_format().dex;
    let generated: Vec<(u64, Generated)> = seeds.map(|s| (s, g.generate(s).expect("a team"))).collect();
    let json: Vec<String> = generated.iter().map(|(_, t)| team_json(dex, &t.team, &team_info(&t.team))).collect();
    let theirs = run_showdown_validator_played(g.validation_format().id, &json)?;
    Ok(generated
        .into_iter()
        .zip(theirs)
        .map(|((seed, generated), Validated { problems: showdown, played })| {
            let problems = g.validator().validate(&generated.team).err().unwrap_or_default();
            let ours = g.validator().messages(&problems);
            let engine = engine_check(g.format(), &generated.team);
            let replayed =
                generated.team.sets.iter().map(|s| played_line(dex, s)).zip(played).filter(|(a, b)| a != b).collect();
            TeamOutcome { seed, generated, showdown, ours, engine, replayed }
        })
        .collect())
}

/// `Team::validate`, then a battle with the team on both sides.
fn engine_check(format: &Format, team: &Team) -> Option<String> {
    if let Err(e) = team.validate(format) {
        return Some(format!("Team::validate: {e}"));
    }
    let options = BattleOptions::new(Seed::fast(0)).without_events();
    Battle::new(format.clone(), [team.clone(), team.clone()], options).err().map(|e| format!("Battle::new: {e:?}"))
}

/// Coverage of the generated teams.
#[derive(Clone, Debug, Default)]
pub struct Coverage {
    pub teams: u64,
    pub sets: u64,
    pub species: BTreeMap<SpeciesId, u64>,
    pub numbers: BTreeSet<i16>,
    pub moves: BTreeSet<MoveId>,
    pub items: BTreeMap<ItemId, u64>,
    pub abilities: BTreeSet<AbilityId>,
    /// By `Nature as u8`.
    pub natures: BTreeSet<u8>,
    pub move_counts: [u64; 5],
    pub redraws: Redraws,
}

impl Coverage {
    pub fn add(&mut self, format: &Format, out: &TeamOutcome) {
        self.teams += 1;
        self.redraws.merge(&out.generated.redraws);
        for s in &out.generated.team.sets {
            self.sets += 1;
            *self.species.entry(s.species).or_default() += 1;
            self.numbers.insert(format.dex.species(s.species).num);
            self.moves.extend(s.moves.iter().copied());
            if let Some(i) = s.item {
                *self.items.entry(i).or_default() += 1;
            }
            self.abilities.insert(s.ability);
            self.natures.insert(s.nature as u8);
            self.move_counts[s.moves.len().min(4)] += 1;
        }
    }
}

/// A recorded golden case for a generated team: named
/// `<preset>-<mode>-<seed>`, with Showdown's verdict as the expectation.
pub fn golden_case(g: &TeamGenerator, out: &TeamOutcome) -> String {
    format_case(&Case {
        name: format!("{}-{}-{}", g.format().id, g.mode().name(), out.seed),
        format: g.validation_format().clone(),
        team: out.generated.team.clone(),
        info: team_info(&out.generated.team),
        expect: out.showdown.clone(),
        played: Vec::new(),
    })
}
