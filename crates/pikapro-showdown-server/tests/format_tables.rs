//! The format ids are written in several tables, each for its own reader:
//! the engine's presets, the Showdown format the conformance oracle plays
//! each as, the battle stream's, the channel reader's, the validator's,
//! the team generator's and this server's list. A preset added to one and
//! forgotten in another fails here. (The Python package's tables are
//! checked the same way by `crates/pikapro-py/tests/test_format_tables.py`.)

use pikapro::Format;
use pikapro_protocol::{BattleInfo, channel};
use pikapro_showdown_server::formats::{self, FORMATS};
use pikapro_teams::{Mode, TeamGenerator};
use pikapro_validate::ValidationFormat;

/// What a battle of `a` has that the stream, which doesn't validate
/// teams, keeps of the preset `b` (it plays sets at their own levels,
/// without clauses, for any team of one to six).
fn same_battle(a: &Format, b: &Format) -> bool {
    use pikapro::format::{Clauses, LevelRule};
    let open =
        |f: &Format| Format { level: LevelRule::AsSet, clauses: Clauses::default(), team_size: (1, 6), ..f.clone() };
    open(a).same_rules(&open(b))
}

#[test]
fn every_preset_is_in_every_table() {
    let presets = Format::presets();
    for preset in presets {
        let id = preset.id;
        // The Showdown format the conformance oracle plays it as.
        let showdown =
            pikapro_conformance::showdown_format(preset).unwrap_or_else(|| panic!("{id}: no Showdown format"));
        let info = BattleInfo::for_format(preset);
        assert_ne!(info.tier, id, "{id}: BattleInfo::for_format has no header for it");
        for name in [id, showdown] {
            // The battle stream starts both as the preset, with its header.
            let stream = pikapro_server::format::resolve(name).unwrap_or_else(|| panic!("the stream: {name}"));
            assert!(same_battle(&stream.format, preset), "the stream plays {name} as {:?}", stream.format);
            assert_eq!((&stream.info.tier, &stream.info.rules), (&info.tier, &info.rules), "the stream: {name}");
            assert_eq!(stream.debug, info.report_exact_hp, "the stream: {name}");
            // The channel reader reads both as the preset.
            let read = channel::showdown_format(name).unwrap_or_else(|| panic!("the channel reader: {name}"));
            assert!(read.format.same_rules(preset), "the channel reader reads {name} as {}", read.format.id);
        }
        // Showdown's own format shows HP as its header says.
        assert_eq!(channel::showdown_format(showdown).unwrap().exact_hp, info.report_exact_hp, "{showdown}");
        // The validator and the team generator know it by the preset's id.
        let validation = ValidationFormat::for_preset(id).unwrap_or_else(|| panic!("{id}: no validation format"));
        assert_eq!(ValidationFormat::preset_id(id), Some(validation.id));
        let generator = TeamGenerator::new(preset, Mode::Sets).unwrap_or_else(|e| panic!("{id}: no teams: {e}"));
        assert_eq!(generator.validation_format().id, validation.id, "{id}");
        // This server: the preset's id, and a listed Showdown id that
        // plays it; validated as the validator's table says, if at all.
        let own = formats::find(id).unwrap_or_else(|| panic!("the server doesn't offer {id}"));
        assert!(same_battle(own.format(), preset), "the server plays {id} as {:?}", own.format());
        let listed: Vec<_> = FORMATS.iter().filter(|f| f.listed && f.format().id == id).collect();
        assert!(!listed.is_empty(), "the server lists no format played as {id}");
        for f in listed.iter().copied().chain([own]) {
            assert!(f.validation.is_none_or(|v| v == validation.id), "{}: validated as {:?}", f.id, f.validation);
            assert_eq!(f.name(), f.stream().info.tier, "{}", f.id);
        }
        // The channel reader knows whether the server validates its teams.
        assert_eq!(channel::showdown_format(id).unwrap().validated, own.validation.is_some(), "{id}");
    }
    // And no table names a format that isn't a preset.
    for f in FORMATS {
        let played = f.format().id;
        assert!(presets.iter().any(|p| p.id == played), "{} is played as {played}", f.id);
        // The channel reader reads each of the server's ids as the preset
        // the server plays, but for its plain custom games: Showdown's
        // have team preview, so a reader can't take those ids for the
        // presets (clients of this server read them as the presets' ids).
        match channel::showdown_format(f.id) {
            Some(read) => assert_eq!(read.format.id, played, "the channel reader: {}", f.id),
            None => assert!(matches!(f.id, "gen9customgame" | "gen9doublescustomgame"), "the channel reader: {}", f.id),
        }
    }
    for v in ValidationFormat::all() {
        assert!(presets.iter().any(|p| ValidationFormat::preset_id(p.id) == Some(v.id)), "{}: no preset", v.id);
    }
}
