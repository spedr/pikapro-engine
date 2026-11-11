"""Evaluation: win rates over mirrored games, with Wilson intervals, and
poke-env-style cross-evaluation tables.

* Built-in policies (``"random"``, ``"max_base_power"``,
  ``"simple_heuristics"``) play in Rust (``pikapro_rl::eval``): every episode
  is played twice with the same battle seed, the policies swapping seats
  (and so teams), so luck of the draw cancels.
* A policy of your own is a callable over *batched* observations and
  masks: one call per step for all battles. It plays on a
  :class:`pikapro.rl.VecEnv` as player 0; each episode is played twice, the
  second time with the teams swapped (the agent keeps its seat; seats
  don't matter to the simulator beyond the teams). The opponent is a
  built-in policy or another callable (self-play against a frozen
  checkpoint). An opponent callable sees everything from player 1's side:
  its ``step`` rows swapped, and an ``env`` whose per-player helpers
  (``last``, ``sample_actions``, ``view``, ``decode``, ``describe``,
  ``side_of``) take player 0 to be itself::

      def policy(step, env):              # -> (n, actions_per_player) ints
          logits = net(torch.as_tensor(step.obs[:, 0]))
          logits[~torch.as_tensor(step.masks[:, 0])] = -inf
          return logits.argmax(-1).numpy()

      r = rl.eval.evaluate(policy, "random", "championsvgc", episodes=500,
                           observation="basic", masks="joint")
      print(r)   # 0.912 [0.887, 0.932] (912-88-0)
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Callable, Dict, List, Optional, Sequence, Union

import numpy as np

from .. import _pikapro
from . import END_REASONS, _opponent_names, _player, _teams_arg

__all__ = ["CrossTable", "WinRecord", "cross_evaluate", "evaluate"]

Policy = Union[str, Callable[[Any, Any], Any]]

#: ``end_reason`` of a battle that failed.
_ERROR = END_REASONS.index("error")


@dataclass
class WinRecord:
    """One policy's games against another. :attr:`errors` counts the
    episodes whose battle failed, which are in no other count."""

    wins: int = 0
    losses: int = 0
    ties: int = 0
    #: games that ended at the format's turn limit or the env's
    #: ``max_turns`` (counted as ties)
    turn_limit: int = 0
    turns: int = 0
    #: episodes whose battle failed (``end_reason`` 5, ``"error"``: a
    #: panic in the engine or in an encoder), in none of the other counts
    #: and in no rate, as a failed game is in no count of Rust's
    #: ``pikapro_rl::eval::Record`` (there it fails the whole evaluation)
    errors: int = 0

    @property
    def games(self) -> int:
        """Games played to a result: wins, losses and ties (not
        :attr:`errors`)."""
        return self.wins + self.losses + self.ties

    @property
    def win_rate(self) -> float:
        """Wins plus half the ties, per game."""
        n = self.games
        return 0.5 if n == 0 else (self.wins + 0.5 * self.ties) / n

    def wilson(self, z: float = 1.96):
        """An interval for :attr:`win_rate` at the normal quantile ``z``
        (1.96 for 95%), computed by Rust's ``pikapro_rl::eval::Record::wilson``.
        A tie is neither a win nor a loss, so it is the Wilson score
        interval of the win share of the decisive games, mapped onto the
        score rate by the share of ties (``0.5 + (x - 0.5) * decisive /
        games``). Without ties it is the plain Wilson interval; ``(0, 1)``
        when no game was decisive."""
        return _pikapro._wilson(self.wins, self.losses, self.ties, z)

    @property
    def significantly_better(self) -> bool:
        """The 95% interval lies above one half."""
        return self.wilson()[0] > 0.5

    def flipped(self) -> "WinRecord":
        """The same games from the opponent's side (wins and losses
        swapped)."""
        return WinRecord(self.losses, self.wins, self.ties, self.turn_limit, self.turns, self.errors)

    def __str__(self) -> str:
        lo, hi = self.wilson()
        errors = f", {self.errors} errors" if self.errors else ""
        return f"{self.win_rate:.3f} [{lo:.3f}, {hi:.3f}] ({self.wins}-{self.losses}-{self.ties}{errors})"


@dataclass
class CrossTable:
    """``records[i][j]``: policy ``i``'s record against ``j`` (``None`` on
    the diagonal)."""

    names: List[str]
    records: List[List[Optional[WinRecord]]]

    def get(self, a: str, b: str) -> Optional[WinRecord]:
        """The record of policy ``a`` against policy ``b`` (``None`` when
        ``a == b``)."""
        return self.records[self.names.index(a)][self.names.index(b)]

    def win_rates(self) -> Dict[str, Dict[str, Optional[float]]]:
        """poke-env's ``cross_evaluate`` shape: ``[a][b]`` is ``a``'s win
        rate against ``b``."""
        return {
            a: {b: (None if r is None else r.win_rate) for b, r in zip(self.names, row)}
            for a, row in zip(self.names, self.records)
        }

    def __str__(self) -> str:
        w = max(8, *(len(n) for n in self.names))
        lines = [" " * w + "".join(f"  {n:>24}" for n in self.names)]
        for n, row in zip(self.names, self.records):
            cells = "".join(f"  {'-' if r is None else str(r).split(' (')[0]:>24}" for r in row)
            lines.append(f"{n:<{w}}{cells}")
        return "\n".join(lines)


def cross_evaluate(
    policies: Sequence[str],
    format: str,
    episodes: int,
    *,
    teams=None,
    seed: int = 0,
    threads: Optional[int] = None,
    hp: str = "percent",
    batch: int = 0,
) -> CrossTable:
    """Every built-in policy against every other over ``episodes``
    episodes, each played twice with seats swapped (``2 * episodes``
    games per pairing), in Rust. ``batch``: battles held at once (0:
    every episode, up to 4,096); the results don't depend on it. For
    poke-env ``Player`` objects, see
    :func:`pikapro.compat.poke_env.cross_evaluate`."""
    names = list(policies)
    raw = _pikapro._cross_evaluate(names, format, _teams_arg(format, teams), episodes, seed, threads, hp, batch)
    records = [[None if r is None else WinRecord(*r) for r in row] for row in raw]
    return CrossTable(names, records)


def _call(policy: Callable, step, env, player: int) -> np.ndarray:
    a = np.asarray(policy(step, env) if player == 0 else policy(_Player1(step), _Player1Env(env)))
    per = env.actions_per_player
    return a.reshape(env.n, per)


class _Player1:
    """A step seen from player 1: every per-player array's rows swapped, so
    an opponent callable reads ``[:, 0]`` as its own."""

    def __init__(self, step):
        self._step = step

    def __getattr__(self, name):
        v = getattr(self._step, name)
        if name in ("obs", "masks", "min_switches", "reward", "needs_action", "status", "final_obs"):
            return v[:, ::-1]
        return v


def _other(player: int) -> int:
    return 1 - _player(player)


class _Player1Env:
    """The environment seen from player 1, what an opponent callable gets
    as ``env``: the helpers that take a player or return per-player rows
    are swapped, so player 0 is the opponent itself. Stepping, resetting
    and rebinding are :func:`evaluate`'s alone."""

    _REFUSED = ("step", "reset", "bind", "set_opponent", "buffers")

    def __init__(self, env):
        self._env = env

    def __getattr__(self, name):
        if name in self._REFUSED:
            raise AttributeError(f"an opponent callable can't use env.{name}: evaluate() steps the environment")
        return getattr(self._env, name)

    @property
    def last(self):
        return _Player1(self._env.last)

    def sample_actions(self, rng: np.random.Generator, players: Sequence[int] = (0, 1)) -> np.ndarray:
        return self._env.sample_actions(rng, players=[_other(p) for p in players])[:, ::-1]

    def view(self, i: int, player: int = 0):
        return self._env.view(i, _other(player))

    def decode(self, i: int, player: int, actions):
        return self._env.decode(i, _other(player), actions)

    def describe(self, i: int, player: int, actions) -> str:
        return self._env.describe(i, _other(player), actions)

    def side_of(self, i: int, player: int) -> int:
        return self._env.side_of(i, _other(player))


