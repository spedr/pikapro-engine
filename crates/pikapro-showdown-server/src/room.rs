//! Battle rooms: Showdown's `GameRoom` + `RoomBattle` (`server/room-battle.ts`).
//!
//! A room owns a [`RoomStream`], relays its updates to everyone in the
//! room (each user reading their channel of `|split|` blocks, as
//! Showdown's `Sockets.channelBroadcast` delivers them), numbers and relays
//! requests (`rqid`), turns `/choose` and `/undo` into stream input with
//! Showdown's checks and errors, and runs Open Team Sheets, forfeits and
//! the battle timer.
//!
//! # Crashes
//!
//! Where Showdown's stream throws, the write returns an error, and the
//! room reports it as Showdown's `RoomBattleStream._write` does: the
//! "battle crashed" broadcast and `[Invalid choice] The battle crashed` to
//! each player with a request; the battle goes on (the stream is kept
//! alive).
//!
//! Every call into the stream runs under `catch_unwind`. A panic ends the
//! room's battle, never the server: the room reports it as a battle that
//! threw and then, since a stream that panicked is not used again, as a
//! simulator that went away (`RoomBattle.listen`: `|bigerror|The simulator
//! process crashed...` and `setEnded`: no winner, no rating change). An
//! `end` message that doesn't parse ends the battle the same way
//! (`listen`'s `JSON.parse` throws). Catching panics needs a build that
//! unwinds (the `release-server` profile).

use std::panic::{AssertUnwindSafe, catch_unwind};

use pikapro::SideId;
use pikapro_server::Message;
use pikapro_server::json::Json;
use pikapro_server::stream::{UpdateLine, update_lines};

use crate::formats::ShowdownFormat;
use crate::stream::{BATTLE_CRASHED, RoomStream};
use crate::users::{ConnId, User, UserKey, Users};

/// Where the room's messages go.
#[derive(Debug)]
pub(crate) struct Ctx<'a> {
    pub(crate) users: &'a Users,
    pub(crate) out: &'a mut Vec<(ConnId, String)>,
    /// Milliseconds on the server's monotonic clock.
    pub(crate) now: u64,
}

impl Ctx<'_> {
    pub(crate) fn send_user(&mut self, key: UserKey, message: &str) {
        if let Some(user) = self.users.get(&key) {
            for &conn in &user.conns {
                self.out.push((conn, message.to_string()));
            }
        }
    }
}

/// A line of the room's log.
#[derive(Clone, Debug)]
enum LogLine {
    Shared(String),
    /// A `|split|pN` block: `side` (0 or 1; 2 and 3 for players this
    /// server doesn't have) reads `secret`, everyone else `shared`
    /// (nothing if empty).
    Split {
        side: usize,
        secret: String,
        shared: String,
    },
}

impl LogLine {
    fn for_channel(&self, channel: Option<usize>) -> Option<&str> {
        match self {
            LogLine::Shared(line) => Some(line),
            LogLine::Split { side, secret, shared } => {
                let line = if channel == Some(*side) { secret } else { shared };
                (!line.is_empty()).then_some(line.as_str())
            }
        }
    }
}

/// `request.isWait`: `false`, `true` or `'cantUndo'`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wait {
    No,
    Yes,
    CantUndo,
}

#[derive(Clone, Debug)]
struct PlayerRequest {
    rqid: u32,
    json: String,
    wait: Wait,
    choice: String,
}

impl PlayerRequest {
    fn none() -> PlayerRequest {
        PlayerRequest { rqid: 0, json: String::new(), wait: Wait::CantUndo, choice: String::new() }
    }
}

/// `RoomBattlePlayer`.
#[derive(Clone, Debug)]
pub(crate) struct Player {
    /// The user in the seat.
    pub(crate) user: Option<UserKey>,
    /// The seat's user id (Showdown's `playerTable` key): a rename in an
    /// unrated battle carries it along.
    pub(crate) id: String,
    pub(crate) name: String,
    avatar: String,
    team: String,
    /// The seed of a random battle's generated team.
    team_seed: Option<String>,
    rating: f64,
    request: PlayerRequest,
    wants_open_team_sheets: Option<bool>,
    eliminated: bool,
    /// The renames written to the battle ([`MAX_RENAMES`]).
    renames: u32,
    /// When the player's last connection closed, if they have not come back.
    pub(crate) disconnected_at: Option<u64>,
    seconds_left: i64,
    turn_seconds_left: i64,
}

impl Player {
    pub(crate) fn new(user: &User, team: String, team_seed: Option<String>, rating: f64) -> Player {
        Player {
            user: Some(user.key),
            id: user.id.clone(),
            name: user.name.clone(),
            avatar: user.avatar.clone(),
            team,
            team_seed,
            rating,
            request: PlayerRequest::none(),
            wants_open_team_sheets: None,
            eliminated: false,
            renames: 0,
            disconnected_at: None,
            seconds_left: 1,
            turn_seconds_left: 1,
        }
    }
}

