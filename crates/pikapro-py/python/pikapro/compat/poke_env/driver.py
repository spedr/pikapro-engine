"""Unmodified poke-env players playing :class:`pikapro.Battle`\\ s in-process.

The driver plays the part of Showdown's battle room: after each update it
sends each player the new lines of its channel, then its ``|request|``
(Showdown 0.11's order: ``battle.sendUpdates`` sends the log, then the
requests), and hands the choice each player's poke-env code makes back to
:meth:`pikapro.Battle.submit`. A rejected choice goes back to the player as
Showdown's ``|error|`` line, and poke-env answers it as it would on a
server.
"""

from __future__ import annotations

import random
import re
import time
from dataclasses import dataclass, field
from itertools import combinations
from typing import Any, Callable, Dict, List, Optional, Sequence, Union

import pikapro

from ._formats import is_doubles, pikapro_format, showdown_format
from .feeder import Feeder, FeederError, choice_text, require_poke_env

TeamsArg = Union[None, Sequence[str], Sequence[Sequence[str]], "pikapro.RandomTeams", Callable[[int, int], Sequence[str]]]


@dataclass
class BattleRecord:
    """One battle the driver played."""

    index: int
    seed: int
    teams: List[str]
    #: 0 or 1 (the side that won), None for a tie or an unfinished battle
    winner: Optional[int] = None
    turns: int = 0
    #: the engine's end reason (``AllFainted``, ``TurnLimit``, ``Forfeit``, ``EndlessBattle``)
    reason: Optional[str] = None
    #: choices rejected as ``[Invalid choice]``, per side (each went back to
    #: the player as Showdown's ``|error|`` line)
    invalid: List[int] = field(default_factory=lambda: [0, 0])
    #: choices rejected as ``[Unavailable choice]``: the choice ran into
    #: something the request hid (a trap from an unrevealed ability, a
    #: disabled move), which the player then learns from the updated
    #: request, as on Showdown
    unavailable: List[int] = field(default_factory=lambda: [0, 0])
    #: the rejection messages, per side
    invalid_messages: List[List[str]] = field(default_factory=lambda: [[], []])
    #: poke-env quirks worked around (docs/python/poke-env.md), by name
    quirks: Dict[str, int] = field(default_factory=dict)
    #: times the driver chose ``default`` for a side (a player that kept
    #: choosing rejected choices, or sent nothing)
    fallbacks: List[int] = field(default_factory=lambda: [0, 0])
    #: decision rounds where no player answered a request that needed one
    stalls: int = 0
    #: decisions asked of each side
    decisions: List[int] = field(default_factory=lambda: [0, 0])
    #: warnings poke-env logged while parsing, per side
    warnings: List[List[tuple]] = field(default_factory=lambda: [[], []])
    #: a feeder or player exception that ended the battle early
    error: Optional[str] = None
    error_line: Optional[str] = None


@dataclass
class PlayResults:
    """What :func:`play` returns."""

    format: str
    players: List[str]
    battles: List[BattleRecord]
    seconds: float

    @property
    def n(self) -> int:
        """The number of battles played."""
        return len(self.battles)

    @property
    def wins(self) -> List[int]:
        """Wins per side: ``[side 1's, side 2's]``."""
        return [sum(1 for b in self.battles if b.winner == s) for s in (0, 1)]

    @property
    def ties(self) -> int:
        """Battles that ended without a winner (and without an error)."""
        return sum(1 for b in self.battles if b.winner is None and b.error is None)

    @property
    def invalid(self) -> int:
        """``[Invalid choice]`` rejections, both sides, every battle."""
        return sum(sum(b.invalid) for b in self.battles)

    @property
    def unavailable(self) -> int:
        """``[Unavailable choice]`` rejections, both sides, every battle."""
        return sum(sum(b.unavailable) for b in self.battles)

    @property
    def quirks(self) -> Dict[str, int]:
        """poke-env quirks worked around, by name, totalled over the
        battles."""
        total: Dict[str, int] = {}
        for b in self.battles:
            for k, v in b.quirks.items():
                total[k] = total.get(k, 0) + v
        return total

    @property
    def fallbacks(self) -> int:
        """Times the driver chose ``default`` for a player, every battle."""
        return sum(sum(b.fallbacks) for b in self.battles)

    @property
    def stalls(self) -> int:
        """Decision rounds where no player answered a request that needed
        one."""
        return sum(b.stalls for b in self.battles)

    @property
    def errors(self) -> List[BattleRecord]:
        """The battles that a feeder or player exception ended early."""
        return [b for b in self.battles if b.error is not None]

    @property
    def turns(self) -> int:
        """Turns played, every battle."""
        return sum(b.turns for b in self.battles)

    @property
    def battles_per_second(self) -> float:
        """Battles per second of wall time."""
        return self.n / self.seconds if self.seconds > 0 else float("inf")

    def summary(self) -> Dict[str, Any]:
        """The totals as a plain dict (for printing or logging)."""
        return {
            "format": self.format,
            "players": self.players,
            "battles": self.n,
            "wins": self.wins,
            "ties": self.ties,
            "turns": self.turns,
            "invalid": self.invalid,
            "unavailable": self.unavailable,
            "quirks": self.quirks,
            "fallbacks": self.fallbacks,
            "stalls": self.stalls,
            "errors": len(self.errors),
            "seconds": round(self.seconds, 3),
            "battles_per_second": round(self.battles_per_second, 1),
        }


