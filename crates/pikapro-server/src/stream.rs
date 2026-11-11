//! Showdown's `BattleStream` (`sim/battle-stream.ts`) on pikapro.
//!
//! Write command lines with [`BattleStream::write`]; read what Showdown's
//! stream would push with [`BattleStream::read`], one [`Message`] per
//! object Showdown pushes:
//!
//! ```text
//! update             sideupdate         end                requesteddata
//! |t:|1700000000     p1                 {"winner":...}     <input log>
//! |gametype|doubles  |request|{...}
//! ```
//!
//! A write is processed as Showdown's `_write` does: each line starting
//! with `>` is a command, run in order; then the battle's `sendUpdates`
//! sends the log written since the last update (with its `|split|pN`
//! blocks) and, after a new decision point, every player's `|request|`.
//! Choice errors (`|error|[Invalid choice] ...`) and re-sent requests go
//! out at once, as Showdown sends them from `side.choose`. After `end`, the
//! stream ends (unless [`StreamOptions::keep_alive`]) and further output
//! is dropped, as Showdown drops pushes after `pushEnd`.
//!
//! Choices are held by the stream until every player with a request has
//! made one, then submitted to the battle together, as Showdown commits
//! them: a player may change or `undo` its choice until then (every
//! format here has Showdown's Cancel Mod), except where a hidden trap or
//! disable makes Showdown refuse (`cantUndo`).
//!
//! # Commands
//!
//! Supported: `>start`, `>player`, `>p1`/`>p2` choices and `undo`,
//! `>forcewin`, `>forcetie`, `>forcelose`, `>tiebreak`, `>reseed`,
//! `>chat`, `>chat-inputlogonly`, `>requestlog`, `>requestexport`,
//! `>requestteam`, `>show-openteamsheets`, `>version`, `>version-origin`.
//! Not supported, and reported as errors: `>eval` and `>editbattle` (they
//! run arbitrary JavaScript or edit Showdown's objects), `>p3` and `>p4`
//! (two-player formats only). Lines not starting with `>` are comments.
//!
//! Where Showdown throws (an unknown command, bad JSON, a choice before
//! `>start`), [`BattleStream::write`] returns a [`StreamError`] and the
//! rest of the write is skipped, as Showdown's stream stops at the
//! exception; `pokemon-showdown simulate-battle` exits on it.
//!
//! # Differences from Showdown
//!
//! - Formats: only those in [`crate::format`]; Showdown plays any format.
//!   A team is required, except in the random battles: there a `>player`
//!   without one plays the team Showdown's generator draws from its `seed`
//!   (`pikapro_teams::random_battle`), or from a fresh `sodium` seed
//!   written into its options and the input log, as Showdown does. Showdown
//!   also generates teams in other formats; pikapro doesn't.
//! - `|debug|` lines: Showdown writes them in debug formats (the custom
//!   games); pikapro does not. `>start`'s `debug` option is ignored.
//!   Showdown also counts them towards the 500 unsent lines after which it
//!   sends an update in the middle of a write (`commitChoices`), so in a
//!   custom game a write spanning many turns may be split elsewhere.
//! - Exception messages are Showdown's where they are its own (`Unrecognized
//!   command`, `Player p1 already has a team!`, the `TypeError`s of
//!   commands before the battle); a JSON syntax error's is not V8's.
//! - `|t:|` shows the stream's clock ([`StreamOptions::clock`]).
//! - The engine checks a team's structure (at most six Pokémon, levels up
//!   to 100, EV and Stat Point limits, no repeated move) where Showdown's
//!   simulator takes anything: such a team is an error.

use std::collections::VecDeque;
use std::fmt;

use pikapro::event::BattleEvent;
use pikapro::request::RequestKind;
use pikapro::{Battle, BattleOptions, BattleRng, Choice, Gender, SideId, SlotAction};
use pikapro_data::GenderRule;
use pikapro_protocol::{
    ChoiceFlags, ChoiceMessage, Perspective, Renderer, RequestUpdate, TeamSheet, side_id, unpack_team,
};

use crate::format::{self, StreamFormat};
use crate::json::Json;
use crate::seed::ShowdownSeed;
use crate::sets;
use pikapro_teams::random_battle::{self, RandomBattle};

/// `Battle.getTeam` without a team: the format's generator draws one
/// (`Teams.getGenerator(format, seed).getTeam()`) from the player's
/// `seed`, or from a fresh one (`PRNG.generateSeed()`) that it writes into
/// the options, so the input log replays the same team.
fn generate_team(generator: RandomBattle, options: &mut Json) -> Result<Vec<Json>, StreamError> {
    let seed = match options.get("seed").filter(|s| s.truthy()) {
        Some(value) => ShowdownSeed::from_option(Some(value)).map_err(StreamError::new)?,
        None => {
            let seed = crate::seed::generate();
            options.set("seed", Json::str(seed.text.clone()));
            seed
        }
    };
    let mut rng = BattleRng::new(seed.seed);
    let sets = random_battle::generate(generator, &mut rng).map_err(|e| StreamError::new(e.message))?;
    Ok(sets.iter().map(sets::generated).collect())
}

/// One object Showdown's `BattleStream` pushes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// `update\n<log lines>`: every channel's lines, `|split|pN` blocks
    /// included.
    Update(String),
    /// `sideupdate\n<pN>\n<lines>`: a player's `|request|` or `|error|`.
    SideUpdate { side: SideId, data: String },
    /// `end\n<JSON>`: the battle's log data (winner, seed, turns, players,
    /// teams, score, input log).
    End(String),
    /// `requesteddata\n<data>`: the answer to `>requestlog`,
    /// `>requestexport` or `>requestteam`.
    RequestedData(String),
    /// Replay mode ([`StreamOptions::replay`]): one channel's lines of an
    /// update, pushed without a type.
    Replay(String),
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Message::Update(data) => write!(f, "update\n{data}"),
            Message::SideUpdate { side, data } => write!(f, "sideupdate\n{}\n{data}", side_id(*side)),
            Message::End(data) => write!(f, "end\n{data}"),
            Message::RequestedData(data) => write!(f, "requesteddata\n{data}"),
            Message::Replay(data) => f.write_str(data),
        }
    }
}

/// An exception Showdown's stream would throw: the write stops there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamError {
    pub message: String,
}

