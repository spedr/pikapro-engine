"""pikapro.showdown's client for a bot that subclasses it: the hooks
(``on_battle_lines``, ``on_pm``, ``hold_decision`` / ``release_decision``,
``send_room``), ``leave_delay``, the websockets log scrubbed for every login,
the arena's format suffixes and a rotation's bad team. Against the scripted
server of ``_showdown_fakes``; no network beyond localhost."""

from __future__ import annotations

import asyncio
import gc
import logging
import random
import time
from dataclasses import replace
from typing import List, Tuple

import pytest

pytest.importorskip("websockets")

from pikapro.showdown import ARENA, LOCAL, RandomRequestHandler, ServerConfig, ShowdownClient  # noqa: E402
from pikapro.showdown.client import _Scrub  # noqa: E402

from _showdown_fakes import Fake, room_frame, run, until  # noqa: E402

RID = "battle-gen9randombattle-7"
RATING = "|raw|bot's rating: 1000 &rarr; <strong>1016</strong><br />(+16 for winning)"


class Hooked(ShowdownClient):
    """Records what its hooks see; ``hold`` holds each room's first decision."""

    def __init__(self, *args, hold: bool = False, **kwargs):
        super().__init__(*args, **kwargs)
        self.frames: List[Tuple[str, List[str], bool]] = []
        self.pms: List[str] = []
        self.hold = hold
        self.held: List[str] = []

    def on_battle_lines(self, room_id, lines):
        self.frames.append((room_id, list(lines), room_id in self.rooms))

    def on_pm(self, line):
        self.pms.append(line)

    def hold_decision(self, room):
        if self.hold and room.id not in self.held:
            self.held.append(room.id)
            return True
        return False


def battle_then_rating(rid: str = RID):
    """A login script: a battle room with a request, then a PM, then (once
    the bot chose) the win and, after it, the rating line."""

    async def on_login(fake, ws, name):
        await ws.send(room_frame(rid, name))
        await ws.send(f"|pm| Alice| {name}|hello there")

    return on_login


def test_the_hooks_see_frames_pms_and_the_lines_after_the_end():
    """``on_battle_lines`` gets every frame, after the client read it (the
    room is live while it lasts) and the rating line after the end, which
    ``leave_delay`` keeps the client in the room for; ``on_pm`` gets the
    PM. ``/leave`` goes ``leave_delay`` seconds after the end."""

    async def main():
        async with Fake(on_login=battle_then_rating()) as fake:
            c = Hooked("bot", server=fake.url, handler=RandomRequestHandler(0), leave_delay=0.4)
            async with c:
                await until(lambda: fake.chooses())
                ws = fake.sockets[-1]
                await ws.send(f">{RID}\n|win|bot")
                await until(lambda: c.results)
                t_end = time.monotonic()
                await ws.send(f">{RID}\n{RATING}")
                await until(lambda: any(RATING in lines for _, lines, _ in c.frames))
                assert not [m for m in fake.received if "/leave" in m]
                await until(lambda: [m for m in fake.received if m == f"|/leave {RID}"], timeout=3)
                waited = time.monotonic() - t_end
            return c, waited

    c, waited = run(main())
    assert waited >= 0.3
    first = c.frames[0]
    assert first[0] == RID and first[1][0] == "|init|battle" and first[2], "read, the room live"
    assert (RID, [RATING], False) in c.frames, "after the end: the room gone, the lines still here"
    assert [p for p in c.pms if "hello there" in p]


def test_no_leave_delay_leaves_at_once():
    async def main():
        async with Fake(on_login=battle_then_rating()) as fake:
            async with Hooked("bot", server=fake.url, handler=RandomRequestHandler(0)) as c:
                await until(lambda: fake.chooses())
                await fake.sockets[-1].send(f">{RID}\n|win|bot")
                await until(lambda: [m for m in fake.received if m == f"|/leave {RID}"], timeout=1)
                assert c.leave_delay == 0.0

    run(main())
    with pytest.raises(ValueError, match="leave_delay"):
        ShowdownClient("bot", leave_delay=-1)


def test_a_held_decision_waits_for_its_release():
    """``hold_decision`` holds the room's decision: nothing is sent until
    ``release_decision``, which asks ``hold_decision`` again first."""

    async def main():
        async with Fake(on_login=battle_then_rating()) as fake:
            async with Hooked("bot", server=fake.url, handler=RandomRequestHandler(0), hold=True) as c:
                await until(lambda: c.held == [RID])
                await asyncio.sleep(0.3)
                assert not fake.chooses()
                c.release_decision("battle-unknown-1")  # no such room: nothing
                c.release_decision(RID)
                await until(lambda: fake.chooses())
                assert c.held == [RID]

    run(main())


class Broken(ShowdownClient):
    def on_battle_lines(self, room_id, lines):
        raise RuntimeError("lines")

    def on_pm(self, line):
        raise RuntimeError("pm")

    def hold_decision(self, room):
        raise RuntimeError("hold")


