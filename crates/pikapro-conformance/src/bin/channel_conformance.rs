//! `channel_conformance`: the player-channel round trip.
//!
//! ```text
//! cargo channel-conformance [--format F] [--battles N] [--seed S] [--jobs J]
//!                           [--source sets|uniform|fuzz|targets|combos|<target>]
//!                           [--masks factored|joint] [--switch-by row|position]
//!                           [--hp percent|exact] [--ots] [--series N] [--reconnect] [--show N] [--record DIR]
//! cargo channel-conformance --showdown [--format F] [--battles N] [--seed S] [--jobs J] [--reconnect]
//!                           [--source fuzz|sets|uniform|targets|combos|<target>] [--record DIR]
//! ```
//!
//! `--reconnect` also starts a new session for each player at every step
//! (decision, with `--showdown`), fed what Showdown sends a player who
//! rejoins the battle's room (the room's log so far, then the current
//! request: `pikapro_conformance::channel::rejoin`); it must give what the
//! session that read everything gives (`check_rejoin`).
//!
//! Each battle is played in a one-battle `VecEnv` with random actions for
//! both players, and each player's channel (our renderer's lines and
//! requests, as Showdown sends them) is fed to a `ChannelSession`, whose
//! view, observation and mask are compared with the environment's at every step
//! (`pikapro_conformance::channel::roundtrip`). `--source targets` cycles
//! through the targeted generators of `view::Target::ALL` by seed;
//! `--source NAME` takes any target by name (the `conformance --target`
//! ones too), and `--source combos` cycles through the combined ones
//! (`Target::COMBOS`). `--ots` plays with Open Team Sheets (`|showteam|`,
//! team preview formats). `--series 3` plays each battle as a best-of-3 series
//! (the environment's series mode, with open sheets): each game's sessions
//! get the series the last game's recorded, and must show the
//! environment's score, game and earlier games' records. Seeds default to
//! 5,000,000.
//!
//! `--showdown` plays conformance scenarios on Showdown instead (the
//! oracle's `players.mjs`, which needs `npm ci` in `tools/showdown-oracle`)
//! and feeds Showdown's own player streams to sessions, against pikapro's
//! typed path of the same battle (`pikapro_conformance::channel_showdown`).
//! Its scenarios come from `--source`: the conformance fuzzer's (`fuzz`,
//! the default), pikapro-teams' legal ones (`sets`, `uniform`), or a
//! targeted generator's (`conformance --target`'s). A failure at a
//! decision that comes after pikapro's own log first differs from
//! Showdown's (`cargo conformance` fails on that battle) is reported apart
//! as the engine's; any other failure, and a name the dex didn't resolve,
//! fails the run. `PIKAPRO_CHANNEL_DUMP=DIR` writes the streams.
//!
//! `PIKAPRO_CHANNEL_DEBUG=1` makes failures verbose: the whole stream
//! instead of its last lines, and the parser's state on a tracker mismatch.
//!
//! `--record DIR` writes every passing battle as a golden channel fixture
//! (`pikapro_conformance::channel_fixture`), which `tests/channel.rs` replays.

use std::process::ExitCode;

use pikapro::Format;
use pikapro_conformance::channel::{Options, Stats, roundtrip};
use pikapro_conformance::cli::{self, Args, Common, par_seeds};
use pikapro_conformance::view::{Failure, Generator, Source, Target};
use pikapro_rl::vec::{MaskLayout, SwitchBy};
use pikapro_view::HpReport;

