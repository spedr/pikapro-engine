"""EXAMPLE: MaskablePPO (sb3-contrib) on championsvgc against the
heuristic opponent, then evaluated against Random and the heuristics.

An example of wiring, not a recommended agent: the observation, the
network and the hyperparameters are placeholders to change.

    pip install 'pikapro[sb3]' torch
    python examples/sb3_maskable_ppo.py                 # the full run
    python examples/sb3_maskable_ppo.py --smoke         # a few steps (the tests)

* The environment: :class:`pikapro.rl.sb3.SB3VecEnv`, ``num_envs`` battles
  stepped in Rust per call, same-step reset with the terminal observation in
  ``infos`` (what SB3 expects), ``action_masks()`` for MaskablePPO.
* The observation: ``"basic"``, a small float vector. See
  ``docs/python/encoders.md``; ``examples/custom_encoder.py`` writes one of
  your own.
* The action: the joint choice index (``masks="joint"``: 2,569 choices in
  VGC, team preview included), masked to what the request allows.
* The reward: ``"ko"`` (win/loss +-1, and +-0.1 per knock-out).
"""

from __future__ import annotations

import argparse
import time

import numpy as np

import pikapro.rl as rl
from pikapro.rl.sb3 import SB3VecEnv

def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--format", default="championsvgc")
    p.add_argument("--timesteps", type=int, default=400_000)
    p.add_argument("--envs", type=int, default=64)
    p.add_argument("--threads", type=int, default=4)
    p.add_argument("--eval-episodes", type=int, default=500)
    p.add_argument("--seed", type=int, default=4_650_000)
    p.add_argument("--save", default=None, help="save the model here")
    p.add_argument("--smoke", action="store_true", help="a few steps only (the tests)")
    args = p.parse_args()
    if args.smoke:
        args.timesteps, args.envs, args.eval_episodes = 256, 8, 4

    import torch
    from sb3_contrib import MaskablePPO

    torch.set_num_threads(args.threads)
    env = SB3VecEnv(
        args.format,
        num_envs=args.envs,
        threads=args.threads,
        seed=args.seed,
        teams="sets",  # random teams from Showdown's random-battle sets, per episode
        opponent="simple_heuristics",
        observation="basic",
        reward="ko",
        max_turns=100,
    )
    model = MaskablePPO(
        "MlpPolicy",
        env,
        n_steps=128 if not args.smoke else 16,
        batch_size=2048 if not args.smoke else 64,
        n_epochs=4,
        learning_rate=3e-4,
        gamma=0.99,
        ent_coef=0.01,
        policy_kwargs={"net_arch": [256, 256]},
        seed=args.seed,
        verbose=0,
    )
    t = time.perf_counter()
    model.learn(args.timesteps)
    train = time.perf_counter() - t
    print(f"trained {args.timesteps} steps in {train:.0f} s ({args.timesteps / train:.0f} steps/s)")
    if args.save:
        model.save(args.save)

    def policy(step, env):
        """The trained model, greedy, on player 0's batched observations."""
        obs, masks = step.obs[:, 0], step.masks[:, 0]
        actions, _ = model.predict(obs, action_masks=masks, deterministic=True)
        return np.where(masks.any(-1), actions, -1)

    # Evaluate on battles training never played: another base seed. Episode
    # seeds are hashed from the base and the episode number, so any other
    # base gives unrelated battles; this one is also far from any base a
    # run would train on.
    eval_seed = args.seed + 1_000_000_000
    kw = dict(teams="sets", observation="basic", masks="joint", seed=eval_seed, threads=args.threads, max_turns=100)
    for opponent in ("random", "simple_heuristics"):
        r = rl.eval.evaluate(policy, opponent, args.format, args.eval_episodes, **kw)
        print(f"vs {opponent:<18} {r}")


if __name__ == "__main__":
    main()
