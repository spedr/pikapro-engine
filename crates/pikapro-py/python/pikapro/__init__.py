"""pikapro: Python bindings for pikapro, a deterministic Pokémon battle simulator.

The engine is a port of Pokémon Showdown's simulator (Gen 9, VGC, Pokémon
Champions) in Rust. This package exposes it at close to native speed:

* :class:`Battle` - one battle: requests (typed and as Showdown's JSON),
  legal actions (as lists and numpy masks over a fixed action space),
  choices (indices, Showdown strings or :class:`Choice` objects), cheap
  :meth:`Battle.fork`, typed events and Showdown log lines for any
  perspective.
* :class:`BattlePool` - many battles stepped per call on Rust threads with
  the GIL released; masks, actions and raw state as numpy arrays.
* :class:`ActionSpace` - the stable integer indexing of actions per format.
* :mod:`pikapro.rl` - the reinforcement-learning environment: a fused vector
  environment over player views (not the omniscient state), the
  ``basic`` observation, Gymnasium / PettingZoo / Stable-Baselines3
  adapters, team pools and evaluation (``docs/RL_GUIDE.md``).

Quick start::

    import pikapro
    team = pikapro.sample_team("gen9vgc")
    b = pikapro.Battle("gen9vgc", [team, team], seed=1)
    while not b.over:
        for side in (0, 1):
            legal = b.legal_actions(side)
            if legal:
                b.submit(side, legal[0])
    print(b.winner, "\\n".join(b.log("p1")))
"""

from __future__ import annotations

import numpy as _np

from ._pikapro import (  # noqa: F401
    FIELD_COLUMNS,
    POKEMON_COLUMNS,
    REQUEST_CODES,
    SIDE_COLUMNS,
    STATUS_CODES,
    TERRAIN_CODES,
    VOLATILE_NAMES,
    WEATHER_CODES,
    ActionSpace,
    Battle,
    BattlePool,
    Choice,
    RandomPolicy,
    RandomTeams,
    Record,
    SlotAction,
    export,
    formats,
    names,
    pack,
    sample_team,
    to_id,
    validate,
)

__all__ = [
    "ACTION_SPACE_VERSION",
    "ActionSpace",
    "Battle",
    "BattlePool",
    "Choice",
    "RandomPolicy",
    "RandomTeams",
    "Record",
    "SlotAction",
    "FIELD_COLUMNS",
    "POKEMON_COLUMNS",
    "REQUEST_CODES",
    "SIDE_COLUMNS",
    "STATUS_CODES",
    "TERRAIN_CODES",
    "VOLATILE_NAMES",
    "WEATHER_CODES",
    "column",
    "export",
    "formats",
    "kind",
    "names",
    "pack",
    "random_team",
    "sample_slots",
    "sample_team",
    "sample_uniform",
    "to_id",
    "validate",
    "volatiles",
]


def _version() -> str:
    from importlib.metadata import PackageNotFoundError, version

    try:
        return version("pikapro")
    except PackageNotFoundError:  # the source tree on sys.path, not installed
        return "0+unknown"


#: The installed distribution's version (``"0+unknown"`` when the package
#: is imported from a source tree that was never installed).
__version__: str = _version()

#: The version of the action layout: :class:`ActionSpace`'s slot-action,
#: choice and team-preview indexing, and the factored and joint masks of
#: :class:`BattlePool` and :class:`pikapro.rl.VecEnv` built on it. Bumped
#: whenever an index would mean a different action; a policy trained on one
#: version must not be used with another.
ACTION_SPACE_VERSION = 1


def kind(value):
    """The variant name of a typed value: ``value`` itself for a unit
    variant (a ``str`` such as ``"Burn"``), ``value._variant`` for a
    :class:`Record`."""
    return value if isinstance(value, str) else value._variant


