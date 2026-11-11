"""pikapro.rl: a fast, text-free reinforcement-learning environment.

An environment, not a bot: it answers "what happened, what can I do, what
did I get". Which features to encode, the network and the algorithm are
yours. The guide is ``docs/RL_GUIDE.md``.

* :class:`VecEnv` - the native fast path: ``n`` battles stepped per call
  on Rust threads with the GIL released, observations, masks, rewards and
  termination for both players written into numpy buffers allocated (or
  bound, :meth:`VecEnv.bind`) once.
* Observations: ``"basic"``, a small float vector for a plain network.
  For features of your own, write a function over :class:`BattleView`
  (:func:`embed_all`), or an encoder in Rust (``docs/python/encoders.md``).
* :class:`ChannelSession` - one battle read from Showdown's text (a
  player's channel and requests), with the view, observation and mask at
  each decision, and actions as ``/choose`` text:
  for playing on a Showdown server (:mod:`pikapro.showdown`);
  :class:`ChannelSeries` carries a best-of series from one game's session
  to the next, as the environment's ``series`` mode plays it.
* :class:`BattleView` - one player's view, read-only and poke-env-shaped:
  what the player has seen, for a Python ``embed_battle``.
* :mod:`pikapro.rl.teams` - tournament team pools (pokepaste lists).
* :mod:`pikapro.rl.eval` - win rates over mirrored games, cross-evaluation.
* Adapters (optional dependencies, imported lazily):
  :mod:`pikapro.rl.gym` (Gymnasium ``VectorEnv`` and ``Env``),
  :mod:`pikapro.rl.pettingzoo` (a ``ParallelEnv``), :mod:`pikapro.rl.sb3`
  (a Stable-Baselines3 ``VecEnv`` for MaskablePPO).

Quick start::

    import numpy as np
    import pikapro.rl as rl

    env = rl.VecEnv("championsvgc", n=256, opponent="simple_heuristics")
    out = env.reset(seed=0)
    rng = np.random.default_rng(0)
    for _ in range(100):
        out = env.step(env.sample_actions(rng))   # your policy here
        # out.obs, out.masks, out.reward, out.terminated, ...
"""

from __future__ import annotations

import operator
from typing import Any, Callable, Dict, Iterator, List, Literal, Optional, Sequence, Tuple, Union

import numpy as np

import pikapro as _pikapro_pkg

from .. import ACTION_SPACE_VERSION, _pikapro  # noqa: F401
from .._pikapro import TeamPool, relative_target  # noqa: F401
from .view import BattleView, MoveView, PokemonView  # noqa: F401
from .session import ChannelSeries, ChannelSession  # noqa: F401

__all__ = [
    "ACTION_KINDS",
    "ACTION_SPACE_VERSION",
    "ACTION_TABLE_COLUMNS",
    "BUILTIN_POLICIES",
    "BattleView",
    "ChannelSeries",
    "ChannelSession",
    "END_REASONS",
    "MoveView",
    "OBSERVATIONS",
    "OPPONENTS",
    "PokemonView",
    "RANDOM_ACTION",
    "STATUS",
    "Step",
    "TeamPool",
    "VecEnv",
    "action_table",
    "embed_all",
    "relative_target",
    "replay",
]

#: An action value that lets the environment choose: a uniformly random
#: choice among the ones the player's mask allows.
RANDOM_ACTION = -2

#: ``end_reason`` codes, by value.
END_REASONS = ["none", "all_fainted", "turn_limit", "forfeit", "max_turns", "error", "endless_battle"]

#: ``status`` codes: what became of a player's action.
STATUS = {"ignored": 0, "submitted": 1, "rejected": -1, "unavailable": 2}

#: The built-in policies, poke-env's three baseline players, played in
#: Rust: the one list the environment's opponents, :mod:`pikapro.rl.eval`
#: and :mod:`pikapro.showdown` take.
BUILTIN_POLICIES = ("random", "max_base_power", "simple_heuristics")

