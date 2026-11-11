"""pikapro.showdown against real servers, all local: pikapro-showdown-server
(built from this workspace) by default, and Showdown's own server (the npm
package the conformance oracle installs) with ``--real-showdown`` or
``PIKAPRO_REAL_SHOWDOWN=1``.

Two clients play each other by challenge and on the ladder in every
format. ``PIKAPRO_SHOWDOWN_GAMES`` sets the games per run (default 6); the
long runs (hundreds of games) use it.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path
from typing import List

import pytest

pytest.importorskip("websockets")

import pikapro  # noqa: E402
from pikapro.showdown import (  # noqa: E402
    BattleHandler,
    BattleResult,
    ChallengeError,
    LoginError,
    RandomRequestHandler,
    SeriesResult,
    ServerConfig,
    SessionHandler,
    ShowdownClient,
    ShowdownError,
    pikapro_server,
)

from _showdown_fakes import FakeSession  # noqa: E402
from conftest import TARGET  # noqa: E402

ROOT = Path(__file__).resolve().parents[3]
TEAMS = ROOT / "crates" / "pikapro-showdown-server" / "tests" / "python" / "teams"
GAMES = int(os.environ.get("PIKAPRO_SHOWDOWN_GAMES", "6"))
#: Best-of series per run of each series test on Showdown's server.
SERIES = int(os.environ.get("PIKAPRO_SHOWDOWN_SERIES", "4"))
SEED = 5200000

# pikapro-showdown-server's formats, and the pikapro preset each plays.
OUR_FORMATS = {
    "gen9championsvgc2026regmc": "championsvgc",
    "gen9championsbssregmc": "championsbss",
    "gen9vgc2025regi": "gen9vgc",
    "gen9customgame": "gen9singles",
    "gen9doublescustomgame": "gen9doubles",
    "gen9ou": "gen9ou",
}


def team_for(fmt: str) -> str:
    path = TEAMS / f"{fmt}.txt"
    if path.exists():
        return path.read_text()
    return pikapro.sample_team(OUR_FORMATS[fmt])


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for_port(port: int, proc: subprocess.Popen, timeout: float) -> None:
    start = time.monotonic()
    while True:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                return
        except OSError:
            if proc.poll() is not None:
                pytest.fail(f"the server exited ({proc.returncode})")
            if time.monotonic() - start > timeout:
                proc.kill()
                pytest.fail("the server did not start")
            time.sleep(0.1)


def wait_for_login(proc: subprocess.Popen, log: Path, name: str, timeout: float) -> None:
    """Waits until ``proc`` (``python -m pikapro.showdown ... --log-json``,
    stderr to ``log``) has logged in as ``name``."""
    start = time.monotonic()
    while True:
        for line in log.read_text().splitlines():
            try:
                record = json.loads(line)
            except ValueError:
                continue
            if record.get("event") == "user" and record.get("named") and record.get("name") == name:
                return
        if proc.poll() is not None:
            pytest.fail(f"{name} exited ({proc.returncode}) before logging in: {log.read_text()[-2000:]}")
        if time.monotonic() - start > timeout:
            proc.kill()
            pytest.fail(f"{name} did not log in in {timeout} s")
        time.sleep(0.1)


def server_binary() -> str:
    path = os.environ.get("PIKAPRO_SHOWDOWN_SERVER")
    if path:
        return path
    for profile in ("release-server", "release", "debug"):
        exe = TARGET / profile / "pikapro-showdown-server"
        if exe.exists():
            return str(exe)
    if shutil.which("cargo") is None:
        pytest.skip("no pikapro-showdown-server binary and no cargo to build it")
    subprocess.run(["cargo", "build", "-q", "-p", "pikapro-showdown-server"], cwd=ROOT, check=True)
    return str(TARGET / "debug" / "pikapro-showdown-server")


@pytest.fixture(scope="module")
def ours():
    """pikapro-showdown-server on a free port: its websocket URL."""
    port = free_port()
    proc = subprocess.Popen(
        [server_binary(), "--port", str(port), "--seed", str(SEED), "--disconnect-grace", "30"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    wait_for_port(port, proc, 30)
    yield f"ws://127.0.0.1:{port}/showdown/websocket"
    proc.kill()
    proc.wait()


def start_showdown(tmp_path_factory, request, extra_config: str = "", security: bool = False):
    """Showdown's own server, from a copy of the oracle's npm package
    (never the installed one): started with ``--no-security``, or with
    ``security`` with only ``noguestsecurity`` (names log in without a
    login server; Showdown's limits apply: games at once,
    starts per IP, message throttle, challenge cooldown). ``extra_config``
    is appended to its ``config.js``. A generator for a fixture: yields
    the websocket URL."""
    if not (request.config.getoption("--real-showdown") or os.environ.get("PIKAPRO_REAL_SHOWDOWN")):
        pytest.skip("real Showdown server tests are off (--real-showdown or PIKAPRO_REAL_SHOWDOWN=1)")
    modules = Path(os.environ.get("PIKAPRO_SHOWDOWN_NODE_MODULES", ROOT / "tools" / "showdown-oracle" / "node_modules"))
    package = modules / "pokemon-showdown"
    if not (package / "dist").is_dir() or shutil.which("node") is None:
        pytest.skip("no Showdown package (cd tools/showdown-oracle && npm ci) or no node")
    base = tmp_path_factory.mktemp("sd") / "node_modules"
    base.mkdir()
    for entry in modules.iterdir():  # the package's dependencies, by link
        if entry.name != "pokemon-showdown":
            (base / entry.name).symlink_to(entry.resolve())
    copy = base / "pokemon-showdown"
    shutil.copytree(package, copy, symlinks=True)
    config = (copy / "config" / "config-example.js").read_text() + "\nexports.repl = false;\n"
    if security:
        # No login server is ever asked (an unreachable one, to be sure).
        config += "exports.noguestsecurity = true;\nexports.loginserver = 'http://127.0.0.1:9/';\n"
    (copy / "config" / "config.js").write_text(config + extra_config)
    for d in ("config/chat-plugins", "config/ladders", "logs/repl", "logs/chat", "logs/modlog"):
        (copy / d).mkdir(parents=True, exist_ok=True)
    port = free_port()
    log = open(copy / "server.log", "w")
    flags = [] if security else ["--no-security"]
    proc = subprocess.Popen(
        ["node", "pokemon-showdown", "start", "--skip-build", *flags, str(port)],
        cwd=copy,
        stdout=log,
        stderr=subprocess.STDOUT,
    )
    wait_for_port(port, proc, 120)
    yield f"ws://127.0.0.1:{port}/showdown/websocket"
    proc.terminate()
    try:
        proc.wait(10)
    except subprocess.TimeoutExpired:
        proc.kill()
    log.close()


@pytest.fixture(scope="module")
def showdown(tmp_path_factory, request):
    """Showdown's own server, started with ``--no-security``."""
    yield from start_showdown(tmp_path_factory, request)


