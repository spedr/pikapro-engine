"""pikapro.showdown's client against a scripted local server: bad battles,
handler bugs, untrusted input, and a connection that ends for good.

The fake server (``Fake``) speaks just enough of Showdown's protocol to log
in, search, accept and open battle rooms; each test scripts the rest. No
network beyond localhost.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import time
from typing import List

import numpy as np
import pytest

pytest.importorskip("websockets")

from websockets.asyncio.server import serve  # noqa: E402

import pikapro  # noqa: E402
from pikapro.rl import view  # noqa: E402
from pikapro.showdown import (  # noqa: E402
    BattleHandler,
    BattleRoom,
    RandomRequestHandler,
    SessionHandler,
    ShowdownClient,
    LoginError,
    ShowdownError,
    pack_team,
    pikapro_server,
    to_id,
)
from pikapro.showdown.client import _log_name, _Waiter  # noqa: E402

from _showdown_fakes import REQUEST, RUNNER, Fake, FakeSession, room_frame, run, until  # noqa: E402

def client(fake: Fake, **kwargs) -> ShowdownClient:
    kwargs.setdefault("handler", RandomRequestHandler(0))
    return ShowdownClient("bot", server=fake.url, **kwargs)


# -- battles the handler can't play --------------------------------------------------------------


def test_unsupported_formats_are_refused_up_front():
    async def main():
        async with Fake() as fake:
            async with client(fake, handler=SessionHandler("random")) as c:
                with pytest.raises(ShowdownError, match="can't play"):
                    await c.ladder(1, "gen9ubers")
                with pytest.raises(ShowdownError, match="can't play"):
                    await c.ladder(1, "gen9ubersbo3")  # a series in a format it can't play
                assert not [m for m in fake.received if "/search" in m]

    run(main())


class NotAnException(BaseException):
    """A BaseException that isn't an Exception, as PyO3's PanicException
    is: ``except Exception`` lets it through."""


class Breaks(RandomRequestHandler):
    """Raises in one method for rooms of one format."""

    def __init__(self, where: str, fmt: str = "gen9ou"):
        super().__init__(0)
        self.where, self.fmt, self.closed = where, fmt, []

    def open(self, room):
        if self.where == "open" and room.format == self.fmt:
            raise ValueError("unsupported Showdown format")

    def feed(self, room, lines):
        if self.where == "feed" and room.format == self.fmt:
            raise RuntimeError("handler bug")
        if self.where == "feed-base" and room.format == self.fmt:
            raise NotAnException("handler bug")
        return super().feed(room, lines)

    def close(self, room):
        self.closed.append(room.id)


@pytest.mark.parametrize("where", ["open", "feed", "feed-base"])
def test_a_bad_battle_is_forfeited_and_the_client_lives(where, caplog):
    """A room the handler fails on (a format the session can't read, a
    bug in feed) is forfeited and recorded; the client plays on."""

    async def on_login(fake, ws, name):
        await ws.send(fake.new_room("gen9ou", name))  # nobody asked for it

    async def main():
        handler = Breaks(where)
        async with Fake(on_login=on_login) as fake:
            async with client(fake, handler=handler) as c:
                await until(lambda: any(r.format == "gen9ou" for r in c.results))
                bad = c.results[0]
                assert not bad.finished and "handler error" in bad.reason
                await until(lambda: f"{bad.room}|/forfeit" in fake.received)
                assert bad.room in handler.closed
                # The connection and the other battles go on.
                task = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
                await until(lambda: len(fake.chooses()) == 1)
                assert not c._run_task.done()
                room = next(iter(c.rooms))
                for ws in fake.sockets:
                    await ws.send(f">{room}\n|win|bot")
                results = await asyncio.wait_for(task, 5)
                assert results[0].won is True

    with caplog.at_level(logging.ERROR, logger="pikapro.showdown"):
        run(main())
    assert any(getattr(r, "event", None) == "handler error" for r in caplog.records)


class NoOU(RandomRequestHandler):
    def supports(self, format):
        return format != "gen9ou"


def test_accept_takes_only_formats_the_handler_plays():
    async def on_login(fake, ws, name):
        fake.challenge_formats = {"ou": "gen9ou", "rando": "gen9randombattle"}
        await ws.send(f"|pm|ou|{name}|/challenge gen9ou|gen9ou|||")
        await ws.send(f"|pm|rando|{name}|/challenge gen9randombattle|gen9randombattle|||")

    async def main():
        async with Fake(on_login=on_login) as fake:
            async with client(fake, handler=NoOU(0)) as c:
                task = asyncio.ensure_future(c.accept_challenges(n=1))
                await until(lambda: bool(c.rooms))
                assert "|/accept rando" in fake.received and "|/accept ou" not in fake.received
                room = next(iter(c.rooms))
                for ws in fake.sockets:
                    await ws.send(f">{room}\n|win|bot")
                results = await asyncio.wait_for(task, 5)
                assert results[0].opponent == "rando"

    run(main())


# -- the connection ending for good -----------------------------------------------------------------


def test_giving_up_ends_every_operation():
    """A battle in progress, a search waiting for its room, an accept
    waiting for a challenge, wait_for_battles: all end promptly when the
    client gives up reconnecting."""

    async def main():
        fake = Fake(rooms_on_search=False)
        await fake.__aenter__()
        c = client(fake, max_reconnects=1, max_concurrent=3)
        await c.connect()
        playing = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
        await until(lambda: "|/search gen9randombattle" in fake.received)
        for ws in fake.sockets:  # the room of the search, then another search that waits
            await ws.send(fake.new_room("gen9randombattle", "bot"))
        await until(lambda: bool(c.rooms))
        searching = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
        accepting = asyncio.ensure_future(c.accept_challenges(n=1))
        waiting = asyncio.ensure_future(c.wait_for_battles())
        await until(lambda: fake.received.count("|/search gen9randombattle") == 2)
        t0 = time.monotonic()
        await fake.stop()  # for good: the reconnect fails
        results = await asyncio.wait_for(playing, 15)
        assert results[0].finished is False and "disconnected" in results[0].reason
        assert [r.room for r in await asyncio.wait_for(waiting, 1)] == [results[0].room]
        for task in (searching, accepting):
            with pytest.raises(ShowdownError, match="gave up"):
                await asyncio.wait_for(task, 1)
        assert time.monotonic() - t0 < 10  # one backoff (1 s) and one refused connect
        with pytest.raises(ShowdownError, match="gave up"):
            await c.ladder(1, "gen9randombattle")
        await asyncio.wait_for(c.close(), 2)

    run(main())


def test_a_login_error_on_reconnect_ends_every_operation():
    async def on_login(fake, ws, name):
        if fake.logins > 1:
            await ws.send("|nametaken|bot|Someone is already using the name")

    async def main():
        async with Fake(on_login=on_login, rooms_on_search=False) as fake:
            c = client(fake)
            await c.connect()
            searching = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
            await until(lambda: "|/search gen9randombattle" in fake.received)
            for ws in fake.sockets:
                ws.transport.abort()
            with pytest.raises(ShowdownError, match="already using"):
                await asyncio.wait_for(searching, 5)
            await c.close()

    run(main())


def test_the_reconnect_limit_counts_failures_in_a_row():
    """Each connection logs in, then drops: every reconnect succeeds, so
    the client never gives up (it did after max_reconnects drops in all)."""

    async def main():
        async with Fake(drop_after=0.2) as fake:
            c = client(fake, max_reconnects=1)
            await c.connect()
            await until(lambda: fake.logins >= 4, timeout=15)
            assert not c._run_task.done() and c._fatal is None
            await c.close()

    run(main(), timeout=30)


def test_a_server_that_closes_after_each_login_is_retried_ever_more_slowly(monkeypatch, caplog):
    """Each connection logs in and is closed at once: the backoff keeps
    doubling up to its cap (it started over at every login, so the client
    logged in, and sent its token, about once a second for good). A
    connection that stays up for ``BACKOFF_RESET_S`` starts it over."""
    from pikapro.showdown import client as client_module

    monkeypatch.setattr(client_module, "BACKOFF_MAX_S", 0.4)
    monkeypatch.setattr(client_module, "BACKOFF_RESET_S", 0.6)
    caplog.set_level(logging.INFO, logger="pikapro.showdown")

    def delays():
        return [r.fields["delay"] for r in caplog.records if getattr(r, "event", "") == "reconnecting"]

    async def main():
        async with Fake(drop_after=0.05) as fake:
            c = client(fake, max_reconnects=1)
            await c.connect()
            c._delay = 0.05  # (connect() starts it at a second)
            await until(lambda: len(delays()) >= 6, timeout=15)
            assert not c._run_task.done() and c._fatal is None, "it never gives up: each login is a success"
            assert delays()[:6] == [0.05, 0.1, 0.2, 0.4, 0.4, 0.4]
            # A connection that lasts ends the backoff.
            fake.drop_after = 0.8
            await until(lambda: delays()[-1] == 1.0, timeout=15)
            await c.close()
            return fake

    fake = run(main(), timeout=40)
    assert fake.logins >= 7


def test_messages_queued_while_disconnected_go_after_the_next_login():
    """The login goes before anything queued meanwhile (which waits for
    it): no deadlock, and nothing is sent as a guest."""

    async def main():
        async with Fake() as fake:
            async with client(fake) as c:
                for ws in fake.sockets:
                    ws.transport.abort()
                await until(lambda: not c.connected)
                await c.send("/cmd hello")
                await until(lambda: "|/cmd hello" in fake.received)
                assert fake.logins == 2
                trn = max(i for i, m in enumerate(fake.received) if m.startswith("|/trn "))
                assert fake.received.index("|/cmd hello") > trn

    run(main())


def test_stale_choice_errors():
    """A choice for no open request (Showdown's and VGC Arena's words)
    is stale, not wrong."""
    from pikapro.showdown.protocol import stale_choice_error

    for m in ("[Invalid choice] There's nothing to choose",
              "[Invalid choice] There is nothing for you to choose right now",
              "[Invalid choice] Sorry, too late to make a different move; the next turn has already started",
              "[Invalid choice] The battle crashed"):
        assert stale_choice_error(m), m
    for m in ("[Invalid choice] Can't move: Eevee doesn't have a move matching tackle",
              "[Unavailable choice] Can't switch: The active Pokémon is trapped", "There's nothing to choose"):
        assert not stale_choice_error(m), m


def test_a_rejoin_whose_request_has_our_choice_sends_none():
    """Showdown's rejoin (``RoomBattle.onConnect``): the room's log and its
    request again, with ``|sentchoice|`` when our choice is in. Nothing is
    sent for that request; the next one is answered."""
    rid = "battle-gen9randombattle-1"

    async def on_login(fake, ws, name):
        await ws.send(room_frame(rid, name, extra="\n|sentchoice|move 1" if fake.logins > 1 else ""))

    async def main():
        async with Fake(on_login=on_login) as fake:
            async with client(fake) as c:
                await until(lambda: fake.chooses())
                fake.sockets[-1].transport.abort()
                await until(lambda: fake.logins == 2 and c.connected)
                await asyncio.sleep(0.3)
                assert len(fake.chooses()) == 1
                await fake.sockets[-1].send(f">{rid}\n|turn|1\n|request|{json.dumps(dict(REQUEST, rqid=2))}")
                await until(lambda: len(fake.chooses()) == 2)
                assert fake.chooses()[-1].endswith("|2")
                await fake.sockets[-1].send(f">{rid}\n|win|bot")
                await until(lambda: c.results)
                return c.results[0]

    r = run(main())
    assert r.won is True and (r.invalid, r.stale, r.fallbacks) == (0, 0, 0)


def test_a_stalled_event_loop_is_logged(caplog, monkeypatch):
    """The loop watch logs a stall (``event loop stalled``), and the
    connection's ``connection lost`` record says how long it lived, how
    long since its last frame, and the loop's longest stall."""
    import pikapro.showdown.client as client_mod

    monkeypatch.setattr(client_mod, "LOOP_TICK", 0.05)
    monkeypatch.setattr(client_mod, "LOOP_STALL_WARN", 0.2)
    caplog.set_level(logging.INFO, logger="pikapro.showdown")

    def events(name):
        return [r for r in caplog.records if getattr(r, "event", None) == name]

    async def main():
        async with Fake() as fake:
            async with client(fake):
                await asyncio.sleep(0.1)
                time.sleep(0.4)  # the loop held
                await until(lambda: events("event loop stalled"))
                fake.sockets[-1].transport.abort()
                await until(lambda: events("connection lost"))

    run(main())
    fields = events("connection lost")[0].fields
    assert fields["loop_stall_max_s"] >= 0.3 and fields["age_s"] >= 0.4 and fields["quiet_s"] is not None


