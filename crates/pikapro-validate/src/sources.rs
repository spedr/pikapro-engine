//! Showdown's `PokemonSources`: the ways a set could have been obtained,
//! narrowed move by move (`sim/team-validator.ts`).
//!
//! Showdown keeps sources as strings (`"9E"`, `"8S0 pikachu"`, `"5Dmew"`,
//! `"8V"`); [`Source`] holds the same information and compares like the
//! strings do. Level-up, TM and tutor sources never become set sources: they
//! only raise `sourcesBefore` ("anything from this generation or earlier").

use pikapro_data::{MoveId, SpeciesId};

/// How a restrictive source obtains the Pokémon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// `E`: bred with the move.
    Egg,
    /// `S<n>`: event `n` of the species.
    Event(u8),
    /// `D`: Dream World.
    DreamWorld,
    /// `V`: Virtual Console (`7V`) or Let's Go (`8V`) transfer.
    Transfer,
}

/// One set source (a Showdown source string).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Source {
    pub generation: u8,
    pub kind: SourceKind,
    /// `E`: the learnset species if it has a prevolution (`"9Egabite"`),
    /// else `None` (`"9E"`); `S`, `D`: the species (`"8S0 pikachu"`).
    pub species: Option<SpeciesId>,
}

impl Source {
    pub const fn egg(generation: u8, species: Option<SpeciesId>) -> Source {
        Source { generation, kind: SourceKind::Egg, species }
    }

    pub const fn event(generation: u8, index: u8, species: SpeciesId) -> Source {
        Source { generation, kind: SourceKind::Event(index), species: Some(species) }
    }

    pub const fn transfer(generation: u8) -> Source {
        Source { generation, kind: SourceKind::Transfer, species: None }
    }

    pub const fn is_egg(self) -> bool {
        matches!(self.kind, SourceKind::Egg)
    }

    pub const fn is_event(self) -> bool {
        matches!(self.kind, SourceKind::Event(_))
    }
}

/// A JavaScript value that is `undefined`, `null` or set; Showdown tells
/// the first two apart in a few places.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Js<T> {
    #[default]
    Undefined,
    Null,
    Some(T),
}

impl<T> Js<T> {
    pub fn truthy(&self) -> bool {
        matches!(self, Js::Some(_))
    }

    pub fn as_ref(&self) -> Option<&T> {
        match self {
            Js::Some(v) => Some(v),
            _ => None,
        }
    }
}

/// The `limitedEggMove` argument of `PokemonSources.add`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limited {
    Undefined,
    Null,
    Move(MoveId),
}

/// Showdown's `PokemonSources`.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    /// Restrictive sources, in order (duplicates possible, as in Showdown).
    pub sources: Vec<Source>,
    /// Any source up to this generation (0: none).
    pub sources_before: u8,
    /// The earliest generation any source may be from.
    pub sources_after: u8,
    /// `isHidden`: `None` is `null`.
    pub is_hidden: Option<bool>,
    pub limited_egg_moves: Js<Vec<MoveId>>,
    /// `possiblyLimitedEggMoves`: Showdown's `toID(gen + move)`.
    pub possibly_limited_egg_moves: Option<Vec<(u8, MoveId)>>,
    pub level_up_egg_moves: Option<Vec<MoveId>>,
    pub pomeg_egg_moves: Option<Vec<MoveId>>,
    pub pomeg_event_egg: Js<Source>,
    pub event_only_min_source_gen: u8,
    pub learnset_domain: Option<Vec<(u8, SpeciesId)>>,
    pub restrictive_moves: Option<Vec<MoveId>>,
    pub restricted_move: Option<MoveId>,
    pub is_from_pokemon_go: Option<bool>,
    pub pokemon_go_source_purified: bool,
    /// `goIncompatibleMoves` (Showdown pushes names; an empty name is
    /// never pushed).
    pub go_incompatible_moves: Option<Vec<MoveId>>,
    pub baby_only: Option<SpeciesId>,
    pub sketch_move: Option<MoveId>,
    pub hm: Option<MoveId>,
    pub move_evo_carry_count: u32,
    pub dream_world_move_count: u32,
}

impl Sources {
    pub fn new(sources_before: u8, sources_after: u8) -> Sources {
        Sources { sources_before, sources_after, ..Sources::default() }
    }

    /// `size()`: `usize::MAX` stands for `Infinity`.
    pub fn size(&self) -> usize {
        if self.sources_before != 0 { usize::MAX } else { self.sources.len() }
    }

    pub fn add(&mut self, source: Source, limited: Limited) {
        if self.sources.last() != Some(&source) {
            self.sources.push(source);
        }
        // (The `1ET` tradeback case needs a Gen 1-2 format.)
        match limited {
            Limited::Move(m) if self.limited_egg_moves != Js::Null => self.limited_egg_moves = Js::Some(vec![m]),
            Limited::Null => self.limited_egg_moves = Js::Null,
            _ => {}
        }
    }

    pub fn add_gen(&mut self, generation: u8) {
        self.sources_before = self.sources_before.max(generation);
        self.limited_egg_moves = Js::Null;
    }

    pub fn min_source_gen(&self) -> u8 {
        if self.event_only_min_source_gen != 0 {
            return self.event_only_min_source_gen;
        }
        if self.sources_before != 0 {
            return if self.sources_after != 0 { self.sources_after } else { 1 };
        }
        self.sources.iter().map(|s| s.generation).min().unwrap_or(0)
    }

    pub fn max_source_gen(&self) -> u8 {
        self.sources.iter().map(|s| s.generation).fold(self.sources_before, u8::max)
    }

