"""pikapro.rl: the native vector environment and views.

Seeds are from 4,600,000 to 4,699,999.
"""

import re

import numpy as np
import pytest

import pikapro
import pikapro.rl as rl
from conftest import raises_exactly

SEED = 4_600_000


def play(env, steps, seed=SEED, players=(0,)):
    rng = np.random.default_rng(seed)
    out = env.reset(seed=seed)
    history = []
    for _ in range(steps):
        a = env.sample_actions(rng, players)
        out = env.step(a)
        history.append({k: v.copy() for k, v in out.as_dict().items()})
    return history


def same(a, b):
    assert len(a) == len(b)
    for x, y in zip(a, b):
        assert x.keys() == y.keys()
        for k in x:
            np.testing.assert_array_equal(x[k], y[k], err_msg=k)


def test_steps_are_deterministic_for_any_thread_count():
    kw = dict(n=12, seed=SEED, opponent="simple_heuristics")
    one = play(rl.VecEnv("championsvgc", threads=1, **kw), 40)
    four = play(rl.VecEnv("championsvgc", threads=4, **kw), 40)
    same(one, four)


def test_shapes_and_dtypes():
    env = rl.VecEnv("championsvgc", n=5, threads=2, seed=SEED)
    out = env.reset(seed=SEED)
    space = env.space
    assert env.actions_per_player == 2 and env.mask_width == 2 * space.slot_size + space.preview_size
    assert out.obs.shape == (5, 2, env.observation["size"]) and out.obs.dtype == np.float32
    assert env.observation["kind"] == "basic" and rl.OBSERVATIONS == ["basic"]
    assert out.masks.shape == (5, 2, env.mask_width) and out.masks.dtype == np.bool_
    assert out.reward.dtype == np.float32 and out.winner.dtype == np.int8 and out.episode.dtype == np.uint64
    assert (out.status == 0).all() and out.needs_action[:, 0].all() and not out.needs_action[:, 1].any()
    assert out.episode.tolist() == list(range(5))
    # Team preview: the orders only.
    assert out.masks[:, 0, 2 * space.slot_size :].any(axis=-1).all()
    basic = rl.VecEnv("gen9vgc", n=3, observation="basic")
    assert basic.reset(seed=SEED).obs.shape == (3, 2, basic.observation["size"])
    assert len(basic.observation["columns"]) == basic.observation["size"] == 1688


def test_player0_actions_alone_and_rejections():
    env = rl.VecEnv("gen9singles", n=4, seed=SEED, masks="joint")
    out = env.reset(seed=SEED)
    assert env.actions_per_player == 1
    bad = np.where(out.masks[:, 0].any(-1), (~out.masks[:, 0]).argmax(-1), -1)
    out = env.step(bad)  # (n,): player 0's actions
    assert (out.status[:, 0] == rl.STATUS["rejected"]).all() and out.needs_action[:, 0].all()
    out = env.step(np.full(4, rl.RANDOM_ACTION))
    assert (out.status[:, 0] == 1).all()
    with pytest.raises(ValueError):
        env.step(np.zeros((4, 3), dtype=np.int64))
    with pytest.raises(TypeError):
        env.step(np.zeros(4, dtype=np.float32))


@pytest.mark.parametrize("masks", ["factored", "joint"])
@pytest.mark.parametrize("format", ["gen9singles", "gen9doubles", "gen9vgc", "gen9ou", "championsvgc", "championsbss"])
def test_sample_actions_are_always_accepted(format, masks):
    """Team preview included (championsbss's factored orders follow a
    one-slot part)."""
    env = rl.VecEnv(format, n=8, threads=1, opponent="external", masks=masks, seed=SEED)
    out = env.reset(seed=SEED)
    rng = np.random.default_rng(SEED)
    asked = 0
    for _ in range(40):
        out = env.step(env.sample_actions(rng))
        assert not (out.status == rl.STATUS["rejected"]).any()
        asked += int((out.status == rl.STATUS["submitted"]).sum())
    assert asked > 0


def test_step_takes_only_the_documented_shapes():
    """A player-major (2, n, per) array has the right size but the wrong
    layout: it is refused, not read as (n, 2, per)."""
    env = rl.VecEnv("championsvgc", n=4, threads=1, opponent="external", seed=SEED)
    env.reset(seed=SEED)
    a = env.sample_actions(np.random.default_rng(0))
    assert a.shape == (4, 2, 2)
    for wrong in (np.ascontiguousarray(a.transpose(1, 0, 2)), np.zeros((1, 16, 1), dtype=np.int64), a.reshape(8, 2, 1)):
        with pytest.raises(ValueError, match="shape"):
            env.step(wrong)
    assert (env.step(a).status == 1).any()


