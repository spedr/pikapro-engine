//! `view_conformance`: fuzz the player view (`pikapro-view`).
//!
//! ```text
//! cargo view-conformance [--format F] [--battles N] [--seed S] [--jobs J]
//!                        [--source sets|uniform|fuzz|targets|combos|<target>]
//!                        [--check soundness,leak,meta,poke-env] [--skip FACTS] [--perturb K]
//!                        [--show N] [--record DIR] [--python PY]
//! cargo view-conformance --replay FILE
//! cargo view-conformance --rerecord DIR [--jobs J] [--allow-loss]
//! ```
//!
//! Checks (default `soundness,leak,meta`):
//! - `soundness`: every fact the view claims is true of the battle state
//!   at each decision point (`pikapro_view::check::check_sound`, plus the
//!   rows of Illusion sides it skips; `--skip what,...` leaves facts out);
//! - `leak`: an event that shows a channel nothing leaves the view alone,
//!   and without team preview rows follow the order of appearance;
//! - `meta`: `--perturb K` (default 3) perturbations of hidden information
//!   per battle, whose views must match up to the first differing line;
//! - `poke-env`: decision records and our view projected onto
//!   `tools/view-oracle/SCHEMA.md`, fed to poke-env by
//!   `tools/view-oracle/pokeenv_view.py` and diffed by
//!   `tools/view-oracle/diff/diff.py` (`--python`, or `PIKAPRO_VIEW_PYTHON`:
//!   an interpreter with `pikapro` and poke-env installed).
//!
//! `--source targets` cycles through the targeted generators of
//! `view::Target::ALL` by seed; `--source NAME` takes any target by name,
//! the `conformance --target` ones (`Target::EXTRA`: `curse`, `octolock`,
//! ...) too; `--source combos` cycles through the combined targets
//! (`Target::COMBOS`: `illusionimposter`, `illusiontera`,
//! `commanderrevival`, `transformgimmick`, `spreadcalled`) the format
//! can express.
//! Failing battles are written to `target/view-conformance/<format>-<seed>.txt`
//! (a fixture without facts: `--replay` it). `--record DIR` writes every
//! passing battle as a golden view fixture; `--rerecord DIR` re-checks the
//! fixtures of DIR and rewrites them with the facts the view has now (after
//! a change to the projection). A fixture whose new facts lose or change a
//! recorded one (the view may only learn more) isn't rewritten: the lost
//! facts are listed, and `--allow-loss` rewrites it anyway (a deliberate
//! projection change). Each fixture is written through a temporary file,
//! and one that can't be read or replayed (or panics, in a profile that
//! unwinds) fails without stopping the others. `PIKAPRO_VIEW_LOG=1` makes
//! `--replay` print the battle's omniscient log and its choices first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use pikapro::Format;
use pikapro_conformance::cli::{self, Common, par_seeds, save};
use pikapro_conformance::view::golden::{self, Fixture};
use pikapro_conformance::view::{Checks, Driver, Failure, Generator, Source, Target, dump, meta};

struct Args {
    /// `--format`, `--battles`, `--seed`, `--jobs` and `--show`.
    common: Common,
    source: String,
    checks: Checks,
    meta: usize,
    poke_env: bool,
    record: Option<PathBuf>,
    python: String,
}

/// What one battle gave.
#[derive(Default)]
struct Outcome {
    seed: u64,
    source: &'static str,
    failure: Option<Failure>,
    /// The failing (or recorded) battle as a fixture.
    fixture: Option<String>,
    events: u64,
    changes: u64,
    updates: usize,
    exempt: u32,
    meta: Vec<meta::Outcome>,
    decisions: String,
    states: String,
}

