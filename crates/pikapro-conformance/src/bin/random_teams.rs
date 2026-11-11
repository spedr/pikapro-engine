//! Diffs pikapro-teams' port of Showdown's random-battle team generator
//! against Showdown's (`tools/showdown-oracle/random-teams.mjs`) over many
//! seeds, and shows the first teams that differ with both generators'
//! draws side by side.
//!
//! ```sh
//! cargo run --release -p pikapro-conformance --bin random_teams -- \
//!     --format gen9randombattle --seeds 100000 [--start 0] [--sodium] [--chunk 5000] [--jobs 4] [--show 3]
//! cargo run --release -p pikapro-conformance --bin random_teams -- --format gen9randombattle --trace 17
//! cargo run --release -p pikapro-conformance --bin random_teams -- --record crates/pikapro-teams/tests/random_battle.txt
//! ```
//!
//! Seed `n` is `gen5,<n in 16 hex digits>`, or with `--sodium` a
//! `sodium,<32 hex digits>` seed drawn from `n` (as Showdown makes its
//! own). Exits with status 1 if a team differs.

use pikapro::Format;
use pikapro_conformance::cli::{Args, default_jobs, par_seeds};
use pikapro_conformance::oracle_available;
use pikapro_conformance::random_teams::{self, SeedKind, compare, run_oracle, seed, trace_report};
use pikapro_teams::random_battle::RandomBattle;

fn main() {
    let mut args = Args::from_env();
    let mut format = RandomBattle::Singles;
    let (mut seeds, mut start, mut chunk, mut jobs, mut show) = (1000u64, 0u64, 5000u64, default_jobs(), 3usize);
    let mut kind = SeedKind::Gen5;
    let mut trace = None;
    let mut record: Option<String> = None;
    while let Some(flag) = args.next_flag() {
        match flag.as_str() {
            "--format" => {
                let v = args.value(&flag);
                format = RandomBattle::for_format(&v).unwrap_or_else(|| panic!("--format: not a random battle: {v:?}"));
            }
            "--seeds" => seeds = args.num(&flag),
            "--start" => start = args.num(&flag),
            "--chunk" => chunk = args.num(&flag).max(1),
            "--jobs" => jobs = args.num(&flag).max(1),
            "--show" => show = args.num(&flag) as usize,
            "--sodium" => kind = SeedKind::Sodium,
            "--trace" => trace = Some(args.num(&flag)),
            "--record" => record = Some(args.value(&flag)),
            other => panic!("unknown flag {other:?}"),
        }
    }
    if !oracle_available() {
        eprintln!("oracle unavailable: run `npm ci` in tools/showdown-oracle (needs Node 22.18+)");
        std::process::exit(2);
    }
    if let Some(path) = record {
        match random_teams::record_file() {
            Ok(text) => {
                std::fs::write(&path, text).unwrap_or_else(|e| panic!("{path}: {e}"));
                println!("wrote {path}");
            }
            Err(e) => {
                println!("not recorded: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let game = Format::by_id(format.format_id()).expect("a preset");
    if let Some(n) = trace {
        // One seed: both teams and both generators' draws.
        let (s, text) = seed(kind, n);
        let oracle = run_oracle(format, std::slice::from_ref(&text), false).unwrap_or_else(|e| panic!("{e}"));
        println!("== {text}");
        match &oracle[0].team {
            Ok((sets, packed)) => {
                println!("Showdown:\n    {}\n    {packed}", sets.split(']').collect::<Vec<_>>().join("\n    "))
            }
            Err(e) => println!("Showdown: error: {e}"),
        }
        println!("{}", compare(format, s, &oracle[0], &game).unwrap_or_else(|| "pikapro: the same".into()));
        println!("{}", trace_report(format, s, &text).unwrap_or_else(|e| e));
        return;
    }
    let chunks = seeds.div_ceil(chunk);
    let results = par_seeds(jobs, chunks, |c| {
        let from = start + c * chunk;
        let to = (from + chunk).min(start + seeds);
        let list: Vec<_> = (from..to).map(|n| seed(kind, n)).collect();
        let texts: Vec<String> = list.iter().map(|(_, t)| t.clone()).collect();
        let oracle = run_oracle(format, &texts, false).unwrap_or_else(|e| panic!("{e}"));
        let mut failures = Vec::new();
        let mut errors = 0u64;
        for ((s, text), o) in list.iter().zip(&oracle) {
            errors += u64::from(o.team.is_err());
            if let Some(diff) = compare(format, *s, o, &game) {
                failures.push((*s, text.clone(), diff));
            }
        }
        (to - from, errors, failures)
    });
    let total: u64 = results.iter().map(|r| r.0).sum();
    let errors: u64 = results.iter().map(|r| r.1).sum();
    let failures: Vec<_> = results.into_iter().flat_map(|r| r.2).collect();
    for (s, text, diff) in failures.iter().take(show) {
        println!("== {text}\n{diff}");
        match trace_report(format, *s, text) {
            Ok(r) => println!("{r}"),
            Err(e) => println!("(no trace: {e})"),
        }
    }
    let kind = if kind == SeedKind::Gen5 { "gen5" } else { "sodium" };
    println!(
        "{}: {} of {total} {kind} seeds from {start} differ ({errors} where Showdown's generator throws)",
        format.format_id(),
        failures.len()
    );
    if !failures.is_empty() {
        std::process::exit(1);
    }
}
