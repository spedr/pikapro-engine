"""Battle handlers: what plays the battles a :class:`~pikapro.showdown.ShowdownClient` carries.

The client routes each battle room's lines to its handler and sends the
choices the handler returns. One handler serves all of a client's
concurrent battles, so it can decide for several of them in one call
(RL models prefer batches).

* :class:`BattleHandler` - the interface.
* :class:`SessionHandler` - plays through :class:`pikapro.rl.ChannelSession`:
  the room's lines become the environment's observations and masks,
  and a policy ``(obs, mask) -> action`` (or a built-in baseline) picks.
* :class:`RandomRequestHandler` - a uniformly random valid choice read
  from the request JSON in Python. For testing the networking; it knows
  nothing of the battle beyond the request.
"""

from __future__ import annotations

import asyncio
import logging
import random
from typing import Any, Callable, Dict, List, Optional, Sequence, Tuple, Union

from .. import _pikapro
from ..rl import BUILTIN_POLICIES, ChannelSeries, ChannelSession
from .protocol import to_id
from .room import BattleRoom

__all__ = ["BattleHandler", "SessionHandler", "RandomRequestHandler", "BUILTIN_POLICIES", "random_choice"]

logger = logging.getLogger("pikapro.showdown")

#: What a handler's failure is never contained as: these stop the program
#: or the task, not one battle. Anything else a handler, a policy or a
#: session raises (even a ``BaseException``) is that battle's alone.
_UNCONTAINED = (KeyboardInterrupt, SystemExit, GeneratorExit, asyncio.CancelledError)


class BattleHandler:
    """Plays a client's battles. Subclass it and override :meth:`feed` and
    :meth:`choose`; the other methods have defaults.

    The client calls, for each battle room, :meth:`open` once, then
    :meth:`feed` with every frame's lines, and :meth:`choose` (batched
    over the rooms waiting) when the room has a request to answer and
    :meth:`feed` said the handler can act. :meth:`error` sees each
    ``|error|`` line, and :meth:`close` is called when the room ends or is
    left. A handler keeps per-battle state in :attr:`BattleRoom.state`,
    not by :attr:`BattleRoom.id`: Showdown renames a battle room when a
    player changes its privacy, and the room (the same object) goes on
    under its new id.

    A failure stays in its battle: if :meth:`open` or :meth:`feed` raises,
    the client forfeits that battle; if :meth:`choose` raises or doesn't
    return one entry per room, the client asks again room by room, and
    sends ``default`` for a room whose own call fails. Before starting a
    game, the client asks :meth:`supports`.
    """

    #: The most rooms :meth:`choose` is given at once (0: no limit).
    max_batch: int = 0

    def supports(self, format: str) -> bool:
        """Whether the handler can play battles in ``format`` (a Showdown
        format id). The client refuses to search or challenge in a format
        the handler can't play, and never accepts a challenge in one. The
        default: every format."""
        return True

    def open(self, room: BattleRoom) -> None:
        """A battle room started (or was joined again after a reconnect,
        with its log starting over). In a best-of series,
        :attr:`BattleRoom.series` is the series (a
        :class:`~pikapro.showdown.SeriesRoom`, whose ``state`` is the
        handler's for the whole series) and :attr:`BattleRoom.game` the
        game's number."""

    def feed(self, room: BattleRoom, lines: List[str]) -> bool:
        """New lines of the room (a whole frame, ``|request|`` and
        ``|error|`` lines included). Returns whether the handler can
        choose now (the client asks only when a request is unanswered)."""
        raise NotImplementedError

    def error(self, room: BattleRoom, message: str) -> None:
        """An ``|error|`` line (``[Invalid choice] ...``,
        ``[Unavailable choice] ...``): the client asks again after an
        invalid choice, and waits for Showdown's updated request after an
        unavailable one."""

    async def choose(self, rooms: List[BattleRoom]) -> List[Optional[str]]:
        """The choices for ``rooms``, one per room, in order: each the
        text after ``/choose `` (``"move 1 2, switch 3"``, ``"team 1234"``;
        one line), or ``None`` to leave that room's request unanswered for
        now (it is asked again when the room sends something new)."""
        raise NotImplementedError

    def close(self, room: BattleRoom) -> None:
        """The room ended or was left; drop its state."""

    def summary(self, room: BattleRoom) -> Dict[str, Any]:
        """Extra facts for the battle's :class:`~pikapro.showdown.BattleResult`
        (``extras``)."""
        return {}

    def series_end(self, series: Any) -> None:
        """A best-of series ended (``series``: its
        :class:`~pikapro.showdown.SeriesRoom`), after its games' rooms
        closed; settle what the handler keeps for the series."""


