//! Pokémon Showdown's battle stream on pikapro.
//!
//! [`BattleStream`] speaks the text protocol of Showdown's `BattleStream`
//! (`sim/battle-stream.ts`, documented in Showdown's `sim/SIMULATOR.md`):
//! write `>start`, `>player` and choice lines, read `update`,
//! `sideupdate` and `end` messages, byte for byte what Showdown sends for
//! the same input (the time in `|t:|` aside, and the `|debug|` lines
//! Showdown writes in its custom-game formats). [`PlayerStreams`] splits
//! it into Showdown's `getPlayerStreams` channels (omniscient, spectator,
//! each player). The `pikapro-simulate` binary is a drop-in for
//! `pokemon-showdown simulate-battle` over stdin and stdout.
//!
//! ```
//! use pikapro_server::{BattleStream, Message, StreamOptions};
//! let mut stream = BattleStream::new(StreamOptions::default());
//! let team = "Pikachu||lightball|static|thunderbolt,quickattack|Timid|,,,252,,252||||50|";
//! stream.write(r#">start {"formatid":"gen9customgame@@@!Team Preview","seed":"sodium,0123"}"#).unwrap();
//! stream.write(&format!(r#">player p1 {{"name":"Alice","team":"{team}"}}"#)).unwrap();
//! stream.write(&format!(r#">player p2 {{"name":"Bob","team":"{team}"}}"#)).unwrap();
//! stream.write(">p1 move thunderbolt\n>p2 move 2").unwrap();
//! for message in stream.drain() {
//!     if let Message::SideUpdate { data, .. } = &message {
//!         assert!(data.starts_with("|request|"));
//!     }
//!     println!("{message}\n");
//! }
//! ```
//!
//! Formats are the pikapro presets, named by the Showdown format each
//! mirrors or by their own id, and `[Gen 9] VGC 2025 Reg I` on the
//! `gen9vgc` preset ([`format`](mod@format)). Seeds are Showdown's
//! (`sodium,<hex>`, `gen5,<hex>` or four numbers); a battle without one
//! gets a fresh `sodium` seed, as Showdown's does, and the battle runs on
//! the same generator as Showdown's (`pikapro::Seed::Sodium`).
//!
//! The library has no dependencies besides pikapro, pikapro-data and
//! pikapro-protocol.

pub mod format;
pub mod json;
pub mod players;
pub mod seed;
pub mod sets;
pub mod stream;

pub use players::{Channel, PlayerStreams};
pub use seed::ShowdownSeed;
pub use stream::{BattleStream, Message, ReplayMode, StreamError, StreamOptions};

/// The README's examples, run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
