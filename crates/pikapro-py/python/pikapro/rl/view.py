"""A read-only, poke-env-shaped facade over one player's view.

:class:`BattleView` answers with names shaped like poke-env's
``AbstractBattle`` / ``Pokemon`` / ``Move`` (about 30 members, chosen from
poke-env's baseline players and examples). It is what a Python
``embed_battle`` reads (:func:`pikapro.rl.embed_all`); an encoder written
in Rust is the fast path. Values are plain strings and numbers (lowercase
ids, ``"fire"``, ``"brn"``), not poke-env's enums.

It holds only what the player knows: the view is built from the log and
the player's request, never from the battle's state.
"""

from __future__ import annotations

import operator
from typing import Any, Dict, List, Optional, Union

import numpy as np

from .. import _pikapro

__all__ = ["BattleView", "MoveView", "PokemonView"]

STATUS_NAMES = [None, "brn", "frz", "par", "psn", "tox", "slp"]
WEATHER_NAMES = [None, "sunnyday", "raindance", "sandstorm", "snow", "desolateland", "primordialsea", "deltastream"]
TERRAIN_NAMES = [None, "electricterrain", "grassyterrain", "mistyterrain", "psychicterrain"]
TARGETS = [
    "normal", "adjacentFoe", "adjacentAlly", "adjacentAllyOrSelf", "any", "allAdjacentFoes", "allAdjacent",
    "self", "allies", "allySide", "foeSide", "all", "randomNormal", "allyTeam", "scripted",
]  # fmt: skip
BOOSTS = ["atk", "def", "spa", "spd", "spe", "accuracy", "evasion"]
_PSEUDO = ["trick_room", "gravity", "magic_room", "wonder_room"]


#: Showdown's ``toID`` (:func:`pikapro.to_id`): ASCII letters and digits only,
#: so accented letters go (``"Flabébé"`` -> ``"flabb"``), as in Showdown and
#: poke-env.
to_id = _pikapro.to_id


class MoveView:
    """A move as the player knows it (poke-env's ``Move``): ``id``,
    ``name``, ``base_power``, ``type``, ``category``, ``accuracy``
    (a fraction, ``True`` if it never misses), ``priority``, ``target``,
    ``current_pp`` / ``max_pp`` (own Pokémon: the request's while active,
    else the last a request showed; ``None`` for the foe's),
    ``disabled``, ``slot`` (its move slot) and ``uses``."""

    __slots__ = ("id", "name", "base_power", "type", "category", "accuracy", "priority", "target",
                 "current_pp", "max_pp", "disabled", "slot", "uses")  # fmt: skip

    def __init__(self, move: Dict[str, Any]):
        self.name = move["name"]
        self.id = to_id(self.name)
        self.base_power = move["base_power"]
        self.type = move["type"]
        self.category = move["category"]
        self.accuracy = True if move["accuracy"] is None else move["accuracy"] / 100
        self.priority = move["priority"]
        self.target = TARGETS[move["target"]] if 0 <= move["target"] < len(TARGETS) else None
        self.current_pp = move["pp"]
        self.max_pp = move["max_pp"]
        self.disabled = move["disabled"]
        self.slot = move["slot"]
        self.uses = move["uses"]

    def __repr__(self) -> str:
        return f"MoveView({self.id!r}, bp={self.base_power}, type={self.type!r})"


