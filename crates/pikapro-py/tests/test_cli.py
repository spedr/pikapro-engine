"""A battle played from Python matches the Rust CLI's (`pikapro play`) output
for the same seed and choices, line for line."""

import subprocess

import pytest

import pikapro
from conftest import teams


def python_play(format_id, seed, perspective, requests):
    """`pikapro play` in Python: the same battle, policy and printing (the
    choices and requests of the players the perspective may see)."""
    b = pikapro.Battle(format_id, teams(format_id), seed=seed)
    policy = pikapro.RandomPolicy(seed ^ 0x5EED)
    out = list(b.header())
    start = 0

    def sees(side):
        return perspective in ("omniscient", f"p{side + 1}")

    def flush():
        nonlocal start
        out.extend(b.log(perspective, start))
        start = b.event_count
        if requests and not b.over:
            for side in filter(sees, (0, 1)):
                out.append(f"|request|p{side + 1}|{b.request_json(side)}")

    flush()
    while not b.over:
        for side in (0, 1):
            choice = policy.choose(b, side)
            if choice is None:
                continue
            if sees(side):
                out.append(f">p{side + 1} {choice}")
            b.submit(side, choice)
        flush()
    return out


def cli_play(cli, format_id, seed, perspective, requests):
    args = [cli, "play", "--format", format_id, "--seed", str(seed), "--perspective", perspective]
    if requests:
        args.append("--requests")
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout.splitlines()


@pytest.mark.parametrize("seed", [1, 2, 3, 4, 5, 6])
def test_matches_cli(pikapro_cli, format_id, seed):
    assert python_play(format_id, seed, "omniscient", True) == cli_play(pikapro_cli, format_id, seed, "omniscient", True)


@pytest.mark.parametrize("perspective", ["p1", "p2", "spectator"])
def test_matches_cli_perspectives(pikapro_cli, perspective):
    for format_id in ("gen9vgc", "championsvgc"):
        assert python_play(format_id, 5, perspective, False) == cli_play(pikapro_cli, format_id, 5, perspective, False)


def test_replaying_cli_choice_strings(pikapro_cli, format_id):
    """Feeding the CLI's `>pN choice` lines back as strings reproduces its
    log: string choices are parsed as Showdown's `side.choose` does."""
    lines = cli_play(pikapro_cli, format_id, 9, "omniscient", False)
    b = pikapro.Battle(format_id, teams(format_id), seed=9)
    for line in lines:
        if line.startswith(">p"):
            side, choice = int(line[2]) - 1, line[4:]
            b.submit(side, choice)
    assert b.over
    assert b.header() + b.log() == [line for line in lines if not line.startswith(">")]
