//! The server's protocol without the network: clients talking to a [`Hub`].

use std::collections::HashMap;

use pikapro::SideId;
use pikapro_server::{BattleStream, Message, StreamError};
use pikapro_showdown_server::{
    Config, Hub, RoomStream, StreamFactory, formats, room_battle_stream, room_battle_streams,
};

const TEAMS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/python/teams");

/// A team file, packed as poke-env packs it.
fn packed(format: &str) -> String {
    let file = if format.contains("champions") { "gen9championsvgc2026regmc" } else { "gen9vgc2025regi" };
    let text = std::fs::read_to_string(format!("{TEAMS}/{file}.txt")).unwrap();
    let core = formats::find(format).unwrap().format();
    let sheet = pikapro_protocol::import_sheet(&text, core).unwrap();
    pikapro_protocol::pack_team(&sheet, core)
}

struct Server {
    hub: Hub,
    now: u64,
    /// Frames not read yet, per connection.
    inbox: HashMap<u64, Vec<String>>,
    /// Every frame, per connection.
    history: HashMap<u64, Vec<String>>,
}

impl Server {
    fn new() -> Server {
        Server::with_streams(room_battle_streams())
    }

    fn with_streams(stream: StreamFactory) -> Server {
        Server::with_config(Config { stream, seed: Some(2_200_000), ..Config::default() })
    }

    fn with_config(config: Config) -> Server {
        Server { hub: Hub::new(config), now: 0, inbox: HashMap::new(), history: HashMap::new() }
    }

    fn pump(&mut self) {
        for (conn, message) in self.hub.take_outbox() {
            self.history.entry(conn).or_default().push(message.clone());
            self.inbox.entry(conn).or_default().push(message);
        }
    }

    fn send(&mut self, conn: u64, message: &str) {
        self.hub.message(conn, message, self.now);
        self.pump();
    }

    /// Connects and logs in as `name`.
    fn login(&mut self, conn: u64, name: &str) {
        self.hub.connect(conn);
        self.pump();
        self.send(conn, &format!("|/trn {name},0,"));
    }

    fn take(&mut self, conn: u64) -> Vec<String> {
        std::mem::take(self.inbox.entry(conn).or_default())
    }

    fn tick(&mut self, ms: u64) {
        self.now += ms;
        self.hub.tick(self.now);
        self.pump();
    }
}

/// The room and rqid of the last request in `frames`, unless it is a wait.
fn last_request(frames: &[String]) -> Option<(String, String)> {
    frames.iter().rev().find_map(|f| {
        let (room, rest) = f.strip_prefix('>')?.split_once('\n')?;
        let json = rest.strip_prefix("|request|")?;
        if json.starts_with("{\"wait\":true") {
            return Some((room.to_string(), String::new()));
        }
        let rqid = json.rsplit_once("\"rqid\":")?.1.trim_end_matches('}').to_string();
        Some((room.to_string(), rqid))
    })
}

/// Whether a battle has ended in these frames.
fn over(frames: &[String]) -> bool {
    frames.iter().any(|f| f.lines().any(|l| l.starts_with("|win|") || l == "|tie"))
}

/// Plays the players' battle to the end with `/choose default`; returns
/// each side's frames (since the start of the connection).
fn play_out(server: &mut Server, conns: [u64; 2]) -> [Vec<String>; 2] {
    let mut answered = [String::new(), String::new()];
    for _ in 0..5000 {
        let mut progressed = false;
        for (i, &conn) in conns.iter().enumerate() {
            server.take(conn);
            let history = server.history.get(&conn).cloned().unwrap_or_default();
            if over(&history) {
                continue;
            }
            if let Some((room, rqid)) = last_request(&history)
                && !rqid.is_empty()
                && rqid != answered[i]
            {
                answered[i] = rqid.clone();
                server.send(conn, &format!("{room}|/choose default|{rqid}"));
                progressed = true;
            }
        }
        let all = conns.map(|c| server.history.get(&c).cloned().unwrap_or_default());
        if all.iter().all(|h| over(h)) {
            for c in conns {
                server.take(c);
            }
            return all;
        }
        assert!(progressed, "the battle is stuck");
    }
    panic!("the battle did not end");
}

#[test]
fn connection_and_login() {
    let mut s = Server::new();
    s.hub.connect(1);
    s.pump();
    let frames = s.take(1);
    assert_eq!(frames.len(), 2);
    let hello: Vec<&str> = frames[0].split('\n').collect();
    assert!(hello[0].starts_with("|updateuser| Guest 1|0|"), "{}", hello[0]);
    assert!(hello[1].starts_with("|customgroups|["));
    assert!(hello[2].starts_with("|formats|,LL|,1|pikapro|[Gen 9 Champions] VGC 2026 Reg M-C,"));
    assert!(frames[1].starts_with("|challstr|4|") && frames[1].len() == 12 + 256);

    s.send(1, "|/trn Ash K,0,");
    let frames = s.take(1);
    assert_eq!(frames[0], "|updatesearch|{\"searching\":[],\"games\":null}");
    assert!(frames[1].starts_with("|updateuser| Ash K|1|"));

    s.hub.connect(2);
    s.pump();
    s.take(2);
    s.send(2, "|/trn ash k,0,");
    assert_eq!(s.take(2), ["|nametaken|ash k|Someone is already using the name \"Ash K\"."]);
    s.send(2, "|/trn 123,0,");
    assert_eq!(s.take(2), ["|nametaken||Your name must contain at least one letter."]);
}

#[test]
fn challenges() {
    let mut s = Server::new();
    s.login(1, "alice");
    s.login(2, "bob");
    s.take(1);
    s.take(2);
    s.send(1, "|/challenge nobody, gen9customgame");
    assert_eq!(s.take(1), ["|popup|The user 'nobody' was not found."]);
    s.send(1, "|/challenge bob, gen9notaformat");
    assert_eq!(s.take(1), ["|popup|Your selected format is invalid:||||- Unrecognized format \"gen9notaformat\""]);
    s.send(1, "|/utm null");
    s.send(1, "|/challenge bob, gen9customgame");
    assert!(
        s.take(1)[0].starts_with("|popup|Your team was rejected for the following reasons:||||- This format requires")
    );

    s.send(1, &format!("|/utm {}", packed("gen9customgame")));
    s.send(1, "|/challenge bob, gen9customgame");
    let expected =
        ["|pm| alice| bob|/challenge gen9customgame|gen9customgame|||", "|pm| alice| bob|/log alice wants to battle!"];
    assert_eq!(s.take(1), expected);
    assert_eq!(s.take(2), expected);
    s.send(2, "|/reject alice");
    let expected = ["|pm| bob| alice|/nonotify bob rejected the challenge.", "|pm| bob| alice|/challenge"];
    assert_eq!(s.take(1), expected);

    s.send(1, "|/challenge bob, gen9customgame");
    s.take(2);
    s.send(1, "|/cancelchallenge bob");
    let frames = s.take(2);
    assert_eq!(frames, ["|pm| alice| bob|/log alice cancelled the challenge.", "|pm| bob| alice|/challenge"]);
}