/// Showdown's `GameTimerSettings`.
#[derive(Clone, Copy, Debug)]
struct TimerSettings {
    starting: i64,
    grace: i64,
    add_per_turn: i64,
    max_per_turn: i64,
    max_first_turn: i64,
    timeout_auto_choose: bool,
    accelerate: bool,
}

const TICK_TIME: i64 = 5;
const TIMER_COOLDOWN_MS: u64 = 20_000;

/// `RoomBattleTimer`, without the disconnection timer (a disconnected
/// player forfeits after the server's grace period instead).
#[derive(Clone, Debug)]
struct Timer {
    settings: TimerSettings,
    requesters: Vec<String>,
    /// When the next tick is due (`this.timer`).
    next_tick: Option<u64>,
    turn: Option<u32>,
    is_first_request: bool,
    last_disabled: Option<(u64, String)>,
}

impl Timer {
    fn new(format: &ShowdownFormat, challenge: bool) -> Timer {
        let settings = if format.vgc_timer {
            // `VGC Timer`: Timer Starting = 420, Grace = 90, Add Per Turn =
            // 0, Max Per Turn = 55, Max First Turn = 90, Timeout Auto Choose.
            TimerSettings {
                starting: 420,
                grace: 90,
                add_per_turn: 0,
                max_per_turn: 55,
                max_first_turn: 90,
                timeout_auto_choose: true,
                accelerate: false,
            }
        } else {
            let (starting, max) = if challenge { (300, 300) } else { (150, 150) };
            TimerSettings {
                starting,
                grace: 60,
                add_per_turn: if format.is_doubles() { 25 } else { 10 },
                max_per_turn: max,
                max_first_turn: max,
                timeout_auto_choose: false,
                accelerate: !challenge,
            }
        };
        Timer {
            settings,
            requesters: Vec::new(),
            next_tick: None,
            turn: None,
            is_first_request: true,
            last_disabled: None,
        }
    }
}

/// How a battle ended, for the ladder.
#[derive(Clone, Debug)]
pub(crate) struct Ending {
    /// The winner's user id; empty for a tie.
    pub(crate) winner: String,
    /// The simulator crashed (`setEnded` without `end`): no result, and
    /// no rating change.
    pub(crate) crashed: bool,
}

/// `listen()`'s message when the simulator stream fails.
const SIMULATOR_CRASHED: &str = "|bigerror|The simulator process crashed. We've been notified and will fix this ASAP.";

/// The `|j|` / `|l|` lines a room keeps in its log, at most. Showdown
/// (`reportjoins`) logs every named user's, and every join sends the whole
/// log: a client joining and leaving in a loop would grow both without
/// bound (Showdown's command throttle, off here by default, slows that
/// down). Past this many they are still sent, not logged.
const MAX_LOGGED_JOINS: usize = 64;

/// The timer requests' lines (`/timer on` and `off`: `|inactive|`, `|inactiveoff|`)
/// a room keeps in its log, at most. Showdown logs every one; past the
/// cooldown on turning it on again, a player can toggle their request
/// while the other keeps the timer on, two lines each time. Past this many
/// they are still sent, not logged.
const MAX_LOGGED_TIMER_REQUESTS: usize = 64;

/// The renames of one player a battle takes (each a `>player` that adds a
/// `|player|` line to the battle's log, in every battle the player is in).
/// Past this many the player can't rename while the battle goes on, as in
/// a battle that doesn't allow renaming.
const MAX_RENAMES: u32 = 64;

/// A battle room.
pub(crate) struct BattleRoom {
    pub(crate) id: String,
    pub(crate) format: &'static ShowdownFormat,
    title: String,
    /// The lower player's rating, 0 if unrated (Showdown's `room.rated`).
    pub(crate) rated: f64,
    /// `None` once the stream panicked.
    stream: Option<Box<dyn RoomStream>>,
    pub(crate) players: [Player; 2],
    /// Users in the room, in join order.
    pub(crate) users: Vec<UserKey>,
    log: Vec<LogLine>,
    buffer: Vec<LogLine>,
    /// `|j|` / `|l|` lines in `log`.
    logged_joins: usize,
    /// Timer requests' lines in `log`.
    logged_timer_requests: usize,
    started: bool,
    /// When the battle ended (set by the hub).
    pub(crate) ended_at: Option<u64>,
    pub(crate) ended: bool,
    ending: Option<Ending>,
    rqid: u32,
    request_count: u32,
    turn: u32,
    timer: Timer,
}

impl std::fmt::Debug for BattleRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BattleRoom").field("id", &self.id).field("ended", &self.ended).finish()
    }
}

/// Showdown's `meansYes` / `meansNo`.
fn means_yes(text: &str) -> bool {
    matches!(text, "yes" | "on" | "true" | "t" | "1" | "y" | "enable" | "enabled" | "allow" | "start")
}

fn means_no(text: &str) -> bool {
    matches!(text, "no" | "off" | "false" | "f" | "0" | "n" | "disable" | "disabled" | "disallow" | "stop")
}

/// JavaScript's `data.split('|', 2)`: the text before the first `|`, and
/// the text between it and the next one (not the rest).
fn split_rqid(data: &str) -> (&str, &str) {
    let mut parts = data.split('|');
    (parts.next().unwrap_or(""), parts.next().unwrap_or(""))
}

