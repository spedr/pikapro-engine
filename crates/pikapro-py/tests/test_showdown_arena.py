"""pikapro.showdown's client on VGC Arena (``ARENA``): a scripted arena
server that logs in with a token as the ``/trn`` assertion, takes only its
documented commands, and puts the bot in battles without being asked.

The fake (``FakeArena``) follows VGC Arena's documented rules: the token
in place of the assertion, ``/utm`` for the team, challenges that aren't
answered, battles that start on their own (the random ladder's two in a
row), ``|popup|`` for a rejected team, the pair limit and an unsupported
command, and a battle sent again after a reconnect; on demand, the team
preview's ``|poke|`` lines and battles held until a test ends them. No
network beyond localhost.
"""

from __future__ import annotations

import asyncio
import json
import logging
from dataclasses import replace
from typing import Any, List, Optional

import pytest

pytest.importorskip("websockets")

from websockets.asyncio.server import serve  # noqa: E402

from pikapro.showdown import ARENA, AcceptPlan, RandomRequestHandler, ShowdownClient, ShowdownError, LoginError, server_config  # noqa: E402
from pikapro.showdown.client import _team_shows  # noqa: E402

from _showdown_fakes import drop, run, until  # noqa: E402

TOKEN = "pka_s3cr3t-T0KEN-value"
FMT = "gen9championsvgc2026regmc"
COMMANDS = {"trn", "utm", "challenge", "cancelchallenge", "accept", "reject", "choose", "forfeit", "cancelsearch"}


def team(*mons: str) -> str:
    """A packed team of species with one move each (``Pikachu:thunderbolt``)."""
    out = []
    for m in mons:
        species, _, move = m.partition(":")
        out.append(f"{species}||leftovers|static|{move or 'protect'},protect|Jolly|,,,,,|||||50|")
    return "]".join(out)


TEAM_A = team("Pikachu:thunderbolt", "Eevee:tackle")
TEAM_B = team("Garchomp:earthquake", "Rotom-Wash:hydropump")
TEAM_SERVER = team("Snorlax:bodyslam", "Gengar:shadowball")


def preview_request(packed: str, rqid: int = 1) -> dict:
    """A team-preview request showing the packed team's Pokémon."""
    mons = []
    for p in packed.split("]"):
        f = p.split("|")
        species = f[1] or f[0]
        mons.append({"ident": f"p1: {f[0]}", "details": f"{species}, L50", "condition": "100/100", "active": False,
                     "stats": {"atk": 100, "def": 100, "spa": 100, "spd": 100, "spe": 100},
                     "moves": [x.lower() for x in f[4].split(",")], "baseAbility": f[3], "item": f[2]})
    return {"teamPreview": True, "maxChosenTeamSize": len(mons), "side": {"name": "bot", "id": "p1", "pokemon": mons},
            "rqid": rqid}


def species_of(packed: str) -> List[str]:
    """A packed team's species, as its team preview shows them."""
    return [(p.split("|")[1] or p.split("|")[0]) for p in packed.split("]")]