def test_close_ends_a_reconnect_backoff_at_once():
    async def main():
        fake = Fake()
        await fake.__aenter__()
        c = client(fake, max_reconnects=10)
        await c.connect()
        await fake.stop()
        await until(lambda: c._delay >= 4, timeout=20)  # in a 2 s backoff at least
        t0 = time.monotonic()
        await c.close()
        assert time.monotonic() - t0 < 1

    run(main())


# -- the handler contract --------------------------------------------------------------------------


def _raise(e: BaseException):
    raise e


class Returns(BattleHandler):
    def __init__(self, value):
        self.value = value

    def feed(self, room, lines):
        return room.request is not None

    async def choose(self, rooms):
        return self.value(rooms) if callable(self.value) else self.value


@pytest.mark.parametrize(
    "value",
    [None, [], "move 1", ["move 1\n/forfeit"], [42], lambda rooms: 1 / 0, lambda rooms: _raise(NotAnException())],
    ids=["none", "short", "a-string", "newline", "not-a-string", "raises", "raises-base"],
)
def test_a_bad_choose_result_sends_default_and_the_loop_lives(value):
    async def main():
        async with Fake() as fake:
            async with client(fake, handler=Returns(value)) as c:
                task = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
                await until(lambda: len(fake.chooses()) == 1)
                assert fake.chooses()[0].split("|")[1] == "/choose default"
                assert not c._tasks[1].done()  # the decision loop
                room = c.rooms[next(iter(c.rooms))]
                assert room.fallbacks == 1
                # The next request is asked for again.
                c.rooms[room.id].observe("|request|" + json.dumps({**REQUEST, "rqid": 2}), c.user_id)
                c._queue_decision(room)
                await until(lambda: len(fake.chooses()) == 2)
                task.cancel()

    run(main())


