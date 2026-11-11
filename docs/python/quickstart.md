# Quickstart

Two agents: a random one on the native vector environment, then
MaskablePPO through the Stable-Baselines3 adapter. The format is
`championsvgc` (Pokémon Champions VGC doubles); `pikapro.formats()` lists
the others.

## A random agent

```python
import numpy as np
import pikapro.rl as rl

env = rl.VecEnv("championsvgc", n=256, opponent="simple_heuristics", seed=0)
out = env.reset(seed=0)
rng = np.random.default_rng(0)

wins = episodes = 0
for _ in range(2000):
    actions = env.sample_actions(rng, players=[0])[:, 0]   # your policy here
    out = env.step(actions)
    done = out.terminated | out.truncated
    wins += int((out.winner[done] == 0).sum())
    episodes += int(done.sum())
print(f"{episodes} episodes, win rate {wins / episodes:.3f}")
```

- Player 0 is the agent. Player 1 is the opponent, here poke-env's
  `SimpleHeuristicsPlayer` ported to Rust.
- Every array has the battle on axis 0 and the player on axis 1.
  `out.terminated`, `out.truncated`, `out.end_reason` and `out.winner` are
  per battle.
- `env.sample_actions` draws uniformly among the actions each mask allows.
  `step` takes player 0's actions alone, `(n, 2)` here for the two active
  slots in doubles, or both players' `(n, 2, 2)`.
- A battle that ends returns its terminal observation, and the next step
  starts its next episode.
- `out`'s arrays are the environment's own buffers. The next step
  overwrites them, so copy what you keep.

`out.obs` is `"basic"`, a flat float vector of what the player knows,
which an MLP can use. [Observations](encoders.md) lists its values and
shows how to write your own.

## MaskablePPO

```sh
pip install torch --index-url https://download.pytorch.org/whl/cpu
pip install 'pikapro[sb3]'
```

In a venv made with `uv venv`, which has no `pip`, use `uv pip install`
for both lines.

```python
import torch
import pikapro.rl as rl
from pikapro.rl.sb3 import SB3VecEnv
from sb3_contrib import MaskablePPO

torch.set_num_threads(4)

env = SB3VecEnv("championsvgc", num_envs=64, observation="basic", teams="sets",
                opponent="simple_heuristics", reward="ko", max_turns=100, threads=4)
# The example script's PPO settings: short rollouts, large minibatches.
model = MaskablePPO("MlpPolicy", env, n_steps=128, batch_size=2048, n_epochs=4,
                    learning_rate=3e-4, gamma=0.99, ent_coef=0.01,
                    policy_kwargs={"net_arch": [256, 256]}, verbose=1)
model.learn(400_000)

# Win rate against Random over 500 mirrored episodes (1,000 games).
from pikapro.rl import eval as ev

def policy(step, venv):
    obs = step.obs[:, 0]
    masks = step.masks[:, 0]
    actions, _ = model.predict(obs, action_masks=masks, deterministic=True)
    return actions

print(ev.evaluate(policy, "random", "championsvgc", 500, observation="basic", masks="joint",
                  reward="ko", max_turns=100))
```

- `reward="ko"` adds ±0.1 per knock-out to the ±1 of a win or a loss.
- With SB3's default PPO settings (`n_steps=2048` per env, `batch_size=64`,
  `n_epochs=10`) the same 400k steps take about 15 times as long.
- `ev.evaluate` plays each episode twice with the teams swapped, and
  prints the score rate with a Wilson 95% interval.

The full script is
[`examples/sb3_maskable_ppo.py`](../../crates/pikapro-py/examples/sb3_maskable_ppo.py).
On 4 CPU threads the snippet's `model.learn(400_000)` takes about 40
seconds
<!-- bench: the snippet as written, timed around model.learn; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, torch 2.14 (CPU) and sb3-contrib 2.9, Python 3.13: 40 s (10,010 steps/s), one run. The example script (its defaults, 4 threads), same session: 39 s, median of 3 -->
(the script's figure is the same). The policy clearly beats Random but
not the heuristics
([example results](../RL_GUIDE.md#examples)).

## Next

- [Concepts](concepts.md): what the observation contains, what a mask
  allows, how rewards and episodes work.
- [Self-play](concepts.md#opponents-and-self-play), and
  [`examples/cleanrl_ppo_selfplay.py`](../../crates/pikapro-py/examples/cleanrl_ppo_selfplay.py).
- [Team pools](concepts.md#team-pools): train on specific teams, or a
  list of pokepaste links.
