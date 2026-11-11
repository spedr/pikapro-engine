# Playing on a server

An agent trained in `pikapro.rl` can play over Showdown's protocol on a
local server (Showdown's own, or
[pikapro-showdown-server](../../crates/pikapro-showdown-server/README.md)),
a server you run, or [VGC Arena](https://vgc.bot).

`pikapro.showdown` is the websocket client. `pikapro.rl.ChannelSession`
reads a player's channel text into the environment's observations,
masks and choices. `SessionHandler` joins the two, so a policy trained in
`VecEnv` plays on a server. Every signature is in the
[API reference](api.md).

## The client

```sh
pip install 'pikapro[showdown]'
```

```python
import asyncio
from pikapro.showdown import ShowdownClient, SessionHandler

async def main():
    handler = SessionHandler("simple_heuristics")          # or your policy: (obs, mask) -> action
    async with ShowdownClient("my-bot", server="local", handler=handler, max_concurrent=4) as client:
        results = await client.ladder(20, "gen9championsvgc2026regmc", team=open("team.txt").read())
    print(sum(r.won is True for r in results), "wins of", len(results))

asyncio.run(main())
```

`async with` connects and logs in. On the way out it forfeits unfinished
battles.

A ladder game needs a server and a second player. Start
pikapro-showdown-server (`cargo run --profile release-server -p
pikapro-showdown-server`, port 8000) or Showdown (`node pokemon-showdown
start --no-security`), write a team and start a random opponent:

```sh
python -c "import pikapro; print(pikapro.sample_team('championsvgc'))" > team.txt
python -m pikapro.showdown ladder --format gen9championsvgc2026regmc --team team.txt \
    --handler random --games 20 --concurrent 4
```

### Servers

`server=` takes a `ServerConfig` or one of:

| `server` | where | login |
|---|---|---|
| `"local"` (default) | `ws://localhost:8000/showdown/websocket` | none |
| `"pikapro"`, `"pikapro:<address>"` | pikapro-showdown-server | none |
| a port, `"host:port"`, a `ws://` or `wss://` URL | a server you run | none |
| `"arena"` | VGC Arena | the bot's token |

The client connects only to these: a host on `psim.us` or
`pokemonshowdown.com` raises `ValueError`, and so does a token over plain
`ws://` to another machine. A redirect is held to the same rules: one
that leads to such a host isn't followed, and the connection ends with a
`ShowdownError`. Against pikapro-showdown-server, pass
`server="pikapro"`, so its custom-game formats play as the `gen9singles`
and `gen9doubles` presets.

If the connection drops, the client reconnects with backoff, logs in
again and rejoins its battles. `reconnect=False` ends them as
`"disconnected"` instead.

**VGC Arena.** A bot logs in with its name and token (the arguments, or
`ARENA_BOT_NAME` and `ARENA_BOT_TOKEN`). The arena pairs bots itself:
`accept_challenges` uploads the team with `/utm` and plays what it is
given. On a random ladder, `FORMAT` + `random`, the arena deals the teams
and `BattleRoom.team` is `None`. `cancel_search()` starts nothing new
while the battles in progress play out. Its rated games can be
best-of-3 sets, and a bot that leaves before a set ends loses it. See the
arena's site for its current rules.

### Games

```python
await client.ladder(n, format, team)                     # /search, n games
await client.challenge(user, format, team, n=1)          # /challenge, one at a time
await client.accept_challenges(from_user=None, n=1, team=None, format=None, plan=None)
await client.wait_for_battles()                          # the battles in progress
```

Each returns a `BattleResult` per game in the order they started, or a
`SeriesResult` per series in a best-of format. A start the server refuses
raises `ShowdownError`, and a challenge not accepted raises
`ChallengeError`.

`team` is a team in export or packed format, `None` for a random-battle
format, or a callable `team(format) -> team` for a pool. `client.formats`
lists the server's formats.

### Best-of series

A format with Showdown's `Best of = 3` rule (the `…bo3` formats, or a
challenge's `@@@Best of = 3`) runs as a series, one battle room per game.
`client.series_results` collects the series and `client.results` every
game. A handler sees `room.series` and `room.game`.
pikapro-showdown-server runs no series.

`open_team_sheets=` answers the Open Team Sheets offer at team preview:

| value | answer |
|---|---|
| `None` (default) | accept in a series' games, leave alone elsewhere |
| `True` / `False` | accept / reject every offer |
| `"mutual"` | wait for the opponent's answer (`ots_wait` seconds) and match it |

### Handlers

A handler plays all of a client's battles, which lets it batch.

```python
class BattleHandler:
    max_batch = 0                                   # most rooms per choose() call; 0: no limit
    def supports(self, format) -> bool: ...         # can it play this format?
    def open(self, room): ...                       # a room started (or was rejoined)
    def feed(self, room, lines) -> bool: ...        # new lines; can the handler choose now?
    def error(self, room, message): ...             # an |error| line
    async def choose(self, rooms) -> list[str | None]: ...  # the text after "/choose " for each room
    def close(self, room): ...                      # the room ended or was left
    def summary(self, room) -> dict: ...            # extras for the BattleResult
```

`choose` returns a one-line choice per room, or `None` to wait. A
handler's failure stays in its battle: the client forfeits it, or sends
`default` for that room.

`SessionHandler(policy, batch=False, observation="basic", masks="factored")`
plays each room through a `ChannelSession`. The policy is a baseline by
name (`"random"`, `"max_base_power"`, `"simple_heuristics"`), a callable
`policy(obs, mask) -> action`, or with `batch=True` a callable taking
stacked observations and masks for all the rooms waiting. Callables run
in a worker thread, so the connection keeps answering while a model
thinks.

`RandomRequestHandler(seed)` picks a random valid choice from the request
alone, so it plays any format. A bot that needs more can subclass
`ShowdownClient`, whose hooks are in the API reference.

### Results and logs

A `BattleResult` has `won` (`True`, `False`, or `None` for a tie or no
result), `reason`, `invalid`, `fallbacks` and `log_path`. `log_dir=` saves
each room's lines. The client logs to the `pikapro.showdown` logger.

### The command line

```sh
python -m pikapro.showdown ladder --format FORMAT [--team FILE ...] [--games N] [--concurrent K]
python -m pikapro.showdown challenge USER --format FORMAT [--team FILE ...] [--games N]
python -m pikapro.showdown accept [--from USER] [--format FORMAT] [--team FILE ...] [--games N]
python -m pikapro.showdown formats [--ladder]
```

Common options are `--server`, `--username`, `--token` (prefer
`$ARENA_BOT_TOKEN`, since arguments show in the process list),
`--handler session|random`, `--policy NAME|module:callable`, `--seed` and
`--save-logs DIR`. `--help` lists the rest. Each battle prints its result
as a JSON line.

## The channel session

`pikapro.rl.ChannelSession` is one battle as one player sees it, read from
the text Showdown sends that player. At each decision it gives the
observation and the mask `VecEnv` gives a player
([how they are compared](../CONFORMANCE.md#the-channel-round-trip)).

`SessionHandler` makes one per room. By hand, feed it every line of the
room and act when it asks:

```python
import pikapro.rl as rl

session = rl.ChannelSession("gen9championsvgc2026regmc", username="Alice")
for line in room_lines:
    session.feed(line)
    if session.needs_action:
        action = policy(session.obs, session.mask)
        send(f"/choose {session.choice(action)}|{rqid}")
```

| member | what it is |
|---|---|
| `needs_action` | the player must choose now |
| `obs`, `mask` | as the player's row of `VecEnv`'s |
| `view` | the view as a `BattleView` |
| `choice(action)` | the `/choose` text for an action |
| `finished`, `winner` | the end of the battle |
| `unknown_lines`, `unknown_names` | what it couldn't read (ignored, never an error) |

[FORMATS.md](../FORMATS.md#reading-showdowns-formats) lists the format ids
a session reads. A best-of series is one session per game, each made with
the series' `pikapro.rl.ChannelSeries`.

**Limits.** Two Pokémon a channel shows alike (duplicate species, no
Species Clause) can't be told apart by any client, so the session takes
the first matching row. An Illusion fools the session as it fools any
client, until the `|replace|`.
