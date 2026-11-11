"""The Gymnasium, PettingZoo and Stable-Baselines3 adapters, with each
library's own checkers. Skipped when a library isn't installed."""

import importlib.util
import re
import sys
import types
from pathlib import Path

import numpy as np
import pytest

import pikapro.rl as rl
from conftest import raises_exactly

SEED = 4_610_000


@pytest.mark.parametrize("format_id", ["gen9singles", "championsvgc"])
def test_gymnasium_check_env(format_id):
    pytest.importorskip("gymnasium")
    from gymnasium.utils.env_checker import check_env

    from pikapro.rl.gym import GymEnv

    env = GymEnv(format_id, opponent="random", seed=SEED)
    check_env(env, skip_render_check=True)
    obs, info = env.reset(seed=SEED)
    assert env.observation_space.contains(obs)
    mask = env.action_masks()
    assert mask.shape == (env.action_space.n,) and mask.any()


def test_gymnasium_single_env_plays_to_the_end():
    pytest.importorskip("gymnasium")
    from pikapro.rl.gym import GymEnv

    env = GymEnv("championsvgc", observation="basic", opponent="simple_heuristics")
    rng = np.random.default_rng(0)
    obs, _ = env.reset(seed=SEED)
    for _ in range(500):
        a = int(rng.choice(np.flatnonzero(obs["action_mask"])))
        obs, r, term, trunc, info = env.step(a)
        assert info["status"] == 1
        if term or trunc:
            assert r in (-1.0, 0.0, 1.0) and info["winner"] in (0, 1, 2)
            obs, _ = env.reset()
            return
    pytest.fail("no battle ended")


def test_gymnasium_vector_env_next_step_autoreset():
    gym = pytest.importorskip("gymnasium")
    from pikapro.rl.gym import GymVectorEnv

    envs = GymVectorEnv("gen9vgc", num_envs=6, opponent="random", seed=SEED)
    assert envs.metadata["autoreset_mode"] == gym.vector.AutoresetMode.NEXT_STEP
    obs, infos = envs.reset(seed=SEED)
    assert envs.observation_space.contains(obs)
    assert obs["action_mask"].shape == (6, envs.single_action_space.n)
    rng = np.random.default_rng(1)
    ended = None
    for _ in range(3000):
        masks = envs.action_masks()
        actions = np.array([rng.choice(np.flatnonzero(m)) if m.any() else 0 for m in masks])
        obs, rew, term, trunc, infos = envs.step(actions)
        assert envs.observation_space.contains(obs)
        if ended is not None:
            # NEXT_STEP: the battle that ended starts again, reward 0.
            assert rew[ended] == 0 and not term[ended] and not trunc[ended]
            assert infos["episode_id"][ended] == last_episode + 6
            return
        done = term | trunc
        if done.any():
            ended = int(np.flatnonzero(done)[0])
            last_episode = int(infos["episode_id"][ended])
    pytest.fail("no battle ended")


def _first_allowed(masks):
    return np.array([np.flatnonzero(m)[0] if m.any() else 0 for m in masks])


def test_gymnasium_record_episode_statistics():
    """Gymnasium's own statistics wrapper owns the "episode" info key."""
    gym = pytest.importorskip("gymnasium")
    from pikapro.rl.gym import GymEnv, GymVectorEnv

    single = gym.wrappers.RecordEpisodeStatistics(GymEnv("gen9singles", seed=SEED))
    single.reset(seed=SEED)
    for _ in range(500):
        a = int(np.flatnonzero(single.unwrapped.action_masks())[0])
        _, _, term, trunc, info = single.step(a)
        if term or trunc:
            assert info["episode"]["l"] > 0 and isinstance(info["episode_id"], int)
            break
    else:
        pytest.fail("no battle ended")

    inner = GymVectorEnv("gen9singles", 4, seed=SEED)
    envs = gym.wrappers.vector.RecordEpisodeStatistics(inner)
    envs.reset(seed=SEED)
    for _ in range(500):
        _, _, term, trunc, infos = envs.step(_first_allowed(inner.action_masks()))
        if (term | trunc).any():
            assert infos["_episode"].any() and infos["_episode_id"].all()
            return
    pytest.fail("no battle ended")