def test_next_step_returns_the_terminal_observation_then_resets():
    env = rl.VecEnv("gen9singles", n=6, seed=SEED, opponent="random")
    rng = np.random.default_rng(1)
    out = env.reset(seed=SEED)
    for _ in range(3000):
        prev_episode = out.episode.copy()
        out = env.step(env.sample_actions(rng, [0]))
        done = out.terminated | out.truncated
        if done.any():
            i = int(np.flatnonzero(done)[0])
            assert out.episode[i] == prev_episode[i] and out.end_reason[i] in (1, 2, 3)
            assert out.winner[i] in (0, 1, 2) and out.reward[i, 0] in (-1, 0, 1)
            assert env.view(i).finished
            assert not out.needs_action[i].any()
            out = env.step(env.sample_actions(rng, [0]))
            assert out.episode[i] == prev_episode[i] + env.n and out.status[i, 0] == 0
            assert not (out.terminated[i] or out.truncated[i]) and out.reward[i, 0] == 0
            return
    pytest.fail("no battle ended")


def test_same_step_autoreset_keeps_the_terminal_observation():
    kw = dict(n=4, seed=SEED, opponent="random", observation="basic", masks="joint")
    nxt = rl.VecEnv("gen9singles", autoreset="next_step", **kw)
    same_ = rl.VecEnv("gen9singles", autoreset="same_step", **kw)
    a, b = nxt.reset(seed=SEED), same_.reset(seed=SEED)
    for step in range(2000):
        # The same actions while no battle has ended.
        act = np.where(a.masks[:, 0].any(-1), a.masks[:, 0].argmax(-1), -1)
        a, b = nxt.step(act), same_.step(act)
        done = a.terminated | a.truncated
        np.testing.assert_array_equal(done, b.terminated | b.truncated)
        if done.any():
            i = np.flatnonzero(done)
            np.testing.assert_array_equal(b.final_obs[i], a.obs[i])
            np.testing.assert_array_equal(b.final_episode[i], a.episode[i])
            np.testing.assert_array_equal(b.winner, a.winner)
            assert (b.episode[i] == a.episode[i] + 4).all()
            return
    pytest.fail("no battle ended")


def test_bound_buffers_are_written_in_place():
    env = rl.VecEnv("gen9vgc", n=3, seed=SEED, observation="basic")
    specs = env.buffer_specs
    obs = np.zeros(specs["obs"][0], dtype=np.float32)
    reward = np.full(6, 7.0, dtype=np.float32)  # flat: any shape with the right size
    env.bind(obs=obs, reward=reward)
    out = env.reset(seed=SEED)
    assert out.obs is obs and out.reward.base is reward or out.reward is reward
    assert obs.any() and (reward == 0).all()
    before = obs.copy()
    out2 = env.step(np.full((3, 2, 2), rl.RANDOM_ACTION))
    assert out2 is out and out.obs is obs and not np.array_equal(before, obs)
    with pytest.raises(TypeError):
        env.bind(obs=np.zeros(specs["obs"][0], dtype=np.float64))
    with pytest.raises(ValueError):
        env.bind(obs=np.zeros(5, dtype=np.float32))
    with pytest.raises(ValueError):
        env.bind(obs=np.zeros(specs["obs"][0], dtype=np.float32)[:, :, ::2])
    with pytest.raises(ValueError):
        env.bind(nothing=obs)
    # A float32 array one byte into its memory: contiguous, but misaligned.
    size = int(np.prod(specs["obs"][0]))
    misaligned = np.frombuffer(bytearray(4 * size + 1), dtype=np.uint8)[1:].view(np.float32).reshape(specs["obs"][0])
    assert not misaligned.flags.aligned and misaligned.flags.c_contiguous
    with pytest.raises(ValueError, match="aligned"):
        env.bind(obs=misaligned)


def test_out_of_range_arguments_are_refused():
    with pytest.raises(ValueError, match="ko_weight"):
        rl.VecEnv("gen9singles", n=1, reward="ko", ko_weight=float("nan"))
    for bad in (2**63, 1_000_001):
        with pytest.raises(ValueError, match="spin_us"):
            rl.VecEnv("gen9singles", n=1, spin_us=bad)
        with pytest.raises(ValueError, match="spin_us"):
            pikapro.BattlePool("gen9singles", [pikapro.sample_team("gen9singles")] * 2, 1, spin_us=bad)


