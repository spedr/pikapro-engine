//! The server's state and command handling: users, `/trn`, `/utm`,
//! matchmaking (`ladders.ts`, `ladders-challenges.ts`), the commands of
//! `chat-commands/core.ts` poke-env sends, and the battle rooms.
//!
//! The hub is synchronous and does no I/O: messages come in through
//! [`Hub::connect`], [`Hub::message`] and [`Hub::disconnect`] (with the
//! time), and what the server sends waits in [`Hub::take_outbox`]. The
//! network layer (`net.rs`) drives it.

use std::collections::{BTreeMap, HashMap};

use pikapro::Gender;
use pikapro_protocol::TeamSheet;
use pikapro_server::json::Json;
use pikapro_validate::{SetInfo, ValidationFormat, Validator};

use crate::formats::{self, ShowdownFormat};
use crate::room::{BattleRoom, Ctx, Player};
use crate::stream::StreamFactory;
use crate::users::{CUSTOM_GROUPS, ConnId, User, UserKey, Users};
use crate::util::{escape_html, get_name, js_length, split_first, to_id};

/// How the server behaves.
pub struct Config {
    /// Creates each battle's stream.
    pub stream: StreamFactory,
    /// Validate teams as Showdown's server does (`pikapro-validate` for the
    /// VGC formats). Off: only what the engine needs (`Team::validate`).
    pub validate: bool,
    /// A player whose connections have all closed forfeits their
    /// unfinished battles after this long (milliseconds).
    pub disconnect_grace_ms: u64,
    /// Seeds battles deterministically from this (and the battle number)
    /// instead of leaving the seed to the stream.
    pub seed: Option<u64>,
    /// Unfinished battles one user id may play at once; 0 for no limit.
    /// Showdown has none; here it bounds what one connection can make the
    /// server run. A search, challenge or accept past it is refused with a
    /// popup, and a search or challenge doesn't match an opponent at it.
    pub max_battles_per_user: usize,
    /// Finished battles' rooms kept at once, over all users; past it the
    /// oldest goes as if it had expired; 0 for no limit. A user's own
    /// finished battles are also kept to [`MAX_ENDED_ROOMS_PER_USER`].
    /// Showdown keeps each for ten minutes without a cap.
    pub max_ended_rooms: usize,
    /// Command lines one connection's user may send per second; 0 (the
    /// default) for no limit, as on a Showdown server started with
    /// `--no-security`. Up to a second's worth may come at once. A line
    /// past it is dropped, with the rest of its message, and the
    /// connection gets Showdown's throttle notice (`|raw|` "Your message
    /// was not sent because you've been typing too quickly."). A dropped
    /// line is lost, battle choices included: a client that sends faster
    /// leaves its battles waiting. Set it only on a server exposed to
    /// untrusted clients, well above what the trusted ones send: poke-env
    /// playing many battles at once sends thousands of lines a second.
    /// (Showdown's `THROTTLE_DELAY*` run a user's lines 600 ms apart, a
    /// trusted user's 100 ms and a public bot's 25 ms.)
    pub max_lines_per_second: u32,
}

impl Default for Config {
    /// The command line's defaults: Showdown's battle streams, teams
    /// validated, a minute's disconnection grace, fresh seeds, the
    /// default limits, and no line throttle.
    fn default() -> Config {
        Config {
            stream: crate::stream::room_battle_streams(),
            validate: true,
            disconnect_grace_ms: 60_000,
            seed: None,
            max_battles_per_user: MAX_BATTLES_PER_USER,
            max_ended_rooms: MAX_ENDED_ROOMS,
            max_lines_per_second: 0,
        }
    }
}

/// [`Config::max_battles_per_user`]'s default.
pub const MAX_BATTLES_PER_USER: usize = 64;

/// [`Config::max_ended_rooms`]'s default.
pub const MAX_ENDED_ROOMS: usize = 1000;

/// The finished battles' rooms kept per player (by user id); past it the
/// player's oldest goes as if it had expired.
pub const MAX_ENDED_ROOMS_PER_USER: usize = 16;

/// The ratings kept (one per format and user id); past it the one updated
/// least recently is forgotten (that user starts again at 1000).
const MAX_RATINGS: usize = 10_000;

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("validate", &self.validate)
            .field("disconnect_grace_ms", &self.disconnect_grace_ms)
            .field("seed", &self.seed)
            .field("max_battles_per_user", &self.max_battles_per_user)
            .field("max_ended_rooms", &self.max_ended_rooms)
            .field("max_lines_per_second", &self.max_lines_per_second)
            .finish()
    }
}

/// How long a room stays after its battle ended (Showdown's
/// `TIMEOUT_EMPTY_DEALLOCATE`: ten minutes).
const ROOM_EXPIRY_MS: u64 = 10 * 60 * 1000;

/// Showdown's problem when validating a team throws.
const VALIDATOR_CRASHED: &str = "Your team crashed the validator. We'll fix this crash within a few hours (we're automatically notified), but if you don't want to wait, just use a different team for now.";

/// The longest message Showdown's sockets take (`100 * 1024`, in UTF-16
/// units).
pub(crate) const MAX_MESSAGE_LENGTH: usize = 100 * 1024;

/// The most lines a message may have (`THROTTLE_MULTILINE_WARN`).
const MAX_LINES: usize = 3;

/// A team that passed validation for a format (`BattleReady`).
#[derive(Clone, Debug)]
struct Ready {
    user: UserKey,
    format: &'static ShowdownFormat,
    team: String,
    /// A random battle's seed for the player's generated team (`--seed`);
    /// `None`: the battle draws a fresh one, as Showdown's does.
    team_seed: Option<String>,
    rating: f64,
}

/// A pending challenge (`BattleChallenge`), between user ids.
#[derive(Clone, Debug)]
struct Challenge {
    from: String,
    to: String,
    ready: Ready,
}

/// The server state.
pub struct Hub {
    config: Config,
    /// Random-battle teams drawn so far: part of each one's seed.
    team_draws: u64,
    out: Vec<(ConnId, String)>,
    conns: HashMap<ConnId, UserKey>,
    users: Users,
    /// Every user by id, guests too: no two users share an id (Showdown's
    /// `Users.users`).
    ids: HashMap<String, UserKey>,
    next_key: UserKey,
    guests: u64,
    /// Ladder searches, oldest first.
    searches: Vec<Ready>,
    challenges: Vec<Challenge>,
    rooms: BTreeMap<String, BattleRoom>,
    battles: u64,
    /// Elo per (format, user id), with when it was last updated (a count
    /// of updates), to forget the stalest past [`MAX_RATINGS`].
    ratings: HashMap<(String, String), (f64, u64)>,
    rating_updates: u64,
    max_ratings: usize,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub").field("users", &self.users.len()).field("rooms", &self.rooms.len()).finish()
    }
}

/// `connection.popup`.
fn popup(text: &str) -> String {
    format!("|popup|{}", text.replace('\n', "||"))
}

/// The `|challstr|` challenge: 128 bytes of hex. Nothing authenticates
/// against it here, so any unpredictable-enough value will do.
fn challenge_string() -> String {
    (0..8).map(|_| pikapro_server::seed::generate().text["sodium,".len()..].to_string()).collect()
}

