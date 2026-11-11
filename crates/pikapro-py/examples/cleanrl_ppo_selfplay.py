"""EXAMPLE: a CleanRL-style, single-file PPO on the native
:class:`pikapro.rl.VecEnv`, with a curriculum: the heuristic opponent first,
then self-play against a frozen copy of the agent, refreshed as it learns.

An example of wiring, not a recommended agent: the observation, the
network and the hyperparameters are placeholders.

    pip install pikapro torch
    python examples/cleanrl_ppo_selfplay.py             # the full run
    python examples/cleanrl_ppo_selfplay.py --smoke     # a few steps (the tests)

What it shows:

* one batched forward pass per step over ``env.last.obs[:, 0]`` (player 0,
  the agent) and, in self-play, ``obs[:, 1]`` (the opponent's own view,
  ego-centric like the agent's);
* masked sampling from the joint masks;
* steps where the agent isn't asked anything (only the opponent must
  switch) are carried through the returns, not trained on;
* the curriculum is a few lines: ``env.set_opponent("external")`` and a
  frozen copy of the network (see ``CURRICULUM`` below).
"""

from __future__ import annotations

import argparse
import copy
import time

import numpy as np

import pikapro.rl as rl

def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--format", default="championsvgc")
    p.add_argument("--envs", type=int, default=128)
    p.add_argument("--steps", type=int, default=64, help="steps per rollout")
    p.add_argument("--updates", type=int, default=100)
    p.add_argument("--selfplay-at", type=float, default=0.5, help="the fraction of updates after which self-play starts")
    p.add_argument("--refresh", type=int, default=10, help="updates between refreshes of the frozen opponent")
    p.add_argument("--threads", type=int, default=4)
    p.add_argument("--lr", type=float, default=3e-4)
    p.add_argument("--eval-episodes", type=int, default=500)
    p.add_argument("--seed", type=int, default=4_660_000)
    p.add_argument("--smoke", action="store_true")
    args = p.parse_args()
    if args.smoke:
        args.envs, args.steps, args.updates, args.eval_episodes, args.refresh = 8, 8, 4, 4, 1

    import torch
    import torch.nn as nn

    torch.manual_seed(args.seed)
    torch.set_num_threads(args.threads)
    env = rl.VecEnv(
        args.format,
        n=args.envs,
        threads=args.threads,
        seed=args.seed,
        teams="sets",
        opponent="simple_heuristics",
        observation="basic",
        masks="joint",
        reward="ko",
        max_turns=100,
    )
    obs_size, n_actions, N, T = env.obs_shape[-1], env.mask_width, args.envs, args.steps

    class Agent(nn.Module):
        def __init__(self):
            super().__init__()
            self.body = nn.Sequential(nn.Linear(obs_size, 256), nn.Tanh(), nn.Linear(256, 256), nn.Tanh())
            self.pi = nn.Linear(256, n_actions)
            self.v = nn.Linear(256, 1)

        def forward(self, x, mask):
            h = self.body(x)
            logits = self.pi(h).masked_fill(~mask, -1e9)
            return torch.distributions.Categorical(logits=logits), self.v(h).squeeze(-1)

    agent = Agent()
    opt = torch.optim.Adam(agent.parameters(), lr=args.lr, eps=1e-5)
    frozen = None

    def act(net, obs, mask, greedy=False):
        """Actions for the rows whose mask allows something; -1 elsewhere."""
        asked = mask.any(-1)
        with torch.no_grad():
            dist, value = net(torch.as_tensor(obs), torch.as_tensor(mask | ~asked[:, None]))
            a = dist.probs.argmax(-1) if greedy else dist.sample()
        return np.where(asked, a.numpy(), -1), dist.log_prob(a).numpy(), value.numpy()

    obs_buf = np.zeros((T, N, obs_size), np.float32)
    mask_buf = np.zeros((T, N, n_actions), bool)
    act_buf = np.zeros((T, N), np.int64)
    logp_buf = np.zeros((T, N), np.float32)
    val_buf = np.zeros((T, N), np.float32)
    rew_buf = np.zeros((T, N), np.float32)
    done_buf = np.zeros((T, N), np.float32)
    valid_buf = np.zeros((T, N), bool)
    actions = np.full((N, 2), -1, np.int64)
    step = env.reset(seed=args.seed)
    gamma, lam, clip = 0.99, 0.95, 0.2
    t0 = time.perf_counter()
    episodes, wins = 0, 0
    for update in range(args.updates):
        # CURRICULUM: heuristics first, then a frozen copy of the agent,
        # refreshed every `refresh` updates.
        start = int(args.selfplay_at * args.updates)
        if update == start:
            env.set_opponent("external")  # from each battle's next episode on
        if update >= start and (update - start) % args.refresh == 0:
            frozen = copy.deepcopy(agent).eval()
        for t in range(T):
            obs, masks = step.obs[:, 0], step.masks[:, 0]
            a, logp, v = act(agent, obs, masks)
            obs_buf[t], mask_buf[t], act_buf[t], logp_buf[t], val_buf[t] = obs, masks, a, logp, v
            valid_buf[t] = step.needs_action[:, 0]
            actions[:, 0] = a
            actions[:, 1] = -1 if frozen is None else act(frozen, step.obs[:, 1], step.masks[:, 1])[0]
            step = env.step(actions)
            rew_buf[t] = step.reward[:, 0]
            done = step.terminated | step.truncated
            done_buf[t] = done
            episodes += int(done.sum())
            wins += int((step.winner[done] == 0).sum())
        # Returns: rewards and ends between two decisions of a battle go to
        # the first; steps without a decision are skipped.
        with torch.no_grad():
            _, next_v = agent(torch.as_tensor(step.obs[:, 0]), torch.as_tensor(step.masks[:, 0] | ~step.masks[:, 0].any(-1)[:, None]))
        next_v = np.where(step.needs_action[:, 0], next_v.numpy(), 0.0)
        adv = np.zeros((T, N), np.float32)
        last = np.zeros(N, np.float32)
        acc_r = np.zeros(N, np.float32)
        acc_d = np.zeros(N, np.float32)
        for t in reversed(range(T)):
            acc_r += rew_buf[t]
            acc_d = np.maximum(acc_d, done_buf[t])
            v = valid_buf[t]
            nonterm = 1.0 - acc_d
            delta = acc_r + gamma * next_v * nonterm - val_buf[t]
            gae = delta + gamma * lam * nonterm * last
            adv[t] = np.where(v, gae, 0.0)
            last = np.where(v, gae, last)
            next_v = np.where(v, val_buf[t], next_v)
            acc_r = np.where(v, 0.0, acc_r)
            acc_d = np.where(v, 0.0, acc_d)
        ret = adv + val_buf
        idx = np.flatnonzero(valid_buf.reshape(-1))
        if len(idx) == 0:
            continue
        b_obs = torch.as_tensor(obs_buf.reshape(-1, obs_size)[idx])
        b_mask = torch.as_tensor(mask_buf.reshape(-1, n_actions)[idx])
        b_act = torch.as_tensor(act_buf.reshape(-1)[idx])
        b_logp = torch.as_tensor(logp_buf.reshape(-1)[idx])
        b_adv = torch.as_tensor(adv.reshape(-1)[idx])
        b_ret = torch.as_tensor(ret.reshape(-1)[idx])
        for _ in range(4):
            for mb in torch.randperm(len(idx)).split(max(1, len(idx) // 4)):
                dist, value = agent(b_obs[mb], b_mask[mb])
                ratio = (dist.log_prob(b_act[mb]) - b_logp[mb]).exp()
                a_ = b_adv[mb]
                a_ = (a_ - a_.mean()) / (a_.std() + 1e-8) if len(mb) > 1 else a_
                pg = torch.max(-a_ * ratio, -a_ * ratio.clamp(1 - clip, 1 + clip)).mean()
                loss = pg + 0.5 * (value - b_ret[mb]).pow(2).mean() - 0.01 * dist.entropy().mean()
                opt.zero_grad()
                loss.backward()
                nn.utils.clip_grad_norm_(agent.parameters(), 0.5)
                opt.step()
        if (update + 1) % max(1, args.updates // 10) == 0:
            phase = "self-play" if frozen is not None else "heuristics"
            rate = wins / max(1, episodes)
            print(f"update {update + 1:4d}  {phase:<10}  episodes {episodes:6d}  win rate {rate:.3f}  {time.perf_counter() - t0:.0f} s")
            episodes = wins = 0

    def policy(step, env):
        return act(agent, step.obs[:, 0], step.masks[:, 0], greedy=True)[0]

    # Evaluate on battles training never played: another base seed. Episode
    # seeds are hashed from the base and the episode number, so any other
    # base gives unrelated battles; this one is also far from any base a
    # run would train on.
    eval_seed = args.seed + 1_000_000_000
    kw = dict(teams="sets", observation="basic", masks="joint", seed=eval_seed, threads=args.threads, max_turns=100)
    for opponent in ("random", "simple_heuristics"):
        print(f"vs {opponent:<18} {rl.eval.evaluate(policy, opponent, args.format, args.eval_episodes, **kw)}")


if __name__ == "__main__":
    main()
