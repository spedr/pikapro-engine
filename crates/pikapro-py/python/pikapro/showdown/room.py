"""A battle room as the client tracks it, and a finished battle's result;
a best-of series' room and its result."""

from __future__ import annotations

import asyncio
import html
import json
import re
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional, Tuple

from .protocol import _room_closed, battle_format, request_rqid, series_room_format, to_id

__all__ = ["BattleRoom", "BattleResult", "SeriesRoom", "SeriesResult"]

# A series game's battle room names its series (``BestOfGame.nextGame``):
# ``|uhtml|bestof|<h2><strong>Game N</strong> of <a href="/game-bestof3-…">``.
# ``|uhtmlchange|`` rewrites it when the series' privacy changes its rooms'
# ids (``setPrivacyOfGames``).
_SERIES_LINK = re.compile(
    r'^\|uhtml(?:change)?\|bestof\|.*?<strong>Game (\d+)</strong> of <a href="/(game-bestof\d+-[a-z0-9-]+)"'
)
# The series room names each game's battle room: ``|uhtml|gameN|<a href="/battle-…">``.
_GAME_LINK = re.compile(r'^\|uhtml(?:change)?\|game(\d+)\|<a href="/(battle-[a-z0-9-]+)"')
# ``onBattleWin``: ``|html|NAME won game N!``, ``|html|Game N was a tie.``
_GAME_WON = re.compile(r"^\|html\|(.+) won game (\d+)!$")
_GAME_TIE = re.compile(r"^\|html\|Game (\d+) was a tie\.$")
# ``updateReadyButton``: the "I'm ready!" button, enabled (``notifying``).
_READY = re.compile(r"Are you ready for game (\d+).*<button class=\"button notifying\"[^>]*/confirmready")
# A player's answer to the Open Team Sheets offer, a plain line (no ``|``):
# ``/acceptopenteamsheets`` adds ``NAME has agreed to open team sheets.``,
# ``/rejectopenteamsheets`` ``NAME rejected open team sheets.``
# (``server/chat-commands/core.ts``).
_OTS_ANSWER = re.compile(r"^(?P<name>[^|].*?) (?P<what>has agreed to|rejected) open team sheets\.$")

#: Showdown's player symbol (``Users.PLAYER_SYMBOL``): a game room's
#: players join it as ``|j|☆NAME``.
PLAYER_SYMBOL = "☆"


def title_opponent(title: str, user_id: str) -> Optional[str]:
    """The opponent a room title names (``"P1 vs. P2"``, Showdown's
    ``createBattle``), read against the player's own id: a name may contain
    " vs. " itself, so the title is split at each " vs. " and the split
    that leaves the player on one side names the opponent. ``None`` when no
    split does, or more than one opponent would fit."""
    parts = title.split(" vs. ")
    found = set()
    for i in range(1, len(parts)):
        left, right = " vs. ".join(parts[:i]), " vs. ".join(parts[i:])
        if to_id(left) == user_id and to_id(right) != user_id:
            found.add(right)
        if to_id(right) == user_id and to_id(left) != user_id:
            found.add(left)
    return found.pop() if len(found) == 1 else None


def joined_player(line: str, user_id: str) -> Optional[str]:
    """The other player a ``|j|☆NAME`` line names (a player of the room's
    game joining it), else ``None``."""
    kind, _, user = line[1:].partition("|")
    if kind not in ("j", "J") or not user.startswith(PLAYER_SYMBOL):
        return None
    name = user[1:]
    if name.endswith("@!"):  # away
        name = name[:-2]
    return name if name and to_id(name) != user_id else None


#: The most lines a room keeps (its saved log); a battle has far fewer
#: (Showdown ends one at turn 1000). Beyond it, lines still reach the
#: handler but aren't kept.
MAX_LINES = 200_000


