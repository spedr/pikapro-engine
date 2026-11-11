//! Differential fuzzing of pikapro-validate against Showdown's team
//! validator.
//!
//! 1. [`random_case`] builds a random legal-looking team for a validation
//!    format and applies a few random [`MUTATIONS`] that make it illegal
//!    (illegal or event-only moves, levels, banned or nonexistent species,
//!    items and abilities, EVs, IVs and Stat Points, battle-only formes,
//!    missing required items, Tera types, team size, nicknames, genders,
//!    shininess, ...).
//! 2. [`run_showdown_validator`] validates a batch of teams with Showdown's
//!    `TeamValidator` (`tools/showdown-oracle/validator.mjs`).
//! 3. The fuzzer compares Showdown's messages with pikapro-validate's, line
//!    for line.

use std::io::Write as _;
use std::process::{Command, Stdio};

use pikapro::{BattleRng, Investment, PokemonSet, Seed, Team};
use pikapro_data::gen9::{moves, species};
use pikapro_data::legality::{BattleOnly, LearnSource, SourceKind, SpeciesTags};
use pikapro_data::{AbilityId, Dex, ItemId, MoveId, Nature, SpeciesId, Stat, Stats, Type};
use pikapro_protocol::json_string;
use pikapro_validate::{Gender, SetInfo, ValidationFormat, Validator};

use crate::{OracleError, oracle_dir};

/// One generated team and what was done to it.
#[derive(Clone, Debug)]
pub struct ValidationCase {
    pub seed: u64,
    pub team: Team,
    pub info: Vec<SetInfo>,
    pub mutations: Vec<&'static str>,
}

/// What the generator draws from, per format.
#[derive(Debug)]
pub struct Pools {
    pub validator: Validator,
    /// Standard, not battle-only, not banned by a tag (Mythical; Restricted
    /// Legendary where banned).
    legal_species: Vec<SpeciesId>,
    restricted: Vec<SpeciesId>,
    mythical: Vec<SpeciesId>,
    nonstandard_species: Vec<SpeciesId>,
    battle_only: Vec<SpeciesId>,
    event_only: Vec<SpeciesId>,
    required_item: Vec<SpeciesId>,
    standard_items: Vec<ItemId>,
    nonstandard_items: Vec<ItemId>,
    mega_stones: Vec<ItemId>,
    standard_moves: Vec<MoveId>,
    nonstandard_moves: Vec<MoveId>,
    nonstandard_abilities: Vec<AbilityId>,
    /// Standard species a tier tag bans (Uber and AG in gen9ou): left out
    /// of `legal_species`.
    tier_banned: Vec<SpeciesId>,
    /// Standard moves the format's rules ban, by name or by a set rule
    /// (OHKO Clause, Sleep Moves Clause), and the abilities and items its
    /// rules ban.
    banned_moves: Vec<MoveId>,
    banned_abilities: Vec<AbilityId>,
    banned_items: Vec<ItemId>,
}

impl Pools {
    pub fn new(format: ValidationFormat) -> Pools {
        let dex = format.dex;
        let leg = format.legality;
        let restricted_banned = format.rules.iter().any(|r| {
            r.sign == pikapro_validate::rules::Sign::Ban
                && r.target == pikapro_validate::rules::Target::Tag(pikapro_validate::rules::Tag::RestrictedLegendary)
        });
        let all_species = (0..dex.species.len() as u16).map(SpeciesId);
        let mut pools = Pools {
            legal_species: Vec::new(),
            restricted: Vec::new(),
            mythical: Vec::new(),
            nonstandard_species: Vec::new(),
            battle_only: Vec::new(),
            event_only: Vec::new(),
            required_item: Vec::new(),
            standard_items: Vec::new(),
            nonstandard_items: Vec::new(),
            mega_stones: Vec::new(),
            standard_moves: Vec::new(),
            nonstandard_moves: Vec::new(),
            nonstandard_abilities: Vec::new(),
            tier_banned: Vec::new(),
            banned_moves: Vec::new(),
            banned_abilities: Vec::new(),
            banned_items: Vec::new(),
            validator: Validator::new(format.clone()),
        };
        use pikapro_validate::rules::{SetRule, Sign, Tag, Target};
        let tier_tags: Vec<Tag> = format
            .rules
            .iter()
            .filter_map(|r| match r.target {
                Target::Tag(t @ (Tag::Uber | Tag::Ag)) if r.sign == Sign::Ban => Some(t),
                _ => None,
            })
            .collect();
        for r in format.rules.iter().filter(|r| r.sign == Sign::Ban) {
            match r.target {
                Target::Move(m) => pools.banned_moves.push(m),
                Target::Ability(a) => pools.banned_abilities.push(a),
                Target::Item(i) => pools.banned_items.push(i),
                _ => {}
            }
        }
        for m in (0..dex.moves.len() as u16).map(MoveId) {
            let data = dex.move_(m);
            let by_rule = |rule: &SetRule| match rule {
                SetRule::OhkoClause => data.ohko,
                SetRule::SleepMovesClause => data.status == Some(pikapro_data::StatusId::Sleep),
            };
            if data.nonstandard.is_none() && format.set_rules.iter().any(by_rule) {
                pools.banned_moves.push(m);
            }
        }
        for s in all_species {
            let data = dex.species(s);
            let l = leg.species(s);
            if data.nonstandard.is_some() {
                pools.nonstandard_species.push(s);
                continue;
            }
            if l.battle_only != BattleOnly::No {
                pools.battle_only.push(s);
                continue;
            }
            if !data.required_items.is_empty() {
                pools.required_item.push(s);
            }
            if l.event_only {
                pools.event_only.push(s);
            }
            if tier_tags.iter().any(|t| t.matches_species(l, data.nonstandard)) {
                pools.tier_banned.push(s);
                continue;
            }
            if l.tags.contains(SpeciesTags::MYTHICAL) {
                pools.mythical.push(s);
            } else if l.tags.contains(SpeciesTags::RESTRICTED_LEGENDARY) {
                pools.restricted.push(s);
                if !restricted_banned {
                    pools.legal_species.push(s);
                }
            } else {
                pools.legal_species.push(s);
            }
        }
        for i in (0..dex.items.len() as u16).map(ItemId) {
            let it = dex.item(i);
            if it.nonstandard.is_some() {
                pools.nonstandard_items.push(i);
            } else {
                pools.standard_items.push(i);
                if !it.mega.is_empty() {
                    pools.mega_stones.push(i);
                }
            }
        }
        for m in (0..dex.moves.len() as u16).map(MoveId) {
            if dex.move_(m).nonstandard.is_some() {
                pools.nonstandard_moves.push(m);
            } else {
                pools.standard_moves.push(m);
            }
        }
        for a in (1..dex.abilities.len() as u16).map(AbilityId) {
            if dex.ability(a).nonstandard.is_some() {
                pools.nonstandard_abilities.push(a);
            }
        }
        pools
    }

