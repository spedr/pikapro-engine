#!/usr/bin/env python3
"""Same seeds, same battles: hashes of what two builds of the extension
compute, to check that a PGO wheel plays exactly as a plain one.

    python tools/wheel_hashes.py PYTHON_A PYTHON_B   # compare two installs
    python tools/wheel_hashes.py                     # print this Python's hashes

PYTHON_A and PYTHON_B are interpreters with a wheel each installed (and
numpy). Each one, run from outside the source tree, plays the same seeded
battles and environment steps and hashes the requests, the legal actions,
the logs (raw, a player's, a spectator's), the results, and every buffer of
`VecEnv` steps (with a built-in opponent, and with both players observed).
Exits 1 if any hash differs.
"""

from __future__ import annotations

import hashlib
import os
import subprocess
import sys
import tempfile

FORMATS = [
    "gen9vgc",
    "championsvgc",
    "gen9singles",
    "gen9doubles",
    "championsbss",
    "gen9ou",
    "gen9randombattle",
    "gen9randomdoublesbattle",
]


def battles(fmt: str, n: int = 60) -> str:
    import pikapro

    h = hashlib.sha256()
    team = pikapro.sample_team(fmt)
    for seed in range(n):
        b = pikapro.Battle(fmt, [team, team], seed=seed)
        policy = pikapro.RandomPolicy(seed ^ 0x5EED)
        while not b.over:
            for side in (0, 1):
                if b.request_kind(side) != "wait":
                    h.update(b.request_json(side).encode())
                    h.update(repr(b.legal_actions(side)).encode())
            for side in (0, 1):
                choice = policy.choose(b, side)
                if choice is not None:
                    b.submit(side, choice)
        for line in b.raw_log() + b.log("p1") + b.log("spectator"):
            h.update(line.encode())
        h.update(repr((b.winner, b.turn)).encode())
    return h.hexdigest()[:16]


def env(fmt: str, opponent: str, steps: int = 300) -> str:
    import numpy as np
    from pikapro import rl

    h = hashlib.sha256()
    e = rl.VecEnv(fmt, n=64, threads=4, seed=11, opponent=opponent)
    e.reset(seed=11)
    actions = np.full((64, 2, e.actions_per_player), rl.RANDOM_ACTION, dtype=np.int64)
    for _ in range(steps):
        e.step(actions)
        for name, buf in sorted(e.buffers.items()):
            h.update(name.encode())
            h.update(np.ascontiguousarray(buf).tobytes())
    return h.hexdigest()[:16]


def hashes() -> None:
    for fmt in FORMATS:
        print(f"battles {fmt} {battles(fmt)}", flush=True)
    for fmt in ["championsvgc", "gen9singles"]:
        for opp in ["random", "external"]:
            print(f"env {opp} {fmt} {env(fmt, opp)}", flush=True)


def main() -> int:
    if len(sys.argv) == 1:
        hashes()
        return 0
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    outs = []
    for python in sys.argv[1:]:
        # -I and a temporary directory: the installed wheel, not python/pikapro.
        with tempfile.TemporaryDirectory() as tmp:
            out = subprocess.run(
                [python, "-I", os.path.abspath(__file__)], cwd=tmp, capture_output=True, text=True, check=False
            )
        if out.returncode != 0:
            print(out.stdout + out.stderr, file=sys.stderr)
            print(f"{python} failed", file=sys.stderr)
            return 1
        outs.append(out.stdout.splitlines())
    same = True
    for a, b in zip(outs[0], outs[1]):
        mark = "same" if a == b else "DIFFERENT"
        same &= a == b
        print(f"{mark:9} {a}" if a == b else f"{mark:9} {a} | {b.rsplit(' ', 1)[1]}")
    if len(outs[0]) != len(outs[1]):
        same = False
    print("identical" if same else "the builds differ")
    return 0 if same else 1


if __name__ == "__main__":
    sys.exit(main())
