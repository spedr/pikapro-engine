import _thread
import threading
import time

import numpy as np
import pytest

import pikapro
from conftest import play_random, teams


def test_shapes(format_id):
    n = 5
    pool = pikapro.BattlePool(format_id, teams(format_id), n=n, seed=1, threads=2)
    s = pool.space
    assert len(pool) == pool.n == n and pool.threads == 2 and pool.format == format_id
    assert pool.masks().shape == (n, 2, s.size) and pool.masks().dtype == np.bool_
    assert pool.masks("slots").shape == (n, 2, s.active, s.slot_size)
    assert pool.masks("preview").shape == (n, 2, s.preview_size)
    assert pool.requests().shape == (n, 2) and pool.requests().dtype == np.int8
    assert pool.sample_random().shape == (n, 2)
    assert pool.sample_random("slots").shape == (n, 2, s.active)
    p, sd, f = pool.state()
    assert p.shape == (n, 2, 6, len(pikapro.POKEMON_COLUMNS)) and p.dtype == np.int32
    assert sd.shape == (n, 2, len(pikapro.SIDE_COLUMNS)) and f.shape == (n, len(pikapro.FIELD_COLUMNS))
    status, done, winner, _ = pool.step(pool.sample_random())
    assert status.shape == (n, 2) and done.shape == (n,) and winner.shape == (n,)
    with pytest.raises(ValueError, match="shape"):
        pool.step(np.zeros((n, 3), dtype=np.int64))
    with pytest.raises(ValueError):
        pikapro.BattlePool(format_id, teams(format_id), n=0)


def test_masks_agree_with_battles(format_id):
    pool = pikapro.BattlePool(format_id, teams(format_id), n=4, seed=3, threads=2)
    for _ in range(25):
        masks = pool.masks()
        slots = pool.masks("slots")
        requests = pool.requests()
        for i in range(4):
            b = pool.battle(i)
            for side in (0, 1):
                np.testing.assert_array_equal(masks[i, side], b.action_mask(side))
                np.testing.assert_array_equal(slots[i, side], b.slot_mask(side))
                assert pikapro.REQUEST_CODES[requests[i, side]] == b.request_kind(side)
        pool.step(pool.sample_random())


def test_rejections_and_statuses():
    fmt = "gen9singles"
    pool = pikapro.BattlePool(fmt, teams(fmt), n=2, seed=1, threads=1)
    before = pool.state()
    status, done, _, _ = pool.step(np.full((2, 2), -1))
    assert (status == -1).all() and not done.any()
    for x, y in zip(before, pool.state()):
        np.testing.assert_array_equal(x, y)
    # A legal p1 choice waits for p2; p2's side then gets 0 when not asked.
    a = pool.sample_random()
    a[:, 1] = -1
    status, _, _, _ = pool.step(a)
    assert status.tolist() == [[1, -1], [1, -1]]
    assert (pool.requests()[:, 0] == 0).all()
    a = pool.sample_random()
    assert (a[:, 0] == -1).all()
    status, _, _, _ = pool.step(a)
    assert status.tolist() == [[0, 1], [0, 1]]


_MASK64 = (1 << 64) - 1


def episode_seed(base: int, episode: int) -> int:
    """`pikapro_rl::env::episode_seed`: splitmix64's output at step
    `episode + 1` of the stream seeded with the mixed base."""

    def mix(z: int) -> int:
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & _MASK64
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & _MASK64
        return z ^ (z >> 31)

    return mix((mix(base) + (episode + 1) * 0x9E3779B97F4A7C15) & _MASK64)


def test_auto_reset_and_episodes():
    fmt = "gen9vgc"
    n = 4
    pool = pikapro.BattlePool(fmt, teams(fmt), n=n, seed=10, threads=2)
    np.testing.assert_array_equal(pool.episodes(), np.arange(n))
    # Hashed from (seed, episode), as VecEnv's: seeds 10 and 11 don't play
    # the same battles shifted by one.
    assert pool.seeds().tolist() == [episode_seed(10, e) for e in range(n)]
    other = pikapro.BattlePool(fmt, teams(fmt), n=n, seed=11, threads=2)
    assert not set(other.seeds().tolist()) & set(pool.seeds().tolist())
    ended = 0
    while ended < 6:
        status, done, winner, _ = pool.step(pool.sample_random())
        assert ((winner >= 0) == done).all()
        ended += int(done.sum())
    assert pool.finished()["battles"] == ended
    assert (pool.episodes() >= np.arange(n)).all() and ((pool.episodes() - np.arange(n)) % n == 0).all()
    pool.reset([0])
    assert pool.battle(0).turn == 0
    no_reset = pikapro.BattlePool(fmt, teams(fmt), n=1, seed=10, autoreset=None)
    while not no_reset.battle(0).over:
        _, done, _, _ = no_reset.step(no_reset.sample_random())
    assert done[0] and no_reset.requests().tolist() == [[0, 0]]