def make_player(cls, format: str, **kwargs):
    """Creates a poke-env player of class ``cls`` for a pikapro format, ready
    for the in-process driver: ``battle_format`` is the format's Showdown
    id, and it doesn't connect to a server (``start_listening=False``)."""
    require_poke_env()
    kwargs.setdefault("battle_format", showdown_format(format))
    kwargs.setdefault("start_listening", False)
    kwargs.setdefault("max_concurrent_battles", 0)
    return cls(**kwargs)


def play(
    p1,
    p2,
    format: str,
    n_battles: int = 1,
    *,
    teams: TeamsArg = None,
    seed: int = 0,
    team_mode: str = "sets",
    register: bool = True,
    max_retries: int = 10,
    raise_errors: bool = False,
    python_seed: Optional[int] = None,
    on_battle: Optional[Callable[[BattleRecord, "pikapro.Battle"], None]] = None,
) -> PlayResults:
    """Plays ``n_battles`` between two poke-env players, ``p1`` on side 1.

    Battle ``i`` uses pikapro seed ``seed + i`` and teams from ``teams``:

    * ``None``: each player's teambuilder (``player.next_team``) if it has
      one, else a random legal team, ``pikapro.RandomTeams(format,
      team_mode).pair(seed + i)``;
    * a pair of team texts (export or packed), or a list of pairs (cycled);
    * a :class:`pikapro.RandomTeams`: its ``pair(seed + i)``;
    * a callable ``(i, seed) -> pair``.

    ``register`` adds each battle to both players' ``battles`` (so their
    ``n_won_battles`` and ``win_rate`` count it). A rejected choice goes
    back to the player as Showdown's ``|error|`` line; after
    ``max_retries`` rejections in a row the driver chooses ``default``
    for it (a *fallback*). A poke-env exception ends that battle with an
    error in its record, or is raised with ``raise_errors``.
    ``python_seed`` seeds Python's ``random``, which poke-env's players
    draw from. Returns :class:`PlayResults`.
    """
    require_poke_env()
    fmt = pikapro_format(format)
    players = [p1, p2]
    want = is_doubles(fmt)
    for p in players:
        if bool(p.format_is_doubles) != want:
            raise ValueError(
                f"{p.username} plays {p.format!r}, which poke-env treats as "
                f"{'doubles' if p.format_is_doubles else 'singles'}; {fmt} needs "
                f"battle_format={showdown_format(fmt)!r} (see make_player)"
            )
    if python_seed is not None:
        random.seed(python_seed)
    team_source = _team_source(fmt, teams, players, team_mode)
    records = []
    start = time.perf_counter()
    for i in range(n_battles):
        s = seed + i
        pair = list(team_source(i, s))
        rec = BattleRecord(index=i, seed=s, teams=pair)
        battle = _play_one(players, fmt, rec, register=register, max_retries=max_retries, raise_errors=raise_errors)
        records.append(rec)
        if on_battle is not None:
            on_battle(rec, battle)
    seconds = time.perf_counter() - start
    return PlayResults(fmt, [p1.username, p2.username], records, seconds)


