//! `conformance`: fuzz pikapro against Pokémon Showdown.
//!
//! ```text
//! cargo conformance [--format ID] [--battles N] [--seed N] [--show N] [--jobs N] [--trace] [--record DIR] [--record-requests]
//!                   [--legal uniform|sets] [--target NAME] [--listed] [--mirror]
//! cargo conformance --rerecord DIR [--jobs N] [--show N]
//! cargo conformance --choices N [--format ID] [--battles N] [--seed N] [--jobs N] [--show N]
//! ```
//!
//! `--trace` also compares every RNG draw, which pinpoints where random
//! number consumption first differs from Showdown.
//!
//! Every request Showdown sends a player (`|request|` JSON) must match
//! ours at the same decision point too; the first differing one is
//! reported with the path of the first differing value.
//!
//! `--record DIR` saves every *passing* scenario as a golden fixture (with
//! RNG draws) in DIR and lists it in DIR's `MANIFEST` (never overwriting
//! one: an existing name is skipped with a message); `tests/golden.rs`
//! replays them without Node. With `--record-requests`, the fixtures hold
//! Showdown's requests too (they make a fixture about six times larger, so
//! it is opt-in).
//!
//! `--rerecord DIR` replays the scenario stored in each fixture of DIR on
//! Showdown again and rewrites the fixture if pikapro still matches it
//! (for a change to what is compared), with Showdown's requests if
//! it had them. Failures are reported and their fixtures left untouched.
//!
//! Failing scenarios are written to `target/conformance/` as oracle input
//! JSON, replayable with `node tools/showdown-oracle/oracle.mjs FILE`.
//!
//! `--jobs N` runs N battles at a time, each with its own Node process
//! (default: the cores, at most 4).
//!
//! `--legal MODE` draws both teams with pikapro-teams (legal in the
//! format's validation format, from what pikapro implements) instead of
//! the fuzzer's own generator. In the random battles, `--legal sets` is
//! Showdown's random-battle generator: each player gets a seed, pikapro
//! plays pikapro-teams' teams for it and Showdown draws its own
//! (`random_generated_scenario`); `--record` names the fixtures
//! `<format>-generated-<seed>.txt`.
//!
//! `--listed` plays legal teams (`--legal`, `sets` by default) that list
//! the battle-only formes their sets can become (`Charizard-Mega-Y @
//! Charizardite Y`, Palafin-Hero, ...): Showdown validates them with its
//! `TeamValidator` and plays what it makes of them, as its server does;
//! pikapro plays what pikapro-validate makes of them
//! (`random_listed_scenario`). A battle whose teams list none is counted
//! apart and not recorded; `--record` names the others
//! `<format>-listed-<seed>.txt`.
//!
//! `--target NAME` forces sets for a mechanic into the fuzzer's teams (a
//! targeted generator, `pikapro_conformance::view::Target`: `metronome`,
//! `curse`, `octolock`, `megasol`, `futuresight`, `arceus`, ...). It fails
//! at once if the format can't express the target (Leek or Emergency Exit
//! in a Gen 9 preset, a Mega outside Champions, Arceus in Champions,
//! Commander in singles), and with `--legal`
//! (whose teams it can't force). A battle whose teams didn't get the
//! target's defining set (its species was already on the team) is counted
//! apart and not recorded; `--record` names the others
//! `<format>-<target>-<seed>.txt` (older ones: `<format>-forced-<seed>.txt`).
//!
//! `--mirror` gives p2 a copy of p1's team (after `--target`'s sets):
//! every Pokémon meets its double, so speed ties, and the draws that
//! break them, come up far more often. A targeted battle has its target
//! if p1's team got the defining set; `--record` names the fixtures
//! `<format>-mirror-<seed>.txt` (`<format>-<target>-mirror-<seed>.txt`).
//!
//! Every battle is also replayed with events off
//! (`pikapro_conformance::silent_replay_differs`): it must reach the same
//! state after every choice as with them.
//!
//! `--choices N` checks choice strings instead: before each choice
//! of each scenario, N other spellings are tried on both simulators, whose
//! verdicts must agree (`pikapro_conformance::choices`).
//! `PIKAPRO_CHOICES_DUMP=1` prints every probe's two verdicts.

use std::process::ExitCode;

