"""One battle driven by Showdown's text: :class:`ChannelSession`.

A session reads what Showdown sends a player (its channel: the battle's
lines and its ``|request|`` messages, from a websocket or
``getPlayerStreams``) and gives, at each decision, what
:class:`pikapro.rl.VecEnv` gives that player: the view, the observation
and the mask (same layout and ``switch_by`` rows). An action chosen from
them becomes the text Showdown's ``/choose`` takes. So a policy trained in
the environment plays on a Showdown server (see :mod:`pikapro.showdown`).

A best-of series (Showdown's ``…bo3`` formats) is one session per game,
each given the :class:`ChannelSeries` the games so far were recorded in,
as :class:`pikapro.rl.VecEnv`'s ``series`` mode plays one.
"""

from __future__ import annotations

from typing import Any, Dict, Iterable, List, Optional, Tuple

import numpy as np

from .. import _pikapro
from .view import BattleView

__all__ = ["ChannelSeries", "ChannelSession"]


class ChannelSeries:
    """A best-of series played on Showdown, one :class:`ChannelSession` per
    game, as :class:`pikapro.rl.VecEnv`'s ``series`` mode plays it: the
    same two teams on the same sides every game, with open sheets.

    ``format_id`` is the series' Showdown format
    (``"gen9championsvgc2026regmcbo3"``) or its preset; ``best_of`` the
    games (default: the format's, else 3). Each game's session is made with
    ``series=`` this object, which gives it the score, the game number and
    the earlier games' public records (what the environment's
    ``PlayerView.series`` holds; the built-in encoders encode each game
    alone, so the observation is a single game's with open sheets). When
    the game ends, :meth:`record` its session: the series moves on to the
    next game, or is settled.

    ::

        series = ChannelSeries("gen9championsvgc2026regmcbo3")
        while not series.over:
            session = ChannelSession("gen9championsvgc2026regmcbo3", username=me, series=series)
            ...  # feed the game's battle room, choose
            series.record(session)
        series.result  # "p1", "p2" or "tie"

    Wins are by side (Showdown keeps each player's side through a series);
    :attr:`ChannelSession.series_row` gives them from the player's side.
    """

    def __init__(self, format_id: str, best_of: Optional[int] = None):
        self._s = _pikapro._ChannelSeries(format_id, best_of)
        self.format_id = format_id

    @property
    def best_of(self) -> int:
        """The series' length (3, 5, 7 or 9 games)."""
        return self._s.best_of

    @property
    def game(self) -> int:
        """The game to play next (1-based); once the series is over, its
        last game."""
        return self._s.game

    @property
    def wins(self) -> Tuple[int, int]:
        """Games won by p1 and by p2."""
        return self._s.wins

    @property
    def ties(self) -> int:
        """Games tied."""
        return self._s.ties

    @property
    def result(self) -> Optional[str]:
        """``"p1"`` or ``"p2"`` (the series' winner), ``"tie"``, or ``None``
        while it goes on. A player wins at ``(best_of - ties) // 2 + 1``
        wins, as Showdown's ``BestOfGame`` counts."""
        return self._s.result

    @property
    def over(self) -> bool:
        """The series is settled."""
        return self._s.over

    @property
    def recorded(self) -> int:
        """Games recorded so far."""
        return self._s.recorded

    def record(self, session: "ChannelSession", winner: Optional[str] = None, tie: bool = False) -> None:
        """Records a game: the session's result and its public record at
        the end, then moves to the next game unless the series is over. A
        session that didn't read the game's end (the room closed first)
        needs the result the server gave: ``winner`` (``"p1"``/``"p2"``) or
        ``tie=True``; so does one whose ``|win|`` named neither player.
        ``ValueError`` for a session of another game, or once the series is
        over."""
        self._s.record(session._s, winner, tie)

    def settle(self, winner: Optional[str] = None, tie: bool = False) -> None:
        """Ends the series as the server did (the series room's ``|win|`` or
        ``|tie``), whatever the games' score: ``winner`` (``"p1"``/``"p2"``)
        or ``tie=True``. Showdown ends a series early when a player forfeits
        it, loses a game by disconnection, or stays away between games; the
        game's room shows only the game's ``|win|``. A no-op for a series
        already settled so; ``ValueError`` for one settled otherwise."""
        self._s.settle(winner, tie)

    def __repr__(self) -> str:
        return repr(self._s)


