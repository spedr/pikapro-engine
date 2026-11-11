<h1>
  <img src="docs/assets/logo-light.png#gh-light-mode-only" alt="pikapro-engine" height="70">
  <img src="docs/assets/logo-dark.png#gh-dark-mode-only" alt="pikapro-engine" height="70">
</h1>

pikapro is the battle simulator behind
[mikumiku](https://www.smogon.com/forums/threads/mikumiku37-a-self-play-vgc-bot-reached-1-on-the-vgc-reg-m-c-ladder.3789199/),
a Pokémon VGC bot. It's written in Rust and plays Gen 9 and Pokémon
Champions exactly as
[Pokémon Showdown](https://github.com/smogon/pokemon-showdown) does, down
to each random number it draws.

The same seed and choices give the same battle on every platform and
thread count. One thread plays about 20,800 Champions VGC battles per
second <!-- bench: engine championsvgc 1 thread; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `cargo bench-sim --format championsvgc --battles 20000 --threads 1`, median of 3 -->, roughly 295× Showdown's rate in turns per second
([how this is measured](docs/python/performance.md#the-engine)).

It comes with Python bindings, a reinforcement-learning environment and a
Showdown-protocol client.

## Install

```sh
pip install pikapro        # CPython 3.10+ on Linux, macOS and Windows
```

```toml
[dependencies]
pikapro = "0.1"
```

## Usage

Play a battle:

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

Step 1,024 battles at once for reinforcement learning, where each player
sees only what a Showdown client would:

```python
import numpy as np
import pikapro.rl as rl

env = rl.VecEnv("championsvgc", n=1024, opponent="simple_heuristics", seed=0)
out = env.reset(seed=0)
rng = np.random.default_rng(0)
for _ in range(100):
    actions = env.sample_actions(rng, players=[0])[:, 0]   # your policy here
    out = env.step(actions)
```

From Rust, see the [`pikapro` crate](crates/pikapro). To play a trained
policy on a server, see [playing on a server](docs/python/showdown.md).

## Formats

| preset | game |
|---|---|
| `championsvgc` | Pokémon Champions VGC (Reg M-C), Mega Evolution |
| `championsbss` | Pokémon Champions Battle Spot Singles (Reg M-C), Mega Evolution |
| `gen9vgc` | Scarlet/Violet VGC (Reg I), Terastallization |
| `gen9ou` | Smogon's Scarlet/Violet OverUsed (Showdown's `[Gen 9] OU`): singles, up to six Pokémon (level 100 by default), team preview, Terastallization |
| `gen9randombattle` | Showdown's `[Gen 9] Random Battle`: singles, six Pokémon at their random-battle levels, Sleep Clause Mod, Illusion Level Mod, Terastallization |
| `gen9randomdoublesbattle` | Showdown's `[Gen 9] Random Doubles Battle`: the same in doubles |
| `gen9singles` | a singles custom game: six Pokémon, no team preview, no bans, Terastallization |
| `gen9doubles` | a doubles custom game: six Pokémon, no team preview, no bans, Terastallization |

Gen 9 OU is the only Smogon tier with a preset. The mechanics are the same
in every Gen 9 tier, so adding another is mostly a matter of its validation
rules. Other generations aren't modelled.
The random battles' teams come from Showdown's random-battle generator,
ported exactly: the same seed gives the same team, and a Showdown input
log replays its battle. See [formats](docs/FORMATS.md).

## Correctness

Every mechanic is ported from Showdown's source. `cargo conformance` plays
random battles on both simulators and compares their logs line by line,
along with every random draw, and over 1,300 matching battles replay as
fixtures in `cargo test`. No divergence is known in these formats, against
Showdown's commit
[`a5df827`](https://github.com/smogon/pokemon-showdown/commit/a5df8274e85b0889bf2a9b3422a08b39732374fc)
([conformance](docs/CONFORMANCE.md)). The Champions and VGC formats have
been tested the longest. OU and the random battles are newer and less
thoroughly tested, so rare divergences are more likely there, though they
have held up well. If you find one, please open an issue with the battle's
seed and teams, or its input log, and the first line that differs.

Before 1.0, a minor version may break the API.

## Documentation

- [Python](docs/python/README.md): [quickstart](docs/python/quickstart.md), [API reference](docs/python/api.md), [FAQ](docs/python/faq.md)
- [Reinforcement learning](docs/RL_GUIDE.md): [observations](docs/python/encoders.md)
- [Architecture](docs/ARCHITECTURE.md): [determinism](docs/DETERMINISM.md)
- Changelogs: [Rust](CHANGELOG.md), [Python](crates/pikapro-py/CHANGELOG.md)

## License

[MIT](LICENSE). The game data and mechanics come from Pokémon Showdown,
and pikapro-rl's baseline players from
[poke-env](https://github.com/hsahovic/poke-env), both MIT. Their notices
are in [THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES).

Pokémon and all related names are trademarks of Nintendo, Creatures Inc.,
Game Freak and The Pokémon Company. pikapro is not affiliated with or
endorsed by them, or by Pokémon Showdown.