def play_parallel(
    p1,
    p2,
    format: str,
    n_battles: int,
    *,
    processes: int = 4,
    seed: int = 0,
    player_kwargs: Sequence[Optional[dict]] = (None, None),
    python_seed: Optional[int] = None,
    **kwargs,
) -> PlayResults:
    """:func:`play` split over ``processes`` worker processes (spawned, so
    poke-env's event-loop thread isn't forked). ``p1`` and ``p2`` are
    player *classes*; each worker creates its own pair with
    :func:`make_player` (``player_kwargs`` per side). Worker ``k`` plays a
    contiguous block of the battles, with the same seeds :func:`play`
    would use, and Python's ``random`` seeded with ``python_seed + k``
    when ``python_seed`` is given. ``teams`` must be picklable (``None``,
    team texts); other ``kwargs`` go to :func:`play`. The records come
    back in battle order; ``seconds`` is the wall time. As with any
    spawned ``multiprocessing`` pool, a script must call it under
    ``if __name__ == "__main__":``."""
    import multiprocessing

    processes = max(1, min(processes, n_battles))
    fmt = pikapro_format(format)
    base, extra = divmod(n_battles, processes)
    jobs = []
    start = 0
    for k in range(processes):
        count = base + (1 if k < extra else 0)
        ps = None if python_seed is None else python_seed + k
        jobs.append((p1, p2, fmt, start, count, seed, tuple(player_kwargs), ps, kwargs))
        start += count
    t0 = time.perf_counter()
    with multiprocessing.get_context("spawn").Pool(processes) as pool:
        parts = pool.map(_worker, jobs)
    seconds = time.perf_counter() - t0
    records = [r for part in parts for r in part[1]]
    return PlayResults(fmt, list(parts[0][0]), records, seconds)


def _worker(job):
    p1, p2, fmt, start, count, seed, player_kwargs, python_seed, kwargs = job
    a = make_player(p1, fmt, **(player_kwargs[0] or {}))
    b = make_player(p2, fmt, **(player_kwargs[1] or {}))
    kwargs = dict(kwargs)
    kwargs.setdefault("register", False)
    result = play(a, b, fmt, count, seed=seed + start, python_seed=python_seed, **kwargs)
    for r in result.battles:
        r.index += start
    return (result.players, result.battles)


def cross_evaluate(
    players: Sequence[Any],
    format: str,
    n_battles: int,
    *,
    seed: int = 0,
    **kwargs,
) -> Dict[str, Dict[str, Optional[float]]]:
    """poke-env's ``cross_evaluate``, in-process: every pair plays
    ``n_battles`` (the first of the pair on side 1, seeds from ``seed``),
    and ``results[a][b]`` is ``a``'s win rate against ``b`` (``None`` on
    the diagonal). Like poke-env, it resets the players' battles after
    each pair. ``kwargs`` go to :func:`play`. It keeps poke-env's
    signature for poke-env players; the built-in policies' table, played
    in Rust over mirrored games, is :func:`pikapro.rl.eval.cross_evaluate`."""
    results: Dict[str, Dict[str, Optional[float]]] = {p.username: {q.username: None for q in players} for p in players}
    for k, (a, b) in enumerate(combinations(players, 2)):
        play(a, b, format, n_battles, seed=seed + k * n_battles, register=True, **kwargs)
        results[a.username][b.username] = a.win_rate
        results[b.username][a.username] = b.win_rate
        a.reset_battles()
        b.reset_battles()
    return results


# -- one battle ---------------------------------------------------------------

_PERSPECTIVES = ("p1", "p2")