impl BattleRoom {
    pub(crate) fn new(
        id: String,
        format: &'static ShowdownFormat,
        players: [Player; 2],
        rated: f64,
        challenge: bool,
        stream: Box<dyn RoomStream>,
    ) -> BattleRoom {
        let title = format!("{} vs. {}", players[0].name, players[1].name);
        let mut timer = Timer::new(format, challenge);
        let mut players = players;
        for p in &mut players {
            p.seconds_left = timer.settings.starting + timer.settings.grace;
            p.turn_seconds_left = p.seconds_left;
        }
        if timer.settings.max_per_turn <= 0 {
            timer.settings.max_per_turn = i64::MAX;
        }
        BattleRoom {
            id,
            format,
            title,
            rated,
            stream: Some(stream),
            players,
            users: Vec::new(),
            log: Vec::new(),
            buffer: Vec::new(),
            logged_joins: 0,
            logged_timer_requests: 0,
            started: false,
            ended_at: None,
            ended: false,
            ending: None,
            rqid: 1,
            request_count: 0,
            turn: 0,
            timer,
        }
    }

    /// The player index of a user.
    pub(crate) fn player_of(&self, key: UserKey) -> Option<usize> {
        self.players.iter().position(|p| p.user == Some(key))
    }

    /// The player index of a user id.
    pub(crate) fn player_by_id(&self, id: &str) -> Option<usize> {
        self.players.iter().position(|p| p.id == id)
    }

    /// Showdown's `allowRenames`: not in a rated battle.
    pub(crate) fn allows_renames(&self) -> bool {
        self.rated <= 0.0
    }

    /// Whether a player has renamed as often as the battle takes
    /// ([`MAX_RENAMES`]).
    pub(crate) fn renames_used_up(&self, key: UserKey) -> bool {
        self.player_of(key).is_some_and(|i| self.players[i].renames >= MAX_RENAMES)
    }

    /// The channel a user reads: their side, or the spectators'.
    fn channel(&self, key: UserKey) -> Option<usize> {
        self.player_of(key)
    }

    /// The user's identity in this room (`☆` for players).
    fn identity(&self, user: &User) -> String {
        if self.player_of(user.key).is_some() { format!("☆{}", user.name) } else { format!(" {}", user.name) }
    }

    fn add(&mut self, line: impl Into<String>) {
        self.buffer.push(LogLine::Shared(line.into()));
    }

    /// `room.update()`: sends the buffered lines to everyone in the room,
    /// each their channel.
    fn update(&mut self, ctx: &mut Ctx<'_>) {
        if self.buffer.is_empty() {
            return;
        }
        for &key in &self.users {
            let channel = self.channel(key);
            let lines: Vec<&str> = self.buffer.iter().filter_map(|l| l.for_channel(channel)).collect();
            if !lines.is_empty() {
                ctx.send_user(key, &format!(">{}\n{}", self.id, lines.join("\n")));
            }
        }
        self.log.append(&mut self.buffer);
    }

    /// `reportJoin` of a `|j|` or `|l|` line: logged (up to
    /// [`MAX_LOGGED_JOINS`]) and sent to the room.
    fn report_join(&mut self, line: String, ctx: &mut Ctx<'_>) {
        if self.logged_joins < MAX_LOGGED_JOINS {
            self.logged_joins += 1;
            self.add(line);
            self.update(ctx);
        } else {
            for &key in &self.users {
                ctx.send_user(key, &format!(">{}\n{line}", self.id));
            }
        }
    }

    /// A timer request's line: logged (up to [`MAX_LOGGED_TIMER_REQUESTS`])
    /// and sent to the room.
    fn report_timer(&mut self, line: String, ctx: &mut Ctx<'_>) {
        if self.logged_timer_requests < MAX_LOGGED_TIMER_REQUESTS {
            self.logged_timer_requests += 1;
            self.add(line);
            self.update(ctx);
        } else {
            for &key in &self.users {
                ctx.send_user(key, &format!(">{}\n{line}", self.id));
            }
        }
    }

    /// `player.sendRoom(data)`.
    fn send_player(&self, index: usize, data: &str, ctx: &mut Ctx<'_>) {
        if let Some(key) = self.players[index].user
            && self.users.contains(&key)
        {
            ctx.send_user(key, &format!(">{}\n{}", self.id, data));
        }
    }

    /// Sends a reply to one user in this room.
    fn reply(&self, key: UserKey, text: &str, ctx: &mut Ctx<'_>) {
        ctx.send_user(key, &format!(">{}\n{}", self.id, text));
    }

    /// `errorReply` in this room.
    fn error_reply(&self, key: UserKey, text: &str, ctx: &mut Ctx<'_>) {
        self.reply(key, &format!("|error|{text}"), ctx);
    }