#: The built-in opponents (:data:`BUILTIN_POLICIES`) and ``"external"``
#: (the caller plays player 1).
OPPONENTS = [*BUILTIN_POLICIES, "external"]

#: The values of ``VecEnv(observation=...)``: the ``basic`` float vector.
OBSERVATIONS = ["basic"]


#: The columns of :func:`action_table`, by offset.
ACTION_TABLE_COLUMNS = ("kind", "move_slot", "target", "gimmick", "switch", "preview")

#: The values of :func:`action_table`'s ``kind`` column.
ACTION_KINDS = {"move": 0, "switch": 1, "pass": 2, "preview": 3}

_ACTION_TABLES: Dict[str, Dict[str, np.ndarray]] = {}


def action_table(format: str) -> Dict[str, np.ndarray]:
    """What every factored action index of ``format`` does, as static
    integer columns a policy can gather from (no per-step cost).

    Row ``i < slot_size`` is slot action ``i`` (a factored mask row's
    ``slot * slot_size + i``), row ``slot_size + k`` team-preview order
    ``k`` (the factored mask's preview part, in the same order). The
    columns (:data:`ACTION_TABLE_COLUMNS`), each an int16 array over the
    rows, are -1 where they don't apply:

    * ``kind`` - :data:`ACTION_KINDS`: 0 move, 1 switch, 2 pass, 3 team
      preview;
    * ``move_slot`` - the move's slot, 0 to 3;
    * ``target`` - the move's target code: 0 none, 1 and 2 the foes in
      slots 0 and 1, 3 and 4 the user's side's slots 0 and 1 (absolute:
      one is the user; :func:`relative_target`); always 0 in singles;
    * ``gimmick`` - 0 none, ``i + 1`` the space's ``i``-th gimmick
      (:attr:`pikapro.ActionSpace.gimmicks`);
    * ``switch`` - ``k`` for switch action ``k``: own row ``k`` with
      ``switch_by="row"`` (the default), team position ``k`` with
      ``switch_by="position"``;
    * ``preview`` - the team-preview order's index.

    Also ``"preview_orders"``, int16 ``(preview_size, bring)``: each
    order's team positions (:meth:`pikapro.ActionSpace.preview_order`). The
    arrays are read-only. With joint masks, a choice ``j < joint_size``
    is ``a0 * slot_size + a1`` (:meth:`pikapro.ActionSpace.split`)."""
    if format not in _ACTION_TABLES:
        info = _pikapro._action_table(format)
        table = info["table"]
        out = {name: np.ascontiguousarray(table[:, k]) for k, name in enumerate(info["columns"])}
        out["preview_orders"] = info["preview_orders"]
        for a in out.values():
            a.flags.writeable = False
        _ACTION_TABLES[format] = out
    return dict(_ACTION_TABLES[format])


# -- the vector environment -----------------------------------------------------