def _play_one(players, fmt, rec: BattleRecord, *, register, max_retries, raise_errors):
    # The players under their own names, as on a server (the sides' names
    # if they share one: poke-env finds its side by name).
    names = [getattr(p, "username", None) for p in players]
    if not all(names) or names[0] == names[1]:
        names = list(_PERSPECTIVES)
    b = pikapro.Battle(fmt, rec.teams, seed=rec.seed, players=names)
    doubles = b.space.active > 1
    feeders = [Feeder(fmt, s, player=players[s], register=register, username=names[s]) for s in (0, 1)]
    state = {"start": 0}

    def update(first=False):
        n = b.event_count
        for s in (0, 1):
            lines = b.log(_PERSPECTIVES[s], state["start"])
            if first:
                lines = b.header() + lines
            feeders[s].receive(lines)
        state["start"] = n

    def requests():
        for s in (0, 1):
            feeders[s].receive_request(b.request_json(s))

    try:
        update(first=True)
        requests()
        _open_team_sheets(feeders, players, b)
        while not b.over:
            resolved = False
            answered = [False, False]
            for s in (0, 1):
                f = feeders[s]
                retries = 0
                while f.pending is not None and not b.over:
                    rec.decisions[s] += 1
                    choice = choice_text(f.decide())
                    if choice is None:  # /forfeit
                        b.forfeit(s)
                        resolved = True
                        break
                    if choice == "":
                        # poke-env sends nothing: Showdown would wait (the timer)
                        rec.fallbacks[s] += 1
                        choice = "default"
                    else:
                        if doubles:
                            choice = _locked_move_targets(choice, f.battle.last_request, rec)
                            choice = _stale_move_targets(choice, f.battle.last_request, rec)
                        choice = _switch_names(choice, f.battle.last_request, b, s, rec)
                    try:
                        resolved = b.submit(s, choice)
                        answered[s] = True
                    except ValueError as e:
                        message = str(e)
                        if message.startswith("[Unavailable choice]"):
                            rec.unavailable[s] += 1
                        else:
                            rec.invalid[s] += 1
                        rec.invalid_messages[s].append(f"turn {b.turn}: {choice!r}: {message}")
                        retries += 1
                        if retries > max_retries:
                            rec.fallbacks[s] += 1
                            f.pending = None
                            resolved = b.submit(s, "default")
                            answered[s] = True
                            break
                        f.receive_error(message)
                        if message.startswith("[Unavailable choice]"):
                            # The request Showdown sends again, updated
                            # (`trapped`, the move `disabled`, `update: true`).
                            f.receive_request(b.request_json(s))
                        continue
                    break
                if resolved or b.over:
                    break
            if resolved or b.over:
                update()
                if not b.over:
                    requests()
                continue
            # Nobody could answer: a request poke-env thought was a wait.
            stuck = [s for s in (0, 1) if not answered[s] and b.request_kind(s) != "wait"]
            if not stuck:
                raise RuntimeError("the battle waits for no one")  # pragma: no cover
            rec.stalls += 1
            for s in stuck:
                rec.fallbacks[s] += 1
                if b.submit(s, "default"):
                    break
            update()
            if not b.over:
                requests()
    except Exception as e:  # a FeederError (poke-env's parsing) or the player's code
        if raise_errors:
            raise
        rec.error = f"{type(e).__name__}: {e}"
        rec.error_line = e.line if isinstance(e, FeederError) else None
    rec.turns = b.turn
    if b.over:
        rec.winner = b.winner if b.winner in (0, 1) else None
        rec.reason = str(b.result.reason) if b.result is not None else None
    rec.warnings = [list(f.warnings) for f in feeders]
    for f in feeders:
        for k, v in f.quirks.items():
            rec.quirks[k] = rec.quirks.get(k, 0) + v
    return b


def _open_team_sheets(feeders, players, b) -> None:
    """poke-env players in VGC formats answer the room's open-team-sheet
    offer, and one that accepts waits for ``|showteam|`` (or a refusal)
    before its team preview. As Showdown's room: when both accept, both
    sides' sheets (:meth:`pikapro.Battle.open_team_sheets`); otherwise the
    line ``<name> rejected open team sheets.`` for the one that refused."""
    accepting = [s for s in (0, 1) if feeders[s].accept_open_team_sheet and "vgc" in players[s].format]
    if not accepting or not feeders[0].battle.teampreview:
        return
    refusing = [s for s in (0, 1) if s not in accepting]
    if refusing:
        name = b.players[refusing[0]]
        lines = [f"{name} rejected open team sheets."]
    else:
        lines = b.open_team_sheets()
    for f in feeders:
        f.receive(lines)


_TARGET = re.compile(r"\s(?:-|\+)?[1-3]$")


def _locked_move_targets(choice: str, request: Optional[dict], rec: BattleRecord) -> str:
    """poke-env quirk ``locked-move-target``: for a Pokémon locked into a
    move (Outrage, Petal Dance, Uproar...), Showdown's request lists that
    move alone and without a ``target``. ``side.chooseMove`` then reads a
    choice by name as a ``normal``-target move and, in doubles, rejects
    ``move outrage`` without a target ("needs a target") before it
    reaches the locked-move case, while ``move 1`` passes (the target is
    ignored for a locked move either way). poke-env's players pick targets
    from their own move data (``randomNormal``: none), so they send
    ``move outrage`` and loop on the error. The driver sends ``move 1``
    for such a slot, as poke-env itself does for Recharge."""
    if not request or not request.get("active") or "move" not in choice:
        return choice
    parts = choice.split(",")
    changed = False
    for slot, part in enumerate(parts):
        text = part.strip()
        if slot >= len(request["active"]) or not text.startswith("move ") or _TARGET.search(text):
            continue
        moves = request["active"][slot].get("moves") or []
        if len(moves) == 1 and "target" not in moves[0] and text != "move 1":
            parts[slot] = " move 1" if slot else "move 1"
            changed = True
    if not changed:
        return choice
    rec.quirks["locked-move-target"] = rec.quirks.get("locked-move-target", 0) + 1
    return ",".join(parts)


