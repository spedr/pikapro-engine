# pikapro

A fast, deterministic Pokémon battle simulator for Python, with a
reinforcement-learning environment and a Showdown client built on it.

`pikapro` runs [pikapro-engine](https://github.com/spedr/pikapro-engine), a Rust
port of Pokémon Showdown's battle simulator (Gen 9 singles and doubles,
VGC, Pokémon Champions), checked against Showdown down to every random
number drawn. Battles run on Rust threads with the GIL released.

- `pikapro.Battle` and `pikapro.BattlePool` are the simulator.
- `pikapro.rl` is the environment: `n` battles per call, each player
  seeing only what a Showdown client knows. It has adapters for
  Gymnasium, PettingZoo and Stable-Baselines3.
- `pikapro.showdown` plays a policy trained in `pikapro.rl` on a local
  Showdown-protocol server or [VGC Arena](https://vgc.bot).
- `pikapro.compat.poke_env` runs unmodified poke-env players on
  `pikapro` battles, in-process.

## Install

```sh
pip install pikapro                 # needs only numpy
pip install 'pikapro[gym]'          # + Gymnasium; also [pettingzoo], [sb3], [poke-env], [showdown], [all]
```

Wheels for CPython 3.10+ on Linux (x86_64, aarch64; glibc and musl),
macOS (Intel, Apple silicon) and Windows (x86_64).

Elsewhere pip builds from the sdist, which needs Rust 1.88 or later; the
[install guide](https://github.com/spedr/pikapro-engine/blob/main/docs/python/install.md)
covers it and building from a checkout.

## Formats

`championsvgc` and `championsbss` (Pokémon Champions VGC and Battle Spot
Singles), `gen9vgc` (Scarlet/Violet VGC), `gen9ou` (Smogon's Gen 9
OverUsed), `gen9randombattle` and `gen9randomdoublesbattle` (Showdown's
random battles, with its random-battle team generator), and the Gen 9 custom games
`gen9singles` and `gen9doubles`. See
[FORMATS](https://github.com/spedr/pikapro-engine/blob/main/docs/FORMATS.md).

## Quick start

One battle, turn by turn:

```python
import pikapro

team = pikapro.sample_team("championsvgc")          # Showdown export text
battle = pikapro.Battle("championsvgc", [team, team], seed=1)
while not battle.over:
    for side in (0, 1):
        legal = battle.legal_actions(side)
        if legal:
            battle.submit(side, legal[0])            # or a Showdown choice: "move 1 2, switch 3"
print(battle.winner, "\n".join(battle.log("p1")[-5:]))   # player 1's Showdown log
```

The environment:

```python
import numpy as np
import pikapro.rl as rl

env = rl.VecEnv("championsvgc", n=1024, opponent="simple_heuristics", seed=0)
out = env.reset(seed=0)
rng = np.random.default_rng(0)
for _ in range(100):
    actions = env.sample_actions(rng, players=[0])[:, 0]   # your policy here
    out = env.step(actions)
    # out.obs, out.masks, out.reward, out.terminated, out.truncated, out.winner
```

On a local server (`pip install 'pikapro[showdown]'`):

```sh
python -m pikapro.showdown ladder --format gen9championsvgc2026regmc \
    --team team.txt --policy simple_heuristics --games 3
```

[VGC Arena](https://vgc.bot) pairs the bots itself, so there the command
is `accept`, with `ARENA_BOT_NAME` and `ARENA_BOT_TOKEN` set:

```sh
python -m pikapro.showdown accept --server arena --team team.txt
```

## Documentation

- [Documentation home](https://github.com/spedr/pikapro-engine/blob/main/docs/python/README.md)
- [Quickstart](https://github.com/spedr/pikapro-engine/blob/main/docs/python/quickstart.md)
- [Concepts](https://github.com/spedr/pikapro-engine/blob/main/docs/python/concepts.md)
- [Playing on a server](https://github.com/spedr/pikapro-engine/blob/main/docs/python/showdown.md)
- [Performance](https://github.com/spedr/pikapro-engine/blob/main/docs/python/performance.md)
- [API reference](https://github.com/spedr/pikapro-engine/blob/main/docs/python/api.md)
- [Changelog](https://github.com/spedr/pikapro-engine/blob/main/crates/pikapro-py/CHANGELOG.md)
- [The project](https://github.com/spedr/pikapro-engine)

## License

MIT.

Game data and mechanics come from
[Pokémon Showdown](https://github.com/smogon/pokemon-showdown) (MIT),
and the built-in baseline opponents are ported from [poke-env](https://github.com/hsahovic/poke-env)
(MIT). Their notices, and the licenses of the Rust crates compiled into
the extension module, are in `THIRD_PARTY_NOTICES`.
Pokémon and all related names are trademarks of Nintendo, Creatures Inc.,
Game Freak and The Pokémon Company. pikapro is not affiliated with or endorsed by
them, or by Pokémon Showdown.
