"""Compares the message types of two recorded sessions (record_proxy.py).

For each direction it counts the protocol messages by type: the server's
lines by their `|type|` (room frames' lines separately from global ones),
the client's by command (`/choose`, `/team`, ...). Types present in only
one session are what to look at; counts differ because battles do.

    python traffic.py showdown.jsonl ours.jsonl
"""

from __future__ import annotations

import collections
import json
import sys


def kinds(path: str) -> collections.Counter:
    counts: collections.Counter = collections.Counter()
    for raw in open(path, encoding="utf-8"):
        record = json.loads(raw)
        data = record["data"]
        if record["dir"] == ">":
            room, _, text = data.partition("|")
            command = text.split(" ", 1)[0] if text.startswith("/") else "(chat)"
            counts[("client", ("room " if room else "global ") + command)] += 1
            continue
        lines = data.split("\n")
        scope = "global"
        if lines[0].startswith(">"):
            scope, lines = "room", lines[1:]
        for line in lines:
            if not line.startswith("|"):
                kind = "(text)" if line else "(blank)"
            else:
                kind = "|" + line.split("|")[1] + "|"
            counts[("server", f"{scope} {kind}")] += 1
    return counts


def main():
    a, b = kinds(sys.argv[1]), kinds(sys.argv[2])
    rows = sorted(set(a) | set(b))
    print(f"{'direction':8} {'message':34} {'A':>6} {'B':>6}")
    for key in rows:
        mark = "" if a[key] and b[key] else "  <-- only in " + ("A" if a[key] else "B")
        print(f"{key[0]:8} {key[1]:34} {a[key]:>6} {b[key]:>6}{mark}")


if __name__ == "__main__":
    main()