    /// A user joins: `|j|` for the others (a named user), `|init|battle`
    /// with the log for them (Showdown's `onJoin` then `onConnect`), and
    /// their request if they are a player coming back.
    pub(crate) fn join(&mut self, key: UserKey, ctx: &mut Ctx<'_>) {
        if self.users.contains(&key) {
            return;
        }
        let Some(user) = ctx.users.get(&key) else { return };
        if user.named {
            let join = format!("|j|{}", self.identity(user));
            self.report_join(join, ctx);
        }
        self.users.push(key);
        let channel = self.channel(key);
        let mut init = format!(">{}\n|init|battle\n|title|{}\n", self.id, self.title);
        for line in self.log.iter().filter_map(|l| l.for_channel(channel)) {
            init.push_str(line);
            init.push('\n');
        }
        ctx.send_user(key, &init);
        if let Some(i) = self.player_of(key) {
            self.players[i].disconnected_at = None;
            let request = &self.players[i].request;
            if !request.json.is_empty() {
                let mut data = format!("|request|{}", request.json);
                if !request.choice.is_empty() {
                    data.push_str(&format!("\n|sentchoice|{}", request.choice));
                }
                self.reply(key, &data, ctx);
            }
        }
    }

    /// A user leaves: `|deinit` for them, `|l|` for the others (a named
    /// user).
    pub(crate) fn leave(&mut self, user: &User, ctx: &mut Ctx<'_>) {
        let Some(pos) = self.users.iter().position(|&k| k == user.key) else { return };
        let identity = self.identity(user);
        self.users.remove(pos);
        ctx.send_user(user.key, &format!(">{}\n|deinit", self.id));
        if user.named {
            self.report_join(format!("|l|{identity}"), ctx);
        }
    }

    /// `RoomBattle.onRename`: a player's user changed their id from
    /// `old_id`. In a rated battle (no renames: a named user can't rename
    /// during one, so this is a guest) the player forfeits; otherwise the
    /// seat follows the user (`updatePlayer`) and the battle gets the new
    /// name (`>player`).
    pub(crate) fn on_rename(&mut self, user: &User, old_id: &str, ctx: &mut Ctx<'_>) {
        if user.id == old_id {
            return;
        }
        let Some(i) = self.player_by_id(old_id).filter(|&i| self.players[i].user == Some(user.key)) else {
            return;
        };
        if !self.allows_renames() {
            self.forfeit(i, " forfeited by changing their name.", ctx);
            return;
        }
        // A named user becoming a guest leaves the seat in Showdown
        // (`onLeave`); the seat stays theirs here.
        if !user.named || self.player_by_id(&user.id).is_some() || self.ended {
            return;
        }
        self.players[i].id = user.id.clone();
        self.players[i].name = user.name.clone();
        self.players[i].renames += 1;
        let mut options = Json::object();
        options.set("name", Json::str(user.name.clone()));
        options.set("avatar", Json::str(user.avatar.clone()));
        self.write(&format!(">player p{} {}", i + 1, options.stringify()), ctx);
    }

    /// Starts the battle: `>start`, then each `>player`.
    pub(crate) fn start(&mut self, ctx: &mut Ctx<'_>, seed: Option<&str>) {
        let mut start = Json::object();
        start.set("formatid", Json::str(self.format.stream_id));
        start.set("roomid", Json::str(self.id.clone()));
        start.set("rated", Json::str(if self.rated > 0.0 { "Rated battle" } else { "" }));
        if let Some(seed) = seed {
            start.set("seed", Json::str(seed));
        }
        self.write(&format!(">start {}", start.stringify()), ctx);
        for i in 0..2 {
            let p = &self.players[i];
            let mut options = Json::object();
            options.set("name", Json::str(p.name.clone()));
            options.set("avatar", Json::str(p.avatar.clone()));
            options.set("team", Json::str(p.team.clone()));
            options.set("rating", Json::Number(p.rating.round()));
            if let Some(seed) = &p.team_seed {
                options.set("seed", Json::str(seed.clone()));
            }
            self.write(&format!(">player p{} {}", i + 1, options.stringify()), ctx);
        }
        self.started = true;
    }

    /// Calls into the stream under `catch_unwind`: `None` if it has
    /// crashed, or does now (then the room ends, see [`Self::crash`]).
    fn call<R>(&mut self, ctx: &mut Ctx<'_>, f: impl FnOnce(&mut dyn RoomStream) -> R) -> Option<R> {
        let stream = self.stream.as_deref_mut()?;
        match catch_unwind(AssertUnwindSafe(|| f(stream))) {
            Ok(r) => Some(r),
            Err(_) => {
                self.crash(ctx);
                None
            }
        }
    }