fn main() -> ExitCode {
    let mut args = Args {
        common: Common::new(Format::champions_vgc(), 100, 4_500_000, 3),
        source: "sets".into(),
        checks: Checks::ALL,
        meta: 3,
        poke_env: false,
        record: None,
        python: std::env::var("PIKAPRO_VIEW_PYTHON").unwrap_or_else(|_| "python3".into()),
    };
    let mut meta_on = true;
    let mut replay: Option<PathBuf> = None;
    let mut rerecord: Option<PathBuf> = None;
    let mut allow_loss = false;
    let mut raw = cli::Args::from_env();
    while let Some(flag) = raw.next_flag() {
        if flag == "--allow-loss" {
            allow_loss = true;
            continue;
        }
        if args.common.take(&flag, &mut raw) {
            continue;
        }
        let value = raw.value_or_empty();
        let value = value.as_str();
        match flag.as_str() {
            "--perturb" => {
                args.meta = value.parse().unwrap_or_else(|_| panic!("{flag}: bad number {value:?}"));
            }
            "--source" => args.source = value.to_string(),
            "--record" => args.record = Some(value.into()),
            "--python" => args.python = value.to_string(),
            "--replay" => replay = Some(value.into()),
            "--rerecord" => rerecord = Some(value.into()),
            "--skip" => {
                let facts: Vec<&'static str> =
                    value.split(',').map(|f| &*Box::leak(f.to_string().into_boxed_str())).collect();
                args.checks.skip = Box::leak(facts.into_boxed_slice());
            }
            "--check" => {
                args.checks.soundness = false;
                args.checks.leak = false;
                meta_on = false;
                for c in value.split(',') {
                    match c {
                        "soundness" => args.checks.soundness = true,
                        "leak" => args.checks.leak = true,
                        "meta" => meta_on = true,
                        "poke-env" => args.poke_env = true,
                        _ => panic!("--check: unknown check {c:?}"),
                    }
                }
            }
            _ => panic!("unknown flag {flag:?}"),
        }
    }
    if !meta_on {
        args.meta = 0;
    }
    if let Some(path) = replay {
        return replay_one(&path, std::env::var_os("PIKAPRO_VIEW_LOG").is_some());
    }
    if let Some(dir) = rerecord {
        return rerecord_all(&dir, args.common.jobs, allow_loss);
    }

    let sources: Vec<Source> = if args.source == "targets" {
        Target::ALL.into_iter().map(Source::Target).collect()
    } else if args.source == "combos" {
        Target::COMBOS.into_iter().filter(|t| t.applies(&args.common.format)).map(Source::Target).collect()
    } else {
        vec![Source::from_name(&args.source).unwrap_or_else(|| panic!("--source: unknown {:?}", args.source))]
    };
    let generators: Vec<Generator> =
        sources.iter().map(|&s| Generator::new(&args.common.format, s).unwrap_or_else(|e| panic!("{e}"))).collect();
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/view-conformance");
    let _ = std::fs::create_dir_all(&out_dir);
    if let Some(dir) = &args.record {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    }

    // A panic is its seed's failure, not the run's end (the `release-tools`
    // profile unwinds).
    let outcomes: Vec<Outcome> = par_seeds(args.common.jobs, args.common.battles, |i| {
        let seed = args.common.seed + i;
        let generator = &generators[(seed % generators.len() as u64) as usize];
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&args, generator, seed)))
            .unwrap_or_else(|e| panicked(&args, generator, seed, &*e))
    });

    let (mut pass, mut fail) = (0, 0);
    let mut by_fact: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let (mut events, mut changes, mut updates, mut exempt) = (0u64, 0u64, 0usize, 0u32);
    let mut meta_runs = 0usize;
    let mut meta_steps = 0usize;
    let mut meta_by_kind: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut meta_stopped: BTreeMap<String, usize> = BTreeMap::new();
    let mut decisions = String::new();
    let mut states = String::new();
    for o in &outcomes {
        events += o.events;
        changes += o.changes;
        updates += o.updates;
        exempt += o.exempt;
        for m in &o.meta {
            meta_runs += 1;
            meta_steps += m.steps;
            let e = meta_by_kind.entry(m.kind.name()).or_default();
            e.0 += 1;
            e.1 += m.steps;
            if let Some(reason) = &m.stopped {
                let key = reason.split(':').next().unwrap_or(reason).to_string();
                *meta_stopped.entry(key).or_default() += 1;
            }
        }
        decisions.push_str(&o.decisions);
        states.push_str(&o.states);
        let Some(f) = &o.failure else {
            pass += 1;
            if let (Some(dir), Some(text)) = (&args.record, &o.fixture) {
                let name = format!("{}-{}.txt", args.common.format.id, o.seed);
                let recorded = pikapro_conformance::fixture::record(dir, &name, text)
                    .unwrap_or_else(|e| panic!("{}: {e}", dir.join(&name).display()));
                if !recorded {
                    eprintln!("{name} exists: not overwritten (`--rerecord` rewrites goldens)");
                }
            }
            continue;
        };
        fail += 1;
        let entry = by_fact.entry(format!("{}/{}", f.check, f.fact)).or_insert((0, o.seed));
        entry.0 += 1;
        let path = out_dir.join(format!("{}-{}.txt", args.common.format.id, o.seed));
        let saved = o.fixture.as_ref().is_some_and(|text| save(&path, text));
        if fail <= args.common.show {
            let at = if saved { path.display().to_string() } else { "not saved".into() };
            println!("FAIL {} seed {} [{}] ({at})\n{f}\n", args.common.format.id, o.seed, o.source);
        }
    }
    println!(
        "{}: {pass} passed, {fail} failed ({} updates, {events} events, {changes} changed the view, {exempt} rows exempt as disguises)",
        args.common.format.id, updates
    );
    for (fact, (n, seed)) in &by_fact {
        println!("  {fact}: {n} (first seed {seed})");
    }
    if meta_runs > 0 {
        println!("  metamorphic: {meta_runs} perturbations, {meta_steps} steps compared");
        for (kind, (n, steps)) in &meta_by_kind {
            println!("    {kind}: {n} runs, {steps} steps");
        }
        for (reason, n) in &meta_stopped {
            println!("    stopped early: {reason}: {n}");
        }
    }
    let mut ok = fail == 0;
    if args.poke_env {
        ok &= poke_env(&args, &out_dir, &decisions, &states);
    }
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Checks one battle.
fn check(args: &Args, generator: &Generator, seed: u64) -> Outcome {
    let setup = generator.setup(seed);
    let mut out = Outcome { seed, source: generator.source.name(), ..Outcome::default() };
    let fixture = |choices: Vec<_>, facts: Option<[Vec<_>; 2]>| {
        let facts = facts.unwrap_or_default();
        golden::write(&Fixture {
            setup: setup.clone(),
            source: generator.source.name().into(),
            checks: args.checks,
            meta: args.meta,
            choices,
            records: facts[0].len(),
            facts,
        })
    };
    // The checks while playing, and the records for poke-env.
    let (records, result) = dump::dump(&setup, Driver::random(seed), args.checks);
    let played = match result {
        Ok(p) => p,
        Err(f) => {
            // Replay to get the choices up to the failure.
            let choices = pikapro_conformance::view::play(&setup, Driver::random(seed), Checks::NONE, |_| Ok(()))
                .map(|p| p.choices)
                .unwrap_or_default();
            out.fixture = Some(fixture(choices, None));
            out.failure = Some(f);
            return out;
        }
    };
    out.events = played.events;
    out.changes = played.changes;
    out.updates = played.updates;
    out.exempt = played.exempt_rows;
    if args.poke_env {
        out.decisions = records.decisions;
        out.states = records.states;
    }
    if args.meta > 0 {
        match meta::run(&setup, args.meta) {
            Ok(m) => out.meta = m,
            Err(f) => {
                out.fixture = Some(fixture(played.choices, None));
                out.failure = Some(f);
                return out;
            }
        }
    }
    if args.record.is_some() {
        match golden::facts(&setup, Driver::random(seed), Checks::NONE) {
            Ok((facts, _)) => out.fixture = Some(fixture(played.choices, Some(facts))),
            Err(f) => out.failure = Some(f),
        }
    }
    out
}