#: Steps in a row in which a battle's asked players were all rejected
#: before :func:`evaluate` calls it stalled.
_STALL_REJECTS = 100


def _stall_bound(env) -> int:
    """Steps a battle may take without ending an episode before
    :func:`evaluate` calls it stalled: far more than a battle to the turn
    limit (Showdown's 1,000 turns, or ``max_turns``) takes."""
    turns = env.config.get("max_turns") or 1000
    return 20 * int(turns) + 1000


def _play(env, policy: Callable, opponent: Optional[Callable], episodes: int, record: WinRecord) -> None:
    """Plays until episodes ``0..episodes`` have ended, adding them to
    ``record`` from player 0's side."""
    n, per = env.n, env.actions_per_player
    step = env.reset(seed=env.config["seed"])
    # In same-step mode a done row's `episode` is the next one already.
    same_step = step.final_episode is not None
    seen = np.zeros(episodes, dtype=bool)
    actions = np.full((n, 2, per), -1, dtype=np.int64)
    since_end = np.zeros(n, dtype=np.int64)
    rejected = np.zeros(n, dtype=np.int64)
    bound = _stall_bound(env)
    while not seen.all():
        actions[:, 0] = _call(policy, step, env, 0)
        if opponent is not None:
            actions[:, 1] = _call(opponent, step, env, 1)
        step = env.step(actions)
        done = step.terminated | step.truncated
        _check_stall(step, done, since_end, rejected, bound)
        for i in np.flatnonzero(done):
            e = int(step.final_episode[i] if same_step else step.episode[i])
            if e >= episodes or seen[e]:
                continue
            seen[e] = True
            if step.end_reason[i] == _ERROR:
                # The battle failed: no result to count.
                record.errors += 1
                continue
            w = int(step.winner[i])
            if w == 0:
                record.wins += 1
            elif w == 1:
                record.losses += 1
            else:
                record.ties += 1
            if step.end_reason[i] in (2, 4):
                record.turn_limit += 1
        # An episode that failed to start in the step that ended the one
        # before it (same-step autoreset) has no row of its own: the
        # environment's errors name it. (The failed episodes that have a
        # row are among them too, and already seen.)
        for _, e, _ in env.take_errors():
            if e < episodes and not seen[e]:
                seen[e] = True
                record.errors += 1