impl Hub {
    pub fn new(config: Config) -> Hub {
        Hub {
            config,
            team_draws: 0,
            out: Vec::new(),
            conns: HashMap::new(),
            users: HashMap::new(),
            ids: HashMap::new(),
            next_key: 1,
            guests: 0,
            searches: Vec::new(),
            challenges: Vec::new(),
            rooms: BTreeMap::new(),
            battles: 0,
            ratings: HashMap::new(),
            rating_updates: 0,
            max_ratings: MAX_RATINGS,
        }
    }

    /// What the server has to send, in order.
    pub fn take_outbox(&mut self) -> Vec<(ConnId, String)> {
        std::mem::take(&mut self.out)
    }

    /// The number of battle rooms (for tests).
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    /// The number of users (for tests).
    pub fn user_count(&self) -> usize {
        self.users.len()
    }

    fn send_conn(&mut self, conn: ConnId, message: impl Into<String>) {
        self.out.push((conn, message.into()));
    }

    fn send_user(&mut self, key: UserKey, message: &str) {
        if let Some(user) = self.users.get(&key) {
            for &conn in &user.conns {
                self.out.push((conn, message.to_string()));
            }
        }
    }

    fn user(&self, key: UserKey) -> &User {
        &self.users[&key]
    }

    /// A user by (anything that becomes) their id (`Users.get`).
    fn find_user(&self, name: &str) -> Option<UserKey> {
        self.ids.get(&to_id(name)).copied()
    }

    /// `user.getIdentity()` of the user with this id, or ` id` for none.
    fn identity_of(&self, id: &str) -> String {
        self.ids.get(id).and_then(|k| self.users.get(k)).map_or_else(|| format!(" {id}"), User::identity)
    }

    fn ctx(&mut self, now: u64) -> (Ctx<'_>, &mut BTreeMap<String, BattleRoom>) {
        (Ctx { users: &self.users, out: &mut self.out, now }, &mut self.rooms)
    }

    // -----------------------------------------------------------------
    // Connections

    /// A new connection: a guest user, `|updateuser|` with the groups and
    /// formats, then `|challstr|` (`Users.socketConnect`).
    pub fn connect(&mut self, conn: ConnId) {
        // `Users.add`: the next guest number, past any a renamed user took.
        self.guests += 1;
        while self.ids.contains_key(&format!("guest{}", self.guests)) {
            self.guests += 1;
        }
        let key = self.next_key;
        self.next_key += 1;
        const AVATARS: [&str; 8] = ["1", "2", "101", "102", "169", "170", "265", "266"];
        let user = User {
            key,
            id: format!("guest{}", self.guests),
            name: format!("Guest {}", self.guests),
            named: false,
            avatar: AVATARS[(self.guests % 8) as usize].to_string(),
            conns: vec![conn],
            team: String::new(),
            games: Vec::new(),
            rooms: Vec::new(),
            budget: None,
        };
        let hello = format!("{}\n{}\n{}", user.updateuser(), CUSTOM_GROUPS, ShowdownFormat::formats_list());
        self.ids.insert(user.id.clone(), key);
        self.users.insert(key, user);
        self.conns.insert(conn, key);
        self.send_conn(conn, hello);
        self.send_conn(conn, format!("|challstr|4|{}", challenge_string()));
    }

    /// A connection closed. When it was the user's last: leave every room,
    /// drop searches and challenges, and start the disconnection grace
    /// period of their unfinished battles.
    pub fn disconnect(&mut self, conn: ConnId, now: u64) {
        let Some(key) = self.conns.remove(&conn) else { return };
        let Some(user) = self.users.get_mut(&key) else { return };
        user.conns.retain(|&c| c != conn);
        if !user.conns.is_empty() {
            return;
        }
        let mut rooms = user.rooms.clone();
        rooms.extend(user.games.iter().filter(|g| !user.rooms.contains(g)).cloned());
        let id = user.id.clone();
        let (mut ctx, all) = self.ctx(now);
        for roomid in &rooms {
            if let Some(room) = all.get_mut(roomid) {
                if let Some(user) = ctx.users.get(&key) {
                    room.leave(user, &mut ctx);
                }
                if let Some(i) = room.player_of(key)
                    && !room.ended
                {
                    room.players[i].disconnected_at = Some(now);
                }
            }
        }
        self.searches.retain(|s| s.user != key);
        self.clear_challenges(&id, "they left");
        self.forget(key);
        self.drop_finished_rooms();
    }

    /// Removes a user without connections from the tables.
    fn forget(&mut self, key: UserKey) {
        if let Some(user) = self.users.remove(&key)
            && self.ids.get(&user.id) == Some(&key)
        {
            self.ids.remove(&user.id);
        }
    }

    /// After a handler panicked (the crash guard, `net.rs`): puts back
    /// what it may have left half-done. Connections and ids that point at
    /// no user are forgotten; a user without connections goes, with their
    /// searches and challenges (silently), from the rooms they were in,
    /// and their unfinished battles start the disconnection grace period.
    pub fn recover(&mut self, now: u64) {
        let users = &self.users;
        self.conns.retain(|_, key| users.contains_key(key));
        let conns = &self.conns;
        for (&key, user) in self.users.iter_mut() {
            user.conns.retain(|c| conns.get(c) == Some(&key));
        }
        let mut gone: Vec<UserKey> = self.users.iter().filter(|(_, u)| u.conns.is_empty()).map(|(&k, _)| k).collect();
        gone.sort_unstable();
        for key in gone {
            let id = self.users[&key].id.clone();
            self.searches.retain(|s| s.user != key);
            self.challenges.retain(|c| c.from != id && c.to != id && c.ready.user != key);
            for room in self.rooms.values_mut() {
                room.users.retain(|&k| k != key);
                if let Some(i) = room.player_of(key)
                    && !room.ended
                    && room.players[i].disconnected_at.is_none()
                {
                    room.players[i].disconnected_at = Some(now);
                }
            }
            self.forget(key);
        }
        let users = &self.users;
        self.ids.retain(|id, key| users.get(key).is_some_and(|u| u.id == *id));
        self.searches.retain(|s| users.contains_key(&s.user));
        self.drop_finished_rooms();
    }

    /// `Challenges.clearFor`: the user's challenges, both ways; each side
    /// is told why. The number cleared.
    fn clear_challenges(&mut self, id: &str, reason: &str) -> usize {
        let (mine, rest): (Vec<Challenge>, Vec<Challenge>) =
            std::mem::take(&mut self.challenges).into_iter().partition(|c| c.from == id || c.to == id);
        self.challenges = rest;
        for c in &mine {
            let other = if c.from == id { &c.to } else { &c.from };
            let header = format!("|pm|{}|{}|", self.identity_of(id), self.identity_of(other));
            let message = format!("{header}/text Challenge cancelled because {reason}.\n{header}/challenge");
            for who in [id, other.as_str()] {
                if let Some(&k) = self.ids.get(who) {
                    self.send_user(k, &message);
                }
            }
        }
        mine.len()
    }