#[test]
fn battles_in_every_format() {
    for (n, format) in ["gen9championsvgc2026regmc", "gen9vgc2025regi", "gen9customgame", "gen9doublescustomgame"]
        .into_iter()
        .enumerate()
    {
        let mut s = Server::new();
        let (a, b) = (10 * n as u64 + 1, 10 * n as u64 + 2);
        s.login(a, "alice");
        s.login(b, "bob");
        s.send(a, &format!("|/utm {}", packed(format)));
        s.send(b, &format!("|/utm {}", packed(format)));
        s.send(a, &format!("|/challenge bob, {format}"));
        s.take(a);
        s.take(b);
        s.send(b, "|/accept alice");
        let first = s.take(a);
        let room = format!("battle-{format}-1");
        assert!(
            first.iter().any(|f| f == &format!(">{room}\n|init|battle\n|title|alice vs. bob\n|j|☆alice\n")),
            "{first:?}"
        );
        assert!(
            first
                .iter()
                .any(|f| f.starts_with("|pm| bob| alice|/nonotify bob accepted the challenge, starting &laquo;"))
        );
        let [p1, p2] = play_out(&mut s, [a, b]);
        for frames in [&p1, &p2] {
            assert!(frames.iter().any(|f| f.starts_with(&format!(">{room}\n|t:|"))));
            assert!(frames.iter().any(|f| f.contains("|request|{") && f.contains("\"rqid\":2")));
            assert_eq!(frames.last().unwrap(), "|updatesearch|{\"searching\":[],\"games\":null}");
        }
        // Each player reads exact HP for their side only.
        let own = p1.iter().any(|f| {
            f.contains("|switch|p1a: ") && f.lines().any(|l| l.starts_with("|switch|p1a") && !l.ends_with("/100"))
        });
        assert!(own || format.contains("custom"), "{format}");
        s.send(a, &format!("|/leave {room}"));
        assert_eq!(s.take(a), [format!(">{room}\n|deinit")]);
        assert_eq!(s.hub.room_count(), 1);
        s.send(b, &format!("{room}|/leave"));
        assert_eq!(s.hub.room_count(), 0);
    }
}

#[test]
fn ladder_open_team_sheets_and_ratings() {
    let mut s = Server::new();
    s.login(1, "alice");
    s.login(2, "bob");
    let team = packed("gen9championsvgc2026regmc");
    for conn in [1, 2] {
        s.send(conn, &format!("|/utm {team}"));
        s.send(conn, "|/search gen9championsvgc2026regmc");
    }
    let frames = s.take(1);
    assert!(
        frames.contains(&"|updatesearch|{\"searching\":[\"gen9championsvgc2026regmc\"],\"games\":null}".to_string())
    );
    let room = "battle-gen9championsvgc2026regmc-1";
    // The later searcher is p1; the battle is rated.
    assert!(frames.iter().any(|f| f.contains("|title|bob vs. alice")));
    assert!(frames.iter().any(|f| f.contains("|player|p1|bob|") && f.ends_with("|1000")));
    assert!(frames.iter().any(|f| f.contains("|tier|[Gen 9 Champions] VGC 2026 Reg M-C\n|rated|\n|rule|")));
    s.take(2);

    s.send(1, &format!("{room}|/acceptopenteamsheets"));
    s.send(1, &format!("{room}|/acceptopenteamsheets"));
    let frames = s.take(1);
    assert_eq!(frames[0], format!(">{room}\n|uhtmlchange|otsrequest|"));
    assert_eq!(frames[1], format!(">{room}\nalice has agreed to open team sheets."));
    assert_eq!(
        frames[2],
        format!(">{room}\n|error|You have already made your decision about agreeing to open team sheets.")
    );
    s.send(2, &format!("{room}|/acceptopenteamsheets"));
    let frames = s.take(1);
    let sheets = frames.last().unwrap();
    assert!(sheets.starts_with(&format!(">{room}\n|showteam|p1|Incineroar||SitrusBerry|Intimidate|DarkestLariat,FlareBlitz,FakeOut,PartingShot|Careful||")));

    let [p1, _] = play_out(&mut s, [2, 1]);
    let ratings: Vec<&String> = p1.iter().filter(|f| f.contains("|raw|")).collect();
    assert_eq!(ratings.len(), 1);
    assert!(ratings[0].contains("'s rating: 1000 &rarr; <strong>"), "{ratings:?}");
}