impl StreamError {
    fn new(message: impl Into<String>) -> StreamError {
        StreamError { message: message.into() }
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StreamError {}

/// `simulate-battle --replay` / `--spectate`: only updates, as one
/// channel's lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayMode {
    Omniscient,
    Spectator,
}

/// Options of a [`BattleStream`] (Showdown's `BattleStreamOptions`).
#[derive(Clone, Copy, Debug)]
pub struct StreamOptions {
    /// Keep the stream open after the battle ends.
    pub keep_alive: bool,
    /// Push only updates, as one channel's lines.
    pub replay: Option<ReplayMode>,
    /// The time `|t:|` lines show, in seconds since the epoch.
    pub clock: fn() -> u64,
}

impl Default for StreamOptions {
    fn default() -> StreamOptions {
        StreamOptions { keep_alive: false, replay: None, clock: system_clock }
    }
}

fn system_clock() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Showdown's `BattleStream`, over a pikapro [`Battle`].
#[derive(Debug)]
pub struct BattleStream {
    options: StreamOptions,
    game: Option<Box<Game>>,
    out: VecDeque<Message>,
    /// Showdown's `pushEnd`: nothing more is pushed.
    closed: bool,
}

impl Default for BattleStream {
    fn default() -> BattleStream {
        BattleStream::new(StreamOptions::default())
    }
}

impl BattleStream {
    pub fn new(options: StreamOptions) -> BattleStream {
        BattleStream { options, game: None, out: VecDeque::new(), closed: false }
    }

    /// Writes a chunk of command lines (Showdown's `stream.write`): each
    /// line starting with `>` is run, then updates are sent. On an error
    /// the rest of the chunk is skipped and nothing more is sent for it.
    pub fn write(&mut self, chunk: &str) -> Result<(), StreamError> {
        for line in chunk.split('\n') {
            if let Some(command) = line.strip_prefix('>') {
                let (kind, message) = command.split_once(' ').unwrap_or((command, ""));
                self.write_line(kind, message)?;
            }
        }
        self.send_updates();
        Ok(())
    }

    /// Ends the input (Showdown's `writeEnd`): the stream ends.
    pub fn write_end(&mut self) {
        self.closed = true;
    }

    /// The next message, if any.
    pub fn read(&mut self) -> Option<Message> {
        self.out.pop_front()
    }

    /// Every message not read yet.
    pub fn drain(&mut self) -> Vec<Message> {
        self.out.drain(..).collect()
    }

    /// Whether the stream has ended (after `end`, or [`write_end`]):
    /// nothing more will be pushed.
    ///
    /// [`write_end`]: BattleStream::write_end
    pub fn is_ended(&self) -> bool {
        self.closed
    }

    /// The battle, once both players have joined.
    pub fn battle(&self) -> Option<&Battle> {
        self.game.as_ref()?.battle.as_ref()
    }

    /// Whether the battle is over (it ended, or was ended by `>forcewin`,
    /// `>forcetie`, `>forcelose` or `>tiebreak`).
    pub fn battle_ended(&self) -> bool {
        self.game.as_ref().is_some_and(|g| g.ended)
    }

    /// The winner's name once the battle is over (`""` for a tie).
    pub fn winner(&self) -> Option<&str> {
        self.game.as_ref().filter(|g| g.ended).map(|g| g.winner.as_str())
    }

    /// Whether `side` has a decision to make: a request, and no complete
    /// choice yet.
    pub fn waiting_for(&self, side: SideId) -> bool {
        self.game.as_ref().is_some_and(|g| {
            let s = &g.sides[side.index()];
            !s.state.is_empty()
                && s.choice.is_none()
                && g.battle.as_ref().is_some_and(|b| b.request(side) != pikapro::Request::Wait)
        })
    }

    /// The request `side` was last sent (`side.activeRequest`, updates
    /// included), as the JSON text after `|request|`: `None` before the
    /// battle and after it ends.
    pub fn request(&self, side: SideId) -> Option<&str> {
        self.game.as_ref()?.sides[side.index()].request.as_deref()
    }

    /// Showdown's `side.requestState`: `"teampreview"`, `"move"` or
    /// `"switch"` while `side` has a request to answer (made or not), `""`
    /// when it waits, before the battle and after it.
    pub fn request_state(&self, side: SideId) -> &'static str {
        self.game.as_ref().map_or("", |g| g.sides[side.index()].state)
    }

    /// Showdown's `battle.inputLog`.
    pub fn input_log(&self) -> &[String] {
        self.game.as_ref().map_or(&[], |g| &g.input_log)
    }

    fn push(&mut self, message: Message) {
        if self.closed {
            return;
        }
        // `pushMessage`: in replay mode only updates, as one channel.
        let message = match (self.options.replay, message) {
            (None, m) | (Some(_), m @ Message::RequestedData(_)) => m,
            (Some(mode), Message::Update(data)) => {
                let perspective = match mode {
                    ReplayMode::Omniscient => Perspective::Omniscient,
                    ReplayMode::Spectator => Perspective::Spectator,
                };
                Message::Replay(extract(&data, perspective).join("\n"))
            }
            (Some(_), Message::End(_)) => {
                if !self.options.keep_alive {
                    self.closed = true;
                }
                return;
            }
            (Some(_), _) => return,
        };
        let end = matches!(message, Message::End(_));
        self.out.push_back(message);
        if end && !self.options.keep_alive {
            self.closed = true;
        }
    }

    fn game(&mut self, what: &str) -> Result<&mut Game, StreamError> {
        match &mut self.game {
            Some(g) => Ok(g),
            None => Err(StreamError::new(format!("Cannot read properties of null (reading '{what}')"))),
        }
    }

