//! Learnset checks: Showdown's `checkCanLearn`, `findEggMoveFathers`,
//! `fatherCanLearn`, `motherCanLearn` (`sim/team-validator.ts`) and the
//! species helpers they use (`getFullLearnset`, `learnsetParent`,
//! `eggMovesOnly` in `sim/dex-species.ts`).
//!
//! Paths that need a Gen 1-2 format (tradebacks), a Gen 3-5 format's
//! ability checks, or rules these formats don't have (Pomeg glitch, Mimic
//! glitch, Sketch Post-Gen 7 Moves) are left out and marked.

use pikapro_data::gen9::{moves, species};
use pikapro_data::legality::{EggGroups, EvoRegion, LearnSource, Learnset, SourceKind as Letter};
use pikapro_data::{GenderRule, MoveFlags, MoveId, SpeciesId, Type};

use crate::Validator;
use crate::problem::CantLearn;
use crate::sources::{Js, Limited, Source, SourceKind, Sources};

/// A learnset source as `checkCanLearn` rewrites it: level-up moves above
/// the set's level become `Eany` (bred from a mother who knows it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Learned {
    Source(LearnSource),
    EggAny(u8),
}

impl Learned {
    fn letter(self) -> Letter {
        match self {
            Learned::Source(s) => s.kind(),
            Learned::EggAny(_) => Letter::Egg,
        }
    }
}

/// HM moves that can't be transferred out of Gen 3 (`checkCanLearn`).
const HM_GEN3: [MoveId; 8] = [
    moves::CUT,
    moves::FLY,
    moves::SURF,
    moves::STRENGTH,
    moves::FLASH,
    moves::ROCK_SMASH,
    moves::WATERFALL,
    moves::DIVE,
];
/// HM moves that can't be transferred out of Gen 4.
const HM_GEN4: [MoveId; 7] =
    [moves::CUT, moves::FLY, moves::SURF, moves::STRENGTH, moves::ROCK_SMASH, moves::WATERFALL, moves::ROCK_CLIMB];

impl Validator {
    pub(crate) fn base(&self, s: SpeciesId) -> SpeciesId {
        self.dex.species(s).base_species
    }

    fn learnset(&self, s: SpeciesId) -> Option<Learnset> {
        self.legality.species(s).learnset
    }

    /// `dex.species.learnsetParent`.
    pub(crate) fn learnset_parent(&self, s: SpeciesId, checking_moves: bool) -> Option<SpeciesId> {
        let data = self.dex.species(s);
        let leg = self.legality.species(s);
        if leg.learnset.is_none() && !data.forme.is_empty() {
            return Some(leg.changes_from.unwrap_or(data.base_species));
        }
        if let Some(prevo) = leg.prevo {
            if self.format.champions_mod {
                return None;
            }
            if self.legality.species(prevo).generation > self.format.generation.max(2) {
                return None;
            }
            return Some(prevo);
        }
        if let Some(from) = leg.changes_from
            && data.base_species != species::KYUREM
        {
            return Some(from);
        }
        if checking_moves && self.legality.species(data.base_species).prevo.is_some() {
            let mut evo = data.base_species;
            while let Some(p) = self.legality.species(evo).prevo {
                evo = p;
            }
            return Some(evo);
        }
        None
    }

    /// `dex.species.getFullLearnset`: the learnsets a species can use, its
    /// own first.
    pub fn full_learnset(&self, id: SpeciesId) -> Vec<(SpeciesId, Learnset)> {
        let mut out = Vec::new();
        let mut checked: Vec<SpeciesId> = Vec::new();
        let mut current = Some(id);
        while let Some(s) = current {
            if checked.contains(&s) {
                break;
            }
            checked.push(s);
            if let Some(ls) = self.learnset(s) {
                out.push((s, ls));
                current = self.learnset_parent(s, true);
                continue;
            }
            let data = self.dex.species(s);
            let next = self.legality.species(s).changes_from.unwrap_or(data.base_species);
            if next != s {
                current = Some(next);
                continue;
            }
            if data.nonstandard.is_some() {
                return out;
            }
            match self.legality.species(s).prevo {
                Some(p) if self.learnset(p).is_some() => current = Some(p),
                // Showdown throws "Species with no learnset data".
                _ => break,
            }
        }
        out
    }

