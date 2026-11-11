"""``python -m pikapro.showdown``: play on a local server or VGC Arena from the shell.

::

    python -m pikapro.showdown ladder --format gen9championsvgc2026regmc --team team.txt \\
        --policy simple_heuristics --games 3 [--server pikapro] [--save-logs logs/]
    python -m pikapro.showdown challenge SOMEONE --format gen9championsvgc2026regmc --team team.txt
    python -m pikapro.showdown accept [--from SOMEONE] --team team.txt --games 5
    python -m pikapro.showdown formats [--ladder]          # the server's formats, as a guest

On VGC Arena the bot's name and token come from ``--username`` /
``ARENA_BOT_NAME`` and ``ARENA_BOT_TOKEN`` (``--token`` works but shows in
the process list).
Each battle prints one JSON line, then a summary line. In a best-of format
(``gen9championsvgc2026regmcbo3``) ``--games`` counts series, and each
series prints one line with its games in it.
"""

from __future__ import annotations

import argparse
import asyncio
import importlib
import json
import logging
import os
import random
import signal
import sys
from typing import Any, Callable, List, Optional, Sequence

from .client import ShowdownClient, ShowdownError
from .handlers import BUILTIN_POLICIES, BattleHandler, RandomRequestHandler, SessionHandler
from .. import _pikapro
from .protocol import pack_team
from .room import BattleResult, SeriesResult


class _JsonFormatter(logging.Formatter):
    def format(self, record: logging.LogRecord) -> str:
        out = {"time": self.formatTime(record), "level": record.levelname, "logger": record.name}
        event = getattr(record, "event", None)
        if event is not None:
            out.update(event=event, user=getattr(record, "user", None), **getattr(record, "fields", {}))
        else:
            out["message"] = record.getMessage()
        return json.dumps(out, default=repr)


class _Usage(Exception):
    """An argument the command line refuses before it connects: its
    message is printed after ``error:``, and the exit status is 2."""


def _log_level(text: str) -> Any:
    """``--log-level``: a logging level's name (any case) or number."""
    if text.strip().isdigit():
        return int(text)
    level = logging.getLevelName(text.strip().upper())
    if not isinstance(level, int):
        raise argparse.ArgumentTypeError(f"{text!r} is not a logging level (DEBUG, INFO, WARNING, ERROR or CRITICAL)")
    return level


def _policy(text: str) -> Any:
    if text in BUILTIN_POLICIES:
        return text
    module, sep, attr = text.partition(":")
    if not sep or not module or not attr:
        raise _Usage(f"--policy: {text!r} is neither a built-in ({', '.join(BUILTIN_POLICIES)}) nor module:callable")
    sys.path.insert(0, os.getcwd())
    try:
        obj: Any = importlib.import_module(module)
        for part in attr.split("."):
            obj = getattr(obj, part)
    except (ImportError, AttributeError) as e:
        raise _Usage(f"--policy {text}: {e}") from None
    if not callable(obj):
        raise _Usage(f"--policy {text}: not a callable")
    return obj


_TEAM_HELP = (
    "A team file is an export (Showdown's teambuilder, or "
    "python -c \"import pikapro; print(pikapro.sample_team('championsvgc'))\" > team.txt) or a packed team"
)


def _check_team(text: str, format_id: str, known: bool = False) -> None:
    """Raises ``ValueError`` unless ``text`` is a team: an export that
    packs, or one line that is a packed team (which the client uploads as
    it is, so it is checked here): every set with Showdown's eleven ``|``
    at least, and, in a format pikapro plays (or with ``known``), sets the
    format's dex reads."""
    packed = pack_team(text, format_id)
    if "\n" in text.strip():
        return
    if any(one.count("|") < 11 for one in packed.split("]")):
        raise ValueError("one line that is not a packed team (NAME|SPECIES|ITEM|ABILITY|MOVES|...)")
    preset = _pikapro._showdown_preset(format_id)
    if preset is not None or known:
        _pikapro.pack(packed, preset or format_id)


def _positive(what: str) -> Callable[[str], int]:
    """An argparse type: an int of at least 1."""

    def read(text: str) -> int:
        try:
            value = int(text)
        except ValueError:
            raise argparse.ArgumentTypeError(f"{text!r} is not a number") from None
        if value < 1:
            raise argparse.ArgumentTypeError(f"{what} must be at least 1, not {value}")
        return value

    return read


