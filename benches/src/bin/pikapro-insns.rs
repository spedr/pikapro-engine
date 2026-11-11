//! `pikapro-insns`: instruction-count regression checks.
//!
//! Wall-clock benchmarks are noisy on shared machines. This harness runs
//! each workload under Valgrind's callgrind, counting only the instructions
//! executed inside [`measured`] (setup is excluded with `--toggle-collect`),
//! and compares the counts with a committed baseline. The counts are
//! deterministic for a given build, toolchain and C library, so a small
//! tolerance catches real slowdowns without flaking.
//!
//! ```text
//! cargo bench-insns                   # check every workload against the baseline
//! cargo bench-insns check battle/championsvgc fork/championsvgc
//! cargo bench-insns update            # rewrite the baseline after an intended change
//! cargo bench-insns list
//! ```
//!
//! Options: `--baseline FILE` (default `benches/insns-baseline.txt`),
//! `--tolerance PCT` (default 2: a workload more than 2% above its baseline
//! fails), `--save FILE` (also writes the counts measured, in the
//! baseline's format). `run NAME` runs one workload directly (what valgrind
//! executes).
//!
//! The baseline is recorded on Ubuntu 22.04 (glibc 2.35, valgrind 3.18),
//! and its counts are exact only there. A change that makes the engine do
//! more work on purpose moves the counts: on that system, run `cargo
//! bench-insns update` and commit `benches/insns-baseline.txt` with the
//! change, saying why. Update it after a speed-up too, so the gain is
//! locked in.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use pikapro_bench as w;

/// Where the instructions are counted: callgrind's `--toggle-collect`
/// starts counting when this is entered and stops when it returns.
#[inline(never)]
fn measured<R>(f: impl FnOnce() -> R) -> R {
    std::hint::black_box(f())
}

/// A workload: its name and what it runs (setup, then [`measured`]).
struct Workload {
    name: String,
    run: Box<dyn Fn() -> u64>,
}

fn workload(name: impl Into<String>, run: impl Fn() -> u64 + 'static) -> Workload {
    Workload { name: name.into(), run: Box::new(run) }
}

/// The stack of the thread a workload runs on (`run`).
const WORKLOAD_STACK: usize = 64 << 20;

/// Battles per format for the step-throughput workloads.
const BATTLES: u64 = 500;
/// Decision points (paused battles) for the per-request workloads.
const SNAPSHOTS: usize = 200;

fn workloads() -> Vec<Workload> {
    let mut out = Vec::new();
    for id in w::FORMATS {
        out.push(workload(format!("battle/{id}"), move || {
            let format = w::format(id);
            let teams = w::teams(&format);
            measured(|| (0..BATTLES).map(|seed| w::play(&format, &teams, seed, false) as u64).sum())
        }));
    }
    out.push(workload("battle-events/championsvgc", || {
        let format = w::format("championsvgc");
        let teams = w::teams(&format);
        measured(|| (0..BATTLES / 2).map(|seed| w::play(&format, &teams, seed, true) as u64).sum())
    }));
    out.push(workload("fork/championsvgc", || {
        let format = w::format("championsvgc");
        let snapshots = w::snapshots(&format, &w::teams(&format), 0, 50);
        measured(|| snapshots.iter().map(|b| w::fork(b, 100)).sum())
    }));
    for id in w::FORMATS {
        out.push(workload(format!("legal/{id}"), move || {
            let format = w::format(id);
            let snapshots = w::snapshots(&format, &w::teams(&format), 0, SNAPSHOTS);
            measured(|| {
                let mut n = 0;
                for _ in 0..10 {
                    for b in &snapshots {
                        n += w::legal_actions(b).iter().map(|l| l.slot(0).len() as u64).sum::<u64>();
                    }
                }
                n
            })
        }));
    }
    out.push(workload("request/championsvgc", || {
        let format = w::format("championsvgc");
        let snapshots = w::snapshots(&format, &w::teams(&format), 0, SNAPSHOTS);
        let renderer = pikapro_protocol::Renderer::new(&format, pikapro_protocol::Perspective::Omniscient);
        let mut buf = String::with_capacity(4096);
        measured(|| snapshots.iter().map(|b| w::requests(b, &renderer, &mut buf) as u64).sum())
    }));
    for (name, perspective) in w::PERSPECTIVES {
        out.push(workload(format!("render/{name}"), move || {
            let format = w::format("championsvgc");
            let teams = w::teams(&format);
            let logs: Vec<_> = (0..20).map(|seed| w::battle_log(&format, &teams, seed)).collect();
            measured(|| logs.iter().map(|log| w::render(&format, perspective, log) as u64).sum())
        }));
    }
    out.push(workload("view/championsvgc", || {
        let format = w::format("championsvgc");
        let teams = w::teams(&format);
        let logs: Vec<_> = (0..50).map(|seed| w::battle_log(&format, &teams, seed)).collect();
        measured(|| logs.iter().map(|log| w::apply_view(&format, log) as u64).sum())
    }));
    // The `basic` encoder over the views of consecutive decision points
    // of a few battles, both sides.
    out.push(workload("encode-basic/championsvgc", || {
        let format = w::format("championsvgc");
        let points = w::view_points(&format, &w::teams(&format), 0, 200);
        let overlays: Vec<_> = points.iter().map(|p| w::overlays(&p.battle)).collect();
        let tables = pikapro_rl::tables::Tables::for_format(&format);
        measured(|| w::encode_points(&points, &overlays, &tables))
    }));
    // One thread (no workers, so no spinning), random actions: the
    // environment's own work per step, with a built-in random opponent
    // and the default observation (`basic`, the agent's alone).
    for (name, id, steps) in
        [("vec-env/championsvgc", "championsvgc", 300), ("vec-env/gen9singles", "gen9singles", 300)]
    {
        out.push(workload(name, move || {
            let format = w::format(id);
            let config = w::env_config(&format, 16, 0, 1);
            let env = pikapro_rl::env::VecEnv::new(config, 1).expect("env");
            let actions = w::random_actions(&env);
            let mut out = env.buffers();
            env.reset(None, &mut out.as_out()).expect("reset");
            measured(|| {
                let mut ended = 0;
                for _ in 0..steps {
                    env.step(&actions, &mut out.as_out()).expect("step");
                    ended += out.end_reason.iter().filter(|&&r| r != 0).count() as u64;
                }
                ended
            })
        }));
    }
    for id in w::FORMATS {
        out.push(workload(format!("construct/{id}"), move || {
            let format = w::format(id);
            let teams = w::teams(&format);
            measured(|| (0..200).map(|seed| w::construct(&format, &teams, seed).turn() as u64).sum())
        }));
    }
    out.push(workload("import/championsvgc", || {
        let format = w::format("championsvgc");
        let text = w::team_text(&format);
        measured(|| (0..200).map(|_| w::import_team(&format, &text).sets.len() as u64).sum())
    }));
    out
}