    /// `dex.species.eggMovesOnly(child, father)`.
    fn egg_moves_only(&self, child: SpeciesId, father: SpeciesId) -> bool {
        if self.base(child) == self.base(father) {
            return false;
        }
        let mut f = Some(father);
        while let Some(s) = f {
            if s == child {
                return false;
            }
            f = self.learnset_parent(s, false);
        }
        true
    }

    /// `motherCanLearn`.
    fn mother_can_learn(&self, mother: Option<SpeciesId>, move_id: MoveId) -> bool {
        let Some(mother) = mother else { return false };
        self.full_learnset(mother).iter().any(|(_, ls)| ls.get(move_id).is_some())
    }

    fn gender(&self, s: SpeciesId) -> GenderRule {
        self.dex.species(s).gender
    }

    /// `checkCanLearn`: `None` if the species can learn `mv` given the set's
    /// other moves (narrowing `set_sources`), else why not.
    pub(crate) fn check_can_learn(
        &self,
        mv: MoveId,
        original: SpeciesId,
        set_sources: &mut Sources,
        level: u16,
    ) -> Option<CantLearn> {
        let dex = self.dex;
        let dex_gen = self.format.generation;
        let move_id = self.legality.move_(mv).id;
        let move_data = dex.move_(mv);
        let level = if level == 0 { 100 } else { level };
        let mut can_learn_species: Vec<SpeciesId> = Vec::new();
        let mut cant_learn_reason: Option<CantLearn> = None;
        let mut limit1 = true;
        let mut sketch = false;
        let mut blocked_hm = false;
        let mut baby_only: Option<SpeciesId> = None;
        let mut min_learn_gen = dex_gen;
        let mut move_sources = Sources::new(0, 0);
        let min_source_gen = self.format.min_source_gen;
        let full = self.full_learnset(original);
        if full.is_empty() {
            return Some(CantLearn::NoLearnset);
        }
        for &(sp, learnset) in &full {
            let sp_leg = self.legality.species(sp);
            let checking_prevo = self.base(sp) != self.base(original);
            if checking_prevo && move_sources.size() == 0 && (set_sources.baby_only.is_none() || sp_leg.prevo.is_none())
            {
                baby_only = Some(sp);
            }
            let forme_cant_inherit = self.egg_moves_only(sp, original);
            if forme_cant_inherit && dex_gen < 9 {
                break;
            }
            let entry = learnset.get(move_id);
            let mut sources: Vec<LearnSource> =
                entry.map(|e| e.sources.iter().map(|&s| LearnSource(s)).collect()).unwrap_or_default();
            if move_id == moves::SKETCH {
                sketch = true;
            } else if let Some(sk) = learnset.get(moves::SKETCH) {
                if move_data.flags.contains(MoveFlags::NO_SKETCH) {
                    cant_learn_reason = Some(CantLearn::CantSketch(mv));
                } else {
                    // (Showdown's post-Gen 7 Sketch rule only applies to
                    // Gen 8 and the pre-DLC Gen 9 mods.)
                    if sources.is_empty() || move_sources.size() == 0 {
                        sketch = true;
                    }
                    let mut all: Vec<LearnSource> = sk.sources.iter().map(|&s| LearnSource(s)).collect();
                    all.extend(sources);
                    sources = all;
                }
            }
            let mut can_use_home_relearner = false;
            for source in sources {
                let learned_gen = source.generation();
                let letter = source.kind();
                if forme_cant_inherit && (letter != Letter::Egg || learned_gen < 9) {
                    continue;
                }
                if let Some(domain) = &set_sources.learnset_domain
                    && !domain.contains(&(learned_gen, self.base(sp)))
                    && (letter != Letter::Egg || learned_gen < 8)
                {
                    if cant_learn_reason.is_none() {
                        let restrictive = set_sources.restrictive_moves.clone().unwrap_or_default();
                        cant_learn_reason = Some(CantLearn::IncompatibleWith(mv, restrictive));
                    }
                    continue;
                }
                if learned_gen < min_source_gen && !can_use_home_relearner {
                    if cant_learn_reason.is_none() {
                        cant_learn_reason = Some(CantLearn::Transfer(mv, learned_gen, min_source_gen));
                    }
                    continue;
                }
                if learned_gen > dex_gen {
                    if cant_learn_reason.is_none() {
                        cant_learn_reason = Some(CantLearn::Transfer(mv, learned_gen, dex_gen));
                    }
                    continue;
                }
                if learned_gen == 9 && letter != Letter::Event {
                    can_use_home_relearner = true;
                }
                if self.legality.species(original).evo_region == Some(EvoRegion::Alola)
                    && checking_prevo
                    && learned_gen >= 8
                    && (dex_gen < 9 || letter != Letter::Egg)
                {
                    cant_learn_reason = Some(CantLearn::Usum(mv, sp, original));
                    continue;
                }
                // (Hidden Ability and ability-generation checks: Gen 8+
                // has the Ability Patch, and `dex.gen < 6` only.)
                if dex_gen >= 4 && learned_gen <= 3 && HM_GEN3.contains(&move_id) {
                    cant_learn_reason = Some(CantLearn::Hm(mv, 3, 4));
                    continue;
                }
                if dex_gen >= 5 && learned_gen <= 4 && HM_GEN4.contains(&move_id) {
                    cant_learn_reason = Some(CantLearn::Hm(mv, 4, 5));
                    continue;
                }
                if dex_gen >= 5 && (move_id == moves::DEFOG || move_id == moves::WHIRLPOOL) && learned_gen <= 4 {
                    blocked_hm = true;
                }
                let mut learned = Learned::Source(source);
                if letter == Letter::LevelUp {
                    let at = source.value();
                    if level >= at || learned_gen == 7 {
                    } else if self.gender(sp) != GenderRule::Genderless && learned_gen >= 2 && sp_leg.can_hatch {
                        if self.gender(sp) == GenderRule::Male && !self.mother_can_learn(sp_leg.mother, move_id) {
                            cant_learn_reason = Some(CantLearn::Level(mv, at));
                            continue;
                        }
                        learned = Learned::EggAny(learned_gen);
                    } else {
                        cant_learn_reason = Some(CantLearn::Level(mv, at));
                        continue;
                    }
                }
                let letter = learned.letter();
                let egg_not_any = letter == Letter::Egg && matches!(learned, Learned::Source(_));
                if (learned_gen >= 8 && egg_not_any)
                    || matches!(letter, Letter::LevelUp | Letter::Machine | Letter::Tutor | Letter::Restricted)
                {
                    if learned_gen == dex_gen && letter != Letter::Restricted {
                        if !(learned_gen >= 8 && letter == Letter::Egg) && baby_only.is_some() && sp_leg.evo_level != 0
                        {
                            set_sources.add_go_incompatible_move(Some(mv));
                        }
                        if move_sources.move_evo_carry_count == 0 && baby_only.is_none() {
                            return None;
                        }
                    }
                    if letter == Letter::Restricted {
                        move_sources.restricted_move = Some(move_id);
                    }
                    limit1 = false;
                    move_sources.add_gen(learned_gen);
                } else if letter == Letter::Egg {
                    let mut limited = Limited::Undefined;
                    if matches!(learned, Learned::EggAny(_)) {
                        if self.gender(sp) == GenderRule::Female {
                            limited = Limited::Move(move_id);
                            move_sources.level_up_egg_moves = Some(vec![move_id]);
                        } else {
                            limited = Limited::Null;
                        }
                    } else if learned_gen < 6
                        || (sp_leg.mother.is_some() && !self.mother_can_learn(sp_leg.mother, move_id))
                    {
                        limited = Limited::Move(move_id);
                    }
                    let egg = Source::egg(learned_gen, sp_leg.prevo.map(|_| sp));
                    // (The `1ET` tradeback source needs a Gen 2 format.)
                    move_sources.add(egg, limited);
                } else if letter == Letter::Event {
                    let index = source.value() as u8;
                    let event = Source::event(learned_gen, index, sp);
                    move_sources.add(event, Limited::Undefined);
                    // Showdown reads the index's first digit only.
                    let first_digit = index.to_string().as_bytes()[0] - b'0';
                    let emerald = sp_leg.events.get(first_digit as usize).is_some_and(|e| e.emerald_event_egg);
                    if emerald && learned_gen == 3 {
                        move_sources.pomeg_event_egg = Js::Some(event);
                    }
                } else if letter == Letter::DreamWorld {
                    move_sources.add(
                        Source { generation: learned_gen, kind: SourceKind::DreamWorld, species: Some(sp) },
                        Limited::Undefined,
                    );
                    if move_sources.sources_before == 0 {
                        move_sources.dream_world_move_count += 1;
                    }
                } else if letter == Letter::Transfer && min_source_gen < learned_gen {
                    if learned_gen == 8 && baby_only.is_some() && sp_leg.evo_level != 0 {
                        set_sources.add_go_incompatible_move(Some(mv));
                    }
                    move_sources.add(Source::transfer(learned_gen), Limited::Undefined);
                }
                if letter == Letter::Egg && learned_gen >= 8 && !can_learn_species.contains(&self.base(original)) {
                    can_learn_species.push(self.base(original));
                }
                if !can_learn_species.contains(&self.base(sp)) {
                    can_learn_species.push(self.base(sp));
                }
                min_learn_gen = min_learn_gen.min(learned_gen);
            }
            if can_use_home_relearner
                && let Some(home) = entry.and_then(|e| e.home_level)
                && level >= home as u16
            {
                return None;
            }
            // (Mimic Glitch Clause is not in these formats.)
            if move_sources.size() == 0
                && ((sp_leg.evo_move.is_some() && sp_leg.evo_move != Some(mv))
                    || (sp == species::SYLVEON && move_data.ty != Type::Fairy))
            {
                move_sources.move_evo_carry_count = 1;
            }
        }
        if limit1 && sketch {
            if let Some(other) = set_sources.sketch_move {
                return Some(CantLearn::OneSketch(mv, other));
            }
            set_sources.sketch_move = Some(mv);
        }
        if blocked_hm {
            if set_sources.hm.is_some() {
                return Some(CantLearn::DefogWhirlpool);
            }
            set_sources.hm = Some(move_id);
        }
        let restrictive = set_sources.restrictive_moves.get_or_insert_with(Vec::new);
        if !restrictive.contains(&mv) {
            restrictive.push(mv);
        }
        let checked = if baby_only.is_some() { full[full.len() - 1].0 } else { original };
        if set_sources.pokemon_go_source_purified || checked == species::MEW {
            self.lgpe_restrictive_moves(checked, mv, move_id, set_sources, level);
        }
        let mut next = Some(original);
        let mut species_count = 0;
        if self.learnset(original).is_none() {
            let data = self.dex.species(original);
            next = Some(self.legality.species(original).changes_from.unwrap_or(data.base_species));
        }
        while let Some(n) = next {
            let base = self.base(n);
            for g in self.legality.species(n).generation..=dex_gen {
                if can_learn_species.contains(&base)
                    || (0 < species_count && species_count < can_learn_species.len())
                    || (species_count == 0 && g >= min_learn_gen)
                    || (species_count == can_learn_species.len() && g <= move_sources.sources_before)
                {
                    move_sources.learnset_domain.get_or_insert_with(Vec::new).push((g, base));
                }
            }
            if can_learn_species.contains(&n) {
                species_count += 1;
            }
            next = self.legality.species(n).prevo;
        }
        if move_sources.size() == 0 {
            return Some(cant_learn_reason.unwrap_or(CantLearn::Move(mv)));
        }
        let has_egg = move_sources.sources.iter().any(|s| s.is_egg());
        if dex_gen >= 3 && has_egg && move_sources.limited_egg_moves == Js::Null && move_sources.sources_before != 0 {
            move_sources.possibly_limited_egg_moves = Some(vec![(move_sources.sources_before, move_id)]);
        }
        let backup = (set_sources.sources.clone(), set_sources.sources_before);
        set_sources.intersect_with(&mut move_sources);
        if set_sources.size() == 0 {
            (set_sources.sources, set_sources.sources_before) = backup;
            return Some(CantLearn::Incompatible(set_sources.restrictive_moves.clone().unwrap_or_default()));
        }
        if baby_only.is_some() {
            set_sources.baby_only = baby_only;
        }
        None
    }

