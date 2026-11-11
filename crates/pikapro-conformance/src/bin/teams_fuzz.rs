//! `teams_fuzz`: checks that pikapro-teams' random teams are legal:
//! every team must be accepted by Showdown's team validator, by
//! pikapro-validate and by the engine (see docs/CONFORMANCE.md).
//!
//! ```sh
//! cargo run --release -p pikapro-conformance --bin teams_fuzz -- --teams 2000 --jobs 2
//! cargo run --release -p pikapro-conformance --bin teams_fuzz -- --format championsvgc --mode sets --verbose
//! cargo run --release -p pikapro-conformance --bin teams_fuzz -- --teams 3 --record crates/pikapro-teams/tests/golden.txt
//! ```
//!
//! Its seeds are 2,300,000–2,399,999, apart from the other tools'.

use std::collections::BTreeMap;
use std::process::ExitCode;

use pikapro::Format;
use pikapro_conformance::cli::Args;
use pikapro_conformance::oracle_available;
use pikapro_conformance::teams::{Coverage, TeamOutcome, check_teams, golden_case};
use pikapro_teams::{Mode, Options, Spreads, TeamGenerator};
use pikapro_validate::SetInfo;
use pikapro_validate::cases::set_line;

const USAGE: &str = "\
teams_fuzz - check pikapro-teams' teams against Showdown's team validator

USAGE:
    teams_fuzz [--format championsvgc|championsbss|gen9vgc|gen9doubles|gen9singles|gen9ou|all]
               [--mode uniform|sets|all] [--teams N] [--seed S] [--batch N]
               [--jobs N] [--show N] [--spreads competitive|showdown]
               [--record FILE] [--verbose]