    /// `RoomBattleStream._write`: writes to the stream and handles what it
    /// sends back. If the battle throws, Showdown's crash report (the
    /// broadcast, `[Invalid choice] The battle crashed` to each side with
    /// a request, the error in the log), then `sendUpdates`.
    fn write(&mut self, input: &str, ctx: &mut Ctx<'_>) {
        let Some(result) = self.call(ctx, |s| s.write(input)) else { return };
        self.pump(ctx);
        let Err(error) = result else { return };
        self.add(BATTLE_CRASHED);
        self.update(ctx);
        for side in SideId::BOTH {
            let Some(state) = self.call(ctx, |s| s.request_state(side)) else { return };
            if !state.is_empty() {
                let data = "|error|[Invalid choice] The battle crashed".to_string();
                self.receive(Message::SideUpdate { side, data }, ctx);
            }
        }
        // The `error` chunk: Showdown's crash log (and a lockdown if it
        // comes soon after the server started).
        crate::net::log(&format!("{}: the battle threw: {error}", self.id));
        // `this.battle.sendUpdates()`: the stream stopped at the error
        // before sending; an empty write sends what was logged.
        if self.call(ctx, |s| s.write("")).is_some() {
            self.pump(ctx);
        }
    }

    /// Reads everything the stream has written.
    fn pump(&mut self, ctx: &mut Ctx<'_>) {
        while let Some(Some(message)) = self.call(ctx, |s| s.read()) {
            self.receive(message, ctx);
        }
    }

    /// The stream panicked (the panic hook has printed where): the room
    /// reports a battle that threw, then a simulator that went away (see
    /// the module docs).
    fn crash(&mut self, ctx: &mut Ctx<'_>) {
        self.drop_stream();
        self.add(BATTLE_CRASHED);
        self.update(ctx);
        for i in 0..2 {
            // `side.requestState`: a request that is not a wait.
            if self.players[i].request.wait != Wait::CantUndo {
                let request = &mut self.players[i].request;
                request.wait = Wait::No;
                request.choice.clear();
                self.send_player(i, "|error|[Invalid choice] The battle crashed", ctx);
            }
        }
        self.stream_died(ctx);
    }

    /// `listen()` once its stream has failed: `|bigerror|` and `setEnded`
    /// (no winner, no rating change), unless the battle had ended.
    fn stream_died(&mut self, ctx: &mut Ctx<'_>) {
        self.drop_stream();
        if !self.ended {
            self.add(SIMULATOR_CRASHED);
            self.update(ctx);
            self.set_ended();
            self.ending = Some(Ending { winner: String::new(), crashed: true });
        }
    }

    fn drop_stream(&mut self) {
        if let Some(stream) = self.stream.take() {
            // Dropping may panic too; the stream is gone either way.
            let _ = catch_unwind(AssertUnwindSafe(move || drop(stream)));
        }
    }

    /// `RoomBattle.receive`.
    fn receive(&mut self, message: Message, ctx: &mut Ctx<'_>) {
        match message {
            Message::Update(data) => {
                for line in update_lines(&data) {
                    match line {
                        UpdateLine::Shared(line) => {
                            if let Some(turn) = line.strip_prefix("|turn|") {
                                self.turn = turn.parse().unwrap_or(self.turn);
                            }
                            self.add(line);
                        }
                        UpdateLine::Split { player, secret, shared } => self.buffer.push(LogLine::Split {
                            side: usize::from(player) - 1,
                            secret: secret.to_string(),
                            shared: shared.to_string(),
                        }),
                    }
                }
                self.update(ctx);
            }
            Message::SideUpdate { side, data } => {
                let index = side.index();
                // `lines[2]`: the message's first line.
                let message = data.split('\n').next().unwrap_or("");
                if message.starts_with("|error|[Invalid choice] Can't do anything") {
                    // Showdown: "should not happen".
                } else if message.starts_with("|error|[Invalid choice]") {
                    let request = &mut self.players[index].request;
                    request.wait = if message.contains("Can't undo") { Wait::CantUndo } else { Wait::No };
                    request.choice.clear();
                } else if let Some(json) = message.strip_prefix("|request|") {
                    self.rqid += 1;
                    // `request.wait` and `request.update`: the stream writes
                    // `wait` as a request's first key, and `update` as the
                    // last one (added to a request it sent before).
                    let wait = json.starts_with("{\"wait\":true");
                    let update = json.ends_with(",\"update\":true}");
                    let json = with_rqid(json, self.rqid);
                    self.send_player(index, &format!("|request|{json}"), ctx);
                    self.players[index].request = PlayerRequest {
                        rqid: self.rqid,
                        json,
                        wait: if wait { Wait::CantUndo } else { Wait::No },
                        choice: String::new(),
                    };
                    self.request_count += 1;
                    if !update {
                        self.timer_next_request(index, ctx);
                    }
                    return;
                }
                self.send_player(index, message, ctx);
            }
            Message::End(data) => match Json::parse(&data) {
                Ok(data) => {
                    let winner = data.get("winner").and_then(Json::as_str).unwrap_or("").to_string();
                    self.end(&winner);
                }
                Err(e) => {
                    crate::net::log(&format!("{}: the battle's end doesn't parse ({e}): {data}", self.id));
                    self.stream_died(ctx);
                }
            },
            Message::RequestedData(_) | Message::Replay(_) => {}
        }
    }

    /// `RoomBattle.end`: the battle is over.
    fn end(&mut self, winner_name: &str) {
        if self.ended {
            return;
        }
        self.set_ended();
        self.ending = Some(Ending { winner: crate::util::to_id(winner_name), crashed: false });
    }

