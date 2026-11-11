//! The player-channel parser and session, without Node: the golden channel fixtures replay (Showdown's
//! own streams and our renderer's, recorded by `cargo channel-conformance
//! --record`), and a few battles of every preset round-trip against
//! `VecEnv` (`cargo channel-conformance` runs thousands). Sessions that
//! join mid-battle (a reconnect) must read as ones that saw everything, at
//! every decision of fixtures and fuzzed battles.

use std::path::Path;

use pikapro::Format;
use pikapro_conformance::channel::{Options, rejoin, roundtrip};
use pikapro_conformance::view::{Generator, Source, Target};
use pikapro_conformance::{channel_fixture, fixture};
use pikapro_rl::vec::{MaskLayout, SwitchBy};

#[test]
fn golden_channels_replay() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/channel");
    // Fixtures are never deleted: the manifest lists every one.
    let paths = fixture::check_manifest(&dir, "txt").unwrap_or_else(|e| panic!("{e}"));
    let (mut failures, mut decisions) = (Vec::new(), 0);
    for path in &paths {
        let text = std::fs::read_to_string(path).expect("readable fixture");
        let mut f = channel_fixture::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        // `PIKAPRO_BLESS=1`: take the digests again from the recorded lines,
        // after a deliberate change to the view's fields, the observation
        // or the dex's ids.
        if std::env::var_os("PIKAPRO_BLESS").is_some() {
            let blessed = channel_fixture::bless(&f).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            std::fs::write(path, &blessed).expect("writable fixture");
            f = channel_fixture::parse(&blessed).expect("a blessed fixture parses");
        }
        // At every decision, a session that rejoins then (Showdown's room
        // log and the current request) too.
        match channel_fixture::replay_with(&f, true) {
            Ok(n) => decisions += n,
            Err(e) => failures.push(format!("{}:\n{e}", path.display())),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} channel fixtures fail:\n{}",
        failures.len(),
        paths.len(),
        failures.join("\n")
    );
    assert!(decisions > 1000, "{decisions}");
}

#[test]
fn battles_round_trip() {
    for format in Format::presets() {
        let g = Generator::new(format, Source::Fuzz).expect("a generator");
        for seed in 5_124_000..5_124_004 {
            let stats =
                roundtrip(&g.setup(seed), &Options::default()).unwrap_or_else(|f| panic!("{} {seed}: {f}", format.id));
            assert!(stats.skipped || stats.decisions > 0);
        }
        let options = Options { masks: MaskLayout::Joint, switch_by: SwitchBy::Position, ..Options::default() };
        roundtrip(&g.setup(5_124_010), &options).unwrap_or_else(|f| panic!("{} joint: {f}", format.id));
    }
}

/// Best-of series (`--series`): each game's sessions carry the series the
/// last game's recorded, and show the environment's series mode (the
/// score, the game, the earlier games' public records, the `series` row)
/// with open sheets, rejoins included; a Bo5 plays up to five games.
#[test]
fn series_round_trip() {
    for format in [Format::champions_vgc(), Format::gen9_vgc()] {
        let g = Generator::new(&format, Source::Fuzz).expect("a generator");
        let (mut games, mut series) = (0, 0);
        for seed in 5_124_200..5_124_206 {
            for (best_of, reconnect) in [(3, seed % 2 == 0), (5, false)] {
                let options = Options { series: Some(best_of), reconnect, ..Options::default() };
                let stats = roundtrip(&g.setup(seed), &options)
                    .unwrap_or_else(|f| panic!("{} {seed} bo{best_of}: {f}", format.id));
                if !stats.skipped {
                    assert!((u64::from(best_of / 2 + 1)..=u64::from(best_of)).contains(&stats.games), "{stats:?}");
                    games += stats.games;
                    series += 1;
                }
            }
        }
        assert!(games > 2 * series, "{} {games} games in {series} series", format.id);
    }
    // Showdown shows the sheets at team preview: no series without it.
    let g = Generator::new(&Format::gen9_singles(), Source::Fuzz).expect("a generator");
    assert!(roundtrip(&g.setup(1), &Options { series: Some(3), ..Options::default() }).is_err());
}

#[test]
fn targets_round_trip() {
    let format = Format::champions_vgc();
    for (i, t) in Target::ALL.into_iter().enumerate() {
        let g = Generator::new(&format, Source::Target(t)).expect("a generator");
        let seed = 5_124_100 + i as u64;
        roundtrip(&g.setup(seed), &Options::default()).unwrap_or_else(|f| panic!("{}: {f}", t.name()));
    }
}

/// Two fainted Pokémon named alike (Tauros-Paldea-Combat and -Aqua, both
/// "Tauros"), one revived: `|-heal|p1: Tauros|49/100|[from] move: Revival
/// Blessing` names it off the field, which no client can tell apart, so
/// the battle is excused from there (`channel::named_alike`), not failed
/// (`--source targets`, championsvgc 10,603,586: Illusion).
#[test]
fn a_revived_one_of_two_alike_is_excused() {
    let format = Format::champions_vgc();
    let g = Generator::new(&format, Source::Target(Target::Illusion)).expect("a generator");
    let stats = roundtrip(&g.setup(10_603_586), &Options::default()).unwrap_or_else(|f| panic!("{f}"));
    assert!(stats.skipped);
}

