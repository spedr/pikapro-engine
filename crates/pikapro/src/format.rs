//! Formats and rulesets.
//!
//! A [`Format`] is plain data: it selects the data tables ([`Dex`]), the
//! mechanics family and every rule toggle. Mechanics code branches on these
//! fields, never on a format *name*. New formats are new values, not new code.

use pikapro_data::Dex;

/// Number of active Pokémon per side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GameType {
    /// One active Pokémon per side.
    Singles,
    /// Two active Pokémon per side.
    Doubles,
}

impl GameType {
    /// 1 or 2.
    #[inline]
    pub const fn active_per_side(self) -> usize {
        match self {
            GameType::Singles => 1,
            GameType::Doubles => 2,
        }
    }
}

/// Which mechanics family a battle follows. Use this for behavioural
/// differences between games that share a generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mechanics {
    /// Pokémon Scarlet/Violet.
    ScarletViolet,
    /// Pokémon Champions (2026): the Gen 9 engine as Showdown's `champions`
    /// mod changes it (Stat Points, its own dex, Mega Evolution); see
    /// docs/FORMATS.md.
    Champions,
}

/// How a set's stat investment is expressed and validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatSystem {
    /// EVs (≤252 per stat, ≤510 total) and IVs (0–31).
    EffortValues,
    /// Champions Stat Points: ≤32 per stat, ≤66 total. IVs are fixed at 31.
    StatPoints,
}

/// A once-per-battle transformation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Gimmick {
    /// Terastallization (Scarlet/Violet).
    Terastallize = 1 << 0,
    /// Mega Evolution (Champions).
    MegaEvolve = 1 << 1,
}

/// Set of [`Gimmick`]s (a format's allowed set, or a side's used set).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Gimmicks(pub u8);

impl Gimmicks {
    /// The empty set.
    pub const NONE: Gimmicks = Gimmicks(0);

    /// This set and `g`.
    #[inline]
    pub const fn with(self, g: Gimmick) -> Gimmicks {
        Gimmicks(self.0 | g as u8)
    }

    /// Whether `g` is in the set.
    #[inline]
    pub const fn contains(self, g: Gimmick) -> bool {
        self.0 & g as u8 != 0
    }

    /// Adds `g`.
    #[inline]
    pub fn insert(&mut self, g: Gimmick) {
        self.0 |= g as u8;
    }
}

/// How set levels become battle levels (Showdown's level rules).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LevelRule {
    /// Use each set's level.
    #[default]
    AsSet,
    /// `Adjust Level Down = n`: levels above `n` are lowered to `n`
    /// (Scarlet/Violet Flat Rules).
    AdjustDown(u8),
    /// `Adjust Level = n`: every Pokémon battles at exactly `n`
    /// (Champions Flat Rules).
    Adjust(u8),
}

impl LevelRule {
    /// The battle level for a set registered at `level`.
    #[inline]
    pub const fn apply(self, level: u8) -> u8 {
        match self {
            LevelRule::AsSet => level,
            LevelRule::AdjustDown(n) if level > n => n,
            LevelRule::AdjustDown(_) => level,
            LevelRule::Adjust(n) => n,
        }
    }
}

/// Team-validation clauses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Clauses {
    /// No two Pokémon of the same species: the same National Dex number,
    /// so two formes count too (Showdown's Species Clause).
    pub species: bool,
    /// No two Pokémon holding the same item.
    pub item: bool,
}

/// Team preview: whether the players see each other's teams before the
/// battle, and how many Pokémon each then brings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TeamPreview {
    /// No team preview: each team battles as registered, its first Pokémon
    /// leading (Showdown's `!Team Preview`).
    #[default]
    None,
    /// Team preview without a picked team size: the whole team is brought,
    /// and the choice sets its order (Showdown's `Team Preview` alone, as
    /// in the Smogon tiers: `|teampreview` without a count, a request
    /// without `maxChosenTeamSize`).
    All,
    /// Team preview, each player brings this many Pokémon (Showdown's
    /// `Picked Team Size = n`; VGC: 4). It must be at least 1: with
    /// `Pick(0)` no team is valid.
    Pick(u8),
}

