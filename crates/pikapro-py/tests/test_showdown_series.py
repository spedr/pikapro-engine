"""pikapro.showdown's best-of series against a scripted server speaking
Showdown's ``BestOfGame`` protocol (``server/room-battle-bestof.ts``): the
series room names each game's battle room, asks whether the player is
ready for the next game, and announces the games' and the series'
winners. The real servers are ``test_showdown_servers.py``'s."""

from __future__ import annotations

import asyncio
import json
from typing import Dict, List

import pytest

pytest.importorskip("websockets")

from pikapro.showdown import (  # noqa: E402
    BattleResult,
    RandomRequestHandler,
    SeriesResult,
    ShowdownClient,
    format_key,
    series_length,
)
from pikapro.showdown.protocol import parse_challenge_pm, series_room_format  # noqa: E402

from _showdown_fakes import REQUEST, Fake, run, until  # noqa: E402

FMT = "gen9championsvgc2026regmcbo3"


class SeriesFake(Fake):
    """A Showdown server that runs a best-of-3 for each search: ``winners``
    are the games' winners (``"me"``, ``"foe"``, ``None`` a tie) in order;
    a game ends when the player chooses. ``early_battle`` sends the first
    game's battle room before the series room (a join's order the client
    must not depend on); ``otsrequest`` offers open sheets in each game.
    ``hold`` never ends a game (for forfeits)."""

    def __init__(self, winners, *, early_battle=False, otsrequest=False, hold=False, **kw):
        super().__init__(**kw)
        self.winners = list(winners)
        self.early_battle = early_battle
        self.otsrequest = otsrequest
        self.hold = hold
        self.series: Dict[str, dict] = {}
        self.ready: List[str] = []

    async def handler(self, ws):
        self.connections += 1
        self.sockets.append(ws)
        await ws.send("|challstr|4|abcdef")
        name = None
        async for msg in ws:
            self.received.append(msg)
            room, _, text = msg.partition("|")
            if text.startswith("/trn "):
                name = text[5:].split(",", 1)[0]
                await ws.send(f"|updateuser| {name}|1|1|{{}}")
            elif text.startswith("/search "):
                fmt = text[len("/search ") :]
                await ws.send(self.searching(fmt))
                await ws.send("|updatesearch|" + json.dumps({"searching": []}))
                await self.start_series(ws, name, fmt)
            elif text.startswith("/choose ") and room.startswith("battle-") and not self.hold:
                await self.end_game(ws, name, room)
            elif text == "/confirmready" and room in self.series:
                self.ready.append(room)
                await self.next_game(ws, name, room)
            elif text.startswith("/forfeit") and room in self.series:
                s = self.series[room]
                await ws.send(f">{room}\n||{name} forfeited.\n|allowleave|\n|win|Foe")
                await ws.send(f">{s['battle']}\n|-message|{name} forfeited.\n|win|Foe")

    async def start_series(self, ws, name, fmt):
        self.rooms += 1
        sid = f"game-bestof3-{fmt}-{self.rooms}"
        self.series[sid] = {"fmt": fmt, "game": 0, "name": name, "wins": {"me": 0, "foe": 0}}
        if self.early_battle:
            await self.next_game(ws, name, sid, announce=False)
        await ws.send(f">{sid}\n|init|battle\n|title|{name} vs. Foe\n|j|☆{name}\n|j|☆Foe")
        await ws.send(f">{sid}\n|cantleave|\n|tempnotifyoff|choice\n|c|~|/uhtml controls,")
        if self.early_battle:
            await self.announce(ws, sid)
        else:
            await self.next_game(ws, name, sid)

    async def next_game(self, ws, name, sid, announce=True):
        s = self.series[sid]
        s["game"] += 1
        self.rooms += 1
        bid = f"battle-{s['fmt']}-{self.rooms}"
        s["battle"] = bid
        n = s["game"]
        await ws.send(f">{bid}\n|init|battle\n|title|{name} vs. Foe\n|j|☆{name}\n|j|☆Foe")
        await ws.send(
            f">{bid}\n|html|<table width=\"100%\"><tr><td>{name}</td><td>Foe</td></tr></table>\n"
            f"|uhtml|bestof|<h2><strong>Game {n}</strong> of <a href=\"/{sid}\">a best-of-3</a></h2>"
        )
        if announce:
            await self.announce(ws, sid)
        extra = ""
        if self.otsrequest:
            extra = '\n|uhtml|otsrequest|<button name="send" value="/acceptopenteamsheets" class="button">Accept</button>'
        await ws.send(f">{bid}\n|player|p1|{name}|1|\n|player|p2|Foe|1|{extra}\n|request|{json.dumps(REQUEST)}")

    async def announce(self, ws, sid):
        s = self.series[sid]
        n = s["game"]
        await ws.send(f">{sid}\n|html|<h2>Game {n}</h2>\n|uhtml|game{n}|<a href=\"/{s['battle']}\">{s['name']} vs. Foe</a>")

    async def end_game(self, ws, name, bid):
        sid = next(k for k, s in self.series.items() if s.get("battle") == bid)
        s = self.series[sid]
        n = s["game"]
        who = self.winners[(n - 1) % len(self.winners)]
        if who is None:
            await ws.send(f">{bid}\n|tie")
            await ws.send(f">{sid}\n|html|Game {n} was a tie.")
        else:
            winner = name if who == "me" else "Foe"
            s["wins"][who] += 1
            await ws.send(f">{bid}\n|\n|win|{winner}")
            await ws.send(f">{sid}\n|html|{winner} won game {n}!")
        if max(s["wins"].values()) >= 2 or n >= 3:
            best = max(s["wins"], key=s["wins"].get)
            end = "|tie" if s["wins"]["me"] == s["wins"]["foe"] else f"|win|{name if best == 'me' else 'Foe'}"
            await ws.send(f">{sid}\n|allowleave|\n{end}")
            return
        await ws.send(
            f">{sid}\n|c|~|/uhtml controls,<div class=\"infobox\"><p style=\"margin:6px\">Are you ready for game {n + 1}, {name}?</p>"
            f"<p style=\"margin:6px\"><button class=\"button notifying\" name=\"send\" value=\"/msgroom {sid},/confirmready\">I'm ready!</button></p></div>"
        )


