"""``accept_challenges``' ``plan`` hook (``AcceptPlan``): per challenge, its
own team and its own answer to Open Team Sheets, against a scripted server
that posts challenges, opens a room for each ``/accept`` (with the sheets
offered at team preview, as Showdown's "Open Team Sheets" rule does) and
can refuse a team as Showdown's validator does (a ``|popup|``)."""

from __future__ import annotations

import asyncio
import json
import logging
import time
from typing import Dict, List, Optional, Tuple

import pytest

pytest.importorskip("websockets")

import pikapro  # noqa: E402
from pikapro.showdown import AcceptPlan, BattleRoom, RandomRequestHandler, ShowdownClient, pack_team  # noqa: E402

from test_showdown_ots import FMT, OFFER, PREVIEW  # noqa: E402
from _showdown_fakes import Fake, run, until  # noqa: E402

TEAM = pikapro.random_team("championsvgc", seed=1)
OURS = pikapro.random_team("championsvgc", seed=2)
assert TEAM != OURS


class ChallengeFake(Fake):
    """Posts a challenge from each of ``challengers`` at login. Each
    ``/accept USER`` ends that challenge and opens a doubles room against
    USER with the sheets offered; USER answers the offer ``answers[USER]``
    (``"accept"``, ``"reject"`` or ``None``) ``delay`` seconds later. A
    ``/utm`` whose team contains ``refuse`` is refused at the next
    ``/accept`` with Showdown's popup (the challenge stays). A choice ends
    the battle."""

    def __init__(self, challengers: List[str], answers: Optional[Dict[str, Optional[str]]] = None,
                 delay: float = 0.2, refuse: Optional[str] = None):
        super().__init__()
        self.challengers = challengers
        self.answers = answers or {}
        self.delay = delay
        self.refuse = refuse
        self.ws = None
        self.name = ""
        self.t0 = 0.0
        self.utm = ""
        self.accepted: List[Tuple[str, str]] = []  # (user, the team uploaded for it)
        self.log: Dict[str, List[str]] = {}
        self.foes: Dict[str, str] = {}
        self.agreed: Dict[str, set] = {}
        self.times: List[Tuple[float, str]] = []

    async def handler(self, ws):
        self.sockets.append(ws)
        await ws.send("|challstr|4|abcdef")
        async for msg in ws:
            self.received.append(msg)
            self.times.append((time.monotonic() - self.t0, msg))
            room, _, text = msg.partition("|")
            if text.startswith("/trn "):
                self.name = text[5:].split(",", 1)[0]
                self.ws = ws
                self.t0 = time.monotonic()
                await ws.send(f"|updateuser| {self.name}|1|1|{{}}")
                for user in self.challengers:
                    await ws.send(f"|pm|{user}|{self.name}|/challenge {FMT}|{FMT}|||")
            elif text.startswith("/utm "):
                self.utm = text[len("/utm "):]
            elif text.startswith("/accept "):
                user = text[len("/accept "):]
                if self.refuse is not None and self.refuse in self.utm:
                    await ws.send("|popup|Your team was rejected for the following reasons:||- Bad set.")
                    continue
                self.accepted.append((user, self.utm))
                await ws.send(f"|pm|{user}|{self.name}|/challenge")
                await self.open_room(user)
            elif text in ("/acceptopenteamsheets", "/rejectopenteamsheets"):
                await self.answer(room, self.name, text == "/acceptopenteamsheets")
            elif text.startswith("/choose "):
                await self.add(room, "|", f"|win|{self.name}")

    async def add(self, rid: str, *lines: str) -> None:
        self.log[rid].extend(lines)
        await self.ws.send(f">{rid}\n" + "\n".join(lines))

    async def open_room(self, user: str) -> None:
        self.rooms += 1
        rid = f"battle-{FMT}-{self.rooms}"
        self.log[rid], self.foes[rid], self.agreed[rid] = [], user, set()
        me = self.name
        await self.add(rid, "|init|battle", f"|title|{me} vs. {user}", "|gametype|doubles", f"|player|p1|{me}|1|",
                       f"|player|p2|{user}|2|", "|teampreview|4", OFFER, f"|request|{json.dumps(PREVIEW)}")
        what = self.answers.get(user)
        if what is not None:
            asyncio.get_running_loop().call_later(
                self.delay, lambda: asyncio.ensure_future(self.answer(rid, user, what == "accept")))

    async def answer(self, rid: str, who: str, accept: bool) -> None:
        if accept:
            self.agreed[rid].add(who)
            await self.add(rid, f"{who} has agreed to open team sheets.")
            if len(self.agreed[rid]) == 2:
                await self.add(rid, "|showteam|p1|Eevee||||Tackle|||||50|", "|showteam|p2|Pikachu||||Thunderbolt|||||50|")
        else:
            await self.add(rid, f"{who} rejected open team sheets.")

    def sent(self, room: str, what: str) -> List[float]:
        return [t for t, m in self.times if m.startswith(room + "|") and what in m]


