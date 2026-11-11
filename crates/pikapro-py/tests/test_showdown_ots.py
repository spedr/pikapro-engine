"""pikapro.showdown's Open Team Sheets answers, the ``"mutual"`` mode
above all: against a scripted server that offers the sheets as Showdown
does (``data/rulesets.ts`` "Open Team Sheets", ``server/chat-commands/
core.ts`` ``acceptopenteamsheets`` / ``rejectopenteamsheets``). The
client never accepts first: it waits for the opponent's answer, accepts
after theirs, does nothing after a rejection, rejects when the wait ends,
and holds the room's team-preview decision until the question is settled.
"""

from __future__ import annotations

import asyncio
import json
import time
from typing import Dict, List, Tuple

import pytest

pytest.importorskip("websockets")

from pikapro.showdown import BattleRoom, RandomRequestHandler, ShowdownClient  # noqa: E402

from _showdown_fakes import REQUEST, Fake, run, until  # noqa: E402

FMT = "gen9championsvgc2026regmc"
OFFER = (
    '|uhtml|otsrequest|<button name="send" value="/acceptopenteamsheets" class="button" style="margin-right: 10px;">'
    '<strong>Accept Open Team Sheets</strong></button><button name="send" value="/rejectopenteamsheets" '
    'class="button" style="margin-top: 10px"><strong>Deny Open Team Sheets</strong></button>'
)
PREVIEW = {**REQUEST, "teamPreview": True, "maxChosenTeamSize": 4}
FOE = "Foe Bot"  # a name with a space: answers are matched by id


class OtsFake(Fake):
    """A server that opens one battle room per entry of ``rooms`` at each
    first login, offering open sheets (``offer``) at team preview. The
    opponent answers (``foe``: ``"accept"`` / ``"reject"`` / ``None``)
    ``delay`` seconds after the room opened (``delay=None``: in the room's
    first frame, before its request), or forfeits then (``foe="forfeit"``).
    The client's answers come back as Showdown's plain lines; with both
    accepted, the sheets (``|showteam|``). A choice ends the battle. A
    ``/join`` (after a reconnect) sends the room's log again."""

    def __init__(self, rooms: List[dict], **kw):
        super().__init__(**kw)
        self.specs = rooms
        self.ws = None
        self.name = ""
        self.t0 = 0.0
        self.log: Dict[str, List[str]] = {}
        self.times: List[Tuple[float, str]] = []  # (seconds since the rooms opened, message)
        self.agreed: Dict[str, set] = {}
        self.rejected: Dict[str, bool] = {}

    async def handler(self, ws):
        self.connections += 1
        self.sockets.append(ws)
        await ws.send("|challstr|4|abcdef")
        async for msg in ws:
            self.received.append(msg)
            self.times.append((time.monotonic() - self.t0, msg))
            room, _, text = msg.partition("|")
            if text.startswith("/trn "):
                self.name = text[5:].split(",", 1)[0]
                self.logins += 1
                self.ws = ws
                await ws.send(f"|updateuser| {self.name}|1|1|{{}}")
                if self.drop_after is not None and self.logins == 1:
                    asyncio.get_running_loop().call_later(self.drop_after, ws.transport.abort)
                if self.logins == 1:
                    self.t0 = time.monotonic()
                    for spec in self.specs:
                        await self.open_room(spec)
            elif text.startswith("/join "):
                rid = text[len("/join ") :]
                if rid in self.log:
                    await ws.send(f">{rid}\n" + "\n".join(self.log[rid]))
            elif text == "/acceptopenteamsheets":
                await self.answer(room, self.name, True)
            elif text == "/rejectopenteamsheets":
                await self.answer(room, self.name, False)
            elif text.startswith("/choose "):
                await self.add(room, "|", f"|win|{self.name}")
            elif text.startswith("/forfeit"):
                await self.add(room, f"|win|{FOE}")

    async def add(self, rid: str, *lines: str) -> None:
        self.log[rid].extend(lines)
        if self.ws is not None:
            try:
                await self.ws.send(f">{rid}\n" + "\n".join(lines))
            except Exception:  # noqa: BLE001 - the connection dropped: the log has it
                pass

    async def open_room(self, spec: dict) -> None:
        self.rooms += 1
        rid = f"battle-{FMT}-{self.rooms}"
        self.agreed[rid] = set()
        self.rejected[rid] = False
        me = self.name
        head = [
            "|init|battle", f"|title|{me} vs. {FOE}", f"|j|☆{me}", f"|j|☆{FOE}", "|gametype|doubles",
            f"|player|p1|{me}|1|", f"|player|p2|{FOE}|2|", "|teampreview|4",
        ]
        if spec.get("offer", True):
            head.append(OFFER)
        if spec.get("foe") == "accept" and spec.get("delay", 0) is None:
            head.append(f"{FOE} has agreed to open team sheets.")
            self.agreed[rid].add(FOE)
        head.append(f"|request|{json.dumps(PREVIEW)}")
        self.log[rid] = []
        await self.add(rid, *head)
        delay = spec.get("delay", 0)
        if spec.get("foe") is not None and delay is not None:
            asyncio.get_running_loop().call_later(delay, lambda: asyncio.ensure_future(self.foe(rid, spec["foe"])))

    async def foe(self, rid: str, what: str) -> None:
        if what == "forfeit":
            await self.add(rid, f"|-message|{FOE} forfeited.", f"|win|{self.name}")
        else:
            await self.answer(rid, FOE, what == "accept")

    async def answer(self, rid: str, who: str, accept: bool) -> None:
        """``acceptopenteamsheets`` / ``rejectopenteamsheets``, as Showdown
        runs them (the errors left out)."""
        if accept:
            if self.rejected[rid]:
                await self.add(rid, "|error|An opponent has already rejected open team sheets.")
                return
            self.agreed[rid].add(who)
            await self.add(rid, f"{who} has agreed to open team sheets.")
            if len(self.agreed[rid]) == 2:
                await self.add(rid, "|showteam|p1|Eevee||||Tackle|||||50|", "|showteam|p2|Pikachu||||Thunderbolt|||||50|")
        else:
            self.rejected[rid] = True
            await self.add(rid, f"{who} rejected open team sheets.")

    def sent(self, what: str) -> List[Tuple[float, str]]:
        return [(t, m) for t, m in self.times if what in m]


