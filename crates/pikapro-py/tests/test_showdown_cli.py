"""``python -m pikapro.showdown``'s arguments: what it refuses, it refuses
before it connects, with a message and exit status 2 (never a
traceback)."""

from __future__ import annotations

import logging

import pytest

pytest.importorskip("websockets")

import pikapro  # noqa: E402
from pikapro.showdown import __main__ as cli  # noqa: E402

FMT = "gen9championsvgc2026regmc"


class NoClient:
    """Stands for ``ShowdownClient``: making one is connecting."""

    def __init__(self, *args, **kwargs):
        raise AssertionError("the command line went on to connect")


class StubClient:
    """A ``ShowdownClient`` that plays nothing: it keeps what it was given."""

    made: list = []

    def __init__(self, *args, **kwargs):
        self.args, self.kwargs = args, kwargs
        self.results, self.series_results = [], []
        self.calls: list = []
        StubClient.made.append(self)

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        return None

    async def ladder(self, games, fmt, team):
        self.calls.append(("ladder", games, fmt, team))
        return []

    async def accept_challenges(self, from_user, games, team, format=None):
        self.calls.append(("accept", from_user, games, team, format))
        return []


@pytest.fixture(autouse=True)
def _logging():
    """``main`` adds a handler to the package's logger and sets its level."""
    log = logging.getLogger("pikapro.showdown")
    handlers, level = list(log.handlers), log.level
    yield
    log.handlers[:] = handlers
    log.setLevel(level)


def refused(monkeypatch, capsys, argv) -> str:
    """Runs the command line, which must refuse ``argv`` with status 2
    before making a client; returns what it printed to stderr."""
    monkeypatch.setattr(cli, "ShowdownClient", NoClient)
    try:
        status = cli.main(argv)
    except SystemExit as e:  # argparse's own refusals
        status = e.code
    err = capsys.readouterr().err
    assert status == 2, (status, err)
    assert "Traceback" not in err and "error: " in err
    return err


def test_a_team_file_that_is_no_team_is_refused_before_connecting(monkeypatch, capsys, tmp_path):
    good = tmp_path / "good.txt"
    good.write_text(pikapro.sample_team("championsvgc"), encoding="utf-8")
    binary = tmp_path / "binary.txt"
    binary.write_bytes(b"Pikachu @ Light Ball\n\xff\xfe\x00- Thunderbolt\n")
    unknown = tmp_path / "unknown.txt"
    unknown.write_text("Pikachu @ Light Ball\nAbility: Static\n- Thunderbolt\n- No Such Move\n", encoding="utf-8")
    garbage = tmp_path / "garbage.txt"
    garbage.write_text("these are not\nthe teams you are looking for\n", encoding="utf-8")
    empty = tmp_path / "empty.txt"
    empty.write_text(" \n\n", encoding="utf-8")
    missing = tmp_path / "missing.txt"
    base = ["ladder", "--format", FMT, "--team"]
    err = refused(monkeypatch, capsys, [*base, str(binary)])
    assert f"error: --team {binary}: not UTF-8 text" in err and "A team file is an export" in err
    err = refused(monkeypatch, capsys, [*base, str(unknown)])
    assert f"error: --team {unknown}: team: line 4: unknown move" in err
    err = refused(monkeypatch, capsys, [*base, str(garbage)])
    assert f"error: --team {garbage}: team: line 1: unknown species" in err
    err = refused(monkeypatch, capsys, [*base, str(empty)])
    assert f"error: --team {empty}: the file is empty" in err
    err = refused(monkeypatch, capsys, [*base, str(missing)])
    assert f"error: --team {missing}: No such file or directory" in err
    # Each file is checked, not only the first; and with no --format too.
    err = refused(monkeypatch, capsys, [*base, str(good), "--team", str(unknown)])
    assert f"--team {unknown}:" in err
    err = refused(monkeypatch, capsys, ["accept", "--team", str(unknown)])
    assert f"--team {unknown}: team: line 4: unknown move" in err
    err = refused(monkeypatch, capsys, ["challenge", "someone", "--format", FMT, "--team", str(binary)])
    assert "not UTF-8 text" in err


