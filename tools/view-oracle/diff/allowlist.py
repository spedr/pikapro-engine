"""Known, accepted differences between our projected view and poke-env's
tracked state (tools/view-oracle/SCHEMA.md). Each rule names a poke-env
tracking choice or a representation difference, never a protocol fact.

A rule is ``(name, predicate)``; ``predicate(d)`` gets a ``Diff`` and
returns True when the difference is that quirk. Rules must be as narrow as
possible: a rule that hides a real divergence hides a view bug.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Callable, Dict, FrozenSet, List, Optional, Set, Tuple

UNKNOWN = "?"
ABSENT = "<absent>"


@dataclass
class Diff:
    """One differing leaf. ``path`` is a tuple of keys (``("team", "p1:
    Pikachu", "hp")``); ``ours``/``theirs`` the values (``ABSENT`` if the
    key is missing); ``our_state``/``their_state`` the whole states."""

    path: Tuple[str, ...]
    ours: Any
    theirs: Any
    our_state: dict
    their_state: dict
    record: dict
    #: The side's channel lines up to this record (empty without
    #: ``--decisions``).
    history: List[str] = field(default_factory=list)
    #: The keys of the Pokémon an Illusion has mixed up for poke-env so
    #: far in this side's battle (``Illusions``).
    illusions: FrozenSet[str] = frozenset()

    def last_line(self, *kinds: str) -> int:
        """The index of the last line of one of ``kinds`` (``"switch"``,
        ``"-formechange"``, ...) naming the Pokémon the path is under,
        or -1."""
        if len(self.path) < 2:
            return -1
        side, _, name = self.path[1].partition(": ")
        idents = {f"{side}: {name}", f"{side}a: {name}", f"{side}b: {name}"}
        for i in range(len(self.history) - 1, -1, -1):
            parts = self.history[i].split("|")
            if len(parts) > 2 and parts[1] in kinds and parts[2] in idents:
                return i
        return -1

    def mon(self, which: str) -> Optional[dict]:
        """The Pokémon the path is under, in ``ours`` or ``theirs``."""
        if len(self.path) < 2 or self.path[0] not in ("team", "opponent_team"):
            return None
        state = self.our_state if which == "ours" else self.their_state
        return state.get(self.path[0], {}).get(self.path[1])


def _field(d: Diff) -> str:
    return d.path[-1]


def _own(d: Diff) -> bool:
    return d.path[0] == "team"


def _foe(d: Diff) -> bool:
    return d.path[0] == "opponent_team"


def own_tera_type(d: Diff) -> bool:
    # poke-env never reads a request's `teraType`: its own side's Tera type
    # stays unknown until `-terastallize`.
    return _own(d) and _field(d) == "tera_type" and d.theirs == UNKNOWN


def weather_since(d: Diff) -> bool:
    # poke-env stores the turn of the last `-weather` line (every
    # `[upkeep]` rewrites it); the view counts upkeeps since it started
    # and doesn't project a turn.
    return d.path == ("weather", "since") and d.ours == UNKNOWN


def start_turns(d: Diff) -> bool:
    # A terrain's, pseudo-weather's or side condition's start turn: the
    # view keeps turns elapsed (never remaining), not the absolute turn.
    return (
        len(d.path) == 2
        and d.path[0] in ("fields", "side_conditions", "opponent_side_conditions")
        and d.ours == UNKNOWN
        and isinstance(d.theirs, int)
    )


def effects(d: Diff) -> bool:
    # `effects` is informational in the schema; the projection doesn't map
    # volatiles yet.
    return "effects" in d.path


def fainted_own_active(d: Diff) -> bool:
    # poke-env keeps a fainted own Pokémon `active` until the request that
    # replaces it.
    mon = d.mon("theirs")
    return _own(d) and _field(d) == "active" and d.theirs is True and bool(mon and mon.get("fainted"))


def regenerator_guess(d: Diff) -> bool:
    # poke-env's `Pokemon.switch_out` heals a Pokémon it believes has
    # Regenerator by a third when it leaves the field; the protocol shows
    # no such heal for the foe (the view mustn't learn it), or shows it
    # (Champions: `-heal|...|[from] ability: Regenerator|[silent]`), which
    # poke-env then applies on top of its guess.
    mon = d.mon("theirs")
    if not (
        _foe(d)
        and _field(d) == "hp"
        and mon is not None
        and not mon.get("active")
        and isinstance(d.ours, int)
        and isinstance(d.theirs, int)
        and d.theirs > d.ours
    ):
        return False
    # (Or a line showed it has Regenerator: the heal, or Trace copying it.)
    shown = any(_names_mon(d, line) and "Regenerator" in line for line in d.history)
    # (A Pokémon that transformed into one with Regenerator: poke-env
    # copied the ability.)
    transformed = any(line.startswith("|-transform|") and _names_mon(d, line) for line in d.history)
    return mon.get("ability") == "regenerator" or shown or transformed


def species_lags(d: Diff) -> bool:
    # poke-env's `species` is the last `details` it read; a Mega Evolution
    # or `detailschange` seen in the log doesn't change it, while the view's
    # details species follows them. The schema compares on `forme`.
    ours, theirs = d.mon("ours"), d.mon("theirs")
    return (
        _field(d) == "species"
        and ours is not None
        and theirs is not None
        and ours.get("forme") == theirs.get("forme")
    )


# Formes that differ only in looks (same base species, stats, types and
# abilities): poke-env's Pokédex entries of such formes share one
# `baseStats` object, so `snapshot`'s forme lookup (by that object) can
# name any of them. Generated by `node tools/view-oracle/diff/cosmetic.mjs`
# from the pinned Showdown; it leaves out formes a battle changes into
# (Minior-Meteor, Cherrim-Sunshine), Megas (Floette-Mega) and formes with
# other stats (Floette-Eternal), which the view must follow.
COSMETIC_GROUPS: Tuple[FrozenSet[str], ...] = (
    frozenset({"alcremie", "alcremiecaramelswirl", "alcremielemoncream", "alcremiematchacream", "alcremiemintcream", "alcremierainbowswirl", "alcremierubycream", "alcremierubyswirl", "alcremiesaltedcream"}),
    frozenset({"burmy", "burmysandy", "burmytrash"}),
    frozenset({"deerling", "deerlingautumn", "deerlingsummer", "deerlingwinter"}),
    frozenset({"dudunsparce", "dudunsparcethreesegment"}),
    frozenset({"flabebe", "flabebeblue", "flabebeorange", "flabebewhite", "flabebeyellow"}),
    frozenset({"floette", "floetteblue", "floetteorange", "floettewhite", "floetteyellow"}),
    frozenset({"florges", "florgesblue", "florgesorange", "florgeswhite", "florgesyellow"}),
    frozenset({"furfrou", "furfroudandy", "furfroudebutante", "furfroudiamond", "furfrouheart", "furfroukabuki", "furfroulareine", "furfroumatron", "furfroupharaoh", "furfroustar"}),
    frozenset({"gastrodon", "gastrodoneast"}),
    frozenset({"genesect", "genesectburn", "genesectchill", "genesectdouse", "genesectshock"}),
    frozenset({"magearna", "magearnaoriginal"}),
    frozenset({"maushold", "mausholdfour"}),
    frozenset({"minior", "miniorblue", "miniorgreen", "miniorindigo", "miniororange", "miniorviolet", "minioryellow"}),
    frozenset({"pikachu", "pikachualola", "pikachuhoenn", "pikachukalos", "pikachuoriginal", "pikachupartner", "pikachusinnoh", "pikachuunova", "pikachuworld"}),
    frozenset({"pikachubelle", "pikachucosplay", "pikachulibre", "pikachuphd", "pikachupopstar", "pikachurockstar"}),
    frozenset({"poltchageist", "poltchageistartisan"}),
    frozenset({"polteageist", "polteageistantique"}),
    frozenset({"sawsbuck", "sawsbuckautumn", "sawsbucksummer", "sawsbuckwinter"}),
    frozenset({"shellos", "shelloseast"}),
    frozenset({"sinistcha", "sinistchamasterpiece"}),
    frozenset({"sinistea", "sinisteaantique"}),
    frozenset({"squawkabilly", "squawkabillyblue"}),
    frozenset({"squawkabillywhite", "squawkabillyyellow"}),
    frozenset({"tatsugiri", "tatsugiridroopy", "tatsugiristretchy"}),
    frozenset({"unown", "unownb", "unownc", "unownd", "unowne", "unownexclamation", "unownf", "unowng", "unownh", "unowni", "unownj", "unownk", "unownl", "unownm", "unownn", "unowno", "unownp", "unownq", "unownquestion", "unownr", "unowns", "unownt", "unownu", "unownv", "unownw", "unownx", "unowny", "unownz"}),
    frozenset({"vivillon", "vivillonarchipelago", "vivilloncontinental", "vivillonelegant", "vivillonfancy", "vivillongarden", "vivillonhighplains", "vivillonicysnow", "vivillonjungle", "vivillonmarine", "vivillonmodern", "vivillonmonsoon", "vivillonocean", "vivillonpokeball", "vivillonpolar", "vivillonriver", "vivillonsandstorm", "vivillonsavanna", "vivillonsun", "vivillontundra"}),
    frozenset({"zarude", "zarudedada"}),
)


def cosmetic_forme(d: Diff) -> bool:
    if _field(d) not in ("forme", "species") or not isinstance(d.ours, str) or not isinstance(d.theirs, str):
        return False
    return any(d.ours in g and d.theirs in g for g in COSMETIC_GROUPS)


def final_own(d: Diff) -> bool:
    # The final record has no request after its lines. The view's facts
    # about its own side (exact HP, status, ability, forme) come from
    # requests, so a change in the last lines (a drain heal, a paralysis,
    # a Mega Evolution) isn't in them; poke-env reads the player channel's
    # exact HP and the Mega's Pokédex ability. (pikapro-view could take its
    # own exact HP from the events.)
    return (
        _own(d)
        and bool(d.our_state.get("finished"))
        and _field(d) in ("hp", "status", "ability", "forme", "types", "item", "mega", "fainted", "terastallized")
    )


def own_illusion_revealed(d: Diff) -> bool:
    # A player's own Illusion user switched in disguised: poke-env matches
    # the switch to the request's active Pokémon and marks it revealed; the
    # public record (what the foe saw) never named it.
    mon = d.mon("ours")
    return _own(d) and _field(d) == "revealed" and mon is not None and mon.get("ability") == "illusion"


class Illusions:
    """The Pokémon whose records an Illusion has mixed up for poke-env, fed
    one side's records in order (``feed``): an Illusion user and each
    Pokémon it showed itself as. Each break names both: ``|replace|`` names
    the Illusion user in the slot whose last ``|switch|`` or ``|drag|``
    named the disguise. A player's own disguise also shows before it
    breaks, or if it never does: its request has the Illusion user active
    in a slot whose last switch-in named another Pokémon. A disguise as
    the Pokémon in the other slot shows as one name in two slots (one
    record in both trackers), even if it never breaks."""

    def __init__(self, side: str) -> None:
        self.side = side
        #: Slot (``p1a``) to the key (``p1: Name``) its last switch-in named.
        self.slots: Dict[str, str] = {}
        self.keys: Set[str] = set()

    @staticmethod
    def _key(ident: str) -> Tuple[str, str]:
        slot, _, name = ident.partition(": ")
        return slot, f"{slot[:2]}: {name}"

    def feed(self, lines: List[str], our_state: dict) -> FrozenSet[str]:
        for line in lines:
            parts = line.split("|")
            if len(parts) < 3 or parts[1] not in ("switch", "drag", "replace", "swap"):
                continue
            slot, key = self._key(parts[2])
            if parts[1] == "replace" and slot in self.slots:
                self.keys.update((key, self.slots[slot]))
            if parts[1] == "swap":
                # `|swap|<mon>|<position>`: Ally Switch trades two slots.
                if len(parts) > 3 and parts[3].isdigit():
                    other = f"{slot[:2]}{'abc'[int(parts[3])]}"
                    a, b = self.slots.get(slot), self.slots.get(other)
                    for s, k in ((other, a), (slot, b)):
                        if k is None:
                            self.slots.pop(s, None)
                        else:
                            self.slots[s] = k
                continue
            if any(s != slot and s[:2] == slot[:2] and k == key for s, k in self.slots.items()):
                # Two slots under one name: one is an Illusion user showing
                # the Pokémon beside it, and both trackers have one record.
                self.keys.add(key)
            self.slots[slot] = key
        team = our_state.get("team", {})
        for i, key in enumerate(our_state.get("active") or []):
            shown = self.slots.get(f"{self.side}{'abc'[i]}")
            if key and shown and shown != key and team.get(key, {}).get("ability") == "illusion":
                self.keys.update((key, shown))
        return frozenset(self.keys)


def illusion_records(d: Diff) -> bool:
    # poke-env's Illusion tracking. A disguised Illusion user's lines go to
    # the record of the Pokémon it shows (moves, item, HP, stages, and
    # `switch_in`'s `revealed`). At a break, `_end_illusion_on` moves only
    # HP and status to the Illusion user, and `was_illusioned` resets the
    # disguise's record (HP 0/0, no status, `switch_out`: not active,
    # stages cleared), even when the disguise is the active ally or a
    # fainted Pokémon in the other slot. A player's own Illusion user is set
    # active from the request (`_update_team_from_request`), at stage 0. A
    # disguise as the Pokémon beside it shares that one's record (both
    # trackers key records by name). Allowed only on the records of an
    # Illusion user and the Pokémon it showed itself as (``Illusions``),
    # and on an active list that differs only in them; the soundness check
    # holds the view to the state there.
    if len(d.path) > 1 and d.path[0] in ("team", "opponent_team"):
        return d.path[1] in d.illusions
    if d.path in (("active",), ("opponent_active",)):
        if not (isinstance(d.ours, list) and isinstance(d.theirs, list) and len(d.ours) == len(d.theirs)):
            return False
        mixed = d.illusions | {None}
        return all(a == b or (a in mixed and b in mixed) for a, b in zip(d.ours, d.theirs))
    return False


def stale_forme(d: Diff) -> bool:
    # poke-env keeps a temporary forme (Morpeko-Hangry, Aegislash-Blade)
    # after the Pokémon leaves the field, even through its next switch-in
    # (`_update_from_details` keeps the Pokédex entry when the species name
    # is the same), until a new forme change; the details show the base
    # forme.
    ours, theirs = d.mon("ours"), d.mon("theirs")
    # (Its types follow the stale forme too: Castform-Snowy's Ice.)
    if _field(d) not in ("forme", "types") or ours is None or theirs is None:
        return False
    if not (ours.get("forme") == theirs.get("species") != theirs.get("forme")):
        return False
    if not ours.get("active"):
        return True
    return d.last_line("switch", "drag") > d.last_line("-formechange", "detailschange", "-mega")


def own_struggle(d: Diff) -> bool:
    # A set that knows Struggle: the request lists it, and poke-env keeps
    # Struggle out of a Pokémon's moves (`SPECIAL_MOVES`).
    if not (_own(d) and _field(d) == "moves" and isinstance(d.ours, list) and isinstance(d.theirs, list)):
        return False
    return "struggle" in d.ours and sorted(m for m in d.ours if m != "struggle") == sorted(d.theirs)


def own_temporary_forme(d: Diff) -> bool:
    # A temporary forme (`-formechange`: Minior-Meteor by Shields Down)
    # leaves `details` alone, and poke-env's own side takes its stats back
    # from each request's details (its forme is then the details species,
    # or a cosmetic forme of it); the view keeps the forme the channel
    # showed last.
    theirs = d.mon("theirs")
    if not (_own(d) and _field(d) == "forme" and isinstance(d.ours, str) and theirs is not None):
        return False
    species = theirs.get("species")
    if not (d.theirs == species or any(d.theirs in g and species in g for g in COSMETIC_GROUPS)):
        return False
    changes = [
        l for l in d.history if l.startswith("|-formechange|") and _names_mon(d, l.split("|")[2])
    ]
    if not changes or d.last_line("-formechange") < d.last_line("switch", "drag"):
        return False
    return _norm(changes[-1].split("|")[3]) == d.ours


def own_transformed_moves(d: Diff) -> bool:
    # An own Pokémon transformed before its first request (Imposter at the
    # lead): every request shows the moves it copied, so the view never saw
    # its own; poke-env knows a transformed Pokémon's moves aren't its own
    # (`_add_move` files them apart) and keeps what it had. Only for a
    # Pokémon a `-transform` line names as the one transforming, and only
    # while the view has moves (an empty list is a lost fact).
    if not (_own(d) and _field(d) == "moves" and isinstance(d.ours, list) and isinstance(d.theirs, list)):
        return False
    if not d.ours or set(d.ours) & set(d.theirs):
        return False
    return any(line.startswith("|-transform|") and _names_mon(d, line.split("|")[2]) for line in d.history)


def fainted_own_max_hp(d: Diff) -> bool:
    # A fainted Pokémon's request `condition` is `0 fnt`, without its max
    # HP, so poke-env keeps the last one it read; the view's overlay has
    # the engine's (a forme regression on fainting changes it: Terapagos).
    mon = d.mon("ours")
    return _own(d) and _field(d) == "max_hp" and mon is not None and bool(mon.get("fainted"))


def mega_stone(d: Diff) -> bool:
    # `-mega|<mon>|<species>|<stone>` names the Mega Stone; poke-env's
    # `_mega_evolve` doesn't record it as the item.
    mon = d.mon("ours")
    # (Formes like Meowstic-M-Mega don't start with "Mega", hence the ID.)
    mega = mon is not None and (bool(mon.get("mega")) or "mega" in str(mon.get("forme", "")))
    return _foe(d) and _field(d) == "item" and d.theirs == UNKNOWN and mega


# Megas whose ability Showdown (at the oracle's pin) has since changed:
# poke-env 0.16.1's Pokédex (`data/static/pokedex/gen9pokedex.json`)
# still has the old one, which `_update_from_pokedex` takes as the Mega's
# ability (`forme_change_ability`), on its own side too. Forme, then
# (Showdown's ability, poke-env's).
STALE_MEGA_ABILITIES = {
    "absolmegaz": ("sharpness", "magicbounce"),
    "garchompmegaz": ("levitate", "sandforce"),
    "golisopodmega": ("toughclaws", "emergencyexit"),
    "lucariomegaz": ("auraguard", "adaptability"),
}


def stale_mega_ability(d: Diff) -> bool:
    mon = d.mon("ours")
    return (
        _field(d) == "ability"
        and mon is not None
        and STALE_MEGA_ABILITIES.get(str(mon.get("forme"))) == (d.ours, d.theirs)
    )


def mega_z(d: Diff) -> bool:
    # `-mega|<mon>|<species>|<stone>` after a Mega-Z's `detailschange`:
    # poke-env's `Pokemon.mega_evolve` takes `<species>mega` whenever the
    # Pokédex has it, so Absol-Mega-Z, Garchomp-Mega-Z and Lucario-Mega-Z
    # become the plain Mega (its forme and types; `species` lags). Its own
    # side's next request details set the forme back; the foe's stays until
    # it switches in again. Only for a Mega-Z the view has and the plain
    # Mega of the same species in poke-env.
    ours, theirs = d.mon("ours"), d.mon("theirs")
    if _field(d) not in ("species", "forme", "types") or ours is None or theirs is None:
        return False
    forme = str(ours.get("forme"))
    return forme.endswith("megaz") and theirs.get("forme") == forme[:-1]


def cud_chew(d: Diff) -> bool:
    # Cud Chew eats last turn's berry again (`-activate|<mon>|ability: Cud
    # Chew`, `-enditem|<mon>|<berry>|[eat]`): poke-env takes the line as
    # losing the held item, which a Trick may have replaced.
    if _field(d) != "item" or d.theirs is not None:
        return False
    lines = [l for l in d.history if _names_mon(d, l) and (l.startswith("|-item|") or l.startswith("|-enditem|"))]
    if not lines or not lines[-1].startswith("|-enditem|"):
        return False
    i = max(i for i, l in enumerate(d.history) if l == lines[-1])
    return i > 0 and "|ability: Cud Chew" in d.history[i - 1] and _names_mon(d, d.history[i - 1])


def revived_tera(d: Diff) -> bool:
    # Showdown deletes `terastallized` when a Pokémon faints, so one
    # Revival Blessing brought back isn't Terastallized (its request says
    # so); poke-env keeps the flag, and the Tera type as its types.
    if _field(d) not in ("terastallized", "types"):
        return False
    if _field(d) == "terastallized" and not (d.ours is False and d.theirs is True):
        return False
    side, _, name = d.path[1].partition(": ")
    head = f"|-heal|{side}: {name}|"
    return any(line.startswith(head) and "Revival Blessing" in line for line in d.history)


def _norm(text: str) -> str:
    return "".join(c for c in text.lower() if c.isalnum())


def _names_mon(d: Diff, line: str) -> bool:
    side, _, name = d.path[1].partition(": ")
    return any(f"{side}{x}: {name}" in line for x in ("", "a", "b"))


def shown_in_channel(d: Diff) -> bool:
    # poke-env reads a foe's ability or item from a few lines only
    # (`-ability`, `-item`, `-enditem`, some `[from]`s); the view also
    # takes it from any line whose effect names it (`[from] ability: X` on
    # `-weather`, `-start`, `-heal`, `-status`, `-fail`, `cant`, `-block`;
    # `-activate|...|ability: X`; Poltergeist's `-activate|<target>|move:
    # Poltergeist|<item>`). Allowed only when such a line in the channel
    # names the Pokémon and the value: a fact the channel doesn't show
    # would be a leak.
    if not (_foe(d) and _field(d) in ("ability", "item") and d.theirs == UNKNOWN and isinstance(d.ours, str)):
        return False
    value = _norm(d.ours)
    return any(_names_mon(d, line) and value in _norm(line) for line in d.history)


def imposter_move(d: Diff) -> bool:
    # poke-env adds Transform to the moves of a Pokémon that transformed
    # through Imposter (`-transform|...|[from] ability: Imposter`), which
    # never used the move.
    if _field(d) != "moves" or not isinstance(d.theirs, list) or "transform" not in d.theirs:
        return False
    if not isinstance(d.ours, list) or "transform" in d.ours:
        return False
    rest = [m for m in d.theirs if m != "transform"]
    imposter = any(
        line.startswith("|-transform|") and _names_mon(d, line) and "Imposter" in line for line in d.history
    )
    return imposter and set(rest) <= set(d.ours)


def fainted_keeps(d: Diff) -> bool:
    # A fainted Pokémon keeps, in poke-env, the stages, types and forme it
    # fainted with until it is replaced; Showdown clears them
    # (`clearVolatile`, forme regression) and so does the view.
    mon = d.mon("theirs")
    if mon is None or not mon.get("fainted"):
        return False
    return "boosts" in d.path or _field(d) in ("types", "forme", "terastallized")


def cant_move(d: Diff) -> bool:
    # `cant|<mon>|move: Taunt|<move>` (Taunt, Disable, Imprison, Gravity,
    # Heal Block, Throat Chop) names the move it chose: its own. poke-env
    # doesn't add it.
    if not (_foe(d) and _field(d) == "moves" and isinstance(d.ours, list) and isinstance(d.theirs, list)):
        return False
    extra = [m for m in d.ours if m not in d.theirs]
    if not extra or any(m not in d.ours for m in d.theirs):
        return False
    shown = [line for line in d.history if line.startswith("|cant|") and _names_mon(d, line)]
    return all(any(m == _norm(line.split("|")[-1]) for line in shown) for m in extra)


def learned_order(d: Diff) -> bool:
    # The same moves, learned in another order: the view also learns a
    # move from lines that name it before it is used (`cant`, Disable,
    # Encore, Spite, ...).
    return (
        _field(d) == "moves"
        and isinstance(d.ours, list)
        and isinstance(d.theirs, list)
        and sorted(d.ours) == sorted(d.theirs)
    )


def transform_ability_kept(d: Diff) -> bool:
    # poke-env keeps the ability a Pokémon copied with Transform after it
    # leaves the field; Showdown restores its own on switching out.
    mon = d.mon("ours")
    return (
        _foe(d)
        and _field(d) == "ability"
        and d.ours == UNKNOWN
        and mon is not None
        and not mon.get("active")
        and any(line.startswith("|-transform|") and _names_mon(d, line) for line in d.history)
    )


def transform_keeps_own(d: Diff) -> bool:
    # A transformed Pokémon has its target's ability (unknown when the
    # target's is); poke-env keeps its own (the one its species implies).
    mon = d.mon("ours")
    if not (_foe(d) and _field(d) == "ability" and mon is not None and mon.get("active")):
        return False
    own = [
        i
        for i, l in enumerate(d.history)
        if _names_mon(d, l) and (l.startswith("|switch|") or l.startswith("|drag|") or l.startswith("|-transform|"))
    ]
    if not own:
        return False
    last = d.history[own[-1]].split("|")
    return last[1] == "-transform" and _names_mon(d, last[2])


def stellar_types(d: Diff) -> bool:
    # A Stellar Terastallization keeps the Pokémon's own types (Showdown's
    # `getTypes`); poke-env gives it the type Stellar.
    return _field(d) == "types" and d.theirs == ["stellar"]


def single_turn_guard(d: Diff) -> bool:
    # Wide Guard and Quick Guard are side conditions for the turn
    # (`-sidestart`); poke-env doesn't keep them.
    return (
        len(d.path) == 2
        and d.path[0] in ("side_conditions", "opponent_side_conditions")
        and d.path[1] in ("wideguard", "quickguard")
        and d.theirs == ABSENT
    )


def baton_pass(d: Diff) -> bool:
    # Baton Pass passes stat stages to the Pokémon it brings in (`|switch|
    # ...|[from] Baton Pass`); poke-env starts it at 0.
    if "boosts" not in d.path:
        return False
    switches = [line for line in d.history if line.startswith(("|switch|", "|drag|")) and _names_mon(d, line)]
    return bool(switches) and switches[-1].endswith("[from] Baton Pass")


def copied_ability(d: Diff) -> bool:
    # Trace and Imposter's Transform give the Pokémon another ability; the
    # view keeps the one it has now (the schema's effective ability),
    # poke-env the Trace or Imposter it showed.
    if not (_field(d) == "ability" and d.theirs in ("trace", "imposter") and isinstance(d.ours, str)):
        return False
    shown = "Trace" if d.theirs == "trace" else "Imposter"
    return any(_names_mon(d, line) and f"ability: {shown}" in line for line in d.history)


def _skill_swaps(d: Diff, allies: bool) -> List[int]:
    """The indices of the Skill Swaps (`-activate` lines) naming ``d``'s
    Pokémon: between allies only (no abilities shown), or all."""
    mark = "|Skill Swap|||" if allies else "|Skill Swap|"
    return [i for i, l in enumerate(d.history) if l.startswith("|-activate|") and mark in l and _names_mon(d, l)]


def ally_skill_swap(d: Diff) -> bool:
    # Skill Swap between allies shows no abilities (`-activate|<source>|
    # Skill Swap|||[of] <ally>`); poke-env ignores the swap and keeps the
    # abilities it had, the view follows them (and knows what it can't).
    # Only for a Pokémon of such a swap that hasn't left the field since:
    # switching out restores its own ability (`skill_swap_kept`). (What
    # the view then claims is checked by the soundness and leak checks.)
    if not (_foe(d) and _field(d) == "ability"):
        return False
    swaps = _skill_swaps(d, allies=True)
    return bool(swaps) and d.last_line("switch", "drag") < swaps[-1]


def skill_swap_kept(d: Diff) -> bool:
    # poke-env keeps an ability a Pokémon got through Skill Swap after it
    # leaves the field or faints; Showdown restores its own then, which the
    # view follows (unknown, or its own if it knows it). Only for a Pokémon
    # that has left the field or fainted since its last Skill Swap, and
    # only when poke-env's ability is one a line naming it showed since
    # its first.
    if not (_foe(d) and _field(d) == "ability" and isinstance(d.theirs, str) and d.theirs != UNKNOWN):
        return False
    swaps = _skill_swaps(d, allies=False)
    if not swaps:
        return False
    mon = d.mon("ours") or {}
    left = not mon.get("active") or mon.get("fainted") or d.last_line("switch", "drag") > swaps[-1]
    theirs = _norm(d.theirs)
    return bool(left) and any(theirs in _norm(l) for l in d.history[swaps[0] :] if _names_mon(d, l))


def transform_into_mega(d: Diff) -> bool:
    # A Pokémon transformed into a Mega Evolved one gets the Mega's types
    # and ability; poke-env takes the target's `species`, which lags the
    # Mega Evolution.
    if _field(d) not in ("types", "ability"):
        return False
    transforms = [line for line in d.history if line.startswith("|-transform|") and _names_mon(d, line)]
    if not transforms:
        return False
    target = transforms[-1].split("|")[3]
    # (Also a target whose types changed: Transform copies its current
    # types, poke-env its species'.)
    changed = f"|-start|{target}|typechange|"
    return any(line.startswith((f"|-mega|{target}|", changed)) for line in d.history)


def wandering_spirit(d: Diff) -> bool:
    # Wandering Spirit swaps abilities with an attacker that makes contact
    # (`-activate|<mon>|ability: Wandering Spirit|...`); poke-env keeps
    # Wandering Spirit, the view follows the swap (its ability is then
    # shown by its effects).
    return (
        _field(d) == "ability"
        and (d.theirs == "wanderingspirit" or d.ours == "wanderingspirit")
        and any("Wandering Spirit" in line and _names_mon(d, line) for line in d.history)
    )


def mega_resets_types(d: Diff) -> bool:
    # A Mega Evolution ends a type change (`setSpecies` sets the forme's
    # types); poke-env keeps the changed types.
    if _field(d) != "types":
        return False
    last_mega = max((i for i, l in enumerate(d.history) if l.startswith("|-mega|") and _names_mon(d, l)), default=-1)
    last_change = max(
        (i for i, l in enumerate(d.history) if "|typechange|" in l and _names_mon(d, l)),
        default=-1,
    )
    return last_change >= 0 and last_mega > last_change


def as_one(d: Diff) -> bool:
    # As One announces itself, then Unnerve (`-ability|<mon>|As One`,
    # `-ability|<mon>|Unnerve`): poke-env keeps the last line's.
    return _field(d) == "ability" and d.theirs == "unnerve" and str(d.ours).startswith("asone")


RULES: List[Tuple[str, Callable[[Diff], bool]]] = [
    ("as-one", as_one),
    ("mega-resets-types", mega_resets_types),
    ("wandering-spirit", wandering_spirit),
    ("ally-skill-swap", ally_skill_swap),
    ("skill-swap-kept", skill_swap_kept),
    ("transform-into-mega", transform_into_mega),
    ("stellar-types", stellar_types),
    ("single-turn-guard", single_turn_guard),
    ("baton-pass", baton_pass),
    ("copied-ability", copied_ability),
    ("fainted-keeps", fainted_keeps),
    ("learned-order", learned_order),
    ("transform-ability-kept", transform_ability_kept),
    ("transform-keeps-own", transform_keeps_own),
    ("cant-move", cant_move),
    ("shown-in-channel", shown_in_channel),
    ("imposter-move", imposter_move),
    ("final-own", final_own),
    ("revived", revived_tera),
    ("mega-stone", mega_stone),
    ("stale-mega-ability", stale_mega_ability),
    ("mega-z", mega_z),
    ("cud-chew", cud_chew),
    ("own-transformed-moves", own_transformed_moves),
    ("own-struggle", own_struggle),
    ("own-temporary-forme", own_temporary_forme),
    ("fainted-own-max-hp", fainted_own_max_hp),
    ("own-illusion-revealed", own_illusion_revealed),
    ("stale-forme", stale_forme),
    ("species-lags", species_lags),
    ("cosmetic-forme", cosmetic_forme),
    ("own-tera-type", own_tera_type),
    ("weather-since", weather_since),
    ("start-turns", start_turns),
    ("effects", effects),
    ("fainted-own-active", fainted_own_active),
    ("regenerator-guess", regenerator_guess),
    ("illusion-records", illusion_records),
]


def allowed(d: Diff) -> Optional[str]:
    """The name of the rule that allows ``d``, if any."""
    for name, rule in RULES:
        if rule(d):
            return name
    return None