def test_choose_none_leaves_the_request_pending():
    async def main():
        async with Fake() as fake:
            handler = Returns([None])
            async with client(fake, handler=handler) as c:
                asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
                await until(lambda: bool(c.rooms))
                await asyncio.sleep(0.1)
                room = c.rooms[next(iter(c.rooms))]
                assert not fake.chooses() and room.pending
                handler.value = ["move 1"]
                c._queue_decision(room)
                await until(lambda: len(fake.chooses()) == 1)
                assert fake.chooses()[0].startswith(f"{room.id}|/choose move 1|")

    run(main())


class Picky(FakeSession):
    """A session that refuses every action in its "bad" rooms."""

    def choice(self, action):
        if self.format_id.endswith("bad"):
            raise ValueError("action is not allowed by the mask")
        return super().choice(action)


def _room(rid: str) -> BattleRoom:
    room = BattleRoom(rid, "me")
    room.observe("|request|" + json.dumps(REQUEST), "me")
    return room


def test_session_handler_contains_each_room():
    """batch=True: one room's refused action, or a room that no longer
    needs a decision, doesn't touch the others' choices."""
    handler = SessionHandler(lambda obs, mask: np.zeros(len(mask), dtype=np.int64), batch=True, session_factory=Picky)
    rooms = [BattleRoom(f"battle-gen9customgame-{i}{'bad' if i == 1 else ''}", "me") for i in range(4)]
    for r in rooms:
        r.format = r.id.split("-", 1)[1]
        r.observe("|request|" + json.dumps(REQUEST), "me")
        handler.open(r)
        handler.feed(r, r.lines)
    rooms[3].state.feed("|win|me")  # ended meanwhile
    assert asyncio.run(handler.choose(rooms)) == ["move 1", "default", "move 1", None]
    # A batched policy returning the wrong number of actions raises: the
    # client then asks room by room.
    wrong = SessionHandler(lambda obs, mask: [0], batch=True, session_factory=FakeSession)
    for r in rooms[:2]:
        r.state = None
        wrong.open(r)
        wrong.feed(r, r.lines)
    with pytest.raises(ValueError, match="1 actions for 2"):
        asyncio.run(wrong.choose(rooms[:2]))