def test_team_pairs_rotate():
    t1 = pikapro.sample_team("gen9vgc")
    # A different team: the sample without its last Pokémon.
    t2 = "\n\n".join(t1.strip().split("\n\n")[:5])
    pool = pikapro.BattlePool("gen9vgc", [[t1, t1], [t2, t1]], n=2, seed=0, threads=1)
    p, _, _ = pool.state()
    assert p[0, 0, 5, pikapro.column("id")] == 5 and (p[1, 0, 5] == -1).all()


def test_sample_slots_policy(format_id):
    """The numpy random policy over slot masks plays legal games."""
    rng = np.random.default_rng(0)
    pool = pikapro.BattlePool(format_id, teams(format_id), n=8, seed=0, threads=2)
    while pool.finished()["battles"] < 8:
        status, _, _, _ = pool.step(pikapro.sample_slots(pool, rng))
        assert not (status == -1).any()


def test_play_random_replays_through_battles():
    """`play_random(b)` plays episodes `0..b`, each a `Battle` with its
    battle seed `s` and a `RandomPolicy(~s)`, as `pikapro bench` plays its own
    seeds."""
    for fmt in ("gen9vgc", "gen9singles"):
        wins = [0, 0, 0]
        for e in range(12):
            s = episode_seed(7, e)
            b = play_random(pikapro.Battle(fmt, teams(fmt), seed=s), pikapro.RandomPolicy(~s & _MASK64))
            wins[2 if b.winner is None else b.winner] += 1
        pool = pikapro.BattlePool(fmt, teams(fmt), n=6, seed=7, threads=3)
        r = pool.play_random(12)
        assert (r["battles"], r["p1_wins"], r["p2_wins"], r["ties"]) == (12, *wins)


def test_battle_copy_is_detached():
    fmt = "gen9doubles"
    pool = pikapro.BattlePool(fmt, teams(fmt), n=1, seed=0, log=True)
    b = pool.battle(0)
    b.submit(0, b.legal_actions(0)[0])
    assert pool.requests()[0, 0] == 2
    with pytest.raises(IndexError):
        pool.battle(1)


def test_teams_by_name_as_vec_env():
    # A string names random teams, as VecEnv's `teams` (one mapping, in Rust).
    import pikapro.rl

    for name, mode in (("sets", "sets"), ("random", "sets"), ("uniform", "uniform")):
        pool = pikapro.BattlePool("gen9singles", name, 2, seed=4)
        expect = pikapro.BattlePool("gen9singles", pikapro.RandomTeams("gen9singles", mode), 2, seed=4)
        for x, y in zip(pool.state(), expect.state()):
            np.testing.assert_array_equal(x, y)
        assert pikapro.rl.VecEnv("gen9singles", n=1, teams=name).config["teams"].mode == mode
    with pytest.raises(ValueError, match="a string names random teams"):
        pikapro.BattlePool("gen9singles", "fixed", 2)
    with pytest.raises(ValueError, match="a string names random teams"):
        pikapro.rl.VecEnv("gen9singles", n=1, teams="fixed")


def test_auto_reset_false_reports_the_end_once():
    fmt = "gen9singles"
    pool = pikapro.BattlePool(fmt, teams(fmt), n=1, seed=3, threads=1, autoreset=None)
    ends = 0
    while not pool.battle(0).over:
        _, done, _, _ = pool.step(pool.sample_random())
        ends += int(done.sum())
    assert ends == 1 and pool.finished()["battles"] == 1
    winner = pool.battle(0).winner
    for _ in range(3):
        status, done, w, _ = pool.step(pool.sample_random())
        assert status.tolist() == [[0, 0]] and not done.any()
        assert w[0] == (2 if winner is None else winner)
    assert pool.finished()["battles"] == 1


