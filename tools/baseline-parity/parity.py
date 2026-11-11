#!/usr/bin/env python3
"""Baseline parity: our Rust ports of poke-env's baseline players against
poke-env's own, decision by decision.

``pikapro-baseline-dump`` (Rust) plays battles and records, at every
decision, what ``MaxBasePowerPolicy`` and ``SimpleHeuristicsPolicy``
choose from the side's view, and the driver's choice it submitted. This
script replays each battle on ``pikapro.Battle``, keeps a poke-env battle
object per side and player in sync through ``pikapro.compat.poke_env.Feeder``
(poke-env's own parsing of the side's channel and requests), asks
poke-env's ``MaxBasePowerPlayer`` and ``SimpleHeuristicsPlayer`` at the
same decisions, and compares the choices as Showdown reads them
(``Battle.parse_choice``).

    cargo run --release -p pikapro-bench --bin pikapro-baseline-dump -- --format gen9singles --battles 200 -o d.jsonl
    python parity.py d.jsonl [--mismatches m.jsonl]

Our players Mega Evolve whenever they can, poke-env's never do (the one
deliberate deviation): choices are compared without Mega Evolution, and
``mega-deviation`` counts the deterministic decisions that differ only
by it.

poke-env draws from Python's ``random`` (a random target, a random switch,
a random order). Each decision is asked twice, with ``random`` forced to
its lowest and its highest outcomes: if the answers agree, the decision
is **deterministic** and ours must be identical; if they agree once move
targets are ignored, it is **random in its target only** and ours must
agree without targets; otherwise it is **random**, and only counted. A
poke-env choice Showdown rejects (a switch out of a hidden trap) is
answered as the in-process driver answers it (the error, the updated
request) and asked again, so both sides are compared on the choice that
is finally accepted.
"""

from __future__ import annotations

import argparse
import collections
import json
import random
import sys
from types import SimpleNamespace

import pikapro
from pikapro.compat.poke_env import Feeder, choice_text, make_player, snapshot
from pikapro.compat.poke_env.driver import _locked_move_targets, _switch_names

PERSPECTIVES = ("p1", "p2")
NAMES = ("maxbp", "heuristic")


def _updated_request(b, side: int, choice: str, message: str) -> str:
    """The request Showdown sends again after an ``[Unavailable choice]``
    (``emitRequest(activeRequest, true)`` after ``updateRequestForPokemon``),
    rebuilt from the error: the choice is only parsed here, never submitted,
    so ``b``'s own request doesn't show it. The trapped slot gets
    ``trapped: true``, the disabled move ``disabled: true``, and the
    request ``update: true``."""
    req = json.loads(b.request_json(side))
    active = req.get("active") or []
    mons = req.get("side", {}).get("pokemon", [])
    parts = [p.strip() for p in choice.split(",")]
    if "is trapped" in message:
        for slot, part in enumerate(parts):
            if part.startswith("switch") and slot < len(active) and not active[slot].get("trapped"):
                active[slot].pop("maybeTrapped", None)
                active[slot]["trapped"] = True
                break
    elif message.endswith(" is disabled") and "'s " in message:
        body = message.split("Can't move: ", 1)[-1][: -len(" is disabled")]
        name, move = body.split("'s ", 1)
        for slot, entry in enumerate(active):
            if slot < len(mons) and mons[slot].get("ident", "")[4:] == name:
                entry.pop("maybeLocked", None)
                if len(active) > 1:
                    entry.pop("maybeDisabled", None)
                for m in entry.get("moves", []):
                    if m.get("move") == move:
                        m["disabled"] = True
                        m["disabledSource"] = ""
                if all(m.get("disabled") or m.get("id") == "struggle" for m in entry.get("moves", [])):
                    for k in ("canMegaEvo", "canMegaEvoX", "canMegaEvoY", "canUltraBurst", "canDynamax"):
                        if entry.get(k):
                            entry[k] = False
                    entry.pop("canZMove", None)
                    entry.pop("maxMoves", None)
                    entry.pop("canTerastallize", None)
                break
    req["update"] = True
    return json.dumps(req, separators=(",", ":"), ensure_ascii=False)


def _players():
    from poke_env.player.baselines import MaxBasePowerPlayer, SimpleHeuristicsPlayer

    return {"maxbp": MaxBasePowerPlayer, "heuristic": SimpleHeuristicsPlayer}