def test_a_stale_decision_is_not_answered():
    """A new request arrives while the policy thinks: the old action is not
    committed to the session, so the new request is decided afresh."""
    rooms = []

    def policy(obs, mask):
        rooms[0].observe("|request|" + json.dumps({**REQUEST, "rqid": 2}), "me")
        return 0

    handler = SessionHandler(policy, session_factory=FakeSession, threaded=False)
    rooms.append(_room("battle-gen9customgame-1"))
    handler.open(rooms[0])
    handler.feed(rooms[0], rooms[0].lines)
    assert asyncio.run(handler.choose(rooms)) == [None]
    assert rooms[0].state.needs_action


def test_channel_session_choice_wants_every_value():
    s = pikapro.rl.ChannelSession("gen9doubles")
    with pytest.raises(ValueError):
        s.choice(0)  # no decision yet, and one value for two slots


# -- untrusted input -------------------------------------------------------------------------------


def test_malformed_server_messages_are_ignored():
    async def on_login(fake, ws, name):
        for line in ("|updatesearch|[]", "|updatesearch|null", '|updatechallenges|{"challengeTo": "x"}',
                     "|updatechallenges|[1]", '|updatechallenges|{"challengesFrom": [1]}',
                     "|updatesearch|" + "[" * 100000 + "]" * 100000):  # fmt: skip
            await ws.send(line)
        await ws.send(fake.new_room("gen9randombattle", name, extra="\n|request|" + "[" * 100000 + "]" * 100000))

    async def main():
        async with Fake(on_login=on_login) as fake:
            async with client(fake) as c:
                await until(lambda: bool(c.rooms))
                await asyncio.sleep(0.1)
                assert not c._run_task.done() and c.searching == [] and c.challenges == {}

    run(main())