    fn dex(&self) -> &'static Dex {
        self.validator.format().dex
    }

    fn stat_points(&self) -> bool {
        self.validator.format().champions_mod
    }

    /// Moves `s` can learn: `(move, sources)` over its full learnset, first
    /// learnset first.
    fn learnable(&self, s: SpeciesId) -> Vec<(MoveId, Vec<LearnSource>)> {
        let mut out: Vec<(MoveId, Vec<LearnSource>)> = Vec::new();
        for (_, ls) in self.validator.full_learnset(s) {
            for e in ls.iter() {
                let sources = e.sources.iter().map(|&x| LearnSource(x));
                match out.iter_mut().find(|(m, _)| *m == e.move_) {
                    Some((_, v)) => v.extend(sources),
                    None => out.push((e.move_, sources.collect())),
                }
            }
        }
        out
    }

    /// Moves of `s` whose sources (over its full learnset) satisfy `keep`.
    fn moves_where(&self, s: SpeciesId, keep: impl Fn(&[LearnSource]) -> bool) -> Vec<MoveId> {
        self.learnable(s).into_iter().filter(|(_, src)| keep(src)).map(|(m, _)| m).collect()
    }

    /// Moves only a prevolution of `s` (a later learnset) knows.
    fn prevo_only_moves(&self, s: SpeciesId) -> Vec<MoveId> {
        let full = self.validator.full_learnset(s);
        let Some(((_, own), rest)) = full.split_first() else { return Vec::new() };
        let mut out = Vec::new();
        for (_, ls) in rest {
            for e in ls.iter() {
                if own.get(e.move_).is_none() && !out.contains(&e.move_) {
                    out.push(e.move_);
                }
            }
        }
        out
    }

    /// A move of `s` with a Gen 9 non-event source.
    fn legal_moves(&self, s: SpeciesId) -> Vec<MoveId> {
        self.learnable(s)
            .into_iter()
            .filter(|(_, src)| src.iter().any(|x| x.generation() == 9 && x.kind() != SourceKind::Event))
            .map(|(m, _)| m)
            .collect()
    }
}

fn pick<T: Copy>(rng: &mut BattleRng, v: &[T]) -> Option<T> {
    if v.is_empty() { None } else { Some(v[rng.random(v.len() as u32) as usize]) }
}

fn spread(rng: &mut BattleRng, max: u32, total: u32) -> Stats<u8> {
    let mut s = Stats::splat(0u8);
    let mut left = total;
    for i in 0..6 {
        let v = rng.random(max.min(left) + 1);
        s.0[i] = v as u8;
        left -= v;
    }
    if s.0.iter().all(|&v| v == 0) {
        s.0[rng.random(6) as usize] = 1;
    }
    s
}

fn investment(pools: &Pools, rng: &mut BattleRng) -> Investment {
    if pools.stat_points() {
        Investment::StatPoints(spread(rng, 32, 66))
    } else {
        Investment::Evs { evs: spread(rng, 252, 510), ivs: Stats::splat(31) }
    }
}

/// A set for `s` that should mostly be legal.
fn legal_set(pools: &Pools, rng: &mut BattleRng, s: SpeciesId) -> PokemonSet {
    let dex = pools.dex();
    let data = dex.species(s);
    let slots = data.abilities;
    let abilities: Vec<AbilityId> =
        [Some(slots.primary), slots.secondary, slots.hidden].into_iter().flatten().collect();
    let mut legal = pools.legal_moves(s);
    rng.shuffle(&mut legal);
    let n = (1 + rng.random(4) as usize).min(legal.len());
    let mut set = PokemonSet::new(s)
        .with_level(pools.level())
        .with_ability(pick(rng, &abilities).unwrap_or(slots.primary))
        .with_nature(Nature::ALL[rng.random(25) as usize])
        .with_investment(investment(pools, rng))
        .with_moves(&legal[..n]);
    if !pools.stat_points() {
        let tera = data.required_tera_type.map(|t| t.ty()).unwrap_or(Type::TERA[rng.random(19) as usize]);
        set.tera_type = Some(tera);
    }
    if let Some(&item) = data.required_items.first() {
        set.item = Some(item);
    } else if rng.random(10) < 7 {
        set.item = pick(rng, &pools.standard_items);
    }
    set
}