def volatiles(bits) -> list:
    """The names of the volatiles set in a Pokémon's volatile bits: the
    four ``volatiles0..3`` columns of the raw state arrays (32 bits each,
    as int32), or one 128-bit int. ``VOLATILE_NAMES[bit]`` names each bit
    (``""`` for an unused one)."""
    if isinstance(bits, int):
        value = bits
    else:
        words = [int(w) & 0xFFFFFFFF for w in bits]
        value = sum(w << (32 * k) for k, w in enumerate(words))
    return [VOLATILE_NAMES[b] for b in range(128) if value >> b & 1 and VOLATILE_NAMES[b]]


def column(name: str, columns=POKEMON_COLUMNS) -> int:
    """The index of a column of the raw state arrays
    (``POKEMON_COLUMNS``, ``SIDE_COLUMNS`` or ``FIELD_COLUMNS``)."""
    return columns.index(name)


def random_team(format: str, mode: str = "uniform", seed: int = 0, *, packed: bool = False, **options) -> str:
    """A random team from ``pikapro-teams``, deterministic from ``seed``.

    Teams pass Showdown's validator, except for ``mode="sets"`` in
    ``gen9randombattle`` and ``gen9randomdoublesbattle``: that is
    Showdown's random-battle team generator, ported exactly, whose teams
    nothing validates. The team for ``seed`` is Showdown's for the seed
    ``gen5,<seed in 16 hex digits>``; it takes no ``options``.

    ``mode`` is ``"uniform"`` (every legal choice drawn uniformly) or
    ``"sets"`` (sets from Showdown's random-battle data). ``options`` are
    :class:`RandomTeams`'s (``level``, ``team_size``, ``spreads``, and
    ``species``/``moves``/``abilities``/``items`` name lists). The generator
    is built once per process and reused. Returns export text, or the
    packed format with ``packed=True``.
    """
    return RandomTeams(format, mode, **options).team(seed, packed=packed)


def sample_uniform(masks: _np.ndarray, rng: _np.random.Generator) -> _np.ndarray:
    """A uniformly random True index along the last axis of a bool array;
    ``-1`` where a row has none. Vectorised: the argmax of uniform noise
    over the True entries (the False ones get ``-1``, below any noise)."""
    noise = rng.random(masks.shape, dtype=_np.float32)
    idx = _np.where(masks, noise, _np.float32(-1)).argmax(axis=-1)
    idx[~masks.any(axis=-1)] = -1
    return idx


def sample_slots(pool: BattlePool, rng: _np.random.Generator) -> _np.ndarray:
    """Random per-slot actions for every battle of ``pool``, in the
    ``(n, 2, active)`` shape :meth:`BattlePool.step` takes, from the
    jointly legal choice masks (:meth:`BattlePool.masks`).

    Slot 0 picks uniformly among its actions that some slot-1 action
    completes; slot 1 then uniformly among those (the choice mask's
    ``(slot_size, slot_size)`` block is the engine's pairing rule). Team
    preview picks a uniformly random order; ``-1`` for sides with nothing
    to decide. A Python-side random policy for benchmarking (masks out and
    actions in through numpy every step); it is not uniform over joint
    choices, and never picks an action ``step`` rejects.
    """
    space = pool.space
    masks = pool.masks()  # (n, 2, size)
    n = masks.shape[0]
    joint = masks[..., : space.joint_size]
    actions = _np.full((n, 2, space.active), -1, dtype=_np.int64)
    if space.active == 1:
        actions[..., 0] = sample_uniform(joint, rng)
    else:
        pairs = joint.reshape(n, 2, space.slot_size, space.slot_size)
        first = sample_uniform(pairs.any(axis=-1), rng)  # (n, 2)
        seconds = _np.take_along_axis(pairs, first.clip(0)[..., None, None], axis=2)[:, :, 0]
        second = sample_uniform(seconds, rng)
        second[first < 0] = -1
        actions[..., 0] = first
        actions[..., 1] = second
    if space.preview_size:
        preview = masks[..., space.joint_size :]
        asked = preview.any(axis=-1)
        if asked.any():
            actions[..., 0][asked] = sample_uniform(preview[asked], rng)
    return actions