class Recorder(RandomRequestHandler):
    """Remembers the rooms it opened (with their series and game)."""

    def __init__(self):
        super().__init__(0)
        self.opened: List[tuple] = []

    def open(self, room):
        self.opened.append((room.id, room.series.id if room.series else None, room.game))


def test_protocol_helpers():
    assert series_length("gen9championsvgc2026regmcbo3") == 3
    assert series_length("gen9championsvgc2026regmc") is None
    assert series_length("gen9vgc2025regi@@@Best of = 5") == 5
    assert series_length("gen9vgc2025regi@@@!Team Preview") is None
    assert format_key("gen9vgc2025regi@@@ Best of = 3 , ") == "gen9vgc2025regi@@@Best of = 3"
    assert format_key("[Gen 9] VGC 2025 Reg I") == "gen9vgc2025regi"
    assert series_room_format("game-bestof3-gen9championsvgc2026regmcbo3-12") == ("gen9championsvgc2026regmcbo3", 3)
    assert series_room_format("battle-gen9ou-1") == ("", None)
    pm = "|pm| Alice| Bob|/challenge gen9vgc2025regi@@@Best of = 3|gen9vgc2025regi|||"
    assert parse_challenge_pm(pm) == ("alice", "bob", "gen9vgc2025regi@@@Best of = 3")


