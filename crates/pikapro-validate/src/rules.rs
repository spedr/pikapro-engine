//! Validation formats: Showdown's resolved rule tables as data.
//!
//! A [`ValidationFormat`] is what Showdown's `RuleTable` holds once a
//! format's rulesets are resolved (`Dex.formats.getRuleTable`): the numeric
//! rules (team size, levels, EV limit, minimum source generation), the
//! validator rules the team validator tests with `ruleTable.has(...)`
//! (Obtainable and its sub-rules), the bans and restrictions in the table's
//! order, and the rules with an `onValidateTeam` hook. Presets transcribe
//! Showdown's tables (`node tools/showdown-oracle/validator.mjs --rules
//! <format>` prints them); the validator branches on these fields only.

use pikapro_data::legality::{Legality, SpeciesLegality, SpeciesTags, Tier};
use pikapro_data::{AbilityId, Dex, ItemId, MoveId, Nature, Nonstandard, SpeciesId};

/// A rule's prefix in a rule table: `-` bans, `+` allows, `*` restricts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sign {
    /// `-`: banned.
    Ban,
    /// `+`: allowed (overrides a ban).
    Allow,
    /// `*`: restricted (counted by the Limit Restricted rules).
    Restrict,
}

/// A tag that rules can name (Showdown's `data/tags.ts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tag {
    /// `Mythical`.
    Mythical,
    /// `Restricted Legendary`.
    RestrictedLegendary,
    /// `Sub-Legendary`.
    SubLegendary,
    /// `Ultra Beast`.
    UltraBeast,
    /// `Paradox`.
    Paradox,
    /// `Uber`: the singles tier `Uber`, `AG` or `(AG)`.
    Uber,
    /// `AG`: the singles tier `AG` or `(AG)`.
    Ag,
    /// `Past`: `isNonstandard: 'Past'`.
    Past,
    /// `Future`: `isNonstandard: 'Future'`.
    Future,
    /// `Unobtainable`: `isNonstandard: 'Unobtainable'`.
    Unobtainable,
    /// Showdown's `nonexistent` (Past, Future, CAP, LGPE, Custom), written
    /// `-nonexistent` rather than `-tag:nonexistent`.
    Nonexistent,
}

impl Tag {
    /// The tag's display name (`Tags[id].name`).
    pub const fn name(self) -> &'static str {
        match self {
            Tag::Mythical => "Mythical",
            Tag::RestrictedLegendary => "Restricted Legendary",
            Tag::SubLegendary => "Sub-Legendary",
            Tag::UltraBeast => "Ultra Beast",
            Tag::Paradox => "Paradox",
            Tag::Uber => "Uber",
            Tag::Ag => "AG",
            Tag::Past => "Past",
            Tag::Future => "Future",
            Tag::Unobtainable => "Unobtainable",
            Tag::Nonexistent => "Nonexistent",
        }
    }

    /// Showdown's `EXISTENCE_TAGS`.
    pub const fn is_existence(self) -> bool {
        matches!(self, Tag::Past | Tag::Future | Tag::Unobtainable | Tag::Nonexistent)
    }

    /// The tag of an `isNonstandard` value.
    pub const fn of_nonstandard(n: Nonstandard) -> Tag {
        match n {
            Nonstandard::Past => Tag::Past,
            Nonstandard::Future => Tag::Future,
            Nonstandard::Unobtainable => Tag::Unobtainable,
        }
    }

    /// Showdown's `genericFilter` for tags that have one (existence tags).
    pub const fn matches_nonstandard(self, n: Option<Nonstandard>) -> bool {
        matches!(
            (self, n),
            (Tag::Past, Some(Nonstandard::Past))
                | (Tag::Future, Some(Nonstandard::Future))
                | (Tag::Unobtainable, Some(Nonstandard::Unobtainable))
                | (Tag::Nonexistent, Some(Nonstandard::Past | Nonstandard::Future))
        )
    }

    /// Showdown's `speciesFilter` (or `genericFilter`) for a species with
    /// legality row `row` and `isNonstandard` value `n`.
    pub const fn matches_species(self, row: &SpeciesLegality, n: Option<Nonstandard>) -> bool {
        let tags = row.tags;
        match self {
            // `species.tier === 'Uber' || species.tier === 'AG' ||
            // species.tier === '(AG)'` (no species has `(AG)`, which
            // `Tier` doesn't hold).
            Tag::Uber => matches!(row.tier, Tier::Uber | Tier::Ag),
            Tag::Ag => matches!(row.tier, Tier::Ag),
            Tag::Mythical => tags.contains(SpeciesTags::MYTHICAL),
            Tag::RestrictedLegendary => tags.contains(SpeciesTags::RESTRICTED_LEGENDARY),
            Tag::SubLegendary => tags.contains(SpeciesTags::SUB_LEGENDARY),
            Tag::UltraBeast => tags.contains(SpeciesTags::ULTRA_BEAST),
            Tag::Paradox => tags.contains(SpeciesTags::PARADOX),
            _ => self.matches_nonstandard(n),
        }
    }
}