use pikapro::Format;
use pikapro_conformance::cli::{Args, Common, par_seeds, save};
use pikapro_conformance::{
    OracleError, Scenario, SentRequest, check_replay_channels, compare, compare_requests, normalize, oracle_available,
    render_raw, replay, requests, run_showdown_with_requests, scenario_json, showdown_format,
};

fn main() -> ExitCode {
    let mut common = Common::new(Format::gen9_singles(), 20, 0, 3);
    let mut trace = false;
    let mut record: Option<std::path::PathBuf> = None;
    let mut rerecord: Option<std::path::PathBuf> = None;
    let mut choices = 0usize;
    let mut record_requests = false;
    let mut listed = false;
    let mut mirror = false;
    let mut legal: Option<pikapro_teams::Mode> = None;
    let mut target: Option<pikapro_conformance::view::Target> = None;
    let mut args = Args::from_env();
    while let Some(flag) = args.next_flag() {
        if common.take(&flag, &mut args) {
            continue;
        }
        if flag == "--trace" {
            trace = true;
            continue;
        }
        if flag == "--record-requests" {
            record_requests = true;
            continue;
        }
        if flag == "--listed" {
            listed = true;
            continue;
        }
        if flag == "--mirror" {
            mirror = true;
            continue;
        }
        let value = args.value_or_empty();
        let value = value.as_str();
        let num = || value.parse::<u64>().unwrap_or_else(|_| panic!("{flag}: bad number {value:?}"));
        match flag.as_str() {
            "--record" => {
                record = Some(value.into());
                trace = true;
            }
            "--rerecord" => rerecord = Some(value.into()),
            "--choices" => choices = num() as usize,
            "--target" => {
                target = Some(
                    pikapro_conformance::view::Target::from_name(value)
                        .unwrap_or_else(|| panic!("--target: {value:?}")),
                )
            }
            "--legal" => {
                legal = Some(pikapro_teams::Mode::from_name(value).unwrap_or_else(|| panic!("--legal: {value:?}")))
            }
            _ => panic!("unknown flag {flag:?}"),
        }
    }
    let Common { format, battles, seed, jobs, show } = common;
    if listed {
        if target.is_some() {
            eprintln!("--listed with --target: the listed teams are legal teams, which can't be forced");
            return ExitCode::FAILURE;
        }
        legal.get_or_insert(pikapro_teams::Mode::Sets);
    }
    if mirror && (legal.is_some() || choices > 0 || rerecord.is_some()) {
        eprintln!("--mirror mirrors the fuzzer's own teams: not with --legal, --listed, --choices or --rerecord");
        return ExitCode::FAILURE;
    }
    if let Some(t) = target {
        if legal.is_some() {
            eprintln!("--target {} with --legal: the legal team generator's sets can't be forced", t.name());
            return ExitCode::FAILURE;
        }
        if !t.applies(&format) {
            eprintln!("--target {}: {} can't express it (nothing to force)", t.name(), format.id);
            return ExitCode::FAILURE;
        }
    }
    if !oracle_available() {
        eprintln!("oracle unavailable: run `npm ci` in tools/showdown-oracle (needs Node 22.18+)");
        return ExitCode::FAILURE;
    }
    if choices > 0 {
        return check_choices(&format, seed, battles, jobs, choices, show);
    }
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/conformance");
    let _ = std::fs::create_dir_all(&out_dir);
    if let Some(dir) = rerecord {
        return rerecord_fixtures(&dir, jobs, show);
    }

    // Each oracle run starts a Node process, so check scenarios in parallel
    // and report in seed order.
    let generator = legal.map(|m| pikapro_conformance::legal_generator(&format, m).expect("a legal team generator"));
    let how = Draw { generator: generator.as_ref(), target, listed, mirror };
    let outcomes: Vec<(u64, Option<String>, String, bool)> =
        par_seeds(jobs, battles, |i| check(&format, how, seed + i, trace, record.as_deref(), record_requests));

    let (mut pass, mut fail) = (0, 0);
    let missed = outcomes.iter().filter(|o| !o.3).count();
    for (seed, failure, json, _) in outcomes {
        let Some(failure) = failure else {
            pass += 1;
            continue;
        };
        fail += 1;
        let path = out_dir.join(format!("{}-{seed}.json", format.id));
        let saved = !json.is_empty() && save(&path, json);
        if fail <= show {
            let at = if saved { path.display().to_string() } else { "not saved".into() };
            println!("FAIL seed {seed} ({at})\n{failure}\n");
        }
    }
    println!("{}: {pass} passed, {fail} failed", format.id);
    if let Some(t) = target {
        println!(
            "--target {}: {} of {battles} battles have it (the others weren't recorded)",
            t.name(),
            battles as usize - missed
        );
        if missed as u64 == battles && battles > 0 {
            println!("--target {}: no battle has it", t.name());
            return ExitCode::FAILURE;
        }
    }
    if listed {
        println!(
            "--listed: {} of {battles} battles list a battle-only forme (the others weren't recorded)",
            battles as usize - missed
        );
    }
    if fail == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// How a battle's teams are drawn.
#[derive(Clone, Copy)]
struct Draw<'a> {
    generator: Option<&'a pikapro_teams::TeamGenerator>,
    target: Option<pikapro_conformance::view::Target>,
    listed: bool,
    /// p2 plays a copy of p1's team (`--mirror`).
    mirror: bool,
}