    /// `_writeLine`.
    fn write_line(&mut self, kind: &str, message: &str) -> Result<(), StreamError> {
        let clock = self.options.clock;
        let mut out = Vec::new();
        let result = match kind {
            "start" => {
                let options = parse_json(message)?;
                Game::new(options, clock).map(|g| self.game = Some(Box::new(g)))
            }
            "player" => {
                let (slot, text) = message.split_once(' ').unwrap_or((message, ""));
                let game = self.game("setPlayer")?;
                let options = parse_json(text)?;
                game.set_player(slot, options)
            }
            "p1" | "p2" => {
                let side = if kind == "p1" { SideId::P1 } else { SideId::P2 };
                let game = self.game(if message == "undo" { "undoChoice" } else { "choose" })?;
                if message == "undo" { game.undo(side, &mut out) } else { game.choose(side, message, &mut out) }
            }
            "p3" | "p4" => Err(StreamError::new("Cannot read properties of undefined (reading 'choose')")),
            "forcewin" | "forcetie" => {
                let game = self.game("win")?;
                let side = if kind == "forcewin" { game.get_side(message) } else { None };
                game.win(side);
                let entry = if message.is_empty() { ">forcetie".to_string() } else { format!(">forcewin {message}") };
                game.input_log.push(entry);
                Ok(())
            }
            "forcelose" => {
                let game = self.game("lose")?;
                if let Some(side) = game.get_side(message) {
                    // `win(side.foe)`: no foe before the battle starts.
                    let foe = SideId::BOTH[1 - side.index()];
                    game.win(game.started.then_some(foe));
                }
                game.input_log.push(format!(">forcelose {message}"));
                Ok(())
            }
            "reseed" => self.game("resetRNG")?.reseed(message),
            "tiebreak" => self.game("tiebreak")?.tiebreak(),
            "chat-inputlogonly" => {
                self.game("inputLog")?.input_log.push(format!(">chat {message}"));
                Ok(())
            }
            "chat" => {
                let game = self.game("inputLog")?;
                game.input_log.push(format!(">chat {message}"));
                game.log.push(format!("|chat|{message}"));
                Ok(())
            }
            "requestlog" => {
                let game = self.game("inputLog")?;
                out.push(Message::RequestedData(game.input_log.join("\n")));
                Ok(())
            }
            "requestexport" => {
                let game = self.game("prngSeed")?;
                out.push(Message::RequestedData(format!("{}\n{}", game.seed_text, game.input_log.join("\n"))));
                Ok(())
            }
            "requestteam" => {
                let slot = message.trim_matches(crate::json::is_js_whitespace);
                // `message.slice(1)`: after the first UTF-16 unit.
                let rest: Vec<u16> = slot.encode_utf16().skip(1).collect();
                let n = crate::json::js_parse_int(&String::from_utf16_lossy(&rest), 0);
                match n.map(|n| n - 1.0) {
                    Some(n) if n >= 0.0 => {
                        let game = self.game("sides")?;
                        // `battle.sides[n]`: null for a player not joined yet,
                        // undefined past the format's players.
                        match (n < 2.0).then(|| game.players[n as usize].as_ref()) {
                            Some(Some(p)) => {
                                let team = sets::pack(&p.sets).map_err(StreamError::new)?;
                                out.push(Message::RequestedData(team));
                                Ok(())
                            }
                            Some(None) => Err(StreamError::new("Cannot read properties of null (reading 'team')")),
                            None => Err(StreamError::new("Cannot read properties of undefined (reading 'team')")),
                        }
                    }
                    _ => {
                        Err(StreamError::new(format!("Team requested for slot {slot}, but that slot does not exist.")))
                    }
                }
            }
            "version" | "version-origin" => Ok(()),
            "show-openteamsheets" => self.game("showOpenTeamSheets")?.show_open_team_sheets(),
            "eval" | "editbattle" => Err(StreamError::new(format!("pikapro-server: >{kind} is not supported"))),
            _ => Err(StreamError::new(format!("Unrecognized command \">{kind} {message}\""))),
        };
        for m in out {
            self.push(m);
        }
        // `commitChoices`: `if (this.log.length - this.sentLogPos > 500) this.sendUpdates()`.
        if self.game.as_mut().is_some_and(|g| std::mem::take(&mut g.flush)) {
            self.send_updates();
        }
        result
    }

    /// `battle.sendUpdates()`.
    fn send_updates(&mut self) {
        let Some(game) = &mut self.game else { return };
        let mut out = Vec::new();
        game.send_updates(&mut out);
        for m in out {
            self.push(m);
        }
    }
}

fn parse_json(text: &str) -> Result<Json, StreamError> {
    Json::parse(text).map_err(StreamError::new)
}

/// A line of an update as Showdown's `extractChannelMessages` reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateLine<'a> {
    /// A line every channel reads.
    Shared(&'a str),
    /// A `|split|pN` block: player `N` (1 to 4) and the omniscient channel
    /// read `secret`, everyone else `shared`. An empty line is not sent.
    Split { player: u8, secret: &'a str, shared: &'a str },
}

impl<'a> UpdateLine<'a> {
    /// The line `perspective` reads, if any.
    pub fn for_perspective(self, perspective: Perspective) -> Option<&'a str> {
        match self {
            UpdateLine::Shared(line) => Some(line),
            UpdateLine::Split { player, secret, shared } => {
                let sees = match perspective {
                    Perspective::Omniscient => true,
                    Perspective::Player(side) => side.index() + 1 == usize::from(player),
                    Perspective::Spectator => false,
                };
                let line = if sees { secret } else { shared };
                (!line.is_empty()).then_some(line)
            }
        }
    }
}

/// The length of the line terminator at `i` (`\n`, `\r`, U+2028 or
/// U+2029, where a JavaScript `.` stops), or 0.
fn terminator_at(bytes: &[u8], i: usize) -> usize {
    match bytes.get(i) {
        Some(b'\n' | b'\r') => 1,
        Some(0xE2) if bytes.get(i + 1) == Some(&0x80) && matches!(bytes.get(i + 2), Some(0xA8 | 0xA9)) => 3,
        _ => 0,
    }
}

/// Where the line starting at `from` ends: the next terminator, or the end.
fn line_end(bytes: &[u8], from: usize) -> usize {
    (from..bytes.len()).find(|&k| terminator_at(bytes, k) > 0).unwrap_or(bytes.len())
}

/// An update's lines as Showdown's `extractChannelMessages` matches them:
/// `/^\|split\|p([1234])\n(.*)\n(.*)|.+/gm`, where `.` stops at any line
/// terminator (`\n`, `\r`, U+2028, U+2029) and `^` matches after one.
pub fn update_lines(data: &str) -> Vec<UpdateLine<'_>> {
    const SPLIT: &[u8] = b"|split|p";
    let bytes = data.as_bytes();
    let mut out = Vec::new();
    let mut p = 0;
    let mut line_start = true;
    while p < bytes.len() {
        let term = terminator_at(bytes, p);
        if term > 0 {
            p += term;
            line_start = true;
            continue;
        }
        if line_start && bytes[p..].starts_with(SPLIT) {
            let q = p + SPLIT.len();
            let digit = bytes.get(q).copied().filter(|d| (b'1'..=b'4').contains(d));
            if let (Some(d), Some(b'\n')) = (digit, bytes.get(q + 1)) {
                let secret_end = line_end(bytes, q + 2);
                if bytes.get(secret_end) == Some(&b'\n') {
                    let shared_end = line_end(bytes, secret_end + 1);
                    out.push(UpdateLine::Split {
                        player: d - b'0',
                        secret: &data[q + 2..secret_end],
                        shared: &data[secret_end + 1..shared_end],
                    });
                    p = shared_end;
                    line_start = false;
                    continue;
                }
            }
        }
        let end = line_end(bytes, p);
        out.push(UpdateLine::Shared(&data[p..end]));
        p = end;
        line_start = false;
    }
    out
}

/// Showdown's `extractChannelMessages` for one channel of an update's
/// text (see [`update_lines`]).
pub fn extract(data: &str, perspective: Perspective) -> Vec<String> {
    update_lines(data).into_iter().filter_map(|l| l.for_perspective(perspective)).map(String::from).collect()
}

