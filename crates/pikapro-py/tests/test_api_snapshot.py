"""The public Python API, pinned.

``api_snapshot.txt`` lists every public name of the stable modules with its
kind and signature (annotations left out), every public member of each
class (with the attributes its instances get in ``__init__``, read from a
sample instance: ``INSTANCES``), and the value (or a fingerprint) of each
constant. It also holds a
fingerprint of the versioned layout, per format (``conftest.FORMATS``):
the action space (``pikapro.ACTION_SPACE_VERSION``). A new format adds its
own entries and leaves the others' fingerprints as they are.

The test runs against whichever ``pikapro`` is installed: the editable
build (``tools/full-gate.sh``) and the wheel ``tools/release-config.sh``
builds as the released ones are.

An unintended change fails here. For an intended one, regenerate the
golden, review its diff, and say what changed in CHANGELOG.md::

    PIKAPRO_UPDATE_API=1 pytest tests/test_api_snapshot.py

A layout whose fingerprint changes must also bump its version.

The optional adapters (``pikapro.rl.gym``, ``pettingzoo``, ``sb3``) are read
with stand-ins for their libraries when those aren't installed, so the
snapshot is the same with and without the extras.
"""

from __future__ import annotations

import dataclasses
import hashlib
import inspect
import json
import os
import types
from pathlib import Path

import pytest

import pikapro
from _api_modules import MODULES, UNDERSCORED, _import
from conftest import FORMATS

GOLDEN = Path(__file__).parent / "api_snapshot.txt"

#: Protocol methods that are part of a class's API when it defines them.
DUNDERS = (
    "__call__",
    "__contains__",
    "__enter__",
    "__exit__",
    "__getattr__",
    "__getitem__",
    "__iter__",
    "__len__",
    "__next__",
    "__str__",
)


def _env():
    import pikapro.rl as rl

    env = rl.VecEnv("gen9singles", n=1, threads=1)
    env.reset(seed=1)
    return env


def _instances() -> dict:
    """A sample instance of each class whose public attributes are set on
    the instance (in ``__init__``), by ``module.qualname``: ``vars()`` of
    it (minus ``_`` names) is pinned with the class. The optional adapters
    (``gym``, ``pettingzoo``, ``sb3``, poke-env's ``Feeder``) need their
    libraries, so they aren't here: the snapshot is the same without them."""
    import pikapro.rl as rl
    from pikapro.compat.poke_env import FeederError
    from pikapro.showdown import BattleRoom, RandomRequestHandler, SessionHandler, ShowdownClient

    return {
        "pikapro.rl.VecEnv": _env,
        "pikapro.rl.session.ChannelSession": lambda: rl.ChannelSession("gen9singles"),
        "pikapro.rl.view.BattleView": lambda: _env().view(0),
        "pikapro.rl.view.PokemonView": lambda: _env().view(0).active_pokemon,
        "pikapro.compat.poke_env.feeder.FeederError": lambda: FeederError("|", ValueError()),
        "pikapro.showdown.room.BattleRoom": lambda: BattleRoom("battle-gen9randombattle-1", "me"),
        "pikapro.showdown.handlers.RandomRequestHandler": lambda: RandomRequestHandler(0),
        "pikapro.showdown.handlers.SessionHandler": lambda: SessionHandler("random"),
        "pikapro.showdown.client.ShowdownClient": lambda: ShowdownClient("bot"),
    }


# -- rendering -------------------------------------------------------------------


def _fingerprint(value) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=False, default=repr).encode()).hexdigest()[:16]


def _simple(v) -> bool:
    if v is None or isinstance(v, (bool, int, float)):
        return True
    if isinstance(v, str):
        return len(v) <= 60
    if isinstance(v, (list, tuple)):
        return all(_simple(x) for x in v)
    if isinstance(v, dict):
        return all(_simple(k) and _simple(x) for k, x in v.items())
    return False


def _value(v) -> str:
    if _simple(v):
        text = repr(v)
        if len(text) <= 160:
            return text
    if isinstance(v, (list, tuple, dict)):
        return f"<{type(v).__name__} of {len(v)}, {_fingerprint(v)}>"
    if isinstance(v, str):
        return f"<str of {len(v)}, {_fingerprint(v)}>"
    return f"<{type(v).__name__}>"