    /// `RoomBattle.setEnded`: no more requests, the timer stops.
    fn set_ended(&mut self) {
        self.ended = true;
        for p in &mut self.players {
            p.request = PlayerRequest::none();
        }
        self.timer.requesters.clear();
        self.timer.next_tick = None;
    }

    /// The battle's ending, once, for the hub (ladder, `|updatesearch|`).
    pub(crate) fn take_ending(&mut self) -> Option<Ending> {
        self.ending.take()
    }

    /// Adds lines and sends them (for the hub: ratings).
    pub(crate) fn add_and_update(&mut self, lines: &[String], ctx: &mut Ctx<'_>) {
        for line in lines {
            self.add(line.clone());
        }
        self.update(ctx);
    }

    /// `/choose` (`RoomBattle.choose`).
    pub(crate) fn choose(&mut self, key: UserKey, data: &str, ctx: &mut Ctx<'_>) {
        let Some(index) = self.player_of(key) else { return };
        let (choice, rqid) = split_rqid(data);
        let request = &self.players[index].request;
        if request.wait == Wait::CantUndo {
            return self.send_player(index, "|error|[Invalid choice] There's nothing to choose", ctx);
        }
        let all_wait = self.players.iter().all(|p| p.request.wait != Wait::No);
        if all_wait || (!rqid.is_empty() && rqid != request.rqid.to_string()) {
            let message =
                "|error|[Invalid choice] Sorry, too late to make a different move; the next turn has already started";
            return self.send_player(index, message, ctx);
        }
        let request = &mut self.players[index].request;
        request.wait = Wait::Yes;
        request.choice = choice.to_string();
        self.write(&format!(">p{} {choice}", index + 1), ctx);
    }

    /// `/undo` (`RoomBattle.undo`).
    pub(crate) fn undo(&mut self, key: UserKey, data: &str, ctx: &mut Ctx<'_>) {
        let Some(index) = self.player_of(key) else { return };
        let (_, rqid) = split_rqid(data);
        let request = &self.players[index].request;
        if request.wait != Wait::Yes {
            return self.send_player(index, "|error|[Invalid choice] There's nothing to cancel", ctx);
        }
        let all_wait = self.players.iter().all(|p| p.request.wait != Wait::No);
        if all_wait || (!rqid.is_empty() && rqid != request.rqid.to_string()) {
            let message = "|error|[Invalid choice] Sorry, too late to cancel; the next turn has already started";
            return self.send_player(index, message, ctx);
        }
        self.players[index].request.wait = Wait::No;
        self.write(&format!(">p{} undo", index + 1), ctx);
    }

    /// `/forfeit`: `forfeitPlayer`.
    pub(crate) fn forfeit(&mut self, index: usize, message: &str, ctx: &mut Ctx<'_>) {
        if self.ended || !self.started || self.players[index].eliminated {
            return;
        }
        self.players[index].eliminated = true;
        let text = if message.is_empty() { " forfeited." } else { message };
        self.add(format!("|-message|{}{text}", self.players[index].name));
        self.update(ctx);
        self.write(&format!(">forcelose p{}", index + 1), ctx);
    }

    /// Whether [`tick`](Self::tick) has something to do at `now`.
    pub(crate) fn is_due(&self, grace_ms: u64, now: u64) -> bool {
        let gone = |p: &Player| p.disconnected_at.is_some_and(|at| now >= at.saturating_add(grace_ms));
        !self.ended
            && ((self.started && self.players.iter().any(gone)) || self.timer.next_tick.is_some_and(|t| now >= t))
    }

    /// `/acceptopenteamsheets`.
    pub(crate) fn accept_open_team_sheets(&mut self, key: UserKey, ctx: &mut Ctx<'_>) {
        let Some(index) = self.player_of(key) else {
            return self.error_reply(key, "Must be a player to agree to open team sheets.", ctx);
        };
        if !self.format.open_team_sheets() {
            let message = "This format does not allow requesting open team sheets. You can both manually agree to it by using !showteam hidestats.";
            return self.error_reply(key, message, ctx);
        }
        if self.turn > 0 {
            let message = "You cannot agree to open team sheets after Team Preview. Each player can still show their own sheet by using this command: !showteam hidestats";
            return self.error_reply(key, message, ctx);
        }
        if self.players.iter().any(|p| p.wants_open_team_sheets == Some(false)) {
            return self.error_reply(key, "An opponent has already rejected open team sheets.", ctx);
        }
        if self.players[index].wants_open_team_sheets.is_some() {
            return self.error_reply(
                key,
                "You have already made your decision about agreeing to open team sheets.",
                ctx,
            );
        }
        self.players[index].wants_open_team_sheets = Some(true);
        self.send_player(index, "|uhtmlchange|otsrequest|", ctx);
        self.add(format!("{} has agreed to open team sheets.", self.players[index].name));
        self.update(ctx);
        if self.players.iter().all(|p| p.wants_open_team_sheets == Some(true)) {
            self.write(">show-openteamsheets", ctx);
        }
    }

