//! How a battle ended.

use crate::state::SideId;

/// Why a battle ended ([`BattleResult::reason`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EndReason {
    /// One side (or both) has no Pokémon left.
    AllFainted,
    /// The format's turn limit was reached.
    TurnLimit,
    /// A side forfeited ([`Battle::forfeit`](crate::Battle::forfeit)).
    Forfeit,
    /// The Endless Battle Clause ended a battle that could have gone on
    /// for ever ([`Format::endless_battle_clause`](crate::Format::endless_battle_clause)):
    /// lost by the side whose team could cycle a restorative berry, or a
    /// tie.
    EndlessBattle,
}

/// How a battle ended: [`Battle::result`](crate::Battle::result) once it is
/// over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BattleResult {
    /// `None` for a tie.
    pub winner: Option<SideId>,
    /// Why it ended.
    pub reason: EndReason,
    /// The turn the battle ended on. At the turn limit it is the limit
    /// itself ([`Format::turn_limit`](crate::Format::turn_limit)), and
    /// under the Endless Battle Clause the turn whose end it acted at,
    /// while [`Battle::turn`](crate::Battle::turn) has moved one past it
    /// (to a turn that doesn't start).
    pub turns: u16,
}