    /// Runs what is due: battle timers, disconnection forfeits.
    pub fn tick(&mut self, now: u64) {
        let grace = self.config.disconnect_grace_ms;
        let due: Vec<String> =
            self.rooms.iter().filter(|(_, r)| r.is_due(grace, now)).map(|(id, _)| id.clone()).collect();
        for id in due {
            let (mut ctx, rooms) = self.ctx(now);
            if let Some(room) = rooms.get_mut(&id) {
                room.tick(grace, &mut ctx);
            }
            self.after_room(&id, now);
        }
        // Showdown deallocates a room some time after its battle ended
        // (`pokeExpireTimer`); whoever is still in it gets `|deinit`.
        let expired: Vec<String> = self
            .rooms
            .iter()
            .filter(|(_, r)| r.ended_at.is_some_and(|t| now >= t.saturating_add(ROOM_EXPIRY_MS)))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.expire_room(&id);
        }
        self.drop_finished_rooms();
    }

    /// Removes a room; whoever is still in it gets `|deinit`.
    fn expire_room(&mut self, id: &str) {
        let Some(room) = self.rooms.remove(id) else { return };
        for key in room.users {
            if let Some(user) = self.users.get_mut(&key) {
                user.rooms.retain(|r| r != id);
            }
            self.send_user(key, &format!(">{id}\n|deinit"));
        }
    }

    /// After `roomid`'s battle ended: expires the oldest finished rooms
    /// past [`MAX_ENDED_ROOMS_PER_USER`] for each of its players, then
    /// past [`Config::max_ended_rooms`] in all (unless it is 0).
    fn expire_surplus_rooms(&mut self, roomid: &str) {
        let Some(room) = self.rooms.get(roomid) else { return };
        let ids: Vec<String> = room.players.iter().map(|p| p.id.clone()).collect();
        let ended = |rooms: &BTreeMap<String, BattleRoom>, id: Option<&str>| {
            let mut ended: Vec<((u64, u64), String)> = rooms
                .iter()
                .filter(|(_, r)| id.is_none_or(|id| r.players.iter().any(|p| p.id == id)))
                .filter_map(|(roomid, r)| {
                    // Oldest first: by when it ended, then by battle number.
                    let number = roomid.rsplit('-').next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
                    Some(((r.ended_at?, number), roomid.clone()))
                })
                .collect();
            ended.sort_unstable();
            ended
        };
        for id in &ids {
            let mine = ended(&self.rooms, Some(id));
            for (_, old) in &mine[..mine.len().saturating_sub(MAX_ENDED_ROOMS_PER_USER)] {
                self.expire_room(old);
            }
        }
        if self.config.max_ended_rooms > 0 {
            let all = ended(&self.rooms, None);
            for (_, old) in &all[..all.len().saturating_sub(self.config.max_ended_rooms)] {
                self.expire_room(old);
            }
        }
    }

    fn drop_finished_rooms(&mut self) {
        self.rooms.retain(|_, room| !room.is_finished());
    }

    // -----------------------------------------------------------------
    // Messages

    /// A message from a connection: `ROOMID|TEXT`, one command or chat
    /// line per line of `TEXT`. Dropped as Showdown's sockets drop them:
    /// empty, a legacy JSON message, no `|` or nothing after it; longer
    /// than 100 KB (`100 * 1024` UTF-16 units, with a popup); more than
    /// three lines (with a popup), a trailing empty line not counted. Each
    /// line goes through the throttle ([`Config::max_lines_per_second`]).
    pub fn message(&mut self, conn: ConnId, message: &str, now: u64) {
        let Some(&key) = self.conns.get(&conn) else { return };
        if message.is_empty() {
            return;
        }
        if message.len() > MAX_MESSAGE_LENGTH && js_length(message) > MAX_MESSAGE_LENGTH {
            return self.send_conn(conn, "|popup|Your message must be below 100KB");
        }
        if message.starts_with('{') {
            return;
        }
        let Some((roomid, text)) = message.split_once('|').filter(|(_, text)| !text.is_empty()) else { return };
        let mut lines: Vec<&str> = text.split('\n').collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        if lines.len() > MAX_LINES {
            let text = "You're sending too many lines at once. Try using a paste service like [[Pastebin]].";
            return self.send_conn(conn, popup(text));
        }
        for line in lines {
            if !self.users.contains_key(&key) {
                return;
            }
            if !self.throttle(key, now) {
                // `User.chat`: the notice, to the room the line was for,
                // and the rest of the message is discarded.
                let notice = "|raw|<strong class=\"message-throttle-notice\">Your message was not sent because you've been typing too quickly.</strong>";
                let text =
                    if self.rooms.contains_key(roomid) { format!(">{roomid}\n{notice}") } else { notice.to_string() };
                return self.send_conn(conn, text);
            }
            self.line(key, conn, roomid, line, now);
        }
    }

    /// Whether `key` may send another line now, taking it from their
    /// budget. Showdown's `User.chat` throttle (`users.ts`) runs a guest's
    /// lines 600 ms apart (`THROTTLE_DELAY`), queueing up to five and
    /// dropping the rest (`THROTTLE_BUFFER_LIMIT`), and is off on a
    /// `--no-security` server. Here it is off unless
    /// [`Config::max_lines_per_second`] is set, and then a token bucket
    /// bounds the rate instead: up to a second's worth at once, refilled
    /// at that rate, lines past it dropped.
    fn throttle(&mut self, key: UserKey, now: u64) -> bool {
        let rate = u64::from(self.config.max_lines_per_second);
        let Some(user) = self.users.get_mut(&key).filter(|_| rate > 0) else { return true };
        let full = rate * 1000;
        let (budget, at) = user.budget.unwrap_or((full, now));
        let budget = budget.saturating_add(now.saturating_sub(at).saturating_mul(rate)).min(full);
        let allowed = budget >= 1000;
        user.budget = Some((if allowed { budget - 1000 } else { budget }, now.max(at)));
        allowed
    }

    fn line(&mut self, key: UserKey, conn: ConnId, roomid: &str, line: &str, now: u64) {
        let room = self.rooms.contains_key(roomid).then(|| roomid.to_string());
        // `/noreply COMMAND` runs the command: its prefixes are taken off
        // in a loop, however many there are.
        let mut line = line;
        let (cmd, target) = loop {
            let Some(command) = line.strip_prefix('/').filter(|c| !c.starts_with('/')) else {
                return self.chat(key, room);
            };
            let (cmd, target) = split_first(command, ' ');
            let target = target.trim();
            if !cmd.eq_ignore_ascii_case("noreply") {
                break (cmd.to_ascii_lowercase(), target);
            }
            if !target.starts_with('/') {
                return;
            }
            line = target;
        };
        match cmd.as_str() {
            "trn" => self.rename(key, conn, target, now),
            "utm" | "useteam" | "saveteam" => {
                if let Some(user) = self.users.get_mut(&key) {
                    user.team = target.to_string();
                }
            }
            "search" => {
                if target.is_empty() {
                    self.cancel_searches(key, None);
                } else {
                    self.search(key, conn, target, now);
                }
            }
            "cancelsearch" => self.cancel_searches(key, (!target.is_empty()).then_some(target)),
            "challenge" | "chall" => self.challenge(key, conn, target, now),
            "cancelchallenge" | "cchall" => self.cancel_challenge(key, conn, target),
            "accept" => self.accept(key, conn, target, now),
            "reject" => self.reject(key, conn, target),
            "join" | "j" => self.join(key, target, now),
            "leave" | "part" => {
                let id = if target.is_empty() { roomid } else { target };
                self.leave(key, id, now);
            }
            "avatar" => self.avatar(key, room.as_deref(), target),
            "cmd" | "crq" | "query" => {
                let (query, _) = split_first(target, ' ');
                self.send_conn(conn, format!("|queryresponse|{query}|null"));
            }
            "choose" | "guess" | "move" | "mv" | "attack" | "switch" | "sw" | "team" | "undo" | "forfeit" | "timer"
            | "acceptopenteamsheets" | "rejectopenteamsheets" => match room {
                Some(room) => self.room_command(key, &room, &cmd, target, now),
                None => self.error_reply(key, None, &format!("/{cmd} - must be used in a chat room, not a console")),
            },
            _ => self.error_reply(
                key,
                room.as_deref(),
                &format!("The command \"/{cmd}\" does not exist. To send a message starting with \"/{cmd}\", type \"//{cmd}\"."),
            ),
        }
    }

    /// `errorReply`: in a room, `|error|`; outside, a PM from `~`.
    fn error_reply(&mut self, key: UserKey, room: Option<&str>, text: &str) {
        let message = match room {
            Some(room) => format!(">{room}\n|error|{text}"),
            None => format!("|pm|{}|~|/error {text}", self.user(key).identity()),
        };
        self.send_user(key, &message);
    }

    /// A chat message: battle rooms are temporary rooms, where Showdown
    /// only lets registered users talk (and nobody is registered here);
    /// guests can't talk anywhere (`checkChat`).
    fn chat(&mut self, key: UserKey, room: Option<String>) {
        let Some(room) = room else { return };
        if !self.user(key).named {
            return self.error_reply(key, Some(&room), "You must choose a name before you can talk.");
        }
        let message = format!(
            ">{room}\n|html|<div class=\"message-error\">You must be registered to chat in temporary rooms (like battles).</div>You may register in the <button name=\"openOptions\"><i class=\"fa fa-cog\"></i> Options</button> menu."
        );
        self.send_user(key, &message);
    }

    /// `/avatar NAME`: `Avatars.canUse`'s cleaning (lowercase letters,
    /// digits, `-`, `.`, `#`). Showdown also checks the name against its
    /// avatar list, which this server doesn't have: any clean name up to
    /// 32 characters goes.
    fn avatar(&mut self, key: UserKey, room: Option<&str>, target: &str) {
        let (avatar, _) = split_first(target, ',');
        let avatar: String = avatar
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '#'))
            .collect();
        let avatar = avatar.trim_start_matches('#');
        if avatar.is_empty() || avatar.len() > 32 {
            return self.error_reply(key, room, "Unrecognized avatar - make sure you're on the right account?");
        }
        if let Some(user) = self.users.get_mut(&key) {
            user.avatar = avatar.to_string();
            let text = user.updateuser();
            self.send_user(key, &text);
        }
    }

    /// `/trn name,registered,assertion` without authentication (Showdown's
    /// `noguestsecurity`): `User.rename`, `handleRename`, `forceRename`.
    fn rename(&mut self, key: UserKey, conn: ConnId, target: &str, now: u64) {
        let Some(user) = self.users.get(&key) else { return };
        if target == user.name {
            return;
        }
        let (raw, _) = split_first(target, ',');
        let mut id = to_id(raw);
        if id != user.id {
            // A named player can't change their name during a battle that
            // doesn't allow renaming (a rated one), and no player can once
            // they have renamed as often as a battle takes (each rename
            // logs a line in every battle they are in).
            for roomid in &user.games {
                let Some(room) = self.rooms.get(roomid).filter(|r| !r.ended) else { continue };
                if (room.allows_renames() || !user.named) && !room.renames_used_up(key) {
                    continue;
                }
                let text = format!(
                    "You can't change your name right now because you're in {}, which doesn't allow renaming.",
                    room.format.battle_title()
                );
                return self.send_user(key, &popup(&text));
            }
        }
        if !raw.chars().any(|c| c.is_ascii_alphabetic()) {
            return self.send_conn(conn, "|nametaken||Your name must contain at least one letter.");
        }
        if id.len() > 18 {
            return self.send_conn(conn, "|nametaken||Your name must be 18 characters or shorter.");
        }
        let mut name = get_name(raw);
        if id != to_id(&name) {
            if name.is_empty() {
                id.clear();
            } else {
                name = id.clone();
            }
        }
        if id.is_empty() {
            return self.send_conn(conn, "|nametaken||Your name contains a banned word.");
        }
        if let Some(&other) = self.ids.get(&id).filter(|&&k| k != key) {
            let taken = self.user(other).name.clone();
            return self.send_conn(conn, format!("|nametaken|{name}|Someone is already using the name \"{taken}\"."));
        }
        self.force_rename(key, name, id, now);
    }

    /// `forceRename`: a new id cancels the user's searches and challenges
    /// (`cancelReady`); an id starting with `guest` is a guest's; the
    /// user's battles follow the rename (`RoomBattle.onRename`).
    fn force_rename(&mut self, key: UserKey, name: String, id: String, now: u64) {
        let old_id = self.user(key).id.clone();
        if id != old_id {
            self.cancel_ready(key, &old_id);
            if self.ids.get(&old_id) == Some(&key) {
                self.ids.remove(&old_id);
            }
            self.ids.insert(id.clone(), key);
        }
        let Some(user) = self.users.get_mut(&key) else { return };
        user.id = id;
        user.name = name;
        user.named = !user.id.starts_with("guest");
        let text = user.updateuser();
        let games = user.games.clone();
        self.send_user(key, &text);
        for roomid in games {
            let (mut ctx, rooms) = self.ctx(now);
            if let (Some(room), Some(user)) = (rooms.get_mut(&roomid), ctx.users.get(&key)) {
                room.on_rename(user, &old_id, &mut ctx);
            }
            self.after_room(&roomid, now);
        }
    }

    /// `cancelReady`: a renamed user's searches and challenges end.
    fn cancel_ready(&mut self, key: UserKey, old_id: &str) {
        let before = self.searches.len();
        self.searches.retain(|s| s.user != key);
        let searches = before - self.searches.len();
        self.update_search(key);
        let challenges = self.clear_challenges(old_id, "they changed their username");
        if searches + challenges > 0 {
            self.send_user(
                key,
                &popup("Your searches and challenges have been cancelled because you changed your username."),
            );
        }
    }

    // -----------------------------------------------------------------
    // Matchmaking

    /// `Ladder.updateSearch`: `|updatesearch|`.
    fn update_search(&mut self, key: UserKey) {
        let Some(user) = self.users.get(&key) else { return };
        let searching: Vec<Json> =
            self.searches.iter().filter(|s| s.user == key).map(|s| Json::str(s.format.id)).collect();
        let mut games = Json::object();
        for room in user.games.iter().filter_map(|id| self.rooms.get(id)) {
            let star = if room.rated > 0.0 { "*" } else { "" };
            games.set(&room.id, Json::str(format!("{}{star}", room.format.battle_title())));
        }
        let mut search = Json::object();
        search.set("searching", Json::Array(searching));
        search.set("games", if games == Json::object() { Json::Null } else { games });
        let text = format!("|updatesearch|{}", search.stringify());
        self.send_user(key, &text);
    }

    /// A random battle's team: none, so that the battle's generator draws
    /// it (`Battle.getTeam`, Showdown's generator ported exactly), as on
    /// Showdown, where a user's own team is ignored. `--seed` gives each
    /// player a seed for it, so the teams are reproducible.
    fn random_team(&mut self, key: UserKey, format: &'static ShowdownFormat, rated: bool) -> Option<Ready> {
        self.team_draws += 1;
        let team_seed = self
            .config
            .seed
            .map(|s| crate::util::sodium_seed(s.wrapping_add(self.team_draws.wrapping_mul(0x9E37_79B9))));
        let rating = if rated { self.rating(format.id, &self.user(key).id) } else { 0.0 };
        Some(Ready { user: key, format, team: String::new(), team_seed, rating })
    }

    /// `Ladder.prepBattle`: the user's team, validated for `format`.
    fn prep_battle(
        &mut self,
        key: UserKey,
        conn: ConnId,
        format: &'static ShowdownFormat,
        rated: bool,
    ) -> Option<Ready> {
        if format.format().random_teams {
            return self.random_team(key, format, rated);
        }
        let team = self.user(key).team.clone();
        let reject = |problems: &[String]| {
            popup(&format!("Your team was rejected for the following reasons:\n\n- {}", problems.join("\n- ")))
        };
        if team.is_empty() || team == "null" {
            let problems = [
                "This format requires you to use your own team.".to_string(),
                "If you're not using a custom client, please report this as a bug.".to_string(),
            ];
            self.send_conn(conn, reject(&problems));
            return None;
        }
        let core = format.format();
        let mut sheet: TeamSheet = match pikapro_protocol::unpack_team(&team, core) {
            Ok(sheet) => sheet,
            Err(e) => {
                let problems = [format!(
                    "You sent invalid team data. If you're not using a custom client, please report this as a bug. ({e})"
                )];
                self.send_conn(conn, reject(&problems));
                return None;
            }
        };
        if self.config.validate
            && let Some(validation) = format.validation.and_then(ValidationFormat::by_id)
        {
            let validator = Validator::new(validation);
            let info: Vec<SetInfo> =
                sheet.team.sets.iter().zip(&sheet.extras).map(|(s, e)| SetInfo::new(s, &e.name, e.shiny)).collect();
            // A panic in the validator rejects the team with Showdown's
            // message for a validator crash (`team-validator-async.ts`).
            let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let checked = validator.check(&sheet.team, &info);
                if checked.problems.is_empty() {
                    Ok((checked.team, checked.names))
                } else {
                    Err(validator.messages(&checked.problems))
                }
            }))
            .unwrap_or_else(|_| Err(vec![VALIDATOR_CRASHED.to_string()]));
            match checked {
                // The sets as the validator leaves them: a listed
                // battle-only forme (`Floette-Mega @ Floettite`) starts as
                // the forme it changes from (`validateForme`), and each
                // nickname is cleaned (`set.name = dex.getName(set.name)`).
                Ok((team, names)) => {
                    sheet.team = team;
                    for (extras, name) in sheet.extras.iter_mut().zip(names) {
                        extras.name = name;
                    }
                }
                Err(messages) => {
                    self.send_conn(conn, reject(&messages));
                    return None;
                }
            }
        } else if let Err(problems) = clean_nicknames(&mut sheet, core, self.config.validate) {
            self.send_conn(conn, reject(&problems));
            return None;
        }
        // What the engine needs to build the battle (the stream's format:
        // one to six Pokémon, levels up to 100, no clauses).
        if let Err(e) = sheet.team.validate(core) {
            self.send_conn(conn, reject(&[e.to_string()]));
            return None;
        }
        // The battle gets the team as the validator leaves it, packed again
        // (`Teams.pack(parsedTeam)`): the simulator plays what it is given.
        // Besides the species, ability, moves and level (above),
        // `validateSet` makes a set without a gender genderless in a Gen 6+
        // format without `Obtainable Misc` (the custom games: `set.gender ||=
        // 'N'`), so the simulator draws none.
        if format.validation.is_none() {
            for set in &mut sheet.team.sets {
                set.gender.get_or_insert(Gender::Genderless);
            }
        }
        let team = pikapro_protocol::pack_team(&sheet, core);
        let rating = if rated { self.rating(format.id, &self.user(key).id) } else { 0.0 };
        Some(Ready { user: key, format, team, team_seed: None, rating })
    }

    fn rating(&self, format: &str, id: &str) -> f64 {
        self.ratings.get(&(format.to_string(), id.to_string())).map_or(1000.0, |&(r, _)| r)
    }

    /// Stores a rating; a new one past [`MAX_RATINGS`] replaces the one
    /// updated least recently.
    fn set_rating(&mut self, format: &str, id: &str, rating: f64) {
        let key = (format.to_string(), id.to_string());
        if !self.ratings.contains_key(&key)
            && self.ratings.len() >= self.max_ratings
            && let Some(stalest) = self.ratings.iter().min_by_key(|&(_, &(_, at))| at).map(|(k, _)| k.clone())
        {
            self.ratings.remove(&stalest);
        }
        self.rating_updates += 1;
        self.ratings.insert(key, (rating, self.rating_updates));
    }

    /// `/search FORMAT` (`searchBattle` + `addSearch`). Every format here
    /// is ladderable, custom games included.
    fn search(&mut self, key: UserKey, conn: ConnId, target: &str, now: u64) {
        let Some(format) = formats::find(target) else {
            return self.send_conn(conn, popup(&format!("Error: Your format {} is not ladderable.", to_id(target))));
        };
        if !self.may_start_battle(key, conn) {
            return;
        }
        let Some(ready) = self.prep_battle(key, conn, format, true) else { return };
        if self.searches.iter().any(|s| s.user == key && s.format.id == format.id) {
            let text = popup(&format!("Couldn't search: You are already searching for a {} battle.", format.id));
            return self.send_conn(conn, text);
        }
        if let Some(pos) =
            self.searches.iter().position(|s| s.format.id == format.id && s.user != key && !self.at_battle_cap(s.user))
        {
            let other = self.searches.remove(pos);
            self.start_battle([ready, other], false, now);
            return;
        }
        self.searches.push(ready);
        self.update_search(key);
    }

    /// `/cancelsearch [FORMAT]`.
    fn cancel_searches(&mut self, key: UserKey, format: Option<&str>) {
        let format = format.map(to_id);
        let before = self.searches.len();
        self.searches.retain(|s| s.user != key || format.as_ref().is_some_and(|f| *f != s.format.id));
        if format.is_none() || self.searches.len() != before {
            self.update_search(key);
        }
    }

    fn challenge_between(&self, a: &str, b: &str) -> Option<usize> {
        self.challenges.iter().position(|c| (c.from == a && c.to == b) || (c.from == b && c.to == a))
    }

    /// `Challenges.update` / `send`: `|pm|` with the challenge's state.
    fn challenge_update(&mut self, a: &str, b: &str) {
        let (from, to, message) = match self.challenge_between(a, b) {
            Some(i) => {
                let c = &self.challenges[i];
                let f = c.ready.format.id;
                (c.from.clone(), c.to.clone(), format!("/challenge {f}|{f}|||"))
            }
            None => (a.to_string(), b.to_string(), "/challenge".to_string()),
        };
        self.pm(&from, &to, &message);
    }

    /// A PM between two users, sent to both.
    fn pm(&mut self, from: &str, to: &str, message: &str) {
        let text = format!("|pm|{}|{}|{message}", self.identity_of(from), self.identity_of(to));
        for id in [from, to] {
            if let Some(&k) = self.ids.get(id) {
                self.send_user(k, &text);
            }
        }
    }

    /// `/challenge USER, FORMAT` (`makeChallenge`).
    fn challenge(&mut self, key: UserKey, conn: ConnId, target: &str, now: u64) {
        let (name, format_name) = split_first(target, ',');
        let (name, format_name) = (name.trim(), format_name.trim());
        let Some(other) = self.find_user(name) else {
            return self.send_conn(conn, popup(&format!("The user '{name}' was not found.")));
        };
        if !self.user(key).named {
            return self.send_conn(conn, popup("You must choose a username before you challenge someone."));
        }
        let Some(format) = formats::find(format_name) else {
            let text = format!("Your selected format is invalid:\n\n- Unrecognized format \"{format_name}\"");
            return self.send_conn(conn, popup(&text));
        };
        if other == key {
            let text = "You can't battle yourself. The best you can do is open PS in Private Browsing (or another browser) and log into a different username, and battle that username.";
            return self.send_conn(conn, popup(text));
        }
        let (me, them) = (self.user(key).id.clone(), self.user(other).id.clone());
        if self.challenges.iter().any(|c| c.from == me) {
            let text = "You are already challenging someone. Cancel that challenge before challenging someone else.";
            return self.send_conn(conn, popup(text));
        }
        if !self.may_start_battle(key, conn) {
            return;
        }
        let Some(ready) = self.prep_battle(key, conn, format, false) else { return };
        if let Some(i) = self.challenge_between(&me, &them) {
            let c = &self.challenges[i];
            if c.from == them && c.to == me && c.ready.format.id == format.id {
                if !self.opponent_may_start_battle(other, conn) {
                    return;
                }
                let c = self.challenges.remove(i);
                self.challenge_update(&them, &me);
                self.start_battle([c.ready, ready], true, now);
            } else {
                let text = format!(
                    "There's already a challenge ({}) between you and {}!",
                    c.ready.format.id,
                    self.user(other).name
                );
                self.send_conn(conn, popup(&text));
                self.challenge_update(&me, &them);
            }
            return;
        }
        self.challenges.push(Challenge { from: me.clone(), to: them.clone(), ready });
        self.challenge_update(&them, &me);
        let text = format!("/log {} wants to battle!", self.user(key).name);
        self.pm(&me, &them, &text);
    }

    fn pm_target(&mut self, conn: ConnId, target: &str) -> Option<UserKey> {
        let (name, rest) = split_first(target, ',');
        if !rest.trim().is_empty() {
            self.send_conn(conn, popup("This command does not support specifying multiple users"));
            return None;
        }
        let found = self.find_user(name);
        if found.is_none() {
            self.send_conn(conn, popup(&format!("User \"{}\" not found.", name.trim())));
        }
        found
    }

    /// `/cancelchallenge USER`.
    fn cancel_challenge(&mut self, key: UserKey, conn: ConnId, target: &str) {
        let Some(other) = self.pm_target(conn, target) else { return };
        let (me, them) = (self.user(key).id.clone(), self.user(other).id.clone());
        let Some(i) = self.challenge_between(&me, &them).filter(|&i| self.challenges[i].from == me) else {
            let text = format!(
                "You are not challenging {}. Maybe they accepted/rejected before you cancelled?",
                self.user(other).name
            );
            return self.send_conn(conn, popup(&text));
        };
        let text = format!("/log {} cancelled the challenge.", self.user(key).name);
        self.pm(&me, &them, &text);
        self.challenges.remove(i);
        self.challenge_update(&them, &me);
    }

    /// `/reject USER`.
    fn reject(&mut self, key: UserKey, conn: ConnId, target: &str) {
        let Some(other) = self.pm_target(conn, target) else { return };
        let (me, them) = (self.user(key).id.clone(), self.user(other).id.clone());
        let Some(i) = self.challenge_between(&me, &them).filter(|&i| self.challenges[i].to == me) else {
            let text = format!("{them} is not challenging you. Maybe they cancelled before you rejected?");
            return self.send_conn(conn, popup(&text));
        };
        let text = format!("/nonotify {} rejected the challenge.", self.user(key).name);
        self.pm(&me, &them, &text);
        self.challenges.remove(i);
        self.challenge_update(&me, &them);
    }

    /// `/accept USER`.
    fn accept(&mut self, key: UserKey, conn: ConnId, target: &str, now: u64) {
        let Some(other) = self.pm_target(conn, target) else { return };
        let (me, them) = (self.user(key).id.clone(), self.user(other).id.clone());
        let found = self.challenge_between(&me, &them).filter(|&i| self.challenges[i].to == me);
        // The challenger is the user now holding their id.
        let Some(i) = found.filter(|&i| self.challenges[i].ready.user == other) else {
            let text = format!("{them} is not challenging you. Maybe they cancelled before you accepted?");
            return self.send_conn(conn, popup(&text));
        };
        let format = self.challenges[i].ready.format;
        if !self.may_start_battle(key, conn) || !self.opponent_may_start_battle(other, conn) {
            return;
        }
        let Some(ready) = self.prep_battle(key, conn, format, false) else { return };
        let c = self.challenges.remove(i);
        self.challenge_update(&me, &them);
        let Some(roomid) = self.create_battle([c.ready, ready], true, now) else { return };
        let text = format!(
            "/nonotify {} accepted the challenge, starting &laquo;<a href=\"/{roomid}\">{roomid}</a>&raquo;",
            escape_html(&self.user(key).name)
        );
        self.pm(&me, &them, &text);
        self.begin_battle(&roomid, now);
    }

    // -----------------------------------------------------------------
    // Battles

    /// Whether `key` plays as many unfinished battles as
    /// [`Config::max_battles_per_user`] allows. Counted by the user's id
    /// over every room's seats, so a reconnect under the same name keeps
    /// its count.
    fn at_battle_cap(&self, key: UserKey) -> bool {
        let max = self.config.max_battles_per_user;
        if max == 0 {
            return false;
        }
        let id = &self.user(key).id;
        let playing = self.rooms.values().filter(|r| !r.ended && r.players.iter().any(|p| p.id == *id)).count();
        playing >= max
    }

    /// Whether `key` may start another battle; if not, a popup tells `conn`.
    fn may_start_battle(&mut self, key: UserKey, conn: ConnId) -> bool {
        if !self.at_battle_cap(key) {
            return true;
        }
        let max = self.config.max_battles_per_user;
        let text = format!("You can't start another battle: this server allows {max} at once per user.");
        self.send_conn(conn, popup(&text));
        false
    }

    /// Whether `other`, the opponent `conn`'s user would start a battle
    /// with, may start another; if not, a popup tells `conn`.
    fn opponent_may_start_battle(&mut self, other: UserKey, conn: ConnId) -> bool {
        if !self.at_battle_cap(other) {
            return true;
        }
        let text = format!(
            "{} can't start another battle now: they are playing the most this server allows.",
            self.user(other).name
        );
        self.send_conn(conn, popup(&text));
        false
    }

    /// `Ladder.match` for a search or a challenge that accepts itself.
    fn start_battle(&mut self, readies: [Ready; 2], challenge: bool, now: u64) {
        if let Some(roomid) = self.create_battle(readies, challenge, now) {
            self.begin_battle(&roomid, now);
        }
    }

    /// `Rooms.createBattle`: the room, the players' `|updatesearch|`, and
    /// both players joining it. `None` if a player is gone.
    fn create_battle(&mut self, readies: [Ready; 2], challenge: bool, now: u64) -> Option<String> {
        let [a, b] = &readies;
        let (Some(ua), Some(ub)) = (self.users.get(&a.user), self.users.get(&b.user)) else {
            crate::net::log("a battle between users who are gone was not started");
            return None;
        };
        let players = [
            Player::new(ua, a.team.clone(), a.team_seed.clone(), a.rating),
            Player::new(ub, b.team.clone(), b.team_seed.clone(), b.rating),
        ];
        let format = a.format;
        self.battles += 1;
        let roomid = format!("battle-{}-{}", format.id, self.battles);
        let rated = if challenge { 0.0 } else { a.rating.min(b.rating) };
        let room = BattleRoom::new(roomid.clone(), format, players, rated, challenge, (self.config.stream)());
        for r in &readies {
            self.searches.retain(|s| s.user != r.user);
            self.update_search(r.user);
        }
        self.rooms.insert(roomid.clone(), room);
        for r in &readies {
            if let Some(user) = self.users.get_mut(&r.user) {
                user.games.push(roomid.clone());
            }
            // Showdown sends it twice: from the player's creation and from
            // joining the room.
            self.update_search(r.user);
            self.update_search(r.user);
        }
        for r in &readies {
            self.join(r.user, &roomid, now);
        }
        Some(roomid)
    }

    /// Writes `>start` and the players to the room's stream.
    fn begin_battle(&mut self, roomid: &str, now: u64) {
        let seed = self.config.seed.map(|s| crate::util::sodium_seed(s.wrapping_add(self.battles)));
        let (mut ctx, rooms) = self.ctx(now);
        if let Some(room) = rooms.get_mut(roomid) {
            room.start(&mut ctx, seed.as_deref());
        }
        self.after_room(roomid, now);
    }

    /// `/join ROOM`.
    fn join(&mut self, key: UserKey, roomid: &str, now: u64) {
        let roomid = roomid.trim();
        let Some(room) = self.rooms.get_mut(roomid) else {
            let message = format!(">{roomid}\n|noinit|nonexistent|The room \"{roomid}\" does not exist.");
            return self.send_user(key, &message);
        };
        let Some(id) = self.users.get(&key).map(|u| u.id.clone()) else { return };
        // A player coming back under their id takes their seat, if the
        // user it was bound to is gone (ids are unique, so that user can't
        // be displaced while online).
        let mut rejoined = false;
        if let Some(i) = room.player_by_id(&id) {
            let bound = room.players[i].user;
            if bound != Some(key) && bound.is_none_or(|k| !self.users.contains_key(&k)) {
                room.players[i].user = Some(key);
                rejoined = !room.ended;
            }
        }
        if let Some(user) = self.users.get_mut(&key) {
            if !user.rooms.iter().any(|r| r == roomid) {
                user.rooms.push(roomid.to_string());
            }
            if rejoined && !user.games.iter().any(|g| g == roomid) {
                user.games.push(roomid.to_string());
            }
        }
        let (mut ctx, rooms) = self.ctx(now);
        if let Some(room) = rooms.get_mut(roomid) {
            room.join(key, &mut ctx);
        }
    }

    /// `/leave ROOM`.
    fn leave(&mut self, key: UserKey, roomid: &str, now: u64) {
        if let Some(user) = self.users.get_mut(&key) {
            user.rooms.retain(|r| r != roomid);
        }
        let (mut ctx, rooms) = self.ctx(now);
        if let (Some(room), Some(user)) = (rooms.get_mut(roomid), ctx.users.get(&key)) {
            room.leave(user, &mut ctx);
        }
        self.drop_finished_rooms();
    }

    /// A command for a battle room.
    fn room_command(&mut self, key: UserKey, roomid: &str, cmd: &str, target: &str, now: u64) {
        let (mut ctx, rooms) = self.ctx(now);
        let Some(room) = rooms.get_mut(roomid) else { return };
        match cmd {
            "choose" | "guess" => room.choose(key, target, &mut ctx),
            "move" | "mv" | "attack" => room.choose(key, &format!("move {target}"), &mut ctx),
            "switch" | "sw" => room.choose(key, &format!("switch {target}"), &mut ctx),
            "team" => room.choose(key, &format!("team {target}"), &mut ctx),
            "undo" => room.undo(key, target, &mut ctx),
            "forfeit" => {
                if let Some(i) = room.player_of(key) {
                    room.forfeit(i, "", &mut ctx);
                }
            }
            "timer" => room.timer_command(key, target, &mut ctx),
            "acceptopenteamsheets" => room.accept_open_team_sheets(key, &mut ctx),
            "rejectopenteamsheets" => room.reject_open_team_sheets(key, &mut ctx),
            _ => {}
        }
        self.after_room(roomid, now);
    }

    /// After a room ran: if its battle just ended, the players' games and
    /// `|updatesearch|`, the ladder, and the finished rooms past the caps.
    fn after_room(&mut self, roomid: &str, now: u64) {
        if self.settle(roomid, now) {
            self.expire_surplus_rooms(roomid);
        }
    }

    /// [`Hub::after_room`] but for the caps; whether the battle just ended.
    fn settle(&mut self, roomid: &str, now: u64) -> bool {
        let Some(room) = self.rooms.get_mut(roomid) else { return false };
        let Some(ending) = room.take_ending() else { return false };
        room.ended_at = Some(now);
        let players: Vec<(Option<UserKey>, String, String)> =
            room.players.iter().map(|p| (p.user, p.id.clone(), p.name.clone())).collect();
        let rated = room.rated > 0.0;
        let format = room.format.id;
        for (user, _, _) in &players {
            if let Some(key) = *user {
                if let Some(u) = self.users.get_mut(&key) {
                    u.games.retain(|g| g != roomid);
                }
                self.update_search(key);
            }
        }
        if !rated || ending.crashed {
            return true;
        }
        // `updateLadder`: `|askreg|` for an unregistered winner, then the
        // new ratings (`LadderStore.updateRating`).
        if let Some(&(Some(key), _, _)) = players.iter().find(|(_, id, _)| *id == ending.winner) {
            let message = format!(">{roomid}\n|askreg|{}", ending.winner);
            self.send_user(key, &message);
        }
        // `RoomBattle.end`: a winner who is neither player is a tie.
        let p1score = if ending.winner == players[0].1 {
            1.0
        } else if ending.winner == players[1].1 {
            0.0
        } else {
            0.5
        };
        let old = [self.rating(format, &players[0].1), self.rating(format, &players[1].1)];
        let new = [elo(old[0], p1score, old[1]), elo(old[1], 1.0 - p1score, old[0])];
        let mut lines = Vec::new();
        for (i, score) in [p1score, 1.0 - p1score].into_iter().enumerate() {
            self.set_rating(format, &players[i].1, new[i]);
            let delta = new[i].round() as i64 - old[i].round() as i64;
            let result = if score > 0.9 {
                "winning"
            } else if score < 0.1 {
                "losing"
            } else {
                "tying"
            };
            let sign = if delta < 0 { "" } else { "+" };
            lines.push(format!(
                "|raw|{}'s rating: {} &rarr; <strong>{}</strong><br />({sign}{delta} for {result})",
                escape_html(&players[i].2),
                old[i].round() as i64,
                new[i].round() as i64
            ));
        }
        let (mut ctx, rooms) = self.ctx(now);
        if let Some(room) = rooms.get_mut(roomid) {
            room.add_and_update(&lines, &mut ctx);
        }
        true
    }
}