@pytest.mark.parametrize("level", ["LOUD", "", "info warning", "-1"])
def test_a_bad_log_level_is_refused(monkeypatch, capsys, level):
    for argv in (["formats", "--log-level", level], ["ladder", "--format", FMT, f"--log-level={level}"]):
        err = refused(monkeypatch, capsys, argv)
        assert "--log-level" in err and "is not a logging level (DEBUG, INFO, WARNING, ERROR or CRITICAL)" in err


@pytest.mark.parametrize(
    "policy, why",
    [
        ("smartest", "neither a built-in"),
        ("no_such_module_xyz:act", "No module named 'no_such_module_xyz'"),
        ("os:no_such_function", "no attribute 'no_such_function'"),
        ("os:sep", "not a callable"),
        ("os:", "neither a built-in"),
    ],
)
def test_a_policy_that_cant_be_loaded_is_refused(monkeypatch, capsys, policy, why):
    err = refused(monkeypatch, capsys, ["ladder", "--format", FMT, "--policy", policy])
    assert "error: --policy" in err and why in err


def test_good_arguments_reach_the_client(monkeypatch, capsys, tmp_path):
    monkeypatch.setattr(cli, "ShowdownClient", StubClient)
    StubClient.made = []
    export = pikapro.sample_team("championsvgc")
    team = tmp_path / "team.txt"
    team.write_bytes(b"\xef\xbb\xbf" + export.encode("utf-8"))  # with a byte order mark, as some editors save
    packed = tmp_path / "packed.txt"
    packed.write_text(pikapro.pack(export, "championsvgc") + "\n", encoding="utf-8")
    assert cli.main(["ladder", "--format", FMT, "--team", str(team), "--log-level", "debug", "--policy", "os.path:join"]) == 0
    assert logging.getLogger("pikapro.showdown").level == logging.DEBUG
    [client] = StubClient.made
    [(_, games, fmt, pick)] = client.calls
    assert (games, fmt) == (1, FMT) and pick(FMT) == export
    import os.path

    assert client.kwargs["handler"].policy is os.path.join
    assert cli.main(["accept", "--team", str(packed), "--team", str(team), "--log-level", "30", "--handler", "random"]) == 0
    assert logging.getLogger("pikapro.showdown").level == logging.WARNING
    pick = StubClient.made[-1].calls[0][3]
    assert {pick("") for _ in range(40)} == {export, packed.read_text(encoding="utf-8")}
    assert '"summary"' in capsys.readouterr().out


@pytest.mark.parametrize("flag", ["--games", "--concurrent"])
@pytest.mark.parametrize("value", ["0", "-3", "two"])
def test_games_and_concurrent_are_at_least_one(monkeypatch, capsys, flag, value):
    """`--games 0` logged in, played nothing and exited 0; `--concurrent 0`
    connected and started a battle."""
    for command in (["ladder", "--format", FMT], ["challenge", "someone", "--format", FMT], ["accept"]):
        err = refused(monkeypatch, capsys, [*command, f"{flag}={value}"])
        assert flag in err and ("must be at least 1" in err or "is not a number" in err)


def test_one_line_that_is_no_packed_team_is_refused(monkeypatch, capsys, tmp_path):
    """One line is a packed team, uploaded as it is: it is checked too."""
    export = pikapro.sample_team("championsvgc")
    packed = pikapro.pack(export, "championsvgc")
    cases = {
        "word.txt": ("notateam\n", "one line that is not a packed team"),
        "short.txt": ("Pikachu|||Static|Thunderbolt\n", "one line that is not a packed team"),
        "unknown.txt": (packed.replace("Incineroar", "Notamon", 1) + "\n", "unknown species"),
        "second.txt": (packed + "]notateam\n", "one line that is not a packed team"),
    }
    for name, (text, why) in cases.items():
        path = tmp_path / name
        path.write_text(text, encoding="utf-8")
        for argv in (["ladder", "--format", FMT, "--team", str(path)], ["accept", "--team", str(path)]):
            err = refused(monkeypatch, capsys, argv)
            assert f"error: --team {path}:" in err and why in err, err
    # A packed team of a format pikapro doesn't play is the server's to judge.
    monkeypatch.setattr(cli, "ShowdownClient", StubClient)
    other = tmp_path / "gen1.txt"
    other.write_text("Missingno||||Pound|||||||\n", encoding="utf-8")
    assert cli.main(["ladder", "--format", "gen1ou", "--team", str(other), "--handler", "random"]) == 0
    capsys.readouterr()