@dataclass
class BattleResult:
    """How one battle ended, from the client's player's side."""

    #: The battle room id (``battle-gen9championsvgc2026regmc-42``).
    room: str
    #: The format id.
    format: str
    #: The opponent's name (``None`` if the room never named one).
    opponent: Optional[str]
    #: ``True`` won, ``False`` lost, ``None`` a tie or no result.
    won: Optional[bool]
    #: The winner's name as the room announced it (``|win|``), if any.
    winner: Optional[str]
    #: The battle ended in a tie (``|tie|``).
    tie: bool
    #: The battle ended with a winner or a tie (not left, crashed or
    #: abandoned when the client closed).
    finished: bool
    #: The last ``|turn|`` number.
    turns: int
    #: The player's side, ``"p1"`` or ``"p2"``.
    side: Optional[str]
    #: ``[Invalid choice]`` errors the room sent.
    invalid: int
    #: ``[Unavailable choice]`` errors the room sent.
    unavailable: int
    #: Choices the client sent as ``default`` after repeated invalid ones.
    fallbacks: int
    #: Where the room's log was saved, if the client saves logs.
    log_path: Optional[str] = None
    #: Why the battle ended without a result (``"forfeited on exit"``,
    #: ``"room closed"``, ``"disconnected"``...), else ``None``.
    reason: Optional[str] = None
    #: What the handler adds (:meth:`BattleHandler.summary`).
    extras: Dict[str, Any] = field(default_factory=dict)
    #: The series room of a best-of series' game, and the game's number.
    series: Optional[str] = None
    game: Optional[int] = None
    #: Choices the room rejected as stale (``[Invalid choice]`` for a
    #: request already answered or gone: an outdated rqid, the open request
    #: sent again after a reconnect); not in ``invalid``.
    stale: int = 0

    def to_dict(self) -> Dict[str, Any]:
        """The result as a JSON-ready dict."""
        return dict(self.__dict__)


@dataclass
class SeriesResult:
    """How a best-of series ended, from the client's player's side (what
    :meth:`ShowdownClient.ladder` and the others return in a series
    format: one per series, with each game's :class:`BattleResult`)."""

    #: The series room id (``game-bestof3-gen9championsvgc2026regmcbo3-7``).
    room: str
    #: The format id.
    format: str
    #: The opponent's name.
    opponent: Optional[str]
    #: ``True`` won the series, ``False`` lost it, ``None`` a tie or no result.
    won: Optional[bool]
    #: The series winner's name (``|win|`` in the series room), if any.
    winner: Optional[str]
    #: The series ended in a tie.
    tie: bool
    #: The series ended with a winner or a tie.
    finished: bool
    #: Games in the series (3 for a Bo3).
    best_of: int
    #: Games won, lost and tied, as the series room announced them.
    wins: int
    losses: int
    ties: int
    #: Each game's result, in order.
    games: List[BattleResult]
    #: The player's side (the same in every game).
    side: Optional[str]
    #: The games' ``[Invalid choice]`` and ``[Unavailable choice]`` errors
    #: and ``default`` fallbacks, summed.
    invalid: int
    unavailable: int
    fallbacks: int
    #: The games' turns, summed.
    turns: int
    #: Where the series room's log was saved, if the client saves logs.
    log_path: Optional[str] = None
    #: Why the series ended without a result, else ``None``.
    reason: Optional[str] = None
    #: The games' handler extras, summed where they are counts
    #: (``unknown_lines``).
    extras: Dict[str, Any] = field(default_factory=dict)
    #: The games' stale choices (:attr:`BattleResult.stale`), summed.
    stale: int = 0

    def to_dict(self) -> Dict[str, Any]:
        """The result as a JSON-ready dict (the games' too)."""
        out = dict(self.__dict__)
        out["games"] = [g.to_dict() for g in self.games]
        return out