def test_a_hook_that_raises_is_logged_and_the_battle_goes_on(caplog):
    """A hook's exception is logged (``hook error``) and contained: a
    raising ``hold_decision`` counts as not holding."""
    caplog.set_level(logging.ERROR, logger="pikapro.showdown")

    async def main():
        async with Fake(on_login=battle_then_rating()) as fake:
            async with Broken("bot", server=fake.url, handler=RandomRequestHandler(0)) as c:
                await until(lambda: fake.chooses())
                await fake.sockets[-1].send(f">{RID}\n|win|bot")
                await until(lambda: c.results)
                return c

    c = run(main())
    assert c.results[0].won is True
    hooks = {r.fields["hook"] for r in caplog.records if getattr(r, "event", None) == "hook error"}
    assert hooks == {"on_battle_lines", "on_pm", "hold_decision"}


def test_send_room_posts_while_connected():
    async def main():
        async with Fake() as fake:
            c = ShowdownClient("bot", server=fake.url)
            async with c:
                assert c.send_room(RID, "gl hf") is True
                with pytest.raises(ValueError):
                    c.send_room(RID, "gl\n/forfeit")
                with pytest.raises(ValueError):
                    c.send_room("a|b", "gl hf")
                await until(lambda: f"{RID}|gl hf" in fake.received)
            assert c.send_room(RID, "after close") is False
            return fake

    fake = run(main())
    assert not [m for m in fake.received if "after close" in m]


# -- the websockets frame log ---------------------------------------------------------------


def test_the_scrub_filter_redacts_assertions_and_the_secret():
    def scrubbed(f: _Scrub, msg: str, *args) -> str:
        record = logging.LogRecord("websockets.client", logging.DEBUG, __file__, 1, msg, args, None)
        assert f.filter(record)
        return record.getMessage()

    f = _Scrub(None)
    assert scrubbed(f, "> TEXT '|/trn Bot,0,' [11 bytes]") == "> TEXT '|/trn Bot,0,' [11 bytes]"  # no token
    assert scrubbed(f, "< TEXT '|updateuser| Bot|1|1|{}'") == "< TEXT '|updateuser| Bot|1|1|{}'"
    tok = _Scrub("pka_token")
    assert scrubbed(tok, "> TEXT '|/trn Bot,0,pka_token'") == "> TEXT '|/trn Bot,0,<token>'"
    assert scrubbed(tok, "echo pka_token here") == "echo <token> here"
    assert scrubbed(tok, "> TEXT '|/trn Bot,0,' [11 bytes]") == "> TEXT '|/trn Bot,0,' [11 bytes]"


def _pieces(token: str, text: str, n: int = 8) -> list:
    """The pieces of ``token`` ``n`` long that ``text`` shows."""
    return [token[i : i + n] for i in range(len(token) - n + 1) if token[i : i + n] in text]


@pytest.mark.parametrize("length", [40, 61, 64, 120, 300, 700])
def test_the_scrub_filter_redacts_a_token_websockets_elides(length):
    """websockets logs a long frame as its head and tail
    (``Frame.__str__``): no piece of the token is left in either."""
    from websockets.frames import Frame, Opcode

    alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789-_"
    token = "".join(random.Random(length).choices(alphabet, k=length))
    frame = Frame(Opcode.TEXT, f"|/trn bot,0,{token}".encode())
    shown = str(frame)
    if length >= 64:
        assert token not in shown and _pieces(token, shown), "websockets elides it: the case under test"
    f = _Scrub(token)
    record = logging.LogRecord("websockets.client", logging.DEBUG, __file__, 1, "> %s", (frame,), None)
    assert f.filter(record)
    text = record.getMessage()
    assert not _pieces(token, text), text
    assert text.startswith("> TEXT '|/trn bot,0,<token>") and text.endswith(f"' [{12 + length} bytes]")
    # A server line that echoes the token, cut by a logger at either end.
    for cut in (f"|popup|you sent {token[: length // 2]}", f"{token[length // 2 :]} is not a token"):
        record = logging.LogRecord("websockets.client", logging.DEBUG, __file__, 1, "< %s", (cut,), None)
        assert f.filter(record)
        assert not _pieces(token, record.getMessage()), record.getMessage()
    # A shorter cut beside a longer one, at either end: both go.
    if length >= 40:
        text = f"a={token[:30]}... b={token[:12]}... c={token[-12:]} d={token[-30:]} e={token[:9]}"
        record = logging.LogRecord("websockets.client", logging.DEBUG, __file__, 1, "%s", (text,), None)
        assert f.filter(record)
        assert record.getMessage() == "a=<token>... b=<token>... c=<token> d=<token> e=<token>"
    # The client's own records too (``_event`` scrubs every field).
    c = ShowdownClient("bot", token, server=ServerConfig(websocket_url="ws://localhost:1/", token_login=True))
    assert not _pieces(token, c._scrub(f"head={token[:50]!r}")) and c._scrub("nothing here") == "nothing here"


