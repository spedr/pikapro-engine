#!/usr/bin/env python3
"""Diffs our projected view against poke-env's tracked state, per record,
and reports the first divergence of each battle (like the conformance
harness).

    python diff.py --ours VIEW.jsonl --theirs POKE-ENV.jsonl [--decisions DECISIONS.jsonl]
                   [--show N] [--context K] [--no-allowlist] [--json]

``VIEW.jsonl`` holds our state records (`view_dump`, or `cargo
view-conformance --check poke-env`), ``POKE-ENV.jsonl`` poke-env's
(``tools/view-oracle/pokeenv_view.py`` run on ``DECISIONS.jsonl``). Records
are matched by ``(format, seed, side, index)`` (SCHEMA.md). With
``--decisions``, a divergence is shown with the last ``K`` lines the side
was sent up to that record.

Values compare as JSON: objects key by key (a key only one side has is a
divergence), lists and scalars whole. A value we mark ``"?"`` (unknown)
where poke-env knows one is **missing** from our view; a value poke-env
marks ``"?"`` where we know one is **extra** (either a poke-env limit, or
our view knowing more than the channel shows: a leak). Known poke-env
quirks are allowed by ``allowlist.py``.

Exit status 1 if any battle diverges (or poke-env raised on a stream).
"""

from __future__ import annotations

import argparse
import collections
import fnmatch
import json
import os
import sys
from typing import Any, Dict, Iterator, List, Tuple

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from allowlist import ABSENT, UNKNOWN, Diff, Illusions, allowed  # noqa: E402

Key = Tuple[str, int, str, int]


def read(path: str) -> Iterator[dict]:
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                yield json.loads(line)


def key(r: dict) -> Key:
    return (r["format"], r["seed"], r["side"], r["index"])


def leaves(a: Any, b: Any, path: Tuple[str, ...] = ()) -> Iterator[Tuple[Tuple[str, ...], Any, Any]]:
    """The differing leaves of two JSON values."""
    if isinstance(a, dict) and isinstance(b, dict):
        for k in list(a.keys()) + [k for k in b.keys() if k not in a]:
            yield from leaves(a.get(k, ABSENT), b.get(k, ABSENT), path + (k,))
    elif a != b:
        yield path, a, b


def kind(ours: Any, theirs: Any) -> str:
    if ours == UNKNOWN and theirs != UNKNOWN:
        return "missing"
    if theirs == UNKNOWN and ours != UNKNOWN:
        return "extra"
    return "differs"


