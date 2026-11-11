"""Plays poke-env players against each other on a Showdown-protocol server.

Used for the integration tests (against pikapro-showdown-server) and for the
comparison with Showdown's own server (same script, other URL):

    python battles.py --server ws://localhost:8000/showdown/websocket \
        --format gen9championsvgc2026regmc --battles 100 --mode challenge

It checks what poke-env's battle objects say (every battle finished, one
winner or a tie, winners agreeing between the two players, team sizes)
and counts the warnings and errors poke-env logs, then prints a JSON
summary. The exit status is 1 if any check failed.
"""

from __future__ import annotations

import argparse
import asyncio
import collections
import json
import logging
import pathlib
import sys
import time

from poke_env import AccountConfiguration, ServerConfiguration
from poke_env.player import MaxBasePowerPlayer, RandomPlayer, SimpleHeuristicsPlayer

TEAMS = pathlib.Path(__file__).parent / "teams"

# The Showdown formats pikapro-showdown-server plays, with the team file
# poke-env registers for each (custom games accept any team).
FORMATS = {
    "gen9customgame": "gen9vgc2025regi.txt",
    "gen9doublescustomgame": "gen9vgc2025regi.txt",
    "gen9vgc2025regi": "gen9vgc2025regi.txt",
    "gen9championsvgc2026regmc": "gen9championsvgc2026regmc.txt",
}

PLAYERS = {"random": RandomPlayer, "maxbp": MaxBasePowerPlayer, "heuristic": SimpleHeuristicsPlayer}


class Counter(logging.Handler):
    """Counts the warnings and errors poke-env logs, by their first words."""

    def __init__(self) -> None:
        super().__init__(logging.WARNING)
        self.counts: collections.Counter[str] = collections.Counter()
        self.examples: dict[str, str] = {}

    def emit(self, record: logging.LogRecord) -> None:
        message = record.getMessage()
        key = f"{record.levelname}: {message.split(':')[0][:60]}"
        self.counts[key] += 1
        self.examples.setdefault(key, message[:300])


def check(format_id: str, a, b) -> list[str]:
    """What poke-env's battle objects of the two players must agree on."""
    problems = []
    if set(a.battles) != set(b.battles):
        problems.append(f"battle tags differ: {sorted(set(a.battles) ^ set(b.battles))[:3]}")
    vgc = "vgc" in format_id
    for tag, battle in a.battles.items():
        other = b.battles.get(tag)
        if not battle.finished or (other is not None and not other.finished):
            problems.append(f"{tag}: not finished")
            continue
        if other is not None:
            if battle.won != other.lost or battle.lost != other.won:
                problems.append(f"{tag}: winners disagree ({battle.won}, {other.won})")
        for side in (battle, other):
            if side is None:
                continue
            if not side.battle_tag.startswith(f"battle-{format_id}-"):
                problems.append(f"{tag}: room id")
            size = len(side.team)
            if size != 6:
                problems.append(f"{tag}: {side.player_username} sees {size} of their Pokémon")
            foes = len(side.opponent_team)
            if foes == 0 or foes > 6:
                problems.append(f"{tag}: {side.player_username} sees {foes} foes")
            if side.turn < 1 and not vgc:
                problems.append(f"{tag}: no turn")
    return problems


async def run(args: argparse.Namespace) -> dict:
    counter = Counter()
    logging.getLogger().addHandler(counter)
    team = (TEAMS / FORMATS.get(args.format, "gen9vgc2025regi.txt")).read_text()
    server = ServerConfiguration(args.server, "http://localhost:8000/action.php?")
    common = dict(
        battle_format=args.format,
        server_configuration=server,
        max_concurrent_battles=args.concurrent,
        team=team,
        log_level=logging.WARNING,
        accept_open_team_sheet=args.ots,
        start_timer_on_battle_start=args.timer,
        strict_battle_tracking=args.strict,
    )
    tag = args.name
    a = PLAYERS[args.p1](AccountConfiguration(f"{tag} a", None), **common)
    b = PLAYERS[args.p2](AccountConfiguration(f"{tag} b", None), **common)
    start = time.perf_counter()
    if args.mode == "challenge":
        await asyncio.wait_for(a.battle_against(b, n_battles=args.battles), args.timeout)
    else:
        await asyncio.wait_for(asyncio.gather(a.ladder(args.battles), b.ladder(args.battles)), args.timeout)
    elapsed = time.perf_counter() - start
    problems = check(args.format, a, b)
    if len(a.battles) != args.battles:
        problems.append(f"{len(a.battles)} battles played, {args.battles} asked for")
    turns = [battle.turn for battle in a.battles.values()]
    summary = {
        "server": args.server,
        "format": args.format,
        "mode": args.mode,
        "players": [args.p1, args.p2],
        "battles": len(a.battles),
        "finished": a.n_finished_battles,
        "wins": [a.n_won_battles, b.n_won_battles],
        "ties": a.n_tied_battles,
        "mean_turns": round(sum(turns) / max(len(turns), 1), 2),
        "seconds": round(elapsed, 3),
        "battles_per_second": round(len(a.battles) / elapsed, 2),
        "log": dict(counter.counts),
        "log_examples": counter.examples if args.verbose else {},
        "problems": problems[:20],
    }
    for player in (a, b):
        await player.ps_client.stop_listening()
    return summary


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--server", default="ws://localhost:8000/showdown/websocket")
    parser.add_argument("--format", default="gen9championsvgc2026regmc")
    parser.add_argument("--battles", type=int, default=10)
    parser.add_argument("--concurrent", type=int, default=1, help="max_concurrent_battles per player")
    parser.add_argument("--mode", choices=["challenge", "ladder"], default="challenge")
    parser.add_argument("--p1", choices=sorted(PLAYERS), default="random")
    parser.add_argument("--p2", choices=sorted(PLAYERS), default="random")
    parser.add_argument("--ots", action="store_true", help="accept open team sheets")
    parser.add_argument("--timer", action="store_true", help="turn the battle timer on")
    parser.add_argument("--strict", action="store_true", help="poke-env checks its tracked state against every request")
    parser.add_argument("--name", default="pikapro", help="username prefix (unique per concurrent run)")
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()
    summary = asyncio.run(run(args))
    print(json.dumps(summary))
    sys.exit(1 if summary["problems"] else 0)


if __name__ == "__main__":
    main()