/// The nicknames as Showdown's validator leaves them, for a team checked
/// without `pikapro-validate`'s rules (the custom games, `--no-validate`):
/// Showdown's server validates every team, and `validateSet` cleans each
/// nickname ([`pikapro_validate::nickname`]: `Dex.getName`, so no `|`,
/// `,`, `[`, `]`, U+202E or white space runs such as `\r`, `\n` and U+2028,
/// at most 18 characters; the base species' name for none or for one that
/// names a species of the same base species). With `strict`, a nickname
/// over 18 UTF-16 units is a problem, as there.
fn clean_nicknames(sheet: &mut TeamSheet, format: &pikapro::Format, strict: bool) -> Result<(), Vec<String>> {
    let dex = format.dex;
    let mut problems = Vec::new();
    for (set, extras) in sheet.team.sets.iter().zip(sheet.extras.iter_mut()) {
        let cleaned = pikapro_validate::nickname(dex, set.species, set.species, &extras.name);
        if let Some(len) = cleaned.too_long.filter(|_| strict) {
            problems.push(format!("{}'s nickname \"{}\" is too long.", dex.species(set.species).name, extras.name));
            problems.push(format!(
                "(It's {len} characters long, but should be 18 or less. Some characters, like emojis, may count as more than one.)"
            ));
        }
        extras.name = cleaned.name;
    }
    if problems.is_empty() { Ok(()) } else { Err(problems) }
}

