"""pikapro.compat.poke_env: poke-env's trackers and players on pikapro.Battle.

Skipped without poke-env. The long run (poke-env's three baseline players,
1,000 battles per pairing and format) is gated: set PIKAPRO_POKE_ENV_LONG=1
(and PIKAPRO_POKE_ENV_BATTLES to change the count, PIKAPRO_POKE_ENV_PROCESSES
the worker processes, default 4).
"""

import importlib.util
import json
import os
import random
import sys

import numpy as np
import pytest

import pikapro
from conftest import FORMATS, ROOT

pytest.importorskip("poke_env")

from poke_env.environment.doubles_env import DoublesEnv  # noqa: E402
from poke_env.environment.singles_env import SinglesEnv  # noqa: E402
from poke_env.player import (  # noqa: E402
    MaxBasePowerPlayer,
    RandomPlayer,
    SimpleHeuristicsPlayer,
)

from pikapro.compat import poke_env as pe  # noqa: E402

SEED = 4_100_000
PERSPECTIVES = ("p1", "p2")


def feed_battle(fmt, seed, choose=None):
    """Plays a random battle, feeding both sides' Feeders as the driver
    does; yields (battle, feeders) after each update, before choices."""
    teams = pikapro.RandomTeams(fmt, "sets").pair(seed)
    b = pikapro.Battle(fmt, teams, seed=seed)
    feeders = [pe.Feeder(fmt, s) for s in (0, 1)]
    rng = random.Random(seed)
    start, first = 0, True
    while True:
        n = b.event_count
        for s in (0, 1):
            lines = b.log(PERSPECTIVES[s], start)
            feeders[s].receive(b.header() + lines if first else lines)
            if not b.over:
                feeders[s].receive_request(b.request_json(s))
        first, start = False, n
        yield b, feeders
        if b.over:
            return
        for s in (0, 1):
            legal = b.legal_actions(s)
            if legal and b.submit(s, rng.choice(legal)):
                break


def hp_of(condition):
    return int(condition.split()[0].split("/")[0])


# -- formats, choices --------------------------------------------------------------


def test_formats():
    assert pe.showdown_format("championsvgc") == "gen9championsvgc2026regmc"
    assert pe.showdown_format("championsbss") == "gen9championsbssregmc"
    assert pe.showdown_format("gen9vgc") == "gen9vgc2025regi"
    assert pe.showdown_format("gen9singles") == "gen9customgame"
    assert pe.showdown_format("gen9doubles") == "gen9doublescustomgame"
    for f in FORMATS:
        assert pe.pikapro_format(pe.showdown_format(f)) == f
    with pytest.raises(ValueError):
        pe.showdown_format("gen9ubers")


def test_choice_text():
    assert pe.choice_text("/choose move thunderbolt 1, switch Pikachu") == "move thunderbolt 1, switch Pikachu"
    assert pe.choice_text("/team 3142") == "team 3142"
    assert pe.choice_text("/choose default") == "default"
    assert pe.choice_text("/forfeit") is None
    with pytest.raises(ValueError):
        pe.choice_text("/timer on")


# -- the feeder --------------------------------------------------------------------


@pytest.mark.parametrize("fmt", FORMATS)
def test_feeder_tracks_the_battle(fmt):
    for k in range(3):
        for b, feeders in feed_battle(fmt, SEED + 10 + k):
            for s, f in enumerate(feeders):
                battle = f.battle
                assert f.warnings == []
                assert battle.player_role == PERSPECTIVES[s]
                assert battle.turn == b.turn
                if b.over:
                    continue
                req = json.loads(b.request_json(s))
                # The own side, from the request: every Pokémon, exact HP.
                for mon in req["side"]["pokemon"]:
                    tracked = battle.team[mon["ident"]]
                    assert tracked.current_hp == hp_of(mon["condition"])
                assert battle.teampreview == bool(req.get("teamPreview"))
                assert battle._wait == bool(req.get("wait"))
                # A pending decision exactly when the request needs one.
                assert (f.pending is not None) == (b.request_kind(s) != "wait")
        for s, f in enumerate(feeders):
            assert f.battle.finished
            assert f.battle.won == (b.winner == s) if b.winner in (0, 1) else f.battle.won is None