async def play(fake: OtsFake, n: int = 1, **kw) -> Tuple[ShowdownClient, Dict[str, BattleRoom]]:
    """Plays the fake's rooms with a ``"mutual"`` client; returns it and
    the rooms (by id) as they ended."""
    kw.setdefault("open_team_sheets", "mutual")
    rooms: Dict[str, BattleRoom] = {}

    class Keep(RandomRequestHandler):
        def close(self, room):
            rooms[room.id] = room

    c = ShowdownClient("bot", server=fake.url, handler=Keep(0), **kw)
    await c.connect()
    try:
        await until(lambda: len(c.results) >= n, timeout=10)
    finally:
        await c.close()
    return c, rooms


def test_the_opponent_accepts_so_we_accept_and_the_preview_waits_for_the_sheets():
    async def main():
        async with OtsFake([{"foe": "accept", "delay": 0.3}]) as fake:
            _, rooms = await play(fake, ots_wait=5)
            return fake, rooms

    fake, rooms = run(main())
    [room] = rooms.values()
    [(ta, _)] = fake.sent("/acceptopenteamsheets")
    assert not fake.sent("/rejectopenteamsheets")
    [(tc, _)] = fake.sent("/choose ")
    assert 0.3 <= ta < tc, "accepted after the opponent; chose after that"
    assert (room.ots_offered, room.ots_foe, room.ots_answer, room.ots_open) == (True, True, True, True)
    assert 300 <= room.ots_wait_ms < 2000
    # The preview decision saw the sheets: chosen after the |showteam| lines.
    assert fake.log[room.id].index("|showteam|p2|Pikachu||||Thunderbolt|||||50|") < fake.log[room.id].index("|")