def test_room_ids_make_safe_log_names(tmp_path):
    assert _log_name("battle-gen9randombattle-12") == "battle-gen9randombattle-12.log"
    for bad in ("battle-/../../x", "battle-\\..\\..\\x", "battle-a\x00b", "battle-" + "a" * 500):
        name = _log_name(bad)
        assert name.startswith("battle-") and name.endswith(".log") and "/" not in name and "\\" not in name
        assert "\x00" not in name and len(name) < 60

    async def main():
        c = ShowdownClient("bot", log_dir=tmp_path)
        c._outbox = None
        room = BattleRoom("battle-a\x00/../b", "bot")
        room.lines = ["|init|battle"]
        c.rooms[room.id] = room
        c._finish(room, "left")
        return c.results[0]

    result = asyncio.run(main())
    assert result.log_path is not None and tmp_path in __import__("pathlib").Path(result.log_path).parents


def test_commands_are_one_line():
    c = ShowdownClient("bot")
    with pytest.raises(ValueError):
        asyncio.run(c.send("hi\n/forfeit"))
    with pytest.raises(ValueError):
        pack_team("Pikachu||||\r/forfeit|||||||", "gen9customgame")
    with pytest.raises(ValueError):
        ShowdownClient("bot\n/forfeit")

    async def main():
        async with Fake() as fake:
            async with client(fake) as c:
                task = asyncio.ensure_future(c.challenge("Foe\n/forfeit", "gen9randombattle"))
                await until(lambda: any("/challenge" in m for m in fake.received))
                task.cancel()
                return [m for m in fake.received if "/challenge" in m]

    assert run(main()) == ["|/challenge foeforfeit, gen9randombattle"]


