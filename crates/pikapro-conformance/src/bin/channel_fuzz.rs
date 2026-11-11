//! `channel_fuzz`: plausible but unusual player channels through whole
//! `ChannelSession`s (`pikapro_conformance::channel_fuzz`).
//!
//! ```text
//! cargo run --profile release-tools -p pikapro-conformance --bin channel_fuzz -- \
//!     [--battles N] [--seed S] [--format F]... [--show N] [--dump DIR]
//! ```
//!
//! `--battles` per format and source (default 20, over the seven presets
//! it covers, all but championsbss), seeds from `--seed` (default 8,130,000). Build it with
//! the `release-tools` profile, which unwinds: panics are caught and
//! reported, where `release` (`panic = "abort"`) stops at the first.
//! `--dump DIR` writes each illegal channel that panicked or left the
//! tracker inconsistent. Exits 1 if anything was found.

use std::path::PathBuf;
use std::process::ExitCode;

use pikapro_conformance::channel_fuzz::{FuzzOptions, run};
use pikapro_conformance::cli::Args;

fn main() -> ExitCode {
    let mut opts = FuzzOptions::new(20, 8_130_000);
    let mut formats = Vec::new();
    let mut show = 200;
    let mut args = Args::from_env();
    while let Some(flag) = args.next_flag() {
        let flag = flag.as_str();
        match flag {
            "--battles" => opts.battles = args.num(flag),
            "--seed" => opts.seed = args.num(flag),
            "--format" => formats.push(args.format(flag)),
            "--show" => show = args.num(flag) as usize,
            "--dump" => opts.dump = Some(PathBuf::from(args.value(flag))),
            _ => panic!("unknown flag {flag:?}"),
        }
    }
    if !formats.is_empty() {
        opts.formats = formats;
    }
    let stats = run(&opts, &mut |format, s| eprintln!("{}: done ({} channels so far)", format.id, s.games));
    print!("{}", stats.report(show));
    if stats.clean() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