@pytest.fixture(scope="module")
def showdown_limits(tmp_path_factory, request):
    """Showdown's own server with its limits on
    (``noguestsecurity`` only: no ``--no-security``)."""
    yield from start_showdown(tmp_path_factory, request, security=True)


#: Showdown's ``/prelockdown`` (before a restart), from the start: the
#: server keeps running games but refuses to start a best-of series.
PRELOCKDOWN = """
setInterval(() => {
    try {
        if (global.Rooms && Rooms.global && Rooms.global.lockdown !== 'pre') Rooms.global.lockdown = 'pre';
    } catch (e) {}
}, 100);
"""


@pytest.fixture(scope="module")
def showdown_prelockdown(tmp_path_factory, request):
    """Showdown's own server (``--no-security``) in ``/prelockdown``."""
    yield from start_showdown(tmp_path_factory, request, extra_config=PRELOCKDOWN)


_names = iter(range(10**6))


def pair(url: str, **kwargs):
    n = next(_names)
    a = ShowdownClient(f"pikapro-a{n}", server=url, handler=RandomRequestHandler(SEED + 2 * n), **kwargs)
    b = ShowdownClient(f"pikapro-b{n}", server=url, handler=RandomRequestHandler(SEED + 2 * n + 1), **kwargs)
    return a, b


