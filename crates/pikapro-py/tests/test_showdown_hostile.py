"""pikapro.showdown against scripted servers that are odd, hostile or
strict: best-of series with reconnects, rejoins and missing lines, names
that break room titles, matches refused after a search, Showdown's game
limit and challenge cooldown, and formats the server plays another way.
The real servers are ``test_showdown_servers.py``'s; this file's servers speak the parts of
Showdown's protocol each test needs.
"""

from __future__ import annotations

import asyncio
import json
import logging
import subprocess
import sys
import time
from dataclasses import replace
from pathlib import Path
from typing import Dict, List

import pytest

pytest.importorskip("websockets")

from websockets.asyncio.server import serve  # noqa: E402

import pikapro.showdown.client as client_mod  # noqa: E402
from pikapro.showdown import (  # noqa: E402
    LOCAL,
    BattleResult,
    RandomRequestHandler,
    SeriesResult,
    ServerConfig,
    ShowdownClient,
    ShowdownError,
)
from pikapro.showdown.room import title_opponent  # noqa: E402

from _showdown_fakes import REQUEST, RUNNER, Fake, run, until  # noqa: E402

FMT = "gen9championsvgc2026regmcbo3"
SINGLE = "gen9championsvgc2026regmc"
LIMIT_POPUP = "Due to high load, you are limited to 5 games at the same time."
TOO_SOON = "You challenged less than 10 seconds after your last challenge! It's cancelled in case it's a misclick."