/// A player's side (Showdown's `Side` before and during the battle).
#[derive(Debug)]
struct Player {
    name: String,
    avatar: String,
    /// `side.team`: Showdown's set objects, as the `Pokemon` constructor
    /// left them.
    sets: Vec<Json>,
    /// The team the engine plays, with the genders drawn when the player
    /// joined.
    sheet: TeamSheet,
}

/// What Showdown keeps per side between requests: the active request
/// (`side.activeRequest`) and the choice being made (`side.choice`).
#[derive(Debug, Default)]
struct SideChoice {
    /// The request sent, as its JSON text (Showdown updates it in place
    /// when a choice error reveals something: see `edit_request`); `None`
    /// for `null` (no request: before the battle, after it ended).
    request: Option<String>,
    /// `side.requestState`: the request's kind, `""` for a wait or none.
    state: &'static str,
    /// A complete choice, waiting for the other player's.
    choice: Option<(Choice, String)>,
    cant_undo: bool,
    /// The move actions of the choice (partial ones too), for `undo`.
    move_slots: Vec<u8>,
    /// `pokemon.maybeLocked` / `pokemon.maybeDisabled` of each slot.
    flags: ChoiceFlags,
}

impl SideChoice {
    /// No request (`null`): the battle is over.
    fn end(&mut self) {
        self.request = None;
        self.state = "";
    }

    fn clear(&mut self) {
        self.choice = None;
        self.cant_undo = false;
        self.move_slots.clear();
    }
}

/// Showdown's `Battle`, as far as the stream sees it.
#[derive(Debug)]
struct Game {
    format: StreamFormat,
    clock: fn() -> u64,
    /// `prngSeed`: the seed as given.
    seed_text: String,
    /// `battle.prng` until the battle starts: the players' genders are
    /// drawn from it as they join, and the battle continues it.
    rng: BattleRng,
    rated: Option<Json>,
    strict_choices: bool,
    players: [Option<Player>; 2],
    /// Which player joined first.
    first: Option<SideId>,
    renderer: Renderer,
    battle: Option<Battle>,
    started: bool,
    /// `log.slice(sentLogPos)`.
    log: Vec<String>,
    sent_requests: bool,
    sent_end: bool,
    ended: bool,
    winner: String,
    input_log: Vec<String>,
    sides: [SideChoice; 2],
    /// `commitChoices` asked for `sendUpdates` (more than 500 lines).
    flush: bool,
}

impl Game {
    /// `new Battle(options)`.
    fn new(options: Json, clock: fn() -> u64) -> Result<Game, StreamError> {
        let formatid = options.get("formatid").cloned();
        let name = match &formatid {
            Some(Json::String(s)) => s.clone(),
            None => String::new(),
            Some(v) => v.to_js_string(),
        };
        let format =
            format::resolve(&name).ok_or_else(|| StreamError::new(format!("Unrecognized format \"{name}\"")))?;
        let seed = ShowdownSeed::from_option(options.get("seed")).map_err(StreamError::new)?;
        let rated = options.get("rated").filter(|r| r.truthy()).cloned();
        let renderer = Renderer::new(&format.format, Perspective::Omniscient).with_info(format.info.clone());
        let mut game = Game {
            clock,
            seed_text: seed.text.clone(),
            rng: BattleRng::new(seed.seed),
            rated,
            strict_choices: options.get("strictChoices").is_some_and(Json::truthy),
            players: [None, None],
            first: None,
            renderer,
            battle: None,
            started: false,
            log: Vec::new(),
            sent_requests: true,
            sent_end: false,
            ended: false,
            winner: String::new(),
            input_log: Vec::new(),
            sides: Default::default(),
            flush: false,
            format,
        };
        game.log.push(format!("|t:|{}", clock()));
        let mut input = Json::object();
        if let Some(f) = formatid {
            input.set("formatid", f);
        }
        input.set("seed", Json::str(game.seed_text.clone()));
        if let Some(r) = &game.rated {
            input.set("rated", r.clone());
        }
        game.input_log.push(format!(">start {}", input.stringify()));
        game.log.push(format!("|gametype|{}", format::game_type(&game.format.format)));
        for key in ["p1", "p2"] {
            if let Some(p) = options.get(key).filter(|p| p.truthy()) {
                game.set_player(key, p.clone())?;
            }
        }
        for key in ["p3", "p4"] {
            if options.get(key).is_some_and(Json::truthy) {
                return Err(StreamError::new(format!("pikapro-server: {key} is not supported")));
            }
        }
        Ok(game)
    }

    /// `battle.getSide(sideid)`: the digit after the first character.
    fn get_side(&self, text: &str) -> Option<SideId> {
        let second = text.encode_utf16().nth(1)?;
        let side = match char::from_u32(second.into())? {
            '1' => SideId::P1,
            '2' => SideId::P2,
            _ => return None,
        };
        self.players[side.index()].is_some().then_some(side)
    }

    /// `battle.setPlayer(slot, options)`.
    fn set_player(&mut self, slot: &str, mut options: Json) -> Result<(), StreamError> {
        let side = match slot.encode_utf16().nth(1).and_then(|c| char::from_u32(c.into())) {
            Some('1') => SideId::P1,
            Some('2') => SideId::P2,
            _ => return Err(StreamError::new(format!("pikapro-server: player slot {slot} is not supported"))),
        };
        let i = side.index();
        let name_opt = options.get("name").filter(|n| n.truthy()).map(Json::to_js_string);
        let avatar_opt = options.get("avatar").filter(|a| a.truthy()).map(Json::to_js_string);
        let team_value = options.get("team").filter(|t| t.truthy()).cloned();
        if let Some(player) = &mut self.players[i] {
            let mut did = false;
            if let Some(name) = name_opt.filter(|n| *n != player.name) {
                player.name = name;
                did = true;
            }
            if let Some(avatar) = avatar_opt.filter(|a| *a != player.avatar) {
                player.avatar = avatar;
                did = true;
            }
            if team_value.is_some() {
                return Err(StreamError::new(format!("Player {slot} already has a team!")));
            }
            if !did {
                return Ok(());
            }
        } else {
            // `getTeam`: a packed (or JSON) string, or set objects; without
            // one, a random battle's generator draws the team.
            let mut team = match &team_value {
                Some(Json::String(s)) => sets::unpack(s),
                Some(Json::Array(a)) => Some(a.clone()),
                Some(_) => return Err(StreamError::new("team is not iterable")),
                None => None,
            };
            if team.is_none()
                && let Some(generator) = RandomBattle::for_format(self.format.format.id)
            {
                team = Some(generate_team(generator, &mut options)?);
            }
            let mut team = team.ok_or_else(|| {
                StreamError::new(format!(
                    "pikapro-server: {slot} needs a team (random teams are only generated in the random battles)"
                ))
            })?;
            for set in &mut team {
                sets::construct(set, self.format.format.dex).map_err(StreamError::new)?;
            }
            let packed = sets::pack(&team).map_err(StreamError::new)?;
            if let Some(Json::Array(_)) = team_value {
                options.set("team", Json::String(packed.clone()));
            }
            let format = &self.format.format;
            let mut sheet =
                unpack_team(&packed, format).map_err(|e| StreamError::new(format!("pikapro-server: {e}")))?;
            // The `Pokemon` constructor: `genders[set.gender] ||
            // species.gender || sample(['M', 'F'])`, set by set.
            for set in &mut sheet.team.sets {
                if set.gender.is_none() && matches!(format.dex.species(set.species).gender, GenderRule::Ratio { .. }) {
                    set.gender = Some(if self.rng.random(2) == 0 { Gender::Male } else { Gender::Female });
                }
            }
            let name = name_opt.unwrap_or_else(|| format!("Player {}", i + 1));
            self.players[i] = Some(Player { name, avatar: avatar_opt.unwrap_or_default(), sets: team, sheet });
            self.first.get_or_insert(side);
        }
        let player = self.players[i].as_ref().expect("set above");
        let rating = options.get("rating").filter(|r| r.truthy()).map(Json::to_js_string).unwrap_or_default();
        let info = &mut self.renderer.info.players[i];
        info.name = player.name.clone();
        info.avatar = player.avatar.clone();
        info.rating = rating.clone();
        self.input_log.push(format!(">player {slot} {}", options.stringify()));
        if self.players.iter().all(Option::is_some) && !self.started {
            // The second player's line comes with `start`'s header.
            self.start()
        } else {
            self.log.push(format!("|player|{}|{}|{}|{rating}", side_id(side), player.name, player.avatar));
            Ok(())
        }
    }