#[test]
fn choices_undo_and_errors() {
    let mut s = Server::new();
    s.login(1, "alice");
    s.login(2, "bob");
    for conn in [1, 2] {
        s.send(conn, &format!("|/utm {}", packed("gen9customgame")));
    }
    s.send(1, "|/challenge bob, gen9customgame");
    s.send(2, "|/accept alice");
    let room = "battle-gen9customgame-1";
    let (_, rqid) = last_request(&s.take(1)).unwrap();
    s.take(2);

    s.send(1, "|/choose move 1");
    assert_eq!(s.take(1), ["|pm| alice|~|/error /choose - must be used in a chat room, not a console"]);
    s.send(1, &format!("{room}|/choose move 9"));
    assert_eq!(
        s.take(1),
        [format!(">{room}\n|error|[Invalid choice] Can't move: Your Incineroar doesn't have a move 9")]
    );
    s.send(1, &format!("{room}|/undo"));
    assert_eq!(s.take(1), [format!(">{room}\n|error|[Invalid choice] There's nothing to cancel")]);
    s.send(1, &format!("{room}|/choose move 1|{rqid}"));
    s.send(1, &format!("{room}|/undo"));
    assert!(s.take(1).is_empty());
    s.send(1, &format!("{room}|/choose move 1|1"));
    let too_late =
        "|error|[Invalid choice] Sorry, too late to make a different move; the next turn has already started";
    assert_eq!(s.take(1), [format!(">{room}\n{too_late}")]);
    s.send(1, &format!("{room}|/move 1"));
    s.send(2, &format!("{room}|/move 1"));
    let frames = s.take(1);
    assert!(frames.iter().any(|f| f.contains("|turn|2")), "{frames:?}");

    s.send(1, &format!("{room}|/notacommand"));
    assert_eq!(
        s.take(1),
        [format!(
            ">{room}\n|error|The command \"/notacommand\" does not exist. To send a message starting with \"/notacommand\", type \"//notacommand\"."
        )]
    );
    s.take(2);
    s.send(1, &format!("{room}|/forfeit"));
    let frames = s.take(2);
    assert_eq!(frames[0], format!(">{room}\n|-message|alice forfeited."));
    assert!(frames[1].ends_with("\n|win|bob"));
    s.send(2, &format!("{room}|/choose move 1"));
    assert_eq!(s.take(2), [format!(">{room}\n|error|[Invalid choice] There's nothing to choose")]);

    // Ten minutes after the end, the room goes, whoever is still in it.
    s.take(1);
    s.tick(599_000);
    assert_eq!(s.hub.room_count(), 1);
    s.tick(1_000);
    assert_eq!(s.hub.room_count(), 0);
    assert_eq!(s.take(1), [format!(">{room}\n|deinit")]);
}

#[test]
fn timer_and_disconnection() {
    let mut s = Server::new();
    s.login(1, "alice");
    s.login(2, "bob");
    for conn in [1, 2] {
        s.send(conn, &format!("|/utm {}", packed("gen9customgame")));
    }
    s.send(1, "|/challenge bob, gen9customgame");
    s.send(2, "|/accept alice");
    let room = "battle-gen9customgame-1";
    s.take(1);
    s.take(2);
    s.send(1, &format!("{room}|/timer on"));
    let frames = s.take(1);
    assert_eq!(
        frames[0],
        format!(
            ">{room}\n|inactive|Battle timer is ON: inactive players will automatically lose when time's up. (requested by alice)"
        )
    );
    assert_eq!(frames[1], format!(">{room}\n|inactive|Time left: 300 sec this turn | 300 sec total | 60 sec grace"));
    // Nobody moves: after 360 seconds both are out of time.
    for _ in 0..80 {
        s.tick(5_000);
    }
    let frames = s.take(2);
    assert!(frames.iter().any(|f| f.contains("|inactive|bob has 30 seconds left.")), "{frames:?}");
    assert!(frames.iter().any(|f| f.contains("|-message|All players are inactive.")));
    assert!(frames.iter().any(|f| f.ends_with("\n|tie")));

    // A disconnected player forfeits after the grace period.
    s.send(1, &format!("{room}|/leave"));
    s.send(2, &format!("{room}|/leave"));
    s.send(1, "|/challenge bob, gen9customgame");
    s.send(2, "|/accept alice");
    s.take(2);
    s.hub.disconnect(1, s.now);
    s.pump();
    assert_eq!(s.take(2), [">battle-gen9customgame-2\n|l|☆alice"]);
    s.tick(59_000);
    assert!(s.take(2).is_empty());
    s.tick(1_000);
    let frames = s.take(2);
    assert_eq!(frames[0], ">battle-gen9customgame-2\n|-message|alice lost due to inactivity.");
    assert!(frames[1].ends_with("\n|win|bob"));
}

/// Reg I's `!! Adjust Level = 50` is the validator's: it sets the levels of
/// the team the battle gets (`validateSet`), and the simulator plays them.
#[test]
fn levels_are_adjusted_as_the_validator_does() {
    let text = std::fs::read_to_string(format!("{TEAMS}/gen9vgc2025regi.txt")).unwrap().replace("Level: 50\n", "");
    let core = formats::find("gen9vgc2025regi").unwrap().format();
    let sheet = pikapro_protocol::import_sheet(&text, core).unwrap();
    assert!(sheet.team.sets.iter().all(|s| s.level == 100));
    let team = pikapro_protocol::pack_team(&sheet, core);
    let mut s = Server::new();
    s.login(1, "alice");
    s.login(2, "bob");
    for conn in [1, 2] {
        s.send(conn, &format!("|/utm {team}"));
    }
    s.send(1, "|/challenge bob, gen9vgc2025regi");
    s.send(2, "|/accept alice");
    let frames = s.take(1).join("\n");
    let pokes: Vec<&str> = frames.lines().filter(|l| l.starts_with("|poke|")).collect();
    assert_eq!(pokes.len(), 12, "{frames}");
    assert!(pokes.iter().all(|l| l.contains(", L50")), "{pokes:?}");
}

/// Where a test stream fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// A panic in `write`, on a move choice.
    Write,
    /// A panic in the `read` after a move choice.
    Read,
    /// Showdown's exception (an error) on a move choice.
    Throw,
    /// An `end` message that isn't JSON.
    BadEnd,
}

/// The real stream, failing where its fault says.
struct Faulty {
    inner: BattleStream,
    fault: Fault,
    armed: bool,
}

impl RoomStream for Faulty {
    fn write(&mut self, input: &str) -> Result<(), StreamError> {
        if self.fault != Fault::None && input.contains(" move ") {
            self.armed = true;
            match self.fault {
                Fault::Write => panic!("injected panic in write"),
                Fault::Throw => return Err(StreamError { message: "injected exception".into() }),
                _ => {}
            }
        }
        self.inner.write(input)
    }

    fn read(&mut self) -> Option<Message> {
        if self.armed && self.fault == Fault::Read {
            panic!("injected panic in read");
        }
        match self.inner.read() {
            Some(Message::End(_)) if self.fault == Fault::BadEnd => Some(Message::End("{\"winner\":".into())),
            other => other,
        }
    }

    fn request_state(&self, side: SideId) -> &'static str {
        self.inner.request_state(side)
    }
}

/// Battle `n` (from 1) gets a stream with `faults[n - 1]`, the others the
/// real one.
fn faulty_streams(faults: &'static [Fault]) -> StreamFactory {
    let n = std::sync::atomic::AtomicUsize::new(0);
    Box::new(move || {
        let fault = faults.get(n.fetch_add(1, std::sync::atomic::Ordering::Relaxed)).copied().unwrap_or(Fault::None);
        Box::new(Faulty { inner: room_battle_stream(), fault, armed: false })
    })
}