class Step:
    """What :meth:`VecEnv.reset` and :meth:`VecEnv.step` return: the
    environment's buffers, by attribute (and item). **The arrays are the
    environment's own, overwritten by the next call**: copy what you keep.

    * ``obs`` - float32 ``(n, 2, size)``: each player's observation
      (:attr:`VecEnv.observation` names its values). Axis 1 is the player,
      and each player's view is ego-centric (its own rows first).
    * ``masks`` bool ``(n, 2, width)``; ``min_switches`` uint8 ``(n, 2)``.
    * ``reward`` float32 ``(n, 2)``; ``terminated``, ``truncated`` bool
      ``(n,)``; ``end_reason`` uint8 ``(n,)`` (:data:`END_REASONS`);
      ``winner`` int8 ``(n,)`` (``-1`` none, ``0``/``1`` the winning
      player, ``2`` a tie).
    * ``needs_action`` bool ``(n, 2)``; ``status`` int8 ``(n, 2)``
      (:data:`STATUS`); ``agent_side`` uint8 ``(n,)``; ``episode`` uint64
      ``(n,)``.
    * series mode: ``series`` uint8 ``(n, 4)`` (game, player 0's wins,
      player 1's wins, result: 1 player 0 won the series, 2 player 1).
      One row per battle, in player 0's frame (the agent's, and what
      ``ChannelSession.series_row`` gives each player for itself): for
      player 1, swap the wins and the result, or read
      ``BattleView.series``, which does.
    * same-step autoreset: ``final_obs`` (as ``obs``), ``final_episode``,
      ``final_turns`` and ``final_series``: the ended episodes' last
      observations (rows of battles that didn't end are stale).
    """

    _FIELDS = (
        "obs",
        "masks",
        "min_switches",
        "reward",
        "terminated",
        "truncated",
        "end_reason",
        "winner",
        "needs_action",
        "status",
        "agent_side",
        "episode",
        "series",
        "final_obs",
        "final_episode",
        "final_turns",
        "final_series",
    )
    __slots__ = _FIELDS

    # For type checkers (the slots above hold them; a buffer this
    # environment doesn't have is None).
    obs: np.ndarray
    masks: np.ndarray
    min_switches: np.ndarray
    reward: np.ndarray
    terminated: np.ndarray
    truncated: np.ndarray
    end_reason: np.ndarray
    winner: np.ndarray
    needs_action: np.ndarray
    status: np.ndarray
    agent_side: np.ndarray
    episode: np.ndarray
    series: Optional[np.ndarray]
    final_obs: Optional[np.ndarray]
    final_episode: Optional[np.ndarray]
    final_turns: Optional[np.ndarray]
    final_series: Optional[np.ndarray]

    def __init__(self, buffers: Dict[str, np.ndarray]):
        for f in self._FIELDS:
            object.__setattr__(self, f, None)
        for k, v in buffers.items():
            object.__setattr__(self, k, v)

    @property
    def done(self) -> np.ndarray:
        """``terminated | truncated`` (a new array)."""
        return self.terminated | self.truncated

    def __getitem__(self, key: str):
        return getattr(self, key)

    def keys(self) -> List[str]:
        """The names of the buffers this step has (``None`` ones left
        out)."""
        return [f for f in self._FIELDS if getattr(self, f) is not None]

    def __iter__(self) -> Iterator[str]:
        return iter(self.keys())

    def as_dict(self) -> Dict[str, Any]:
        """The buffers by name, as a dict (the same arrays, not copies)."""
        return {k: getattr(self, k) for k in self.keys()}

    def __repr__(self) -> str:
        return f"Step({', '.join(self.keys())})"


def _ots_probability(ots) -> float:
    if ots is True:
        return 1.0
    if ots is False or ots is None:
        return 0.0
    p = float(ots)
    if not 0.0 <= p <= 1.0:
        raise ValueError(f"ots is False, True or a probability, not {ots!r}")
    return p


def _series_games(series) -> Optional[int]:
    if series is None:
        return None
    if isinstance(series, str):
        s = series.lower()
        if s.startswith("bo") and s[2:].isdigit():
            return int(s[2:])
        raise ValueError(f"series must be None, 'bo3', 'bo5', 'bo7', 'bo9' or a number of games, not {series!r}")
    if isinstance(series, (bool, np.bool_)):
        raise TypeError("series is an int, not a bool")
    try:
        # Not int(): a float such as 3.9 isn't a number of games.
        return operator.index(series)
    except TypeError:
        raise ValueError(f"series must be None, 'bo3', 'bo5', 'bo7', 'bo9' or a number of games, not {series!r}") from None


def _teams_arg(format: str, teams):
    if teams is None:
        return _pikapro_pkg.RandomTeams(format, "sets")
    if isinstance(teams, str):
        # The names BattlePool takes too: one mapping, in Rust.
        return _pikapro._named_teams(format, teams)
    return teams


