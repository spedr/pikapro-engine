//! Golden view fixtures (`crates/pikapro-conformance/fixtures/view/*.txt`):
//! battles that passed the view checks, with our projected view at every
//! decision record, replayed by `cargo test` (`tests/view.rs`).
//!
//! ```text
//! # Recorded by view_conformance. Do not edit by hand.
//! format championsvgc
//! seed 4500123
//! source sets
//! checks soundness leak meta
//! team p1
//! ...export format...
//! end
//! team p2
//! ...
//! end
//! choice p1 team 1234
//! view p1 0 team.p1: Incineroar.hp=202
//! drop p2 7 opponent_team.p1: Rillaboom.item
//! ```
//!
//! `view <side> <index> <path>=<value>` lines are the facts of that
//! record's state ([`Json::flatten`](super::project::Json::flatten)) that are new or
//! changed since the side's previous record; `drop` lines the facts that
//! went away. Replaying a fixture re-runs its checks and requires every
//! recorded fact to still hold, with the same value: the view may learn
//! more (a fact that was unknown), never lose or change one. Record with
//! `cargo view-conformance --record crates/pikapro-conformance/fixtures/view`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use pikapro::{Format, SideId};
use pikapro_protocol::{export_sheet, import_sheet};

use super::project::Projector;
use super::{Checks, Driver, Failure, Setup, overlays, play};

/// Per side, per record index, every fact (`path` to JSON value).
pub type Facts = [Vec<BTreeMap<String, String>>; 2];

/// A fixture: the battle and each record's facts.
#[derive(Clone, Debug)]
pub struct Fixture {
    pub setup: Setup,
    pub source: String,
    pub checks: Checks,
    /// Run the metamorphic check on replay too.
    pub meta: usize,
    pub choices: Vec<(SideId, String)>,
    /// Records per side.
    pub records: usize,
    /// Per side, per record index, every fact.
    pub facts: Facts,
}

/// Each side's facts at each record of a battle.
pub fn facts(setup: &Setup, driver: Driver, checks: Checks) -> Result<(Facts, super::Played), Failure> {
    let mut projectors = SideId::BOTH.map(|s| Projector::new(setup, s));
    let mut facts: [Vec<BTreeMap<String, String>>; 2] = [Vec::new(), Vec::new()];
    let played = play(setup, driver, checks, |u| {
        let over = u.battle.is_over();
        let own = overlays(u.battle);
        for side in SideId::BOTH {
            let state = projectors[side.index()].state(setup, u.tracker, (!over).then_some(&own[side.index()]));
            let mut leaves = Vec::new();
            state.flatten("", &mut leaves);
            facts[side.index()].push(leaves.into_iter().collect());
        }
        Ok(())
    })?;
    Ok((facts, played))
}

pub fn write(f: &Fixture) -> String {
    let mut out = String::new();
    let s = &f.setup;
    let _ = writeln!(out, "# Recorded by view_conformance. Do not edit by hand.");
    let _ = writeln!(out, "format {}", s.format.id);
    let _ = writeln!(out, "seed {}", s.seed);
    let _ = writeln!(out, "source {}", f.source);
    let mut checks = Vec::new();
    if f.checks.soundness {
        checks.push("soundness".to_string());
    }
    if f.checks.leak {
        checks.push("leak".to_string());
    }
    if f.meta > 0 {
        checks.push(format!("meta={}", f.meta));
    }
    let _ = writeln!(out, "checks {}", checks.join(" "));
    for side in SideId::BOTH {
        let _ = writeln!(out, "team {}", pikapro_protocol::side_id(side));
        out.push_str(&export_sheet(&s.sheets[side.index()], &s.format));
        out.push_str("end\n");
    }
    for (side, choice) in &f.choices {
        let _ = writeln!(out, "choice {} {choice}", pikapro_protocol::side_id(*side));
    }
    let _ = writeln!(out, "records {}", f.records);
    let empty = BTreeMap::new();
    for side in SideId::BOTH {
        let who = pikapro_protocol::side_id(side);
        for (i, now) in f.facts[side.index()].iter().enumerate() {
            let before = if i == 0 { &empty } else { &f.facts[side.index()][i - 1] };
            for (path, value) in now {
                if before.get(path) != Some(value) {
                    let _ = writeln!(out, "view {who} {i} {path}={value}");
                }
            }
            for path in before.keys() {
                if !now.contains_key(path) {
                    let _ = writeln!(out, "drop {who} {i} {path}");
                }
            }
        }
    }
    out
}