def test_the_opponent_rejects_so_nothing_is_sent_and_the_preview_goes():
    async def main():
        async with OtsFake([{"foe": "reject", "delay": 0.3}]) as fake:
            _, rooms = await play(fake, ots_wait=5)
            return fake, rooms

    fake, rooms = run(main())
    [room] = rooms.values()
    assert not fake.sent("openteamsheets"), "no accept, and no reject after theirs"
    [(tc, _)] = fake.sent("/choose ")
    assert 0.3 <= tc < 2
    assert (room.ots_foe, room.ots_answer, room.ots_open) == (False, None, False)
    assert 300 <= room.ots_wait_ms < 2000


def test_no_answer_rejects_when_the_wait_ends():
    async def main():
        async with OtsFake([{"foe": None}]) as fake:
            _, rooms = await play(fake, ots_wait=0.5)
            return fake, rooms

    fake, rooms = run(main())
    [room] = rooms.values()
    [(tr, _)] = fake.sent("/rejectopenteamsheets")
    assert not fake.sent("/acceptopenteamsheets")
    [(tc, _)] = fake.sent("/choose ")
    assert 0.5 <= tr <= tc < 2, "the preview decision was held until the reject"
    assert (room.ots_foe, room.ots_answer, room.ots_open) == (None, False, False)
    assert 500 <= room.ots_wait_ms < 2000


def test_an_acceptance_after_the_wait_comes_too_late():
    async def main():
        async with OtsFake([{"foe": "accept", "delay": 1.0}]) as fake:
            _, rooms = await play(fake, ots_wait=0.3)
            await asyncio.sleep(0)
            return fake, rooms

    fake, rooms = run(main())
    [room] = rooms.values()
    assert [m.split("|")[1] for _, m in fake.sent("openteamsheets")] == ["/rejectopenteamsheets"]
    assert not room.ots_open


def test_no_offer_means_no_wait():
    async def main():
        async with OtsFake([{"offer": False}]) as fake:
            _, rooms = await play(fake, ots_wait=5)
            return fake, rooms

    fake, rooms = run(main())
    [room] = rooms.values()
    assert not fake.sent("openteamsheets")
    [(tc, _)] = fake.sent("/choose ")
    assert tc < 0.3
    assert (room.ots_offered, room.ots_wait_ms) == (False, None)


def test_an_acceptance_before_the_request_is_answered_at_once():
    async def main():
        async with OtsFake([{"foe": "accept", "delay": None}]) as fake:
            _, rooms = await play(fake, ots_wait=5)
            return fake, rooms

    fake, rooms = run(main())
    [room] = rooms.values()
    [(ta, _)] = fake.sent("/acceptopenteamsheets")
    [(tc, _)] = fake.sent("/choose ")
    assert ta < tc < 0.5 and room.ots_open


def test_a_held_room_does_not_hold_the_others():
    async def main():
        async with OtsFake([{"foe": None}, {"offer": False}, {"foe": "accept", "delay": 0.2}]) as fake:
            c, rooms = await play(fake, n=3, ots_wait=0.8, max_concurrent=3)
            return fake, rooms

    fake, rooms = run(main())
    chosen = {m.split("|")[0]: t for t, m in fake.sent("/choose ")}
    ids = sorted(rooms, key=lambda r: int(r.rsplit("-", 1)[1]))
    assert chosen[ids[1]] < 0.2, "the room without an offer went at once"
    assert 0.2 <= chosen[ids[2]] < 0.8, "the accepted one went after the sheets"
    assert chosen[ids[0]] >= 0.8, "the unanswered one after its wait"
    assert [rooms[i].ots_open for i in ids] == [False, False, True]