#: Move targets that Showdown wants in doubles (``side.chooseMove``,
#: ``targetTypeChoices``) but that leave the slot one legal choice, as
#: ``target -> slot -> choice``: the user itself for ``adjacentAllyOrSelf``
#: (what a move that poke-env's data still gives ``self`` means), the ally
#: for ``adjacentAlly``.
_ONLY_TARGET: Dict[str, Callable[[int], str]] = {
    "adjacentAllyOrSelf": lambda slot: f"-{slot + 1}",
    "adjacentAlly": lambda slot: f"-{2 - slot}",
}


def _stale_move_targets(choice: str, request: Optional[dict], rec: BattleRecord) -> str:
    """poke-env quirk ``stale-move-target``: poke-env picks a move's target
    from its own move data, which can lag Showdown's. Champions gives Milk
    Drink ``adjacentAllyOrSelf`` (``data/mods/champions/moves.ts``) where
    poke-env 0.16.1's gen9 data still says ``self``, so its players send
    ``move milkdrink`` with no target, and Showdown's ``chooseMove``
    rejects that in doubles (``[Invalid choice] Can't move: Milk Drink
    needs a target``). The request carries each move's real ``target``:
    when a slot's move has none in the choice and that target leaves the
    slot one legal choice (:data:`_ONLY_TARGET`), the driver adds it."""
    if not request or not request.get("active") or "move" not in choice:
        return choice
    parts = choice.split(",")
    changed = False
    for slot, part in enumerate(parts):
        text = part.strip()
        # The target as Showdown's ``side.choose`` reads it: after the move
        # (``move 2`` is the second move, with no target).
        if slot >= len(request["active"]) or not text.startswith("move ") or _TARGET.search(text[5:].strip()):
            continue
        words = text.split()
        if len(words) < 2:
            continue
        moves = request["active"][slot].get("moves") or []
        if words[1].isdigit():
            k = int(words[1]) - 1
            move = moves[k] if 0 <= k < len(moves) else None
        else:
            move = next((m for m in moves if m.get("id") == words[1]), None)
        only = _ONLY_TARGET.get((move or {}).get("target", ""))
        if only is None:
            continue
        parts[slot] = (" " if slot else "") + f"{text} {only(slot)}"
        changed = True
    if not changed:
        return choice
    rec.quirks["stale-move-target"] = rec.quirks.get("stale-move-target", 0) + 1
    return ",".join(parts)


def _switch_names(choice: str, request: Optional[dict], b, side: int, rec: BattleRecord) -> str:
    """poke-env quirk ``switch-name-ambiguous``: poke-env switches by name
    (``switch Beedrill``), and Showdown's ``side.chooseSwitch`` takes the
    first Pokémon whose name *or current species* matches. An earlier
    Pokémon can match by species: a Ditto transformed into the foe's
    Beedrill, while the player means its own Beedrill. Showdown then
    reads the switch as one to the active Ditto and rejects it ("You
    can't switch to an active Pokémon"); poke-env chooses the same again.
    When Showdown's reading of the names differs from the Pokémon they
    name in the request, the driver sends the positions instead."""
    if "switch " not in choice or not request:
        return choice
    mons = request.get("side", {}).get("pokemon", [])
    parts = []
    named = False
    for part in choice.split(","):
        text = part.strip()
        if text.startswith("switch ") and not text[7:].strip().isdigit():
            name = text[7:].strip().lower()
            idx = [i for i, m in enumerate(mons) if m.get("ident", "")[4:].lower() == name]
            if len(idx) == 1:
                text = f"switch {idx[0] + 1}"
                named = True
        parts.append(text)
    if not named:
        return choice
    positional = ", ".join(parts)
    try:
        intended = str(b.parse_choice(side, positional))
    except ValueError:
        return choice
    try:
        read = str(b.parse_choice(side, choice))
    except ValueError:
        read = None
    if read == intended:
        return choice
    rec.quirks["switch-name-ambiguous"] = rec.quirks.get("switch-name-ambiguous", 0) + 1
    return positional


def _team_source(fmt: str, teams: TeamsArg, players, team_mode: str):
    if teams is None:
        builders = [getattr(p, "_team", None) for p in players]
        rt = None if all(builders) else pikapro.RandomTeams(fmt, team_mode)

        def source(i, s):
            pair = rt.pair(s) if rt is not None else [None, None]
            return [builders[k].yield_team() if builders[k] is not None else pair[k] for k in (0, 1)]

        return source
    if isinstance(teams, pikapro.RandomTeams):
        return lambda i, s: teams.pair(s)
    if callable(teams):
        return teams
    teams = list(teams)
    if len(teams) == 2 and all(isinstance(t, str) for t in teams):
        return lambda i, s: teams
    return lambda i, s: teams[i % len(teams)]
