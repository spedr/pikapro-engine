"""pikapro.showdown and rooms the server closes, expires or renames, against
a scripted server.

Showdown closes a room with ``|deinit`` (``server/users.ts``), sends
``|expire|`` before it destroys one (``Room.expire``), and renames a battle
room when a player changes its privacy (``Room.setPrivate`` ->
``Room.rename``: ``>OLD`` / ``|noinit|rename|NEW|TITLE``, after which the
room's frames come under ``NEW``); a best-of series' privacy renames the
series room and each game's (``BestOfGame.setPrivacyOfGames``, which also
rewrites the links with ``|uhtmlchange|``)."""

from __future__ import annotations

import asyncio
import json
from typing import List

import pytest

pytest.importorskip("websockets")

from pikapro.showdown import RandomRequestHandler, SeriesResult, ShowdownClient  # noqa: E402

from _showdown_fakes import REQUEST, Fake, room_frame, run, until  # noqa: E402
from test_showdown_series import FMT, SeriesFake  # noqa: E402

RID = "battle-gen9randombattle-7"


class Recorder(RandomRequestHandler):
    """Remembers the rooms it opened and closed, by the id they had then."""

    def __init__(self):
        super().__init__(0)
        self.opened: List[str] = []
        self.closed: List[str] = []

    def open(self, room):
        self.opened.append(room.id)

    def close(self, room):
        self.closed.append(room.id)


def test_a_room_that_closes_ends_its_battle():
    """``|deinit`` in a room being played: its battle ends unfinished."""

    async def main():
        async with Fake() as fake:
            handler = Recorder()
            async with ShowdownClient("bot", server=fake.url, handler=handler) as c:
                task = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
                await until(lambda: bool(fake.chooses()))
                [rid] = list(c.rooms)
                await fake.sockets[0].send(f">{rid}\n|deinit")
                [r] = await asyncio.wait_for(task, 5)
                assert await asyncio.wait_for(c.wait_for_battles(), 5) == []
                return handler, c, rid, r

    handler, c, rid, r = run(main())
    assert (r.room, r.finished, r.won, r.reason) == (rid, False, None, "room closed")
    assert handler.opened == [rid] and handler.closed == [rid]
    assert not c.rooms and c.results == [r]


@pytest.mark.parametrize("closing", ["|deinit", "|noinit|nonexistent|The room does not exist.", "|expire|"])
def test_a_room_that_closes_before_it_is_claimed_ends(closing):
    """A battle room that hasn't named its players yet (so no operation
    or handler has it) and closes: it ends, and nothing waits for it."""

    async def main():
        async with Fake() as fake:
            handler = Recorder()
            async with ShowdownClient("bot", server=fake.url, handler=handler) as c:
                ws = fake.sockets[0]
                await ws.send(f">{RID}\n|init|battle\n|title|bot vs. Foe")
                await until(lambda: RID in c.rooms)
                waiting = asyncio.ensure_future(c.wait_for_battles())
                await asyncio.sleep(0.05)
                assert not waiting.done() and not handler.opened
                await ws.send(f">{RID}\n{closing}")
                [r] = await asyncio.wait_for(waiting, 5)
                return handler, c, r

    handler, c, r = run(main())
    reason = "room expired" if closing == "|expire|" else "room closed"
    assert (r.room, r.finished, r.reason, r.opponent) == (RID, False, reason, "Foe")
    assert not handler.opened and not handler.closed, "no handler ever had it"
    assert not c.rooms and c.results == [r]


def test_a_room_that_expires_ends_its_battle_once():
    """``Room.expire``: ``|expire|``, then the room is destroyed and its
    users leave (``|deinit``). One result, which says the room expired."""

    async def main():
        async with Fake() as fake:
            async with ShowdownClient("bot", server=fake.url, handler=Recorder()) as c:
                task = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
                await until(lambda: bool(fake.chooses()))
                [rid] = list(c.rooms)
                await fake.sockets[0].send(f">{rid}\n|expire|")
                await fake.sockets[0].send(f">{rid}\n|deinit")
                [r] = await asyncio.wait_for(task, 5)
                await asyncio.sleep(0.1)
                return c, rid, r

    c, rid, r = run(main())
    assert (r.room, r.finished, r.reason) == (rid, False, "room expired")
    assert c.results == [r] and not c.rooms


