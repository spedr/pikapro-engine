"""A Stable-Baselines3 ``VecEnv`` (``pip install 'pikapro[sb3]'``) over
:class:`pikapro.rl.VecEnv`, for PPO and sb3-contrib's ``MaskablePPO``.

SB3 resets an ended environment within the same step and passes the
episode's last observation in ``infos[i]["terminal_observation"]``, so the
environment runs with ``autoreset="same_step"``. The agent is player 0
against a built-in opponent (changeable with :meth:`SB3VecEnv.set_opponent`,
also through ``env_method("set_opponent", ...)``); ``"external"`` raises
``ValueError`` wherever it is given, as nothing would play player 1 (for
self-play, use :mod:`pikapro.rl.pettingzoo` or :class:`pikapro.rl.VecEnv`). Observations are the
``basic`` float vector (``MlpPolicy``); the action is the joint choice
index; ``action_masks()`` gives MaskablePPO the masks::

    from sb3_contrib import MaskablePPO
    env = SB3VecEnv("championsvgc", num_envs=64, opponent="simple_heuristics")
    model = MaskablePPO("MlpPolicy", env).learn(200_000)
"""

from __future__ import annotations

from typing import Any, List, Optional, Sequence

import numpy as np


def _sb3():
    try:
        import stable_baselines3.common.vec_env as vec_env
    except ImportError as e:  # pragma: no cover
        raise ImportError("pikapro.rl.sb3 needs stable-baselines3: pip install 'pikapro[sb3]'") from e
    return vec_env


_SB3 = "SB3VecEnv"

#: How many times in a row a failed battle's next episode may fail to start
#: before :class:`SB3VecEnv` raises.
_RESTART_TRIES = 8


