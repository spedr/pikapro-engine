//! The player-view differential.
//!
//! `pikapro-view` must hold exactly what a player's channel shows. Three
//! independent checks, fuzzed like conformance:
//!
//! 1. **Soundness** ([`sound`]): every fact the view claims is true of the
//!    omniscient [`BattleState`](pikapro::BattleState) at each decision
//!    point (Illusion exempt until its `|replace|`).
//! 2. **No leaks** ([`sound::Absorber`], [`meta`]): an event that shows a
//!    channel nothing must not change the view; hidden information
//!    perturbed (an unrevealed move, an item, EVs, IVs, Stat Points, the
//!    nature, an ability, the team order without team preview) must leave
//!    the views bit-identical up to the first line where the two players'
//!    channels differ.
//! 3. **Completeness** ([`dump`], [`project`]): decision records
//!    (`tools/view-oracle/SCHEMA.md`) and our view projected onto
//!    poke-env's normalised state, diffed against poke-env's own tracker by
//!    `tools/view-oracle/diff/diff.py`.
//!
//! Battles are played on pikapro with Showdown-order updates: after each
//! resolution both sides are shown the new lines and sent a request; then
//! side 1 and side 2 choose (a uniformly random legal choice from
//! `BattleRng(seed ^ 0x5EED)`, as `pikapro.RandomPolicy` does) until a submit
//! resolves. [`golden`] records passing battles as fixtures.

pub mod dump;
pub mod features;
pub mod golden;
pub mod meta;
pub mod project;
pub mod sound;
pub mod targets;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;

use pikapro::{Battle, BattleEvent, BattleOptions, BattleRng, Choice, Format, Progress, Seed, SideId, Team};
use pikapro_protocol::{
    BattleInfo, Perspective, Renderer, TeamSheet, export_sheet, format_choice, import_sheet, parse_choice_for,
};
use pikapro_view::{HpReport, OwnOverlay, PublicTracker, ViewConfig};

pub use targets::Target;

/// Mixed into a battle seed for its choices (`pikapro.RandomPolicy(seed ^
/// 0x5EED)` in `tools/view-oracle/gen_decisions.py`).
pub const POLICY_SALT: u64 = 0x5EED;

/// Mixed into a battle seed for its teams, as `pikapro_rl::teams::pair`
/// (what `pikapro.RandomTeams(format, mode).pair(seed)` returns).
const TEAM_SEED: u64 = 0x7EA5_5EED_7EA5_5EED;

/// Everything a battle needs before its first choice: the format, the seed
/// of its RNG (`Seed::fast`) and both teams with their nicknames.
#[derive(Clone, Debug)]
pub struct Setup {
    pub format: Format,
    pub seed: u64,
    pub sheets: [TeamSheet; 2],
    /// A view configuration other than [`Setup::view_config`]'s (the
    /// checks' own tests use a leaky one to see them fail).
    pub view: Option<ViewConfig>,
}

impl Setup {
    /// A fresh battle.
    pub fn battle(&self) -> Battle {
        self.try_battle().unwrap_or_else(|e| panic!("{} seed {}: invalid teams: {e}", self.format.id, self.seed))
    }

    /// A fresh battle, or why the teams are invalid.
    pub fn try_battle(&self) -> Result<Battle, String> {
        let teams = self.sheets.each_ref().map(|s| s.team.clone());
        let format = crate::oracle_format(&self.format);
        Battle::new(format, teams, BattleOptions::new(Seed::fast(self.seed))).map_err(|e| e.to_string())
    }

    /// The renderer of a perspective, with the sheets' nicknames, shininess
    /// and Poké Balls (what `pikapro.Battle.log` uses).
    pub fn renderer(&self, perspective: Perspective) -> Renderer {
        Renderer::new(&self.format, perspective).with_sheets([&self.sheets[0], &self.sheets[1]], &self.format)
    }

    /// The names each side's Pokémon go by (by `PokemonState::id`).
    pub fn names(&self) -> [Vec<String>; 2] {
        self.sheets.each_ref().map(|s| s.nicknames(&self.format))
    }