class FakeArena:
    """A scripted VGC Arena on a free port. ``on_login(fake, ws)`` runs
    after each login. :meth:`battle` puts the bot in a battle: its init
    frame (with ``foe_shows``, the team preview's ``|poke|`` lines of both
    sides, as Showdown's log has them), then a team-preview request
    showing ``shows`` (the team the battle plays: by default the last
    ``/utm``); the bot's ``/choose`` ends it (``|win|`` for the bot), or
    with ``hold`` only records it (``chosen``) and :meth:`end` ends it.
    ``drop_on_choose``: the first choice drops the connection instead (the
    battle is sent again, with its request, after the next login).
    ``closed``: the loop time each connection ended; ``end_times``: each
    battle's ``|win|``. ``|updatesearch|`` lists the battles in progress
    after each login, battle start and end, and ``/cancelsearch``
    (``cancelled`` until the next login; ``answer_cancel`` False: never
    answered)."""

    def __init__(self, token: str = TOKEN):
        self.token = token
        self.received: List[str] = []
        self.utms: List[str] = []
        self.logins = 0
        self.connections = 0
        self.sockets: List[Any] = []
        self.ws: Any = None
        self.name = "bot"  # the bot logged in
        self.on_login = None
        self.battles = 0
        self.open: dict = {}  # room -> (foe, request)
        self.ended: List[str] = []
        self.drop_on_choose = False
        self.after_end = None  # (fake, ws, room): what follows a battle's end
        self.on_command = None  # (fake, ws, command, text) -> True when it answered a command
        self.hold = False
        self.chosen: List[str] = []  # (hold) the rooms the bot chose in, in order
        self.previews: dict = {}  # room -> its init frame's team-preview lines
        self.closed: List[float] = []
        self.end_times: dict = {}
        self.cancelled = False
        self.answer_cancel = True

    async def handler(self, ws):
        try:
            await self._serve(ws)
        finally:
            self.closed.append(asyncio.get_running_loop().time())

    async def _serve(self, ws):
        self.connections += 1
        self.sockets.append(ws)
        await ws.send("|updateuser| Guest 1|0|1|{}")
        await ws.send("|challstr|4|abcdef")
        name: Optional[str] = None
        async for msg in ws:
            self.received.append(msg)
            room, _, text = msg.partition("|")
            cmd = text[1:].split(" ", 1)[0].lower() if text.startswith("/") else None
            if cmd == "trn":
                n, _, rest = text[5:].partition(",")
                tok = rest.partition(",")[2]
                if tok != self.token:
                    await ws.send(f"|nametaken|{n}|Unknown bot name or invalid token.")
                    continue
                name = self.name = n
                self.logins += 1
                self.ws = ws
                self.cancelled = False  # (a login undoes /cancelsearch)
                await ws.send(f"|updateuser| {n}|1|1|{{}}")
                await ws.send(self.search_update())
                for rid, (foe, req) in self.open.items():  # a battle in progress is sent again
                    await ws.send(self.init_frame(rid, n, foe, self.previews.get(rid, "")))
                    await ws.send(f">{rid}\n|request|{json.dumps(req)}")
                if self.on_login is not None:
                    await self.on_login(self, ws)
            elif name is None:
                await ws.send("|popup|Log in with /trn before sending other commands.")
            elif cmd not in COMMANDS:
                await ws.send(f"|popup|The command /{cmd} is not supported by VGC Arena.")
            elif self.on_command is not None and await self.on_command(self, ws, cmd, text):
                pass  # the test's script answered it
            elif cmd == "utm":
                self.utms.append(text[5:])
            elif cmd == "cancelsearch":
                self.cancelled = True
                if self.answer_cancel:
                    await ws.send(self.search_update())
            elif cmd == "choose" and room in self.open:
                if self.drop_on_choose:
                    self.drop_on_choose = False
                    ws.transport.abort()
                    return
                if self.hold:
                    self.chosen.append(room)
                    continue
                await self.end(room, ws)

    async def end(self, room: str, ws: Any = None) -> None:
        """Ends battle ``room`` (``|win|`` for the bot), then ``after_end``."""
        ws = ws if ws is not None else self.ws
        del self.open[room]
        self.ended.append(room)
        self.end_times[room] = asyncio.get_running_loop().time()
        await ws.send(f">{room}\n|win|{self.name}")
        if self.after_end is not None:
            await self.after_end(self, ws, room)
        if self.answer_cancel or not self.cancelled:
            await ws.send(self.search_update())

    def search_update(self) -> str:
        """``|updatesearch|`` with the battles in progress (``null``: none)."""
        games = {rid: f"[Gen 9] VGC vs. {foe}" for rid, (foe, _) in self.open.items()} or None
        return f"|updatesearch|{json.dumps({'searching': [], 'games': games})}"

    @staticmethod
    def init_frame(rid: str, me: str, foe: str, preview: str = "") -> str:
        return (f">{rid}\n|init|battle\n|title|{me} vs. {foe}\n|player|p1|{me}|1|\n|player|p2|{foe}|1|"
                f"\n|gametype|doubles{preview}")

    async def battle(self, ws, foe: str = "Other Bot", shows: Optional[str] = None, fmt: str = FMT,
                     foe_shows: Optional[str] = None) -> str:
        self.battles += 1
        rid = f"battle-{fmt}-{self.battles}"
        ours = shows if shows is not None else self.utms[-1]
        req = preview_request(ours)
        self.open[rid] = (foe, req)
        if foe_shows is not None:
            pokes = [f"|poke|p1|{s}, L50|" for s in species_of(ours)] + [f"|poke|p2|{s}, L50, F|"
                                                                         for s in species_of(foe_shows)]
            self.previews[rid] = "\n" + "\n".join(["|clearpoke", *pokes, "|teampreview|4"])
        await ws.send(self.init_frame(rid, self.name, foe, self.previews.get(rid, "")))
        await ws.send(f">{rid}\n|request|{json.dumps(req)}")
        await ws.send(self.search_update())
        return rid

    async def __aenter__(self) -> "FakeArena":
        self.server = await serve(self.handler, "127.0.0.1", 0)
        port = self.server.sockets[0].getsockname()[1]
        self.url = f"ws://127.0.0.1:{port}/showdown/websocket"
        return self

    async def __aexit__(self, *exc) -> None:
        self.server.close()
        for ws in self.sockets:
            ws.transport.abort()
        await self.server.wait_closed()


def arena_client(fake: FakeArena, **kwargs) -> ShowdownClient:
    kwargs.setdefault("handler", RandomRequestHandler(0))
    return ShowdownClient("bot", TOKEN, server=replace(ARENA, websocket_url=fake.url, throttle=0.0), **kwargs)


def sent(fake: FakeArena, command: str) -> List[str]:
    return [m for m in fake.received if m.partition("|")[2].startswith(f"/{command}")]


# -- configuration ---------------------------------------------------------------------------------


def test_arena_config():
    assert server_config("arena") is ARENA
    assert ARENA.websocket_url == "wss://vgc.bot/showdown/websocket"
    assert ARENA.token_login and ARENA.auto_battles and ARENA.max_games == 5 and not ARENA.series
    assert ARENA.credentials_env == ("ARENA_BOT_NAME", "ARENA_BOT_TOKEN")
    assert server_config("wss://vgc.bot/showdown/websocket").token_login
    # Its other host (which redirects to vgc.bot) is the arena too.
    old = server_config("wss://vgc.ellep.dev/showdown/websocket")
    assert old.token_login and old.websocket_url == "wss://vgc.ellep.dev/showdown/websocket"
    # The random ladders and the open-sheet format play as the format.
    assert all(ARENA.play_format(FMT + x) == FMT for x in ("random", "ots", "otsrandom"))


def test_token_needed_and_never_sent_in_clear(monkeypatch):
    monkeypatch.delenv("ARENA_BOT_NAME", raising=False)
    monkeypatch.delenv("ARENA_BOT_TOKEN", raising=False)
    with pytest.raises(ValueError, match="token"):
        ShowdownClient("bot", server="arena")
    with pytest.raises(ValueError, match="unencrypted"):
        ShowdownClient("bot", TOKEN, server=replace(ARENA, websocket_url="ws://example.com/showdown/websocket"))
    # The environment's name and token.
    monkeypatch.setenv("ARENA_BOT_NAME", "My Bot")
    monkeypatch.setenv("ARENA_BOT_TOKEN", TOKEN)
    c = ShowdownClient(server="arena")
    assert c.username == "My Bot" and c._token == TOKEN
    assert c.max_concurrent == 1