def test_feeder_errors_and_waits():
    b = pikapro.Battle("gen9singles", pikapro.RandomTeams("gen9singles", "sets").pair(SEED), seed=SEED)
    f = pe.Feeder("gen9singles", "p1")
    f.receive(b.header() + b.log("p1"))
    assert f.pending is None
    assert f.receive_request(b.request_json(0)) == pe.Decision(False)
    f.pending = None
    f.receive_error("[Invalid choice] Can't move: Your Pikachu doesn't have a move matching tackle")
    assert f.pending == pe.Decision(True)
    f.pending = None
    f.receive_error("[Unavailable choice] Can't switch: The active Pokémon is trapped")
    assert f.pending is None  # poke-env waits for the updated request
    assert f.errors[-1].startswith("[Unavailable choice]")
    wait = json.loads(b.request_json(0))
    f.receive_request({"wait": True, "side": wait["side"]})
    assert f.pending is None and f.battle._wait
    with pytest.raises(pe.FeederError) as e:
        f.receive(["|switch|p2a: Nobody|NotASpecies, L50|100/100"])
    assert e.value.line.startswith("|switch|")


# -- poke-env quirks -------------------------------------------------------------------

BOUNCE_TEAMS = [
    "Hypno\nAbility: Insomnia\n- Poison Gas\n- Splash\n\nWobbuffet\nAbility: Telepathy\n- Memento\n",
    "Espeon\nAbility: Magic Bounce\n- Splash\n\nSkarmory\nAbility: Sturdy\n- Splash\n",
]
# Showdown 0.11.11 sends this line for this battle too (gen5 seed 16,7,3,4
# on gen9doublescustomgame@@@!Team Preview, checked with the oracle)
BOUNCE_LINE = "|move|p2a: Espeon|Poison Gas|p1a: Hypno|[from] ability: Magic Bounce|[miss]|[spread] "


def test_quirk_miss_before_spread():
    split = BOUNCE_LINE.split("|")
    assert pe.feeder.workaround(split) == "miss-before-spread"
    assert split[-2:] == ["[spread] ", "[miss]"]
    assert pe.feeder.workaround("|move|p1a: A|Tackle|p2a: B|[miss]".split("|")) is None
    b = pikapro.Battle("gen9doubles", BOUNCE_TEAMS, seed=(16, 7, 3, 4))
    feeders = [pe.Feeder("gen9doubles", s) for s in (0, 1)]
    start = 0
    for c1, c2 in [("move 2, move 1 2", "move 1, move 1"), ("move 1, pass", "move 1, move 1"), (None, None)]:
        for s in (0, 1):
            lines = b.log(PERSPECTIVES[s], start)
            feeders[s].receive((b.header() if start == 0 else []) + lines)
            feeders[s].receive_request(b.request_json(s))
        start = b.event_count
        if c1:
            b.submit(0, c1)
            b.submit(1, c2)
    assert BOUNCE_LINE in b.log("p1")
    for f in feeders:
        assert f.quirks == {"miss-before-spread": 1} and f.warnings == []
        espeon = next(m for k, m in f.battle.team.items() if "Espeon" in k) if f.role == "p2" else f.battle.opponent_team["p2: Espeon"]
        assert "poisongas" not in espeon.moves


def test_quirk_locked_move_target():
    from pikapro.compat.poke_env.driver import BattleRecord, _locked_move_targets

    rec = BattleRecord(0, 0, [])
    locked = {"active": [{"moves": [{"move": "Outrage", "id": "outrage"}], "trapped": True}, {"moves": [{"move": "Tackle", "id": "tackle", "target": "normal"}]}]}
    assert _locked_move_targets("move outrage, move tackle 1", locked, rec) == "move 1, move tackle 1"
    assert _locked_move_targets("move outrage 2, move tackle 1", locked, rec) == "move outrage 2, move tackle 1"
    assert _locked_move_targets("move tackle 1, move outrage", {"active": locked["active"][::-1]}, rec) == "move tackle 1, move 1"
    assert rec.quirks == {"locked-move-target": 2}