    /// `battle.start()`, when the second player joins: builds the battle and
    /// renders its first events.
    fn start(&mut self) -> Result<(), StreamError> {
        let format = &self.format.format;
        let sheets: Vec<&TeamSheet> = self.players.iter().flatten().map(|p| &p.sheet).collect();
        let teams = [sheets[0].team.clone(), sheets[1].team.clone()];
        let seed = self.rng.current_seed().expect("Showdown's seeds continue");
        let battle = Battle::new(format.clone(), teams, BattleOptions::new(seed))
            .map_err(|e| StreamError::new(format!("pikapro-server: invalid team: {e}")))?;
        self.renderer.nicknames = [sheets[0].nicknames(format), sheets[1].nicknames(format)];
        self.renderer.shiny =
            [sheets[0].extras.iter().map(|e| e.shiny).collect(), sheets[1].extras.iter().map(|e| e.shiny).collect()];
        self.renderer.pokeballs = [sheets[0].pokeballs(), sheets[1].pokeballs()];
        self.battle = Some(battle);
        self.started = true;
        // The engine's events begin with both `|player|` lines, the header
        // after p2's; the first player's line went out when it joined.
        let events: Vec<BattleEvent> = self.battle.as_mut().expect("set above").drain_events().collect();
        self.renderer.info.timestamp = (self.clock)();
        let mut lines = self.renderer.raw(&events);
        let first = format!("|player|{}|", side_id(self.first.unwrap_or(SideId::P1)));
        if let Some(k) = lines.iter().position(|l| l.starts_with(&first)) {
            lines.remove(k);
        }
        if let Some(rated) = &self.rated {
            let text = match rated {
                Json::String(s) if s != "Rated battle" => s.clone(),
                _ => String::new(),
            };
            if let Some(k) = lines.iter().position(|l| l.starts_with("|tier|")) {
                lines.insert(k + 1, format!("|rated|{text}"));
            }
        }
        self.log.extend(lines);
        self.after_resolution();
        Ok(())
    }

    /// After the battle moved: the battle's end, or the new requests
    /// (`makeRequest`).
    fn after_resolution(&mut self) {
        let battle = self.battle.as_ref().expect("a battle");
        for s in &mut self.sides {
            s.clear();
        }
        if let Some(result) = battle.result() {
            self.ended = true;
            self.winner = result.winner.map_or(String::new(), |side| self.name(side));
            for s in &mut self.sides {
                s.end();
            }
            return;
        }
        for side in SideId::BOTH {
            let data = battle.request_data(side);
            let s = &mut self.sides[side.index()];
            s.request = Some(self.renderer.request_json(&data));
            s.state = match &data.kind {
                RequestKind::Wait => "",
                RequestKind::TeamPreview { .. } => "teampreview",
                RequestKind::ForceSwitch { .. } => "switch",
                RequestKind::Move { .. } => "move",
            };
            s.flags = ChoiceFlags::default();
            if let RequestKind::Move { active } = &data.kind {
                for (slot, a) in active.iter().enumerate() {
                    if let Some(a) = a {
                        s.flags.maybe_locked[slot] = a.maybe_locked;
                        s.flags.maybe_disabled[slot] = a.maybe_disabled;
                    }
                }
            }
        }
        self.sent_requests = false;
    }

    fn name(&self, side: SideId) -> String {
        self.players[side.index()].as_ref().map_or(String::new(), |p| p.name.clone())
    }

    /// Renders the battle's new events into the log.
    fn render(&mut self) {
        let Some(battle) = &mut self.battle else { return };
        let events: Vec<BattleEvent> = battle.drain_events().collect();
        self.renderer.info.timestamp = (self.clock)();
        let lines = self.renderer.raw(&events);
        self.log.extend(lines);
    }

    /// `battle.sendUpdates()`.
    fn send_updates(&mut self, out: &mut Vec<Message>) {
        if self.log.is_empty() {
            return;
        }
        out.push(Message::Update(self.log.join("\n")));
        self.log.clear();
        if !self.sent_requests {
            for side in SideId::BOTH {
                self.emit_request(side, false, out);
            }
            self.sent_requests = true;
        }
        if !self.sent_end && self.ended {
            if let Some(end) = self.end_log() {
                out.push(Message::End(end));
            }
            self.sent_end = true;
        }
    }

    /// The `end` message's JSON.
    fn end_log(&self) -> Option<String> {
        let battle = self.battle.as_ref();
        let (p1, p2) = (self.players[0].as_ref()?, self.players[1].as_ref()?);
        let mut log = Json::object();
        log.set("winner", Json::str(self.winner.clone()));
        log.set("seed", Json::str(self.seed_text.clone()));
        log.set("turns", Json::Number(battle.map_or(0.0, |b| f64::from(b.turn()))));
        log.set("p1", Json::str(p1.name.clone()));
        log.set("p2", Json::str(p2.name.clone()));
        log.set("p1team", Json::Array(p1.sets.clone()));
        log.set("p2team", Json::Array(p2.sets.clone()));
        let score = |side: SideId| -> f64 {
            match battle {
                Some(b) => b.state().side(side).team.iter().filter(|m| !m.is_fainted()).count() as f64,
                None => self.players[side.index()].as_ref().map_or(0.0, |p| p.sets.len() as f64),
            }
        };
        log.set("score", Json::Array(vec![Json::Number(score(SideId::P1)), Json::Number(score(SideId::P2))]));
        log.set("inputLog", Json::Array(self.input_log.iter().map(|l| Json::str(l.clone())).collect()));
        Some(log.stringify())
    }

