//! [`ViewCtx`]: a [`PlayerView`] resolved once per encode into ego-centric
//! observation rows ([`MonRef`]), with helpers for the common facts. Every
//! built-in encoder reads the view through it, and custom encoders can too.
//!
//! **Rows.** Side 0 is the player's (`view.me`), side 1 the opponent's.
//! - Own row `k` is the player's Pokémon with identity `k`
//!   ([`OwnMon::id`], its registration index): stable for the whole
//!   battle, whatever the team order becomes. With team preview it is also
//!   its public row (the `|poke|` order).
//! - Foe row `k` is the public row `k`: the `|poke|` order with team
//!   preview, the order of first appearance without it.
//!
//! Own facts come from the request ([`OwnOverlay`](pikapro_view::OwnOverlay)),
//! and what no request shows from the player's own team when the view has
//! it ([`OwnTeam`](pikapro_view::OwnTeam): natures, the Pokémon left out at
//! team preview, PP outside a move request); what the log has shown about
//! them (revealed moves, a lost item, turns asleep) from the public record.
//! Per-switch-in facts (boosts, volatiles) come from the public record of
//! the active *slot*, which stays right under Illusion.

use pikapro::{Gender, MAX_ACTIVE, MAX_MOVES, MAX_TEAM};
use pikapro_data::{MoveId, Nature, SpeciesId, Type, TypeSet};
use pikapro_view::{
    AbilityState, ActiveView, HpHint, ItemState, MonFlags, MonView, OwnActive, OwnMon, OwnMoves, Phase, PlayerView,
    PublicSide, SheetMon, StatusKind, TeamMon,
};

use crate::tables::Tables;

/// Rows per side.
pub const ROWS: usize = MAX_TEAM;

/// The HP of a Pokémon whose HP was never shown, on the percentages'
/// scale: full ([`MonRef::hp`]).
pub const FULL_HP: u16 = 100;

/// What is known of a move slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MoveKnowledge {
    /// Not revealed.
    #[default]
    Unknown,
    /// Known to be empty (a Pokémon with fewer than four moves).
    Empty,
    Known(MoveId),
}

impl MoveKnowledge {
    pub const fn id(self) -> Option<MoveId> {
        match self {
            MoveKnowledge::Known(id) => Some(id),
            _ => None,
        }
    }
}

/// One move slot of a row: own move slots (in slot order), or a foe's
/// moves (its Open Team Sheet's, or the revealed ones in the order seen).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MoveInfo {
    pub slot: MoveKnowledge,
    /// PP left and maximum PP: own Pokémon only (the request's while it
    /// lists the Pokémon's moves, in a move request; otherwise the last a
    /// request showed, from the player's team, [`TeamMon::pp`]).
    pub pp: Option<u8>,
    pub max_pp: Option<u8>,
    /// Disabled in the request (own active Pokémon).
    pub disabled: bool,
    /// Uses and PP spent as the log shows them.
    pub uses: u8,
    pub pp_spent: u8,
    /// The log has shown it (for own moves: the opponent has seen it).
    pub revealed: bool,
}

impl MoveInfo {
    /// PP left as a fraction: exact from the request, otherwise estimated
    /// from the PP the log shows spent against the move's maximum in a set
    /// ([`MoveFacts::max_pp`](crate::tables::MoveFacts::max_pp)). 0 for an
    /// unknown or empty slot.
    pub fn pp_frac(&self, tables: &Tables) -> f32 {
        let Some(id) = self.slot.id() else { return 0.0 };
        if let (Some(pp), Some(max)) = (self.pp, self.max_pp) {
            return if max == 0 { 0.0 } else { f32::from(pp) / f32::from(max) };
        }
        let max = u32::from(tables.move_facts(id).max_pp);
        if max == 0 {
            return 0.0;
        }
        let left = max.saturating_sub(u32::from(self.pp_spent));
        left as f32 / max as f32
    }
}

/// A Pokémon's types as far as the player can tell ([`MonRef::types`]):
/// its types, and a type added by Forest's Curse or Trick-or-Treat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Types {
    pub set: TypeSet,
    pub added: Option<Type>,
}

impl Types {
    /// A bit per type (`1 << Type as u8`).
    pub fn bits(self) -> u32 {
        let mut b = 1 << self.set.primary as u8;
        if let Some(t) = self.set.secondary {
            b |= 1 << t as u8;
        }
        if let Some(t) = self.added {
            b |= 1 << t as u8;
        }
        b
    }

