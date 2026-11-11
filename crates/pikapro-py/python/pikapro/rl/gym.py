"""Gymnasium adapters (``pip install 'pikapro[gym]'``): a ``VectorEnv`` over
:class:`pikapro.rl.VecEnv` and a single-battle ``Env``.

The agent is player 0; player 1 is a built-in opponent (``"random"``,
``"max_base_power"``, ``"simple_heuristics"`` or a mixture), changeable
with ``set_opponent``. ``"external"`` raises ``ValueError`` wherever it is
given (the constructor, a mixture, ``set_opponent``): nothing would play
player 1. For self-play, use :class:`pikapro.rl.VecEnv` directly
(``opponent="external"``) or :mod:`pikapro.rl.pettingzoo`.

* Observations are dicts ``{"observation": ..., "action_mask": ...}``:
  the observation's float vector and the player's mask (int8,
  ``MultiBinary``, as ``space.sample(mask=...)`` takes it).
* Actions are one ``Discrete`` index per battle: the joint choice index
  (``masks="joint"``, the default here: exact, including doubles pairing
  and team preview). ``action_masks()`` returns the masks, as sb3-contrib's
  ``MaskablePPO`` asks for them.
* Autoreset is Gymnasium 1.x's ``NEXT_STEP``: the step that ends an episode
  returns its terminal observation; the next step starts the next episode
  (its action is ignored, its reward 0). The masks are honest there too:
  on the step that ends a battle, its mask row (in the observation and in
  ``action_masks()``) is all False, since nothing can be chosen. A masked
  categorical built on such a row gives NaN logits, so replace the row
  (for example with any one action allowed) before sampling.
* ``info`` holds ``winner``, ``end_reason``, ``status`` and ``episode_id``
  (the episode number, :attr:`pikapro.rl.Step.episode`); no key clashes with
  the ``"episode"`` statistics Gymnasium's ``RecordEpisodeStatistics``
  adds. A vector env's infos are copied arrays with Gymnasium's ``_key``
  masks.
* Episode ``e`` after ``reset(seed=s)`` plays with a battle seed hashed
  from ``(s, e)``, so envs seeded ``s`` and ``s + 1`` (as ``SyncVectorEnv``
  and Stable-Baselines3 seed their sub-envs) play unrelated battles.
"""

from __future__ import annotations

from typing import Any, Dict, Optional, Sequence, Union

import numpy as np

from . import _opponent_names


def _gym():
    try:
        import gymnasium
    except ImportError as e:  # pragma: no cover
        raise ImportError("pikapro.rl.gym needs gymnasium: pip install 'pikapro[gym]'") from e
    return gymnasium


def observation_space(env):
    """The Gymnasium space of one player's observation of ``env`` (a
    :class:`pikapro.rl.VecEnv`): ``Dict(observation, action_mask)``."""
    gym = _gym()
    spaces = gym.spaces
    specs = env.buffer_specs
    shape = specs["obs"][0][2:]
    f = np.finfo(np.float32)
    obs = spaces.Box(f.min, f.max, shape=shape, dtype=np.float32)
    # int8, as Gymnasium's and PettingZoo's `sample(mask=...)` take it.
    mask = spaces.MultiBinary(env.mask_width)
    return spaces.Dict({"observation": obs, "action_mask": mask})


def _player_obs(step, i=None, player: int = 0, copy: bool = False):
    o = step.obs[:, player] if i is None else step.obs[i, player]
    if copy:
        o = np.array(o)
    m = step.masks[:, player] if i is None else step.masks[i, player]
    m = m.view(np.int8)  # the same bytes: 0 or 1
    return {"observation": o, "action_mask": np.array(m) if copy else m}


def _check_builtin(opponent, adapter: str = "GymEnv and GymVectorEnv") -> None:
    """Refuses ``"external"`` in an opponent spec (alone or in a mixture):
    an adapter steps player 0 only, so an external player 1 would never
    get an action and every battle would stall."""
    if "external" in _opponent_names(opponent):
        raise ValueError(
            f"{adapter}: the agent is player 0 against a built-in opponent (or a mixture of them), so the "
            "opponent can't be 'external': nothing would play player 1 and every battle would stall. For "
            "self-play, use pikapro.rl.pettingzoo.ParallelEnv or pikapro.rl.VecEnv(..., opponent='external')"
        )