@pytest.mark.parametrize("length", [27, 400])
def test_a_token_never_reaches_the_websockets_log(caplog, length):
    """The ``/trn`` websockets logs at DEBUG has its token scrubbed, also
    when the frame is long enough for websockets to log only its head and
    tail; the filter goes with the client."""
    token = ("S3CRETTOKEN0123456789abcdef" + "".join(f"{i:x}-{i * 7919:x}_" for i in range(60)))[:length]
    caplog.set_level(logging.DEBUG, logger="websockets")
    caplog.set_level(logging.DEBUG, logger="pikapro.showdown")

    async def main():
        async with Fake() as fake:
            config = ServerConfig(websocket_url=fake.url, token_login=True)
            async with ShowdownClient("bot", token, server=config) as c:
                assert c.connected
                assert logging.getLogger("websockets.client").filters
            return fake

    fake = run(main())
    assert [m for m in fake.received if m == f"|/trn bot,0,{token}"]
    text = "\n".join(r.getMessage() for r in caplog.records if not r.name.startswith("websockets.server"))
    assert token not in text and not _pieces(token, text)
    frames = [r.getMessage() for r in caplog.records if r.name == "websockets.client"]
    assert [t for t in frames if "/trn bot,0,<token>" in t], "websockets' own record of the frame"
    assert not logging.getLogger("websockets.client").filters


def test_every_t_line_reaches_the_handler_in_order():
    """Every raw ``|t:|`` line is in ``room.lines``, in arrival order, for
    a handler's choice to read."""
    stamps = [f"|t:|{1_700_000_000 + k}" for k in range(5)]
    seen: List[str] = []

    class Reads(RandomRequestHandler):
        def feed(self, room, lines):
            seen[:] = [x for x in room.lines if x.startswith("|t:|")]
            return super().feed(room, lines)

    async def on_login(fake, ws, name):
        await ws.send(room_frame(RID, name, extra=f"\n{stamps[0]}\n{stamps[1]}"))
        await ws.send(f">{RID}\n{stamps[2]}\n|turn|1\n{stamps[3]}")
        await ws.send(f">{RID}\n{stamps[4]}")

    async def main():
        async with Fake(on_login=on_login) as fake:
            async with ShowdownClient("bot", server=fake.url, handler=Reads(0)):
                await until(lambda: len(seen) == len(stamps))

    run(main())
    assert seen == stamps


def test_a_close_from_a_callback_is_kept_and_contained():
    """A close started from a callback is referenced until it
    ends, and its failure is logged, not left unretrieved."""

    class BadSocket:
        async def close(self):
            raise OSError("already broken")

    async def main():
        c = ShowdownClient("bot")
        loop = asyncio.get_running_loop()
        unretrieved = []
        loop.set_exception_handler(lambda _loop, ctx: unretrieved.append(ctx))
        c._close_later(BadSocket())
        assert len(c._closers) == 1
        gc.collect()
        await until(lambda: not c._closers)
        return unretrieved

    assert run(main()) == []


# -- formats and teams ------------------------------------------------------------------------


def test_the_arena_plays_a_ladders_suffix_as_its_format():
    """``FORMAT`` + ``random``, ``ots``, ``otsrandom`` play as
    ``FORMAT``, a later regulation's too (when the engine reads it); an
    alias still wins, and a format the engine reads whole is kept."""
    fmt = "gen9championsvgc2026regmc"
    for x in ("", "random", "ots", "otsrandom"):
        assert ARENA.play_format(fmt + x) == fmt
    assert ARENA.play_format("gen9championsvgc2026regmbots") == "gen9championsvgc2026regmb"
    assert ARENA.play_format("gen9championsvgc2026regmbotsrandom") == "gen9championsvgc2026regmb"
    assert ARENA.play_format("gen9nosuchformatots") == "gen9nosuchformatots"
    assert LOCAL.play_format(fmt + "ots") == fmt + "ots"
    assert replace(ARENA, format_aliases=((fmt + "ots", "x"),)).play_format(fmt + "ots") == "x"


def test_a_rotations_bad_team_makes_way_for_the_next(caplog):
    """On a server that starts battles itself, a rotation whose team fails
    (raises, even a ``BaseException`` as a Rust panic is) moves on to its
    next; five failures in a row keep the team uploaded before, and with
    none uploaded yet raise."""
    caplog.set_level(logging.ERROR, logger="pikapro.showdown")

    class Panic(BaseException):
        pass

    good = "Pikachu||leftovers|static|thunderbolt,protect|Jolly|,,,,,|||||50|"
    calls = []

    def rotation(fmt):
        calls.append(fmt)
        if len(calls) == 1:
            raise Panic("pack panicked")
        return good

    config = replace(ARENA, websocket_url="ws://127.0.0.1:9/showdown/websocket")
    c = ShowdownClient("bot", "pka_token", server=config)
    assert c._auto_pack(rotation, "gen9championsvgc2026regmc") == good
    assert len(calls) == 2
    assert [r for r in caplog.records if getattr(r, "event", None) == "team failed"]

    def broken(fmt):
        raise Panic("always")

    with pytest.raises(Panic):
        c._auto_pack(broken, "gen9championsvgc2026regmc")  # nothing uploaded yet
    c._utm = good
    assert c._auto_pack(broken, "gen9championsvgc2026regmc") is None  # the last upload stays
    with pytest.raises(ValueError):
        c._auto_pack("not a team\nat all", "gen9championsvgc2026regmc")  # a fixed team: raised