    /// `/rejectopenteamsheets`.
    pub(crate) fn reject_open_team_sheets(&mut self, key: UserKey, ctx: &mut Ctx<'_>) {
        let Some(index) = self.player_of(key) else {
            return self.error_reply(key, "Must be a player to reject open team sheets.", ctx);
        };
        if !self.format.open_team_sheets() {
            return self.error_reply(key, "This format does not allow requesting open team sheets.", ctx);
        }
        if self.turn > 0 {
            return self.error_reply(key, "You cannot reject open team sheets after Team Preview.", ctx);
        }
        if self.players[index].wants_open_team_sheets.is_some() {
            return self.error_reply(
                key,
                "You have already made your decision about agreeing to open team sheets.",
                ctx,
            );
        }
        self.players[index].wants_open_team_sheets = Some(false);
        for i in 0..2 {
            self.send_player(i, "|uhtmlchange|otsrequest|", ctx);
        }
        self.add(format!("{} rejected open team sheets.", self.players[index].name));
        self.update(ctx);
    }

    /// `/timer [on|off]`.
    pub(crate) fn timer_command(&mut self, key: UserKey, target: &str, ctx: &mut Ctx<'_>) {
        let target = crate::util::to_id(target);
        if target.is_empty() {
            let text = if self.timer.requesters.is_empty() {
                "The game timer is OFF.".to_string()
            } else {
                format!("The game timer is ON (requested by {})", self.timer.requesters.join(", "))
            };
            return self.reply(key, &text, ctx);
        }
        let Some(index) = self.player_of(key) else {
            return self.error_reply(key, "Access denied.", ctx);
        };
        if means_no(&target) {
            if self.timer.requesters.is_empty() {
                return self.error_reply(key, "The timer is already off.", ctx);
            }
            self.timer_stop(index, ctx);
        } else if means_yes(&target) {
            self.timer_start(index, ctx);
        } else {
            self.error_reply(key, &format!("\"{target}\" is not a recognized timer state."), ctx);
        }
    }

    fn timer_start(&mut self, index: usize, ctx: &mut Ctx<'_>) {
        let (id, name) = (self.players[index].id.clone(), self.players[index].name.clone());
        if self.timer.requesters.contains(&id) {
            return;
        }
        if self.ended {
            return self.send_player(index, "|inactiveoff|The timer can't be enabled after a battle has ended.", ctx);
        }
        if !self.timer.requesters.is_empty() {
            self.report_timer(format!("|inactive|{name} also wants the timer to be on."), ctx);
            self.timer.requesters.push(id);
            return;
        }
        if let Some((at, by)) = &self.timer.last_disabled {
            let remaining = (at + TIMER_COOLDOWN_MS).saturating_sub(ctx.now);
            if *by == id && remaining > 0 {
                let message = format!(
                    "|inactiveoff|The timer can't be re-enabled so soon after disabling it ({} seconds remaining).",
                    remaining.div_ceil(1000)
                );
                return self.send_player(index, &message, ctx);
            }
        }
        self.timer.requesters.push(id);
        self.report_timer(
            format!(
                "|inactive|Battle timer is ON: inactive players will automatically lose when time's up. (requested by {name})"
            ),
            ctx,
        );
        for i in 0..2 {
            self.timer_next_request(i, ctx);
        }
    }

    fn timer_stop(&mut self, index: usize, ctx: &mut Ctx<'_>) {
        let (id, name) = (self.players[index].id.clone(), self.players[index].name.clone());
        let Some(pos) = self.timer.requesters.iter().position(|r| *r == id) else { return };
        self.timer.requesters.remove(pos);
        self.timer.last_disabled = Some((ctx.now, id));
        if !self.timer.requesters.is_empty() {
            let line = format!(
                "|inactive|{name} no longer wants the timer on, but the timer is staying on because {} still does.",
                self.timer.requesters.join(", ")
            );
            return self.report_timer(line, ctx);
        }
        if self.timer.next_tick.take().is_some() {
            self.report_timer("|inactiveoff|Battle timer is now OFF.".to_string(), ctx);
        }
    }

    /// `updateTurn`.
    fn timer_update_turn(&mut self) {
        let Some(turn) = self.timer.turn else {
            self.timer.turn = Some(self.turn);
            return;
        };
        if self.turn <= turn {
            if self.players.iter().filter(|p| p.request.wait == Wait::No).count() <= 1 {
                self.timer.is_first_request = false;
                let add = self.timer.settings.add_per_turn.min(TICK_TIME);
                for p in &mut self.players {
                    p.seconds_left += add;
                }
            }
            return;
        }
        self.timer.turn = Some(self.turn);
        self.timer.is_first_request = false;
        let mut add = self.timer.settings.add_per_turn;
        if self.timer.settings.accelerate && add > 0 {
            if self.turn > 100 && add > TICK_TIME {
                add -= TICK_TIME;
            }
            if self.turn > 200 && (self.request_count / 2) % 2 == 1 {
                add = 0;
            }
        }
        let starting = self.timer.settings.starting;
        for p in &mut self.players {
            p.seconds_left = (p.seconds_left + add).min(starting);
        }
    }