class _Default:
    def __init__(self, v):
        self.v = v

    def __repr__(self) -> str:
        if type(self.v).__name__ == "_HAS_DEFAULT_FACTORY_CLASS":  # a dataclass field(default_factory=...)
            return "<factory>"
        return _value(self.v)


def _sig(sig: inspect.Signature) -> str:
    params = [
        p.replace(
            annotation=inspect.Parameter.empty,
            default=p.default if p.default is inspect.Parameter.empty else _Default(p.default),
        )
        for p in sig.parameters.values()
    ]
    return str(sig.replace(parameters=params, return_annotation=inspect.Signature.empty))


def _signature(obj) -> str:
    try:
        return _sig(inspect.signature(obj))
    except (TypeError, ValueError):
        return "(...)"


def _class_signature(cls) -> str:
    init = vars(cls).get("__init__")
    if isinstance(init, types.FunctionType):
        sig = inspect.signature(init)
        return _sig(sig.replace(parameters=list(sig.parameters.values())[1:]))
    return _signature(cls)


def _member(cls, name: str, v) -> str:
    if isinstance(v, property):
        return f"{name}: property{' (settable)' if v.fset else ''}"
    if isinstance(v, staticmethod):
        return f"{name}{_signature(v.__func__)}: staticmethod"
    if isinstance(v, classmethod):
        return f"{name}{_signature(v.__func__)}: classmethod"
    kind = type(v).__name__
    if kind in ("getset_descriptor", "member_descriptor"):
        return f"{name}: attribute"
    if callable(v):
        return f"{name}{_signature(getattr(cls, name))}"
    return f"{name}: attribute = {_value(v)}"


def _class(cls, out: list, name: str) -> None:
    bases = [b.__name__ for b in cls.__bases__ if b is not object]
    out.append(f"class {name}{_class_signature(cls)}" + (f" bases {', '.join(bases)}" if bases else ""))
    own = vars(cls)
    names = {n for n in own if not n.startswith("_") or n in DUNDERS or n in UNDERSCORED.get(name, ())}
    if dataclasses.is_dataclass(cls):  # fields without a class-level default too
        names |= {f.name for f in dataclasses.fields(cls) if not f.name.startswith("_")}
    for n in sorted(names):
        out.append("    " + (_member(cls, n, own[n]) if n in own else f"{n}: attribute"))
    make = _instances().get(f"{cls.__module__}.{cls.__qualname__}")
    if make is not None:
        listed = names | {n for base in cls.__mro__ for n in vars(base)}
        for n in sorted(k for k in vars(make()) if not k.startswith("_") and k not in listed):
            out.append(f"    {n}: instance attribute")


def _public(mod) -> list:
    names = getattr(mod, "__all__", None)
    if names is not None:
        return sorted(names)
    out = []
    for n, v in vars(mod).items():
        if n.startswith("_") or isinstance(v, types.ModuleType):
            continue
        if (inspect.isclass(v) or inspect.isfunction(v)) and v.__module__ == mod.__name__:
            out.append(n)
    return sorted(out)


def snapshot_api() -> list:
    out = []
    for modname in MODULES:
        mod = _import(modname)
        out.append(f"## {modname}")
        for name in _public(mod):
            obj = getattr(mod, name)
            if inspect.isclass(obj):
                _class(obj, out, name)
            elif isinstance(obj, types.ModuleType):
                out.append(f"module {name}")
            elif callable(obj):
                out.append(f"def {name}{_signature(obj)}")
            else:
                out.append(f"{name}: {type(obj).__name__} = {_value(obj)}")
        out.append("")
    return out


# -- the versioned layouts ---------------------------------------------------------


def _action_layout(f):
    import pikapro.rl as rl

    spaces = []
    s = pikapro.ActionSpace(f)
    fields = ["active", "targets", "gimmicks", "slot_size", "joint_size", "bring", "preview_size", "size"]
    fields += ["switch_base", "pass_index"]
    spaces.append([f, [getattr(s, k) for k in fields], [s.describe(i) for i in range(s.size)]])
    spaces.append([f, [str(s.decode_slot(i)) for i in range(s.slot_size)]])
    for masks in ("factored", "joint"):
        env = rl.VecEnv(f, n=1, threads=1, masks=masks)
        spaces.append([f, masks, env.actions_per_player, env.mask_width])
    return pikapro.ACTION_SPACE_VERSION, _fingerprint(spaces)