class BoServer:
    """A Showdown-like server for one user: it keeps each room's log and,
    like Showdown's ``rejoinGames``, joins a user who logs in to every room
    of an unended game (the series room first). Ready prompts are sent,
    not logged, as ``BestOfPlayer.updateReadyButton`` does. A search is
    listed in ``|updatesearch|``, dropped (``createBattle`` cancels it),
    and matched at once.

    Knobs: ``winners`` per game ("me"/"foe"/None); ``drop = (game, when)``
    drops the connection once, ``when`` = "request" (after game N's
    request) or "between" (after game N ends, before the prompt: the
    next game then starts while the player is away, after ``away_start``
    s, as Showdown's does after 40 s); ``never_ready``: no next game;
    ``series_for``: the formats a search opens a series in (default: the
    ``…bo3`` ones); ``no_series_for``: formats played as single games even
    if ``…bo3``; ``game_win=False``: a game's own room never sends
    ``|win|`` (only the series room announces it); ``series_first``: the
    series room announces a game before the game's room ends it;
    ``no_rejoin``: no ``rejoinGames`` on login; ``dup_series_init``: the
    series room's log is sent twice (a ``/join`` after the server's own
    rejoin); ``foe``: the opponent's name; ``limit``: Showdown's game
    limit (``Monitor.countConcurrentBattle``: a search is refused while
    the user is in more than ``limit`` games, a series room and its game
    counting one each); ``choose_delay``: seconds the server takes to end
    a game after a choice."""

    def __init__(self, winners=("me", "foe", "me"), *, drop=None, away_start=0.3, never_ready=False,
                 series_for=None, no_series_for=(), game_win=True, series_first=False, no_rejoin=False,
                 dup_series_init=False, foe="Foe", limit=None, choose_delay=0.0):
        self.foe = foe
        self.winners = list(winners)
        self.drop = drop
        self.away_start = away_start
        self.never_ready = never_ready
        self.series_for = series_for
        self.no_series_for = set(no_series_for)
        self.game_win = game_win
        self.series_first = series_first
        self.no_rejoin = no_rejoin
        self.dup_series_init = dup_series_init
        self.limit = limit
        self.choose_delay = choose_delay
        self.logs: Dict[str, List[str]] = {}
        self.ended: set = set()
        self.series: Dict[str, dict] = {}
        self.received: List[str] = []
        self.popups: List[str] = []
        self.peak_games = 0
        self.ws = None
        self.name = None
        self.n = 0
        self.connections = 0
        self.dropped = False
        self.joins: List[str] = []
        self.sockets = []

    def games(self) -> int:
        """The user's games as Showdown counts them (``user.games``)."""
        count = sum(1 for rid in self.logs if rid.startswith("battle-") and rid not in self.ended)
        return count + sum(1 for sid in self.series if sid not in self.ended)

    async def send_room(self, room: str, lines: List[str], log=True):
        new = room not in self.logs
        if log:
            self.logs.setdefault(room, []).extend(lines)
        self.peak_games = max(self.peak_games, self.games())
        if self.ws is not None:
            try:
                head = "|init|battle\n" if new else ""
                await self.ws.send(f">{room}\n{head}" + "\n".join(lines))
            except Exception:
                pass

    async def init_room(self, room: str):
        if self.ws is not None:
            await self.ws.send(f">{room}\n|init|battle\n" + "\n".join(self.logs.get(room, [])))

    async def handler(self, ws):
        self.connections += 1
        self.sockets.append(ws)
        await ws.send("|challstr|4|abcdef")
        async for msg in ws:
            self.received.append(msg)
            room, _, text = msg.partition("|")
            if text.startswith("/trn "):
                self.name = text[5:].split(",", 1)[0]
                self.ws = ws
                await ws.send(f"|updateuser| {self.name}|1|1|{{}}")
                if not self.no_rejoin:  # rejoinGames: the unended games' rooms, in creation order
                    for rid in list(self.logs):
                        if rid not in self.ended:
                            await self.init_room(rid)
                            if rid in self.series:
                                await self.prompt(rid)  # BestOfGame.onConnect
            elif text.startswith("/join "):
                rid = text[6:].strip()
                self.joins.append(rid)
                if rid in self.logs:
                    await self.init_room(rid)
            elif text.startswith("/search "):
                fmt = text[len("/search "):]
                if self.limit is not None and self.games() > self.limit:
                    self.popups.append(LIMIT_POPUP)
                    await ws.send(f"|popup|{LIMIT_POPUP}")
                    continue
                await ws.send("|updatesearch|" + json.dumps({"searching": [fmt]}))
                await ws.send("|updatesearch|" + json.dumps({"searching": []}))
                is_series = fmt.endswith("bo3") if self.series_for is None else fmt in self.series_for
                if fmt in self.no_series_for:
                    is_series = False
                if is_series:
                    await self.start_series(fmt)
                else:
                    self.n += 1
                    bid = f"battle-{fmt}-{self.n}"
                    await self.send_room(bid, [f"|title|{self.name} vs. {self.foe}", f"|player|p1|{self.name}|1|",
                                               f"|player|p2|{self.foe}|1|", f"|request|{json.dumps(REQUEST)}"])
            elif text.startswith("/choose ") and room.startswith("battle-"):
                if self.choose_delay:
                    asyncio.get_running_loop().call_later(self.choose_delay, lambda r=room: asyncio.ensure_future(self.end_game(r)))
                else:
                    await self.end_game(room)
            elif text == "/confirmready" and room in self.series:
                s = self.series[room]
                if s.get("waiting") and not self.never_ready:
                    s["waiting"] = False
                    await self.next_game(room)
            elif text.startswith("/forfeit") and room in self.series:
                s = self.series[room]
                self.ended.add(room)
                await self.send_room(room, [f"||{self.name} forfeited.", "|allowleave|", f"|win|{self.foe}"])
                if s.get("battle") and s["battle"] not in self.ended:
                    self.ended.add(s["battle"])
                    await self.send_room(s["battle"], [f"|-message|{self.name} forfeited.", f"|win|{self.foe}"])
            elif text.startswith("/forfeit") and room.startswith("battle-"):
                self.ended.add(room)
                await self.send_room(room, [f"|win|{self.foe}"])

    async def prompt(self, sid):
        s = self.series[sid]
        if s.get("waiting"):
            n = s["game"] + 1
            await self.send_room(sid, [
                f'|c|~|/uhtml controls,<div class="infobox"><p style="margin:6px">Are you ready for game {n}, {self.name}?</p>'
                f'<p style="margin:6px"><button class="button notifying" name="send" value="/msgroom {sid},/confirmready">I\'m ready!</button></p></div>'
            ], log=False)
        else:
            await self.send_room(sid, ["|c|~|/uhtml controls,"], log=False)

    async def start_series(self, fmt):
        self.n += 1
        sid = f"game-bestof3-{fmt}-{self.n}"
        self.series[sid] = {"fmt": fmt, "game": 0, "wins": {"me": 0, "foe": 0}, "waiting": False}
        await self.send_room(sid, [f"|title|{self.name} vs. {self.foe}", f"|j|☆{self.name}", f"|j|☆{self.foe}"])
        if self.dup_series_init:
            await self.init_room(sid)
        await self.send_room(sid, ["|cantleave|"], log=False)
        await self.prompt(sid)
        await self.next_game(sid)

    async def next_game(self, sid):
        s = self.series[sid]
        s["game"] += 1
        self.n += 1
        bid = f"battle-{s['fmt']}-{self.n}"
        s["battle"] = bid
        n = s["game"]
        await self.send_room(bid, [f"|title|{self.name} vs. {self.foe}", f"|j|☆{self.name}", f"|j|☆{self.foe}",
                                   f'|uhtml|bestof|<h2><strong>Game {n}</strong> of <a href="/{sid}">a best-of-3</a></h2>'])
        await self.send_room(sid, [f"|html|<h2>Game {n}</h2>", f'|uhtml|game{n}|<a href="/{bid}">{self.name} vs. {self.foe}</a>'])
        await self.send_room(bid, [f"|player|p1|{self.name}|1|", f"|player|p2|{self.foe}|1|", f"|request|{json.dumps(REQUEST)}"])
        if self.drop == (n, "request") and not self.dropped:
            self.dropped = True
            await self.abort()

    async def abort(self):
        ws, self.ws = self.ws, None
        if ws is not None:
            ws.transport.abort()

    async def end_game(self, bid):
        sid = next((k for k, s in self.series.items() if s.get("battle") == bid), None)
        if bid in self.ended:
            return
        if sid is None:  # a single battle
            self.ended.add(bid)
            await self.send_room(bid, ["|win|" + self.name])
            return
        s = self.series[sid]
        n = s["game"]
        who = self.winners[(n - 1) % len(self.winners)]
        winner = self.name if who == "me" else self.foe
        if who is not None:
            s["wins"][who] += 1
        game_lines = ["|", "|win|" + winner] if who is not None else ["|tie"]
        series_line = f"|html|{winner} won game {n}!" if who is not None else f"|html|Game {n} was a tie."
        self.ended.add(bid)
        if self.series_first:
            await self.send_room(sid, [series_line])
        if self.game_win:
            await self.send_room(bid, game_lines)
        if not self.series_first:
            await self.send_room(sid, [series_line])
        if max(s["wins"].values()) >= 2 or n >= 3:
            best = max(s["wins"], key=s["wins"].get)
            end = "|tie" if s["wins"]["me"] == s["wins"]["foe"] else f"|win|{self.name if best == 'me' else self.foe}"
            self.ended.add(sid)
            await self.send_room(sid, ["|allowleave|", end])
            return
        s["waiting"] = True
        if self.drop == (n, "between") and not self.dropped:
            self.dropped = True
            await self.abort()

            async def later():  # BestOfGame starts the next game without the player
                await asyncio.sleep(self.away_start)
                s["waiting"] = False
                await self.next_game(sid)

            asyncio.ensure_future(later())
            return
        await self.prompt(sid)

    async def __aenter__(self):
        self.server = await serve(self.handler, "127.0.0.1", 0)
        port = self.server.sockets[0].getsockname()[1]
        self.url = f"ws://127.0.0.1:{port}/showdown/websocket"
        return self

    async def __aexit__(self, *exc):
        self.server.close()
        for ws in self.sockets:
            ws.transport.abort()
        await self.server.wait_closed()