    /// `nextRequest`.
    fn timer_next_request(&mut self, index: usize, ctx: &mut Ctx<'_>) {
        if self.players[index].seconds_left <= 0 {
            return;
        }
        if self.players[index].request.wait != Wait::No {
            self.players[index].turn_seconds_left = self.timer.settings.max_per_turn;
            return;
        }
        self.timer.next_tick = None;
        if self.ended || self.timer.requesters.is_empty() {
            return;
        }
        if self.players.iter().filter(|p| p.seconds_left > 0).count() <= 1 {
            return;
        }
        self.timer_update_turn();
        let s = self.timer.settings;
        let max_turn = if self.timer.is_first_request { s.max_first_turn } else { s.max_per_turn };
        let player = &mut self.players[index];
        player.turn_seconds_left = player.seconds_left.min(max_turn);
        let seconds = player.turn_seconds_left;
        let grace = (player.seconds_left - s.starting).max(0);
        let mut text =
            format!("|inactive|Time left: {seconds} sec this turn | {} sec total", player.seconds_left - grace);
        if grace > 0 {
            text.push_str(&format!(" | {grace} sec grace"));
        }
        let name = player.name.clone();
        self.send_player(index, &text, ctx);
        if seconds <= 30 && seconds < s.starting {
            self.add(format!("|inactive|{name} has {seconds} seconds left this turn."));
        }
        self.update(ctx);
        self.timer.next_tick = Some(ctx.now + TICK_TIME as u64 * 1000);
    }

    /// Runs what is due: the timer's tick, disconnected players' forfeits.
    pub(crate) fn tick(&mut self, grace_ms: u64, ctx: &mut Ctx<'_>) {
        if self.ended {
            return;
        }
        for i in 0..2 {
            let gone = self.players[i].disconnected_at.is_some_and(|at| ctx.now >= at.saturating_add(grace_ms));
            if gone && self.started {
                self.players[i].disconnected_at = None;
                self.forfeit(i, " lost due to inactivity.", ctx);
                if self.ended {
                    return;
                }
            }
        }
        if self.timer.next_tick.is_some_and(|t| ctx.now >= t) {
            self.timer_tick(ctx);
        }
    }

    /// `nextTick`.
    fn timer_tick(&mut self, ctx: &mut Ctx<'_>) {
        self.timer.next_tick = None;
        if self.ended || self.timer.requesters.is_empty() {
            return;
        }
        if self.players.iter().all(|p| p.request.wait == Wait::CantUndo) {
            return;
        }
        for i in 0..2 {
            let p = &mut self.players[i];
            if p.request.wait != Wait::No {
                continue;
            }
            p.seconds_left -= TICK_TIME;
            p.turn_seconds_left -= TICK_TIME;
            let seconds = p.turn_seconds_left;
            if seconds == 0 {
                continue;
            }
            if seconds % 30 == 0 || seconds <= 20 {
                let line = format!("|inactive|{} has {seconds} seconds left.", p.name);
                self.add(line);
            }
        }
        self.update(ctx);
        if !self.timer_check_timeout(ctx) {
            self.timer.next_tick = Some(ctx.now + TICK_TIME as u64 * 1000);
        }
    }

    /// `checkTimeout`.
    fn timer_check_timeout(&mut self, ctx: &mut Ctx<'_>) -> bool {
        if self.players.iter().all(|p| p.turn_seconds_left <= 0)
            && (!self.timer.settings.timeout_auto_choose || self.players.iter().all(|p| p.seconds_left <= 0))
        {
            self.add("|-message|All players are inactive.");
            self.update(ctx);
            self.write(">forcetie", ctx);
            return true;
        }
        let mut did = false;
        for i in 0..2 {
            let p = &self.players[i];
            if p.eliminated || p.turn_seconds_left > 0 {
                continue;
            }
            if self.timer.settings.timeout_auto_choose && p.seconds_left > 0 {
                self.write(&format!(">p{} default", i + 1), ctx);
            } else {
                self.forfeit(i, " lost due to inactivity.", ctx);
            }
            did = true;
        }
        did
    }

    /// Whether the room can be dropped: over, and nobody left in it.
    pub(crate) fn is_finished(&self) -> bool {
        self.ended && self.users.is_empty()
    }
}

/// Adds Showdown's `rqid` as the request's last key (`request.rqid =
/// this.rqid` on the parsed request, which has none).
fn with_rqid(json: &str, rqid: u32) -> String {
    match json.strip_suffix('}') {
        Some(body) if body.len() > 1 => format!("{body},\"rqid\":{rqid}}}"),
        _ => format!("{{\"rqid\":{rqid}}}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rqid_is_the_last_key() {
        assert_eq!(with_rqid("{\"wait\":true,\"side\":{}}", 3), "{\"wait\":true,\"side\":{},\"rqid\":3}");
    }

    #[test]
    fn rqids_split_as_javascript_does() {
        assert_eq!(split_rqid("move 1|3|x"), ("move 1", "3"));
        assert_eq!(split_rqid("move 1|3"), ("move 1", "3"));
        assert_eq!(split_rqid("move 1"), ("move 1", ""));
    }
}
