"""pikapro.showdown: play on Showdown-protocol servers.

A websocket client for Showdown's server protocol (``PROTOCOL.md``) that
carries battles between a server and a battle handler: log in, search the
ladder, challenge and accept, and play each battle room. With
:class:`SessionHandler`, an agent trained in :class:`pikapro.rl.VecEnv` plays
on a server with the same observations, masks and actions: the room's
lines go through :class:`pikapro.rl.ChannelSession`.

It connects to a local server (Showdown's own or
pikapro-showdown-server), a server you run, and VGC Arena
(``server="arena"``, a bot ladder that starts battles itself:
:data:`ARENA`). It needs the ``websockets`` package
(``pip install 'pikapro[showdown]'``).

Best-of series (``gen9championsvgc2026regmcbo3``) play through Showdown's
series room: the client follows each game's battle room, answers the
"ready for the next game" prompt, and returns one :class:`SeriesResult`
per series; :class:`SessionHandler` plays each game with the series'
score and the earlier games' records, as ``VecEnv``'s series mode does.

Quick start::

    import asyncio
    from pikapro.showdown import ShowdownClient, SessionHandler

    async def main():
        handler = SessionHandler("simple_heuristics")
        async with ShowdownClient("my-bot", server="local", handler=handler) as client:
            results = await client.ladder(5, "gen9championsvgc2026regmc", team=open("team.txt").read())
        print(sum(r.won is True for r in results), "wins")

    asyncio.run(main())

From the shell: ``python -m pikapro.showdown ladder --format ... --team team.txt``
(``python -m pikapro.showdown --help``). The guide is ``docs/python/showdown.md``.
"""

from __future__ import annotations

import logging as _logging

from .client import (
    ARENA,
    LOCAL,
    AcceptPlan,
    PIKAPRO,
    PIKAPRO_FORMAT_ALIASES,
    ChallengeError,
    LoginError,
    ServerConfig,
    ShowdownClient,
    ShowdownError,
    pikapro_server,
    server_config,
)
from .handlers import BUILTIN_POLICIES, BattleHandler, RandomRequestHandler, SessionHandler, random_choice
from .protocol import Format, format_key, pack_team, parse_formats, series_length, to_id
from .room import BattleResult, BattleRoom, SeriesResult, SeriesRoom

__all__ = [
    "ARENA",
    "AcceptPlan",
    "BUILTIN_POLICIES",
    "BattleHandler",
    "BattleResult",
    "BattleRoom",
    "ChallengeError",
    "Format",
    "LOCAL",
    "PIKAPRO",
    "PIKAPRO_FORMAT_ALIASES",
    "LoginError",
    "RandomRequestHandler",
    "SeriesResult",
    "SeriesRoom",
    "ServerConfig",
    "SessionHandler",
    "ShowdownClient",
    "ShowdownError",
    "format_key",
    "pack_team",
    "parse_formats",
    "series_length",
    "pikapro_server",
    "random_choice",
    "server_config",
    "to_id",
]

# A library logs to "pikapro.showdown" and leaves the handlers to the
# application (the CLI installs one).
_logging.getLogger("pikapro.showdown").addHandler(_logging.NullHandler())