    /// The `LGPERestrictiveMoves` check of `checkCanLearn` (Mew, and
    /// Pokémon purified from Team GO Rocket).
    fn lgpe_restrictive_moves(&self, checked: SpeciesId, mv: MoveId, move_id: MoveId, ss: &mut Sources, level: u16) {
        let lgpe = self.legality.species(checked).lgpe_restrictive_moves;
        if self.legality.species(checked).go.is_none() || lgpe.is_empty() {
            return;
        }
        let mut level_up_move_count = 0;
        let restrictive: Vec<MoveId> = ss.restrictive_moves.clone().unwrap_or_default();
        for &(restrictive_move, move_level) in lgpe {
            if move_id == restrictive_move && (move_level == 0 || (level != 0 && level < move_level as u16)) {
                ss.add_go_incompatible_move(Some(mv));
            }
            if level_up_move_count != 0 {
                level_up_move_count += 1;
            }
            if restrictive.contains(&restrictive_move) {
                if level_up_move_count == 0 {
                    level_up_move_count += 1;
                } else if level_up_move_count > 4 {
                    ss.add_go_incompatible_move(None);
                }
            }
        }
    }

    /// `findEggMoveFathers` (without `getAll`): whether some father can pass
    /// the set's limited egg moves on.
    pub(crate) fn find_egg_move_fathers(
        &self,
        source: Source,
        sp: SpeciesId,
        set_sources: &Sources,
        blacklist: &mut Vec<SpeciesId>,
        no_recurse: bool,
    ) -> bool {
        if !blacklist.contains(&sp) {
            blacklist.push(sp);
        }
        let egg_gen = source.generation.max(2);
        let sp_leg = self.legality.species(sp);
        if egg_gen >= 6 && set_sources.level_up_egg_moves.is_none() && sp_leg.mother.is_none() {
            return true;
        }
        let mut egg_moves: Vec<MoveId> = match &set_sources.limited_egg_moves {
            Js::Some(m) => m.clone(),
            _ => return true,
        };
        if egg_gen == 3
            && let Some(pomeg) = &set_sources.pomeg_egg_moves
        {
            egg_moves.retain(|m| !pomeg.contains(m));
        }
        if egg_moves.len() <= 1 && set_sources.level_up_egg_moves.is_none() {
            return true;
        }
        if let Some(level_up) = &set_sources.level_up_egg_moves
            && egg_gen >= 6
        {
            egg_moves.clone_from(level_up);
        }
        let mut egg_groups = sp_leg.egg_groups;
        if sp == species::NIDOQUEEN || sp == species::NIDORINA {
            egg_groups = self.legality.species(species::NIDORAN_F).egg_groups;
        } else if sp == species::SHEDINJA {
            egg_groups = self.legality.species(species::NINCADA).egg_groups;
        }
        if egg_groups.is_undiscovered() {
            egg_groups = sp_leg.first_evo.map_or(EggGroups(0), |e| self.legality.species(e).egg_groups);
        }
        if egg_groups.is_undiscovered() || egg_groups.is_empty() {
            // Showdown throws ("has no egg groups"); no Gen 9 source gets here.
            return true;
        }
        let smeargle_available = self.dex.species(species::SMEARGLE).nonstandard.is_none();
        if egg_groups.intersects(EggGroups::FIELD) && smeargle_available {
            return true;
        }
        for i in 0..self.dex.species.len() {
            let father = SpeciesId(i as u16);
            let data = self.dex.species(father);
            let leg = self.legality.species(father);
            if data.nonstandard.is_some()
                || leg.generation > egg_gen
                || matches!(data.gender, GenderRule::Genderless | GenderRule::Female)
                || leg.learnset.is_none()
            {
                continue;
            }
            if blacklist.contains(&father) && father != species::DRAGONITE && father != species::SNORLAX {
                continue;
            }
            if let Some(evo) = leg.first_evo {
                let evolved = self.legality.species(evo);
                if evolved.generation <= egg_gen && self.dex.species(evo).gender != GenderRule::Female {
                    continue;
                }
            }
            if !leg.egg_groups.intersects(egg_groups) {
                continue;
            }
            if !self.father_can_learn(sp, father, &egg_moves, egg_gen, blacklist, no_recurse) {
                continue;
            }
            return true;
        }
        false
    }