def check_pair(ra: List[BattleResult], rb: List[BattleResult], n: int) -> None:
    assert len(ra) == len(rb) == n
    assert len({r.room for r in ra}) == n
    by_room = {r.room: r for r in rb}
    for r in ra:
        other = by_room[r.room]
        assert r.finished and other.finished, (r, other)
        assert r.winner == other.winner and r.tie == other.tie
        if not r.tie:
            assert r.won is not other.won
        assert {r.side, other.side} == {"p1", "p2"}
        assert r.invalid == other.invalid == 0, "the random handler only makes valid choices"
        assert r.fallbacks == other.fallbacks == 0


async def play(url: str, fmt: str, team, mode: str, n: int, concurrent: int = 3, **kwargs):
    a, b = pair(url, max_concurrent=concurrent, **kwargs)
    await asyncio.gather(a.connect(), b.connect())
    try:
        if mode == "challenge":
            ra, rb = await asyncio.wait_for(
                asyncio.gather(a.challenge(b.username, fmt, team, n=n), b.accept_challenges(a.username, n, team)),
                600,
            )
        else:
            ra, rb = await asyncio.wait_for(asyncio.gather(a.ladder(n, fmt, team), b.ladder(n, fmt, team)), 600)
    finally:
        await asyncio.gather(a.close(), b.close())
    return a, b, ra, rb


# -- pikapro-showdown-server ------------------------------------------------------------------


@pytest.mark.parametrize("mode", ["challenge", "ladder"])
@pytest.mark.parametrize("fmt", list(OUR_FORMATS))
def test_ours_games_in_every_format(ours, fmt, mode):
    a, b, ra, rb = asyncio.run(play(ours, fmt, team_for(fmt), mode, GAMES))
    check_pair(ra, rb, GAMES)
    assert all(r.format == fmt for r in ra)
    assert {r.opponent for r in ra} == {b.name} and {r.opponent for r in rb} == {a.name}
    assert not a.rooms and not b.rooms


def test_ours_session_handler_batches(ours):
    """SessionHandler (on a stand-in session) plays a ladder series, and a
    batched policy sees several battles in one call."""
    import numpy as np

    sizes = []

    def policy(obs, mask):
        sizes.append(len(mask))
        time.sleep(0.01)  # a model thinking: the other battles' requests pile up
        return np.zeros(len(mask), dtype=np.int64)

    async def run():
        fmt = "gen9championsvgc2026regmc"
        a = ShowdownClient("pikapro-batch-a", server=ours, max_concurrent=4, handler=SessionHandler(policy, batch=True, session_factory=FakeSession))
        b = ShowdownClient("pikapro-batch-b", server=ours, max_concurrent=4, handler=SessionHandler("random", session_factory=FakeSession))
        await asyncio.gather(a.connect(), b.connect())
        try:
            return await asyncio.wait_for(
                asyncio.gather(a.challenge(b.username, fmt, team_for(fmt), n=8), b.accept_challenges(a.username, 8, team_for(fmt))),
                300,
            )
        finally:
            await asyncio.gather(a.close(), b.close())

    ra, rb = asyncio.run(run())
    check_pair(ra, rb, 8)
    assert max(sizes) > 1, sizes
    assert all("unknown_lines" in r.extras for r in ra)


def test_ours_logs_are_saved(ours, tmp_path):
    fmt = "gen9customgame"
    a, b, ra, rb = asyncio.run(play(ours, fmt, team_for(fmt), "challenge", 2, log_dir=tmp_path))
    for r in ra + rb:
        text = Path(r.log_path).read_text()
        assert text.startswith("|init|battle\n") and f"|win|{r.winner}" in text and "|request|" in text


class Stubborn(BattleHandler):
    """Always chooses a move that doesn't exist."""

    def __init__(self):
        self.errors = []

    def feed(self, room, lines):
        return room.request is not None and not room.request.get("wait")

    def error(self, room, message):
        self.errors.append(message)

    async def choose(self, rooms):
        return ["move 9" if not r.request.get("teamPreview") else "team 1234" for r in rooms]