async def accept(fake: ChallengeFake, n: int, plan, team, **kw) -> Dict[str, BattleRoom]:
    rooms: Dict[str, BattleRoom] = {}

    class Keep(RandomRequestHandler):
        def close(self, room):
            rooms[room.opponent] = room

    async with ShowdownClient("bot", server=fake.url, handler=Keep(0), max_concurrent=2, **kw) as c:
        await asyncio.wait_for(c.accept_challenges(n=n, team=team, format=FMT, plan=plan), 10)
    return rooms


def test_a_plan_plays_its_own_team_and_accepts_the_sheets_at_once():
    """alice's plan: her team, sheets accepted at once (the client itself
    rejects them), and the preview decision held until she answered;
    bob: the operation's team (asked once, for him alone) and the client's
    answer."""
    asked: List[str] = []

    def plan(user, fmt):
        assert fmt == FMT
        return AcceptPlan(team=TEAM, open_team_sheets=True) if user == "alice" else None

    def team(fmt):
        asked.append(fmt)
        return OURS

    async def main():
        async with ChallengeFake(["alice", "bob"], {"alice": "accept", "bob": "accept"}, delay=0.4) as fake:
            rooms = await accept(fake, 2, plan, team, open_team_sheets=False, ots_wait=5)
            return fake, rooms

    fake, rooms = run(main())
    assert dict(fake.accepted) == {"alice": pack_team(TEAM, FMT), "bob": pack_team(OURS, FMT)}
    assert asked == [FMT]
    a, b = rooms["alice"], rooms["bob"]
    assert a.team == pack_team(TEAM, FMT) and b.team == pack_team(OURS, FMT)
    [ta] = fake.sent(a.id, "/acceptopenteamsheets")
    [tc] = fake.sent(a.id, "/choose ")
    assert ta < 0.4 <= tc, "accepted before her answer; chose after it"
    assert (a.ots_answer, a.ots_foe, a.ots_open) == (True, True, True)
    # The preview decision saw the sheets.
    assert fake.log[a.id].index("|showteam|p2|Pikachu||||Thunderbolt|||||50|") < fake.log[a.id].index("|")
    assert fake.sent(b.id, "/rejectopenteamsheets") and not fake.sent(b.id, "/acceptopenteamsheets")
    assert not b.ots_open


@pytest.mark.parametrize("answer", ["reject", None])
def test_a_plan_whose_opponent_rejects_or_says_nothing_plays_on(answer):
    """Showdown opens the sheets only when both accept: after her
    rejection the decision goes at once; with no answer, after
    ``ots_wait``."""

    async def main():
        async with ChallengeFake(["alice"], {"alice": answer}, delay=0.2) as fake:
            rooms = await accept(fake, 1, lambda u, f: AcceptPlan(team=TEAM, open_team_sheets=True), OURS,
                                 open_team_sheets="mutual", ots_wait=0.6)
            return fake, rooms

    fake, rooms = run(main())
    a = rooms["alice"]
    [ta] = fake.sent(a.id, "/acceptopenteamsheets")
    [tc] = fake.sent(a.id, "/choose ")
    assert not fake.sent(a.id, "/rejectopenteamsheets")
    assert not a.ots_open and a.ots_answer is True
    if answer == "reject":
        assert ta < 0.2 <= tc < 0.6 and a.ots_foe is False
    else:
        assert 0.6 <= tc < 2 and a.ots_foe is None


