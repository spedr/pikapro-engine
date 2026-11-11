//! [`OwnTeam`]: what a player knows of its own team besides its request.

use pikapro::battle::move_max_pp;
use pikapro::stats::calc_stat;
use pikapro::util::StackVec;
use pikapro::{Format, Gender, MAX_ACTIVE, MAX_MOVES, MAX_TEAM, Team};
use pikapro_data::{AbilityId, GenderRule, ItemId, MoveId, Nature, SpeciesId, Stat, Type};

use crate::overlay::{OwnActive, OwnMoves, OwnOverlay};

/// One Pokémon of a player's own team ([`OwnTeam`]): its set, and the PP
/// its moves had the last time a request showed them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TeamMon {
    /// Its set's species, as its details show it.
    pub species: SpeciesId,
    pub level: u8,
    /// `None` until a request shows it when the set leaves it to chance.
    pub gender: Option<Gender>,
    /// `None` when the team came from requests alone (they don't show it).
    pub nature: Option<Nature>,
    pub item: Option<ItemId>,
    pub ability: AbilityId,
    pub moves: StackVec<MoveId, MAX_MOVES>,
    /// Each move slot's maximum PP (0 until known).
    pub max_pp: [u8; MAX_MOVES],
    /// Max HP, then Attack through Speed, from the set (what a request
    /// shows for it outside a temporary forme).
    pub stats: [u16; 6],
    /// Its Tera type (the set's, or what the battle gives it).
    pub tera_type: Option<Type>,
    /// Each move slot's PP the last time a move request showed the Pokémon
    /// active (its maximum until then, which the team gives). A benched
    /// Pokémon's PP can't change, but the request before it left doesn't
    /// count the move it used then (U-turn, a move before a drag or an
    /// Eject Button), which no later request shows.
    pub pp: [u8; MAX_MOVES],
    /// Whether the team or a request gave `pp` (else it is unknown: a
    /// session without the team that hasn't seen the Pokémon active).
    pub pp_known: bool,
    /// Per move slot, a move a move request showed there that isn't the
    /// set's (a transformed Pokémon's copies, Mimic's move), with its PP
    /// left and maximum the last time one did: so that an active
    /// Pokémon's PP outside a move request is the last shown for those
    /// moves too.
    pub other_pp: [Option<(MoveId, u8, u8)>; MAX_MOVES],
}

impl Default for TeamMon {
    fn default() -> TeamMon {
        TeamMon {
            species: SpeciesId::default(),
            level: 0,
            gender: None,
            nature: None,
            item: None,
            ability: AbilityId::NONE,
            moves: StackVec::new(),
            max_pp: [0; MAX_MOVES],
            stats: [0; 6],
            tera_type: None,
            pp: [0; MAX_MOVES],
            pp_known: false,
            other_pp: [None; MAX_MOVES],
        }
    }
}

impl TeamMon {
    /// Move slot `k`'s last PP seen ([`TeamMon::pp`]), if known.
    pub fn last_pp(&self, k: usize) -> Option<u8> {
        (self.pp_known && k < self.moves.len()).then(|| self.pp[k])
    }
}

/// A player's own team as it knows it: the sets it built (the nature,
/// which no request shows; the Pokémon left out at team preview, which the
/// requests after it don't list) and the PP its Pokémon had when a request
/// last showed them active. One per player per battle, by identity
/// ([`OwnMon::id`](crate::OwnMon::id), the registration index): `mons[k]`
/// is the Pokémon registered `k`-th.
///
/// Build it from the team ([`OwnTeam::from_team`]) or, without the team,
/// empty: [`OwnTeam::observe`] learns from each request what it shows (a
/// team preview request lists every Pokémon; a move request the active
/// ones' PP). Plain `Copy` data without heap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct OwnTeam {
    pub mons: StackVec<TeamMon, MAX_TEAM>,
}