/// What a rule names (the part after the sign: `pokemon:garchomp`,
/// `tag:mythical`, `nonexistent`, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// `pokemon:<species>`: that forme.
    Pokemon(SpeciesId),
    /// `basepokemon:<species>`: the species and all its formes.
    BasePokemon(SpeciesId),
    /// `item:<item>`.
    Item(ItemId),
    /// `item:noitem`: holding nothing.
    NoItem,
    /// `move:<move>`.
    Move(MoveId),
    /// `ability:<ability>`.
    Ability(AbilityId),
    /// `nature:<nature>`.
    Nature(Nature),
    /// `tag:<tag>` (or `nonexistent`).
    Tag(Tag),
    /// `unreleased` (Showdown's `Unreleased` ban; only read for unreleased
    /// Hidden Abilities).
    Unreleased,
}

/// One entry of a rule table: `sign` + `target`, added by `source` (the
/// rule or format that brought it in, `""` for the format itself).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rule {
    /// Ban, allow or restrict.
    pub sign: Sign,
    /// What the rule names.
    pub target: Target,
    /// The rule or format that added it (`""`: the format itself).
    pub source: &'static str,
}

impl Rule {
    /// A ban of `target` added by `source`.
    pub const fn ban(target: Target, source: &'static str) -> Rule {
        Rule { sign: Sign::Ban, target, source }
    }

    /// A restriction of `target` added by `source`.
    pub const fn restrict(target: Target, source: &'static str) -> Rule {
        Rule { sign: Sign::Restrict, target, source }
    }
}

/// A rule with an `onValidateTeam` hook, run in rule-table order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TeamRule {
    /// Obtainable's own check: one Kyurem-Black/White, one Necrozma-Dusk-Mane,
    /// one Necrozma-Dawn-Wings, one Calyrex-Ice/Shadow.
    Obtainable,
    /// `Species Clause`: one Pokémon per National Dex number.
    SpeciesClause,
    /// `Nickname Clause`: no two Pokémon with the same nickname.
    NicknameClause,
    /// `Item Clause = n`.
    ItemClause(u8),
    /// `Limit One Restricted` (1) / `Limit Two Restricted` (2).
    LimitRestricted(u8),
}

/// A rule with an `onValidateSet` hook, run for each set in rule-table
/// order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SetRule {
    /// `OHKO Clause`: no move with `ohko` (Fissure, Guillotine, Horn
    /// Drill, Sheer Cold).
    OhkoClause,
    /// `Sleep Moves Clause`: no move whose `status` is sleep (Spore,
    /// Hypnosis, ...). Yawn, which has none, is in the rule's ban list.
    SleepMovesClause,
}

impl SetRule {
    /// The rule's name, as its messages write it.
    pub const fn name(self) -> &'static str {
        match self {
            SetRule::OhkoClause => "OHKO Clause",
            SetRule::SleepMovesClause => "Sleep Moves Clause",
        }
    }
}

/// Why something is banned (Showdown's `RuleTable.getReason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    /// `"not obtainable"` (the `nonexistent` ban and the Obtainable rules).
    NotObtainable,
    /// `"banned by <source>"`, or `"banned"` for the format's own bans.
    BannedBy(&'static str),
}