def test_negative_or_oversized_ints_raise_value_or_index_error():
    # Not pyo3's OverflowError, which `except ValueError` misses.
    for bad in (-1, 2**64):
        with pytest.raises(ValueError, match="out of range"):
            rl.VecEnv("gen9singles", n=1, seed=bad)
        with pytest.raises(ValueError, match="out of range"):
            rl.VecEnv("gen9singles", n=bad)
    for bad in (-1, 2**16):
        with pytest.raises(ValueError, match="out of range"):
            rl.VecEnv("gen9singles", n=1, max_turns=bad)
    for bad in ("bo999", 2**64):
        with pytest.raises(ValueError):
            rl.VecEnv("gen9singles", n=1, series=bad)
    env = rl.VecEnv("gen9singles", n=2, seed=SEED)
    for bad in (-5, 2**64):
        with pytest.raises(ValueError, match="out of range"):
            env.reset(seed=bad)
    for bad in (-1, 2, 2**64):
        with pytest.raises(IndexError):
            env.battle(bad)
        with pytest.raises(IndexError):
            env.view(bad, 0)
    with pytest.raises(IndexError):
        env.set_opponent("random", which=[-1])
    pool = pikapro.BattlePool("gen9singles", "sets", 2)
    with pytest.raises(IndexError):
        pool.battle(-1)
    with pytest.raises(IndexError):
        pool.reset([-1])
    with pytest.raises(ValueError, match="out of range"):
        pikapro.BattlePool("gen9singles", "sets", 2, seed=-1)
    with pytest.raises(ValueError, match="out of range"):
        pikapro.Battle("gen9singles", [pikapro.sample_team("gen9singles")] * 2, seed=-1)
    with pytest.raises(ValueError, match="out of range"):
        pikapro.RandomPolicy(-1)
    with pytest.raises(ValueError, match="out of range"):
        pikapro.RandomTeams("gen9singles", "sets").team(-1)
    with pytest.raises(IndexError):
        pikapro.Battle("gen9singles", [pikapro.sample_team("gen9singles")] * 2).space.decode(-1)


def test_out_of_range_int_messages_name_the_argument():
    # pyo3 names the argument only in an exception note, from Python 3.11
    # on: the message itself does, on 3.10 too. raises_exactly matches the
    # message alone (pytest's match= also searches the notes).
    team = pikapro.sample_team("gen9doubles")
    battle = pikapro.Battle("gen9doubles", [team, team])
    for bad in (-1, 2**64, np.int64(-1)):
        with raises_exactly(ValueError, r"seed=-?\d+ is out of range \(0 to 2\*\*64 - 1\)"):
            rl.VecEnv("gen9singles", n=1, seed=bad)
        with pytest.raises(ValueError, match="^seed="):
            battle.fork(bad)
        with pytest.raises(ValueError, match="^n="):
            pikapro.BattlePool("gen9singles", "sets", bad)
        with pytest.raises(ValueError, match="^threads="):
            rl.VecEnv("gen9singles", n=1, threads=bad)
        with pytest.raises(ValueError, match="^shards_per_thread="):
            rl.VecEnv("gen9singles", n=1, shards_per_thread=bad)
        with pytest.raises(IndexError, match="^i="):
            rl.VecEnv("gen9singles", n=1).battle(bad)
        with pytest.raises(IndexError, match="^start="):
            battle.log(start=bad)
    for bad in (-1, 2**16):
        with raises_exactly(ValueError, r"max_turns=-?\d+ is out of range \(1 to 65535\)"):
            rl.VecEnv("gen9singles", n=1, max_turns=bad)
    # Not the width of the Rust type where the argument's range is narrower.
    for bad in (-1, 2**64, 1_000_001):
        with raises_exactly(ValueError, r"spin_us must be 0 to 1000000 \(1 s\), got -?\d+"):
            rl.VecEnv("gen9singles", n=1, spin_us=bad)
    for bad in (-1, 300, 2**64):
        with raises_exactly(ValueError, r"a series is best of 3, 5, 7 or 9 games \(Showdown's rule\), not -?\d+"):
            rl.VecEnv("gen9singles", n=1, series=bad)
        with pytest.raises(ValueError, match="best of 3, 5, 7 or 9"):
            rl.ChannelSeries("gen9vgc", best_of=bad)
    with pytest.raises(ValueError, match="series must be"):
        rl.VecEnv("gen9singles", n=1, series=3.9)
    with raises_exactly(ValueError, "level must be 1 to 100, got 300"):
        pikapro.RandomTeams("gen9singles", "sets", level=300)
    with raises_exactly(ValueError, r"team_size=-1 is out of range \(1 to 6\)"):
        pikapro.RandomTeams("gen9singles", "sets", team_size=-1)
    with pytest.raises(ValueError, match="^min_switches="):
        battle.space.pair_legal([True] * (2 * battle.space.slot_size), -1, 0, 0)
    with pytest.raises(IndexError, match="^a1="):
        battle.space.pair_legal([True] * (2 * battle.space.slot_size), 0, 0, -1)
    with pytest.raises(ValueError, match="^side must be 0 or 1"):
        battle.request(2**64)
    env = rl.VecEnv("gen9singles", n=1)
    with pytest.raises(ValueError, match="^player must be 0 or 1"):
        env.view(0, 2**64)
    with pytest.raises(ValueError, match="^action "):
        env.decode(0, 0, [2**64])