def _teams(paths: Optional[Sequence[str]], seed: int, fmt: Optional[str] = None) -> Optional[Callable[[str], Optional[str]]]:
    """The ``--team`` files' teams, one picked per game. Each is read and
    packed here, before anything connects, as the client will pack it for
    the upload (in ``fmt``; with no ``--format``, in a Gen 9 format or a
    Champions one, whichever takes it): a file that can't be read, isn't
    UTF-8 text or isn't a team is refused, naming the file."""
    if not paths:
        return None
    teams = []
    for p in paths:
        try:
            with open(p, "rb") as f:
                # A file saved on Windows: its CRLF line ends, as \n.
                text = f.read().decode("utf-8-sig").replace("\r\n", "\n")
        except OSError as e:
            raise _Usage(f"--team {p}: {e.strerror or e}. {_TEAM_HELP}") from None
        except UnicodeDecodeError:
            raise _Usage(f"--team {p}: not UTF-8 text. {_TEAM_HELP}") from None
        if not text.strip():
            raise _Usage(f"--team {p}: the file is empty. {_TEAM_HELP}")
        problem: Optional[str] = None
        for format_id in [fmt] if fmt else ["gen9vgc", "championsvgc"]:
            try:
                _check_team(text, format_id, known=fmt is None)
                problem = None
                break
            except ValueError as e:
                problem = problem or str(e)
        if problem is not None:
            raise _Usage(f"--team {p}: {problem}. {_TEAM_HELP}")
        teams.append(text)
    rng = random.Random(seed)
    return lambda fmt: teams[0] if len(teams) == 1 else rng.choice(teams)


def _handler(args: argparse.Namespace) -> BattleHandler:
    if args.handler == "random":
        return RandomRequestHandler(args.seed)
    return SessionHandler(_policy(args.policy), batch=args.batch, seed=args.seed)


def _summary(results: List[Any]) -> dict:
    series = [r for r in results if isinstance(r, SeriesResult)]
    games: List[BattleResult] = [g for r in results for g in (r.games if isinstance(r, SeriesResult) else [r])]
    out = {
        "games": len(games),
        "wins": sum(r.won is True for r in games),
        "losses": sum(r.won is False for r in games),
        "ties": sum(r.tie for r in games),
        "unfinished": sum(not r.finished for r in games),
        "invalid": sum(r.invalid for r in games),
        "unavailable": sum(r.unavailable for r in games),
        "fallbacks": sum(r.fallbacks for r in games),
        "unknown_lines": sum(r.extras.get("unknown_lines", 0) for r in games),
    }
    if series:
        out.update(
            series=len(series),
            series_wins=sum(r.won is True for r in series),
            series_losses=sum(r.won is False for r in series),
            series_ties=sum(r.tie for r in series),
            series_unfinished=sum(not r.finished for r in series),
        )
    return out


def _error(e: BaseException) -> str:
    """An error as the command line prints it, with what to do about the
    common ones."""
    text = str(e) or type(e).__name__
    if "could not connect" in text:
        text += (
            " (is a server running there? Start one (pikapro-showdown-server, or Showdown's own with "
            "--no-security), or pick another with --server)"
        )
    return f"error: {text}"


async def _main(args: argparse.Namespace, team: Any = None, handler: Optional[BattleHandler] = None) -> int:
    if args.command == "formats":
        try:
            async with ShowdownClient(server=args.server, guest=True, reconnect=False) as client:
                for f in client.formats.values():
                    if args.ladder and not f.ladder:
                        continue
                    flags = [k for k in ("ladder", "challenge", "random_teams", "best_of") if getattr(f, k)]
                    print(f"{f.id}\t{f.name}\t{f.section}\t{','.join(flags)}")
        except ValueError as e:  # a refused host
            print(f"error: {e}", file=sys.stderr)
            return 2
        except ShowdownError as e:
            print(_error(e), file=sys.stderr)
            return 1
        return 0

    try:
        client = ShowdownClient(
            args.username,
            args.token,
            server=args.server,
            handler=handler,
            avatar=args.avatar,
            max_concurrent=args.concurrent,
            log_dir=args.save_logs,
            timer=args.timer,
            open_team_sheets={"auto": None, "accept": True, "reject": False, "mutual": "mutual"}[args.open_team_sheets],
        )
    except ValueError as e:  # a token for a server with no login, a refused host, a bad name...
        print(f"error: {e}", file=sys.stderr)
        return 2

    def show(results: List[Any]) -> None:
        for r in results:
            print(json.dumps(r.to_dict(), default=repr), flush=True)

    def done() -> List[Any]:  # what finished, when interrupted: series, and games outside them
        games = [r for r in client.results if r.series is None]
        return [*client.series_results, *games]

    task = asyncio.current_task()
    try:  # SIGTERM stops as Ctrl-C does: forfeit what is unfinished, report what finished
        assert task is not None
        asyncio.get_running_loop().add_signal_handler(signal.SIGTERM, task.cancel)
    except (NotImplementedError, RuntimeError, AssertionError):  # pragma: no cover (Windows)
        pass
    try:
        async with client:
            if args.command == "ladder":
                results = await client.ladder(args.games, args.format, team)
            elif args.command == "challenge":
                results = await client.challenge(args.user, args.format, team, n=args.games)
            else:
                results = await client.accept_challenges(args.from_user, args.games, team, format=args.format)
    except (ShowdownError, asyncio.TimeoutError) as e:
        # What finished before the error, its summary, then the error.
        show(done())
        print(json.dumps({"summary": _summary(done()), "error": str(e) or type(e).__name__}), flush=True)
        print(_error(e), file=sys.stderr)
        return 1
    except (KeyboardInterrupt, asyncio.CancelledError):
        show(done())
        print(json.dumps({"summary": _summary(done()), "interrupted": True}))
        return 130
    show(results)
    print(json.dumps({"summary": _summary(results)}), flush=True)
    return 0 if all(r.finished for r in results) else 1


