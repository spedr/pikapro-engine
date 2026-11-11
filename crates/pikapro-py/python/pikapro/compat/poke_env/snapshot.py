"""poke-env's tracked state, normalized to plain JSON.

This is the schema the view differential compares on (see
``tools/view-oracle/SCHEMA.md``, which is the reference for every field).
Only ints, strings, bools, null, lists and objects: no floats, so another
implementation can produce byte-identical JSON.
"""

from __future__ import annotations

from typing import Any, Dict, List, Optional

#: The marker for a fact poke-env doesn't know.
UNKNOWN = "?"

BOOSTS = ("accuracy", "atk", "def", "evasion", "spa", "spd", "spe")

_FORMES: Dict[int, Dict[int, str]] = {}


def _forme_of(mon) -> str:
    """The Pokédex entry whose base stats poke-env last loaded for ``mon``
    (``_update_from_pokedex`` keeps a reference to the entry's
    ``baseStats``): the forme after a Mega Evolution or forme change,
    which ``Pokemon.species`` doesn't follow."""
    from poke_env.data import GenData

    gen = mon.gen
    table = _FORMES.get(gen)
    if table is None:
        dex = GenData.from_gen(gen).pokedex
        table = {id(entry["baseStats"]): key for key, entry in dex.items() if "baseStats" in entry}
        _FORMES[gen] = table
    return table.get(id(getattr(mon, "_base_stats", None)), mon.species)


def _is_mega(mon, forme: str) -> bool:
    from poke_env.data import GenData

    entry = GenData.from_gen(mon.gen).pokedex.get(forme, {})
    return str(entry.get("forme", "")).startswith("Mega")


def _name(enum_value) -> Optional[str]:
    """A poke-env enum member as a Showdown ID: ``STEALTH_ROCK`` ->
    ``stealthrock`` (poke-env names members after Showdown's names)."""
    return None if enum_value is None else enum_value.name.lower().replace("_", "")


def _item(mon) -> Optional[str]:
    from poke_env.data import GenData

    item = mon.item
    if item == GenData.from_gen(mon.gen).UNKNOWN_ITEM:
        return UNKNOWN
    return item or None


def pokemon(mon) -> Dict[str, Any]:
    """One poke-env ``Pokemon``, normalized."""
    forme = _forme_of(mon)
    gender = mon.gender
    return {
        "species": mon.species,
        "forme": forme,
        "level": mon.level,
        "gender": {"MALE": "M", "FEMALE": "F", "NEUTRAL": "N"}[gender.name] if gender is not None else None,
        "hp": mon.current_hp,
        "max_hp": mon.max_hp,
        "status": _name(mon.status),
        "boosts": {k: int(mon.boosts.get(k, 0)) for k in BOOSTS},
        "item": _item(mon),
        "ability": mon.ability if mon.ability is not None else UNKNOWN,
        "moves": list(mon._moves._base_moves.keys()),
        "types": [_name(t) for t in mon.types if t is not None],
        "tera_type": _name(mon.tera_type) if mon.tera_type is not None else UNKNOWN,
        "terastallized": bool(mon.is_terastallized),
        "mega": _is_mega(mon, forme),
        "fainted": bool(mon.fainted),
        "active": bool(mon.active),
        "revealed": bool(mon.revealed),
        "effects": {_name(e): int(n) for e, n in mon.effects.items()},
    }


def _keys(team: Dict[str, Any], mons: List[Any]) -> List[Optional[str]]:
    by_id = {id(m): k for k, m in team.items()}
    return [by_id.get(id(m)) if m is not None else None for m in mons]


def snapshot(battle) -> Dict[str, Any]:
    """A poke-env ``Battle`` or ``DoubleBattle``'s tracked state as plain
    JSON (see ``tools/view-oracle/SCHEMA.md``)."""
    from poke_env.battle.double_battle import DoubleBattle

    doubles = isinstance(battle, DoubleBattle)
    if battle.player_role is None:
        active: List[Any] = []
        opp_active: List[Any] = []
    elif doubles:
        active = list(battle.active_pokemon)
        opp_active = list(battle.opponent_active_pokemon)
    else:
        active = [battle.active_pokemon]
        opp_active = [battle.opponent_active_pokemon]
    force = battle.force_switch
    weather = next(iter(battle.weather.items()), None)
    return {
        "role": battle.player_role,
        "turn": battle.turn,
        "finished": bool(battle.finished),
        "won": battle.won,
        "teampreview": bool(battle.teampreview),
        "wait": bool(battle._wait),
        "force_switch": [bool(x) for x in force] if isinstance(force, list) else [bool(force)],
        "used_tera": bool(battle.used_tera),
        "opponent_used_tera": bool(battle.opponent_used_tera),
        "used_mega": bool(battle.used_mega_evolve),
        "opponent_used_mega": bool(battle.opponent_used_mega_evolve),
        "weather": {"name": _name(weather[0]), "since": int(weather[1])} if weather else None,
        "fields": {_name(f): int(t) for f, t in battle.fields.items()},
        "side_conditions": {_name(c): int(v) for c, v in battle.side_conditions.items()},
        "opponent_side_conditions": {_name(c): int(v) for c, v in battle.opponent_side_conditions.items()},
        "active": _keys(battle.team, active),
        "opponent_active": _keys(battle._opponent_team, opp_active),
        "team": {k: pokemon(m) for k, m in battle.team.items()},
        "opponent_team": {k: pokemon(m) for k, m in battle._opponent_team.items()},
        "opponent_preview": [m.species for m in battle.teampreview_opponent_team],
    }
