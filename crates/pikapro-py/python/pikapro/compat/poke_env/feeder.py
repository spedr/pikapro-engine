"""Keeps a poke-env ``Battle``/``DoubleBattle`` in sync with one side of a
battle, in-process, from what that side's client receives.

The feeder is a client, not a fork: every line goes through poke-env's own
code (``AbstractBattle.parse_message``, ``parse_request``, ``won_by``,
``tied``), dispatched the way ``Player._handle_battle_message`` dispatches
a battle room's messages in poke-env 0.16.1. Only the parts that need a
websocket (creating the battle from ``|init|``, the battle-count queues,
``/leave``, ``/timer``, the open-team-sheet replies) are left out; where
``_handle_battle_message`` would call ``_handle_battle_request`` the
feeder records a pending decision instead, which :meth:`Feeder.decide`
answers the way ``_handle_battle_request`` does.
"""

from __future__ import annotations

import asyncio
import itertools
import json
import logging
import random
from dataclasses import dataclass
from typing import Any, Dict, Iterable, List, Optional

from ._formats import is_doubles, pikapro_format, showdown_format


def require_poke_env():
    """Imports poke-env, or raises an ImportError that says how to get it."""
    try:
        import poke_env  # noqa: F401
    except ImportError as e:  # pragma: no cover - exercised without poke-env
        raise ImportError(
            "pikapro.compat.poke_env needs poke-env (>= 0.16): pip install 'pikapro[poke-env]'"
        ) from e
    return poke_env


class FeederError(RuntimeError):
    """poke-env raised while parsing a line. ``line`` is the line (or the
    ``|request|`` line) it choked on; ``__cause__`` is poke-env's error."""

    def __init__(self, line: str, cause: BaseException):
        super().__init__(f"poke-env failed on {line[:300]!r}: {type(cause).__name__}: {cause}")
        self.line = line
        self.cause = cause


@dataclass
class Decision:
    """A request poke-env would answer now (``_handle_battle_request``).
    ``maybe_default``: it follows an ``[Invalid choice]`` error."""

    maybe_default: bool = False


class _CountingHandler(logging.Handler):
    def __init__(self, sink: list):
        super().__init__(logging.WARNING)
        self.sink = sink

    def emit(self, record: logging.LogRecord) -> None:
        try:
            msg = record.getMessage()
        except Exception:  # pragma: no cover
            msg = str(record.msg)
        self.sink.append((record.levelname, msg))


try:  # poke-env reads requests with orjson; json is the fallback
    from orjson import loads as _loads
except ImportError:  # pragma: no cover
    _loads = json.loads

_TAGS = itertools.count(1)
_LOOP: Optional[asyncio.AbstractEventLoop] = None


def _resolve(value):
    """Awaits ``value`` if poke-env handed back an awaitable (``choose_move``
    and ``teampreview`` may be coroutines), on a private event loop (in a
    helper thread if this thread already runs one, as in Jupyter)."""
    if not (asyncio.iscoroutine(value) or hasattr(value, "__await__")):
        return value

    async def wait():
        return await value

    try:
        asyncio.get_running_loop()
    except RuntimeError:
        global _LOOP
        if _LOOP is None or _LOOP.is_closed():
            _LOOP = asyncio.new_event_loop()
        return _LOOP.run_until_complete(wait())
    import concurrent.futures

    with concurrent.futures.ThreadPoolExecutor(1) as pool:
        return pool.submit(asyncio.run, wait()).result()