    /// The damage multiplier of an attack of type `attack` against these
    /// types, in sixteenths (16 neutral; [`Tables::effect16`]).
    #[inline]
    pub fn effect16(self, attack: Type, tables: &Tables) -> u32 {
        tables.effect16(attack, self.set, self.added)
    }
}

/// One observation row: the sources of one Pokémon's facts. `Default` is
/// an empty row (no Pokémon seen there). Two rows are equal when they
/// show the same facts: the sources are compared by value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonRef<'a> {
    /// The player's own Pokémon.
    pub own: bool,
    pub row: u8,
    /// Its public record.
    pub public: Option<&'a MonView>,
    /// Its request entry (own Pokémon).
    pub own_mon: Option<&'a OwnMon>,
    /// Its current team position (own Pokémon): what
    /// `SlotAction::Switch(i)` sends in.
    pub team_pos: Option<u8>,
    /// The active slot it is in.
    pub slot: Option<u8>,
    /// Its active slot's public record (boosts, volatiles).
    pub active: Option<&'a ActiveView>,
    /// Its slot's part of a move request (own active Pokémon).
    pub request: Option<&'a OwnActive>,
    /// Its Open Team Sheet entry (foe Pokémon, when sheets are open).
    pub sheet: Option<&'a SheetMon>,
    /// Its set in the player's own team (own Pokémon, when the view has
    /// the team: [`PlayerView::team`]).
    pub team: Option<&'a TeamMon>,
    /// Own Pokémon: in the battle (the request lists it once the battle
    /// has started; with team preview, one of those brought).
    pub brought: bool,
}

impl<'a> MonRef<'a> {
    /// A Pokémon is known in this row.
    #[inline]
    pub fn present(&self) -> bool {
        self.public.is_some() || self.own_mon.is_some() || self.team.is_some()
    }