def _check_stall(step, done, since_end: np.ndarray, rejected: np.ndarray, bound: int) -> None:
    """Raises if a battle has stopped moving: its asked players rejected
    :data:`_STALL_REJECTS` steps in a row (a policy returning ``-1`` or
    illegal actions for a player it is asked about), or no episode ended
    in ``bound`` steps."""
    status = step.status
    accepted = ((status == 1) | (status == 2)).any(axis=1)
    all_rejected = (status == -1).any(axis=1) & ~accepted & ~done
    rejected[:] = np.where(all_rejected, rejected + 1, 0)
    since_end[:] = np.where(done, 0, since_end + 1)
    stuck = np.flatnonzero(rejected >= _STALL_REJECTS)
    if stuck.size:
        i = int(stuck[0])
        players = [p for p in (0, 1) if status[i, p] == -1]
        raise RuntimeError(
            f"evaluate stalled: battle {i} (episode {int(step.episode[i])}) had player(s) {players} rejected "
            f"{_STALL_REJECTS} steps in a row. A policy must return an allowed action for every player it is "
            f"asked about (step.needs_action, step.masks); an opponent callable reads its own rows at [:, 0]."
        )
    slow = np.flatnonzero(since_end > bound)
    if slow.size:
        i = int(slow[0])
        raise RuntimeError(
            f"evaluate stalled: battle {i} (episode {int(step.episode[i])}) ended no episode in {bound} steps"
        )


