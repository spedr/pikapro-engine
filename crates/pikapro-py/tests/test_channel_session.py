"""pikapro.rl.ChannelSession: a battle read from Showdown's text gives the
environment's observations and masks, and plays through choice text."""

import numpy as np
import pytest

import pikapro
import pikapro.rl as rl

FORMATS = ["championsvgc", "championsbss", "gen9vgc", "gen9doubles", "gen9singles", "gen9ou"]


def random_action(space, masks, mask, min_switches, rng):
    """A uniformly random action the player's mask row allows."""
    slots = space.active * space.slot_size
    if masks == "joint":
        return [int(rng.choice(np.flatnonzero(mask)))]
    preview = mask[slots:]
    if preview.any():
        return [int(rng.choice(np.flatnonzero(preview)))] + [0] * (space.active - 1)
    if space.active == 1:
        return [int(rng.choice(np.flatnonzero(mask[: space.slot_size])))]
    pairs = [
        (a, b)
        for a in np.flatnonzero(mask[: space.slot_size])
        for b in range(space.slot_size)
        if space.pair_legal(list(mask[:slots]), int(min_switches), int(a), int(b))
    ]
    a, b = pairs[rng.integers(len(pairs))]
    return [int(a), int(b)]


def feed_update(battle, sessions, start):
    """Each player's new lines, then both requests (Showdown's order)."""
    for side, s in enumerate(sessions):
        s.feed_many(battle.log(f"p{side + 1}", start))
    if not battle.over:
        for side, s in enumerate(sessions):
            s.feed(f"|request|{battle.request_json(side)}")
    return battle.event_count


@pytest.mark.parametrize("fmt", FORMATS)
@pytest.mark.parametrize("masks,switch_by", [("factored", "row"), ("joint", "position")])
def test_session_matches_vec_env(fmt, masks, switch_by):
    """Two sessions fed a battle's channels (rendered by pikapro.Battle from
    the environment's own choices) give its observations and masks."""
    for seed in range(3):
        env = rl.VecEnv(fmt, 1, opponent="external", record=True, masks=masks, switch_by=switch_by, threads=1)
        out = env.reset(seed=5_000_000 + seed)
        record = env.record(0)
        battle = pikapro.Battle(fmt, list(record["teams"]), seed=record["seed"])
        # Each session is given its player's team, as the environment has it.
        teams = list(record["teams"])
        sessions = [rl.ChannelSession(fmt, masks=masks, switch_by=switch_by, team=teams[p]) for p in range(2)]
        for s in sessions:
            s.feed_many(battle.header())
        start = feed_update(battle, sessions, 0)
        done = 0
        rng = np.random.default_rng(seed)
        while True:
            if out.terminated[0] or out.truncated[0]:
                # No request after the end: the own side stays the last
                # request's (no decision is made on it).
                assert all(s.finished for s in sessions)
                break
            for p, s in enumerate(sessions):
                assert s.side == f"p{p + 1}"
                np.testing.assert_array_equal(s.obs, out.obs[0, p])
                assert s.needs_action == bool(out.needs_action[0, p])
                if s.needs_action:
                    np.testing.assert_array_equal(s.mask, out.masks[0, p])
            if out.terminated[0] or out.truncated[0]:
                break
            actions = np.zeros((1, 2, env.actions_per_player), dtype=np.int64)
            for p, s in enumerate(sessions):
                if s.needs_action:
                    a = random_action(env.space, masks, out.masks[0, p], out.min_switches[0, p], rng)
                    actions[0, p] = a
                    text = s.choice(a)
                    assert text == env.describe(0, p, a)
            out = env.step(actions)
            if (out.status[0] == 2).any():
                # An unavailable choice: Showdown's update isn't rendered here
                # (the Rust round trip checks it).
                break
            for side, choice in env.record(0)["choices"][done:]:
                if battle.submit(side, choice):
                    start = feed_update(battle, sessions, start)
            done = len(env.record(0)["choices"])
        for s in sessions:
            assert not s.unknown_lines and s.unknown_count == 0


@pytest.mark.parametrize("fmt", FORMATS)
def test_sessions_play_a_battle(fmt):
    """Two sessions play a pikapro.Battle through its channels and choice
    text, one with the built-in heuristics, one with random actions."""
    teams = [pikapro.sample_team(fmt), pikapro.sample_team(fmt)]
    battle = pikapro.Battle(fmt, teams, seed=7, players=["Alice", "Bob"])
    sessions = [
        rl.ChannelSession(fmt, username="Alice", observation="basic"),
        rl.ChannelSession(fmt, username="Bob"),
    ]
    for s in sessions:
        s.feed_many(battle.header())
    start = feed_update(battle, sessions, 0)
    rng = np.random.default_rng(1)
    k = 0
    while not battle.over:
        texts = [None, None]
        for side, s in enumerate(sessions):
            if s.needs_action:
                if side == 0:
                    texts[side] = s.choose_builtin("simple_heuristics", k)
                else:
                    a = random_action(pikapro.ActionSpace(s.format), "factored", s.mask, s.min_switches, rng)
                    texts[side] = s.choice(a)
                k += 1
        assert any(texts), "nobody could act"
        for side, text in enumerate(texts):
            if text is not None and battle.submit(side, text):
                start = feed_update(battle, sessions, start)
        assert k < 5000
    for side, s in enumerate(sessions):
        assert s.finished
        assert s.side == f"p{side + 1}"
        want = None if battle.winner is None else ["Alice", "Bob"][battle.winner]
        assert s.winner == want
        assert isinstance(s.view, rl.BattleView)
    assert sessions[0].obs.shape == (sessions[0].observation["size"],)


def test_session_reads_room_lines_and_rejects_unknown_formats():
    s = rl.ChannelSession("gen9championsvgc2026regmc", username="Alice")
    s.feed_many(
        [
            ">battle-gen9championsvgc2026regmc-1\n|init|battle\n|title|Alice vs. Bob",
            "|j|☆Alice",
            "|c|☆Alice|gl hf",
            "|raw|<b>hi</b>",
            "|inactive|Battle timer is ON",
            "|player|p1|Alice|1|",
            "|player|p2|Bob|2|",
            "|request|",
            "|something-new|x",
            '|request|{"wait":true,"side":{"name":"Alice","id":"p1","pokemon":[]}}',
        ]
    )
    assert s.unknown_lines == ["|something-new|x"]
    assert s.request_kind == "wait" and not s.needs_action
    assert s.side == "p1"
    with pytest.raises(ValueError):
        rl.ChannelSession("gen1ou")
    assert rl.ChannelSession("championsvgc").format == "championsvgc"
    assert rl.ChannelSession("gen9vgc2025regi").format == "gen9vgc"
    assert rl.ChannelSession("gen9randombattle").format == "gen9randombattle"
