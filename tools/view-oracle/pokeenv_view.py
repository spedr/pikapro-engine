#!/usr/bin/env python3
"""poke-env's side of the view differential: decision records in, poke-env's
tracked state out (SCHEMA.md).

Each (format, seed, side) stream of records is fed, in order, to a
``pikapro.compat.poke_env.Feeder`` (poke-env's own ``Battle``/``DoubleBattle``,
driven as its ``Player`` would drive it); after each record the battle's
state is written, normalized.

    python pokeenv_view.py decisions.jsonl -o poke-env.jsonl
    python gen_decisions.py --format gen9singles --battles 5 | python pokeenv_view.py - > poke-env.jsonl

A stream poke-env chokes on gets one ``error`` record (the line and
poke-env's exception) and no more states. A summary goes to stderr.
"""

from __future__ import annotations

import argparse
import collections
import json
import sys

from pikapro.compat.poke_env import Feeder, FeederError, snapshot


def states(records):
    """Yields an output record per input record (SCHEMA.md, "Output")."""
    feeders = {}
    failed = set()
    for record in records:
        key = (record["format"], record["seed"], record["side"])
        head = {k: record[k] for k in ("format", "seed", "side", "index", "turn")}
        if key in failed:
            continue
        if record["index"] == 0 or key not in feeders:
            feeders[key] = Feeder(record["format"], record["side"])
        feeder = feeders[key]
        try:
            feeder.receive(record["lines"])
            if record["request"] is not None:
                feeder.receive_request(record["request"])
            state = snapshot(feeder.battle)
        except Exception as e:  # noqa: BLE001 - every poke-env failure is reported
            failed.add(key)
            feeders.pop(key, None)
            cause = e.cause if isinstance(e, FeederError) else e
            yield {
                **head,
                "error": f"{type(cause).__name__}: {cause}",
                "line": getattr(e, "line", None),
            }
            continue
        out = {**head, "state": state}
        if feeder.warnings:
            out["warnings"] = [m for _, m in feeder.warnings]
            feeder.warnings.clear()
        yield out
        if record["request"] is None:
            feeders.pop(key, None)


def _read(path):
    f = sys.stdin if path == "-" else open(path, encoding="utf-8")
    try:
        for line in f:
            line = line.strip()
            if line:
                yield json.loads(line)
    finally:
        if f is not sys.stdin:
            f.close()


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("input", help="decision records (JSONL), '-' for stdin")
    ap.add_argument("-o", "--out", default="-", help="output JSONL (default: stdout)")
    args = ap.parse_args(argv)
    out = sys.stdout if args.out == "-" else open(args.out, "w", encoding="utf-8")
    n = 0
    streams = set()
    errors = collections.Counter()
    warnings = collections.Counter()
    try:
        for rec in states(_read(args.input)):
            n += 1
            streams.add((rec["format"], rec["seed"], rec["side"]))
            if "error" in rec:
                errors[rec["error"][:120]] += 1
            for w in rec.get("warnings", ()):
                warnings[w[:120]] += 1
            out.write(json.dumps(rec, ensure_ascii=False, separators=(",", ":")) + "\n")
    finally:
        if out is not sys.stdout:
            out.close()
    print(f"{len(streams)} streams, {n} records, {sum(errors.values())} failed streams", file=sys.stderr)
    for msg, k in errors.most_common(20):
        print(f"  error x{k}: {msg}", file=sys.stderr)
    for msg, k in warnings.most_common(20):
        print(f"  poke-env warning x{k}: {msg}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
