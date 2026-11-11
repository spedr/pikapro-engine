//! `pikapro-env-split`: where the RL environment's step goes, and its
//! throughput against the raw engine.
//!
//! For each thread count it runs `pikapro_rl::env::VecEnv` with
//! random self-play (player 0's random actions, a built-in random
//! opponent, or with `--opponent external` random actions for both players
//! from the caller, both observed; the `basic` observation; factored
//! masks), once with the per-phase clock (the time split) and once
//! without (throughput), and the raw pool for comparison: its
//! `sample_random` + `step` loop (two dispatches per step, events off)
//! and `play_random` (whole battles on the workers, no dispatch per step).
//!
//! ```text
//! cargo run --release -p pikapro-bench --bin pikapro-env-split -- [--format championsvgc]
//!     [--threads 1,8,16] [--per-thread 256] [--n 16384] [--shards 8] [--spin 200]
//!     [--seconds 3] [--rounds 5] [--opponent random|external] [--ots]
//! ```
//!
//! `--n` fixes the battle count for every thread count (instead of
//! `--per-thread` times the threads); `--spin` is how long idle workers
//! poll for the next call, in microseconds (both pools). The throughputs
//! are measured in `--rounds` interleaved rounds of `--seconds / --rounds`
//! each, so that a change in the machine's load affects all three alike.

use std::time::{Duration, Instant};

use pikapro_bench as w;
use pikapro_rl::env::VecEnv;
use pikapro_rl::pool::{ActionMode, Pool};

struct Args {
    format: String,
    threads: Vec<usize>,
    per_thread: usize,
    n: Option<usize>,
    shards: usize,
    spin: u64,
    seconds: f64,
    rounds: usize,
    opponent: String,
    ots: bool,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        format: "championsvgc".into(),
        threads: vec![1, 8, 16],
        per_thread: 256,
        n: None,
        shards: 8,
        spin: 200,
        seconds: 3.0,
        rounds: 5,
        opponent: "random".into(),
        ots: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--format" => a.format = value()?,
            "--threads" => {
                a.threads = value()?
                    .split(',')
                    .map(|t| t.parse().map_err(|_| format!("bad thread count {t:?}")))
                    .collect::<Result<_, _>>()?
            }
            "--per-thread" => a.per_thread = value()?.parse().map_err(|_| "bad --per-thread")?,
            "--n" => a.n = Some(value()?.parse().map_err(|_| "bad --n")?),
            "--shards" => a.shards = value()?.parse().map_err(|_| "bad --shards")?,
            "--opponent" => {
                a.opponent = value()?;
                if !["random", "external"].contains(&a.opponent.as_str()) {
                    return Err(format!("--opponent is random or external, not {:?}", a.opponent));
                }
            }
            "--ots" => a.ots = true,
            "--rounds" => a.rounds = value()?.parse::<usize>().map_err(|_| "bad --rounds")?.max(1),
            "--spin" => a.spin = value()?.parse().map_err(|_| "bad --spin")?,
            "--seconds" => a.seconds = value()?.parse().map_err(|_| "bad --seconds")?,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(a)
}

/// Runs `f` (one call) until `seconds` have passed, after a short warmup
/// (`f(false)`; `f(true)` once measuring). Returns the calls measured and
/// the time they took.
fn timed(seconds: f64, mut f: impl FnMut(bool)) -> (u64, Duration) {
    let warm = Instant::now();
    while warm.elapsed() < Duration::from_secs_f64(seconds / 10.0) {
        f(false);
    }
    let start = Instant::now();
    let mut calls = 0;
    while start.elapsed() < Duration::from_secs_f64(seconds) {
        f(true);
        calls += 1;
    }
    (calls, start.elapsed())
}

/// The raw pool's configuration: events off, idle workers polling for
/// `spin` microseconds.
fn raw_config(format: &pikapro::Format, n: usize, spin: u64) -> pikapro_rl::pool::PoolConfig {
    let mut config = w::pool_config(format, n, 4_300_000, false);
    config.spin = Duration::from_micros(spin);
    config
}

