"""pikapro.showdown without a server: the protocol helpers, the random
request handler, SessionHandler's batching, the throttle and the login.

The client against real servers is ``test_showdown_servers.py``.
"""

from __future__ import annotations

import asyncio
import json
import random
import time
from dataclasses import replace

import numpy as np
import pytest

import pikapro
from pikapro.showdown import (
    LOCAL,
    BattleRoom,
    RandomRequestHandler,
    ServerConfig,
    SessionHandler,
    ShowdownClient,
    pack_team,
    parse_formats,
    random_choice,
    server_config,
    to_id,
)
from pikapro.showdown.client import _Outbox, _redact
from pikapro.showdown.protocol import battle_format, parse_challenge_pm, request_rqid, split_frame

FORMATS = ["gen9singles", "gen9doubles", "gen9vgc", "gen9ou", "championsvgc", "championsbss"]

# A trimmed copy of Showdown's |formats| line (2026-09-27): the
# hexadecimal flags, a section header, a name with a comma.
LIVE_FORMATS = (
    "|formats|,1|S/V Singles|[Gen 9] Random Battle,4f|[Gen 9] OU,e|,1|Champions|"
    "[Gen 9 Champions] VGC 2026 Reg M-C,5e|[Gen 9 Champions] VGC 2026 Reg M-B,5c|"
    "[Gen 9 Champions] Custom Game,c|,3|Randomized Metas|[Gen 9] Random Battle (Shared Power, B12P6),4f"
)


# -- protocol ---------------------------------------------------------------------------


def test_to_id_and_frames():
    assert to_id("[Gen 9 Champions] VGC 2026 Reg M-B") == "gen9championsvgc2026regmb"
    assert to_id(" Some Dude!") == "somedude"
    assert split_frame(">battle-gen9ou-1\n|init|battle\n\n|title|a vs. b") == (
        "battle-gen9ou-1",
        ["|init|battle", "|title|a vs. b"],
    )
    assert split_frame("|challstr|4|abc") == ("", ["|challstr|4|abc"])
    assert battle_format("battle-gen9randombattle-123") == "gen9randombattle"
    assert battle_format("battle-gen9ou-123-secretpassword") == "gen9ou"
    assert battle_format("lobby") == ""
    assert request_rqid('|request|{"wait":true,"rqid":17}') == 17
    assert request_rqid('|request|{"wait":true}') is None


def test_parse_formats_live_flags():
    formats = parse_formats(LIVE_FORMATS)
    mc = formats["gen9championsvgc2026regmc"]
    assert (mc.ladder, mc.challenge, mc.random_teams, mc.level_50, mc.best_of) == (True, True, False, True, True)
    mb = formats["gen9championsvgc2026regmb"]
    assert (mb.ladder, mb.challenge) == (False, True), "0x5c has no ladder bit"
    assert mb.section == "Champions" and mb.name == "[Gen 9 Champions] VGC 2026 Reg M-B"
    assert formats["gen9randombattle"].random_teams and formats["gen9randombattle"].ladder
    assert not formats["gen9championscustomgame"].ladder
    shared = formats["gen9randombattlesharedpowerb12p6"]
    assert shared.name == "[Gen 9] Random Battle (Shared Power, B12P6)" and shared.ladder
    assert formats["gen9ou"].section == "S/V Singles"


def test_parse_formats_local_prefix_and_legacy():
    formats = parse_formats("|formats|,LL|,1|pikapro|[Gen 9] Custom Game,e|[Gen 9] Random Battle,#|[Gen 9] Old,,")
    assert set(formats) == {"gen9customgame", "gen9randombattle", "gen9old"}
    assert formats["gen9randombattle"].random_teams
    assert formats["gen9old"].ladder and not formats["gen9old"].challenge


def test_challenge_pms():
    assert parse_challenge_pm("|pm| Alice| Bob|/challenge gen9ou|gen9ou|||") == ("alice", "bob", "gen9ou")
    assert parse_challenge_pm("|pm| Bob| Alice|/challenge") == ("bob", "alice", None)
    assert parse_challenge_pm("|pm| Alice| Bob|/log Alice wants to battle!") is None
    assert parse_challenge_pm("|popup|hi") is None


def test_pack_team():
    team = pikapro.sample_team("championsvgc")
    packed = pack_team(team, "gen9championsvgc2026regmc")
    assert "\n" not in packed and packed.count("]") == 5
    assert pack_team(packed, "gen9championsvgc2026regmc") == packed
    assert pack_team(None) == "null" and pack_team("") == "null"
    assert "\n" not in pack_team(pikapro.sample_team("gen9singles"), "gen9customgame")


