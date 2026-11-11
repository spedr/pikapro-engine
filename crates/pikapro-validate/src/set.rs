//! One set: Showdown's `validateSet` and what it calls (`validateForme`,
//! `getValidationSpecies`, `checkSpecies` / `checkItem` / `checkMove` /
//! `checkAbility`, `checkTagRules`, `validateStats`, `validatePokemonGo`,
//! `validateMoves`, `validateSource`, `validateEvent`, `getEventOnlyData`).
//!
//! Showdown mutates the set while validating it (the species moves to its
//! out-of-battle forme, levels are adjusted, the Hidden Power type and the
//! gender are filled in); `Work` is that mutable copy.

use pikapro::{Investment, PokemonSet};
use pikapro_data::gen9::{abilities, items, moves, species};
use pikapro_data::legality::{BattleOnly, EventGender, EventInfo, EventShiny, GoSources, SpeciesTags};
use pikapro_data::{AbilityId, GenderRule, ItemId, MoveId, Nature, Nonstandard, SpeciesId, Stat, Type};

use crate::problem::{Because, EventProblem, FormeProblem, From, GoIntro, GoProblem, ProblemKind, Thing};
use crate::rules::{Check, SetRule, Sign, Tag, Target};
use crate::sources::{Source, SourceKind, Sources};
use crate::{Gender, SetInfo, Validator};

/// Showdown's `set.gender` once validation fills it: `M`, `F` or (from a
/// genderless species) `N`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetGender {
    M,
    F,
    N,
}

/// The set as `validateSet` mutates it.
#[derive(Clone, Debug)]
pub(crate) struct Work {
    /// `set.name`: the nickname, or the base species.
    pub name: String,
    pub species: SpeciesId,
    pub item: Option<ItemId>,
    pub ability: AbilityId,
    pub nature: Nature,
    pub moves: Vec<MoveId>,
    pub level: u16,
    /// `set.evs` (Stat Points under the Champions mod), hp to spe.
    pub evs: [u16; 6],
    pub ivs: [u8; 6],
    pub gender: Option<SetGender>,
    pub shiny: bool,
    pub hp_type: Option<Type>,
    pub tera_type: Option<Type>,
}

/// What `validateSet` leaves for the team checks and the battle.
#[derive(Clone, Debug)]
pub(crate) struct SetResult {
    pub problems: Vec<ProblemKind>,
    /// The set with the species, ability, moves, level, Tera type and
    /// gender `validateSet` left it (the level by `Adjust Level` / `Adjust
    /// Level Down`; `validateForme`: a battle-only forme becomes the forme it
    /// changes from, Behemoth Blade/Bash become Iron Head, ...; a Mega's
    /// ability becomes the base forme's first; an event's gender); the
    /// other fields as given. A gender the species fixes is left unset
    /// unless given (the battle gives it the same).
    pub played: PokemonSet,
    pub name: String,
}

/// `checkTagRules`'s `nonexistentCheck`: falsy, `''` or a reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nonexistent {
    Falsy,
    Allowed,
    Banned,
}

/// Showdown's `getHiddenPower(ivs).type` (Gen 3+), IVs hp to spe.
pub(crate) fn hidden_power_type(ivs: &[u8; 6]) -> Type {
    const TYPES: [Type; 16] = [
        Type::Fighting,
        Type::Flying,
        Type::Poison,
        Type::Ground,
        Type::Rock,
        Type::Bug,
        Type::Ghost,
        Type::Steel,
        Type::Fire,
        Type::Water,
        Type::Grass,
        Type::Electric,
        Type::Psychic,
        Type::Ice,
        Type::Dragon,
        Type::Dark,
    ];
    // Showdown's order: hp, atk, def, spe, spa, spd.
    let order = [0, 1, 2, 5, 3, 4];
    let mut x = 0u32;
    for (bit, &stat) in order.iter().enumerate() {
        x += (1 << bit) * (ivs[stat] as u32 % 2);
    }
    TYPES[(x * 15 / 63) as usize]
}

/// The IVs a Hidden Power type needs in Gen 3+ (`typechart.ts` `HPivs`),
/// hp to spe, 31 elsewhere.
fn hidden_power_ivs(t: Type) -> [u8; 6] {
    let mut ivs = [31u8; 6];
    let set = |ivs: &mut [u8; 6], s: &[(usize, u8)]| {
        for &(i, v) in s {
            ivs[i] = v;
        }
    };
    let (hp, atk, def, spa, spd, spe) = (0, 1, 2, 3, 4, 5);
    match t {
        Type::Bug => set(&mut ivs, &[(atk, 30), (def, 30), (spd, 30)]),
        Type::Dark => {}
        Type::Dragon => set(&mut ivs, &[(atk, 30)]),
        Type::Electric => set(&mut ivs, &[(spa, 30)]),
        Type::Fighting => set(&mut ivs, &[(def, 30), (spa, 30), (spd, 30), (spe, 30)]),
        Type::Fire => set(&mut ivs, &[(atk, 30), (spa, 30), (spe, 30)]),
        Type::Flying => set(&mut ivs, &[(hp, 30), (atk, 30), (def, 30), (spa, 30), (spd, 30)]),
        Type::Ghost => set(&mut ivs, &[(def, 30), (spd, 30)]),
        Type::Grass => set(&mut ivs, &[(atk, 30), (spa, 30)]),
        Type::Ground => set(&mut ivs, &[(spa, 30), (spd, 30)]),
        Type::Ice => set(&mut ivs, &[(atk, 30), (def, 30)]),
        Type::Poison => set(&mut ivs, &[(def, 30), (spa, 30), (spd, 30)]),
        Type::Psychic => set(&mut ivs, &[(atk, 30), (spe, 30)]),
        Type::Rock => set(&mut ivs, &[(def, 30), (spd, 30), (spe, 30)]),
        Type::Steel => set(&mut ivs, &[(spd, 30)]),
        Type::Water => set(&mut ivs, &[(atk, 30), (def, 30), (spa, 30)]),
        _ => {}
    }
    ivs
}

/// `possibleBottleCapHpType`.
fn possible_bottle_cap_hp_type(t: Type, ivs: &[u8; 6]) -> bool {
    let (atk, spe) = (ivs[1], ivs[5]);
    if matches!(t, Type::Dark | Type::Dragon | Type::Grass | Type::Ghost | Type::Poison) && spe % 2 == 0 {
        return false;
    }
    if matches!(t, Type::Psychic | Type::Fire | Type::Rock | Type::Fighting) && spe != 31 && spe % 2 == 1 {
        return false;
    }
    if t == Type::Dark && atk % 2 == 0 {
        return false;
    }
    if matches!(t, Type::Ice | Type::Water) && spe % 2 == 0 && atk % 2 == 0 {
        return false;
    }
    true
}