def main(argv: Optional[Sequence[str]] = None) -> int:
    """The command line (``python -m pikapro.showdown --help``)."""
    parser = argparse.ArgumentParser(prog="python -m pikapro.showdown", description="Play on a local server or VGC Arena.")
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument(
        "--server",
        default="local",
        help=(
            "local (default), arena (VGC Arena), pikapro (pikapro-showdown-server on port 8000), "
            "pikapro:<address> (pikapro-showdown-server elsewhere), a ws:// or wss:// URL of a server you run, "
            "host:port or a port"
        ),
    )
    common.add_argument("--log-level", default="INFO", type=_log_level,
                        help="logging level: DEBUG, INFO (default), WARNING, ERROR or CRITICAL")
    common.add_argument("--log-json", action="store_true", help="log JSON records to stderr")

    play = argparse.ArgumentParser(add_help=False, parents=[common])
    play.add_argument("--username", default=None, help="on the arena, default $ARENA_BOT_NAME; else a random pikapro-... name")
    play.add_argument("--token", default=None, help="the arena's bot token, default $ARENA_BOT_TOKEN (prefer the variable)")
    play.add_argument("--team", action="append", help="a team file (export or packed); repeat for a random pick per game")
    play.add_argument("--games", type=_positive("--games"), default=1, help="games to play (default 1); series, in a best-of format (…bo3)")
    play.add_argument("--concurrent", type=_positive("--concurrent"), default=1, help="battles at once (default 1)")
    play.add_argument("--handler", choices=("session", "random"), default="session",
                      help="session: pikapro.rl.ChannelSession and --policy (default); random: a random valid choice from the request")
    play.add_argument("--policy", default="simple_heuristics",
                      help=f"{', '.join(BUILTIN_POLICIES)}, or module:callable taking (obs, mask)")
    play.add_argument("--batch", action="store_true", help="the policy takes batches (obs_batch, mask_batch)")
    play.add_argument("--seed", type=int, default=0, help="seed for built-in policies and team picks")
    play.add_argument("--save-logs", metavar="DIR", default=None, help="save each battle's log as DIR/ROOM.log")
    play.add_argument("--timer", action="store_true", help="turn the battle timer on")
    play.add_argument("--open-team-sheets", choices=("auto", "accept", "reject", "mutual"), default="auto",
                      help="answer Open Team Sheets offers: auto accepts in a series' games (default), or always accept or "
                           "reject; mutual accepts only once the opponent did (it waits 20 s, then rejects)")
    play.add_argument("--avatar", default=None)

    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("ladder", parents=[play], help="search the ladder")
    p.add_argument("--format", required=True)
    p = sub.add_parser("challenge", parents=[play], help="challenge a user")
    p.add_argument("user")
    p.add_argument("--format", required=True)
    p = sub.add_parser("accept", parents=[play], help="accept challenges")
    p.add_argument("--from", dest="from_user", default=None, help="only from this user")
    p.add_argument("--format", default=None, help="only in this format (default: any format the handler can play)")
    p = sub.add_parser("formats", parents=[common], help="list the server's formats (as a guest)")
    p.add_argument("--ladder", action="store_true", help="only the ladder's")

    args = parser.parse_args(argv)
    # Everything that can be refused is, before anything connects.
    team, battle_handler = None, None
    if args.command != "formats":
        try:
            team = _teams(args.team, args.seed, args.format)
            battle_handler = _handler(args)
        except _Usage as e:
            print(f"error: {e}", file=sys.stderr)
            return 2
    handler = logging.StreamHandler()
    handler.setFormatter(_JsonFormatter() if args.log_json else logging.Formatter("%(asctime)s %(levelname)s %(message)s"))
    root = logging.getLogger("pikapro.showdown")
    root.addHandler(handler)
    root.setLevel(args.log_level)
    try:
        return asyncio.run(_main(args, team, battle_handler))
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())
