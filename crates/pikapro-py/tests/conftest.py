import contextlib
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

import pikapro

# The tests import shared helpers (``_showdown_fakes``) and a few each
# other's fixtures by module name: this folder is on the path in any of
# pytest's import modes, not only the default ``prepend``.
if str(Path(__file__).resolve().parent) not in sys.path:
    sys.path.insert(0, str(Path(__file__).resolve().parent))

ROOT = Path(__file__).resolve().parents[3]
# Where cargo puts what it builds from ROOT: $CARGO_TARGET_DIR (relative to
# ROOT) or target/.
TARGET = ROOT / (os.environ.get("CARGO_TARGET_DIR") or "target")
FORMATS = [
    "gen9singles",
    "gen9doubles",
    "gen9vgc",
    "gen9ou",
    "gen9randombattle",
    "gen9randomdoublesbattle",
    "championsvgc",
    "championsbss",
]


@pytest.fixture(scope="session")
def pikapro_cli():
    """The `pikapro` CLI binary: $PIKAPRO_CLI, a built one in cargo's target
    directory, or one built now with cargo (debug)."""
    path = os.environ.get("PIKAPRO_CLI")
    if path:
        return path
    for profile in ("release", "debug"):
        exe = TARGET / profile / "pikapro"
        if exe.exists():
            return str(exe)
    if shutil.which("cargo") is None:
        pytest.skip("no pikapro CLI and no cargo to build it")
    subprocess.run(["cargo", "build", "-q", "-p", "pikapro-cli"], cwd=ROOT, check=True)
    return str(TARGET / "debug" / "pikapro")


@pytest.fixture(params=FORMATS)
def format_id(request):
    return request.param


def teams(format_id):
    t = pikapro.sample_team(format_id)
    return [t, t]


@contextlib.contextmanager
def raises_exactly(exc_type, pattern):
    """pytest.raises(exc_type), and `pattern` matching the whole message:
    str() of the exception, without its notes. pytest's match= searches the
    message joined with the notes, and from Python 3.11 on pyo3 notes the
    argument it was converting ("while processing 'seed'"), so a match=
    anchored with $ fails there."""
    with pytest.raises(exc_type) as info:
        yield info
    message = str(info.value)
    assert re.fullmatch(pattern, message), f"{pattern!r} does not match {message!r}"


def play_random(battle, policy):
    """Plays to the end with a RandomPolicy, as `pikapro play` does."""
    while not battle.over:
        for side in (0, 1):
            choice = policy.choose(battle, side)
            if choice is not None:
                battle.submit(side, choice)
    return battle


def pytest_addoption(parser):
    parser.addoption(
        "--real-showdown",
        action="store_true",
        default=False,
        help="also run pikapro.showdown's tests against Showdown's own server (needs node and tools/showdown-oracle)",
    )


def pytest_configure(config):
    config.addinivalue_line(
        "markers", "real_showdown: needs Showdown's own server (off unless --real-showdown or PIKAPRO_REAL_SHOWDOWN=1)"
    )