# -- the request-JSON random handler ----------------------------------------------


def _fainted(mon: dict) -> bool:
    return str(mon.get("condition", "")).endswith(" fnt")


def random_choice(request: dict, rng: random.Random) -> Optional[str]:
    """A uniformly random valid choice for a Showdown request (the JSON
    of ``|request|``), or ``None`` for a ``wait`` request.

    Team preview picks a random order (``maxChosenTeamSize`` of it in
    VGC); a forced switch picks random benched Pokémon; a move request
    picks, per active Pokémon, among its enabled moves (a random foe
    target in doubles) and, unless trapped, its switches. No gimmicks.
    """
    if request.get("wait"):
        return None
    side = request.get("side", {}).get("pokemon", [])
    if request.get("teamPreview"):
        order = list(range(1, len(side) + 1))
        rng.shuffle(order)
        size = request.get("maxChosenTeamSize") or len(order)
        return "team " + "".join(str(i) for i in order[:size])
    reviving = any(m.get("reviving") for m in side)
    chosen: set = set()

    def bench() -> List[int]:
        if reviving:  # Revival Blessing: pick a fainted Pokémon
            return [i for i, m in enumerate(side) if _fainted(m) and i not in chosen]
        return [i for i, m in enumerate(side) if not m.get("active") and not _fainted(m) and i not in chosen]

    parts = []
    if "forceSwitch" in request:
        for flag in request["forceSwitch"]:
            options = bench() if flag else []
            if not options:
                parts.append("pass")
                continue
            i = rng.choice(options)
            chosen.add(i)
            parts.append(f"switch {i + 1}")
        return ", ".join(parts)
    active = request.get("active", [])
    doubles = len(active) >= 2
    for slot, a in enumerate(active):
        mon = side[slot] if slot < len(side) else {}
        if _fainted(mon) or a.get("commanding"):
            parts.append("pass")
            continue
        options: List[Any] = []
        for j, m in enumerate(a.get("moves", [])):
            if m.get("disabled"):
                continue
            target = m.get("target")
            text = f"move {j + 1}"
            if doubles and target in ("normal", "any", "adjacentFoe"):
                text += f" {rng.choice((1, 2))}"
            elif doubles and target == "adjacentAlly":
                text += f" -{2 - slot}"
            elif doubles and target == "adjacentAllyOrSelf":
                text += f" -{slot + 1}"
            options.append(text)
        if not a.get("trapped"):
            options.extend(("switch", i) for i in bench())
        if not options:
            parts.append("default")
            continue
        pick = rng.choice(options)
        if isinstance(pick, tuple):
            chosen.add(pick[1])
            pick = f"switch {pick[1] + 1}"
        parts.append(pick)
    if not parts:
        return "default"
    return ", ".join(parts)


class RandomRequestHandler(BattleHandler):
    """Answers every request with :func:`random_choice`, reading only the
    request JSON. For testing the networking (it needs neither the engine
    nor a session), and for formats pikapro doesn't simulate."""

    def __init__(self, seed: Optional[int] = None):
        self.rng = random.Random(seed)

    def feed(self, room: BattleRoom, lines: List[str]) -> bool:
        """Can act on any request but a ``wait``."""
        return room.request is not None and not room.request.get("wait")

    async def choose(self, rooms: List[BattleRoom]) -> List[str]:
        """:func:`random_choice` for each room's request."""
        return [random_choice(r.request or {}, self.rng) or "default" for r in rooms]