def test_quirk_stale_move_target():
    from pikapro.compat.poke_env.driver import BattleRecord, _stale_move_targets

    rec = BattleRecord(0, 0, [])
    milk = {"move": "Milk Drink", "id": "milkdrink", "target": "adjacentAllyOrSelf"}
    tackle = {"move": "Tackle", "id": "tackle", "target": "normal"}
    helping = {"move": "Helping Hand", "id": "helpinghand", "target": "adjacentAlly"}
    request = {"active": [{"moves": [tackle, milk]}, {"moves": [milk, helping]}]}
    # Champions' Milk Drink needs a target, which poke-env's data doesn't give it.
    assert _stale_move_targets("move tackle 1, move milkdrink", request, rec) == "move tackle 1, move milkdrink -2"
    assert _stale_move_targets("move milkdrink terastallize, pass", request, rec) == "move milkdrink terastallize -1, pass"
    assert _stale_move_targets("move 2, move helpinghand", request, rec) == "move 2 -1, move helpinghand -1"
    # A target given, or a move whose target has more than one legal value: as sent.
    for choice in ("move milkdrink -1, move milkdrink -2", "move tackle, move 1 -2", "switch 3, pass"):
        assert _stale_move_targets(choice, request, rec) == choice
    assert rec.quirks == {"stale-move-target": 3}


def test_quirk_stale_move_target_in_a_battle():
    """A seeded championsvgc battle in which poke-env's RandomPlayer picks
    Milk Drink (a long-run battle, seed 4110117): no invalid choice."""
    fmt = "championsvgc"
    pair = pikapro.RandomTeams(fmt, "sets").pair(4110117)
    assert "Milk Drink" in pair[0]
    a, b = pe.make_player(RandomPlayer, fmt), pe.make_player(MaxBasePowerPlayer, fmt)
    result = pe.play(a, b, fmt, 1, teams=pair, seed=4110117, python_seed=33)
    check_clean(result, 1)
    assert result.quirks.get("stale-move-target", 0) >= 1, result.quirks


def test_quirk_switch_name_ambiguous():
    from pikapro.compat.poke_env.driver import BattleRecord, _switch_names

    # Imposter copies p2b's Beedrill: Showdown's chooseSwitch then reads
    # "switch Beedrill" as the active Ditto (current species beedrill)
    teams = [
        "Ditto\nAbility: Imposter\n- Transform\n\nPikachu\nAbility: Static\n- Thunder Shock\n\nBeedrill\nAbility: Swarm\n- Poison Jab\n",
        "Pikachu\nAbility: Static\n- Thunder Shock\n\nBeedrill\nAbility: Swarm\n- Poison Jab\n",
    ]
    b = pikapro.Battle("gen9doubles", teams, seed=1)
    assert "|-transform|p1a: Ditto|p2b: Beedrill|[from] ability: Imposter" in b.log("p1")
    with pytest.raises(ValueError, match="switch to an active"):
        b.parse_choice(0, "switch Beedrill, move 1 1")
    request = json.loads(b.request_json(0))
    rec = BattleRecord(0, 0, [])
    assert _switch_names("move 1 1, switch Beedrill", request, b, 0, rec) == "move 1 1, switch 3"
    assert _switch_names("switch Pikachu, move 1 1", request, b, 0, rec) == "switch Pikachu, move 1 1"
    assert rec.quirks == {"switch-name-ambiguous": 1}