def _env_kwargs(kwargs: Dict[str, Any]) -> Dict[str, Any]:
    kwargs = dict(kwargs)
    kwargs.setdefault("masks", "joint")
    if kwargs["masks"] != "joint":
        raise ValueError("the Gymnasium adapters take one Discrete action per battle: masks='joint'")
    _check_builtin(kwargs.get("opponent", "random"))
    kwargs.setdefault("autoreset", "next_step")
    if kwargs["autoreset"] != "next_step":
        raise ValueError("the Gymnasium adapters use NEXT_STEP autoreset")
    return kwargs


_INFO_KEYS = ("winner", "end_reason", "status", "episode_id")


def _info(step, i=None) -> Dict[str, Any]:
    """The step's infos, copied (never views of the env's buffers, which
    the next step overwrites). For a vector env (``i`` None): arrays over
    the battles, with Gymnasium's ``_key`` masks (every battle has every
    key). For one battle: Python scalars."""
    values = (step.winner, step.end_reason, step.status[:, 0], step.episode)
    if i is not None:
        return {k: v[i].item() for k, v in zip(_INFO_KEYS, values)}
    info: Dict[str, Any] = {k: np.array(v) for k, v in zip(_INFO_KEYS, values)}
    for k in _INFO_KEYS:
        info["_" + k] = np.ones(len(step.winner), dtype=bool)
    return info


def _vector_base():
    return _gym().vector.VectorEnv


def _base_seed(seed: Any, n: int) -> Optional[int]:
    """The base seed a vector reset's ``seed`` stands for (an int, or
    Gymnasium's per-environment list ``[s, s + 1, ...]``)."""
    if isinstance(seed, (bool, np.bool_)):
        raise TypeError("seed is an int, not a bool")
    if seed is None or isinstance(seed, (int, np.integer)):
        return None if seed is None else int(seed)
    seeds = list(seed)
    if len(seeds) == n and all(s is None for s in seeds):
        return None
    ints = all(isinstance(s, (int, np.integer)) and not isinstance(s, bool) for s in seeds)
    if len(seeds) == n and ints and all(int(s) == int(seeds[0]) + i for i, s in enumerate(seeds)):
        return int(seeds[0])
    raise ValueError(
        f"the {n} battles share one base seed: pass an int, or Gymnasium's list for one ([s, s + 1, ...]), not {seed!r}"
    )