class Feeder:
    """A poke-env battle object kept in sync with one side of a battle.

    ``format`` is a pikapro format (``"championsvgc"``) or its Showdown id;
    ``side`` is ``0``/``"p1"`` or ``1``/``"p2"``. Pass ``player`` (a
    poke-env ``Player``) to take its settings (open team sheets, strict
    tracking, its teambuilder's team as ``teampreview_team``, doubles or
    singles) and, with ``register=True``, to add the battle to
    ``player.battles`` so its win counters see it.

    The battle knows its player by its name in the log: ``username``, which
    must be the name :class:`pikapro.Battle` gives that side (``players=``);
    by default the side's name (``p1`` or ``p2``), pikapro's default.

    With ``workarounds`` (the default), lines Showdown sends but poke-env
    misreads are rewritten first to an equivalent form poke-env reads
    (:func:`workaround`; counted in :attr:`quirks`).

    Feed it with :meth:`receive`, the lines the side's client gets in the
    order it gets them (a channel's log lines and ``|request|<json>``
    lines; :meth:`receive_request` wraps the latter). After a request that
    needs an answer, :attr:`pending` is a :class:`Decision` and
    :meth:`decide` asks the player for the choice message.
    """

    def __init__(
        self,
        format: str,
        side,
        *,
        player: Any = None,
        register: bool = False,
        battle_tag: Optional[str] = None,
        logger: Optional[logging.Logger] = None,
        save_replays=False,
        workarounds: bool = True,
        username: Optional[str] = None,
    ):
        require_poke_env()
        from poke_env.battle.battle import Battle
        from poke_env.battle.double_battle import DoubleBattle
        from poke_env.battle.pokemon import Pokemon
        from poke_env.data import GenData
        from poke_env.player.player import Player
        from poke_env.teambuilder.constant_teambuilder import ConstantTeambuilder

        # what the player's own message handler ignores (Player.MESSAGES_TO_IGNORE)
        self._ignore = type(player).MESSAGES_TO_IGNORE if player is not None else Player.MESSAGES_TO_IGNORE

        self.format = pikapro_format(format)
        self.showdown_format = showdown_format(format)
        self.side = _side_index(side)
        self.role = f"p{self.side + 1}"
        self.username = username or self.role
        self.player = player
        self.warnings: List[tuple] = []
        if logger is None:
            logger = logging.Logger(f"pikapro.compat.poke_env.{self.role}", logging.WARNING)
            logger.addHandler(_CountingHandler(self.warnings))
            logger.propagate = False
        self.logger = logger
        self.battle_tag = battle_tag or f"battle-{self.showdown_format}-{next(_TAGS)}"
        gen = GenData.from_format(self.showdown_format).gen
        doubles = player.format_is_doubles if player is not None else is_doubles(self.format)
        cls = DoubleBattle if doubles else Battle
        # Player._create_battle
        self.battle = cls(
            battle_tag=self.battle_tag,
            username=self.username,
            logger=logger,
            gen=gen,
            save_replays=save_replays,
        )
        if player is not None and isinstance(getattr(player, "_team", None), ConstantTeambuilder):
            self.battle.teampreview_team = [Pokemon(gen=gen, teambuilder=tb_mon) for tb_mon in player._team.team]
        if player is not None and register:
            player._battles[self.battle_tag] = self.battle
        self.accept_open_team_sheet = bool(getattr(player, "accept_open_team_sheet", False))
        self.strict_battle_tracking = bool(getattr(player, "_strict_battle_tracking", False))
        self.format_is_bestof = bool(getattr(player, "format_is_bestof", False))
        self.pending: Optional[Decision] = None
        self.errors: List[str] = []
        #: poke-env quirks the feeder worked around (see :func:`workaround`), by name
        self.quirks: Dict[str, int] = {}
        self.workarounds = workarounds

    # -- input -------------------------------------------------------------

    def receive(self, lines: Iterable[str]) -> Optional[Decision]:
        """Feeds lines, in order, as one client message's body (the lines
        after ``>battle-...``). Returns :attr:`pending`."""
        for line in lines:
            split_message = line.split("|")
            if self.workarounds:
                quirk = workaround(split_message)
                if quirk is not None:
                    self.quirks[quirk] = self.quirks.get(quirk, 0) + 1
            try:
                self._dispatch(split_message)
            except Exception as e:
                raise FeederError(line, e) from e
        return self.pending

    def receive_request(self, request) -> Optional[Decision]:
        """Feeds a ``|request|`` line: ``request`` is the JSON text (or a
        dict, dumped as Showdown writes it)."""
        if not isinstance(request, str):
            request = json.dumps(request, separators=(",", ":"), ensure_ascii=False)
        return self.receive([f"|request|{request}"])

    def receive_error(self, message: str) -> Optional[Decision]:
        """Feeds Showdown's ``|error|`` line for a rejected choice
        (``message`` is ``"[Invalid choice] ..."`` or
        ``"[Unavailable choice] ..."``)."""
        return self.receive([f"|error|{message}"])

    def _dispatch(self, split_message: List[str]) -> None:
        # Player._handle_battle_message, for one line of a battle room's message
        battle = self.battle
        if not split_message:
            return
        elif len(split_message) == 1:
            if battle.teampreview and self.accept_open_team_sheet and "rejected open team sheets." in split_message[0]:
                self._request()
        elif split_message[1] == "":
            battle.parse_message(split_message)
        elif split_message[1] in self._ignore:
            pass
        elif split_message[1] == "request":
            if split_message[2]:
                request = _loads(split_message[2])
                battle.parse_request(request, self.strict_battle_tracking)
                if self.format_is_bestof or not (battle.teampreview and self.accept_open_team_sheet):
                    self._request()
        elif split_message[1] == "showteam":
            from poke_env.teambuilder.teambuilder import Teambuilder

            role = split_message[2]
            teambuilder_team = Teambuilder.parse_packed_team("|".join(split_message[3:]))
            teampreview_team = battle.teampreview_team if role == battle.player_role else battle.teampreview_opponent_team
            for preview_mon in teampreview_team:
                teambuilder_mon = [
                    m for m in teambuilder_team if m.nickname is not None and preview_mon.identifies_as(m.nickname)
                ][0]
                mon = battle.get_pokemon(f"{role}: {teambuilder_mon.nickname}", details=preview_mon._last_details)
                mon._update_from_teambuilder(teambuilder_mon)
            if not self.format_is_bestof and self.accept_open_team_sheet and role == "p2":
                self._request()
        elif split_message[1] == "win" or split_message[1] == "tie":
            if split_message[1] == "win":
                battle.won_by(split_message[2])
            else:
                battle.tied()
            self.pending = None
            if self.player is not None:
                self.player._battle_finished_callback(battle)
        elif split_message[1] == "error":
            self.errors.append("|".join(split_message[2:]))
            self.logger.log(25, "Error message received: %s", "|".join(split_message))
            if split_message[2].startswith("[Unavailable choice]"):
                pass  # _trying_again.set(): the updated request that follows is answered
            elif split_message[2].startswith("[Invalid choice]"):
                self._request(maybe_default=True)
            else:
                self.logger.critical("Unexpected error message: %s", split_message)
        elif split_message[1] == "bigerror":
            self.logger.warning("Received 'bigerror' message: %s", split_message)
        else:
            battle.parse_message(split_message)

    def _request(self, maybe_default: bool = False) -> None:
        # Player._handle_battle_request: nothing to answer while waiting
        if self.battle._wait:
            self.pending = None
            return
        self.pending = Decision(maybe_default)

    # -- output ------------------------------------------------------------

    def decide(self, player: Any = None) -> str:
        """Asks ``player`` (default: the feeder's) for the pending decision,
        as ``Player._handle_battle_request`` does, and returns the message
        it would send (``"/choose move 1, switch 3"``, ``"/team 1234"``,
        ``"/forfeit"``; ``""`` for no message). Clears :attr:`pending`."""
        player = player if player is not None else self.player
        if player is None:
            raise ValueError("decide needs a poke-env Player")
        decision, self.pending = self.pending, None
        if decision is None:
            raise RuntimeError("no pending decision")
        battle = self.battle
        if decision.maybe_default and random.random() < player.DEFAULT_CHOICE_CHANCE:
            return player.choose_default_move().message
        if battle.teampreview:
            return _resolve(player.teampreview(battle)) or ""
        choice = _resolve(player.choose_move(battle))
        return choice.message


