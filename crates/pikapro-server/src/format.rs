//! Which formats the stream accepts, and how each is set up.
//!
//! A `>start`'s `formatid` is either one of Showdown's formats that a
//! pikapro preset mirrors (the formats `pikapro-conformance` checks each
//! preset against, see `docs/CONFORMANCE.md`) or the preset's own id,
//! which is played as that Showdown format.
//!
//! | Showdown `formatid` | preset |
//! |---|---|
//! | `gen9customgame@@@!Team Preview` | `gen9singles` |
//! | `gen9doublescustomgame@@@!Team Preview` | `gen9doubles` |
//! | `gen9doublescustomgame@@@Picked Team Size = 4,Adjust Level Down = 50` | `gen9vgc` |
//! | `gen9championsvgc2026regmc` | `championsvgc` |
//! | `gen9championsvgc2026regmb` | `championsvgc` |
//! | `gen9championsbssregmc` | `championsbss` |
//! | `gen9championsbssregmb` | `championsbss` |
//! | `gen9vgc2025regi` | `gen9vgc` |
//! | `gen9ou` | `gen9ou` |
//! | `gen9randombattle` | `gen9randombattle` |
//! | `gen9randomdoublesbattle` | `gen9randomdoublesbattle` |
//!
//! `[Gen 9 Champions] VGC 2026 Reg M-B` (and `BSS Reg M-B`, played as
//! `championsbss` the same way) has a mod of its own. On
//! Showdown master (the pinned oracle) its `championsregmb` mod inherits
//! the `champions` scripts, so it plays as `championsvgc` (Reg M-C) with
//! its own header and the mod's data. Of that data, only `moves.ts` changes
//! a battle: Wish and Strength Sap have 10 PP, not Reg M-C's 5. The stream
//! plays it on [`reg_mb_dex`], the Champions dex with that overlay, so
//! requests show Showdown's `pp` / `maxpp`. The rest of the mod
//! (`formats-data`, `items`, `learnsets`) is legality, which the stream
//! doesn't check.
//!
//! `[Gen 9] VGC 2025 Reg I` is the format `pikapro-showdown-server` offers for
//! the `gen9vgc` preset: the same battle as the doubles custom game above
//! (Picked Team Size 4, which Flat Rules' `Auto` is in doubles; Cancel
//! Mod), with its own header (tier, the Species and Item Clause rules,
//! Open Team Sheets) and without the custom game's `debug` (no exact HP in
//! the shared half of a `|split|` block). Its `!! Adjust Level = 50`, like
//! the rest of Flat Rules, is the validator's. Reg I has Obtainable
//! Abilities: requests show `maybeTrapped` next to a foe whose species
//! could have a trapping ability. The stream enables that behavior and
//! cartridge integer overflows, as in Showdown's Reg I format.
//!
//! The format name is read as Showdown's `Dex.formats.get` does (trimmed,
//! `toID`, custom rules after `@@@`); the custom rules may come in any
//! order and spacing, and are shown as written, as Showdown shows them.
//!
//! Showdown's simulator does not validate teams: level rules (`Adjust
//! Level Down`, `Adjust Level`) and clauses belong to its team validator,
//! so a battle started over the stream plays each set at its own level and
//! allows duplicate species and items. The preset is changed accordingly
//! ([`LevelRule::AsSet`], no clauses, any team of one to six Pokémon).

use std::sync::OnceLock;

use pikapro::format::{Clauses, LevelRule};
use pikapro::{Format, GameType};
use pikapro_data::{Dex, MoveData};
use pikapro_protocol::BattleInfo;

/// A format the stream plays.
#[derive(Clone, Debug)]
pub struct StreamFormat {
    /// The preset, adapted to an unvalidated battle.
    pub format: Format,
    /// The header Showdown writes for the format (with the custom rules as
    /// given).
    pub info: BattleInfo,
    /// Showdown's `format.debug` (custom games): `|split|` blocks show
    /// exact HP to everyone, and Showdown writes `|debug|` lines, which
    /// pikapro does not.
    pub debug: bool,
}

/// Showdown's `toID`.
pub use pikapro_data::to_id;