def test_negative_or_oversized_slot_actions_orders_and_codes():
    # SlotAction, Choice.team, relative_target, WinRecord.wilson: ValueError
    # or IndexError, not OverflowError.
    for bad in (-1, 256, 2**64):
        with pytest.raises(IndexError, match="^index="):
            pikapro.SlotAction("move", bad)
        with pytest.raises(IndexError, match="^index="):
            pikapro.SlotAction("switch", bad)
        with pytest.raises(IndexError, match="^order: "):
            pikapro.Choice.team([0, bad])
    for bad in (0, 3, -3, 300, -(2**70), 2**64):
        with raises_exactly(ValueError, f"target must be 1, 2, -1 or -2, got {bad}"):
            pikapro.SlotAction("move", 0, target=bad)
    assert pikapro.SlotAction("move", np.int64(1), target=np.int8(-1)).target == -1
    assert pikapro.Choice.team(np.arange(4)).order == [0, 1, 2, 3]
    for bad in (-1, 2**64):
        with pytest.raises(ValueError, match="^slot="):
            rl.relative_target(bad, 0)
        with pytest.raises(ValueError, match="^target="):
            rl.relative_target(0, bad)
        for field in ("wins", "losses", "ties"):
            with pytest.raises(ValueError, match=f"^{field}="):
                rl.eval.WinRecord(**{field: bad}).wilson()
    with pytest.raises(ValueError, match="no target code"):
        rl.relative_target(2, 0)


def test_bools_are_not_ints():
    """A bool, Python's or numpy's, where an int is expected raises
    TypeError naming the argument (as Battle.submit does) instead of running
    as 0 or 1; numpy ints are still ints."""
    fmt = "gen9doubles"
    team = pikapro.sample_team(fmt)
    battle = pikapro.Battle(fmt, [team, team], seed=1)
    env = rl.VecEnv(fmt, n=2, seed=SEED)
    env.reset(seed=SEED)
    pool = pikapro.BattlePool(fmt, "sets", 2, seed=SEED)
    for bad in (True, False, np.bool_(True), np.bool_(False)):
        cases = {
            "seed": [
                lambda: pikapro.Battle(fmt, [team, team], seed=bad),
                lambda: battle.fork(bad),
                lambda: rl.VecEnv(fmt, n=1, seed=bad),
                lambda: env.reset(seed=bad),
                lambda: pikapro.BattlePool(fmt, "sets", 1, seed=bad),
            ],
            "seed: a word": [lambda: pikapro.Battle(fmt, [team, team], seed=[0, bad, 0, 0])],
            "start": [lambda: battle.log(start=bad), lambda: battle.events(start=bad)],
            "index": [lambda: pikapro.SlotAction("move", bad), lambda: battle.space.decode(bad)],
            "target": [lambda: pikapro.SlotAction("move", 0, target=bad)],
            "order: an item": [lambda: pikapro.Choice.team([0, bad, 2, 3])],
            "i": [lambda: env.battle(bad), lambda: pool.battle(bad)],
            "n": [lambda: pikapro.BattlePool(fmt, "sets", bad)],
            "player": [lambda: env.view(0, bad), lambda: env.decode(0, bad, [0, 0])],
            "an action": [lambda: env.decode(0, 0, [0, bad]), lambda: env.decode(0, 0, np.array([bad, bad]))],
            "side": [lambda: battle.request(bad), lambda: battle.log(bad)],
            "series": [lambda: rl.VecEnv(fmt, n=1, series=bad)],
            "best_of": [lambda: rl.ChannelSeries("gen9vgc", best_of=bad)],
        }
        for name, calls in cases.items():
            for call in calls:
                with raises_exactly(TypeError, f"{re.escape(name)} is an int, not a bool"):
                    call()
    # numpy ints are ints.
    assert pikapro.Battle(fmt, [team, team], seed=np.array([1, 2, 3, 4], dtype=np.uint16)).seed == (1, 2, 3, 4)
    assert battle.log(start=np.int64(0)) == battle.log()
    assert env.battle(np.int64(1)).log() == env.battle(1).log()
    assert pool.battle(np.uint8(1)).log() == pool.battle(1).log()
    assert str(env.decode(0, np.int64(0), np.array([0, 0]))) == str(env.decode(0, 0, [0, 0]))
    assert battle.request(np.int8(1)) == battle.request(1)


def test_torch_pinned_buffers():
    torch = pytest.importorskip("torch")
    env = rl.VecEnv("gen9singles", n=4, seed=SEED, observation="basic")
    shape = env.buffer_specs["obs"][0]
    t = torch.zeros(shape, dtype=torch.float32)
    try:
        t = t.pin_memory()
    except RuntimeError:
        pass  # no accelerator: plain memory works the same way
    env.bind(obs=t.numpy())
    env.reset(seed=SEED)
    assert t.abs().sum() > 0  # the step wrote into the tensor's memory


def test_external_opponent_and_set_opponent():
    env = rl.VecEnv("gen9vgc", n=4, seed=SEED, opponent="external")
    out = env.reset(seed=SEED)
    assert out.needs_action.all()
    assert out.obs[:, 1].any()
    env.set_opponent("random")
    rng = np.random.default_rng(3)
    for _ in range(400):
        out = env.step(env.sample_actions(rng))
    # Episodes started after the change play the built-in opponent.
    assert not out.needs_action[:, 1].all()
    env.set_opponent({"random": 1.0, "max_base_power": 1.0, "simple_heuristics": 2.0}, which=[0, 1])
    with pytest.raises(ValueError):
        env.set_opponent("nobody")
    with pytest.raises(ValueError):
        env.set_opponent({"random": 0.0})


