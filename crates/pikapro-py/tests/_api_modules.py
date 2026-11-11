"""The stable public modules of the Python package and how to import them.

Shared by ``test_api_snapshot.py``, which pins their API, and by the
maintainers' generator of the API reference (docs/python/api.md).
"""

from __future__ import annotations

import importlib
import importlib.util
import sys
import types

#: The stable public modules, in reading order .
MODULES = [
    "pikapro",
    "pikapro.rl",
    "pikapro.rl.view",
    "pikapro.rl.teams",
    "pikapro.rl.eval",
    "pikapro.rl.gym",
    "pikapro.rl.pettingzoo",
    "pikapro.rl.sb3",
    "pikapro.compat.poke_env",
    "pikapro.showdown",
]

#: Members that are public although they start with an underscore.
UNDERSCORED = {"Record": ("_keys", "_to_dict", "_variant")}


def _stand_ins() -> dict:
    """Stand-ins for the optional adapters' libraries (``gymnasium``,
    ``pettingzoo``, ``stable_baselines3``): the names the adapters read at
    import."""
    def module(name, **attrs):
        m = types.ModuleType(name)
        m.__dict__.update(attrs)
        return m

    gym_vector = module("gymnasium.vector", VectorEnv=type("VectorEnv", (), {}))
    vec_env = module("stable_baselines3.common.vec_env", VecEnv=type("VecEnv", (), {}))
    common = module("stable_baselines3.common", vec_env=vec_env)
    return {
        "gymnasium": module("gymnasium", Env=type("Env", (), {}), vector=gym_vector),
        "gymnasium.vector": gym_vector,
        "pettingzoo": module("pettingzoo", ParallelEnv=type("ParallelEnv", (), {})),
        "stable_baselines3": module("stable_baselines3", common=common),
        "stable_baselines3.common": common,
        "stable_baselines3.common.vec_env": vec_env,
    }


def _import(name: str) -> types.ModuleType:
    """The module ``name``, imported with stand-ins for the adapters'
    libraries that aren't installed, so its API reads the same with and
    without the extras."""
    try:
        return importlib.import_module(name)
    except ImportError:
        pass
    before = set(sys.modules)
    fakes = {k: m for k, m in _stand_ins().items() if importlib.util.find_spec(k.split(".")[0]) is None}
    try:
        for k, m in fakes.items():
            sys.modules.setdefault(k, m)
        return importlib.import_module(name)
    finally:
        for k in set(sys.modules) - before:
            if k in fakes or k.startswith("pikapro."):
                del sys.modules[k]
