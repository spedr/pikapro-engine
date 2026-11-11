//! `view_dump`: decision records and our projected view for fuzzed battles
//! (`tools/view-oracle/SCHEMA.md`).
//!
//! ```text
//! cargo run --release -p pikapro-conformance --bin view_dump -- \
//!     [--format F] [--battles N] [--seed S] [--source sets|uniform|fuzz|<target>] [--out PREFIX]
//! ```
//!
//! Writes `PREFIX.decisions.jsonl` (decision records, the input of
//! `tools/view-oracle/pokeenv_view.py`) and `PREFIX.view.jsonl` (our state
//! records, which `tools/view-oracle/diff/diff.py` compares with
//! poke-env's). The default prefix is
//! `target/view-conformance/<format>-<seed>-<battles>`. Battle `k` has
//! seed `S + k`; the teams and choices follow the schema's generator
//! recipe (`pikapro.RandomTeams(format, source).pair(seed)`, choices from
//! `RandomPolicy(seed ^ 0x5EED)`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pikapro::Format;
use pikapro_conformance::cli::Args;
use pikapro_conformance::view::{Checks, Driver, Generator, Source, dump};

fn main() -> ExitCode {
    let mut format = Format::champions_vgc();
    let (mut battles, mut seed) = (10u64, 4_500_000u64);
    let mut source = Source::Sets;
    let mut out: Option<PathBuf> = None;
    let mut args = Args::from_env();
    while let Some(flag) = args.next_flag() {
        match flag.as_str() {
            "--format" => format = args.format(&flag),
            "--battles" => battles = args.num(&flag),
            "--seed" => seed = args.num(&flag),
            "--source" => {
                let v = args.value(&flag);
                source = Source::from_name(&v).unwrap_or_else(|| panic!("--source: unknown {v:?}"));
            }
            "--out" => out = Some(args.value(&flag).into()),
            _ => panic!("unknown flag {flag:?}"),
        }
    }
    let generator = Generator::new(&format, source).unwrap_or_else(|e| panic!("{e}"));
    let prefix = out.unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../target/view-conformance/{}-{seed}-{battles}", format.id))
    });
    if let Some(dir) = prefix.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let (mut decisions, mut states, mut records, mut failed) = (String::new(), String::new(), 0, 0);
    for k in 0..battles {
        let setup = generator.setup(seed + k);
        let (d, result) = dump::dump(&setup, Driver::random(seed + k), Checks::NONE);
        if let Err(e) = result {
            eprintln!("seed {}: {e}", seed + k);
            failed += 1;
        }
        decisions.push_str(&d.decisions);
        states.push_str(&d.states);
        records += d.records;
    }
    let with = |ext: &str| PathBuf::from(format!("{}.{ext}", prefix.display()));
    std::fs::write(with("decisions.jsonl"), decisions).expect("writable decisions");
    std::fs::write(with("view.jsonl"), states).expect("writable states");
    eprintln!("{battles} battles, {records} records -> {}.{{decisions,view}}.jsonl", prefix.display());
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