const USAGE: &str = "\
usage: pikapro-insns [check|update] [--baseline FILE] [--tolerance PCT] [--save FILE] [WORKLOAD...]
       pikapro-insns list
       pikapro-insns run WORKLOAD";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    let (command, rest) = match args.first().map(String::as_str) {
        Some(c @ ("check" | "update" | "list" | "run")) => (c, &args[1..]),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        _ => ("check", args),
    };
    let all = workloads();
    match command {
        "list" => {
            for w in &all {
                println!("{}", w.name);
            }
            Ok(ExitCode::SUCCESS)
        }
        "run" => {
            let name = rest.first().ok_or("run needs a workload")?.clone();
            if !all.iter().any(|w| w.name == name) {
                return Err(format!("unknown workload {name:?}"));
            }
            // On a thread of its own. The main thread's stack starts below
            // the environment and the arguments, so where its frames fall
            // (and with that how many instructions an unaligned copy takes)
            // moves with them; a new thread's stack is mapped on its own,
            // the same in every run.
            let thread = std::thread::Builder::new().stack_size(WORKLOAD_STACK).spawn(move || {
                let all = workloads();
                let w = all.iter().find(|w| w.name == name).expect("checked above");
                (w.name.clone(), (w.run)())
            });
            let (name, n) = thread.map_err(|e| e.to_string())?.join().map_err(|_| "the workload panicked")?;
            println!("{name} {n}");
            Ok(ExitCode::SUCCESS)
        }
        _ => check(&all, rest, command == "update"),
    }
}