fn main() {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    let format = w::format(&a.format);
    println!(
        "# {} (sample teams, random self-play), {:.0} s per measurement, {} shards per thread",
        a.format, a.seconds, a.shards
    );
    for &threads in &a.threads {
        let n = a.n.unwrap_or(a.per_thread * threads);
        println!("\n## {threads} thread(s), n = {n}");

        // VecEnv, per-phase clock on: the split.
        let mut config = w::env_config(&format, n, 4_300_000, a.shards);
        config.spin = Duration::from_micros(a.spin);
        if a.opponent == "external" {
            config.opponent = pikapro_rl::vec::Opponent::External.into();
        }
        if a.ots {
            config.ots = 1.0;
        }
        let env = VecEnv::new(config, threads).expect("env");
        let actions = w::random_actions(&env);
        let mut out = env.buffers();
        env.reset(None, &mut out.as_out()).unwrap();
        env.set_profile(true);
        timed(a.seconds, |measuring| {
            env.step(&actions, &mut out.as_out()).unwrap();
            if !measuring {
                env.take_profile();
            }
        });
        let split = env.take_profile();
        let wall = split.wall as f64;
        let work = split.work() as f64;
        let sync = (wall - work / threads as f64).max(0.0);
        println!("phase         share of wall   ns per battle step");
        let per_step = |ns: f64| ns / split.steps as f64;
        for (name, ns) in [
            ("engine", split.engine),
            ("view apply", split.view),
            ("overlay", split.overlay),
            ("encoding", split.encode),
            ("masks", split.masks),
            ("other", split.other),
        ] {
            let ns = ns as f64 / threads as f64;
            println!("{name:<13} {:>6.1}%        {:>8.0}", 100.0 * ns / wall, per_step(ns * threads as f64));
        }
        println!("{:<13} {:>6.1}%        {:>8.0}", "sync + copy", 100.0 * sync / wall, per_step(sync * threads as f64));
        println!(
            "  of which the caller copying {:.1}%, waiting after its last shard {:.1}% (of wall)",
            100.0 * split.copy as f64 / wall,
            100.0 * split.wait as f64 / wall
        );

        // Throughput, no clock: the vector environment and the two raw
        // loops, interleaved over `--rounds` rounds so a machine whose load
        // changes affects them alike.
        env.set_profile(false);
        let step_pool = Pool::new(raw_config(&format, n, a.spin), threads).expect("pool");
        let play_pool = Pool::new(raw_config(&format, n, a.spin), threads).expect("pool");
        let slice = a.seconds / a.rounds as f64;
        let (mut env_time, mut env_calls, mut env_battles) = (0.0, 0u64, 0u64);
        let (mut step_time, mut step_calls, mut step_battles) = (0.0, 0u64, 0u64);
        let (mut play_time, mut play_battles, mut play_decisions) = (0.0, 0u64, 0u64);
        for _ in 0..a.rounds {
            let (calls, took) = timed(slice, |measuring| {
                env.step(&actions, &mut out.as_out()).unwrap();
                if measuring {
                    env_battles += out.end_reason.iter().filter(|&&r| r != 0).count() as u64;
                }
            });
            env_calls += calls;
            env_time += took.as_secs_f64();
            // Raw pool: sample + step, two dispatches, events off.
            let (calls, took) = timed(slice, |measuring| {
                let actions = step_pool.sample_random(ActionMode::Slots).unwrap();
                let done = step_pool.step(actions, ActionMode::Slots).unwrap();
                if measuring {
                    step_battles += done.done.iter().filter(|&&d| d).count() as u64;
                }
            });
            step_calls += calls;
            step_time += took.as_secs_f64();
            // Raw pool: whole battles on the workers, a battle per slot per
            // call.
            timed(slice, |measuring| {
                let (totals, secs) = play_pool.play_random(n as u64, || Ok(())).unwrap();
                if measuring {
                    play_time += secs;
                    play_battles += totals.battles;
                    play_decisions += totals.decisions;
                }
            });
        }
        let fused_steps = (env_calls * n as u64) as f64 / env_time;
        let fused_battles = env_battles as f64 / env_time;
        println!(
            "vec env:      {fused_steps:>10.0} steps/s {fused_battles:>8.0} battles/s ({:.1} steps per battle)",
            fused_steps / fused_battles.max(1e-9)
        );
        let raw_steps = (step_calls * n as u64) as f64 / step_time;
        println!("raw step:     {raw_steps:>10.0} steps/s {:>8.0} battles/s", step_battles as f64 / step_time);
        let raw_play = play_battles as f64 / play_time;
        println!("raw play:     {:>10.0} decisions/s {:>8.0} battles/s", play_decisions as f64 / play_time, raw_play);
        println!(
            "vec env / raw play: {:.0}% (battles/s); vec env / raw step: {:.0}% (steps/s)",
            100.0 * fused_battles / raw_play,
            100.0 * fused_steps / raw_steps
        );
    }
}