/// A custom rule, compared without case and spacing.
fn rule_key(rule: &str) -> String {
    rule.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

/// The format a `formatid` names, if the stream can play it.
pub fn resolve(formatid: &str) -> Option<StreamFormat> {
    let name = formatid.trim();
    let (base, rules) = match name.split_once("@@@") {
        Some((base, rules)) => (base.trim(), Some(rules)),
        None => (name, None),
    };
    let custom: Vec<String> = rules.map(|r| r.split(',').map(str::to_string).collect()).unwrap_or_default();
    let mut keys: Vec<String> = custom.iter().map(|r| rule_key(r)).collect();
    keys.sort();
    keys.dedup();
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    let preset = match (to_id(base).as_str(), keys.as_slice()) {
        ("gen9customgame", ["!teampreview"]) => Format::gen9_singles(),
        ("gen9doublescustomgame", ["!teampreview"]) => Format::gen9_doubles(),
        ("gen9doublescustomgame", ["adjustleveldown=50", "pickedteamsize=4"]) => Format::gen9_vgc(),
        ("gen9championsvgc2026regmc", []) => Format::champions_vgc(),
        ("gen9championsvgc2026regmb", []) => {
            let mut info = BattleInfo::for_format(&Format::champions_vgc());
            info.tier = "[Gen 9 Champions] VGC 2026 Reg M-B".into();
            return Some(adapt(Format { dex: reg_mb_dex(), ..Format::champions_vgc() }, info));
        }
        ("gen9championsbssregmc", []) => Format::champions_bss(),
        ("gen9championsbssregmb", []) => {
            let mut info = BattleInfo::for_format(&Format::champions_bss());
            info.tier = "[Gen 9 Champions] BSS Reg M-B".into();
            return Some(adapt(Format { dex: reg_mb_dex(), ..Format::champions_bss() }, info));
        }
        // Reg I has Obtainable Abilities (`maybeTrapped` by foes' species)
        // and, not being a custom game, the cartridge's overflows.
        ("gen9vgc2025regi", []) => {
            let format = Format { obtainable_abilities: true, cartridge_overflow: true, ..Format::gen9_vgc() };
            return Some(adapt(format, reg_i_info()));
        }
        // Smogon's OverUsed: Showdown's id is the preset's.
        ("gen9ou", []) => Format::gen9_ou(),
        // Showdown's random battles: the same (the teams are given).
        ("gen9randombattle", []) => Format::gen9_random_battle(),
        ("gen9randomdoublesbattle", []) => Format::gen9_random_doubles_battle(),
        // pikapro's own ids: the Showdown format each mirrors.
        (_, []) if rules.is_none() => {
            let preset = Format::by_id(name)?;
            let mut info = BattleInfo::for_format(&preset);
            info.timestamp = 0;
            return Some(adapt(preset, info));
        }
        _ => return None,
    };
    let mut info = BattleInfo::for_format(&preset);
    if !custom.is_empty() {
        info.custom_rules = custom;
    }
    Some(adapt(preset, info))
}

/// Reg M-B's dex: Showdown master's `championsregmb` mod over the
/// Champions dex. Its `moves.ts` gives Wish and Strength Sap `pp: 10`
/// (`inherit: true`); nothing else in the mod is read during a battle.
pub fn reg_mb_dex() -> &'static Dex {
    static MOVES: OnceLock<Vec<MoveData>> = OnceLock::new();
    static DEX: OnceLock<Dex> = OnceLock::new();
    DEX.get_or_init(|| {
        let base = &pikapro_data::champions::DEX;
        let moves = MOVES.get_or_init(|| {
            let mut moves = base.moves.to_vec();
            for id in [pikapro_data::gen9::moves::WISH, pikapro_data::gen9::moves::STRENGTH_SAP] {
                moves[id.index()].pp = 10;
            }
            moves
        });
        Dex { name: "championsregmb", moves, ..*base }
    })
}

/// `[Gen 9] VGC 2025 Reg I`'s header: the `onBegin` messages of Species
/// Clause and Item Clause, and Open Team Sheets' buttons at team preview.
fn reg_i_info() -> BattleInfo {
    BattleInfo::for_format(&Format::gen9_vgc()).with_flat_rules("[Gen 9] VGC 2025 Reg I", true)
}

fn adapt(preset: Format, info: BattleInfo) -> StreamFormat {
    let debug = info.report_exact_hp;
    let format = Format { level: LevelRule::AsSet, clauses: Clauses::default(), team_size: (1, 6), ..preset };
    StreamFormat { format, info, debug }
}