impl Reason {
    /// The reason as Showdown words it in a problem message.
    pub fn text(self) -> String {
        match self {
            Reason::NotObtainable => "not obtainable".to_owned(),
            Reason::BannedBy("") => "banned".to_owned(),
            Reason::BannedBy(source) => format!("banned by {source}"),
        }
    }
}

/// The outcome of `RuleTable.check`: whitelisted (`''`), neutral (`null`)
/// or banned (a reason).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Check {
    /// Whitelisted by a `+` rule (`''`).
    Allowed,
    /// No rule names it (`null`).
    Neutral,
    /// Banned, and why.
    Banned(Reason),
}

/// A format's validation rules (Showdown's resolved `RuleTable` plus the
/// properties of its dex the validator reads).
#[derive(Clone, Debug)]
pub struct ValidationFormat {
    /// Showdown's format id (`gen9vgc2025regi`).
    pub id: &'static str,
    /// Showdown's format name.
    pub name: &'static str,
    /// The dex the format's teams are checked against.
    pub dex: &'static Dex,
    /// The dex's legality tables (learnsets, events, ...).
    pub legality: &'static Legality,
    /// The dex's generation.
    pub generation: u8,
    /// The Champions mod (`dex.currentMod.startsWith('champions')`): Stat
    /// Points in the EV fields, IVs fixed at 31, no Tera type check, and
    /// learnsets that don't inherit from prevolutions (`learnsetParent`).
    pub champions_mod: bool,
    /// `Terastal Clause`.
    pub terastal_clause: bool,
    /// Showdown's `ruleTable.minTeamSize`.
    pub min_team_size: u8,
    /// Showdown's `ruleTable.maxTeamSize`.
    pub max_team_size: u8,
    /// Showdown's `ruleTable.minLevel`.
    pub min_level: u16,
    /// Showdown's `ruleTable.maxLevel`.
    pub max_level: u16,
    /// Showdown's `ruleTable.defaultLevel`: the level of a set without one.
    pub default_level: u16,
    /// `Adjust Level = n`.
    pub adjust_level: Option<u16>,
    /// `Adjust Level Down = n`.
    pub adjust_level_down: Option<u16>,
    /// `EV Limit` (66 Stat Points under the Champions mod).
    pub ev_limit: Option<u16>,
    /// `Min Source Gen`.
    pub min_source_gen: u8,
    /// `Obtainable Moves`.
    pub obtainable_moves: bool,
    /// `Obtainable Abilities`.
    pub obtainable_abilities: bool,
    /// `Obtainable Formes`.
    pub obtainable_formes: bool,
    /// `Obtainable Misc`.
    pub obtainable_misc: bool,
    /// The table's bans, allows and restrictions, in its order.
    pub rules: &'static [Rule],
    /// The rules with an `onValidateTeam` hook, in the table's order.
    pub team_rules: &'static [TeamRule],
    /// The rules with an `onValidateSet` hook, in the table's order.
    pub set_rules: &'static [SetRule],
}

/// The rule table entries Obtainable brings in (`banlist: ['Unreleased',
/// 'Unobtainable', 'Nonexistent']`).
const OBTAINABLE_BANS: [Rule; 3] = [
    Rule::ban(Target::Unreleased, "Obtainable"),
    Rule::ban(Target::Tag(Tag::Unobtainable), "Obtainable"),
    Rule::ban(Target::Tag(Tag::Nonexistent), "Obtainable"),
];

