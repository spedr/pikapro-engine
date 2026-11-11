# pikapro.rl: options

The main options of `pikapro.rl`. [Concepts](python/concepts.md) explains the
ideas and [observations](python/encoders.md) covers what a step gives
you. To get started, see the [install guide](python/install.md) and the
[quickstart](python/quickstart.md).

**The arrays a step returns are the environment's buffers, and the next
call overwrites them: copy what you keep.**

## Observations

Each player gets `"basic"`, a float32 vector `(n, 2, 1688)` for a plain
network ([its values](python/encoders.md#the-basic-vector));
`env.observation["columns"]` names them.

Foe HP is a percentage unless you pass `hp="exact"`. A built-in
opponent's rows are zeros unless you pass `observe_builtin=True`.

For your own features, write a function over `BattleView` and call it
with `rl.embed_all(env, fn)`
([example](python/encoders.md#your-own-features-in-python)), or implement
`pikapro_rl::encode::Encoder` in Rust
([how](python/encoders.md#your-own-encoder-in-rust)).

## Masks and actions

`masks=` sets the layout of both masks and actions:

- **`"factored"`** (the default): `active * slot_size + preview_size`
  bools per player (`2 * 47 + 360` in VGC). Actions are `(n, 2, active)`
  slot action indices.
- **`"joint"`**: every joint choice (2,569 in VGC; 15 in `gen9singles` and
  `gen9randombattle`, 21 in `gen9ou`, 135 in `championsbss`). Actions
  are `(n, 2)` choice indices.

At gen9ou's team preview, where every Pokémon is brought, an action names
the lead (6 choices, not the 720 orders); the others keep their team
order.

[Actions and rows](python/concepts.md#actions-and-rows) explains both,
including `env.pair_mask` for pairing the two slots.

`switch_by="row"` (the default) or `"position"` sets what a switch
action names. `env.decode(i, player, actions)` returns the
`pikapro.Choice` the actions make, and `env.describe(...)` its Showdown
string. `env.space` is the format's `pikapro.ActionSpace`.

`step` takes both players' actions `(n, 2, per)` (or `(n, 2)` when
`per == 1`), or player 0's alone `(n, per)` (or `(n,)`).
`rl.RANDOM_ACTION` (−2) lets the environment pick a random allowed
action, and `env.sample_actions(rng, players)` does the same in numpy.

## Rewards, opponents, teams

| option | values |
|---|---|
| `reward=` | `"terminal"` (default) or `"ko"`, with `ko_weight` (0.1) ([rewards](python/concepts.md#rewards-and-termination)) |
| `max_turns=` | cuts an episode (end reason 4, `truncated`) |
| `autoreset=` | `"next_step"` (default) or `"same_step"` ([autoreset](python/concepts.md#autoreset)) |
| `opponent=` | `"random"`, `"max_base_power"`, `"simple_heuristics"`, `"external"`, or a mixture `{"random": 1, "simple_heuristics": 3}`; `env.set_opponent(...)` changes it ([opponents](python/concepts.md#opponents-and-self-play)) |
| `teams=` | `None`, `"sets"`, `"uniform"`, a `pikapro.RandomTeams`, a pair or list of pairs of team texts, a `TeamPool` or two ([team pools](python/concepts.md#team-pools)) |
| `ots=`, `series=` | open team sheets, best-of series, in formats with team preview ([sheets and series](python/concepts.md#open-team-sheets-and-best-of-3)) |
| `record=` | keeps each episode's choices for `env.record(i)` and `rl.replay` ([replay](python/concepts.md#determinism-and-replay)) |

## Zero-copy buffers

`env.bind(name=array, ...)` makes a step write into your arrays, of the
buffer's dtype and size. `env.buffer_specs` lists the names, shapes and
dtypes. [Performance](python/performance.md#buffers-and-copies) has a
torch example.

## BattleView

`env.view(i, player=0)` or `rl.BattleView.from_battle(battle, side)`
reads one view with poke-env's names, read-only: what the player has
seen. Values are plain strings and numbers, not poke-env's enums. Every
member is in the
[API reference](python/api.md#class-pikaprorlbattleview).

## Evaluation

```python
from pikapro.rl import eval as ev
print(ev.cross_evaluate(["random", "max_base_power", "simple_heuristics"], "championsvgc", 500))
r = ev.evaluate(my_policy, "random", "championsvgc", 500, observation="basic", masks="joint")
print(r)          # 0.912 [0.887, 0.932] (912-88-0): score rate, Wilson 95% interval, W-L-T
```

Every episode is played twice with the same battle seed, the policies
swapping seats and teams. A policy of yours is a callable `(step, env) ->
actions` over batched observations and masks, as player 0.

`evaluate` returns a `WinRecord`: `wins`, `losses`, `ties`, `win_rate`
(ties count half), `wilson(z)` and `significantly_better`.

## Gymnasium, PettingZoo and Stable-Baselines3

The agent is player 0 against a built-in opponent (except in PettingZoo).
Observations are `{"observation": ..., "action_mask": ...}` (the mask
int8), and actions are one `Discrete` joint choice index.
`action_masks()` serves sb3-contrib's `MaskablePPO`.

```python
from pikapro.rl.gym import GymEnv, GymVectorEnv
env = GymEnv("championsvgc", observation="basic", opponent="simple_heuristics")     # gymnasium.Env
envs = GymVectorEnv("championsvgc", num_envs=256, observation="basic")              # gymnasium.vector.VectorEnv, NEXT_STEP
envs = GymVectorEnv("championsvgc", num_envs=256, observation="basic", copy=False) # views: the fast path

from pikapro.rl.pettingzoo import ParallelEnv
env = ParallelEnv("championsvgc", observation="basic")   # agents "p1", "p2"; one battle

from pikapro.rl.sb3 import SB3VecEnv
from sb3_contrib import MaskablePPO
env = SB3VecEnv("championsvgc", num_envs=64, observation="basic", opponent="simple_heuristics")
model = MaskablePPO("MlpPolicy", env).learn(400_000)
env.env_method("set_opponent", "random")
```

- In PettingZoo every step takes an action for each live agent. An agent
  that isn't asked has an all-zero mask, and its action is ignored.
- The SB3 `VecEnv` resets within the step and puts the terminal
  observation in `infos[i]["terminal_observation"]`. Use `MlpPolicy`.
- The Gymnasium and SB3 adapters refuse `opponent="external"`; for
  self-play through an adapter, use the PettingZoo `ParallelEnv`.

## Examples

Examples of wiring, not recommended agents, in
`crates/pikapro-py/examples/`:

- `sb3_maskable_ppo.py`: MaskablePPO against the heuristic opponent, then
  evaluated against Random and the heuristics.
- `cleanrl_ppo_selfplay.py`: a single-file PPO on the native `VecEnv`,
  against the heuristics first, then in self-play against a frozen copy.
- `custom_encoder.py`: your own observation, a Python function over
  `BattleView`.

Full runs with their defaults (championsvgc, `"sets"` teams, the `ko`
reward, `max_turns=100`) on 4 CPU threads of an Apple M4 Pro, evaluated
over 500 mirrored episodes with a greedy policy:

| example | training | vs Random | vs SimpleHeuristics |
|---|---|---|---|
| `sb3_maskable_ppo.py` | 400k steps against the heuristics, 39 s <!-- bench: RL_GUIDE sb3 example wall time; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `CUDA_VISIBLE_DEVICES= python crates/pikapro-py/examples/sb3_maskable_ppo.py` (its defaults: 4 threads; torch 2.14 and sb3-contrib 2.9 on the CPU), the training time it prints, median of 3 --> | 0.861 [0.838, 0.881] | 0.172 [0.150, 0.197] |
| `cleanrl_ppo_selfplay.py` | 819k steps: 50 updates against the heuristics, 50 in self-play; 60 s <!-- bench: RL_GUIDE cleanrl example wall time; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `python crates/pikapro-py/examples/cleanrl_ppo_selfplay.py` (its defaults: 4 threads, 100 updates; torch 2.14 on the CPU), the time at the last update, median of 3 --> | 0.890 [0.869, 0.908] | 0.178 [0.156, 0.203] |

For speed, see [performance](python/performance.md).
