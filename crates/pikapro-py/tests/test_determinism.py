"""Same format, teams, seed and choices: same battle, bit for bit."""

import numpy as np
import pytest

import pikapro
from conftest import play_random, raises_exactly, teams


def test_same_seed_same_log(format_id):
    logs = [play_random(pikapro.Battle(format_id, teams(format_id), seed=77), pikapro.RandomPolicy(78)).raw_log() for _ in range(2)]
    assert logs[0] == logs[1]
    other = play_random(pikapro.Battle(format_id, teams(format_id), seed=79), pikapro.RandomPolicy(78)).raw_log()
    assert other != logs[0]


def test_same_seed_same_events_and_state(format_id):
    a = play_random(pikapro.Battle(format_id, teams(format_id), seed=5), pikapro.RandomPolicy(6))
    b = play_random(pikapro.Battle(format_id, teams(format_id), seed=5), pikapro.RandomPolicy(6))
    assert a.events() == b.events()
    assert a.state() == b.state()
    for x, y in zip(a.arrays(), b.arrays()):
        np.testing.assert_array_equal(x, y)


def test_logging_off_changes_nothing_else(format_id):
    a = play_random(pikapro.Battle(format_id, teams(format_id), seed=8), pikapro.RandomPolicy(9))
    b = play_random(pikapro.Battle(format_id, teams(format_id), seed=8, log=False), pikapro.RandomPolicy(9))
    assert b.event_count == 0 and b.log() == []
    assert (a.winner, a.turn, a.state()) == (b.winner, b.turn, b.state())


def test_pool_is_independent_of_threads():
    fmt = "gen9vgc"
    results = []
    for threads in (1, 3, 8):
        pool = pikapro.BattlePool(fmt, teams(fmt), n=10, seed=40, threads=threads)
        for _ in range(60):
            pool.step(pool.sample_random())
        results.append((pool.state(), pool.finished(), pool.episodes()))
    for state, finished, episodes in results[1:]:
        for x, y in zip(state, results[0][0]):
            np.testing.assert_array_equal(x, y)
        assert finished == results[0][1]
        np.testing.assert_array_equal(episodes, results[0][2])


def test_pool_battle_replays_standalone():
    """A pool battle is `Battle(format, teams, seed)` with the same choices."""
    fmt = "championsvgc"
    pool = pikapro.BattlePool(fmt, teams(fmt), n=3, seed=500, threads=2, log=True, autoreset=None)
    choices = []
    while not all(pool.battle(i).over for i in range(3)):
        actions = pool.sample_random()
        choices.append(actions[1].copy())
        pool.step(actions)
    seed = int(pool.seeds()[1])
    b = pikapro.Battle(fmt, teams(fmt), seed=seed)
    for a in choices:
        if b.over:
            break
        requests = [b.request_kind(s) for s in (0, 1)]
        for side in (0, 1):
            if requests[side] != "wait" and not b.over:
                b.submit(side, int(a[side]))
    assert b.log() == pool.battle(1).log()


def test_numpy_int_seeds():
    """`BattlePool.seeds()` holds numpy uint64s: `Battle` and `fork` take a
    numpy int as the int it is."""
    fmt = "gen9singles"
    pool = pikapro.BattlePool(fmt, teams(fmt), n=2, seed=600)
    seed = pool.seeds()[0]
    assert isinstance(seed, np.uint64)
    a = pikapro.Battle(fmt, teams(fmt), seed)
    b = pikapro.Battle(fmt, teams(fmt), int(seed))
    assert a.seed == b.seed == int(seed)
    assert play_random(a, pikapro.RandomPolicy(1)).raw_log() == play_random(b, pikapro.RandomPolicy(1)).raw_log()
    start = pikapro.Battle(fmt, teams(fmt), np.int64(5))
    forks = [start.fork(s) for s in (np.int64(3), np.uint8(3), 3)]
    logs = [play_random(f, pikapro.RandomPolicy(2)).raw_log() for f in forks]
    assert logs[0] == logs[1] == logs[2]
    for bad in (np.int64(-1), -1, 2**64):
        with pytest.raises(ValueError, match="seed"):
            pikapro.Battle(fmt, teams(fmt), bad)
        with pytest.raises(ValueError, match="seed"):
            start.fork(bad)


def test_seed_words_out_of_range():
    """Four words, one of them past 16 bits: ValueError, as for an int seed
    out of range. Four things that aren't ints get the TypeError that says
    what a seed is."""
    fmt = "gen9singles"
    start = pikapro.Battle(fmt, teams(fmt), 5)
    for bad in ([70000, 0, 0, 0], (0, 0, 0, -1), [0, np.int64(2**16), 0, 0], [0, 0, 2**64, 0]):
        with raises_exactly(ValueError, "seed words must be 0 to 65535, got -?\\d+"):
            pikapro.Battle(fmt, teams(fmt), bad)
        with pytest.raises(ValueError, match="^seed words must be 0 to 65535"):
            start.fork(bad)
    with pytest.raises(TypeError, match="^seed must be an int"):
        pikapro.Battle(fmt, teams(fmt), ["a", 0, 0, 0])
    assert pikapro.Battle(fmt, teams(fmt), [65535, 0, np.uint16(1), 2]).seed == (65535, 0, 1, 2)
