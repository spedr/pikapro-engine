//! The Showdown formats this server plays.
//!
//! A client names a format by its Showdown id (`/search gen9championsvgc2026regmc`),
//! and that id is what the room id carries (`battle-gen9championsvgc2026regmc-1`):
//! poke-env matches rooms to players by it. Each id maps to the `formatid`
//! the room's battle stream (`pikapro-server`) is started with, which sets the
//! battle, its header, its name and whether it has Open Team Sheets
//! ([`pikapro_server::format::resolve`], the one table of battle formats);
//! this table adds what belongs to the server: the validation format
//! Showdown's server would check teams against (with `pikapro-validate`),
//! the level rule its validator applies, the timer and the formats list.
//!
//! | Showdown id | stream `formatid` (preset) | validation | levels |
//! |---|---|---|---|
//! | `gen9championsvgc2026regmc` | the same (`championsvgc`) | `gen9championsvgc2026regmc` | Adjust Level = 50 |
//! | `gen9championsbssregmc` | the same (`championsbss`) | `gen9championsbssregmc` | Adjust Level = 50 |
//! | `gen9vgc2025regi` | the same (`gen9vgc`) | `gen9vgc2025regi` | Adjust Level = 50 |
//! | `gen9ou` | the same (`gen9ou`, whose id is Showdown's) | `gen9ou` | as set |
//! | `gen9randombattle` | the same | none (teams generated) | as set |
//! | `gen9randomdoublesbattle` | the same | none (teams generated) | as set |
//! | `gen9customgame` | `gen9customgame@@@!Team Preview` (`gen9singles`) | none (structural) | as set |
//! | `gen9doublescustomgame` | `gen9doublescustomgame@@@!Team Preview` (`gen9doubles`) | none (structural) | as set |
//!
//! Reg M-B (VGC or BSS) isn't offered: on Showdown master it is a legality overlay
//! (`championsregmb`) on Reg M-C's mechanics that pikapro-validate can't check.
//!
//! The custom games are played as the presets, without team preview. The
//! other preset ids (`championsvgc`, `championsbss`, `gen9vgc`, `gen9singles`, `gen9doubles`)
//! are accepted too, and started as themselves: the stream plays each as
//! the Showdown format the conformance suite mirrors.
//!
//! The level rule is the validator's `Adjust Level` / `Adjust Level Down`:
//! Showdown's validator rewrites each set's level (`validateSet`), and the
//! simulator plays the level it is given, so the room's team carries the
//! adjusted levels (see `hub.rs`, `prep_battle`).

use std::sync::OnceLock;

use pikapro::Format;
use pikapro::format::LevelRule;
use pikapro_server::format::StreamFormat;

/// A format the server plays.
#[derive(Clone, Copy, Debug)]
pub struct ShowdownFormat {
    /// The Showdown id: `/search`, `/challenge`, room ids.
    pub id: &'static str,
    /// The `formatid` of the battle stream's `>start` (a format
    /// `pikapro-server` plays).
    pub stream_id: &'static str,
    /// The `pikapro-validate` format teams are checked against, if any.
    pub validation: Option<&'static str>,
    /// The validator's level rule (`Adjust Level`, `Adjust Level Down`).
    pub level: LevelRule,
    /// Showdown's `VGC Timer` rule (the timer's settings).
    pub vgc_timer: bool,
    /// Listed in the `|formats|` message sent on connection.
    pub listed: bool,
}

