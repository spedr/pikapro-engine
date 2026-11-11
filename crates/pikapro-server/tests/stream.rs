//! The stream's own API and what the golden transcripts can't show
//! (Showdown doesn't know pikapro's format ids, and unseeded battles
//! differ by design).

use pikapro::SideId;
use pikapro_server::{BattleStream, Channel, Message, PlayerStreams, StreamOptions};

const PIKACHU: &str = "Pikachu||lightball|static|thunderbolt,quickattack|Timid|,,,252,,252||||50|";
const SNORLAX: &str = "Snorlax||leftovers|thickfat|rest,bodyslam|Careful|252,,,,252,||||50|";

fn clock() -> u64 {
    1_700_000_000
}

fn stream() -> BattleStream {
    BattleStream::new(StreamOptions { clock, ..StreamOptions::default() })
}

fn players(s: &mut BattleStream) {
    s.write(&format!(r#">player p1 {{"name":"Ann","team":"{PIKACHU}"}}"#)).unwrap();
    s.write(&format!(r#">player p2 {{"name":"Ben","team":"{SNORLAX}"}}"#)).unwrap();
}

#[test]
fn plays_a_battle_through_the_api() {
    let mut s = stream();
    s.write(r#">start {"formatid":"gen9customgame@@@!Team Preview","seed":"sodium,1"}"#).unwrap();
    assert!(!s.waiting_for(SideId::P1));
    players(&mut s);
    let messages = s.drain();
    assert_eq!(messages[0], Message::Update("|t:|1700000000\n|gametype|singles".into()));
    assert!(
        matches!(&messages[3], Message::SideUpdate { side: SideId::P1, data } if data.starts_with("|request|{\"active\""))
    );
    assert!(s.waiting_for(SideId::P1) && s.waiting_for(SideId::P2));
    assert!(s.request(SideId::P1).is_some_and(|r| r.starts_with("{\"active\":[")));
    assert_eq!((s.request_state(SideId::P1), s.request_state(SideId::P2)), ("move", "move"));
    s.write(">p1 move thunderbolt").unwrap();
    assert!(!s.waiting_for(SideId::P1), "stored until p2 chooses");
    assert!(s.drain().is_empty());
    s.write(">p1 undo").unwrap();
    assert!(s.waiting_for(SideId::P1));
    s.write(">p1 move 2\n>p2 move rest").unwrap();
    let text: Vec<String> = s.drain().iter().map(ToString::to_string).collect();
    assert!(
        text[0].starts_with("update\n|\n|t:|1700000000\n|move|p1a: Pikachu|Quick Attack|p2a: Snorlax"),
        "{}",
        text[0]
    );
    assert_eq!(s.input_log()[3], ">p1 move quickattack");
    s.write(">forcewin p2").unwrap();
    assert!(s.battle_ended() && s.is_ended());
    assert_eq!(s.winner(), Some("Ben"));
    let end = s.drain().pop().unwrap();
    let Message::End(json) = end else { panic!("{end:?}") };
    assert!(json.starts_with(
        r#"{"winner":"Ben","seed":"sodium,1","turns":2,"p1":"Ann","p2":"Ben","p1team":[{"name":"Pikachu""#
    ));
    // Nothing more after the end (no keepAlive), as Showdown drops pushes.
    s.write(">p1 move 1").unwrap();
    assert!(s.read().is_none());
}

#[test]
fn accepts_preset_ids_as_their_showdown_formats() {
    for (id, tier) in [
        ("gen9singles", "[Gen 9] Custom Game"),
        ("championsvgc", "[Gen 9 Champions] VGC 2026 Reg M-C"),
        ("gen9vgc", "[Gen 9] Doubles Custom Game"),
        ("gen9ou", "[Gen 9] OU"),
    ] {
        let mut s = stream();
        // Champions reads Stat Points from the EV field.
        let sp = |team: &str| if id == "championsvgc" { team.replace("252", "32") } else { team.to_string() };
        s.write(&format!(r#">start {{"formatid":"{id}","seed":"gen5,0001000200030004"}}"#)).unwrap();
        s.write(&format!(r#">player p1 {{"team":"{}"}}"#, sp(PIKACHU))).unwrap();
        s.write(&format!(r#">player p2 {{"team":"{}"}}"#, sp(SNORLAX))).unwrap();
        let text: String = s.drain().iter().map(|m| m.to_string() + "\n").collect();
        assert!(text.contains(&format!("|tier|{tier}\n")), "{id}: {text}");
        assert!(text.contains("|player|p1|Player 1||"), "{id}");
    }
    let mut s = stream();
    let err = s.write(r#">start {"formatid":"gen9ubers"}"#).unwrap_err();
    assert_eq!(err.message, "Unrecognized format \"gen9ubers\"");
}

#[test]
fn unseeded_battles_get_a_fresh_sodium_seed() {
    let seed = |s: &BattleStream| s.input_log()[0].clone();
    let mut a = stream();
    a.write(r#">start {"formatid":"gen9championsvgc2026regmc"}"#).unwrap();
    let mut b = stream();
    b.write(r#">start {"formatid":"gen9championsvgc2026regmc","seed":""}"#).unwrap();
    let (sa, sb) = (seed(&a), seed(&b));
    assert!(sa.starts_with(r#">start {"formatid":"gen9championsvgc2026regmc","seed":"sodium,"#), "{sa}");
    assert_eq!(sa.len(), sb.len());
    assert_ne!(sa, sb);
}

/// A random battle's player without a team gets the generator's team for
/// its seed, or for a fresh one written into the input log, which then
/// replays the same battle; other formats still need a team.
#[test]
fn random_battles_generate_teams_without_one() {
    let mut a = stream();
    a.write(r#">start {"formatid":"gen9randombattle","seed":"gen5,0001000200030004"}"#).unwrap();
    a.write(r#">player p1 {"name":"Ann","seed":"gen5,00000000000000ff"}"#).unwrap();
    a.write(r#">player p2 {"name":"Ben"}"#).unwrap();
    let log = a.input_log().to_vec();
    assert_eq!(log[1], r#">player p1 {"name":"Ann","seed":"gen5,00000000000000ff"}"#);
    assert!(log[2].starts_with(r#">player p2 {"name":"Ben","seed":"sodium,"#), "{}", log[2]);
    let first: String = a.drain().iter().map(|m| m.to_string() + "\n").collect();
    // p1's team is the generator's for its seed.
    let mut rng = pikapro::BattleRng::new(pikapro::Seed::ShowdownGen5([0, 0, 0, 0xff]));
    let team =
        pikapro_teams::random_battle::generate(pikapro_teams::random_battle::RandomBattle::Singles, &mut rng).unwrap();
    let lead = pikapro_data::gen9::DEX.species(team[0].species).name;
    assert!(first.contains(&format!("|switch|p1a: {}", pikapro_data::gen9::DEX.species(team[0].name).name)), "{first}");
    assert!(first.contains(lead), "{first}");
    // The input log replays it.
    let mut b = stream();
    for line in &log {
        b.write(line).unwrap();
    }
    let again: String = b.drain().iter().map(|m| m.to_string() + "\n").collect();
    assert_eq!(again, first);
    let mut c = stream();
    c.write(r#">start {"formatid":"gen9customgame@@@!Team Preview"}"#).unwrap();
    let err = c.write(r#">player p1 {"name":"Ann"}"#).unwrap_err();
    assert!(err.message.contains("needs a team"), "{}", err.message);
}

/// Each player's genders are drawn when it joins, as Showdown's `Side`
/// constructor draws them: p2 first here, then a reseed, then p1.
#[test]
fn draws_genders_as_players_join() {
    let mut s = stream();
    s.write(r#">start {"formatid":"gen9customgame@@@!Team Preview","seed":"sodium,1"}"#).unwrap();
    s.write(&format!(r#">player p2 {{"team":"{SNORLAX}"}}"#)).unwrap();
    s.write(">reseed gen5,0001000200030004").unwrap();
    s.write(&format!(r#">player p1 {{"team":"{PIKACHU}"}}"#)).unwrap();
    let text: String = s.drain().iter().map(|m| m.to_string() + "\n").collect();
    let p2 = text.find("|player|p2|").unwrap();
    let p1 = text.find("|player|p1|").unwrap();
    assert!(p2 < p1 && text[p1..].starts_with("|player|p1|Player 1||\n|gen|9\n"), "{text}");
    // p2's gender from `sodium,1`'s first draw, p1's from the reseeded one.
    let first = |seed| pikapro::BattleRng::new(seed).random(2);
    let g = |d| if d == 0 { "M" } else { "F" };
    let sodium = pikapro_server::ShowdownSeed::parse("sodium,1").unwrap().seed;
    assert!(text.contains(&format!("Snorlax, L50, {}|", g(first(sodium)))), "{text}");
    let gen5 = pikapro::Seed::ShowdownGen5([1, 2, 3, 4]);
    assert!(text.contains(&format!("Pikachu, L50, {}|", g(first(gen5)))), "{text}");
}

#[test]
fn player_streams_split_channels() {
    let mut p = PlayerStreams::new(stream());
    p.write(Channel::Omniscient, r#">start {"formatid":"gen9customgame@@@!Team Preview","seed":"sodium,1"}"#).unwrap();
    p.write(Channel::Omniscient, &format!(r#">player p1 {{"team":"{PIKACHU}"}}"#)).unwrap();
    p.write(Channel::Omniscient, &format!(r#">player p2 {{"team":"{SNORLAX}"}}"#)).unwrap();
    assert!(p.write(Channel::Spectator, "move 1").is_err());
    p.write(Channel::Player(SideId::P1), "move 1").unwrap();
    p.write(Channel::Player(SideId::P2), "move 9").unwrap();
    let p2 = p.drain(Channel::Player(SideId::P2));
    assert!(p2.last().unwrap().starts_with("|error|[Invalid choice] Can't move: Your Snorlax doesn't have a move 9"));
    p.write(Channel::Player(SideId::P2), "move 1").unwrap();
    let spectator = p.drain(Channel::Spectator).join("\n");
    let omniscient = p.drain(Channel::Omniscient).join("\n");
    // Custom games show exact HP to everyone; `|split|` is never shown.
    assert!(!spectator.contains("|split|") && spectator.contains("|move|p1a: Pikachu|Thunderbolt|p2a: Snorlax"));
    assert!(omniscient.contains("|-damage|p2a: Snorlax|"));
    let p1 = p.drain(Channel::Player(SideId::P1));
    assert!(p1.iter().any(|c| c.starts_with("|request|")) && p1.iter().any(|c| c.contains("|move|")));
}

/// `extractChannelMessages`' regex: a `|split|pN` block needs `\n` after
/// the marker and after the secret line; any line terminator (`\r`,
/// U+2028) ends a line.
#[test]
fn update_lines_match_extract_channel_messages() {
    use pikapro_protocol::Perspective;
    use pikapro_server::stream::{UpdateLine, extract, update_lines};
    let data = "|a\n|split|p1\n|secret\n|shared\n|split|p2\n|x\r|y\n|b\u{2028}|c\n\n|split|p3\n\n|pub";
    let shared = UpdateLine::Shared;
    assert_eq!(
        update_lines(data),
        [
            shared("|a"),
            UpdateLine::Split { player: 1, secret: "|secret", shared: "|shared" },
            shared("|split|p2"),
            shared("|x"),
            shared("|y"),
            shared("|b"),
            shared("|c"),
            UpdateLine::Split { player: 3, secret: "", shared: "|pub" },
        ]
    );
    let p1 = extract(data, Perspective::Player(SideId::P1));
    assert_eq!(p1, ["|a", "|secret", "|split|p2", "|x", "|y", "|b", "|c", "|pub"]);
    let spectator = extract(data, Perspective::Spectator);
    assert_eq!(spectator[1], "|shared");
    // Player 3's secret line is empty: the omniscient channel reads nothing.
    assert_eq!(extract(data, Perspective::Omniscient).last().unwrap(), "|c");
}