/// A random team that respects Species and Item Clause and the restricted
/// limit.
fn legal_team(pools: &Pools, rng: &mut BattleRng) -> Team {
    let mut sets: Vec<PokemonSet> = Vec::new();
    let mut restricted = 0;
    let size = pools.validator.format().max_team_size as usize;
    let mut tries = 0;
    while sets.len() < size && tries < 200 {
        tries += 1;
        let s = pick(rng, &pools.legal_species).expect("species");
        let num = pools.dex().species(s).num;
        if sets.iter().any(|x| pools.dex().species(x.species).num == num) {
            continue;
        }
        if pools.restricted.contains(&s) {
            if restricted >= 2 {
                continue;
            }
            restricted += 1;
        }
        let mut set = legal_set(pools, rng, s);
        if set.item.is_some_and(|i| sets.iter().any(|x| x.item == Some(i))) {
            set.item = None;
        }
        sets.push(set);
    }
    Team::new(sets)
}

/// The mutations the fuzzer applies, by name.
pub const MUTATIONS: &[&str] = &[
    "illegal-move",
    "event-move",
    "level",
    "low-level",
    "species-nonstandard",
    "species-banned",
    "restricted-many",
    "duplicate-species",
    "duplicate-item",
    "item-nonstandard",
    "item-random",
    "ability-random",
    "ability-nonstandard",
    "no-ability",
    "move-nonstandard",
    "evs",
    "ivs",
    "stat-points",
    "zero-investment",
    "battle-only",
    "required-item",
    "tera",
    "team-size",
    "nickname",
    "gender",
    "shiny",
    "duplicate-move",
    "no-moves",
    "mega-stone",
    "hidden-power",
    "event-species",
    "go-species",
    "investment-kind",
    // Targeted: the deeper learnset paths.
    "prevo-moves",
    "egg-moves",
    "high-level-moves",
    "event-moves",
    "old-moves",
    "forced-forme",
    "special-forme",
    "evo-move-carry",
    "legendary-ivs",
    "random-species",
];

/// The mutations a format with tier, move, ability or item bans gets
/// besides [`MUTATIONS`] (gen9ou). The other formats draw from
/// [`MUTATIONS`] alone, so their seeds give the cases they always gave.
pub const BAN_MUTATIONS: &[&str] = &["tier-banned", "banned-move", "banned-ability", "banned-item"];

impl Pools {
    /// Whether the format bans a tier, a move, an ability or an item
    /// (gen9ou).
    fn has_bans(&self) -> bool {
        !self.tier_banned.is_empty()
            || !self.banned_moves.is_empty()
            || !self.banned_abilities.is_empty()
            || !self.banned_items.is_empty()
    }

    /// The mutations this format's cases draw from.
    pub fn mutations(&self) -> Vec<&'static str> {
        let mut list = MUTATIONS.to_vec();
        if self.has_bans() {
            list.extend_from_slice(BAN_MUTATIONS);
        }
        list
    }

    /// The level of a set that should be legal: 50, or 100 in a tier
    /// format (where Showdown asks whether a level 50 set is a mistake).
    fn level(&self) -> u8 {
        if self.has_bans() { 100 } else { 50 }
    }
}