def test_open_team_sheets():
    fmt = "gen9vgc"
    a = pe.make_player(SimpleHeuristicsPlayer, fmt, accept_open_team_sheet=True)
    b = pe.make_player(RandomPlayer, fmt)
    result = pe.play(a, b, fmt, 2, seed=SEED + 900)
    check_clean(result, 2)
    # Both accept: both sheets, as Showdown's room sends them.
    a = pe.make_player(RandomPlayer, fmt, accept_open_team_sheet=True)
    b = pe.make_player(RandomPlayer, fmt, accept_open_team_sheet=True)
    known = []

    def on_battle(rec, battle):
        known.append(rec.winner)

    result = pe.play(a, b, fmt, 2, seed=SEED + 910, on_battle=on_battle)
    check_clean(result, 2)
    assert result.quirks == {} and len(known) == 2
    # The players' own battles know every foe's ability and moves from the
    # sheets.
    for battle in a.battles.values():
        assert battle.player_username == a.username and battle.finished
        foes = battle.opponent_team.values()
        assert len(foes) == 6 and all(m.ability and len(m.moves) == 4 for m in foes)


# -- order and action maps -----------------------------------------------------------


@pytest.mark.parametrize("fmt", FORMATS)
def test_order_maps_round_trip(fmt):
    rng = random.Random(fmt)
    counts = {"order": 0, "action": 0, "mask": 0}
    env = DoublesEnv if pikapro.ActionSpace(fmt).active == 2 else SinglesEnv
    for k in range(3):
        for b, feeders in feed_battle(fmt, SEED + 100 + k):
            if b.over:
                break
            for s in (0, 1):
                legal = b.legal_actions(s)
                if not legal:
                    continue
                battle = feeders[s].battle
                # ours -> poke-env -> ours is the identity, over every legal index
                for i in rng.sample(legal, min(len(legal), 25)):
                    order = pe.index_to_order(i, battle, b.space)
                    assert pe.order_to_index(order, b, s) == i, (b.space.describe(i), str(order))
                    counts["order"] += 1
                    if i < b.space.joint_size:
                        action = pe.index_to_action(i, battle, b.space)
                        assert pe.action_to_index(action, battle, b, s) == i
                        counts["action"] += 1
                if battle.teampreview:
                    continue
                # poke-env's mask -> ours -> poke-env is the identity: every
                # action poke-env offers is one Showdown accepts
                mask = env.get_action_mask(battle)
                if env is SinglesEnv:
                    actions = [a for a, m in enumerate(mask) if m]
                else:
                    half = len(mask) // 2
                    first = [a for a in range(half) if mask[a]]
                    second = [a for a in range(half) if mask[half + a]]
                    actions = [np.array([x, y]) for x in first for y in second]
                for a in rng.sample(actions, min(len(actions), 25)):
                    try:
                        i = pe.action_to_index(a, battle, b, s)
                    except ValueError as e:
                        # poke-env's per-slot mask pairs two switches to one
                        # Pokémon or two gimmicks; a trap the request hid
                        assert "incompatible" in str(e) or str(e).startswith("[Unavailable choice]"), e
                        continue
                    back = pe.index_to_action(i, battle, b.space)
                    assert np.array_equal(np.asarray(back), np.asarray(a)), (a, b.space.describe(i), back)
                    counts["mask"] += 1
    assert min(counts.values()) > 50, counts


# -- the driver ----------------------------------------------------------------------


def check_clean(result, n):
    s = result.summary()
    assert result.n == n
    assert s["errors"] == 0, [(b.seed, b.error, b.error_line) for b in result.errors][:3]
    assert s["invalid"] == 0, [m for b in result.battles for side in b.invalid_messages for m in side][:5]
    assert s["fallbacks"] == 0 and s["stalls"] == 0, s
    for rec in result.battles:
        assert rec.reason is not None and rec.turns > 0
        assert rec.warnings == [[], []]
    return s


@pytest.mark.parametrize("fmt", FORMATS)
def test_driver_plays_every_format(fmt):
    for i, (A, B) in enumerate([(RandomPlayer, SimpleHeuristicsPlayer), (MaxBasePowerPlayer, RandomPlayer)]):
        a, b = pe.make_player(A, fmt), pe.make_player(B, fmt)
        result = pe.play(a, b, fmt, 4, seed=SEED + 200 + 10 * i, python_seed=i)
        check_clean(result, 4)
        assert a.n_finished_battles == 4 and b.n_finished_battles == 4
        assert a.n_won_battles == result.wins[0] and b.n_won_battles == result.wins[1]