    /// `side.emitRequest(activeRequest, updated)`.
    fn emit_request(&mut self, side: SideId, updated: bool, out: &mut Vec<Message>) {
        if updated {
            self.edit_request(side, |r| {
                r.set("update", Json::Bool(true));
                true
            });
        }
        let json = self.sides[side.index()].request.as_deref().unwrap_or("null");
        out.push(Message::SideUpdate { side, data: format!("|request|{json}") });
    }

    /// Changes `side`'s active request in place, as Showdown changes the
    /// object: the request is kept as text and parsed only here (after a
    /// choice error or an undo that reveals something). `edit` says
    /// whether it changed anything; the text is rewritten only then.
    fn edit_request(&mut self, side: SideId, edit: impl FnOnce(&mut Json) -> bool) -> bool {
        let request = &mut self.sides[side.index()].request;
        let Some(text) = request.as_deref() else { return false };
        let mut json = Json::parse(text).expect("a request's JSON parses");
        let changed = edit(&mut json);
        if changed {
            *request = Some(json.stringify());
        }
        changed
    }

    /// `emitChoiceError`'s line.
    fn choice_error(&self, side: SideId, unavailable: bool, message: &str, out: &mut Vec<Message>) -> String {
        let kind = if unavailable { "Unavailable" } else { "Invalid" };
        let text = format!("[{kind} choice] {message}");
        out.push(Message::SideUpdate { side, data: format!("|error|{text}") });
        text
    }

    fn strict(&self, text: String) -> Result<(), StreamError> {
        if self.strict_choices { Err(StreamError::new(text)) } else { Ok(()) }
    }

    /// `battle.choose(side, input)`.
    fn choose(&mut self, side: SideId, input: &str, out: &mut Vec<Message>) -> Result<(), StreamError> {
        let i = side.index();
        if self.players[i].is_none() {
            return Err(StreamError::new("Cannot read properties of null (reading 'choose')"));
        }
        if self.sides[i].state.is_empty() {
            let why = if self.ended { "The game is over" } else { "It's not your turn" };
            let text = self.choice_error(side, false, &format!("Can't do anything: {why}"), out);
            return self.strict(text);
        }
        if self.sides[i].cant_undo {
            let text = self.choice_error(
                side,
                false,
                "Can't undo: A trapping/disabling effect would cause undo to leak information",
                out,
            );
            return self.strict(text);
        }
        self.sides[i].clear();
        // `getMoveRequestData` again: `maybeLocked ||= maybeDisabled`.
        let flags = &mut self.sides[i].flags;
        for slot in 0..flags.maybe_locked.len() {
            flags.maybe_locked[slot] |= flags.maybe_disabled[slot];
        }
        let flags = *flags;
        let battle = self.battle.as_ref().expect("a request means a battle");
        let chosen = pikapro_protocol::choose(battle, side, input, &self.renderer.nicknames[i], Some(flags));
        for message in &chosen.messages {
            match message {
                ChoiceMessage::Error { message, update } => {
                    let updated = update.is_some_and(|u| self.apply_update(side, u));
                    let text = self.choice_error(side, updated, message, out);
                    if updated {
                        self.emit_request(side, true, out);
                    }
                    self.strict(text)?;
                }
                ChoiceMessage::Resend { slot } => {
                    self.update_disabled_request(side, *slot as usize);
                    self.emit_request(side, true, out);
                }
            }
        }
        let s = &mut self.sides[i];
        s.cant_undo = chosen.cant_undo;
        s.move_slots = chosen.move_slots.clone();
        if let Some(choice) = chosen.done {
            s.choice = Some((choice, chosen.input_log.clone()));
        }
        if chosen.result.is_ok() && self.all_choices_done() {
            self.commit();
        }
        Ok(())
    }

    /// `allChoicesDone`.
    fn all_choices_done(&mut self) -> bool {
        let battle = self.battle.as_ref().expect("a request means a battle");
        for side in SideId::BOTH {
            let s = &mut self.sides[side.index()];
            // Showdown's isChoiceDone fills implicit passes even though its
            // wire request still contains `active`, not `wait: true`.
            if s.state == "move" && s.choice.is_none() && battle.request(side) == pikapro::Request::Wait {
                s.choice = Some((
                    Choice::Actions([SlotAction::Pass; pikapro::MAX_ACTIVE]),
                    vec!["pass"; battle.format().active_per_side()].join(", "),
                ));
            }
        }
        self.sides.iter().all(|s| s.state.is_empty() || s.choice.is_some())
    }

    /// `commitChoices`: logs and submits both choices, then renders what
    /// happened.
    ///
    /// The battle accepting both is an invariant, not an error: each
    /// choice was checked against this request by `pikapro_protocol::choose`,
    /// as Showdown's sides check the choice objects they then commit
    /// (Showdown has no way to refuse one here). A refusal is a bug
    /// between the engine and the protocol layer that would leave the
    /// battle half-submitted, so it panics: `pikapro-showdown-server` ends
    /// that room as a crashed battle, `pikapro-simulate` exits.
    fn commit(&mut self) {
        for side in SideId::BOTH {
            if let Some((_, text)) = &self.sides[side.index()].choice
                && !text.is_empty()
            {
                self.input_log.push(format!(">{} {text}", side_id(side)));
            }
        }
        let choices = [self.sides[0].choice.take(), self.sides[1].choice.take()];
        let battle = self.battle.as_mut().expect("a request means a battle");
        // Snapshot before submitting: an actionable side can resolve the turn
        // alone, so the other side's implicit passes must not enter the next turn.
        let requests = SideId::BOTH.map(|side| battle.request(side));
        for (side, choice) in SideId::BOTH.into_iter().zip(choices) {
            if requests[side.index()] != pikapro::Request::Wait
                && let Some((choice, text)) = choice
                && let Err(e) = battle.submit(side, choice)
            {
                panic!("pikapro-server: the battle refused {} {text}, a choice its request allows: {e}", side_id(side));
            }
        }
        self.render();
        self.after_resolution();
        if self.log.len() > 500 {
            self.flush = true;
        }
    }