def test_server_config():
    assert server_config("local") is LOCAL and server_config(None) is LOCAL
    assert server_config(8001).websocket_url == "ws://localhost:8001/showdown/websocket"
    c = server_config("example.org:8000")
    assert c.websocket_url == "ws://example.org:8000/showdown/websocket" and not c.token_login
    # The scheme is read in any case.
    assert server_config("WSS://example.org/showdown/websocket").websocket_url == "wss://example.org/showdown/websocket"
    assert server_config("Ws://example.org:8000/x").websocket_url == "ws://example.org:8000/x"
    assert server_config("WSS://vgc.bot/showdown/websocket").token_login


@pytest.mark.parametrize(
    "server",
    [
        "psim.us",
        "foo.psim.us",
        "wss://foo.psim.us/showdown/websocket",
        "FOO.PSIM.US:443",
        "pokemonshowdown.com",
        "wss://sim.pokemonshowdown.com/showdown/websocket",
        "pikapro:foo.psim.us",
        "foo.psim.us.",
        "Foo.Psim.Us.",
        "WSS://foo.psim.us/showdown/websocket",
        "wss://Sim3.PSIM.us./showdown/websocket",
        # Hosts that resolve as a refused one once IDNA-encoded (as websockets does).
        "ws://sim3.psim\u3002us/showdown/websocket",
        "sim3\uff0epsim\uff61us:8000",
        "wss://sim3.\uff50\uff53\uff49\uff4d.us/showdown/websocket",
        "wss://play.pokemonshowdown\u3002com/showdown/websocket",
        # Not host names at all.
        "ws://foo..psim.us/showdown/websocket",
        "ws://" + "a" * 64 + ".example/showdown/websocket",
    ],
)
def test_refused_hosts(server):
    with pytest.raises(ValueError, match="doesn't connect to"):
        server_config(server)


def test_server_config_refuses_other_schemes():
    for url in ("http://example.org", "https://example.org/showdown/websocket", "ftp://localhost:8000"):
        with pytest.raises(ValueError, match="starts with ws:// or wss://"):
            server_config(url)
    with pytest.raises(ValueError, match="starts with ws:// or wss://"):
        server_config("pikapro:http://localhost:8000")


def test_refused_hosts_in_a_config():
    with pytest.raises(ValueError, match="doesn't connect to"):
        ServerConfig(websocket_url="wss://foo.psim.us/showdown/websocket")
    with pytest.raises(ValueError, match="doesn't connect to"):
        replace(LOCAL, websocket_url="wss://pokemonshowdown.com/showdown/websocket")
    # Not a suffix match on the name alone.
    assert server_config("notpsim.us:8000").websocket_url.startswith("ws://notpsim.us")


def test_login_timeout_is_positive():
    with pytest.raises(ValueError, match="login_timeout"):
        ShowdownClient("bot", login_timeout=0)


def test_redact_hides_the_assertion():
    assert _redact("|/trn bot,0,SECRET;sig") == "|/trn bot,0,<assertion>"
    assert _redact("battle-x|/choose move 1|3") == "battle-x|/choose move 1|3"


# -- the random request handler ---------------------------------------------------------------


@pytest.mark.parametrize("format_id", FORMATS)
def test_random_choice_is_always_valid(format_id):
    """Every choice random_choice makes from pikapro's Showdown requests is
    accepted, except the switches a hidden trap makes unavailable (as on a
    server)."""
    rng = random.Random(5200001)
    errors = []
    for seed in range(5200000, 5200015):
        battle = pikapro.Battle(format_id, pikapro.RandomTeams(format_id, "sets").pair(seed), seed=seed)
        for _ in range(2000):
            if battle.over:
                break
            for side in (0, 1):
                if battle.request_kind(side) == "wait":
                    continue
                choice = random_choice(json.loads(battle.request_json(side)), rng)
                try:
                    battle.submit(side, choice)
                except ValueError as e:
                    if not str(e).startswith("[Unavailable choice]"):
                        errors.append((seed, choice, str(e)))
        assert battle.over
    assert not errors


def test_random_choice_shapes():
    rng = random.Random(0)
    assert random_choice({"wait": True, "side": {"pokemon": []}}, rng) is None
    side = {"pokemon": [{"condition": "100/100", "active": i < 2} for i in range(6)]}
    team = random_choice({"teamPreview": True, "maxChosenTeamSize": 4, "side": side}, rng)
    assert team.startswith("team ") and len(team) == 9 and len(set(team[5:])) == 4
    forced = random_choice({"forceSwitch": [True, False], "side": side}, rng)
    assert forced.startswith("switch ") and forced.endswith(", pass")
    assert int(forced.split(",")[0].split()[1]) > 2