fn mutate(pools: &Pools, rng: &mut BattleRng, team: &mut Team, info: &mut Vec<SetInfo>, name: &str) {
    let dex = pools.dex();
    if team.sets.is_empty() {
        return;
    }
    let i = rng.random(team.sets.len() as u32) as usize;
    match name {
        "illegal-move" => {
            if let Some(m) = pick(rng, &pools.standard_moves) {
                replace_move(rng, &mut team.sets[i], m);
            }
        }
        "event-move" => {
            let s = team.sets[i].species;
            let events: Vec<MoveId> = pools
                .learnable(s)
                .into_iter()
                .filter(|(_, src)| src.iter().any(|x| x.kind() == SourceKind::Event))
                .map(|(m, _)| m)
                .collect();
            if let Some(m) = pick(rng, &events) {
                replace_move(rng, &mut team.sets[i], m);
            }
        }
        "level" => {
            team.sets[i].level = match rng.random(4) {
                0 => 1 + rng.random(100) as u8,
                1 => 101 + rng.random(155) as u8,
                2 => 100,
                _ => 0,
            };
        }
        "low-level" => team.sets[i].level = 1 + rng.random(40) as u8,
        "species-nonstandard" => {
            if let Some(s) = pick(rng, &pools.nonstandard_species) {
                let set = legal_set(pools, rng, s);
                team.sets[i] = set;
            }
        }
        "species-banned" => {
            let pool = if rng.random(2) == 0 { &pools.mythical } else { &pools.restricted };
            if let Some(s) = pick(rng, pool) {
                team.sets[i] = legal_set(pools, rng, s);
            }
        }
        "tier-banned" => {
            if let Some(s) = pick(rng, &pools.tier_banned) {
                team.sets[i] = legal_set(pools, rng, s);
            }
        }
        "banned-move" => {
            // One the set's species learns, half the time (so that the ban
            // is the only problem), else any.
            let learnable: Vec<MoveId> = {
                let legal = pools.legal_moves(team.sets[i].species);
                pools.banned_moves.iter().copied().filter(|m| legal.contains(m)).collect()
            };
            let pool = if !learnable.is_empty() && rng.random(2) == 0 { &learnable } else { &pools.banned_moves };
            if let Some(m) = pick(rng, pool) {
                replace_move(rng, &mut team.sets[i], m);
            }
        }
        "banned-ability" => {
            // A species that has the ability, half the time, else the
            // ability on whatever is there.
            if let Some(a) = pick(rng, &pools.banned_abilities) {
                let has = |s: &SpeciesId| {
                    let slots = dex.species(*s).abilities;
                    [Some(slots.primary), slots.secondary, slots.hidden].contains(&Some(a))
                };
                let with: Vec<SpeciesId> = pools.legal_species.iter().copied().filter(has).collect();
                if rng.random(2) == 0
                    && let Some(s) = pick(rng, &with)
                {
                    team.sets[i] = legal_set(pools, rng, s);
                }
                team.sets[i].ability = a;
            }
        }
        "banned-item" => {
            if let Some(item) = pick(rng, &pools.banned_items) {
                team.sets[i].item = Some(item);
            }
        }
        "restricted-many" => {
            for j in 0..3.min(team.sets.len()) {
                if let Some(s) = pick(rng, &pools.restricted) {
                    team.sets[j] = legal_set(pools, rng, s);
                }
            }
        }
        "duplicate-species" => {
            let j = rng.random(team.sets.len() as u32) as usize;
            team.sets[j].species = team.sets[i].species;
            team.sets[j].ability = team.sets[i].ability;
        }
        "duplicate-item" => {
            let j = rng.random(team.sets.len() as u32) as usize;
            team.sets[j].item = team.sets[i].item.or(pick(rng, &pools.standard_items));
            team.sets[i].item = team.sets[j].item;
        }
        "item-nonstandard" => team.sets[i].item = pick(rng, &pools.nonstandard_items),
        "item-random" => team.sets[i].item = Some(ItemId(rng.random(dex.items.len() as u32) as u16)),
        "ability-random" => team.sets[i].ability = AbilityId(rng.random(dex.abilities.len() as u32) as u16),
        "ability-nonstandard" => {
            if let Some(a) = pick(rng, &pools.nonstandard_abilities) {
                team.sets[i].ability = a;
            }
        }
        "no-ability" => team.sets[i].ability = AbilityId::NONE,
        "move-nonstandard" => {
            if let Some(m) = pick(rng, &pools.nonstandard_moves) {
                replace_move(rng, &mut team.sets[i], m);
            }
        }
        "evs" => {
            let mut s = Stats::splat(0u8);
            let total_target = match rng.random(4) {
                0 => 508,
                1 => 510,
                _ => 0,
            };
            for v in &mut s.0 {
                *v = rng.random(256) as u8;
            }
            if total_target != 0 {
                s = Stats([255, 253 - (510 - total_target) as u8, 2, 0, 0, 0]);
            }
            team.sets[i].investment = match team.sets[i].investment {
                Investment::Evs { ivs, .. } => Investment::Evs { evs: s, ivs },
                Investment::StatPoints(_) => Investment::StatPoints(s),
            };
        }
        "ivs" => {
            let mut ivs = Stats::splat(31u8);
            for v in &mut ivs.0 {
                if rng.random(2) == 0 {
                    *v = rng.random(32) as u8;
                }
            }
            let evs = match team.sets[i].investment {
                Investment::Evs { evs, .. } | Investment::StatPoints(evs) => evs,
            };
            team.sets[i].investment = Investment::Evs { evs, ivs };
        }
        "stat-points" => {
            let mut s = Stats::splat(0u8);
            for v in &mut s.0 {
                *v = rng.random(40) as u8;
            }
            team.sets[i].investment = match team.sets[i].investment {
                Investment::Evs { ivs, .. } => Investment::Evs { evs: s, ivs },
                Investment::StatPoints(_) => Investment::StatPoints(s),
            };
        }
        "zero-investment" => {
            team.sets[i].investment = match team.sets[i].investment {
                Investment::Evs { ivs, .. } => Investment::Evs { evs: Stats::splat(0), ivs },
                Investment::StatPoints(_) => Investment::StatPoints(Stats::splat(0)),
            };
            if rng.random(2) == 0 {
                team.sets[i].nature = Nature::Serious;
            }
        }
        "battle-only" => {
            if let Some(s) = pick(rng, &pools.battle_only) {
                let mut set = legal_set(pools, rng, s);
                if rng.random(2) == 0 {
                    set.item = pick(rng, &pools.standard_items);
                }
                team.sets[i] = set;
            }
        }
        "required-item" => {
            if let Some(s) = pick(rng, &pools.required_item) {
                let mut set = legal_set(pools, rng, s);
                set.item = if rng.random(2) == 0 { None } else { pick(rng, &pools.standard_items) };
                team.sets[i] = set;
            }
        }
        "tera" => {
            let pool = [species::OGERPON, species::OGERPON_WELLSPRING, species::TERAPAGOS, species::TERAPAGOS_STELLAR];
            if rng.random(2) == 0 {
                let s = pool[rng.random(4) as usize];
                team.sets[i] = legal_set(pools, rng, s);
            }
            team.sets[i].tera_type = Some(Type::TERA[rng.random(19) as usize]);
        }
        "team-size" => {
            if rng.random(2) == 0 {
                team.sets.truncate(1 + rng.random(3) as usize);
            } else {
                for _ in 0..1 + rng.random(2) {
                    let s = pick(rng, &pools.legal_species).expect("species");
                    team.sets.push(legal_set(pools, rng, s));
                }
            }
        }
        "nickname" => {
            let nick = match rng.random(5) {
                0 => "Chompy".to_owned(),
                1 => "A Very Long Nickname Indeed".to_owned(),
                2 => dex.species(pick(rng, &pools.legal_species).expect("species")).name.to_owned(),
                3 => dex.species(dex.species(team.sets[i].species).base_species).name.to_owned(),
                _ => format!("Mon{}", rng.random(3)),
            };
            info_at(info, i).nickname = Some(nick.clone());
            if rng.random(3) == 0 {
                let j = rng.random(team.sets.len() as u32) as usize;
                info_at(info, j).nickname = Some(nick);
            }
        }
        "gender" => info_at(info, i).gender = Some(if rng.random(2) == 0 { Gender::Male } else { Gender::Female }),
        "shiny" => info_at(info, i).shiny = true,
        "duplicate-move" => {
            let set = &mut team.sets[i];
            if let Some(&m) = set.moves.first() {
                if set.moves.len() < 4 {
                    set.moves.push(m);
                } else {
                    set.moves[3] = m;
                }
            }
        }
        "no-moves" => team.sets[i].moves.clear(),
        "mega-stone" => {
            let stone = pick(rng, &pools.mega_stones);
            if let Some(stone) = stone {
                if rng.random(2) == 0 {
                    // The stone's own species.
                    if let Some(&(base, _)) = dex.item(stone).mega.first() {
                        let mut set = legal_set(pools, rng, base);
                        set.item = Some(stone);
                        team.sets[i] = set;
                        return;
                    }
                }
                team.sets[i].item = Some(stone);
            }
        }
        "hidden-power" => {
            let hp: Vec<MoveId> = (0..dex.moves.len() as u16)
                .map(MoveId)
                .filter(|&m| dex.move_(m).name.starts_with("Hidden Power"))
                .collect();
            if let Some(m) = pick(rng, &hp) {
                replace_move(rng, &mut team.sets[i], m);
            }
        }
        "event-species" => {
            if let Some(s) = pick(rng, &pools.event_only) {
                team.sets[i] = legal_set(pools, rng, s);
            }
        }
        "go-species" => {
            let s = [species::MELTAN, species::MELMETAL, species::GIMMIGHOUL_ROAMING][rng.random(3) as usize];
            team.sets[i] = legal_set(pools, rng, s);
        }
        "investment-kind" => {
            let set = &mut team.sets[i];
            set.investment = match set.investment {
                Investment::Evs { evs, .. } => Investment::StatPoints(evs),
                Investment::StatPoints(sp) => Investment::Evs { evs: sp, ivs: Stats::splat(31) },
            };
        }
        "prevo-moves" => {
            let s = pick(rng, &pools.legal_species).expect("species");
            let mut ms = pools.prevo_only_moves(s);
            if ms.is_empty() {
                return;
            }
            rng.shuffle(&mut ms);
            let mut set = legal_set(pools, rng, s);
            let n = 1 + rng.random(ms.len().min(4) as u32) as usize;
            set.moves = ms[..n].iter().copied().collect();
            if rng.random(2) == 0 {
                set.level = 1 + rng.random(50) as u8;
            }
            team.sets[i] = set;
        }
        "egg-moves" => {
            let s = pick(rng, &pools.legal_species).expect("species");
            let mut ms = pools.moves_where(s, |src| src.iter().any(|x| x.kind() == SourceKind::Egg));
            if ms.is_empty() {
                return;
            }
            rng.shuffle(&mut ms);
            let mut set = legal_set(pools, rng, s);
            let n = 1 + rng.random(ms.len().min(4) as u32) as usize;
            set.moves = ms[..n].iter().copied().collect();
            if rng.random(2) == 0 {
                set.level = 1 + rng.random(30) as u8;
            }
            team.sets[i] = set;
        }
        "high-level-moves" => {
            let s = pick(rng, &pools.legal_species).expect("species");
            let mut ms = pools.moves_where(s, |src| {
                src.iter().any(|x| x.kind() == pikapro_data::legality::SourceKind::LevelUp && x.value() > 20)
            });
            if ms.is_empty() {
                return;
            }
            rng.shuffle(&mut ms);
            let mut set = legal_set(pools, rng, s);
            let n = 1 + rng.random(ms.len().min(4) as u32) as usize;
            set.moves = ms[..n].iter().copied().collect();
            set.level = 1 + rng.random(60) as u8;
            team.sets[i] = set;
        }
        "event-moves" => {
            let species_pool: Vec<SpeciesId> =
                if rng.random(2) == 0 { pools.event_only.clone() } else { pools.legal_species.clone() };
            let s = pick(rng, &species_pool).expect("species");
            let mut ms = pools.moves_where(s, |src| src.iter().any(|x| x.kind() == SourceKind::Event));
            if ms.is_empty() {
                return;
            }
            rng.shuffle(&mut ms);
            let mut set = legal_set(pools, rng, s);
            let n = 1 + rng.random(ms.len().min(3) as u32) as usize;
            for &m in &ms[..n] {
                replace_move(rng, &mut set, m);
            }
            if rng.random(3) == 0 {
                set.level = 1 + rng.random(100) as u8;
            }
            team.sets[i] = set;
            let inf = info_at(info, i);
            inf.shiny = rng.random(3) == 0;
            inf.gender = match rng.random(3) {
                0 => Some(Gender::Male),
                1 => Some(Gender::Female),
                _ => None,
            };
        }
        "old-moves" => {
            let s = pick(rng, &pools.legal_species).expect("species");
            let ms = pools.moves_where(s, |src| src.iter().all(|x| x.generation() < 9));
            if let Some(m) = pick(rng, &ms) {
                replace_move(rng, &mut team.sets[i], m);
                team.sets[i].species = s;
            }
        }
        "forced-forme" => {
            let leg = pools.validator.format().legality;
            let forced: Vec<ItemId> =
                (0..dex.items.len() as u16).map(ItemId).filter(|&it| leg.item(it).forced_forme.is_some()).collect();
            if let Some(it) = pick(rng, &forced) {
                let forme = leg.item(it).forced_forme.expect("forced");
                let base = dex.species(forme).base_species;
                let s = if rng.random(3) == 0 { forme } else { base };
                let mut set = legal_set(pools, rng, s);
                set.item = Some(it);
                team.sets[i] = set;
            }
        }
        "special-forme" => {
            use pikapro_data::gen9::{abilities, items};
            let cases: [FormeCase; 16] = [
                (species::GRENINJA, None, Some(abilities::BATTLE_BOND), &[]),
                (species::ROCKRUFF, None, Some(abilities::OWN_TEMPO), &[]),
                (species::ZACIAN, Some(items::RUSTED_SWORD), None, &[]),
                (species::ZACIAN_CROWNED, Some(items::RUSTED_SWORD), None, &[moves::BEHEMOTH_BLADE]),
                (species::ZAMAZENTA_CROWNED, None, None, &[moves::BEHEMOTH_BASH]),
                (species::GROUDON, Some(items::RED_ORB), None, &[]),
                (species::KYOGRE, Some(items::BLUE_ORB), None, &[]),
                (species::NECROZMA_ULTRA, Some(items::ULTRANECROZIUM_Z), None, &[moves::SUNSTEEL_STRIKE]),
                (species::NECROZMA_ULTRA, None, None, &[]),
                (species::ZYGARDE_COMPLETE, Some(items::ZYGARDITE), None, &[]),
                (species::ZYGARDE, Some(items::ZYGARDITE), Some(abilities::AURA_BREAK), &[]),
                (species::HOOPA, None, None, &[moves::HYPERSPACE_FURY]),
                (species::HOOPA_UNBOUND, None, None, &[moves::HYPERSPACE_HOLE]),
                (species::KELDEO, None, None, &[moves::SECRET_SWORD]),
                (species::KELDEO_RESOLUTE, None, None, &[moves::SURF]),
                (species::TERAPAGOS_STELLAR, None, None, &[moves::TERA_STARSTORM]),
            ];
            let (s, item, ability, ms) = cases[rng.random(cases.len() as u32) as usize];
            let mut set = legal_set(pools, rng, s);
            if item.is_some() || rng.random(2) == 0 {
                set.item = item;
            }
            if let Some(a) = ability {
                set.ability = a;
            }
            for &m in ms {
                replace_move(rng, &mut set, m);
            }
            team.sets[i] = set;
        }
        "evo-move-carry" => {
            let leg = pools.validator.format().legality;
            let evolved: Vec<SpeciesId> = pools
                .legal_species
                .iter()
                .copied()
                .filter(|&s| leg.species(s).evo_move.is_some() || s == species::SYLVEON)
                .collect();
            if let Some(s) = pick(rng, &evolved) {
                let mut ms = pools.prevo_only_moves(s);
                ms.extend(pools.legal_moves(s));
                rng.shuffle(&mut ms);
                let mut set = legal_set(pools, rng, s);
                set.moves = ms.iter().copied().take(4).collect();
                team.sets[i] = set;
            }
        }
        "legendary-ivs" => {
            let pool: Vec<SpeciesId> = pools.restricted.iter().chain(&pools.mythical).copied().collect();
            if let Some(s) = pick(rng, &pool) {
                let mut set = legal_set(pools, rng, s);
                let mut ivs = Stats::splat(31u8);
                for v in &mut ivs.0 {
                    if rng.random(2) == 0 {
                        *v = rng.random(32) as u8;
                    }
                }
                let evs = match set.investment {
                    Investment::Evs { evs, .. } | Investment::StatPoints(evs) => evs,
                };
                set.investment = Investment::Evs { evs, ivs };
                set.level = [50, 50, 10, 15, 20, 30][rng.random(6) as usize];
                team.sets[i] = set;
                info_at(info, i).shiny = rng.random(3) == 0;
            }
        }
        "random-species" => {
            let s = SpeciesId(rng.random(dex.species.len() as u32) as u16);
            let mut ms: Vec<MoveId> = pools.learnable(s).into_iter().map(|(m, _)| m).collect();
            rng.shuffle(&mut ms);
            let mut set = legal_set(pools, rng, s);
            if !ms.is_empty() {
                set.moves = ms.iter().copied().take(1 + rng.random(4) as usize).collect();
            }
            team.sets[i] = set;
        }
        _ => unreachable!("unknown mutation {name}"),
    }
}

