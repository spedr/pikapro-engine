//! The battle stream a room drives: `pikapro-server`'s port of Showdown's
//! `BattleStream`, as Showdown's server runs it (`RoomBattleStream` in
//! `server/room-battle.ts`: kept alive after the battle ends).
//!
//! A room writes the lines Showdown's `RoomBattle` writes to its simulator
//! process and reads back [`Message`]s:
//!
//! | written | meaning |
//! |---|---|
//! | `>start {"formatid":"...","roomid":"...","rated":"...","seed":"..."}` | create the battle |
//! | `>player p1 {"name":"...","avatar":"...","team":"<packed>","rating":1000}` | add a player (or rename one) |
//! | `>p1 move 1, switch 3` / `>p1 undo` / `>p1 default` | a player's choice |
//! | `>forcelose p1`, `>forcetie` | end the battle |
//! | `>show-openteamsheets` | both players accepted Open Team Sheets |
//!
//! | read | meaning |
//! |---|---|
//! | [`Message::Update`] | log lines for everyone, `\|split\|pN` blocks included |
//! | [`Message::SideUpdate`] | a player's `\|request\|` (the room adds `rqid`) or `\|error\|` |
//! | [`Message::End`] | the battle is over (`winner`, ...) |
//!
//! Where Showdown's stream throws, the write returns the error; the room
//! reports it as `RoomBattleStream._write` does (see `room.rs`).

use pikapro::SideId;
use pikapro_server::{BattleStream, Message, StreamError, StreamOptions};

/// What a battle room drives: [`BattleStream`], which is what the server
/// runs ([`room_battle_streams`]). A trait so that tests can put a stream
/// that panics in its place.
pub trait RoomStream: Send {
    /// Writes one or more input lines (each starting with `>`) and does
    /// all the work they cause; the output waits in [`read`](Self::read).
    /// An error is Showdown's exception: the rest of the input is skipped.
    fn write(&mut self, input: &str) -> Result<(), StreamError>;
    /// The next message, if any.
    fn read(&mut self) -> Option<Message>;
    /// Showdown's `side.requestState` (`""` when `side` has nothing to
    /// decide).
    fn request_state(&self, side: SideId) -> &'static str;
}

impl RoomStream for BattleStream {
    fn write(&mut self, input: &str) -> Result<(), StreamError> {
        BattleStream::write(self, input)
    }

    fn read(&mut self) -> Option<Message> {
        BattleStream::read(self)
    }

    fn request_state(&self, side: SideId) -> &'static str {
        BattleStream::request_state(self, side)
    }
}

/// Creates the stream for each new battle room.
pub type StreamFactory = Box<dyn Fn() -> Box<dyn RoomStream> + Send + Sync>;

/// `new RoomBattleStream()`: `super({keepAlive: true})`. Battles without a
/// seed in `>start` get a fresh `sodium` seed, as Showdown's do.
pub fn room_battle_stream() -> BattleStream {
    BattleStream::new(StreamOptions { keep_alive: true, ..StreamOptions::default() })
}

/// The server's stream factory: a [`room_battle_stream`] per battle.
pub fn room_battle_streams() -> StreamFactory {
    Box::new(|| Box::new(room_battle_stream()))
}

/// `RoomBattleStream._write`'s broadcast when the battle throws.
pub const BATTLE_CRASHED: &str =
    "|html|<div class=\"broadcast-red\"><b>The battle crashed</b><br />Don't worry, we're working on fixing it.</div>";