def pattern(path: Tuple[str, ...]) -> str:
    """A path with Pokémon keys replaced by ``*``, for summaries."""
    out = list(path)
    if len(out) > 1 and out[0] in ("team", "opponent_team"):
        out[1] = "*"
    return ".".join(out)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ours", required=True)
    ap.add_argument("--theirs", required=True)
    ap.add_argument("--decisions")
    ap.add_argument("--show", type=int, default=5, help="divergences to print in full")
    ap.add_argument("--context", type=int, default=12, help="lines of context")
    ap.add_argument("--no-allowlist", action="store_true")
    ap.add_argument("--json", action="store_true", help="print the first divergence per battle as JSON lines")
    ap.add_argument("--all", action="store_true", help="also count every divergence of every record, by field")
    ap.add_argument("--seed", type=int, help="only this battle")
    ap.add_argument("--ignore", default="", help="comma-separated field patterns to ignore (surveys)")
    args = ap.parse_args(argv)
    ignore = [g for g in args.ignore.split(",") if g]

    ours: Dict[Key, dict] = {key(r): r for r in read(args.ours)}
    theirs: Dict[Key, dict] = {key(r): r for r in read(args.theirs)}
    decisions: Dict[Key, dict] = {key(r): r for r in read(args.decisions)} if args.decisions else {}

    battles: Dict[Tuple[str, int], List[Key]] = collections.defaultdict(list)
    for k in ours:
        battles[(k[0], k[1])].append(k)

    clean = diverged = errors = 0
    records = 0
    by_pattern: Dict[str, List] = collections.defaultdict(lambda: [0, None])
    by_kind: Dict[str, int] = collections.Counter()
    by_rule: Dict[str, int] = collections.Counter()
    every: Dict[str, List] = collections.defaultdict(lambda: [0, None, None])
    shown = 0
    for battle in sorted(battles):
        if args.seed is not None and battle[1] != args.seed:
            continue
        ks = sorted(battles[battle], key=lambda k: (k[3], k[2]))
        first = None
        # Each side's channel lines so far (with --decisions), for rules
        # that depend on what was shown.
        history: Dict[str, List[str]] = {}
        illusions: Dict[str, Illusions] = {}
        for k in ks:
            records += 1
            t = theirs.get(k)
            if t is None:
                first = (k, [], "poke-env has no record")
                break
            if "error" in t:
                errors += 1
                first = (k, [], f"poke-env raised: {t['error']} on {t.get('line')!r}")
                break
            o = ours[k]
            dk = decisions.get(k)
            if dk is not None:
                history.setdefault(k[2], []).extend(dk["lines"])
            mixed = illusions.setdefault(k[2], Illusions(k[2])).feed(dk["lines"] if dk else [], o["state"])
            diffs = []
            for path, a, b in leaves(o["state"], t["state"]):
                d = Diff(path, a, b, o["state"], t["state"], o, history.get(k[2], []), mixed)
                rule = None if args.no_allowlist else allowed(d)
                if rule:
                    by_rule[rule] += 1
                    continue
                if any(fnmatch.fnmatchcase(pattern(d.path), g) for g in ignore):
                    by_rule["--ignore"] += 1
                    continue
                diffs.append(d)
            if diffs and args.all:
                # Every divergence, not just the first: for surveys.
                for d in diffs:
                    every[pattern(d.path)][0] += 1
                    if every[pattern(d.path)][1] is None:
                        every[pattern(d.path)][1] = k
                        every[pattern(d.path)][2] = (d.ours, d.theirs)
            if diffs and first is None:
                first = (k, diffs, None)
                if not args.all:
                    break
        if first is None:
            clean += 1
            continue
        diverged += 1
        k, diffs, note = first
        for d in diffs:
            p = pattern(d.path)
            by_pattern[p][0] += 1
            if by_pattern[p][1] is None:
                by_pattern[p][1] = k
            by_kind[kind(d.ours, d.theirs)] += 1
        if note:
            by_pattern[note.split(":")[0]][0] += 1
            if by_pattern[note.split(":")[0]][1] is None:
                by_pattern[note.split(":")[0]][1] = k
        if args.json:
            print(
                json.dumps(
                    {
                        "format": k[0],
                        "seed": k[1],
                        "side": k[2],
                        "index": k[3],
                        "note": note,
                        "diffs": [{"path": list(d.path), "ours": d.ours, "theirs": d.theirs} for d in diffs],
                    }
                )
            )
        elif shown < args.show:
            shown += 1
            print(f"DIVERGE {k[0]} seed {k[1]} {k[2]} record {k[3]} (turn {ours[k]['turn']})")
            lines: List[str] = []
            for i in range(k[3] + 1):
                dk = (k[0], k[1], k[2], i)
                if dk in decisions:
                    lines.extend(decisions[dk]["lines"])
            for line in lines[-args.context :]:
                print(f"      {line}")
            if note:
                print(f"  {note}")
            for d in diffs[:12]:
                print(f"  {'.'.join(d.path)}: ours {json.dumps(d.ours)}  poke-env {json.dumps(d.theirs)}  [{kind(d.ours, d.theirs)}]")
            if len(diffs) > 12:
                print(f"  ... {len(diffs) - 12} more")
            print()

    total = clean + diverged
    print(f"view diff: {clean}/{total} battles clean, {diverged} diverged ({records} records compared, {errors} poke-env errors)")
    if by_pattern:
        print("  first divergences by field:")
        for p, (n, k) in sorted(by_pattern.items(), key=lambda x: -x[1][0]):
            print(f"    {p}: {n} (e.g. {k[0]} seed {k[1]} {k[2]} record {k[3]})")
        print("  by kind: " + ", ".join(f"{k} {n}" for k, n in sorted(by_kind.items())))
    if by_rule:
        print("  allowed: " + ", ".join(f"{r} {n}" for r, n in sorted(by_rule.items())))
    if every:
        print("  every divergence by field (--all):")
        for p, (n, k, (a, b)) in sorted(every.items(), key=lambda x: -x[1][0]):
            example = f"ours {json.dumps(a)} poke-env {json.dumps(b)}"
            print(f"    {p}: {n} (e.g. {k[0]} seed {k[1]} {k[2]} record {k[3]}: {example[:160]})")
    return 1 if diverged else 0


if __name__ == "__main__":
    sys.exit(main())