fn check(all: &[Workload], args: &[String], update: bool) -> Result<ExitCode, String> {
    let mut baseline_path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/insns-baseline.txt"));
    let mut tolerance = 2.0f64;
    let mut save = None;
    let mut names = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--baseline" => baseline_path = it.next().ok_or("--baseline needs a file")?.into(),
            "--save" => save = Some(PathBuf::from(it.next().ok_or("--save needs a file")?)),
            "--tolerance" => {
                let v = it.next().ok_or("--tolerance needs a percentage")?;
                tolerance = v.parse().map_err(|_| format!("bad tolerance {v:?}"))?;
            }
            name => {
                if !all.iter().any(|w| w.name == name) {
                    return Err(format!("unknown workload {name:?}"));
                }
                names.push(name.to_owned());
            }
        }
    }
    let selected: Vec<&Workload> = all.iter().filter(|w| names.is_empty() || names.contains(&w.name)).collect();
    let mut baseline = read_baseline(&baseline_path)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;

    println!("{:<36} {:>15} {:>15} {:>8}", "workload", "baseline", "now", "change");
    let mut failed = Vec::new();
    let mut counts = BTreeMap::new();
    for w in selected {
        let now = count(&exe, &w.name)?;
        let (base, change) = match baseline.get(&w.name) {
            Some(&base) => {
                let pct = (now as f64 - base as f64) * 100.0 / base as f64;
                (base.to_string(), format!("{pct:+.2}%"))
            }
            None => ("-".to_owned(), "new".to_owned()),
        };
        let verdict = match baseline.get(&w.name) {
            Some(&base) if (now as f64) > base as f64 * (1.0 + tolerance / 100.0) => {
                failed.push(w.name.clone());
                "  REGRESSED"
            }
            Some(&base) if (now as f64) < base as f64 * (1.0 - tolerance / 100.0) => "  improved: update the baseline",
            None if !update => {
                failed.push(w.name.clone());
                "  no baseline"
            }
            _ => "",
        };
        println!("{:<36} {:>15} {:>15} {:>8}{verdict}", w.name, base, now, change);
        counts.insert(w.name.clone(), now);
    }
    if let Some(path) = &save {
        write_baseline(path, &counts)?;
    }
    if update {
        baseline.extend(counts);
        write_baseline(&baseline_path, &baseline)?;
        println!("wrote {}", baseline_path.display());
        return Ok(ExitCode::SUCCESS);
    }
    if failed.is_empty() {
        println!("ok: every workload within {tolerance}% of {}", baseline_path.display());
        Ok(ExitCode::SUCCESS)
    } else {
        println!(
            "FAILED: {} above the baseline by more than {tolerance}% (or missing): {}",
            failed.len(),
            failed.join(", ")
        );
        println!("If the change is intended, run `cargo bench-insns update` and commit the baseline.");
        Ok(ExitCode::FAILURE)
    }
}

/// No `rep movsb` / `rep stosb` (valgrind counts a whole string copy as a
/// few instructions, and the size where glibc switches to it depends on the
/// CPU): copies cost instructions in proportion to their size.
const GLIBC_TUNABLES: &str = "glibc.cpu.x86_rep_movsb_threshold=0x100000:glibc.cpu.x86_rep_stosb_threshold=0x100000:\
    glibc.cpu.x86_non_temporal_threshold=0x400000:glibc.cpu.x86_data_cache_size=0x8000:\
    glibc.cpu.x86_shared_cache_size=0x1000000";

/// Runs `name` under callgrind and returns the instructions executed inside
/// [`measured`].
///
/// The counts must not depend on the machine or the caller's environment:
/// glibc picks its `memcpy` strategy from the CPU (fixed with
/// [`GLIBC_TUNABLES`]), and the size of the environment and of the
/// program's path move the main thread's stack, which changes how many
/// instructions an unaligned copy takes. So the workload runs on a thread
/// of its own (`run`), with only `GLIBC_TUNABLES` set, as
/// `./pikapro-insns` from its own directory.
fn count(exe: &std::path::Path, name: &str) -> Result<u64, String> {
    let valgrind = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("valgrind"))
        .find(|p| p.is_file())
        .ok_or("valgrind is not installed (not found in PATH)")?;
    let dir = exe.parent().ok_or("no directory for the executable")?;
    let file = exe.file_name().ok_or("no file name for the executable")?;
    let output = Command::new(valgrind)
        .env_clear()
        .env("GLIBC_TUNABLES", GLIBC_TUNABLES)
        .current_dir(dir)
        .args(["--tool=callgrind", "--toggle-collect=*measured*", "--callgrind-out-file=/dev/null"])
        .arg(std::path::Path::new(".").join(file))
        .args(["run", name])
        .output()
        .map_err(|e| format!("running valgrind: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!("{name}: valgrind failed:\n{stderr}"));
    }
    // `==PID== Collected : N`
    stderr
        .lines()
        .find_map(|l| l.split_once("Collected :").map(|(_, n)| n.trim().replace(',', "")))
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("{name}: no instruction count in valgrind's output:\n{stderr}"))
}

fn read_baseline(path: &std::path::Path) -> Result<BTreeMap<String, u64>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut out = BTreeMap::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (name, n) = line.split_once(char::is_whitespace).ok_or_else(|| format!("bad baseline line {line:?}"))?;
        out.insert(name.to_owned(), n.trim().parse().map_err(|_| format!("bad count in {line:?}"))?);
    }
    Ok(out)
}

fn write_baseline(path: &std::path::Path, counts: &BTreeMap<String, u64>) -> Result<(), String> {
    let mut text = String::from(
        "# Instructions (callgrind Ir) executed by each `cargo bench-insns` workload\n\
         # (benches/src/bin/pikapro-insns.rs), release profile without default\n\
         # features, Linux x86-64. Regenerate with `cargo bench-insns update` after\n\
         # an intended change and commit it with that change (see the top of pikapro-insns.rs).\n",
    );
    for (name, n) in counts {
        text.push_str(&format!("{name} {n}\n"));
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}