def test_gymnasium_vector_infos_are_copies():
    gym = pytest.importorskip("gymnasium")
    from pikapro.rl.gym import GymVectorEnv

    envs = GymVectorEnv("gen9singles", 4, seed=SEED)
    envs.reset(seed=SEED)
    _, _, _, _, first = envs.step(_first_allowed(envs.action_masks()))
    kept = {k: v.copy() for k, v in first.items()}
    assert not np.shares_memory(first["episode_id"], envs.env.last.episode)
    for _ in range(300):
        envs.step(_first_allowed(envs.action_masks()))
    for k in kept:
        assert np.array_equal(first[k], kept[k]), k
    listed = gym.wrappers.vector.DictInfoToList(GymVectorEnv("gen9singles", 2, seed=SEED))
    _, infos = listed.reset(seed=SEED)
    assert set(infos[0]) == {"winner", "end_reason", "status", "episode_id"}


def test_gymnasium_vector_reset_takes_a_seed_list_for_one_base_seed():
    """Gymnasium's per-environment list ``[s, s + 1, ...]`` (what an int
    ``s`` stands for) is the base seed ``s``; the battles share one base
    seed, so another list raises ValueError."""
    pytest.importorskip("gymnasium")
    from pikapro.rl.gym import GymVectorEnv

    envs = GymVectorEnv("gen9singles", 3, seed=SEED)
    envs.reset(seed=SEED)
    a = [(envs.env.battle(i).seed, envs.env.battle(i).teams()) for i in range(3)]
    envs.reset(seed=[SEED, SEED + 1, SEED + 2])
    assert [(envs.env.battle(i).seed, envs.env.battle(i).teams()) for i in range(3)] == a
    envs.reset(seed=[None, None, None])
    for bad in ([SEED, SEED, SEED], [SEED, SEED + 1], [1.5, 2.5, 3.5], [True, 2, 3]):
        with pytest.raises(ValueError, match="one base seed"):
            envs.reset(seed=bad)
    for bad in (True, np.bool_(False)):
        with raises_exactly(TypeError, "seed is an int, not a bool"):
            envs.reset(seed=bad)


def test_neighbouring_seeds_play_different_battles():
    """SyncVectorEnv and SB3 seed sub-env i with seed + i: env 1 must not
    replay env 0's battles one episode later."""
    pytest.importorskip("gymnasium")
    from pikapro.rl.gym import GymEnv

    a = GymEnv("gen9singles", observation="basic")
    b = GymEnv("gen9singles", observation="basic")
    a.reset(seed=SEED)
    a1, _ = a.reset()
    b0, _ = b.reset(seed=SEED + 1)
    assert not np.array_equal(a1["observation"], b0["observation"])
    # Still reproducible.
    c = GymEnv("gen9singles", observation="basic")
    c.reset(seed=SEED)
    c1, _ = c.reset()
    assert np.array_equal(a1["observation"], c1["observation"])


def test_pettingzoo_parallel_api():
    pytest.importorskip("pettingzoo")
    from pettingzoo.test import parallel_api_test

    from pikapro.rl.pettingzoo import ParallelEnv

    parallel_api_test(ParallelEnv("gen9singles", observation="basic", seed=SEED, max_turns=40), num_cycles=300)
    parallel_api_test(ParallelEnv("championsvgc", seed=SEED), num_cycles=100)


def test_pettingzoo_self_play_to_the_end():
    pytest.importorskip("pettingzoo")
    from pikapro.rl.pettingzoo import ParallelEnv

    env = ParallelEnv("championsvgc", observation="basic", seed=SEED)
    rng = np.random.default_rng(2)
    obs, infos = env.reset(seed=SEED)
    for _ in range(1000):
        actions = {}
        for a in env.agents:
            m = obs[a]["action_mask"]
            actions[a] = int(rng.choice(np.flatnonzero(m))) if m.any() else 0
        obs, rewards, term, trunc, infos = env.step(actions)
        assert set(obs) == set(rewards) == set(term)
        if not env.agents:
            assert rewards["p1"] == -rewards["p2"]
            assert infos["p1"]["winner"] in (0, 1, 2)
            return
    pytest.fail("no battle ended")