class Forced:
    """Python's ``random`` forced to its lowest (``high=False``) or highest
    outcomes, for the draws poke-env's players make."""

    def __init__(self, high: bool):
        self.high = high

    def __enter__(self):
        self.saved = (random.random, random.choice, random.shuffle)
        if self.high:
            random.random = lambda: 0.999999999
            random.choice = lambda seq: seq[-1]
            random.shuffle = lambda x: x.reverse()
        else:
            random.random = lambda: 0.0
            random.choice = lambda seq: seq[0]
            random.shuffle = lambda x: None
        return self

    def __exit__(self, *exc):
        random.random, random.choice, random.shuffle = self.saved
        return False


def canonical(b, side, text, request, doubles):
    """Showdown's reading of a choice string, or ``(None, error)``."""
    rec = SimpleNamespace(quirks={})
    if doubles:
        text = _locked_move_targets(text, request, rec)
    text = _switch_names(text, request, b, side, rec)
    try:
        return str(b.parse_choice(side, text)), None
    except ValueError as e:
        return None, str(e)


def key(b, side, canon, targets=True):
    """A canonical choice as comparable slot actions, without Mega
    Evolution (our one deliberate deviation) and, unless ``targets``,
    without move targets."""
    if canon is None or canon == "forfeit":
        return canon
    c = b.parse_choice(side, canon)
    return tuple(
        (a.kind, a.index, a.target if targets else None, None if a.gimmick == "mega" else a.gimmick) for a in c.actions
    )


def ask(feeder, player, b, side, doubles, high):
    """poke-env's accepted answer: asks, and while Showdown rejects the
    choice, feeds the error (and the updated request) to a copy of the
    feeder and asks again, as the in-process driver does. The caller's
    feeder keeps its state. Returns ``(canonical, first_error)``."""
    import copy

    pending = feeder.pending
    with Forced(high):
        text = choice_text(feeder.decide(player))
    feeder.pending = pending
    if text is None:
        return "forfeit", None
    canon, first_error = canonical(b, side, text, feeder.battle.last_request, doubles)
    if canon is not None:
        return canon, None
    f = copy.copy(feeder)
    logger = feeder.battle.logger
    f.battle = copy.deepcopy(feeder.battle, {id(logger): logger})
    error = first_error
    for _ in range(4):
        f.receive_error(error)
        if error.startswith("[Unavailable choice]"):
            f.receive_request(_updated_request(b, side, text, error))
        if f.pending is None:
            break
        with Forced(high):
            text = choice_text(f.decide(player))
        if text is None:
            return "forfeit", first_error
        canon, error = canonical(b, side, text, f.battle.last_request, doubles)
        if canon is not None:
            return canon, first_error
    return None, first_error


def battles(records):
    """Groups the dump's records by battle (they come in order)."""
    group = []
    for r in records:
        if r["n"] == 0 and group:
            yield group
            group = []
        group.append(r)
    if group:
        yield group


