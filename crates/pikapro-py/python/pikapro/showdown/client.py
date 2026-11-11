"""The Showdown websocket client: connection, login, games, battle rooms.

See :class:`ShowdownClient`. Pure asyncio; the websocket library is the
optional ``websockets`` package (``pip install 'pikapro[showdown]'``).
"""

from __future__ import annotations

import asyncio
import collections
import hashlib
import ipaddress
import json
import logging
import os
import re
import secrets
import time
import urllib.parse
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any, Callable, Deque, Dict, FrozenSet, List, Optional, Tuple, Union

from .. import _pikapro
from .handlers import _UNCONTAINED, BattleHandler, RandomRequestHandler
from .protocol import (
    Format,
    _room_closed,
    _room_renamed,
    format_key,
    is_command_safe,
    pack_team,
    parse_challenge_pm,
    parse_formats,
    series_length,
    split_frame,
    stale_choice_error,
    to_id,
)
from .room import MAX_LINES, BattleResult, BattleRoom, SeriesResult, SeriesRoom

__all__ = [
    "AcceptPlan",
    "ServerConfig",
    "LOCAL",
    "ARENA",
    "PIKAPRO",
    "PIKAPRO_FORMAT_ALIASES",
    "server_config",
    "pikapro_server",
    "ShowdownClient",
    "ShowdownError",
    "LoginError",
    "ChallengeError",
]

logger = logging.getLogger("pikapro.showdown")

Team = Union[None, str, Callable[[str], Optional[str]]]


@dataclass(frozen=True)
class AcceptPlan:
    """How :meth:`ShowdownClient.accept_challenges` accepts one challenge,
    as its ``plan`` hook decides it (e.g. a team a user sent for their own
    game)."""

    #: The team to upload instead of the operation's (an export or packed
    #: team); ``None``: the operation's.
    team: Optional[str] = None
    #: The battle's answer to an Open Team Sheets offer: ``True`` accepts
    #: at once and holds the team-preview decision until the opponent
    #: answered (at most ``ots_wait`` seconds), so open sheets come before
    #: it; ``False`` rejects at once; ``None``: the client's
    #: ``open_team_sheets``. (A best-of series' games answer as the
    #: client does.)
    open_team_sheets: Optional[bool] = None
    #: Called with the reason when the accept with this plan failed (the
    #: server refused the team, the challenge went away). If the plan
    #: uploaded its own team and the challenge still stands, the challenge
    #: is then accepted again (once) with what ``plan`` says next.
    failed: Optional[Callable[[str], None]] = None


class ShowdownError(RuntimeError):
    """The server refused something (its ``|popup|`` text), or the
    connection failed."""


class LoginError(ShowdownError):
    """Logging in failed: a token refused, a name in use
    (``|nametaken|``)."""


class ChallengeError(ShowdownError):
    """A challenge was rejected or cancelled, or no battle followed it."""


class _Disconnected(ShowdownError):
    """The connection dropped while an operation waited on it."""


#: Domains the client refuses to connect to, subdomains included: it is
#: for local servers, VGC Arena and servers you run.
_REFUSED_DOMAINS = ("psim.us", "pokemonshowdown.com")


def _check_host(host: str) -> None:
    """Raises ``ValueError`` for a host on :data:`_REFUSED_DOMAINS`, or
    one that isn't a host name. The name is compared as the connection
    resolves it: IDNA-encoded (``psim。us`` is ``psim.us``), in lower case,
    without the root's trailing dot."""
    try:
        name = host.encode("idna").decode("ascii").lower().rstrip(".")
    except UnicodeError:
        raise ValueError(f"pikapro.showdown doesn't connect to {host!r}: not a host name") from None
    if any(name == d or name.endswith("." + d) for d in _REFUSED_DOMAINS):
        raise ValueError(f"pikapro.showdown doesn't connect to {name}: use a local server, VGC Arena or a server you run")


@dataclass(frozen=True)
class ServerConfig:
    """Where a client connects, and the limits it keeps there."""

    #: The websocket URL (``ws://host:port/showdown/websocket``).
    websocket_url: str
    #: Seconds between outgoing messages.
    throttle: float = 0.0
    #: Seconds between two challenges.
    challenge_interval: float = 0.0
    #: The most battles at once (0: no limit).
    max_games: int = 0
    #: Format ids this server uses for another format, as ``(id, format)``
    #: pairs: a battle in ``id`` is played as ``format`` (what handlers and
    #: ``ChannelSession`` see). pikapro-showdown-server (:data:`PIKAPRO`) serves
    #: pikapro's ``gen9singles`` and ``gen9doubles`` presets, which have
    #: no team preview, under the custom-game ids; Showdown's own custom
    #: games have team preview, so the ids alone don't say which.
    format_aliases: Tuple[Tuple[str, str], ...] = ()
    #: The server runs best-of series (Showdown's ``BestOfGame``, the ``…bo3``
    #: formats). pikapro-showdown-server (:data:`PIKAPRO`) plays single
    #: games only: a series there is refused before it is asked for.
    series: bool = True
    #: The client logs in with a token sent as the ``/trn`` assertion
    #: (VGC Arena's bot tokens, :data:`ARENA`). It is never logged, and
    #: never sent over a plain ``ws://`` URL to another machine. Without
    #: it: ``/trn NAME,0,`` with an empty assertion, which a local server
    #: started with ``--no-security`` (and pikapro-showdown-server) accepts.
    token_login: bool = False
    #: The environment variables the username and the token default to.
    credentials_env: Optional[Tuple[str, str]] = None
    #: The commands the server takes (``trn``, ``utm``, ...); ``None``: all
    #: of Showdown's. The client doesn't send the others (``/avatar``,
    #: ``/join``, ``/leave``, ``/timer``...): it logs them once and drops
    #: them.
    commands: Optional[Tuple[str, ...]] = None
    #: The server starts battles itself: it pairs the users online with a
    #: team (``/utm``, sent once after logging in and again when it
    #: changes) by itself, and a challenge isn't answered (the battle
    #: starts once both players are free, and the server puts them in it).
    #: On such a server :meth:`ShowdownClient.accept_challenges` plays the
    #: battles it is put in, and never sends ``/accept``.
    auto_battles: bool = False
    #: Suffixes of format ids this server plays as the id without them:
    #: a battle in ``FORMAT + suffix`` is played as ``FORMAT`` when the
    #: engine reads ``FORMAT`` but not the whole id (the longest suffix
    #: first, after ``format_aliases``). VGC Arena's ladders
    #: (:data:`ARENA`), whatever the regulation.
    format_suffixes: Tuple[str, ...] = ()
    #: Websocket close codes after which the client doesn't reconnect: the
    #: operation ends with the server's reason. VGC Arena's 4001 (logged
    #: in from another connection): a reconnect would close that one in
    #: turn.
    final_close_codes: Tuple[int, ...] = ()

    def __post_init__(self) -> None:
        _check_host(urllib.parse.urlparse(self.websocket_url).hostname or "")

    def play_format(self, format_id: str) -> str:
        """The format a battle in ``format_id`` is played as on this server
        (what handlers and ``ChannelSession`` see): its alias
        (:attr:`format_aliases`), else the id without one of
        :attr:`format_suffixes` when only that is a format the engine
        reads, else the id itself."""
        aliased = dict(self.format_aliases).get(format_id)
        if aliased is not None:
            return aliased
        if self.format_suffixes and _pikapro._showdown_preset(format_id) is None:
            for suffix in sorted(self.format_suffixes, key=len, reverse=True):
                stem = format_id[: -len(suffix)] if suffix and format_id.endswith(suffix) else ""
                if stem and _pikapro._showdown_preset(stem) is not None:
                    return stem
        return format_id


#: A local server on port 8000: pikapro-showdown-server, or Showdown's own
#: started with ``--no-security`` (no login server, no throttling).
LOCAL = ServerConfig(websocket_url="ws://localhost:8000/showdown/websocket")

#: pikapro-showdown-server's :attr:`ServerConfig.format_aliases`: its
#: custom-game ids are played as pikapro's ``gen9singles`` and
#: ``gen9doubles`` presets.
PIKAPRO_FORMAT_ALIASES: Tuple[Tuple[str, str], ...] = (
    ("gen9customgame", "gen9singles"),
    ("gen9doublescustomgame", "gen9doubles"),
)
#: pikapro-showdown-server on port 8000: :data:`LOCAL` with
#: :data:`PIKAPRO_FORMAT_ALIASES`, and without best-of series.
PIKAPRO = replace(LOCAL, format_aliases=PIKAPRO_FORMAT_ALIASES, series=False)

#: VGC Arena (https://vgc.bot): a bot-vs-bot ladder on
#: Showdown's protocol. A bot logs in with its token as the ``/trn``
#: assertion (``ARENA_BOT_NAME``, ``ARENA_BOT_TOKEN``) and is put in rated
#: battles without accepting them (:attr:`ServerConfig.auto_battles`;
#: ``/search`` does nothing, challenges start unrated practice battles):
#: one at a time, or up to 5 if its owner allows it on the site
#: (``max_concurrent`` must be at least that setting). Its ladders
#: (``gen9championsvgc2026regmc`` and that id + ``ots``, ``random`` or
#: ``otsrandom``) are all played as the format without the suffix
#: (:attr:`ServerConfig.format_suffixes`). The server takes only the
#: commands below, and ``/cancelsearch`` stops a bot without losing
#: anything (:meth:`ShowdownClient.cancel_search`). For the arena at
#: another address: ``dataclasses.replace(ARENA, websocket_url=...)``, or a
#: URL on one of its hosts (:func:`server_config`).
ARENA = ServerConfig(
    websocket_url="wss://vgc.bot/showdown/websocket",
    throttle=0.05,  # the arena closes a connection past 100 messages per second
    challenge_interval=10.0,
    max_games=5,  # "Battles at once": 1 unless the bot's owner raises it, up to 5
    format_suffixes=("random", "ots", "otsrandom"),
    series=False,
    token_login=True,
    credentials_env=("ARENA_BOT_NAME", "ARENA_BOT_TOKEN"),
    commands=("trn", "utm", "challenge", "cancelchallenge", "accept", "reject", "choose", "forfeit", "cancelsearch"),
    auto_battles=True,
    final_close_codes=(4001,),
)
# The arena's hosts (the second redirects to the first).
_ARENA_HOSTS = ("vgc.bot", "vgc.ellep.dev")


def pikapro_server(address: Union[str, int, None] = None) -> ServerConfig:
    """pikapro-showdown-server at ``address`` (a URL, ``host:port`` or a
    port; ``None``: :data:`PIKAPRO`): a local-style server whose custom-game
    ids are pikapro's presets."""
    if address is None:
        return PIKAPRO
    return replace(server_config(address), format_aliases=PIKAPRO_FORMAT_ALIASES, series=False)


def server_config(server: Union[str, ServerConfig, None]) -> ServerConfig:
    """A :class:`ServerConfig` from a name or an address.

    ``"arena"`` is :data:`ARENA` (VGC Arena; so is a URL on its host),
    ``"local"`` or ``None`` is :data:`LOCAL`, ``"pikapro"`` is
    :data:`PIKAPRO` (pikapro-showdown-server) and ``"pikapro:<address>"`` is
    :func:`pikapro_server` there; a ``ws://``/``wss://`` URL, ``host:port`` or
    a port number is a server you run there (no login, no limits).
    A URL with another scheme (``http://``) raises ``ValueError``, and so
    does a host on ``psim.us`` or ``pokemonshowdown.com``.
    """
    if isinstance(server, ServerConfig):
        return server
    if server is None or server == "local":
        return LOCAL
    if server == "pikapro":
        return PIKAPRO
    if isinstance(server, str) and server.startswith("pikapro:"):
        return pikapro_server(server[len("pikapro:"):])
    if server == "arena":
        return ARENA
    text = str(server)
    if text.isdigit():
        text = f"localhost:{text}"
    scheme, sep, rest = text.partition("://")
    if sep and scheme.lower() in ("ws", "wss"):
        text = f"{scheme.lower()}://{rest}"
    elif sep:
        raise ValueError(f"server {text!r}: a websocket URL starts with ws:// or wss://, not {scheme}://")
    else:
        text = f"ws://{text}/showdown/websocket"
    host = urllib.parse.urlparse(text).hostname or ""
    if host in _ARENA_HOSTS:
        return replace(ARENA, websocket_url=text)
    return ServerConfig(websocket_url=text)


def _local(host: str) -> bool:
    """A loopback host (traffic that never leaves the machine)."""
    host = host.strip("[]").rstrip(".").lower()
    if host == "localhost" or host.endswith(".localhost"):
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:  # (a name: 127.example.com is another machine)
        return False


def _cleartext_login(config: "ServerConfig", url: Optional[str] = None) -> Optional[str]:
    """Why a login on ``config`` (at ``url``: where a redirect leads)
    would send the token unencrypted to another machine, or ``None``."""
    url = config.websocket_url if url is None else url
    ws = urllib.parse.urlparse(url)
    if config.token_login and ws.scheme != "wss" and not _local(ws.hostname or ""):
        return f"the login token would go unencrypted to {url}: use a wss:// URL"
    return None


class _RedirectRefused(Exception):
    """The server redirected the connection somewhere the client doesn't go."""


def _redirect_refusal(config: "ServerConfig", url: str, *, token: bool) -> Optional[str]:
    """Why the client doesn't follow a redirect to ``url``, or ``None``:
    the rules of the configured URL hold for every URL a server sends the
    connection to (:func:`_check_host`, and :func:`_cleartext_login` when
    the client logs in with a ``token``)."""
    try:
        _check_host(urllib.parse.urlparse(url).hostname or "")
    except ValueError as e:
        return str(e)
    return _cleartext_login(config, url) if token else None


def _connect(config: "ServerConfig", *, token: bool, **kwargs: Any) -> Any:
    """``websockets.connect`` to ``config``'s URL, with each redirect
    checked (:func:`_redirect_refusal`): websockets follows redirects,
    cross-origin ones included, and would otherwise connect wherever the
    server says."""
    from websockets.asyncio.client import connect

    class checked_connect(connect):
        def process_redirect(self, exc: Exception) -> Any:
            try:
                target = super().process_redirect(exc)
            except ValueError as e:  # a Location that isn't a URL (UnicodeError included)
                return _RedirectRefused(f"the redirect's address can't be read ({type(e).__name__})")
            if isinstance(target, str):
                why = _redirect_refusal(config, target, token=token)
                if why is not None:
                    return _RedirectRefused(why)
            return target

    return checked_connect(config.websocket_url, **kwargs)


def _redact(message: str) -> str:
    """An outgoing message as it may be logged: a ``/trn`` assertion is
    replaced (it logs the user in for this connection)."""
    room, _, text = message.partition("|")
    if text.startswith("/trn "):
        name = text[5:].split(",", 1)[0]
        return f"{room}|/trn {name},0,<assertion>"
    if len(text) > 200:
        return f"{room}|{text[:200]}..."
    return message


def _command(message: str) -> Optional[str]:
    """The command an outgoing ``ROOM|TEXT`` message runs (``"utm"`` for
    ``|/utm ...``), or ``None`` for plain text."""
    text = message.partition("|")[2]
    if not text.startswith("/") or text.startswith("//"):
        return None
    return text[1:].split(" ", 1)[0].lower()


def _set_key(species: str, moves: Any) -> Tuple[str, FrozenSet[str]]:
    # Showdown's request writes Return and Frustration with their power
    # (``return102``), and Hidden Power with its type and power.
    return to_id(species), frozenset(re.sub(r"\d+$", "", to_id(m)) for m in moves if to_id(m))


def _base_key(species: str, moves: Any) -> Tuple[str, FrozenSet[str]]:
    """:func:`_set_key` by the species' name before its first hyphen: a
    request after team preview names the forme the Pokémon is in
    (``Garchomp-Mega-Z`` after a Mega Evolution, ``Palafin-Hero``), not
    the team's (``Garchomp``)."""
    name, key = _set_key(species, moves)
    return to_id(str(species).split("-")[0]) or name, key


def _team_shows(packed: Optional[str], request: Dict[str, Any]) -> bool:
    """Whether a request's ``side.pokemon`` shows Pokémon of the packed
    team (each one a different set of it, by species and moves: the four
    brought, after team preview, or all six; after team preview by
    the species' base name, as a Mega Evolution or another forme change
    renames it).

    A team-preview request shows the whole team in its order, before
    anything moved an item: each Pokémon is compared with the set in its
    place, by its item and ability too. Two teams of the same Pokémon and
    moves (a rotation's: other items, spreads, order) are told apart so."""
    if not packed or not isinstance(request, dict):
        return False
    side = request.get("side")
    mons = side.get("pokemon") if isinstance(side, dict) else None
    if not isinstance(mons, list) or not mons or not all(isinstance(m, dict) for m in mons):
        return False
    sets = []
    for p in packed.split("]"):
        f = p.split("|")
        if len(f) < 5:
            return False
        moves = f[4].split(",")
        sets.append((_set_key(f[1] or f[0], moves), to_id(f[2]), to_id(f[3]), _base_key(f[1] or f[0], moves)))
    if request.get("teamPreview") and len(mons) == len(sets):
        for m, (key, item, ability, _) in zip(mons, sets):
            if _set_key(str(m.get("details") or "").split(",")[0], m.get("moves") or ()) != key:
                return False
            if to_id(str(m.get("item") or "")) != item:
                return False
            # (a packed ability may be a slot: "", "0", "1", "H")
            if ability not in ("", "0", "1", "h") and to_id(str(m.get("baseAbility") or "")) != ability:
                return False
        return True
    keys = [base for _, _, _, base in sets]
    for m in mons:
        key = _base_key(str(m.get("details") or "").split(",")[0], m.get("moves") or ())
        if key not in keys:
            return False
        keys.remove(key)
    return True