class PokemonView:
    """One Pokémon as the player knows it (poke-env's ``Pokemon``)."""

    def __init__(self, battle: "BattleView", side: int, row: int):
        self._b = battle
        self.side = side
        self.row = row
        #: What the view has of it; ``None`` for a row with no Pokémon.
        self._m: Optional[Dict[str, Any]] = battle._view["mons"][side][row]

    def _get(self, key: str, default=None):
        return default if self._m is None else self._m[key]

    @property
    def own(self) -> bool:
        """Whether it is the player's own Pokémon (``False`` for a
        foe's)."""
        return self.side == 0

    @property
    def species(self) -> str:
        """The species id shown now (``"urshifurapidstrike"``); ``""`` if
        unknown."""
        return to_id(self.species_name or "")

    @property
    def species_name(self) -> Optional[str]:
        """The species' display name shown now
        (``"Urshifu-Rapid-Strike"``); ``None`` if unknown."""
        return self._get("species")

    @property
    def base_species(self) -> Optional[str]:
        """The base species' id (``"urshifu"``); ``None`` if unknown."""
        name = self._get("base_species")
        return None if name is None else to_id(name)

    @property
    def level(self) -> int:
        """Its level (as shown in the details for a foe)."""
        return self._get("level", 0)

    @property
    def gender(self) -> str:
        """``"male"``, ``"female"`` or ``"genderless"``."""
        return self._get("gender", "genderless")

    @property
    def current_hp(self) -> int:
        """HP as the player knows it: exact for its own Pokémon, the shown
        value (a percentage by default) for the foe's."""
        return self._get("hp", 0)

    @property
    def max_hp(self) -> int:
        """The maximum of :attr:`current_hp`: its exact max HP for its
        own Pokémon, the shown scale (100 by default) for the foe's."""
        return self._get("max_hp", 0)

    @property
    def current_hp_fraction(self) -> float:
        """``current_hp / max_hp``; 1.0 before any HP was shown, 0 fainted."""
        if self.fainted:
            return 0.0
        m = self.max_hp
        return 1.0 if m <= 0 else self.current_hp / m

    @property
    def status(self) -> Optional[str]:
        """``None``, ``"brn"``, ``"frz"``, ``"par"``, ``"psn"``, ``"tox"``
        or ``"slp"`` (``"fnt"`` when fainted)."""
        if self.fainted:
            return "fnt"
        s = self._get("status", 0)
        return STATUS_NAMES[s] if 0 <= s < len(STATUS_NAMES) else None

    @property
    def status_counter(self) -> int:
        """Turns the status was observed to last (sleep, toxic)."""
        return self._get("status_turns", 0)

    @property
    def boosts(self) -> Dict[str, int]:
        """Stat stages by name (``atk``, ``def``, ``spa``, ``spd``,
        ``spe``, ``accuracy``, ``evasion``), -6 to 6."""
        return dict(zip(BOOSTS, self._get("boosts", [0] * len(BOOSTS))))

    @property
    def types(self) -> List[str]:
        """Its current types (Tera and type changes applied), then an added
        type; empty while unknown."""
        return list(self._get("types", []))

    @property
    def type_1(self) -> Optional[str]:
        """The first of :attr:`types`; ``None`` while unknown."""
        t = self.types
        return t[0] if t else None

    @property
    def type_2(self) -> Optional[str]:
        """The second of :attr:`types`; ``None`` for a single type or
        while unknown."""
        t = self.types
        return t[1] if len(t) > 1 else None

    @property
    def tera_type(self) -> Optional[str]:
        """Its Tera type if known (its own, an Open Team Sheet's, or
        shown by Terastallizing); ``None`` otherwise, and in a format
        without Terastallization."""
        return self._get("tera_type")

    @property
    def is_terastallized(self) -> bool:
        """Whether it has Terastallized."""
        return self._get("terastallized", False)

    @property
    def is_mega(self) -> bool:
        """Whether it has Mega Evolved (Champions)."""
        return self._get("mega", False)

    @property
    def item(self) -> str:
        """poke-env's convention: ``"unknown_item"`` while unknown, ``""``
        for no item (or a lost one), else the item's id."""
        name = self._get("item")
        if name is None:
            return "unknown_item"
        return to_id(name)

    @property
    def ability(self) -> Optional[str]:
        """The ability's id, ``None`` while unknown."""
        name = self._get("ability")
        return to_id(name) if name else None

    @property
    def moves(self) -> Dict[str, MoveView]:
        """The known moves by id: every own move; a foe's revealed ones (or
        its Open Team Sheet's)."""
        out = {}
        for move in self._get("moves", []):
            m = MoveView(move)
            out[m.id] = m
        return out

    @property
    def active(self) -> bool:
        """Whether it is on the field now."""
        return self._get("active", False)

    @property
    def fainted(self) -> bool:
        """Whether it has fainted."""
        return self._get("fainted", False)

    @property
    def revealed(self) -> bool:
        """Whether it has been on the field (a foe's Pokémon only seen at
        team preview is not revealed)."""
        return self._get("revealed", False)

    @property
    def slot(self) -> Optional[int]:
        """The active slot it is in."""
        return self._get("slot")

    @property
    def stats(self) -> Dict[str, Optional[int]]:
        """Exact stats for its own Pokémon (``None`` for a foe's)."""
        names = ("atk", "def", "spa", "spd", "spe")
        stats = self._get("stats")
        return dict.fromkeys(names) if stats is None else dict(zip(names, stats))

    @property
    def base_stats(self) -> Dict[str, int]:
        """The species' base stats by name (``hp`` to ``spe``); empty
        while the species is unknown."""
        stats = self._get("base_stats")
        return {} if stats is None else dict(zip(("hp", "atk", "def", "spa", "spd", "spe"), stats))

    @property
    def effects(self) -> List[str]:
        """Its public volatiles (active only): ``"substitute"``,
        ``"confusion"``, ``"taunt"``, ..."""
        return list(self._get("effects", []))

    @property
    def first_turn(self) -> bool:
        """Whether it is active and switched in this turn (Fake Out,
        First Impression)."""
        return self.active and self._get("turns_active") == 0

    @property
    def protect_counter(self) -> int:
        """Consecutive successful protections (Protect, Detect, ...) so
        far."""
        return self._get("protect_chain", 0)

    @property
    def must_recharge(self) -> bool:
        """Whether it must recharge this turn (after Hyper Beam and the
        like)."""
        return "must_recharge" in self._get("effects", [])

    def damage_multiplier(self, attack: Union[str, MoveView]) -> float:
        """The type effectiveness of ``attack`` (a type name or a move)
        against this Pokémon's current types."""
        t = attack.type if isinstance(attack, MoveView) else attack
        return _pikapro._type_multiplier(t.lower(), self.types)

    def identifier(self, role: str) -> str:
        """poke-env's identifier for it under ``role`` (``"p1"`` or
        ``"p2"``): ``"p1: Garchomp"``."""
        return f"{role}: {self.species_name}"

    def __repr__(self) -> str:
        hp = f"{self.current_hp}/{self.max_hp}"
        return f"PokemonView({self.species_name!r}, {'own' if self.own else 'foe'}, hp={hp}, status={self.status})"