/// `LadderStore.calculateElo`.
fn elo(old: f64, score: f64, foe: f64) -> f64 {
    let mut k = 50.0;
    if old < 1200.0 {
        if score < 0.5 {
            k = 10.0 + (old - 1000.0) * 40.0 / 200.0;
        } else if score > 0.5 {
            k = 90.0 - (old - 1000.0) * 40.0 / 200.0;
        }
    } else if old > 1350.0 && old <= 1600.0 {
        k = 40.0;
    } else {
        k = 32.0;
    }
    let expected = 1.0 / (1.0 + 10f64.powf((foe - old) / 400.0));
    (old + k * (score - expected)).max(1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elo_matches_showdown() {
        // Showdown's `LadderStore.calculateElo`: 1000 vs 1000, win: +45;
        // then 1045 beats 1000: +35.
        assert_eq!(elo(1000.0, 1.0, 1000.0).round(), 1045.0);
        assert_eq!(elo(1000.0, 0.0, 1000.0).round(), 1000.0);
        assert_eq!(elo(1045.0, 1.0, 1000.0).round(), 1080.0);
    }

    fn hub() -> Hub {
        let config = Config { validate: false, disconnect_grace_ms: 1000, seed: Some(1), ..Config::default() };
        Hub::new(config)
    }

    /// Past the cap, a new rating replaces the one updated least recently.
    #[test]
    fn ratings_are_bounded() {
        let mut hub = hub();
        hub.max_ratings = 2;
        hub.set_rating("f", "a", 1100.0);
        hub.set_rating("f", "b", 1200.0);
        hub.set_rating("f", "a", 1150.0);
        hub.set_rating("f", "c", 1300.0);
        assert_eq!(hub.ratings.len(), 2);
        assert_eq!([hub.rating("f", "a"), hub.rating("f", "b"), hub.rating("f", "c")], [1150.0, 1000.0, 1300.0]);
    }

    /// A user's lines past their budget are dropped with Showdown's notice,
    /// with the rest of the message; the budget refills with time.
    #[test]
    fn lines_are_throttled() {
        let mut hub = hub();
        hub.config.max_lines_per_second = 2;
        hub.connect(1);
        hub.take_outbox();
        hub.message(1, "|/cmd a\n/cmd b\n/cmd c", 0);
        let out: Vec<String> = hub.take_outbox().into_iter().map(|(_, m)| m).collect();
        assert_eq!(out.len(), 3, "{out:?}");
        assert!(out[2].contains("you've been typing too quickly"), "{out:?}");
        hub.message(1, "|/cmd d", 400);
        assert!(hub.take_outbox()[0].1.contains("typing too quickly"), "0.8 lines refilled");
        hub.message(1, "|/cmd e", 500);
        assert_eq!(hub.take_outbox()[0].1, "|queryresponse|e|null");
    }

    /// A panic half-way through a disconnection (the connection gone, the
    /// user not): the crash guard's recovery removes the user, so their
    /// name is free again.
    #[test]
    fn recovery_forgets_users_without_connections() {
        let mut hub = hub();
        hub.connect(1);
        hub.message(1, "|/trn alice,0,", 0);
        let key = hub.conns[&1];
        hub.conns.remove(&1);
        hub.users.get_mut(&key).unwrap().conns.clear();
        hub.recover(0);
        assert_eq!(hub.user_count(), 0);
        assert!(hub.ids.is_empty() && hub.conns.is_empty());
        hub.connect(2);
        hub.take_outbox();
        hub.message(2, "|/trn alice,0,", 0);
        assert!(hub.take_outbox().iter().any(|(_, m)| m.starts_with("|updateuser| alice|1|")));
        // A consistent state is left alone.
        hub.recover(0);
        assert_eq!((hub.user_count(), hub.ids.len()), (1, 1));
    }
}