def _check_env_kwargs(opponent, env_kwargs: Dict[str, Any]) -> None:
    """Refuses what :func:`evaluate` can't evaluate correctly on a
    :class:`pikapro.rl.VecEnv`."""
    if not callable(opponent) and "external" in _opponent_names(opponent):
        raise ValueError(
            "evaluate plays both players itself: pass the opponent as a callable (step, env) -> actions "
            "instead of 'external'"
        )
    if env_kwargs.get("series") is not None:
        raise ValueError(
            "evaluate plays each episode's teams twice, mirrored, and a series carries teams from game to game: "
            "series= isn't supported (evaluate single games, or play series on a VecEnv yourself)"
        )


def evaluate(
    policy: Policy,
    opponent: Policy = "random",
    format: str = "championsvgc",
    episodes: int = 100,
    *,
    teams=None,
    seed: int = 0,
    threads: Optional[int] = None,
    n: Optional[int] = None,
    mirrored: bool = True,
    hp: str = "percent",
    **env_kwargs,
) -> WinRecord:
    """``policy``'s record against ``opponent`` over ``episodes`` episodes
    (twice each when ``mirrored``). Each is a built-in name or a callable
    ``(step, env) -> actions``: ``step`` is the :class:`pikapro.rl.Step` (an
    opponent callable sees the players' rows swapped, so ``[:, 0]`` is its
    own), and the actions are ``(n, actions_per_player)`` ints (``(n,)``
    with one action per player). Two built-in names play in Rust
    (:func:`cross_evaluate`, ``n`` battles at once), always mirrored and
    with no environment options: ``mirrored=False`` raises ``ValueError``
    and ``env_kwargs`` raise ``TypeError`` there. Otherwise the games play
    on a :class:`pikapro.rl.VecEnv` of ``n`` battles (default ``min(episodes,
    256)``) built with ``env_kwargs`` (``observation``, ``masks``,
    ``max_turns``, ...); an episode whose battle fails there is counted in
    :attr:`WinRecord.errors` (not played again), and a failure in Rust
    raises ``RuntimeError``.

    ``"external"`` (alone or in a mixture) and ``series=`` raise
    ``ValueError``: pass a callable opponent for self-play, and evaluate
    series on a :class:`pikapro.rl.VecEnv` yourself. Either autoreset mode
    works. If a battle stops moving (its asked players' actions rejected
    100 steps in a row, or no episode ended in ``20 * max_turns + 1000``
    steps, 1,000 turns without ``max_turns``), ``RuntimeError`` says
    which."""
    if isinstance(episodes, (bool, np.bool_)):
        raise TypeError("episodes is an int, not a bool")
    if not isinstance(episodes, (int, np.integer)) or episodes < 0:
        raise ValueError(f"episodes must be a non-negative int, got {episodes!r}")
    _check_env_kwargs(policy, env_kwargs)
    _check_env_kwargs(opponent, env_kwargs)
    if isinstance(policy, str) and isinstance(opponent, str):
        if env_kwargs:
            raise TypeError(
                f"two built-in policies play in Rust, which takes no environment options: {sorted(env_kwargs)}"
            )
        if not mirrored:
            raise ValueError("two built-in policies always play mirrored games (2 * episodes)")
        t = cross_evaluate(
            [policy, opponent], format, episodes, teams=teams, seed=seed, threads=threads, hp=hp, batch=n or 0
        )
        return t.records[0][1] or WinRecord()
    from . import VecEnv

    if isinstance(policy, str):
        # A built-in policy against a callable: the callable's record, flipped.
        return evaluate(opponent, policy, format, episodes, teams=teams, seed=seed, threads=threads, n=n,
                        mirrored=mirrored, hp=hp, **env_kwargs).flipped()  # fmt: skip
    if episodes == 0:
        return WinRecord()  # as two built-in policies give
    pairs = [list(p) for p in _pikapro._episode_teams(_teams_arg(format, teams), format, episodes, seed)]
    passes = [pairs, [p[::-1] for p in pairs]] if mirrored else [pairs]
    record = WinRecord()
    for pass_pairs in passes:
        env = VecEnv(
            format,
            n=n or min(episodes, 256),
            threads=threads,
            seed=seed,
            teams=pass_pairs,
            opponent="external" if callable(opponent) else opponent,
            hp=hp,
            **env_kwargs,
        )
        _play(env, policy, opponent if callable(opponent) else None, episodes, record)
    return record
