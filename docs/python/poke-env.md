# poke-env compatibility

`pikapro.compat.poke_env` runs [poke-env](https://github.com/hsahovic/poke-env)
(0.16) on `pikapro.Battle`, in-process, with no server. Unmodified poke-env
players play pikapro battles, and poke-env's own battle tracker follows
either side. poke-env is an optional dependency, imported on first use:

```sh
uv pip install 'poke-env>=0.16,<0.17'    # or: pip install 'pikapro[poke-env]'
```

```python
from poke_env.player import RandomPlayer, SimpleHeuristicsPlayer
from pikapro.compat.poke_env import make_player, play, cross_evaluate

a = make_player(SimpleHeuristicsPlayer, "championsvgc")   # battle_format="gen9championsvgc2026regmc", no server
b = make_player(RandomPlayer, "championsvgc")
result = play(a, b, "championsvgc", 100, seed=0)          # a on side 1
print(result.summary())      # wins, ties, turns, invalid, unavailable, quirks, errors, battles/s
print(a.win_rate)            # the players count the battles, as on a server

cross_evaluate([a, b, make_player(RandomPlayer, "championsvgc")], "championsvgc", 20)
```

| function | |
|---|---|
| `make_player(cls, format, **kwargs)` | a poke-env player for a pikapro format: its Showdown id as `battle_format`, `start_listening=False` |
| `play(p1, p2, format, n_battles, *, teams=None, seed=0, ...)` | plays `n_battles`; battle `i` has seed `seed + i`. `teams`: `None`, a pair, a list of pairs, a `RandomTeams`, or `(i, seed) -> pair`. Returns `PlayResults` |
| `play_parallel(p1, p2, format, n_battles, *, processes=4, ...)` | the same over spawned worker processes; `p1` and `p2` are player classes. Call it under `if __name__ == "__main__":` |
| `cross_evaluate(players, format, n_battles)` | poke-env's `cross_evaluate` table: `results[a][b]` is `a`'s win rate against `b` |
| `Feeder(format, side, *, player=None, username=None)` | a poke-env battle kept in sync with one side |
| `order_to_index(...)`, `index_to_order(...)`, `action_to_index(...)`, `index_to_action(...)` | convert poke-env orders and `SinglesEnv` / `DoublesEnv` actions to and from `pikapro.ActionSpace` indices |
| `showdown_format(f)`, `pikapro_format(f)` | between pikapro presets and Showdown ids |

Signatures are in the [API reference](api.md#pikaprocompatpoke_env).

**How it works.** The driver plays Showdown's battle room: it sends each
player the new lines of its channel, then its request, and submits the
player's choice to `Battle.submit`. A rejected choice goes back as
Showdown's `|error|` line and poke-env chooses again. `[Invalid choice]`
is counted in `invalid`, and after `max_retries` in a row the driver
sends `default`. `[Unavailable choice]` is counted in `unavailable`.

**Known poke-env quirks.** poke-env 0.16.1 mishandles some lines and
requests that real Showdown sends too. The driver and the feeder work
around them and count each use in the battle record's `quirks`:

| quirk | what happens | workaround |
|---|---|---|
| `locked-move-target` | a Pokémon locked into a move in doubles gets `move outrage` sent with no target, which Showdown rejects | the driver sends `move 1` |
| `stale-move-target` | poke-env's move data lags Champions' targets (Milk Drink), so it sends no target | when the move's real target leaves one legal choice, the driver adds it |
| `switch-name-ambiguous` | poke-env switches by name, and a Ditto transformed into your Beedrill takes the switch | the driver sends the position |
| `miss-before-spread` | `parse_message` misreads a bounced spread move that missed | the feeder moves `[miss]` to the end of the line |

The own side's Tera type stays unknown to poke-env, which doesn't read
the request's `teraType`.

For speed, see [performance](performance.md#poke-env-players).