    /// `fatherCanLearn`.
    fn father_can_learn(
        &self,
        child: SpeciesId,
        father: SpeciesId,
        egg_moves: &[MoveId],
        egg_gen: u8,
        blacklist: &mut Vec<SpeciesId>,
        no_recurse: bool,
    ) -> bool {
        let father_leg = self.legality.species(father);
        if father_leg.learnset.is_none() {
            return false;
        }
        if father == species::SMEARGLE {
            return true;
        }
        let with_smeargle = father_leg.egg_groups.intersects(EggGroups::FIELD)
            && self.dex.species(species::SMEARGLE).nonstandard.is_none();
        let mut all = Sources::new(egg_gen, 0);
        for &m in egg_moves {
            let mut egg_sources = Sources::new(0, 0);
            for (cur, learnset) in self.full_learnset(father) {
                let egg_pokemon = self.legality.species(cur).prevo.map(|_| cur);
                if let Some(entry) = learnset.get(m) {
                    for &raw in entry.sources {
                        let s = LearnSource(raw);
                        let g = s.generation();
                        if egg_gen > 8 && g <= 8 {
                            continue;
                        }
                        if g > egg_gen {
                            continue;
                        }
                        let letter = s.kind();
                        let from_smeargle = letter == Letter::Egg && with_smeargle;
                        let restrictive =
                            matches!(letter, Letter::Egg | Letter::Event | Letter::DreamWorld | Letter::Transfer);
                        if !restrictive || from_smeargle {
                            egg_sources.add_gen(g);
                            break;
                        }
                        // Showdown concatenates the source and the egg
                        // Pokémon (`"7Egible"`, `"8S0gible"`); these only
                        // meet each other.
                        let kind = match letter {
                            Letter::Egg => SourceKind::Egg,
                            Letter::Event => SourceKind::Event(s.value() as u8),
                            Letter::DreamWorld => SourceKind::DreamWorld,
                            _ => SourceKind::Transfer,
                        };
                        let src = Source { generation: g, kind, species: egg_pokemon };
                        let limited = if letter == Letter::Egg { Limited::Move(m) } else { Limited::Undefined };
                        egg_sources.add(src, limited);
                    }
                }
                if egg_sources.sources_before == egg_gen {
                    break;
                }
            }
            if egg_sources.sources_before == egg_gen {
                continue;
            }
            if egg_sources.sources_before == 0 && egg_sources.sources.is_empty() {
                return false;
            }
            let only_eggs = egg_sources.sources.iter().any(|s| s.is_egg());
            if egg_gen >= 3 && only_eggs && egg_sources.limited_egg_moves == Js::Null && egg_sources.sources_before != 0
            {
                egg_sources.possibly_limited_egg_moves = Some(vec![(egg_sources.sources_before, m)]);
            }
            all.intersect_with(&mut egg_sources);
            if all.size() == 0 {
                return false;
            }
        }
        blacklist.push(father);
        if let Js::Some(limited) = &all.limited_egg_moves
            && limited.len() > 1
        {
            if no_recurse {
                return false;
            }
            let child_groups = self.legality.species(child).egg_groups;
            let can_chainbreed = father_leg.egg_groups.0 & !child_groups.0 != 0;
            if !can_chainbreed && limited.len() == egg_moves.len() {
                return false;
            }
            let mut set_sources = Sources::new(0, 0);
            set_sources.limited_egg_moves = Js::Some(limited.clone());
            // Showdown passes `allEggSources.sources[0]`, and crashes
            // when there is none.
            let Some(&first) = all.sources.first() else { return false };
            return self.find_egg_move_fathers(first, father, &set_sources, blacklist, true);
        }
        true
    }
}
