# pikapro-showdown-server

A websocket server that speaks the part of Pokémon Showdown's server
protocol [poke-env](https://github.com/hsahovic/poke-env) uses, with
battles simulated by pikapro. poke-env players and Gymnasium
environments battle on it unchanged.

**It is a local test server, not a public one.** There are no passwords
or accounts, so anyone who can connect can log in under any free name.
It binds to 127.0.0.1 by default and warns at startup when bound to
another address. Don't expose it to an untrusted network without an
authenticating proxy in front.

## Running poke-env against it

```sh
cargo run --profile release-server -p pikapro-showdown-server             # ws://127.0.0.1:8000/showdown/websocket
cargo run --profile release-server -p pikapro-showdown-server -- --help   # --host, --port, --no-validate, --seed, --max-connections, ...
```

Use the `release-server` profile: a crashed battle then ends only its
room. With `--release`, a panic stops the server.

It listens where a local Showdown server does, so poke-env's
`LocalhostServerConfiguration` points at it:

```python
import asyncio
from poke_env.player import RandomPlayer
from poke_env import LocalhostServerConfiguration

team = open("team.txt").read()          # Showdown export format, or packed
kwargs = dict(battle_format="gen9championsvgc2026regmc", team=team,
              server_configuration=LocalhostServerConfiguration)
p1, p2 = RandomPlayer(**kwargs), RandomPlayer(**kwargs)
asyncio.run(p1.battle_against(p2, n_battles=100))
print(p1.n_won_battles, p2.n_won_battles)
```

Laddering, challenges and `SinglesEnv` / `DoublesEnv` work too.

### Formats

| `battle_format` | pikapro preset | teams validated as | notes |
|---|---|---|---|
| `gen9championsvgc2026regmc` | `championsvgc` | `[Gen 9 Champions] VGC 2026 Reg M-C` | Open Team Sheets, VGC timer |
| `gen9championsbssregmc` | `championsbss` | `[Gen 9 Champions] BSS Reg M-C` | singles, VGC timer |
| `gen9vgc2025regi` | `gen9vgc`, every Pokémon at level 50 | `[Gen 9] VGC 2025 Reg I` | Open Team Sheets, VGC timer |
| `gen9ou` | `gen9ou` | `[Gen 9] OU` | singles, the whole team brought at team preview |
| `gen9randombattle` | `gen9randombattle` | generated (not validated) | Showdown's random-battle generator, ported exactly, draws the teams; a player's own team is ignored |
| `gen9randomdoublesbattle` | `gen9randomdoublesbattle` | generated (not validated) | the same |
| `gen9customgame` | `gen9singles` | engine checks only | no team preview (as the preset) |
| `gen9doublescustomgame` | `gen9doubles` | engine checks only | no team preview (as the preset) |

The preset ids are accepted too. Teams are required (generated in the
random battles), and `--no-validate` accepts any team the
engine can build. `--seed N` gives each battle a
seed derived from `N`, and each random-battle player a team seed. `tests/python/teams/` has a legal team for each
VGC format.

## Protocol

Supported: connection and `/trn` without authentication; `/utm`;
`/search` (rated, Showdown's local Elo), `/cancelsearch`; `/challenge`,
`/cancelchallenge`, `/accept`, `/reject`; battle rooms with each user's
channel of the log and `|request|`; `/choose`, `/move`, `/switch`,
`/team`, `/undo`, with Showdown's choice errors; `/timer`, `/forfeit`,
`/acceptopenteamsheets`, `/rejectopenteamsheets`, `/join`, `/leave`,
`/avatar`, `/noreply`, `/cmd`.

Not supported: authentication and registered accounts, chat, the lobby
and other chat rooms, private messages beyond challenges, spectator
privacy, best-of series, other generations' random battles, replays, tournaments,
`/offertie`, `/showteam`, `/vtm`, `/importinputlog`, the disconnection
timer. A player whose last connection closes forfeits after
`--disconnect-grace` seconds (60).

## Differences from Showdown's server

- Every format here is ladderable. Showdown keeps `gen9vgc2025regi` and
  the custom games off its ladder.
- Custom games have no team preview here and send no `|debug|` lines.
- There is no throttle by default, like a `--no-security` Showdown
  server. `--max-lines-per-second N` turns one on; past it, the rest of
  the message is dropped, battle choices included.

Limits (see `--help` for the flags): 1,000 connections, 64 unfinished
battles per user, 16 finished rooms per player and 1,000 in all, 4 MiB of
output waiting for one client (past it the client is dropped), and
256 MiB waiting for all clients.

Throughput is in the
[performance guide](../../docs/python/performance.md#poke-env-players).

MIT licensed. Built on pikapro, a port of [Pokémon Showdown](https://github.com/smogon/pokemon-showdown)'s
simulator (MIT); the notices are in the repository root's
[`THIRD_PARTY_NOTICES`](../../THIRD_PARTY_NOTICES).
Pokémon and all related names are trademarks of Nintendo, Creatures Inc.,
Game Freak and The Pokémon Company. pikapro is not affiliated with or endorsed by
them, or by Pokémon Showdown.