    /// An own Pokémon the request doesn't list (left out at team preview),
    /// whose facts come from the team.
    fn left_out(&self) -> Option<&'a TeamMon> {
        if self.own_mon.is_some() { None } else { self.team.filter(|t| t.level > 0) }
    }

    /// The species shown now (a temporary forme too). For an own Pokémon
    /// the request's, unless the public record shows a forme of it.
    pub fn species(&self) -> Option<SpeciesId> {
        match (self.own_mon, self.public) {
            (Some(o), Some(p)) if p.details_species == o.species => Some(p.species),
            (Some(o), _) => Some(o.species),
            (None, _) if self.left_out().is_some() => self.left_out().map(|t| t.species),
            (None, Some(p)) => Some(p.species),
            (None, None) => None,
        }
    }

    /// The species its details show (permanent formes only).
    pub fn details_species(&self) -> Option<SpeciesId> {
        self.own_mon
            .map(|o| o.species)
            .or(self.left_out().map(|t| t.species))
            .or(self.public.map(|p| p.details_species))
    }

    pub fn level(&self) -> u8 {
        self.own_mon.map(|o| o.level).or(self.left_out().map(|t| t.level)).or(self.public.map(|p| p.level)).unwrap_or(0)
    }

    pub fn gender(&self) -> Gender {
        self.own_mon
            .map(|o| o.gender)
            .or(self.left_out().and_then(|t| t.gender))
            .or(self.public.map(|p| p.gender))
            .unwrap_or_default()
    }

    /// `(hp, max)` as the player knows them: exact for its own Pokémon (one
    /// left out at team preview is at full HP), as shown for the foe's
    /// (`max` 100 under percentages). A Pokémon whose HP was never shown
    /// hasn't been on the field, so it is at full HP: `(100, 100)`, the
    /// percentages' scale.
    pub fn hp(&self) -> (u16, u16) {
        match (self.own_mon, self.left_out(), self.public) {
            (Some(o), _, _) => (o.hp, o.max_hp),
            (None, Some(t), _) => (t.stats[0], t.stats[0]),
            (None, None, Some(p)) if p.hp.is_known() => (p.hp.hp, p.hp.max),
            _ if self.present() => (FULL_HP, FULL_HP),
            _ => (0, 0),
        }
    }

    /// HP as a fraction: 1 for a Pokémon whose HP hasn't been shown (it
    /// hasn't been on the field: full), 0 for an empty row.
    pub fn hp_frac(&self) -> f32 {
        match self.hp() {
            (_, 0) => 0.0,
            (hp, max) => f32::from(hp) / f32::from(max),
        }
    }

    /// The HP the public record shows (for an own Pokémon, what the
    /// opponent sees): `(100, 100)` if none was shown.
    pub fn public_hp(&self) -> (u16, u16) {
        match self.public {
            Some(p) if p.hp.is_known() => (p.hp.hp, p.hp.max),
            _ if self.present() => (FULL_HP, FULL_HP),
            _ => (0, 0),
        }
    }

    /// The Champions health-bar hint on the shown percentage (foes only).
    pub fn hp_hint(&self) -> HpHint {
        match (self.own_mon, self.public) {
            (None, Some(p)) => p.hp.hint,
            _ => HpHint::None,
        }
    }

    /// Its types if team preview hid its forme (`Urshifu-*`): the types
    /// every forme shares ([`pikapro_view::wildcard_types`]), `None` past
    /// them, and whether those are all of them; `None` if its forme is
    /// known.
    pub fn wildcard_types(&self, tables: &Tables) -> Option<([Option<Type>; 2], bool)> {
        if !self.hidden_forme() {
            return None;
        }
        Some(tables.wildcard_types(self.species()?))
    }

    /// A foe whose forme team preview hid ([`MonRef::wildcard_types`]).
    #[inline]
    fn hidden_forme(&self) -> bool {
        self.own_mon.is_none() && self.left_out().is_none() && self.forme_hidden()
    }

    /// Its Tera type once it Terastallized.
    #[inline]
    fn tera_now(&self, tables: &Tables) -> Option<Type> {
        if self.terastallized() { self.tera_type(tables) } else { None }
    }

    pub fn status(&self) -> StatusKind {
        self.own_mon.map(|o| o.status).or(self.public.map(|p| p.status)).unwrap_or_default()
    }

    /// Turns the status has been observed to last.
    pub fn status_turns(&self) -> u8 {
        match self.public {
            Some(p) if p.status == self.status() => p.status_turns,
            _ => 0,
        }
    }

    fn flag(&self, bit: u16) -> bool {
        self.public.is_some_and(|p| p.flags.has(bit))
    }

    /// In an active slot now (for own Pokémon, per the request; for a
    /// foe's, a slot of the public record shows it). Read from the slot on
    /// both sides, never from a record's flag alone: a row no slot holds
    /// isn't on the field.
    pub fn is_active(&self) -> bool {
        self.slot.is_some()
    }

    pub fn is_fainted(&self) -> bool {
        match self.own_mon {
            Some(o) => o.hp == 0 && o.max_hp > 0,
            None if self.left_out().is_some() => false,
            None => self.flag(MonFlags::FAINTED),
        }
    }

    pub fn is_revealed(&self) -> bool {
        self.flag(MonFlags::REVEALED)
    }

    pub fn is_previewed(&self) -> bool {
        self.flag(MonFlags::PREVIEWED)
    }

    pub fn forme_hidden(&self) -> bool {
        self.flag(MonFlags::FORME_HIDDEN)
    }

    pub fn is_mega(&self) -> bool {
        self.flag(MonFlags::MEGA)
    }

    /// Terastallized (own: per the request).
    pub fn terastallized(&self) -> bool {
        match self.own_mon {
            Some(o) => o.terastallized.is_some(),
            None => self.flag(MonFlags::TERASTALLIZED),
        }
    }

    /// Its Tera type, if known; `None` in a format without
    /// Terastallization, where no Pokémon has one (the request shows a
    /// placeholder).
    pub fn tera_type(&self, tables: &Tables) -> Option<Type> {
        if !tables.tera {
            return None;
        }
        match (self.own_mon, self.left_out(), self.public) {
            (Some(o), _, _) => Some(o.terastallized.unwrap_or(o.tera_type)),
            (None, Some(t), _) => t.tera_type,
            (None, None, Some(p)) => p.tera_type.or(self.sheet.map(|s| s.tera_type)),
            (None, None, None) => None,
        }
    }

    /// Its held item as far as the player knows. Own Pokémon: the
    /// request's item, or how the log says it was lost.
    pub fn item(&self) -> ItemState {
        match (self.own_mon, self.public) {
            (Some(o), p) => match (o.item, p.map(|p| p.item)) {
                (Some(i), _) => ItemState::Known(i),
                (None, Some(g @ ItemState::Gone { .. })) => g,
                (None, _) => ItemState::NoItem,
            },
            (None, _) if self.left_out().is_some() => {
                self.left_out().and_then(|t| t.item).map_or(ItemState::NoItem, ItemState::Known)
            }
            (None, Some(p)) => p.item,
            (None, None) => ItemState::Unknown,
        }
    }

    /// Its ability now, as far as the player knows.
    pub fn ability(&self) -> AbilityState {
        match (self.own_mon, self.left_out(), self.public) {
            (Some(o), _, _) => AbilityState::Known(o.ability),
            (None, Some(t), _) => AbilityState::Known(t.ability),
            (None, None, Some(p)) => p.ability,
            (None, None, None) => AbilityState::Unknown,
        }
    }

    /// Its own ability (the set's), as far as the player knows.
    pub fn base_ability(&self) -> AbilityState {
        match (self.own_mon, self.public) {
            (Some(o), _) => AbilityState::Known(o.base_ability),
            (None, _) if self.left_out().is_some() => self.ability(),
            (None, Some(p)) => match (p.base_ability, self.sheet) {
                (AbilityState::Unknown, Some(s)) => AbilityState::Known(s.ability),
                (a, _) => a,
            },
            (None, None) => AbilityState::Unknown,
        }
    }

    /// Its nature, if known: an own Pokémon's from the player's team (the
    /// request doesn't show it), a foe's from an Open Team Sheet (Champions
    /// shows it).
    pub fn nature(&self) -> Option<Nature> {
        if self.own { self.team.and_then(|t| t.nature) } else { self.sheet.and_then(|s| s.nature) }
    }

    /// The set's stats `[hp, atk, def, spa, spd, spe]`, exact (own
    /// Pokémon: the request's max HP and stats, Showdown's
    /// `baseStoredStats`). They are not always the stats in use: a
    /// transformed Pokémon uses its target's but HP, and Power Trick,
    /// Guard Split, Power Split and Speed Swap change stored stats the
    /// request doesn't show.
    pub fn exact_stats(&self) -> Option<[u16; 6]> {
        match (self.own_mon, self.left_out()) {
            (Some(o), _) => {
                let [a, b, c, d, e] = o.stats;
                Some([o.max_hp, a, b, c, d, e])
            }
            (None, Some(t)) => Some(t.stats),
            _ => None,
        }
    }

    /// Stat stages (0 off the field).
    pub fn boosts(&self) -> [i8; 7] {
        self.active.map_or([0; 7], |a| a.boosts)
    }

    /// Its current types, as Showdown's `getTypes` (and
    /// [`PublicTracker::types`](pikapro_view::PublicTracker::types)) has them:
    /// its Tera type once it Terastallized (not Stellar, which keeps its
    /// types), with no added type; else the types a type change gave it
    /// (Soak, Protean, Transform, ...), else its species' (the one it
    /// transformed into, if it did), without Flying while it roosts (Normal
    /// if nothing is left); plus a type added by Forest's Curse or
    /// Trick-or-Treat.
    ///
    /// `None` for a foe whose forme team preview hid: only the types its
    /// formes share are known ([`MonRef::wildcard_types`]).
    #[inline(never)]
    pub fn types(&self, tables: &Tables) -> Option<Types> {
        // `wildcard_types` is `Some` exactly when this holds and the
        // species is known; without it, `None` either way.
        if self.hidden_forme() {
            return None;
        }
        let species = self.species()?;
        let tera = self.tera_now(tables);
        let shown = tables.species_types(self.active.and_then(|a| a.transformed).map_or(species, |t| t.species));
        // The tracker's rule (`pikapro_view::shown_types`), with what the
        // player knows of its own (species, Tera type).
        let set = pikapro_view::shown_types(shown, tera, self.active);
        let added = self.active.and_then(|a| a.added_type).filter(|_| tera.is_none_or(|t| t == Type::Stellar));
        Some(Types { set, added })
    }

    /// Its four move slots (see [`MoveInfo`]).
    #[inline]
    pub fn moves(&self) -> [MoveInfo; MAX_MOVES] {
        let mut out = [MoveInfo::default(); MAX_MOVES];
        self.fill_moves(&mut out);
        out
    }

    /// [`MonRef::moves`] into `out`. Out of line and in place: built as a
    /// local and returned, the slots would be kept field by field in
    /// registers and stored one byte at a time.
    #[inline(never)]
    fn fill_moves(&self, out: &mut [MoveInfo; MAX_MOVES]) {
        self.each_move(|i, m| out[i] = m);
    }

    /// Calls `f(i, slot)` with each of its four move slots in order (see
    /// [`MoveInfo`]). Inlined into its caller: a slot goes from registers to
    /// where `f` writes it (written into an array of slots and read back,
    /// its bytes would be reloaded wider than they are stored, which stalls
    /// the load).
    #[inline(always)]
    fn each_move(&self, mut f: impl FnMut(usize, MoveInfo)) {
        // Uses, PP spent and revealed, as the log shows them.
        let public = |id: MoveId| match self.public.and_then(|p| p.revealed(id)) {
            Some(r) => (r.uses, r.pp_spent, true),
            None => (0, 0, false),
        };
        // An own move slot no request lists now: the last PP seen (the
        // team's; for a move that isn't the set's, a transformed
        // Pokémon's copy or Mimic's, the last a move request showed).
        let team = |i: usize, id: MoveId| match self.team {
            Some(t) if t.moves.get(i) == Some(&id) => (t.last_pp(i), Some(t.max_pp[i]).filter(|&x| x > 0)),
            Some(t) => {
                let mut m = MoveInfo::default();
                other_pp(t, i, id, &mut m);
                (m.pp, m.max_pp)
            }
            None => (None, None),
        };
        let own = self.own_mon.map(|o| &o.moves).or(self.left_out().map(|t| &t.moves));
        if let Some(own) = own {
            let request = match self.request.map(|r| &r.moves) {
                Some(OwnMoves::Moves(ms)) => Some(ms),
                _ => None,
            };
            for i in 0..MAX_MOVES {
                let Some(&id) = own.get(i) else {
                    f(i, MoveInfo { slot: MoveKnowledge::Empty, ..MoveInfo::default() });
                    continue;
                };
                let (pp, max_pp, disabled) = match request.and_then(|ms| ms.get(i)).filter(|r| r.id == id) {
                    Some(r) => (Some(r.pp), Some(r.max_pp), r.disabled),
                    // No move list for it (benched, or a forced switch, a
                    // wait, a locked or recharge turn): the last PP a
                    // request showed.
                    None => {
                        let (pp, max_pp) = team(i, id);
                        (pp, max_pp, false)
                    }
                };
                let (uses, pp_spent, revealed) = public(id);
                f(i, MoveInfo { slot: MoveKnowledge::Known(id), pp, max_pp, disabled, uses, pp_spent, revealed });
            }
        } else if let Some(sheet) = self.sheet {
            for i in 0..MAX_MOVES {
                let m = match sheet.moves.get(i) {
                    Some(&id) => {
                        let (uses, pp_spent, revealed) = public(id);
                        MoveInfo { slot: MoveKnowledge::Known(id), uses, pp_spent, revealed, ..MoveInfo::default() }
                    }
                    None => MoveInfo { slot: MoveKnowledge::Empty, ..MoveInfo::default() },
                };
                f(i, m);
            }
        } else if let Some(p) = self.public {
            for i in 0..MAX_MOVES {
                let m = match p.moves.get(i) {
                    Some(r) => MoveInfo {
                        slot: MoveKnowledge::Known(r.id),
                        uses: r.uses,
                        pp_spent: r.pp_spent,
                        revealed: true,
                        ..MoveInfo::default()
                    },
                    None => MoveInfo::default(),
                };
                f(i, m);
            }
        } else {
            for i in 0..MAX_MOVES {
                f(i, MoveInfo::default());
            }
        }
    }
}