    /// The view configuration that matches the channels: exact HP where the
    /// format reports it to everyone (the custom-game presets), else
    /// percentages.
    pub fn view_config(&self) -> ViewConfig {
        if let Some(config) = self.view {
            return config;
        }
        let config = ViewConfig::new(&self.format);
        if BattleInfo::for_format(&self.format).report_exact_hp { config.with_hp(HpReport::Exact) } else { config }
    }

    /// Both teams in the export format (the first decision record's
    /// `teams`).
    pub fn exported(&self) -> [String; 2] {
        self.sheets.each_ref().map(|s| export_sheet(s, &self.format))
    }

    /// A copy with every gender fixed to the one the battle gives it, so
    /// that the battle draws none: reordering or changing the sets then
    /// leaves the RNG stream alone.
    pub fn with_pinned_genders(&self) -> Setup {
        let battle = self.battle();
        let mut out = self.clone();
        for side in SideId::BOTH {
            for m in battle.state().side(side).team.iter() {
                if let Some(set) = out.sheets[side.index()].team.sets.get_mut(m.id as usize) {
                    set.gender = Some(m.gender);
                }
            }
        }
        out
    }
}

/// Where a battle's teams come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `pikapro.RandomTeams(format, "sets").pair(seed)`: legal random-battle
    /// sets (the view-oracle recipe's default).
    Sets,
    /// `pikapro.RandomTeams(format, "uniform").pair(seed)`.
    Uniform,
    /// The conformance fuzzer's teams (`random_team`: what pikapro
    /// implements, not legal), with nicknames and shininess.
    Fuzz,
    /// The conformance fuzzer's teams with sets forced for a mechanic.
    Target(Target),
}

impl Source {
    pub fn from_name(name: &str) -> Option<Source> {
        Some(match name {
            "sets" => Source::Sets,
            "uniform" => Source::Uniform,
            "fuzz" => Source::Fuzz,
            _ => Source::Target(Target::from_name(name)?),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Source::Sets => "sets",
            Source::Uniform => "uniform",
            Source::Fuzz => "fuzz",
            Source::Target(t) => t.name(),
        }
    }
}

/// Builds each battle's [`Setup`] from its seed.
pub struct Generator {
    pub format: Format,
    pub source: Source,
    teams: Option<pikapro_teams::TeamGenerator>,
}

impl std::fmt::Debug for Generator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Generator").field("format", &self.format.id).field("source", &self.source).finish()
    }
}

impl Generator {
    pub fn new(format: &Format, source: Source) -> Result<Generator, String> {
        let mode = match source {
            Source::Sets => Some(pikapro_teams::Mode::Sets),
            Source::Uniform => Some(pikapro_teams::Mode::Uniform),
            Source::Fuzz | Source::Target(_) => None,
        };
        let teams = match mode {
            Some(mode) => Some(
                pikapro_teams::TeamGenerator::with_options(format, mode, pikapro_teams::Options::default())
                    .map_err(|e| format!("{}: {e:?}", format.id))?,
            ),
            None => None,
        };
        Ok(Generator { format: format.clone(), source, teams })
    }