@pytest.mark.parametrize("early", [False, True])
def test_a_series_is_played_game_by_game(early):
    async def main():
        async with SeriesFake(["me", "foe", "me"], early_battle=early) as fake:
            handler = Recorder()
            async with ShowdownClient("bot", server=fake.url, handler=handler) as c:
                results = await c.ladder(1, FMT)
            return fake, handler, c, results

    fake, handler, c, results = run(main())
    [r] = results
    assert isinstance(r, SeriesResult)
    assert (r.won, r.wins, r.losses, r.ties, r.finished, r.best_of) == (True, 2, 1, 0, True, 3)
    assert [g.won for g in r.games] == [True, False, True] and all(isinstance(g, BattleResult) for g in r.games)
    assert [g.game for g in r.games] == [1, 2, 3] and {g.series for g in r.games} == {r.room}
    assert r.format == FMT and r.opponent == "Foe" and r.side == "p1"
    # Every game was the series' handler's, seen as a game of the series.
    assert [(s, g) for _, s, g in handler.opened] == [(r.room, 1), (r.room, 2), (r.room, 3)]
    # Ready for games 2 and 3, once each; the rooms were left.
    assert fake.ready == [r.room, r.room]
    assert f"|/leave {r.room}" in fake.received
    assert c.series_results == [r] and len(c.results) == 3 and not c.series and not c.rooms


def test_a_series_can_end_in_two_games_or_a_tie():
    async def main(winners):
        async with SeriesFake(winners) as fake:
            async with ShowdownClient("bot", server=fake.url, max_concurrent=2) as c:
                return await c.ladder(2, FMT)

    rs = run(main(["foe", "foe"]))
    assert [(r.won, r.wins, r.losses, len(r.games)) for r in rs] == [(False, 0, 2, 2)] * 2
    [r, _] = run(main([None, "me", "foe"]))
    assert (r.won, r.tie, r.wins, r.losses, r.ties, r.finished) == (None, True, 1, 1, 1, True)


def test_close_forfeits_the_series_in_its_room():
    async def main():
        async with SeriesFake(["me"], hold=True) as fake:
            c = ShowdownClient("bot", server=fake.url)
            await c.connect()
            task = asyncio.ensure_future(c.ladder(1, FMT))
            await until(lambda: bool(c.rooms) and all(r.request for r in c.rooms.values()))
            await c.close()
            [r] = await asyncio.wait_for(task, 10)
            return fake, r

    fake, r = run(main())
    forfeits = [m for m in fake.received if m.endswith("|/forfeit")]
    assert forfeits == [f"{r.room}|/forfeit"], "the series, not only its game"
    assert r.won is False and r.finished and len(r.games) == 1


def test_open_team_sheets_are_accepted_in_a_series():
    async def main(**kw):
        async with SeriesFake(["me", "me"], otsrequest=True) as fake:
            async with ShowdownClient("bot", server=fake.url, **kw) as c:
                await c.ladder(1, FMT)
            return [m for m in fake.received if "openteamsheets" in m]

    accepted = run(main())
    assert len(accepted) == 2 and all(m.endswith("|/acceptopenteamsheets") for m in accepted)
    rejected = run(main(open_team_sheets=False))
    assert len(rejected) == 2 and all(m.endswith("|/rejectopenteamsheets") for m in rejected)


def test_an_offer_outside_a_series_is_left_alone_by_default():
    extra = '\n|uhtml|otsrequest|<button name="send" value="/acceptopenteamsheets" class="button">Accept</button>'

    async def on_search(fake, ws, name, fmt):
        await ws.send(fake.searching(fmt))
        await ws.send(fake.new_room(fmt, name, extra=extra))

    async def main(**kw):
        async with Fake(on_search=on_search) as fake:
            c = ShowdownClient("bot", server=fake.url, **kw)
            await c.connect()
            task = asyncio.ensure_future(c.ladder(1, "gen9championsvgc2026regmc"))
            await until(lambda: bool(c.rooms))
            await asyncio.sleep(0.2)
            await c.close()
            await asyncio.wait_for(task, 10)
            return [m for m in fake.received if "openteamsheets" in m]

    assert run(main()) == []
    assert [m.split("|")[1] for m in run(main(open_team_sheets=True))] == ["/acceptopenteamsheets"]
