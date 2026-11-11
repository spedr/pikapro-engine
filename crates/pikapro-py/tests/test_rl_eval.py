"""pikapro.rl.eval: built-in policies in Rust, callables over batched steps."""

import signal
import subprocess
import sys
import time
from types import SimpleNamespace

import numpy as np
import pytest

import pikapro
import pikapro.rl as rl
from pikapro.rl import eval as ev
from conftest import raises_exactly

SEED = 4_630_000


@pytest.mark.skipif(sys.platform == "win32", reason="SIGINT to a child process is POSIX")
def test_cross_evaluate_stops_on_ctrl_c():
    """A long evaluation in Rust answers Ctrl-C (KeyboardInterrupt) at once."""
    code = (
        "import pikapro.rl as rl; print('go', flush=True); "
        "rl.eval.cross_evaluate(['random', 'simple_heuristics'], 'championsvgc', 3_000_000, threads=2)"
    )
    p = subprocess.Popen([sys.executable, "-c", code], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        assert p.stdout.readline() == "go\n"
        time.sleep(1.0)
        p.send_signal(signal.SIGINT)
        _, err = p.communicate(timeout=20)
    finally:
        p.kill()
    assert "KeyboardInterrupt" in err and p.returncode != 0


def test_records_and_wilson():
    r = ev.WinRecord(wins=60, losses=40)
    lo, hi = r.wilson()
    assert abs(lo - 0.5020) < 1e-3 and abs(hi - 0.6906) < 1e-3
    assert r.significantly_better and not r.flipped().significantly_better
    assert ev.WinRecord().wilson() == (0.0, 1.0) and ev.WinRecord().win_rate == 0.5
    assert str(r).startswith("0.600 [0.502, 0.691] (60-40-0)")
    # Ties: Rust's interval (`pikapro_rl::eval::Record::wilson`), the decisive
    # games' interval pulled towards one half by the tie share, not ties
    # as half-wins.
    t = ev.WinRecord(wins=60, losses=40, ties=100)
    tlo, thi = t.wilson()
    assert abs(tlo - (0.5 + (lo - 0.5) / 2)) < 1e-12 and abs(thi - (0.5 + (hi - 0.5) / 2)) < 1e-12
    assert tlo < t.win_rate == 0.55 < thi and t.significantly_better == r.significantly_better
    assert ev.WinRecord(ties=5).wilson() == (0.0, 1.0)
    assert r.wilson(2.58) == pikapro._pikapro._wilson(60, 40, 0, 2.58)
    # z <= 0 would invert the interval; NaN and infinity mean nothing.
    for z in (0.0, -1.96, float("nan"), float("inf")):
        with pytest.raises(ValueError, match="z must be"):
            r.wilson(z)


def test_zero_episodes_give_an_empty_record():
    """With a callable as with two built-in policies; a negative count is
    refused."""
    assert ev.evaluate("random", "max_base_power", "gen9singles", 0) == ev.WinRecord()
    assert ev.evaluate(lambda step, env: rl.RANDOM_ACTION, "random", "gen9singles", 0) == ev.WinRecord()
    for bad in (-1, 1.5):
        with pytest.raises(ValueError, match="episodes"):
            ev.evaluate("random", "max_base_power", "gen9singles", bad)
    with raises_exactly(TypeError, "episodes is an int, not a bool"):
        ev.evaluate("random", "max_base_power", "gen9singles", True)


class _FailingEnv:
    """A stand-in for a `VecEnv` of two battles: episode 1 fails
    (`end_reason` 5), episodes 0 and 2 end with p1 and p2 winning."""

    n, actions_per_player, config = 2, 1, {"seed": 0}

    def __init__(self):
        self.steps = [((0, 1), (0, -1), (1, 5)), ((2, 1), (1, -1), (1, 5))]

    def _step(self, episode, winner, reason):
        return SimpleNamespace(
            masks=np.ones((2, 2, 1), dtype=bool),
            episode=np.array(episode),
            winner=np.array(winner),
            end_reason=np.array(reason, dtype=np.uint8),
            terminated=np.array([r == 1 for r in reason]),
            truncated=np.array([r == 5 for r in reason]),
            status=np.ones((2, 2), dtype=np.int8),
            final_episode=None,
        )

    def reset(self, seed):
        return self._step((0, 1), (-1, -1), (0, 0))

    def step(self, actions):
        return self._step(*self.steps.pop(0))

    def take_errors(self):
        return []


class _SkippingEnv(_FailingEnv):
    """Two battles with same-step autoreset, as `VecEnv` reports them:
    battle 0 ends episode 0 and its next, episode 2, fails to start in
    that step, which gives it no row of its own (the row is episode 0's
    end), only an error; battle 0 goes on with episode 4. Battle 1 ends
    episodes 1 and 3."""

    def __init__(self):
        # (final episodes, episodes now, winners, end reasons, errors)
        self.steps = [
            ((0, 0), (2, 1), (0, -1), (1, 0), [(0, 2, "episode 2: can't start")]),
            ((0, 1), (4, 3), (-1, 1), (0, 1), []),
            ((0, 3), (4, 5), (-1, 0), (0, 1), []),
        ]
        self.errors = []

    def step(self, actions):
        if not self.steps:
            raise AssertionError("evaluate goes on although every episode ended or failed")
        final, episode, winner, reason, errors = self.steps.pop(0)
        self.errors += errors
        step = self._step(episode, winner, reason)
        step.final_episode = np.array(final)
        return step

    def reset(self, seed):
        step = self._step((0, 1), (-1, -1), (0, 0))
        step.final_episode = np.array((0, 0))
        return step

    def take_errors(self):
        errors, self.errors = self.errors, []
        return errors


def test_an_episode_that_fails_to_start_ends_the_evaluation_too():
    """Same-step autoreset: an episode that fails to start has no end row,
    so `evaluate` waited for it for good; the environment's errors count
    it."""
    r = ev.WinRecord()
    ev._play(_SkippingEnv(), lambda step, env: np.zeros(env.n, dtype=np.int64), None, 4, r)
    assert (r.wins, r.losses, r.ties, r.errors, r.games) == (2, 1, 0, 1, 3)


def test_failed_battles_are_errors_not_ties():
    r = ev.WinRecord()
    ev._play(_FailingEnv(), lambda step, env: np.zeros(env.n, dtype=np.int64), None, 3, r)
    assert (r.wins, r.losses, r.ties, r.errors, r.games) == (1, 1, 0, 1, 2)
    assert r.win_rate == 0.5 and r.flipped().errors == 1
    assert str(r).endswith("(1-1-0, 1 errors)")


def test_cross_evaluation_of_built_ins():
    t = ev.cross_evaluate(list(rl.BUILTIN_POLICIES), "gen9singles", 6, seed=SEED, threads=2)
    assert t.names == list(rl.BUILTIN_POLICIES) and t.records[0][0] is None
    for i in range(3):
        for j in range(3):
            if i != j:
                assert t.records[i][j].games == 12
                assert t.records[i][j] == t.records[j][i].flipped()
    rates = t.win_rates()
    assert rates["random"]["random"] is None and 0 <= rates["random"]["max_base_power"] <= 1
    assert "simple_heuristics" in str(t)
    r = ev.evaluate("max_base_power", "random", "gen9singles", 6, seed=SEED, threads=2)
    two = ev.cross_evaluate(["max_base_power", "random"], "gen9singles", 6, seed=SEED, threads=1)
    assert r == two.get("max_base_power", "random") and r.games == 12
    # A game's policies draw from streams seeded by its episode and seat
    # alone: the order of the entrants and the other entrants don't matter.
    assert ev.evaluate("random", "max_base_power", "gen9singles", 6, seed=SEED, threads=2) == r.flipped()
    assert t.get("max_base_power", "random") == r


def test_built_in_pairs_take_only_what_rust_honours():
    """Two built-ins play mirrored in Rust: `n` sets the batch (results
    unchanged); what Rust can't do is refused, not silently ignored."""
    r = ev.evaluate("random", "max_base_power", "gen9singles", 5, seed=SEED, threads=1)
    assert r.games == 10
    assert ev.evaluate("random", "max_base_power", "gen9singles", 5, seed=SEED, threads=1, n=2) == r
    with pytest.raises(ValueError, match="mirrored"):
        ev.evaluate("random", "max_base_power", "gen9singles", 5, mirrored=False)
    with pytest.raises(TypeError, match="max_turns"):
        ev.evaluate("random", "max_base_power", "gen9singles", 5, max_turns=1)
    assert rl.OPPONENTS[:-1] == list(rl.BUILTIN_POLICIES)


def first_legal(step, env):
    """A deterministic policy: the first allowed action."""
    m = step.masks[:, 0]
    return np.where(m.any(-1), m.argmax(-1), -1)


def test_callable_policies_play_mirrored_episodes():
    r = ev.evaluate(first_legal, "random", "gen9singles", 8, seed=SEED, masks="joint", threads=2)
    assert r.games == 16
    # The same games, whichever side the callable is given as.
    flipped = ev.evaluate("random", first_legal, "gen9singles", 8, seed=SEED, masks="joint", threads=2)
    assert flipped == r.flipped()
    # A callable opponent (self-play against a frozen copy): it sees its
    # own rows first.
    seen = []

    def opponent(step, env):
        seen.append(step.masks.shape)
        return first_legal(step, env)

    s = ev.evaluate(first_legal, opponent, "gen9singles", 4, seed=SEED, masks="joint", mirrored=False, n=4)
    assert s.games == 4 and seen
    # Mirrored self-play of one policy is even.
    m = ev.evaluate(first_legal, first_legal, "gen9singles", 6, seed=SEED, masks="joint", n=6)
    assert m.wins == m.losses


def test_heuristics_beat_random_in_championsvgc():
    r = ev.evaluate("simple_heuristics", "random", "championsvgc", 40, seed=SEED)
    assert r.significantly_better, r


def test_torch_policy_example_shape():
    torch = pytest.importorskip("torch")
    env = rl.VecEnv("gen9singles", n=4, observation="basic", masks="joint")
    net = torch.nn.Linear(env.observation["size"], env.mask_width)

    def policy(step, env):
        with torch.no_grad():
            logits = net(torch.as_tensor(step.obs[:, 0]))
        logits[~torch.as_tensor(step.masks[:, 0])] = -torch.inf
        a = logits.argmax(-1).numpy()
        return np.where(step.masks[:, 0].any(-1), a, -1)

    r = ev.evaluate(policy, "random", "gen9singles", 4, seed=SEED, observation="basic", masks="joint", n=4)
    assert r.games == 8
    assert pikapro is not None


def test_callable_evaluation_plays_the_environment_s_episodes():
    # `evaluate` with a callable plays episode `e` with the teams the
    # environment gives its episode `e` (battle seed `episode_seed(seed,
    # e)`, not `seed + e`); Rust's `cross_evaluate` plays the same ones
    # (`eval::tests::episodes_are_the_environment_s`).
    fmt, n = "gen9vgc", 4
    source = pikapro.RandomTeams(fmt, "sets")
    pairs = pikapro._pikapro._episode_teams(source, fmt, n, SEED)
    env = rl.VecEnv(fmt, n=n, seed=SEED, teams=source, record=True)
    env.reset(seed=SEED)
    for e in range(n):
        r = env.record(e)
        assert r["episode"] == e and list(r["teams"]) == list(pairs[e]), e


def sampled(step, env):
    """A random allowed action for player 0 through the env's helper (an
    opponent callable gets an env seen from its own side)."""
    return env.sample_actions(np.random.default_rng(int(step.episode.sum()) % 2**32), players=[0])[:, 0]


def _timed(fn, limit=60):
    """Runs ``fn`` under a SIGALRM timeout where there is one (a hang
    fails the test)."""
    if not hasattr(signal, "SIGALRM"):
        return fn()

    def alarm(*_):
        raise TimeoutError(f"no result after {limit}s")

    old = signal.signal(signal.SIGALRM, alarm)
    signal.alarm(limit)
    try:
        return fn()
    finally:
        signal.alarm(0)
        signal.signal(signal.SIGALRM, old)


def test_evaluate_counts_same_step_episodes():
    """With autoreset="same_step" a done row's `episode` is the next one;
    evaluate counts the ended one (`final_episode`) and gives the same
    record as next-step."""
    kw = dict(seed=8_300_100, masks="joint", threads=2, n=3)
    nxt = _timed(lambda: ev.evaluate(first_legal, "random", "gen9singles", 7, **kw))
    same = _timed(lambda: ev.evaluate(first_legal, "random", "gen9singles", 7, autoreset="same_step", **kw))
    assert nxt.games == 14 and same == nxt


def test_evaluate_refuses_what_it_cant_evaluate():
    for opponent in ["external", {"random": 1, "external": 1}, [("external", 1.0)]]:
        with pytest.raises(ValueError, match="callable"):
            ev.evaluate(first_legal, opponent, "gen9singles", 2)
    with pytest.raises(ValueError, match="callable"):
        ev.evaluate("external", first_legal, "gen9singles", 2)
    with pytest.raises(ValueError, match="callable"):
        ev.evaluate("random", "external", "gen9singles", 2)
    with pytest.raises(ValueError, match="series"):
        ev.evaluate(first_legal, "random", "gen9singles", 2, masks="joint", series="bo3")


def test_an_opponent_callable_sees_the_env_from_its_side():
    """A callable opponent that uses the env's helpers
    (`sample_actions(players=[0])`, `view`, `side_of`) gets them for
    itself."""
    sides = []

    def opponent(step, env):
        sides.append((env.side_of(0, 0), env.view(0).player_role, env.last.agent_side[0]))
        # `embed_all` reads the opponent's own views too.
        roles = rl.embed_all(env, lambda view: [int(view.player_role[1])])
        assert roles[0, 0] == env.side_of(0, 0) + 1
        return sampled(step, env)

    r = _timed(lambda: ev.evaluate(sampled, opponent, "gen9singles", 4, seed=8_300_200, masks="joint", n=2))
    assert r.games == 8 and sides
    # Player 1 is the side player 0 isn't.
    assert all(side == 1 - agent_side and role == f"p{side + 1}" for side, role, agent_side in sides)
    with pytest.raises(AttributeError, match="evaluate"):
        ev.evaluate(first_legal, lambda step, env: env.step(None), "gen9singles", 1, masks="joint")


def test_evaluate_raises_on_a_stall():
    """A policy that never answers for a player it is asked about stops
    its battle; evaluate raises instead of spinning forever."""

    def never(step, env):
        return np.full(env.n, -1)

    with pytest.raises(RuntimeError, match="stalled"):
        _timed(lambda: ev.evaluate(first_legal, never, "gen9singles", 2, masks="joint", n=2), limit=120)