/// The outcome of a seed whose checks panicked: the panic as its failure,
/// and the battle as a failure file when it plays without the checks.
fn panicked(args: &Args, generator: &Generator, seed: u64, payload: &(dyn std::any::Any + Send)) -> Outcome {
    let message = pikapro_conformance::panic_message(payload);
    let failure = Failure { check: "panic", fact: "panic".into(), message, context: Vec::new() };
    let fixture = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let setup = generator.setup(seed);
        let played = pikapro_conformance::view::play(&setup, Driver::random(seed), Checks::NONE, |_| Ok(())).ok()?;
        Some(golden::write(&Fixture {
            setup,
            source: generator.source.name().into(),
            checks: args.checks,
            meta: args.meta,
            choices: played.choices,
            records: 0,
            facts: Default::default(),
        }))
    }))
    .ok()
    .flatten();
    Outcome { seed, source: generator.source.name(), failure: Some(failure), fixture, ..Outcome::default() }
}

/// `--replay FILE`: re-runs a written battle's checks.
fn replay_one(path: &Path, log: bool) -> ExitCode {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut f = golden::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    if log {
        // The omniscient log, one update per block, with the choices.
        let renderer = f.setup.renderer(pikapro_protocol::Perspective::Omniscient);
        let _ = pikapro_conformance::view::play(&f.setup, Driver::script(f.choices.clone()), Checks::NONE, |u| {
            println!("== update {} (turn {})", u.index, u.battle.turn());
            for line in renderer.log(&u.battle.events()[u.start..]) {
                println!("{line}");
            }
            Ok(())
        });
        for (side, c) in &f.choices {
            println!("choice {} {c}", pikapro_protocol::side_id(*side));
        }
    }
    if f.records == 0 {
        // A failure file: the checks only.
        f.checks.soundness = true;
        f.checks.leak = true;
        let result = pikapro_conformance::view::play(&f.setup, Driver::script(f.choices.clone()), f.checks, |_| Ok(()))
            .and_then(|_| if f.meta > 0 { meta::run(&f.setup, f.meta).map(|_| ()) } else { Ok(()) });
        return match result {
            Ok(()) => {
                println!("{}: passes", path.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                println!("FAIL {}\n{e}", path.display());
                ExitCode::FAILURE
            }
        };
    }
    match golden::replay(&f) {
        Ok(()) => {
            println!("{}: passes", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("FAIL {}\n{e}", path.display());
            ExitCode::FAILURE
        }
    }
}

/// `--rerecord DIR`: re-checks each fixture's battle and rewrites its facts
/// (see the module docs: lost facts refuse the rewrite unless
/// `allow_loss`).
fn rerecord_all(dir: &Path, jobs: u64, allow_loss: bool) -> ExitCode {
    let paths = match pikapro_conformance::fixture::check_manifest(dir, "txt") {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let results: Vec<(String, Result<usize, String>)> = par_seeds(jobs, paths.len() as u64, |i| {
        let path = &paths[i as usize];
        let r = std::panic::catch_unwind(|| rerecord_one(path, allow_loss))
            .unwrap_or_else(|e| Err(format!("the replay panicked: {}", pikapro_conformance::panic_message(&*e))));
        (path.display().to_string(), r)
    });
    let (mut rewritten, mut failed, mut lost) = (0, 0, 0);
    for (path, r) in &results {
        match r {
            Ok(n) => {
                rewritten += 1;
                lost += n;
                if *n > 0 {
                    println!("{path}: rewritten, {n} recorded facts lost or changed (--allow-loss)");
                }
            }
            Err(e) => {
                failed += 1;
                println!("FAIL {path}\n{e}\n");
            }
        }
    }
    println!("rerecord: {rewritten} rewritten ({lost} facts lost or changed), {failed} failed");
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Re-checks one fixture and rewrites its facts. Returns how many recorded
/// facts were lost or changed (0 unless `allow_loss`).
fn rerecord_one(path: &Path, allow_loss: bool) -> Result<usize, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("unreadable: {e}"))?;
    let mut f = golden::parse(&text).map_err(|e| format!("unparsable: {e}"))?;
    let (facts, _) = golden::facts(&f.setup, Driver::script(f.choices.clone()), f.checks).map_err(|e| e.to_string())?;
    if f.meta > 0 {
        meta::run(&f.setup, f.meta).map_err(|e| e.to_string())?;
    }
    let lost = golden::lost_facts(&f.facts, &facts);
    if !lost.is_empty() && !allow_loss {
        const SHOWN: usize = 12;
        let mut report =
            format!("{} recorded facts lost or changed (not rewritten; --allow-loss to accept):", lost.len());
        for l in lost.iter().take(SHOWN) {
            report.push_str(&format!("\n  {l}"));
        }
        if lost.len() > SHOWN {
            report.push_str(&format!("\n  ... {} more", lost.len() - SHOWN));
        }
        return Err(report);
    }
    f.records = facts[0].len();
    f.facts = facts;
    pikapro_conformance::write_atomically(path, &golden::write(&f)).map_err(|e| format!("unwritable: {e}"))?;
    Ok(lost.len())
}

/// Check 3: poke-env's tracker on our decision records, diffed against our
/// projected view.
fn poke_env(args: &Args, out_dir: &Path, decisions: &str, states: &str) -> bool {
    let stem = out_dir.join(format!("{}-{}-{}", args.common.format.id, args.common.seed, args.common.battles));
    let with = |ext: &str| PathBuf::from(format!("{}.{ext}", stem.display()));
    let (d, v, p) = (with("decisions.jsonl"), with("view.jsonl"), with("poke-env.jsonl"));
    std::fs::write(&d, decisions).expect("writable decisions");
    std::fs::write(&v, states).expect("writable view states");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let feeder = root.join("tools/view-oracle/pokeenv_view.py");
    let diff = root.join("tools/view-oracle/diff/diff.py");
    let status = Command::new(&args.python).arg(&feeder).arg(&d).arg("-o").arg(&p).status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => println!("poke-env: {} exited with {s} (see {})", feeder.display(), p.display()),
        Err(e) => {
            println!("poke-env: can't run {} ({e}); set --python or PIKAPRO_VIEW_PYTHON", args.python);
            return false;
        }
    }
    let status = Command::new(&args.python)
        .arg(&diff)
        .arg("--decisions")
        .arg(&d)
        .arg("--ours")
        .arg(&v)
        .arg("--theirs")
        .arg(&p)
        .arg("--show")
        .arg(args.common.show.to_string())
        .status();
    matches!(status, Ok(s) if s.success())
}