fn main() -> ExitCode {
    let mut common = Common::new(Format::champions_vgc(), 100, 5_000_000, 3);
    let mut source = "fuzz".to_string();
    let mut options = Options::default();
    let mut showdown = false;
    let mut record: Option<std::path::PathBuf> = None;
    let mut args = Args::from_env();
    while let Some(flag) = args.next_flag() {
        if common.take(&flag, &mut args) {
            continue;
        }
        if flag == "--showdown" {
            showdown = true;
            continue;
        }
        if flag == "--ots" {
            options.ots = true;
            continue;
        }
        if flag == "--reconnect" {
            options.reconnect = true;
            continue;
        }
        let value = args.value_or_empty();
        let value = value.as_str();
        let num = || value.parse::<u64>().unwrap_or_else(|_| panic!("{flag}: bad number {value:?}"));
        match flag.as_str() {
            "--series" => options.series = Some(u8::try_from(num()).unwrap_or_else(|_| panic!("--series: {value:?}"))),
            "--source" => source = value.to_string(),
            "--record" => {
                record = Some(value.into());
                options.keep_logs = true;
            }
            "--masks" => {
                options.masks = match value {
                    "factored" => MaskLayout::Factored,
                    "joint" => MaskLayout::Joint,
                    _ => panic!("--masks: {value:?}"),
                }
            }
            "--switch-by" => {
                options.switch_by = match value {
                    "row" => SwitchBy::Row,
                    "position" => SwitchBy::Position,
                    _ => panic!("--switch-by: {value:?}"),
                }
            }
            "--hp" => {
                options.hp = match value {
                    "percent" => HpReport::Percent,
                    "exact" => HpReport::Exact,
                    _ => panic!("--hp: {value:?}"),
                }
            }
            _ => panic!("unknown flag {flag:?}"),
        }
    }
    let Common { format, battles, seed, jobs, show } = common;
    if options.series.is_some() && (showdown || record.is_some()) {
        println!(
            "--series plays our renderer's channels only, and a fixture is one game: not with --showdown or --record"
        );
        return ExitCode::FAILURE;
    }
    if let Some(dir) = &record {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    }
    if showdown {
        return showdown_run(&format, &source, battles, seed, jobs, show, &options, record.as_deref());
    }
    let sources: Vec<Source> = if source == "targets" {
        Target::ALL.into_iter().map(Source::Target).collect()
    } else if source == "combos" {
        Target::COMBOS.into_iter().filter(|t| t.applies(&format)).map(Source::Target).collect()
    } else {
        vec![Source::from_name(&source).unwrap_or_else(|| panic!("--source: unknown {source:?}"))]
    };
    let generators: Vec<Generator> =
        sources.iter().map(|&s| Generator::new(&format, s).unwrap_or_else(|e| panic!("{e}"))).collect();
    let (generators, options) = (&generators, &options);
    let outcomes: Vec<(u64, &'static str, Result<Stats, Failure>)> = par_seeds(jobs, battles, |i| {
        let s = seed + i;
        let g = &generators[(s % generators.len() as u64) as usize];
        let setup = g.setup(s);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| roundtrip(&setup, options)))
            .unwrap_or_else(|p| Err(Failure::new("panic", "panic", pikapro_conformance::panic_message(&*p))));
        (s, g.source.name(), r)
    });
    let (mut pass, mut fail, mut skipped) = (0u64, 0u64, 0u64);
    let mut total = Stats::default();
    let mut by_fact: std::collections::BTreeMap<String, (u64, u64)> = Default::default();
    for (s, src, r) in &outcomes {
        match r {
            Ok(st) if st.skipped => skipped += 1,
            Ok(st) => {
                pass += 1;
                if let (Some(dir), Some(logs)) = (&record, &st.logs) {
                    save(dir, &format, src, *s, options, logs);
                }
                total.decisions += st.decisions;
                total.views += st.views;
                total.lines += st.lines;
                total.unavailable += st.unavailable;
                total.turns += st.turns;
                total.rejoins += st.rejoins;
                total.ambiguous_rejoins += st.ambiguous_rejoins;
                total.games += st.games;
            }
            Err(f) => {
                fail += 1;
                let e = by_fact.entry(format!("{}/{}", f.check, f.fact)).or_insert((0, *s));
                e.0 += 1;
                if fail <= show as u64 {
                    println!("FAIL {} seed {s} [{src}]\n{f}\n", format.id);
                }
            }
        }
    }
    println!(
        "{}: {pass} passed, {fail} failed, {skipped} skipped as indistinguishable ({} games, {} decisions, {} views, {} lines, {} unavailable choices, {} turns, {} rejoins, {} rejoins where the lines fit two orders)",
        format.id,
        total.games,
        total.decisions,
        total.views,
        total.lines,
        total.unavailable,
        total.turns,
        total.rejoins,
        total.ambiguous_rejoins
    );
    for (fact, (n, s)) in &by_fact {
        println!("  {fact}: {n} (first seed {s})");
    }
    if fail == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Writes a passing battle's channels as a golden fixture.