impl TeamPreview {
    /// Whether there is a team preview.
    #[inline]
    pub const fn shown(self) -> bool {
        !matches!(self, TeamPreview::None)
    }

    /// Showdown's `ruleTable.pickedTeamSize`: `Some(n)` for
    /// [`TeamPreview::Pick`].
    #[inline]
    pub const fn picked(self) -> Option<u8> {
        match self {
            TeamPreview::Pick(n) => Some(n),
            TeamPreview::None | TeamPreview::All => None,
        }
    }

    /// How many Pokémon a team of `team_len` brings to the battle
    /// (Showdown's `side.pickedTeamSize()`): all of them, or at most the
    /// picked size.
    #[inline]
    pub const fn brought(self, team_len: usize) -> usize {
        match self {
            TeamPreview::Pick(n) if (n as usize) < team_len => n as usize,
            _ => team_len,
        }
    }
}

/// A format: the data tables and every rule a battle follows.
///
/// Start from a preset ([`Format::gen9_singles`], [`Format::gen9_doubles`],
/// [`Format::gen9_vgc`], [`Format::gen9_ou`], [`Format::gen9_random_battle`],
/// [`Format::gen9_random_doubles_battle`], [`Format::champions_vgc`],
/// [`Format::champions_bss`], or [`Format::by_id`])
/// and change fields with struct update syntax for a custom ruleset:
///
/// ```
/// use pikapro::Format;
/// let short = Format { turn_limit: 100, ..Format::gen9_singles() };
/// assert_eq!(short.active_per_side(), 1);
/// ```
///
/// Each preset is checked against the Showdown format it mirrors
/// (`pikapro-conformance`'s `showdown_format`); a custom one has no such
/// reference.
#[derive(Clone, Debug)]
pub struct Format {
    /// Stable identifier, e.g. `"gen9vgc"`. It names the preset:
    /// pikapro-validate and pikapro-teams look up their rules by it, so
    /// give a custom format its own id.
    pub id: &'static str,
    /// The species, moves, abilities, items and type chart.
    pub dex: &'static Dex,
    /// The mechanics family.
    pub mechanics: Mechanics,
    /// Singles or doubles.
    pub game_type: GameType,
    /// EVs and IVs, or Stat Points.
    pub stat_system: StatSystem,
    /// Minimum and maximum team size at registration. A minimum of 0 counts
    /// as 1 ([`Team::validate`](crate::Team::validate)).
    pub team_size: (u8, u8),
    /// Whether there is a team preview, and how many Pokémon each player
    /// brings from it.
    pub team_preview: TeamPreview,
    /// How set levels become battle levels.
    pub level: LevelRule,
    /// The gimmicks a side may use (each once per battle).
    pub gimmicks: Gimmicks,
    /// Species and Item Clause, checked by [`Team::validate`](crate::Team::validate).
    pub clauses: Clauses,
    /// The battle ends in a tie after this many turns.
    pub turn_limit: u16,
    /// Showdown's `ruleTable.has('obtainableabilities')`, which its
    /// simulator reads for `maybeTrapped`: at the end of each turn a Pokémon
    /// is also `maybeTrapped` (in its request) by a trapping ability any
    /// foe's species could have. Championsvgc's Showdown format has it; the
    /// custom games the Gen 9 presets mirror don't (a server playing
    /// `[Gen 9] VGC 2025 Reg I` on the `gen9vgc` preset turns it on).
    pub obtainable_abilities: bool,
    /// The cartridge's integer overflows, which Showdown's `trunc(num,
    /// bits)` reproduces: damage wraps at 16 bits and the turn count Wish
    /// and future moves compare (`getOverflowedTurnCount`) at 8. Showdown's
    /// custom games, which the Gen 9 presets are checked against, turn them
    /// off (`battle: { trunc: Math.trunc }`).
    pub cartridge_overflow: bool,
    /// Showdown's Endless Battle Clause (`maybeTriggerEndlessBattleClause`
    /// in `sim/battle.js`): after turn 100, a battle in which every side's
    /// active Pokémon are stale (they restored PP or HP with a berry they
    /// got back, or a foe did it for them) and a side can't switch to one
    /// that isn't ends, lost by the side whose team could cycle a
    /// restorative berry. Smogon's tiers have it (`Standard AG`); the
    /// custom games and the VGC and Champions formats don't.
    pub endless_battle_clause: bool,
    /// The abilities the format bans (Showdown's `-ability:` rules) among
    /// those that can trap: `endTurn` skips them when it marks a Pokémon
    /// `maybeTrapped` by an ability a foe's species could have
    /// ([`obtainable_abilities`](Format::obtainable_abilities)). Bans of
    /// other abilities change nothing in a battle and aren't listed (the
    /// team validator, `pikapro-validate`, has the whole ban list).
    pub banned_trapping_abilities: &'static [pikapro_data::AbilityId],
    /// Showdown's Sleep Clause Mod (`data/rulesets.js`, `onSetStatus`, a
    /// field handler sorting last of the `SetStatus` event): sleep from a
    /// foe fails while a Pokémon of the target's side is asleep and its
    /// sleep didn't come from its own side. The random battles have it;
    /// no other preset does (OU's Sleep Moves Clause is a validator rule).
    pub sleep_clause_mod: bool,
    /// Showdown's Illusion Level Mod (`sim/pokemon.js` `getFullDetails`,
    /// `data/abilities.js` Illusion): a disguised Pokémon's details show
    /// the disguise's level instead of its own, and the end of the
    /// disguise gives a hint once per battle. The random battles have it.
    pub illusion_level_mod: bool,
    /// Showdown's `format.team`: the format's teams are random, drawn for
    /// the players rather than brought and validated. pikapro-teams then
    /// draws them from its random-battle sets
    /// ([`gen9_random_battle`](Format::gen9_random_battle)); the battle
    /// itself is unaffected.
    pub random_teams: bool,
}