# -- the ChannelSession handler ------------------------------------------------------


def _stack(values: Sequence[Any]) -> Any:
    import numpy as np

    first = values[0]
    if isinstance(first, dict):
        return {k: _stack([v[k] for v in values]) for k in first}
    if isinstance(first, tuple):
        return tuple(_stack([v[i] for v in values]) for i in range(len(first)))
    return np.stack([np.asarray(v) for v in values])


def _unstack(actions: Any, n: int) -> List[Any]:
    if len(actions) != n:
        raise ValueError(f"the policy returned {len(actions)} actions for {n} battles")
    if isinstance(actions, (list, tuple)):
        return list(actions)
    return [actions[i] for i in range(n)]


#: A policy call that raised (for one battle).
_FAILED = object()


def _deciding(room: BattleRoom) -> bool:
    """The room's session waits for a decision."""
    s = room.state
    return s is not None and bool(s.needs_action) and not s.finished


def _room_choice(room: BattleRoom, choose: Callable[[Any], str]) -> str:
    """``choose(session)``, or ``"default"`` (logged) when the session
    refuses."""
    try:
        return choose(room.state)
    except _UNCONTAINED:
        raise
    except BaseException as e:
        logger.error("no choice for %s: %r", room.id, e)
        return "default"


Policy = Union[str, Callable[..., Any]]

