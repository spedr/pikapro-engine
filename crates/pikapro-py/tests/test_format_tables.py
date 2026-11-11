"""Every preset is in each of the package's tables of formats: the
engine's list, the poke-env ids, the sessions that read Showdown's text,
the sample teams, the validator, the team generator and the environment.
(The Rust tables are checked the same way by pikapro-showdown-server's
tests/format_tables.rs.)"""

import numpy as np
import pytest

import pikapro
import pikapro.rl as rl
from conftest import FORMATS
from pikapro import _pikapro
from pikapro.compat.poke_env import _formats


def test_the_lists_name_the_same_presets():
    assert sorted(pikapro.formats()) == sorted(FORMATS)
    assert sorted(_formats.SHOWDOWN_FORMATS) == sorted(FORMATS)
    # Two presets never share a Showdown id.
    assert len(set(_formats.SHOWDOWN_FORMATS.values())) == len(FORMATS)


@pytest.mark.parametrize("fmt", sorted(pikapro.formats()))
def test_a_preset_is_known_everywhere(fmt):
    # Its sample team is legal in it.
    team = pikapro.sample_team(fmt)
    assert pikapro.validate(team, fmt) == []
    # The sessions read its own id and its poke-env (Showdown) id as it.
    assert _pikapro._showdown_preset(fmt) == fmt
    assert rl.ChannelSession(fmt).format == fmt
    showdown = _formats.showdown_format(fmt)
    assert _formats.pikapro_format(showdown) == fmt
    # (pikapro-showdown-server's plain custom games are read through the
    # client's aliases, not as Showdown's ids, whose games have team preview.)
    if showdown not in ("gen9customgame", "gen9doublescustomgame"):
        assert _pikapro._showdown_preset(showdown) == fmt
    # A battle, an action space and an environment with generated teams.
    battle = pikapro.Battle(fmt, [team, team], seed=1)
    space = pikapro.ActionSpace(fmt)
    assert battle.space.size == space.size
    for teams in (None, "uniform", "sets"):
        env = rl.VecEnv(fmt, n=2, threads=1, seed=3, teams=teams)
        out = env.reset(seed=3)
        assert env.mask_width == space.active * space.slot_size + space.preview_size
        out = env.step(env.sample_actions(np.random.default_rng(0)))
        assert not (out.status == rl.STATUS["rejected"]).any()