def _opponent_names(opponent) -> List[Any]:
    """The policy names in an opponent spec: a name, or a mixture (a dict
    or a list of ``(name, weight)``); none for anything else."""
    if isinstance(opponent, str):
        return [opponent]
    if isinstance(opponent, dict):
        return list(opponent)
    try:
        return [p[0] for p in opponent]
    except (TypeError, IndexError, KeyError):
        return []


def _player(player) -> int:
    """A player index, 0 or 1 (``ValueError`` otherwise, ``TypeError``
    for a bool)."""
    if isinstance(player, (bool, np.bool_)):
        raise TypeError("player is an int, not a bool")
    if not isinstance(player, (int, np.integer)) or player not in (0, 1):
        raise ValueError(f"player must be 0 or 1, got {player!r}")
    return int(player)


class VecEnv:
    """``n`` battles of ``format`` stepped together: the native fast path.

    Rows are by *player*: player 0 is the agent (its actions always come
    from the caller), player 1 its opponent (a built-in policy, or the
    caller with ``opponent="external"``). Each call is one request round;
    observations, masks and rewards come out for both players, so self-play
    is one batched forward pass.

    Options:

    * ``threads`` (default: every core), ``seed`` (episode ``e`` of battle
      ``e % n`` plays with a battle seed hashed from ``(seed, e)``, which
      :meth:`record` gives; any thread count gives the same episodes, and
      base seeds ``s`` and ``s + 1`` give unrelated ones).
    * ``teams``: ``None`` (random ``"sets"`` teams), ``"sets"`` /
      ``"uniform"`` / a :class:`pikapro.RandomTeams`, a pair of team texts
      (fixed teams), a list of pairs (episode ``e`` plays pair ``e %
      len``), a :class:`TeamPool` (both players draw from it: a tournament
      pool, :mod:`pikapro.rl.teams`), or two pools (one per player).
    * ``opponent``: ``"random"``, ``"max_base_power"``,
      ``"simple_heuristics"``, ``"external"``, or a mixture drawn per
      episode (``{"random": 1, "simple_heuristics": 3}``); changeable
      between calls with :meth:`set_opponent`.
    * ``reward``: ``"terminal"`` (+1 win, -1 loss, 0 tie) or ``"ko"``
      (terminal plus ``ko_weight``, 0.1 by default, per foe knocked out,
      minus per own: the return is the result plus ``ko_weight`` times the
      final KO margin). ``ko_weight`` without ``reward="ko"`` raises
      ``ValueError``.
    * ``max_turns``: cut an episode (``truncated``, end reason 4) past it.
    * ``observation``: :data:`OBSERVATIONS`: ``"basic"`` (a float vector
      for a plain network, see ``docs/python/encoders.md``).
    * ``masks``: ``"factored"`` (each slot's own actions, then the team
      preview orders; actions ``(n, 2, active)``) or ``"joint"`` (every
      joint choice; actions ``(n, 2)``).
    * ``switch_by``: ``"row"`` (switch ``k`` sends in own observation row
      ``k``, stable for the battle) or ``"position"`` (Showdown's team
      position).
    * ``autoreset``: ``"next_step"`` (the step that ends an episode returns
      its terminal observation; the next step starts the next episode and
      ignores the battle's actions) or ``"same_step"`` (the next episode
      starts at once; the terminal observation is in ``final_obs``).
    * ``ots``: Open Team Sheets: ``False``, ``True`` or a probability per
      episode. Only in a format with team preview, where Showdown shows
      the sheets (``ValueError`` otherwise).
      ``side_randomization``: player 0 plays p1 or p2 at random.
    * ``ots_view``: who sees the open sheets: ``"both"`` (the default) or
      ``"player0"`` (player 0 alone; player 1 plays as with closed
      sheets), for measuring what a sheet is worth.
    * ``series``: ``None``, or ``"bo3"`` / ``"bo5"`` / ``"bo7"`` /
      ``"bo9"`` or the number of games (Showdown's best-of rules, open
      sheets, so a format with team preview; the score in
      ``Step.series``).
    * ``observe_builtin``: encode a built-in opponent's view too (its rows
      are zeros otherwise).
    * ``preview``: ``"learned"`` (the player chooses the team order),
      ``"random"`` or ``"fixed"``. ``hp``: foe HP as ``"percent"`` (the
      ladder) or ``"exact"``. ``record``: keep each episode's choices for
      :meth:`record`.

    :attr:`config` gives the options back by these names:
    ``VecEnv(**env.config)`` makes an environment configured the same.
    """

    def __init__(
        self,
        format: str,
        n: int = 1,
        *,
        threads: Optional[int] = None,
        seed: int = 0,
        teams=None,
        opponent: Union[str, Dict[str, float], Sequence] = "random",
        reward: str = "terminal",
        ko_weight: Optional[float] = None,
        max_turns: Optional[int] = None,
        observation: str = "basic",
        masks: str = "factored",
        switch_by: str = "row",
        autoreset: str = "next_step",
        ots: Union[bool, float] = False,
        ots_view: str = "both",
        side_randomization: bool = False,
        series: Union[None, Literal["bo3", "bo5", "bo7", "bo9"], int] = None,
        observe_builtin: bool = False,
        preview: str = "learned",
        hp: str = "percent",
        record: bool = False,
        shards_per_thread: int = 8,
        spin_us: int = 50,
    ):
        if ko_weight is not None and reward != "ko":
            raise ValueError(f"ko_weight is the 'ko' reward's weight: pass reward='ko' with it (reward={reward!r})")
        teams = _teams_arg(format, teams)
        self._env = _pikapro._VecEnv(
            format,
            n,
            teams,
            threads=threads,
            seed=seed,
            opponent=opponent,
            reward=reward,
            ko_weight=0.1 if ko_weight is None else ko_weight,
            max_turns=max_turns,
            observation=observation,
            masks=masks,
            switch_by=switch_by,
            autoreset=autoreset,
            ots=_ots_probability(ots),
            ots_view=ots_view,
            randomize_sides=side_randomization,
            series=_series_games(series),
            observe_builtin=observe_builtin,
            preview=preview,
            hp=hp,
            record=record,
            shards_per_thread=shards_per_thread,
            spin_us=spin_us,
        )
        self.format = format
        self.n = n
        # What `config` gives back that the Rust class doesn't keep.
        self._options = {
            "teams": teams,
            "opponent": opponent,
            "shards_per_thread": shards_per_thread,
            "spin_us": spin_us,
        }
        self.space = self._env.space
        self.actions_per_player = self._env.actions_per_player
        self.mask_width = self._env.mask_width
        self._factored = self._env.config["masks"] == "factored"
        info = self._env.observation
        #: The observation: its ``kind``, ``size`` and ``columns`` (each
        #: value's name, by offset).
        self.observation = info
        specs = {name: (tuple(shape), dtype) for name, shape, dtype in self._env.buffer_specs()}
        self.buffer_specs = specs
        self.obs_shape = specs["obs"][0]
        self._full = (n, 2, self.actions_per_player)
        self._acts = np.full((n, 2, self.actions_per_player), -1, dtype=np.int64)
        self._refresh()

    # -- buffers ---------------------------------------------------------------

    def _refresh(self) -> None:
        self._step = Step(self._env.buffers())

    def bind(self, **arrays: np.ndarray) -> "VecEnv":
        """Binds output buffers by name (see :attr:`buffer_specs` for names,
        shapes and dtypes): each must have the buffer's dtype and number of
        values, be C-contiguous and writeable, and share no memory with
        another buffer (``ValueError``). Steps then write straight
        into them, with no copy: for instance numpy views of pinned torch
        tensors (``torch.empty(shape, dtype=torch.float32).pin_memory()
        .numpy()``), which ``tensor.to("cuda", non_blocking=True)`` then
        moves. Buffers not given stay the environment's. All or nothing:
        if one array is refused, none is bound (the buffers stay as they
        were before the call)."""
        before = self._env.buffers()
        bound = {k: v for k, v in before.items() if k not in arrays}
        given = list(arrays.items())
        for k, (name, array) in enumerate(given):
            if not isinstance(array, np.ndarray):
                continue  # the Rust side names the type it wants
            others = list(bound.items()) + given[:k]
            for other, b in others:
                if isinstance(b, np.ndarray) and np.may_share_memory(array, b):
                    raise ValueError(f"bind: {name} shares memory with the {other} buffer; each needs its own")
        done: List[str] = []
        try:
            for name, array in arrays.items():
                self._env.bind(name, array)
                done.append(name)
        except BaseException:
            for name in done:  # back to the buffers the call found
                self._env.bind(name, before[name])
            raise
        finally:
            self._refresh()
        return self

    @property
    def buffers(self) -> Dict[str, np.ndarray]:
        """The bound buffers by name (the arrays steps write into)."""
        return dict(self._env.buffers())

    # -- stepping --------------------------------------------------------------

    def reset(self, seed: Optional[int] = None) -> Step:
        """Starts a new episode in every battle. With ``seed``, episodes
        are numbered from 0 again with that base seed (reproducible);
        without, each battle starts its next episode."""
        self._env.reset(seed)
        return self._step

    def _actions(self, actions) -> np.ndarray:
        a = np.asarray(actions)
        if a.dtype.kind not in "iu":
            raise TypeError(f"actions must be integers, got dtype {a.dtype}")
        n, per = self.n, self.actions_per_player
        if a.shape == (n, 2, per) or (per == 1 and a.shape == (n, 2)):
            return np.ascontiguousarray(a, dtype=np.int64).reshape(n, 2, per)
        # Player 0's actions only (player 1 is built-in, or waits).
        if a.shape == (n, per) or (per == 1 and a.shape == (n,)):
            full = self._acts
            full.fill(-1)
            full[:, 0, :] = a.reshape(n, per)
            return full
        both = f"({n}, 2, {per})" + (f" or ({n}, 2)" if per == 1 else "")
        one = f"({n}, {per})" + (f" or ({n},)" if per == 1 else "")
        raise ValueError(f"actions must have shape {both} (both players) or {one} (player 0), got {a.shape}")

    def step(self, actions) -> Step:
        """One request round of every battle. ``actions``: ints, ``(n, 2,
        actions_per_player)`` for both players (``(n, 2)`` with one action
        per player), or player 0's alone ``(n, actions_per_player)`` (``(n,)``
        with one). Values for players that aren't asked are ignored;
        :data:`RANDOM_ACTION` plays a random allowed one. An int64
        C-contiguous array of the full shape goes to Rust without a copy."""
        a = actions
        if type(a) is np.ndarray and a.dtype == np.int64 and a.shape == self._full and a.flags.c_contiguous:
            self._env.step(a)
        else:
            self._env.step(self._actions(a))
        return self._step

    @property
    def last(self) -> Step:
        """The buffers as the last call left them."""
        return self._step

    def set_opponent(self, opponent, which: Optional[Sequence[int]] = None) -> None:
        """Changes who plays player 1 in the given battles (all by default),
        from each one's next episode on (in series mode, its next series):
        a name, ``"external"``, or a mixture ``{name: weight}``. A curriculum is a few lines::

            if iteration == 100:
                env.set_opponent("external")   # self-play from here on
        """
        self._env.set_opponent(opponent, None if which is None else list(which))
        if which is None:
            self._options["opponent"] = opponent

    def set_pairs(self, positions: Sequence[int], pairs: Sequence[Tuple[str, str]]) -> None:
        """Replaces entries of the list of team pairs the environment was
        made with (``teams=[(p1, p2), ...]``): ``pairs[j]`` goes to position
        ``positions[j]``, for the episodes started from now on (an episode
        under way keeps its teams; in series mode, for the series started
        from now on). The list keeps its length: episode ``e``
        still plays position ``e % len``. Raises (changing nothing) for a
        team the format refuses, a position out of range, or another kind
        of team source."""
        self._env.set_pairs(list(positions), [tuple(p) for p in pairs])

    # -- helpers ---------------------------------------------------------------

    def sample_actions(self, rng: np.random.Generator, players: Sequence[int] = (0, 1)) -> np.ndarray:
        """Uniformly random allowed actions for the given players, from the
        last step's masks, in the full ``(n, 2, actions_per_player)`` shape
        (``-1`` where a player isn't asked). In factored doubles, slot 1
        draws among the actions that complete a legal choice with slot 0's
        (:meth:`pair_mask`), and a slot-0 action nothing completes is left
        to the environment (:data:`RANDOM_ACTION`). Vectorised numpy: for
        tests, baselines and exploration."""
        s = self._step
        space = self.space
        n = self.n
        out = np.full((n, 2, self.actions_per_player), -1, dtype=np.int64)
        players = list(players)
        m = s.masks[:, players]
        S = space.slot_size
        if self.actions_per_player == 1:
            a = _pikapro_pkg.sample_uniform(m, rng)
            if self._factored:
                # The factored mask puts the orders after the slot part,
                # but the action is the order's own index.
                a = np.where(a >= S, a - S, a)
            out[:, players, 0] = a
            return out
        preview = m[..., space.active * S :]
        in_preview = preview.any(axis=-1)
        ms = s.min_switches[:, players]
        a0 = _pikapro_pkg.sample_uniform(m[..., :S], rng)
        a1 = _pikapro_pkg.sample_uniform(self.pair_mask(m, ms, np.maximum(a0, 0)), rng)
        stuck = (a0 >= 0) & (a1 < 0)
        a0 = np.where(stuck, RANDOM_ACTION, a0)
        a1 = np.where(stuck, RANDOM_ACTION, np.where(a0 < 0, -1, a1))
        if preview.shape[-1]:
            pv = _pikapro_pkg.sample_uniform(preview, rng)
            a0 = np.where(in_preview, pv, a0)
            a1 = np.where(in_preview, -1, a1)
        out[:, players, 0] = a0
        out[:, players, 1] = a1
        return out

    def pair_mask(self, masks: np.ndarray, min_switches, a0) -> np.ndarray:
        """For factored masks ``(..., width)``, ``min_switches`` ``(...)``
        and slot-0 actions ``a0`` ``(...)``: the slot-1 actions that make a
        legal choice with them, bool ``(..., slot_size)`` (autoregressive
        policies)."""
        return self.space.pair_mask(masks, min_switches, a0)

    def decode(self, i: int, player: int, actions) -> "_pikapro_pkg.Choice":
        """The :class:`pikapro.Choice` ``actions`` (one player's, as
        :meth:`step` takes them) make for battle ``i``'s current request
        (switches as team positions; ``str()`` is Showdown's choice
        string)."""
        return self._env.decode(i, _player(player), list(np.asarray(actions, dtype=object).reshape(-1)))

    def describe(self, i: int, player: int, actions) -> str:
        """Showdown's choice string for :meth:`decode`."""
        return str(self.decode(i, player, actions))

    def battle(self, i: int) -> "_pikapro_pkg.Battle":
        """A copy of battle ``i`` as a :class:`pikapro.Battle` (for inspection
        and :meth:`pikapro.Battle.fork`; its event log holds only the events the
        environment hasn't consumed)."""
        return self._env.battle(i)

    def view(self, i: int, player: int = 0) -> BattleView:
        """Player ``player``'s view of battle ``i`` as a :class:`BattleView`,
        with its current mask."""
        return BattleView.from_env(self, i, _player(player))

    def _view(self, i: int, player: int) -> Dict[str, Any]:
        """Player ``player``'s current view of battle ``i`` as plain values
        (what :class:`BattleView` reads)."""
        return self._env._view(i, _player(player))

    def side_of(self, i: int, player: int) -> int:
        """The side (0: p1, 1: p2) ``player`` plays in battle ``i``'s episode."""
        return self._env.side_of(i, _player(player))

    def record(self, i: int, previous: bool = False) -> Optional[Dict[str, Any]]:
        """Battle ``i``'s current (or ``previous``) episode, with
        ``record=True``: ``episode``, ``seed``, ``teams`` (p1's and p2's,
        packed), ``agent_side`` and ``choices`` (``(side, Choice)`` in
        order). :func:`replay` plays it again through :class:`pikapro.Battle`."""
        return self._env.record(i, previous)

    @property
    def config(self) -> Dict[str, Any]:
        """The environment's resolved configuration as a dict, by the
        names the constructor takes, so ``VecEnv(**env.config)`` makes one
        configured the same: ``format``, ``n``, ``seed`` (the base seed of
        the episodes under way: :meth:`reset` with a seed changes it),
        ``teams`` (the team source, a :class:`pikapro.RandomTeams` for the
        string names), ``opponent`` (the constructor's, or the last one
        :meth:`set_opponent` gave every battle), ``reward`` (with
        ``ko_weight`` for ``"ko"``), ``observation``, ``threads``, and the
        other options."""
        c = dict(self._env.config)
        c["observation"] = self.observation["kind"]
        c["threads"] = self.threads
        c.update(self._options)
        return c

    @property
    def threads(self) -> int:
        """The number of threads stepping the battles."""
        return self._env.threads

    def _set_profile(self, on: bool = True) -> None:
        """Per-phase timing (engine, view, overlays, encoding, masks), for
        the benchmark scripts."""
        self._env.set_profile(on)

    def _take_profile(self) -> Dict[str, int]:
        """Nanoseconds per phase since the last call (``_set_profile``)."""
        return dict(self._env.take_profile())

    def take_errors(self) -> List[Tuple[int, int, str]]:
        """The battles that failed since the last call, in order, as
        ``(battle, episode, message)``: a panic (an engine bug, a policy or
        encoder that raised in Rust) or an error while stepping one ends
        its episode with ``end_reason`` 5 (``"error"``, ``truncated``, no
        winner, zeroed observation) instead of failing the call, and the
        battle starts its next episode at the next step. At most 1,024 are
        kept."""
        return [tuple(e) for e in self._env.take_errors()]

    def __len__(self) -> int:
        return self.n

    def close(self) -> None:
        """Does nothing: the threads stop when the environment is
        garbage-collected. Here for the Gymnasium-style interface."""

    def __repr__(self) -> str:
        return repr(self._env)