pub fn parse(text: &str) -> Result<Fixture, String> {
    let mut format: Option<Format> = None;
    let mut seed = None;
    let mut source = String::new();
    let mut checks = Checks::NONE;
    let mut meta = 0;
    let mut records = 0;
    let mut sheets = [None, None];
    let mut choices = Vec::new();
    let mut changes: [Vec<(usize, String, Option<String>)>; 2] = [Vec::new(), Vec::new()];
    let mut lines = text.lines().enumerate();
    let side_of = |s: &str| match s {
        "p1" => Some(SideId::P1),
        "p2" => Some(SideId::P2),
        _ => None,
    };
    while let Some((n, line)) = lines.next() {
        let err = |m: &str| format!("line {}: {m}", n + 1);
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "format" => format = Some(Format::by_id(rest).ok_or_else(|| err("unknown format"))?),
            "seed" => seed = Some(rest.parse::<u64>().map_err(|_| err("bad seed"))?),
            "source" => source = rest.to_string(),
            "records" => records = rest.parse().map_err(|_| err("bad records"))?,
            "checks" => {
                for c in rest.split_whitespace() {
                    match c {
                        "soundness" => checks.soundness = true,
                        "leak" => checks.leak = true,
                        _ => {
                            meta =
                                c.strip_prefix("meta=").and_then(|n| n.parse().ok()).ok_or_else(|| err("bad check"))?
                        }
                    }
                }
            }
            "team" => {
                let format = format.as_ref().ok_or_else(|| err("team before format"))?;
                let mut body = String::new();
                for (_, l) in lines.by_ref() {
                    if l == "end" {
                        break;
                    }
                    body.push_str(l);
                    body.push('\n');
                }
                let side = side_of(rest).ok_or_else(|| err("bad side"))?;
                sheets[side.index()] = Some(import_sheet(&body, format).map_err(|e| err(&e.to_string()))?);
            }
            "choice" => {
                let (who, choice) = rest.split_once(' ').ok_or_else(|| err("bad choice"))?;
                choices.push((side_of(who).ok_or_else(|| err("bad side"))?, choice.to_string()));
            }
            "view" | "drop" => {
                let mut parts = rest.splitn(3, ' ');
                let side = parts.next().and_then(side_of).ok_or_else(|| err("bad side"))?;
                let index: usize = parts.next().and_then(|i| i.parse().ok()).ok_or_else(|| err("bad index"))?;
                let fact = parts.next().ok_or_else(|| err("missing fact"))?;
                let change = if key == "view" {
                    let (path, value) = fact.split_once('=').ok_or_else(|| err("missing ="))?;
                    (index, path.to_string(), Some(value.to_string()))
                } else {
                    (index, fact.to_string(), None)
                };
                changes[side.index()].push(change);
            }
            _ => return Err(err("unknown key")),
        }
    }
    let [Some(s1), Some(s2)] = sheets else { return Err("missing team".into()) };
    let setup = Setup {
        format: format.ok_or("missing format")?,
        seed: seed.ok_or("missing seed")?,
        sheets: [s1, s2],
        view: None,
    };
    let facts = changes.map(|list| {
        let mut out: Vec<BTreeMap<String, String>> = Vec::with_capacity(records);
        for (index, path, value) in list {
            while out.len() <= index {
                out.push(out.last().cloned().unwrap_or_default());
            }
            match value {
                Some(v) => out[index].insert(path, v),
                None => out[index].remove(&path),
            };
        }
        while out.len() < records {
            out.push(out.last().cloned().unwrap_or_default());
        }
        out
    });
    Ok(Fixture { setup, source, checks, meta, choices, records, facts })
}

/// The facts of `was` that `now` lost or changed (the golden invariant: a
/// view may learn more, never lose or change a fact), as `<side> record
/// <i>: <path> was <value>, now <value or unknown>`; a record count that
/// differs is one entry.
pub fn lost_facts(was: &Facts, now: &Facts) -> Vec<String> {
    let mut lost = Vec::new();
    for side in SideId::BOTH {
        let who = pikapro_protocol::side_id(side);
        let (was, now) = (&was[side.index()], &now[side.index()]);
        if was.len() != now.len() {
            lost.push(format!("{who}: {} records recorded, {} now", was.len(), now.len()));
        }
        for (i, (a, b)) in was.iter().zip(now).enumerate() {
            for (path, value) in a {
                if b.get(path) != Some(value) {
                    let now = b.get(path).map_or("unknown", String::as_str);
                    lost.push(format!("{who} record {i}: {path} was {value}, now {now}"));
                }
            }
        }
    }
    lost
}

/// Replays a fixture: its checks, then every recorded fact.
pub fn replay(f: &Fixture) -> Result<(), Failure> {
    let (now, _) = facts(&f.setup, Driver::script(f.choices.clone()), f.checks)?;
    if f.meta > 0 {
        super::meta::run(&f.setup, f.meta)?;
    }
    for side in SideId::BOTH {
        let (was, is) = (&f.facts[side.index()], &now[side.index()]);
        if was.len() != is.len() {
            return Err(Failure::new(
                "golden",
                "records",
                format!("{}: {} records recorded, {} now", pikapro_protocol::side_id(side), was.len(), is.len()),
            ));
        }
        for (i, (a, b)) in was.iter().zip(is).enumerate() {
            for (path, value) in a {
                if b.get(path) != Some(value) {
                    return Err(Failure::new(
                        "golden",
                        path.clone(),
                        format!(
                            "{} record {i}: {path} was {value}, now {}",
                            pikapro_protocol::side_id(side),
                            b.get(path).map_or("unknown", String::as_str)
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}
