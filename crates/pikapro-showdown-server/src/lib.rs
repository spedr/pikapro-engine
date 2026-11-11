//! # pikapro-showdown-server
//!
//! A websocket server speaking the subset of Pokémon Showdown's server
//! protocol that [poke-env](https://github.com/hsahovic/poke-env) uses
//! against a local server, with battles simulated by pikapro. Point a
//! poke-env `ServerConfiguration` at it and its players battle unchanged:
//!
//! ```text
//! cargo run --profile release-server -p pikapro-showdown-server -- --port 8000
//! ```
//!
//! What it speaks (Showdown's `server/` is the reference throughout):
//!
//! * connection: `|updateuser|` (guest), `|customgroups|`, `|formats|`,
//!   `|challstr|`;
//! * `/trn NAME,0,` without authentication (a `--no-security` server),
//!   `|nametaken|`;
//! * `/utm` teams, validated on `/search` and `/challenge` as Showdown's
//!   server validates them (`pikapro-validate` for the VGC formats);
//! * `/search`, `/cancelsearch`, `/challenge`, `/cancelchallenge`,
//!   `/accept`, `/reject`, with `|updatesearch|`, the challenge `|pm|`s and
//!   `|popup|` errors; ladder ratings (Showdown's local Elo);
//! * battle rooms (`>battle-FORMAT-N`): `|init|battle`, each player's
//!   channel of the log, `|request|` with `rqid`, `/choose` (`/move`,
//!   `/switch`, `/team`) with or without an rqid, `/undo`, `/timer`,
//!   `/forfeit`, Open Team Sheets, `/join`, `/leave`, `|win|` / `|tie|`,
//!   `|deinit`.
//!
//! [`Hub`] is the server's state, without I/O; [`serve`] runs it on
//! websockets. The formats are in [`formats`]. Rooms drive a
//! [`RoomStream`]: `pikapro-server`'s port of Showdown's `BattleStream`
//! ([`room_battle_streams`]). A panic in a battle ends that room, not the
//! server (build with `--profile release-server`, which unwinds; see the
//! README).

pub mod formats;
mod hub;
mod net;
mod room;
mod stream;
mod users;
mod util;

pub use hub::{Config, Hub, MAX_BATTLES_PER_USER, MAX_ENDED_ROOMS, MAX_ENDED_ROOMS_PER_USER};
pub use net::{Limits, log, serve};
pub use stream::{BATTLE_CRASHED, RoomStream, StreamFactory, room_battle_stream, room_battle_streams};