def test_a_start_prefers_the_operation_expecting_the_opponent():
    async def main():
        c = ShowdownClient("bot")
        ladder = _Waiter(lambda r: r.format == "gen9randombattle" and not r._claimed, RandomRequestHandler())
        accept = _Waiter(lambda r: r.format == "gen9randombattle" and to_id(r.opponent or "") == "alice", RandomRequestHandler(), "alice")
        c._waiters = [ladder, accept]
        room = BattleRoom("battle-gen9randombattle-1", "bot")
        room.opponent = "Alice"
        c._claim(room)
        assert not accept.future.done(), "a room is claimed once its battle starts"
        room.observe("|player|p1|bot|1|", "bot")
        c._claim(room)
        assert accept.future.done() and not ladder.future.done()

    asyncio.run(main())


def test_a_room_that_ends_at_once_still_gives_its_result():
    async def on_search(fake, ws, name, fmt):
        await ws.send(fake.searching(fmt))
        await ws.send(fake.new_room(fmt, name, extra="\n|win|bot"))

    async def main():
        async with Fake(on_search=on_search) as fake:
            async with client(fake) as c:
                results = await c.ladder(1, "gen9randombattle")
                assert results[0].won is True

    run(main())


def test_an_unrelated_popup_does_not_abort_a_start():
    async def on_search(fake, ws, name, fmt):
        await ws.send("|popup|The server will restart for maintenance soon.")
        await ws.send(fake.searching(fmt))
        await ws.send(fake.new_room(fmt, name, extra="\n|win|bot"))

    async def refuse(fake, ws, name, fmt):
        await ws.send("|popup|Your team was rejected for the following reasons:||- no")

    async def main(script):
        async with Fake(on_search=script) as fake:
            async with client(fake) as c:
                return await c.ladder(1, "gen9randombattle")

    assert run(main(on_search))[0].won is True
    with pytest.raises(ShowdownError, match="rejected"):
        run(main(refuse))


def test_one_to_id():
    """Showdown's toID keeps ASCII letters and digits only."""
    for f in (pikapro.to_id, to_id, view.to_id):
        assert f("Flabébé") == "flabb" and f("[Gen 9] VGC 2025 Reg I") == "gen9vgc2025regi"


def test_session_handler_gives_the_uploaded_team_to_the_session():
    """The team a client uploaded for a battle reaches its ChannelSession
    (the player's own rows come from it)."""
    team = pikapro.RandomTeams("championsvgc").team(1)
    room = BattleRoom("battle-gen9championsvgc2026regmc-1", "me")
    room.team = pack_team(team, "gen9championsvgc2026regmc")
    SessionHandler("random").open(room)
    assert room.state is not None
    room.team = "not a team"
    with pytest.raises(ValueError):
        SessionHandler("random").open(room)


# -- a hostile server, and Rust panics -------------------------------------------------------------


def battle_lines(me: str = "bot", foe: str = "Foe", *, playable: bool = True) -> List[str]:
    """A gen9singles battle room's first lines from ``me``'s channel, as a
    server sends them (pikapro.Battle renders them), ending with the first
    request. Not ``playable``: the request allows nothing (a revival with
    nothing fainted, while the bench is healthy), so the room's
    ChannelSession has a decision with an empty mask, which once made
    ``choose_builtin`` panic. (A move request showing no moves now reads
    as Struggle's ``move 1``, as Showdown's request would show it.)"""
    team = pikapro.sample_team("gen9singles")
    b = pikapro.Battle("gen9singles", [team, team], seed=1)
    names = {"|player|p1|": me, "|player|p2|": foe}
    lines = ["|init|battle", f"|title|{me} vs. {foe}", *b.header()]
    for line in b.log("p1", 0):
        head = line[: len("|player|p1|")]
        lines.append(f"{head}{names[head]}||" if head in names else line)
    request = json.loads(b.request_json(0))
    if not playable:
        del request["active"]
        request["forceSwitch"] = [True]
        request["side"]["pokemon"][0]["reviving"] = True
    return lines + ["|request|" + json.dumps(request)]