def test_ours_invalid_choices_reach_the_handler_then_default(ours):
    async def run():
        handler = Stubborn()
        a, b = pair(ours, max_retries=2)
        a.handler = handler
        await asyncio.gather(a.connect(), b.connect())
        fmt = "gen9customgame"
        try:
            ra, rb = await asyncio.wait_for(
                asyncio.gather(a.challenge(b.username, fmt, team_for(fmt)), b.accept_challenges(a.username, 1, team_for(fmt))),
                120,
            )
        finally:
            await asyncio.gather(a.close(), b.close())
        return handler, ra[0]

    handler, result = asyncio.run(run())
    assert result.finished
    assert result.invalid >= 3 and result.fallbacks >= 1
    assert all(e.startswith("[Invalid choice]") for e in handler.errors)


class Silent(BattleHandler):
    """Never chooses."""

    def feed(self, room, lines):
        return False

    async def choose(self, rooms):  # pragma: no cover
        return []


def test_ours_close_forfeits_unfinished_battles(ours):
    async def run():
        a, b = pair(ours)
        a.handler = Silent()
        await asyncio.gather(a.connect(), b.connect())
        fmt = "gen9customgame"
        game = asyncio.ensure_future(b.accept_challenges(a.username, 1, team_for(fmt)))
        challenge = asyncio.ensure_future(a.challenge(b.username, fmt, team_for(fmt)))
        while not a.rooms:
            await asyncio.sleep(0.01)
        await asyncio.sleep(0.2)
        await a.close()
        rb = await asyncio.wait_for(game, 30)
        ra = await challenge
        await b.close()
        return ra[0], rb[0]

    ra, rb = asyncio.run(run())
    assert rb.won is True and rb.finished
    assert ra.won is False or ra.reason == "forfeited on exit"


def test_ours_reconnect_rejoins_the_battle(ours):
    """The connection drops mid-battle: the client reconnects, logs in
    again, rejoins the room, and the battle finishes."""

    class Dropper(RandomRequestHandler):
        def __init__(self, client_box):
            super().__init__(SEED)
            self.box = client_box
            self.dropped = False
            self.opened = 0

        def open(self, room):
            self.opened += 1

        def feed(self, room, lines):
            if not self.dropped and room.turn >= 2:
                self.dropped = True
                asyncio.get_running_loop().create_task(self.box[0]._ws.close())
            return super().feed(room, lines)

    async def run():
        box = []
        a, b = pair(ours)
        dropper = Dropper(box)
        a.handler = dropper
        box.append(a)
        await asyncio.gather(a.connect(), b.connect())
        fmt = "gen9customgame"
        try:
            ra, rb = await asyncio.wait_for(
                asyncio.gather(a.challenge(b.username, fmt, team_for(fmt)), b.accept_challenges(a.username, 1, team_for(fmt))),
                120,
            )
        finally:
            await asyncio.gather(a.close(), b.close())
        return dropper, a, ra[0], rb[0]

    dropper, a, ra, rb = asyncio.run(run())
    assert dropper.dropped and dropper.opened == 2, "the room opened again after the rejoin"
    assert a._generation == 2
    assert ra.finished and rb.finished and ra.winner == rb.winner


def test_ours_errors(ours):
    async def run():
        a, b = pair(ours)
        await asyncio.gather(a.connect(), b.connect())
        try:
            with pytest.raises(ShowdownError, match="no format"):
                await a.ladder(1, "gen9nosuchformat", None)
            # No best-of series here: said up front, as the server lists no
            # series format (with the server's address).
            with pytest.raises(ShowdownError, match="doesn't run best-of series"):
                await a.ladder(1, "gen9championsvgc2026regmcbo3", team_for("gen9championsvgc2026regmc"))
            with pytest.raises(ShowdownError, match="invalid"):  # the server refuses the rule
                await a.challenge(b.username, "gen9championsvgc2026regmc@@@Best of = 3", team_for("gen9championsvgc2026regmc"))
            ours_named = ShowdownClient("pikapro-named", server=pikapro_server(ours), reconnect=False)
            async with ours_named:
                with pytest.raises(ShowdownError, match="play series on a local Showdown server"):
                    await ours_named.ladder(1, "gen9championsvgc2026regmcbo3", team_for("gen9championsvgc2026regmc"))
            bad = team_for("gen9championsvgc2026regmc").replace("Garchomp", "Mewtwo")
            with pytest.raises(ShowdownError, match="rejected"):
                await a.ladder(1, "gen9championsvgc2026regmc", bad)
            fmt = "gen9customgame"
            challenge = asyncio.ensure_future(a.challenge(b.username, fmt, team_for(fmt)))
            while (a.user_id, b.user_id) not in b.challenges:
                await asyncio.sleep(0.01)
            await b.send(f"/reject {a.username}")
            with pytest.raises(ChallengeError):
                await asyncio.wait_for(challenge, 10)
            clone = ShowdownClient(a.username, server=ours, reconnect=False)
            with pytest.raises(LoginError):
                await clone.connect()
        finally:
            await asyncio.gather(a.close(), b.close())

    asyncio.run(run())