/// Runs one scenario on both engines. Returns `(seed, failure, oracle
/// input, whether a `--target` landed or `--listed` listed a forme)`.
fn check(
    format: &Format,
    how: Draw<'_>,
    seed: u64,
    trace: bool,
    record: Option<&std::path::Path>,
    record_requests: bool,
) -> (u64, Option<String>, String, bool) {
    let Draw { generator, target, listed, mirror } = how;
    // (`main` refuses `--legal` with `--target`, and gives `--listed` a
    // generator.) A panic is this seed's failure, not the run's end (the
    // `release-tools` profile unwinds); one while drawing the battle
    // leaves no scenario to save.
    let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match (generator, target) {
        (Some(g), _) if listed => pikapro_conformance::random_listed_scenario(g, seed),
        (Some(g), _) => (pikapro_conformance::random_legal_scenario(g, seed), true),
        (None, t) => pikapro_conformance::scenario_as(format, seed, t, mirror),
    }));
    let (s, landed) = match drawn {
        Ok(drawn) => drawn,
        Err(e) => {
            let failure = format!("pikapro panicked drawing the battle: {}", pikapro_conformance::panic_message(&*e));
            return (seed, Some(failure), String::new(), true);
        }
    };
    let json = scenario_json(&s, showdown_format(&s.format).unwrap_or(""));
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check_scenario(&s, trace)))
        .unwrap_or_else(|e| Err(format!("pikapro panicked: {}", pikapro_conformance::panic_message(&*e))));
    let failure = match checked {
        Ok((expected, sent)) => {
            // A targeted battle without its target isn't recorded: its
            // name would promise a mechanic it doesn't have.
            if let Some(dir) = record.filter(|_| landed) {
                let name = match target {
                    Some(t) if mirror => format!("{}-{}-mirror-{seed}.txt", format.id, t.name()),
                    None if mirror => format!("{}-mirror-{seed}.txt", format.id),
                    Some(t) => format!("{}-{}-{seed}.txt", format.id, t.name()),
                    None if listed => format!("{}-listed-{seed}.txt", format.id),
                    None if generator.is_some_and(|g| g.random_battle().is_some()) => {
                        format!("{}-generated-{seed}.txt", format.id)
                    }
                    None => format!("{}-{seed}.txt", format.id),
                };
                let mut text = pikapro_conformance::fixture::write(&s, &expected);
                if record_requests {
                    text.push_str(&pikapro_conformance::fixture::write_requests(&sent));
                }
                let recorded = pikapro_conformance::fixture::record(dir, &name, &text)
                    .unwrap_or_else(|e| panic!("{}: {e}", dir.join(&name).display()));
                if !recorded {
                    eprintln!("{name} exists: not overwritten (`--rerecord` rewrites goldens)");
                }
            }
            return (seed, None, json, landed);
        }
        Err(failure) => failure,
    };
    (seed, Some(failure), json, landed)
}