class GymVectorEnv(_vector_base()):  # type: ignore[misc]
    """``num_envs`` battles of ``format`` as a ``gymnasium.vector.VectorEnv``
    (see the module docs). ``copy`` (default True) returns copies of the
    environment's buffers; ``copy=False`` returns views, overwritten by the
    next step (the fast path). Other keywords go to :class:`pikapro.rl.VecEnv`
    (``teams``, ``opponent``, ``reward``, ``observation``, ...)."""

    def __init__(self, format: str, num_envs: int = 1, *, copy: bool = True, **kwargs):
        from . import VecEnv

        gym = _gym()
        self.env = VecEnv(format, n=num_envs, **_env_kwargs(kwargs))
        self.num_envs = num_envs
        self.copy = copy
        self.single_observation_space = observation_space(self.env)
        self.single_action_space = gym.spaces.Discrete(self.env.mask_width)
        self.observation_space = gym.vector.utils.batch_space(self.single_observation_space, num_envs)
        self.action_space = gym.vector.utils.batch_space(self.single_action_space, num_envs)
        self.metadata = {"autoreset_mode": gym.vector.AutoresetMode.NEXT_STEP}

    def reset(self, *, seed: Union[int, Sequence[Optional[int]], None] = None, options: Optional[dict] = None):
        """Starts a new episode in every battle (``seed``: episodes
        renumbered from 0 with that base seed; without one, each battle's
        next episode of the base seed in use: the constructor's ``seed=``
        until a seeded reset replaces it). The battles share one base
        seed, so of Gymnasium's per-environment lists only the one an int
        ``s`` stands for is taken (``[s, s + 1, ...]``, or every entry
        ``None``); another raises ``ValueError``. Returns ``(observations,
        infos)``."""
        step = self.env.reset(seed=_base_seed(seed, self.num_envs))
        return _player_obs(step, copy=self.copy), _info(step)

    def step(self, actions):
        """One step of every battle: ``actions`` is one joint choice
        index per battle. Returns ``(observations, rewards, terminations,
        truncations, infos)``; ``infos`` has ``winner``, ``end_reason``,
        ``status`` and ``episode_id`` per battle (copies, with ``_key``
        masks)."""
        step = self.env.step(np.asarray(actions, dtype=np.int64).reshape(self.num_envs))
        obs = _player_obs(step, copy=self.copy)
        reward = step.reward[:, 0].copy()
        return obs, reward, step.terminated.copy(), step.truncated.copy(), _info(step)

    def action_masks(self) -> np.ndarray:
        """``(num_envs, n_actions)`` bool: each battle's allowed actions."""
        return self.env.last.masks[:, 0].copy()

    def set_opponent(self, opponent, which=None) -> None:
        """Changes player 1's policy from each battle's next episode on
        (see :meth:`pikapro.rl.VecEnv.set_opponent`): a built-in policy or
        a mixture of them (``"external"`` raises ``ValueError``)."""
        _check_builtin(opponent)
        self.env.set_opponent(opponent, which)

    def close_extras(self, **kwargs) -> None:
        """Closes the underlying :class:`pikapro.rl.VecEnv`."""
        self.env.close()


class GymEnv(_gym().Env):  # type: ignore[misc]
    """One battle of ``format`` as a single-agent ``gymnasium.Env`` (a
    :class:`pikapro.rl.VecEnv` of one battle, one thread): dict observations
    ``{"observation", "action_mask"}``, a ``Discrete`` action (the joint
    choice index), and ``action_masks()`` for ``MaskablePPO``. Keywords go
    to :class:`pikapro.rl.VecEnv`."""

    metadata = {"render_modes": []}

    def __init__(self, format: str = "championsvgc", **kwargs):
        from . import VecEnv

        gym = _gym()
        kwargs = _env_kwargs(kwargs)
        kwargs.setdefault("threads", 1)
        self.env = VecEnv(format, n=1, **kwargs)
        self.observation_space = observation_space(self.env)
        self.action_space = gym.spaces.Discrete(self.env.mask_width)

    def reset(self, *, seed: Optional[int] = None, options: Optional[dict] = None):
        """Starts the next episode (``seed``: episodes renumbered from 0
        with that base seed; without one, the next episode of the base
        seed in use: the constructor's ``seed=``, 0 by default, until a
        seeded reset replaces it). Returns ``(observation, info)``."""
        super().reset(seed=seed)
        step = self.env.reset(seed=seed)
        return _player_obs(step, 0, copy=True), _info(step, 0)

    def step(self, action):
        """Plays one joint choice index. Returns ``(observation, reward,
        terminated, truncated, info)``; ``info`` has ``winner``,
        ``end_reason``, ``status`` and ``episode_id``."""
        step = self.env.step(np.asarray([int(action)], dtype=np.int64))
        obs = _player_obs(step, 0, copy=True)
        return obs, float(step.reward[0, 0]), bool(step.terminated[0]), bool(step.truncated[0]), _info(step, 0)

    def action_masks(self) -> np.ndarray:
        """``(n_actions,)`` bool: the actions allowed now (for
        ``MaskablePPO``)."""
        return self.env.last.masks[0, 0].copy()

    def set_opponent(self, opponent) -> None:
        """Changes player 1's policy from the next episode on (see
        :meth:`pikapro.rl.VecEnv.set_opponent`): a built-in policy or a
        mixture of them (``"external"`` raises ``ValueError``)."""
        _check_builtin(opponent)
        self.env.set_opponent(opponent)