impl OwnTeam {
    /// The team's sets, in registration order, as the battle builds them
    /// (the format's level rule, stats by Showdown's formula, every PP Up):
    /// what the player built. A Zacian holding a Rusted Sword or a
    /// Zamazenta holding a Rusted Shield is crowned, as the battle's start
    /// makes it (its species' `onBattleStart`): the Crowned forme's stats
    /// and ability, Iron Head as Behemoth Blade or Bash (as
    /// [`OpenSheet::from_team`](crate::OpenSheet::from_team) shows it).
    pub fn from_team(team: &Team, format: &Format) -> OwnTeam {
        use pikapro_data::gen9::{items, moves, species as s};
        let dex = format.dex;
        let mut out = OwnTeam::default();
        for set in team.sets.iter().take(MAX_TEAM) {
            let crowned = match (set.species, set.item) {
                (s::ZACIAN, Some(items::RUSTED_SWORD)) => Some((s::ZACIAN_CROWNED, moves::BEHEMOTH_BLADE)),
                (s::ZAMAZENTA, Some(items::RUSTED_SHIELD)) => Some((s::ZAMAZENTA_CROWNED, moves::BEHEMOTH_BASH)),
                _ => None,
            };
            let mut set_moves = set.moves;
            if let Some((_, signature)) = crowned
                && let Some(m) = set_moves.iter_mut().find(|m| **m == moves::IRON_HEAD)
            {
                *m = signature;
            }
            let species_id = crowned.map_or(set.species, |(c, _)| c);
            let species = dex.species(species_id);
            let ability = if crowned.is_some() { species.abilities.primary } else { set.ability };
            let level = format.level.apply(set.level);
            let (evs, ivs) = set.investment.effective();
            let stats = [Stat::Hp, Stat::Atk, Stat::Def, Stat::SpA, Stat::SpD, Stat::Spe]
                .map(|s| calc_stat(s, species.base_stats[s], ivs[s], evs[s], level, set.nature));
            let mut max_pp = [0; MAX_MOVES];
            for (m, &id) in max_pp.iter_mut().zip(set_moves.iter()) {
                *m = move_max_pp(format.mechanics, dex.move_(id));
            }
            // The gender and the Tera type are the set's species' (the battle
            // takes them before it crowns).
            let base = dex.species(set.species);
            let gender = set.gender.or(match base.gender {
                GenderRule::Male => Some(Gender::Male),
                GenderRule::Female => Some(Gender::Female),
                GenderRule::Genderless => Some(Gender::Genderless),
                GenderRule::Ratio { .. } => None,
            });
            out.mons.push(TeamMon {
                species: pikapro_protocol::details_species(species_id),
                level,
                gender,
                nature: Some(set.nature),
                item: set.item,
                ability,
                moves: set_moves,
                max_pp,
                stats,
                tera_type: Some(set.tera_type.unwrap_or(base.types.primary)),
                pp: max_pp,
                pp_known: true,
                other_pp: [None; MAX_MOVES],
            });
        }
        out
    }

    /// The Pokémon with identity `id`.
    pub fn get(&self, id: u8) -> Option<&TeamMon> {
        self.mons.get(id as usize)
    }

    /// Learns what a request shows (`own`, its overlay): the Pokémon it
    /// lists (a team preview request lists every one) fill what the team
    /// left open (the gender drawn at random; everything when the team
    /// wasn't given); a move request shows each active Pokémon's PP. A move
    /// slot that isn't the set's (Mimic, Sketch; Transform's copies, whose
    /// maximum PP is 5) keeps its PP apart ([`TeamMon::other_pp`]). It
    /// reads the request alone, so the order a channel sends the request
    /// and the log in doesn't matter.
    pub fn observe(&mut self, own: &OwnOverlay) {
        // Which active slots show a transformed Pokémon's copied moves.
        let copied: [bool; MAX_ACTIVE] = core::array::from_fn(|slot| copied_moves(own, slot));
        for (i, m) in own.mons.iter().enumerate() {
            let id = m.id as usize;
            if id >= MAX_TEAM {
                continue;
            }
            while self.mons.len() <= id && !self.mons.is_full() {
                self.mons.push(TeamMon::default());
            }
            let Some(t) = self.mons.get_mut(id) else { continue };
            let unknown = t.level == 0;
            t.gender = t.gender.or(Some(m.gender));
            // What follows learns the set: nothing once it is known.
            if !unknown && !t.moves.is_empty() {
                continue;
            }
            // A transformed Pokémon's move slots are the ones it copied
            // (an Imposter lead, before the first request): not its set's,
            // which a later request shows.
            let moves = if copied.get(i).copied().unwrap_or(false) { StackVec::new() } else { m.moves };
            if unknown {
                let [a, b, c, d, e] = m.stats;
                *t = TeamMon {
                    species: m.species,
                    level: m.level,
                    item: m.item,
                    ability: m.base_ability,
                    moves,
                    stats: [m.max_hp, a, b, c, d, e],
                    tera_type: Some(m.tera_type),
                    ..*t
                };
            } else if t.moves.is_empty() {
                t.moves = moves;
            }
        }
        for (slot, active) in own.active.iter().enumerate() {
            let (Some(a), Some(m)) = (active, own.mons.get(slot)) else { continue };
            let OwnMoves::Moves(moves) = &a.moves else { continue };
            let copied = copied[slot];
            let Some(t) = self.mons.get_mut(m.id as usize) else { continue };
            if !copied && !t.pp_known {
                t.pp = t.max_pp;
                t.pp_known = true;
            }
            for (k, r) in moves.iter().enumerate().take(MAX_MOVES) {
                let set = !copied && t.moves.get(k) == Some(&r.id) && (t.max_pp[k] == 0 || t.max_pp[k] == r.max_pp);
                if set {
                    t.pp[k] = r.pp;
                    t.max_pp[k] = r.max_pp;
                } else {
                    t.other_pp[k] = Some((r.id, r.pp, r.max_pp));
                }
            }
        }
    }
}

/// Whether `own.mons[i]` is active with the move slots of a transformed
/// Pokémon: Transform gives each copied move 5 PP at most (1 for a move
/// with 1; `transformInto`), and no set's move has so few with every PP Up
/// but the few that take none (Sketch, Revival Blessing).
fn copied_moves(own: &OwnOverlay, i: usize) -> bool {
    match own.active.get(i) {
        Some(Some(OwnActive { moves: OwnMoves::Moves(ms), .. })) => !ms.is_empty() && ms.iter().all(|m| m.max_pp <= 5),
        _ => false,
    }
}