def test_a_request_the_session_cant_play_stays_in_its_room():
    """The reviewer's hostile request: the session's mask is empty. Whatever
    the session does about it (once, a Rust panic, a BaseException), the
    room gets a choice and the other room plays on."""
    lines = battle_lines(playable=False)
    s = pikapro.rl.ChannelSession("gen9singles", username="bot")
    s.feed_many(lines)
    # Nothing to switch in: at most a pass is offered (Showdown accepts a
    # pass for a forced switch with no target).
    assert s.needs_action and s.mask.sum() <= 1
    try:
        s.choose_builtin("random", 1)
    except Exception:
        pass  # an Exception (not a BaseException), or a choice

    async def on_login(fake, ws, name):
        await ws.send(">battle-gen9customgame-1\n" + "\n".join(battle_lines(name, playable=False)))
        await ws.send(">battle-gen9customgame-2\n" + "\n".join(battle_lines(name)))

    async def main():
        async with Fake(on_login=on_login) as fake:
            server = pikapro_server(fake.url)
            async with ShowdownClient("bot", server=server, handler=SessionHandler("random")) as c:
                await until(lambda: {m.split("|", 1)[0] for m in fake.chooses()} >= {"battle-gen9customgame-1", "battle-gen9customgame-2"})
                assert not c._run_task.done() and not any(t.done() for t in c._tasks)
                good = [m for m in fake.chooses() if m.startswith("battle-gen9customgame-2|")]
                assert "/choose default" not in good[0]
                assert c.rooms["battle-gen9customgame-2"].fallbacks == 0

    run(main())


class Silent(BattleHandler):
    """Never answers: its battles stay open until the server ends them."""

    def feed(self, room, lines):
        return False

    async def choose(self, rooms):
        return [None] * len(rooms)


def test_a_cancelled_wait_for_battles_leaves_the_battles_running():
    """wait_for_battles() under a timeout: the battles and the ladder()
    playing them go on, and max_concurrent still holds."""

    async def main():
        async with Fake() as fake:
            async with client(fake, handler=Silent(), max_concurrent=1, forfeit_on_exit=False) as c:
                ladder = asyncio.ensure_future(c.ladder(2, "gen9randombattle"))
                await until(lambda: len(c.rooms) == 1)
                with pytest.raises(asyncio.TimeoutError):
                    await asyncio.wait_for(c.wait_for_battles(), 0.2)
                await asyncio.sleep(0.3)
                (room,) = c.rooms.values()
                assert not room._end.cancelled() and not ladder.done()
                assert fake.rooms == 1  # the second battle waits for the first one's slot
                for n in (1, 2):
                    await until(lambda n=n: fake.rooms == n and len(c.rooms) == 1)
                    for ws in fake.sockets:
                        await ws.send(f">{next(iter(c.rooms))}\n|win|bot")
                results = await asyncio.wait_for(ladder, 5)
                assert [r.won for r in results] == [True, True]

    run(main())


def test_connect_gives_up_on_a_server_that_never_logs_in():
    """A server that never sends |challstr|: connect() raises LoginError
    after login_timeout, and the client can connect again later."""

    async def silent(ws):
        async for _ in ws:
            pass

    async def main():
        server = await serve(silent, "127.0.0.1", 0)
        port = server.sockets[0].getsockname()[1]
        try:
            c = ShowdownClient("bot", server=f"ws://127.0.0.1:{port}/showdown/websocket", login_timeout=0.3)
            start = time.monotonic()
            with pytest.raises(LoginError, match="not logged in"):
                await c.connect()
            assert time.monotonic() - start < 3 and c._run_task is None
        finally:
            server.close()
            await server.wait_closed()

    run(main())