def test_pettingzoo_refuses_same_step_autoreset():
    """The adapter's agents end with the battle, and ``reset()`` starts the
    next: an environment that started it at once would lose an episode."""
    pytest.importorskip("pettingzoo")
    from pikapro.rl.pettingzoo import ParallelEnv

    with pytest.raises(ValueError, match="next_step"):
        ParallelEnv("gen9singles", autoreset="same_step")


def test_sb3_vec_env_with_maskable_ppo():
    pytest.importorskip("stable_baselines3")
    sb3_contrib = pytest.importorskip("sb3_contrib")
    from sb3_contrib.common.maskable.utils import get_action_masks

    from pikapro.rl.sb3 import SB3VecEnv

    env = SB3VecEnv("gen9singles", num_envs=4, observation="basic", opponent="random", seed=SEED)
    obs = env.reset()
    assert obs.shape == (4, env.observation_space.shape[0])
    masks = get_action_masks(env)
    assert masks.shape == (4, env.action_space.n) and masks.any(axis=1).all()
    # Same-step reset: a finished battle's last observation is in its info.
    rng = np.random.default_rng(3)
    for _ in range(2000):
        m = env.action_masks()
        env.step_async(np.array([rng.choice(np.flatnonzero(x)) for x in m]))
        obs, rew, dones, infos = env.step_wait()
        if dones.any():
            i = int(np.flatnonzero(dones)[0])
            assert infos[i]["terminal_observation"].shape == obs[i].shape
            assert not np.array_equal(infos[i]["terminal_observation"], obs[i])
            break
    else:
        pytest.fail("no battle ended")
    model = sb3_contrib.MaskablePPO("MlpPolicy", env, n_steps=16, batch_size=32, n_epochs=1, seed=0)
    model.learn(64)
    env.env_method("set_opponent", "simple_heuristics")
    env.env_method("set_opponent", "random", indices=[0])
    env.env_method("set_opponent", "max_base_power", which=[1])


def _sb3_vec_env():
    """`pikapro.rl.sb3.SB3VecEnv`: with stable-baselines3 if it is
    installed, else over a stand-in for its `VecEnv` base (which only
    keeps the constructor's arguments), so that the adapter's own logic is
    tested either way."""
    pytest.importorskip("gymnasium")
    if importlib.util.find_spec("stable_baselines3") is not None:
        from pikapro.rl.sb3 import SB3VecEnv

        return SB3VecEnv

    class VecEnv:
        def __init__(self, num_envs, observation_space, action_space):
            self.num_envs, self.observation_space, self.action_space = num_envs, observation_space, action_space

        def _indices(self, indices):
            return range(self.num_envs) if indices is None else indices

    fakes = {}
    for name in ("stable_baselines3", "stable_baselines3.common", "stable_baselines3.common.vec_env"):
        fakes[name] = types.ModuleType(name)
    fakes["stable_baselines3"].common = fakes["stable_baselines3.common"]
    fakes["stable_baselines3.common"].vec_env = fakes["stable_baselines3.common.vec_env"]
    fakes["stable_baselines3.common.vec_env"].VecEnv = VecEnv
    sys.modules.update(fakes)
    try:
        sys.modules.pop("pikapro.rl.sb3", None)
        return importlib.import_module("pikapro.rl.sb3").SB3VecEnv
    finally:
        for name in [*fakes, "pikapro.rl.sb3"]:
            sys.modules.pop(name, None)


def _fail_rows(step, i):
    """Battle `i`'s rows as `VecEnv` writes them when its battle fails
    (`pikapro_rl`'s `fail`): its episode truncated with end reason 5, no
    winner, no reward, zeroed observations, nobody asked, nothing allowed."""
    step.obs[i] = 0
    step.masks[i] = False
    step.needs_action[i] = False
    step.reward[i] = 0
    step.terminated[i], step.truncated[i] = False, True
    step.end_reason[i], step.winner[i] = 5, -1
    step.final_obs[i] = 0