def test_set_pairs_replaces_list_entries_for_later_episodes():
    a, b = pikapro.sample_team("gen9singles"), pikapro.random_team("gen9singles", seed=5)
    env = rl.VecEnv("gen9singles", n=1, seed=SEED, teams=[(a, a), (a, a)], record=True)
    env.reset(seed=SEED)
    env.set_pairs([1], [(b, b)])
    first = env.record(0)["teams"]
    rng = np.random.default_rng(5)
    seen = [first]
    for _ in range(3000):
        env.step(env.sample_actions(rng))
        r = env.record(0)
        if r["episode"] == 1:
            seen.append(r["teams"])
            break
    assert len(seen) == 2 and seen[1] != first, "episode 1 plays the new pair at position 1"
    with pytest.raises(ValueError):
        env.set_pairs([2], [(a, a)])  # out of range
    with pytest.raises(ValueError):
        env.set_pairs([0, 1], [(a, a)])
    with pytest.raises(ValueError):
        rl.VecEnv("gen9singles", n=1, seed=SEED).set_pairs([0], [(a, a)])  # not a list of pairs


def test_episodes_replay_through_a_battle():
    env = rl.VecEnv("championsvgc", n=3, seed=SEED, record=True, side_randomization=True)
    rng = np.random.default_rng(4)
    out = env.reset(seed=SEED)
    checked = 0
    for _ in range(600):
        out = env.step(env.sample_actions(rng, [0]))
        for i in np.flatnonzero(out.terminated | out.truncated):
            rec = env.record(int(i))
            b = rl.replay("championsvgc", rec)
            assert b.over and rec["agent_side"] == out.agent_side[i]
            w = b.winner
            expected = 2 if w is None else (0 if w == rec["agent_side"] else 1)
            assert out.winner[i] == expected
            checked += 1
    assert checked >= 3


@pytest.mark.parametrize("fmt", ["gen9singles", "championsvgc"])
def test_replayed_records_give_the_environment_s_observations(fmt):
    """`replay(format, record).view(side)` is the view the environment
    gave, at every step and for both players: the replayed
    battle remembers each side's own team as its requests showed it (the
    PP of a benched Pokémon), as the environment does. Episodes where a
    choice was refused as unavailable are left out (a replay doesn't see
    the refusal)."""
    n = 4
    env = rl.VecEnv(fmt, n=n, seed=8_300_400, opponent="external", record=True, threads=1, side_randomization=True)
    out = env.reset(seed=8_300_400)
    actions = np.full((n, 2, env.actions_per_player), rl.RANDOM_ACTION, dtype=np.int64)
    refused = set()
    checked = 0
    for _ in range(120):
        for i in range(n):
            if (i, int(out.episode[i])) in refused:
                continue
            rec = env.record(i)
            b = rl.replay(fmt, rec)
            for p in (0, 1):
                view = b.view(env.side_of(i, p))
                assert view._view == env.view(i, p)._view, (fmt, i, int(out.episode[i]), p)
                checked += 1
        out = env.step(actions)
        for i in np.flatnonzero((out.status == 2).any(axis=1)):
            refused.add((int(i), int(out.episode[i])))
    assert checked > 500 and (out.episode >= n).any()


def test_config_round_trips_into_the_constructor():
    """`VecEnv(**env.config)` is configured the same, and `config` has
    the current base seed."""
    env = rl.VecEnv(
        "championsbss", n=2, seed=5, side_randomization=True, series="bo3", ots=True, reward="ko", ko_weight=0.25,
        observation="basic", opponent={"random": 1, "simple_heuristics": 2}, threads=2, max_turns=30,
    )  # fmt: skip
    cfg = env.config
    assert cfg["side_randomization"] is True and "randomize_sides" not in cfg
    assert cfg["observation"] == "basic" and cfg["opponent"] == {"random": 1, "simple_heuristics": 2}
    again = rl.VecEnv(**cfg)
    assert again.config == cfg
    env.reset(seed=77)
    assert env.config["seed"] == 77
    env.set_opponent("external")
    assert env.config["opponent"] == "external"


def test_one_sided_open_sheets():
    """`ots_view="player0"`: player 1 sees the game with closed sheets,
    player 0 with open ones (the Rust test `one_sided_open_sheets` steps
    through whole battles)."""
    kw = dict(n=4, seed=SEED, opponent="external", teams="sets")
    views = {}
    for name, extra in {
        "closed": {},
        "open": dict(ots=True),
        "mine": dict(ots=True, ots_view="player0"),
    }.items():
        env = rl.VecEnv("championsvgc", **kw, **extra)
        out = env.reset(seed=SEED)
        views[name] = out.obs.copy()
        assert env.config.get("ots_view", "both") == extra.get("ots_view", "both")
    assert (views["mine"][:, 1] == views["closed"][:, 1]).all()
    assert (views["mine"][:, 0] == views["open"][:, 0]).all()
    assert (views["open"][:, 1] != views["closed"][:, 1]).any()
    with pytest.raises(ValueError, match="ots_view"):
        rl.VecEnv("championsvgc", ots_view="player1")