def test_step_takes_integer_actions_only():
    fmt = "gen9singles"
    pool = pikapro.BattlePool(fmt, teams(fmt), n=2, seed=3, threads=1)
    a = pool.sample_random()
    for bad in (a.astype(float) + 0.9, a >= 0, [[1.0, 2.0], [3.0, 4.0]], [[True, False], [True, True]]):
        with pytest.raises(TypeError, match="integer"):
            pool.step(bad)
    status, _, _, _ = pool.step(a.astype(np.int32))
    assert (status == 1).all()
    status, _, _, _ = pool.step(pool.sample_random().astype(np.uint16).tolist())
    assert (status == 1).all()


def test_finished_counts_decisions():
    fmt = "gen9vgc"
    pool = pikapro.BattlePool(fmt, teams(fmt), n=3, seed=5, threads=2)
    r = pool.play_random(9)
    assert r["decisions"] > 0 and pool.finished()["decisions"] == r["decisions"]
    stepped = pikapro.BattlePool(fmt, teams(fmt), n=2, seed=5, threads=1, autoreset=None)
    submitted = 0
    while not (stepped.battle(0).over and stepped.battle(1).over):
        status, _, _, _ = stepped.step(stepped.sample_random())
        submitted += int((status == 1).sum())
    assert stepped.finished()["decisions"] == submitted


def test_play_random_stops_on_ctrl_c():
    fmt = "gen9singles"
    pool = pikapro.BattlePool(fmt, teams(fmt), n=4, seed=0, threads=2)
    timer = threading.Timer(0.3, _thread.interrupt_main)
    timer.start()
    t = time.perf_counter()
    with pytest.raises(KeyboardInterrupt):
        pool.play_random(10**9)
    assert time.perf_counter() - t < 30
    timer.join()
    # The rounds played stay played, and the pool still works.
    assert pool.finished()["battles"] > 0
    pool.step(pool.sample_random())


def test_waiting_for_the_pool_releases_the_gil():
    fmt = "gen9singles"
    pool = pikapro.BattlePool(fmt, teams(fmt), n=4, seed=0, threads=1)
    worker = threading.Thread(target=pool.play_random, args=(3000,))
    worker.start()
    time.sleep(0.05)
    waiters = [threading.Thread(target=f) for f in (pool.finished, pool.seeds, pool.episodes, lambda: pool.battle(0))]
    for w in waiters:
        w.start()
    # This thread keeps running while the others wait for the pool.
    spins = 0
    while worker.is_alive() and spins < 1000:
        spins += 1
        time.sleep(0.0005)
    assert spins > 10, spins
    worker.join()
    for w in waiters:
        w.join()


def test_sample_uniform_never_picks_a_masked_entry():
    class Zeros:
        def random(self, shape, dtype):
            return np.zeros(shape, dtype=dtype)

    masks = np.array([[False, False, True, False], [False, False, False, False], [True, True, False, True]])
    idx = pikapro.sample_uniform(masks, Zeros())
    assert idx.tolist() == [2, -1, 0]
    rng = np.random.default_rng(0)
    masks = rng.random((200, 30)) < 0.1
    idx = pikapro.sample_uniform(masks, rng)
    for row, i in zip(masks, idx):
        assert (i == -1) == (not row.any()) and (i == -1 or row[i])


def test_end_reasons_and_autoreset_modes():
    """Each mode reports the ended episode's winner and end reason; same-step
    keeps its terminal state, next-step starts the episode a step later,
    None never restarts it."""
    fmt = "gen9singles"
    t = teams(fmt)
    pools = {m: pikapro.BattlePool(fmt, t, n=3, seed=4_640_000, threads=1, autoreset=m) for m in ("same_step", "next_step", None)}
    assert [p.autoreset for p in pools.values()] == ["same_step", "next_step", None]
    same, nxt, off = pools.values()
    for _ in range(3000):
        a = same.sample_random()
        _, done, winner, reason = same.step(a)
        _, done2, winner2, reason2 = nxt.step(a)
        assert (done == done2).all() and (winner == winner2).all() and (reason == reason2).all()
        assert ((reason > 0) == done).all() and ((winner >= 0) == done).all()
        if done.any():
            i = int(done.argmax())
            assert reason[i] in (1, 2, 3)
            final = same.final_state()
            assert final["ended"][i] and final["turns"][i] == nxt.battle(i).turn
            np.testing.assert_array_equal(final["pokemon"][i], nxt.state()[0][i])
            assert final["episode"][i] == nxt.episodes()[i]
            assert same.episodes()[i] == final["episode"][i] + 3  # already the next one
            # Next step: still over, then the next episode starts and this
            # step's action for it is ignored.
            assert nxt.requests()[i].tolist() == [0, 0]
            status, done3, winner3, _ = nxt.step(nxt.sample_random())
            assert status[i].tolist() == [0, 0] and not done3[i] and winner3[i] == -1
            assert nxt.episodes()[i] == final["episode"][i] + 3
            break
    else:
        pytest.fail("no battle ended")
    with pytest.raises(ValueError):
        pikapro.BattlePool(fmt, t, n=1, autoreset="sometimes")