/// Every format, the Showdown ids first.
pub const FORMATS: &[ShowdownFormat] = &[
    ShowdownFormat {
        id: "gen9championsvgc2026regmc",
        stream_id: "gen9championsvgc2026regmc",
        validation: Some("gen9championsvgc2026regmc"),
        level: LevelRule::Adjust(50),
        vgc_timer: true,
        listed: true,
    },
    ShowdownFormat {
        id: "gen9championsbssregmc",
        stream_id: "gen9championsbssregmc",
        validation: Some("gen9championsbssregmc"),
        level: LevelRule::Adjust(50),
        vgc_timer: true,
        listed: true,
    },
    ShowdownFormat {
        id: "gen9vgc2025regi",
        stream_id: "gen9vgc2025regi",
        validation: Some("gen9vgc2025regi"),
        // `!! Adjust Level = 50` (every Pokémon at level 50), where the
        // conformance preset mirrors a custom game with `Adjust Level Down`.
        level: LevelRule::Adjust(50),
        vgc_timer: true,
        listed: true,
    },
    // Smogon's OverUsed: Showdown's id is the preset's.
    ShowdownFormat {
        id: "gen9ou",
        stream_id: "gen9ou",
        validation: Some("gen9ou"),
        level: LevelRule::AsSet,
        vgc_timer: false,
        listed: true,
    },
    // Showdown's random battles: the server draws the teams
    // (`pikapro-teams`' random-battle sets; a player's team is ignored, as
    // Showdown's `getTeam` ignores it).
    ShowdownFormat {
        id: "gen9randombattle",
        stream_id: "gen9randombattle",
        validation: None,
        level: LevelRule::AsSet,
        vgc_timer: false,
        listed: true,
    },
    ShowdownFormat {
        id: "gen9randomdoublesbattle",
        stream_id: "gen9randomdoublesbattle",
        validation: None,
        level: LevelRule::AsSet,
        vgc_timer: false,
        listed: true,
    },
    ShowdownFormat {
        id: "gen9customgame",
        stream_id: "gen9customgame@@@!Team Preview",
        validation: None,
        level: LevelRule::AsSet,
        vgc_timer: false,
        listed: true,
    },
    ShowdownFormat {
        id: "gen9doublescustomgame",
        stream_id: "gen9doublescustomgame@@@!Team Preview",
        validation: None,
        level: LevelRule::AsSet,
        vgc_timer: false,
        listed: true,
    },
    ShowdownFormat {
        id: "championsvgc",
        stream_id: "championsvgc",
        validation: Some("gen9championsvgc2026regmc"),
        level: LevelRule::Adjust(50),
        vgc_timer: true,
        listed: false,
    },
    ShowdownFormat {
        id: "championsbss",
        stream_id: "championsbss",
        validation: Some("gen9championsbssregmc"),
        level: LevelRule::Adjust(50),
        vgc_timer: true,
        listed: false,
    },
    ShowdownFormat {
        id: "gen9vgc",
        stream_id: "gen9vgc",
        validation: Some("gen9vgc2025regi"),
        level: LevelRule::AdjustDown(50),
        vgc_timer: false,
        listed: false,
    },
    ShowdownFormat {
        id: "gen9singles",
        stream_id: "gen9singles",
        validation: None,
        level: LevelRule::AsSet,
        vgc_timer: false,
        listed: false,
    },
    ShowdownFormat {
        id: "gen9doubles",
        stream_id: "gen9doubles",
        validation: None,
        level: LevelRule::AsSet,
        vgc_timer: false,
        listed: false,
    },
];

/// The format with this id (compared as Showdown ids: case, spaces and
/// punctuation ignored).
pub fn find(id: &str) -> Option<&'static ShowdownFormat> {
    let id = crate::util::to_id(id);
    FORMATS.iter().find(|f| f.id == id)
}