def test_sb3_a_failed_battle_starts_its_next_episode_with_a_mask():
    """A battle that fails ends its episode with a zeroed observation and
    an all-False mask, and starts its next episode at the environment's
    next step. The adapter takes that step itself: SB3 gets the first
    observation and a usable mask, and the other battles are as they were.

    The failure is injected into the step's rows (nothing in Python makes
    the engine panic); the engine's side, the next step starting the next
    episode, is pikapro-rl's failure tests'."""
    SB3VecEnv = _sb3_vec_env()
    kwargs = dict(num_envs=4, observation="basic", opponent="random", seed=SEED)
    env, twin = SB3VecEnv("gen9singles", **kwargs), SB3VecEnv("gen9singles", **kwargs)
    np.testing.assert_array_equal(env.reset(), twin.reset())
    inner, real_step = env.env, env.env.step
    calls = []

    def step(actions):
        out = real_step(actions)
        calls.append(np.array(actions).reshape(-1).tolist())
        if len(calls) == 6:
            _fail_rows(out, 2)
        return out

    inner.step = step
    rng = np.random.default_rng(3)
    for t in range(40):
        masks = env.action_masks()
        np.testing.assert_array_equal(masks, twin.action_masks())
        assert masks.any(axis=1).all(), "every battle has an action to choose"
        actions = np.array([rng.choice(np.flatnonzero(m)) for m in masks])
        env.step_async(actions)
        twin.step_async(actions)
        obs, rewards, dones, infos = env.step_wait()
        obs2, rewards2, dones2, infos2 = twin.step_wait()
        if t == 5:
            # The failed episode's end, as SB3 reads it.
            assert dones[2] and infos[2]["end_reason"] == 5 and infos[2]["winner"] == -1
            assert infos[2]["TimeLimit.truncated"] and not infos[2]["terminal_observation"].any()
            assert rewards[2] == 0
            # One more step, with no action for anyone, started what was to start.
            assert calls[-1] == [-1] * 4 and len(calls) == 7
            others = [0, 1, 3]
            np.testing.assert_array_equal(rewards[others], rewards2[others])
            np.testing.assert_array_equal(dones[others], dones2[others])
        else:
            np.testing.assert_array_equal(rewards, rewards2)
            np.testing.assert_array_equal(dones, dones2)
            assert [sorted(i) for i in infos] == [sorted(i) for i in infos2]
        # The observation and the mask are a battle's, never the failure's zeros.
        np.testing.assert_array_equal(obs, obs2)
        assert obs.any(axis=1).all()
    assert len(calls) == 41

    # A battle whose episodes keep failing to start: an error that says so,
    # not a mask with nothing in it.
    def failing(actions):
        out = real_step(actions)
        _fail_rows(out, 1)
        return out

    inner.step = failing
    env.step_async(actions)
    with pytest.raises(RuntimeError, match="battle 1 has nothing to choose after 8 restarts"):
        env.step_wait()


def test_a_reset_without_a_seed_keeps_the_constructors():
    """Every adapter: `reset()` goes on from the constructor's `seed=` (the
    first one replaced it with 0 in all but `GymVectorEnv`), and
    `reset(seed=s)` starts over from `s`."""
    pytest.importorskip("gymnasium")
    pytest.importorskip("pettingzoo")
    from pikapro.rl.gym import GymEnv, GymVectorEnv
    from pikapro.rl.pettingzoo import ParallelEnv

    def battles(adapter):
        env = adapter.env
        return [(env.battle(i).seed, env.battle(i).teams(), int(env.last.episode[i])) for i in range(env.n)]

    def sb3_reset(adapter, seed=None):
        if seed is not None:
            adapter.seed(seed)
        adapter.reset()

    makers = {
        "GymEnv": (lambda **kw: GymEnv("gen9singles", **kw), lambda a, seed=None: a.reset(seed=seed)),
        "GymVectorEnv": (lambda **kw: GymVectorEnv("gen9singles", 2, **kw), lambda a, seed=None: a.reset(seed=seed)),
        "ParallelEnv": (lambda **kw: ParallelEnv("gen9singles", **kw), lambda a, seed=None: a.reset(seed=seed)),
        "SB3VecEnv": (lambda **kw: _sb3_vec_env()("gen9singles", num_envs=2, **kw), sb3_reset),
    }
    for name, (make, reset) in makers.items():
        kept, seeded, default, zero = make(seed=SEED), make(), make(), make()
        reset(kept)
        reset(seeded, SEED)
        reset(default)
        reset(zero, 0)
        assert battles(kept) == battles(seeded), f"{name}: reset() left the constructor's seed"
        assert battles(default) == battles(zero), f"{name}: the default seed is 0"
        assert battles(kept) != battles(default), name
        # The next episodes too, and a seeded reset starts over.
        reset(kept)
        reset(seeded)
        assert battles(kept) == battles(seeded) and battles(kept)[0][2] > 0, name
        reset(kept, SEED + 1)
        fresh = make()
        reset(fresh, SEED + 1)
        assert battles(kept) == battles(fresh), name