class BattleView:
    """One player's view of a battle, poke-env-shaped and read-only.

    Get one from a :class:`pikapro.Battle` (:meth:`from_battle`, or
    ``battle.view(side)``), from a :class:`pikapro.rl.VecEnv`
    (:meth:`from_env`, or ``env.view(i)``) or from a
    :class:`pikapro.rl.ChannelSession` (``session.view``). In doubles,
    per-slot members are lists (``active_pokemon[slot]``), as poke-env's
    ``DoubleBattle`` has them.
    """

    def __init__(
        self,
        view: Dict[str, Any],
        format: str,
        *,
        mask: Optional[np.ndarray] = None,
        masks: str = "factored",
        switch_by: str = "row",
        player_role: Optional[str] = None,
        series: Optional[Dict[str, int]] = None,
    ):
        from .. import ActionSpace

        if view.get("format") != format:
            raise ValueError(f"a view of a {view.get('format')} battle, not {format}")
        self._view = view
        self.format = format
        self.space = ActionSpace(format)
        self.mask = None if mask is None else np.asarray(mask, dtype=bool)
        self.masks_layout = masks
        self.switch_by = switch_by
        self.player_role = player_role
        #: In a best-of series: ``game`` (1-based), ``wins`` (the player's),
        #: ``opponent_wins`` and ``result`` (0 going on, 1 won, 2 lost, 3
        #: tied); ``None`` for a single game.
        self.series = series
        self._team = [PokemonView(self, 0, r) for r in range(6)]
        self._foes = [PokemonView(self, 1, r) for r in range(6)]

    # -- construction ---------------------------------------------------------

    @classmethod
    def from_battle(cls, battle, side) -> "BattleView":
        """``side``'s view of a :class:`pikapro.Battle` (which must record
        events), with its request's legal actions as the mask (switches by
        team position). ``side`` is 0 or 1, or ``"p1"`` or ``"p2"`` in any
        case, as for the battle's own methods; anything else raises
        ``ValueError`` (``2``, ``"p3"``) or ``TypeError`` (a bool)."""
        values = battle._view(side)  # (refuses a bad side, as every Battle method does)
        s = int(side.lower() == "p2") if isinstance(side, str) else operator.index(side)
        return cls(
            values, battle.format, mask=battle.slot_mask(s).reshape(-1), masks="slots",
            switch_by="position", player_role=f"p{s + 1}",
        )  # fmt: skip

    @classmethod
    def from_env(cls, env, i: int, player: int = 0) -> "BattleView":
        """Player ``player``'s view of battle ``i`` of a
        :class:`pikapro.rl.VecEnv`, as it is now, with the last step's
        mask."""
        role = f"p{env.side_of(i, player) + 1}"  # (first: it refuses a bad player)
        step = env.last
        mask = step.masks[i, player] if step.masks is not None else None
        cfg = env.config
        series = None
        if step.series is not None:
            game, w0, w1, result = (int(x) for x in step.series[i])
            wins, theirs = (w0, w1) if player == 0 else (w1, w0)
            if player == 1 and result in (1, 2):
                result = 3 - result
            series = {"game": game, "wins": wins, "opponent_wins": theirs, "result": result}
        return cls(
            env._view(i, player), env.format, mask=mask, masks=cfg["masks"], switch_by=cfg["switch_by"], player_role=role,
            series=series,
        )  # fmt: skip

    # -- battle -----------------------------------------------------------------

    @property
    def turn(self) -> int:
        """The current turn (0 at team preview)."""
        return self._view["turn"]

    @property
    def teampreview(self) -> bool:
        """Whether the battle is at team preview."""
        return self._view["phase"] == 1

    @property
    def finished(self) -> bool:
        """Whether the battle has ended."""
        return self._view["phase"] == 3

    @property
    def won(self) -> Optional[bool]:
        """``True`` if the player won, ``False`` if it lost or tied;
        ``None`` while the battle goes on."""
        o = self._view["outcome"]
        return None if o == 0 else o == 1

    @property
    def lost(self) -> Optional[bool]:
        """``True`` if the player lost, ``False`` if it won or tied;
        ``None`` while the battle goes on."""
        o = self._view["outcome"]
        return None if o == 0 else o == 2

    @property
    def doubles(self) -> bool:
        """Whether the format has two active slots per side."""
        return self.space.active > 1

    def _present(self, mons: List[PokemonView]) -> List[PokemonView]:
        return [m for m in mons if m._m is not None]

    def _listed(self, m: PokemonView) -> bool:
        """An own Pokémon the request lists: every one at team preview, the
        ones brought after it."""
        return m._m is not None and (m._m["brought"] or self._view["phase"] < 2)

    @property
    def team(self) -> Dict[str, PokemonView]:
        """The player's Pokémon its request lists, by identifier (``"p1:
        Garchomp"``), in own row order: after team preview, the ones
        brought."""
        role = self.player_role or "p1"
        return {m.identifier(role): m for m in self._team if self._listed(m)}

    @property
    def opponent_team(self) -> Dict[str, PokemonView]:
        """The foe's Pokémon seen so far (previewed or revealed)."""
        role = {"p1": "p2", "p2": "p1"}.get(self.player_role or "p1", "p2")
        return {m.identifier(role): m for m in self._present(self._foes)}

    def _actives(self, mons: List[PokemonView]) -> List[Optional[PokemonView]]:
        out: List[Optional[PokemonView]] = [None] * self.space.active
        for m in self._present(mons):
            s = m.slot
            if m.active and s is not None and s < len(out):
                out[s] = m
        return out

    @property
    def active_pokemon(self):
        """The player's active Pokémon (a list by slot in doubles)."""
        a = self._actives(self._team)
        return a if self.doubles else a[0]

    @property
    def opponent_active_pokemon(self):
        """The foe's active Pokémon (a list by slot in doubles; ``None``
        for an empty slot)."""
        a = self._actives(self._foes)
        return a if self.doubles else a[0]

    @property
    def all_active_pokemons(self) -> List[Optional[PokemonView]]:
        """Every active slot: the player's, then the foe's (``None`` for
        an empty one)."""
        return self._actives(self._team) + self._actives(self._foes)

    # -- what the player may do -------------------------------------------------

    def _slot_masks(self) -> Optional[np.ndarray]:
        """Each slot's own allowed actions, ``(active, slot_size)``."""
        if self.mask is None:
            return None
        sp = self.space
        S = sp.slot_size
        m = self.mask
        if self.masks_layout == "joint":
            joint = m[: sp.joint_size]
            if sp.active == 1:
                return joint[None, :S]
            j = joint.reshape(S, S)
            return np.stack([j.any(axis=1), j.any(axis=0)])
        return m[: sp.active * S].reshape(sp.active, S)

    def _row_of_switch(self, k: int) -> Optional[PokemonView]:
        if self.switch_by == "row":
            return self._team[k] if k < 6 else None
        for m in self._team:
            if self._listed(m) and m._m["team_pos"] == k:
                return m
        return None

    def _available_moves(self, slot: int) -> List[MoveView]:
        sp = self.space
        per_move = (1 + len(sp.gimmicks)) * sp.targets
        active = self._actives(self._team)[slot]
        if active is None:
            return []
        moves = active.moves
        masks = self._slot_masks()
        by_slot = {m.slot: m for m in moves.values()}
        if masks is None:
            if not active._m["request_moves"]:
                return []
            return [m for m in moves.values() if not m.disabled and (m.current_pp is None or m.current_pp > 0)]
        return [by_slot[k] for k in range(4) if k in by_slot and masks[slot, k * per_move : (k + 1) * per_move].any()]

    def _available_switches(self, slot: int) -> List[PokemonView]:
        sp = self.space
        masks = self._slot_masks()
        if masks is None:
            return [m for m in self._team if self._listed(m) and not m.active and not m.fainted]
        out = []
        for k in range(6):
            if masks[slot, sp.switch_base + k]:
                m = self._row_of_switch(k)
                if m is not None:
                    out.append(m)
        return out

    @property
    def available_moves(self):
        """The moves the request allows (per slot in doubles)."""
        a = [self._available_moves(s) for s in range(self.space.active)]
        return a if self.doubles else a[0]

    @property
    def available_switches(self):
        """The Pokémon the player may switch in (per slot in doubles)."""
        a = [self._available_switches(s) for s in range(self.space.active)]
        return a if self.doubles else a[0]

    def _per_slot(self, col: str):
        a = [m._m[col] if m is not None else False for m in self._actives(self._team)]
        return a if self.doubles else a[0]

    @property
    def can_tera(self):
        """Whether the player may Terastallize (per slot in doubles;
        always ``False`` without Terastallization)."""
        return self._per_slot("can_tera")

    @property
    def can_mega_evolve(self):
        """Whether the player may Mega Evolve (per slot in doubles;
        Champions; always ``False`` without Mega Evolution)."""
        return self._per_slot("can_mega")

    @property
    def trapped(self):
        """Whether the request says the active Pokémon is trapped (per
        slot in doubles)."""
        return self._per_slot("trapped")

    @property
    def maybe_trapped(self):
        """Whether the request says the active Pokémon may be trapped by
        something unseen, such as a foe's Shadow Tag (per slot in
        doubles). Its switches stay allowed; see the honest masks in
        ``docs/python/concepts.md``."""
        return self._per_slot("maybe_trapped")

    @property
    def force_switch(self):
        """Slots that must switch (a forced-switch request)."""
        forced = self._view["force_switch"]
        a = [s < len(forced) and forced[s] for s in range(self.space.active)]
        return a if self.doubles else a[0]

    # -- field --------------------------------------------------------------------

    @property
    def weather(self) -> Dict[str, int]:
        """``{weather: turns elapsed}`` (empty without weather)."""
        w = self._view["weather"]
        return {WEATHER_NAMES[w]: self._view["weather_turns"]} if 0 < w < len(WEATHER_NAMES) else {}

    @property
    def fields(self) -> Dict[str, int]:
        """Terrain and pseudo-weathers: ``{name: turns elapsed}``."""
        out = {}
        t = self._view["terrain"]
        if 0 < t < len(TERRAIN_NAMES):
            out[TERRAIN_NAMES[t]] = self._view["terrain_turns"]
        for p in _PSEUDO:
            v = self._view[p]
            if v is not None:
                out[to_id(p)] = v
        if self._view["fairy_lock"] is not None:
            out["fairylock"] = self._view["fairy_lock"]
        return out

    def _conditions(self, side: int) -> Dict[str, int]:
        return {to_id(c): layers for c, layers in self._view["sides"][side]["conditions"].items()}

    @property
    def side_conditions(self) -> Dict[str, int]:
        """The player's side conditions: ``{id: layers}``."""
        return self._conditions(0)

    @property
    def opponent_side_conditions(self) -> Dict[str, int]:
        """The foe's side conditions: ``{id: layers}``."""
        return self._conditions(1)

    @property
    def used_tera(self) -> bool:
        """Whether the player has used its Terastallization."""
        return self._view["sides"][0]["tera_used"]

    @property
    def opponent_used_tera(self) -> bool:
        """Whether the foe has used its Terastallization."""
        return self._view["sides"][1]["tera_used"]

    @property
    def used_mega_evolve(self) -> bool:
        """Whether the player has Mega Evolved (Champions)."""
        return self._view["sides"][0]["mega_used"]

    @property
    def opponent_used_mega_evolve(self) -> bool:
        """Whether the foe has Mega Evolved (Champions)."""
        return self._view["sides"][1]["mega_used"]

    def __repr__(self) -> str:
        return f"BattleView({self.format!r}, turn={self.turn}, role={self.player_role!r})"