fn save(dir: &std::path::Path, format: &Format, source: &str, seed: u64, options: &Options, logs: &[Vec<String>; 2]) {
    let text = pikapro_conformance::channel_fixture::record(
        format,
        source,
        seed,
        options.masks,
        options.switch_by,
        options.hp,
        logs,
    )
    .unwrap_or_else(|f| panic!("recording seed {seed}: {f}"));
    let name = format!("{}-{source}-{seed}.txt", format.id);
    let recorded = pikapro_conformance::fixture::record(dir, &name, &text)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.join(&name).display()));
    if !recorded {
        eprintln!("{name} exists: not overwritten (golden fixtures are never rewritten by `--record`)");
    }
}

/// Marks a failure where pikapro's own log diverges from Showdown's.
const ENGINE: &str = "engine: ";

/// Where `--showdown`'s scenarios come from (`--source`).
enum Scenarios {
    Fuzz,
    Legal(Box<pikapro_teams::TeamGenerator>),
    Targets(Vec<Target>),
}

impl Scenarios {
    fn new(format: &Format, source: &str) -> Result<Scenarios, String> {
        Ok(match source {
            "fuzz" => Scenarios::Fuzz,
            "sets" | "uniform" => {
                let mode = pikapro_teams::Mode::from_name(source).ok_or("a team mode")?;
                Scenarios::Legal(Box::new(
                    pikapro_conformance::legal_generator(format, mode).map_err(|e| e.to_string())?,
                ))
            }
            "targets" => Scenarios::Targets(Target::ALL.into_iter().filter(|t| t.applies(format)).collect()),
            "combos" => Scenarios::Targets(Target::COMBOS.into_iter().filter(|t| t.applies(format)).collect()),
            name => {
                let t = Target::from_name(name).ok_or_else(|| format!("--source: unknown {name:?}"))?;
                if !t.applies(format) {
                    return Err(format!("--source {name}: {} can't express it", format.id));
                }
                Scenarios::Targets(vec![t])
            }
        })
    }

    fn scenario(&self, format: &Format, seed: u64) -> pikapro_conformance::Scenario {
        match self {
            Scenarios::Fuzz => pikapro_conformance::view_scenario(format, seed, None),
            Scenarios::Legal(g) => pikapro_conformance::random_legal_scenario(g, seed),
            Scenarios::Targets(ts) => {
                pikapro_conformance::view_scenario(format, seed, Some(ts[(seed % ts.len() as u64) as usize]))
            }
        }
    }
}

/// Where pikapro's own log first differs from Showdown's (a line, or a
/// request: `cargo conformance` fails on either), as each side's number
/// of decisions (requests asking for an action) before that point; `None`
/// if they agree. A session's failure at an earlier decision saw only
/// lines and requests both simulators wrote.
fn engine_decisions(scenario: &pikapro_conformance::Scenario) -> Option<[usize; 2]> {
    let ours = pikapro_conformance::run_ours_with_requests(scenario);
    let theirs = match pikapro_conformance::run_showdown_with_requests(scenario, false) {
        Ok(theirs) => theirs,
        // Showdown refused a choice pikapro took: its log stops there (a
        // shorter log differs at its end, if not before).
        Err(pikapro_conformance::OracleError::Failed { log, .. }) => log,
        Err(_) => return None,
    };
    let line = pikapro_conformance::compare(&ours, &theirs, false).map(|d| d.index);
    let request = pikapro_conformance::compare_requests(&ours, &theirs).map(|d| d.index);
    if line.is_none() && request.is_none() {
        return None;
    }
    let (mut lines, mut requests, mut decisions) = (0usize, 0usize, [0usize; 2]);
    for l in &theirs {
        if line.is_some_and(|i| lines >= i) || request.is_some_and(|i| requests >= i) {
            break;
        }
        if let Some(rest) = l.strip_prefix("|request|") {
            requests += 1;
            let (side, json) = rest.split_once('|').unwrap_or(("", rest));
            if !json.contains("\"wait\":true") {
                match side {
                    "p1" => decisions[0] += 1,
                    "p2" => decisions[1] += 1,
                    _ => {}
                }
            }
        } else if !pikapro_conformance::normalize(std::slice::from_ref(l), false).is_empty() {
            lines += 1;
        }
    }
    Some(decisions)
}

