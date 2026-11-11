//! # pikapro
//!
//! A deterministic Pokémon battle simulator. This crate is the *simulation
//! layer only*: it knows the rules of a battle and nothing about networking,
//! text protocols, observation encodings or learning algorithms.
//!
//! ```
//! use pikapro::data::gen9::{abilities as a, items as i, moves as m, species as s};
//! use pikapro::data::{Nature, Stats};
//! use pikapro::{Battle, BattleEvent, BattleOptions, BattleRng, Format, Investment, PokemonSet};
//! use pikapro::{Seed, SideId, Team};
//!
//! // A set per Pokémon: IDs from the dex, a nature, EVs and IVs, up to four moves.
//! let evs = Investment::Evs { evs: Stats::new(252, 252, 0, 0, 4, 0), ivs: Stats::splat(31) };
//! let set = |species, ability, item, moves: &[_]| {
//!     PokemonSet::new(species)
//!         .with_level(50)
//!         .with_ability(ability)
//!         .with_item(item)
//!         .with_nature(Nature::Adamant)
//!         .with_investment(evs)
//!         .with_moves(moves)
//! };
//! let team = Team::new(vec![
//!     set(s::GARCHOMP, a::ROUGH_SKIN, i::LIFE_ORB, &[m::EARTHQUAKE, m::DRAGON_CLAW, m::PROTECT]),
//!     set(s::INCINEROAR, a::INTIMIDATE, i::SITRUS_BERRY, &[m::FAKE_OUT, m::FLARE_BLITZ]),
//!     set(s::AMOONGUSS, a::REGENERATOR, i::ROCKY_HELMET, &[m::SPORE, m::RAGE_POWDER]),
//! ]);
//!
//! let format = Format::gen9_doubles();
//! let options = BattleOptions::new(Seed::fast(42));
//! let mut battle = Battle::new(format, [team.clone(), team], options).unwrap();
//! let mut policy = BattleRng::new(Seed::fast(7)); // the policy's own randomness
//!
//! while !battle.is_over() {
//!     for side in SideId::BOTH {
//!         let legal = battle.legal_actions(side); // per-slot legal actions
//!         if legal.is_waiting() {
//!             continue;
//!         }
//!         battle.submit(side, legal.sample(&mut policy)).unwrap(); // resolves once both are in
//!     }
//! }
//!
//! // What happened is in the events (pikapro-protocol renders them as Showdown's log).
//! let faints = battle.events().iter().filter(|e| matches!(e, BattleEvent::Faint { .. })).count();
//! println!("{:?} after {faints} faints", battle.result());
//! ```
//!
//! ## Where to start
//!
//! - **Teams:** [`PokemonSet`] and [`Team`], with IDs from [`data`]'s dex
//!   (`pikapro-protocol`'s `import_team` reads Showdown's export text;
//!   `pikapro-validate` checks legality).
//! - **A battle:** [`Battle::new`] with a [`Format`] preset and
//!   [`BattleOptions`] (the [`Seed`] decides everything random).
//! - **Playing:** each side answers its [`Request`] ([`Battle::request`])
//!   with a [`Choice`] through [`Battle::submit`]; [`LegalActions`]
//!   ([`Battle::legal_actions`]) lists what is legal. A mid-turn forced
//!   switch (U-turn, a faint) is a [`Request::ForceSwitch`] for one side.
//!   [`Battle::request_data`] is what Showdown's `|request|` JSON says.
//! - **Reading it:** [`Battle::events`] ([`BattleEvent`]: what a Showdown
//!   log says, typed; `pikapro-protocol` renders it), [`Battle::state`]
//!   (everything, hidden information included) and [`Battle::result`].
//! - **Search:** `Clone` a battle for an identical future, or
//!   [`Battle::fork`] it for a new random stream.
//!
//! ## Layout
//!
//! | module        | responsibility |
//! |---------------|----------------|
//! | [`battle`]    | [`Battle`]: owns state + RNG, accepts choices, drives phases |
//! | [`state`]     | Plain-data [`BattleState`]: sides, Pokémon, field. `Clone` = snapshot |
//! | [`choice`]    | What a player submits: [`Choice`], [`SlotAction`], [`Target`] |
//! | [`request`]   | What the battle is waiting for from each side |
//! | [`legal`]     | Legal action generation and choice validation |
//! | `resolve` (internal) | Turn resolution: action queue, moves, damage, switching, residuals |
//! | [`format`](mod@format) | [`Format`] / ruleset: game type, clauses, gimmicks, level rules |
//! | [`team`]      | Team *input* ([`PokemonSet`], [`Team`]) and validation |
//! | [`rng`]       | [`BattleRng`]: the only source of randomness |
//! | [`event`]     | Structured [`BattleEvent`] log (rendered to text by `pikapro-protocol`) |
//! | [`result`]    | [`BattleResult`] |
//! | [`stats`]     | Stat formulas: stats from a set, stages, 4096-based modifiers |
//! | [`typing`]    | A move's effective type and effectiveness, for readers that know only part of a battle |
//! | [`util`]      | [`StackVec`](util::StackVec) and other allocation-free containers |
//! | [`testing`]   | Sample teams and helpers for tests, examples and benchmarks; not simulation API |

// Mechanics must be integer-only to be deterministic across platforms.
#![deny(clippy::float_arithmetic)]
// Every public item has a doc (`cargo ci-check` turns this into an error).
#![warn(missing_docs)]

pub mod battle;
pub mod choice;
pub mod event;
pub mod format;
pub mod legal;
pub mod request;
pub(crate) mod resolve;
pub mod result;
pub mod rng;
pub mod state;
pub mod stats;
pub mod team;
pub mod testing;
pub mod typing;
pub mod util;

pub use pikapro_data as data;

pub use battle::{Battle, BattleOptions, Progress};
pub use choice::{Choice, ChoiceError, SlotAction, Target, TeamOrder};
pub use event::{BattleEvent, EventLog};
pub use format::{Clauses, Format, GameType, Gimmick, Gimmicks, LevelRule, Mechanics, StatSystem, TeamPreview};
pub use legal::LegalActions;
pub use request::Request;
pub use result::{BattleResult, EndReason};
pub use rng::{BattleRng, Seed};
pub use state::{
    BattleState, EndlessState, FieldState, MAX_ACTIVE, MAX_MOVES, MAX_TEAM, MoveSlot, PokemonState, Position,
    SideConditions, SideId, SideState, Staleness, Status,
};
pub use team::{Gender, Investment, PokemonSet, SetError, Team, TeamError};

/// The README's examples, run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