    /// The battle with seed `seed`.
    pub fn setup(&self, seed: u64) -> Setup {
        let format = &self.format;
        let sheets = match &self.teams {
            Some(g) => {
                // `pikapro_rl::teams::pair`, then the export format and back, as
                // `pikapro.Battle(format, teams)` reads the text.
                let mut rng = BattleRng::new(Seed::fast(seed ^ TEAM_SEED));
                let mut redraws = pikapro_teams::Redraws::default();
                let mut team = || g.team_from(&mut rng, &mut redraws).expect("a random team");
                let pair = [TeamSheet::new(team()), TeamSheet::new(team())];
                pair.map(|s| import_sheet(&export_sheet(&s, format), format).expect("an exported team imports"))
            }
            None => {
                let target = match self.source {
                    Source::Target(t) => Some(t),
                    _ => None,
                };
                let (_, teams, _) = crate::draw_view_teams(format, seed, target);
                let names = crate::random_nicknames(&teams, seed);
                let shiny = crate::random_shiny(&teams, seed);
                let [t1, t2] = teams;
                let sheet = |team: Team, names: &[String], shiny: &[bool]| {
                    let extras = (0..team.sets.len())
                        .map(|i| pikapro_protocol::SetExtras {
                            name: names.get(i).cloned().unwrap_or_default(),
                            shiny: shiny.get(i).copied().unwrap_or(false),
                            ..Default::default()
                        })
                        .collect();
                    TeamSheet { team, extras }
                };
                [sheet(t1, &names[0], &shiny[0]), sheet(t2, &names[1], &shiny[1])]
            }
        };
        Setup { format: format.clone(), seed, sheets, view: None }
    }
}

/// Where the choices come from.
#[derive(Clone, Debug)]
pub enum Driver {
    /// A uniformly random legal choice per side, side 1 first
    /// (`LegalActions::sample`).
    Random(BattleRng),
    /// Recorded choice strings, in submission order (a fixture).
    Script { choices: Vec<(SideId, String)>, next: usize },
}

impl Driver {
    /// The random policy of battle `seed`.
    pub fn random(seed: u64) -> Driver {
        Driver::Random(BattleRng::new(Seed::fast(seed ^ POLICY_SALT)))
    }

    pub fn script(choices: Vec<(SideId, String)>) -> Driver {
        Driver::Script { choices, next: 0 }
    }

    /// One round of choices, as `tools/view-oracle/gen_decisions.py` makes
    /// them: side 1 chooses and submits if it has something to decide,
    /// then side 2 (asked after side 1's submit, so when side 1's choice
    /// alone resolved a request, side 2 answers the next one). Each choice
    /// is appended to `log`. Scripts may also submit explicit automatic
    /// passes from a recording. `Ok(false)`: a script ran out.
    pub fn advance(
        &mut self,
        battle: &mut Battle,
        names: &[Vec<String>; 2],
        log: &mut Vec<(SideId, Choice, String)>,
    ) -> Result<bool, String> {
        let active = battle.format().active_per_side();
        let mut resolved = false;
        for side in SideId::BOTH {
            if battle.is_over() || battle.issued_request(side) == pikapro::Request::Wait {
                continue;
            }
            let (choice, text) = match self {
                Driver::Random(rng) => {
                    if battle.legal_actions(side).is_waiting() {
                        continue;
                    }
                    let choice = battle.legal_actions(side).sample(rng);
                    (choice, format_choice(&choice, active))
                }
                Driver::Script { choices, next } => {
                    // Showdown records explicit passes even when every
                    // slot passes automatically. Scripts may include or
                    // omit them; the decision API reports Wait either way.
                    if battle.legal_actions(side).is_waiting() && choices.get(*next).is_none_or(|(who, _)| *who != side)
                    {
                        continue;
                    }
                    let Some((who, text)) = choices.get(*next) else { return Ok(false) };
                    if *who != side {
                        return Err(format!("recorded choice {text:?} is for the other side"));
                    }
                    *next += 1;
                    let choice = parse_choice_for(battle, side, text, &names[side.index()])
                        .map_err(|e| format!("recorded choice {text:?}: {e}"))?;
                    (choice, text.clone())
                }
            };
            let progress = battle.submit(side, choice).map_err(|e| format!("choice {text:?} rejected: {e}"))?;
            log.push((side, choice, text));
            resolved |= progress == Progress::Resolved;
        }
        if resolved { Ok(true) } else { Err("no side could resolve the request round".into()) }
    }
}

/// Whether `event` shows `perspective` anything: a line of its own, or an
/// annotation of the last `|move|` line ([`BattleEvent::MoveAttr`], which
/// amends a line already shown).
pub fn shows(renderer: &Renderer, event: &BattleEvent) -> bool {
    matches!(event, BattleEvent::MoveAttr { .. }) || !renderer.log(std::slice::from_ref(event)).is_empty()
}