def bot(srv, **kw) -> ShowdownClient:
    kw.setdefault("handler", RandomRequestHandler(0))
    return ShowdownClient("bot", server=kw.pop("server", srv.url), **kw)


def leftovers():
    return [t for t in asyncio.all_tasks() if t is not asyncio.current_task() and t.get_name() != RUNNER]


def check_series(r, games=3, score=(2, 1)):
    assert isinstance(r, SeriesResult), r
    assert (r.won, (r.wins, r.losses), len(r.games), r.finished) == (score[0] > score[1], score, games, True), r
    assert [g.game for g in r.games] == list(range(1, games + 1))


# -- the ordinary series, and reconnects mid-series ---------------------------------------------


def test_a_series_leaves_nothing_behind():
    async def main():
        async with BoServer() as srv:
            async with bot(srv) as c:
                [r] = await c.ladder(1, FMT)
            return c, r, leftovers()

    c, r, left = run(main())
    check_series(r)
    assert r.opponent == "Foe" and left == []


@pytest.mark.parametrize("game", [1, 2])
def test_a_drop_mid_game_rejoins_the_series(game):
    async def main():
        async with BoServer(drop=(game, "request")) as srv:
            async with bot(srv) as c:
                [r] = await asyncio.wait_for(c.ladder(1, FMT), 20)
            return srv, c, r

    srv, c, r = run(main())
    assert srv.connections == 2
    check_series(r)
    assert len(c.results) == 3


@pytest.mark.parametrize("rejoin", [True, False])
def test_a_game_that_started_while_away_is_joined(rejoin):
    """Game 1 ends and the connection drops; game 2 starts while the player
    is away. Showdown's ``rejoinGames`` joins the player to it on login;
    without it, the client joins the game its series room names."""

    async def main():
        async with BoServer(drop=(1, "between"), away_start=0.2, no_rejoin=not rejoin) as srv:
            async with bot(srv) as c:
                [r] = await asyncio.wait_for(c.ladder(1, FMT), 20)
            return srv, c, r

    srv, c, r = run(main())
    check_series(r)
    assert len({g.room for g in r.games}) == 3
    game2 = r.games[1].room
    if rejoin:
        assert game2 not in srv.joins, "the server joined it: no /join"
    else:
        assert srv.joins.count(game2) == 1


def test_a_series_log_is_saved_once_after_a_second_init(tmp_path):
    async def main():
        async with BoServer(dup_series_init=True) as srv:
            async with bot(srv, log_dir=tmp_path) as c:
                [r] = await c.ladder(1, FMT)
            return r

    r = run(main())
    check_series(r)
    text = Path(r.log_path).read_text().splitlines()
    assert len([line for line in text if line.startswith("|title|")]) == 1


# -- how a game's end is announced -----------------------------------------------------------------