def _stats_show(packed: str, request: Dict[str, Any], format_id: str) -> Optional[bool]:
    """Whether a team-preview request's stats and max HP are the packed
    team's, as the engine computes them in ``format_id``; ``None`` when that
    can't be told (another request, a format or team the engine refuses)."""
    if not request.get("teamPreview"):
        return None
    try:
        preset = _pikapro._showdown_preset(format_id) or format_id
        mine = json.loads(_pikapro.Battle(preset, [packed, packed], seed=0).request_json(0))["side"]["pokemon"]
        shown = request["side"]["pokemon"]
        if len(mine) != len(shown):
            return False

        def max_hp(m: Dict[str, Any]) -> str:
            return str(m.get("condition") or "").split(" ")[0].partition("/")[2]

        return all(a.get("stats") == b.get("stats") and max_hp(a) == max_hp(b) for a, b in zip(mine, shown))
    except Exception:  # noqa: BLE001 - not told: the other checks decide
        return None


#: The shortest piece of the login token :func:`_scrub_secret` takes out
#: of a text that has only a part of it.
SCRUB_MIN = 8

# A ``/trn NAME,0,ASSERTION`` in a logged frame, the assertion going up to
# the end of the record but for how websockets closes its frame's text (a
# quote and ``[N bytes]``).
_TRN = re.compile(r"/trn [^,]*,[^,]*,")
_TRN_TAIL = re.compile(r"['\"](?: \[[^\[\]]*\])?$")


def _scrub_trn(text: str) -> str:
    """``text`` with ``<token>`` for the assertion of a ``/trn`` in it."""
    m = _TRN.search(text)
    if m is None:
        return text
    tail = _TRN_TAIL.search(text, m.end())
    end = tail.start() if tail is not None else len(text)
    if end <= m.end():  # no assertion: a login without a token
        return text
    return text[: m.end()] + "<token>" + text[end:]


def _longest(text: str, secret: str, head: bool) -> int:
    """The length of the longest head (or tail) of ``secret`` in ``text``,
    of those shorter than the whole and at least :data:`SCRUB_MIN` long;
    0 for none."""
    piece = (lambda k: secret[:k]) if head else (lambda k: secret[-k:])
    lo, hi = SCRUB_MIN, len(secret) - 1
    if hi < lo or piece(lo) not in text:
        return 0
    while lo < hi:  # (a head's own heads are in the text too)
        mid = (lo + hi + 1) // 2
        if piece(mid) in text:
            lo = mid
        else:
            hi = mid - 1
    return lo


def _scrub_secret(text: str, secret: Optional[str]) -> str:
    """``text`` with ``<token>`` for the login token: the whole of it, and
    a part of it cut at either end. A logger that shortens long text shows
    such parts: websockets' frame log keeps a long frame's head and tail
    (``Frame.__str__``), which for a ``/trn`` with a token of some sixty
    characters or more are the token's. Every head and every tail of the
    token of :data:`SCRUB_MIN` characters or more goes, the longest first.
    Text that merely starts or ends like the token goes too, so a token
    should not start with the bot's name."""
    if not secret:
        return text
    text = text.replace(secret, "<token>")
    for head in (True, False):
        # Each pass takes the longest piece left; a shorter cut beside it
        # is the next pass's.
        for _ in range(len(secret)):
            n = _longest(text, secret, head)
            if not n:
                break
            text = text.replace(secret[:n] if head else secret[-n:], "<token>")
    return text


class _Scrub(logging.Filter):
    """Takes the login token out of every record a logger passes:
    websockets' frame log, at DEBUG, has each ``/trn``. ``secret`` becomes
    ``<token>`` wherever it is, whole or cut (:func:`_scrub_secret`), and
    so does whatever a ``/trn`` carries as its assertion."""

    def __init__(self, secret: Optional[str] = None):
        super().__init__()
        self.secret = secret or None

    def filter(self, record: logging.LogRecord) -> bool:
        try:
            text = record.getMessage()
        except Exception:  # noqa: BLE001 - a record that can't format: dropped
            return False
        clean = text
        if self.secret is not None:
            clean = _scrub_secret(_scrub_trn(clean), self.secret)
        if clean != text:
            record.msg, record.args = clean, ()
        return True


class _Outbox:
    """Outgoing messages, spaced ``interval`` seconds apart. A group of
    lines goes out together (``/utm`` then ``/search``); urgent groups
    (battle choices) go before the others."""

    def __init__(self, interval: float):
        self.interval = interval
        self.urgent: Deque[List[str]] = collections.deque()
        self.normal: Deque[List[str]] = collections.deque()
        self.wake = asyncio.Event()
        self.idle = asyncio.Event()
        self.idle.set()
        self.last = 0.0
        #: The group the send loop is sending.
        self.sending: Optional[List[str]] = None

    def put(self, lines: List[str], urgent: bool = False) -> None:
        (self.urgent if urgent else self.normal).append(lines)
        self.idle.clear()
        self.wake.set()

    def take(self) -> Optional[List[str]]:
        if self.urgent:
            return self.urgent.popleft()
        if self.normal:
            return self.normal.popleft()
        return None

    def clear(self) -> None:
        self.urgent.clear()
        self.normal.clear()
        self.idle.set()

    def rename(self, old: str, new: str) -> None:
        """Room ``old`` is now ``new``: the messages waiting for it (and
        the rest of the group being sent) go to ``new``."""
        for group in (*self.urgent, *self.normal, self.sending or []):
            for i, message in enumerate(group):
                if message.startswith(f"{old}|"):
                    group[i] = new + message[len(old) :]
                elif message in (f"|/join {old}", f"|/leave {old}"):
                    group[i] = message[: -len(old)] + new


class _Slots:
    """The client's concurrency: ``capacity`` units, taken in weights (a
    best-of series is two of Showdown's games), first come first
    served."""

    def __init__(self, capacity: int):
        self.capacity = capacity
        self.used = 0
        self._queue: Deque[Tuple[int, asyncio.Future]] = collections.deque()

    async def acquire(self, weight: int = 1) -> int:
        """Takes ``weight`` units (at most the capacity) once free, and
        returns what it took (for :meth:`release`)."""
        weight = max(1, min(weight, self.capacity))
        if not self._queue and self.used + weight <= self.capacity:
            self.used += weight
            return weight
        fut = asyncio.get_running_loop().create_future()
        self._queue.append((weight, fut))
        try:
            await fut
        except BaseException:
            if fut.done() and not fut.cancelled():  # granted, then cancelled
                self.release(weight)
            self._wake()
            raise
        return weight

    def release(self, weight: int) -> None:
        self.used -= weight
        self._wake()

    def _wake(self) -> None:
        while self._queue:
            weight, fut = self._queue[0]
            if fut.done():
                self._queue.popleft()
                continue
            if self.used + weight > self.capacity:
                return
            self._queue.popleft()
            self.used += weight
            fut.set_result(None)


class _Waiter:
    """A game-start operation waiting for its battle room, or its series
    room in a best-of format (``series``); ``opponent``: the opponent's id
    it expects, for a challenge or an accept."""

    def __init__(
        self,
        match: Callable[[Any], bool],
        handler: BattleHandler,
        opponent: Optional[str] = None,
        series: bool = False,
    ):
        self.match = match
        self.handler = handler
        self.opponent = opponent
        self.series = series
        #: The team this operation uploaded (``/utm``, packed), if any.
        self.team: Optional[str] = None
        #: The battle's own answer to Open Team Sheets (``AcceptPlan``),
        #: ``None``: the client's.
        self.open_team_sheets: Optional[bool] = None
        self.future: asyncio.Future = asyncio.get_running_loop().create_future()
        #: A ladder search's format; whether ``|updatesearch|`` listed it;
        #: the last refusal popup since; the check that fails the waiter
        #: when the search went away without a battle.
        self.search: Optional[str] = None
        self.searched = False
        self.refusal: Optional[str] = None
        self.drop_check: Optional[asyncio.TimerHandle] = None
        #: Resolved once ``|updatesearch|`` lists the search.
        self.listed: asyncio.Future = asyncio.get_running_loop().create_future()


#: The largest frame the client reads (Showdown's biggest, a long battle's
#: log on joining a room, is well under a megabyte).
MAX_FRAME = 16 * 1024 * 1024

#: ``|popup|`` texts that refuse a game start (Showdown's
#: ``server/ladders.ts``, ``monitor.ts``, ``users.ts`` and ``rooms.ts``,
#: and pikapro-showdown-server's). Any other popup during a start is logged
#: and the start keeps waiting.
_REFUSALS = (
    "server is restarting",
    # ``Rooms.createBattle`` under ``/prelockdown``: "The server will be
    # restarting soon. Best-of-3 battles cannot be started at this time."
    "restarting soon",
    "cannot be started",
    # ``Rooms.createBattle``: "Your special battle settings don't match".
    "special battle settings",
    "server is under attack",
    "barred from starting",
    "you are limited to",
    "format is invalid",
    "selected format",
    "team was rejected",
    "can't battle yourself",
    "already challenging",
    "not accepting challenges",
    "less than 10 seconds",
    "already a challenge",
    "pending challenges",
    "not ladderable",
    "already searching",
    "have been cancelled",
    "no longer looking",
    "was not found",
    "not found.",
    "must choose a username",
    "you are locked",
    "too many lines",
    "must be below",
    "invalid message",
)


def _refuses(popup: str) -> bool:
    text = popup.lower()
    return any(r in text for r in _REFUSALS)


def _game_limit(error: BaseException) -> bool:
    """One of Showdown's limits refused a start: the games at once
    (``Monitor.countConcurrentBattle``: "Due to high load, you are limited
    to 5 games at the same time.") or the starts per IP
    (``Monitor.countPrepBattle``: "... limited to 12 battles and team
    validations every 3 minutes.")."""
    return isinstance(error, ShowdownError) and "you are limited to" in str(error).lower()


def _start_limit(error: BaseException) -> bool:
    """The limit that refused a start is the starts per IP (it passes with
    time, not with a game's end)."""
    return "every 3 minutes" in str(error).lower()


def _challenge_too_soon(error: BaseException) -> bool:
    """Showdown refused a challenge within 10 s of the last one
    (``Ladders.makeChallenge``)."""
    return isinstance(error, ShowdownError) and "less than 10 seconds" in str(error).lower()


#: Seconds a connection must stay logged in for its end to reset the
#: reconnect backoff (to one second). A shorter one keeps it doubling, so a
#: server that takes the login and then closes is retried every
#: :data:`BACKOFF_MAX_S` seconds in the end, not every second.
BACKOFF_RESET_S = 30.0
#: The longest wait between two reconnects, in seconds.
BACKOFF_MAX_S = 60.0

#: A battle room id as Showdown makes them (``battle-FORMAT-NUMBER``, and a
#: hidden room's ``-PASSWORD``): the only ones the client plays in.
_ROOM_ID = re.compile(r"battle-[a-z0-9-]{1,200}")
#: A best-of series' room id (``game-bestof3-FORMAT-NUMBER``, Showdown's
#: ``createBattle``).
_SERIES_ID = re.compile(r"game-bestof[0-9]{1,2}-[a-z0-9-]{1,200}")
_LOG_NAME = re.compile(r"(battle|game-bestof[0-9]{1,2})-[a-z0-9-]{1,200}")

#: Seconds a series' last games may take to end after the series did
#: (Showdown ends them together: a forfeit forfeits the game being played).
SERIES_END_GRACE = 15.0

#: Seconds a search the server dropped (no longer in ``|updatesearch|``)
#: may take to become a battle (Showdown's ``createBattle`` cancels the
#: searches, then opens the room or refuses the match with a popup).
SEARCH_DROP_GRACE = 5.0

#: How many teams of a rotation are tried for one upload, and how many
#: rejected uploads in a row are answered with the rotation's next team.
TEAM_TRIES = 5

#: Seconds the server has to accept a request that starts a game (or to
#: refuse it with a popup).
START_CONFIRM_TIMEOUT = 60.0

#: Seconds an accepted challenge's room has to open.
ACCEPT_ROOM_TIMEOUT = 60.0

#: Seconds before a challenge the server found too soon is sent again, on
#: a server without a challenge interval of its own.
CHALLENGE_RETRY_S = 10.5

#: Seconds a series waits for its next game after a game ended. Showdown
#: starts it after both players are ready, or 40 s after the game
#: (``BEST_OF_IN_BETWEEN_TIME``); past this the client forfeits the series
#: and ends it.
NEXT_GAME_TIMEOUT = 150.0

#: Seconds the held team-preview decision waits for the sheets
#: (``|showteam|``) after the client accepted Open Team Sheets in
#: ``open_team_sheets="mutual"`` mode (the opponent had accepted, so
#: Showdown shows them at once); past this the decision goes anyway.
OTS_SHOW_GRACE = 5.0

#: What a room rejoined after a reconnect keeps of its Open Team Sheets
#: state (the replayed log announces the answers again; the wait goes on).
_OTS_CARRIED = (
    "_ots_answered", "ots_offered", "ots_foe", "ots_answer", "ots_open", "ots_wait_ms",
    "_ots_hold", "_ots_since", "_ots_deferred", "_ots_timer", "_ots_mode",
)

#: The event loop watch (``ShowdownClient._watch_loop``): it wakes every
#: ``LOOP_TICK`` seconds and logs a wake-up later than ``LOOP_STALL_WARN``
#: seconds (synchronous work on the loop, or the GIL held by a worker): a
#: stalled loop answers no keepalive ping and reads no frame.
LOOP_TICK = 1.0
LOOP_STALL_WARN = 5.0

#: Seconds after a series room names a game the client isn't in before
#: the client joins it (Showdown joins the players to it itself).
GAME_JOIN_DELAY = 1.0

#: Showdown's game limit refused a start: the client waits for one of its
#: games to end (at most this many seconds) and tries again. With none of
#: its own in progress (the account's games elsewhere count too), it tries
#: :data:`GAME_LIMIT_TRIES` times.
GAME_LIMIT_WAIT = 30.0
GAME_LIMIT_TRIES = 3

#: The uploads (``/utm``) a server that starts battles itself remembers
#: (:attr:`ShowdownClient.uploads`): with several battles at once, uploads
#: follow one another within seconds, and a battle paired meanwhile plays
#: any of the last few.
UTM_KEPT = 16

#: A server that starts battles itself (VGC Arena): a battle room its
#: ``|updatesearch|`` no longer lists ("lists every room the bot is in"),
#: and that gets no frame for this many seconds, is over: it ended while
#: the client couldn't see it (after a reconnect the arena resends only
#: the battles still in progress, so a battle that ended during the
#: reconnect would otherwise stay open). Longer than a battle can go
#: without a frame (the 90 s of team preview).
UNLISTED_GRACE_S = 120.0
#: The result reason of such a room (:meth:`ShowdownClient.close_unlisted`).
UNLISTED_REASON = "ended unseen (the server no longer lists it)"


def _json_object(text: str) -> Dict[str, Any]:
    """A JSON object from the server, ``{}`` for anything else (invalid,
    too deep, not an object)."""
    try:
        data = json.loads(text)
    except (ValueError, RecursionError):
        return {}
    return data if isinstance(data, dict) else {}


def _log_name(room_id: str) -> str:
    """A safe file name for a room's log: the id itself when it is a plain
    battle room id, else a hash of it (the id comes from the server)."""
    if _LOG_NAME.fullmatch(room_id):
        return f"{room_id}.log"
    return "battle-" + hashlib.sha256(room_id.encode("utf-8", "replace")).hexdigest()[:32] + ".log"