def test_a_renamed_room_is_followed_to_its_new_id(tmp_path):
    """A player made the battle private: the room goes on under its new
    id, with its state, its handler and its log."""

    async def main():
        async with Fake() as fake:
            handler = Recorder()
            async with ShowdownClient("bot", server=fake.url, handler=handler, log_dir=tmp_path) as c:
                task = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
                await until(lambda: len(fake.chooses()) == 1)
                [old] = list(c.rooms)
                room = c.rooms[old]
                new = f"{old}-0123456789abcdefghijklmnopqrstupw"
                ws = fake.sockets[0]
                await ws.send(f">{old}\n|noinit|rename|{new}|bot vs. Foe")
                await until(lambda: new in c.rooms)
                assert c.rooms[new] is room and old not in c.rooms and room.id == new
                assert not task.done() and not handler.closed
                # The battle goes on under the new id: the next request is
                # answered there.
                await ws.send(f">{new}\n|turn|2\n|request|{json.dumps({**REQUEST, 'rqid': 2})}")
                await until(lambda: len(fake.chooses()) == 2)
                await ws.send(f">{new}\n|win|bot")
                [r] = await asyncio.wait_for(task, 5)
                await until(lambda: f"|/leave {new}" in fake.received)
                return fake, handler, c, old, new, r

    fake, handler, c, old, new, r = run(main())
    first, second = fake.chooses()
    assert first.startswith(f"{old}|/choose ") and second.startswith(f"{new}|/choose ") and second.endswith("|2")
    assert (r.room, r.won, r.finished, r.reason, r.turns) == (new, True, True, None, 2)
    assert handler.opened == [old] and handler.closed == [new], "one handler, opened once"
    assert not c.rooms and c.results == [r]
    # One log, under the new id, with the lines from before the rename.
    log = (tmp_path / f"{new}.log").read_text()
    assert "|player|p1|bot|1|" in log and f"|noinit|rename|{new}|" in log and "|win|bot" in log
    assert not (tmp_path / f"{old}.log").exists()
    assert f"|/leave {old}" not in fake.received


def test_two_renames_in_one_frame_are_both_followed():
    async def main():
        async with Fake() as fake:
            handler = Recorder()
            async with ShowdownClient("bot", server=fake.url, handler=handler) as c:
                task = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
                await until(lambda: len(fake.chooses()) == 1)
                [old] = list(c.rooms)
                room = c.rooms[old]
                mid, new = f"{old}-aaaapw", f"{old}-bbbbpw"
                ws = fake.sockets[0]
                await ws.send(f">{old}\n|noinit|rename|{mid}|t\n|noinit|rename|{new}|t\n|turn|2")
                await until(lambda: new in c.rooms)
                assert c.rooms[new] is room and list(c.rooms) == [new] and room.turn == 2
                await ws.send(f">{new}\n|win|bot")
                [r] = await asyncio.wait_for(task, 5)
                return handler, old, new, r

    handler, old, new, r = run(main())
    assert (r.room, r.won, r.turns) == (new, True, 2)
    assert handler.opened == [old] and handler.closed == [new]


def test_a_room_renamed_after_its_end_is_left_under_its_new_id():
    """``leave_delay``: the ``/leave`` goes to the id the room has when it
    is sent, not the one it had when the battle ended."""

    async def main():
        async with Fake() as fake:
            async with ShowdownClient("bot", server=fake.url, handler=Recorder(), leave_delay=0.3) as c:
                task = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
                await until(lambda: len(fake.chooses()) == 1)
                [old] = list(c.rooms)
                new = f"{old}-aaaapw"
                ws = fake.sockets[0]
                await ws.send(f">{old}\n|win|bot")
                [r] = await asyncio.wait_for(task, 5)
                await ws.send(f">{old}\n|noinit|rename|{new}|t")
                await until(lambda: any("/leave" in m for m in fake.received), timeout=3)
                await asyncio.sleep(0.1)
                return fake, c, old, new, r

    fake, c, old, new, r = run(main())
    assert [m for m in fake.received if "/leave" in m] == [f"|/leave {new}"]
    assert r.room == old and r.won is True and not c._leaving and not c.rooms


def test_a_game_renamed_before_it_is_joined_is_joined_under_its_new_id():
    """A series' game the client isn't in yet is joined a moment after the
    series names it: under the id the series names by then."""

    async def main():
        async with Fake() as fake:
            async with ShowdownClient("bot", server=fake.url, handler=Recorder()) as c:
                sid, old = f"game-bestof3-{FMT}-1", f"battle-{FMT}-2"
                new = f"{old}-aaaapw"
                ws = fake.sockets[0]
                await ws.send(f">{sid}\n|init|battle\n|title|bot vs. Foe\n|j|☆bot\n|j|☆Foe")
                await ws.send(f">{sid}\n|uhtml|game1|<a href=\"/{old}\">bot vs. Foe</a>")
                await until(lambda: sid in c.series and c.series[sid].games == {1: old})
                await ws.send(f">{sid}\n|uhtmlchange|game1|<a href=\"/{new}\">bot vs. Foe</a>")
                await until(lambda: any("/join" in m for m in fake.received), timeout=5)
                await asyncio.sleep(0.3)
                return fake, new

    fake, new = run(main())
    assert [m for m in fake.received if "/join" in m] == [f"|/join {new}"]


