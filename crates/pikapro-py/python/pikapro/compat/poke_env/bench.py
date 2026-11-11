"""Throughput of unmodified poke-env players on the in-process driver.

    python -m pikapro.compat.poke_env.bench --format championsvgc --battles 500
    python -m pikapro.compat.poke_env.bench --format gen9singles --players random,maxbp --processes 4

Prints one JSON line per run: battles and turns per second of wall time,
plus the driver's counters. Compare with pikapro-showdown-server's
``tests/python/throughput.py`` (the same players over a websocket): one
process here corresponds to one pair there with one battle at a time.
"""

from __future__ import annotations

import argparse
import json

PLAYERS = {
    "random": "RandomPlayer",
    "maxbp": "MaxBasePowerPlayer",
    "heuristics": "SimpleHeuristicsPlayer",
}


def main(argv=None):
    from poke_env import player as players

    from . import make_player, play, play_parallel

    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--format", default="championsvgc")
    ap.add_argument("--battles", type=int, default=200)
    ap.add_argument("--players", default="random,random", help=f"two of {sorted(PLAYERS)}")
    ap.add_argument("--processes", type=int, default=1)
    ap.add_argument("--seed", type=int, default=4_100_000)
    ap.add_argument("--warmup", type=int, default=5, help="battles played first and not timed (per process: loads poke-env's data)")
    args = ap.parse_args(argv)
    a, b = (getattr(players, PLAYERS[name]) for name in args.players.split(","))
    if args.processes == 1:
        p1, p2 = make_player(a, args.format), make_player(b, args.format)
        if args.warmup:
            play(p1, p2, args.format, args.warmup, seed=args.seed - args.warmup, register=False)
        result = play(p1, p2, args.format, args.battles, seed=args.seed, python_seed=0, register=False)
    else:
        result = play_parallel(a, b, args.format, args.battles, processes=args.processes, seed=args.seed, python_seed=0)
    summary = result.summary()
    summary["processes"] = args.processes
    summary["turns_per_second"] = round(result.turns / result.seconds, 1)
    print(json.dumps(summary))


if __name__ == "__main__":
    main()