def test_the_series_room_may_announce_a_game_first():
    async def main():
        async with BoServer(series_first=True) as srv:
            async with bot(srv) as c:
                [r] = await asyncio.wait_for(c.ladder(1, FMT), 10)
            return r

    r = run(main())
    check_series(r)
    assert all(g.finished for g in r.games)


def test_a_game_only_the_series_room_announces_takes_its_winner(monkeypatch):
    monkeypatch.setattr(client_mod, "SERIES_END_GRACE", 0.5)

    async def main():
        async with BoServer(game_win=False, winners=("me", "foe", "me")) as srv:
            c = bot(srv)
            await c.connect()
            task = asyncio.ensure_future(c.ladder(1, FMT))
            # Each game's room stays open: the client moves on when the
            # series room names the next game, and ends the last with it.
            [r] = await asyncio.wait_for(task, 10)
            await c.close()
            return r

    r = run(main())
    assert (r.won, r.wins, r.losses, r.finished) == (True, 2, 1, True), r
    assert [(g.won, g.finished, g.reason) for g in r.games] == [(True, True, None), (False, True, None), (True, True, None)]


# -- a next game that never starts -----------------------------------------------------------------


def test_a_next_game_that_never_starts_ends_the_series(monkeypatch):
    monkeypatch.setattr(client_mod, "NEXT_GAME_TIMEOUT", 0.5)

    async def main():
        async with BoServer(never_ready=True) as srv:
            async with bot(srv) as c:
                [r] = await asyncio.wait_for(c.ladder(1, FMT), 10)
                assert not c.series
            return srv, r

    srv, r = run(main())
    assert not r.finished and "game 2 did not start" in r.reason, r
    assert len(r.games) == 1 and r.games[0].won is True
    assert f"{r.room}|/forfeit" in srv.received


def test_close_while_waiting_for_the_next_game_forfeits_the_series():
    async def main():
        async with BoServer(never_ready=True) as srv:
            c = bot(srv)
            await c.connect()
            task = asyncio.ensure_future(c.ladder(1, FMT))
            await until(lambda: any(s.results for s in c.series.values()), 5)
            await asyncio.sleep(0.3)
            assert not task.done()
            await c.close()
            [r] = await asyncio.wait_for(task, 5)
            return srv, r, leftovers()

    srv, r, left = run(main())
    assert [m for m in srv.received if m.endswith("/forfeit")] == [f"{r.room}|/forfeit"]
    assert r.won is False and r.finished and left == []


# -- formats the server plays another way ------------------------------------------------------------


def test_a_series_for_a_single_game_format_is_played(caplog):
    async def main():
        async with BoServer(series_for={SINGLE}) as srv:
            async with bot(srv) as c:
                return await asyncio.wait_for(c.ladder(1, SINGLE), 10)

    with caplog.at_level(logging.WARNING, logger="pikapro.showdown"):
        [r] = run(main())
    check_series(r)
    assert "series in a single-game format" in caplog.text


def test_a_series_format_played_as_single_games_is_played(caplog):
    async def main():
        async with BoServer(no_series_for={FMT}) as srv:
            async with bot(srv) as c:
                return await asyncio.wait_for(c.ladder(2, FMT), 10)

    with caplog.at_level(logging.WARNING, logger="pikapro.showdown"):
        rs = run(main())
    assert [type(r) for r in rs] == [BattleResult, BattleResult] and all(r.won for r in rs)
    assert "single game in a series format" in caplog.text


# -- concurrency and Showdown's game limit ---------------------------------------------------------


class Slow(RandomRequestHandler):
    def __init__(self, delay):
        super().__init__(0)
        self.delay = delay

    async def choose(self, rooms):
        await asyncio.sleep(self.delay)
        return await super().choose(rooms)


def test_a_series_takes_one_slot_on_a_server_without_limits():
    async def main():
        async with BoServer() as srv:
            async with bot(srv, max_concurrent=2, handler=Slow(0.02)) as c:
                peak = 0

                async def watch():
                    nonlocal peak
                    while True:
                        peak = max(peak, len([s for s in c.series.values() if not s.ended]))
                        await asyncio.sleep(0.002)

                w = asyncio.ensure_future(watch())
                rs = await c.ladder(4, FMT)
                w.cancel()
            return rs, peak

    rs, peak = run(main())
    assert len(rs) == 4 and all(isinstance(r, SeriesResult) for r in rs)
    assert peak == 2


def test_a_series_counts_as_two_games_against_showdowns_limit():
    """Showdown's ``user.games`` holds a series room and its game in
    progress, and ``Ladder.prepBattle`` refuses a start past 5 games: with
    ``max_games=5`` the client plays 2 series at once, not 5."""

    async def main():
        async with BoServer(limit=5, choose_delay=0.05) as srv:
            config = ServerConfig(websocket_url=srv.url, max_games=5)
            async with bot(srv, server=config, max_concurrent=5) as c:
                rs = await asyncio.wait_for(c.ladder(6, FMT), 30)
            return srv, rs

    srv, rs = run(main(), timeout=40)
    assert len(rs) == 6 and all(r.finished for r in rs)
    assert srv.popups == [] and srv.peak_games <= 4, (srv.popups, srv.peak_games)
    # Single games take one slot each: 5 at once.
    assert ShowdownClient("x", server=ServerConfig(websocket_url="ws://localhost:1/", max_games=5), max_concurrent=9).max_concurrent == 5