def test_a_reconnect_that_never_logs_in_counts_as_failed():
    """After a drop, a connection that isn't logged in within login_timeout
    is closed and retried; a connection that ended for good is connected
    again by connect()."""

    async def main():
        fake = Fake(drop_after=0.2)
        async with fake:
            c = client(fake, login_timeout=0.3, max_reconnects=1)
            await c.connect()
            fake_handler = fake.handler

            async def mute(ws):  # the next connections never get a challstr
                fake.connections += 1
                fake.sockets.append(ws)
                async for _ in ws:
                    pass

            fake.handler = mute
            fake.server.close()
            await fake.server.wait_closed()
            fake.server = await serve(lambda ws: fake.handler(ws), "127.0.0.1", int(fake.url.split(":")[2].split("/")[0]))
            await asyncio.wait_for(c._run_task, 10)  # gave up: 1 retry, not logged in
            assert c._fatal is not None and "gave up" in str(c._fatal)
            fake.handler = fake_handler
            await c.connect()  # starts over
            assert c.connected
            await c.close()

    run(main())


def test_room_ids_with_a_bar_or_control_characters_are_ignored():
    """A server-sent room id goes back into commands (ROOM|/choose ...): one
    with a | (another command) or a control character is not played."""
    bad = ["battle-gen9randombattle-1|/avatar 999", "battle-gen9random\x0bbattle-2", "battle-Gen9-3"]

    async def on_login(fake, ws, name):
        for rid in bad:
            await ws.send(room_frame(rid, name))
        await ws.send(room_frame("battle-gen9randombattle-4", name))

    async def main():
        async with Fake(on_login=on_login) as fake:
            async with client(fake, timer=True) as c:
                await until(lambda: len(fake.chooses()) == 1)
                await asyncio.sleep(0.1)
                assert list(c.rooms) == ["battle-gen9randombattle-4"]
                assert not any("avatar" in m or "\x0b" in m for m in fake.received)

    run(main())


def test_updatechallenges_queues_each_challenge_once():
    """Older servers resend every standing challenge on each change: only
    a new one is queued for accept_challenges."""

    async def on_login(fake, ws, name):
        for _ in range(20):
            await ws.send("|updatechallenges|" + json.dumps({"challengesFrom": {"alice": "gen9randombattle"}, "challengeTo": None}))
        await ws.send("|updatechallenges|" + json.dumps({"challengesFrom": {"alice": "gen9randombattle", "bob": "gen9randombattle"}}))

    async def main():
        async with Fake(on_login=on_login) as fake:
            async with client(fake) as c:
                await until(lambda: len(c.challenges) == 2)
                await asyncio.sleep(0.1)
                assert c._incoming.qsize() == 2

    run(main())


def test_the_uploaded_team_reaches_the_rooms_session():
    """The team a ladder() uploads is its room's (BattleRoom.team), and
    stays the room's after a rejoin."""
    team = pikapro.sample_team("gen9singles")

    async def main():
        async with Fake() as fake:
            async with client(fake, handler=Silent()) as c:
                task = asyncio.ensure_future(c.ladder(1, "gen9randombattle", team=team))
                await until(lambda: bool(c.rooms))
                room = next(iter(c.rooms.values()))
                assert room.team == pack_team(team, "gen9randombattle")
                for ws in fake.sockets:  # joined again: the log starts over
                    await ws.send(room_frame(room.id, "bot"))
                await asyncio.sleep(0.1)
                assert c.rooms[room.id] is not room and c.rooms[room.id].team == room.team
                task.cancel()

    run(main())


# -- battle privacy ---------------------------------------------------------------------------------


def test_hide_battles_sends_hidenext_before_every_search():
    """``hide_battles``: ``/hidenext`` right before each ``/search`` (the
    server copies it into that one search, then resets it); off by default."""

    async def main():
        for hide in (True, False):
            fake = Fake(rooms_on_search=False)
            await fake.__aenter__()
            c = client(fake, hide_battles=hide)
            await c.connect()
            task = asyncio.ensure_future(c.ladder(1, "gen9randombattle"))
            await until(lambda: "|/search gen9randombattle" in fake.received)
            got = [m for m in fake.received if m.startswith("|/")]
            i = got.index("|/search gen9randombattle")
            if hide:
                assert got[i - 2] == "|/hidenext" and got[i - 1].startswith("|/utm "), got
            else:
                assert "|/hidenext" not in got, got
            task.cancel()
            with contextlib.suppress(BaseException):
                await task
            await c.close()
            await fake.stop()

    run(main())
