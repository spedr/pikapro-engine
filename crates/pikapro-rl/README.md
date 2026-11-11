# pikapro-rl

The reinforcement-learning environment of
[pikapro-engine](https://github.com/spedr/pikapro-engine), a Pokémon battle
simulator faithful to Pokémon Showdown.

- `env::VecEnv`: one call steps `n` battles on worker threads and writes
  both players' observations, masks, rewards and termination into
  buffers allocated once;
- observations from each player's view
  ([`pikapro-view`](https://crates.io/crates/pikapro-view)), never from
  the battle's state: a `basic` float vector, or your own
  `encode::Encoder`;
- honest action masks, built from what the player's request shows,
  factored per slot or joint;
- the `terminal` and `ko` reward presets, autoreset, best-of-3 series,
  open team sheets, random teams or team pools;
- poke-env's three baseline players as native opponents, and win-rate
  evaluation;
- `session::ChannelSession`: the observation and mask from a Showdown
  server's text, for playing a trained policy on a server.

Most users drive it from Python, through the `pikapro` package's
`pikapro.rl`. From Rust:

```rust
use std::sync::Arc;

use pikapro::Format;
use pikapro_rl::env::{EnvConfig, VecEnv};
use pikapro_rl::vec::{RANDOM_ACTION, StepBuffers, Teams};
use pikapro_teams::{Mode, TeamGenerator};

let format = Format::champions_vgc();
let teams = Teams::Generator(Arc::new(TeamGenerator::new(&format, Mode::Sets).unwrap()));
let n = 64;
let env = VecEnv::new(EnvConfig::new(format, n, teams), 2).unwrap(); // the `basic` observation, a random opponent
let mut out = StepBuffers::new(env.shape());
env.reset(Some(0), &mut out.as_out()).unwrap();

// Both players' actions, by battle and player; RANDOM_ACTION lets the
// environment pick a random allowed one (the built-in opponent's are ignored).
let actions = vec![RANDOM_ACTION; n * 2 * env.actions_per_player()];
let mut episodes = 0;
for _ in 0..200 {
    env.step(&actions, &mut out.as_out()).unwrap();
    episodes += out.terminated.iter().zip(&out.truncated).filter(|(t, u)| **t || **u).count();
}
println!("{episodes} episodes finished");
```

Part of pikapro: see the
[main README](https://github.com/spedr/pikapro-engine/blob/main/README.md), the
[RL guide](https://github.com/spedr/pikapro-engine/blob/main/docs/RL_GUIDE.md)
and [observations](https://github.com/spedr/pikapro-engine/blob/main/docs/python/encoders.md).

MIT licensed. Built on pikapro, a port of [Pokémon Showdown](https://github.com/smogon/pokemon-showdown)'s
simulator (MIT). The baseline players are ported from [poke-env](https://github.com/hsahovic/poke-env) (MIT).
The notices are in `THIRD_PARTY_NOTICES`.
Pokémon and all related names are trademarks of Nintendo, Creatures Inc.,
Game Freak and The Pokémon Company. pikapro is not affiliated with or endorsed by
them, or by Pokémon Showdown.
