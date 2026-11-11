"""Scripted protocol probe: the same sessions against any Showdown-protocol server.

Drives raw websocket clients (no poke-env) through the commands poke-env
sends and the error cases around them, and prints a transcript: each step's
outgoing message and the frames every client received until the server went
quiet. Running it against Showdown's server and against pikapro-showdown-server
and diffing the (normalised) transcripts shows where the protocols differ:

    python probe.py --server ws://localhost:8000/showdown/websocket > showdown.txt
    python probe.py --server ws://localhost:8001/showdown/websocket > ours.txt
    python normalize.py showdown.txt ours.txt
"""

from __future__ import annotations

import argparse
import asyncio
import json
import pathlib
import re

import websockets

TEAMS = pathlib.Path(__file__).parent / "teams"

# One Pokémon with one move, so custom-game battles are short.
SOLO = "Garchomp||choicescarf|roughskin|earthquake|Jolly|,252,,,,252||||50|"
SOLO2 = "Incineroar||sitrusberry|intimidate|flareblitz|Careful|252,,,,252,||||50|"


def packed(name: str) -> str:
    """A team file (export format) packed the way poke-env packs it."""
    from poke_env.teambuilder.constant_teambuilder import ConstantTeambuilder

    return ConstantTeambuilder((TEAMS / name).read_text()).yield_team()


class Client:
    def __init__(self, name: str, ws, log: list):
        self.name = name
        self.ws = ws
        self.log = log
        self.inbox: asyncio.Queue[str] = asyncio.Queue()
        self.rqid = None
        self.team_preview = False
        self.task = asyncio.create_task(self._read())

    async def _read(self):
        try:
            async for message in self.ws:
                await self.inbox.put(str(message))
        except websockets.ConnectionClosed:
            pass

    async def send(self, message: str):
        self.log.append(f">>> [{self.name}] {message[:160]}")
        await self.ws.send(message)

    async def wait_for(self, pattern: str, timeout: float = 10.0) -> str:
        """Reads (and logs) frames until one matches `pattern`."""
        while True:
            frame = await asyncio.wait_for(self.inbox.get(), timeout)
            self.record(frame)
            if re.search(pattern, frame):
                return frame

    def record(self, frame: str):
        for rqid in re.findall(r'"rqid":(\d+)', frame):
            self.rqid = rqid
        if "|request|" in frame:
            self.team_preview = '"teamPreview":true' in frame
        for line in frame.split("\n"):
            self.log.append(f"<<< [{self.name}] {line[:1000]}")

    async def drain(self, quiet: float):
        while True:
            try:
                frame = await asyncio.wait_for(self.inbox.get(), quiet)
            except asyncio.TimeoutError:
                return
            self.record(frame)


async def connect_raw(url: str, name: str, log: list) -> Client:
    ws = await websockets.connect(url, max_queue=None, ping_interval=None)
    client = Client(name, ws, log)
    await client.wait_for(r"\|challstr\|")
    return client


async def connect(url: str, name: str, log: list) -> Client:
    client = await connect_raw(url, name, log)
    await client.send(f"|/trn {name},0,")
    await client.wait_for(r"\|updateuser\| " + re.escape(name))
    return client


async def settle(clients, quiet: float):
    await asyncio.gather(*(c.drain(quiet) for c in clients))