def workaround(split_message: List[str]) -> Optional[str]:
    """Rewrites, in place, a line Showdown sends that poke-env 0.16.1
    misreads, to an equivalent one it reads; returns the quirk's name, or
    None if the line is fine. docs/python/poke-env.md lists the quirks.

    * ``miss-before-spread``: Showdown appends a move's attributes in the
      order they happen, so a Magic Bounce'd spread move that misses reads
      ``|move|p2a: Espeon|Poison Gas|p1a: Hypno|[from] ability: Magic
      Bounce|[miss]|[spread] `` (Showdown 0.11.11 sends exactly this).
      ``AbstractBattle.parse_message`` looks for ``[miss]`` only as the
      last field, before it strips ``[spread]``, so the ``[miss]`` stays,
      the ``[from] ability:`` is never read, and the bounced move is added
      to the bouncer's moves (with a warning). ``[miss]`` moves to the end.
    """
    if len(split_message) > 5 and split_message[1] == "move" and "[miss]" in split_message:
        i = split_message.index("[miss]")
        if i < len(split_message) - 1 and any(p.startswith("[spread]") for p in split_message[i + 1 :]):
            del split_message[i]
            split_message.append("[miss]")
            return "miss-before-spread"
    return None


def choice_text(message: str) -> Optional[str]:
    """The Showdown choice in a poke-env message: ``"/choose move 1"`` ->
    ``"move 1"``, ``"/team 1234"`` -> ``"team 1234"``. ``None`` for
    ``/forfeit``; ``""`` for no choice."""
    message = message.strip()
    if message.startswith("/choose "):
        return message[len("/choose ") :].strip()
    if message == "/forfeit":
        return None
    if message.startswith("/team "):
        return "team " + message[len("/team ") :].strip()
    if message.startswith("/"):
        raise ValueError(f"not a choice: {message!r}")
    return message


def _side_index(side) -> int:
    if side in (0, "p1", "P1"):
        return 0
    if side in (1, "p2", "P2"):
        return 1
    raise ValueError(f"side must be 0, 1, 'p1' or 'p2', not {side!r}")