def test_team_shows():
    assert _team_shows(TEAM_A, preview_request(TEAM_A))
    assert not _team_shows(TEAM_B, preview_request(TEAM_A))
    # After preview, the four brought (here one of two) are a subset.
    req = preview_request(TEAM_A)
    req["side"]["pokemon"] = req["side"]["pokemon"][1:]
    assert _team_shows(TEAM_A, req)
    assert not _team_shows(TEAM_A, {"side": {}})


# TEAM_A's Pokémon and moves with other items, abilities and order (two
# teams of a rotation, as a race between their uploads plays them).
TEAM_A_ITEMS = "]".join(reversed([p.replace("leftovers|static", "sitrusberry|lightningrod") for p in TEAM_A.split("]")]))


def test_team_shows_tells_apart_teams_of_the_same_pokemon_and_moves():
    """A team preview shows each Pokémon in its place with its item and
    ability: a team of the same Pokémon and moves doesn't fit it. After
    the preview (the four brought, items possibly moved) species and moves
    decide, as before."""
    assert _team_shows(TEAM_A_ITEMS, preview_request(TEAM_A_ITEMS))
    assert not _team_shows(TEAM_A_ITEMS, preview_request(TEAM_A))
    assert not _team_shows(TEAM_A, preview_request(TEAM_A_ITEMS))
    turn = preview_request(TEAM_A)
    del turn["teamPreview"]
    assert _team_shows(TEAM_A_ITEMS, turn) and _team_shows(TEAM_A, turn)


def test_a_battle_with_the_previous_team_of_the_same_pokemon_gets_it():
    """The battle starts before the rotation's new team (the same Pokémon
    and moves as the last one) lands: the room gets the team it plays, the
    previous one, and the rotation doesn't move on until the new one is
    played: a handler given the wrong team would read sets the requests
    contradict."""
    seen = {}
    teams = iter([TEAM_A, TEAM_A_ITEMS, TEAM_B])

    class Recorder(RandomRequestHandler):
        def open(self, room):
            seen[room.id] = room.team

    async def main():
        async with FakeArena() as fake:
            async with arena_client(fake, handler=Recorder(0)) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=3, team=lambda fmt: next(teams), format=FMT))
                await until(lambda: fake.utms)
                a = await fake.battle(fake.ws)  # TEAM_A
                await until(lambda: len(fake.utms) == 2)
                assert fake.utms[-1] == TEAM_A_ITEMS
                b = await fake.battle(fake.ws, shows=TEAM_A)  # started before TEAM_A_ITEMS landed
                await until(lambda: b in fake.ended)
                await asyncio.sleep(0.1)
                assert len(fake.utms) == 2
                c3 = await fake.battle(fake.ws)  # TEAM_A_ITEMS
                await asyncio.wait_for(op, 10)
                return a, b, c3

    a, b, c3 = run(main())
    assert (seen[a], seen[b], seen[c3]) == (TEAM_A, TEAM_A, TEAM_A_ITEMS)


def test_teams_alike_but_for_their_stats_are_told_apart_by_them():
    """Two teams the request's sets fit alike (another nature on one
    Pokémon): the stats the engine computes for each pick the one the
    battle plays, the previous upload here."""
    import types

    import pikapro

    from pikapro.showdown.protocol import pack_team

    played = pack_team(pikapro.sample_team("championsvgc"), FMT)
    mons = [p.split("|") for p in played.split("]")]
    mons[0][5] = "Adamant" if mons[0][5] != "Adamant" else "Modest"
    other = "]".join("|".join(f) for f in mons)
    req = json.loads(pikapro.Battle("championsvgc", [played, played], seed=1).request_json(0))
    assert _team_shows(played, req) and _team_shows(other, req)
    c = ShowdownClient("bot", TOKEN, server=ARENA)
    c._utm, c._utm_prev = other, played
    room = types.SimpleNamespace(id="battle-1", request=req, play_format=FMT, format=FMT)
    assert c._auto_room_team(room) == played and not c._utm_played
    c._utm, c._utm_prev = played, other
    assert c._auto_room_team(room) == played and c._utm_played


# -- login, team, battles ------------------------------------------------------------------------------


def test_token_login_team_and_a_battle_it_is_put_in(caplog):
    """The token goes as the /trn assertion; the team goes up after the
    login; a challenge's |pm| only announces it, and the battle the arena
    puts the bot in is played (no /accept); commands the arena doesn't take
    (/avatar, /timer, /leave, /join) are never sent; the token is in no
    log record, websockets' frame log at DEBUG included."""
    caplog.set_level(logging.DEBUG, logger="pikapro.showdown")
    caplog.set_level(logging.DEBUG, logger="websockets")

    async def main():
        async with FakeArena() as fake:
            async with arena_client(fake, avatar="dawn", timer=True) as c:
                assert c.connected
                op = asyncio.ensure_future(c.accept_challenges(n=1, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                assert fake.utms == [TEAM_A]
                await fake.ws.send(f"|pm| Other Bot| bot|/challenge {FMT}|{FMT}|||")
                await asyncio.sleep(0.05)
                rid = await fake.battle(fake.ws)
                results = await asyncio.wait_for(op, 10)
                assert [r.room for r in results] == [rid] and results[0].won is True
                assert results[0].opponent == "Other Bot"
            assert sent(fake, "trn") == [f"|/trn bot,0,{TOKEN}"]
            for cmd in ("accept", "avatar", "timer", "leave", "join", "cancelsearch"):
                assert not sent(fake, cmd), cmd
            assert not [m for m in fake.received if "/choose" in m and not m.endswith("|1")]  # the rqid goes along

    run(main())
    # (The fake server's own records, websockets.server, are the server's.)
    text = "\n".join(r.getMessage() + repr(getattr(r, "fields", "")) for r in caplog.records
                     if not r.name.startswith("websockets.server"))
    assert TOKEN not in text
    assert "/trn bot,0,<assertion>" in text
    assert "/trn bot,0,<token>" in text  # websockets' own record of the frame
    assert not logging.getLogger("websockets.client").filters  # removed on close
    assert "command not sent" in text


def test_accept_plan_is_not_asked_on_the_arena(caplog):
    """``accept_challenges(plan=...)`` (``AcceptPlan``, e.g. a bot that
    lets users send it a team) on a server that starts battles itself: nothing is
    accepted, so the plan is never asked and no /accept goes out; the
    battle the arena starts is played with the operation's team."""
    caplog.set_level(logging.INFO, logger="pikapro.showdown")
    asked = []

    def plan(user, fmt):
        asked.append((user, fmt))
        return AcceptPlan(team=TEAM_B, open_team_sheets=True)

    async def main():
        async with FakeArena() as fake:
            async with arena_client(fake) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=1, team=TEAM_A, format=FMT, plan=plan))
                await until(lambda: fake.utms)
                await fake.ws.send(f"|pm| Other Bot| bot|/challenge {FMT}|{FMT}|||")
                await asyncio.sleep(0.05)
                rid = await fake.battle(fake.ws)
                results = await asyncio.wait_for(op, 10)
                assert [r.room for r in results] == [rid] and results[0].won is True
            assert fake.utms == [TEAM_A]
            assert not sent(fake, "accept")

    run(main())
    assert asked == []
    assert any("plan ignored" in r.getMessage() for r in caplog.records)


