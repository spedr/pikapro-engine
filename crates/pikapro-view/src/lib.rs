//! # pikapro-view
//!
//! A player's view of a battle, its *information set*: what a Showdown
//! client knows, built from the engine's typed events and the player's
//! request, never from the omniscient [`BattleState`](pikapro::BattleState).
//! Projecting from state would leak what no client sees: Regenerator on a
//! benched foe, the remaining weather turns (a Damp Rock), the remaining
//! sleep turns, a disguise's true HP under Illusion.
//!
//! * [`PublicTracker`]: what a spectator knows, one per battle, updated
//!   with [`PublicTracker::apply`] for each event after `submit` returns.
//!   Per side a [`PublicSide`] with a [`MonView`] per Pokémon seen (rows
//!   in `|poke|` order, or order of appearance without team preview; a
//!   row never moves) and an [`ActiveView`] per slot; the
//!   [`PublicField`]. Illusion fools it as it fools a client, until the
//!   `|replace|`; Open Team Sheets fill it with
//!   [`PublicTracker::set_open_sheet`].
//! * [`OwnOverlay`]: what a player's request adds for its own side (exact
//!   HP, stats, moves and PP, item, ability, what it may do): one per
//!   request, [`OwnOverlay::from_request`] of the request's data, or
//!   [`OwnOverlay::from_battle`] (the same, cheaper) from the battle. A
//!   choice the request allows but the battle refuses (a hidden trap,
//!   Imprison) is Showdown's `[Unavailable choice]`: [`OwnOverlay::reject`]
//!   makes the request's update, and [`OwnOverlay::learn_update`] learns
//!   the same from an update's text.
//! * [`OwnTeam`]: what a player knows of its own team besides the
//!   request: the sets it built (natures, the Pokémon left out at team
//!   preview) and its benched Pokémon's PP when last seen.
//! * [`PlayerView`]: the three together for one player; what encoders,
//!   policies and search read, with the mappings between rows, ids and
//!   team positions.
//! * [`check`]: a test oracle that holds every claim of a tracker against
//!   the battle's state.
//!
//! The view holds only facts the log or the request states. Inference
//! (what a foe's set probably is) belongs to its users. Visibility rules
//! shared with the text protocol (HP percentages, team preview wildcards,
//! the species details show) come from `pikapro-protocol`
//! ([`pikapro_protocol::hp_shown`], [`pikapro_protocol::preview_wildcard`],
//! [`pikapro_protocol::details_species`], which the renderer uses too), so
//! they have one source of truth.
//!
//! Everything is plain `Copy` data without heap (the engine's
//! [`StackVec`] and compact IDs), so a view is copied with a fork. Not RL
//! code: bots, search and servers use it too. No external dependencies.

// Encoders and search read the view: integer-only, like the engine.
#![deny(clippy::float_arithmetic)]

mod apply;
pub mod check;
mod mon;
mod overlay;
mod side;
mod team;
mod tracker;
mod view;

pub use mon::{
    AbilityState, HpView, ItemLoss, ItemState, MAX_REVEALED_MOVES, MonFlags, MonView, OwnItem, RevealedMove, StatusKind,
};
pub use overlay::{OwnActive, OwnMon, OwnMove, OwnMoves, OwnOverlay, OwnRequest, Unavailable};
pub use pikapro::util::StackVec;
pub use pikapro_protocol::HpHint;
pub use side::{
    ActiveView, CantKind, ConditionView, FutureView, MoveOutcome, ObservedCounters, PublicField, PublicSide,
    PublicVolatiles, SCREENS, SIDE_CONDITIONS, SlotConditions, TransformView, TurnAction, TurnRecord, TurnTarget,
    taken_by,
};
pub use team::{OwnTeam, TeamMon};
pub use tracker::{
    HpReport, OpenSheet, Outcome, Phase, PublicTracker, SheetMon, ViewConfig, shown_types, wildcard_types,
};
pub use view::{PlayerView, SeriesView};

#[cfg(test)]
mod tests;

/// The README's examples, run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