def test_the_game_limit_waits_for_a_game_to_end_then_retries(monkeypatch):
    """A refusal from the game limit (games elsewhere, or a server that
    counts differently) waits for a slot and tries again instead of
    failing the whole operation."""
    monkeypatch.setattr(client_mod, "GAME_LIMIT_WAIT", 0.2)
    refusals = [LIMIT_POPUP, LIMIT_POPUP]

    async def on_search(fake, ws, name, fmt):
        if refusals:
            await ws.send(f"|popup|{refusals.pop()}")
            return
        await ws.send(fake.searching(fmt))
        await ws.send(fake.new_room(fmt, name, extra="\n|win|bot"))

    async def main():
        async with Fake(on_search=on_search) as fake:
            async with ShowdownClient("bot", server=fake.url, handler=RandomRequestHandler(0)) as c:
                return await c.ladder(1, "gen9randombattle"), fake

    [r], fake = run(main())
    assert r.won is True and not refusals
    assert len([m for m in fake.received if "/search" in m]) == 3


def test_the_game_limit_with_no_game_of_ours_gives_up_with_a_reason(monkeypatch):
    monkeypatch.setattr(client_mod, "GAME_LIMIT_WAIT", 0.05)

    async def on_search(fake, ws, name, fmt):
        await ws.send(f"|popup|{LIMIT_POPUP}")

    async def main():
        async with Fake(on_search=on_search) as fake:
            async with ShowdownClient("bot", server=fake.url, handler=RandomRequestHandler(0)) as c:
                with pytest.raises(ShowdownError, match="elsewhere count too"):
                    await c.ladder(1, "gen9randombattle")
            return fake

    fake = run(main())
    assert len([m for m in fake.received if "/search" in m]) == client_mod.GAME_LIMIT_TRIES + 1


# -- a match refused after the search was accepted -------------------------------------------------


@pytest.mark.parametrize(
    "popup",
    [
        "The server will be restarting soon. Best-of-3 battles cannot be started at this time.",
        "The server is restarting. Battles will be available again in a few minutes.",
        "Your special battle settings don't match: \"x\" vs. \"undefined\"",
        None,  # no popup at all: the search just went away
    ],
)
@pytest.mark.parametrize("listed", [0.2, 0.0])
def test_a_match_refused_after_the_search_ends_the_search(monkeypatch, popup, listed):
    """Showdown's ``createBattle`` (``/prelockdown``, ``/lockdown``, special
    battle settings): the search was listed, the match cancels it and
    sends a popup, and no room comes. ``listed=0``: the list changes back
    at once (a 20 ms poll could miss it)."""
    monkeypatch.setattr(client_mod, "SEARCH_DROP_GRACE", 0.3)

    async def on_search(fake, ws, name, fmt):
        await ws.send(fake.searching(fmt))
        if listed:
            await asyncio.sleep(listed)
        await ws.send("|updatesearch|" + json.dumps({"searching": []}))
        if popup is not None:
            await ws.send(f"|popup|{popup}")

    async def main():
        async with Fake(on_search=on_search) as fake:
            async with ShowdownClient("bot", server=fake.url, handler=RandomRequestHandler(0)) as c:
                start = time.monotonic()
                with pytest.raises(ShowdownError) as e:
                    await asyncio.wait_for(c.ladder(1, FMT), 5)
                assert not c._waiters and not c.rooms
                return str(e.value), time.monotonic() - start

    text, took = run(main())
    assert took < 2
    # The popup is the error (it may come before the client saw the search
    # listed); with none, the drop check says what happened.
    assert (popup or "search ended without a battle: the server dropped the search") in text


def test_an_unconfirmed_search_is_a_showdown_error(monkeypatch):
    async def on_search(fake, ws, name, fmt):
        pass  # never answered

    async def main():
        async with Fake(on_search=on_search) as fake:
            async with ShowdownClient("bot", server=fake.url, handler=RandomRequestHandler(0)) as c:
                real = c._popup_or

                async def quick(fut, timeout, what="the request"):
                    return await real(fut, 0.2, what)

                monkeypatch.setattr(c, "_popup_or", quick)
                with pytest.raises(ShowdownError, match="did not confirm the gen9randombattle search"):
                    await c.ladder(1, "gen9randombattle")

    run(main())


# -- names that break a room's title -----------------------------------------------------------------


def test_title_opponent_reads_the_title_against_the_players_name():
    assert title_opponent("bot vs. abc vs. def", "bot") == "abc vs. def"
    assert title_opponent("abc vs. def vs. bot", "bot") == "abc vs. def"
    assert title_opponent("bot vs. Foe", "bot") == "Foe"
    assert title_opponent("abc vs. x vs. abc", "abc") is None  # either could be the player
    assert title_opponent("someone vs. else", "bot") is None