/// A panic in a battle ends that room as Showdown ends a crashed battle,
/// and nothing else: other rooms play on, the players can battle again.
#[test]
fn a_panicking_battle_ends_only_its_room() {
    let mut s = Server::with_streams(faulty_streams(&[Fault::Write, Fault::None, Fault::Read]));
    for (conn, name) in [(1, "alice"), (2, "bob"), (3, "carol"), (4, "dave")] {
        s.login(conn, name);
        s.send(conn, &format!("|/utm {}", packed("gen9customgame")));
    }
    // Battle 1, rated (the ladder): its stream panics on the first move.
    s.send(1, "|/search gen9customgame");
    s.send(2, "|/search gen9customgame");
    // Battle 2, between two other players.
    s.send(3, "|/challenge dave, gen9customgame");
    s.send(4, "|/accept carol");
    let crashed = "battle-gen9customgame-1";
    for conn in 1..=4 {
        s.take(conn);
    }
    // bob searched last: p1.
    s.send(2, &format!("{crashed}|/choose move 1"));
    let expected = [
        format!(
            ">{crashed}\n|html|<div class=\"broadcast-red\"><b>The battle crashed</b><br />Don't worry, we're working on fixing it.</div>"
        ),
        format!(">{crashed}\n|error|[Invalid choice] The battle crashed"),
        format!(">{crashed}\n|bigerror|The simulator process crashed. We've been notified and will fix this ASAP."),
        "|updatesearch|{\"searching\":[],\"games\":null}".to_string(),
    ];
    for conn in [1, 2] {
        let frames = s.take(conn);
        assert_eq!(frames, expected, "{conn}");
    }
    assert!(s.take(3).is_empty() && s.take(4).is_empty());
    // The room is over: no choices, no forfeit, no rating.
    s.send(2, &format!("{crashed}|/choose move 1"));
    assert_eq!(s.take(2), [format!(">{crashed}\n|error|[Invalid choice] There's nothing to choose")]);
    s.send(1, &format!("{crashed}|/forfeit"));
    assert!(s.take(1).is_empty() && s.take(2).is_empty());

    // Battle 2 plays to the end.
    let [p1, _] = play_out(&mut s, [3, 4]);
    assert!(p1.iter().any(|f| f.starts_with(">battle-gen9customgame-2\n") && f.contains("\n|win|")));

    // Battle 3: the stream panics while being read.
    s.send(1, "|/challenge bob, gen9customgame");
    s.send(2, "|/accept alice");
    s.take(1);
    s.take(2);
    let room = "battle-gen9customgame-3";
    s.send(1, &format!("{room}|/choose move 1"));
    let frames = s.take(1);
    assert!(frames.iter().any(|f| f.contains("|bigerror|The simulator process crashed.")), "{frames:?}");
    assert_eq!(frames.last().unwrap(), "|updatesearch|{\"searching\":[],\"games\":null}");

    // The crashed rooms close when their players leave; the players battle on.
    for room in [crashed, room] {
        s.send(1, &format!("|/leave {room}"));
        s.send(2, &format!("|/leave {room}"));
    }
    assert_eq!(s.hub.room_count(), 1);
    s.send(1, "|/challenge bob, gen9customgame");
    s.send(2, "|/accept alice");
    let [p1, _] = play_out(&mut s, [1, 2]);
    assert!(p1.iter().any(|f| f.starts_with(">battle-gen9customgame-4\n") && f.contains("\n|win|")));
}

/// Starts a challenge battle between two logged-in users in the custom
/// game; returns the room id.
fn challenge_battle(s: &mut Server, a: (u64, &str), b: (u64, &str)) -> String {
    for (conn, name) in [a, b] {
        s.login(conn, name);
        s.send(conn, &format!("|/utm {}", packed("gen9customgame")));
    }
    s.send(a.0, &format!("|/challenge {}, gen9customgame", b.1));
    s.send(b.0, &format!("|/accept {}", a.1));
    let frames = s.take(a.0);
    s.take(b.0);
    let (room, _) = last_request(&frames).expect("a request");
    room
}

/// `/noreply` prefixes are taken off in a loop: thousands of them (a 99 KB
/// message) run the command without deep recursion.
#[test]
fn noreply_prefixes_nest_without_recursion() {
    let mut s = Server::new();
    s.login(1, "alice");
    s.take(1);
    let message = format!("|{}/cmd rooms", "/noreply ".repeat(11_000));
    assert!(message.len() < 100 * 1024);
    s.send(1, &message);
    assert_eq!(s.take(1), ["|queryresponse|rooms|null"]);
    s.send(1, "|/noreply /noreply hello");
    assert!(s.take(1).is_empty());
}

/// Showdown's sockets and `socketReceive`: blank messages and a trailing
/// empty line are dropped (no chat error), messages over 100 KB and over
/// three lines are refused with a popup.
#[test]
fn messages_are_dropped_and_refused_as_showdown_does() {
    let mut s = Server::new();
    let room = challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    for blank in ["", "|", &format!("{room}|"), "{\"legacy\":1}", "no pipe"] {
        s.send(1, blank);
        assert!(s.take(1).is_empty(), "{blank:?}");
    }
    s.send(1, &format!("{room}|/choose move 1\n"));
    assert!(s.take(1).is_empty(), "the trailing empty line is not a chat message");
    s.send(1, &format!("{room}|hello"));
    assert!(s.take(1)[0].contains("You must be registered to chat in temporary rooms"));
    s.send(1, "|/cmd a\n/cmd b\n/cmd c\n/cmd d");
    assert_eq!(
        s.take(1),
        ["|popup|You're sending too many lines at once. Try using a paste service like [[Pastebin]]."]
    );
    s.send(1, "|/cmd a\n/cmd b\n/cmd c\n");
    assert_eq!(s.take(1).len(), 3);
    // 100 * 1024 UTF-16 units at most: 60,000 two-unit characters are over.
    s.send(1, &format!("|/utm {}", "\u{1F600}".repeat(60_000)));
    assert_eq!(s.take(1), ["|popup|Your message must be below 100KB"]);
    s.send(1, &format!("|/utm {}", "\u{e9}".repeat(60_000)));
    assert!(s.take(1).is_empty(), "60,000 one-unit characters are not");
}

