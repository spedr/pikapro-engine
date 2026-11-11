"""pikapro format ids and the Showdown format ids poke-env expects."""

from __future__ import annotations

from functools import lru_cache

import pikapro

# The ids pikapro-showdown-server offers for each preset (its `formats.rs`),
# which are also the ones poke-env players are created with to play them.
SHOWDOWN_FORMATS = {
    "championsvgc": "gen9championsvgc2026regmc",
    "championsbss": "gen9championsbssregmc",
    "gen9vgc": "gen9vgc2025regi",
    "gen9ou": "gen9ou",
    "gen9randombattle": "gen9randombattle",
    "gen9randomdoublesbattle": "gen9randomdoublesbattle",
    "gen9singles": "gen9customgame",
    "gen9doubles": "gen9doublescustomgame",
}

_BY_SHOWDOWN = {v: k for k, v in SHOWDOWN_FORMATS.items()}


def showdown_format(format: str) -> str:
    """The Showdown format id poke-env players use for a pikapro format
    (``"championsvgc"`` -> ``"gen9championsvgc2026regmc"``). A Showdown id
    is returned unchanged."""
    if format in SHOWDOWN_FORMATS:
        return SHOWDOWN_FORMATS[format]
    if format in _BY_SHOWDOWN:
        return format
    raise ValueError(f"unknown format {format!r}: expected one of {sorted(SHOWDOWN_FORMATS)}")


def pikapro_format(format: str) -> str:
    """The pikapro format for a pikapro or Showdown format id."""
    if format in SHOWDOWN_FORMATS:
        return format
    if format in _BY_SHOWDOWN:
        return _BY_SHOWDOWN[format]
    raise ValueError(f"unknown format {format!r}: expected one of {sorted(SHOWDOWN_FORMATS)} or {sorted(_BY_SHOWDOWN)}")


@lru_cache(maxsize=None)
def is_doubles(format: str) -> bool:
    """Whether a pikapro or Showdown format id is played in doubles: its
    action space has two active slots per side."""
    return pikapro.ActionSpace(pikapro_format(format)).active == 2