@pytest.mark.parametrize("mode", ["ladder", "challenge"])
def test_an_opponent_named_with_vs_is_read(mode):
    """A name Showdown allows ("abc vs. def" passes ``Chat.namefilter``)
    makes the series room's title "bot vs. abc vs. def"."""
    foe = "abc vs. def"

    class Challenged(BoServer):
        async def handler(self, ws):  # a challenge is accepted at once: a series
            orig = ws.recv

            async def recv(*a, **k):
                msg = await orig(*a, **k)
                room, _, text = msg.partition("|")
                if text.startswith("/challenge "):
                    self.received.append(msg)
                    fmt = text.split(", ", 1)[1]
                    await ws.send(f"|pm| {self.name}| {foe}|/challenge {fmt}|{fmt}|||")
                    await ws.send(f"|pm| {self.name}| {foe}|/challenge")
                    await self.start_series(fmt)
                    return await recv(*a, **k)
                return msg

            ws.recv = recv
            await super().handler(ws)

    async def main():
        async with Challenged(foe=foe) as srv:
            async with bot(srv) as c:
                if mode == "ladder":
                    [r] = await asyncio.wait_for(c.ladder(1, FMT), 10)
                else:
                    [r] = await asyncio.wait_for(c.challenge(foe, FMT), 10)
            return r

    r = run(main())
    check_series(r)
    assert r.opponent == foe and {g.opponent for g in r.games} == {foe}


# -- challenges: Showdown's cooldown ---------------------------------------------------------------


class ChallengeServer(Fake):
    """Challenges as Showdown's ``Ladders.makeChallenge`` treats them: the
    team is validated (``validation`` s), then the challenge is posted
    (the challenge ``|pm|``) and ``user.lastChallenge`` stamped; one within
    ``cooldown`` s of the last is refused. The opponent accepts at once."""

    def __init__(self, validation=0.0, cooldown=0.5, refuse_first=False, **kw):
        super().__init__(**kw)
        self.validation = validation
        self.cooldown = cooldown
        self.refuse_first = refuse_first
        self.last = -1e9
        self.refused = 0
        self.posted: List[float] = []

    async def handler(self, ws):
        orig = ws.recv

        async def recv(*a, **k):
            while True:
                msg = await orig(*a, **k)
                room, _, text = msg.partition("|")
                if not text.startswith("/challenge "):
                    return msg
                self.received.append(msg)
                target, fmt = text[len("/challenge "):].split(", ", 1)
                if time.monotonic() < self.last + self.cooldown or (self.refuse_first and not self.refused):
                    self.refused += 1
                    await ws.send(f"|popup|{TOO_SOON}")
                    continue
                await asyncio.sleep(self.validation)
                await ws.send(f"|pm| bot| {target}|/challenge {fmt}|{fmt}|||")
                self.last = time.monotonic()
                self.posted.append(self.last)
                await ws.send(f"|pm| bot| {target}|/challenge")
                await ws.send(self.new_room(fmt, "bot", target, extra="\n|win|bot"))

        ws.recv = recv
        await super().handler(ws)


def test_the_challenge_cooldown_runs_from_the_servers_acknowledgement():
    """The team validation takes longer than the cooldown's margin: counted
    from sending, the next challenge would come too soon."""

    async def main():
        async with ChallengeServer(validation=0.4, cooldown=0.5) as fake:
            config = replace(LOCAL, websocket_url=fake.url, challenge_interval=0.55)
            async with ShowdownClient("bot", server=config, handler=RandomRequestHandler(0)) as c:
                rs = await c.challenge("Foe", "gen9randombattle", n=3)
            return fake, rs

    fake, rs = run(main())
    assert len(rs) == 3 and all(r.won for r in rs)
    assert fake.refused == 0
    assert all(b - a >= 0.5 for a, b in zip(fake.posted, fake.posted[1:]))


def test_a_challenge_refused_as_too_soon_is_sent_again_once():
    async def main():
        async with ChallengeServer(refuse_first=True, cooldown=0.0) as fake:
            config = replace(LOCAL, websocket_url=fake.url, challenge_interval=0.2)
            async with ShowdownClient("bot", server=config, handler=RandomRequestHandler(0)) as c:
                rs = await c.challenge("Foe", "gen9randombattle")
            return fake, rs

    fake, rs = run(main())
    assert rs[0].won is True and fake.refused == 1
    assert len([m for m in fake.received if "/challenge " in m]) == 2


def test_a_challenge_refused_as_too_soon_twice_fails():
    async def main():
        async with ChallengeServer(cooldown=60.0) as fake:
            fake.last = time.monotonic()
            config = replace(LOCAL, websocket_url=fake.url, challenge_interval=0.2)
            async with ShowdownClient("bot", server=config, handler=RandomRequestHandler(0)) as c:
                with pytest.raises(ShowdownError, match="less than 10 seconds"):
                    await c.challenge("Foe", "gen9randombattle")
            return fake

    assert run(main()).refused == 2