def test_driver_is_deterministic_and_takes_teams():
    fmt = "gen9vgc"
    pair = pikapro.RandomTeams(fmt, "sets").pair(SEED + 300)
    runs = []
    for _ in range(2):
        a, b = pe.make_player(RandomPlayer, fmt), pe.make_player(RandomPlayer, fmt)
        r = pe.play(a, b, fmt, 3, teams=pair, seed=SEED + 300, python_seed=7, register=False)
        runs.append([(x.winner, x.turns) for x in r.battles])
        assert all(x.teams == pair for x in r.battles)
    assert runs[0] == runs[1]


@pytest.mark.parametrize("fmt", FORMATS)
def test_players_and_driver_agree_on_doubles(fmt):
    """poke-env's reading of the Showdown format id, the action space's
    active count and the driver's check agree for every format (the
    driver once took every format but gen9singles for doubles, refusing
    championsbss's players)."""
    doubles = pikapro.ActionSpace(fmt).active == 2
    assert pe.make_player(RandomPlayer, fmt).format_is_doubles == doubles
    assert pe._formats.is_doubles(fmt) == doubles == pe._formats.is_doubles(pe.showdown_format(fmt))


def test_driver_plays_a_short_championsbss_game():
    a, b = pe.make_player(RandomPlayer, "championsbss"), pe.make_player(MaxBasePowerPlayer, "championsbss")
    result = pe.play(a, b, "championsbss", 2, seed=SEED + 250, python_seed=3)
    check_clean(result, 2)
    assert a.n_finished_battles == 2 and b.n_finished_battles == 2


def test_driver_rejects_a_singles_player_in_doubles():
    a = pe.make_player(RandomPlayer, "gen9singles")
    b = pe.make_player(RandomPlayer, "gen9doubles")
    with pytest.raises(ValueError, match="battle_format"):
        pe.play(a, b, "gen9doubles", 1)


class AsyncRandomPlayer(RandomPlayer):
    async def choose_move(self, battle):
        return self.choose_random_move(battle)

    async def teampreview(self, battle):
        return self.random_teampreview(battle)


def test_driver_awaits_async_players():
    fmt = "championsvgc"
    a, b = pe.make_player(AsyncRandomPlayer, fmt), pe.make_player(RandomPlayer, fmt)
    check_clean(pe.play(a, b, fmt, 2, seed=SEED + 400), 2)


def test_cross_evaluate():
    fmt = "gen9vgc"
    players = [pe.make_player(cls, fmt) for cls in (RandomPlayer, MaxBasePowerPlayer, SimpleHeuristicsPlayer)]
    table = pe.cross_evaluate(players, fmt, 3, seed=SEED + 500)
    names = [p.username for p in players]
    assert set(table) == set(names)
    for x in names:
        for y in names:
            if x == y:
                assert table[x][y] is None
            else:
                assert 0 <= table[x][y] <= 1
    assert all(p.n_finished_battles == 0 for p in players)  # reset after each pair


def test_play_parallel_matches_play():
    fmt = "gen9singles"
    serial = pe.play(pe.make_player(RandomPlayer, fmt), pe.make_player(MaxBasePowerPlayer, fmt), fmt, 4, seed=SEED + 600, python_seed=3)
    parallel = pe.play_parallel(RandomPlayer, MaxBasePowerPlayer, fmt, 4, processes=1, seed=SEED + 600, python_seed=3)
    assert [(r.seed, r.winner, r.turns) for r in serial.battles] == [(r.seed, r.winner, r.turns) for r in parallel.battles]
    two = pe.play_parallel(RandomPlayer, MaxBasePowerPlayer, fmt, 4, processes=2, seed=SEED + 600)
    assert [r.index for r in two.battles] == [0, 1, 2, 3]
    check_clean(two, 4)