/// A session started at any step of a battle (Showdown's rejoin: the
/// room's log so far, then the current request) gives what the session
/// that read the whole battle gives: own ids and rows, the rest of the
/// view, the observation and the mask (`--reconnect`). Every preset, VGC's
/// bring-4 at more seeds, and Illusion users, which lines name as others.
#[test]
fn sessions_rejoin_at_every_step() {
    let options = Options { reconnect: true, ..Options::default() };
    for format in Format::presets() {
        let fuzz = Generator::new(format, Source::Fuzz).expect("a generator");
        let illusion = Generator::new(format, Source::Target(Target::Illusion)).expect("a generator");
        let seeds = if format.team_preview.shown() { 6_040_000..6_040_004 } else { 6_040_000..6_040_002 };
        for seed in seeds {
            for g in [&fuzz, &illusion] {
                let stats = roundtrip(&g.setup(seed), &options).unwrap_or_else(|f| panic!("{} {seed}: {f}", format.id));
                assert!(stats.skipped || stats.rejoins > 0, "{} {seed}", format.id);
            }
        }
    }
}

/// A lead Neutralizing Gas suppresses the other side's lead Illusion
/// before the gas shows: a session that rejoins works the team order out
/// knowing it (`--reconnect --source targets`, gen9singles 5,000,058,
/// Illusion).
#[test]
fn rejoin_after_a_lead_neutralizing_gas() {
    let format = Format::gen9_singles();
    let g = Generator::new(&format, Source::Target(Target::Illusion)).expect("a generator");
    let options = Options { reconnect: true, ..Options::default() };
    let stats = roundtrip(&g.setup(5_000_058), &options).unwrap_or_else(|f| panic!("{f}"));
    assert!(stats.rejoins > 0);
}

/// Two own Pokémon of one name and species, a male never seen and a
/// female that switched in: a rejoin's replay matches each request entry
/// to the lines' Pokémon of its gender first (`--reconnect --source
/// targets`, gen9singles 11,620,353, Knock Off).
#[test]
fn rejoin_tells_two_of_one_name_by_gender() {
    let format = Format::gen9_singles();
    let g = Generator::new(&format, Source::Target(Target::KnockOff)).expect("a generator");
    let options = Options { reconnect: true, ..Options::default() };
    let stats = roundtrip(&g.setup(11_620_353), &options).unwrap_or_else(|f| panic!("{f}"));
    assert!(stats.rejoins > 0);
}

/// The rejoin transcript: no request but the last, no `|error|` line.
#[test]
fn a_rejoin_replays_the_log_then_the_request() {
    let stream =
        ["|player|p1|Alice||", "|request|{\"wait\":true}", "|turn|1", "|error|[Invalid choice] x", "|request|{}"];
    let lines = rejoin(&stream);
    assert_eq!(lines.first().map(String::as_str), Some("|init|battle"));
    assert_eq!(lines.last().map(String::as_str), Some("|request|{}"));
    assert_eq!(lines.iter().filter(|l| l.starts_with("|request|") || l.starts_with("|error|")).count(), 1);
    assert!(lines.contains(&"|turn|1".to_string()));
}

/// A rejoined request the player had answered (`|sentchoice|`) asks for
/// nothing; a session holds at most `MAX_HELD` lines for its first
/// request, then reads them as they come.
#[test]
fn sentchoice_and_the_held_lines() {
    use pikapro_rl::session::{ChannelSession, MAX_HELD, SessionOptions};
    let format = Format::gen9_singles();
    let json = r#"{"active":[{"moves":[{"move":"Tackle","id":"tackle","pp":35,"maxpp":56,"target":"normal","disabled":false}]}],"side":{"name":"p1","id":"p1","pokemon":[{"ident":"p1: Pikachu","details":"Pikachu, L50, M","condition":"100/100","active":true,"stats":{"atk":1,"def":1,"spa":1,"spd":1,"spe":1},"moves":["tackle"],"baseAbility":"static","item":"","ability":"static"}]}}"#;
    let mut s = ChannelSession::for_format(format.clone(), true, SessionOptions::default()).expect("a session");
    s.feed("|switch|p1a: Pikachu|Pikachu, L50, M|100/100");
    s.feed(&format!("|request|{json}"));
    assert!(s.needs_action());
    s.feed("|sentchoice|move 1");
    assert!(!s.needs_action() && s.overlay().answered);
    assert!(s.mask().iter().all(|&b| !b));

    let mut s = ChannelSession::for_format(format, true, SessionOptions::default()).expect("a session");
    for _ in 0..MAX_HELD {
        s.feed("|c|☆someone|hello");
    }
    s.feed("|switch|p2a: Pikachu|Pikachu, L50, M|100/100");
    assert_eq!(s.tracker().side(pikapro::SideId::P2).mons.len(), 1, "read once the hold is full");
}