/// Names are cleaned as Showdown's `getName` cleans them: no `|` (which
/// would add protocol fields), cut to 18 characters. Avatars too.
#[test]
fn names_and_avatars_cannot_add_protocol_fields() {
    let mut s = Server::new();
    s.hub.connect(1);
    s.pump();
    s.take(1);
    s.send(1, "|/trn Ash|K]x\u{202e},0,");
    let frames = s.take(1);
    assert!(frames[1].starts_with("|updateuser| Ash K x|1|"), "{frames:?}");
    s.send(1, "|/trn a b c d e f g h i j,0,");
    assert!(s.take(1)[1].starts_with("|updateuser| abcdefghij|1|"));
    s.send(1, "|/avatar lucas|x|y");
    assert!(s.take(1)[0].starts_with("|updateuser| abcdefghij|1|lucasxy|"));
    s.send(1, "|/avatar |||");
    assert_eq!(s.take(1), ["|pm| abcdefghij|~|/error Unrecognized avatar - make sure you're on the right account?"]);
}

/// A user id belongs to one user at a time, guests' ids included: a name
/// whose id a guest holds is taken, and a new guest skips an id a renamed
/// user took.
#[test]
fn ids_are_unique_guests_included() {
    let mut s = Server::new();
    for conn in [1, 2] {
        s.hub.connect(conn);
    }
    s.pump();
    s.take(2);
    s.send(2, "|/trn Guest 1,0,");
    assert_eq!(s.take(2), ["|nametaken|Guest 1|Someone is already using the name \"Guest 1\"."]);
    // An id starting with `guest` is a guest's (Showdown's `named`).
    s.send(2, "|/trn Guest 4,0,");
    let frames = s.take(2);
    assert!(frames.iter().any(|f| f.starts_with("|updateuser| Guest 4|0|")), "{frames:?}");
    s.send(2, "|/challenge guest1, gen9customgame");
    assert_eq!(s.take(2), ["|popup|You must choose a username before you challenge someone."]);
    for conn in [3, 4] {
        s.hub.connect(conn);
    }
    s.pump();
    assert!(s.take(3)[0].starts_with("|updateuser| Guest 3|0|"));
    assert!(s.take(4)[0].starts_with("|updateuser| Guest 5|0|"), "guest4 is taken");
}

/// A rename carries the player's seat along (Showdown's `onRename` in an
/// unrated battle): someone logging in under the old name afterwards is a
/// spectator, gets no request and can't choose.
#[test]
fn a_renamed_players_seat_follows_them() {
    let mut s = Server::new();
    let room = challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    s.send(1, "|/trn alice2,0,");
    let frames = s.take(1);
    assert!(frames.iter().any(|f| f.starts_with("|updateuser| alice2|1|")), "{frames:?}");
    assert!(frames.iter().any(|f| f == &format!(">{room}\n|player|p1|alice2|2|")), "{frames:?}");
    s.login(3, "alice");
    s.send(3, &format!("|/join {room}"));
    let frames = s.take(3);
    assert!(frames.iter().all(|f| !f.contains("|request|")), "{frames:?}");
    s.send(3, &format!("{room}|/choose move 1"));
    assert!(s.take(3).is_empty());
    // The seat is still alice2's.
    s.send(1, &format!("{room}|/choose move 1"));
    s.send(2, &format!("{room}|/choose move 1"));
    assert!(s.take(1).iter().any(|f| f.contains("|turn|2")));
    let [p1, _] = play_out(&mut s, [1, 2]);
    assert!(p1.iter().any(|f| f.contains("|win|")));
}

/// A seat goes to a user logging in under the player's id only when the
/// user it was bound to is gone (the player reconnecting).
#[test]
fn a_seat_is_taken_back_only_from_a_user_who_is_gone() {
    let mut s = Server::new();
    let room = challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    s.hub.disconnect(1, s.now);
    s.pump();
    s.login(3, "alice");
    s.send(3, &format!("|/join {room}"));
    let frames = s.take(3);
    assert!(frames.iter().any(|f| f.contains("|request|{")), "{frames:?}");
    s.send(3, &format!("{room}|/choose move 1"));
    s.send(2, &format!("{room}|/choose move 1"));
    assert!(s.take(3).iter().any(|f| f.contains("|turn|2")));
    // No forfeit for the disconnection: alice came back.
    s.tick(61_000);
    assert!(s.take(2).iter().all(|f| !f.contains("lost due to inactivity")));
}

/// In a rated battle a named player can't rename (Showdown's
/// `allowRenames`), and a guest player who does forfeits.
#[test]
fn renames_in_rated_battles() {
    let mut s = Server::new();
    s.login(1, "alice");
    s.hub.connect(2);
    s.pump();
    for conn in [1, 2] {
        s.send(conn, &format!("|/utm {}", packed("gen9customgame")));
        s.send(conn, "|/search gen9customgame");
    }
    let room = "battle-gen9customgame-1";
    s.take(1);
    s.take(2);
    s.send(1, "|/trn alice2,0,");
    assert_eq!(
        s.take(1),
        [
            "|popup|You can't change your name right now because you're in [Gen 9] Custom Game Battle, which doesn't allow renaming."
        ]
    );
    s.send(2, "|/trn carol,0,");
    let frames = s.take(1);
    assert!(frames.iter().any(|f| f == &format!(">{room}\n|-message|Guest 2 forfeited by changing their name.")));
    assert!(frames.iter().any(|f| f.ends_with("\n|win|alice")), "{frames:?}");
}

/// A rename ends the user's searches and challenges (`cancelReady`):
/// nothing is left for someone taking the old name to accept or inherit.
#[test]
fn a_rename_clears_challenges_and_searches() {
    let mut s = Server::new();
    s.login(1, "alice");
    s.login(2, "bob");
    for conn in [1, 2] {
        s.send(conn, &format!("|/utm {}", packed("gen9customgame")));
    }
    s.send(1, "|/challenge bob, gen9customgame");
    s.send(1, "|/search gen9doublescustomgame");
    s.take(1);
    s.take(2);
    s.send(1, "|/trn alice2,0,");
    let cancelled =
        "|pm| alice| bob|/text Challenge cancelled because they changed their username.\n|pm| alice| bob|/challenge";
    assert_eq!(s.take(2), [cancelled]);
    let frames = s.take(1);
    assert_eq!(frames[0], "|updatesearch|{\"searching\":[],\"games\":null}");
    assert_eq!(frames[1], cancelled);
    assert_eq!(frames[2], "|popup|Your searches and challenges have been cancelled because you changed your username.");
    assert!(frames[3].starts_with("|updateuser| alice2|1|"));
    // Someone else as alice: no challenge to accept, and no crash.
    s.login(3, "alice");
    s.send(2, "|/accept alice");
    assert_eq!(s.take(2), ["|popup|alice is not challenging you. Maybe they cancelled before you accepted?"]);
    assert_eq!(s.hub.room_count(), 0);
}