impl ValidationFormat {
    /// `[Gen 9] VGC 2025 Reg I`: Flat Rules, `!! Adjust Level = 50`,
    /// `Min Source Gen = 9`, VGC Timer, Open Team Sheets, Limit Two
    /// Restricted (restricted: Restricted Legendary).
    pub fn gen9_vgc_2025_reg_i() -> ValidationFormat {
        const RULES: [Rule; 6] = [
            OBTAINABLE_BANS[0],
            OBTAINABLE_BANS[1],
            OBTAINABLE_BANS[2],
            Rule::ban(Target::Tag(Tag::Mythical), "Flat Rules"),
            Rule::ban(Target::Pokemon(pikapro_data::gen9::species::GRENINJA_BOND), "Flat Rules"),
            Rule::restrict(Target::Tag(Tag::RestrictedLegendary), ""),
        ];
        ValidationFormat {
            id: "gen9vgc2025regi",
            name: "[Gen 9] VGC 2025 Reg I",
            dex: &pikapro_data::gen9::DEX,
            legality: &pikapro_data::legality::gen9::LEGALITY,
            generation: 9,
            champions_mod: false,
            terastal_clause: false,
            min_team_size: 4,
            max_team_size: 6,
            min_level: 1,
            max_level: 100,
            default_level: 100,
            adjust_level: Some(50),
            adjust_level_down: None,
            ev_limit: Some(510),
            min_source_gen: 9,
            obtainable_moves: true,
            obtainable_abilities: true,
            obtainable_formes: true,
            obtainable_misc: true,
            rules: &RULES,
            team_rules: &[
                TeamRule::Obtainable,
                TeamRule::SpeciesClause,
                TeamRule::NicknameClause,
                TeamRule::ItemClause(1),
                TeamRule::LimitRestricted(2),
            ],
            set_rules: &[],
        }
    }

    /// `[Gen 9 Champions] VGC 2026 Reg M-C`: the Champions mod's Flat Rules
    /// (Adjust Level = 50, Min Team Size = 6, bans Mythical and Restricted
    /// Legendary), VGC Timer, Open Team Sheets. (Reg M-B has the same rules
    /// on Showdown master, but its own mod's legality, `championsregmb`,
    /// which pikapro-data doesn't generate.)
    pub fn champions_vgc_2026_reg_mc() -> ValidationFormat {
        const RULES: [Rule; 5] = [
            OBTAINABLE_BANS[0],
            OBTAINABLE_BANS[1],
            OBTAINABLE_BANS[2],
            Rule::ban(Target::Tag(Tag::Mythical), "Flat Rules"),
            Rule::ban(Target::Tag(Tag::RestrictedLegendary), "Flat Rules"),
        ];
        ValidationFormat {
            id: "gen9championsvgc2026regmc",
            name: "[Gen 9 Champions] VGC 2026 Reg M-C",
            dex: &pikapro_data::champions::DEX,
            legality: &pikapro_data::legality::champions::LEGALITY,
            champions_mod: true,
            min_team_size: 6,
            ev_limit: Some(66),
            rules: &RULES,
            team_rules: &[
                TeamRule::Obtainable,
                TeamRule::SpeciesClause,
                TeamRule::NicknameClause,
                TeamRule::ItemClause(1),
            ],
            ..ValidationFormat::gen9_vgc_2025_reg_i()
        }
    }

    /// `[Gen 9 Champions] BSS Reg M-C`: the same rule table as
    /// [`ValidationFormat::champions_vgc_2026_reg_mc`] (the Champions mod's
    /// Flat Rules, VGC Timer; no Open Team Sheets, which the validator
    /// doesn't read). Showdown's validator never reads the game type, and
    /// `Picked Team Size = Auto` (3 in singles) is below Flat Rules' `Min
    /// Team Size = 6`, so a team is valid here iff it is valid in VGC.
    pub fn champions_bss_reg_mc() -> ValidationFormat {
        ValidationFormat {
            id: "gen9championsbssregmc",
            name: "[Gen 9 Champions] BSS Reg M-C",
            ..ValidationFormat::champions_vgc_2026_reg_mc()
        }
    }

    /// `[Gen 9] Anything Goes` with Species Clause: Smogon's legality floor
    /// without tier bans. Obtainable (`EV Limit = Auto`, 510; Showdown's
    /// Gen 9 `Min Source Gen = 9`), one to six Pokémon up to level 100,
    /// Mythicals and restricted legendaries allowed, no two of a species.
    /// Standard AG's other rules (HP Percentage Mod, Endless Battle Clause)
    /// are battle mods, not validation.
    pub fn gen9_anything_goes_species_clause() -> ValidationFormat {
        ValidationFormat {
            id: "gen9anythinggoes@@@Species Clause",
            name: "[Gen 9] Anything Goes",
            min_team_size: 1,
            adjust_level: None,
            rules: &OBTAINABLE_BANS,
            team_rules: &[TeamRule::Obtainable, TeamRule::SpeciesClause],
            ..ValidationFormat::gen9_vgc_2025_reg_i()
        }
    }

