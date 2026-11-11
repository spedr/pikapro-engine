"""Best-of series through pikapro.rl.ChannelSession and ChannelSeries: each
game's session, given the series the earlier games were recorded in,
gives what VecEnv's series mode gives (observations, masks, the series
row), with open sheets; and a Bo3 recorded on Showdown's own server reads
the same way, its ``|showteam|`` sheets as the environment's ``ots``
shows them."""

import json
from pathlib import Path

import numpy as np
import pytest

import pikapro
import pikapro.rl as rl

from test_channel_session import random_action

FIXTURE = Path(__file__).parent / "fixtures" / "showdown" / "bo3-gen9championsvgc2026regmcbo3.json"


def mirrored(row):
    """The environment's series row (player 0's) as player 1 sees it."""
    game, w0, w1, result = (int(x) for x in row)
    return [game, w1, w0, {1: 2, 2: 1}.get(result, result)]


@pytest.mark.parametrize("fmt", ["championsvgc", "gen9vgc"])
def test_series_sessions_match_vec_env_series_mode(fmt):
    games = 0
    for seed in range(3):
        env = rl.VecEnv(fmt, 1, opponent="external", record=True, series=3, threads=1)
        out = env.reset(seed=5_100_000 + seed)
        # Each player carries its own series from game to game.
        series = [rl.ChannelSeries(fmt), rl.ChannelSeries(fmt)]
        rng = np.random.default_rng(seed)
        cut = False
        while not cut:
            record = env.record(0)
            teams = list(record["teams"])
            battle = pikapro.Battle(fmt, teams, seed=record["seed"])
            sessions = [rl.ChannelSession(fmt, team=teams[p], series=series[p]) for p in range(2)]
            # The first update, the sheets (Showdown's Open Team Sheets at
            # team preview: each side's sets, packed), then the requests.
            for side, s in enumerate(sessions):
                s.feed_many(battle.header())
                s.feed_many(battle.log(f"p{side + 1}", 0))
                s.feed_many([f"|showteam|p{k + 1}|{teams[k]}" for k in range(2)])
            for side, s in enumerate(sessions):
                s.feed(f"|request|{battle.request_json(side)}")
            start, done = battle.event_count, 0
            while True:
                over = bool(out.terminated[0] or out.truncated[0])
                for p, s in enumerate(sessions):
                    # The series row, the game's result included at its end.
                    want = [int(x) for x in out.series[0]] if p == 0 else mirrored(out.series[0])
                    assert list(s.series_row) == want
                    if over:
                        # No request after the end: the own side stays the
                        # last request's (no decision is made on it).
                        continue
                    np.testing.assert_array_equal(s.obs, out.obs[0, p])
                    assert s.needs_action == bool(out.needs_action[0, p])
                    if s.needs_action:
                        np.testing.assert_array_equal(s.mask, out.masks[0, p])
                if over:
                    break
                actions = np.zeros((1, 2, env.actions_per_player), dtype=np.int64)
                for p, s in enumerate(sessions):
                    if s.needs_action:
                        a = random_action(env.space, "factored", out.masks[0, p], out.min_switches[0, p], rng)
                        actions[0, p] = a
                        s.choice(a)
                out = env.step(actions)
                if (out.status[0] == 2).any():
                    cut = True  # an unavailable choice: its update isn't rendered here
                    break
                for side, choice in env.record(0)["choices"][done:]:
                    if battle.submit(side, choice):
                        for k, s in enumerate(sessions):
                            s.feed_many(battle.log(f"p{k + 1}", start))
                        if not battle.over:
                            for k, s in enumerate(sessions):
                                s.feed(f"|request|{battle.request_json(k)}")
                        start = battle.event_count
                done = len(env.record(0)["choices"])
            if cut:
                break
            games += 1
            for p, s in enumerate(sessions):
                assert s.finished and not s.unknown_lines
                series[p].record(s)
            assert series[0].wins == series[1].wins and series[0].game == series[1].game
            if series[0].over:
                result = int(out.series[0][3])
                assert series[0].result == {1: "p1", 2: "p2", 3: "tie"}[result]
                break
            # NEXT_STEP autoreset: this step starts the series' next game.
            out = env.step(np.zeros((1, 2, env.actions_per_player), dtype=np.int64))
            assert int(out.series[0][0]) == series[0].game
    assert games >= 5


