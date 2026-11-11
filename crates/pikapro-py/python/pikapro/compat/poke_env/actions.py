"""Conversions between poke-env orders and actions and pikapro action indices.

Everything goes through Showdown's choice strings, with no tables of our
own: a poke-env order's ``message`` is the choice poke-env would send, and
:meth:`pikapro.Battle.parse_choice` reads it as Showdown's ``side.choose``
does (names, move ids, ``default``); :meth:`pikapro.ActionSpace.encode`
indexes the result. The other way, :meth:`pikapro.ActionSpace.decode` gives
the positional choice (``move 2 1 terastallize, switch 3``) and the
request poke-env last parsed names the move and the Pokémon at those
positions, which is how poke-env builds its own orders.

poke-env's ``SinglesEnv`` (26 actions) and ``DoublesEnv`` (107 per slot)
number actions by poke-env's own battle state: switches by
``battle.team`` order, moves by the order the active Pokémon's moves were
learned. So the maps take the poke-env battle the actions refer to.
"""

from __future__ import annotations

from typing import Any, Optional, Union

from .feeder import choice_text, require_poke_env


def order_to_index(order, battle, side) -> int:
    """The :class:`pikapro.ActionSpace` index of a poke-env order (a
    ``BattleOrder`` or its message, ``"/choose move thunderbolt 1"``,
    ``"/team 3142"``), read against ``battle`` (a :class:`pikapro.Battle`
    waiting for ``side``'s choice) as Showdown would read it.
    Raises ``ValueError`` if Showdown would reject it."""
    message = order if isinstance(order, str) else order.message
    choice = choice_text(message)
    if choice is None:
        raise ValueError("/forfeit has no action index")
    return battle.space.encode(battle.parse_choice(side, choice))


def choice_to_order(choice, battle) -> Any:
    """The poke-env order for a :class:`pikapro.Choice`, with the moves and
    Pokémon it names taken from the request the poke-env ``battle`` last
    parsed. A team order is a ``"/team 3142"`` string, as poke-env's
    ``teampreview`` returns."""
    require_poke_env()
    from poke_env.battle.double_battle import DoubleBattle
    from poke_env.player.battle_order import DoubleBattleOrder, PassBattleOrder

    if choice.kind == "team":
        return "/team " + "".join(str(i + 1) for i in choice.order)
    request = battle.last_request
    orders = [_slot_order(a, slot, battle, request) for slot, a in enumerate(choice.actions)]
    if isinstance(battle, DoubleBattle):
        while len(orders) < 2:
            orders.append(PassBattleOrder())
        return DoubleBattleOrder(orders[0], orders[1])
    return orders[0]


def index_to_order(index: int, battle, space) -> Any:
    """The poke-env order for :class:`pikapro.ActionSpace` index ``index``
    (``space``: the format's space, or a :class:`pikapro.Battle`), for the
    poke-env ``battle`` that holds the request it answers."""
    space = getattr(space, "space", space)
    return choice_to_order(space.decode(int(index)), battle)


def action_to_index(action, battle, pikapro_battle, side, *, fake: bool = False, strict: bool = True) -> int:
    """A poke-env ``SinglesEnv``/``DoublesEnv`` action (an int, or a pair
    in doubles) to a :class:`pikapro.ActionSpace` index: the env's own
    ``action_to_order`` on the poke-env ``battle``, then
    :func:`order_to_index` on ``pikapro_battle``."""
    import numpy as np

    env = _env(battle)
    if _is_doubles(battle):
        order = env.action_to_order(np.asarray(action, dtype=np.int64), battle, fake=fake, strict=strict)
    else:
        order = env.action_to_order(np.int64(action), battle, fake=fake, strict=strict)
    return order_to_index(order, pikapro_battle, side)


def index_to_action(index: int, battle, space, *, fake: bool = False, strict: bool = True) -> Union[int, Any]:
    """A :class:`pikapro.ActionSpace` index to a poke-env ``SinglesEnv`` action
    (an int) or ``DoublesEnv`` action (an int64 array of two), through
    :func:`index_to_order` and the env's own ``order_to_action``."""
    order = index_to_order(index, battle, space)
    if isinstance(order, str):
        raise ValueError("team preview orders have no SinglesEnv/DoublesEnv action")
    action = _env(battle).order_to_action(order, battle, fake=fake, strict=strict)
    return action if _is_doubles(battle) else int(action)


def _slot_order(action, slot: int, battle, request: Optional[dict]):
    from poke_env.battle.double_battle import DoubleBattle
    from poke_env.battle.move import Move
    from poke_env.player.battle_order import PassBattleOrder, SingleBattleOrder

    if action.kind == "pass":
        return PassBattleOrder()
    if not request:
        raise ValueError("the poke-env battle has parsed no request")
    if action.kind == "switch":
        ident = request["side"]["pokemon"][action.index]["ident"]
        return SingleBattleOrder(battle.team[ident])
    entry = request["active"][slot]["moves"][action.index]
    move_id = entry["id"]
    available = battle.available_moves[slot] if isinstance(battle, DoubleBattle) else battle.available_moves
    move = next((m for m in available if m.id == move_id), None)
    if move is None:
        active = battle.active_pokemon[slot] if isinstance(battle, DoubleBattle) else battle.active_pokemon
        move = active.moves.get(move_id) if active is not None else None
    if move is None:
        move = Move(move_id, gen=battle.gen)
    return SingleBattleOrder(
        move,
        mega=action.gimmick == "mega",
        terastallize=action.gimmick == "terastallize",
        move_target=action.target or 0,
    )


def _is_doubles(battle) -> bool:
    from poke_env.battle.double_battle import DoubleBattle

    return isinstance(battle, DoubleBattle)


def _env(battle):
    require_poke_env()
    if _is_doubles(battle):
        from poke_env.environment.doubles_env import DoublesEnv

        return DoublesEnv
    from poke_env.environment.singles_env import SinglesEnv

    return SinglesEnv