/// Runs a scenario on both engines: the raw logs, then each perspective's
/// channel, then the requests (both logs carry them, which `compare`
/// ignores). Returns Showdown's normalised raw log and its requests if
/// everything matches. First, pikapro alone: the same battle with events
/// off must reach the same states (`silent_replay_differs`).
fn check_scenario(s: &Scenario, trace: bool) -> Result<(Vec<String>, Vec<SentRequest>), String> {
    let replayed = replay(s, true);
    if let Some(d) = pikapro_conformance::silent_replay_differs(s, &replayed) {
        return Err(d);
    }
    let ours = render_raw(s, &replayed);
    Err(match run_showdown_with_requests(s, trace) {
        Ok(theirs) => match compare(&ours, &theirs, trace) {
            None => match check_replay_channels(s, &replayed, &normalize(&theirs, trace), trace) {
                Some((p, d)) => format!("{p:?} log diverges at line {}:\n{d}", d.index),
                None => match compare_requests(&ours, &theirs) {
                    None => return Ok((normalize(&theirs, true), requests(&theirs))),
                    Some(d) => format!("the log matches, but {d}"),
                },
            },
            Some(d) => format!("diverges at line {}:\n{d}", d.index),
        },
        // Once the logs diverge, recorded choices stop being valid for
        // Showdown; the divergence is the interesting part.
        Err(OracleError::Failed { log, stderr, .. }) => {
            let rejected = stderr.lines().next().unwrap_or("").to_string();
            match compare(&ours, &log, trace) {
                Some(d) => format!("diverges at line {}:\n{d}\n  showdown rejected: {rejected}", d.index),
                None => format!("oracle rejected a choice: {rejected}"),
            }
        }
        Err(e) => e.to_string(),
    })
}

/// `--rerecord DIR`: re-checks every fixture's scenario and rewrites the
/// passing ones (each through a temporary file and a rename). A fixture
/// that can't be read or replayed is a failure; the others go on.
fn rerecord_fixtures(dir: &std::path::Path, jobs: u64, show: usize) -> ExitCode {
    let paths = match pikapro_conformance::fixture::check_manifest(dir, "txt") {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let failures: Vec<(String, String)> = par_seeds(jobs, paths.len() as u64, |i| {
        let path = &paths[i as usize];
        rerecord_one(path).err().map(|failure| (path.file_name().unwrap().to_string_lossy().into_owned(), failure))
    })
    .into_iter()
    .flatten()
    .collect();
    for (name, failure) in failures.iter().take(show) {
        println!("FAIL {name}\n{failure}\n");
    }
    println!("rerecord: {} passed and rewritten, {} failed", paths.len() - failures.len(), failures.len());
    if failures.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Re-checks one fixture and rewrites it if it still matches Showdown.
fn rerecord_one(path: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("unreadable: {e}"))?;
    let (s, _) = pikapro_conformance::fixture::parse(&text).map_err(|e| format!("unparsable: {e}"))?;
    let had_requests = !pikapro_conformance::fixture::parse_requests(&text).is_empty();
    // A recorded choice pikapro now rejects fails this fixture without a
    // panic (which the release profile's `panic = "abort"` couldn't catch).
    pikapro_conformance::try_replay(&s, false).map_err(|e| format!("pikapro can't replay it: {e}"))?;
    // Any other panic is caught in a profile that unwinds (`cargo
    // conformance` builds `release-tools`) and aborts the run in `release`.
    let (expected, sent) = std::panic::catch_unwind(|| check_scenario(&s, true))
        .unwrap_or_else(|e| Err(format!("pikapro's replay panicked: {}", pikapro_conformance::panic_message(&*e))))?;
    let mut text = pikapro_conformance::fixture::write(&s, &expected);
    if had_requests {
        text.push_str(&pikapro_conformance::fixture::write_requests(&sent));
    }
    pikapro_conformance::write_atomically(path, &text).map_err(|e| format!("unwritable: {e}"))
}

/// `--choices`: probes per decision, compared with Showdown's verdicts.
fn check_choices(format: &Format, seed: u64, battles: u64, jobs: u64, per_decision: usize, show: usize) -> ExitCode {
    let outcomes: Vec<(u64, Result<usize, String>)> = par_seeds(jobs, battles, |i| {
        (seed + i, pikapro_conformance::choices::check_choices(format, seed + i, per_decision, 8))
    });
    let (mut pass, mut fail, mut probes) = (0, 0, 0);
    for (seed, outcome) in outcomes {
        match outcome {
            Ok(n) => {
                pass += 1;
                probes += n;
            }
            Err(e) => {
                fail += 1;
                if fail <= show {
                    println!("FAIL seed {seed}\n{e}\n");
                }
            }
        }
    }
    println!("{}: choices: {pass} passed ({probes} probes), {fail} failed", format.id);
    if fail == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