def replay(format: str, record: Dict[str, Any], *, log: bool = True) -> "_pikapro_pkg.Battle":
    """Plays a :meth:`VecEnv.record` again through :class:`pikapro.Battle`:
    the same teams, seed and choices give the same battle. Its
    :meth:`pikapro.Battle.view` for a side is the view the environment gave
    that side's player (the battle remembers each side's own team as its
    requests showed it, as the environment does), for an environment with
    ``hp="percent"``, no open team sheets and no series, and unless a
    choice was refused as unavailable in the episode (the record holds the
    accepted choices only). ``record(i)`` mid-episode replays the episode
    so far."""
    b = _pikapro_pkg.Battle(format, list(record["teams"]), seed=record["seed"], log=log)
    for side, choice in record["choices"]:
        b.submit(side, choice)
    return b


def embed_all(env: VecEnv, fn: Callable[[BattleView], Any], player: int = 0) -> np.ndarray:
    """An observation of your own in Python, poke-env's ``embed_battle``:
    ``fn`` on each battle's :class:`BattleView` for ``player``, stacked.
    It costs Python per battle per step; ``"basic"`` and an encoder
    written in Rust don't."""
    return np.stack([np.asarray(fn(env.view(i, player))) for i in range(env.n)])


# The submodules, as attributes (pikapro.rl.eval, pikapro.rl.teams). The
# package's `eval` is the submodule whatever the import says (Python binds
# a submodule on its package): nothing in this file calls the builtin.
from . import eval, teams  # noqa: E402,F401,A004