/// A special forme case: species, item, ability, moves.
type FormeCase = (SpeciesId, Option<ItemId>, Option<AbilityId>, &'static [MoveId]);

fn info_at(info: &mut Vec<SetInfo>, i: usize) -> &mut SetInfo {
    if info.len() <= i {
        info.resize(i + 1, SetInfo::default());
    }
    &mut info[i]
}

fn replace_move(rng: &mut BattleRng, set: &mut PokemonSet, m: MoveId) {
    if set.moves.contains(&m) {
        return;
    }
    if set.moves.is_empty() || (set.moves.len() < 4 && rng.random(2) == 0) {
        set.moves.push(m);
    } else {
        let j = rng.random(set.moves.len() as u32) as usize;
        set.moves[j] = m;
    }
}

/// The case for `seed`: a legal-looking team and 0-3 mutations.
pub fn random_case(pools: &Pools, seed: u64) -> ValidationCase {
    let mut rng = BattleRng::new(Seed::fast(seed));
    let mut team = legal_team(pools, &mut rng);
    let mut info = vec![SetInfo::default(); team.sets.len()];
    let mut mutations = Vec::new();
    let list = pools.mutations();
    for _ in 0..rng.random(4) {
        let m = list[rng.random(list.len() as u32) as usize];
        mutate(pools, &mut rng, &mut team, &mut info, m);
        mutations.push(m);
    }
    info.truncate(team.sets.len());
    ValidationCase { seed, team, info, mutations }
}

/// A set as a Showdown `PokemonSet` object.
fn set_json(dex: &Dex, set: &PokemonSet, info: &SetInfo) -> String {
    // Showdown's Champions mod reads Stat Points from the EV fields.
    let (evs, ivs) = match set.investment {
        Investment::Evs { evs, ivs } => (evs, ivs),
        Investment::StatPoints(sp) => (sp, Stats::splat(31)),
    };
    let stats = |s: Stats<u8>| {
        let fields: Vec<String> = Stat::ALL.iter().map(|&n| format!("\"{}\":{}", n.id(), s[n])).collect();
        format!("{{{}}}", fields.join(","))
    };
    let moves: Vec<String> = set.moves.iter().map(|&m| json_string(dex.move_(m).name)).collect();
    let mut out = format!(
        "{{\"name\":{},\"species\":{},\"item\":{},\"ability\":{},\"moves\":[{}],\"nature\":{},\"evs\":{},\"ivs\":{},\"level\":{},\"gender\":{},\"shiny\":{}",
        json_string(info.nickname.as_deref().unwrap_or("")),
        json_string(dex.species(set.species).name),
        json_string(set.item.map_or("", |i| dex.item(i).name)),
        json_string(dex.ability(set.ability).name),
        moves.join(","),
        json_string(set.nature.name()),
        stats(evs),
        stats(ivs),
        set.level,
        json_string(info.gender.or(set.gender).map_or("", Gender::letter)),
        info.shiny,
    );
    if let Some(t) = set.tera_type {
        out += &format!(",\"teraType\":{}", json_string(t.name()));
    }
    out.push('}');
    out
}

/// A team as a JSON array of Showdown sets.
pub fn team_json(dex: &Dex, team: &Team, info: &[SetInfo]) -> String {
    let default = SetInfo::default();
    let sets: Vec<String> =
        team.sets.iter().enumerate().map(|(i, s)| set_json(dex, s, info.get(i).unwrap_or(&default))).collect();
    format!("[{}]", sets.join(","))
}

/// Validates teams (JSON arrays of sets) with Showdown's validator for
/// `formatid`; one list of problem strings per team.
pub fn run_showdown_validator(formatid: &str, teams: &[String]) -> Result<Vec<Vec<String>>, OracleError> {
    run_validator(formatid, teams, false)
}

/// What Showdown's validator makes of a team: its problems, and the team
/// as it leaves it (the sets as `played` lines,
/// [`pikapro_validate::cases::played_line`]), which Showdown's server
/// plays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Validated {
    pub problems: Vec<String>,
    pub played: Vec<String>,
}