def test_bad_token_raises_without_the_token():
    async def main():
        async with FakeArena(token="pka_other") as fake:
            c = arena_client(fake, reconnect=False)
            with pytest.raises(LoginError) as e:
                await c.connect()
            assert "invalid token" in str(e.value) and TOKEN not in str(e.value)

    run(main())


def test_rotation_uploads_the_next_team_after_a_battle_with_it():
    """A rotation's next team goes up once a battle was played with the last
    one; a battle on the server's team (the random ladder) doesn't move it,
    and is played with no team of ours; a battle that started with the
    previous team (before the new upload landed) gets that one."""
    teams = iter([TEAM_A, TEAM_B, TEAM_A, TEAM_B])

    async def main():
        async with FakeArena() as fake:
            async with arena_client(fake) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=5, team=lambda fmt: next(teams), format=FMT))
                await until(lambda: fake.utms)
                assert fake.utms == [TEAM_A]
                await fake.battle(fake.ws)  # own ladder, TEAM_A
                await until(lambda: len(fake.utms) == 2)
                assert fake.utms[-1] == TEAM_B
                rid = await fake.battle(fake.ws, shows=TEAM_SERVER, fmt=FMT + "random")  # the arena's team
                await until(lambda: rid in fake.ended)
                await asyncio.sleep(0.1)
                assert len(fake.utms) == 2  # not played with TEAM_B: no rotation
                rid3 = await fake.battle(fake.ws, shows=TEAM_A)  # started before TEAM_B landed
                await until(lambda: rid3 in fake.ended)
                await asyncio.sleep(0.1)
                assert len(fake.utms) == 2
                rid4 = await fake.battle(fake.ws)  # TEAM_B
                await until(lambda: len(fake.utms) == 3)  # the fifth game's team
                assert fake.utms[-1] == TEAM_A
                op.cancel()
                return [r for r in c.results if r.room in (rid, rid3, rid4)]

    results = run(main())
    assert [r.won for r in results] == [True] * 3


def test_room_team_is_the_one_played():
    seen = {}

    class Recorder(RandomRequestHandler):
        def open(self, room):
            seen[room.id] = (room.team, room.play_format)

    async def main():
        async with FakeArena() as fake:
            async with arena_client(fake, handler=Recorder(0)) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=2, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                a = await fake.battle(fake.ws)
                await until(lambda: a in fake.ended)
                b = await fake.battle(fake.ws, shows=TEAM_SERVER, fmt=FMT + "random")
                await asyncio.wait_for(op, 10)
                return a, b

    a, b = run(main())
    assert seen[a] == (TEAM_A, FMT)
    assert seen[b] == (None, FMT)  # the server's team; played as the format


def test_random_ladder_second_game_follows_on_its_own():
    """The random ladder: the arena puts both bots straight into the second
    game with the teams swapped; the client plays both without asking."""

    async def swap(fake, ws, room):
        if fake.battles == 1:
            await fake.battle(ws, shows=TEAM_B, fmt=FMT + "random")

    async def main():
        async with FakeArena() as fake:
            fake.after_end = swap
            async with arena_client(fake) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=2, team=None, format=FMT))
                await until(lambda: c._auto_ops)  # the operation is waiting
                await fake.battle(fake.ws, shows=TEAM_SERVER, fmt=FMT + "random")
                results = await asyncio.wait_for(op, 10)
                assert not fake.utms  # no team of ours: nothing uploaded
                return results

    results = run(main())
    assert [r.format for r in results] == [FMT + "random"] * 2
    assert all(r.won for r in results)


def test_popups_rejected_team_pair_limit_unsupported(caplog):
    """A rejected team: the rotation's next goes up; the pair limit and an
    unsupported command are logged by name; ``on_popup`` sees each."""
    caplog.set_level(logging.INFO, logger="pikapro.showdown")
    teams = iter([TEAM_A, TEAM_B, TEAM_A])
    popups = []

    async def main():
        async with FakeArena() as fake:
            async with arena_client(fake) as c:
                c.on_popup = popups.append
                op = asyncio.ensure_future(c.accept_challenges(n=1, team=lambda fmt: next(teams), format=FMT))
                await until(lambda: fake.utms)
                await fake.ws.send("|popup|Your team was rejected for the following reasons:||- Pikachu is banned.")
                await until(lambda: len(fake.utms) == 2)
                assert fake.utms[-1] == TEAM_B
                await fake.ws.send("|popup|You and Other Bot have played each other 10 times in the last 60 minutes.")
                await c.send("/hello")  # not a command the arena takes: never sent
                await fake.ws.send("|popup|The command /x is not supported by VGC Arena.")
                await until(lambda: len(popups) == 3)
                await fake.battle(fake.ws)
                await asyncio.wait_for(op, 10)
                assert not sent(fake, "hello")

    run(main())
    events = [getattr(r, "event", None) for r in caplog.records]
    assert "team rejected" in events and "pair limit" in events and "unsupported command" in events
    assert popups[0].startswith("Your team was rejected") and "\n- Pikachu" in popups[0]