/// `--showdown`: conformance scenarios played on Showdown, whose player
/// streams go to sessions (`pikapro_conformance::channel_showdown`).
#[allow(clippy::too_many_arguments)]
fn showdown_run(
    format: &Format,
    source: &str,
    battles: u64,
    seed: u64,
    jobs: u64,
    show: usize,
    options: &Options,
    record: Option<&std::path::Path>,
) -> ExitCode {
    use pikapro_conformance::channel_showdown::{ShowdownStats, check_at, showdown_streams};
    let scenarios = match Scenarios::new(format, source) {
        Ok(s) => s,
        Err(e) => {
            println!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let scenarios = &scenarios;
    if !pikapro_conformance::oracle_available() {
        println!("the oracle is unavailable: run `npm ci` in tools/showdown-oracle");
        return ExitCode::FAILURE;
    }
    let outcomes: Vec<(u64, Result<ShowdownStats, String>)> = par_seeds(jobs, battles, |i| {
        let s = seed + i;
        let scenario = scenarios.scenario(format, s);
        let r = match showdown_streams(&scenario) {
            Ok(streams) => {
                // `PIKAPRO_CHANNEL_DUMP=DIR`: each player's stream to a file.
                if let Some(dir) = std::env::var_os("PIKAPRO_CHANNEL_DUMP") {
                    for (side, lines) in ["p1", "p2"].iter().zip(&streams) {
                        let path = std::path::Path::new(&dir).join(format!("{}-{s}-{side}.txt", format.id));
                        cli::save(&path, lines.join("\n"));
                    }
                }
                let r = check_at(&scenario, &streams, options);
                if let (Some(dir), Ok(st)) = (record, &r)
                    && !st.skipped
                {
                    save(dir, format, "showdown", s, options, &streams);
                }
                r.map_err(|(f, at)| {
                    // A failure at a decision after pikapro's own
                    // log first differs from Showdown's (`cargo
                    // conformance` fails too) isn't the parser's to
                    // match; one before it is.
                    let after =
                        at.is_some_and(|(side, k)| engine_decisions(&scenario).is_some_and(|d| k >= d[side.index()]));
                    if after { format!("{ENGINE}{f}") } else { f.to_string() }
                })
            }
            Err(e) => Err(format!("oracle: {e}")),
        };
        (s, r)
    });
    let (mut pass, mut fail, mut skipped, mut engine) = (0u64, 0u64, 0u64, Vec::new());
    let mut total = ShowdownStats::default();
    for (s, r) in &outcomes {
        match r {
            Ok(st) if st.skipped => skipped += 1,
            Err(e) if e.starts_with(ENGINE) => engine.push(*s),
            Ok(st) if st.unknown_names > 0 => {
                fail += 1;
                if fail <= show as u64 {
                    println!(
                        "FAIL {} seed {s} (Showdown)\n{} names the dex didn't resolve\n",
                        format.id, st.unknown_names
                    );
                }
            }
            Ok(st) => {
                pass += 1;
                total.decisions += st.decisions;
                total.lines += st.lines;
                total.unknown_lines += st.unknown_lines;
                total.unknown_names += st.unknown_names;
                total.rejoins += st.rejoins;
                total.ambiguous_rejoins += st.ambiguous_rejoins;
            }
            Err(e) => {
                fail += 1;
                if fail <= show as u64 {
                    println!("FAIL {} seed {s} (Showdown)\n{e}\n", format.id);
                }
            }
        }
    }
    println!(
        "{} (Showdown): {pass} passed, {fail} failed, {skipped} skipped as indistinguishable, {} where pikapro's log differs from Showdown's ({} decisions, {} lines, {} unknown lines, {} unknown names, {} rejoins, {} where the lines fit two orders)",
        format.id,
        engine.len(),
        total.decisions,
        total.lines,
        total.unknown_lines,
        total.unknown_names,
        total.rejoins,
        total.ambiguous_rejoins
    );
    if !engine.is_empty() {
        println!("  pikapro diverges from Showdown (cargo conformance --seed S --battles 1): seeds {engine:?}");
    }
    if fail == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
