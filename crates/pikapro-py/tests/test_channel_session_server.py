"""pikapro.rl.ChannelSession on a real server: pikapro.showdown's SessionHandler
plays pikapro-showdown-server's formats through real sessions (the built-in
heuristics against a policy on the session's observation and joint mask),
and every line the room sends is read."""

from __future__ import annotations

import asyncio
import os

import numpy as np
import pytest

pytest.importorskip("websockets")

from pikapro.showdown import SessionHandler, ShowdownClient, pikapro_server  # noqa: E402

from test_showdown_servers import OUR_FORMATS, check_pair, ours, showdown, team_for  # noqa: E402,F401

GAMES = int(os.environ.get("PIKAPRO_CHANNEL_GAMES", "3"))


@pytest.mark.parametrize("fmt", list(OUR_FORMATS))
def test_sessions_play_on_our_server(ours, fmt):  # noqa: F811
    rng = np.random.default_rng(0)
    decisions = []

    def policy(obs, mask):
        decisions.append(int(mask.sum()))
        return int(rng.choice(np.flatnonzero(mask)))

    async def run():
        k = list(OUR_FORMATS).index(fmt)
        a = ShowdownClient(f"pikapro-cs-a{k}", server=pikapro_server(ours), max_concurrent=2, handler=SessionHandler("simple_heuristics"))
        b = ShowdownClient(f"pikapro-cs-b{k}", server=pikapro_server(ours), max_concurrent=2, handler=SessionHandler(policy, masks="joint"))
        await asyncio.gather(a.connect(), b.connect())
        try:
            return await asyncio.wait_for(
                asyncio.gather(
                    a.challenge(b.username, fmt, team_for(fmt), n=GAMES),
                    b.accept_challenges(a.username, GAMES, team_for(fmt)),
                ),
                600,
            )
        finally:
            await asyncio.gather(a.close(), b.close())

    ra, rb = asyncio.run(run())
    check_pair(ra, rb, GAMES)
    assert decisions and min(decisions) > 0
    for r in ra + rb:
        assert r.extras["unknown_lines"] == 0, r.extras
        assert r.extras["session_winner"] == r.winner


REAL = ["gen9championsvgc2026regmc", "gen9randombattle", "gen9randomdoublesbattle"]


@pytest.mark.real_showdown
@pytest.mark.parametrize("fmt", REAL)
def test_sessions_play_on_showdown(showdown, fmt):  # noqa: F811
    """The same on Showdown's own server (with ``--real-showdown`` or
    ``PIKAPRO_REAL_SHOWDOWN=1``): its lines, requests and choice checks."""
    rng = np.random.default_rng(1)

    def policy(obs, mask):
        return int(rng.choice(np.flatnonzero(mask)))

    team = team_for(fmt) if fmt == "gen9championsvgc2026regmc" else None

    async def run():
        k = REAL.index(fmt)
        a = ShowdownClient(f"pikapro-sds-a{k}", server=showdown, max_concurrent=2, handler=SessionHandler("simple_heuristics"))
        b = ShowdownClient(f"pikapro-sds-b{k}", server=showdown, max_concurrent=2, handler=SessionHandler(policy, masks="joint"))
        await asyncio.gather(a.connect(), b.connect())
        try:
            return await asyncio.wait_for(
                asyncio.gather(a.challenge(b.username, fmt, team, n=GAMES), b.accept_challenges(a.username, GAMES, team)),
                600,
            )
        finally:
            await asyncio.gather(a.close(), b.close())

    ra, rb = asyncio.run(run())
    check_pair(ra, rb, GAMES)
    for r in ra + rb:
        assert r.extras["unknown_lines"] == 0, r.extras
        assert r.extras["session_winner"] == r.winner
