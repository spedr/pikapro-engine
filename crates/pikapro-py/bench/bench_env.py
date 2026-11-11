"""Throughput of the RL environment from Python.

    python bench/bench_env.py --format championsvgc --threads 1 16 --seconds 10 --rounds 5

Every run plays random-vs-random battles (the env's own random actions,
``RANDOM_ACTION``, so no Python policy cost) on the sample teams and
reports steps/s (decision rounds of one battle) and battles/s:

* ``VecEnv``: :class:`pikapro.rl.VecEnv` with the ``basic`` observation,
  both players observed (``opponent="external"``), factored masks.
* ``BattlePool step``: the pool API, ``sample_random`` + ``step`` (two
  dispatches per decision round, no views, no encoding).
* ``BattlePool play_random``: whole battles on the workers, no Python per
  step (the ceiling the ratios are taken against).
* ``poke-env (in-process)``: two poke-env ``RandomPlayer``\\ s through the
  compat driver (``pikapro.compat.poke_env.play``), when poke-env is installed.

For each thread count and ``n``, the modes run in ``--rounds`` interleaved
rounds of ``--seconds / --rounds`` each, so that a change in the machine's
load affects them alike: compare the ratios, not the absolute numbers, on a
shared machine.

The VecEnv columns also show where the Rust step's wall time went
(``VecEnv._take_profile``): ``copy``, the calling thread copying finished
shards into the numpy buffers; ``wait``, the calling thread waiting for the
workers' last shards; ``wrapper``, the Python call's time over the Rust
step's (argument checks, buffer borrows, the GIL).
"""

from __future__ import annotations

import argparse
import time

import numpy as np

import pikapro
import pikapro.rl as rl


class EnvRun:
    """A VecEnv stepped with random actions, timed in slices."""

    def __init__(self, fmt, team, n, threads, seed):
        self.n = n
        self.env = rl.VecEnv(fmt, n=n, threads=threads, seed=seed, teams=[team, team], opponent="external")
        self.actions = np.full((n, 2, self.env.actions_per_player), rl.RANDOM_ACTION, dtype=np.int64)
        self.env.reset(seed=seed)
        for _ in range(20):  # warm up
            self.env.step(self.actions)
        self.env._set_profile(True)
        self.env._take_profile()
        self.time = 0.0
        self.steps = self.battles = self.py_ns = 0

    def run(self, seconds):
        t0 = time.perf_counter()
        while time.perf_counter() - t0 < seconds:
            s = time.perf_counter_ns()
            out = self.env.step(self.actions)
            self.py_ns += time.perf_counter_ns() - s
            self.steps += self.n
            self.battles += int(out.terminated.sum() + out.truncated.sum())
        self.time += time.perf_counter() - t0

    def result(self):
        prof = self.env._take_profile()
        wall = max(prof["wall"], 1)
        return {
            "steps": self.steps / self.time,
            "battles": self.battles / self.time,
            "copy": prof["copy"] / wall,
            "wait": prof["wait"] / wall,
            "wrapper": (self.py_ns - wall) / wall,
        }


class PoolStepRun:
    def __init__(self, fmt, team, n, threads, seed):
        self.n = n
        # Same-step: every step plays (none is spent starting an episode).
        self.pool = pikapro.BattlePool(fmt, [team, team], n=n, seed=seed, threads=threads, autoreset="same_step")
        self.time = 0.0
        self.steps = self.battles = 0

    def run(self, seconds):
        t0 = time.perf_counter()
        while time.perf_counter() - t0 < seconds:
            _, done, _, _ = self.pool.step(self.pool.sample_random())
            self.steps += self.n
            self.battles += int(done.sum())
        self.time += time.perf_counter() - t0

    def result(self):
        return {"steps": self.steps / self.time, "battles": self.battles / self.time}


class PoolPlayRun:
    def __init__(self, fmt, team, n, threads, seed):
        self.n = n
        self.pool = pikapro.BattlePool(fmt, [team, team], n=n, seed=seed, threads=threads)
        r = self.pool.play_random(max(n, 200))
        self.per = r["elapsed"] / r["battles"]
        self.time = 0.0
        self.decisions = self.battles = 0

    def run(self, seconds):
        r = self.pool.play_random(max(int(seconds / self.per) + 1, self.n))
        self.time += r["elapsed"]
        self.decisions += r["decisions"]
        self.battles += r["battles"]

    def result(self):
        return {"steps": self.decisions / self.time / 2, "battles": self.battles / self.time}


def run_poke_env(fmt, battles, seed):
    try:
        import pikapro.compat.poke_env as pe
        from poke_env.player import RandomPlayer
    except ImportError:
        return None
    a, b = pe.make_player(RandomPlayer, fmt), pe.make_player(RandomPlayer, fmt)
    r = pe.play(a, b, fmt, battles, seed=seed, register=False)
    return r.battles_per_second


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--format", default="championsvgc")
    p.add_argument("--threads", type=int, nargs="+", default=[1, 16])
    p.add_argument("--n", type=int, nargs="+", default=None, help="battles per env (default: 256 per thread, at least 1,024)")
    p.add_argument("--seconds", type=float, default=10.0, help="per mode, over all rounds")
    p.add_argument("--rounds", type=int, default=5)
    p.add_argument("--modes", nargs="+", default=["env", "step", "play"], help="env, step, play")
    p.add_argument("--seed", type=int, default=4_800_000)
    p.add_argument("--poke-env-battles", type=int, default=100)
    args = p.parse_args()
    fmt = args.format
    team = pikapro.sample_team(fmt)
    print(f"{fmt}, sample teams, random actions, {args.seconds:.0f} s per mode in {args.rounds} interleaved rounds")
    print(
        f"{'mode':<24} {'threads':>7} {'n':>6} {'steps/s':>10} {'battles/s':>10} {'/ play':>7}"
        f" {'copy':>6} {'wait':>6} {'wrapper':>8}"
    )
    for threads in args.threads:
        for n in args.n or [max(1024, 256 * threads)]:
            runs = {}
            for mode in args.modes:
                if mode == "env":
                    runs["VecEnv"] = EnvRun(fmt, team, n, threads, args.seed)
                elif mode == "step":
                    runs["BattlePool step"] = PoolStepRun(fmt, team, n, threads, args.seed)
                elif mode == "play":
                    runs["BattlePool play_random"] = PoolPlayRun(fmt, team, n, threads, args.seed)
            for _ in range(args.rounds):
                for r in runs.values():
                    r.run(args.seconds / args.rounds)
            results = {name: r.result() for name, r in runs.items()}
            play = results.get("BattlePool play_random", {}).get("battles")
            for name, r in results.items():
                ratio = f"{r['battles'] / play:>7.0%}" if play else f"{'':>7}"
                extra = (
                    f" {r['copy']:>6.1%} {r['wait']:>6.1%} {r['wrapper']:>8.1%}" if "copy" in r else f" {'':>6} {'':>6} {'':>8}"
                )
                print(f"{name:<24} {threads:>7} {n:>6} {r['steps']:>10.0f} {r['battles']:>10.0f} {ratio}{extra}")
            del runs
    if args.poke_env_battles:
        bps = run_poke_env(fmt, args.poke_env_battles, args.seed)
        if bps is not None:
            print(f"{'poke-env (in-process)':<24} {1:>7} {1:>6} {'':>10} {bps:>10.1f}")


if __name__ == "__main__":
    main()
