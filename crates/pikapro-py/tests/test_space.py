"""The action space: masks, indices, typed choices and Showdown strings
round-trip."""

import numpy as np
import pytest

import pikapro
from conftest import teams

SIZES = {
    "gen9singles": (1, 15, 15, 0),
    "gen9doubles": (2, 47, 47 * 47, 0),
    "gen9vgc": (2, 47, 47 * 47, 360),
    # Team preview brings the whole team: one choice per lead.
    "gen9ou": (1, 15, 15, 6),
    # The random battles have no team preview.
    "gen9randombattle": (1, 15, 15, 0),
    "gen9randomdoublesbattle": (2, 47, 47 * 47, 0),
    "championsvgc": (2, 47, 47 * 47, 360),
    # Singles, 4 moves x (plain, Mega) + 6 switches + pass; 6 * 5 * 4 orders.
    "championsbss": (1, 15, 15, 120),
}


def test_sizes(format_id):
    s = pikapro.ActionSpace(format_id)
    assert (s.active, s.slot_size, s.joint_size, s.preview_size) == SIZES[format_id]
    assert s.size == s.joint_size + s.preview_size
    assert (s.lead_only, s.bring) == ((True, 1) if format_id == "gen9ou" else (False, s.bring))
    assert s.gimmicks == (["mega"] if format_id.startswith("champions") else ["terastallize"])
    assert s.pass_index == s.slot_size - 1 and s.switch_base == s.pass_index - 6


def test_decode_encode_everything(format_id):
    s = pikapro.ActionSpace(format_id)
    for i in range(s.size):
        c = s.decode(i)
        assert s.encode(c) == i
        if i < s.joint_size:
            slots = s.split(i)
            assert s.joint_index(slots) == i
            assert [s.encode_slot(a) for a in c.actions] == slots
        else:
            order = s.preview_order(i - s.joint_size)
            assert type(order) is list and all(type(p) is int for p in order)
            if s.lead_only:
                # The lead, then the rest of a team of six in order.
                assert c.order == order + [p for p in range(6) if p not in order] and len(order) == 1
            else:
                assert c.order == order
    for i in range(s.slot_size):
        assert s.encode_slot(s.decode_slot(i)) == i
    with pytest.raises(IndexError):
        s.decode(s.size)


def test_typed_constructors():
    s = pikapro.ActionSpace("gen9vgc")
    tera = pikapro.SlotAction("move", 1, target=2, gimmick="terastallize")
    ally = pikapro.SlotAction("move", 0, target=-1)
    assert (tera.kind, tera.index, tera.target, tera.gimmick) == ("move", 1, 2, "terastallize")
    c = pikapro.Choice.from_actions([tera, pikapro.SlotAction("switch", 3)])
    assert str(c) == "move 2 2 terastallize, switch 4"
    assert s.decode(s.encode(c)) == c
    assert str(pikapro.Choice.from_actions([ally, pikapro.SlotAction("pass")])) == "move 1 -1, pass"
    assert str(pikapro.Choice.team([3, 2, 1, 0])) == "team 4321"
    assert s.encode(pikapro.Choice.team([0, 1, 2, 3])) == s.joint_size
    # A full order counts as its first `bring` positions.
    assert s.encode(pikapro.Choice.team([0, 1, 2, 3, 4, 5])) == s.joint_size
    with pytest.raises(ValueError):
        pikapro.SlotAction("move", 0, target=3)
    with pytest.raises(ValueError):
        pikapro.SlotAction("dance")


def test_masks_match_legal_lists(format_id):
    """Every legal choice is in the mask and nothing else; its slot actions
    are in the slot mask; its Showdown string parses back to it."""
    b = pikapro.Battle(format_id, teams(format_id), seed=21)
    policy = pikapro.RandomPolicy(22)
    s = b.space
    checked = 0
    while not b.over:
        for side in (0, 1):
            legal = b.legal_actions(side)
            mask = b.action_mask(side)
            assert mask.dtype == np.bool_ and mask.shape == (s.size,)
            assert np.flatnonzero(mask).tolist() == legal
            assert [b.choice_index(side, c) for c in b.legal_choices(side)] == legal
            slot_mask = b.slot_mask(side)
            assert slot_mask.shape == (s.active, s.slot_size)
            per_slot = b.slot_actions(side)
            for slot in range(s.active):
                assert np.flatnonzero(slot_mask[slot]).tolist() == sorted(per_slot[slot])
            for i in legal:
                if i < s.joint_size:
                    assert all(slot_mask[slot, a] for slot, a in enumerate(s.split(i)))
                # The string form round-trips through Showdown's parser.
                assert b.choice_index(side, s.describe(i)) == i
                checked += 1
            c = policy.choose(b, side)
            if c is not None:
                b.submit(side, c)
    assert checked > 0


def test_action_table(format_id):
    """Every row of :func:`pikapro.rl.action_table` describes the action its
    index decodes to."""
    import pikapro.rl as rl

    s = pikapro.ActionSpace(format_id)
    t = rl.action_table(format_id)
    assert set(t) == {*rl.ACTION_TABLE_COLUMNS, "preview_orders"}
    n = s.slot_size + s.preview_size
    for name in rl.ACTION_TABLE_COLUMNS:
        assert t[name].dtype == np.int16 and t[name].shape == (n,)
        assert not t[name].flags.writeable
    assert t["preview_orders"].shape == (s.preview_size, s.bring)
    kinds = {v: k for k, v in rl.ACTION_KINDS.items()}
    # Showdown's target locations by target code.
    locations = [None, 1, 2, -1, -2]
    for i in range(n):
        kind = kinds[int(t["kind"][i])]
        if kind == "preview":
            k = int(t["preview"][i])
            assert i == s.slot_size + k
            assert t["preview_orders"][k].tolist() == list(s.preview_order(k))
            continue
        a = s.decode_slot(i)
        assert a.kind == kind
        if kind == "move":
            assert a.index == t["move_slot"][i]
            assert a.target == locations[t["target"][i]]
            g = int(t["gimmick"][i])
            assert a.gimmick == (None if g == 0 else s.gimmicks[g - 1])
            assert t["switch"][i] == -1 and t["preview"][i] == -1
        else:
            assert (t["move_slot"][i], t["target"][i], t["gimmick"][i]) == (-1, -1, -1)
            assert t["switch"][i] == (a.index if kind == "switch" else -1)
    if s.active == 1:
        assert (t["target"] <= 0).all()


def test_relative_target():
    import pikapro.rl as rl

    assert [rl.relative_target(0, t) for t in range(5)] == [0, 1, 2, 3, 4]
    assert [rl.relative_target(1, t) for t in range(5)] == [0, 1, 2, 4, 3]
    with pytest.raises(ValueError):
        rl.relative_target(0, 5)