async def run(url: str, quiet: float, prefix: str) -> list[str]:
    log: list[str] = []

    def step(title: str):
        log.append(f"### {title}")

    step("connect and log in")
    a = await connect(url, f"{prefix} a", log)
    b = await connect(url, f"{prefix} b", log)
    ida, idb = f"{prefix}a", f"{prefix}b"
    await settle([a, b], quiet)

    step("nametaken")
    c = await connect_raw(url, f"{prefix} c", log)
    await c.send(f"|/trn {prefix} a,0,")
    await settle([c], quiet)
    await c.send("|/trn 12345,0,")
    await settle([c], quiet)
    await c.ws.close()

    step("search: unknown format")
    await a.send("|/search gen9notaformat")
    await settle([a], quiet)

    step("search: no team")
    await a.send("|/utm null")
    await a.send("|/search gen9championsvgc2026regmc")
    await settle([a], quiet)

    step("search: illegal team")
    await a.send(f"|/utm {SOLO}")
    await a.send("|/search gen9championsvgc2026regmc")
    await settle([a], quiet)

    step("search: custom game")
    await a.send("|/search gen9customgame")
    await settle([a], quiet)

    step("search then cancelsearch")
    await a.send(f"|/utm {packed('gen9championsvgc2026regmc.txt')}")
    await a.send("|/search gen9championsvgc2026regmc")
    await settle([a], quiet)
    await a.send("|/cancelsearch")
    await settle([a], quiet)

    step("challenge: unknown user")
    await a.send("|/challenge nobodyhere, gen9customgame")
    await settle([a], quiet)

    step("accept: no challenge")
    await a.send(f"|/accept {idb}")
    await settle([a], quiet)

    step("unknown command")
    await a.send("|/notacommand")
    await settle([a], quiet)

    step("choose outside a battle")
    await a.send("|/choose move 1")
    await settle([a], quiet)
    await a.send("battle-gen9customgame-999999|/choose move 1")
    await settle([a], quiet)

    step("challenge, cancelchallenge")
    await a.send(f"|/utm {SOLO}")
    await a.send(f"|/challenge {idb}, gen9customgame")
    await settle([a, b], quiet)
    await a.send(f"|/cancelchallenge {idb}")
    await settle([a, b], quiet)

    step("challenge, reject")
    await a.send(f"|/challenge {idb}, gen9customgame")
    await settle([a, b], quiet)
    await b.send(f"|/reject {ida}")
    await settle([a, b], quiet)

    step("challenge, accept (custom game)")
    await a.send(f"|/challenge {idb}, gen9customgame")
    await settle([a, b], quiet)
    await b.send(f"|/utm {SOLO2}")
    await b.send(f"|/accept {ida}")
    frame = await a.wait_for(r"\|request\|")
    room = frame.split("\n")[0][1:]
    await settle([a, b], quiet)

    if a.team_preview:
        step("team preview (Showdown's custom games have it)")
        await a.send(f"{room}|/team 1")
        await b.send(f"{room}|/team 1")
        await settle([a, b], quiet)

    step("timer on, off, on")
    await a.send(f"{room}|/timer on")
    await settle([a, b], quiet)
    await a.send(f"{room}|/timer off")
    await settle([a, b], quiet)
    await b.send(f"{room}|/timer")
    await settle([a, b], quiet)

    step("invalid choices")
    await a.send(f"{room}|/choose move 9")
    await settle([a, b], quiet)
    await a.send(f"{room}|/choose switch 2")
    await settle([a, b], quiet)
    await a.send(f"{room}|/choose foo")
    await settle([a, b], quiet)

    step("choose, undo, choose")
    await a.send(f"{room}|/choose move 1")
    await settle([a, b], quiet)
    await a.send(f"{room}|/undo")
    await settle([a, b], quiet)
    await a.send(f"{room}|/undo")
    await settle([a, b], quiet)
    await a.send(f"{room}|/choose move earthquake")
    await settle([a, b], quiet)
    await a.send(f"{room}|/choose move 1")
    await settle([a, b], quiet)

    step("stale rqid")
    await b.send(f"{room}|/choose move 1|1")
    await settle([a, b], quiet)

    step("choose with rqid (resolves the turn)")
    await b.send(f"{room}|/choose move 1|{b.rqid}")
    await settle([a, b], quiet)

    step("chat in the battle room")
    await b.send(f"{room}|hello there")
    await settle([a, b], quiet)

    step("forfeit")
    await a.send(f"{room}|/forfeit")
    await settle([a, b], quiet)

    step("choose after the end")
    await b.send(f"{room}|/choose move 1")
    await settle([a, b], quiet)

    step("leave")
    await a.send(f"|/leave {room}")
    await settle([a, b], quiet)
    await b.send(f"{room}|/leave")
    await settle([a, b], quiet)

    step("ladder (gen9vgc2025regi)")
    team = packed("gen9vgc2025regi.txt")
    await a.send(f"|/utm {team}")
    await a.send("|/search gen9vgc2025regi")
    await b.send(f"|/utm {team}")
    await b.send("|/search gen9vgc2025regi")
    await settle([a, b], quiet)

    step("ladder (championsvgc), OTS both accept")
    team = packed("gen9championsvgc2026regmc.txt")
    await a.send(f"|/utm {team}")
    await a.send("|/search gen9championsvgc2026regmc")
    await b.send(f"|/utm {team}")
    await b.send("|/search gen9championsvgc2026regmc")
    frame = await a.wait_for(r"\|request\|")
    room = frame.split("\n")[0][1:]
    await settle([a, b], quiet)
    await a.send(f"{room}|/acceptopenteamsheets")
    await settle([a, b], quiet)
    await a.send(f"{room}|/acceptopenteamsheets")
    await settle([a, b], quiet)
    await b.send(f"{room}|/acceptopenteamsheets")
    await settle([a, b], quiet)
    await a.send(f"{room}|/team 1234")
    await b.send(f"{room}|/team 2143")
    await settle([a, b], quiet)

    step("disconnect mid-battle")
    await b.ws.close()
    await settle([a], quiet * 3)

    await a.ws.close()
    return log


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", default="ws://localhost:8000/showdown/websocket")
    parser.add_argument("--quiet", type=float, default=0.4, help="seconds of silence that end a step")
    parser.add_argument("--prefix", default="probe")
    args = parser.parse_args()
    log = asyncio.run(run(args.server, args.quiet, args.prefix))
    print("\n".join(log))


if __name__ == "__main__":
    main()