    /// `[Gen 9] OU`: Standard (Standard AG's Obtainable; Species Clause,
    /// Nickname Clause, OHKO Clause, Evasion Items Clause, Evasion Moves
    /// Clause), Evasion Abilities Clause and Sleep Moves Clause, without
    /// Sleep Clause Mod; bans the Uber and AG tiers, Arena Trap, Moody,
    /// Shadow Tag, King's Rock, Razor Fang, Baton Pass, Last Respects,
    /// Shed Tail and Tera Blast. One to six Pokémon up to level 100. The
    /// table's other rules (Team Preview, HP Percentage Mod, Cancel Mod,
    /// Endless Battle Clause) are the battle's.
    ///
    /// `node tools/showdown-oracle/validator.mjs --rules gen9ou` prints
    /// the table this transcribes.
    pub fn gen9_ou() -> ValidationFormat {
        use pikapro_data::gen9::{abilities as a, items as i, moves as m};
        const ITEMS: &str = "Evasion Items Clause";
        const MOVES: &str = "Evasion Moves Clause";
        const ABILITIES: &str = "Evasion Abilities Clause";
        const RULES: [Rule; 22] = [
            OBTAINABLE_BANS[0],
            OBTAINABLE_BANS[1],
            OBTAINABLE_BANS[2],
            Rule::ban(Target::Item(i::BRIGHT_POWDER), ITEMS),
            Rule::ban(Target::Item(i::LAX_INCENSE), ITEMS),
            Rule::ban(Target::Move(m::ACUPRESSURE), MOVES),
            Rule::ban(Target::Move(m::MINIMIZE), MOVES),
            Rule::ban(Target::Move(m::DOUBLE_TEAM), MOVES),
            Rule::ban(Target::Ability(a::SAND_VEIL), ABILITIES),
            Rule::ban(Target::Ability(a::SNOW_CLOAK), ABILITIES),
            Rule::ban(Target::Move(m::YAWN), "Sleep Moves Clause"),
            Rule::ban(Target::Tag(Tag::Uber), ""),
            Rule::ban(Target::Tag(Tag::Ag), ""),
            Rule::ban(Target::Ability(a::ARENA_TRAP), ""),
            Rule::ban(Target::Ability(a::MOODY), ""),
            Rule::ban(Target::Ability(a::SHADOW_TAG), ""),
            Rule::ban(Target::Item(i::KINGS_ROCK), ""),
            Rule::ban(Target::Item(i::RAZOR_FANG), ""),
            Rule::ban(Target::Move(m::BATON_PASS), ""),
            Rule::ban(Target::Move(m::LAST_RESPECTS), ""),
            Rule::ban(Target::Move(m::SHED_TAIL), ""),
            Rule::ban(Target::Move(m::TERA_BLAST), ""),
        ];
        ValidationFormat {
            id: "gen9ou",
            name: "[Gen 9] OU",
            min_team_size: 1,
            adjust_level: None,
            rules: &RULES,
            team_rules: &[TeamRule::Obtainable, TeamRule::SpeciesClause, TeamRule::NicknameClause],
            set_rules: &[SetRule::OhkoClause, SetRule::SleepMovesClause],
            ..ValidationFormat::gen9_vgc_2025_reg_i()
        }
    }

    /// Every validation format.
    pub fn all() -> [ValidationFormat; 5] {
        [
            ValidationFormat::gen9_ou(),
            ValidationFormat::gen9_vgc_2025_reg_i(),
            ValidationFormat::champions_vgc_2026_reg_mc(),
            ValidationFormat::champions_bss_reg_mc(),
            ValidationFormat::gen9_anything_goes_species_clause(),
        ]
    }

    /// The validation format for a pikapro preset: `gen9vgc` validates as
    /// `gen9vgc2025regi`, `championsvgc` as `gen9championsvgc2026regmc`,
    /// `championsbss` as `gen9championsbssregmc`, `gen9ou` as `gen9ou`, and
    /// the custom games (`gen9singles`, `gen9doubles`) and the random battles
    /// as Anything Goes with Species Clause.
    pub fn for_preset(preset_id: &str) -> Option<ValidationFormat> {
        ValidationFormat::by_id(ValidationFormat::preset_id(preset_id)?)
    }