/// [`run_showdown_validator`], with each team as the validator leaves it.
pub fn run_showdown_validator_played(formatid: &str, teams: &[String]) -> Result<Vec<Validated>, OracleError> {
    let lines = run_validator(formatid, teams, true)?;
    if lines.len() != 2 * teams.len() {
        return Err(OracleError::Failed {
            status: None,
            stderr: format!("validator.mjs: {} lines for {} teams", lines.len(), teams.len()),
            log: Vec::new(),
        });
    }
    let mut it = lines.into_iter();
    let mut out = Vec::with_capacity(teams.len());
    while let (Some(problems), Some(played)) = (it.next(), it.next()) {
        out.push(Validated { problems, played });
    }
    Ok(out)
}

fn run_validator(formatid: &str, teams: &[String], played: bool) -> Result<Vec<Vec<String>>, OracleError> {
    let input =
        format!("{{\"formatid\":{},\"played\":{played},\"teams\":[{}]}}", json_string(formatid), teams.join(","));
    let mut child = Command::new("node")
        .arg(oracle_dir().join("validator.mjs"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| OracleError::Unavailable(e.to_string()))?;
    let mut stdin = child.stdin.take().expect("piped");
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().map_err(|e| OracleError::Unavailable(e.to_string()))?;
    writer.join().expect("writer").map_err(|e| OracleError::Unavailable(e.to_string()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(OracleError::Failed {
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            log: stdout.lines().map(str::to_owned).collect(),
        });
    }
    stdout.lines().map(parse_string_array).collect::<Result<_, _>>().map_err(|e| OracleError::Failed {
        status: None,
        stderr: e,
        log: Vec::new(),
    })
}

/// Parses a JSON array of strings (what `validator.mjs` prints).
fn parse_string_array(line: &str) -> Result<Vec<String>, String> {
    let mut chars = line.trim().chars().peekable();
    let mut out = Vec::new();
    if chars.next() != Some('[') {
        return Err(format!("not an array: {line}"));
    }
    loop {
        match chars.next() {
            Some(']') => return Ok(out),
            Some(',') => {}
            Some('"') => {
                let mut s = String::new();
                loop {
                    match chars.next().ok_or("unterminated string")? {
                        '"' => break,
                        '\\' => match chars.next().ok_or("bad escape")? {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            'r' => s.push('\r'),
                            'b' => s.push('\u{8}'),
                            'f' => s.push('\u{c}'),
                            'u' => {
                                let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                                let mut code = u32::from_str_radix(&hex, 16).map_err(|e| e.to_string())?;
                                if (0xd800..0xdc00).contains(&code) {
                                    let (a, b) = (chars.next(), chars.next());
                                    if a != Some('\\') || b != Some('u') {
                                        return Err("lone surrogate".into());
                                    }
                                    let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                                    let low = u32::from_str_radix(&hex, 16).map_err(|e| e.to_string())?;
                                    code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                                }
                                s.push(char::from_u32(code).ok_or("bad code point")?);
                            }
                            c => s.push(c),
                        },
                        c => s.push(c),
                    }
                }
                out.push(s);
            }
            other => return Err(format!("unexpected {other:?} in {line}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Battle-only formes: what the validator makes of a set that lists one.

/// Whether `validateForme` has something to say about `s`: a battle-only
/// forme (a Mega, a Primal, Mimikyu-Busted, ...), a forme an item or a move
/// makes (Ogerpon's masks, the Origin formes, Arceus, Keldeo-Resolute), or
/// one of the formes it names (Necrozma-Ultra, Zygarde, Terapagos).
fn forme_of_interest(pools: &Pools, s: SpeciesId) -> bool {
    let dex = pools.dex();
    let leg = pools.validator.format().legality.species(s);
    let data = dex.species(s);
    leg.battle_only != BattleOnly::No
        || !data.required_items.is_empty()
        || leg.required_move.is_some()
        || leg.required_ability.is_some()
        || matches!(data.base_species, species::ZYGARDE | species::TERAPAGOS | species::NECROZMA)
}

/// The team-independent part of a forme case: a lone set, so that the only
/// problems are the set's own and the team size.
fn forme_set(pools: &Pools, species: SpeciesId, moves_of: SpeciesId) -> PokemonSet {
    let f = pools.validator.format();
    // The level the format adjusts to (`Adjust Level = 50` in VGC and
    // BSS), else the highest it allows.
    let level = f.adjust_level.unwrap_or(f.max_level).min(100) as u8;
    let investment = if pools.stat_points() {
        Investment::StatPoints(Stats::new(2, 32, 0, 0, 0, 32))
    } else {
        Investment::Evs { evs: Stats::new(4, 252, 0, 0, 0, 252), ivs: Stats::splat(31) }
    };
    let mut legal = pools.legal_moves(moves_of);
    legal.truncate(2);
    PokemonSet::new(species).with_level(level).with_nature(Nature::Jolly).with_investment(investment).with_moves(&legal)
}

/// Cases for every forme [`validateForme`](pikapro_validate) handles in
/// the format's dex, each a lone set (named `<format>-<species>-<variant>`):
///
/// * `listed`: the forme with its required item and move, and its own first
///   ability (a Mega's, which the validator gives the base forme's first);
/// * `base-ability`: the same with the forme it changes from's first ability;
/// * `no-item`: without its required item (rejected);
/// * `wrong-ability`: another ability than its required one (rejected);
/// * `behemoth`: a Crowned hero with its Behemoth move (Iron Head in
///   battle);
/// * `from-base`: the forme it changes from with the forme's required item
///   and move (a forme an item or a move sets, like Ogerpon's masks or
///   Keldeo-Resolute; a Mega Stone leaves the species alone).
pub fn forme_cases(pools: &Pools) -> Vec<(String, Team)> {
    let dex = pools.dex();
    let f = pools.validator.format();
    let leg = f.legality;
    let slug = |s: &str| s.to_lowercase().replace([' ', '%', '\'', '.', ':'], "");
    let mut out = Vec::new();
    for s in (0..dex.species.len() as u16).map(SpeciesId) {
        if !forme_of_interest(pools, s) {
            continue;
        }
        let data = dex.species(s);
        let l = leg.species(s);
        let base = match l.battle_only {
            BattleOnly::Species(b) => b,
            _ => l.changes_from.unwrap_or(data.base_species),
        };
        let name = |variant: &str| format!("{}-{}-{variant}", f.id, slug(data.name));
        let mut listed = forme_set(pools, s, base).with_ability(data.abilities.primary);
        if let Some(&item) = data.required_items.first() {
            listed.item = Some(item);
        }
        if let Some(m) = l.required_move
            && !listed.moves.contains(&m)
        {
            listed.moves.push(m);
        }
        out.push((name("listed"), Team::new(vec![listed.clone()])));
        let base_ability = dex.species(base).abilities.primary;
        if base_ability != data.abilities.primary {
            out.push((name("base-ability"), Team::new(vec![listed.clone().with_ability(base_ability)])));
        }
        // Past formes (the Megas outside Champions, ...): the listed set
        // shows the verdict; the rest adds nothing.
        if data.nonstandard.is_some() {
            continue;
        }
        if listed.item.is_some() {
            let mut no_item = listed.clone();
            no_item.item = None;
            out.push((name("no-item"), Team::new(vec![no_item])));
        }
        if let Some(required) = l.required_ability {
            let slots = dex.species(base).abilities;
            let other = [Some(slots.primary), slots.secondary, slots.hidden]
                .into_iter()
                .flatten()
                .find(|&a| a != required)
                .unwrap_or(AbilityId::NONE);
            out.push((name("wrong-ability"), Team::new(vec![listed.clone().with_ability(other)])));
        }
        let behemoth = match s {
            species::ZACIAN_CROWNED => Some(moves::BEHEMOTH_BLADE),
            species::ZAMAZENTA_CROWNED => Some(moves::BEHEMOTH_BASH),
            _ => None,
        };
        if let Some(m) = behemoth {
            let mut set = listed.clone();
            set.moves.truncate(1);
            set.moves.push(m);
            out.push((name("behemoth"), Team::new(vec![set])));
        }
        if base != s && (listed.item.is_some() || l.required_move.is_some()) {
            let mut from_base = listed.clone().with_ability(base_ability);
            from_base.species = base;
            out.push((name("from-base"), Team::new(vec![from_base])));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_string_arrays() {
        assert_eq!(parse_string_array("[]").unwrap(), Vec::<String>::new());
        assert_eq!(
            parse_string_array(r#"["a \"b\"","Pokémon","é"]"#).unwrap(),
            vec!["a \"b\"".to_owned(), "Pokémon".to_owned(), "é".to_owned()]
        );
    }

    #[test]
    fn cases_are_deterministic() {
        let pools = Pools::new(ValidationFormat::champions_vgc_2026_reg_mc());
        let a = random_case(&pools, 1_800_000);
        let b = random_case(&pools, 1_800_000);
        assert_eq!(a.team, b.team);
        assert_eq!(a.mutations, b.mutations);
    }

    /// Only a format with bans gets the ban mutations, and gen9ou's pools
    /// hold what its rules ban.
    #[test]
    fn ban_mutations_are_for_formats_with_bans() {
        use pikapro_data::gen9::{abilities, items, moves, species};
        for f in ValidationFormat::all() {
            let pools = Pools::new(f.clone());
            assert_eq!(pools.mutations().len() > MUTATIONS.len(), f.id == "gen9ou", "{}", f.id);
        }
        let pools = Pools::new(ValidationFormat::gen9_ou());
        assert!(pools.tier_banned.contains(&species::KORAIDON) && !pools.legal_species.contains(&species::KORAIDON));
        assert!(pools.legal_species.contains(&species::GARCHOMP));
        for m in [moves::FISSURE, moves::SPORE, moves::YAWN, moves::BATON_PASS, moves::DOUBLE_TEAM] {
            assert!(pools.banned_moves.contains(&m), "{m:?}");
        }
        assert!(pools.banned_abilities.contains(&abilities::SHADOW_TAG));
        assert!(pools.banned_items.contains(&items::KINGS_ROCK));
    }
}
