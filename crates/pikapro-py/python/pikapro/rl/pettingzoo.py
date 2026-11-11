"""A PettingZoo ``ParallelEnv`` (``pip install 'pikapro[pettingzoo]'``): one
battle, agents ``"p1"`` and ``"p2"`` (players 0 and 1 of a
:class:`pikapro.rl.VecEnv` with ``opponent="external"``; with
``side_randomization`` they may play either side).

Each agent observes ``{"observation": ..., "action_mask": ...}`` (its own
ego-centric view) and acts with one ``Discrete`` index, the joint choice
index. Every step takes an action for every live agent; an agent that
isn't asked anything (the other side's forced switch, say) has an all-False
mask and its action is ignored. When the battle ends, both agents are done
and ``agents`` empties; ``reset()`` starts the next battle.
"""

from __future__ import annotations

from typing import Any, Dict, Optional

import numpy as np

from .gym import _player_obs, observation_space


def _pz():
    try:
        import pettingzoo
    except ImportError as e:  # pragma: no cover
        raise ImportError("pikapro.rl.pettingzoo needs pettingzoo: pip install 'pikapro[pettingzoo]'") from e
    return pettingzoo


class ParallelEnv(_pz().ParallelEnv):  # type: ignore[misc]
    """One battle of ``format`` for two agents (see the module docs).
    Keywords go to :class:`pikapro.rl.VecEnv` (``teams``, ``reward``,
    ``observation``, ``max_turns``, ...; ``masks`` must be ``"joint"`` and
    ``autoreset`` ``"next_step"``)."""

    metadata = {"name": "pikapro_v0", "render_modes": [], "is_parallelizable": True}

    def __init__(self, format: str = "championsvgc", **kwargs):
        import gymnasium

        from . import VecEnv

        kwargs = dict(kwargs)
        kwargs.setdefault("masks", "joint")
        kwargs.setdefault("threads", 1)
        if kwargs["masks"] != "joint":
            raise ValueError("the PettingZoo adapter takes one Discrete action per agent: masks='joint'")
        kwargs["opponent"] = "external"
        # An ended battle stays ended until reset(): the step that ends it
        # returns its terminal observation.
        kwargs.setdefault("autoreset", "next_step")
        if kwargs["autoreset"] != "next_step":
            raise ValueError("the PettingZoo adapter uses autoreset='next_step' (reset() starts the next battle)")
        self.env = VecEnv(format, n=1, **kwargs)
        self.possible_agents = ["p1", "p2"]
        self.agents = []
        self._obs_space = observation_space(self.env)
        self._act_space = gymnasium.spaces.Discrete(self.env.mask_width)

    def observation_space(self, agent: str):
        """The observation space of ``agent`` (the same for both):
        ``Dict(observation, action_mask)``."""
        return self._obs_space

    def action_space(self, agent: str):
        """The action space of ``agent`` (the same for both):
        ``Discrete`` joint choice indices."""
        return self._act_space

    def _observe(self, step) -> Dict[str, Any]:
        return {a: _player_obs(step, 0, p, copy=True) for p, a in enumerate(self.possible_agents) if a in self.agents}

    def _infos(self, step) -> Dict[str, Dict[str, Any]]:
        return {
            a: {"needs_action": bool(step.needs_action[0, p]), "status": int(step.status[0, p])}
            for p, a in enumerate(self.possible_agents)
            if a in self.agents
        }

    def reset(self, seed: Optional[int] = None, options: Optional[dict] = None):
        """Starts the next battle (``seed``: episodes renumbered from 0
        with that base seed; without one, the next episode of the base
        seed in use: the constructor's ``seed=``, 0 by default, until a
        seeded reset replaces it). Returns ``(observations, infos)`` by
        agent."""
        step = self.env.reset(seed=seed)
        self.agents = list(self.possible_agents)
        return self._observe(step), self._infos(step)

    def step(self, actions: Dict[str, int]):
        """One step: ``actions`` maps each live agent to a joint choice
        index (ignored for an agent that isn't asked). Returns
        ``(observations, rewards, terminations, truncations, infos)`` by
        agent; when the battle ends, ``infos`` has ``winner`` and
        ``end_reason`` and :attr:`agents` empties."""
        a = np.full((1, 2), -1, dtype=np.int64)
        for p, agent in enumerate(self.possible_agents):
            if agent in actions and actions[agent] is not None:
                a[0, p] = int(actions[agent])
        step = self.env.step(a)
        obs = self._observe(step)
        infos = self._infos(step)
        rewards = {agent: float(step.reward[0, p]) for p, agent in enumerate(self.possible_agents) if agent in self.agents}
        term = bool(step.terminated[0])
        trunc = bool(step.truncated[0])
        terminations = {agent: term for agent in self.agents}
        truncations = {agent: trunc for agent in self.agents}
        if term or trunc:
            for agent in self.agents:
                infos[agent]["winner"] = int(step.winner[0])
                infos[agent]["end_reason"] = int(step.end_reason[0])
            self.agents = []
        return obs, rewards, terminations, truncations, infos

    def action_masks(self, agent: str) -> np.ndarray:
        """``(n_actions,)`` bool: the actions ``agent`` may take now (all
        False when it isn't asked)."""
        return self.env.last.masks[0, self.possible_agents.index(agent)].copy()

    def render(self):  # pragma: no cover
        """Does nothing: there is no rendering (``pikapro.Battle.log`` is
        the text log)."""
        return None

    def close(self):
        """Closes the underlying :class:`pikapro.rl.VecEnv`."""
        self.env.close()

    def state(self):  # pragma: no cover
        """Not available: the battle's full state is hidden information.
        Raises ``NotImplementedError``."""
        raise NotImplementedError("the environment's state is hidden information; use the agents' observations")