    /// `addGoIncompatibleMove`.
    pub fn add_go_incompatible_move(&mut self, m: Option<MoveId>) {
        let list = self.go_incompatible_moves.get_or_insert_with(Vec::new);
        if let Some(m) = m {
            list.push(m);
        }
        self.is_from_pokemon_go = Some(false);
    }

    /// `intersectWith`.
    pub fn intersect_with(&mut self, other: &mut Sources) {
        let is_3e = |s: &Source| s.generation == 3 && s.is_egg();
        if self.pomeg_event_egg.truthy() && other.pomeg_egg_moves.is_some() {
            let egg = *self.pomeg_event_egg.as_ref().expect("truthy");
            for s in &mut other.sources {
                if is_3e(s) {
                    *s = egg;
                }
            }
        } else if other.pomeg_event_egg.truthy() && self.pomeg_event_egg != Js::Null {
            let egg = *other.pomeg_event_egg.as_ref().expect("truthy");
            for s in &mut self.sources {
                if is_3e(s) {
                    *s = egg;
                }
            }
            self.pomeg_event_egg = Js::Some(egg);
        } else if other.pomeg_egg_moves.is_none() && other.sources_before == 0 {
            self.pomeg_event_egg = Js::Null;
        }

        if other.sources_before != 0 || self.sources_before != 0 {
            if other.sources_before > self.sources_before {
                for s in &self.sources {
                    if s.generation <= other.sources_before {
                        other.sources.push(*s);
                    }
                }
            } else if self.sources_before > other.sources_before {
                for s in &other.sources {
                    if s.generation <= self.sources_before {
                        self.sources.push(*s);
                    }
                }
            }
            self.sources_before = self.sources_before.min(other.sources_before);
        }
        if !self.sources.is_empty() {
            if !other.sources.is_empty() {
                self.sources.retain(|s| other.sources.contains(s));
            } else {
                self.sources.clear();
            }
        }

        if other.restricted_move.is_some() && other.restricted_move != self.restricted_move {
            if self.restricted_move.is_some() {
                self.sources.clear();
                self.sources_before = 0;
            } else {
                self.restricted_move = other.restricted_move;
            }
        }

        if let Js::Some(other_moves) = &other.limited_egg_moves {
            match &mut self.limited_egg_moves {
                Js::Some(moves) => moves.extend_from_slice(other_moves),
                _ => self.limited_egg_moves = Js::Some(other_moves.clone()),
            }
        }
        merge(&mut self.possibly_limited_egg_moves, &other.possibly_limited_egg_moves);
        merge(&mut self.level_up_egg_moves, &other.level_up_egg_moves);
        merge(&mut self.pomeg_egg_moves, &other.pomeg_egg_moves);
        // Showdown filters `learnsetDomain` here but drops the result, so
        // the first domain set is kept.
        if self.learnset_domain.is_none() && other.learnset_domain.is_some() {
            self.learnset_domain.clone_from(&other.learnset_domain);
        }

        let min_egg_gen = self.sources.iter().filter(|s| s.is_egg()).map(|s| s.generation).min();
        if let (Some(possibly), 0, Some(min_egg_gen)) =
            (&self.possibly_limited_egg_moves, self.sources_before, min_egg_gen)
        {
            for &(generation, m) in possibly {
                if !self.limited_egg_moves.truthy() {
                    self.limited_egg_moves = Js::Some(Vec::new());
                }
                if let Js::Some(limited) = &mut self.limited_egg_moves
                    && generation < min_egg_gen
                    && !limited.contains(&m)
                {
                    limited.push(m);
                }
            }
        }

        self.move_evo_carry_count += other.move_evo_carry_count;
        self.dream_world_move_count += other.dream_world_move_count;
        if other.sources_after > self.sources_after {
            self.sources_after = other.sources_after;
        }
        if other.is_hidden == Some(true) {
            self.is_hidden = Some(true);
        }
        if other.sources_before < 8
            && !other.sources.contains(&Source::transfer(8))
            && let Some(last) = self.restrictive_moves.as_ref().map(|r| r.last().copied())
        {
            self.add_go_incompatible_move(last);
        }
    }
}

/// `if (other.x) { if (!this.x) this.x = other.x; else this.x.push(...other.x); }`
fn merge<T: Clone>(this: &mut Option<Vec<T>>, other: &Option<Vec<T>>) {
    if let Some(o) = other {
        match this {
            Some(t) => t.extend_from_slice(o),
            None => *this = Some(o.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersection_keeps_shared_sources() {
        let s = SpeciesId(1);
        let mut set = Sources::new(9, 9);
        let mut a = Sources::new(0, 0);
        a.add(Source::event(9, 0, s), Limited::Undefined);
        a.add(Source::event(9, 1, s), Limited::Undefined);
        set.intersect_with(&mut a);
        assert_eq!(set.sources_before, 0);
        assert_eq!(set.sources, vec![Source::event(9, 0, s), Source::event(9, 1, s)]);
        let mut b = Sources::new(0, 0);
        b.add(Source::event(9, 1, s), Limited::Undefined);
        set.intersect_with(&mut b);
        assert_eq!(set.sources, vec![Source::event(9, 1, s)]);
        assert_eq!(set.size(), 1);
    }

    #[test]
    fn generations_widen_and_null_limited_egg_moves() {
        let mut s = Sources::new(0, 0);
        s.add(Source::egg(5, None), Limited::Move(MoveId(3)));
        assert_eq!(s.limited_egg_moves, Js::Some(vec![MoveId(3)]));
        s.add_gen(8);
        assert_eq!(s.limited_egg_moves, Js::Null);
        assert_eq!(s.size(), usize::MAX);
        assert_eq!(s.max_source_gen(), 8);
        assert_eq!(s.min_source_gen(), 1);
    }
}