def test_adapters_refuse_what_they_cant_play():
    pytest.importorskip("gymnasium")
    from pikapro.rl.gym import GymEnv

    with pytest.raises(ValueError):
        GymEnv("championsvgc", masks="factored")
    with pytest.raises(ValueError):
        GymEnv("championsvgc", opponent="external")
    assert rl.OPPONENTS[-1] == "external"


def test_adapters_refuse_external_everywhere():
    """An adapter steps player 0 only: "external" anywhere (the
    constructor, a mixture, set_opponent, env_method) would stall every
    battle, so it raises."""
    pytest.importorskip("gymnasium")
    from pikapro.rl.gym import GymEnv, GymVectorEnv

    mixes = ["external", {"random": 1, "external": 1}, [("simple_heuristics", 1.0), ("external", 2.0)]]
    for mix in mixes:
        with pytest.raises(ValueError, match="pettingzoo"):
            GymEnv("gen9singles", opponent=mix)
        with pytest.raises(ValueError, match="pettingzoo"):
            GymVectorEnv("gen9singles", num_envs=2, opponent=mix)
    g, v = GymEnv("gen9singles"), GymVectorEnv("gen9singles", num_envs=2)
    for mix in mixes:
        with pytest.raises(ValueError, match="external"):
            g.set_opponent(mix)
        with pytest.raises(ValueError, match="external"):
            v.set_opponent(mix)
    v.set_opponent({"random": 1, "max_base_power": 1})  # built-ins are fine
    if importlib.util.find_spec("stable_baselines3") is None:
        return
    from pikapro.rl.sb3 import SB3VecEnv

    for mix in mixes:
        with pytest.raises(ValueError, match="SB3VecEnv"):
            SB3VecEnv("gen9singles", num_envs=2, opponent=mix)
    s = SB3VecEnv("gen9singles", num_envs=2)
    for mix in mixes:
        with pytest.raises(ValueError, match="external"):
            s.set_opponent(mix)
        with pytest.raises(ValueError, match="external"):
            s.env_method("set_opponent", mix)
        with pytest.raises(ValueError, match="external"):
            s.env_method("set_opponent", mix, indices=[1])
    s.env_method("set_opponent", "simple_heuristics", indices=[0])


def _doc_blocks(path, needle):
    text = path.read_text(encoding="utf-8")
    blocks = re.findall(r"```python\n(.*?)```", text, flags=re.S)
    return [b for b in blocks if needle in b]


@pytest.mark.parametrize("doc", ["docs/python/concepts.md"])
def test_the_guides_curriculum_example_runs(doc):
    """The curriculum example switches the VecEnv to "external" and
    plays both players; it runs as written, and player 1 is asked once
    self-play starts."""
    path = Path(__file__).resolve().parents[3] / doc
    if not path.exists():
        pytest.skip(f"{doc} isn't here (not a repository checkout)")
    [block] = _doc_blocks(path, 'env.set_opponent("external")')
    asked = {}

    class Agent:
        def act(self, obs, masks):
            m = masks.reshape(len(masks), -1)
            asked[id(self)] = asked.get(id(self), 0) + int(m.any(-1).sum())
            return np.where(m.any(-1), m.argmax(-1), -1)

    ns = {"Agent": Agent}
    exec(compile(block, doc, "exec"), ns)
    out = ns["out"]
    assert asked.get(id(ns["frozen"]), 0) > 0, "player 1 was never asked"
    # Nobody is stuck: every asked player has a choice.
    assert (out.masks.any(-1) == out.needs_action).all()