/// Own move slot `i` holding `id`, a move that isn't the set's (a
/// transformed Pokémon's copy, Mimic's): the PP a move request last showed
/// for it ([`TeamMon::other_pp`]). Out of line: it is rare, and keeping it
/// out keeps the common path cheap.
#[cold]
#[inline(never)]
fn other_pp(t: &TeamMon, i: usize, id: MoveId, m: &mut MoveInfo) {
    if let Some(&Some((x, pp, max))) = t.other_pp.get(i)
        && x == id
    {
        m.pp = Some(pp);
        m.max_pp = Some(max);
    }
}

/// A [`PlayerView`] resolved into ego-centric rows, once per encode.
#[derive(Clone, Copy, Debug)]
pub struct ViewCtx<'a> {
    pub view: PlayerView<'a>,
    pub tables: &'a Tables,
    /// `mons[0]` the player's rows, `mons[1]` the opponent's.
    pub mons: [[MonRef<'a>; ROWS]; 2],
    /// The public sides, the player's first.
    pub sides: [&'a PublicSide; 2],
    /// The row in each active slot, per side.
    pub slot_rows: [[Option<u8>; MAX_ACTIVE]; 2],
}

impl<'a> ViewCtx<'a> {
    pub fn new(view: &PlayerView<'a>, tables: &'a Tables) -> ViewCtx<'a> {
        let view = *view;
        let active = (tables.active as usize).min(MAX_ACTIVE);
        let sides = [view.my_side(), view.foe_side()];
        let mut mons = [[MonRef::default(); ROWS]; 2];
        let mut slot_rows = [[None; MAX_ACTIVE]; 2];

        // Own rows, by identity. The request lists the Pokémon in the
        // battle once it has started (all of them at team preview, where
        // it marks the first ones `active` though nothing is on the field
        // yet: no slot then).
        let [own_side, foe_side] = sides;
        let started = !matches!(view.public.phase, Phase::Setup | Phase::TeamPreview);
        let team = |id: usize| view.team.and_then(|t| t.get(id as u8));
        for (pos, m) in view.own.mons.iter().enumerate() {
            let row = m.id as usize;
            if row >= ROWS {
                continue;
            }
            let slot = (started && pos < active && m.active).then_some(pos as u8);
            let public = view.own_row(m.id).and_then(|r| own_side.mon(r));
            let active_view = slot.and_then(|s| own_side.slots.get(s as usize)).filter(|a| a.row.is_some());
            let request = slot.and_then(|s| view.own.active[s as usize].as_ref());
            if let Some(s) = slot {
                slot_rows[0][s as usize] = Some(row as u8);
            }
            mons[0][row] = MonRef {
                own: true,
                row: row as u8,
                public,
                own_mon: Some(m),
                team_pos: Some(pos as u8),
                slot,
                active: active_view,
                request,
                sheet: None,
                team: team(row),
                brought: started,
            };
        }
        // Own Pokémon left out of the battle: their sets (the team), and
        // with team preview their previewed record (public row = identity).
        for (row, r) in mons[0].iter_mut().enumerate() {
            if r.own_mon.is_some() {
                continue;
            }
            let public = if view.public.config.team_preview { own_side.mon(row as u8) } else { None };
            if public.is_some() || team(row).is_some() {
                *r = MonRef { own: true, row: row as u8, public, team: team(row), ..MonRef::default() };
            }
        }

        // Foe rows, by public row.
        let sheet = view.public.ots[view.me.foe().index()].as_ref();
        for (row, p) in foe_side.mons.iter().enumerate().take(ROWS) {
            let slot = (0..active).find(|&s| foe_side.slots[s].row == Some(row as u8) && p.is_active());
            if let Some(s) = slot {
                slot_rows[1][s] = Some(row as u8);
            }
            let active = slot.map(|s| &foe_side.slots[s]);
            mons[1][row] = MonRef {
                own: false,
                row: row as u8,
                public: Some(p),
                own_mon: None,
                team_pos: None,
                slot: slot.map(|s| s as u8),
                active,
                request: None,
                sheet: sheet.and_then(|s| s.mons.get(row)),
                team: None,
                brought: false,
            };
        }
        ViewCtx { view, tables, mons, sides, slot_rows }
    }

    /// Row `row` of ego side `side` (0 own, 1 foe).
    #[inline]
    pub fn mon(&self, side: usize, row: usize) -> &MonRef<'a> {
        &self.mons[side][row]
    }

    /// The Pokémon in active slot `slot` of ego side `side`.
    pub fn active_in(&self, side: usize, slot: usize) -> Option<&MonRef<'a>> {
        let row = self.slot_rows[side].get(slot).copied().flatten()?;
        Some(&self.mons[side][row as usize])
    }

    /// Active slots per side in this format.
    #[cfg(test)]
    #[inline]
    pub(crate) fn active_slots(&self) -> usize {
        (self.tables.active as usize).min(MAX_ACTIVE)
    }
}
