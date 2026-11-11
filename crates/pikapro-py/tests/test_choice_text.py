"""``str(Choice)`` parses back: parse, write, parse over random legal
choices and ``default`` in every format."""

import json
import random

import pytest

import pikapro
from conftest import FORMATS

SEED = 8_300_300


@pytest.mark.parametrize("fmt", FORMATS)
def test_parsed_choices_round_trip_through_str(fmt):
    teams = pikapro.RandomTeams(fmt, "sets")
    doubles = pikapro.ActionSpace(fmt).active == 2
    checked = auto = 0
    for k in range(8):
        b = pikapro.Battle(fmt, teams.pair(SEED + k), seed=SEED + k, log=False)
        rng = random.Random(SEED + k)
        while not b.over and b.turn < 60:
            for side in (0, 1):
                if b.over or b.request_kind(side) == "wait":
                    continue
                sampled = rng.choice(b.legal_choices(side))
                text = str(sampled)
                inputs = [text, "default"]
                if doubles and "," in text:
                    inputs.append(text.split(",")[0] + ", default")
                for s in inputs:
                    try:
                        c = b.parse_choice(side, s)
                    except ValueError:
                        continue
                    written = str(c)
                    assert b.parse_choice(side, written) == c, f"{fmt}: {s!r} was written {written!r}"
                    checked += 1
                    auto += "default" in written
                b.submit(side, sampled)
    assert checked > 200
    # In doubles `default` can leave a targeted move without a target,
    # which only `default` writes.
    assert (auto > 0) == doubles


@pytest.mark.parametrize("fmt", ["gen9vgc", "championsvgc"])
def test_a_second_team_piece_is_refused_not_a_panic(fmt):
    """Two `team` pieces at team preview in doubles queued eight positions
    for a team order of six: `submit` raised pyo3's PanicException (a
    BaseException), and a battle on the bundled server crashed. Showdown
    queues both and then plays a team with duplicates, or throws; pikapro
    refuses the second piece."""
    team = pikapro.sample_team(fmt)
    b = pikapro.Battle(fmt, [team, team], seed=SEED)
    for side in ("p1", "p2"):
        for text in ("default, team 1", "auto,team 1", " team 12, team 34", " team 1, team 2"):
            for call in (b.submit, b.parse_choice):
                with pytest.raises(ValueError, match="You must choose exactly 4 Pokémon"):
                    call(side, text)
            assert b.request_kind(side) == "team_preview"
    assert b.submit("p1", "default, default") is False
    assert b.submit("p2", " team 4321, default") is True and b.turn == 1


@pytest.mark.parametrize("side", [0, 1])
@pytest.mark.parametrize("replacement", ["move 2", "move 9"])
def test_replacing_a_submitted_choice(side, replacement):
    team = "Mew\n- Splash\n- Celebrate"
    b = pikapro.Battle("gen9singles", [team, team], seed=1)
    assert b.submit(side, "move 1") is False
    # Read-only parsing must also see the still-active issued request.
    assert str(b.parse_choice(side, "move 2")) == "move 2"
    if replacement == "move 9":
        with pytest.raises(ValueError, match="Your Mew doesn't have a move 9"):
            b.submit(side, replacement)
        assert b.request_kind(side) == "move"
        assert b.submit(1 - side, "move 1") is False
        assert b.turn == 1
        assert b.submit(side, "move 2") is True
    else:
        assert b.submit(side, replacement) is False
        assert b.submit(1 - side, "move 1") is True
    assert b.turn == 2
    moves = json.loads(b.request_json(side))["active"][0]["moves"]
    assert moves[0]["pp"] == moves[0]["maxpp"]
    assert moves[1]["pp"] == moves[1]["maxpp"] - 1


def test_a_choice_that_reveals_a_hidden_disable_cannot_be_replaced():
    b = pikapro.Battle("gen9singles", ["Mew\n- Splash\n- Celebrate", "Mew\n- Imprison\n- Splash"], seed=1)
    b.submit(0, "move 1")
    b.submit(1, "move 1")
    assert b.submit(0, "move 2") is False
    for call in (b.submit, b.parse_choice):
        with pytest.raises(ValueError, match="Can't undo: A trapping/disabling effect"):
            call(0, "move 1")
    # The refused replacement preserves the choice when Showdown's cantUndo
    # check rejects it before clearChoice, and the other side can resolve.
    assert b.submit(1, "move 2") is True
    assert b.turn == 3