/// `|gametype|`.
pub(crate) fn game_type(format: &Format) -> &'static str {
    match format.game_type {
        GameType::Singles => "singles",
        GameType::Doubles => "doubles",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_showdown_ids_to_presets() {
        let f = |id: &str| resolve(id).map(|f| f.format.id);
        assert_eq!(f("gen9customgame@@@!Team Preview"), Some("gen9singles"));
        assert_eq!(f(" gen9doublescustomgame @@@!team preview"), Some("gen9doubles"));
        assert_eq!(f("gen9doublescustomgame@@@Adjust Level Down = 50, Picked Team Size = 4"), Some("gen9vgc"));
        assert_eq!(f("[Gen 9 Champions] VGC 2026 Reg M-C"), Some("championsvgc"));
        assert_eq!(f("[Gen 9 Champions] VGC 2026 Reg M-B"), Some("championsvgc"));
        assert_eq!(resolve("gen9championsvgc2026regmc").unwrap().info.tier, "[Gen 9 Champions] VGC 2026 Reg M-C");
        assert_eq!(resolve("gen9championsvgc2026regmb").unwrap().info.tier, "[Gen 9 Champions] VGC 2026 Reg M-B");
        assert_eq!(f("championsvgc"), Some("championsvgc"));
        assert_eq!(f("[Gen 9 Champions] BSS Reg M-C"), Some("championsbss"));
        assert_eq!(f("[Gen 9 Champions] BSS Reg M-B"), Some("championsbss"));
        assert_eq!(f("championsbss"), Some("championsbss"));
        let bss = resolve("gen9championsbssregmc").unwrap();
        assert_eq!(bss.info.tier, "[Gen 9 Champions] BSS Reg M-C");
        assert!(!bss.info.open_team_sheets && !bss.debug);
        assert_eq!(bss.format.team_preview, pikapro::TeamPreview::Pick(3));
        let bss_mb = resolve("gen9championsbssregmb").unwrap();
        assert_eq!(bss_mb.info.tier, "[Gen 9 Champions] BSS Reg M-B");
        assert!(core::ptr::eq(bss_mb.format.dex, reg_mb_dex()));
        assert_eq!(f("gen9vgc"), Some("gen9vgc"));
        assert_eq!(f("gen9customgame"), None);
        assert_eq!(f("gen9ubers"), None);
        let ou = resolve("[Gen 9] OU").unwrap();
        assert_eq!((ou.format.id, ou.info.tier.as_str()), ("gen9ou", "[Gen 9] OU"));
        assert_eq!(ou.info.rules.len(), 8);
        assert!(!ou.debug && !ou.info.open_team_sheets && ou.info.custom_rules.is_empty());
        assert_eq!(ou.format.team_preview, pikapro::TeamPreview::All);
        assert!(ou.format.endless_battle_clause && ou.format.cartridge_overflow);
        assert_eq!(f("gen9ou@@@!Team Preview"), None);
        let reg_i = resolve("[Gen 9] VGC 2025 Reg I").unwrap();
        assert_eq!(reg_i.format.id, "gen9vgc");
        assert_eq!(reg_i.info.tier, "[Gen 9] VGC 2025 Reg I");
        assert!(reg_i.info.custom_rules.is_empty() && reg_i.info.open_team_sheets && !reg_i.debug);
        assert_eq!(f("gen9vgc2025regi@@@!Team Preview"), None);
        let vgc = resolve("gen9doublescustomgame@@@Adjust Level Down = 50, Picked Team Size = 4").unwrap();
        assert_eq!(vgc.info.custom_rules, ["Adjust Level Down = 50", " Picked Team Size = 4"]);
        assert_eq!(vgc.format.level, LevelRule::AsSet);
        assert!(vgc.debug);
        assert!(!resolve("gen9championsvgc2026regmc").unwrap().debug);
        assert!(!resolve("gen9championsvgc2026regmb").unwrap().debug);
    }

    /// `championsregmb`'s `moves.ts`: Wish and Strength Sap have 10 PP in
    /// Reg M-B, 5 in Reg M-C; everything else is Reg M-C's.
    #[test]
    fn reg_mb_overlays_wish_and_strength_sap_pp() {
        use pikapro_data::gen9::moves::{PROTECT, STRENGTH_SAP, WISH};
        let (mb, mc) = (resolve("gen9championsvgc2026regmb").unwrap(), resolve("gen9championsvgc2026regmc").unwrap());
        for id in [WISH, STRENGTH_SAP] {
            assert_eq!((mb.format.dex.move_(id).pp, mc.format.dex.move_(id).pp), (10, 5));
        }
        assert_eq!(mb.format.dex.move_(PROTECT), mc.format.dex.move_(PROTECT));
        assert_eq!(mb.format.dex.moves.len(), mc.format.dex.moves.len());
        assert!(core::ptr::eq(mb.format.dex, reg_mb_dex()));
    }
}
