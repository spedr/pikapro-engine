"""poke-env compatibility: poke-env's own battle trackers and unmodified
poke-env players on :class:`pikapro.Battle`, in-process, with no server.

poke-env is an optional dependency (``pip install 'pikapro[poke-env]'``); it
is imported when a function here first needs it.

* :class:`Feeder` keeps a poke-env ``Battle``/``DoubleBattle`` in sync with
  one side, from that side's channel lines and requests, through poke-env's
  own parsing.
* :func:`play` and :func:`cross_evaluate` let poke-env ``Player`` objects
  play each other; :func:`make_player` creates one for a pikapro format.
* :func:`order_to_index`, :func:`index_to_order`,
  :func:`action_to_index` and :func:`index_to_action` convert between
  poke-env's orders, ``SinglesEnv``/``DoublesEnv`` actions and pikapro's
  :class:`pikapro.ActionSpace` indices, through Showdown choice strings.
* :func:`snapshot` is poke-env's tracked state as plain JSON
  (``tools/view-oracle/SCHEMA.md``).

Formats map to the Showdown ids poke-env players use (:data:`SHOWDOWN_FORMATS`).
"""

from ._formats import SHOWDOWN_FORMATS, pikapro_format, showdown_format
from .actions import (
    action_to_index,
    choice_to_order,
    index_to_action,
    index_to_order,
    order_to_index,
)
from .driver import BattleRecord, PlayResults, cross_evaluate, make_player, play, play_parallel
from .feeder import Decision, Feeder, FeederError, choice_text, require_poke_env
from .snapshot import UNKNOWN, snapshot

__all__ = [
    "SHOWDOWN_FORMATS",
    "UNKNOWN",
    "BattleRecord",
    "Decision",
    "Feeder",
    "FeederError",
    "PlayResults",
    "action_to_index",
    "choice_text",
    "choice_to_order",
    "cross_evaluate",
    "index_to_action",
    "index_to_order",
    "make_player",
    "order_to_index",
    "pikapro_format",
    "play",
    "play_parallel",
    "require_poke_env",
    "showdown_format",
    "snapshot",
]