def test_series_objects():
    series = rl.ChannelSeries("gen9championsvgc2026regmcbo3")
    assert (series.best_of, series.game, series.wins, series.ties, series.result, series.over) == (3, 1, (0, 0), 0, None, False)
    assert rl.ChannelSeries("championsvgc", 5).best_of == 5
    with pytest.raises(ValueError):
        rl.ChannelSeries("championsvgc", 4)
    for bad in (256, -1):  # out of u8 range: the rule's message, not an OverflowError
        with pytest.raises(ValueError, match="best of 3, 5, 7 or 9"):
            rl.ChannelSeries("championsvgc", bad)
    with pytest.raises(ValueError, match="gen9championsvgc2026regmc"):  # names the formats it reads
        rl.ChannelSeries("gen1ou")
    with pytest.raises(ValueError, match="gen9championsvgc2026regmc"):
        rl.ChannelSession("gen9ubers")
    session = rl.ChannelSession("gen9championsvgc2026regmcbo3", series=series)
    assert list(session.series_row) == [1, 0, 0, 0]
    with pytest.raises(ValueError):  # no end read, no result given
        series.record(session)
    with pytest.raises(ValueError):
        rl.ChannelSession("gen9vgc2025regi", series=series)  # another format's series
    series.record(session, winner="p2")
    assert (series.game, series.wins, series.recorded) == (2, (0, 1), 1)
    second = rl.ChannelSession("gen9championsvgc2026regmcbo3", username="Bob", series=series)
    second.feed_many(["|player|p1|Alice|1|", "|player|p2|Bob|1|"])
    assert list(second.series_row) == [2, 1, 0, 0], "p2's wins first, from Bob's side"
    assert second.view.series == {"game": 2, "wins": 1, "opponent_wins": 0, "result": 0}
    with pytest.raises(ValueError):  # game 1 again
        series.record(session, tie=True)
    assert rl.ChannelSession("championsvgc").series_row is None


def test_a_series_the_server_ended_is_settled():
    """A series the server ends early (a forfeit in the series room, a game
    lost by disconnection): the series and a game's session show it
    settled, whatever the games' score."""
    fmt = "gen9championsvgc2026regmcbo3"
    series = rl.ChannelSeries(fmt)
    session = rl.ChannelSession(fmt, username="Bob", series=series)
    session.feed_many(["|player|p1|Alice|1|", "|player|p2|Bob|1|"])
    series.record(session, winner="p2")
    with pytest.raises(ValueError):
        series.settle()  # a winner or a tie
    series.settle(winner="p1")  # Bob forfeited the series in its room
    assert (series.over, series.result, series.wins) == (True, "p1", (0, 1))
    series.settle(winner="p1")  # again: nothing changes
    with pytest.raises(ValueError):
        series.settle(tie=True)
    with pytest.raises(ValueError):
        series.record(session, winner="p1")  # over
    game = rl.ChannelSession(fmt, username="Bob", series=rl.ChannelSeries(fmt))
    game.feed_many(["|player|p1|Alice|1|", "|player|p2|Bob|1|"])
    game.settle_series(winner="p2")
    assert list(game.series_row) == [1, 0, 0, 1], "Bob's side won the series"
    with pytest.raises(ValueError):
        rl.ChannelSession(fmt).settle_series(tie=True)  # not a series


