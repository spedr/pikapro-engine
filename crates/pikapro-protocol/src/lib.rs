//! Pokémon Showdown protocol adapter for `pikapro`.
//!
//! The core engine speaks in typed values ([`Choice`](pikapro::Choice),
//! [`BattleEvent`](pikapro::BattleEvent)). This crate translates them to
//! and from the text formats that the Showdown ecosystem (including poke-env)
//! understands:
//!
//! * [`choice`]: choice strings such as `move 1 2 terastallize, switch 3`.
//! * [`log`]: battle log lines such as `|move|p1a: Incineroar|Knock Off|p2a: Rillaboom`,
//!   rendered for a [`Perspective`] so hidden information
//!   (exact foe HP) stays hidden.
//! * [`team`]: the team export format and the packed format, with the
//!   fields the simulator does not model ([`SetExtras`]: nicknames, ...).
//! * [`request`]: Showdown's `|request|` JSON ([`Renderer::request_json`]).
//! * [`js`]: the JavaScript semantics these formats inherit (`parseInt`,
//!   `trim`, number printing, `JSON.stringify` of a string).
//! * [`channel`]: the way back for a player's channel: protocol lines and
//!   `|request|` JSON from a server, read into typed events and requests
//!   ([`channel::ChannelParser`], [`channel::parse_request`]). Its input is
//!   untrusted text.
//!
//! Nothing here changes how a battle plays out.

pub mod channel;
pub mod choice;
pub mod js;
pub mod log;
pub mod request;
pub mod team;

pub use choice::{
    ChoiceFlags, ChoiceMessage, ChoiceRejected, Chosen, RequestUpdate, choose, format_choice, format_choice_for,
    parse_choice_for,
};
pub use js::{json_string, push_json_string};
pub use log::{
    BattleInfo, Entry, HpHint, Perspective, PlayerInfo, Renderer, details_species, extract_channel, hp_shown,
    preview_wildcard, side_id,
};
pub use team::{
    ImportError, PackedSet, SetExtras, TeamSheet, export_sheet, import_sheet, import_team, pack_team, pokemon_name,
    unpack_sets, unpack_team,
};

/// The README's examples, run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