/// Spectators joining and leaving in a loop don't grow the room's log:
/// past a few dozen, `|j|` / `|l|` lines are sent but not logged, so what
/// each join sends stays the same size.
#[test]
fn joins_and_leaves_do_not_grow_the_log() {
    let mut s = Server::new();
    let room = challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    s.login(3, "carol");
    s.take(3);
    let mut sizes = Vec::new();
    for _ in 0..200 {
        s.send(3, &format!("|/join {room}"));
        let init = s.take(3).into_iter().find(|f| f.contains("|init|battle")).expect("init");
        sizes.push(init.len());
        s.send(3, &format!("|/leave {room}"));
        s.take(3);
    }
    assert_eq!(sizes[100], sizes[199]);
    assert!(sizes[199] < 3 * sizes[0], "{} → {}", sizes[0], sizes[199]);
    // The players still see who comes and goes.
    s.take(1);
    s.send(3, &format!("|/join {room}"));
    assert_eq!(s.take(1), [format!(">{room}\n|j| carol")]);
}

/// A player renaming in a loop doesn't grow their battles' logs: each
/// rename is a `|player|` line in every battle they play, so past a few
/// dozen in one battle they can't rename while it goes on (Showdown's
/// popup for a battle that doesn't allow renaming).
#[test]
fn renames_in_a_battle_are_capped() {
    let mut s = Server::new();
    let room = challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    let mut renamed = 0;
    for i in 0..100 {
        s.send(1, &format!("|/trn alice{},0,", i % 2));
        if s.take(1).iter().any(|f| f.starts_with("|popup|You can't change your name right now")) {
            break;
        }
        renamed += 1;
    }
    assert_eq!(renamed, 64);
    let name = "alice1";
    s.login(3, "carol");
    s.send(3, &format!("|/join {room}"));
    let init = s.take(3).into_iter().find(|f| f.contains("|init|battle")).expect("init");
    assert_eq!(init.matches("|player|p1|").count(), 65, "{init}");
    // The seat is still theirs, under the last name the battle took.
    assert!(init.contains(&format!("|player|p1|{name}|")), "{init}");
    s.send(1, &format!("{room}|/choose move 1"));
    s.send(2, &format!("{room}|/choose move 1"));
    assert!(s.take(1).iter().any(|f| f.contains("|turn|2")));
    // Once the battle is over, they can rename again.
    s.send(1, &format!("{room}|/forfeit"));
    s.take(1);
    s.send(1, "|/trn alice9,0,");
    assert!(s.take(1).iter().any(|f| f.starts_with("|updateuser| alice9|1|")));
}

/// A player turning their timer request on and off while the other keeps
/// the timer on doesn't grow the log: past a few dozen, those lines are
/// sent but not logged.
#[test]
fn timer_requests_do_not_grow_the_log() {
    let mut s = Server::new();
    let room = challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    s.send(1, &format!("{room}|/timer on"));
    s.login(3, "carol");
    s.take(3);
    let mut sizes = Vec::new();
    for _ in 0..200 {
        s.send(2, &format!("{room}|/timer on"));
        s.send(2, &format!("{room}|/timer off"));
        s.send(3, &format!("|/join {room}"));
        let init = s.take(3).into_iter().find(|f| f.contains("|init|battle")).expect("init");
        sizes.push(init.len());
        s.send(3, &format!("|/leave {room}"));
        s.take(3);
    }
    assert_eq!(sizes[100], sizes[199]);
    // The players still see each request.
    s.take(1);
    s.send(2, &format!("{room}|/timer on"));
    assert_eq!(s.take(1), [format!(">{room}\n|inactive|bob also wants the timer to be on.")]);
}

/// Showdown's exception in a write (`RoomBattleStream._write`): the crash
/// broadcast and `[Invalid choice] The battle crashed` to each player with
/// a request; the battle goes on.
#[test]
fn a_throwing_battle_reports_the_crash_and_goes_on() {
    let mut s = Server::with_streams(faulty_streams(&[Fault::Throw]));
    let room = challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    s.send(1, &format!("{room}|/choose move 1"));
    let frames = s.take(2);
    assert_eq!(
        frames,
        [
            format!(">{room}\n{}", pikapro_showdown_server::BATTLE_CRASHED),
            format!(">{room}\n|error|[Invalid choice] The battle crashed")
        ]
    );
    // Not ended: the players can still forfeit.
    s.send(2, &format!("{room}|/forfeit"));
    assert!(s.take(1).iter().any(|f| f.ends_with("\n|win|alice")));
}

/// An `end` message that doesn't parse ends the battle as a crashed one
/// (Showdown's `listen` throws on `JSON.parse`), not as a tie.
#[test]
fn a_bad_end_is_a_crash_not_a_tie() {
    let mut s = Server::with_streams(faulty_streams(&[Fault::BadEnd]));
    s.login(1, "alice");
    s.login(2, "bob");
    for conn in [1, 2] {
        s.send(conn, &format!("|/utm {}", packed("gen9customgame")));
        s.send(conn, "|/search gen9customgame");
    }
    let room = "battle-gen9customgame-1";
    s.take(1);
    s.send(1, &format!("{room}|/forfeit"));
    let frames = s.take(1);
    assert!(frames.iter().any(|f| f.contains("|bigerror|The simulator process crashed.")), "{frames:?}");
    assert!(frames.iter().all(|f| !f.contains("|raw|") && !f.contains("rating")), "no rating change: {frames:?}");
}

