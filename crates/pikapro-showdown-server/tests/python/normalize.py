"""Normalises protocol transcripts (probe.py output, or record_proxy.py JSON
lines) so that the traffic of two servers can be diffed.

What differs between any two runs is replaced by placeholders: timestamps,
guest numbers, avatars, challenge strings, battle numbers, rqids, and with
--shapes the battle log itself (each line reduced to its message type,
since battles are seeded differently) and the request JSON (reduced to its
keys).

    python normalize.py showdown.txt ours.txt            # unified diff
    python normalize.py --shapes showdown.txt ours.txt   # message types only
"""

from __future__ import annotations

import argparse
import difflib
import json
import re
import sys

# Lines of the battle log proper: their content depends on the seed.
BATTLE_LINES = re.compile(
    r"^\|(-[a-z]+|move|switch|drag|detailschange|replace|faint|turn|cant|swap|poke|teamsize|"
    r"showteam|split|p[12]|win|tie|raw|inactive|request)\|"
)


def keys(value, prefix=""):
    """The key paths of a JSON value, list elements merged."""
    out = set()
    if isinstance(value, dict):
        for k, v in value.items():
            out.add(prefix + k)
            out |= keys(v, prefix + k + ".")
    elif isinstance(value, list):
        for v in value:
            out |= keys(v, prefix + "[].")
    return out


def normalise(line: str, shapes: bool) -> str:
    line = re.sub(r"\|t:\|\d+", "|t:|T", line)
    line = re.sub(r"battle-([a-z0-9]+)-\d+", r"battle-\1-N", line)
    line = re.sub(r"Guest \d+", "Guest N", line)
    line = re.sub(r"\|challstr\|4\|[0-9a-f]+", "|challstr|4|HEX", line)
    line = re.sub(r'"rqid":\d+', '"rqid":R', line)
    line = re.sub(r"\|updateuser\|( [^|]*)\|([01])\|[^|]*\|", r"|updateuser|\1|\2|AVATAR|", line)
    line = re.sub(r"\|player\|(p[12])\|([^|]*)\|[^|]*\|", r"|player|\1|\2|AVATAR|", line)
    line = re.sub(r"\|formats\|.*", "|formats|...", line)
    line = re.sub(r"(\|customgroups\|).*", r"\1...", line)
    line = re.sub(r"\|/utm .*", "|/utm TEAM", line)
    if shapes:
        body = line.split("] ", 1)[1] if line.startswith(("<<<", ">>>")) else line
        if body.startswith("|request|"):
            # The request's kind: its first key (the line may be cut short).
            m = re.match(r'\|request\|\{"(\w+)"', body)
            kind = m.group(1) if m else body[len("|request|"):]
            return line.split("|request|")[0] + f"|request|<{kind}>"
        m = BATTLE_LINES.match(body)
        if m:
            return line[: len(line) - len(body)] + f"|{m.group(1)}|..."
        if line.startswith(">>>") and "/choose" in line:
            return re.sub(r"/choose .*", "/choose ...", line)
    return line


def by_client(lines: list[str]) -> list[str]:
    """Within each `### step` of a probe transcript: what was sent, then
    what each client received, client by client (the interleaving of
    different connections is not part of the protocol)."""
    out, step = [], []

    def flush():
        sent = [l for l in step if l.startswith(">>>")]
        received = [l for l in step if not l.startswith(">>>")]
        clients = sorted({l.split("]")[0] for l in received})
        out.extend(sent)
        for c in clients:
            out.extend(l for l in received if l.split("]")[0] == c)

    for line in lines:
        if line.startswith("### "):
            flush()
            step = []
            out.append(line)
        else:
            step.append(line)
    flush()
    return out


def load(path: str, shapes: bool) -> list[str]:
    out = []
    with open(path, encoding="utf-8") as f:
        for raw in f:
            raw = raw.rstrip("\n")
            if raw.startswith("{"):
                record = json.loads(raw)
                prefix = f"{record['dir'] * 3} [{record['conn']}] "
                lines = [prefix + l for l in record["data"].split("\n")]
            else:
                lines = [raw]
            out.extend(normalise(line, shapes) for line in lines)
    if any(l.startswith("### ") for l in out):
        out = by_client(out)
    else:
        # A recorded session: each connection's traffic in order.
        conns = sorted({l.split("]")[0][4:] for l in out}, key=lambda c: (len(c), c))
        out = [l for c in conns for l in out if l.split("]")[0][4:] == c]
    if shapes:
        # Consecutive battle lines of one kind collapse (battles differ in length).
        collapsed = []
        for line in out:
            if collapsed and line == collapsed[-1] and line.endswith("|..."):
                continue
            collapsed.append(line)
        out = collapsed
    return out


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("a")
    parser.add_argument("b")
    parser.add_argument("--shapes", action="store_true")
    args = parser.parse_args()
    a, b = load(args.a, args.shapes), load(args.b, args.shapes)
    sys.stdout.writelines(l + "\n" for l in difflib.unified_diff(a, b, args.a, args.b, n=2, lineterm=""))


if __name__ == "__main__":
    main()