impl Format {
    /// Gen 9 singles as a custom game: 6v6 at the sets' levels, no team
    /// preview, no clauses, Tera allowed.
    pub const fn gen9_singles() -> Format {
        Format {
            id: "gen9singles",
            dex: &pikapro_data::gen9::DEX,
            mechanics: Mechanics::ScarletViolet,
            game_type: GameType::Singles,
            stat_system: StatSystem::EffortValues,
            team_size: (1, 6),
            team_preview: TeamPreview::None,
            level: LevelRule::AsSet,
            gimmicks: Gimmicks::NONE.with(Gimmick::Terastallize),
            clauses: Clauses { species: false, item: false },
            turn_limit: 1000,
            obtainable_abilities: false,
            cartridge_overflow: false,
            endless_battle_clause: false,
            banned_trapping_abilities: &[],
            sleep_clause_mod: false,
            illusion_level_mod: false,
            random_teams: false,
        }
    }

    /// Gen 9 doubles as a custom game: no team preview, no VGC
    /// restrictions.
    ///
    /// Like Showdown's custom game, it accepts a one-Pokémon team; Showdown
    /// itself then fails (a `TypeError` in `battle.js` when the lone
    /// Pokémon's ally slot is read), so such a battle has no reference to
    /// be checked against.
    pub const fn gen9_doubles() -> Format {
        Format { id: "gen9doubles", game_type: GameType::Doubles, ..Format::gen9_singles() }
    }

    /// Scarlet/Violet VGC (Flat Rules): doubles, bring 4-6 pick 4, levels
    /// adjusted down to 50, species and item clause, Tera allowed.
    pub const fn gen9_vgc() -> Format {
        Format {
            id: "gen9vgc",
            game_type: GameType::Doubles,
            team_size: (4, 6),
            team_preview: TeamPreview::Pick(4),
            level: LevelRule::AdjustDown(50),
            clauses: Clauses { species: true, item: true },
            ..Format::gen9_singles()
        }
    }