@pytest.mark.parametrize("fmt", ["gen9singles", "gen9doubles"])
def test_open_sheets_need_team_preview(fmt):
    """Showdown shows sheets at team preview only: `ots` (any non-zero
    probability) and `series` are refused without it, in the adapters
    too."""
    rl.VecEnv(fmt, n=1, ots=False)
    for ots in (True, 0.25):
        with pytest.raises(ValueError, match="ots needs a format with team preview"):
            rl.VecEnv(fmt, n=1, ots=ots)
    with pytest.raises(ValueError, match="need team preview"):
        rl.VecEnv(fmt, n=1, series="bo3")
    pytest.importorskip("gymnasium")
    from pikapro.rl.gym import GymEnv, GymVectorEnv
    from pikapro.rl.pettingzoo import ParallelEnv

    for make in (lambda: GymEnv(fmt, ots=True), lambda: GymVectorEnv(fmt, num_envs=2, ots=0.5), lambda: ParallelEnv(fmt, ots=True)):
        with pytest.raises(ValueError, match="ots needs a format with team preview"):
            make()


def test_open_sheets_land_on_their_pokemon():
    """Each sheet entry is its own Pokémon's, whatever order the foe's
    Pokémon come in."""
    env = rl.VecEnv("championsvgc", n=8, ots=True, seed=901, opponent="external", record=True, threads=1)
    env.reset(seed=901)
    rng = np.random.default_rng(1)
    checked = 0
    for _ in range(12):
        env.step(env.sample_actions(rng))
        for i in range(8):
            teams = env.record(i)["teams"]
            for p in (0, 1):
                sets = {}
                for text in pikapro.export(teams[1 - env.side_of(i, p)], "championsvgc").strip().split("\n\n"):
                    name = re.sub(r" \([MF]\)$", "", text.splitlines()[0].split(" @ ")[0])
                    sets[name.split("-")[0]] = {pikapro.to_id(line[2:]) for line in text.splitlines() if line.startswith("- ")}
                assert len(sets) == 6
                for m in env.view(i, p).opponent_team.values():
                    moves = sets[m.species_name.split("-")[0]]
                    assert set(m.moves) == moves, (i, p, m.species_name)
                    checked += 1
    assert checked > 500


def test_misused_arguments_are_named():
    """Misused arguments raise errors that name them."""
    with pytest.raises(ValueError, match="reward='ko'"):
        rl.VecEnv("gen9singles", ko_weight=5.0)
    env = rl.VecEnv("gen9singles", n=2, seed=SEED)
    env.reset(seed=SEED)
    for bad in (5, -1, 2):
        for call in (
            lambda: env.decode(0, bad, [0]),
            lambda: env.describe(0, bad, [0]),
            lambda: env.side_of(0, bad),
            lambda: env.view(0, bad),
        ):
            with pytest.raises(ValueError, match="player must be 0 or 1"):
                call()
    with pytest.raises(ValueError, match="names: basic"):
        rl.VecEnv("gen9singles", observation="token")
    with pytest.raises(ValueError, match="'sets', 'uniform' or 'random'"):
        rl.VecEnv("gen9singles", teams="sets2")
    # Overlapping buffers are refused when bound, naming both.
    base = np.zeros(8, dtype=bool)
    with pytest.raises(ValueError, match="shares memory"):
        env.bind(terminated=base[:2], truncated=base[1:3])
    own = env.buffers["terminated"]
    with pytest.raises(ValueError, match="terminated"):
        env.bind(truncated=own)
    with pytest.raises(TypeError, match="uint64"):
        env.bind(episode=np.zeros(2, dtype=np.int64))
    env.bind(terminated=base[:2], truncated=base[2:4])  # disjoint: fine


def test_a_bind_that_fails_binds_nothing():
    """One refused array among several: the others aren't bound either, and
    the `Step` shows the buffers steps write into."""
    env = rl.VecEnv("gen9singles", n=2, seed=SEED, opponent="random")
    specs = env.buffer_specs
    before = env.buffers
    obs = np.full(specs["obs"][0], 7, dtype=np.float32)
    reward = np.full(specs["reward"][0], 7, dtype=np.float32)
    bad = [
        dict(obs=obs, reward=reward, episode=np.zeros(2, dtype=np.int64)),  # the last one's dtype
        dict(obs=obs, reward=reward, nothing=obs.copy()),  # no such buffer
        dict(obs=obs, reward=reward, winner="no array"),
        dict(obs=obs, reward=np.zeros(5, dtype=np.float32)),
    ]
    for arrays in bad:
        with pytest.raises((ValueError, TypeError)):
            env.bind(**arrays)
        now = env.buffers
        assert now.keys() == before.keys() and all(now[k] is before[k] for k in before), arrays.keys()
        assert env.last.obs is before["obs"] and env.last.reward is before["reward"]
    out = env.reset(seed=SEED)
    for _ in range(5):
        out = env.step(env.sample_actions(np.random.default_rng(0), [0]))
    assert out.obs is before["obs"] and out.obs.any()
    assert (obs == 7).all() and (reward == 7).all(), "nothing was written into the arrays that weren't bound"
    # The same arrays with nothing refused are bound.
    env.bind(obs=obs, reward=reward)
    assert env.last.obs is obs and env.buffers["reward"] is reward
    env.step(env.sample_actions(np.random.default_rng(0)))