/// Both overlays of the current requests.
pub fn overlays(battle: &Battle) -> [OwnOverlay; 2] {
    SideId::BOTH.map(|side| OwnOverlay::from_request(&battle.request_data(side)))
}

/// A tracker for a setup's battle.
pub fn tracker(setup: &Setup) -> PublicTracker {
    PublicTracker::new(setup.view_config())
}

/// The first failure found in a battle, with its context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Which check: `soundness`, `leak`, `metamorphic`, `team-order`,
    /// `golden`, `engine`.
    pub check: &'static str,
    /// The kind of fact (`hp`, `item`, `event`, ...), for summaries.
    pub fact: String,
    pub message: String,
    /// The spectator's last lines before it.
    pub context: Vec<String>,
}

impl Failure {
    pub fn new(check: &'static str, fact: impl Into<String>, message: impl Into<String>) -> Failure {
        Failure { check, fact: fact.into(), message: message.into(), context: Vec::new() }
    }

    pub fn with_context(mut self, context: &[String]) -> Failure {
        self.context = context[context.len().saturating_sub(10)..].to_vec();
        self
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for line in &self.context {
            writeln!(f, "      {line}")?;
        }
        write!(f, "  {} ({}): {}", self.check, self.fact, self.message)
    }
}

/// A short text of a value's `Debug` form where two differ: the first
/// differing field path, roughly (`sides[1].mons[2].hp`), with both
/// excerpts.
pub fn debug_diff<T: std::fmt::Debug>(a: &T, b: &T) -> String {
    text_diff(&masked(&format!("{a:?}"), false), &masked(&format!("{b:?}"), false))
}

/// Excerpts of two texts around their first difference.
pub fn text_diff(a: &str, b: &str) -> String {
    let Some(at) =
        a.bytes().zip(b.bytes()).position(|(x, y)| x != y).or((a.len() != b.len()).then(|| a.len().min(b.len())))
    else {
        return "equal".into();
    };
    let excerpt = |s: &str| {
        let start = s[..at].rfind([',', '{', '[']).map_or(0, |i| i + 1);
        let mut start = start.min(at).saturating_sub(40);
        while !s.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (at + 60).min(s.len());
        while !s.is_char_boundary(end) {
            end += 1;
        }
        s[start..end].to_string()
    };
    let mut out = String::new();
    let _ = write!(out, "before …{}…\n        after  …{}…", excerpt(a), excerpt(b));
    out
}

/// `Debug` text without the fields that aren't the view's facts: the
/// tracker's and overlay's bookkeeping (`ctx`, `built`, which their
/// `PartialEq` ignores too) and, with `ids`, each side's private id-to-row
/// map, which holds registration ids (they differ when teams are
/// reordered).
pub fn masked(text: &str, ids: bool) -> String {
    let mut fields = vec!["ctx: ", "built: "];
    if ids {
        fields.push("row_of_id: ");
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((at, name)) = fields.iter().filter_map(|f| rest.find(f).map(|i| (i, *f))).min() {
        out.push_str(&rest[..at]);
        // Skip the value: up to the `,` or closing bracket at its depth.
        let value = &rest[at + name.len()..];
        let mut depth = 0i32;
        let mut end = value.len();
        for (i, c) in value.char_indices() {
            match c {
                '[' | '{' | '(' => depth += 1,
                ']' | '}' | ')' if depth == 0 => {
                    end = i;
                    break;
                }
                ']' | '}' | ')' => depth -= 1,
                ',' if depth == 0 => {
                    end = i + 1;
                    break;
                }
                _ => {}
            }
        }
        rest = value[end..].trim_start();
    }
    out.push_str(rest);
    out
}

/// [`masked`] `Debug` of a tracker, without its id-to-row maps.
pub fn masked_debug(tracker: &PublicTracker) -> String {
    masked(&format!("{tracker:?}"), true)
}

/// Names the lines that usually explain a wrong slot or faint count, so
/// summaries group them: `slot` after a `|replace|` (Illusion broken) or a
/// `|swap|` (Ally Switch), `fainted` after a Revival Blessing.
fn hint(mut f: Failure, context: &[String]) -> Failure {
    let seen = |p: &str| context.iter().any(|l| l.contains(p));
    let why = match f.fact.as_str() {
        "slot" | "active" if seen("|swap|") => Some("swap"),
        "slot" | "active" if seen("|replace|") => Some("replace"),
        "fainted" if seen("[from] move: Revival Blessing") => Some("revival"),
        _ => None,
    };
    if let Some(why) = why {
        f.fact = format!("{}({why})", f.fact);
    }
    f
}

/// Which checks [`play`] runs while it plays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Checks {
    /// Check 1 at every decision point.
    pub soundness: bool,
    /// Check 2's per-event rule, and the team order without team preview.
    pub leak: bool,
    /// Soundness facts not reported (known gaps, while exploring).
    pub skip: &'static [&'static str],
}