/// A team that lists Mega formes (`Charizard-Mega-Y @ Charizardite Y`, as
/// pastes do) plays as the validator leaves it (`validateForme`): each in
/// the forme it Mega Evolves from, with that forme's ability.
#[test]
fn listed_mega_formes_start_as_their_base_formes() {
    let text = std::fs::read_to_string(format!("{TEAMS}/gen9championsvgc2026regmc.txt"))
        .unwrap()
        .replace("Charizard @ Charizardite Y\nAbility: Blaze", "Charizard-Mega-Y @ Charizardite Y\nAbility: Drought")
        .replace("Tyranitar @ Life Orb", "Tyranitar-Mega @ Tyranitarite");
    let core = formats::find("gen9championsvgc2026regmc").unwrap().format();
    let sheet = pikapro_protocol::import_sheet(&text, core).unwrap();
    let team = pikapro_protocol::pack_team(&sheet, core);
    assert!(team.contains("Charizard-Mega-Y") && team.contains("Tyranitar-Mega"), "{team}");
    let mut s = Server::new();
    s.login(1, "alice");
    s.login(2, "bob");
    for conn in [1, 2] {
        s.send(conn, &format!("|/utm {team}"));
    }
    s.send(1, "|/challenge bob, gen9championsvgc2026regmc");
    s.send(2, "|/accept alice");
    let frames = s.take(1).join("\n");
    let pokes: Vec<&str> = frames.lines().filter(|l| l.starts_with("|poke|")).collect();
    assert_eq!(pokes.len(), 12, "{frames}");
    assert!(pokes.iter().all(|l| !l.contains("-Mega")), "{pokes:?}");
    assert!(pokes.iter().any(|l| l.starts_with("|poke|p1|Charizard, L50")), "{pokes:?}");
    assert!(pokes.iter().any(|l| l.starts_with("|poke|p1|Tyranitar, L50")), "{pokes:?}");
    // The request's own team: Charizard with Blaze (Drought is the Mega's).
    let request = frames.lines().find(|l| l.starts_with("|request|")).expect("a request");
    assert!(
        request.contains("\"details\":\"Charizard, L50") && request.contains("\"baseAbility\":\"blaze\""),
        "{request}"
    );
}

/// A server allowing one battle per user.
fn capped() -> Server {
    Server::with_config(Config { seed: Some(2_200_000), max_battles_per_user: 1, ..Config::default() })
}

/// Whether `frames` refuse something (a popup) and start no battle.
fn refused(frames: &[String]) -> bool {
    frames.iter().any(|f| f.contains("|popup|")) && !frames.iter().any(|f| f.contains("|init|battle"))
}

fn ready(s: &mut Server, conn: u64, name: &str) {
    s.login(conn, name);
    s.send(conn, &format!("|/utm {}", packed("gen9customgame")));
    s.take(conn);
}

/// A user may play [`Config::max_battles_per_user`] unfinished battles at
/// once: a search, challenge or accept past it is refused, and a finished
/// battle frees its place.
#[test]
fn battles_per_user_are_capped() {
    let mut s = capped();
    let room = challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    ready(&mut s, 3, "carol");
    s.send(1, "|/challenge carol, gen9customgame");
    assert!(refused(&s.take(1)), "challenge");
    s.send(1, "|/search gen9customgame");
    assert!(refused(&s.take(1)), "search");
    s.send(3, "|/challenge alice, gen9customgame");
    s.take(3);
    s.send(1, "|/accept carol");
    assert!(refused(&s.take(1)), "accept");
    // Once it is over, she can.
    s.send(1, &format!("{room}|/forfeit"));
    s.take(1);
    s.take(2);
    s.send(1, "|/accept carol");
    let frames = s.take(1);
    assert!(frames.iter().any(|f| f.contains("|init|battle")), "a finished battle frees its place");
}

/// The cap counts the user's id over the rooms' seats: reconnecting under
/// the same name doesn't reset it.
#[test]
fn the_battle_cap_survives_a_reconnect() {
    let mut s = capped();
    challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    s.hub.disconnect(1, s.now);
    s.pump();
    ready(&mut s, 5, "alice");
    ready(&mut s, 3, "carol");
    s.send(5, "|/challenge carol, gen9customgame");
    assert!(refused(&s.take(5)), "the same name, a new connection");
}

/// A challenge whose challenger reached the cap since can't be accepted
/// (starting a battle clears a player's searches, so a capped player is
/// never matched by a search either).
#[test]
fn an_opponent_at_the_cap_is_not_matched() {
    let mut s = capped();
    for (conn, name) in [(6, "frank"), (7, "gina"), (8, "harry")] {
        ready(&mut s, conn, name);
    }
    // Frank challenges Gina, then accepts Harry's challenge: he is at the cap.
    s.send(6, "|/challenge gina, gen9customgame");
    s.send(8, "|/challenge frank, gen9customgame");
    s.send(6, "|/accept harry");
    assert!(s.take(6).iter().any(|f| f.contains("|init|battle")), "frank plays harry");
    s.take(7);
    s.take(8);
    s.send(7, "|/accept frank");
    assert!(refused(&s.take(7)), "the challenger is at the cap");
}

/// `packed(format)` with the first set's nickname replaced by `nick`.
fn nicknamed(format: &str, nick: &str) -> String {
    let team = packed(format);
    let (first, rest) = team.split_once(']').unwrap();
    let mut fields: Vec<String> = first.split('|').map(str::to_string).collect();
    if fields[1].is_empty() {
        fields[1] = pikapro_data::to_id(&fields[0]);
    }
    fields[0] = nick.to_string();
    format!("{}]{rest}", fields.join("|"))
}

/// Nicknames are cleaned as Showdown's validator cleans them
/// (`dex.getName`), in every format: line breaks (`\r`, U+2028), which
/// would end a protocol line early, become spaces, and a nickname of white
/// space names the Pokémon after its species.
#[test]
fn nicknames_are_cleaned_in_every_format() {
    for format in ["gen9customgame", "gen9vgc2025regi"] {
        let mut s = Server::new();
        s.login(1, "alice");
        s.login(2, "bob");
        s.send(1, &format!("|/utm {}", nicknamed(format, "Bad\rNick\u{2028}X")));
        s.send(2, &format!("|/utm {}", nicknamed(format, " \u{2029} ")));
        s.send(1, &format!("|/challenge bob, {format}"));
        s.send(2, "|/accept alice");
        let frames = [s.take(1).join("\n"), s.take(2).join("\n")];
        for f in &frames {
            assert!(!f.contains('\r') && !f.contains('\u{2028}') && !f.contains('\u{2029}'), "{format}: {f}");
            assert!(!f.contains("|popup|"), "{format}: {f}");
        }
        assert!(frames[0].contains("\"ident\":\"p1: Bad Nick X\""), "{format}: {}", frames[0]);
        // Bob's request names his first Pokémon after its species.
        let species = packed(format).split('|').next().unwrap().to_string();
        assert!(frames[1].contains(&format!("\"ident\":\"p2: {species}\"")), "{format}: {}", frames[1]);
    }
}