#: Each versioned layout, per format.
LAYOUTS = {
    **{f"action_space:{f}": (lambda f=f: _action_layout(f)) for f in FORMATS},
}
VERSION_NAMES = {
    **{f"action_space:{f}": f"pikapro.ACTION_SPACE_VERSION, {f}" for f in FORMATS},
}


def snapshot_layouts() -> list:
    out = ["## layouts"]
    for name, fn in LAYOUTS.items():
        version, fp = fn()
        out.append(f"{name} {version} {fp}")
    return out


def snapshot() -> str:
    return "\n".join(snapshot_api() + snapshot_layouts()) + "\n"


# -- tests ---------------------------------------------------------------------------


def _golden_layouts() -> dict:
    lines = GOLDEN.read_text(encoding="utf-8").split("## layouts\n", 1)[1].splitlines()
    return {name: (int(version), fp) for name, version, fp in (line.split() for line in lines if line.strip())}


@pytest.fixture(scope="module")
def current() -> str:
    text = snapshot()
    if os.environ.get("PIKAPRO_UPDATE_API"):
        GOLDEN.write_text(text, encoding="utf-8")
    return text


def test_layout_versions(current):
    """A layout that changed must bump its version (then the golden is
    regenerated, with a CHANGELOG entry)."""
    golden = _golden_layouts()
    for line in current.split("## layouts\n", 1)[1].splitlines():
        name, version, fp = line.split()
        if name not in golden:
            continue  # a new layout (a new format)
        old_version, old_fp = golden[name]
        if fp != old_fp and int(version) == old_version:
            pytest.fail(
                f"the {name} layout changed but its version is still {version}: bump {VERSION_NAMES[name]}, "
                "regenerate the golden (PIKAPRO_UPDATE_API=1) and add a CHANGELOG entry"
            )


def test_api_matches_golden(current):
    """The public API is the golden's. On an intended change: regenerate
    it with ``PIKAPRO_UPDATE_API=1`` and add a CHANGELOG entry."""
    golden = GOLDEN.read_text(encoding="utf-8")
    if current != golden:
        import difflib

        diff = "".join(difflib.unified_diff(golden.splitlines(True), current.splitlines(True), "golden", "current"))
        pytest.fail(
            "the public API changed (tests/api_snapshot.txt). If intended, regenerate it with "
            "PIKAPRO_UPDATE_API=1 pytest tests/test_api_snapshot.py and add a CHANGELOG entry.\n" + diff
        )


def test_version():
    """``pikapro.__version__`` is the installed distribution's version."""
    assert isinstance(pikapro.__version__, str) and pikapro.__version__
    try:
        from importlib.metadata import version

        assert pikapro.__version__ == version("pikapro")
    except ImportError:  # pragma: no cover
        pass


def test_every_sample_instance_is_of_a_public_class():
    """Each entry of ``_instances`` names a class of the stable modules (a
    renamed class must not silently drop its instance attributes from the
    snapshot)."""
    classes = set()
    for modname in MODULES:
        mod = _import(modname)
        for name in _public(mod):
            obj = getattr(mod, name)
            if inspect.isclass(obj):
                classes.add(f"{obj.__module__}.{obj.__qualname__}")
    assert set(_instances()) <= classes, set(_instances()) - classes


def test_every_public_name_has_a_docstring():
    """Every public class, function, method and property of the stable
    modules is documented (the API reference is generated from them)."""
    missing = []
    seen = set()
    for modname in MODULES:
        mod = _import(modname)
        if not (mod.__doc__ or "").strip():
            missing.append(modname)
        for name in _public(mod):
            obj = getattr(mod, name)
            if id(obj) in seen:
                continue
            seen.add(id(obj))
            if inspect.isclass(obj):
                if not (obj.__doc__ or "").strip():
                    missing.append(f"{modname}.{name}")
                own = vars(obj)
                for k in sorted(own):
                    if k.startswith("_") and k not in UNDERSCORED.get(name, ()):
                        continue
                    v = own[k]
                    if isinstance(v, (staticmethod, classmethod)):
                        v = v.__func__
                    if isinstance(v, property) or callable(v) or type(v).__name__ == "getset_descriptor":
                        if not (getattr(v, "__doc__", None) or "").strip():
                            missing.append(f"{modname}.{name}.{k}")
            elif callable(obj) and not (obj.__doc__ or "").strip():
                missing.append(f"{modname}.{name}")
    assert not missing, "no docstring: " + ", ".join(missing)