class SessionHandler(BattleHandler):
    """Plays each battle through a :class:`pikapro.rl.ChannelSession`.

    The session reads the room's lines as the player's channel and gives
    the environment's observation (``obs``) and mask for each decision;
    the policy picks an action and the session turns it into the choice
    text. So a policy trained in :class:`pikapro.rl.VecEnv` plays on a
    Showdown server.

    ``policy``:

    * a built-in name (:data:`BUILTIN_POLICIES`: ``"random"``,
      ``"max_base_power"``, ``"simple_heuristics"``), played by the
      session (``choose_builtin``);
    * a callable ``policy(obs, mask) -> action``, called once per decision;
    * with ``batch=True``, a callable ``policy(obs_batch, mask_batch) ->
      actions`` called once for all the battles waiting at that moment:
      observations and masks are stacked on a new axis 0 (dicts and tuples
      of arrays item by item), and ``actions[i]`` is battle ``i``'s.

    ``observation``, ``masks`` and ``switch_by`` configure the session as
    they configure ``VecEnv``. ``threaded`` runs the policy call in a
    worker thread (the default for callables) so the connection keeps
    answering pings while a model thinks. ``seed`` seeds the built-in
    policies (decision ``k`` uses ``seed + k``). ``team`` is the team the
    client plays (as it sends it: export or packed format), for the
    sessions' own rows (natures, the Pokémon left out at team preview), as
    ``VecEnv`` has them; by default each battle's session gets the team
    this client uploaded for it (:attr:`BattleRoom.team`), so only a
    battle the client didn't start (an unsolicited challenge accepted by
    another client) needs it. ``session_factory`` replaces
    ``ChannelSession`` (tests).

    A best-of series (``…bo3``) is played as :class:`pikapro.rl.VecEnv`'s
    ``series`` mode plays one: the handler keeps a
    :class:`pikapro.rl.ChannelSeries` per series (in
    :attr:`SeriesRoom.state`), makes each game's session with it (the
    score, the game, the earlier games' public records), and records each
    game's session into it when the game ends. A game's result's
    ``extras`` has the session's ``series_row`` (the environment's
    ``series`` row for this player). A series the server ends early (a
    forfeit in the series room, a game lost by disconnection) is settled
    as the series room says: the ``ChannelSeries`` (:meth:`series_end`)
    and a game's session still open (its ``series_row``).
    """

    def __init__(
        self,
        policy: Policy = "random",
        *,
        batch: bool = False,
        max_batch: int = 0,
        observation: str = "basic",
        masks: str = "factored",
        switch_by: str = "row",
        threaded: Optional[bool] = None,
        seed: int = 0,
        team: Optional[str] = None,
        session_factory: Optional[Callable[..., Any]] = None,
    ):
        if isinstance(policy, str) and policy not in BUILTIN_POLICIES:
            raise ValueError(f"unknown policy {policy!r}: expected a callable or one of {BUILTIN_POLICIES}")
        if not isinstance(policy, str) and not callable(policy):
            raise TypeError("policy must be a callable or a built-in policy name")
        self.policy = policy
        self.batch = batch
        self.max_batch = max_batch
        self.options: Dict[str, Any] = {"observation": observation, "masks": masks, "switch_by": switch_by}
        if team is not None:
            self.options["team"] = team
        self.threaded = (not isinstance(policy, str)) if threaded is None else threaded
        self.seed = seed
        self.decisions = 0
        self._factory = session_factory
        # Each series' side for this client, by series room (the object:
        # a room's id changes when Showdown renames it).
        self._series_sides: Dict[Any, str] = {}

    def _session_class(self) -> Callable[..., Any]:
        return self._factory if self._factory is not None else ChannelSession

    def supports(self, format: str) -> bool:
        """Whether a ``ChannelSession`` reads ``format`` (the formats in
        ``docs/python/showdown.md``; any, with a ``session_factory``)."""
        return self._factory is not None or _pikapro._showdown_preset(format) is not None

    def open(self, room: BattleRoom) -> None:
        """Starts the room's ``ChannelSession`` (in :attr:`BattleRoom.state`),
        in a series with the series' :class:`pikapro.rl.ChannelSeries`.
        Raises ``ValueError`` for a format the session can't read."""
        options = dict(self.options)
        if "team" not in options and room.team is not None and self._factory is None:
            options["team"] = room.team  # the team this client uploaded for the battle
        series = self._series(room)
        if series is not None:
            options["series"] = series
        room.state = self._session_class()(room.play_format, username=room.username, **options)

    def _series(self, room: BattleRoom) -> Any:
        """The room's series as its sessions carry it (made at its first
        game), or ``None`` outside a series (and with a ``session_factory``)."""
        owner = room.series
        if owner is None or self._factory is not None:
            return None
        if owner.state is None:
            owner.state = ChannelSeries(room.play_format, owner.best_of)
        return owner.state

    def _series_end(self, owner: Any, side: Optional[str]) -> Optional[Tuple[Optional[str], bool]]:
        """The series room's result, once it announced one, as a
        ``ChannelSeries`` settles it (``(winner side, tie)``); ``None``
        before the end or when it can't be told."""
        if owner is None or not owner.ended or side not in ("p1", "p2"):
            return None
        if owner.tie:
            return (None, True)
        if owner.winner is None:
            return None
        mine = to_id(owner.winner) == to_id(owner.username)
        return (side if mine else ("p2" if side == "p1" else "p1"), False)

    def series_end(self, series: Any) -> None:
        """Settles the series' ``ChannelSeries`` as the series room ended it
        (the client calls it when the series ends): its games' results may
        not have (a forfeit in the series room, a game lost by
        disconnection)."""
        side = self._series_sides.pop(series, None)
        end = self._series_end(series, side)
        state = series.state
        if end is None or state is None:
            return
        try:
            state.settle(winner=end[0], tie=end[1])
        except ValueError as e:
            logger.warning("series %s: not settled: %s", series.id, e)

    def _record(self, room: BattleRoom) -> None:
        """Records a finished game's session into its series (with the
        server's result when the session didn't read the end)."""
        series, session = self._series(room), room.state
        if room.series is not None and room.side in ("p1", "p2"):
            self._series_sides[room.series] = room.side
        if series is None or session is None or series.over:
            return
        winner, tie = None, False
        if not session.finished:
            if room.tie:
                tie = True
            elif room.winner is not None and room.side in ("p1", "p2"):
                mine = to_id(room.winner) == to_id(room.username)
                winner = room.side if mine else ("p2" if room.side == "p1" else "p1")
            else:
                logger.warning("series %s: game %s has no result to record", room.series.id, room.game)
                return
        try:
            series.record(session, winner=winner, tie=tie)
        except ValueError as e:
            logger.warning("series %s: game %s not recorded: %s", room.series.id, room.game, e)

    def feed(self, room: BattleRoom, lines: List[str]) -> bool:
        """Feeds the session; it can act when it ``needs_action``."""
        session = room.state
        session.feed_many(lines)
        return bool(session.needs_action) and not session.finished

    async def choose(self, rooms: List[BattleRoom]) -> List[Optional[str]]:
        """The policy's actions for the rooms' current decisions (one call
        for all of them with ``batch=True``), as the sessions' choices.

        Each room stands alone: a room with nothing to decide any more (it
        ended, or a newer request came while the policy ran) gets ``None``,
        and a room whose session refuses the action (a masked action, a
        wrong length) or whose own policy call fails gets ``"default"``. A
        batched policy call that fails raises, and the client asks again
        room by room."""
        out: List[Optional[str]] = [None] * len(rooms)
        asked = [(i, r, r.requests) for i, r in enumerate(rooms) if _deciding(r)]
        if isinstance(self.policy, str):
            name = self.policy
            for i, room, _ in asked:
                seed = self.seed + self.decisions
                out[i] = _room_choice(room, lambda s: s.choose_builtin(name, seed))
                self.decisions += 1
            return out
        obs: List[Any] = []
        masks: List[Any] = []
        ready = []
        for i, room, requests in asked:
            try:
                o, m = room.state.obs, room.state.mask
            except _UNCONTAINED:
                raise
            except BaseException as e:
                logger.error("session error in %s: %r", room.id, e)
                out[i] = "default"
                continue
            obs.append(o)
            masks.append(m)
            ready.append((i, room, requests))
        if not ready:
            return out
        policy = self.policy
        if self.batch:

            def call() -> List[Any]:
                return _unstack(policy(_stack(obs), _stack(masks)), len(obs))

        else:

            def call() -> List[Any]:
                actions: List[Any] = []
                for o, m in zip(obs, masks):
                    try:
                        actions.append(policy(o, m))
                    except _UNCONTAINED:
                        raise
                    except BaseException as e:  # this battle's alone
                        logger.error("policy error: %r", e)
                        actions.append(_FAILED)
                return actions

        actions = await asyncio.to_thread(call) if self.threaded else call()
        self.decisions += len(ready)
        for (i, room, requests), a in zip(ready, actions):
            if room.requests != requests or not _deciding(room):
                continue  # stale: the room's new decision is asked separately
            out[i] = "default" if a is _FAILED else _room_choice(room, lambda s, a=a: s.choice(a))
        return out

    def close(self, room: BattleRoom) -> None:
        """Drops the room's session (a series' game first records it in its
        series; a room closed before its end, as on a rejoin, records
        nothing)."""
        if room.series is not None and room.state is not None and (room.ended or room.state.finished):
            self._record(room)
        room.state = None

    def summary(self, room: BattleRoom) -> Dict[str, Any]:
        """``unknown_lines`` (how many lines the session kept as unread),
        ``unknown_sample`` (the first five), ``session_winner`` and, in a
        series, ``series_row``."""
        session = room.state
        if session is None:
            return {}
        end = self._series_end(room.series, room.side)
        if end is not None:
            try:
                session.settle_series(winner=end[0], tie=end[1])
            except ValueError:
                pass  # the games settled it otherwise: the session's row stands
        unknown = list(session.unknown_lines or [])
        out = {"unknown_lines": len(unknown), "unknown_sample": unknown[:5], "session_winner": session.winner}
        row = session.series_row
        if row is not None:
            out["series_row"] = [int(x) for x in row]
        return out