class SB3VecEnv(_sb3().VecEnv):  # type: ignore[misc]
    """``num_envs`` battles of ``format`` as an SB3 ``VecEnv`` (see the
    module docs). Keywords go to :class:`pikapro.rl.VecEnv`."""

    def __init__(self, format: str = "championsvgc", num_envs: int = 1, **kwargs):
        import gymnasium

        from . import VecEnv
        from .gym import _check_builtin, observation_space

        kwargs = dict(kwargs)
        kwargs.setdefault("masks", "joint")
        if kwargs["masks"] != "joint":
            raise ValueError("the SB3 adapter takes one Discrete action per battle: masks='joint'")
        _check_builtin(kwargs.get("opponent", "random"), _SB3)
        kwargs["autoreset"] = "same_step"
        self.env = VecEnv(format, n=num_envs, **kwargs)
        space = observation_space(self.env)
        obs_space = space["observation"]
        act_space = gymnasium.spaces.Discrete(self.env.mask_width)
        self.render_mode = None
        super().__init__(num_envs, obs_space, act_space)
        self._actions: Optional[np.ndarray] = None
        self._seed: Optional[int] = None

    def _obs(self, obs) -> np.ndarray:
        return np.array(obs[:, 0])

    def _restart_failed(self) -> None:
        """Starts the next episode of each battle that failed.

        A battle that fails (a panic in the engine:
        :meth:`pikapro.rl.VecEnv.take_errors` says why) ends its episode
        with a zeroed observation and no allowed action, and starts its
        next episode at the environment's next step; so does one whose
        next episode failed to start. SB3 expects that episode's first
        observation, and ``MaskablePPO`` a mask with an action in it, from
        this step. So the environment is stepped once more with no action
        for anyone, which starts those episodes and changes nothing in the
        other battles (a step without an action leaves a battle as it is).
        ``RuntimeError`` if a battle's episodes fail to start
        ``_RESTART_TRIES`` (8) times in a row."""
        env = self.env
        for _ in range(_RESTART_TRIES):
            last = env.last
            failed = ~last.needs_action[:, 0] & ~last.masks[:, 0].any(axis=1)
            if not failed.any():
                return
            env.step(np.full(self.num_envs, -1, dtype=np.int64))
        last = env.last
        failed = np.flatnonzero(~last.needs_action[:, 0] & ~last.masks[:, 0].any(axis=1))
        if failed.size:
            raise RuntimeError(
                f"SB3VecEnv: battle {int(failed[0])} has nothing to choose after {_RESTART_TRIES} restarts "
                "(its episodes fail to start): env.take_errors() says why"
            )

    def reset(self):
        """Starts a new episode in every battle (with the seed
        :meth:`seed` set: episodes renumbered from 0 with that base seed;
        else each battle's next episode of the base seed in use: the
        constructor's ``seed=`` until a seeded reset replaces it) and
        returns player 0's observations."""
        seed = self._seed
        self._seed = None
        self.env.reset(seed=seed)
        self._restart_failed()
        return self._obs(self.env.last.obs)

    def step_async(self, actions: np.ndarray) -> None:
        """Stores the actions (one joint choice index per battle) for
        :meth:`step_wait`."""
        self._actions = np.asarray(actions, dtype=np.int64).reshape(self.num_envs)

    def step_wait(self):
        """Steps every battle. Returns ``(observations, rewards, dones,
        infos)``; an ended battle has already started its next episode,
        and its ``infos[i]`` has ``terminal_observation``,
        ``TimeLimit.truncated``, ``winner`` and ``end_reason``. A battle
        that failed (``end_reason`` 5, a zeroed ``terminal_observation``)
        has started its next episode too."""
        step = self.env.step(self._actions)
        rewards = step.reward[:, 0].astype(np.float32)
        dones = step.terminated | step.truncated
        infos: List[dict] = [{} for _ in range(self.num_envs)]
        for i in np.flatnonzero(dones):
            info = infos[i]
            info["terminal_observation"] = np.array(step.final_obs[i, 0])
            info["TimeLimit.truncated"] = bool(step.truncated[i] and not step.terminated[i])
            info["winner"] = int(step.winner[i])
            info["end_reason"] = int(step.end_reason[i])
        # (After the step's own values are copied: it steps again.)
        self._restart_failed()
        return self._obs(self.env.last.obs), rewards, dones, infos

    def action_masks(self) -> np.ndarray:
        """``(num_envs, n_actions)`` bool."""
        return self.env.last.masks[:, 0].copy()

    def set_opponent(self, opponent, which=None) -> None:
        """Changes player 1's policy from each battle's next episode on
        (see :meth:`pikapro.rl.VecEnv.set_opponent`): a built-in policy or
        a mixture of them (``"external"`` raises ``ValueError``)."""
        from .gym import _check_builtin

        _check_builtin(opponent, _SB3)
        self.env.set_opponent(opponent, which)

    def seed(self, seed: Optional[int] = None) -> Sequence[Optional[int]]:
        """Sets the base seed the next :meth:`reset` uses (episodes
        renumbered from 0); ``None`` unsets it."""
        self._seed = seed
        return [None if seed is None else seed + i for i in range(self.num_envs)]

    def close(self) -> None:
        """Closes the underlying :class:`pikapro.rl.VecEnv`."""
        self.env.close()

    def get_attr(self, attr_name: str, indices=None) -> List[Any]:
        """SB3's ``get_attr``: the attribute of this object, once per
        index."""
        return [getattr(self, attr_name) for _ in self._indices(indices)]

    def set_attr(self, attr_name: str, value: Any, indices=None) -> None:
        """SB3's ``set_attr``: sets the attribute on this object."""
        setattr(self, attr_name, value)

    def env_method(self, method_name: str, *method_args, indices=None, **method_kwargs) -> List[Any]:
        """SB3's ``env_method``. ``"action_masks"`` returns each battle's
        mask and ``"set_opponent"`` changes the given battles' opponent;
        any other method of this object is called once."""
        idx = self._indices(indices)
        if method_name == "action_masks":
            masks = self.env.last.masks[:, 0]
            return [masks[i].copy() for i in idx]
        if method_name == "set_opponent":
            # SB3 selects battles with ``indices``; an explicit ``which``
            # keyword wins.
            which = method_kwargs.pop("which", None if indices is None else list(idx))
            self.set_opponent(*method_args, which=which, **method_kwargs)
            return [None for _ in idx]
        result = getattr(self, method_name)(*method_args, **method_kwargs)
        return [result for _ in idx]

    def env_is_wrapped(self, wrapper_class, indices=None) -> List[bool]:
        """SB3's ``env_is_wrapped``: always False (the battles aren't
        Gymnasium environments)."""
        return [False for _ in self._indices(indices)]

    def _indices(self, indices) -> List[int]:
        if indices is None:
            return list(range(self.num_envs))
        if isinstance(indices, int):
            return [indices]
        return list(indices)