def test_ours_cli(ours, tmp_path):
    team = TEAMS / "gen9championsvgc2026regmc.txt"
    common = ["--server", ours, "--handler", "random", "--team", str(team), "--games", "2", "--log-level", "WARNING"]
    # The acceptor logs at INFO, as JSON, to a file: its login (`user`,
    # named) says it is ready to be challenged.
    log = tmp_path / "accept.log"
    with log.open("w") as stderr:
        accept = subprocess.Popen(
            [sys.executable, "-m", "pikapro.showdown", "accept", "--username", "pikapro-cli-b", *common,
             "--log-level", "INFO", "--log-json"],
            stdout=subprocess.PIPE,
            stderr=stderr,
            text=True,
        )
    wait_for_login(accept, log, "pikapro-cli-b", 60)
    out = subprocess.run(
        [
            sys.executable, "-m", "pikapro.showdown", "challenge", "pikapro-cli-b", "--format", "gen9championsvgc2026regmc",
            "--username", "pikapro-cli-a", "--save-logs", str(tmp_path), *common,
        ],
        capture_output=True,
        text=True,
        timeout=120,
    )
    accepted, _ = accept.communicate(timeout=60)
    assert out.returncode == 0 and accept.returncode == 0, out.stderr
    for text in (out.stdout, accepted):
        lines = [json.loads(line) for line in text.splitlines() if line.startswith("{")]
        assert lines[-1]["summary"]["games"] == 2 and lines[-1]["summary"]["unfinished"] == 0
    assert len(list(tmp_path.glob("battle-*.log"))) == 2


# -- Showdown's own server (off by default) ------------------------------------------------


@pytest.mark.real_showdown
@pytest.mark.parametrize(
    "fmt,mode",
    [
        ("gen9championsvgc2026regmc", "challenge"),
        ("gen9championsvgc2026regmc", "ladder"),
        ("gen9randombattle", "challenge"),
        ("gen9randombattle", "ladder"),
        ("gen9championsrandombattle", "ladder"),
        ("gen9randomdoublesbattle", "challenge"),
    ],
)
def test_showdown_games(showdown, fmt, mode):
    team = team_for(fmt) if fmt == "gen9championsvgc2026regmc" else None
    a, b, ra, rb = asyncio.run(play(showdown, fmt, team, mode, GAMES))
    check_pair(ra, rb, GAMES)
    assert {r.opponent for r in ra} == {b.name}


@pytest.mark.real_showdown
def test_showdown_session_handler(showdown):
    """SessionHandler (on the stand-in session) on Showdown's server."""
    fmt = "gen9championsvgc2026regmc"

    async def run():
        a = ShowdownClient("pikapro-sd-a", server=showdown, max_concurrent=3, handler=SessionHandler("random", session_factory=FakeSession))
        b = ShowdownClient("pikapro-sd-b", server=showdown, max_concurrent=3, handler=SessionHandler("random", session_factory=FakeSession))
        await asyncio.gather(a.connect(), b.connect())
        try:
            return await asyncio.wait_for(asyncio.gather(a.ladder(4, fmt, team_for(fmt)), b.ladder(4, fmt, team_for(fmt))), 300)
        finally:
            await asyncio.gather(a.close(), b.close())

    ra, rb = asyncio.run(run())
    check_pair(ra, rb, 4)