def test_a_refused_plan_team_is_reported_and_the_challenge_accepted_again():
    """The server refuses the plan's team: ``failed`` hears why, and the
    challenge (still standing) is accepted again with the next plan (none:
    the operation's team)."""
    reasons: List[str] = []
    plans = [AcceptPlan(team=TEAM, open_team_sheets=True, failed=reasons.append)]

    def plan(user, fmt):
        return plans.pop() if plans else None

    async def main():
        packed = pack_team(TEAM, FMT)
        async with ChallengeFake(["alice"], refuse=packed) as fake:
            rooms = await accept(fake, 1, plan, OURS, open_team_sheets=False)
            return fake, rooms

    fake, rooms = run(main())
    assert len(reasons) == 1 and "team was rejected" in reasons[0]
    assert fake.accepted == [("alice", pack_team(OURS, FMT))]
    a = rooms["alice"]
    assert a.team == pack_team(OURS, FMT)
    assert fake.sent(a.id, "/rejectopenteamsheets"), "the client's own answer again"


def test_each_refused_plan_team_is_retried_not_only_the_first():
    """A user's plan team is refused, the challenge is accepted again with
    the operation's team; later their next challenge's plan team is refused
    too, and that challenge is accepted again as well (the retry guard
    lasts one attempt, not the whole operation)."""
    reasons: List[str] = []
    plans = [AcceptPlan(team=TEAM, failed=reasons.append), None, AcceptPlan(team=TEAM, failed=reasons.append), None]

    def plan(user, fmt):
        return plans.pop(0) if plans else None

    async def main():
        async with ChallengeFake(["alice"], refuse=pack_team(TEAM, FMT)) as fake:
            async with ShowdownClient("bot", server=fake.url, handler=RandomRequestHandler(0)) as c:
                task = asyncio.ensure_future(c.accept_challenges(n=2, team=OURS, format=FMT, plan=plan))
                await until(lambda: len(fake.accepted) == 1 and len(c.results) == 1)
                await fake.ws.send(f"|pm|alice|{fake.name}|/challenge {FMT}|{FMT}|||")  # her next challenge
                await asyncio.wait_for(task, 10)
            return fake

    fake = run(main())
    assert len(reasons) == 2 and all("team was rejected" in r for r in reasons)
    assert fake.accepted == [("alice", pack_team(OURS, FMT))] * 2


def test_a_plan_that_raises_accepts_as_usual(caplog):
    def plan(user, fmt):
        raise RuntimeError("bug")

    async def main():
        async with ChallengeFake(["alice"]) as fake:
            await accept(fake, 1, plan, OURS)
            return fake

    fake = run(main())
    assert fake.accepted == [("alice", pack_team(OURS, FMT))]


class Panic(BaseException):
    """What a Rust panic is in Python (PyO3's PanicException): a
    BaseException, which ``except Exception`` lets through."""


def test_a_team_that_fails_skips_that_challenge_only(caplog):
    """The operation's team fails for one challenge (a rotation's
    bug, a panic packing it): that challenge isn't accepted, and the
    operation goes on to the next one instead of ending."""
    caplog.set_level(logging.ERROR, logger="pikapro.showdown")
    calls: List[str] = []

    def team(fmt):
        calls.append(fmt)
        if len(calls) == 1:
            raise Panic("pack panicked")
        return OURS

    async def main():
        async with ChallengeFake(["alice", "bob"]) as fake:
            rooms = await accept(fake, 1, None, team)
            return fake, rooms

    fake, rooms = run(main())
    assert fake.accepted == [("bob", pack_team(OURS, FMT))]
    assert list(rooms) == ["bob"]
    refused = [r for r in caplog.records if getattr(r, "event", None) == "challenge not accepted"]
    assert refused and refused[0].fields["by"] == "alice" and "Panic" in refused[0].fields["reason"]


def test_a_plan_team_that_fails_is_reported_and_the_challenge_accepted_again():
    """A user's team that can't be packed (it raises, a panic included):
    the plan's ``failed`` hears why, and the challenge is accepted again
    with the next plan (none: the operation's team)."""
    reasons: List[str] = []

    def bad(fmt):
        raise Panic("pack panicked")

    plans = [AcceptPlan(team=bad, failed=reasons.append)]  # type: ignore[arg-type]

    async def main():
        async with ChallengeFake(["alice"]) as fake:
            rooms = await accept(fake, 1, lambda u, f: plans.pop() if plans else None, OURS)
            return fake, rooms

    fake, rooms = run(main())
    assert len(reasons) == 1 and "Panic" in reasons[0]
    assert fake.accepted == [("alice", pack_team(OURS, FMT))]