def test_the_room_ending_during_the_wait_sends_nothing():
    async def main():
        async with OtsFake([{"foe": "forfeit", "delay": 0.2}]) as fake:
            c, rooms = await play(fake, ots_wait=0.5)
            await asyncio.sleep(0.6)  # the wait's end has passed
            return fake, c, rooms

    fake, c, rooms = run(main())
    [room] = rooms.values()
    assert not fake.sent("openteamsheets") and not fake.sent("/choose ")
    [r] = c.results
    assert r.won and r.finished
    assert 200 <= room.ots_wait_ms < 500


def test_a_reconnect_during_the_wait_keeps_the_wait():
    """The connection drops 0.2 s into a 2 s wait (the client reconnects
    after its 1 s backoff): the room rejoined (its log, the offer included,
    sent again) still waits until 2 s from the first offer, then rejects
    once."""

    async def main():
        async with OtsFake([{"foe": None}], drop_after=0.2) as fake:
            c, rooms = await play(fake, ots_wait=2.0)
            return fake, c, rooms

    fake, c, rooms = run(main())
    assert fake.logins == 2
    [(tj, _)] = fake.sent("/join ")
    [(tr, _)] = fake.sent("/rejectopenteamsheets")
    [(tc, _)] = fake.sent("/choose ")
    assert tj < 2.0 <= tr <= tc < 3.0
    [room] = rooms.values()
    assert 2000 <= room.ots_wait_ms < 3000
    assert len(c.results) == 1 and c.results[0].won


def test_a_reconnect_after_the_opponent_accepted_accepts_once():
    async def main():
        # The opponent accepts while the client is away (it reconnects after
        # its 1 s backoff): the rejoined room's log shows it.
        async with OtsFake([{"foe": "accept", "delay": 0.4}], drop_after=0.2) as fake:
            _, rooms = await play(fake, ots_wait=5)
            return fake, rooms

    fake, rooms = run(main())
    assert fake.logins == 2
    [(tj, _)] = fake.sent("/join ")
    [(ta, _)] = fake.sent("/acceptopenteamsheets")
    assert tj <= ta
    [room] = rooms.values()
    assert room.ots_open and room.ots_foe is True


@pytest.mark.parametrize("mode", [True, False, None])
def test_the_other_modes_answer_at_once(mode):
    async def main():
        async with OtsFake([{"foe": None}]) as fake:
            _, rooms = await play(fake, open_team_sheets=mode)
            return fake, rooms

    fake, rooms = run(main())
    answers = [m.split("|")[1] for _, m in fake.sent("openteamsheets")]
    assert answers == {True: ["/acceptopenteamsheets"], False: ["/rejectopenteamsheets"], None: []}[mode]
    [(tc, _)] = fake.sent("/choose ")
    assert tc < 0.3
    [room] = rooms.values()
    assert room.ots_wait_ms is None


def test_bad_settings_are_refused():
    with pytest.raises(ValueError):
        ShowdownClient("bot", open_team_sheets="sometimes")
    with pytest.raises(ValueError):
        ShowdownClient("bot", open_team_sheets="mutual", ots_wait=0)


def test_answers_are_read_by_name():
    room = BattleRoom(f"battle-{FMT}-1", "Bot")
    for line in ["|player|p1|Bot|1|", f"|player|p2|{FOE}|1|", OFFER,
                 "Someone Else has agreed to open team sheets.",  # not a player of the room
                 f"|c|{FOE}|Bot has agreed to open team sheets.",  # chat, not the server's line
                 "foebot rejected open team sheets.", "Bot has agreed to open team sheets."]:
        room.observe(line, "bot")
    assert (room.ots_offered, room.ots_foe, room.ots_answer) == (True, False, True)


def test_mutual_accepts_in_a_series():
    from test_showdown_series import FMT as BO3, SeriesFake

    async def main():
        async with SeriesFake(["me", "me"], otsrequest=True) as fake:
            async with ShowdownClient("bot", server=fake.url, open_team_sheets="mutual") as c:
                await c.ladder(1, BO3)
            return [m for m in fake.received if "openteamsheets" in m]

    accepted = run(main())
    assert len(accepted) == 2 and all(m.endswith("|/acceptopenteamsheets") for m in accepted)