impl Validator {
    fn sp_name(&self, s: SpeciesId) -> &'static str {
        self.dex.species(s).name
    }

    fn has_move(&self, w: &Work, m: MoveId) -> bool {
        w.moves.contains(&m)
    }

    /// `validateSet`.
    pub(crate) fn validate_set(&self, set: &PokemonSet, info: &SetInfo) -> SetResult {
        let f = &self.format;
        let dex = self.dex;
        let mut problems: Vec<ProblemKind> = Vec::new();
        let original = set.species;
        let (evs, ivs) = match set.investment {
            Investment::Evs { evs, ivs } => (evs.0.map(u16::from), ivs.0),
            // Showdown reads Stat Points from the EV fields; IVs are 31.
            Investment::StatPoints(sp) => (sp.0.map(u16::from), [31; 6]),
        };
        let mut w = Work {
            name: String::new(),
            species: original,
            item: set.item,
            ability: set.ability,
            nature: set.nature,
            moves: set.moves.to_vec(),
            level: set.level as u16,
            evs,
            ivs,
            // `if (!['M', 'F'].includes(set.gender)) set.gender = ''`.
            gender: match info.gender.or(set.gender) {
                Some(Gender::Male) => Some(SetGender::M),
                Some(Gender::Female) => Some(SetGender::F),
                Some(Gender::Genderless) | None => None,
            },
            shiny: info.shiny,
            hp_type: None,
            tera_type: set.tera_type,
        };
        let base_name = self.sp_name(self.base(original));
        let mut nickname = info.nickname.clone().unwrap_or_default();
        if !nickname.is_empty() && nickname.encode_utf16().count() > 18 {
            if nickname == self.sp_name(original) {
                nickname = base_name.to_owned();
            } else {
                let len = nickname.encode_utf16().count();
                problems.push(ProblemKind::NicknameTooLong { species: original, nickname: nickname.clone(), len });
            }
        }
        w.name = pikapro_data::get_name(&nickname);
        if w.name.is_empty() {
            w.name = base_name.to_owned();
        }
        // validateSet's `name`.
        let name = if w.name != self.sp_name(original) && w.name != base_name {
            format!("{} ({})", w.name, self.sp_name(original))
        } else {
            self.sp_name(original).to_owned()
        };

        if w.level == 0 {
            w.level = f.default_level;
        }
        let mut adjust_level = f.adjust_level;
        if let Some(down) = f.adjust_level_down
            && w.level >= down
        {
            adjust_level = Some(down);
        }
        if Some(w.level) == adjust_level || (w.level == 100 && f.max_level < 100) {
            w.level = f.max_level;
        }
        if w.level < f.min_level {
            problems.push(ProblemKind::LevelBelowMin { name: name.clone(), level: w.level, min: f.min_level });
        }
        if w.level > f.max_level {
            problems.push(ProblemKind::LevelAboveMax { name: name.clone(), level: w.level, max: f.max_level });
        }
        if f.obtainable_formes {
            problems.extend(self.validate_forme(&mut w).into_iter().map(ProblemKind::Forme));
        }
        let species = w.species;
        let mut set_sources = self.all_sources(species);
        // Obtainable Misc's `onChangeSet`: duplicate moves.
        if f.obtainable_misc {
            let mut seen: Vec<MoveId> = Vec::new();
            for &m in &w.moves {
                let id = self.legality.move_(m).id;
                if seen.contains(&id) {
                    problems.push(ProblemKind::DuplicateMove { base: self.base(w.species), move_: m });
                    break;
                }
                seen.push(id);
            }
        }
        if f.generation <= 5 || f.obtainable_misc {
            match dex.species(species).gender {
                GenderRule::Male => w.gender = Some(SetGender::M),
                GenderRule::Female => w.gender = Some(SetGender::F),
                GenderRule::Genderless => w.gender = Some(SetGender::N),
                GenderRule::Ratio { .. } => {}
            }
        }
        let (out_of_battle, tier_species) = self.validation_species(&w);
        if w.ability == abilities::BATTLE_BOND && self.base(species) == species::GRENINJA && f.obtainable_misc {
            if w.gender.is_some_and(|g| g != SetGender::M) {
                problems.push(ProblemKind::BattleBondMale);
            }
            w.gender = Some(SetGender::M);
        }
        if f.generation == 9 && !f.champions_mod && !f.terastal_clause {
            let required = dex.species(species).required_tera_type.map(|t| t.ty());
            let ty = w.tera_type.or(required).unwrap_or_else(|| dex.species(species).types.primary);
            if let Some(r) = required
                && r != ty
                && f.obtainable_misc
            {
                problems.push(ProblemKind::TeraTypeRequired { species, required: r });
            }
            w.tera_type = Some(ty);
        } else {
            w.tera_type = None;
        }
        if let Some(p) = self.check_species(&w, species, tier_species) {
            problems.push(p);
        }
        if let Some(p) = self.check_item(&w) {
            problems.push(p);
        }
        if f.obtainable_abilities {
            // Showdown compares the ability before the Mega fix-up below
            // with the Hidden Ability.
            let ability = w.ability;
            let slots = dex.species(species).abilities;
            let all = [Some(slots.primary), slots.secondary, slots.hidden, slots.special];
            if w.ability == AbilityId::NONE {
                problems.push(ProblemKind::NeedsAbility { name: name.clone() });
            } else if !all.contains(&Some(w.ability)) {
                if dex.species(tier_species).abilities.primary == w.ability {
                    w.ability = slots.primary;
                } else {
                    problems.push(ProblemKind::CantHaveAbility { name: name.clone(), ability: w.ability });
                }
            }
            // `unreleasedHidden` and `maleOnlyHidden` are in no Gen 9 data
            // (datagen checks).
            set_sources.is_hidden = Some(slots.hidden == Some(ability));
        }
        if let Some(p) = self.check_ability(&w) {
            problems.push(p);
        }
        if let Some(p) = self.check_nature(&w) {
            problems.push(p);
        }
        if w.moves.is_empty() {
            problems.push(ProblemKind::NoMoves { name: name.clone() });
        }
        let mut go_problems = self.validate_pokemon_go(out_of_battle, &w, &mut set_sources, None);
        if f.is_banned(Target::Tag(Tag::Nonexistent)) {
            problems.extend(self.validate_stats(&mut w, species, &mut set_sources, go_problems.as_deref()));
        }
        for &m in &w.moves.clone() {
            if let Some(p) = self.check_move(&w, m) {
                problems.push(p);
            }
        }
        let mut underleveled: Option<(SpeciesId, u8)> = None;
        if f.obtainable_misc {
            let mut evo = species;
            while let Some(prevo) = self.legality.species(evo).prevo {
                let evo_level = self.legality.species(evo).evo_level;
                if w.level < evo_level as u16 {
                    underleveled = Some((evo, evo_level));
                    break;
                }
                evo = prevo;
            }
        }
        if f.obtainable_moves {
            let move_problems = self.validate_moves(out_of_battle, &w, &mut set_sources, &name);
            problems.extend(move_problems);
            if let (Some(gp), Some(incompatible)) = (go_problems.as_mut(), &set_sources.go_incompatible_moves) {
                if incompatible.is_empty() {
                    gp.push(GoProblem::Incompatible(name.clone()));
                } else {
                    gp.push(GoProblem::IncompatibleMoves(name.clone(), incompatible.clone()));
                }
            }
        }
        let go_ok = go_problems.as_ref().is_some_and(Vec::is_empty);
        let go_failed = go_problems.as_ref().is_some_and(|g| !g.is_empty());
        let push_go = |problems: &mut Vec<ProblemKind>, intro: GoIntro, go: &Option<Vec<GoProblem>>| {
            problems.push(ProblemKind::GoIntro { intro, name: name.clone() });
            for p in go.iter().flatten() {
                problems.push(ProblemKind::Go(p.clone()));
            }
        };
        if (set_sources.sources_before == 0 && !set_sources.sources.is_empty()) || underleveled.is_some() {
            let mut check_go_legality = false;
            let mut skipped_egg_source = true;
            let mut legal_sources: Vec<Source> = Vec::new();
            if let (Some((under, _)), true) = (underleveled, set_sources.sources.is_empty()) {
                let mut evo = species;
                while self.legality.species(evo).prevo.is_some() {
                    for (i, event) in self.legality.species(evo).events.iter().enumerate() {
                        if event.level != 0 && w.level >= event.level as u16 {
                            set_sources.sources.push(Source::event(event.generation, i as u8, evo));
                        }
                    }
                    if evo == under {
                        break;
                    }
                    evo = self.legality.species(evo).prevo.expect("checked");
                }
            } else {
                check_go_legality = true;
            }
            for source in set_sources.sources.clone() {
                if let Some((under, _)) = underleveled {
                    if let (SourceKind::Event(i), Some(s)) = (source.kind, source.species) {
                        // Showdown reads the species from `source.substr(3)`,
                        // which only works for single-digit event indices.
                        let event_base = if i < 10 { Some(self.base(s)) } else { None };
                        if event_base != Some(self.base(species)) && event_base != Some(self.base(under)) {
                            continue;
                        }
                    } else if source != Source::transfer(8) {
                        continue;
                    }
                }
                if matches!(source.generation, 2 | 3) && source.is_egg() && w.level < 5 {
                    continue;
                }
                skipped_egg_source = false;
                if self.validate_source(&mut w, source, &set_sources, out_of_battle, None).is_err() {
                    continue;
                }
                legal_sources.push(source);
            }
            if check_go_legality && !legal_sources.contains(&Source::transfer(8)) {
                set_sources.is_from_pokemon_go = Some(false);
            }
            if set_sources.is_from_pokemon_go != Some(false) && go_ok {
                if legal_sources.is_empty() {
                    set_sources.is_from_pokemon_go = Some(true);
                }
                if !legal_sources.contains(&Source::transfer(8)) {
                    legal_sources.push(Source::transfer(8));
                }
            }
            if !legal_sources.is_empty() {
                set_sources.sources = legal_sources;
            } else if let Some((_, required_level)) = underleveled {
                problems.push(ProblemKind::Underleveled { name: name.clone(), level: required_level });
                if let Some(&first) = set_sources.sources.iter().find(|s| s.is_event()) {
                    let because = Some(Because::Underleveled);
                    if let Err(e) = self.validate_source(&mut w, first, &set_sources, out_of_battle, because) {
                        problems.extend(e);
                    }
                }
                if go_failed {
                    push_go(&mut problems, GoIntro::Failed, &go_problems);
                }
            } else {
                match set_sources.sources.iter().find(|s| !s.is_egg()).copied() {
                    None => {
                        if skipped_egg_source {
                            problems.push(ProblemKind::EggBelowLevel5 { name: name.clone() });
                        } else {
                            let moves = set_sources.limited_egg_moves.as_ref().cloned().unwrap_or_default();
                            problems.push(ProblemKind::EggMoveCombination { name: name.clone(), moves });
                        }
                    }
                    Some(non_egg) => {
                        if species == species::MEW && go_ok {
                            set_sources.is_from_pokemon_go = Some(true);
                        } else {
                            if set_sources.sources.len() > 1 {
                                problems.push(ProblemKind::EventExclusive { name: name.clone() });
                            }
                            let because = Some(Because::MoveOnlyAvailable);
                            if let Err(e) = self.validate_source(&mut w, non_egg, &set_sources, out_of_battle, because)
                            {
                                problems.extend(e);
                            }
                            if species == species::MEW && go_failed {
                                push_go(&mut problems, GoIntro::Additionally, &go_problems);
                            }
                        }
                    }
                }
            }
        } else if let Some((event_species, events)) =
            self.event_only_data(out_of_battle, false).filter(|_| f.obtainable_misc)
        {
            let mut legal = false;
            for event in events {
                if self.validate_event(&mut w, &set_sources, event, event_species, None, From::AnEvent).is_err() {
                    continue;
                }
                set_sources.event_only_min_source_gen = event.generation;
                legal = true;
                break;
            }
            if !legal && self.legality.species(species).generation <= 2 && f.generation >= 7 {
                let vc = Source::transfer(7);
                if self.validate_source(&mut w, vc, &set_sources, species, None).is_ok() {
                    legal = true;
                }
            }
            if !legal {
                if go_problems.is_none() || go_failed {
                    problems.push(ProblemKind::EventOnly { species, several: events.len() > 1 });
                    for (i, event) in events.iter().enumerate() {
                        if event.generation <= f.generation
                            && (event.generation >= f.min_source_gen || f.generation > 8)
                        {
                            let which = if events.len() > 1 { Some(i + 1) } else { None };
                            let from = From::ItsEvent(which);
                            if let Err(e) = self.validate_event(
                                &mut w,
                                &set_sources,
                                event,
                                event_species,
                                Some(Because::ToBe),
                                from,
                            ) {
                                problems.extend(e);
                            }
                        }
                    }
                    if go_failed {
                        push_go(&mut problems, GoIntro::Additionally, &go_problems);
                    }
                } else {
                    set_sources.is_from_pokemon_go = Some(true);
                }
            }
        }
        const GO_ONLY: [SpeciesId; 3] = [species::MELTAN, species::MELMETAL, species::GIMMIGHOUL_ROAMING];
        if f.obtainable_misc && GO_ONLY.contains(&species) && go_failed {
            push_go(&mut problems, GoIntro::OnlyFromGo, &go_problems);
        }
        // (The checks after this in Showdown need Gen 7 or earlier.)
        // Then the rules' `onValidateSet`, in the rule table's order: each
        // walks the set's moves.
        for &rule in f.set_rules {
            for &m in &w.moves {
                let data = dex.move_(m);
                let banned = match rule {
                    // `if (move.ohko) problems.push(...)`.
                    SetRule::OhkoClause => data.ohko,
                    // `if (move.status === 'slp') problems.push(...)`.
                    SetRule::SleepMovesClause => data.status == Some(pikapro_data::StatusId::Sleep),
                };
                if banned {
                    problems.push(ProblemKind::MoveBannedByClause { move_: m, rule });
                }
            }
        }
        if let Some(named) = self.species_named(&w.name) {
            if self.base(named) == self.base(species) {
                w.name = self.sp_name(self.base(species)).to_owned();
            } else if named != species
                && named != self.base(species)
                && f.team_rules.contains(&crate::rules::TeamRule::NicknameClause)
            {
                problems.push(ProblemKind::NicknameSpecies { name });
            }
        }
        let mut played = set.clone();
        played.species = w.species;
        played.ability = w.ability;
        played.moves = w.moves.iter().copied().collect();
        // `set.level` as rewritten above, and `if (adjustLevel) set.level =
        // adjustLevel` for a set without problems.
        let level = match adjust_level {
            Some(level) if problems.is_empty() => level,
            _ => w.level,
        };
        played.level = u8::try_from(level).unwrap_or(set.level);
        // `set.teraType = type.name` (the given one, else the species'
        // forced one, else its first type), or deleted without Terastallization.
        played.tera_type = w.tera_type;
        // `set.gender`: an `N` given is dropped (a species that can be
        // either then draws one in battle); an event's or Battle Bond's is
        // set.
        let gender = w.gender.map(|g| match g {
            SetGender::M => Gender::Male,
            SetGender::F => Gender::Female,
            SetGender::N => Gender::Genderless,
        });
        let fixed = match dex.species(w.species).gender {
            GenderRule::Male => Some(Gender::Male),
            GenderRule::Female => Some(Gender::Female),
            GenderRule::Genderless => Some(Gender::Genderless),
            GenderRule::Ratio { .. } => None,
        };
        played.gender =
            if fixed.is_some() && gender == fixed { set.gender.filter(|&g| Some(g) == fixed) } else { gender };
        SetResult { problems, played, name: w.name }
    }

    /// A species whose name is `name`, ignoring case (`dex.species.get(name)`
    /// with its exact-name check).
    fn species_named(&self, name: &str) -> Option<SpeciesId> {
        let lower = name.to_lowercase();
        let id = self.dex.species_by_name(name)?;
        (self.sp_name(id).to_lowercase() == lower).then_some(id)
    }

    /// `allSources`.
    pub(crate) fn all_sources(&self, s: SpeciesId) -> Sources {
        let f = &self.format;
        let mut min = f.min_source_gen;
        if f.generation >= 3 && min < 3 {
            min = 3;
        }
        min = min.max(self.legality.species(s).generation);
        Sources::new(f.generation, min)
    }

    /// `getValidationSpecies`: `(outOfBattleSpecies, tierSpecies)`.
    fn validation_species(&self, w: &Work) -> (SpeciesId, SpeciesId) {
        let f = &self.format;
        let s = w.species;
        let mut out = s;
        let mut tier = s;
        if w.ability == abilities::BATTLE_BOND && self.base(s) == species::GRENINJA {
            out = species::GRENINJA_BOND;
            if f.obtainable_formes {
                tier = out;
            }
        }
        if w.ability == abilities::OWN_TEMPO && self.base(s) == species::ROCKRUFF {
            out = species::ROCKRUFF_DUSK;
            if f.obtainable_formes {
                tier = out;
            }
        }
        if f.obtainable_formes {
            let mega = w.item.and_then(|i| self.dex.item(i).mega.iter().find(|&&(base, _)| base == s).map(|&(_, m)| m));
            if let Some(m) = mega {
                tier = m;
            } else if w.item == Some(items::RED_ORB) && s == species::GROUDON {
                tier = species::GROUDON_PRIMAL;
            } else if w.item == Some(items::BLUE_ORB) && s == species::KYOGRE {
                tier = species::KYOGRE_PRIMAL;
            } else if w.item == Some(items::RUSTED_SWORD) && s == species::ZACIAN {
                tier = species::ZACIAN_CROWNED;
            } else if w.item == Some(items::RUSTED_SHIELD) && s == species::ZAMAZENTA {
                tier = species::ZAMAZENTA_CROWNED;
            }
            // (Mega Rayquaza's Dragon Ascent rule needs Gen 7 or `+Past`.)
        }
        (out, tier)
    }

    /// `validateForme`.
    fn validate_forme(&self, w: &mut Work) -> Vec<FormeProblem> {
        let dex = self.dex;
        let mut problems = Vec::new();
        let s = w.species;
        let data = dex.species(s);
        let leg = self.legality.species(s);
        let name = w.name.clone();
        if s == species::NECROZMA_ULTRA {
            let which = u8::from(self.has_move(w, moves::SUNSTEEL_STRIKE))
                + 2 * u8::from(self.has_move(w, moves::MOONGEIST_BEAM));
            if w.item != Some(items::ULTRANECROZIUM_Z) {
                problems.push(FormeProblem::NecrozmaUltraItem);
            } else if which == 1 {
                w.species = species::NECROZMA_DUSK_MANE;
                w.ability = abilities::PRISM_ARMOR;
            } else if which == 2 {
                w.species = species::NECROZMA_DAWN_WINGS;
                w.ability = abilities::PRISM_ARMOR;
            } else {
                problems.push(FormeProblem::NecrozmaUltraStart);
            }
        } else if data.base_species == species::ZYGARDE {
            if s == species::ZYGARDE_COMPLETE || s == species::ZYGARDE_MEGA {
                problems.push(FormeProblem::ZygardeStart(s));
            }
            if w.item == Some(items::ZYGARDITE) && w.ability != abilities::POWER_CONSTRUCT {
                problems.push(FormeProblem::ZygarditeAbility);
            }
        } else if data.base_species == species::TERAPAGOS {
            w.species = species::TERAPAGOS;
            w.ability = abilities::TERA_SHIFT;
        } else if let BattleOnly::Species(target) = leg.battle_only {
            if let Some(a) = leg.required_ability
                && w.ability != a
            {
                problems.push(FormeProblem::TransformAbility(s, a));
            }
            if !data.required_items.is_empty() && !w.item.is_some_and(|i| data.required_items.contains(&i)) {
                problems.push(FormeProblem::TransformItem(s, leg.required_item));
            }
            if let Some(m) = leg.required_move
                && !self.has_move(w, m)
            {
                problems.push(FormeProblem::TransformMove(s, m));
            }
            w.species = target;
        } else {
            if !data.required_items.is_empty() && !w.item.is_some_and(|i| data.required_items.contains(&i)) {
                if self.format.generation >= 8
                    && (data.base_species == species::ARCEUS || data.base_species == species::SILVALLY)
                {
                    problems.push(FormeProblem::NeedsItemArceus { name: name.clone(), species: s });
                } else {
                    let revert = leg.changes_from.map_or("", |c| self.legality.species(c).base_forme);
                    let revert = if revert.is_empty() { "base" } else { revert };
                    problems.push(FormeProblem::NeedsItem {
                        name: name.clone(),
                        species: s,
                        revert: revert.to_owned(),
                    });
                }
            }
            if let Some(m) = leg.required_move
                && !self.has_move(w, m)
            {
                let revert = leg.changes_from.map_or("", |c| self.legality.species(c).base_forme);
                problems.push(FormeProblem::NeedsMove {
                    name: name.clone(),
                    species: s,
                    move_: m,
                    revert: revert.to_owned(),
                });
            }
            if let Some(forme) = w.item.and_then(|i| self.legality.item(i).forced_forme)
                && s == dex.species(forme).base_species
            {
                w.species = forme;
            }
        }
        // (Pikachu-Cosplay's check compares move names with IDs, so it never
        // matches a set given by name.)
        if s == species::KELDEO && self.has_move(w, moves::SECRET_SWORD) && self.format.generation >= 8 {
            w.species = species::KELDEO_RESOLUTE;
        }
        let behemoth = if s == species::ZACIAN_CROWNED {
            Some(moves::BEHEMOTH_BLADE)
        } else if s == species::ZAMAZENTA_CROWNED {
            Some(moves::BEHEMOTH_BASH)
        } else {
            None
        };
        if let Some(b) = behemoth
            && let Some(slot) = w.moves.iter_mut().find(|m| **m == b)
        {
            *slot = moves::IRON_HEAD;
        }
        if data.base_species == species::HOOPA && self.format.generation >= 9 {
            if s == species::HOOPA && self.has_move(w, moves::HYPERSPACE_FURY) {
                problems.push(FormeProblem::HoopaFury);
            } else if s == species::HOOPA_UNBOUND && self.has_move(w, moves::HYPERSPACE_HOLE) {
                problems.push(FormeProblem::HoopaUnboundHole);
            }
        }
        if data.base_species == species::GRENINJA && w.ability == abilities::BATTLE_BOND {
            w.species = species::GRENINJA_BOND;
        }
        if data.base_species == species::ROCKRUFF && w.ability == abilities::OWN_TEMPO {
            w.species = species::ROCKRUFF_DUSK;
        }
        problems
    }

    /// `checkSpecies`.
    fn check_species(&self, w: &Work, species: SpeciesId, tier: SpeciesId) -> Option<ProblemKind> {
        let f = &self.format;
        let mut s = species;
        if (tier == species::ZAMAZENTA_CROWNED && s == species::ZAMAZENTA)
            || (tier == species::ZACIAN_CROWNED && s == species::ZACIAN)
        {
            s = tier;
        }
        match f.check(Target::Pokemon(s)) {
            Check::Banned(reason) => return Some(ProblemKind::SpeciesBanned { species: s, reason }),
            Check::Allowed => return None,
            Check::Neutral => {}
        }
        if tier != s {
            match f.check(Target::Pokemon(tier)) {
                Check::Banned(reason) => return Some(ProblemKind::SpeciesBanned { species: tier, reason }),
                Check::Allowed => return None,
                Check::Neutral => {}
            }
        }
        // (No format here bans `tag:mega` or Gigantamax.)
        let base = self.base(s);
        match f.check(Target::BasePokemon(base)) {
            Check::Banned(reason) => return Some(ProblemKind::SpeciesBanned { species: s, reason }),
            Check::Allowed if self.dex.species(base).nonstandard == self.dex.species(s).nonstandard => return None,
            _ => {}
        }
        self.check_tag_rules(w, Thing::Species(tier))
    }

    /// `checkItem`.
    fn check_item(&self, w: &Work) -> Option<ProblemKind> {
        let f = &self.format;
        let target = w.item.map_or(Target::NoItem, Target::Item);
        match (f.check(target), w.item) {
            (Check::Banned(reason), None) => return Some(ProblemKind::NoItemBanned { name: w.name.clone(), reason }),
            (Check::Banned(reason), Some(item)) => {
                return Some(ProblemKind::ItemBanned { name: w.name.clone(), item, reason });
            }
            (Check::Allowed, _) => return None,
            (Check::Neutral, _) => {}
        }
        let item = w.item?;
        self.check_tag_rules(w, Thing::Item(item))
    }

    /// `checkMove`.
    fn check_move(&self, w: &Work, m: MoveId) -> Option<ProblemKind> {
        match self.format.check(Target::Move(self.legality.move_(m).id)) {
            Check::Banned(reason) => return Some(ProblemKind::MoveBanned { name: w.name.clone(), move_: m, reason }),
            Check::Allowed => return None,
            Check::Neutral => {}
        }
        self.check_tag_rules(w, Thing::Move(m))
    }

    /// `checkAbility`.
    fn check_ability(&self, w: &Work) -> Option<ProblemKind> {
        let f = &self.format;
        let a = w.ability;
        match f.check(Target::Ability(a)) {
            Check::Banned(reason) => {
                return Some(ProblemKind::AbilityBanned { name: w.name.clone(), ability: a, reason });
            }
            Check::Allowed => return None,
            Check::Neutral => {}
        }
        let nonstandard = self.dex.ability(a).nonstandard?;
        let tag = Tag::of_nonstandard(nonstandard);
        match f.check(Target::Tag(tag)) {
            Check::Banned(reason) => {
                return Some(ProblemKind::AbilityTagged { name: w.name.clone(), ability: a, tag, reason });
            }
            Check::Allowed => return None,
            Check::Neutral => {}
        }
        match f.check(Target::Tag(Tag::Nonexistent)) {
            Check::Banned(_) => {
                Some(ProblemKind::AbilityDoesNotExist { name: w.name.clone(), ability: a, generation: f.generation })
            }
            _ => None,
        }
    }

    /// `checkNature`: natures are all standard; only explicit bans apply.
    fn check_nature(&self, w: &Work) -> Option<ProblemKind> {
        match self.format.check(Target::Nature(w.nature)) {
            Check::Banned(reason) => Some(ProblemKind::NatureBanned { name: w.name.clone(), nature: w.nature, reason }),
            _ => None,
        }
    }

    /// `checkTagRules` for a species (checked as its tier species), an item
    /// or a move.
    fn check_tag_rules(&self, w: &Work, thing: Thing) -> Option<ProblemKind> {
        let f = &self.format;
        let nonstandard = match thing {
            Thing::Species(s) => self.dex.species(s).nonstandard,
            Thing::Item(i) => self.dex.item(i).nonstandard,
            Thing::Move(m) => self.dex.move_(m).nonstandard,
        };
        let matches = |tag: Tag| match thing {
            Thing::Species(s) => tag.matches_species(self.legality.species(s), nonstandard),
            _ => tag.matches_nonstandard(nonstandard),
        };
        let mut nonexistent = if Tag::Nonexistent.matches_nonstandard(nonstandard) {
            match f.check(Target::Tag(Tag::Nonexistent)) {
                Check::Banned(_) => Nonexistent::Banned,
                Check::Allowed => Nonexistent::Allowed,
                Check::Neutral => Nonexistent::Falsy,
            }
        } else {
            Nonexistent::Falsy
        };
        for (sign, tag) in f.tag_rules() {
            if sign == Sign::Restrict || !matches(tag) {
                continue;
            }
            if sign == Sign::Allow {
                if !tag.is_existence() && nonexistent == Nonexistent::Banned {
                    continue;
                }
                return None;
            }
            if tag.is_existence() {
                nonexistent = Nonexistent::Banned;
                break;
            }
            let reason = match f.check(Target::Tag(tag)) {
                Check::Banned(r) => Some(r),
                _ => None,
            };
            return Some(ProblemKind::Tagged { name: w.name.clone(), thing, tag, reason });
        }
        if nonexistent != Nonexistent::Banned {
            return None;
        }
        let name = w.name.clone();
        Some(match nonstandard {
            Some(Nonstandard::Unobtainable) => {
                let in_gen = match thing {
                    Thing::Move(m) if f.generation >= 9 && self.legality.move_(m).generation < f.generation => {
                        Some(f.generation)
                    }
                    _ => None,
                };
                ProblemKind::Unobtainable { name, thing, in_gen }
            }
            _ => ProblemKind::DoesNotExist { name, thing, generation: f.generation },
        })
    }

    /// `validateStats`.
    fn validate_stats(
        &self,
        w: &mut Work,
        species: SpeciesId,
        set_sources: &mut Sources,
        go_problems: Option<&[GoProblem]>,
    ) -> Vec<ProblemKind> {
        let f = &self.format;
        let stat_points = f.champions_mod;
        let misc = f.obtainable_misc;
        let can_bottle_cap = f.generation >= 7 && (w.level >= if f.generation < 9 { 100 } else { 50 } || !misc);
        let mut problems = Vec::new();
        let name = w.name.clone();
        let maxed = w.ivs.iter().all(|&v| v == 31);
        if stat_points && !maxed {
            problems.push(ProblemKind::IvsNotMaxed { name: name.clone() });
        }
        for &m in &w.moves {
            let ty = self.dex.move_(m).ty;
            if self.legality.move_(m).id == moves::HIDDEN_POWER && ty != Type::Normal {
                match w.hp_type {
                    None => w.hp_type = Some(ty),
                    Some(hp) if hp != ty && misc => {
                        problems.push(ProblemKind::HiddenPowerTypes { name: name.clone(), hp, move_type: ty });
                    }
                    _ => {}
                }
            }
        }
        if let Some(hp) = w.hp_type
            && maxed
            && misc
            && !can_bottle_cap
        {
            w.ivs = hidden_power_ivs(hp);
        }
        if w.hp_type.is_none() && w.moves.iter().any(|&m| self.legality.move_(m).id == moves::HIDDEN_POWER) {
            w.hp_type = Some(hidden_power_type(&w.ivs));
        }
        let data = self.dex.species(species);
        let leg = self.legality.species(species);
        let base = data.base_species;
        let cant_breed_nor_evolve = leg.egg_groups.is_undiscovered() && leg.prevo.is_none() && !data.nfe;
        const NOT_LEGENDARY: [SpeciesId; 11] = [
            species::PIKACHU,
            species::UNOWN,
            species::DRACOZOLT,
            species::ARCTOZOLT,
            species::DRACOVISH,
            species::ARCTOVISH,
            species::GOUGING_FIRE,
            species::RAGING_BOLT,
            species::IRON_BOULDER,
            species::IRON_CROWN,
            species::TERAPAGOS,
        ];
        const LEGENDARY: [SpeciesId; 5] =
            [species::MANAPHY, species::COSMOG, species::COSMOEM, species::SOLGALEO, species::LUNALA];
        let is_legendary =
            (cant_breed_nor_evolve && !leg.tags.contains(SpeciesTags::PARADOX) && !NOT_LEGENDARY.contains(&base))
                || LEGENDARY.contains(&base);
        let diancie_exception = species == species::DIANCIE && !w.shiny;
        let has_3_perfect = set_sources.min_source_gen() >= 6 && is_legendary && !diancie_exception;
        if w.hp_type == Some(Type::Fighting) && misc && has_3_perfect {
            problems.push(ProblemKind::HiddenPowerFightingLegendary { name: name.clone() });
        }
        if has_3_perfect && misc {
            let perfect = w.ivs.iter().filter(|&&v| v >= 31).count();
            if perfect < 3 {
                if go_problems.is_none_or(|g| !g.is_empty()) {
                    let this_gen_only = (f.min_source_gen == 6).then_some(f.generation);
                    problems.push(ProblemKind::LegendaryIvs { name: name.clone(), this_gen_only });
                    if let Some(g) = go_problems.filter(|g| !g.is_empty()) {
                        problems.push(ProblemKind::GoIntro { intro: GoIntro::Additionally, name: name.clone() });
                        problems.extend(g.iter().cloned().map(ProblemKind::Go));
                    }
                } else {
                    set_sources.is_from_pokemon_go = Some(true);
                }
            }
        }
        if let Some(hp) = w.hp_type {
            if !can_bottle_cap {
                let iv_hp = hidden_power_type(&w.ivs);
                if hp != iv_hp {
                    problems.push(ProblemKind::HiddenPowerIvs { name: name.clone(), hp, iv_hp });
                }
            } else if !possible_bottle_cap_hp_type(hp, &w.ivs) {
                problems.push(ProblemKind::HiddenPowerBottleCap { name: name.clone(), hp });
            }
        }
        for (i, &stat) in Stat::ALL.iter().enumerate() {
            let limit = if stat_points { 32 } else { 255 };
            if w.evs[i] > limit {
                problems.push(ProblemKind::StatTooHigh { name: name.clone(), stat, stat_points });
            }
        }
        let total: u32 = w.evs.iter().map(|&e| e as u32).sum();
        let ev_limit = f.ev_limit;
        if w.level > 1 && ev_limit != Some(0) && total == 0 {
            if stat_points {
                if w.nature == Nature::Serious {
                    problems.push(ProblemKind::ZeroStatPoints { name: name.clone() });
                }
            } else {
                problems.push(ProblemKind::ZeroEvs { name: name.clone() });
            }
        } else if !matches!(ev_limit, Some(508 | 510)) && matches!(total, 508 | 510) {
            problems.push(ProblemKind::Exactly510 { name: name.clone(), total });
        }
        // (Under Adjust Level = 50, level 50 was moved to 100 above.)
        if w.level == 50 && f.max_level != 50 && ev_limit != Some(0) && total.is_multiple_of(4) {
            problems.push(ProblemKind::Level50 { name: name.clone(), max_level: f.max_level });
        }
        if let Some(limit) = ev_limit
            && total > limit as u32
        {
            problems.push(if limit == 0 {
                ProblemKind::InvestmentNotAllowed { name, stat_points }
            } else {
                ProblemKind::OverLimit { name, total, limit, stat_points }
            });
        }
        problems
    }

    /// `validatePokemonGo`: `None` if the species can't come from GO,
    /// else the reasons this set can't.
    fn validate_pokemon_go(
        &self,
        s: SpeciesId,
        w: &Work,
        set_sources: &mut Sources,
        name: Option<&str>,
    ) -> Option<Vec<GoProblem>> {
        let leg = self.legality.species(s);
        let mut name = name.unwrap_or(self.sp_name(s)).to_owned();
        let Some(go) = leg.go else {
            let other = leg.changes_from.or(leg.prevo);
            if let (Some(other), 0) = (other, leg.evo_level)
                && let Some(p) = self.validate_pokemon_go(other, w, set_sources, Some(&name))
            {
                return Some(p);
            }
            set_sources.is_from_pokemon_go = Some(false);
            return None;
        };
        let mut problems = Vec::new();
        let (mut min_level, mut min_ivs) = (50u8, 15u8);
        if w.shiny {
            name = format!("Shiny {name}");
        }
        if w.shiny && go.contains(GoSources::NO_SHINY) {
            problems.push(GoProblem::NotObtainable(name));
            return Some(problems);
        }
        if go.contains(GoSources::WILD) {
            min_level = 1;
            min_ivs = 0;
        }
        if go.contains(GoSources::EGG) {
            min_level = min_level.min(2);
            min_ivs = min_ivs.min(10);
        }
        if go.contains(GoSources::EGG_12KM) {
            min_level = min_level.min(8);
            min_ivs = min_ivs.min(10);
        }
        if go.contains(GoSources::RAID) {
            min_level = min_level.min(20);
            min_ivs = min_ivs.min(10);
        }
        if s == species::MEWTWO && w.level >= 20 {
            min_ivs = 0;
        }
        if go.contains(GoSources::RESEARCH) {
            min_level = min_level.min(if s == species::CRESSELIA { 10 } else { 15 });
            min_ivs = min_ivs.min(10);
        }
        if go.contains(GoSources::GIOVANNI) && !w.shiny {
            min_level = min_level.min(8);
            min_ivs = min_ivs.min(1);
            if w.level < 12 {
                set_sources.pokemon_go_source_purified = true;
            }
        }
        if !go.contains(GoSources::NO_TRADE) && (!w.shiny || s != species::DEOXYS) {
            let special_trade = go.contains(GoSources::SPECIAL_TRADE) || w.shiny;
            min_level = min_level.min(12);
            min_ivs = min_ivs.min(if special_trade { 1 } else { 0 });
        }
        if w.level < min_level as u16 {
            problems.push(GoProblem::Level(name.clone(), min_level));
        }
        let ivs = &w.ivs;
        let post_transfer = min_ivs * 2 + 1;
        let mut too_low = false;
        let mut even = false;
        for (i, &v) in ivs.iter().enumerate() {
            if i == 5 {
                continue;
            }
            if v < post_transfer {
                too_low = true;
            }
            if v % 2 == 0 {
                even = true;
            }
        }
        if too_low {
            problems.push(GoProblem::MinIvs(name.clone(), post_transfer));
        }
        if even {
            problems.push(GoProblem::OddIvs(name.clone()));
        }
        let can_bottle_cap =
            self.format.generation >= 7 && w.level >= if self.format.generation < 9 { 100 } else { 50 };
        let (atk, def, spa, spd) = (ivs[1], ivs[2], ivs[3], ivs[4]);
        if atk != spa && !(can_bottle_cap && (atk == 31 || spa == 31)) {
            problems.push(GoProblem::AtkSpa(name.clone()));
        }
        if def != spd && !(can_bottle_cap && (def == 31 || spd == 31)) {
            problems.push(GoProblem::DefSpd(name));
        }
        Some(problems)
    }

    /// `validateMoves`.
    fn validate_moves(&self, s: SpeciesId, w: &Work, set_sources: &mut Sources, name: &str) -> Vec<ProblemKind> {
        let mut problems = Vec::new();
        for &m in &w.moves {
            if let Some(why) = self.check_can_learn(m, s, set_sources, w.level) {
                problems.push(ProblemKind::CantLearn { name: name.to_owned(), why });
                break;
            }
        }
        if set_sources.size() != 0 && set_sources.move_evo_carry_count > 3 {
            if set_sources.sources_before < 6 {
                set_sources.sources_before = 0;
            }
            set_sources.sources.retain(|src| src.is_egg() && src.generation >= 6);
            if set_sources.size() == 0 {
                let leg = self.legality.species(s);
                problems.push(ProblemKind::EvoMoveCarry {
                    name: name.to_owned(),
                    evo_move: leg.evo_move,
                    prevo: leg.prevo,
                });
            }
        }
        if !problems.is_empty() {
            return problems;
        }
        if set_sources.is_hidden == Some(true) {
            set_sources.sources.retain(|src| src.generation >= 5);
            if set_sources.sources_before < 5 {
                set_sources.sources_before = 0;
            }
            // (Gen 8+ has the Ability Patch: no problem here.)
        }
        if let Some(baby) = set_sources.baby_only
            && !set_sources.sources.is_empty()
        {
            let baby_evo = self.legality.species(baby).first_evo;
            set_sources.sources.retain(|src| match src.kind {
                SourceKind::Event(_) => src.species == Some(baby),
                SourceKind::Egg | SourceKind::DreamWorld => !(baby_evo.is_some() && src.species == baby_evo),
                SourceKind::Transfer => true,
            });
            if set_sources.size() == 0 {
                problems.push(ProblemKind::BabyOnlyEvolution { name: name.to_owned(), baby });
            }
        }
        if let Some(baby) = set_sources.baby_only
            && set_sources.size() != 0
            && self.format.generation > 2
        {
            let baby_gen = self.legality.species(baby).generation;
            set_sources
                .sources
                .retain(|src| !(baby_gen > src.generation || (baby_gen > 2 && *src == Source::transfer(7))));
            if set_sources.sources_before < baby_gen {
                set_sources.sources_before = 0;
            }
            if set_sources.size() == 0 {
                problems.push(ProblemKind::BabyOnlyGen { name: name.to_owned(), baby, generation: baby_gen });
            }
        }
        problems
    }

    /// `validateSource`: `Ok` if the set can come from `source`.
    fn validate_source(
        &self,
        w: &mut Work,
        source: Source,
        set_sources: &Sources,
        s: SpeciesId,
        because: Option<Because>,
    ) -> Result<(), Vec<ProblemKind>> {
        let (event, event_species, from) = match source.kind {
            SourceKind::Event(i) => {
                let event_species = source.species.expect("event sources name their species");
                let Some(&event) = self.legality.species(event_species).events.get(i as usize) else {
                    // Showdown throws: an event source without event data.
                    return Err(Vec::new());
                };
                (event, event_species, From::AnEvent)
            }
            SourceKind::Transfer if source.generation == 7 => {
                let is_mew = s == species::MEW;
                let is_celebi = s == species::CELEBI;
                let mut e = EventInfo::blank(2);
                e.level = if is_mew {
                    5
                } else if is_celebi {
                    30
                } else {
                    3
                };
                e.perfect_ivs = if is_mew || is_celebi { 5 } else { 3 };
                e.shiny = if is_mew { EventShiny::Never } else { EventShiny::Maybe };
                (e, s, From::VirtualConsole)
            }
            SourceKind::Transfer => {
                let is_mew = s == species::MEW;
                let mut e = EventInfo::blank(8);
                e.perfect_ivs = if is_mew { 3 } else { 0 };
                e.shiny = if is_mew { EventShiny::Never } else { EventShiny::Maybe };
                (e, s, From::LetsGo)
            }
            SourceKind::DreamWorld => {
                let mut e = EventInfo::blank(5);
                e.level = 10;
                (e, s, From::DreamWorld)
            }
            SourceKind::Egg => {
                let mut blacklist = Vec::new();
                return if self.find_egg_move_fathers(source, s, set_sources, &mut blacklist, false) {
                    Ok(())
                } else {
                    Err(Vec::new())
                };
            }
        };
        self.validate_event(w, set_sources, &event, event_species, because, from)
    }

    /// `validateEvent`. Without `because` it stops at the first problem
    /// (Showdown's `fastReturn`) and returns no messages.
    fn validate_event(
        &self,
        w: &mut Work,
        set_sources: &Sources,
        event: &EventInfo,
        _event_species: SpeciesId,
        because: Option<Because>,
        from: From,
    ) -> Result<(), Vec<ProblemKind>> {
        let f = &self.format;
        let fast = because.is_none();
        let current = w.species;
        let name = if w.name != self.sp_name(current) && self.sp_name(self.base(current)) != w.name {
            format!("{} ({})", w.name, self.sp_name(current))
        } else {
            self.sp_name(current).to_owned()
        };
        let mut problems = Vec::new();
        macro_rules! fail {
            ($p:expr) => {{
                if fast {
                    return Err(Vec::new());
                }
                problems.push(ProblemKind::Event { name: name.clone(), problem: $p, because, from });
            }};
        }
        // (The minimum-source-generation check before this needs Gen 7 or
        // earlier; no event is from a generation after the format's.)
        if event.level != 0 && w.level < event.level as u16 {
            fail!(EventProblem::Level(event.level));
        }
        let shiny_problem =
            (event.shiny == EventShiny::Always && !w.shiny) || (event.shiny == EventShiny::Never && w.shiny);
        if shiny_problem {
            fail!(EventProblem::Shiny(event.shiny == EventShiny::Always));
        }
        if let Some(g) = event.gender {
            let want = if g == EventGender::Male { SetGender::M } else { SetGender::F };
            if w.gender.is_some_and(|have| have != want) {
                fail!(EventProblem::Gender(g));
            }
        }
        // (Natures can be changed with Mints from Gen 8.)
        let mut required_ivs = 0;
        if let Some(ivs) = event.ivs {
            let can_bottle_cap = f.generation >= 7 && w.level >= if f.generation < 9 { 100 } else { 50 };
            for (i, &v) in ivs.iter().enumerate() {
                if v == EventInfo::NO_IV {
                    continue;
                }
                if can_bottle_cap && w.ivs[i] == 31 {
                    continue;
                }
                if w.ivs[i] != v {
                    fail!(EventProblem::Iv(Stat::ALL[i], v));
                }
            }
            if can_bottle_cap && ivs.iter().all(|&v| v != EventInfo::NO_IV) {
                let required = hidden_power_type(&ivs);
                if w.hp_type.is_some_and(|t| t != required) {
                    fail!(EventProblem::HiddenPower(required));
                }
                w.hp_type = Some(required);
            }
        } else {
            required_ivs = event.perfect_ivs;
        }
        if required_ivs != 0 {
            let perfect = w.ivs.iter().filter(|&&v| v >= 31).count();
            if perfect < required_ivs as usize {
                if fast {
                    return Err(Vec::new());
                }
                if event.perfect_ivs != 0 {
                    problems.push(ProblemKind::Event {
                        name: name.clone(),
                        problem: EventProblem::PerfectIvs(required_ivs),
                        because,
                        from,
                    });
                }
            }
            if f.generation >= 3 && required_ivs >= 3 && w.hp_type == Some(Type::Fighting) {
                fail!(EventProblem::HiddenPowerFighting);
            } else if f.generation >= 3
                && required_ivs >= 5
                && w.hp_type
                    .is_some_and(|t| !matches!(t, Type::Dark | Type::Dragon | Type::Electric | Type::Steel | Type::Ice))
            {
                fail!(EventProblem::HiddenPowerFivePerfect);
            }
        }
        if f.obtainable_moves {
            let max = set_sources.max_source_gen();
            if max != 0 && event.generation > max {
                fail!(EventProblem::MovesOnlyGen(max));
            }
            if from == From::DreamWorld && set_sources.dream_world_move_count > 1 {
                problems.push(ProblemKind::Event {
                    name: name.clone(),
                    problem: EventProblem::OneDreamWorld,
                    because,
                    from,
                });
            }
        }
        // (The ability checks need Gen 8 or earlier.)
        if !problems.is_empty() {
            return Err(problems);
        }
        if let Some(g) = event.gender {
            w.gender = Some(if g == EventGender::Male { SetGender::M } else { SetGender::F });
        }
        Ok(())
    }

    /// `getEventOnlyData`.
    fn event_only_data(&self, s: SpeciesId, no_recurse: bool) -> Option<(SpeciesId, &'static [EventInfo])> {
        let leg = self.legality.species(s);
        if !leg.event_only {
            if no_recurse {
                return None;
            }
            return self.event_only_data(leg.prevo?, true);
        }
        if leg.events.is_empty() && !self.dex.species(s).forme.is_empty() {
            return self.event_only_data(self.base(s), true);
        }
        Some((s, leg.events))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_power_ivs_give_their_type() {
        for t in Type::ALL {
            if matches!(t, Type::Normal | Type::Fairy) {
                continue;
            }
            assert_eq!(hidden_power_type(&hidden_power_ivs(t)), t, "{t:?}");
        }
        assert_eq!(hidden_power_type(&[31; 6]), Type::Dark);
    }
}