def check_series(ra: List[SeriesResult], rb: List[SeriesResult], n: int) -> None:
    """Both players' series results agree, game by game, with no invalid
    choice, fallback or unread line."""
    assert len(ra) == len(rb) == n
    by_room = {r.room: r for r in rb}
    for r in ra:
        other = by_room[r.room]
        assert r.finished and other.finished, (r, other)
        assert r.winner == other.winner and r.tie == other.tie
        assert (r.wins, r.losses, r.ties) == (other.losses, other.wins, other.ties)
        assert 2 <= len(r.games) <= 3 and len(r.games) == len(other.games)
        assert r.won is (r.wins > r.losses) and (r.wins == 2 or r.losses == 2)
        assert [g.game for g in r.games] == list(range(1, len(r.games) + 1))
        assert {g.series for g in r.games} == {r.room} and len({g.room for g in r.games}) == len(r.games)
        assert {g.side for g in r.games} == {r.side}, "each player keeps its side"
        for x in (r, other):
            assert x.invalid == x.fallbacks == 0 and x.extras.get("unknown_lines", 0) == 0, x
            rows = [g.extras["series_row"] for g in x.games]
            # The session's series row at each game's end: the game, the
            # score with that game counted, the series' result at the last.
            assert [row[0] for row in rows] == [g.game for g in x.games]
            assert rows[-1][1:3] == [x.wins, x.losses] and rows[-1][3] == (1 if x.won else 2)
            assert all(row[3] == 0 for row in rows[:-1])


@pytest.mark.real_showdown
@pytest.mark.parametrize(
    "fmt,mode",
    [
        ("gen9championsvgc2026regmcbo3", "ladder"),
        ("gen9championsvgc2026regmcbo3", "challenge"),
        # Gen 9 VGC has no Bo3 ladder: a challenge's "Best of = 3" rule,
        # with Open Team Sheets offered and accepted in each game.
        ("gen9vgc2025regi@@@Best of = 3", "challenge"),
    ],
)
def test_showdown_series(showdown, fmt, mode):
    """Best-of-3 series on Showdown's own server, both players through
    ChannelSession: the built-in heuristics against random actions (so some
    series go to three games), two at a time, with the timer on."""
    team = team_for("gen9vgc2025regi" if fmt.startswith("gen9vgc") else "gen9championsvgc2026regmc")

    async def run():
        n = next(_names)
        a = ShowdownClient(f"pikapro-s{n}a", server=showdown, max_concurrent=2, timer=True, handler=SessionHandler("simple_heuristics", seed=n))
        b = ShowdownClient(f"pikapro-s{n}b", server=showdown, max_concurrent=2, timer=True, handler=SessionHandler("random", seed=n))
        await asyncio.gather(a.connect(), b.connect())
        try:
            if mode == "ladder":
                return await asyncio.wait_for(asyncio.gather(a.ladder(SERIES, fmt, team), b.ladder(SERIES, fmt, team)), 900)
            return await asyncio.wait_for(
                asyncio.gather(a.challenge(b.username, fmt, team, n=SERIES), b.accept_challenges(a.username, SERIES, team)),
                900,
            )
        finally:
            await asyncio.gather(a.close(), b.close())

    ra, rb = asyncio.run(run())
    assert all(isinstance(r, SeriesResult) for r in ra + rb)
    check_series(ra, rb, SERIES)
    assert {r.format for r in ra} == {"gen9championsvgc2026regmcbo3" if "bo3" in fmt else "gen9vgc2025regi"}


# -- Showdown's own server with its limits on, and in /prelockdown ------------------------


class Popups(logging.Handler):
    """The ``popup`` events the clients log (Showdown's refusals)."""

    def __init__(self):
        super().__init__()
        self.texts: List[str] = []

    def emit(self, record):
        if getattr(record, "event", None) == "popup":
            self.texts.append(record.fields["text"])