# -- SessionHandler -----------------------------------------------------------------------------


from _showdown_fakes import FakeSession  # noqa: E402


def _room(i, request):
    room = BattleRoom(f"battle-gen9customgame-{i}", "me")
    room.observe("|request|" + json.dumps(request), "me")
    return room


def test_session_handler_batches_and_unstacks():
    request = {"active": [{"moves": [{"move": "Tackle", "target": "normal"}]}], "side": {"pokemon": [{"condition": "1/1", "active": True}]}}
    seen = []

    def policy(obs, mask):
        seen.append((obs["n"].shape, obs["x"].shape, mask.shape))
        return np.arange(len(mask)) % 4

    handler = SessionHandler(policy, batch=True, session_factory=FakeSession)
    rooms = [_room(i, request) for i in range(3)]
    for r in rooms:
        handler.open(r)
        assert handler.feed(r, r.lines)
        assert r.state.username == "me" and r.state.options == ("basic", "factored", "row")
    choices = asyncio.run(handler.choose(rooms))
    assert seen == [((3, 1), (3, 2, 3), (3, 4))]
    assert choices == ["move 1"] * 3
    assert handler.decisions == 3


def test_session_handler_single_and_builtin():
    request = {"active": [{"moves": [{"move": "Tackle", "target": "normal"}]}], "side": {"pokemon": [{"condition": "1/1", "active": True}]}}
    calls = []
    single = SessionHandler(lambda o, m: calls.append(1) or 0, session_factory=FakeSession, threaded=False)
    builtin = SessionHandler("simple_heuristics", session_factory=FakeSession, seed=5200000)
    for handler in (single, builtin):
        rooms = [_room(i, request) for i in range(2)]
        for r in rooms:
            handler.open(r)
            handler.feed(r, r.lines)
        assert asyncio.run(handler.choose(rooms)) == ["move 1", "move 1"]
    assert calls == [1, 1]
    with pytest.raises(ValueError):
        SessionHandler("alphazero")


# -- the throttle, the start limit and the login ---------------------------------------------------


class FakeWebSocket:
    def __init__(self):
        self.sent = []

    async def send(self, message):
        self.sent.append((time.monotonic(), message))

    async def close(self):
        pass


def test_outbox_spaces_messages_and_sends_choices_first():
    async def run():
        client = ShowdownClient("tester", server=LOCAL)
        client._outbox = _Outbox(0.05)
        client._connected = asyncio.Event()
        client._connected.set()
        client._logged_in = asyncio.Event()
        client._logged_in.set()
        ws = client._ws = FakeWebSocket()
        client._send(["|/utm null", "|/search gen9randombattle"])
        client._send(["|/avatar 1"])
        client._send(["battle-x|/choose move 1|2"], urgent=True)
        task = asyncio.create_task(client._send_loop())
        await asyncio.wait_for(client._outbox.idle.wait(), 5)
        await asyncio.sleep(0)
        task.cancel()
        return ws.sent

    sent = asyncio.run(run())
    order = [m for _, m in sent]
    assert order[0] == "battle-x|/choose move 1|2"
    assert order[1:] == ["|/utm null", "|/search gen9randombattle", "|/avatar 1"]
    gaps = [b - a for (a, _), (b, _) in zip(sent, sent[1:])]
    assert min(gaps) >= 0.045


def test_names_on_a_local_server():
    anon = ShowdownClient(server="local")
    assert anon.username.startswith("pikapro-") and len(anon.username) <= 18
    assert ShowdownClient(server="local", guest=True).username is None
    assert ShowdownClient("x", server="local")._assertion() == ""


def test_concurrency_is_capped_by_the_server():
    assert ShowdownClient("x", "token", server="arena", max_concurrent=50).max_concurrent == 5
    assert ShowdownClient("x", server="local", max_concurrent=50).max_concurrent == 50


def test_random_request_handler_waits_on_wait_requests():
    handler = RandomRequestHandler(0)
    room = _room(1, {"wait": True, "side": {"pokemon": []}})
    assert not handler.feed(room, room.lines)


def test_loopback_is_an_address_not_a_prefix():
    from pikapro.showdown.client import _local

    for host in ("localhost", "LOCALHOST.", "a.localhost", "127.0.0.1", "127.8.9.10", "::1", "[::1]"):
        assert _local(host), host
    # A name that only starts like a loopback address is another machine.
    for host in ("127.evil.example", "127.0.0.1.example.com", "localhost.example.com", "128.0.0.1", "", "::2"):
        assert not _local(host), host