def test_decode_and_describe():
    env = rl.VecEnv("gen9vgc", n=2, seed=SEED)
    out = env.reset(seed=SEED)
    order = int(np.flatnonzero(out.masks[0, 0, 2 * env.space.slot_size :])[0])
    assert env.describe(0, 0, [order, -1]).startswith("team ")
    env.step(env.sample_actions(np.random.default_rng(0), [0]))
    c = env.decode(0, 0, env.sample_actions(np.random.default_rng(1), [0])[0, 0])
    assert c.kind == "actions" and len(c.actions) == 2


def test_pair_mask_completes_the_first_slot():
    env = rl.VecEnv("championsvgc", n=16, seed=SEED)
    rng = np.random.default_rng(5)
    env.reset(seed=SEED)
    S = env.space.slot_size
    for _ in range(30):
        out = env.step(env.sample_actions(rng, [0]))
        m = out.masks[:, 0]
        for a0 in range(S):
            pm = env.pair_mask(m, out.min_switches[:, 0], np.full(16, a0))
            assert pm.shape == (16, S)
            assert not (pm & ~m[:, S : 2 * S]).any()
            assert not pm[~m[:, a0]].any()
            if env.space.switch_base <= a0 < env.space.pass_index:
                assert not pm[:, a0].any()


def test_series_best_of_three():
    env = rl.VecEnv("championsvgc", n=2, seed=SEED, series="bo3", opponent="random")
    out = env.reset(seed=SEED)
    assert out.series.shape == (2, 4) and (out.series[:, 0] == 1).all()
    rng = np.random.default_rng(6)
    seen = set()
    for _ in range(1500):
        out = env.step(env.sample_actions(rng, [0]))
        seen.update(int(g) for g in out.series[:, 0])
    assert {1, 2} <= seen


@pytest.mark.parametrize("series", ["bo5", "bo9", 7])
def test_longer_series(series):
    env = rl.VecEnv("championsbss", n=2, seed=SEED, series=series, opponent="random")
    out = env.reset(seed=SEED)
    assert (out.series[:, 0] == 1).all()
    with pytest.raises(ValueError, match="best of 3, 5, 7 or 9"):
        rl.VecEnv("championsbss", n=1, series="bo4")


def test_observations():
    env = rl.VecEnv("championsvgc", n=2)
    out = env.reset(seed=SEED)
    size = env.observation["size"]
    assert env.obs_shape == (2, 2, size) and out.obs.shape == (2, 2, size) and np.isfinite(out.obs).all()
    columns = env.observation["columns"]
    assert len(columns) == size and "own0.hp" in columns and "field.weather_rain" in columns
    assert out.obs[0, 0, columns.index("own0.hp")] == 1.0
    assert rl.OBSERVATIONS == ["basic"]
    with pytest.raises(ValueError, match="unknown observation"):
        rl.VecEnv("championsvgc", observation="hp_frac")
    with pytest.raises(ValueError, match="unknown observation"):
        rl.VecEnv("championsvgc", observation="raw")
    with pytest.raises(TypeError):
        rl.VecEnv("championsvgc", observation=3)