impl ShowdownFormat {
    /// What `pikapro-server` plays for [`stream_id`](Self::stream_id): the
    /// battle's format and its header.
    pub fn stream(&self) -> &'static StreamFormat {
        static STREAMS: OnceLock<Vec<StreamFormat>> = OnceLock::new();
        let streams = STREAMS.get_or_init(|| {
            FORMATS
                .iter()
                .map(|f| {
                    pikapro_server::format::resolve(f.stream_id)
                        .unwrap_or_else(|| panic!("pikapro-server plays {}", f.stream_id))
                })
                .collect()
        });
        let i = FORMATS.iter().position(|f| f.id == self.id).expect("a format of the table");
        &streams[i]
    }

    /// The pikapro format battles run in: the preset as the stream plays
    /// it, without the validator's rules (teams are read and checked
    /// structurally in it).
    pub fn format(&self) -> &'static Format {
        &self.stream().format
    }

    /// Showdown's name (`|updatesearch|` game titles, the formats list):
    /// the battle's `|tier|`.
    pub fn name(&self) -> &'static str {
        &self.stream().info.tier
    }

    /// Showdown's `Open Team Sheets` rule: `/acceptopenteamsheets` and
    /// `/rejectopenteamsheets` during team preview.
    pub fn open_team_sheets(&self) -> bool {
        self.stream().info.open_team_sheets
    }

    /// The game's title in `|updatesearch|` (Showdown's `RoomBattle.title`).
    pub fn battle_title(&self) -> String {
        format!("{} Battle", self.name())
    }

    /// Whether the format is doubles.
    pub fn is_doubles(&self) -> bool {
        self.format().active_per_side() == 2
    }

    /// Showdown's `|formats|` message (`GlobalRoomState.formatListText`):
    /// one section with the listed formats and their display codes
    /// (searchable, challengeable, tournaments; level 50). Never Showdown's
    /// `bestOfDefault` bit (64): this server runs no best-of series, and a
    /// client seeing the bit (Showdown's own) would default a challenge in
    /// the format to `@@@Best of = 3`, which the server refuses.
    pub fn formats_list() -> String {
        let mut out = String::from("|formats|,LL|,1|pikapro");
        for f in FORMATS.iter().filter(|f| f.listed) {
            let mut code = 2 | 4 | 8;
            if f.format().random_teams {
                code |= 1;
            }
            if matches!(f.level, LevelRule::Adjust(50) | LevelRule::AdjustDown(50)) {
                code |= 16;
            }
            out.push_str(&format!("|{},{code:x}", f.name()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_list_advertises_no_best_of() {
        // `|formats|,LL|,1|pikapro|NAME,CODE|...`: no series here, so no
        // format carries Showdown's `bestOfDefault` bit.
        let list = ShowdownFormat::formats_list();
        let entries: Vec<&str> = list.split('|').skip(5).collect();
        assert_eq!(entries.len(), FORMATS.iter().filter(|f| f.listed).count());
        for entry in entries {
            let code = u32::from_str_radix(entry.rsplit(',').next().unwrap(), 16).unwrap();
            assert_eq!(code & 64, 0, "{entry}");
            assert_eq!(code & (2 | 4 | 8), 2 | 4 | 8, "{entry}");
        }
    }

    #[test]
    fn formats_resolve() {
        assert_eq!(find("gen9championsvgc2026regmc").unwrap().format().id, "championsvgc");
        assert!(find("gen9championsvgc2026regmb").is_none());
        assert_eq!(find("[Gen 9 Champions] BSS Reg M-C").unwrap().format().id, "championsbss");
        assert!(!find("gen9championsbssregmc").unwrap().is_doubles());
        assert!(find("gen9championsbssregmb").is_none());
        assert_eq!(find("[Gen 9] VGC 2025 Reg I").unwrap().id, "gen9vgc2025regi");
        assert_eq!(find("gen9customgame").unwrap().format().id, "gen9singles");
        let random = find("gen9randombattle").unwrap();
        assert!(random.format().random_teams && random.validation.is_none() && random.listed);
        // Smogon's OverUsed, under Showdown's id (the preset's too):
        // validated as `gen9ou`, sets at their own levels.
        let ou = find("[Gen 9] OU").unwrap();
        assert_eq!(
            (ou.id, ou.format().id, ou.validation, ou.level),
            ("gen9ou", "gen9ou", Some("gen9ou"), LevelRule::AsSet)
        );
        assert!(!ou.is_doubles() && ou.listed);
        assert!(find("gen9ubers").is_none());
        let names: Vec<&str> = FORMATS.iter().map(|f| f.name()).collect();
        assert_eq!(
            names,
            [
                "[Gen 9 Champions] VGC 2026 Reg M-C",
                "[Gen 9 Champions] BSS Reg M-C",
                "[Gen 9] VGC 2025 Reg I",
                "[Gen 9] OU",
                "[Gen 9] Random Battle",
                "[Gen 9] Random Doubles Battle",
                "[Gen 9] Custom Game",
                "[Gen 9] Doubles Custom Game",
                "[Gen 9 Champions] VGC 2026 Reg M-C",
                "[Gen 9 Champions] BSS Reg M-C",
                "[Gen 9] Doubles Custom Game",
                "[Gen 9] Custom Game",
                "[Gen 9] Doubles Custom Game",
            ]
        );
        let ots: Vec<bool> = FORMATS.iter().map(|f| f.open_team_sheets()).collect();
        assert_eq!(ots, [true, false, true, false, false, false, false, false, true, false, false, false, false]);
        for f in FORMATS {
            if let Some(v) = f.validation {
                assert!(pikapro_validate::ValidationFormat::by_id(v).is_some(), "{v}");
            }
            // The preset's level rule, but for Reg I's `Adjust Level`.
            let preset = Format::by_id(f.format().id).unwrap();
            if f.id != "gen9vgc2025regi" {
                assert_eq!(f.level, preset.level, "{}", f.id);
            }
        }
        // Reg I is the stream's: Obtainable Abilities and the cartridge's
        // overflows, which the `gen9vgc` preset (a custom game) lacks.
        let reg_i = find("gen9vgc2025regi").unwrap().format();
        assert!(reg_i.obtainable_abilities && reg_i.cartridge_overflow);
    }
}