/// A nickname over 18 characters is a problem in a custom game too, as
/// Showdown's validator finds it in every format.
#[test]
fn long_nicknames_are_refused_in_custom_games() {
    let mut s = Server::new();
    s.login(1, "alice");
    s.login(2, "bob");
    s.send(1, &format!("|/utm {}", nicknamed("gen9customgame", "ThisNameIsFarTooLongToKeep")));
    s.take(1);
    s.send(1, "|/challenge bob, gen9customgame");
    let frames = s.take(1);
    assert!(refused(&frames), "{frames:?}");
    assert!(frames.iter().any(|f| f.contains("nickname \"ThisNameIsFarTooLongToKeep\" is too long.")), "{frames:?}");
}

/// A player's finished battles are kept to `MAX_ENDED_ROOMS_PER_USER`, all
/// finished battles to `Config::max_ended_rooms`: past it the oldest
/// expires (`|deinit`) as if its ten minutes were up.
#[test]
fn finished_rooms_are_bounded() {
    let per_user = pikapro_showdown_server::MAX_ENDED_ROOMS_PER_USER;
    let mut s = Server::new();
    let first = challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    s.send(1, &format!("{first}|/forfeit"));
    for _ in 0..per_user {
        s.tick(1);
        s.send(1, "|/challenge bob, gen9customgame");
        s.send(2, "|/accept alice");
        let (room, _) = last_request(&s.take(1)).expect("a request");
        s.send(1, &format!("{room}|/forfeit"));
    }
    assert_eq!(s.hub.room_count(), per_user);
    let deinit = format!(">{first}\n|deinit");
    assert!(s.history[&1].contains(&deinit), "the oldest expired");
    let deinits = s.history[&1].iter().filter(|f| f.ends_with("|deinit")).count();
    assert_eq!(deinits, 1, "only the oldest");

    let mut s = Server::with_config(Config { seed: Some(2_200_000), max_ended_rooms: 2, ..Config::default() });
    let mut rooms = Vec::new();
    for (i, (a, b)) in [("alice", "bob"), ("carol", "dave"), ("erin", "frank")].into_iter().enumerate() {
        let conn = 10 * i as u64;
        let room = challenge_battle(&mut s, (conn + 1, a), (conn + 2, b));
        s.send(conn + 1, &format!("{room}|/forfeit"));
        s.tick(1);
        rooms.push(room);
    }
    assert_eq!(s.hub.room_count(), 2);
    assert!(s.history[&1].contains(&format!(">{}\n|deinit", rooms[0])), "the oldest expired");
    for (conn, room) in [(11, &rooms[1]), (21, &rooms[2])] {
        assert!(!s.history[&conn].iter().any(|f| f.ends_with("|deinit")), "{room} is kept");
    }

    // 0: no limit.
    let mut s = Server::with_config(Config { seed: Some(2_200_000), max_ended_rooms: 0, ..Config::default() });
    let room = challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    s.send(1, &format!("{room}|/forfeit"));
    assert_eq!(s.hub.room_count(), 1);
}

/// Showdown's throttle notice, as `User.chat` sends it to the line's room.
const THROTTLE_NOTICE: &str = "|raw|<strong class=\"message-throttle-notice\">Your message was not sent because you've been typing too quickly.</strong>";

/// Sends `count` `/cmd rooms` lines in `room`, three to a message, all at
/// the same instant; returns how many were answered and whether the
/// throttle notice came.
fn burst(s: &mut Server, conn: u64, room: &str, count: usize) -> (usize, bool) {
    let mut sent = 0;
    while sent < count {
        let n = (count - sent).min(3);
        s.send(conn, &format!("{room}|{}", vec!["/cmd rooms"; n].join("\n")));
        sent += n;
    }
    let frames = s.take(conn);
    let answered = frames.iter().filter(|f| f.ends_with("|queryresponse|rooms|null")).count();
    (answered, frames.iter().any(|f| f.contains("typing too quickly")))
}

/// Both players choose `default` for their current request; whether the
/// battle went on to the next turn.
fn both_choose(s: &mut Server, room: &str) -> bool {
    for conn in [1, 2] {
        let (_, rqid) = last_request(&s.history[&conn]).expect("a request");
        s.send(conn, &format!("{room}|/choose default|{rqid}"));
    }
    s.take(2);
    s.take(1).iter().any(|f| f.contains("\n|turn|"))
}

/// The default has no line throttle, as on a `--no-security` Showdown
/// server: 20,000 lines from one connection in the same millisecond are
/// all run, and a choice after them still counts.
#[test]
fn lines_are_not_throttled_by_default() {
    assert_eq!(Config::default().max_lines_per_second, 0);
    let mut s = Server::new();
    let room = challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    assert_eq!(burst(&mut s, 1, &room, 20_000), (20_000, false));
    assert!(both_choose(&mut s, &room));
}

/// An explicit limit: a second's worth of lines at once, then the rest of
/// the message is dropped (a choice too) and the connection gets
/// Showdown's notice in the line's room; the budget refills with time.
#[test]
fn an_explicit_limit_throttles_and_notifies() {
    let mut s = Server::with_config(Config { seed: Some(2_200_000), max_lines_per_second: 10, ..Config::default() });
    let room = challenge_battle(&mut s, (1, "alice"), (2, "bob"));
    s.tick(1000);
    assert_eq!(burst(&mut s, 1, &room, 9), (9, false), "nine of the second's ten");
    s.send(1, &format!("{room}|/cmd rooms\n/cmd rooms\n/cmd rooms"));
    let frames = s.take(1);
    assert_eq!(frames.len(), 2, "{frames:?}");
    assert_eq!(frames[1], format!(">{room}\n{THROTTLE_NOTICE}"), "the rest of the message is dropped");
    assert!(!both_choose(&mut s, &room), "alice's choice was dropped");
    let notice = s.history[&1].last().cloned();
    assert_eq!(notice, Some(format!(">{room}\n{THROTTLE_NOTICE}")));
    s.tick(1000);
    let (_, rqid) = last_request(&s.history[&1]).expect("a request");
    s.send(1, &format!("{room}|/choose default|{rqid}"));
    assert!(s.take(1).iter().any(|f| f.contains("\n|turn|")), "refilled: the choice counts");
}