class ShowdownClient:
    """A player on a Showdown server: it logs in, starts games (ladder,
    challenges), and plays each battle room with a :class:`BattleHandler`.

    ::

        async with ShowdownClient("my-bot", server="local", handler=SessionHandler("simple_heuristics")) as c:
            results = await c.ladder(10, "gen9championsvgc2026regmc", team=open("team.txt").read())

    A local server takes a name and no login. Without a ``username`` the
    client takes a random ``pikapro-…`` name (``guest=True`` stays a guest
    instead, able to read the server but not to battle). VGC Arena logs in
    with a bot's name and ``token``, by default ``ARENA_BOT_NAME`` and
    ``ARENA_BOT_TOKEN``; the token is never logged.

    * ``server``: a :class:`ServerConfig` or a name for
      :func:`server_config` (``"local"``, ``"arena"``, a URL, ``host:port``).
    * ``max_concurrent``: battles at once, all operations together (capped
      at the server's ``max_games``).
    * ``log_dir``: each room's lines are saved there as ``ROOM.log``.
    * ``timer``: turns the battle timer on in each room.
    * ``forfeit_on_exit``: :meth:`close` forfeits unfinished battles.
    * ``leave_delay``: seconds the client stays in a finished battle room
      (to read a rated game's rating lines).
    * ``reconnect``: reconnects after a dropped connection (exponential
      backoff, from one second to a minute, which starts over once a
      connection has stayed logged in for 30 seconds; gives up after
      ``max_reconnects`` failed attempts in a row) and rejoins the battles.
    * ``login_timeout``: a connection not logged in that many seconds
      after it opened is closed; :meth:`connect` raises
      :class:`LoginError`, and a reconnect counts a failed attempt.
    * ``max_retries``: after that many invalid choices in a row for one
      request the client sends ``default`` for the handler.
    * ``open_team_sheets``: the answer to an Open Team Sheets offer at
      team preview. ``None`` (the default) accepts in a best-of series'
      games and leaves other offers alone, ``True`` accepts, ``False``
      rejects, and ``"mutual"`` waits up to ``ots_wait`` seconds for the
      opponent's answer and matches it (rejecting when the wait ends),
      holding the room's team-preview decision meanwhile. The room
      records it (``BattleRoom.ots_*``).

    A best-of series (a ``…bo3`` format, a challenge's ``Best of = 3``
    rule) is played as one: each game's room goes to the handler of the
    operation that started the series (:attr:`BattleRoom.series`), and the
    operation's result is a :class:`SeriesResult` with every game's.

    A subclass reads more of what the server sends, or holds decisions,
    through hooks that do nothing by default: :meth:`on_battle_lines`,
    :meth:`on_pm` and :meth:`hold_decision` (with :meth:`release_decision`);
    :meth:`send_room` posts in a room.

    A handler's failure is its battle's alone: an exception in its
    ``open`` or ``feed`` forfeits that battle (its result's ``reason``
    says why), and one in ``choose`` sends ``default`` for the battles it
    was asked about. If the connection ends for good (the client gave up
    reconnecting, or a login failed), every battle ends as disconnected
    and the operations waiting to start one raise the error.
    """

    def __init__(
        self,
        username: Optional[str] = None,
        token: Optional[str] = None,
        *,
        server: Union[str, ServerConfig, None] = "local",
        handler: Optional[BattleHandler] = None,
        avatar: Optional[str] = None,
        guest: bool = False,
        max_concurrent: int = 1,
        log_dir: Union[str, os.PathLike, None] = None,
        timer: bool = False,
        forfeit_on_exit: bool = True,
        reconnect: bool = True,
        max_reconnects: int = 8,
        max_retries: int = 3,
        open_timeout: float = 20.0,
        login_timeout: Optional[float] = 60.0,
        ping_interval: Optional[float] = 20.0,
        log: Optional[logging.Logger] = None,
        open_team_sheets: Union[bool, str, None] = None,
        hide_battles: bool = False,
        ots_wait: float = 20.0,
        leave_delay: float = 0.0,
    ):
        self.config = server_config(server)
        why = None if guest else _cleartext_login(self.config)
        if why is not None:
            raise ValueError(why)
        if open_team_sheets is not None and not isinstance(open_team_sheets, bool) and open_team_sheets != "mutual":
            raise ValueError(f"open_team_sheets must be True, False, None or 'mutual', not {open_team_sheets!r}")
        if not ots_wait > 0:
            raise ValueError(f"ots_wait must be positive, not {ots_wait!r}")
        if not leave_delay >= 0:
            raise ValueError(f"leave_delay must be at least 0, not {leave_delay!r}")
        if login_timeout is not None and not login_timeout > 0:
            raise ValueError(f"login_timeout must be positive (or None), not {login_timeout!r}")
        if token is not None and not self.config.token_login and not guest:
            raise ValueError(f"a token was given, but {self.config.websocket_url} takes no login: leave the token out")
        env = self.config.credentials_env
        if env is not None:
            if username is None:
                username = os.environ.get(env[0]) or None
            if token is None and username is not None and username == os.environ.get(env[0]):
                token = os.environ.get(env[1]) or None
        if self.config.token_login and not guest and (username is None or not token):
            hint = f" (or set ${env[0]} and ${env[1]})" if env is not None else ""
            raise ValueError(f"{self.config.websocket_url} logs in with a bot name and its token: pass them{hint}")
        if username is None and not guest:
            username = "pikapro-" + secrets.token_hex(4)
        for what, value in (("username", username), ("avatar", avatar)):
            if value is not None and (not is_command_safe(value) or "," in value):
                raise ValueError(f"{what} {value!r} has a comma, a line break or a control character")
        self.username: Optional[str] = username
        self._token = token
        self.guest = guest
        self.avatar = avatar
        self.handler: BattleHandler = handler if handler is not None else RandomRequestHandler()
        cap = self.config.max_games or max_concurrent
        self.max_concurrent = max(1, min(max_concurrent, cap))
        self.log_dir = Path(log_dir) if log_dir is not None else None
        self.timer = timer
        self.forfeit_on_exit = forfeit_on_exit
        self.reconnect = reconnect
        self.max_reconnects = max_reconnects
        self.max_retries = max_retries
        self.open_timeout = open_timeout
        self.login_timeout = login_timeout
        self.ping_interval = ping_interval
        self.log = log or logger
        self.open_team_sheets = open_team_sheets
        #: Seconds ``open_team_sheets="mutual"`` waits for the opponent.
        self.ots_wait = float(ots_wait)
        #: Hide every battle this client starts from spectators: ``/hidenext``
        #: goes out before each ``/search`` and ``/challenge`` (the server
        #: copies the setting into that one search and then resets it).
        self.hide_battles = hide_battles
        #: Seconds the client stays in a finished battle room before it
        #: leaves (``/leave``): to read what the server posts after the end,
        #: such as a rated game's rating lines (:meth:`on_battle_lines`).
        self.leave_delay = float(leave_delay)

        #: The name the server shows for the client, once logged in.
        self.name: Optional[str] = None
        #: The server's formats (``|formats|``), by id.
        self.formats: Dict[str, Format] = {}
        #: Battles being played, by room id.
        self.rooms: Dict[str, BattleRoom] = {}
        #: Every finished battle's result, in the order they ended (a
        #: series' games included).
        self.results: List[BattleResult] = []
        #: Best-of series being played, by series room id.
        self.series: Dict[str, SeriesRoom] = {}
        #: Every finished series' result, in the order they ended.
        self.series_results: List[SeriesResult] = []
        #: Formats the server says the client is searching (``|updatesearch|``).
        self.searching: List[str] = []
        #: Standing challenges, ``(from_id, to_id) -> format``.
        self.challenges: Dict[Tuple[str, str], str] = {}
        #: The games the server says the client is in (``|updatesearch|``'s
        #: ``games``: room id -> title).
        self.games: Dict[str, Any] = {}
        #: Called with each ``|popup|``'s text (line breaks restored), after
        #: the client has read it; an exception in it is logged.
        self.on_popup: Optional[Callable[[str], None]] = None
        # A server that starts battles itself (``ServerConfig.auto_battles``):
        # the team uploaded last (packed; sent again after each login) and
        # the one before (a battle may start before a new upload lands),
        # whether a battle was played with the last one (the rotation then
        # moves on), the operation's team (``Team``) and format, and the
        # rejections in a row (the next team of a rotation is tried).
        self._utm: Optional[str] = None
        self._utm_prev: Optional[str] = None
        self._utm_played = False
        self._utm_rejections = 0
        self._auto_team: Optional[Tuple[Team, str]] = None
        # The last uploads, oldest first, with when each went out
        # (monotonic): a battle the server paired while uploads followed
        # one another (several battles at once) plays one of them.
        self._utm_sent: Deque[Tuple[float, str]] = collections.deque(maxlen=UTM_KEPT)
        #: A server that starts battles itself: whether a rotation's next
        #: team goes up as :meth:`accept_challenges` waits for its next
        #: battle (once a battle was played with the last one). ``False``:
        #: the caller puts each next team up itself (:meth:`upload_team`),
        #: and the operation uploads only the first.
        self.rotate_on_wait = True
        # ``cancel_search``: asked (sent again after each login); the
        # ``|updatesearch|`` frames read; their count when ``/cancelsearch``
        # last went out on this connection (None: not yet).
        self._search_cancelled = False
        self._search_updates = 0
        self._cancel_mark: Optional[int] = None
        # The operations playing the battles the server starts (``_auto_accept``):
        # (match, handler, battles put in before the operation waited).
        self._auto_ops: List[Tuple[Callable[[BattleRoom], bool], BattleHandler, Deque[BattleRoom]]] = []
        # Commands the server doesn't take (``ServerConfig.commands``) the
        # client already said it won't send.
        self._unsent: set = set()
        self._ws_filter: Optional[logging.Filter] = None
        # Closes started from a callback (``_close_later``), kept until done.
        self._closers: set = set()

        self._ws: Any = None
        self._run_task: Optional[asyncio.Task] = None
        self._tasks: List[asyncio.Task] = []
        self._connected: Optional[asyncio.Event] = None
        self._logged_in: Optional[asyncio.Event] = None
        self._login_error: Optional[BaseException] = None
        self._closing = False
        self._outbox: Optional[_Outbox] = None
        self._start_lock: Optional[asyncio.Lock] = None
        self._slots: Optional[_Slots] = None
        self._waiters: List[_Waiter] = []
        self._room_handlers: Dict[str, BattleHandler] = {}
        self._series_handlers: Dict[str, BattleHandler] = {}
        # Finished battle rooms not left yet (``leave_delay``), by id.
        self._leaving: Dict[str, BattleRoom] = {}
        # Series rooms asked for (a game's room named one the client isn't in).
        self._joining: set = set()
        self._popups: List[asyncio.Future] = []
        self._pm_waiters: List[Tuple[Callable[[str], bool], asyncio.Future]] = []
        self._incoming: Optional[asyncio.Queue] = None
        self._decide_wake: Optional[asyncio.Event] = None
        self._last_challenge = 0.0
        self._generation = 0
        # Failed reconnects in a row (reset by a login), and the next
        # backoff (reset by a connection that stayed logged in for
        # BACKOFF_RESET_S, since ``_login_at``); set by close() to end a
        # backoff early; the login task.
        self._reconnects = 0
        self._delay = 1.0
        self._login_at: Optional[float] = None
        self._stop: Optional[asyncio.Event] = None
        self._login_task: Optional[asyncio.Task] = None
        # The connection's diagnostics (``connection lost``): when it opened,
        # when its last frame came, and the event loop's longest stall since
        # it opened (``_watch_loop``).
        self._opened_at = 0.0
        self._frame_at = 0.0
        self._stall_max = 0.0
        #: The error that ended the connection for good (``None`` while it
        #: lives, or after :meth:`close`).
        self._fatal: Optional[BaseException] = None
        if max_concurrent > cap:
            self._event(logging.WARNING, "concurrency capped", asked=max_concurrent, cap=cap)

    # -- logging -----------------------------------------------------------------

    def _event(self, level: int, event: str, **fields: Any) -> None:
        """A structured log record: ``event`` plus ``fields`` (also in the
        record's ``extra`` as ``event`` and ``fields``)."""
        if not self.log.isEnabledFor(level):
            return
        # A login token can't leak through a field (an error's text, a
        # server line that echoes it): it is scrubbed from every record.
        fields = {k: self._scrub(v) if isinstance(v, str) else v for k, v in fields.items()}
        text = self._scrub(" ".join(f"{k}={v!r}" for k, v in fields.items()))
        user = self.name or self.username or "guest"
        self.log.log(level, "[%s] %s %s", user, event, text, extra={"event": event, "fields": fields, "user": user})

    def _scrub(self, text: str) -> str:
        """``text`` without the login token."""
        secret = self._token
        if secret and len(secret) >= 4:
            return _scrub_secret(text, secret)
        return text

    # -- connection ----------------------------------------------------------------

    @property
    def user_id(self) -> str:
        """The client's user id (``toID`` of the name)."""
        return to_id(self.name or self.username or "")

    @property
    def connected(self) -> bool:
        """The websocket is open and the client is logged in."""
        return self._logged_in is not None and self._logged_in.is_set()

    async def __aenter__(self) -> "ShowdownClient":
        await self.connect()
        return self

    async def __aexit__(self, *exc: Any) -> None:
        await self.close()

    async def connect(self) -> None:
        """Connects and logs in (returns once logged in). Raises
        :class:`LoginError` or :class:`ShowdownError` on failure (a
        :class:`LoginError` when not logged in within ``login_timeout``
        seconds). After the connection ended for good, connects again."""
        if self._run_task is not None:
            if not self._run_task.done():
                return
            await self.close()  # it ended for good: start over
        try:
            import websockets  # noqa: F401
        except ImportError as e:  # pragma: no cover
            raise ImportError("pikapro.showdown needs the websockets package: pip install 'pikapro[showdown]'") from e
        self._connected = asyncio.Event()
        self._logged_in = asyncio.Event()
        self._outbox = _Outbox(self.config.throttle)
        self._start_lock = asyncio.Lock()
        self._slots = _Slots(self.max_concurrent)
        self._incoming = asyncio.Queue()
        self._decide_wake = asyncio.Event()
        self._stop = asyncio.Event()
        self._closing = False
        self._fatal = None
        self._login_error = None
        self._reconnects = 0
        self._delay = 1.0
        self._login_at = None
        if self._ws_filter is None:
            # websockets logs each frame at DEBUG, the /trn with its
            # token too: its records are scrubbed while this client runs.
            self._ws_filter = _Scrub(self._token)
            logging.getLogger("websockets.client").addFilter(self._ws_filter)
        self._run_task = asyncio.create_task(self._run(), name="pikapro.showdown.run")
        self._tasks = [
            asyncio.create_task(self._send_loop(), name="pikapro.showdown.send"),
            asyncio.create_task(self._decide_loop(), name="pikapro.showdown.decide"),
            asyncio.create_task(self._watch_loop(), name="pikapro.showdown.watch"),
        ]
        await self._wait_logged_in()

    async def _wait_logged_in(self) -> None:
        assert self._logged_in is not None and self._run_task is not None
        login = asyncio.ensure_future(self._logged_in.wait())
        try:
            await asyncio.wait({login, self._run_task}, timeout=self.login_timeout, return_when=asyncio.FIRST_COMPLETED)
        finally:
            login.cancel()
        if not self._logged_in.is_set() and not self._run_task.done() and self._login_error is None:
            await self.close()
            raise LoginError(f"not logged in within {self.login_timeout} s of connecting")
        if self._login_error is not None:
            error = self._login_error
            await self.close()
            raise error
        if not self._logged_in.is_set():
            await self.close()
            raise ShowdownError("the connection closed before logging in")

    async def _run(self) -> None:
        """Connects, reads, and reconnects with backoff until closed. When
        it ends for any other reason than :meth:`close`, every battle and
        every waiting operation ends with the error (:meth:`_on_dead`)."""
        try:
            await self._connection_loop()
        except asyncio.CancelledError:
            raise
        except Exception as e:  # pragma: no cover (a bug: the loop contains its errors)
            self._event(logging.ERROR, "client error", error=repr(e))
            self._login_error = self._login_error or e
            raise
        finally:
            if not self._closing:
                self._on_dead()

    async def _connection_loop(self) -> None:
        while not self._closing:
            overdue: Optional[asyncio.TimerHandle] = None
            try:
                async with _connect(
                    self.config,
                    token=not self.guest,
                    open_timeout=self.open_timeout,
                    ping_interval=self.ping_interval,
                    ping_timeout=self.ping_interval,
                    max_size=MAX_FRAME,
                    max_queue=64,
                ) as ws:
                    self._ws = ws
                    self._generation += 1
                    self._opened_at = self._frame_at = time.monotonic()
                    self._stall_max = 0.0
                    assert self._connected is not None
                    self._connected.set()
                    self._event(logging.INFO, "connected", url=self.config.websocket_url)
                    if self.login_timeout is not None:
                        overdue = asyncio.get_running_loop().call_later(self.login_timeout, self._login_overdue, ws)
                    async for frame in ws:
                        self._frame_at = time.monotonic()
                        try:
                            self._on_frame(frame if isinstance(frame, str) else frame.decode("utf-8", "replace"))
                        except _UNCONTAINED:
                            raise
                        except BaseException as e:  # a frame the client can't read must not end the connection
                            self._event(logging.ERROR, "bad frame", error=repr(e), head=str(frame)[:120])
                        if self._login_error is not None:
                            return
            except _RedirectRefused as e:
                # Not a failure to retry: the server would redirect again.
                self._login_error = ShowdownError(f"{self.config.websocket_url} redirects elsewhere: {e}")
                self._event(logging.ERROR, "redirect refused", error=str(e))
            except (OSError, asyncio.TimeoutError) as e:
                self._event(logging.WARNING, "connection failed", error=repr(e))
            except Exception as e:  # websockets' ConnectionClosed, InvalidURI, InvalidHandshake...
                if not type(e).__module__.startswith("websockets"):
                    raise
                # How long it lived, how long since its last frame, and the
                # loop's longest stall meanwhile: a drop the keepalive saw
                # (``keepalive ping timeout``, the client's close frame), one the other
                # end or the network made (no close frame either way), or one
                # after a stalled loop.
                now = time.monotonic()
                self._event(logging.WARNING, "connection lost", error=repr(e),
                            age_s=round(now - self._opened_at, 1) if self._opened_at else None,
                            quiet_s=round(now - self._frame_at, 1) if self._frame_at else None,
                            loop_stall_max_s=round(self._stall_max, 1))
                close = getattr(e, "rcvd", None)  # the server's close frame (ConnectionClosed)
                code = getattr(close, "code", None)
                if isinstance(code, int) and code in self.config.final_close_codes:
                    reason = self._scrub(str(getattr(close, "reason", "") or ""))
                    self._login_error = ShowdownError(
                        f"the server closed the connection ({code}: {reason or 'no reason'}); not reconnecting")
                    self._event(logging.ERROR, "connection ended", code=code, reason=reason)
            finally:
                if overdue is not None:
                    overdue.cancel()
                self._ws = None
                was_logged_in = self.connected
                # A connection that stayed logged in for a while ends the
                # backoff; one the server closes right after its login
                # doesn't, so that such a server is retried (and sent the
                # login) ever more slowly, up to the cap.
                login_at, self._login_at = self._login_at, None
                if was_logged_in and login_at is not None and time.monotonic() - login_at >= BACKOFF_RESET_S:
                    self._delay = 1.0
                self._on_disconnect()
            if self._closing or self._login_error is not None:
                return
            if not self.reconnect or not was_logged_in and self._generation == 0:
                closed = "the connection closed" if self._generation else "could not connect to"
                self._login_error = ShowdownError(f"{closed} {self.config.websocket_url}")
                return
            self._reconnects += 1
            if self._reconnects > self.max_reconnects:
                self._login_error = ShowdownError(f"gave up after {self.max_reconnects} failed reconnects in a row")
                self._event(logging.ERROR, "giving up", reconnects=self.max_reconnects)
                return
            delay = self._delay
            self._delay = min(delay * 2, BACKOFF_MAX_S)
            self._event(logging.INFO, "reconnecting", delay=delay, attempt=self._reconnects)
            assert self._stop is not None
            try:  # close() ends the wait early
                await asyncio.wait_for(self._stop.wait(), delay)
            except asyncio.TimeoutError:
                pass

    async def _watch_loop(self) -> None:
        """Watches the event loop: a wake-up :data:`LOOP_STALL_WARN` seconds
        late or more is logged (``event loop stalled``), and the longest
        since the connection opened goes into its ``connection lost``
        record. Decisions run in worker threads (``SessionHandler``'s
        ``threaded``), so the loop keeps answering the keepalive pings; a
        stall says something held it."""
        loop = asyncio.get_running_loop()
        while True:
            t = loop.time()
            await asyncio.sleep(LOOP_TICK)
            late = loop.time() - t - LOOP_TICK
            if late > self._stall_max:
                self._stall_max = late
            if late >= LOOP_STALL_WARN:
                self._event(logging.WARNING, "event loop stalled", seconds=round(late, 1))
            if self.config.auto_battles:
                try:
                    self.close_unlisted(UNLISTED_GRACE_S)
                except _UNCONTAINED:
                    raise
                except BaseException as e:  # pragma: no cover (a bug): the watch must go on
                    self._event(logging.ERROR, "unlisted rooms check failed", error=repr(e))

    def _login_overdue(self, ws: Any) -> None:
        """Closes ``ws`` if it still isn't logged in (``login_timeout``)."""
        if ws is self._ws and not self.connected and not self._closing:
            self._event(logging.WARNING, "login timed out", timeout=self.login_timeout)
            self._close_later(ws)

    def _close_later(self, ws: Any) -> None:
        """Closes ``ws`` in a task the client keeps (one only referenced by
        the event loop can be collected mid-close), whose failure is
        logged."""
        task = asyncio.ensure_future(self._close_ws(ws))
        self._closers.add(task)
        task.add_done_callback(self._closers.discard)

    async def _close_ws(self, ws: Any) -> None:
        """Closes ``ws``; a failure (the connection already broken) is
        logged, never raised."""
        try:
            await ws.close()
        except Exception as e:  # noqa: BLE001 - closing a broken connection
            self._event(logging.DEBUG, "close failed", error=repr(e))

    def _on_dead(self) -> None:
        """The connection ended for good (not by :meth:`close`): every
        battle ends as disconnected, and the operations waiting on the
        connection fail with the error."""
        error = self._login_error or ShowdownError("the connection closed")
        self._fatal = error
        self._event(logging.ERROR, "connection ended", error=str(error))
        for room in list(self.rooms.values()):
            self._finish(room, f"disconnected ({error})")
        for series in list(self.series.values()):
            self._finish_series(series, f"disconnected ({error})")
        waiting = [*self._popups, *(w.future for w in self._waiters), *(f for _, f in self._pm_waiters)]
        for fut in waiting:
            if not fut.done():
                fut.set_exception(error)
                fut.exception()  # retrieved: an operation that gave up won't await it
        if self._decide_wake is not None:
            self._decide_wake.set()

    def _check_alive(self) -> None:
        """Raises the error that ended the connection, if it did."""
        if self._fatal is not None:
            raise self._fatal
        if self._run_task is None:
            raise ShowdownError("the client is not connected: call connect() (or use `async with`)")

    def _closed_error(self, otherwise: str = "the connection closed") -> ShowdownError:
        """The error for an operation cut off by the connection's end: said
        plainly when :meth:`close` ended it."""
        return ShowdownError("the client was closed (close())" if self._closing else otherwise)

    async def _guard(self, aw: Any) -> Any:
        """Awaits ``aw``, unless the connection ends for good first: then
        raises its error."""
        assert self._run_task is not None
        task = asyncio.ensure_future(aw)
        run = self._run_task
        try:
            await asyncio.wait({task, run}, return_when=asyncio.FIRST_COMPLETED)
        except BaseException:
            task.cancel()
            raise
        if task.done():
            return task.result()
        task.cancel()
        try:
            await task
        except BaseException:
            pass
        raise self._fatal or self._login_error or self._closed_error()

    def _on_disconnect(self) -> None:
        assert self._connected is not None and self._logged_in is not None and self._outbox is not None
        self._connected.clear()
        self._logged_in.clear()
        self._outbox.clear()  # choices for the old connection are stale; rooms resend requests on rejoin
        self.searching = []
        self.games = {}
        self._cancel_mark = None
        self.challenges.clear()
        for fut in [*self._popups, *(w.future for w in self._waiters)]:
            if not fut.done():
                fut.set_exception(_Disconnected("disconnected"))
                fut.exception()  # retrieved: an operation that gave up won't await it
        self._joining.clear()
        if not self.reconnect or self._closing:
            for room in list(self.rooms.values()):
                self._finish(room, "disconnected")
            for series in list(self.series.values()):
                self._finish_series(series, "disconnected")

    async def close(self) -> None:
        """Leaves cleanly: cancels searches and challenges, forfeits (or
        leaves) unfinished battles, waits briefly for them to end, and
        closes the connection."""
        if self._run_task is None:
            return
        if self.connected and self._outbox is not None:
            if self.searching:
                self._send(["|/cancelsearch"])
            for (frm, to), _ in list(self.challenges.items()):
                if frm == self.user_id:
                    self._send([f"|/cancelchallenge {to}"])
            # A series is forfeited in its room (``BestOfGame.forfeit``, which
            # forfeits the game being played): a game's own forfeit would
            # only start the next game.
            live_series = [s for s in self.series.values() if not s.ended]
            live = [r for r in self.rooms.values() if not r.ended and r.series is None]
            if self.forfeit_on_exit:
                for series in live_series:
                    self._send([f"{series.id}|/forfeit"], urgent=True)
                # A game whose series room the client hasn't read yet is
                # forfeited in the series room it names, too.
                forfeited = {s.id for s in live_series}
                for target in dict.fromkeys(r.series_id or r.id for r in live):
                    if target not in forfeited:
                        self._send([f"{target}|/forfeit"], urgent=True)
            if (live or live_series) and self.forfeit_on_exit:
                ends = [r._end for r in [*live, *live_series] if r._end is not None]
                if ends:
                    await asyncio.wait(ends, timeout=5.0)
            try:
                await asyncio.wait_for(self._outbox.idle.wait(), timeout=5.0)
            except asyncio.TimeoutError:
                pass
        self._closing = True
        if self._stop is not None:
            self._stop.set()  # ends a reconnect backoff at once
        for room in list(self.rooms.values()):
            self._finish(room, "forfeited on exit" if self.forfeit_on_exit else "left on exit")
        for series in list(self.series.values()):
            self._finish_series(series, "forfeited on exit" if self.forfeit_on_exit else "left on exit")
        if self._ws is not None:
            try:
                await asyncio.wait_for(self._ws.close(), timeout=5.0)
            except Exception:  # pragma: no cover
                pass
        # Whatever the connection task waits on now (a connect, a backoff),
        # it stops.
        tasks = [*self._tasks, self._run_task]
        if self._login_task is not None:
            tasks.append(self._login_task)
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        self._tasks = []
        self._run_task = None
        self._login_task = None
        if self._ws_filter is not None:
            logging.getLogger("websockets.client").removeFilter(self._ws_filter)
            self._ws_filter = None
        self._event(logging.INFO, "closed")

    # -- hooks for subclasses -------------------------------------------------------------
    #
    # A bot subclasses the client to read more of what the server sends, or
    # to hold a decision. These do nothing by default; the client calls them
    # on the event loop, and an exception in one is logged (``hook error``),
    # never raised into the connection.

    def on_battle_lines(self, room_id: str, lines: List[str]) -> None:
        """Called with each frame of battle room ``room_id``'s lines, after
        the client read them (:attr:`rooms` ``[room_id]`` has them, while
        the battle lasts). It also gets the frames of a room that ended or
        that the client doesn't play: a rated battle's rating lines come
        after its end, while the client is still in the room
        (``leave_delay``). A room that waited to be claimed (a best-of
        series' game, before its series room was read) comes again with no
        lines when it is. A room Showdown renames (a player changed the
        battle's privacy) comes once more under its old id, with the
        ``|noinit|rename|NEWID|TITLE`` line; it is ``rooms[NEWID]`` from
        then on."""

    def on_pm(self, line: str) -> None:
        """Called with each ``|pm|`` line after the client read it: a
        challenge's, a user's message, or a message of the client's that the
        server refused."""

    def hold_decision(self, room: BattleRoom) -> bool:
        """Called when ``room`` has a request to decide, before the handler
        is asked: ``True`` holds it (nothing is asked or sent) until
        :meth:`release_decision`. By default, ``False``; an exception counts
        as ``False``. (Open Team Sheets' wait, ``open_team_sheets="mutual"``,
        holds a decision before this is asked.)"""
        return False

    def release_decision(self, room_id: str) -> None:
        """Asks for the decision :meth:`hold_decision` held in ``room_id``,
        if the room still waits for one (:meth:`hold_decision` is asked
        again first)."""
        room = self.rooms.get(room_id)
        if room is not None and room.pending:
            self._queue_decision(room)

    def send_room(self, room_id: str, text: str) -> bool:
        """Sends ``text`` in room ``room_id`` (battle chat, or a command
        there), throttled with the rest, unless the client is closing or
        has no connection: whether it was queued. Raises ``ValueError`` for
        a line break or another control character, as :meth:`send` does."""
        if not is_command_safe(text) or not is_command_safe(room_id) or "|" in room_id:
            raise ValueError("a message is one line: no line breaks or control characters")
        if self._closing or self._ws is None or self._outbox is None:
            return False
        self._send([f"{room_id}|{text}"])
        return True

    def _hook(self, name: str, fn: Callable[..., Any], *args: Any) -> Any:
        """Calls a subclass hook: its value, or ``None`` when it raised
        (logged)."""
        try:
            return fn(*args)
        except _UNCONTAINED:
            raise
        except BaseException as e:  # a subclass bug must not end the connection
            self._event(logging.ERROR, "hook error", hook=name, error=repr(e))
            return None

    # -- sending -------------------------------------------------------------------------

    def _send(self, messages: List[str], urgent: bool = False) -> None:
        """Queues ``ROOM|TEXT`` messages, sent in order and throttled. A
        command the server doesn't take (:attr:`ServerConfig.commands`)
        isn't sent (logged once per command)."""
        assert self._outbox is not None
        allowed = self.config.commands
        if allowed is not None:
            kept = []
            for message in messages:
                command = _command(message)
                if command is None or command in allowed:
                    kept.append(message)
                    continue
                level = logging.DEBUG if command in self._unsent else logging.INFO
                self._unsent.add(command)
                self._event(level, "command not sent", command=command, reason="the server doesn't take it")
            if not kept:
                return
            messages = kept
        self._outbox.put(messages, urgent)

    async def send(self, text: str, room: str = "") -> None:
        """Sends one message (``text`` to ``room``), throttled with the
        rest. Raises ``ValueError`` for a line break or another control
        character (Showdown would run each line as its own command)."""
        if not is_command_safe(text) or not is_command_safe(room) or "|" in room:
            raise ValueError("a message is one line: no line breaks or control characters")
        self._send([f"{room}|{text}"])

    async def _send_loop(self) -> None:
        assert self._outbox is not None and self._logged_in is not None
        box = self._outbox
        while True:
            group = box.sending = box.take()
            if group is None:
                box.idle.set()
                box.wake.clear()
                await box.wake.wait()
                continue
            # Only once logged in (the login's /trn goes straight to the
            # socket). A group belongs to the connection it was taken on:
            # after a reconnect its remaining lines are stale (a /search
            # after a /utm the server forgot), and the operation that sent
            # them sends them again.
            generation = self._generation if self.connected else None
            for at in range(len(group)):
                await self._logged_in.wait()
                message = group[at]  # (read after the wait: a rename rewrites the group)
                if generation is None:
                    generation = self._generation
                if self._generation != generation:
                    self._event(logging.DEBUG, "stale messages dropped", message=_redact(message))
                    break
                wait = box.last + box.interval - time.monotonic()
                if wait > 0:
                    await asyncio.sleep(wait)
                    message = group[at]
                ws = self._ws
                if ws is None or self._generation != generation:
                    break
                try:
                    await ws.send(message)
                except Exception as e:
                    self._event(logging.WARNING, "send failed", error=repr(e))
                    break
                box.last = time.monotonic()
                if message == "|/cancelsearch":
                    self._cancel_mark = self._search_updates
                self._event(logging.DEBUG, ">>", message=_redact(message))

    # -- receiving -------------------------------------------------------------------------

    def _on_frame(self, frame: str) -> None:
        room, lines = split_frame(frame)
        if self.log.isEnabledFor(logging.DEBUG):
            self._event(logging.DEBUG, "<<", room=room, lines=len(lines), head=lines[0][:120] if lines else "")
        if room.startswith("battle-"):
            # The id goes back into commands (``ROOM|/choose ...``): a `|`
            # or a control character in it would make another command.
            if not _ROOM_ID.fullmatch(room):
                self._event(logging.WARNING, "bad room id", room=room[:120])
                return
            room, lines = self._follow_rename(room, lines)
            if room is not None:
                self._on_battle(room, lines)
            return
        if room.startswith("game-bestof"):
            if not _SERIES_ID.fullmatch(room):
                self._event(logging.WARNING, "bad room id", room=room[:120])
                return
            room, lines = self._follow_rename(room, lines)
            if room is not None:
                self._on_series(room, lines)
            return
        for line in lines:
            self._on_global(line)

    def _follow_rename(self, room_id: str, lines: List[str]) -> Tuple[Optional[str], List[str]]:
        """A battle or series room's frame, with a rename in it followed:
        ``(the room's id now, the frame's other lines)``, or ``(None, [])``
        when nothing is left to read (the frame was the rename alone, as
        Showdown sends it). Several renames in a frame are followed one
        after the other."""
        if not any(_room_renamed(l) is not None for l in lines):
            return room_id, lines
        rest = []
        for line in lines:  # each rename in turn: the next one is of the new id
            new = _room_renamed(line)
            if new is None:
                rest.append(line)
            elif not self._rename(room_id, new, line):
                return None, []
            else:
                room_id = new
        return (room_id, rest) if rest else (None, [])

    def _rename(self, old: str, new: str, line: str) -> bool:
        """Showdown renamed room ``old`` to ``new`` (``Room.rename``: a
        player changed a battle's privacy, which adds or removes the id's
        password; a series' privacy renames its room and each game's). The
        room goes on under the new id with everything it had: its state
        and lines, its handler, its place in its series, the messages
        waiting to be sent to it. A new id the client can't use (not a
        room id of the same kind, or another room's) ends the room.
        Returns whether the room goes on."""
        is_series = old.startswith("game-")
        table: Dict[str, Any] = self.series if is_series else self.rooms
        room = table.get(old)
        self._joining.discard(old)
        if room is None:
            late = None if is_series else self._leaving.pop(old, None)
            if late is not None and _ROOM_ID.fullmatch(new):
                # Finished, not left yet (``leave_delay``): its ``/leave``
                # goes to the id it has when it is sent.
                late.id = new
                self._leaving[new] = late
                if self._outbox is not None:
                    self._outbox.rename(old, new)
                return False
            self._event(logging.DEBUG, "rename of an unknown room", room=old, to=new[:120])
            return False
        if len(room.lines) < MAX_LINES:
            room.lines.append(line)
        if new == old:
            return True
        pattern = _SERIES_ID if is_series else _ROOM_ID
        if not pattern.fullmatch(new) or new in self.rooms or new in self.series:
            self._event(logging.WARNING, "bad room id", room=old, renamed_to=new[:120])
            reason = "room renamed to an id the client can't use"
            if is_series:
                self._finish_series(room, None if room.ended else reason)
            else:
                self._finish(room, None if room.ended else reason)
            return False
        # In place, the order kept: callers may hold the dict.
        items = [(new if k == old else k, v) for k, v in table.items()]
        table.clear()
        table.update(items)
        room.id = new
        handlers: Dict[str, Any] = self._series_handlers if is_series else self._room_handlers
        if old in handlers:
            handlers[new] = handlers.pop(old)
        if is_series:
            for game in self.rooms.values():
                if game.series_id == old:
                    game.series_id = new
            for result in room.results.values():
                result.series = new
        else:
            for series in self.series.values():
                for n, game_id in series.games.items():
                    if game_id == old:
                        series.games[n] = new
            timer = room._ots_timer
            if timer is not None:  # keyed by the room's id (``_ots_timeout``)
                timer.cancel()
                room._ots_timer = asyncio.get_running_loop().call_at(timer.when(), self._ots_timeout, new)
        if self._outbox is not None:
            self._outbox.rename(old, new)
        self._event(logging.INFO, "room renamed", room=new, was=old)
        if not is_series:
            self._hook("on_battle_lines", self.on_battle_lines, old, [line])
        return True

    def _on_global(self, line: str) -> None:
        kind = line.split("|", 2)[1] if line.startswith("|") else ""
        if kind == "challstr":
            if self._login_task is not None and not self._login_task.done():
                self._login_task.cancel()
            self._login_task = asyncio.create_task(self._login(), name="pikapro.showdown.login")
        elif kind == "updateuser":
            parts = line.split("|")
            # USER: the rank symbol (a space for none), then the name,
            # with "@!" appended when away.
            name = parts[2][1:] if len(parts) > 2 else ""
            named = len(parts) > 3 and parts[3] == "1"
            if name.endswith("@!"):
                name = name[:-2]
            self.name = name
            self._event(logging.INFO, "user", name=name, named=named)
            wanted = to_id(self.username or "")
            if (named and to_id(name) == wanted) or (self.guest and not named):
                if self._logged_in is not None and not self._logged_in.is_set():
                    self._logged_in.set()
                    self._on_logged_in()
        elif kind == "nametaken":
            parts = line.split("|", 3)
            self._login_error = LoginError(self._scrub(parts[3]) if len(parts) > 3 else "name taken")
            self._event(logging.ERROR, "login failed", reason=str(self._login_error))
        elif kind == "formats":
            self.formats = parse_formats(line)
        elif kind == "updatesearch":
            data = _json_object(line[len("|updatesearch|") :])
            searching = data.get("searching")
            self.searching = [to_id(f) for f in searching if isinstance(f, str)] if isinstance(searching, list) else []
            games = data.get("games")
            self.games = dict(games) if isinstance(games, dict) else {}
            self._search_updates += 1
            if self.config.auto_battles:
                self._note_listed()
            self._on_searching()
        elif kind == "updatechallenges":  # older servers
            data = _json_object(line[len("|updatechallenges|") :])
            received = data.get("challengesFrom")
            received = received if isinstance(received, dict) else {}
            before = self.challenges
            self.challenges = {
                (to_id(frm), self.user_id): to_id(fmt) for frm, fmt in received.items() if isinstance(fmt, str)
            }
            to = data.get("challengeTo")
            if isinstance(to, dict) and isinstance(to.get("to"), str) and isinstance(to.get("format"), str):
                key = (self.user_id, to_id(to["to"]))
                if key not in before:
                    self._last_challenge = time.monotonic()
                self.challenges[key] = to_id(to["format"])
            for key, fmt in self.challenges.items():  # the new ones (the list is sent again on each change)
                if key[1] == self.user_id and before.get(key) != fmt and self._incoming is not None:
                    self._challenged(key[0], fmt)
        elif kind == "pm":
            self._on_pm(line)
        elif kind == "popup":
            text = line[len("|popup|") :].replace("||", "\n")
            if self.config.auto_battles:
                self._auto_popup(text)
            else:
                self._event(logging.WARNING, "popup", text=text)
            if self.on_popup is not None:
                try:
                    self.on_popup(text)
                except Exception as e:  # noqa: BLE001 - the caller's hook must not break the connection
                    self._event(logging.ERROR, "on_popup error", error=repr(e))
            if _refuses(text):  # else unrelated to a start (a notice): it waits on
                for fut in self._popups:
                    if not fut.done():
                        fut.set_result(text)
                # A search already accepted can still be refused when it is
                # matched (``Rooms.createBattle``): kept for its drop check.
                for w in self._waiters:
                    if w.search is not None and w.searched and not w.future.done():
                        w.refusal = text
        elif kind in ("customgroups", "usercount", "queryresponse", ""):
            pass
        else:
            self._event(logging.DEBUG, "unhandled", line=line[:200])

    def _on_pm(self, line: str) -> None:
        self._read_pm(line)
        self._hook("on_pm", self.on_pm, line)

    def _read_pm(self, line: str) -> None:
        challenge = parse_challenge_pm(line)
        if challenge is not None:
            frm, to, fmt = challenge
            key = (frm, to)
            if fmt is None:  # sent as (user who acted, other user): either direction
                self.challenges.pop(key, None)
                self.challenges.pop((to, frm), None)
            else:
                new = key not in self.challenges
                self.challenges[key] = fmt
                if new and frm == self.user_id:
                    # Showdown stamps ``user.lastChallenge`` as it posts the
                    # challenge (after validating the team): the cooldown
                    # counts from here, not from sending it.
                    self._last_challenge = time.monotonic()
                if new and to == self.user_id and self._incoming is not None:
                    self._challenged(frm, fmt)
        else:
            # A PM the server refused comes back to the sender as one: an
            # error (``/error``) or an error box (``message-error``). Logged,
            # so a bot sees when its messages don't reach anyone.
            parts = line.split("|", 4)
            body = parts[4] if len(parts) > 4 else ""
            if body.startswith("/error") or "message-error" in body:
                self._event(logging.WARNING, "pm refused", line=line[:300])
            else:
                self._event(logging.DEBUG, "pm", line=line[:200])
        for pred, fut in self._pm_waiters:
            if not fut.done() and pred(line):
                fut.set_result(line)

    def _challenged(self, frm: str, fmt: str) -> None:
        """A new challenge to the client: queued for :meth:`accept_challenges`, or
        only noted on a server that starts the battle itself."""
        assert self._incoming is not None
        if self.config.auto_battles:
            self._event(logging.INFO, "challenged", by=frm, format=fmt, note="the server starts the battle itself")
            return
        self._event(logging.INFO, "challenged", by=frm, format=fmt)
        self._incoming.put_nowait((frm, fmt))

    def _auto_popup(self, text: str) -> None:
        """A ``|popup|`` on a server that starts battles itself (VGC
        Arena's: no team for the ladder, a team rejected, a pair's battle
        limit, an unsupported command, a command before logging in, a
        ``/search``, which the arena answers but never acts on), logged by
        what it says. A rejected team of a rotation makes way for the next
        one (at most five in a row)."""
        low = text.lower()
        # "Send a team legal in [FORMAT] with /utm first." (or "... legal team with /utm ...")
        if "legal team with /utm" in low or (low.startswith("send a team") and "with /utm first" in low):
            self._event(logging.WARNING, "no team", text=text, uploaded=self._utm is not None)
            return
        if "no queue to join" in low or "no ladder search" in low:
            # (the client never sends /search on the arena: ServerConfig.commands)
            self._event(logging.WARNING, "no ladder search", text=text,
                        note="the arena pairs bots online with a team itself")
            return
        if "team was rejected" in low:
            self._event(logging.ERROR, "team rejected", text=text)
            team = self._auto_team
            self._utm_rejections += 1
            if team is not None and callable(team[0]) and self._utm_rejections < TEAM_TRIES and self.connected:
                self._utm_played = True  # the rotation moves on
                self._auto_upload(*team, force=True)
            return
        if "paused it" in low:
            # "This bot's owner paused it: the arena starts nothing new for it until they resume it ..."
            # (the arena's rules for taking a bot offline): the battles in progress go on; not an error.
            self._event(logging.INFO, "paused by its owner", text=text)
            return
        if "starts nothing new" in low:
            # The answer to /cancelsearch ("The arena starts nothing new for this bot: it plays what it is playing,
            # and any return game it owes. Once |updatesearch| lists no games ..."): not an error.
            self._event(logging.INFO, "search cancelled", text=text)
            return
        if "have played each other" in low:
            self._event(logging.WARNING, "pair limit", text=text)
        elif "is not supported" in low:
            self._event(logging.WARNING, "unsupported command", text=text)
        else:
            self._event(logging.WARNING, "popup", text=text)

    def _auto_upload(self, team: Team, fmt: str, force: bool = False) -> None:
        """Uploads the team (``/utm``) on a server that starts battles
        itself: the first time, and for a rotation (``team`` a callable)
        once a battle was played with the last one. The server plays the
        team uploaded when the battle starts."""
        if team is None:
            return  # a random-team format: the server gives the team
        if self._utm is not None and not force:
            if callable(team) and (not self._utm_played or not self.rotate_on_wait):
                return
            if not callable(team) and self._utm == self._team(team, fmt):
                return
        packed = self._auto_pack(team, fmt)
        if packed is None:
            return  # the team uploaded before stays
        if packed != self._utm:
            self._utm_prev, self._utm = self._utm, packed
        self._utm_played = False
        self._event(logging.INFO, "team uploaded", species=[p.split("|")[1] or p.split("|")[0] for p in packed.split("]")])
        self._send([f"|/utm {packed}"])
        self._utm_sent.append((time.monotonic(), packed))

    def upload_team(self, team: Team = None, format: Optional[str] = None) -> None:
        """Uploads a team now (``/utm``) on a server that starts battles
        itself, even if it is the one up: ``team`` (a rotation callable
        gives its next team), or by default the team and format of the
        running :meth:`accept_challenges` (:attr:`auto_team`). The server
        plays the team up when it pairs the next battle. With
        :attr:`rotate_on_wait` ``False`` this is how a rotation moves on.
        Errors as :meth:`accept_challenges`' upload (a rotation tries its
        next teams)."""
        if team is None and self._auto_team is not None:
            team, default_format = self._auto_team
            format = format or default_format
        self._auto_upload(team, format or "", force=True)

    @property
    def uploaded_team(self) -> Optional[str]:
        """The team uploaded last (packed), on a server that starts battles
        itself; ``None`` before the first upload."""
        return self._utm

    @property
    def uploads(self) -> Tuple[Tuple[float, str], ...]:
        """The last uploads (at most :data:`UTM_KEPT`), oldest first, each
        with when it went out (``time.monotonic()``): with several battles
        at once a battle may play any of the last few
        (:attr:`BattleRoom.opened` says when its room opened)."""
        return tuple(self._utm_sent)

    @property
    def auto_team(self) -> Optional[Tuple[Team, str]]:
        """The team (a fixed team or a rotation callable) and format of the
        running or last :meth:`accept_challenges` on a server that starts
        battles itself; ``None`` before one."""
        return self._auto_team

    def _auto_pack(self, team: Team, fmt: str) -> Optional[str]:
        """``team`` packed for an upload. A rotation (``team`` a callable)
        whose team fails (it raises, or the team can't be packed) moves on
        to its next, up to five teams; if all five fail, ``None`` keeps the
        team uploaded before, and with none uploaded yet the error is
        raised. A fixed team's failure is raised."""
        tries = TEAM_TRIES if callable(team) else 1
        for attempt in range(1, tries + 1):
            try:
                return self._team(team, fmt)
            except _UNCONTAINED:
                raise
            except BaseException as e:  # a rotation's bad team: the next one
                if not callable(team) or (attempt == tries and self._utm is None):
                    raise
                self._event(logging.ERROR, "team failed", error=repr(e), attempt=attempt,
                            note="the rotation's next team" if attempt < tries else "the last upload stays")
        return None

    def _auto_room_team(self, room: BattleRoom) -> Optional[str]:
        """The team a battle the server started plays, as its first request
        shows it: the one uploaded last, or the one before (the battle
        started before the new upload landed); ``None`` for the server's
        own (a random-team ladder) or a team the client didn't upload.
        Two teams the request's sets fit alike (the same Pokémon, moves,
        items and abilities, in the same order) are told apart by their
        stats, as the engine computes them. With several battles at once an
        older upload may be the one (``earlier``): uploads follow one
        another within seconds."""
        request = room.request or {}
        ups = [("uploaded", self._utm), ("previous", self._utm_prev)]
        ups += [("earlier", p) for _, p in reversed(self._utm_sent)]
        known: set = set()
        fits = []
        for which, packed in ups:
            if packed is None or packed in known:
                continue
            known.add(packed)
            if _team_shows(packed, request):
                fits.append((which, packed))
        if len(fits) > 1:
            fits = [f for f in fits if _stats_show(f[1], request, room.play_format) is not False] or fits
        if fits:
            which, packed = fits[0]
            if packed is self._utm:
                self._utm_played = True
                self._utm_rejections = 0
            self._event(logging.INFO, "battle team", room=room.id, team=which)
            return packed
        self._event(logging.INFO, "battle team", room=room.id, team="the server's")
        return None

    async def _login(self) -> None:
        try:
            if self.guest:
                if self._logged_in is not None and not self._logged_in.is_set():
                    self._logged_in.set()
                    self._on_logged_in()
                return
            assert self.username is not None
            ws = self._ws
            assertion = self._assertion()
            # Straight to this connection, not through the outbox: the
            # outbox sends only once logged in.
            if ws is not None and ws is self._ws:
                message = f"|/trn {self.username},0,{assertion}"
                await ws.send(message)
                if self._outbox is not None:
                    self._outbox.last = time.monotonic()
                self._event(logging.DEBUG, ">>", message=_redact(message))
        except Exception as e:
            self._login_error = e if isinstance(e, ShowdownError) else LoginError(self._scrub(f"login failed: {e!r}"))
            self._event(logging.ERROR, "login failed", reason=str(self._login_error))
            if self._ws is not None:
                await self._close_ws(self._ws)

    def _assertion(self) -> str:
        """The ``/trn`` assertion: the token on a token login
        (:attr:`ServerConfig.token_login`), else empty."""
        if not self.config.token_login:
            return ""
        if not self._token:
            raise LoginError("no token to log in with")
        return self._token

    def _on_logged_in(self) -> None:
        self._event(logging.INFO, "logged in", name=self.name)
        # A login ends a run of failed reconnects. The backoff stays until
        # the connection has lasted (``_connection_loop``).
        self._reconnects = 0
        self._login_at = time.monotonic()
        if self.avatar:
            self._send([f"|/avatar {self.avatar}"])
        if self._search_cancelled:
            # A login undoes ``/cancelsearch`` (VGC Arena's rules): sent
            # again, before the team.
            self._send(["|/cancelsearch"])
        if self.config.auto_battles and self._utm is not None:
            # A server that starts battles itself forgets the team with the
            # connection: sent again (it is the one a battle gets).
            self._send([f"|/utm {self._utm}"])
        # After a reconnect: rejoin the battles still being played (the
        # server resends each room's log and request; one that rejoins
        # them itself doesn't take /join, which the client then drops).
        for series in self.series.values():
            if not series.ended:
                self._send([f"|/join {series.id}"])
        for room in self.rooms.values():
            if not room.ended:
                self._send([f"|/join {room.id}"])

    # -- battle rooms -----------------------------------------------------------------------

    def _on_battle(self, room_id: str, lines: List[str]) -> None:
        self._read_battle(room_id, lines)
        self._hook("on_battle_lines", self.on_battle_lines, room_id, lines)

    def _note_listed(self) -> None:
        """A server that starts battles itself: an ``|updatesearch|`` came.
        Each battle room it lists is in progress; one it doesn't is marked
        (``_unlisted``: since when) for :meth:`close_unlisted`. A list whose
        keys aren't room ids marks nothing (no room is taken as over on a
        list the client can't read)."""
        keys = set(self.games)
        if keys and not any(_ROOM_ID.fullmatch(str(k)) for k in keys):
            return
        now = time.monotonic()
        for room in self.rooms.values():
            if room.ended or room.series is not None:
                continue
            if room.id in keys:
                room._unlisted = None
            elif room._unlisted is None:
                room._unlisted = now

    def close_unlisted(self, grace: float = UNLISTED_GRACE_S) -> List[str]:
        """On a server that starts battles itself (VGC Arena): ends each
        battle room that its last ``|updatesearch|`` didn't list, and that
        got no frame for ``grace`` seconds since: the battle ended while
        the client couldn't see it (during a reconnect, whose rooms the
        arena sends again only while they are in progress). Its result is
        unfinished, with that reason (the arena's record of the battle
        tells who won). Called every second while connected; ``grace=0``
        ends at once every room the last list left out (it listed no games
        after :meth:`cancel_search`, say). Returns the rooms ended."""
        if not self.config.auto_battles or not self.connected:
            return []
        keys = set(self.games)
        now = time.monotonic()
        gone = [r for r in list(self.rooms.values()) if not r.ended and r._unlisted is not None
                and r.id not in keys and now - r._unlisted >= grace]
        for room in gone:
            self._event(logging.WARNING, "battle over unseen", room=room.id, turn=room.turn,
                        note="the server lists it no more: it ended while the client couldn't see it")
            self._finish(room, UNLISTED_REASON)
        return [r.id for r in gone]

    def _read_battle(self, room_id: str, lines: List[str]) -> None:
        room = self.rooms.get(room_id)
        if room is not None:
            room._unlisted = None  # (a frame: in progress, whatever the last list said)
        if lines and lines[0] == "|init|battle":
            self._joining.discard(room_id)
            if room is not None:  # joined again (after a reconnect): the log starts over
                handler = self._room_handlers.get(room_id)
                if handler is not None:
                    self._handler_call(room, "close", handler.close, room)
                fresh = BattleRoom(room_id, self.name or "")
                fresh.play_format = self.config.play_format(fresh.format)
                fresh._claimed, fresh._end, fresh.team = room._claimed, room._end, room.team
                fresh.invalid, fresh.unavailable, fresh.fallbacks = room.invalid, room.unavailable, room.fallbacks
                fresh.stale, fresh._opened = room.stale, room._opened
                fresh.series, fresh.series_id, fresh.game = room.series, room.series_id, room.game
                for k in _OTS_CARRIED:
                    setattr(fresh, k, getattr(room, k))
                self.rooms[room_id] = room = fresh
                if handler is not None and not self._handler_call(room, "open", handler.open, room)[0]:
                    self._drop(room)
                    return
            else:
                room = BattleRoom(room_id, self.name or "")
                room.play_format = self.config.play_format(room.format)
                room._end = asyncio.get_running_loop().create_future()
                room._opened = time.monotonic()
                self.rooms[room_id] = room
                self._event(logging.INFO, "battle", room=room_id)
        if room is None:
            if not lines or _room_closed(lines[0]) is not None:
                return
            self._event(logging.DEBUG, "lines for an unknown room", room=room_id, head=lines[0][:120])
            return
        for line in lines:
            room.observe(line, self.user_id)
            if line.startswith("|error|"):
                self._on_error(room, line[len("|error|") :])
        feed = lines
        closed = next((c for c in map(_room_closed, lines) if c is not None), None)
        unclaimed = room_id not in self._room_handlers
        if unclaimed:
            # Claimed before the sheets are answered: the operation that
            # started the room may answer them its own way (``AcceptPlan``).
            self._claim(room)
        self._open_sheets(room)
        if unclaimed:
            if room_id not in self._room_handlers:
                if closed is not None:
                    # Gone before an operation or a handler had it: it ends
                    # here (nothing else would end it).
                    self._finish(room, None if room.ended else closed)
                return  # not claimed yet: the handler gets the lines so far when it is
            if not self._handler_call(room, "open", self._room_handlers[room_id].open, room)[0]:
                self._drop(room)
                return
            feed = list(room.lines)
            if self.timer:
                self._send([f"{room_id}|/timer on"])
        handler = self._room_handlers[room_id]
        fed, can_act = self._handler_call(room, "feed", handler.feed, room, feed)
        if not fed:  # the handler failed: its view of the battle is lost
            self._drop(room)
            return
        if room.ended or closed is not None:
            self._finish(room, None if room.ended else closed)
            return
        if room.pending and (can_act or room._retry or room._ots_deferred):
            if room._ots_hold:  # until Open Team Sheets is settled (``_ots_release``)
                room._ots_deferred = True
            else:
                room._ots_deferred = False
                self._queue_decision(room)

    def _handler_call(self, room: BattleRoom, what: str, fn: Callable[..., Any], *args: Any) -> Tuple[bool, Any]:
        """Calls a handler method for ``room``: ``(True, its value)``, or
        ``(False, None)`` when it raised (logged, and kept as the room's
        error)."""
        try:
            return True, fn(*args)
        except _UNCONTAINED:
            raise
        except BaseException as e:  # a handler bug must not end the client, nor other battles
            self._event(logging.ERROR, "handler error", room=room.id, method=what, error=repr(e))
            room._error = room._error or f"handler error in {what}: {e!r}"
            return False, None

    def _guarded(self, fn: Callable[..., Any], *args: Any, **fields: Any) -> Tuple[bool, Any]:
        """Calls a handler's method or an operation's hook: ``(True, its
        value)``, or ``(False, None)`` when it raised (logged as a handler
        error with ``fields``): its bug must not end the client."""
        try:
            return True, fn(*args)
        except _UNCONTAINED:
            raise
        except BaseException as e:
            self._event(logging.ERROR, "handler error", **fields, error=repr(e))
            return False, None

    def _drop(self, room: BattleRoom) -> None:
        """Ends a battle the handler can't play: forfeits it and records it
        with the handler's error as the reason."""
        if not room.ended and self._ws is not None and not self._closing:
            self._send([f"{room.id}|/forfeit"], urgent=True)
        self._finish(room, room._error or "handler error")

    def _open_sheets(self, room: BattleRoom) -> None:
        """Answers an Open Team Sheets offer (``open_team_sheets``), and in
        ``"mutual"`` mode holds the room's decision until the question is
        settled. Runs after each frame of the room's lines (``room`` has
        read them): Showdown takes answers only before turn 1."""
        if not room.ots_offered:
            return
        if room.ended or room.turn > 0:
            self._ots_release(room, "ended" if room.ended else "battle started")
            return
        if room._ots_mode is True:
            # The operation's own "accept at once" (``AcceptPlan``): the
            # decision waits for the opponent's answer, so that sheets they
            # open come before it.
            if not room._ots_answered and room.ots_answer is None and room.ots_foe is not False:
                self._ots_send(room, True)
                self._ots_hold_for(room, OTS_SHOW_GRACE if room.ots_foe else self.ots_wait)
            if room._ots_hold and (room.ots_open or room.ots_foe is False):
                self._ots_release(room, "sheets open" if room.ots_open else "closed")
            return
        mode = self.open_team_sheets if room._ots_mode is None else room._ots_mode
        if room._ots_mode is None and mode in (None, "mutual") and (room.series is not None or room.series_id is not None):
            mode = True  # a series' games: the environment's series mode plays with open sheets
        if mode != "mutual":
            if mode is not None and not room._ots_answered and room.ots_answer is None:
                self._ots_send(room, bool(mode))
            return
        if not room._ots_answered and room.ots_answer is None:
            if room.ots_foe is True:
                # They accepted: so does the client, and the sheets open for both. The
                # decision waits for them (``|showteam|``).
                self._ots_send(room, True)
                self._ots_hold_for(room, OTS_SHOW_GRACE)
            elif room.ots_foe is False:
                self._ots_release(room, "opponent rejected")  # nothing to send: they're closed
            elif room._ots_since is None:
                self._ots_hold_for(room, self.ots_wait)
                self._event(logging.INFO, "open team sheets wait", room=room.id, seconds=self.ots_wait)
        if room._ots_hold and (room.ots_open or room.ots_foe is False or room.ots_answer is False):
            self._ots_release(room, "sheets open" if room.ots_open else "closed")

    def _ots_send(self, room: BattleRoom, accept: bool) -> None:
        room._ots_answered = True
        room.ots_answer = accept
        command = "/acceptopenteamsheets" if accept else "/rejectopenteamsheets"
        self._event(logging.INFO, "open team sheets", room=room.id, accept=accept)
        self._send([f"{room.id}|{command}"], urgent=True)

    def _ots_hold_for(self, room: BattleRoom, seconds: float) -> None:
        """Holds ``room``'s decision (from now, if it wasn't held) for at
        most ``seconds`` more (``_ots_timeout``)."""
        if room._ots_since is None:
            room._ots_since = time.monotonic()
        room._ots_hold = True
        if room._ots_timer is not None:
            room._ots_timer.cancel()
        room._ots_timer = asyncio.get_running_loop().call_later(seconds, self._ots_timeout, room.id)

    def _ots_timeout(self, room_id: str) -> None:
        """The hold's time is up: an unanswered offer is rejected (the
        opponent didn't accept in ``ots_wait`` seconds), and the decision
        goes. Keyed by room id: a reconnect replaces the room."""
        room = self.rooms.get(room_id)
        if room is None:
            return
        room._ots_timer = None
        if not room._ots_hold:
            return
        if not room._ots_answered and room.ots_answer is None and room.ots_foe is None and not room.ended and room.turn == 0:
            self._ots_send(room, False)
            self._ots_release(room, "timeout")
        else:
            self._ots_release(room, "no sheets" if room.ots_answer else "timeout")

    def _ots_release(self, room: BattleRoom, why: str, *, decide: bool = True) -> None:
        """Ends ``room``'s hold, and asks for the decision it held."""
        if room._ots_timer is not None:
            room._ots_timer.cancel()
            room._ots_timer = None
        if not room._ots_hold:
            return
        room._ots_hold = False
        if room._ots_since is not None:
            room.ots_wait_ms = round((time.monotonic() - room._ots_since) * 1e3, 1)
        self._event(logging.INFO, "open team sheets settled", room=room.id, why=why, waited_ms=room.ots_wait_ms,
                    foe=room.ots_foe, ours=room.ots_answer)
        if decide and room._ots_deferred and room.pending:
            room._ots_deferred = False
            self._queue_decision(room)

    def _series_of(self, room: BattleRoom) -> Optional[SeriesRoom]:
        """The series ``room`` is a game of, if the client knows it."""
        if room.series_id is not None and room.series_id in self.series:
            return self.series[room.series_id]
        for series in self.series.values():
            if room.id in series.games.values():
                return series
        return None

    def _claim(self, room: BattleRoom) -> None:
        """Hands a new room to the operation waiting for it (else to the
        client's handler). An operation expecting this opponent (a
        challenge, an accept) goes before one that takes any (a search).
        A series' game goes to its series' handler: a game's room names its
        series before the battle starts, so a room is claimed once it has
        (a ``|player|`` line or a request). On a server that starts battles
        itself, a room is claimed at its first request, whose Pokémon say
        which team it plays (:meth:`_auto_room_team`)."""
        if not room._started and not room.ended:
            return
        if self.config.auto_battles:
            if room.request is None and not room.ended:
                return
            self._claim_auto(room)
            return
        series = self._series_of(room)
        if series is not None:
            if not series._claimed:
                # The series waited for its opponent (a room title it can't
                # read): the game's ``|player|`` lines name them.
                if series.opponent is None and room.opponent is not None:
                    series.opponent = room.opponent
                self._claim_series(series, skip=room.id)
                if not series._claimed:
                    return
            room.series = series
            if room.game is None:
                room.game = next((n for n, r in series.games.items() if r == room.id), None)
            room._claimed = True
            room.team = series.team
            self._room_handlers[room.id] = self._series_handlers.get(series.id, self.handler)
            self._event(logging.INFO, "battle start", room=room.id, format=room.format, opponent=room.opponent, series=series.id, game=room.game)
            return
        if room.series_id is not None:
            # A series' game whose series room hasn't been read yet (a
            # reconnect's join): join it, and wait.
            if room.series_id not in self._joining and self.connected:
                self._joining.add(room.series_id)
                self._send([f"|/join {room.series_id}"])
            return
        # A series game names its series before it starts: a room of a
        # best-of format that started without naming one is a single game
        # (a server that plays the format so), which an operation waiting
        # for a series takes too.
        unlinked = series_length(room.format) is not None
        waiter = self._pick_waiter(room, series=False, fallback=unlinked)
        if waiter is False:
            return  # wait for the room to name the opponent
        if waiter is not None:
            if waiter.series:
                self._event(logging.WARNING, "single game in a series format", room=room.id, format=room.format)
            self._waiters.remove(waiter)
            room._claimed = True
            room.team = waiter.team
            room._ots_mode = waiter.open_team_sheets
            self._room_handlers[room.id] = waiter.handler
            waiter.future.set_result(room)
        else:
            self._room_handlers[room.id] = self.handler
            self._event(logging.INFO, "unsolicited battle", room=room.id, opponent=room.opponent)
        self._event(logging.INFO, "battle start", room=room.id, format=room.format, opponent=room.opponent)

    def _claim_auto(self, room: BattleRoom) -> None:
        """:meth:`_claim` on a server that starts battles itself: the room
        goes to the operation waiting for a battle in its format (else to
        the client's handler), with the team its request shows."""
        waiter = self._pick_waiter(room, series=False, fallback=False)
        if waiter is False:
            return  # wait for the room to name the opponent
        op = next((o for o in self._auto_ops if o[0](room)), None) if waiter is None else None
        room._claimed = True
        room.team = self._auto_room_team(room)
        if waiter is not None:
            self._waiters.remove(waiter)
            self._room_handlers[room.id] = waiter.handler
            waiter.future.set_result(room)
        elif op is not None:  # the operation's next game, which isn't waiting yet
            self._room_handlers[room.id] = op[1]
            op[2].append(room)
        else:
            self._room_handlers[room.id] = self.handler
            self._event(logging.INFO, "unsolicited battle", room=room.id, opponent=room.opponent)
        self._event(logging.INFO, "battle start", room=room.id, format=room.format, opponent=room.opponent)

    def _pick_waiter(self, room: Any, *, series: bool, fallback: bool) -> Any:
        """The waiting operation a new battle or series room goes to: of
        its kind (``series``), else (``fallback``) of the other; one
        expecting the room's opponent first. ``None``: no taker; ``False``:
        wait for the room to name its opponent."""
        pending = [w for w in self._waiters if not w.future.done() and w.series == series]
        others = [w for w in self._waiters if not w.future.done() and w.series != series] if fallback else []
        if room.opponent is None and (pending or others):
            return False
        opponent = to_id(room.opponent or "")
        for group in (pending, others):
            matching = [w for w in group if w.match(room)]
            expected = [w for w in matching if w.opponent is not None and w.opponent == opponent]
            if expected or matching:
                return expected[0] if expected else matching[0]
        return None

    def _on_error(self, room: BattleRoom, message: str) -> None:
        stale = stale_choice_error(message)
        self._event(logging.INFO if stale else logging.WARNING, "stale choice" if stale else "choice error",
                    room=room.id, message=message)
        if room._ots_hold and "open team sheets" in message:
            # The server refused the answer (too late, or already given):
            # nothing more to wait for.
            self._ots_release(room, "refused")
        handler = self._room_handlers.get(room.id)
        if message.startswith("[Unavailable choice]"):
            room.unavailable += 1
        elif stale:
            # A choice for no open request (an outdated rqid, a request that
            # already has a choice, as after a reconnect): not a wrong
            # choice, so nothing is asked again; the next request is
            # answered as usual.
            room.stale += 1
        elif message.startswith("[Invalid choice]"):
            room.invalid += 1
            if room.request is not None:
                room._retries += 1
                if room._retries > self.max_retries:
                    self._choose_default(room)
                else:
                    room._retry = True
        if handler is not None:
            self._handler_call(room, "error", handler.error, room, message)

    def _choose_default(self, room: BattleRoom) -> None:
        room.fallbacks += 1
        room._retries = 0
        room._retry = False
        room._answered = room.requests
        self._send([self._choose_message(room, "default")], urgent=True)

    @staticmethod
    def _choose_message(room: BattleRoom, choice: str) -> str:
        rqid = f"|{room.rqid}" if room.rqid is not None else ""
        return f"{room.id}|/choose {choice}{rqid}"

    def _save_log(self, room_id: str, lines: List[str]) -> Optional[str]:
        """Saves a room's lines in :attr:`log_dir`: the file's path, or
        ``None`` without a ``log_dir`` or when it couldn't be written
        (logged)."""
        if self.log_dir is None:
            return None
        try:
            self.log_dir.mkdir(parents=True, exist_ok=True)
            path = self.log_dir / _log_name(room_id)
            path.write_text("\n".join(lines) + "\n", encoding="utf-8", errors="replace")
            return str(path)
        except (OSError, ValueError) as e:
            self._event(logging.WARNING, "log not saved", room=room_id, error=repr(e))
            return None

    def _finish(self, room: BattleRoom, reason: Optional[str]) -> None:
        if self.rooms.get(room.id) is not room:
            return
        del self.rooms[room.id]
        self._ots_release(room, "ended", decide=False)
        handler = self._room_handlers.pop(room.id, None)
        log_path = self._save_log(room.id, room.lines)
        result = room.result(self.user_id, log_path, reason)
        series = room.series
        if series is not None:
            game = room.game if room.game is not None else max(series.results, default=0) + 1
            result.game = game
            if not result.finished and game in series.game_winners:
                # The game's own room never said (it ended with the series):
                # the series room announced its winner.
                winner = series.game_winners[game]
                result.winner, result.tie, result.finished, result.reason = winner, winner is None, True, None
                result.won = None if winner is None else to_id(winner) == self.user_id
            series.results[game] = result
        if handler is not None:
            _, extras = self._handler_call(room, "summary", handler.summary, room)
            if isinstance(extras, dict):
                result.extras = extras
            self._handler_call(room, "close", handler.close, room)
        self.results.append(result)
        self._event(
            logging.INFO,
            "battle end",
            room=room.id,
            won=result.won,
            winner=result.winner,
            turns=result.turns,
            reason=result.reason,
            invalid=result.invalid,
        )
        if not self._closing and self._ws is not None:
            if self.leave_delay > 0:
                self._leaving[room.id] = room
                asyncio.get_running_loop().call_later(self.leave_delay, self._leave_late, room)
            else:
                self._send([f"|/leave {room.id}"])
        done = room._end
        if done is not None and not done.done():
            done.set_result(result)
        if series is not None:
            self._maybe_finish_series(series)
            if self.series.get(series.id) is series and not series.ended:
                # The next game should follow (Showdown: once both players
                # are ready, or after 40 s); a watchdog for when it doesn't.
                count = max(series.games, default=0)
                asyncio.get_running_loop().call_later(NEXT_GAME_TIMEOUT, self._next_game_overdue, series, count)

    def _leave_late(self, room: BattleRoom) -> None:
        """A finished battle room's ``/leave``, ``leave_delay`` seconds
        after its end, under the id the room has by then (a rename
        meanwhile: ``_rename``)."""
        if self._leaving.get(room.id) is room:
            del self._leaving[room.id]
        if not self._closing and self._ws is not None:
            self._send([f"|/leave {room.id}"])

    # -- series ----------------------------------------------------------------------------------

    def _on_series(self, room_id: str, lines: List[str]) -> None:
        """A series room's frame (Showdown's ``BestOfGame``): it names each
        game's battle room, asks whether the player is ready for the next
        game, and announces each game's winner and the series'."""
        series = self.series.get(room_id)
        if lines and lines[0].startswith("|init|"):
            self._joining.discard(room_id)
            if series is None:
                series = SeriesRoom(room_id, self.name or "")
                series._end = asyncio.get_running_loop().create_future()
                self.series[room_id] = series
                self._event(logging.INFO, "series", room=room_id)
            else:  # joined again (a reconnect): its log and its prompts come again
                series._ready.clear()
                series.lines = []
        if series is None:
            if lines and _room_closed(lines[0]) is None:
                self._event(logging.DEBUG, "lines for an unknown series", room=room_id, head=lines[0][:120])
            return
        events = [e for e in (series.observe(line, self.user_id) for line in lines) if e is not None]
        if not series._claimed:
            self._claim_series(series)
        for event in events:
            if event[0] == "game":
                self._event(logging.INFO, "series game", room=series.id, game=event[1], battle=event[2])
                room = self.rooms.get(event[2])
                if room is not None and event[2] not in self._room_handlers:
                    self._on_battle(event[2], [])  # its claim waited for the link
                elif room is None and event[1] not in series.results:
                    # Showdown joins the players to each game itself; a game
                    # the client still isn't in shortly after (it started
                    # while the client was away, on a server that doesn't
                    # rejoin games) is joined.
                    asyncio.get_running_loop().call_later(GAME_JOIN_DELAY, self._join_game, series, event[1], event[2])
            elif event[0] == "ready":
                if event[1] not in series._ready and not series.ended:
                    series._ready.add(event[1])
                    self._event(logging.INFO, "series ready", room=series.id, game=event[1])
                    self._send([f"{series.id}|/confirmready"], urgent=True)
            elif event[0] == "end":
                self._event(logging.INFO, "series over", room=series.id, winner=series.winner, tie=series.tie)
                self._maybe_finish_series(series)
            elif event[0] == "closed":
                self._finish_series(series, None if series.ended else event[1])

    def _join_game(self, series: SeriesRoom, game: int, room_id: str) -> None:
        """Joins game ``game`` of ``series`` (``room_id``) unless the client
        is in it already, it has ended, or a later game started."""
        if self.series.get(series.id) is not series or series.ended or not self.connected:
            return
        # The game's room as the series names it now (a rename since).
        room_id = series.games.get(game, room_id)
        if room_id in self.rooms or game in series.results or max(series.games, default=0) != game:
            return
        if room_id in self._joining or not _ROOM_ID.fullmatch(room_id):
            return
        self._event(logging.INFO, "joining game", room=series.id, game=game, battle=room_id)
        self._joining.add(room_id)
        self._send([f"|/join {room_id}"])

    def _claim_series(self, series: SeriesRoom, skip: Optional[str] = None) -> None:
        """Hands a new series to the operation waiting for it (else to the
        client's handler), as :meth:`_claim` hands a battle. An operation
        waiting for a single game in the format takes it when none waits
        for a series (a server that runs the format as a series). Then
        its games that waited are claimed (but ``skip``, the one being
        claimed)."""
        waiter = self._pick_waiter(series, series=True, fallback=True)
        if waiter is False:
            return
        series._claimed = True
        if waiter is not None:
            if not waiter.series:
                self._event(logging.WARNING, "series in a single-game format", room=series.id, format=series.format)
            self._waiters.remove(waiter)
            series.team = waiter.team
            self._series_handlers[series.id] = waiter.handler
            waiter.future.set_result(series)
        else:
            self._series_handlers[series.id] = self.handler
            self._event(logging.INFO, "unsolicited series", room=series.id, opponent=series.opponent)
        self._event(logging.INFO, "series start", room=series.id, format=series.format, opponent=series.opponent, best_of=series.best_of)
        for room in list(self.rooms.values()):  # games that waited for it
            if room.id != skip and room.id not in self._room_handlers and self._series_of(room) is series:
                self._on_battle(room.id, [])

    def _maybe_finish_series(self, series: SeriesRoom) -> None:
        """Ends the series once it is over and its games have ended (their
        rooms announce it too); a game still going after
        :data:`SERIES_END_GRACE` seconds ends then."""
        if self.series.get(series.id) is not series or not series.ended:
            return
        if any(r.series is series or r.series_id == series.id for r in self.rooms.values()):
            asyncio.get_running_loop().call_later(SERIES_END_GRACE, self._series_overdue, series)
            return
        self._finish_series(series, None)

    def _series_overdue(self, series: SeriesRoom) -> None:
        if self.series.get(series.id) is series:
            self._finish_series(series, None)

    def _next_game_overdue(self, series: SeriesRoom, games: int) -> None:
        """:data:`NEXT_GAME_TIMEOUT` seconds after game ``games`` ended: if
        no later game started, the series is forfeited and ends (while the
        client is disconnected, it waits on)."""
        if self.series.get(series.id) is not series or series.ended or max(series.games, default=0) != games:
            return
        if any(r.series is series or r.series_id == series.id for r in self.rooms.values()):
            return  # a game is being played
        if not self.connected and not self._closing:
            asyncio.get_running_loop().call_later(NEXT_GAME_TIMEOUT, self._next_game_overdue, series, games)
            return
        reason = f"game {games + 1} did not start within {NEXT_GAME_TIMEOUT:g} s"
        self._event(logging.WARNING, "series stalled", room=series.id, reason=reason)
        if self._ws is not None and not self._closing:
            self._send([f"{series.id}|/forfeit"], urgent=True)
        self._finish_series(series, reason)

    def _finish_series(self, series: SeriesRoom, reason: Optional[str]) -> None:
        if self.series.get(series.id) is not series:
            return
        del self.series[series.id]
        handler = self._series_handlers.pop(series.id, None)
        for room in list(self.rooms.values()):  # its games end with it
            if room.series is series or room.series_id == series.id:
                self._finish(room, reason or "the series ended")
        # The handler's series state learns how the series room ended it.
        if handler is not None:
            self._guarded(lambda: handler.series_end(series), room=series.id, method="series_end")
        log_path = self._save_log(series.id, series.lines)
        result = series.result(self.user_id, log_path, reason)
        self.series_results.append(result)
        self._event(
            logging.INFO,
            "series end",
            room=series.id,
            won=result.won,
            winner=result.winner,
            score=(result.wins, result.losses, result.ties),
            games=len(result.games),
            reason=result.reason,
        )
        if not self._closing and self._ws is not None:
            self._send([f"|/leave {series.id}"])
        done = series._end
        if done is not None and not done.done():
            done.set_result(result)

    # -- decisions ----------------------------------------------------------------------------

    def _queue_decision(self, room: BattleRoom) -> None:
        if self._hook("hold_decision", self.hold_decision, room):
            return  # asked for again with release_decision()
        room._queued = True
        assert self._decide_wake is not None
        self._decide_wake.set()

    async def _decide_loop(self) -> None:
        """Asks the handlers for choices, batching the rooms that wait."""
        assert self._decide_wake is not None
        while True:
            await self._decide_wake.wait()
            self._decide_wake.clear()
            try:
                waiting = [r for r in self.rooms.values() if r._queued]
                groups: Dict[int, Tuple[BattleHandler, List[BattleRoom]]] = {}
                for room in waiting:
                    room._queued = False
                    if not room.pending:
                        continue
                    if room._ots_hold:  # asked once Open Team Sheets is settled
                        room._ots_deferred = True
                        continue
                    handler = self._room_handlers.get(room.id)
                    if handler is not None:
                        groups.setdefault(id(handler), (handler, []))[1].append(room)
                for handler, rooms in groups.values():
                    size = handler.max_batch if isinstance(handler.max_batch, int) and handler.max_batch > 0 else len(rooms)
                    for i in range(0, len(rooms), size):
                        await self._decide(handler, rooms[i : i + size])
            except _UNCONTAINED:
                raise
            except BaseException as e:  # pragma: no cover (a bug): the loop must go on
                self._event(logging.ERROR, "decision error", error=repr(e))

    async def _ask(self, handler: BattleHandler, rooms: List[BattleRoom]) -> List[Any]:
        """The handler's choices for ``rooms``: one entry per room. A batch
        that fails (an exception, or not one entry per room) is asked again
        room by room, so one room's failure is that room's alone; a room
        whose own call fails gets ``default``."""
        try:
            choices = await handler.choose(rooms)
            if choices is None or isinstance(choices, (str, bytes)):
                raise TypeError(f"choose() returned {type(choices).__name__}, not a list")
            choices = list(choices)
            if len(choices) != len(rooms):
                raise ValueError(f"choose() returned {len(choices)} choices for {len(rooms)} rooms")
            return choices
        except _UNCONTAINED:
            raise
        except BaseException as e:
            self._event(logging.ERROR, "handler error", method="choose", error=repr(e), rooms=[r.id for r in rooms])
            if len(rooms) == 1:
                rooms[0]._fallback = True
                return ["default"]
        out: List[Any] = []
        for room in rooms:
            out.extend(await self._ask(handler, [room]))
        return out

    async def _decide(self, handler: BattleHandler, rooms: List[BattleRoom]) -> None:
        asked = [(r, r.requests) for r in rooms]
        for r in rooms:
            r._fallback = False
        choices = await self._ask(handler, rooms)
        for (room, requests), choice in zip(asked, choices):
            if room.ended or self.rooms.get(room.id) is not room:
                continue
            if room.requests != requests:  # a newer request came while deciding
                continue
            if choice is None:  # the handler passes; the request stays unanswered
                continue
            if not isinstance(choice, str) or not choice.strip() or not is_command_safe(choice):
                self._event(logging.ERROR, "handler error", room=room.id, method="choose", error=f"bad choice {choice!r}")
                choice, room._fallback = "default", True
            if room._fallback:
                room.fallbacks += 1
            room._answered = requests
            room._retry = False
            self._send([self._choose_message(room, choice)], urgent=True)

    # -- starting games ---------------------------------------------------------------------------

    def _hidenext(self) -> List[str]:
        """The message that hides the next battle, with :attr:`hide_battles`."""
        return ["|/hidenext"] if self.hide_battles else []

    def _team(self, team: Team, fmt: str) -> str:
        value = team(fmt) if callable(team) else team
        return pack_team(value, fmt)

    def _unplayable(self, fmt: str, handler: BattleHandler) -> Optional[str]:
        """Why ``handler`` can't play ``fmt`` on this client, or ``None``."""
        _, ok = self._guarded(lambda: handler.supports(self.config.play_format(fmt)), method="supports")
        if not ok:
            return f"the handler can't play {fmt!r}"
        return None

    def _check_format(self, fmt: str, handler: BattleHandler, *, ladder: bool = False) -> None:
        self._check_alive()
        if not fmt:
            raise ShowdownError("no format given")
        if ladder and self.config.commands is not None and "search" not in self.config.commands:
            raise ShowdownError(
                f"{self.config.websocket_url} has no ladder search (/search): the server pairs the users online "
                "with a team itself (accept_challenges plays the battles it is put in)"
            )
        if series_length(fmt) is not None and not self.config.series:
            raise ShowdownError(
                f"{fmt!r} is a best-of series, which this server ({self.config.websocket_url}) doesn't run: "
                "play series on a local Showdown server"
            )
        if self.formats:
            info = self.formats.get(to_id(fmt.partition("@@@")[0]))
            if info is None and series_length(fmt) is not None:
                raise ShowdownError(
                    f"the server has no format {fmt!r}: it doesn't run best-of series "
                    "(pikapro-showdown-server plays single games; a local Showdown server runs series)"
                )
            if info is None:
                base = to_id(fmt.partition("@@@")[0])
                hint = "`python -m pikapro.showdown formats` lists the server's formats"
                # (pikapro's presets and the Showdown format each is played
                # as on pikapro-showdown-server: poke-env's map)
                from ..compat.poke_env._formats import SHOWDOWN_FORMATS

                if base in SHOWDOWN_FORMATS:
                    hint = (
                        f"{base!r} is a pikapro preset name, not a Showdown format id; "
                        f"its Showdown format is {SHOWDOWN_FORMATS[base]!r} ({hint})"
                    )
                raise ShowdownError(f"the server has no format {fmt!r}: {hint}")
            if ladder and not info.ladder:
                raise ShowdownError(f"{fmt!r} is not on this server's ladder")
        why = self._unplayable(fmt, handler)
        if why is not None:
            raise ShowdownError(why)

    async def _popup_or(self, fut: "asyncio.Future[Any]", timeout: Optional[float], what: str = "the request") -> Any:
        """Waits for ``fut``; a ``|popup|`` refusing a start first raises
        it as an error, and so does ``timeout`` (``what`` names what the
        server didn't confirm)."""
        popup = asyncio.get_running_loop().create_future()
        self._popups.append(popup)
        try:
            done, _ = await asyncio.wait({fut, popup}, timeout=timeout, return_when=asyncio.FIRST_COMPLETED)
            if fut in done:
                return fut.result()
            if popup in done:
                raise ShowdownError(popup.result())
            raise ShowdownError(f"the server did not confirm {what} within {timeout:g} s")
        finally:
            self._popups.remove(popup)
            if not popup.done():
                popup.cancel()

    async def _start(
        self,
        messages: List[str],
        match: Callable[[BattleRoom], bool],
        handler: BattleHandler,
        confirmed: Callable[[], "asyncio.Future[Any]"],
        timeout: Optional[float],
        opponent: Optional[str] = None,
        series: bool = False,
        search: Optional[str] = None,
        what: str = "the request",
        open_team_sheets: Optional[bool] = None,
    ) -> Any:
        """Sends the messages that start one game and returns its room (in
        a best-of format, ``series``: its series room).

        Holds the start lock until the server has accepted the request
        (``confirmed``, or the room itself), so that a ``|popup|`` is this
        request's; then waits for the room (``timeout``). A ladder search
        (``search``: its format) is accepted when ``|updatesearch|`` lists
        it (``confirmed`` is not used), and ends with an error if it leaves
        the list with no battle following it (:meth:`_on_searching`)."""
        assert self._start_lock is not None
        async with self._start_lock:
            waiter = _Waiter(match, handler, opponent, series)
            waiter.search = search
            waiter.open_team_sheets = open_team_sheets
            for message in messages:
                if message.startswith("|/utm "):
                    packed = message[len("|/utm ") :]
                    waiter.team = packed if packed and packed != "null" else None
            self._waiters.append(waiter)
            ok = confirmed() if search is None else waiter.listed
            self._send(messages)
            try:
                first = asyncio.ensure_future(asyncio.wait({waiter.future, ok}, return_when=asyncio.FIRST_COMPLETED))
                await self._popup_or(first, timeout=START_CONFIRM_TIMEOUT, what=what)
            except BaseException:
                self._drop_waiter(waiter)
                ok.cancel()
                raise
            if ok is not waiter.listed:
                ok.cancel()
        try:
            return await asyncio.wait_for(asyncio.shield(waiter.future), timeout)
        finally:
            self._drop_waiter(waiter)

    def _drop_waiter(self, waiter: _Waiter) -> None:
        if waiter in self._waiters:
            self._waiters.remove(waiter)
        if waiter.drop_check is not None:
            waiter.drop_check.cancel()
            waiter.drop_check = None

    def _on_searching(self) -> None:
        """``|updatesearch|`` changed :attr:`searching`: a waiting search
        the list names is accepted; one that left it (Showdown's
        ``createBattle`` cancels the searches of a match, then opens its
        room or refuses it with a popup: ``/prelockdown``, ``/lockdown``)
        gets :data:`SEARCH_DROP_GRACE` seconds for its room to come."""
        for w in self._waiters:
            if w.search is None or w.future.done():
                continue
            if w.search in self.searching:
                w.searched = True
                w.refusal = None
                if not w.listed.done():
                    w.listed.set_result(None)
                if w.drop_check is not None:
                    w.drop_check.cancel()
                    w.drop_check = None
            elif w.searched and w.drop_check is None:
                w.drop_check = asyncio.get_running_loop().call_later(SEARCH_DROP_GRACE, self._search_dropped, w)

    def _search_dropped(self, waiter: _Waiter) -> None:
        """A search left :attr:`searching` :data:`SEARCH_DROP_GRACE` seconds
        ago: unless its room came (or is on its way), its operation ends
        with the server's refusal."""
        waiter.drop_check = None
        if waiter.future.done() or waiter not in self._waiters or waiter.search in self.searching:
            return
        fmt = waiter.search
        arriving = [r for r in [*self.rooms.values(), *self.series.values()] if r.format == fmt and not r._claimed]
        if arriving and not self._closing:  # a room of the format not claimed yet: its claim decides
            waiter.drop_check = asyncio.get_running_loop().call_later(SEARCH_DROP_GRACE, self._search_dropped, waiter)
            return
        reason = waiter.refusal or "the server dropped the search and no battle followed"
        self._event(logging.WARNING, "search dropped", format=fmt, reason=reason)
        self._waiters.remove(waiter)
        waiter.future.set_exception(ShowdownError(f"the {fmt} search ended without a battle: {reason}"))
        waiter.future.exception()  # retrieved: an operation that gave up won't await it

    @staticmethod
    def _until(condition: Callable[[], bool]) -> "asyncio.Future[Any]":
        async def wait() -> None:
            while not condition():
                await asyncio.sleep(0.02)

        return asyncio.ensure_future(wait())

    def _pm_future(self, pred: Callable[[str], bool]) -> "asyncio.Future[Any]":
        fut = asyncio.get_running_loop().create_future()
        entry = (pred, fut)
        self._pm_waiters.append(entry)
        fut.add_done_callback(lambda _: self._pm_waiters.remove(entry) if entry in self._pm_waiters else None)
        return fut

    def _series_weight(self) -> int:
        """The slots a best-of series takes: Showdown counts a series as two
        games (the series room and the game being played, ``user.games``
        in ``Ladder.prepBattle``), so on a server with a game limit
        (:attr:`ServerConfig.max_games`) a series takes two."""
        assert self._slots is not None
        return min(2, self._slots.capacity) if self.config.max_games else 1

    async def _play(self, starter: Callable[[], Any], n: int, series: bool = False) -> List[Any]:
        """Starts ``n`` games (or series) with ``starter`` (at most
        ``max_concurrent`` at once; a series takes :meth:`_series_weight`
        slots), and returns their results in start order. If the connection
        ends for good while a game is being started, raises its error (the
        results so far are in :attr:`results`); the games already started
        end as disconnected."""
        assert self._slots is not None
        slots = self._slots
        weight = self._series_weight() if series else 1
        if series and weight > 1 and n > 1 and slots.capacity // weight < min(n, slots.capacity):
            self._event(
                logging.INFO,
                "concurrency capped",
                reason="a series counts as two of Showdown's games",
                max_concurrent=slots.capacity,
                series_at_once=max(1, slots.capacity // weight),
            )
        ends: List[asyncio.Future] = []
        for _ in range(n):
            taken = await self._guard(slots.acquire(weight))
            try:
                room = await self._guard(starter())
            except BaseException:
                slots.release(taken)
                raise
            end = room._end
            assert end is not None
            end.add_done_callback(lambda _, taken=taken: slots.release(taken))
            ends.append(end)
        # Shielded: an operation cancelled (Ctrl-C) leaves its battles
        # running, for close() to forfeit and record.
        return list(await asyncio.gather(*(asyncio.shield(e) for e in ends)))

    async def _wait_game_limit(self, error: BaseException, tries: int) -> int:
        """After Showdown's game limit refused a start: waits for one of the
        client's games to end (at most :data:`GAME_LIMIT_WAIT` seconds)
        before the start is tried again. With none in progress (the
        account's games elsewhere count too), after
        :data:`GAME_LIMIT_TRIES` tries it raises the refusal. Returns the
        tries so far."""
        live = [*self.series.values(), *self.rooms.values()]
        ends = [x._end for x in live if x._end is not None and not x._end.done()]
        if _start_limit(error):
            # Showdown's window is 3 minutes: waiting on a game doesn't help.
            ends = []
            tries += 1
            if tries * GAME_LIMIT_WAIT > 200.0 + GAME_LIMIT_WAIT * GAME_LIMIT_TRIES:
                raise ShowdownError(f"{error} (other clients on this IP address start games too?)")
        elif not ends:
            tries += 1
            if tries > GAME_LIMIT_TRIES:
                raise ShowdownError(
                    f"{error} (none of this client's games is in progress: the account's games elsewhere count too)"
                )
        self._event(logging.WARNING, "game limit", games=len(ends), wait=GAME_LIMIT_WAIT)
        if ends:
            await asyncio.wait(ends, timeout=GAME_LIMIT_WAIT, return_when=asyncio.FIRST_COMPLETED)
        else:
            await asyncio.sleep(GAME_LIMIT_WAIT)
        return tries

    async def ladder(
        self,
        n: int,
        format: str,
        team: Team = None,
        *,
        handler: Optional[BattleHandler] = None,
        search_timeout: Optional[float] = None,
    ) -> List[Any]:
        """Plays ``n`` ladder games in ``format`` (``/search``), at most
        :attr:`max_concurrent` at once. ``team``: an export or packed team,
        ``None`` for a random-battle format, or ``team(format) -> team``
        called for each game. Returns the results in start order: a
        :class:`BattleResult` per game, or in a best-of format
        (``…bo3``) a :class:`SeriesResult` per series (``n`` counts series).
        Raises :class:`ShowdownError` for a format the server doesn't
        ladder or the handler can't play (``BattleHandler.supports``), or
        when the connection ends for good."""
        fmt = to_id(format)
        handler = handler or self.handler
        self._check_format(fmt, handler, ladder=True)
        series = series_length(fmt) is not None

        async def start() -> BattleRoom:
            messages = self._hidenext() + [f"|/utm {self._team(team, fmt)}", f"|/search {fmt}"]
            interrupted = limited = 0
            while True:
                try:
                    return await self._start(
                        messages,
                        lambda r: r.format == fmt and not r._claimed,
                        handler,
                        lambda: asyncio.get_running_loop().create_future(),  # unused: ``search``
                        search_timeout,
                        series=series,
                        search=fmt,
                        what=f"the {fmt} search",
                    )
                except _Disconnected:
                    interrupted += 1
                    self._event(logging.INFO, "search interrupted", format=fmt, attempt=interrupted)
                    if interrupted >= 3:
                        raise ShowdownError("the search kept being interrupted") from None
                    await self._wait_reconnected()
                except ShowdownError as e:
                    if not _game_limit(e) or self._fatal is not None:
                        raise
                    limited = await self._wait_game_limit(e, limited)
                except asyncio.TimeoutError:
                    if fmt in self.searching:
                        self._send([f"|/cancelsearch {fmt}"])
                    raise ShowdownError(f"no {fmt} battle started within search_timeout ({search_timeout} s)") from None

        return await self._play(start, n, series)

    async def _wait_reconnected(self) -> None:
        assert self._logged_in is not None and self._run_task is not None
        wait = asyncio.ensure_future(self._logged_in.wait())
        try:
            await asyncio.wait({wait, self._run_task}, return_when=asyncio.FIRST_COMPLETED)
        finally:
            wait.cancel()
        if not self._logged_in.is_set():
            raise self._fatal or self._login_error or self._closed_error("disconnected")

    async def challenge(
        self,
        user: str,
        format: str,
        team: Team = None,
        *,
        n: int = 1,
        handler: Optional[BattleHandler] = None,
        accept_timeout: Optional[float] = 300.0,
    ) -> List[Any]:
        """Challenges ``user`` to ``n`` games in ``format``, one challenge
        at a time (Showdown allows one outgoing challenge), at most
        :attr:`max_concurrent` battles at once. ``format`` may carry a
        custom game's rules after ``@@@`` (``"gen9vgc2025regi@@@Best of =
        3"`` challenges to a best-of-3 series). Returns the results as
        :meth:`ladder` does. Raises :class:`ChallengeError` if a challenge
        is rejected, cancelled or not accepted within ``accept_timeout``
        seconds, and :class:`ShowdownError` as :meth:`ladder` does."""
        key = format_key(format)
        if not is_command_safe(key) or "|" in key:
            raise ValueError(f"format {format!r} has a line break, a '|' or a control character")
        fmt = to_id(key.partition("@@@")[0])
        series = series_length(key) is not None
        target = to_id(user)
        if not target:
            raise ValueError(f"no user to challenge in {user!r}")
        handler = handler or self.handler
        self._check_format(key, handler)

        async def start() -> BattleRoom:
            interrupted = limited = too_soon = 0
            while True:
                try:
                    return await start_once()
                except _Disconnected:
                    interrupted += 1
                    self._event(logging.INFO, "challenge interrupted", user=target, attempt=interrupted)
                    if interrupted >= 3:
                        raise ChallengeError("the challenge kept being interrupted") from None
                    await self._wait_reconnected()
                except ShowdownError as e:
                    if self._fatal is not None:
                        raise
                    if _challenge_too_soon(e) and too_soon == 0:
                        # The server's cooldown ran from later than the client's (a
                        # slow team validation): once more, a full interval on.
                        too_soon += 1
                        self._event(logging.WARNING, "challenge too soon", user=target, retry_in=self.config.challenge_interval or CHALLENGE_RETRY_S)
                        self._last_challenge = time.monotonic()
                        if not self.config.challenge_interval:
                            await asyncio.sleep(CHALLENGE_RETRY_S)
                        continue
                    if _game_limit(e):
                        limited = await self._wait_game_limit(e, limited)
                        continue
                    if series and "format is invalid" in str(e).lower():
                        raise ShowdownError(f"{e} (does this server run best-of series? a 'Best of = N' rule needs one)") from None
                    raise

        async def start_once() -> BattleRoom:
            # The cooldown runs from Showdown's acknowledgement of the last
            # challenge (the challenge |pm|, which :meth:`_on_pm` stamps).
            wait = self._last_challenge + self.config.challenge_interval - time.monotonic()
            if wait > 0:
                await asyncio.sleep(wait)
            me = self.user_id

            def posted() -> "asyncio.Future[Any]":
                return self._until(lambda: self.challenges.get((me, target)) == key)

            def ends_challenge(line: str) -> bool:
                parts = line.split("|", 4)
                return (
                    len(parts) == 5
                    and {to_id(parts[2]), to_id(parts[3])} == {me, target}
                    and any(t in parts[4] for t in ("rejected the challenge", "cancelled the challenge", "Challenge cancelled"))
                )

            rejected = self._pm_future(ends_challenge)
            room_task: Optional[asyncio.Future] = None
            try:
                room_task = asyncio.ensure_future(
                    self._start(
                        self._hidenext() + [f"|/utm {self._team(team, key)}", f"|/challenge {target}, {key}"],
                        lambda r: r.format == fmt and to_id(r.opponent or "") == target and not r._claimed,
                        handler,
                        posted,
                        accept_timeout,
                        opponent=target,
                        series=series,
                        what=f"the challenge to {user}",
                    )
                )
                done, _ = await asyncio.wait({room_task, rejected}, return_when=asyncio.FIRST_COMPLETED)
                if room_task in done:
                    return room_task.result()
                if rejected.exception() is not None:  # the connection ended
                    raise rejected.exception()  # type: ignore[misc]
                raise ChallengeError(f"{user} rejected or cancelled the challenge")
            except asyncio.TimeoutError:
                self._send([f"|/cancelchallenge {target}"])
                raise ChallengeError(f"{user} did not accept the challenge in {accept_timeout} s") from None
            finally:
                if not rejected.done():
                    rejected.cancel()
                if room_task is not None and not room_task.done():
                    room_task.cancel()

        return await self._play(start, n, series)

    async def accept_challenges(
        self,
        from_user: Optional[str] = None,
        n: int = 1,
        team: Team = None,
        *,
        format: Optional[str] = None,
        handler: Optional[BattleHandler] = None,
        plan: Optional[Callable[[str, str], Optional[AcceptPlan]]] = None,
    ) -> List[Any]:
        """Accepts ``n`` challenges (from ``from_user`` only, if given; in
        ``format`` only, if given, its custom rules included; others are
        left standing), at most :attr:`max_concurrent` battles at once, and
        plays them. A challenge in a format the handler can't play
        (``BattleHandler.supports``) is never accepted: it is left
        standing. A challenge to a best-of series is played as one (its
        result is a :class:`SeriesResult`).

        ``plan(user_id, format)``, if given, is asked right before each
        accept (a battle slot is free by then): an :class:`AcceptPlan`
        changes that battle's team and its answer to Open Team Sheets;
        ``None`` accepts as usual (``team`` is then asked for its team; a
        plan's own team doesn't ask it).

        On a server that starts battles itself
        (:attr:`ServerConfig.auto_battles`, VGC Arena) nothing is
        accepted: the team goes up with ``/utm`` (a rotation's next one
        once a battle was played with it; again after each login), and
        the next ``n`` battles the server puts the client in (in
        ``format``, or any) are played; ``from_user`` can't be honoured
        there, and ``plan`` isn't asked (no challenge is accepted)."""
        self._check_alive()
        assert self._incoming is not None
        want_user = to_id(from_user) if from_user else None
        want_fmt = format_key(format) if format else None
        handler = handler or self.handler
        if want_fmt is not None:
            self._check_format(want_fmt, handler)
        if self.config.auto_battles:
            if want_user:
                self._event(logging.WARNING, "from_user ignored", reason="the server picks the battles", user=want_user)
            if plan is not None:
                self._event(logging.WARNING, "plan ignored", reason="the server picks the battles")
            return await self._auto_accept(n, team, want_fmt, handler)
        skipped: List[Tuple[str, str]] = []
        # Challenges queued again after their plan's team failed, until that
        # second attempt ends (then a later refusal is retried too).
        again: set = set()

        def ask_plan(frm: str, fmt: str) -> Optional[AcceptPlan]:
            if plan is None:
                return None
            # (the hook's bug: the challenge is accepted as usual)
            _, p = self._guarded(plan, frm, fmt, method="plan", by=frm)
            return p if isinstance(p, AcceptPlan) else None

        def plan_failed(p: AcceptPlan, frm: str, fmt: str, why: str, retry: bool = True) -> None:
            if p.failed is not None:
                self._guarded(p.failed, why, method="plan failed", by=frm)
            if retry and p.team is not None and (frm, fmt) not in again and self.challenges.get((frm, self.user_id)) == fmt:
                again.add((frm, fmt))
                assert self._incoming is not None
                self._incoming.put_nowait((frm, fmt))

        async def start() -> BattleRoom:
            assert self._incoming is not None
            while True:
                frm, fmt = await self._incoming.get()
                # A retry after a refused plan team (``again``) is allowed
                # once per refusal: it ends with this attempt, however it ends.
                retrying = (frm, fmt) in again
                try:
                    if (want_user and frm != want_user) or (want_fmt and fmt != want_fmt):
                        skipped.append((frm, fmt))
                        continue
                    if self.challenges.get((frm, self.user_id)) != fmt:
                        continue  # withdrawn meanwhile
                    why = self._unplayable(fmt, handler)
                    if why is not None:
                        self._event(logging.WARNING, "challenge not accepted", by=frm, format=fmt, reason=why)
                        skipped.append((frm, fmt))
                        continue
                    base = to_id(fmt.partition("@@@")[0])
                    series = series_length(fmt) is not None
                    # A series takes a second slot where Showdown counts it as
                    # two games (the operation took one before the challenge).
                    extra = self._series_weight() - 1 if series else 0
                    if extra:
                        assert self._slots is not None
                        await self._slots.acquire(extra)
                    held = extra  # given back on every way out, unless the room takes it
                    try:
                        chosen = ask_plan(frm, fmt)
                        own = chosen is not None and chosen.team is not None
                        limited = 0
                        while True:
                            try:
                                packed = self._team(chosen.team if own else team, fmt)  # type: ignore[union-attr]
                            except _UNCONTAINED:
                                raise
                            except BaseException as e:  # a team that can't be packed (a user's, a rotation's bug): this challenge's alone
                                self._event(logging.ERROR, "challenge not accepted", by=frm, format=fmt,
                                            reason=f"the {'plan' if own else 'operation'}'s team failed: {e!r}")
                                if chosen is not None:
                                    # (a plan's own team: the challenge is accepted
                                    # again, once, with what the plan says next)
                                    plan_failed(chosen, frm, fmt, f"the team failed: {e!r}", retry=own)
                                break
                            try:
                                room = await self._start(
                                    self._hidenext() + [f"|/utm {packed}", f"|/accept {frm}"],
                                    lambda r, frm=frm, base=base: r.format == base and to_id(r.opponent or "") == frm and not r._claimed,
                                    handler,
                                    lambda: asyncio.get_running_loop().create_future(),
                                    ACCEPT_ROOM_TIMEOUT,
                                    opponent=frm,
                                    series=series,
                                    what=f"the accepted challenge from {frm}",
                                    open_team_sheets=chosen.open_team_sheets if chosen is not None else None,
                                )
                            except (ShowdownError, asyncio.TimeoutError, ValueError) as e:
                                if self._fatal is None and _game_limit(e) and self.challenges.get((frm, self.user_id)) == fmt:
                                    try:  # the challenge still stands: accept it once a game ends
                                        limited = await self._wait_game_limit(e, limited)
                                        continue
                                    except BaseException as stop:
                                        if chosen is not None:
                                            plan_failed(chosen, frm, fmt, repr(stop), retry=False)
                                        raise
                                if self._fatal is not None:
                                    if chosen is not None:
                                        plan_failed(chosen, frm, fmt, str(e), retry=False)
                                    raise
                                self._event(logging.WARNING, "accept failed", by=frm, format=fmt, error=str(e))
                                if chosen is not None:
                                    plan_failed(chosen, frm, fmt, str(e) or type(e).__name__)
                                break
                            except BaseException as stop:
                                if chosen is not None:  # cancelled (a drain), or the connection ended
                                    plan_failed(chosen, frm, fmt, repr(stop), retry=False)
                                raise
                            if extra:
                                assert room._end is not None
                                room._end.add_done_callback(lambda _, extra=extra: self._slots.release(extra))  # type: ignore[union-attr]
                                held = 0
                            return room
                    finally:
                        if held:
                            self._slots.release(held)  # type: ignore[union-attr]
                finally:
                    if retrying:
                        again.discard((frm, fmt))

        try:
            return await self._play(start, n)
        finally:
            for item in skipped:  # leave them for another accept_challenges
                self._incoming.put_nowait(item)

    async def _auto_accept(self, n: int, team: Team, fmt: Optional[str], handler: BattleHandler) -> List[Any]:
        """:meth:`accept_challenges` on a server that starts battles itself."""
        play = self.config.play_format(to_id(fmt.partition("@@@")[0])) if fmt else None
        self._auto_team = (team, fmt or "")

        def match(r: BattleRoom) -> bool:
            return (play is None or r.play_format == play) and not r._claimed

        # Battles the server put the client in while no game waited (the next one
        # can start as the last ends, before the operation waits again: the
        # random ladder's second game): played by this operation's handler,
        # and taken by its next game.
        op = (match, handler, collections.deque())
        self._auto_ops.append(op)

        async def start() -> BattleRoom:
            interrupted = 0
            while True:
                self._auto_upload(team, fmt or "")
                if op[2]:
                    return op[2].popleft()
                waiter = _Waiter(match, handler)
                self._waiters.append(waiter)
                try:
                    return await waiter.future
                except _Disconnected:
                    # The battle the server puts the client in survives the drop:
                    # wait on after the login (which sends the team again).
                    interrupted += 1
                    self._event(logging.INFO, "waiting interrupted", attempt=interrupted)
                    await self._wait_reconnected()
                finally:
                    self._drop_waiter(waiter)

        try:
            return await self._play(start, n)
        finally:
            self._auto_ops.remove(op)

    def cancel_search(self) -> None:
        """Asks the server to start no new battle for the client
        (``/cancelsearch``), now and again after each login (a login
        undoes it). On VGC Arena this is how a bot stops without losing
        anything: the battles in progress go on, and so does a return game
        the bot owes; once nothing is left to play, the arena says so with
        an ``|updatesearch|`` listing no games (:attr:`stopped`). On
        another server it cancels the client's ladder searches."""
        self._search_cancelled = True
        if self._outbox is not None:
            self._send(["|/cancelsearch"])

    @property
    def stopped(self) -> bool:
        """After :meth:`cancel_search`: an ``|updatesearch|`` read since
        ``/cancelsearch`` went out on this connection listed no games. The
        server's list decides: a room of the client's it left out ended
        unseen (:meth:`close_unlisted` with ``grace=0`` ends it)."""
        return self._cancel_mark is not None and self._search_updates > self._cancel_mark and not self.games

    async def wait_for_battles(self) -> List[Any]:
        """Waits until every battle in progress has ended; returns their
        results (battles cut off by the connection ending for good end as
        disconnected). Cancelling it (a timeout) leaves the battles, and
        the operations playing them, running. A series in progress is
        waited for as one (its :class:`SeriesResult`)."""
        ends = [s._end for s in list(self.series.values()) if s._end is not None]
        ends += [r._end for r in list(self.rooms.values()) if r._end is not None and r.series is None]
        # Shielded, as in _play: cancelling the gather must not cancel the
        # rooms' futures, which the operations playing them wait on (and
        # whose end frees their concurrency slot).
        return list(await asyncio.gather(*(asyncio.shield(e) for e in ends))) if ends else []
