//! `feature_audit`: the encoded features against what a perfect observer
//! of the public log computes (`view::features`), over many battles.
//!
//! ```text
//! cargo run --release -p pikapro-conformance --bin feature_audit -- \
//!     [--format F] [--battles N] [--seed S] [--jobs J] [--show K]
//!     [--source sets|uniform|fuzz|targets|combos|<target>|teams:FILE] [--dump SEED]
//! ```
//!
//! `teams:FILE` pairs the teams of a teams file (Showdown's backup format,
//! `=== [format] name ===` headers: tournament pastes, say): battle `seed`
//! plays team `seed % n` against team `(seed * 7 + 3) % n`. Choices
//! are uniformly random legal ones. Prints each rule's counts, the share of
//! battles that broke it, and examples. `--dump SEED` audits that one
//! battle and prints its report and omniscient log.

use std::process::ExitCode;

use pikapro::Format;
use pikapro_conformance::cli::{Args, Common};
use pikapro_conformance::view::features::{FeatureAudit, audit_battle};
use pikapro_conformance::view::{Driver, Generator, Setup, Source, Target};
use pikapro_protocol::TeamSheet;

enum Teams {
    Generated(Vec<Generator>),
    File(Vec<TeamSheet>),
}

/// The teams of a teams file as training imports them
/// (`pikapro_rl::teams::import_pool`: validated, and played as the
/// validator leaves them, so a listed Mega forme starts as its base forme),
/// that a battle accepts.
fn load(path: &str, format: &Format) -> Vec<TeamSheet> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut blocks = vec![String::new()];
    for line in text.lines() {
        if line.starts_with("===") {
            blocks.push(String::new());
        } else if let Some(b) = blocks.last_mut() {
            b.push_str(line);
            b.push('\n');
        }
    }
    blocks.retain(|b| !b.trim().is_empty());
    let (pool, rejected) = pikapro_rl::teams::import_pool(&blocks, format);
    eprintln!("{} of {} teams rejected", rejected.len(), blocks.len());
    pool.into_iter()
        .filter(|sheet| {
            pikapro::Battle::new(
                format.clone(),
                [sheet.team.clone(), sheet.team.clone()],
                pikapro::BattleOptions::new(pikapro::Seed::fast(1)),
            )
            .is_ok()
        })
        .collect()
}

fn main() -> ExitCode {
    let mut common = Common::new(Format::champions_vgc(), 200, 9_100_000, 3);
    let mut source = "sets".to_string();
    let mut dump: Option<u64> = None;
    let mut args = Args::from_env();
    while let Some(flag) = args.next_flag() {
        if common.take(&flag, &mut args) {
            continue;
        }
        match flag.as_str() {
            "--source" => source = args.value(&flag),
            "--dump" => dump = Some(args.num(&flag)),
            _ => panic!("unknown flag {flag:?}"),
        }
    }
    let Common { format, battles, seed, jobs, show } = common;
    let teams = if let Some(path) = source.strip_prefix("teams:") {
        let sheets = load(path, &format);
        assert!(!sheets.is_empty(), "{path}: no team valid for {}", format.id);
        eprintln!("{} teams from {path}", sheets.len());
        Teams::File(sheets)
    } else {
        let sources: Vec<Source> = match source.as_str() {
            "targets" => Target::ALL.into_iter().filter(|t| t.applies(&format)).map(Source::Target).collect(),
            "combos" => Target::COMBOS.into_iter().filter(|t| t.applies(&format)).map(Source::Target).collect(),
            name => vec![Source::from_name(name).unwrap_or_else(|| panic!("--source: unknown {name:?}"))],
        };
        Teams::Generated(
            sources.iter().map(|&s| Generator::new(&format, s).unwrap_or_else(|e| panic!("{e}"))).collect(),
        )
    };
    let setup = |seed: u64| -> Setup {
        match &teams {
            Teams::Generated(g) => g[(seed % g.len() as u64) as usize].setup(seed),
            Teams::File(sheets) => {
                let n = sheets.len() as u64;
                let (a, b) = (seed % n, (seed.wrapping_mul(7) + 3) % n);
                Setup {
                    format: format.clone(),
                    seed,
                    sheets: [sheets[a as usize].clone(), sheets[b as usize].clone()],
                    view: None,
                }
            }
        }
    };
    let setup = &setup;
    if let Some(seed) = dump {
        let s = setup(seed);
        let mut audit = FeatureAudit::new(50);
        let _ = audit_battle(&s, Driver::random(seed), &mut audit);
        print!("{}", audit.report(50));
        let mut battle = s.battle();
        let mut driver = Driver::random(seed);
        let names = s.names();
        let mut log = Vec::new();
        while !battle.is_over() {
            if !matches!(driver.advance(&mut battle, &names, &mut log), Ok(true)) {
                break;
            }
        }
        for line in s.renderer(pikapro_protocol::Perspective::Omniscient).log(battle.events()) {
            println!("{line}");
        }
        return ExitCode::SUCCESS;
    }
    let total = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..jobs)
            .map(|j| {
                scope.spawn(move || {
                    let mut audit = FeatureAudit::new(show);
                    for i in (0..battles).filter(|i| i % jobs == j) {
                        let s = setup(seed + i);
                        if let Err(f) = audit_battle(&s, Driver::random(s.seed), &mut audit) {
                            eprintln!("seed {}: {f}", s.seed);
                        }
                    }
                    audit
                })
            })
            .collect();
        let mut total = FeatureAudit::new(show);
        for h in handles {
            total.merge(h.join().expect("worker panicked"));
        }
        total
    });
    println!("{} ({source}):", format.id);
    print!("{}", total.report(show));
    let broken = total.broken();
    if broken.is_empty() {
        ExitCode::SUCCESS
    } else {
        println!("broken: {broken:?}");
        ExitCode::FAILURE
    }
}