class BattleRoom:
    """One battle room the client plays in.

    Handlers read it and keep their per-battle state in :attr:`state`.
    Attributes:

    * ``id`` - the room id; ``format`` - the format id (from the room id);
      ``username`` - the player's name when the room opened.
    * ``lines`` - every line the room sent, in order (the saved log; at
      most :data:`MAX_LINES`).
    * ``side`` - ``"p1"`` / ``"p2"`` once the room's ``|player|`` lines
      name the client; ``opponent`` - the opponent's name (``|player|`` or
      ``|title|``).
    * ``request`` - the latest request (the parsed ``|request|`` JSON),
      ``None`` before one; ``rqid`` - its ``rqid`` if the server sends
      them; ``requests`` - requests received (new ones, or one sent again).
    * ``turn`` - the last ``|turn|``.
    * ``ended``, ``winner``, ``tie`` - set by ``|win|`` / ``|tie|``.
    * ``invalid``, ``unavailable`` - the choice errors the room sent;
      ``stale`` - the choices it rejected as stale (for a request already
      answered or gone; not in ``invalid``); ``fallbacks`` - ``default``
      choices the client sent after repeated invalid ones.
    * ``state`` - the handler's.
    * ``ots_offered`` - the room offered Open Team Sheets
      (``|uhtml|otsrequest|``); ``ots_foe`` / ``ots_answer`` - the
      opponent's answer and the client's (``True`` accepted, ``False``
      rejected, ``None`` none yet; the client's as sent, or as the room
      announced it);
      ``ots_open`` - the sheets were shown (``|showteam|``);
      ``ots_wait_ms`` - how long the client held the team-preview decision
      for the opponent's answer (``open_team_sheets="mutual"``), else
      ``None``.
    """

    def __init__(self, id: str, username: str):
        self.id = id
        self.username = username
        self.format = battle_format(id)
        self._play_format: Optional[str] = None
        #: The team this client uploaded for the battle (packed), when an
        #: operation of this client started it; ``None`` for a random battle
        #: or a battle it didn't start. ``SessionHandler`` gives it to the
        #: session, which fills the player's own rows from it.
        self.team: Optional[str] = None
        self.lines: List[str] = []
        self.side: Optional[str] = None
        self.opponent: Optional[str] = None
        self.request: Optional[Dict[str, Any]] = None
        self.rqid: Optional[int] = None
        self.requests = 0
        self.turn = 0
        self.state: Any = None
        self.ended = False
        self.winner: Optional[str] = None
        self.tie = False
        self.invalid = 0
        self.stale = 0
        self.unavailable = 0
        self.fallbacks = 0
        # The client's bookkeeping: the request count when it last sent a
        # choice, invalid choices in a row for the current request, whether
        # to ask again (after an invalid one), whether an operation claimed
        # the room, and whether it waits for the decision loop.
        self._answered = 0
        self._retries = 0
        self._retry = False
        self._claimed = False
        self._queued = False
        # When the battle ends (its result), why the handler gave it up,
        # and whether the choice being sent is a fallback.
        self._end: Optional["asyncio.Future[BattleResult]"] = None
        self._error: Optional[str] = None
        self._fallback = False
        # When the client first read the room (``time.monotonic()``; kept
        # when a reconnect sends it again); since when the server's
        # ``|updatesearch|`` hasn't listed it, with no frame since (None:
        # listed, or a frame came).
        self._opened = 0.0
        self._unlisted: Optional[float] = None
        #: The series this battle is a game of (a best-of format), once the
        #: client linked it; ``game`` is its number. ``series_id`` is the
        #: series room the battle room names.
        self.series: Optional["SeriesRoom"] = None
        self.series_id: Optional[str] = None
        self.game: Optional[int] = None
        # The battle has started (a ``|player|`` line or a request came):
        # a series game names its series before that. Whether the client
        # answered the Open Team Sheets offer.
        self._started = False
        self._ots_answered = False
        self.ots_offered = False
        self.ots_foe: Optional[bool] = None
        self.ots_answer: Optional[bool] = None
        self.ots_open = False
        self.ots_wait_ms: Optional[float] = None
        # Mutual Open Team Sheets (the client's): the decision is held
        # (``_ots_hold``) since ``_ots_since`` (monotonic), a held decision
        # waits (``_ots_deferred``), and the pending timer.
        self._ots_hold = False
        self._ots_since: Optional[float] = None
        self._ots_deferred = False
        self._ots_timer: Optional[asyncio.TimerHandle] = None
        # The room's own answer to the offer, from the operation that
        # started it (``AcceptPlan.open_team_sheets``); ``None``: the
        # client's ``open_team_sheets``.
        self._ots_mode: Optional[bool] = None

    @property
    def opened(self) -> float:
        """When the client first read the room (``time.monotonic()``; kept
        when a reconnect sends it again); 0 for a room never read."""
        return self._opened

    @property
    def play_format(self) -> str:
        """The format the handler plays: :attr:`format`, or what the
        server's ``ServerConfig.format_aliases`` maps it to
        (pikapro-showdown-server's custom games are pikapro's
        ``gen9singles`` / ``gen9doubles``)."""
        return self._play_format or self.format

    @play_format.setter
    def play_format(self, value: str) -> None:
        self._play_format = value

    def __repr__(self) -> str:
        return f"BattleRoom({self.id!r}, side={self.side!r}, opponent={self.opponent!r}, turn={self.turn})"

    def observe(self, line: str, user_id: str) -> None:
        """Updates the room's own bookkeeping from one line."""
        if len(self.lines) < MAX_LINES:
            self.lines.append(line)
        if line.startswith("|request|"):
            payload = line[len("|request|") :]
            if not payload:
                return
            try:
                request = json.loads(payload)
            except (ValueError, RecursionError):
                return
            if isinstance(request, dict):
                self._started = True
                self.request = request
                self.rqid = request_rqid(line)
                self.requests += 1
                self._retry = False
                self._retries = 0
        elif line.startswith("|sentchoice|"):
            # Showdown's rejoin (``RoomBattle.onConnect``): the request sent
            # again already has the client's choice (made before the connection
            # dropped). It is answered: nothing to send.
            if self.request is not None:
                self._answered = self.requests
                self._retry = False
        elif line.startswith(("|uhtml|bestof|", "|uhtmlchange|bestof|")):
            m = _SERIES_LINK.match(line)
            if m:
                self.game = int(m.group(1))
                self.series_id = m.group(2)
        elif line.startswith("|player|"):
            self._started = True
            parts = line.split("|")
            if len(parts) >= 4 and parts[3]:
                if to_id(parts[3]) == user_id:
                    self.side = parts[2]
                else:
                    self.opponent = parts[3]
        elif line.startswith("|title|") and self.opponent is None:
            self.opponent = title_opponent(line[len("|title|") :], user_id)
        elif line.startswith("|turn|"):
            try:
                self.turn = int(line.split("|")[2])
            except (IndexError, ValueError):
                pass
        elif line.startswith("|win|"):
            self.ended = True
            self.winner = line[len("|win|") :]
        elif line == "|tie" or line.startswith("|tie|"):
            self.ended = True
            self.tie = True
        elif line.startswith("|uhtml|otsrequest|") and "/acceptopenteamsheets" in line:
            self.ots_offered = True
        elif line.startswith("|showteam|"):
            self.ots_open = True
        elif line and line[0] != "|":
            m = _OTS_ANSWER.match(line)
            if m:
                accepted = m.group("what") == "has agreed to"
                who = to_id(m.group("name"))
                if who == user_id:
                    self.ots_answer = accepted
                elif self.opponent is None or who == to_id(self.opponent):
                    self.ots_foe = accepted

    @property
    def pending(self) -> bool:
        """A request (or a retry after an invalid choice) is unanswered."""
        return not self.ended and self.request is not None and (self.requests > self._answered or self._retry)

    def result(self, user_id: str, log_path: Optional[str] = None, reason: Optional[str] = None) -> BattleResult:
        """The room's :class:`BattleResult`."""
        finished = self.winner is not None or self.tie
        won = None if self.winner is None else to_id(self.winner) == user_id
        return BattleResult(
            room=self.id,
            format=self.format,
            opponent=self.opponent,
            won=won,
            winner=self.winner,
            tie=self.tie,
            finished=finished,
            turns=self.turn,
            side=self.side,
            invalid=self.invalid,
            unavailable=self.unavailable,
            fallbacks=self.fallbacks,
            log_path=log_path,
            reason=None if finished else reason,
            series=self.series.id if self.series is not None else self.series_id,
            game=self.game,
            stale=self.stale,
        )