class ChannelSession:
    """One battle, as one player sees it, read from Showdown's text.

    ``format_id`` is the Showdown format (``"gen9championsvgc2026regmc"``,
    ``"gen9vgc2025regi"``, ``"gen9randombattle"``, ...; a custom game's
    rules after ``@@@``) or a pikapro preset id (``"championsvgc"``, ...);
    ``ValueError`` for one it can't read exactly. ``observation``,
    ``masks`` and ``switch_by`` are :class:`pikapro.rl.VecEnv`'s options of
    the same names, and ``hp`` its ``hp`` (``"exact"`` only where the
    channel shows exact HP: custom games). ``username`` is the player's
    Showdown name: its ``|player|`` line says which side is its own
    (without it, the first request does). ``team`` is the team the player
    plays (Showdown's export or packed format): what no request shows
    comes from it, the natures and, once the battle started, the Pokémon
    left out at team preview, as in :class:`pikapro.rl.VecEnv`. Without it,
    the session knows what the requests showed (not the natures).
    ``series`` makes the battle a game of a best-of series
    (:class:`ChannelSeries`, at the game it plays next).

    Feed it every line the battle's room sends (:meth:`feed`,
    :meth:`feed_many`): room and chat lines are ignored, lines it doesn't
    know are kept in :attr:`unknown_lines`. When :attr:`needs_action`,
    read :attr:`obs` and :attr:`mask`, choose an action as for the
    environment, and send ``/choose`` with :meth:`choice`'s text (or let a
    built-in policy choose: :meth:`choose_builtin`).

    Before the battle's first request, its lines are held: the request
    names the player's own Pokémon, which the lines must be read with.
    """

    def __init__(
        self,
        format_id: str,
        *,
        observation: str = "basic",
        masks: str = "factored",
        switch_by: str = "row",
        username: Optional[str] = None,
        hp: str = "percent",
        team: Optional[str] = None,
        series: Optional[ChannelSeries] = None,
    ):
        self._s = _pikapro._ChannelSession(
            format_id,
            observation=observation,
            masks=masks,
            switch_by=switch_by,
            username=username,
            hp=hp,
            team=team,
            series=None if series is None else series._s,
        )
        self.format_id = format_id
        self.masks = masks
        self.switch_by = switch_by
        self.username = username

    # -- feeding ---------------------------------------------------------------

    def feed(self, line: str) -> None:
        """Feeds one line of the channel (or several joined by ``\\n``)."""
        self._s.feed(line)

    def feed_many(self, lines: Iterable[str]) -> None:
        """Feeds lines in order (each may hold several joined by ``\\n``)."""
        self._s.feed_many([str(line) for line in lines])

    # -- state -----------------------------------------------------------------

    @property
    def needs_action(self) -> bool:
        """The player must choose now (a request that asks for a choice,
        not yet answered)."""
        return self._s.needs_action

    @property
    def request_kind(self) -> str:
        """``"none"`` (no request yet), ``"wait"``, ``"teampreview"``,
        ``"move"`` or ``"switch"`` (a forced switch)."""
        return self._s.request_kind

    @property
    def finished(self) -> bool:
        """``|win|`` or ``|tie|`` was read."""
        return self._s.finished

    @property
    def winner(self) -> Optional[str]:
        """The winner's name as the channel gives it (``None`` for a tie or
        before the end)."""
        return self._s.winner

    @property
    def winner_side(self) -> Optional[str]:
        """The winner's side, ``"p1"`` or ``"p2"``."""
        return self._s.winner_side

    @property
    def side(self) -> Optional[str]:
        """The player's side, ``"p1"`` or ``"p2"``, once known."""
        return self._s.side

    @property
    def format(self) -> str:
        """The pikapro format the battle is read as (``"championsvgc"``, ...)."""
        return self._s.format

    @property
    def series_row(self) -> Optional[np.ndarray]:
        """In a series, :class:`pikapro.rl.VecEnv`'s ``series`` row for this
        player (as the environment's player 0): ``(4,)`` uint8, the game,
        the player's wins, the opponent's, and the series' result (0 going
        on, 1 this player won it, 2 the opponent did, 3 a tie), with this
        game's result once it ended. ``None`` outside a series."""
        row = self._s.series_row
        return None if row is None else np.asarray(row, dtype=np.uint8)

    @property
    def turn(self) -> int:
        """The last ``|turn|`` read (0 before the first)."""
        return self._s.turn

    @property
    def unknown_lines(self) -> List[str]:
        """Lines this session didn't know (the first 1000; their number is
        :attr:`unknown_count`)."""
        return self._s.unknown_lines

    @property
    def unknown_count(self) -> int:
        """How many lines this session didn't know."""
        return self._s.unknown_count

    @property
    def unknown_names(self) -> int:
        """Species, moves, items and abilities the format's dex didn't know
        (a newer server's), each read as a stand-in."""
        return self._s.unknown_names

    @property
    def error(self) -> Optional[str]:
        """Why the last unreadable request couldn't be read, if any."""
        return self._s.error

    @property
    def observation(self) -> Dict[str, Any]:
        """What :attr:`obs` holds, as :attr:`pikapro.rl.VecEnv.observation`."""
        return self._s.observation

    @property
    def mask_width(self) -> int:
        """The mask's length (:attr:`pikapro.rl.VecEnv.mask_width`)."""
        return self._s.mask_width

    @property
    def actions_per_player(self) -> int:
        """Values in an action: the active slots with factored masks, 1
        with joint masks."""
        return self._s.actions_per_player

    # -- the decision ----------------------------------------------------------

    @property
    def obs(self) -> np.ndarray:
        """The observation, as one player's row of :class:`pikapro.rl.VecEnv`'s
        ``obs``: a float32 array. A fresh copy."""
        return self._s.obs()

    @property
    def mask(self) -> np.ndarray:
        """The mask of the current request (``(mask_width,)`` bool), as the
        environment's mask row for this player. It stays valid until the
        next request, answered or not (a choice Showdown refused as
        invalid is made again from it); all false outside a decision."""
        return self._s.mask()

    @property
    def min_switches(self) -> int:
        """The fewest switches a joint action must make (the environment's
        ``min_switches``)."""
        return self._s.min_switches

    @property
    def view(self) -> BattleView:
        """The player's view as a :class:`pikapro.rl.BattleView`, with the
        mask (and in a series, its ``series``)."""
        row = self._s.series_row
        series = None
        if row is not None:
            game, wins, theirs, result = row
            series = {"game": game, "wins": wins, "opponent_wins": theirs, "result": result}
        return BattleView(
            self._s._view(),
            self.format,
            mask=self._s.mask(),
            masks=self.masks,
            switch_by=self.switch_by,
            player_role=self.side,
            series=series,
        )

    def choice(self, action) -> str:
        """Showdown's ``/choose`` text (what follows ``/choose ``) for an
        action, as :meth:`pikapro.rl.VecEnv.step` takes one player's: the
        slot-action indices with factored masks (at team preview, the
        order's index first), one choice index with joint masks. Switches
        name rows or positions as ``switch_by`` says. ``actions_per_player``
        values (at team preview, the order's index alone may be given).
        ``ValueError`` for another number of values, or if the mask doesn't
        allow the action."""
        acts = list(np.asarray(action, dtype=object).reshape(-1))
        per = self.actions_per_player
        if len(acts) == 1 and per > 1 and self.request_kind == "teampreview":
            acts += [0] * (per - 1)
        if len(acts) != per:
            raise ValueError(f"an action here is {per} values, got {len(acts)}")
        return self._s.choice(acts)

    def choose_builtin(self, name: str, seed: int = 0) -> str:
        """A built-in policy's choice (``"random"``, ``"max_base_power"``,
        ``"simple_heuristics"``: the environment's opponents), as ``/choose``
        text; ``seed`` seeds its randomness."""
        return self._s.choose_builtin(name, seed)

    def settle_series(self, winner: Optional[str] = None, tie: bool = False) -> None:
        """The series ended as the server says (the series room's ``|win|``
        or ``|tie``: ``winner`` ``"p1"``/``"p2"``, or ``tie=True``), whatever
        this game showed: :attr:`series_row` shows it settled.
        ``ValueError`` outside a series, or for one the games settled
        otherwise."""
        self._s.settle_series(winner, tie)

    def __repr__(self) -> str:
        return repr(self._s)
