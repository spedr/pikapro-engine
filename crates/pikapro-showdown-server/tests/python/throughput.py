"""Throughput of poke-env against a Showdown-protocol server.

Runs `--pairs` pairs of RandomPlayers in parallel processes (one pair per
process, as poke-env players share an event loop per process), each pair
playing `--battles` battles with `--concurrent` battles at a time, and
reports the total battles per second of wall time.

    python throughput.py --server ws://localhost:8000/showdown/websocket \
        --format gen9championsvgc2026regmc --battles 100 --pairs 4 --concurrent 8
"""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).parent


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", required=True)
    parser.add_argument("--format", required=True)
    parser.add_argument("--battles", type=int, default=100, help="battles per pair")
    parser.add_argument("--pairs", type=int, default=1)
    parser.add_argument("--concurrent", type=int, default=1)
    parser.add_argument("--mode", default="challenge")
    parser.add_argument("--name", default="tp")
    args = parser.parse_args()
    start = time.perf_counter()
    procs = [
        subprocess.Popen(
            [
                sys.executable,
                str(HERE / "battles.py"),
                "--server", args.server,
                "--format", args.format,
                "--battles", str(args.battles),
                "--concurrent", str(args.concurrent),
                "--mode", args.mode,
                "--name", f"{args.name}{i}",
                "--timeout", "3000",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        for i in range(args.pairs)
    ]
    results = []
    for p in procs:
        out, _ = p.communicate()
        lines = [l for l in out.splitlines() if l.startswith("{")]
        results.append(json.loads(lines[-1]) if lines else {"battles": 0, "problems": ["no output"]})
    elapsed = time.perf_counter() - start
    battles = sum(r["battles"] for r in results)
    turns = sum(r.get("mean_turns", 0) * r["battles"] for r in results)
    summary = {
        "server": args.server,
        "format": args.format,
        "pairs": args.pairs,
        "concurrent": args.concurrent,
        "battles": battles,
        "seconds": round(elapsed, 2),
        "battles_per_second": round(battles / elapsed, 2),
        "turns_per_second": round(turns / elapsed, 1),
        "problems": [p for r in results for p in r.get("problems", [])][:5],
    }
    print(json.dumps(summary))


if __name__ == "__main__":
    main()