class SeriesRoom:
    """A best-of series the client plays: Showdown's series room
    (``game-bestof3-FORMAT-NUM``, a ``BestOfGame``), whose games are
    battle rooms of their own (``battle-FORMAT-NUM``), one after the other,
    between the same two players with the same teams on the same sides.

    Handlers see it as :attr:`BattleRoom.series` in each game's room, and
    keep their per-series state in :attr:`state` (``SessionHandler`` keeps
    the :class:`pikapro.rl.ChannelSeries` there). Attributes:

    * ``id``, ``format`` (from the id), ``best_of``; ``opponent``.
    * ``games`` - the battle room of each game, by number; ``results`` -
      each finished game's :class:`BattleResult`, by number;
      ``game_winners`` - each game's winner as the series room announced it
      (``None``: a tie).
    * ``ended``, ``winner``, ``tie`` - set by the series room's
      ``|win|`` / ``|tie``.
    * ``lines`` - the series room's lines (its log).
    """

    def __init__(self, id: str, username: str):
        self.id = id
        self.username = username
        fmt, best_of = series_room_format(id)
        self.format = fmt
        self.best_of = best_of or 3
        self.lines: List[str] = []
        self.opponent: Optional[str] = None
        self.games: Dict[int, str] = {}
        self.results: Dict[int, BattleResult] = {}
        self.game_winners: Dict[int, Optional[str]] = {}
        self.ended = False
        self.winner: Optional[str] = None
        self.tie = False
        self.state: Any = None
        #: The team uploaded for it (every game plays it), when an
        #: operation of this client started it.
        self.team: Optional[str] = None
        self._claimed = False
        self._end: Optional["asyncio.Future[SeriesResult]"] = None
        # Games whose "ready" prompt was answered.
        self._ready: set = set()

    def __repr__(self) -> str:
        return f"SeriesRoom({self.id!r}, opponent={self.opponent!r}, games={len(self.games)}, ended={self.ended})"

    def observe(self, line: str, user_id: str) -> Optional[Tuple[Any, ...]]:
        """Reads one line of the series room: ``("game", n, battle_room)``
        when a game starts, ``("ready", n)`` when Showdown asks whether
        the player is ready for game ``n``, ``("end",)`` at the series'
        end, ``("closed", why)`` when the room is gone, else ``None``."""
        if len(self.lines) < MAX_LINES:
            self.lines.append(line)
        if line.startswith(("|uhtml|game", "|uhtmlchange|game")):
            m = _GAME_LINK.match(line)
            if m:
                n = int(m.group(1))
                self.games[n] = m.group(2)
                return ("game", n, m.group(2))
        elif line.startswith("|html|"):
            m = _GAME_WON.match(line)
            if m:
                self.game_winners[int(m.group(2))] = html.unescape(m.group(1))
            else:
                t = _GAME_TIE.match(line)
                if t:
                    self.game_winners[int(t.group(1))] = None
        elif line.startswith("|c|~|/uhtml controls,"):
            m = _READY.search(line)
            if m:
                return ("ready", int(m.group(1)))
        elif line.startswith("|title|") and self.opponent is None:
            self.opponent = title_opponent(line[len("|title|") :], user_id)
        elif line.startswith(("|j|", "|J|")) and self.opponent is None:
            # The series room has no ``|player|`` lines; its players join
            # it as ``|j|☆NAME``.
            self.opponent = joined_player(line, user_id)
        elif line.startswith("|win|"):
            self.ended = True
            self.winner = line[len("|win|") :]
            return ("end",)
        elif line == "|tie" or line.startswith("|tie|"):
            self.ended = True
            self.tie = True
            return ("end",)
        else:
            closed = _room_closed(line)
            if closed is not None:
                return ("closed", closed)
        return None

    def result(self, user_id: str, log_path: Optional[str] = None, reason: Optional[str] = None) -> SeriesResult:
        """The series' :class:`SeriesResult`."""
        games = [self.results[n] for n in sorted(self.results)]
        # Each game's winner as the series room announced it, else as its
        # battle room did.
        winners: Dict[int, Optional[str]] = {}
        for n, r in self.results.items():
            if r.finished:
                winners[n] = r.winner
        winners.update(self.game_winners)
        wins = sum(1 for w in winners.values() if w is not None and to_id(w) == user_id)
        ties = sum(1 for w in winners.values() if w is None)
        finished = self.ended
        extras: Dict[str, Any] = {}
        for g in games:
            for k, v in g.extras.items():
                if isinstance(v, int) and not isinstance(v, bool):
                    extras[k] = extras.get(k, 0) + v
        return SeriesResult(
            room=self.id,
            format=self.format,
            opponent=self.opponent,
            won=None if self.winner is None else to_id(self.winner) == user_id,
            winner=self.winner,
            tie=self.tie,
            finished=finished,
            best_of=self.best_of,
            wins=wins,
            losses=len(winners) - wins - ties,
            ties=ties,
            games=games,
            side=next((g.side for g in games if g.side), None),
            invalid=sum(g.invalid for g in games),
            unavailable=sum(g.unavailable for g in games),
            fallbacks=sum(g.fallbacks for g in games),
            turns=sum(g.turns for g in games),
            log_path=log_path,
            reason=None if finished else reason,
            extras=extras,
            stale=sum(g.stale for g in games),
        )