impl Checks {
    pub const NONE: Checks = Checks { soundness: false, leak: false, skip: &[] };
    pub const ALL: Checks = Checks { soundness: true, leak: true, skip: &[] };
}

/// One update: a resolution's events (the first update is the battle's
/// creation), after which both sides are sent a request.
#[derive(Debug)]
pub struct Update<'a> {
    /// From 0.
    pub index: usize,
    pub battle: &'a Battle,
    /// Where the update's events start in `battle.events()`.
    pub start: usize,
    /// The tracker after the update's events.
    pub tracker: &'a PublicTracker,
}

/// A battle [`play`] finished.
#[derive(Clone, Debug, Default)]
pub struct Played {
    /// Every choice submitted, as a string, in order (a script replays it).
    pub choices: Vec<(SideId, String)>,
    pub updates: usize,
    pub turns: u16,
    pub finished: bool,
    /// Events applied, and events that changed the tracker.
    pub events: u64,
    pub changes: u64,
    /// Rows exempt from soundness as Illusion disguises.
    pub exempt_rows: u32,
}

/// Plays a battle, feeding each update's events to a tracker and running
/// `checks`, then calls `hook` with the update.
pub fn play(
    setup: &Setup,
    mut driver: Driver,
    checks: Checks,
    mut hook: impl FnMut(&Update<'_>) -> Result<(), Failure>,
) -> Result<Played, Failure> {
    let mut battle = setup.try_battle().map_err(|e| Failure::new("engine", "teams", e))?;
    let mut tracker = tracker(setup);
    let mut absorber = sound::Absorber::new(setup);
    let names = setup.names();
    let mut log = Vec::new();
    let (mut start, mut index) = (0, 0);
    loop {
        let end = battle.events().len();
        absorber.absorb(&mut tracker, &battle.events()[start..end], checks.leak)?;
        if checks.soundness {
            sound::check(&battle, &tracker, &mut absorber.taint, checks.skip)
                .map_err(|f| hint(f, &absorber.context).with_context(&absorber.context))?;
        }
        if checks.leak {
            sound::team_order(&setup.format, &tracker, &absorber.taint)
                .map_err(|f| f.with_context(&absorber.context))?;
        }
        hook(&Update { index, battle: &battle, start, tracker: &tracker })
            .map_err(|f| if f.context.is_empty() { f.with_context(&absorber.context) } else { f })?;
        start = end;
        index += 1;
        if battle.is_over() {
            break;
        }
        match driver.advance(&mut battle, &names, &mut log) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => return Err(Failure::new("engine", "choice", e).with_context(&absorber.context)),
        }
    }
    Ok(Played {
        choices: log.into_iter().map(|(side, _, text)| (side, text)).collect(),
        updates: index,
        turns: battle.turn(),
        finished: battle.is_over(),
        events: absorber.events,
        changes: absorber.changes,
        exempt_rows: absorber.taint.iter().map(|t| t.count_ones()).sum(),
    })
}