def test_battle_view_over_the_env_and_a_battle():
    env = rl.VecEnv("championsvgc", n=4, seed=SEED, opponent="random")
    rng = np.random.default_rng(7)
    env.reset(seed=SEED)
    for _ in range(8):
        out = env.step(env.sample_actions(rng, [0]))
    v = env.view(0)
    assert v.turn >= 1 and len(v.team) == 4 and v.player_role == "p1"  # brought
    actives = [m for m in v.active_pokemon if m is not None]
    assert actives and all(m.active and 0 <= m.current_hp_fraction <= 1 for m in actives)
    for m in v.team.values():
        assert m.item != "unknown_item" and m.ability is not None and m.moves
        assert all(k in m.boosts for k in ("atk", "spe", "evasion"))
    for slot, moves in enumerate(v.available_moves):
        for mv in moves:
            assert mv.base_power >= 0 and mv.category in ("physical", "special", "status")
    assert isinstance(v.can_tera, list) and isinstance(v.weather, dict) and isinstance(v.fields, dict)
    assert v.opponent_team  # previewed
    # What the view shows is what `basic` encodes.
    columns = env.observation["columns"]
    for row, m in enumerate(v._team):
        if m.species_name is None:
            continue
        assert out.obs[0, 0, columns.index(f"own{row}.hp")] == np.float32(m.current_hp) / np.float32(m.max_hp)
        assert out.obs[0, 0, columns.index(f"own{row}.active")] == m.active
        assert out.obs[0, 0, columns.index(f"own{row}.fainted")] == m.fainted
    emb = rl.embed_all(env, lambda view: [view.turn, len(view.team)])
    assert emb.shape == (4, 2)
    # A plain Battle.
    t = pikapro.sample_team("gen9singles")
    b = pikapro.Battle("gen9singles", [t, t], seed=SEED)
    bv = rl.BattleView.from_battle(b, 0)
    assert bv.active_pokemon is not None and len(bv.available_moves) >= 1 and bv.available_switches
    foe = bv.opponent_active_pokemon
    assert foe is not None and foe.current_hp_fraction == 1.0 and foe.item == "unknown_item"
    mv = next(iter(bv.active_pokemon.moves.values()))
    assert foe.damage_multiplier(mv) in (0, 0.25, 0.5, 1, 2, 4)
    assert b.view(0).team.keys() == bv.team.keys()
    # A side as every Battle method takes it, and nothing else: "P1" was
    # read as player 2.
    for side, role in [(0, "p1"), ("p1", "p1"), ("P1", "p1"), (np.int64(0), "p1"), (1, "p2"), ("p2", "p2"), ("P2", "p2")]:
        for view in (b.view(side), rl.BattleView.from_battle(b, side)):
            assert view.player_role == role, side
            assert (view.mask == b.slot_mask(side).reshape(-1)).all()
    for side in (2, -1, "p3", "", "player1"):
        with pytest.raises(ValueError, match="side must be"):
            b.view(side)
        with pytest.raises(ValueError, match="side must be"):
            rl.BattleView.from_battle(b, side)
    for side in (True, False, np.True_, None, 1.0):
        with pytest.raises(TypeError):
            b.view(side)
    for player in (2, -1, True, "p1"):
        with pytest.raises((ValueError, TypeError)):
            env.view(0, player)
        with pytest.raises((ValueError, TypeError)):
            rl.BattleView.from_env(env, 0, player)
    grass_bug = type("Mon", (), {"types": ["grass", "bug"]})()
    assert rl.PokemonView.damage_multiplier(grass_bug, "fire") == 4 and rl.PokemonView.damage_multiplier(grass_bug, "Ground") == 0.25
    with pytest.raises(ValueError, match="unknown type"):
        foe.damage_multiplier("light")
    assert all(type(x) is int for x in foe.base_stats.values()) and len(foe.base_stats) == 6
    with pytest.raises(ValueError, match="a view of a gen9singles battle"):
        rl.BattleView(b._view(0), "championsvgc")


def test_volatile_names():
    names = pikapro.VOLATILE_NAMES
    assert len(names) == 128 and names[0] == "protect" and "substitute" in names
    assert pikapro.volatiles(1 | 1 << 16) == ["protect", "substitute"]
    assert pikapro.volatiles([1, 0, 0, 0]) == ["protect"]
    assert pikapro.volatiles([0, 0, 1 << 1, 0]) == [names[65]]


def test_status_none_is_a_name_and_fields_win():
    t = pikapro.sample_team("gen9singles")
    b = pikapro.Battle("gen9singles", [t, t], seed=SEED)
    mon = b.state().sides[0].team[0]
    assert mon.status == "None" and mon.item is not None
    req = b.request(0)
    assert req._variant == "RequestData" and pikapro.kind(req.kind) == "Move"
    assert "kind" in req._keys() and "kind" in dir(req) and "_variant" in dir(req)


def test_views_per_format():
    """A format without a gimmick reads it as absent."""
    for f in ("championsvgc", "gen9singles"):
        env = rl.VecEnv(f, n=2, seed=SEED)
        env.reset(seed=SEED)
        v = env.view(0)
        if f == "championsvgc":
            assert v.can_tera == [False, False] and not v.used_tera and not v.opponent_used_tera
            assert all(m.tera_type is None and not m.is_terastallized for m in v.team.values())
        else:
            assert v.can_mega_evolve is False and not v.used_mega_evolve
            assert all(m.tera_type is not None and not m.is_mega for m in v.team.values())


@pytest.mark.parametrize("masks", ["joint", "factored"])
def test_gen9_mega_stones_keep_vector_actions_encodable(masks):
    team = "Charizard @ Charizardite Y\n- Splash"
    env = rl.VecEnv("gen9singles", 1, teams=[team, team], opponent="external", masks=masks, threads=1)
    out = env.reset(seed=1)
    rng = np.random.default_rng(1)
    for _ in range(8):
        assert out.needs_action.all()
        actions = env.sample_actions(rng, (0, 1))
        assert (actions >= 0).all()
        for side in (0, 1):
            assert "mega" not in env.describe(0, side, actions[0, side])
        out = env.step(actions)
        assert (out.status == 1).all()
