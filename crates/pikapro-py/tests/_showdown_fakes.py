"""What pikapro.showdown's tests share: a scripted Showdown server
(:class:`Fake`), a stand-in for ``ChannelSession`` (:class:`FakeSession`),
a request, and the asyncio helpers (:func:`run`, :func:`until`).

A module of its own, not a test module, so that a test imports it in any
of pytest's import modes (``conftest.py`` puts this folder on the path).
"""

from __future__ import annotations

import asyncio
import json
import random
import time
from typing import Any, Callable, List, Optional

import numpy as np

from pikapro.showdown import random_choice


class FakeSession:
    """A stand-in for pikapro.rl.ChannelSession: its interface, with the
    choices drawn from the request JSON (four candidates, the mask all
    true)."""

    def __init__(self, format_id, *, observation="basic", masks="factored", switch_by="row", username=None):
        self.format_id = format_id
        self.username = username
        self.options = (observation, masks, switch_by)
        self.lines = []
        self.request = None
        self.request_kind = "wait"
        self.needs_action = False
        self.finished = False
        self.winner = None
        self.unknown_lines = []
        self.series_row = None
        self.settled = None
        self.candidates = []
        self.rng = random.Random(len(format_id))

    def feed(self, line):
        self.lines.append(line)
        if line.startswith("|request|"):
            payload = line[len("|request|") :]
            if not payload:
                return
            self.request = json.loads(payload)
            if self.request.get("wait"):
                self.request_kind, self.needs_action = "wait", False
                return
            self.request_kind = "team_preview" if self.request.get("teamPreview") else "move"
            self.candidates = [random_choice(self.request, self.rng) for _ in range(4)]
            self.needs_action = True
        elif line.startswith("|error|[Invalid choice]") and self.request is not None:
            self.needs_action = True
        elif line.startswith("|win|"):
            self.finished, self.winner, self.needs_action = True, line[5:], False
        elif line == "|tie" or line.startswith("|tie|"):
            self.finished, self.needs_action = True, False
        elif line.startswith("|debug|"):
            self.unknown_lines.append(line)

    def feed_many(self, lines):
        for line in lines:
            self.feed(line)

    @property
    def obs(self):
        return {"n": np.array([len(self.lines)], dtype=np.int64), "x": np.zeros((2, 3), dtype=np.float32)}

    @property
    def mask(self):
        return np.ones(4, dtype=bool)

    @property
    def view(self):
        return None

    def choice(self, action):
        self.needs_action = False
        return self.candidates[int(action)]

    def settle_series(self, winner=None, tie=False):
        self.settled = (winner, tie)

    def choose_builtin(self, name, seed):
        self.needs_action = False
        return random_choice(self.request, random.Random(seed))


REQUEST = {
    "active": [{"moves": [{"move": "Tackle", "id": "tackle", "pp": 35, "maxpp": 35, "target": "normal"}]}],
    "side": {"name": "X", "id": "p1", "pokemon": [{"ident": "p1: Eevee", "details": "Eevee, L50", "condition": "100/100", "active": True}]},
    "rqid": 1,
}


def room_frame(rid: str, me: str, foe: str = "Foe", request: Optional[dict] = None, extra: str = "") -> str:
    request = REQUEST if request is None else request
    return (
        f">{rid}\n|init|battle\n|title|{me} vs. {foe}\n|player|p1|{me}|1|\n|player|p2|{foe}|1|\n"
        f"|request|{json.dumps(request)}{extra}"
    )


class Fake:
    """A scripted Showdown server on a free port.

    ``on_login(fake, ws, name)`` scripts what follows a login, and
    ``on_search(fake, ws, name, fmt)`` the answer to a search (by default:
    the search confirmed, then a battle room in its format). ``drop_after`` aborts each
    connection that long after its login (a network drop). ``/forfeit``
    ends the room with the foe winning."""

    def __init__(self, *, on_login=None, on_search=None, drop_after: Optional[float] = None, rooms_on_search: bool = True):
        self.on_login = on_login
        self.on_search = on_search
        self.drop_after = drop_after
        self.rooms_on_search = rooms_on_search
        self.connections = 0
        self.logins = 0
        self.received: List[str] = []
        self.rooms = 0
        self.server = None
        self.url = ""
        self.sockets: List[Any] = []

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
                self.logins += 1
                await ws.send(f"|updateuser| {name}|1|1|{{}}")
                if self.drop_after is not None:
                    asyncio.get_running_loop().call_later(self.drop_after, ws.transport.abort)
                if self.on_login is not None:
                    await self.on_login(self, ws, name)
            elif text.startswith("/search "):
                fmt = text[len("/search ") :]
                if self.on_search is not None:
                    await self.on_search(self, ws, name, fmt)
                    continue
                await ws.send(self.searching(fmt))
                if self.rooms_on_search:
                    await ws.send(self.new_room(fmt, name))
            elif text.startswith("/accept "):
                frm = text[len("/accept ") :]
                fmt = self.challenge_formats.get(frm, "gen9randombattle")
                await ws.send(f"|pm|{frm}|{name}|/challenge")
                await ws.send(self.new_room(fmt, name, frm))
            elif text.startswith("/forfeit") and room:
                await ws.send(f">{room}\n|win|Foe")

    challenge_formats: dict = {}

    @staticmethod
    def searching(fmt: str) -> str:
        return f"|updatesearch|{json.dumps({'searching': [fmt]})}"

    def new_room(self, fmt: str, me: str, foe: str = "Foe", extra: str = "") -> str:
        self.rooms += 1
        return room_frame(f"battle-{fmt}-{self.rooms}", me, foe, extra=extra)

    def chooses(self) -> List[str]:
        return [m for m in self.received if "|/choose " in m]

    async def __aenter__(self) -> "Fake":
        from websockets.asyncio.server import serve

        self.server = await serve(self.handler, "127.0.0.1", 0)
        port = self.server.sockets[0].getsockname()[1]
        self.url = f"ws://127.0.0.1:{port}/showdown/websocket"
        return self

    async def __aexit__(self, *exc) -> None:
        await self.stop()

    async def stop(self) -> None:
        if self.server is not None:
            self.server.close()
            for ws in self.sockets:
                ws.transport.abort()
            await self.server.wait_closed()
            self.server = None


async def drop(ws) -> None:
    """Drops a connection after what was sent to it has gone out: Windows'
    event loop writes in the background, and ``abort()`` discards what it
    hasn't written."""
    for _ in range(200):
        if ws.transport.get_write_buffer_size() == 0:
            break
        await asyncio.sleep(0.005)
    ws.transport.abort()


def run(coro, timeout: float = 30.0):
    async def runner():
        # Named so leak checks can tell it apart: before Python 3.12,
        # `wait_for` runs `coro` in a task of its own, and this one waits on it.
        asyncio.current_task().set_name(RUNNER)
        return await asyncio.wait_for(coro, timeout)

    return asyncio.run(runner())


RUNNER = "test-runner"

#: How early a timed event may look: asyncio runs a timer up to one clock
#: tick early, and the tests time with another clock (``time.monotonic``),
#: so a bound can be missed by two ticks (15.6 ms each on Windows, a
#: nanosecond elsewhere). Events within a tick get the same time.
EARLY = 2 * time.get_clock_info("monotonic").resolution


async def until(condition: Callable[[], bool], timeout: float = 5.0) -> None:
    start = time.monotonic()
    while not condition():
        if time.monotonic() - start > timeout:
            raise AssertionError("timed out")
        await asyncio.sleep(0.01)