    /// Smogon's Gen 9 OverUsed (Showdown's `[Gen 9] OU`): singles, teams of
    /// 1 to 6 at their set levels (100 by default), team preview with the
    /// whole team brought, Species Clause, Tera allowed, Endless Battle
    /// Clause. Unlike the custom games, it is a rated format's battle:
    /// foes' HP in percent, the cartridge's overflows, and `maybeTrapped`
    /// by the abilities a foe's species could have (except the banned
    /// Arena Trap and Shadow Tag). Its bans and clauses on sets (the tier,
    /// OHKO, evasion and sleep moves) are the team validator's
    /// (`pikapro-validate`), not the battle's.
    pub const fn gen9_ou() -> Format {
        Format {
            id: "gen9ou",
            team_preview: TeamPreview::All,
            clauses: Clauses { species: true, item: false },
            obtainable_abilities: true,
            cartridge_overflow: true,
            endless_battle_clause: true,
            banned_trapping_abilities: &[
                pikapro_data::gen9::abilities::ARENA_TRAP,
                pikapro_data::gen9::abilities::SHADOW_TAG,
            ],
            ..Format::gen9_singles()
        }
    }

    /// Showdown's `[Gen 9] Random Battle`: singles, teams at their
    /// levels (generated teams have six; as in Showdown, the battle takes
    /// any size), no team preview, Species Clause, Tera allowed, foes' HP in
    /// percent, the cartridge's overflows, `maybeTrapped` by the abilities
    /// a foe's species could have, Sleep Clause Mod and Illusion Level Mod.
    /// The battles are Showdown's; the teams are supplied (`pikapro-teams`
    /// draws them from its random-battle sets), not its generator's.
    pub const fn gen9_random_battle() -> Format {
        Format {
            id: "gen9randombattle",
            clauses: Clauses { species: true, item: false },
            obtainable_abilities: true,
            cartridge_overflow: true,
            sleep_clause_mod: true,
            illusion_level_mod: true,
            random_teams: true,
            ..Format::gen9_singles()
        }
    }

    /// Showdown's `[Gen 9] Random Doubles Battle`: `gen9randombattle` in
    /// doubles.
    pub const fn gen9_random_doubles_battle() -> Format {
        Format { id: "gen9randomdoublesbattle", game_type: GameType::Doubles, ..Format::gen9_random_battle() }
    }

    /// Pokémon Champions ranked doubles (Regulation M-*, Champions Flat
    /// Rules): doubles, bring exactly 6 pick 4, every Pokémon at level 50,
    /// Stat Points, species and item clause, Mega Evolution.
    pub const fn champions_vgc() -> Format {
        Format {
            id: "championsvgc",
            dex: &pikapro_data::champions::DEX,
            mechanics: Mechanics::Champions,
            stat_system: StatSystem::StatPoints,
            team_size: (6, 6),
            level: LevelRule::Adjust(50),
            gimmicks: Gimmicks::NONE.with(Gimmick::MegaEvolve),
            obtainable_abilities: true,
            cartridge_overflow: true,
            ..Format::gen9_vgc()
        }
    }

    /// Pokémon Champions Battle Spot Singles (Showdown's `[Gen 9 Champions]
    /// BSS Reg M-C`: the Champions mod's Flat Rules and VGC Timer):
    /// singles, bring exactly 6 pick 3 (`Picked Team Size = Auto`, 3 in
    /// singles), every Pokémon at level 50, Stat Points, species and item
    /// clause, Mega Evolution. Unlike `championsvgc`, no Open Team Sheets.
    pub const fn champions_bss() -> Format {
        Format {
            id: "championsbss",
            game_type: GameType::Singles,
            team_preview: TeamPreview::Pick(3),
            ..Format::champions_vgc()
        }
    }