def test_reconnect_logs_in_again_resends_the_team_and_finishes_the_battle(caplog):
    caplog.set_level(logging.DEBUG, logger="pikapro.showdown")

    async def main():
        async with FakeArena() as fake:
            fake.drop_on_choose = True
            async with arena_client(fake) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=1, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                rid = await fake.battle(fake.ws)
                results = await asyncio.wait_for(op, 15)
            assert fake.logins == 2 and fake.connections == 2
            assert fake.utms == [TEAM_A, TEAM_A]  # sent again after the second login
            assert sent(fake, "trn") == [f"|/trn bot,0,{TOKEN}"] * 2
            assert not sent(fake, "join")  # the arena sends the battle again by itself
            assert [r.room for r in results] == [rid] and results[0].won is True

    run(main(), timeout=40)
    assert TOKEN not in "\n".join(r.getMessage() for r in caplog.records)


def test_logged_in_from_another_connection_does_not_reconnect(caplog):
    """The arena closes a connection with 4001 when the bot logs in from
    another one: the client doesn't reconnect (that would close the other
    in turn), and the operation ends with the server's reason. Another
    close (here 4002, the login timeout) is reconnected as usual."""
    caplog.set_level(logging.INFO, logger="pikapro.showdown")

    async def main():
        async with FakeArena() as fake:
            async with arena_client(fake) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=1, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                await fake.ws.close(4002, "Login timed out")
                await until(lambda: fake.logins == 2)
                await fake.ws.close(4001, "Logged in from another connection")
                with pytest.raises(ShowdownError, match="4001: Logged in from another connection"):
                    await asyncio.wait_for(op, 10)
                await asyncio.sleep(0.3)
                assert fake.connections == 2

    run(main(), timeout=30)
    assert "connection ended" in [getattr(r, "event", None) for r in caplog.records]


def test_ladder_search_is_refused_up_front():
    async def main():
        async with FakeArena() as fake:
            async with arena_client(fake) as c:
                with pytest.raises(Exception, match="no ladder search"):
                    await c.ladder(1, FMT, TEAM_A)
                await c.send(f"/search {FMT}")  # not a command the arena takes: dropped
                await asyncio.sleep(0.05)
                assert not sent(fake, "search")

    run(main())