# -- errors a first-time user meets ------------------------------------------------------------------


def test_errors_say_what_to_do():
    async def accept_first():
        c = ShowdownClient("bot")
        with pytest.raises(ShowdownError, match=r"call connect\(\)"):
            await c.accept_challenges()

    asyncio.run(accept_first())
    with pytest.raises(ValueError, match="takes no login"):
        ShowdownClient("bot", "hunter2", server="ws://127.0.0.1:8000/showdown/websocket")

    async def preset_name():
        async with Fake() as fake:
            async def formats(fake, ws, name):
                await ws.send("|formats|,LL|,1|pikapro|[Gen 9 Champions] VGC 2026 Reg M-C,5e")

            fake.on_login = formats
            async with ShowdownClient("bot", server=fake.url) as c:
                await until(lambda: bool(c.formats))
                with pytest.raises(ShowdownError, match="'gen9championsvgc2026regmc'"):
                    await c.ladder(1, "championsvgc")

    run(preset_name())


def test_close_says_the_client_was_closed():
    async def main():
        async with Fake(rooms_on_search=False) as fake:
            c = ShowdownClient("bot", server=fake.url, handler=RandomRequestHandler(0))
            await c.connect()
            task = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
            await until(lambda: "gen9randombattle" in c.searching)
            await c.close()
            with pytest.raises(ShowdownError, match="client was closed"):
                await asyncio.wait_for(task, 5)

    run(main())


def cli(*args: str, timeout: float = 60) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, "-m", "pikapro.showdown", *args], capture_output=True, text=True, timeout=timeout)


def test_cli_errors_end_cleanly(tmp_path):
    missing = cli("ladder", "--format", SINGLE, "--team", str(tmp_path / "team.txt"))
    assert missing.returncode != 0 and "Traceback" not in missing.stderr
    assert "--team" in missing.stderr and "sample_team" in missing.stderr
    port = _free_port()
    down = cli("ladder", "--format", "gen9randombattle", "--server", str(port), "--log-level", "ERROR")
    assert down.returncode == 1 and "Traceback" not in down.stderr
    assert "could not connect" in down.stderr and "--server" in down.stderr
    token = cli("ladder", "--format", "gen9randombattle", "--server", str(port), "--token", "x", "--username", "bot")
    assert token.returncode == 2 and "takes no login" in token.stderr
    refused = cli("formats", "--server", "psim.us")
    assert refused.returncode == 2 and "doesn't connect to" in refused.stderr and "Traceback" not in refused.stderr


def test_cli_a_refused_search_ends_with_a_summary(monkeypatch):
    """The CLI prints what finished, a summary and the error (no
    traceback) when a search ends without a battle."""

    async def serve_refusal():
        async def on_search(fake, ws, name, fmt):
            await ws.send(fake.searching(fmt))
            await ws.send("|updatesearch|" + json.dumps({"searching": []}))
            await ws.send("|popup|The server will be restarting soon. Best-of-3 battles cannot be started at this time.")

        async with Fake(on_search=on_search) as fake:
            proc = await asyncio.create_subprocess_exec(
                sys.executable, "-m", "pikapro.showdown", "ladder", "--format", FMT, "--handler", "random",
                "--server", fake.url, "--log-level", "ERROR",
                stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
            )
            out, err = await asyncio.wait_for(proc.communicate(), 30)
            return proc.returncode, out.decode(), err.decode()

    code, out, err = run(serve_refusal(), timeout=40)
    assert code == 1 and "Traceback" not in err, err
    assert "restarting soon" in err
    summary = json.loads(out.strip().splitlines()[-1])
    assert summary["summary"]["games"] == 0 and "restarting soon" in summary["error"]


def _free_port() -> int:
    import socket

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def test_many_series_leave_nothing_behind():
    async def main():
        async with BoServer() as srv:
            async with bot(srv, max_concurrent=5) as c:
                rs = await c.ladder(40, FMT)
                pending = (len(c._waiters), len(c._popups), len(c._pm_waiters), len(c.rooms), len(c.series),
                           len(c._room_handlers), len(c._series_handlers), c._slots.used)
            return rs, pending, leftovers()

    rs, pending, left = run(main(), timeout=120)
    assert len(rs) == 40
    assert pending == (0,) * 8, pending
    assert left == [], left


# -- redirects ----------------------------------------------------------------------------------------