    /// All built-in presets. A slice, so that a release with more presets
    /// has the same signature: don't rely on its length or order.
    pub fn presets() -> &'static [Format] {
        static PRESETS: [Format; 8] = [
            Format::gen9_singles(),
            Format::gen9_doubles(),
            Format::gen9_vgc(),
            Format::gen9_ou(),
            Format::gen9_random_battle(),
            Format::gen9_random_doubles_battle(),
            Format::champions_vgc(),
            Format::champions_bss(),
        ];
        &PRESETS
    }

    /// The preset with this id (`gen9singles`, `gen9doubles`, `gen9vgc`,
    /// `gen9ou`, `gen9randombattle`, `gen9randomdoublesbattle`, `championsvgc`,
    /// `championsbss`), compared as Showdown compares ids
    /// (case and punctuation ignored).
    pub fn by_id(id: &str) -> Option<Format> {
        Format::presets().iter().find(|f| pikapro_data::id_eq(f.id, id)).cloned()
    }

    /// Whether `other` has the same rules: every field equal (the `dex`
    /// the same table). A format made from a preset keeps the preset's
    /// `id`, whatever it changed, so the id alone doesn't tell.
    pub fn same_rules(&self, other: &Format) -> bool {
        // Destructured, so that a new field is a compile error here (and
        // only here: code outside this crate should compare formats with
        // this, not field by field).
        let Format {
            id,
            dex,
            mechanics,
            game_type,
            stat_system,
            team_size,
            team_preview,
            level,
            gimmicks,
            clauses,
            turn_limit,
            obtainable_abilities,
            cartridge_overflow,
            endless_battle_clause,
            banned_trapping_abilities,
            sleep_clause_mod,
            illusion_level_mod,
            random_teams,
        } = self;
        *id == other.id
            && core::ptr::eq(*dex, other.dex)
            && *mechanics == other.mechanics
            && *game_type == other.game_type
            && *stat_system == other.stat_system
            && *team_size == other.team_size
            && *team_preview == other.team_preview
            && *level == other.level
            && *gimmicks == other.gimmicks
            && *clauses == other.clauses
            && *turn_limit == other.turn_limit
            && *obtainable_abilities == other.obtainable_abilities
            && *cartridge_overflow == other.cartridge_overflow
            && *endless_battle_clause == other.endless_battle_clause
            && *banned_trapping_abilities == other.banned_trapping_abilities
            && *sleep_clause_mod == other.sleep_clause_mod
            && *illusion_level_mod == other.illusion_level_mod
            && *random_teams == other.random_teams
    }

    /// Active Pokémon per side: 1 or 2.
    #[inline]
    pub fn active_per_side(&self) -> usize {
        self.game_type.active_per_side()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_rules() {
        assert_eq!(LevelRule::AsSet.apply(30), 30);
        assert_eq!(LevelRule::AdjustDown(50).apply(100), 50);
        assert_eq!(LevelRule::AdjustDown(50).apply(30), 30);
        assert_eq!(LevelRule::Adjust(50).apply(30), 50);
        assert_eq!(LevelRule::Adjust(50).apply(100), 50);
    }

    #[test]
    fn presets_are_found_by_id_and_told_apart_by_their_rules() {
        let presets = Format::presets();
        assert!(presets.len() >= 6);
        for (i, a) in presets.iter().enumerate() {
            assert!(Format::by_id(a.id).is_some_and(|f| f.same_rules(a)), "{}", a.id);
            assert!(Format::by_id(&a.id.to_uppercase()).is_some_and(|f| f.same_rules(a)));
            for (j, b) in presets.iter().enumerate() {
                assert_eq!(a.same_rules(b), i == j, "{} and {}", a.id, b.id);
            }
            // The preset's id, other rules.
            assert!(!a.same_rules(&Format { turn_limit: a.turn_limit - 1, ..a.clone() }));
            let dex = if core::ptr::eq(a.dex, &pikapro_data::gen9::DEX) {
                &pikapro_data::champions::DEX
            } else {
                &pikapro_data::gen9::DEX
            };
            assert!(!a.same_rules(&Format { dex, ..a.clone() }));
        }
        assert!(Format::by_id("gen9ubers").is_none());
        let ou = Format::by_id("[Gen 9] OU").expect("gen9ou");
        assert_eq!((ou.team_preview, ou.endless_battle_clause), (TeamPreview::All, true));
        assert!(ou.same_rules(&Format::gen9_ou()));
        assert!(Format::gen9_vgc().same_rules(&Format::gen9_vgc()));
        let random = Format::by_id("[Gen 9] Random Battle").expect("gen9randombattle");
        assert!(
            random.same_rules(&Format::gen9_random_battle()) && random.sleep_clause_mod && random.illusion_level_mod
        );
        let doubles = Format::by_id("[Gen 9] Random Doubles Battle").expect("gen9randomdoublesbattle");
        assert_eq!(doubles.game_type, GameType::Doubles);
    }
}
