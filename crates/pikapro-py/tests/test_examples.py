"""The examples' smoke runs (``--smoke``: a few steps each), skipped when
their optional dependencies are missing."""

import importlib.util
import subprocess
import sys
from pathlib import Path

import pytest

EXAMPLES = Path(__file__).resolve().parents[1] / "examples"


def needs(*modules):
    missing = [m for m in modules if importlib.util.find_spec(m) is None]
    return pytest.mark.skipif(bool(missing), reason=f"needs {', '.join(missing)}")


def run(script):
    r = subprocess.run([sys.executable, str(EXAMPLES / script), "--smoke"], capture_output=True, text=True, timeout=600)
    assert r.returncode == 0, r.stdout[-2000:] + r.stderr[-4000:]
    return r.stdout


@needs("torch", "stable_baselines3", "sb3_contrib")
def test_sb3_maskable_ppo():
    out = run("sb3_maskable_ppo.py")
    assert "vs random" in out and "vs simple_heuristics" in out


@needs("torch")
def test_cleanrl_ppo_selfplay():
    out = run("cleanrl_ppo_selfplay.py")
    assert "self-play" in out and "vs random" in out


def test_custom_encoder():
    out = run("custom_encoder.py")
    assert "BattleView features: (8, 18)" in out