def test_a_rename_to_an_id_the_client_cant_use_ends_the_battle():
    """A new id that isn't a battle room's (it would go back into the
    client's commands): the battle ends, as when its room closes."""

    async def main(new):
        async with Fake() as fake:
            async with ShowdownClient("bot", server=fake.url, handler=Recorder()) as c:
                task = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
                await until(lambda: bool(fake.chooses()))
                [old] = list(c.rooms)
                await fake.sockets[0].send(f">{old}\n|noinit|rename|{new}|bot vs. Foe")
                [r] = await asyncio.wait_for(task, 5)
                return c, old, r

    for new in ["lobby", "battle-Gen9\tx-1", f"game-bestof3-{FMT}-1", ""]:
        c, old, r = run(main(new))
        assert (r.room, r.finished, r.reason) == (old, False, "room renamed to an id the client can't use")
        assert not c.rooms


class RenamingSeries(SeriesFake):
    """A best-of-3 whose opponent makes it private while game 1 is played
    (``BestOfGame``'s room, then ``setPrivacyOfGames``): at the player's
    first choice the series room and the game's are renamed, and the
    series goes on under the new ids. ``only_game``: the game's room alone
    is renamed (``/hideroom`` in the battle room)."""

    def __init__(self, winners, *, only_game=False, **kw):
        super().__init__(winners, **kw)
        self.only_game = only_game
        self.renamed: List[tuple] = []

    async def end_game(self, ws, name, bid):
        if not self.renamed:
            sid = next(k for k, s in self.series.items() if s.get("battle") == bid)
            s = self.series[sid]
            new_bid = f"{bid}-0123456789abcdefghijklmnopqrstupw"
            new_sid = sid if self.only_game else f"{sid}-abcdefghijklmnopqrstu0123456789pw"
            if new_sid != sid:
                self.series[new_sid] = self.series.pop(sid)
                await ws.send(f">{sid}\n|noinit|rename|{new_sid}|{name} vs. Foe")
            s["battle"] = new_bid
            await ws.send(f">{bid}\n|noinit|rename|{new_bid}|{name} vs. Foe")
            await ws.send(f">{new_sid}\n|uhtmlchange|game1|<a href=\"/{new_bid}\">{name} vs. Foe</a>")
            await ws.send(
                f">{new_bid}\n|uhtmlchange|bestof|<h2><strong>Game 1</strong> of "
                f"<a href=\"/{new_sid}\">a best-of-3</a></h2>"
            )
            self.renamed.append((sid, new_sid, bid, new_bid))
            # The choice went to a room that is no more: asked again.
            await ws.send(f">{new_bid}\n|request|{json.dumps({**REQUEST, 'rqid': 2})}")
            return
        await super().end_game(ws, name, bid)


@pytest.mark.parametrize("only_game", [False, True])
def test_a_series_renamed_mid_game_is_played_to_its_end(only_game):
    async def main():
        async with RenamingSeries(["me", "foe", "me"], only_game=only_game) as fake:
            handler = Recorder()
            async with ShowdownClient("bot", server=fake.url, handler=handler) as c:
                results = await asyncio.wait_for(c.ladder(1, FMT), 20)
            return fake, handler, c, results

    fake, handler, c, results = run(main())
    [(sid, new_sid, bid, new_bid)] = fake.renamed
    [r] = results
    assert isinstance(r, SeriesResult)
    assert (r.room, r.won, r.wins, r.losses, r.finished, r.reason) == (new_sid, True, 2, 1, True, None)
    assert [g.won for g in r.games] == [True, False, True] and [g.game for g in r.games] == [1, 2, 3]
    assert r.games[0].room == new_bid and {g.series for g in r.games} == {new_sid}
    # Game 1 was one battle for the handler; the three games each opened once.
    assert handler.opened[0] == bid and handler.closed[0] == new_bid and len(handler.opened) == 3
    # Ready for games 2 and 3 in the series room as it is now called.
    assert fake.ready == [new_sid, new_sid]
    assert f"|/leave {new_sid}" in fake.received and f"|/leave {new_bid}" in fake.received
    assert c.series_results == [r] and len(c.results) == 3 and not c.series and not c.rooms


def test_frames_for_a_room_the_client_left_are_ignored():
    """A rename or a close of a room the client doesn't have."""

    async def main():
        async with Fake() as fake:
            async with ShowdownClient("bot", server=fake.url, handler=Recorder()) as c:
                ws = fake.sockets[0]
                await ws.send(f">{RID}\n|noinit|rename|{RID}-xpw|a vs. b")
                await ws.send(f">{RID}\n|deinit")
                await ws.send(f">{RID}\n|expire|")
                await ws.send(f">game-bestof3-{FMT}-1\n|noinit|rename|game-bestof3-{FMT}-1-xpw|a vs. b")
                await ws.send(room_frame("battle-gen9randombattle-9", "bot", extra="\n|win|bot"))
                await until(lambda: bool(c.results))
                return c

    c = run(main())
    assert [r.room for r in c.results] == ["battle-gen9randombattle-9"] and not c.rooms and not c.series