def test_matched_battles_and_the_arenas_popups(caplog):
    """The arena pairs bots itself: a battle
    starts with ``|init|battle``, no challenge before it, and plays the team
    up when it was paired; the popups of that spec (no team for the
    ladder's format, a ``/search``'s "no queue") are read by their words;
    the keepalive pings come at least every 30 s."""
    caplog.set_level(logging.INFO, logger="pikapro.showdown")
    seen = {}

    class Recorder(RandomRequestHandler):
        def open(self, room):
            seen[room.id] = room.team

    async def main():
        async with FakeArena() as fake:
            async with arena_client(fake, handler=Recorder(0)) as c:
                assert c.ping_interval is not None and c.ping_interval <= 30
                op = asyncio.ensure_future(c.accept_challenges(n=1, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                await fake.ws.send("|popup|Send a team legal in [Gen 9 Champions] VGC 2026 Reg M-C with /utm first.")
                await fake.ws.send("|popup|There is no queue to join: while your bot is online, with a team sent for "
                                   "its ladders, the arena pairs it for rated battles by itself. Challenges are "
                                   "practice and not rated.")
                rid = await fake.battle(fake.ws, foe="Matched Bot")  # no |pm| before it: the arena's pairing
                results = await asyncio.wait_for(op, 10)
                assert [r.room for r in results] == [rid] and results[0].opponent == "Matched Bot"
                for cmd in ("challenge", "search", "accept"):
                    assert not sent(fake, cmd), cmd
                return rid

    rid = run(main())
    assert seen[rid] == TEAM_A
    events = [getattr(r, "event", None) for r in caplog.records]
    assert "no team" in events and "no ladder search" in events


# -- a reconnect in the middle of a battle ----------------------------------------------------------


def move_request(rqid: int) -> dict:
    """A turn's request: one active Pokémon with one move."""
    return {"active": [{"moves": [{"move": "Tackle", "id": "tackle", "pp": 35, "maxpp": 35, "target": "normal",
                                   "disabled": False}]}],
            "side": {"name": "bot", "id": "p1", "pokemon": [{"ident": "p1: Eevee", "details": "Eevee, L50",
                                                             "condition": "100/100", "active": True,
                                                             "moves": ["tackle"], "baseAbility": "runaway",
                                                             "item": ""}]},
            "rqid": rqid}


class Turns:
    """The arena's choice handling (``FakeArena.on_command``): a choice for
    the open request is kept, and the request then takes no other (``There
    is nothing for you to choose right now``); one with another rqid is
    too late. ``drop_after_choice``: the first choice kept drops the
    connection (the arena keeps the choice, and sends the battle and its
    open request again after the next login). :meth:`next_turn` is the
    opponent's choice: turn 1 starts with a move request; the bot's choice
    for it wins the battle."""

    NOTHING = "[Invalid choice] There is nothing for you to choose right now"
    TOO_LATE = "[Invalid choice] Sorry, too late to make a different move; the next turn has already started"

    def __init__(self, drop_after_choice: bool = False):
        self.drop_after_choice = drop_after_choice
        self.chosen: dict = {}  # room -> the rqid that has our choice
        self.choices: List[tuple] = []  # (connection, rqid, choice) of every /choose
        self.errors: List[str] = []

    async def __call__(self, fake, ws, cmd, text):
        if cmd != "choose":
            return False
        room = fake.received[-1].partition("|")[0]
        choice, _, rqid = text[len("/choose "):].partition("|")
        self.choices.append((fake.connections, rqid, choice))
        if room not in fake.open:
            return True
        foe, req = fake.open[room]
        if rqid != str(req["rqid"]):
            await self.error(ws, room, self.TOO_LATE)
        elif self.chosen.get(room) == rqid:
            await self.error(ws, room, self.NOTHING)
        else:
            self.chosen[room] = rqid
            if self.drop_after_choice:
                self.drop_after_choice = False
                ws.transport.abort()
            elif not req.get("teamPreview"):
                del fake.open[room]
                fake.ended.append(room)
                await ws.send(f">{room}\n|win|{fake.name}")
        return True

    async def error(self, ws, room: str, message: str) -> None:
        self.errors.append(message)
        await ws.send(f">{room}\n|error|{message}")

    async def next_turn(self, fake, room: str, stale_error: bool = False) -> None:
        """The opponent chose: turn 1 and its request (rqid 2); with
        ``stale_error``, the "too late" answer to a choice still on its way
        for the old request comes after it."""
        foe, _ = fake.open[room]
        req = move_request(2)
        fake.open[room] = (foe, req)
        await fake.ws.send(f">{room}\n|\n|start\n|turn|1")
        await fake.ws.send(f">{room}\n|request|{json.dumps(req)}")
        if stale_error:
            await self.error(fake.ws, room, self.TOO_LATE)


def test_reconnect_after_our_choice_resyncs_with_the_open_request(caplog):
    """The connection drops after the bot's team-preview choice reached the
    arena, while the opponent thinks. After the next login the arena sends
    the battle and its open request again; the bot answers it once, the
    arena says there is nothing to choose (it has the choice): a stale
    choice, so nothing is asked again and nothing falls back to
    ``default``. The opponent then chooses: the next request (and a late
    "too late" for the old one) is answered with a normal choice, and none
    of it counts as an invalid choice."""
    caplog.set_level(logging.INFO, logger="pikapro.showdown")
    script = Turns(drop_after_choice=True)

    async def main():
        async with FakeArena() as fake:
            fake.on_command = script
            async with arena_client(fake) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=1, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                rid = await fake.battle(fake.ws)
                await until(lambda: fake.logins == 2)
                await until(lambda: Turns.NOTHING in script.errors)  # the replayed request answered again
                await asyncio.sleep(0.3)  # (a retry would go out now)
                await script.next_turn(fake, rid, stale_error=True)
                results = await asyncio.wait_for(op, 10)
            return rid, results

    rid, results = run(main(), timeout=40)
    by_conn = [(conn, rqid) for conn, rqid, _ in script.choices]
    # The preview's choice, its one answer after the reconnect, the turn's.
    assert by_conn == [(1, "1"), (2, "1"), (2, "2")]
    assert not [ch for _, _, ch in script.choices if ch == "default"]
    assert script.choices[-1][2].startswith("move 1")  # the handler's own choice
    r = results[0]
    assert r.room == rid and r.won is True
    assert (r.invalid, r.stale, r.fallbacks) == (0, 2, 0)
    events = [getattr(x, "event", None) for x in caplog.records]
    assert "choice error" not in events and events.count("stale choice") == 2


def test_reconnect_while_deciding_answers_the_replayed_request_once():
    """The connection drops while the handler decides (its choice never
    reached the arena). After the next login the arena sends the battle and
    its open request again: the decision made for the old connection's
    room is dropped, and the replayed request is answered by the handler,
    once, with its rqid."""

    class Slow(RandomRequestHandler):
        def __init__(self):
            super().__init__(0)
            self.calls = 0
            self.entered = asyncio.Event()
            self.release = asyncio.Event()

        async def choose(self, rooms):
            self.calls += 1
            if self.calls == 1:
                self.entered.set()
                await self.release.wait()
                return ["team 21"]  # for the old connection's room: never sent
            return ["team 12"]

    async def main():
        async with FakeArena() as fake:
            slow = Slow()
            async with arena_client(fake, handler=slow) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=1, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                rid = await fake.battle(fake.ws)
                await asyncio.wait_for(slow.entered.wait(), 5)
                old = c.rooms[rid]
                fake.ws.transport.abort()  # the drop, mid-decision
                await until(lambda: fake.logins == 2 and c.rooms.get(rid) not in (None, old))
                slow.release.set()
                results = await asyncio.wait_for(op, 10)
            return fake, slow, results

    fake, slow, results = run(main(), timeout=40)
    assert [m.partition("|")[2] for m in fake.received if "/choose" in m] == ["/choose team 12|1"]
    assert slow.calls == 2
    r = results[0]
    assert r.won is True and (r.invalid, r.stale, r.fallbacks) == (0, 0, 0)


# -- several battles at once ("Battles at once" on the owner's account page) -------------------------------


def test_the_arena_allows_five_battles_at_once():
    c = ShowdownClient("bot", TOKEN, server=ARENA, max_concurrent=5)
    assert c.max_concurrent == 5
    assert ShowdownClient("bot", TOKEN, server=ARENA, max_concurrent=9).max_concurrent == 5


@pytest.mark.parametrize("concurrent", [1, 5])
def test_five_battles_started_at_once_are_all_answered(concurrent):
    """The arena starts five battles within a moment (its rooms, its clocks):
    every room's team preview is answered at once, each in its own room
    with its own rqid, whatever ``max_concurrent`` (a room past the
    client's slots is played all the same); a slow handler holds the rooms
    one batch, not one each."""

    class Slow(RandomRequestHandler):
        def __init__(self):
            super().__init__(0)
            self.batches: List[List[str]] = []

        async def choose(self, rooms):
            self.batches.append([r.id for r in rooms])
            await asyncio.sleep(0.3)
            return await super().choose(rooms)

    async def main():
        async with FakeArena() as fake:
            fake.hold = True
            slow = Slow()
            async with arena_client(fake, handler=slow, max_concurrent=concurrent) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=5, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                t0 = asyncio.get_running_loop().time()
                rooms = [await fake.battle(fake.ws, foe=f"Bot {i}") for i in range(5)]
                await until(lambda: set(fake.chosen) == set(rooms), 5)
                took = asyncio.get_running_loop().time() - t0
                for rid in rooms:
                    await fake.end(rid)
                results = await asyncio.wait_for(op, 10)
            return fake, slow, rooms, results, took

    fake, slow, rooms, results, took = run(main())
    assert sorted(r.room for r in results) == sorted(rooms) and all(r.won for r in results)
    assert took < 1.5  # (two batches of 0.3 s at most: the first room's, then the four that came meanwhile)
    assert len(slow.batches) <= 2 and sorted(sum(slow.batches, [])) == sorted(rooms)
    chose = [m for m in fake.received if "/choose" in m]
    assert sorted(m.partition("|")[0] for m in chose) == sorted(rooms) and all(m.endswith("|1") for m in chose)


def test_a_mega_evolved_request_still_shows_the_team():
    """After a Mega Evolution the request names the forme (``Garchomp-Mega-Z``):
    the team still shows (it is not taken for a team changed mid-battle)."""
    packed = team("Garchomp:earthquake", "Charizard:heatwave", "Pikachu:thunderbolt")
    req = preview_request(packed)
    del req["teamPreview"]
    req["side"]["pokemon"][0]["details"] = "Garchomp-Mega-Z, L50"
    req["side"]["pokemon"][1]["details"] = "Charizard-Mega-Y, L50, M"
    assert _team_shows(packed, req)
    req["side"]["pokemon"][2]["details"] = "Raichu, L50"
    assert not _team_shows(packed, req)


def test_a_battle_on_an_older_upload_gets_it():
    """Several battles at once: three uploads went up in a row, and a
    battle paired before the last two landed plays the first: the room
    gets it (``earlier``), not the server's."""
    team_c = team("Snorlax:bodyslam", "Gengar:shadowball")
    teams = iter([TEAM_A, TEAM_B, team_c])
    seen = {}

    class Recorder(RandomRequestHandler):
        def open(self, room):
            seen[room.id] = room.team

    async def main():
        async with FakeArena() as fake:
            fake.hold = True
            async with arena_client(fake, handler=Recorder(0)) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=1, team=lambda fmt: next(teams), format=FMT))
                await until(lambda: fake.utms)
                c.upload_team()
                c.upload_team(c.auto_team[0], FMT)
                await until(lambda: len(fake.utms) == 3)
                assert [p for _, p in c.uploads] == [TEAM_A, TEAM_B, team_c] and c.uploaded_team == team_c
                rid = await fake.battle(fake.ws, shows=TEAM_A)
                await until(lambda: rid in fake.chosen)
                assert c.rooms[rid].opened > 0
                await fake.end(rid)
                await asyncio.wait_for(op, 10)
                return rid

    rid = run(main())
    assert seen[rid] == TEAM_A