    /// The Showdown id of [`ValidationFormat::for_preset`]'s format: the one
    /// table of which preset validates as what.
    pub fn preset_id(preset_id: &str) -> Option<&'static str> {
        const AG: &str = "gen9anythinggoes@@@Species Clause";
        const PRESETS: [(&str, &str); 8] = [
            ("gen9ou", "gen9ou"),
            // Showdown doesn't validate a random battle's teams; these
            // validate a team someone brings to a random-battle preset
            // anyway, in a format they can pass.
            ("gen9randombattle", AG),
            ("gen9randomdoublesbattle", AG),
            ("gen9vgc", "gen9vgc2025regi"),
            ("championsvgc", "gen9championsvgc2026regmc"),
            ("championsbss", "gen9championsbssregmc"),
            ("gen9singles", AG),
            ("gen9doubles", AG),
        ];
        PRESETS.iter().find(|(p, _)| pikapro_data::id_eq(p, preset_id)).map(|&(_, id)| id)
    }

    /// A validation format by its Showdown id.
    pub fn by_id(id: &str) -> Option<ValidationFormat> {
        ValidationFormat::all().into_iter().find(|f| pikapro_data::id_eq(f.id, id))
    }

    fn find(&self, sign: Sign, target: Target) -> Option<&Rule> {
        self.rules.iter().find(|r| r.sign == sign && r.target == target)
    }

    /// `ruleTable.has(sign + target)`.
    pub fn has(&self, sign: Sign, target: Target) -> bool {
        self.find(sign, target).is_some()
    }

    /// `ruleTable.check(target)`.
    pub fn check(&self, target: Target) -> Check {
        if self.has(Sign::Allow, target) {
            return Check::Allowed;
        }
        match self.find(Sign::Ban, target) {
            None => Check::Neutral,
            Some(r) if r.target == Target::Tag(Tag::Nonexistent) => Check::Banned(Reason::NotObtainable),
            Some(r) => Check::Banned(Reason::BannedBy(r.source)),
        }
    }

    /// `ruleTable.isBanned(target)`.
    pub fn is_banned(&self, target: Target) -> bool {
        !self.has(Sign::Allow, target) && self.has(Sign::Ban, target)
    }

    /// `ruleTable.tagRules`: the tag rules, last added first.
    pub fn tag_rules(&self) -> impl Iterator<Item = (Sign, Tag)> + '_ {
        self.rules.iter().rev().filter_map(|r| match r.target {
            Target::Tag(t) => Some((r.sign, t)),
            _ => None,
        })
    }

    /// `ruleTable.isRestrictedSpecies`.
    pub fn is_restricted_species(&self, species: SpeciesId) -> bool {
        if self.has(Sign::Allow, Target::Pokemon(species)) {
            return false;
        }
        if self.has(Sign::Restrict, Target::Pokemon(species)) {
            return true;
        }
        let base = self.dex.species(species).base_species;
        if self.has(Sign::Allow, Target::BasePokemon(base)) {
            return false;
        }
        if self.has(Sign::Restrict, Target::BasePokemon(base)) {
            return true;
        }
        let row = self.legality.species(species);
        let nonstandard = self.dex.species(species).nonstandard;
        for (sign, tag) in self.tag_rules() {
            if sign == Sign::Ban || !tag.matches_species(row, nonstandard) {
                continue;
            }
            return sign == Sign::Restrict;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro_data::gen9::species;

    #[test]
    fn tag_rules_follow_showdowns_order() {
        // `node tools/showdown-oracle/validator.mjs --rules gen9vgc2025regi`
        let regi: Vec<_> = ValidationFormat::gen9_vgc_2025_reg_i().tag_rules().collect();
        assert_eq!(
            regi,
            [
                (Sign::Restrict, Tag::RestrictedLegendary),
                (Sign::Ban, Tag::Mythical),
                (Sign::Ban, Tag::Nonexistent),
                (Sign::Ban, Tag::Unobtainable),
            ]
        );
        // `--rules "gen9anythinggoes@@@Species Clause"`.
        let ag: Vec<_> = ValidationFormat::gen9_anything_goes_species_clause().tag_rules().collect();
        assert_eq!(ag, [(Sign::Ban, Tag::Nonexistent), (Sign::Ban, Tag::Unobtainable)]);
        // `--rules gen9ou`.
        let ou: Vec<_> = ValidationFormat::gen9_ou().tag_rules().collect();
        assert_eq!(
            ou,
            [
                (Sign::Ban, Tag::Ag),
                (Sign::Ban, Tag::Uber),
                (Sign::Ban, Tag::Nonexistent),
                (Sign::Ban, Tag::Unobtainable)
            ]
        );
        let champions: Vec<_> = ValidationFormat::champions_vgc_2026_reg_mc().tag_rules().collect();
        assert_eq!(
            champions,
            [
                (Sign::Ban, Tag::RestrictedLegendary),
                (Sign::Ban, Tag::Mythical),
                (Sign::Ban, Tag::Nonexistent),
                (Sign::Ban, Tag::Unobtainable),
            ]
        );
    }

    #[test]
    fn checks_give_showdowns_reasons() {
        let f = ValidationFormat::gen9_vgc_2025_reg_i();
        assert_eq!(f.check(Target::Tag(Tag::Nonexistent)), Check::Banned(Reason::NotObtainable));
        assert_eq!(f.check(Target::Tag(Tag::Mythical)), Check::Banned(Reason::BannedBy("Flat Rules")));
        assert_eq!(f.check(Target::Tag(Tag::Past)), Check::Neutral);
        assert_eq!(Reason::BannedBy("Flat Rules").text(), "banned by Flat Rules");
        assert!(f.is_restricted_species(species::MEWTWO));
        assert!(!f.is_restricted_species(species::GARCHOMP));
        assert!(!ValidationFormat::champions_vgc_2026_reg_mc().is_restricted_species(species::MEWTWO));
        // The format's own bans have no source; a clause's name it.
        let ou = ValidationFormat::gen9_ou();
        assert_eq!(ou.check(Target::Tag(Tag::Uber)), Check::Banned(Reason::BannedBy("")));
        let yawn = Target::Move(pikapro_data::gen9::moves::YAWN);
        assert_eq!(ou.check(yawn), Check::Banned(Reason::BannedBy("Sleep Moves Clause")));
        assert_eq!(f.check(yawn), Check::Neutral);
        // Tier tags read the species' singles tier: `Uber` matches AG too.
        let legality = ou.legality;
        let nonstandard = |s| ou.dex.species(s).nonstandard;
        for (s, uber, ag) in
            [(species::KORAIDON, true, false), (species::RAYQUAZA, true, false), (species::GARCHOMP, false, false)]
        {
            let row = legality.species(s);
            assert_eq!(Tag::Uber.matches_species(row, nonstandard(s)), uber, "{s:?}");
            assert_eq!(Tag::Ag.matches_species(row, nonstandard(s)), ag, "{s:?}");
        }
        // Every AG species is tagged Uber too.
        let ags: Vec<_> = (0..ou.dex.species.len() as u16)
            .map(pikapro_data::SpeciesId)
            .filter(|&s| Tag::Ag.matches_species(legality.species(s), nonstandard(s)))
            .collect();
        assert!(!ags.is_empty(), "no AG species");
        assert!(ags.iter().all(|&s| Tag::Uber.matches_species(legality.species(s), nonstandard(s))), "{ags:?}");
    }

    #[test]
    fn presets_map_to_validation_formats() {
        assert_eq!(ValidationFormat::for_preset("gen9vgc").map(|f| f.id), Some("gen9vgc2025regi"));
        assert_eq!(ValidationFormat::for_preset("championsvgc").map(|f| f.id), Some("gen9championsvgc2026regmc"));
        assert_eq!(ValidationFormat::for_preset("championsbss").map(|f| f.id), Some("gen9championsbssregmc"));
        let ag = Some("gen9anythinggoes@@@Species Clause");
        assert_eq!(ValidationFormat::for_preset("gen9singles").map(|f| f.id), ag);
        assert_eq!(ValidationFormat::for_preset("gen9doubles").map(|f| f.id), ag);
    }
}
