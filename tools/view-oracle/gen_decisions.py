#!/usr/bin/env python3
"""Writes decision records (SCHEMA.md, "Input") for fuzzed pikapro battles.

Each battle plays uniformly random legal choices. After every update the
battle sends, each side gets one record: the new lines of its channel and
the request it is sent (Showdown's order: the log, then the request).

    python gen_decisions.py --format championsvgc --battles 100 --seed 4100000 -o decisions.jsonl

Battle ``k`` has seed ``s = seed + k``, teams
``pikapro.RandomTeams(format, teams).pair(s)`` and choices from
``pikapro.RandomPolicy(s ^ 0x5EED)`` (asked for side 1, then side 2, after
each update), so a later Rust ``view_dump`` can play the same battles.
"""

from __future__ import annotations

import argparse
import json
import sys

import pikapro

PERSPECTIVES = ("p1", "p2")
POLICY_SALT = 0x5EED


def battle_records(format: str, seed: int, teams_mode: str = "sets"):
    """The decision records of one battle, in the order a client pair
    receives them: after each update, side 1's then side 2's."""
    teams = pikapro.RandomTeams(format, teams_mode).pair(seed)
    b = pikapro.Battle(format, teams, seed=seed)
    policy = pikapro.RandomPolicy(seed ^ POLICY_SALT)
    start = 0
    index = [0, 0]
    first = True
    while True:
        n = b.event_count
        for side in (0, 1):
            lines = b.log(PERSPECTIVES[side], start)
            if first:
                lines = b.header() + lines
            record = {
                "format": format,
                "seed": seed,
                "side": PERSPECTIVES[side],
                "index": index[side],
                "turn": b.turn,
                "lines": lines,
                "request": None if b.over else b.request_json(side),
            }
            if first:
                record["teams"] = list(teams)
            index[side] += 1
            yield record
        first = False
        start = n
        if b.over:
            return
        resolved = False
        for side in (0, 1):
            choice = policy.choose(b, side)
            if choice is not None:
                resolved = b.submit(side, choice) or resolved
        if not resolved and not b.over:
            raise RuntimeError(f"{format} seed {seed}: no side could resolve the turn")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--format", required=True, choices=pikapro.formats())
    ap.add_argument("--battles", type=int, default=10)
    ap.add_argument("--seed", type=int, default=4_100_000)
    ap.add_argument("--teams", default="sets", choices=["sets", "uniform"], help="pikapro.RandomTeams mode")
    ap.add_argument("-o", "--out", default="-", help="output JSONL (default: stdout)")
    args = ap.parse_args(argv)
    out = sys.stdout if args.out == "-" else open(args.out, "w", encoding="utf-8")
    records = 0
    try:
        for k in range(args.battles):
            for record in battle_records(args.format, args.seed + k, args.teams):
                out.write(json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n")
                records += 1
    finally:
        if out is not sys.stdout:
            out.close()
    print(f"{args.battles} battles, {records} records", file=sys.stderr)


if __name__ == "__main__":
    main()
