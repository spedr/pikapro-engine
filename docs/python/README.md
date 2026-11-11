# pikapro: Python documentation

`pikapro` is the Python package of [pikapro](../../README.md), a
deterministic Pokémon battle simulator in Rust, checked against Pokémon
Showdown down to every random number drawn.

| module | what |
|---|---|
| `pikapro` | the simulator: `Battle`, `BattlePool`, `ActionSpace`, typed events and requests, Showdown's logs, random teams, validation |
| `pikapro.rl` | a fast reinforcement-learning environment on each player's view, with Gymnasium, PettingZoo and SB3 adapters |
| `pikapro.showdown` | a client that plays a `pikapro.rl` policy on a local server or VGC Arena |
| `pikapro.compat.poke_env` | unmodified poke-env players and trackers on `pikapro` battles, in-process |

## Start here

1. [Install](install.md): wheels, extras, building from source.
2. [Quickstart](quickstart.md): a random agent, then MaskablePPO.
3. [Concepts](concepts.md): how an episode, a mask, a reward and a team
   pool work.

## Guides

- [Observations](encoders.md): the `basic` vector and your own
  features.
- [The simulator API](simulator.md): `Battle` and `BattlePool`, for
  search, tooling and custom environments.
- [poke-env compatibility](poke-env.md): poke-env players and trackers
  in-process.
- [Playing on a server](showdown.md): the `pikapro.showdown` client.
- [Performance](performance.md): threads, batch size and measured numbers.
- [FAQ](faq.md).

## Reference

- [API reference](api.md): every public class and function.
- [Changelog](../../crates/pikapro-py/CHANGELOG.md), and the Rust
  crates' [changelog](../../CHANGELOG.md).
- The [RL guide](../RL_GUIDE.md): every option in one page, with the
  [examples](../RL_GUIDE.md#examples).
- How the simulator is checked against Showdown:
  [CONFORMANCE.md](../CONFORMANCE.md).
