"""EXAMPLE: your own observation in Python.

    python examples/custom_encoder.py            # prints what it gives
    python examples/custom_encoder.py --smoke    # the same, fewer steps (the tests)

A function over :class:`pikapro.rl.BattleView` (poke-env's
``embed_battle``), called on every battle with :func:`pikapro.rl.embed_all`.
It costs Python per battle per step; an encoder written in Rust doesn't
(``docs/python/encoders.md``).

Which features matter is the modeler's call; copy this and change it.
"""

from __future__ import annotations

import argparse
import time

import numpy as np

import pikapro.rl as rl


def embed(view: rl.BattleView) -> np.ndarray:
    """poke-env's example embedding: per own active slot, the base power
    of its four moves and their best type effectiveness against the foes on
    the field; then the fraction of each side's team that fainted."""
    slots = view.space.active
    out = np.zeros(slots * 8 + 2, np.float32)
    actives = view.active_pokemon if view.doubles else [view.active_pokemon]
    foes = view.opponent_active_pokemon if view.doubles else [view.opponent_active_pokemon]
    foes = [f for f in foes if f is not None and f.types]
    for s, active in enumerate(actives):
        if active is None:
            continue
        for k, m in enumerate(list(active.moves.values())[:4]):
            out[s * 8 + k] = m.base_power / 100
            out[s * 8 + 4 + k] = max((f.damage_multiplier(m) for f in foes), default=1.0)
    out[-2] = sum(m.fainted for m in view.team.values()) / 6
    out[-1] = sum(m.fainted for m in view.opponent_team.values()) / 6
    return out


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--format", default="championsvgc")
    p.add_argument("--n", type=int, default=64)
    p.add_argument("--steps", type=int, default=50)
    p.add_argument("--smoke", action="store_true")
    args = p.parse_args()
    if args.smoke:
        args.n, args.steps = 8, 10
    rng = np.random.default_rng(0)

    env = rl.VecEnv(args.format, n=args.n, opponent="random", seed=4_670_000)
    env.reset(seed=4_670_000)
    t = time.perf_counter()
    for _ in range(args.steps):
        env.step(env.sample_actions(rng, [0])[:, 0])
        x = rl.embed_all(env, embed)
    dt = time.perf_counter() - t
    print(f"BattleView features: {x.shape}, {args.n * args.steps / dt:.0f} steps/s; battle 0: {np.round(x[0], 2)}")


if __name__ == "__main__":
    main()
