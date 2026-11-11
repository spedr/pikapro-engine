"""Small, pure helpers for Showdown's server protocol (``PROTOCOL.md``).

No I/O here: frames are split into rooms and lines, the ``|formats|`` list
and the challenge ``|pm|``\\ s are parsed, and names are turned into ids as
Showdown's ``toID`` does. :mod:`pikapro.showdown.client` builds on them.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple

from .. import _pikapro
from .._pikapro import to_id  # Showdown's toID: the engine's own, pikapro.to_id

__all__ = [
    "Format",
    "parse_formats",
    "split_frame",
    "to_id",
    "battle_format",
    "format_key",
    "series_length",
    "series_room_format",
    "parse_challenge_pm",
    "request_rqid",
    "stale_choice_error",
    "pack_team",
    "is_command_safe",
]

_RQID = re.compile(r'"rqid"\s*:\s*(\d+)')


def split_frame(frame: str) -> Tuple[str, List[str]]:
    """A server frame as ``(room, lines)``. ``room`` is ``""`` for the
    global room (a frame that doesn't start with ``>ROOMID``); empty lines
    are dropped."""
    if frame.startswith(">"):
        head, _, rest = frame.partition("\n")
        room = head[1:].strip()
    else:
        room, rest = "", frame
    return room, [line for line in rest.split("\n") if line]


def _room_renamed(line: str) -> Optional[str]:
    """The room's new id, if ``line`` is Showdown's rename
    (``|noinit|rename|NEWID|NEWTITLE``, ``Room.rename``: sent under the old
    id when a player changes a battle's privacy; the room's frames then
    come under the new one), else ``None``. The id is the server's: check
    it before use."""
    if not line.startswith("|noinit|rename|"):
        return None
    return line.split("|", 4)[3]


def _room_closed(line: str) -> Optional[str]:
    """Why the room is gone, if ``line`` says it is: ``"room closed"`` for
    ``|deinit`` (the user left, or the room was destroyed) and ``|noinit``
    (it can't be joined), ``"room expired"`` for ``|expire|``
    (``Room.expire``, sent before the room is destroyed); else ``None``,
    as for a rename (:func:`_room_renamed`), which closes nothing."""
    if line == "|deinit" or line.startswith("|deinit|"):
        return "room closed"
    if line == "|noinit" or line.startswith("|noinit|"):
        return None if line.startswith("|noinit|rename|") else "room closed"
    if line == "|expire" or line.startswith("|expire|"):
        return "room expired"
    return None


def battle_format(room: str) -> str:
    """The format id of a battle room id (``battle-gen9randombattle-123``
    -> ``gen9randombattle``; a hidden room's password suffix is ignored)."""
    parts = room.split("-")
    return parts[1] if len(parts) >= 3 and parts[0] == "battle" else ""


def format_key(text: str) -> str:
    """A format as the client keys it: the format id, and a custom game's
    rules after ``@@@`` as given (trimmed, comma-separated), which
    ``/challenge`` takes as they are (``"gen9vgc2025regi@@@Best of = 3"``)."""
    base, sep, rules = text.partition("@@@")
    if not sep:
        return to_id(base)
    kept = ",".join(r.strip() for r in rules.split(",") if r.strip())
    return f"{to_id(base)}@@@{kept}" if kept else to_id(base)


def series_length(format: str) -> Optional[int]:
    """The games in a best-of series of ``format`` (Showdown's ``Best of =
    N`` rule), ``None`` for single games: a ``…bo3`` format
    (``gen9championsvgc2026regmcbo3``: Showdown names them "(Bo3)"), or a
    challenge's ``Best of = N`` custom rule."""
    base, _, rules = format.partition("@@@")
    for rule in rules.split(","):
        name, sep, value = rule.partition("=")
        if sep and to_id(name) == "bestof" and value.strip().isdigit():
            return int(value.strip())
    m = re.search(r"bo([3579])$", to_id(base))
    return int(m.group(1)) if m else None


def series_room_format(room: str) -> Tuple[str, Optional[int]]:
    """The format id and the length of a series room id
    (``game-bestof3-gen9championsvgc2026regmcbo3-12`` ->
    ``("gen9championsvgc2026regmcbo3", 3)``; Showdown's
    ``createBattle``); ``("", None)`` for another id."""
    parts = room.split("-")
    if len(parts) < 4 or parts[0] != "game" or not parts[1].startswith("bestof"):
        return "", None
    n = parts[1][len("bestof") :]
    return parts[2], (int(n) if n.isdigit() else None)


def request_rqid(line: str) -> Optional[int]:
    """The ``rqid`` of a ``|request|JSON`` line, if it has one."""
    m = _RQID.search(line)
    return int(m.group(1)) if m else None


# A choice error that says the choice came for no open request, not that
# the choice was wrong: Showdown's ``RoomBattle.choose`` (``There's nothing
# to choose``; ``Sorry, too late to make a different move; the next turn
# has already started`` for an outdated rqid), VGC Arena's ``There is
# nothing for you to choose right now`` (the request already has our
# choice, e.g. the open request sent again after a reconnect), and a
# crashed battle.
_STALE_CHOICE = re.compile(r"too late|nothing (?:for you )?to choose|battle crashed", re.IGNORECASE)


def stale_choice_error(message: str) -> bool:
    """Whether an ``|error|`` message rejects a choice as stale (sent for a
    request already answered or gone) rather than as a wrong choice: asking
    again won't help, and the next request is answered as usual."""
    return message.startswith("[Invalid choice]") and _STALE_CHOICE.search(message) is not None


@dataclass(frozen=True)
class Format:
    """One entry of a server's ``|formats|`` list."""

    #: The format id (``toID`` of the name), what ``/search`` and
    #: ``/challenge`` take.
    id: str
    #: The display name (``"[Gen 9 Champions] VGC 2026 Reg M-C"``).
    name: str
    #: The section the client lists it under.
    section: str
    #: The format generates teams (a random battle): ``/utm null``.
    random_teams: bool
    #: On the ladder (``/search``).
    ladder: bool
    #: Challengeable (``/challenge``).
    challenge: bool
    #: Offered for tournaments.
    tournament: bool
    #: Levels are 50 (``Adjust Level`` or ``Max Level`` 50).
    level_50: bool
    #: Challenges in it default to a best-of series (Showdown's
    #: ``bestOfDefault``); a plain ``/challenge`` is still one game. The
    #: series formats themselves are the ``…bo3`` ids.
    best_of: bool


def parse_formats(line: str) -> Dict[str, Format]:
    """The ``|formats|`` line, by format id.

    Showdown's ``formatListText`` (``server/rooms.ts``): sections are
    ``|,COLUMN|SECTION``, and each format is ``NAME,FLAGS`` where ``FLAGS``
    is a hexadecimal bit set (1 random teams, 2 ladder, 4 challenge, 8
    tournaments, 16 level 50, 64 best-of). The legacy suffixes (``,#``
    random teams, ``,,`` search only, ``,`` challenge only) are read too.
    """
    body = line[len("|formats|") :] if line.startswith("|formats|") else line
    parts = body.split("|")
    out: Dict[str, Format] = {}
    section = ""
    i = 0
    while i < len(parts):
        part = parts[i]
        if part.startswith(",") and part[1:].isdigit():
            section = parts[i + 1] if i + 1 < len(parts) else ""
            i += 2
            continue
        i += 1
        if part.startswith(","):  # a flag such as ",LL" (the ladder list prefix)
            continue
        if not part:
            continue
        name, sep, flags = part.rpartition(",")
        if sep and re.fullmatch(r"[0-9a-f]+", flags):
            code = int(flags, 16)
        else:  # legacy: NAME, NAME,# (random), NAME,, (search only), NAME, (challenge only)
            name = part.rstrip(",#")
            code = 4 | 2
            if part.endswith(",#"):
                code |= 1
            elif part.endswith(",,"):
                code = 2
            elif part.endswith(","):
                code = 4
        out[to_id(name)] = Format(
            id=to_id(name),
            name=name,
            section=section,
            random_teams=bool(code & 1),
            ladder=bool(code & 2),
            challenge=bool(code & 4),
            tournament=bool(code & 8),
            level_50=bool(code & 16),
            best_of=bool(code & 64),
        )
    return out


def parse_challenge_pm(line: str) -> Optional[Tuple[str, str, Optional[str]]]:
    """A challenge ``|pm|`` as ``(from_id, to_id, format)``.

    Showdown's ``BattleChallenges.update`` sends
    ``|pm|FROM|TO|/challenge FORMAT|...`` to both users while a challenge
    stands, and ``|pm|FROM|TO|/challenge`` (format ``None``) when it is
    gone (accepted, rejected or cancelled). Any other line: ``None``. The
    format is a :func:`format_key` (a custom game's rules kept).
    """
    if not line.startswith("|pm|"):
        return None
    parts = line.split("|", 4)
    if len(parts) < 5 or not parts[4].startswith("/challenge"):
        return None
    rest = parts[4][len("/challenge") :].strip()
    fmt = rest.split("|", 1)[0].strip() if rest else ""
    return to_id(parts[2]), to_id(parts[3]), (format_key(fmt) or None)


def pack_team(team: Optional[str], format_id: str = "") -> str:
    """A team as ``/utm`` wants it: packed, or ``"null"`` for none.

    An export-format team (it has a newline) is packed with :func:`pikapro.pack`;
    a packed team is kept as it is. ``format_id`` (a Showdown or pikapro format
    id) picks the dex: Champions formats pack with the Champions dex.
    """
    if team is None or not team.strip() or team.strip() == "null":
        return "null"
    team = team.strip()
    if "\n" not in team:
        if not is_command_safe(team):
            raise ValueError("a packed team can't hold control characters")
        return team  # already packed
    fmt = _pikapro._showdown_preset(format_id)
    if fmt is None:  # a format no session reads: its dex by name
        fmt = "championsvgc" if "champions" in format_id else "gen9singles"
    return _pikapro.pack(team, fmt)


def is_command_safe(text: str) -> bool:
    """Whether ``text`` can go in a command: no line breaks or other
    control characters (Showdown runs each line of a message as its own
    command, so ``"move 1\n/forfeit"`` would forfeit)."""
    return not any(ord(c) < 32 or ord(c) == 127 or c in "\u2028\u2029" for c in text)