def replay(group, players, stats, mismatches, details):
    head = group[0]
    fmt, seed, teams = head["format"], head["seed"], head["teams"]
    b = pikapro.Battle(fmt, teams, seed=seed)
    doubles = b.space.active > 1
    feeders = {(s, name): Feeder(fmt, s, player=players[name]) for s in (0, 1) for name in NAMES}
    start = 0

    def update(first=False):
        nonlocal start
        n = b.event_count
        for s in (0, 1):
            lines = b.log(PERSPECTIVES[s], start)
            if first:
                lines = b.header() + lines
            for name in NAMES:
                feeders[(s, name)].receive(lines)
        start = n
        if not b.over:
            for s in (0, 1):
                request = b.request_json(s)
                for name in NAMES:
                    feeders[(s, name)].receive_request(request)

    update(first=True)
    for r in group:
        s = r["side"]
        if b.request_kind(s) == "wait":
            raise RuntimeError(f"seed {seed} decision {r['n']}: side {s} waits in pikapro.Battle")
        kind = r["kind"]
        for name in NAMES:
            st = stats[(fmt, name)]
            if kind == "preview":
                st["preview"] += 1
                continue
            f = feeders[(s, name)]
            if f.pending is None:
                st["no-request"] += 1
                continue
            try:
                lo, err_lo = ask(f, players[name], b, s, doubles, high=False)
                hi, _ = ask(f, players[name], b, s, doubles, high=True)
            except Exception as e:  # noqa: BLE001 - poke-env's own failures are counted
                st["poke-env-error"] += 1
                st_errors = stats[("errors", name)]
                st_errors[f"{type(e).__name__}: {str(e)[:100]}"] += 1
                continue
            ours = canonical(b, s, r[name], None, False)[0]
            if err_lo is not None:
                st["rejected-first"] += 1
            if lo is None:
                st["poke-env-rejected"] += 1
                continue
            try:
                if lo == hi:
                    cls = "deterministic"
                    exact = ours == lo
                    agree = key(b, s, ours) == key(b, s, lo)
                elif key(b, s, lo, targets=False) == key(b, s, hi, targets=False):
                    cls = "target-random"
                    exact = False
                    agree = key(b, s, ours, targets=False) == key(b, s, lo, targets=False)
                else:
                    cls = "random"
                    exact = ours in (lo, hi)
                    agree = exact or key(b, s, ours) in (key(b, s, lo), key(b, s, hi))
            except ValueError as e:
                # A canonical string that doesn't parse back (it should:
                # `str(Choice)` writes `default` where only it does):
                # counted, as poke-env's failures are.
                st["canonical-error"] += 1
                stats[("errors", name)][f"canonical: {str(e)[:100]}"] += 1
                continue
            st[cls] += 1
            st[f"{cls}-agree"] += int(agree)
            st[f"{kind}-{cls}"] += 1
            st[f"{kind}-{cls}-agree"] += int(agree)
            if agree and not exact and cls == "deterministic":
                st["mega-deviation"] += 1
            if not agree and cls != "random" and mismatches is not None:
                m = {
                    "format": fmt,
                    "seed": seed,
                    "side": s,
                    "n": r["n"],
                    "turn": r["turn"],
                    "kind": kind,
                    "player": name,
                    "class": cls,
                    "ours": ours,
                    "poke_env": lo,
                    "poke_env_high": hi,
                }
                if details:
                    m["request"] = json.loads(b.request_json(s))
                    m["poke_env_state"] = snapshot(f.battle)
                mismatches.write(json.dumps(m, ensure_ascii=False, separators=(",", ":")) + "\n")
        if not b.submit(s, r["driver"]):
            continue
        update()


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


def report(stats):
    out = {}
    for (fmt, name), st in sorted(stats.items()):
        if fmt == "errors":
            continue
        row = dict(st)
        for cls in ("deterministic", "target-random", "random"):
            n = st.get(cls, 0)
            if n:
                row[f"{cls}-rate"] = round(st.get(f"{cls}-agree", 0) / n, 4)
        out[f"{fmt}/{name}"] = row
    errors = {name: dict(c) for (fmt, name), c in stats.items() if fmt == "errors"}
    if errors:
        out["poke-env errors"] = errors
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("input", help="pikapro-baseline-dump's JSONL ('-' for stdin)")
    ap.add_argument("--mismatches", help="write each disagreement (JSONL)")
    ap.add_argument("--details", action="store_true", help="add the request and poke-env's state to mismatches")
    ap.add_argument("--limit", type=int, default=0, help="stop after this many battles")
    ap.add_argument("-o", "--out", default="-", help="summary JSON (default: stdout)")
    args = ap.parse_args(argv)
    stats = collections.defaultdict(collections.Counter)
    mismatches = open(args.mismatches, "w", encoding="utf-8") if args.mismatches else None
    players = None
    count = 0
    try:
        for group in battles(_read(args.input)):
            if players is None:
                classes = _players()
                players = {n: make_player(classes[n], group[0]["format"]) for n in NAMES}
            replay(group, players, stats, mismatches, args.details)
            count += 1
            if args.limit and count >= args.limit:
                break
    finally:
        if mismatches is not None:
            mismatches.close()
    summary = {"battles": count, **report(stats)}
    text = json.dumps(summary, indent=1)
    if args.out == "-":
        print(text)
    else:
        with open(args.out, "w", encoding="utf-8") as f:
            f.write(text + "\n")
        print(text, file=sys.stderr)


if __name__ == "__main__":
    main()