def test_a_rotation_driven_by_the_caller_goes_up_once():
    """``rotate_on_wait`` False (the caller puts each next team up itself,
    with ``upload_team``, as a battle starts): the operation uploads the
    first team only, however many battles start at once."""
    teams = iter([TEAM_A, TEAM_B, TEAM_A, TEAM_B])

    async def main():
        async with FakeArena() as fake:
            fake.hold = True
            async with arena_client(fake, max_concurrent=3) as c:
                c.rotate_on_wait = False
                op = asyncio.ensure_future(c.accept_challenges(n=3, team=lambda fmt: next(teams), format=FMT))
                await until(lambda: fake.utms)
                rooms = [await fake.battle(fake.ws, foe=f"Bot {i}", shows=TEAM_A) for i in range(3)]
                await until(lambda: set(fake.chosen) == set(rooms))
                for rid in rooms:
                    await fake.end(rid)
                await asyncio.wait_for(op, 10)
                return fake

    fake = run(main())
    assert fake.utms == [TEAM_A]


def test_a_reconnect_with_several_battles_sends_each_again():
    """The connection drops with three battles in progress: after the login
    the arena sends each again (``|init|battle``, its open request), and
    the client rebuilds and answers every one."""

    async def main():
        async with FakeArena() as fake:
            fake.hold = True
            async with arena_client(fake, max_concurrent=3) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=3, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                rooms = [await fake.battle(fake.ws, foe=f"Bot {i}") for i in range(3)]
                await until(lambda: set(fake.chosen) == set(rooms))
                old = {rid: c.rooms[rid] for rid in rooms}
                fake.chosen.clear()
                fake.ws.transport.abort()
                await until(lambda: fake.logins == 2 and set(fake.chosen) == set(rooms), 15)
                assert all(c.rooms[rid] is not old[rid] for rid in rooms)  # rebuilt from the resent log
                for rid in rooms:
                    await fake.end(rid)
                return fake, await asyncio.wait_for(op, 10)

    fake, results = run(main(), timeout=40)
    assert sorted(r.room for r in results) == sorted(f"battle-{FMT}-{i}" for i in (1, 2, 3))
    assert all(r.won for r in results) and fake.connections == 2


