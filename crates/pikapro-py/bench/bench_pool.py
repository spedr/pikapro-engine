"""Throughput from Python versus `pikapro bench`.

    python bench/bench_pool.py --format gen9vgc --battles 20000 --threads 1 8

Every mode plays random-vs-random battles with events off and reports
battles per second:

* ``rust``: ``pikapro bench`` (the CLI, no Python at all), if found.
* ``pool.play_random``: the pool's built-in random policy, the whole run in
  one call (the pool's own overhead).
* ``pool.step + sample_random``: a Python loop, one ``step`` per decision,
  actions from the Rust policy (the per-call overhead of stepping from
  Python: argument conversion, dispatch to the workers, result arrays).
* ``pool.step + numpy policy``: the same loop with a numpy random policy
  over the choice masks, slot by slot (``pikapro.sample_slots``), i.e. masks
  out and actions in through numpy every step.
* ``Battle loop``: one ``pikapro.Battle`` at a time, a Python call per
  choice (no pool).
"""

import argparse
import os
import re
import subprocess
import time
from pathlib import Path

import numpy as np

import pikapro

ROOT = Path(__file__).resolve().parents[3]


def rust_bench(fmt, battles, threads, seed):
    exe = os.environ.get("PIKAPRO_CLI") or str(ROOT / "target" / "release" / "pikapro")
    if not Path(exe).exists():
        return None
    out = subprocess.run(
        [exe, "bench", "--format", fmt, "--battles", str(battles), "--threads", str(threads), "--seed", str(seed)],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return float(re.search(r"battles/s\s+(\d+)", out).group(1))


def play_random(fmt, team, battles, threads, n, seed):
    pool = pikapro.BattlePool(fmt, [team, team], n=n, seed=seed, threads=threads)
    r = pool.play_random(battles)
    return r["battles"] / r["elapsed"]


def step_loop(fmt, team, battles, threads, n, seed, policy):
    pool = pikapro.BattlePool(fmt, [team, team], n=n, seed=seed, threads=threads)
    rng = np.random.default_rng(seed)
    steps = 0
    t = time.perf_counter()
    while pool.finished()["battles"] < battles:
        if policy == "rust":
            pool.step(pool.sample_random())
        else:
            pool.step(pikapro.sample_slots(pool, rng))
        steps += 1
    elapsed = time.perf_counter() - t
    return pool.finished()["battles"] / elapsed, steps / elapsed


def battle_loop(fmt, team, battles, seed):
    t = time.perf_counter()
    for i in range(battles):
        b = pikapro.Battle(fmt, [team, team], seed=seed + i, log=False)
        policy = pikapro.RandomPolicy(~(seed + i) & (2**64 - 1))
        while not b.over:
            for side in (0, 1):
                c = policy.choose(b, side)
                if c is not None:
                    b.submit(side, c)
    return battles / (time.perf_counter() - t)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--format", default="gen9vgc")
    ap.add_argument("--battles", type=int, default=20000)
    ap.add_argument("--threads", type=int, nargs="+", default=[1, os.cpu_count() or 1])
    ap.add_argument("--n", type=int, default=0, help="pool size (default: 64 per thread)")
    ap.add_argument("--seed", type=int, default=0)
    args = ap.parse_args()
    fmt, battles, seed = args.format, args.battles, args.seed
    team = pikapro.sample_team(fmt)
    print(f"format {fmt}, {battles} battles, battles/s (higher is better)")
    for threads in args.threads:
        n = args.n or 64 * threads
        rows = []
        rust = rust_bench(fmt, battles, threads, seed)
        rows.append(("rust: pikapro bench", rust, None))
        rows.append(("pool.play_random", play_random(fmt, team, battles, threads, n, seed), None))
        bps, sps = step_loop(fmt, team, battles, threads, n, seed, "rust")
        rows.append(("pool.step + sample_random", bps, sps))
        bps, sps = step_loop(fmt, team, max(battles // 4, 1), threads, n, seed, "numpy")
        rows.append(("pool.step + numpy policy", bps, sps))
        if threads == 1:
            rows.append(("Battle loop (no pool)", battle_loop(fmt, team, max(battles // 10, 1), seed), None))
        print(f"\nthreads {threads}, pool n={n}")
        for name, bps, sps in rows:
            if bps is None:
                print(f"  {name:28} (not found; build it with cargo build --release -p pikapro-cli)")
                continue
            rel = f"{bps / rust:6.1%} of rust" if rust else ""
            steps = f"  {sps:8.0f} steps/s" if sps else ""
            print(f"  {name:28} {bps:10.0f}  {rel}{steps}")


if __name__ == "__main__":
    main()