    /// `battle.undoChoice(side)`.
    fn undo(&mut self, side: SideId, out: &mut Vec<Message>) -> Result<(), StreamError> {
        let i = side.index();
        if self.players[i].is_none() {
            return Err(StreamError::new("Cannot read properties of null (reading 'requestState')"));
        }
        let state = self.sides[i].state;
        if state.is_empty() {
            return Ok(());
        }
        if self.sides[i].cant_undo {
            let text = self.choice_error(
                side,
                false,
                "Can't undo: A trapping/disabling effect would cause undo to leak information",
                out,
            );
            return self.strict(text);
        }
        let mut updated = false;
        if state == "move" {
            for slot in self.sides[i].move_slots.clone() {
                updated |= self.update_disabled_request(side, slot as usize);
            }
        }
        self.sides[i].clear();
        if updated {
            self.emit_request(side, true, out);
        }
        Ok(())
    }

    /// Tries a rejected choice's update on the active request
    /// (`updateRequestForPokemon`): whether it changed anything.
    fn apply_update(&mut self, side: SideId, update: RequestUpdate) -> bool {
        match update {
            RequestUpdate::Disabled { slot, move_index } => {
                let updated = self.update_disabled_request(side, slot as usize);
                let marked = self.edit_request(side, |request| {
                    let Some(m) = active_entry(request, slot as usize)
                        .and_then(|req| req.get_mut("moves"))
                        .and_then(Json::as_array_mut)
                        .and_then(|m| m.get_mut(move_index as usize))
                    else {
                        return false;
                    };
                    let mut updated = false;
                    if !m.get("disabled").is_some_and(Json::truthy) {
                        m.set("disabled", Json::Bool(true));
                        updated = true;
                    }
                    if m.get("disabledSource") != Some(&Json::str("")) {
                        m.set("disabledSource", Json::str(""));
                        updated = true;
                    }
                    updated
                });
                updated | marked
            }
            RequestUpdate::Trapped { slot } => self.edit_request(side, |request| {
                let Some(req) = active_entry(request, slot as usize) else { return false };
                let mut updated = false;
                if req.get("maybeTrapped").is_some_and(Json::truthy) {
                    req.remove("maybeTrapped");
                    updated = true;
                }
                if !req.get("trapped").is_some_and(Json::truthy) {
                    req.set("trapped", Json::Bool(true));
                    updated = true;
                }
                updated
            }),
        }
    }

    /// `side.updateDisabledRequest(pokemon, req)`.
    fn update_disabled_request(&mut self, side: SideId, slot: usize) -> bool {
        let doubles = self.format.format.active_per_side() > 1;
        // `pokemon.getMoveData(m.id)?.disabled`: the move is not usable
        // now (a disable the request may have hidden).
        let disabled: Vec<(String, bool)> = match &self.battle {
            Some(battle) => {
                let legal = battle.legal_actions(side);
                let mon = battle.state().side(side).active(slot);
                let usable = mon.is_some_and(|m| m.has_usable_move());
                match mon {
                    Some(m) => m
                        .moves
                        .iter()
                        .enumerate()
                        .map(|(k, ms)| {
                            let enabled = legal
                                .slot(slot)
                                .iter()
                                .any(|a| matches!(a, SlotAction::Move { index, .. } if *index as usize == k));
                            (format::to_id(battle.format().dex.move_(ms.id).name), !usable || !enabled)
                        })
                        .collect(),
                    None => Vec::new(),
                }
            }
            None => Vec::new(),
        };
        let flags = &mut self.sides[side.index()].flags;
        let (locked, maybe_disabled) = (flags.maybe_locked[slot], flags.maybe_disabled[slot]);
        flags.maybe_locked[slot] = false;
        if doubles {
            flags.maybe_disabled[slot] = false;
        }
        self.edit_request(side, |request| {
            let Some(req) = active_entry(request, slot) else { return false };
            let mut updated = false;
            if locked {
                req.remove("maybeLocked");
                updated = true;
            }
            if maybe_disabled && doubles {
                req.remove("maybeDisabled");
                updated = true;
                if let Some(moves) = req.get_mut("moves").and_then(Json::as_array_mut) {
                    for m in moves {
                        let id = m.get("id").and_then(Json::as_str).unwrap_or("").to_string();
                        if disabled.iter().any(|(mid, d)| *d && *mid == id) {
                            m.set("disabled", Json::Bool(true));
                            updated = true;
                        }
                    }
                }
            }
            let all_disabled = req.get("moves").and_then(Json::as_array).is_some_and(|moves| {
                moves
                    .iter()
                    .all(|m| m.get("disabled").is_some_and(Json::truthy) || m.get("id") == Some(&Json::str("struggle")))
            });
            if all_disabled {
                for key in ["canMegaEvo", "canMegaEvoX", "canMegaEvoY", "canUltraBurst"] {
                    if req.get(key).is_some_and(Json::truthy) {
                        req.set(key, Json::Bool(false));
                        updated = true;
                    }
                }
                if req.get("canZMove").is_some_and(Json::truthy) {
                    req.remove("canZMove");
                    updated = true;
                }
                if req.get("canDynamax").is_some_and(Json::truthy) {
                    req.set("canDynamax", Json::Bool(false));
                    req.remove("maxMoves");
                    updated = true;
                }
                if req.get("canTerastallize").is_some_and(Json::truthy) {
                    req.remove("canTerastallize");
                    updated = true;
                }
            }
            updated
        })
    }

    /// `battle.win(side)`.
    fn win(&mut self, side: Option<SideId>) {
        if self.ended {
            return;
        }
        self.winner = side.map_or(String::new(), |s| self.name(s));
        self.log.push("|".into());
        match side {
            Some(s) => {
                let line = format!("|win|{}", self.name(s));
                self.log.push(line);
            }
            None => self.log.push("|tie".into()),
        }
        self.ended = true;
        for s in &mut self.sides {
            s.end();
        }
    }

    /// `battle.resetRNG(seed)`.
    fn reseed(&mut self, text: &str) -> Result<(), StreamError> {
        let seed = ShowdownSeed::parse(text).map_err(StreamError::new)?;
        match &mut self.battle {
            Some(battle) => *battle = battle.fork(seed.seed),
            None => self.rng = BattleRng::new(seed.seed),
        }
        self.log.push("|message|The battle's RNG was reset.".into());
        self.input_log.push(format!(">reseed {}", seed.current()));
        Ok(())
    }