# (Battle 0 ends first with the first seed, battle 2 with the second.)
@pytest.mark.parametrize("seed, battles", [(4_640_000, b) for b in (0, 1, 2, 3, 7)] + [(4_640_002, b) for b in (0, 1, 2, 3)])
def test_play_random_after_a_next_step_end_owes_no_second_reset(seed, battles):
    """Next-step autoreset: a battle that ended is owed a reset at the next
    step. `play_random` starts its next episode itself, so the step after
    it must not start another (it dropped the episode under way); a battle
    that played none of `play_random`'s battles is still owed its reset."""
    fmt = "gen9singles"
    n = 3
    pool = pikapro.BattlePool(fmt, teams(fmt), n=n, seed=seed, threads=1, autoreset="next_step")
    for _ in range(3000):
        _, done, _, _ = pool.step(pool.sample_random())
        if done.any():
            break
    else:
        pytest.fail("no battle ended")
    ended = [int(i) for i in np.flatnonzero(done)]
    assert ended == [0 if seed == 4_640_000 else 2]
    assert all(pool.battle(i).over for i in ended)
    before = pool.finished()["battles"]
    r = pool.play_random(battles)
    assert r["battles"] == battles
    share = [battles // n + (i < battles % n) for i in range(n)]
    episodes = pool.episodes().copy()
    status, done2, winner, _ = pool.step(pool.sample_random())
    for i in ended:
        if share[i]:
            # Its episode under way goes on: the action counted, no reset.
            assert not pool.battle(i).over and status[i].tolist() != [0, 0]
            assert pool.episodes()[i] == episodes[i], "the step dropped the episode play_random started"
        else:
            # Untouched by play_random: reset by this step, as owed.
            assert status[i].tolist() == [0, 0] and winner[i] == -1 and not done2[i]
            assert pool.episodes()[i] == episodes[i] + n and not pool.battle(i).over
    assert pool.finished()["battles"] == before + battles + int(done2.sum())
    # And on: nothing else is owed (no episode is skipped afterwards).
    episodes = pool.episodes().copy()
    ends = np.zeros(n, dtype=np.int64)
    owed = done2.copy()
    for _ in range(400):
        _, d, _, _ = pool.step(pool.sample_random())
        ends += owed  # the battles that ended last step start their next episode in this one
        owed = d
    np.testing.assert_array_equal(pool.episodes(), episodes + n * ends)


def test_failures_are_taken_not_raised():
    # A battle that fails ends its episode alone (end reason 5) and the
    # others go on; nothing failed here (the Rust tests inject failures).
    fmt = "gen9singles"
    pool = pikapro.BattlePool(fmt, teams(fmt), n=2, seed=3, threads=1)
    for _ in range(20):
        _, _, _, reason = pool.step(pool.sample_random())
        assert 5 not in reason.tolist()
    assert pool.take_errors() == []


@pytest.mark.parametrize("mode", ["choice", "slots"])
def test_gen9_mega_stones_only_sample_encodable_actions(mode):
    team = "Charizard @ Charizardite Y\n- Splash"
    pool = pikapro.BattlePool("gen9singles", [team, team], n=8, seed=1, threads=1)
    for _ in range(16):
        masks = pool.masks()
        actions = pool.sample_random(mode)
        assert (actions >= 0).all()
        for i in range(pool.n):
            for side in (0, 1):
                index = int(actions[i, side]) if mode == "choice" else int(actions[i, side, 0])
                assert masks[i, side, index]
        status, done, _, _ = pool.step(actions)
        assert (status == 1).all() and not done.any()