def test_cancel_search_stops_once_the_arena_lists_no_games(caplog):
    """``cancel_search``: ``/cancelsearch`` goes out (again after a login,
    before the team); ``stopped`` waits for an ``|updatesearch|`` listing
    no games after it, and for every battle to end. The owner's pause
    popup is an ordinary notice."""
    caplog.set_level(logging.INFO, logger="pikapro.showdown")

    async def main():
        async with FakeArena() as fake:
            fake.hold = True
            async with arena_client(fake, max_concurrent=2) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=2, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                a = await fake.battle(fake.ws, foe="Bot A")
                b = await fake.battle(fake.ws, foe="Bot B")
                await until(lambda: {a, b} <= set(fake.chosen))
                assert not c.stopped
                c.cancel_search()
                await until(lambda: fake.cancelled)
                await asyncio.sleep(0.2)
                assert not c.stopped  # (two battles listed)
                await fake.ws.send("|popup|This bot's owner paused it: the arena starts nothing new for it until "
                                   "they resume it from their account. It plays what it is playing, and any "
                                   "return game it owes.")
                await fake.ws.send("|popup|The arena starts nothing new for this bot: it plays what it is playing, "
                                   "and any return game it owes. Once |updatesearch| lists no games it can go "
                                   "offline without losing anything. /search, or logging in again, undoes this.")
                await drop(fake.ws)  # a drop: the login undoes /cancelsearch, so it goes again
                await until(lambda: fake.logins == 2 and fake.cancelled, 15)
                await fake.end(a)
                await asyncio.sleep(0.2)
                assert not c.stopped
                await fake.end(b)
                await until(lambda: c.stopped)
                await asyncio.wait_for(op, 10)
                return fake

    fake = run(main(), timeout=40)
    second = fake.received[[i for i, m in enumerate(fake.received) if m.startswith("|/trn")][1]:]
    assert second.index("|/cancelsearch") < next(i for i, m in enumerate(second) if m.startswith("|/utm"))
    events = [(r.levelno, getattr(r, "event", None)) for r in caplog.records]
    assert (logging.INFO, "paused by its owner") in events and (logging.INFO, "search cancelled") in events
    assert not [e for lvl, e in events if lvl >= logging.WARNING and e in ("popup", "connection ended")]


# -- the arena's |updatesearch| decides which battles are in progress --------------------------------------


def test_a_battle_that_ends_unseen_is_closed_once_unlisted(monkeypatch):
    """The arena ends a battle without the client seeing its ``|win|``: once
    ``|updatesearch|`` stops listing it (and no frame comes for the grace),
    its room is closed as ended unseen and its slot is free for the next
    battle; the battle still listed plays on."""
    import pikapro.showdown.client as client_mod

    monkeypatch.setattr(client_mod, "UNLISTED_GRACE_S", 0.5)

    async def main():
        async with FakeArena() as fake:
            fake.hold = True
            async with arena_client(fake, max_concurrent=2) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=3, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                a = await fake.battle(fake.ws, foe="Bot A")
                b = await fake.battle(fake.ws, foe="Bot B")
                await until(lambda: {a, b} <= set(fake.chosen))
                del fake.open[b]  # over on the arena; its |win| never reaches us
                await fake.ws.send(fake.search_update())
                await asyncio.sleep(0.2)
                assert b in c.rooms  # (within the grace)
                await until(lambda: b not in c.rooms, 5)
                assert a in c.rooms
                d = await fake.battle(fake.ws, foe="Bot D")  # the freed slot's battle
                await until(lambda: d in fake.chosen)
                await fake.end(a)
                await fake.end(d)
                return a, b, d, await asyncio.wait_for(op, 10)

    a, b, d, results = run(main())
    by = {r.room: r for r in results}
    assert set(by) == {a, b, d}
    assert by[a].won is True and by[d].won is True
    assert by[b].finished is False and by[b].reason == client_mod.UNLISTED_REASON


def test_a_battle_that_ends_during_a_reconnect_is_closed(monkeypatch):
    """The battle ends while the client reconnects: the arena sends again
    only the battles in progress, so
    this one never comes back; the login's ``|updatesearch|`` doesn't list
    it, and its room is closed. After ``cancel_search`` the arena's empty
    list stops the client whatever rooms it still had."""
    import pikapro.showdown.client as client_mod

    monkeypatch.setattr(client_mod, "UNLISTED_GRACE_S", 0.5)

    async def main():
        async with FakeArena() as fake:
            fake.hold = True
            async with arena_client(fake, max_concurrent=2) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=2, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                a = await fake.battle(fake.ws, foe="Bot A")
                b = await fake.battle(fake.ws, foe="Bot B")
                await until(lambda: {a, b} <= set(fake.chosen))
                fake.chosen.clear()
                del fake.open[a]  # ends during the drop
                fake.ws.transport.abort()
                await until(lambda: fake.logins == 2 and b in fake.chosen, 15)
                await until(lambda: a not in c.rooms, 5)
                assert b in c.rooms
                # the stop: b ended unseen too, and the arena lists no games
                del fake.open[b]
                c.cancel_search()
                await until(lambda: c.stopped, 5)
                assert c.close_unlisted(0.0) == [b]
                return a, b, await asyncio.wait_for(op, 10)

    a, b, results = run(main(), timeout=40)
    assert {r.room: (r.finished, r.reason) for r in results} == {a: (False, client_mod.UNLISTED_REASON),
                                                                 b: (False, client_mod.UNLISTED_REASON)}


def test_an_unreadable_game_list_closes_nothing(monkeypatch):
    """A ``|updatesearch|`` whose games aren't keyed by room id leaves the
    rooms alone (the client can't tell which are in progress)."""
    import pikapro.showdown.client as client_mod

    monkeypatch.setattr(client_mod, "UNLISTED_GRACE_S", 0.2)

    async def main():
        async with FakeArena() as fake:
            fake.hold = True
            async with arena_client(fake) as c:
                op = asyncio.ensure_future(c.accept_challenges(n=1, team=TEAM_A, format=FMT))
                await until(lambda: fake.utms)
                a = await fake.battle(fake.ws, foe="Bot A")
                await until(lambda: a in fake.chosen)
                await fake.ws.send('|updatesearch|{"searching":[],"games":{"174258":"Bot A"}}')
                await asyncio.sleep(0.8)
                assert a in c.rooms
                await fake.end(a)
                return await asyncio.wait_for(op, 10)

    assert run(main())[0].won is True