# -- the view oracle -------------------------------------------------------------------


def load_tool(name):
    path = ROOT / "tools" / "view-oracle" / f"{name}.py"
    spec = importlib.util.spec_from_file_location(f"view_oracle_{name}", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def no_floats(value):
    if isinstance(value, float):
        return False
    if isinstance(value, dict):
        return all(isinstance(k, str) and no_floats(v) for k, v in value.items())
    if isinstance(value, list):
        return all(no_floats(v) for v in value)
    return value is None or isinstance(value, (str, int, bool))


@pytest.mark.parametrize("fmt", FORMATS)
def test_view_oracle(fmt):
    gen = load_tool("gen_decisions")
    view = load_tool("pokeenv_view")
    records = [r for k in range(3) for r in gen.battle_records(fmt, SEED + 700 + k)]
    # JSON round trip, as the files carry them
    records = [json.loads(json.dumps(r)) for r in records]
    assert records[0]["index"] == 0 and records[0]["lines"][0].startswith("|t:|")
    assert records[-1]["request"] is None
    out = list(view.states(records))
    assert len(out) == len(records)
    for rec, state in zip(records, out):
        assert "error" not in state, state
        assert (state["seed"], state["side"], state["index"]) == (rec["seed"], rec["side"], rec["index"])
        assert no_floats(state["state"])
        assert state["state"]["role"] == rec["side"]
    last = [s["state"] for s in out if s["index"] > 0 and records[out.index(s)]["request"] is None]
    assert len(last) == 6 and all(s["finished"] for s in last)


def test_snapshot_schema():
    for b, feeders in feed_battle("championsvgc", SEED + 800):
        if b.turn >= 2:
            break
    snap = pe.snapshot(feeders[0].battle)
    assert set(snap) == {
        "role", "turn", "finished", "won", "teampreview", "wait", "force_switch",
        "used_tera", "opponent_used_tera", "used_mega", "opponent_used_mega",
        "weather", "fields", "side_conditions", "opponent_side_conditions",
        "active", "opponent_active", "team", "opponent_team", "opponent_preview",
    }  # fmt: skip
    assert len(snap["active"]) == 2 and len(snap["opponent_preview"]) == 6
    mon = next(iter(snap["team"].values()))
    assert set(mon) == {
        "species", "forme", "level", "gender", "hp", "max_hp", "status", "boosts", "item",
        "ability", "moves", "types", "tera_type", "terastallized", "mega", "fainted",
        "active", "revealed", "effects",
    }  # fmt: skip
    assert mon["level"] == 50 and len(mon["moves"]) == 4 and mon["item"] != pe.UNKNOWN
    foe = [m for m in snap["opponent_team"].values() if m["revealed"]]
    assert foe and all(m["max_hp"] == 100 for m in foe)  # Champions shows percentages
    json.dumps(snap)


# -- the long run ----------------------------------------------------------------------

LONG = os.environ.get("PIKAPRO_POKE_ENV_LONG")


@pytest.mark.skipif(not LONG, reason="set PIKAPRO_POKE_ENV_LONG=1 for the 1,000-battle baseline runs")
@pytest.mark.parametrize("fmt", FORMATS)
def test_baselines_long(fmt):
    n = int(os.environ.get("PIKAPRO_POKE_ENV_BATTLES", "1000"))
    processes = int(os.environ.get("PIKAPRO_POKE_ENV_PROCESSES", "4"))
    pairings = [
        (RandomPlayer, MaxBasePowerPlayer),
        (MaxBasePowerPlayer, SimpleHeuristicsPlayer),
        (SimpleHeuristicsPlayer, RandomPlayer),
        (RandomPlayer, RandomPlayer),
    ]
    for k, (A, B) in enumerate(pairings):
        result = pe.play_parallel(A, B, fmt, n, processes=processes, seed=SEED + 10_000 * (k + 1), python_seed=k)
        s = check_clean(result, n)
        print(f"\n{fmt} {A.__name__} vs {B.__name__}: {json.dumps(s)}", file=sys.stderr)