class SlowRandom(RandomRequestHandler):
    """Random choices, each after ``delay`` s (a model thinking): a series
    lasts long enough for the next ones to start alongside it."""

    def __init__(self, seed, delay):
        super().__init__(seed)
        self.delay = delay

    async def choose(self, rooms):
        await asyncio.sleep(self.delay)
        return await super().choose(rooms)


@pytest.mark.real_showdown
def test_showdown_series_within_its_limits(showdown_limits):
    """Showdown counts a series as two of its 5 games at once (the series
    room and the game being played, ``Ladder.prepBattle``): on a server run
    without ``--no-security``, with ``max_concurrent=5``, the clients play 2 series
    at once, and neither the game limit nor the challenge cooldown (10 s
    from the server's acknowledgement, after the team validation) refuses
    a start: 5 Bo3 challenges, the accepting side slow enough that the
    series overlap (a client that sent them too fast would have its 4th
    refused). By challenge only: with IP checks on, Showdown's ladder never
    matches two users on one IP (``matchmakingOK``), and a search goes
    through the same ``prepBattle``."""
    n = 5
    team = team_for("gen9championsvgc2026regmc")
    fmt = "gen9championsvgc2026regmcbo3"
    config = ServerConfig(websocket_url=showdown_limits, throttle=0.7, challenge_interval=10.5, max_games=5)
    popups = Popups()
    log = logging.getLogger("pikapro.showdown")
    log.addHandler(popups)
    level = log.level
    log.setLevel(logging.INFO)

    async def run():
        k = next(_names)
        a = ShowdownClient(f"pikapro-lim{k}a", server=config, max_concurrent=5, handler=RandomRequestHandler(k))
        b = ShowdownClient(f"pikapro-lim{k}b", server=config, max_concurrent=5, handler=SlowRandom(k, 1.5))
        await asyncio.gather(a.connect(), b.connect())
        peak = 0

        async def watch():
            nonlocal peak
            while True:
                peak = max(peak, len(a.series), len(b.series))
                await asyncio.sleep(0.05)

        watcher = asyncio.ensure_future(watch())
        try:
            results = await asyncio.wait_for(
                asyncio.gather(a.challenge(b.username, fmt, team, n=n), b.accept_challenges(a.username, n, team)), 900
            )
        finally:
            watcher.cancel()
            await asyncio.gather(a.close(), b.close())
        return results, peak

    try:
        (ra, rb), peak = asyncio.run(run())
    finally:
        log.removeHandler(popups)
        log.setLevel(level)
    assert popups.texts == [], popups.texts
    assert peak <= 2, peak
    assert len(ra) == len(rb) == n and all(isinstance(r, SeriesResult) and r.finished for r in ra + rb)
    by_room = {r.room: r for r in rb}
    assert all(by_room[r.room].winner == r.winner for r in ra)


@pytest.mark.real_showdown
def test_showdown_prelockdown_refuses_a_series_search(showdown_prelockdown):
    """Under ``/prelockdown`` Showdown accepts a best-of search, then
    refuses the match (``Rooms.createBattle``: "Best-of-3 battles cannot be
    started at this time"): both players' ``ladder()`` end with that
    error, the one matched on its search and the one searching first."""
    team = team_for("gen9championsvgc2026regmc")
    fmt = "gen9championsvgc2026regmcbo3"

    async def run():
        k = next(_names)
        a = ShowdownClient(f"pikapro-pre{k}a", server=showdown_prelockdown, handler=RandomRequestHandler(k))
        b = ShowdownClient(f"pikapro-pre{k}b", server=showdown_prelockdown, handler=RandomRequestHandler(k + 1))
        await asyncio.gather(a.connect(), b.connect())
        try:
            first = asyncio.ensure_future(a.ladder(1, fmt, team))
            await asyncio.sleep(3)
            second = asyncio.ensure_future(b.ladder(1, fmt, team))
            done = await asyncio.wait_for(asyncio.gather(first, second, return_exceptions=True), 60)
            return done, a.searching, b.searching
        finally:
            await asyncio.gather(a.close(), b.close())

    (ra, rb), sa, sb = asyncio.run(run())
    for r in (ra, rb):
        assert isinstance(r, ShowdownError), r
        assert "cannot be started" in str(r), r
    assert sa == sb == []