--teams N     teams per format and mode (default 500)
--seed S      first seed (default 2300000; this tool's range is 2,300,000-2,399,999)
--batch N     teams per Showdown validator run (default 500)
--jobs N      validator runs at once (default 2)
--record FILE append the first N teams of each format and mode (N = --teams)
              to FILE as golden cases (pikapro_validate::cases format)
--verbose     print every species' count and every redraw rule
";

struct Opts {
    formats: Vec<&'static str>,
    modes: Vec<Mode>,
    teams: u64,
    seed: u64,
    batch: u64,
    jobs: u64,
    show: usize,
    spreads: Spreads,
    record: Option<String>,
    verbose: bool,
}

const FORMATS: [&str; 6] = ["championsvgc", "championsbss", "gen9vgc", "gen9doubles", "gen9singles", "gen9ou"];

fn parse() -> Result<Opts, String> {
    let mut args = Args::from_env();
    let mut o = Opts {
        formats: FORMATS.to_vec(),
        modes: Mode::ALL.to_vec(),
        teams: 500,
        seed: 2_300_000,
        batch: 500,
        jobs: 2,
        show: 5,
        spreads: Spreads::Competitive,
        record: None,
        verbose: false,
    };
    while let Some(flag) = args.next_flag() {
        let flag = flag.as_str();
        match flag {
            "--format" => {
                let v = args.value(flag);
                o.formats = if v == "all" {
                    FORMATS.to_vec()
                } else {
                    vec![*FORMATS.iter().find(|f| **f == v).ok_or(format!("unknown format {v}"))?]
                };
            }
            "--mode" => {
                let v = args.value(flag);
                o.modes = if v == "all" {
                    Mode::ALL.to_vec()
                } else {
                    vec![Mode::from_name(&v).ok_or(format!("unknown mode {v}"))?]
                };
            }
            "--teams" => o.teams = args.num(flag),
            "--seed" => o.seed = args.num(flag),
            "--batch" => o.batch = args.num(flag).max(1),
            "--jobs" => o.jobs = args.num(flag).max(1),
            "--show" => o.show = args.num(flag) as usize,
            "--spreads" => {
                o.spreads = match args.value(flag).as_str() {
                    "competitive" => Spreads::Competitive,
                    "showdown" => Spreads::Showdown,
                    s => return Err(format!("unknown spreads {s}")),
                }
            }
            "--record" => o.record = Some(args.value(flag)),
            "--verbose" => o.verbose = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            a => return Err(format!("unknown argument {a}\n\n{USAGE}")),
        }
    }
    Ok(o)
}

fn show(g: &TeamGenerator, out: &TeamOutcome) {
    let dex = g.validation_format().dex;
    println!("--- {} {} seed {}", g.format().id, g.mode().name(), out.seed);
    for s in &out.generated.team.sets {
        println!("    set {}", set_line(dex, s, &SetInfo { gender: s.gender, ..SetInfo::default() }));
    }
    for m in &out.showdown {
        println!("  showdown: {m}");
    }
    for m in &out.ours {
        println!("  ours:     {m}");
    }
    if let Some(e) = &out.engine {
        println!("  engine:   {e}");
    }
    for (ours, theirs) in &out.replayed {
        println!("  played:   {ours} -> Showdown's {theirs}");
    }
}

/// Runs one format and mode; returns whether every team was accepted.
fn run(o: &Opts, preset: &str, mode: Mode, recorded: &mut String) -> Result<bool, String> {
    let format = Format::by_id(preset).ok_or("no preset")?;
    let options = Options { spreads: o.spreads, ..Options::default() };
    let g = TeamGenerator::with_options(&format, mode, options).map_err(|e| e.to_string())?;
    let batches: Vec<std::ops::Range<u64>> = (0..o.teams.div_ceil(o.batch))
        .map(|b| {
            let start = o.seed + b * o.batch;
            start..(start + o.batch).min(o.seed + o.teams)
        })
        .collect();
    let mut outcomes: Vec<TeamOutcome> = Vec::new();
    for chunk in batches.chunks(o.jobs as usize) {
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = chunk.iter().map(|r| scope.spawn(|| check_teams(&g, r.clone()))).collect();
            handles.into_iter().map(|h| h.join().expect("worker panicked")).collect()
        });
        for r in results {
            outcomes.extend(r.map_err(|e| e.to_string())?);
        }
    }
    let mut cov = Coverage::default();
    let (mut showdown, mut ours, mut engine, mut played, mut shown) = (0, 0, 0, 0, 0);
    for out in &outcomes {
        cov.add(&format, out);
        showdown += u64::from(out.showdown.is_empty());
        ours += u64::from(out.ours.is_empty());
        engine += u64::from(out.engine.is_none());
        played += u64::from(out.replayed.is_empty());
        if !out.accepted() && shown < o.show {
            shown += 1;
            show(&g, out);
        }
        if o.record.is_some() && out.accepted() && out.seed < o.seed + o.teams {
            recorded.push_str(&golden_case(&g, out));
        }
    }
    let n = outcomes.len() as u64;
    let dex = format.dex;
    println!(
        "{preset} {} ({}): seeds {}-{}: Showdown accepts {showdown}/{n}, pikapro-validate {ours}/{n}, engine {engine}/{n}, \
         played as generated {played}/{n}",
        mode.name(),
        g.validation_format().id,
        o.seed,
        o.seed + n - 1,
    );
    let pct = |a: usize, b: usize| if b == 0 { 0 } else { a * 100 / b };
    println!(
        "  coverage: {} sets; {}/{} Pokédex numbers ({}%), {} species/formes, {} moves, {} items, {} abilities, {} natures",
        cov.sets,
        cov.numbers.len(),
        g.species_numbers(),
        pct(cov.numbers.len(), g.species_numbers()),
        cov.species.len(),
        cov.moves.len(),
        cov.items.len(),
        cov.abilities.len(),
        cov.natures.len(),
    );
    println!(
        "  moves per set: 1: {}, 2: {}, 3: {}, 4: {}",
        cov.move_counts[1], cov.move_counts[2], cov.move_counts[3], cov.move_counts[4]
    );
    let mut by_count: Vec<(SpeciesIdCount, &str)> =
        cov.species.iter().map(|(&s, &c)| ((c, s.0), dex.species(s).name)).collect();
    by_count.sort();
    if let (Some(lo), Some(hi)) = (by_count.first(), by_count.last()) {
        println!("  species draws: least {} ({}), most {} ({})", lo.0.0, lo.1, hi.0.0, hi.1);
    }
    let mut items: Vec<(u64, &str)> = cov.items.iter().map(|(&i, &c)| (c, dex.item(i).name)).collect();
    items.sort_by(|a, b| b.cmp(a));
    let top: Vec<String> = items.iter().take(5).map(|(c, name)| format!("{name} {c}")).collect();
    println!("  top items: {}", top.join(", "));
    let total = cov.redraws.total();
    println!("  redraws: {total} ({}.{:02} per team)", total / n.max(1), (total % n.max(1)) * 100 / n.max(1));
    let mut rules: Vec<(u64, &str)> = cov.redraws.iter().map(|(k, c)| (c, k)).collect();
    rules.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    for (c, k) in rules.iter().take(if o.verbose { usize::MAX } else { 8 }) {
        println!("    {c:>8}  {k}");
    }
    if o.verbose {
        let mut all: BTreeMap<u64, Vec<&str>> = BTreeMap::new();
        for ((c, _), name) in &by_count {
            all.entry(*c).or_default().push(name);
        }
        for (c, names) in all {
            println!("    {c:>5}: {}", names.join(", "));
        }
    }
    Ok(showdown == n && ours == n && engine == n)
}

/// `(count, species index)`, sortable.
type SpeciesIdCount = (u64, u16);

fn main() -> ExitCode {
    let o = match parse() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if !oracle_available() {
        eprintln!("error: node or tools/showdown-oracle/node_modules is missing (run `npm ci` there)");
        return ExitCode::FAILURE;
    }
    let mut ok = true;
    let mut recorded = String::new();
    for preset in &o.formats {
        for &mode in &o.modes {
            match run(&o, preset, mode, &mut recorded) {
                Ok(pass) => ok &= pass,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    if let Some(path) = &o.record {
        let text = std::fs::read_to_string(path).unwrap_or_default() + &recorded;
        if let Err(e) = std::fs::write(path, text) {
            eprintln!("error: {path}: {e}");
            return ExitCode::FAILURE;
        }
        println!("recorded {} cases in {path}", recorded.matches("\nend\n").count());
    }
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