    /// `battle.showOpenTeamSheets()`: at turn 0 (team preview), each
    /// side's sets as the other player may see them, packed.
    fn show_open_team_sheets(&mut self) -> Result<(), StreamError> {
        let Some(battle) = &self.battle else {
            // `this.turn` is 0 until the battle starts; a missing side throws.
            return Err(StreamError::new("Cannot read properties of null (reading 'pokemon')"));
        };
        if battle.turn() != 0 {
            return Ok(());
        }
        let champions = self.format.format.mechanics == pikapro::Mechanics::Champions;
        let dex = self.format.format.dex;
        let mut lines = Vec::new();
        for side in SideId::BOTH {
            let player = self.players[side.index()].as_mut().expect("both players in a battle");
            // Zacian holding a Rusted Sword and Zamazenta holding a Rusted
            // Shield show as their Crowned formes, Iron Head as the
            // signature move, "so the client doesn't flag [them] as
            // illusions". The sheet shares `set.moves` with the set, so the
            // set's moves change too (the `end` log and `>requestteam`
            // show the signature move from then on).
            let crowned: Vec<Option<&'static str>> = player.sets.iter_mut().map(crown).collect();
            let player = &*player;
            let mons = &battle.state().side(side).team;
            let team: Vec<Json> = player
                .sets
                .iter()
                .zip(&player.sheet.team.sets)
                .zip(crowned)
                .enumerate()
                .map(|(i, ((set, typed), crowned))| {
                    // `pokemon.gender`: the set's, the species' only one, or
                    // the one the battle drew.
                    let drawn = mons.iter().find(|m| usize::from(m.id) == i).map(|m| m.gender);
                    let gender = drawn.or(typed.gender).or(match dex.species(typed.species).gender {
                        GenderRule::Male => Some(Gender::Male),
                        GenderRule::Female => Some(Gender::Female),
                        _ => None,
                    });
                    let gender = match gender {
                        Some(g @ (Gender::Male | Gender::Female)) => g.letter(),
                        _ => "",
                    };
                    let mut open = Json::object();
                    open.set("name", Json::str(""));
                    for key in ["species", "item", "ability", "moves"] {
                        if let Some(v) = set.get(key) {
                            open.set(key, v.clone());
                        }
                    }
                    let nature = if champions { set.get("nature").cloned() } else { Some(Json::str("")) };
                    if let Some(n) = nature {
                        open.set("nature", n);
                    }
                    open.set("gender", Json::str(gender));
                    open.set("evs", Json::Null);
                    open.set("ivs", Json::Null);
                    if let Some(l) = set.get("level") {
                        open.set("level", l.clone());
                    }
                    // Gen 9 without Terastal Clause: the Tera Type.
                    if let Some(t) = set.get("teraType") {
                        open.set("teraType", t.clone());
                    }
                    if let Some(species) = crowned {
                        open.set("species", Json::str(species));
                    }
                    open
                })
                .collect();
            let packed = sets::pack(&team).map_err(StreamError::new)?;
            lines.push(format!("|showteam|{}|{packed}", side_id(side)));
        }
        self.log.extend(lines);
        Ok(())
    }

    /// `battle.tiebreak()`.
    fn tiebreak(&mut self) -> Result<(), StreamError> {
        if self.ended {
            return Ok(());
        }
        let Some(battle) = &self.battle else {
            return Err(StreamError::new("Cannot read properties of null (reading 'pokemon')"));
        };
        self.input_log.push(">tiebreak".into());
        self.log.push("|message|Time's up! Going to tiebreaker...".into());
        let names = [self.name(SideId::P1), self.name(SideId::P2)];
        let teams = SideId::BOTH.map(|s| battle.state().side(s).team);
        let not_fainted: Vec<usize> = teams.iter().map(|t| t.iter().filter(|m| !m.is_fainted()).count()).collect();
        let line: Vec<String> = (0..2).map(|k| format!("{}: {} Pokemon left", names[k], not_fainted[k])).collect();
        self.log.push(format!("|-message|{}", line.join("; ")));
        let max = *not_fainted.iter().max().expect("two sides");
        let mut tied: Vec<usize> = (0..2).filter(|&k| not_fainted[k] == max).collect();
        if tied.len() <= 1 {
            self.win(tied.first().map(|&k| SideId::BOTH[k]));
            return Ok(());
        }
        // `pokemon.hp / pokemon.maxhp` summed, `* 100 / 6`, as doubles.
        let pct: Vec<f64> = tied
            .iter()
            .map(|&k| {
                let sum: f64 = teams[k].iter().map(|m| f64::from(m.hp) / f64::from(m.max_hp())).sum();
                sum * 100.0 / 6.0
            })
            .collect();
        let line: Vec<String> =
            tied.iter().zip(&pct).map(|(&k, p)| format!("{}: {}% total HP left", names[k], js_round(*p))).collect();
        self.log.push(format!("|-message|{}", line.join("; ")));
        let max = pct.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        tied = tied.iter().zip(&pct).filter(|(_, p)| **p == max).map(|(k, _)| *k).collect();
        if tied.len() <= 1 {
            self.win(tied.first().map(|&k| SideId::BOTH[k]));
            return Ok(());
        }
        let totals: Vec<u32> = tied.iter().map(|&k| teams[k].iter().map(|m| u32::from(m.hp)).sum()).collect();
        let line: Vec<String> =
            tied.iter().zip(&totals).map(|(&k, t)| format!("{}: {t} total HP left", names[k])).collect();
        self.log.push(format!("|-message|{}", line.join("; ")));
        let max = *totals.iter().max().expect("tied sides");
        tied = tied.iter().zip(&totals).filter(|(_, t)| **t == max).map(|(k, _)| *k).collect();
        self.win(if tied.len() <= 1 { tied.first().map(|&k| SideId::BOTH[k]) } else { None });
        Ok(())
    }
}

/// `showOpenTeamSheets`' rewrite of Zacian with a Rusted Sword and
/// Zamazenta with a Rusted Shield: the Crowned forme's name for the sheet
/// (`Dex.species.get(set.species + 'crowned').name`), and the set's first
/// Iron Head becomes the signature move's id, in the set itself.
fn crown(set: &mut Json) -> Option<&'static str> {
    let id = |key: &str, set: &Json| set.get(key).map_or(String::new(), |v| format::to_id(&v.to_js_string()));
    let (species, signature) = match (id("species", set).as_str(), id("item", set).as_str()) {
        ("zacian", "rustedsword") => ("Zacian-Crowned", "behemothblade"),
        ("zamazenta", "rustedshield") => ("Zamazenta-Crowned", "behemothbash"),
        _ => return None,
    };
    let moves = set.get_mut("moves").and_then(Json::as_array_mut);
    if let Some(m) = moves.and_then(|m| m.iter_mut().find(|m| format::to_id(&m.to_js_string()) == "ironhead")) {
        *m = Json::str(signature);
    }
    Some(species)
}

/// `activeRequest.active[slot]`.
fn active_entry(request: &mut Json, slot: usize) -> Option<&mut Json> {
    request.get_mut("active")?.as_array_mut()?.get_mut(slot)
}

/// JavaScript's `Math.round` (halves round up).
fn js_round(x: f64) -> String {
    let r = x.floor();
    let r = if x - r >= 0.5 { r + 1.0 } else { r };
    crate::json::number_to_string(r)
}