class Redirector:
    """An HTTP server that answers every request with a 302 to ``location``."""

    def __init__(self, location: str):
        self.location = location
        self.requests = 0

    async def handler(self, reader, writer):
        await reader.readuntil(b"\r\n\r\n")
        self.requests += 1
        writer.write(f"HTTP/1.1 302 Found\r\nLocation: {self.location}\r\nContent-Length: 0\r\n\r\n".encode())
        await writer.drain()
        writer.close()

    async def __aenter__(self):
        self.server = await asyncio.start_server(self.handler, "127.0.0.1", 0)
        port = self.server.sockets[0].getsockname()[1]
        self.url = f"ws://127.0.0.1:{port}/showdown/websocket"
        return self

    async def __aexit__(self, *exc):
        self.server.close()
        await self.server.wait_closed()


def local_connections_only() -> List[str]:
    """Makes the running loop refuse to open a connection to another
    machine, and returns the list the hosts it was asked for go in (so a
    test of a redirect never reaches the network, fixed or not)."""
    loop = asyncio.get_running_loop()
    asked: List[str] = []
    create_connection = loop.create_connection

    async def create(factory, host=None, port=None, **kw):
        asked.append(host)
        if host != "127.0.0.1":
            raise OSError(f"the test doesn't connect to {host}")
        return await create_connection(factory, host, port, **kw)

    loop.create_connection = create
    return asked


@pytest.mark.parametrize(
    "location",
    [
        "wss://sim3.psim.us/showdown/websocket",
        "ws://sim3.psim.us:8000/showdown/websocket",
        "wss://SIM3.PSIM.US./showdown/websocket",
        "//play.pokemonshowdown.com/showdown/websocket",
    ],
)
def test_a_redirect_to_a_refused_host_is_not_followed(location):
    async def main():
        asked = local_connections_only()
        async with Redirector(location) as srv:
            with pytest.raises(ShowdownError, match="redirects elsewhere: pikapro.showdown doesn't connect to") as e:
                async with bot(srv):
                    pass
            return asked, srv.requests, str(e.value)

    asked, requests, error = run(main())
    assert asked == ["127.0.0.1"], asked  # nothing was opened where the redirect points
    assert requests == 1, "a refused redirect isn't retried"
    assert "psim.us" in error or "pokemonshowdown.com" in error


def test_a_redirect_that_cannot_be_read_is_not_followed():
    # Header values arrive as Latin-1: a host outside ASCII isn't the name it was sent as.
    async def main():
        asked = local_connections_only()
        async with Redirector("wss://sim3.psim。us/showdown/websocket") as srv:
            with pytest.raises(ShowdownError, match="redirects elsewhere: "):
                async with bot(srv):
                    pass
            return asked, srv.requests

    assert run(main()) == (["127.0.0.1"], 1)


def test_a_redirect_through_an_allowed_host_to_a_refused_one_is_not_followed():
    async def main():
        asked = local_connections_only()
        async with Redirector("wss://sim3.psim.us/showdown/websocket") as last, Redirector(last.url) as first:
            with pytest.raises(ShowdownError, match="doesn't connect to sim3.psim.us"):
                async with bot(first):
                    pass
            return asked, first.requests, last.requests

    assert run(main()) == (["127.0.0.1", "127.0.0.1"], 1, 1)


def test_a_redirect_to_an_allowed_host_is_followed():
    async def main():
        async with Fake() as srv, Redirector(srv.url) as redirector:
            async with bot(redirector) as c:
                return c.connected, redirector.requests

    assert run(main()) == (True, 1)


def test_a_redirect_never_sends_the_token_in_clear_to_another_machine():
    token_server = ServerConfig(websocket_url="ws://127.0.0.1:1/showdown/websocket", token_login=True)

    async def main(location, **kw):
        asked = local_connections_only()
        async with Redirector(location) as srv:
            with pytest.raises(ShowdownError) as e:
                async with ShowdownClient("bot", server=replace(token_server, websocket_url=srv.url),
                                          handler=RandomRequestHandler(0), reconnect=False, **kw):
                    pass
            return asked, str(e.value)

    # A local ws:// server may take a token; one it redirects to on another machine may not.
    asked, error = run(main("ws://example.com/showdown/websocket", token="a-secret-token"))
    assert asked == ["127.0.0.1"] and "unencrypted to ws://example.com/showdown/websocket" in error
    assert "a-secret-token" not in error
    # Over TLS it may, and a guest has no token to send: both follow the redirect.
    asked, _ = run(main("wss://example.com/showdown/websocket", token="a-secret-token"))
    assert asked == ["127.0.0.1", "example.com"]
    asked, _ = run(main("ws://example.com/showdown/websocket", guest=True))
    assert asked == ["127.0.0.1", "example.com"]


def test_the_arenas_own_redirect_is_allowed():
    from pikapro.showdown import ARENA

    old = replace(ARENA, websocket_url="wss://vgc.ellep.dev/showdown/websocket")
    assert client_mod._redirect_refusal(old, ARENA.websocket_url, token=True) is None
    assert "unencrypted" in client_mod._redirect_refusal(old, "ws://vgc.bot/showdown/websocket", token=True)
    assert "doesn't connect to" in client_mod._redirect_refusal(old, "wss://sim.psim.us/showdown/websocket", token=True)
