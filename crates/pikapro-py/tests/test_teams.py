"""Random legal teams (pikapro-teams) from Python and in pools."""

import numpy as np
import pytest

import pikapro
from conftest import FORMATS, play_random

MODES = ["uniform", "sets"]


@pytest.mark.parametrize("mode", MODES)
def test_random_teams_are_legal_and_deterministic(format_id, mode):
    teams = [pikapro.random_team(format_id, mode, seed) for seed in range(2_500_000, 2_500_006)]
    for t in teams:
        # A random battle's sets are Showdown's generator's: not validated.
        if format_id.startswith("gen9random") and mode == "sets":
            break
        assert pikapro.validate(t, format_id) == [], t
        assert pikapro.validate(pikapro.pack(t, format_id), format_id) == []
    assert teams[0] == pikapro.random_team(format_id, mode, 2_500_000)
    assert len(set(teams)) == len(teams)
    packed = pikapro.random_team(format_id, mode, 2_500_000, packed=True)
    assert "\n" not in packed and pikapro.export(packed, format_id) == pikapro.export(teams[0], format_id)


@pytest.mark.parametrize("mode", MODES)
def test_battles_on_random_teams_complete(format_id, mode):
    rt = pikapro.RandomTeams(format_id, mode)
    for seed in range(2_500_100, 2_500_104):
        b = pikapro.Battle(format_id, rt.pair(seed), seed=seed)
        play_random(b, pikapro.RandomPolicy(seed))
        assert b.over and b.result is not None


def test_options():
    rt = pikapro.RandomTeams("gen9singles", "uniform", level=80, team_size=3, spreads="showdown")
    t = rt.team(2_500_200)
    assert t.count("Level: 80") == 3 and pikapro.validate(t, "gen9singles") == []
    only = ["Garchomp", "Pikachu", "Snorlax", "Gengar", "Dragonite", "Tyranitar"]
    t = pikapro.random_team("gen9singles", "uniform", 2_500_201, species=only)
    b = pikapro.Battle("gen9singles", [t, t])
    assert {mon.species for mon in b.state().sides[0].team} <= set(only)
    with pytest.raises(ValueError, match="mode"):
        pikapro.RandomTeams("gen9vgc", "random")
    with pytest.raises(ValueError, match="unknown species"):
        pikapro.RandomTeams("gen9vgc", species=["Missingno"])
    with pytest.raises(ValueError):
        pikapro.RandomTeams("gen9vgc", team_size=9)
    # A random battle's sets are Showdown's generator's, which takes none.
    with pytest.raises(ValueError, match="no options"):
        pikapro.RandomTeams("gen9randombattle", "sets", level=50)


def test_validate_reports_problems():
    t = pikapro.random_team("championsvgc", "sets", 2_500_300)
    # Six copies of one set break Species Clause and Item Clause.
    first = t.strip().split("\n\n")[0]
    problems = pikapro.validate("\n\n".join([first] * 6), "championsvgc")
    assert problems and any("Species Clause" in p for p in problems)


def test_pool_draws_fresh_teams_per_episode():
    fmt = "gen9vgc"
    rt = pikapro.RandomTeams(fmt, "sets")
    pool = pikapro.BattlePool(fmt, rt, n=4, seed=2_500_400, threads=2, log=True, autoreset=None)
    seeds = pool.seeds()
    teams = [pool.battle(i).teams() for i in range(4)]
    for i in range(4):
        assert teams[i] == rt.pair(int(seeds[i]))
    assert len({t[0] for t in teams}) == 4
    while not all(pool.battle(i).over for i in range(4)):
        pool.step(pool.sample_random())
    # A pool battle is Battle(format, teams, seed) with its episode's teams.
    b = pikapro.Battle(fmt, pool.battle(2).teams(), seed=int(seeds[2]))
    assert b.teams() == teams[2]
    with pytest.raises(ValueError, match="RandomTeams for gen9vgc"):
        pikapro.BattlePool("championsvgc", rt, n=1)


@pytest.mark.parametrize("mode", MODES)
def test_pool_battles_on_random_teams_complete(mode):
    for fmt in FORMATS:
        pool = pikapro.BattlePool(fmt, pikapro.RandomTeams(fmt, mode), n=8, seed=2_500_500, threads=3)
        r = pool.play_random(24)
        assert r["battles"] == 24 and r["p1_wins"] + r["p2_wins"] + r["ties"] == 24
        # Stepping from Python too, with the numpy policy.
        rng = np.random.default_rng(0)
        before = pool.finished()["battles"]
        while pool.finished()["battles"] < before + 8:
            status, _, _, _ = pool.step(pikapro.sample_slots(pool, rng))
            assert not (status == -1).any()


def test_pool_on_random_teams_is_independent_of_threads():
    fmt = "championsvgc"
    rt = pikapro.RandomTeams(fmt, "uniform")
    results = []
    for threads in (1, 4):
        pool = pikapro.BattlePool(fmt, rt, n=6, seed=2_500_600, threads=threads)
        for _ in range(40):
            pool.step(pool.sample_random())
        results.append((pool.state(), pool.finished()))
    for x, y in zip(results[0][0], results[1][0]):
        np.testing.assert_array_equal(x, y)
    assert results[0][1] == results[1][1]


def test_impossible_options_are_errors():
    for mode in MODES:
        for level in (0, 101, 200):
            with pytest.raises(ValueError, match=f"level {level} is outside 1 to 100"):
                pikapro.RandomTeams("gen9vgc", mode, level=level)
        legends = ["Kyogre", "Groudon", "Dialga", "Palkia", "Lugia", "Ho-Oh", "Mewtwo"]
        with pytest.raises(ValueError, match="restricted limit allow only 2"):
            pikapro.RandomTeams("gen9vgc", mode, species=legends)
    with pytest.raises(ValueError, match="Item Clause"):
        pikapro.RandomTeams("gen9vgc", "sets", items=["Leftovers"])
    # Every draw fails (no species learns the move): an error, not a panic.
    rt = pikapro.RandomTeams(
        "gen9singles", "uniform", team_size=4, species=["Kyogre", "Groudon", "Lugia", "Ho-Oh"], moves=["Spacial Rend"]
    )
    with pytest.raises(ValueError, match="no legal team"):
        rt.team(1)


def test_uniform_sets_go_without_items_when_none_is_free():
    rt = pikapro.RandomTeams("gen9vgc", "uniform", items=["Leftovers"])
    for seed in range(3):
        t = rt.team(seed)
        assert pikapro.validate(t, "gen9vgc") == [] and t.count("@ Leftovers") == 1
    t = pikapro.random_team("gen9vgc", "uniform", 1, items=[])
    assert pikapro.validate(t, "gen9vgc") == [] and "@" not in t