def test_a_win_naming_neither_player_needs_the_result():
    fmt = "gen9championsvgc2026regmcbo3"
    series = rl.ChannelSeries(fmt)
    session = rl.ChannelSession(fmt, series=series)
    session.feed_many(["|player|p1|Alice|1|", "|player|p2|Bob|1|", "|start", "|win|Nobody"])
    assert session.finished and session.winner is None
    with pytest.raises(ValueError):
        series.record(session)  # not a tie: its result is unknown
    series.record(session, winner="p1")
    assert (series.wins, series.ties) == ((1, 0), 0)


def test_the_session_handler_settles_a_series_the_room_ended():
    from pikapro.showdown import SessionHandler
    from pikapro.showdown.room import SeriesRoom

    fmt = "gen9championsvgc2026regmcbo3"
    room = SeriesRoom(f"game-bestof3-{fmt}-1", "Bob")
    room.state = rl.ChannelSeries(fmt)
    handler = SessionHandler()
    handler._series_sides[room] = "p2"
    for line in ["|title|Alice vs. Bob", "|win|Alice"]:
        room.observe(line, "bob")
    handler.series_end(room)
    assert (room.state.over, room.state.result) == (True, "p1")


def fixture():
    return json.loads(FIXTURE.read_text())


def sheet_lines(f, lines):
    """The game's lines with Showdown's ``|showteam|`` sheets replaced by
    the teams as the environment's ``ots`` has them (packed by pikapro)."""
    me = next(line.split("|")[2] for line in lines if line.startswith("|player|") and line.split("|")[3] == f["username"])
    packed = {
        me: pikapro.pack(f["team"], "championsvgc"),
        ("p2" if me == "p1" else "p1"): pikapro.pack(f["opponent_team"], "championsvgc"),
    }
    out = []
    for line in lines:
        if line.startswith("|showteam|"):
            side = line.split("|")[2]
            line = f"|showteam|{side}|{packed[side]}"
        out.append(line)
    return out


def decisions(session, lines):
    """The observation and mask at each decision of a game fed line by line."""
    seen = []
    for line in lines:
        session.feed(line)
        if line.startswith("|request|") and session.needs_action:
            seen.append((session.obs, session.mask))
    return seen


def test_a_showdown_bo3_reads_as_the_environment_series():
    """A Bo3 recorded on Showdown's own server (player a's battle rooms):
    each game read with the series so far, its sheets from ``|showteam|``
    exactly the environment's ``ots`` sheets (Showdown's sheets drop the
    stats; the environment's show none either)."""
    f = fixture()
    assert len(f["games"]) == 3
    series = rl.ChannelSeries(f["format"])
    for k, lines in enumerate(f["games"]):
        assert sum(line.startswith("|showteam|") for line in lines) == 2
        s = rl.ChannelSession(f["format"], username=f["username"], team=f["team"], series=series)
        assert list(s.series_row)[:1] == [k + 1]
        ours = rl.ChannelSession(f["format"], username=f["username"], team=f["team"], series=series)
        closed = rl.ChannelSession(f["format"], username=f["username"], team=f["team"], series=series)
        got = decisions(s, lines)
        want = decisions(ours, sheet_lines(f, lines))
        without = decisions(closed, [line for line in lines if not line.startswith("|showteam|")])
        assert len(got) == len(want) > 3
        for (a, ma), (b, mb) in zip(got, want):
            np.testing.assert_array_equal(a, b)
            np.testing.assert_array_equal(ma, mb)
        # The sheets show: without them the team preview observation differs.
        assert not np.array_equal(got[0][0], without[0][0])
        assert s.finished and s.error is None and s.unknown_count == 0
        assert s.winner == f["winners"][k]
        series.record(s)
        # The session's row counts this game; the series recorded it.
        mine, theirs = (0, 1) if s.side == "p1" else (1, 0)
        assert list(s.series_row)[1:3] == [series.wins[mine], series.wins[theirs]]
        assert sum(series.wins) + series.ties == series.recorded == k + 1
    assert series.over
    winner_side = series.result
    assert winner_side in ("p1", "p2")
    assert (winner_side == s.side) == (f["series_winner"] == f["username"])
