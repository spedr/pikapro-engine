//! [`OwnOverlay`]: what a player's request tells it about its own side.

use pikapro::legal::Revealed;
use pikapro::request::{ActiveRequest, MoveChoices, RequestData, RequestKind};
use pikapro::state::{SlotCondition, Volatiles};
use pikapro::stats::calc_stat;
use pikapro::util::StackVec;
use pikapro::{Battle, Gender, MAX_ACTIVE, MAX_MOVES, MAX_TEAM, Request, SideId, SlotAction};
use pikapro_data::{AbilityId, ItemId, MoveId, MoveTarget, SpeciesId, Stat, Type};

use crate::mon::StatusKind;

/// The decision a request asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OwnRequest {
    /// Nothing to decide (the other side is choosing, or the battle is
    /// over).
    #[default]
    Wait,
    /// Choose the Pokémon to bring (`bring` of them) and their order.
    TeamPreview { bring: u8 },
    /// Choose an action for every active slot ([`OwnOverlay::active`]).
    Move,
    /// Replace the Pokémon in the slots marked `true`.
    ForceSwitch { slots: [bool; MAX_ACTIVE] },
}

/// One of the player's own Pokémon, as its request shows it
/// (`side.pokemon[i]`): exact HP, stats, moves, item and ability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnMon {
    /// Its stable identity ([`pikapro::PokemonState::id`], the
    /// registration index). [`PlayerView::own_row`](crate::PlayerView::own_row)
    /// maps it to its row in the public record.
    pub id: u8,
    /// Its details' species (Showdown's `baseSpecies`: the set's species,
    /// or the forme a permanent forme change made), as the request's
    /// `details` show it (Greninja-Bond as Greninja).
    pub species: SpeciesId,
    pub level: u8,
    /// Its own gender (under Illusion too).
    pub gender: Gender,
    pub hp: u16,
    pub max_hp: u16,
    /// The status the request shows (`slp`, not the turns left).
    pub status: StatusKind,
    /// In a slot now (fainted or not); at team preview, the first
    /// `active_per_side` registered (Showdown's request marks them so).
    pub active: bool,
    /// Showdown's `baseStoredStats`: Attack, Defense, Special Attack,
    /// Special Defense, Speed of its current forme, from its set.
    pub stats: [u16; 5],
    /// Its move slots now (Transform, Mimic and Sketch change them).
    pub moves: StackVec<MoveId, MAX_MOVES>,
    pub item: Option<ItemId>,
    pub ability: AbilityId,
    pub base_ability: AbilityId,
    pub tera_type: Type,
    /// The type it Terastallized into.
    pub terastallized: Option<Type>,
    /// A Tatsugiri inside a Dondozo.
    pub commanding: bool,
    /// Its slot has Revival Blessing (it can be chosen to revive).
    pub reviving: bool,
}

impl Default for OwnMon {
    fn default() -> OwnMon {
        OwnMon {
            id: 0,
            species: SpeciesId::default(),
            level: 0,
            gender: Gender::Genderless,
            hp: 0,
            max_hp: 0,
            status: StatusKind::None,
            active: false,
            stats: [0; 5],
            moves: StackVec::new(),
            item: None,
            ability: AbilityId::NONE,
            base_ability: AbilityId::NONE,
            tera_type: Type::Normal,
            terastallized: None,
            commanding: false,
            reviving: false,
        }
    }
}

/// One move slot of an active Pokémon's request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnMove {
    pub id: MoveId,
    pub pp: u8,
    pub max_pp: u8,
    /// Its target type now (Curse from a non-Ghost targets the user).
    pub target: MoveTarget,
    pub disabled: bool,
}

impl Default for OwnMove {
    fn default() -> OwnMove {
        OwnMove { id: MoveId::default(), pp: 0, max_pp: 0, target: MoveTarget::Normal, disabled: false }
    }
}

/// What an active Pokémon can choose, as its request says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OwnMoves {
    /// Nothing but Struggle.
    #[default]
    Struggle,
    /// Must recharge.
    Recharge,
    /// Locked into this move.
    Locked(MoveId),
    /// Each move slot.
    Moves(StackVec<OwnMove, MAX_MOVES>),
}

/// An active slot's part of a move request.
///
/// The request is honest about what the player can't see: under
/// `maybe_trapped` it offers switches, under `maybe_disabled` moves, that
/// the battle may refuse. Showdown then answers `[Unavailable choice]` with
/// the request updated; [`OwnOverlay::reject`] makes the same update.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OwnActive {
    pub moves: OwnMoves,
    /// It can't switch out.
    pub trapped: bool,
    /// It may be trapped by a foe's ability it can't see (Shadow Tag, Arena
    /// Trap, Magnet Pull): the request allows a switch the battle may
    /// reject.
    pub maybe_trapped: bool,
    /// It may have a move a foe's Imprison disables (the request shows the
    /// move usable).
    pub maybe_disabled: bool,
    pub maybe_locked: bool,
    pub can_mega_evo: bool,
    /// The type it can Terastallize into.
    pub can_terastallize: Option<Type>,
}

/// A player's own side as its request describes it: everything Showdown's
/// `|request|` JSON shows, nothing else. Engine counters the client never
/// sees (sleep turns left, confusion turns, stall counters) are not here.
/// Public facts (boosts, volatiles, side conditions) come from the shared
/// [`PublicTracker`](crate::PublicTracker).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OwnOverlay {
    pub side: SideId,
    pub request: OwnRequest,
    /// The side has answered this request and waits for the other.
    pub answered: bool,
    /// Showdown's `noCancel`: the side is the only one deciding, so its
    /// choice can't be taken back.
    pub no_cancel: bool,
    /// Its Pokémon in Showdown's current team order: `mons[i]` is the one
    /// `SlotAction::Switch(i)` sends in, `mons[slot]` the one active in
    /// `slot`.
    pub mons: StackVec<OwnMon, MAX_TEAM>,
    /// Each active slot's choices (move requests only; `None` for a slot
    /// never filled and outside move requests).
    pub active: [Option<OwnActive>; MAX_ACTIVE],
    /// Learnt from a refused choice ([`OwnOverlay::reject`]) since the
    /// request came: the slot's Pokémon is trapped after all.
    pub known_trapped: [bool; MAX_ACTIVE],
    /// Learnt from a refused choice: the slot's move slots (bit `i` for
    /// move `i`) that are disabled after all.
    pub known_disabled: [u8; MAX_ACTIVE],
    /// A refused choice updated the request (Showdown's `update: true`).
    pub updated: bool,
}

/// A choice the request allowed but the battle refused because of what
/// the player couldn't see (Showdown's `[Unavailable choice]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unavailable {
    /// A switch out of a Pokémon that turned out to be trapped
    /// ([`OwnActive::maybe_trapped`]).
    Switch,
    /// Move slot `index`, which turned out to be disabled (Imprison:
    /// [`OwnActive::maybe_disabled`]).
    Move { index: u8 },
}

impl OwnOverlay {
    /// The overlay of a request ([`Battle::request_data`](pikapro::Battle::request_data)).
    pub fn from_request(request: &RequestData) -> OwnOverlay {
        let mut out = OwnOverlay {
            side: request.side,
            answered: request.answered,
            no_cancel: request.no_cancel,
            ..OwnOverlay::default()
        };
        out.request = match request.kind {
            RequestKind::Wait => OwnRequest::Wait,
            // Without a picked team size, the whole team.
            RequestKind::TeamPreview { max_chosen_team_size } => {
                OwnRequest::TeamPreview { bring: max_chosen_team_size.unwrap_or(request.pokemon.len() as u8) }
            }
            RequestKind::Move { active } => {
                for (slot, a) in active.iter().enumerate().take(MAX_ACTIVE) {
                    out.active[slot] = a.as_ref().map(own_active);
                }
                OwnRequest::Move
            }
            RequestKind::ForceSwitch { slots } => {
                let mut s = [false; MAX_ACTIVE];
                for (o, &b) in s.iter_mut().zip(slots.iter()) {
                    *o = b;
                }
                OwnRequest::ForceSwitch { slots: s }
            }
        };
        for p in request.pokemon.iter() {
            let [_, atk, def, spa, spd, spe] = p.stats.0;
            out.mons.push(OwnMon {
                id: p.id,
                species: pikapro_protocol::details_species(p.species),
                level: p.level,
                gender: p.gender,
                hp: p.hp,
                max_hp: p.max_hp,
                status: StatusKind::of(p.status),
                active: p.active,
                stats: [atk, def, spa, spd, spe],
                moves: p.moves,
                item: p.item,
                ability: p.ability,
                base_ability: p.base_ability,
                tera_type: p.tera_type,
                terastallized: p.terastallized,
                commanding: p.commanding,
                reviving: p.reviving,
            });
        }
        out
    }

    /// `side`'s overlay now, equal to
    /// `OwnOverlay::from_request(&battle.request_data(side))` (fuzz-tested)
    /// but cheaper: the Pokémon come from the state, which holds exactly
    /// what the request copies of them, and only each active slot's move
    /// data ([`Battle::active_request`]: locked moves, the moves the
    /// request shows usable, trapping) from the engine. A Pokémon's stats
    /// are its set's (`set_stats`) unless a temporary forme or Transform
    /// changed its species, when the formula gives them as the request
    /// does.
    pub fn from_battle(battle: &Battle, side: SideId) -> OwnOverlay {
        let mut out = OwnOverlay::default();
        out.refresh(battle, side);
        out
    }

    /// Makes this `side`'s overlay now, in place: what
    /// `*self = OwnOverlay::from_battle(battle, side)` does (what refused
    /// choices taught goes with the old request).
    pub fn refresh(&mut self, battle: &Battle, side: SideId) {
        let format = battle.format();
        let n_active = format.active_per_side();
        let issued = battle.issued_request(side);
        let out = self;
        out.side = side;
        // `pending`: answered, waiting for the other side.
        out.answered = issued != Request::Wait && battle.request(side) == Request::Wait;
        // `noCancel`: the only side with a decision to make.
        out.no_cancel = issued != Request::Wait && battle.issued_request(side.foe()) == Request::Wait;
        out.known_trapped = [false; MAX_ACTIVE];
        out.known_disabled = [0; MAX_ACTIVE];
        out.updated = false;
        out.active = [None; MAX_ACTIVE];
        out.request = match issued {
            Request::Wait => OwnRequest::Wait,
            Request::TeamPreview { bring } => OwnRequest::TeamPreview { bring },
            Request::ForceSwitch { slots } => {
                let mut s = [false; MAX_ACTIVE];
                s[..n_active].copy_from_slice(&slots[..n_active]);
                OwnRequest::ForceSwitch { slots: s }
            }
            Request::Move => {
                for slot in 0..n_active {
                    out.active[slot] = battle.active_request(side, slot).as_ref().map(own_active);
                }
                OwnRequest::Move
            }
        };
        out.mons.clear();
        let s = battle.state().side(side);
        for (i, mon) in s.team.iter().enumerate() {
            let transformed = mon.volatiles.has(Volatiles::TRANSFORMED);
            // `baseStoredStats`: the current forme's (the one it
            // transformed in, if it did), from the set.
            let stats = match mon.volatile_counters.transform {
                None if !transformed && mon.species == mon.base_species => {
                    let st = mon.set_stats;
                    [st[Stat::Atk], st[Stat::Def], st[Stat::SpA], st[Stat::SpD], st[Stat::Spe]]
                }
                t => {
                    let species = match t {
                        Some(base) if transformed => base.own_species,
                        _ => mon.species,
                    };
                    let base = format.dex.species(species).base_stats;
                    [Stat::Atk, Stat::Def, Stat::SpA, Stat::SpD, Stat::Spe]
                        .map(|st| calc_stat(st, base[st], mon.ivs[st], mon.evs[st], mon.level, mon.nature))
                }
            };
            let active = i < n_active;
            out.mons.push(OwnMon {
                id: mon.id,
                species: pikapro_protocol::details_species(mon.base_species),
                level: mon.level,
                gender: mon.gender,
                hp: mon.hp,
                max_hp: mon.max_hp(),
                // `0 fnt`: a fainted Pokémon's request shows no status.
                status: if mon.hp == 0 { StatusKind::None } else { StatusKind::of(mon.status) },
                active,
                stats,
                moves: mon.moves.map(|m| m.id),
                item: mon.item,
                ability: mon.ability,
                base_ability: mon.base_ability,
                tera_type: battle.shown_tera_type(mon),
                terastallized: mon.terastallized.then_some(mon.tera_type),
                commanding: mon.volatiles.has(Volatiles::COMMANDING) && !mon.is_fainted(),
                reviving: active && s.active(i).is_some() && s.conditions.slots[i].has(SlotCondition::RevivalBlessing),
            });
        }
    }

    /// Makes the update Showdown makes to the request when it refuses a
    /// choice the request allowed (`side.ts`'s `emitChoiceError` with an
    /// `update`), and records what it taught in
    /// [`OwnOverlay::known_trapped`] / [`OwnOverlay::known_disabled`] as
    /// [`OwnOverlay::learn_update`] learns it from the updated request:
    ///
    /// * [`Unavailable::Switch`]: `maybeTrapped` goes, `trapped` is set.
    /// * [`Unavailable::Move`]: `updateDisabledRequest` (`maybeLocked`
    ///   goes; in doubles `maybeDisabled` goes too and every disabled move
    ///   shows as such), then the refused move is disabled; with no move
    ///   left, the gimmicks go.
    ///
    /// Returns whether the request changed: Showdown's `[Unavailable
    /// choice]` (with a new request) rather than `[Invalid choice]`. The
    /// battle is read only for the side's own Pokémon, as Showdown's
    /// update does (and `pikapro-server`'s: a move is disabled when the legal
    /// actions don't list it).
    ///
    /// A choice is unavailable when this overlay's request allows it and
    /// [`Battle::legal_actions`] doesn't: `Battle::submit` refuses it with
    /// `ChoiceError::IllegalAction`, as it does any illegal choice.
    pub fn reject(&mut self, battle: &Battle, slot: usize, choice: Unavailable) -> bool {
        let doubles = battle.format().active_per_side() > 1;
        // `pokemon.getMoveData(m.id)?.disabled` (a disable the request may
        // have hidden), as `pikapro-server` reads it: the battle's legal
        // actions don't list the move.
        let legal = battle.legal_actions(self.side).slot(slot);
        let usable = battle.state().side(self.side).active(slot).is_some_and(|m| m.has_usable_move());
        let enabled = |k: usize| {
            usable && legal.iter().any(|a| matches!(a, SlotAction::Move { index, .. } if *index as usize == k))
        };
        let old = *self;
        let Some(Some(a)) = self.active.get_mut(slot) else { return false };
        let mut refused = [SlotAction::Pass; MAX_ACTIVE];
        match choice {
            Unavailable::Switch => {
                refused[slot] = SlotAction::Switch(0);
                a.maybe_trapped = false;
                a.trapped = true;
            }
            Unavailable::Move { index } => {
                refused[slot] = SlotAction::Move { index, target: None, gimmick: None };
                // `updateDisabledRequest`.
                a.maybe_locked = false;
                if let OwnMoves::Moves(ms) = &mut a.moves {
                    if a.maybe_disabled && doubles {
                        a.maybe_disabled = false;
                        for (i, m) in ms.iter_mut().enumerate() {
                            m.disabled |= !enabled(i);
                        }
                    }
                    if ms.iter().all(|m| m.disabled) {
                        a.can_mega_evo = false;
                        a.can_terastallize = None;
                    }
                    // Then the refused move.
                    if let Some(m) = ms.get_mut(index as usize) {
                        m.disabled = true;
                    }
                }
            }
        }
        self.learn_update(&old, &refused, doubles)
    }

    /// What an `[Unavailable choice]`'s updated request taught: `self` is
    /// the update (`update: true`), `old` the request it replaces (with
    /// what earlier refusals taught), `refused` the choice Showdown refused
    /// (per slot). The one inference [`OwnOverlay::reject`] (from the
    /// battle) and a channel's session (from the updated request's text)
    /// both make:
    ///
    /// * a slot the update shows `trapped` and `old` didn't is known
    ///   trapped;
    /// * a move the update shows disabled and `old` didn't is known
    ///   disabled, and so is the refused move; in doubles, when
    ///   `maybeDisabled` went, every move shown disabled
    ///   (`updateDisabledRequest` disables all the battle won't take).
    ///
    /// [`OwnOverlay::known_trapped`] and [`OwnOverlay::known_disabled`]
    /// keep what `old` knew; [`OwnOverlay::updated`] is set when the
    /// request changed, which this returns (else Showdown answers
    /// `[Invalid choice]`, not an update).
    pub fn learn_update(&mut self, old: &OwnOverlay, refused: &[SlotAction; MAX_ACTIVE], doubles: bool) -> bool {
        (self.known_trapped, self.known_disabled, self.updated) = (old.known_trapped, old.known_disabled, old.updated);
        let answered = self.answered;
        self.answered = old.answered;
        let changed = *self != *old;
        self.answered = answered;
        for (slot, refused) in refused.iter().enumerate() {
            let (Some(o), Some(n)) = (old.active[slot], self.active[slot]) else { continue };
            if n.trapped && !o.trapped {
                self.known_trapped[slot] = true;
            }
            if let (OwnMoves::Moves(om), OwnMoves::Moves(nm)) = (o.moves, n.moves) {
                let all = o.maybe_disabled && !n.maybe_disabled && doubles;
                for (i, (a, b)) in om.iter().zip(nm.iter()).enumerate() {
                    if b.disabled && (all || !a.disabled) {
                        self.known_disabled[slot] |= 1 << i;
                    }
                }
                if let SlotAction::Move { index, .. } = *refused
                    && nm.get(index as usize).is_some_and(|m| m.disabled)
                {
                    self.known_disabled[slot] |= 1 << index;
                }
            }
        }
        self.updated |= changed;
        changed
    }

    /// What refused choices revealed about this request, as the engine's
    /// [`shown_actions`](pikapro::legal::shown_actions) takes it: the
    /// actions to offer from now on are
    /// `shown_actions(battle, side, &overlay.revealed())`.
    pub fn revealed(&self) -> Revealed {
        Revealed { trapped: self.known_trapped, disabled: self.known_disabled }
    }

    /// The Pokémon with identity `id`, with its current team position.
    pub fn by_id(&self, id: u8) -> Option<(usize, &OwnMon)> {
        self.mons.iter().enumerate().find(|(_, m)| m.id == id)
    }
}

/// A slot's move-request data as the overlay keeps it.
fn own_active(a: &ActiveRequest) -> OwnActive {
    OwnActive {
        moves: match a.moves {
            MoveChoices::Struggle => OwnMoves::Struggle,
            MoveChoices::Recharge => OwnMoves::Recharge,
            MoveChoices::Locked(m) => OwnMoves::Locked(m),
            MoveChoices::Moves(ms) => OwnMoves::Moves(ms.map(|m| OwnMove {
                id: m.id,
                pp: m.pp,
                max_pp: m.max_pp,
                target: m.target,
                disabled: m.disabled,
            })),
        },
        trapped: a.trapped,
        maybe_trapped: a.maybe_trapped,
        maybe_disabled: a.maybe_disabled,
        maybe_locked: a.maybe_locked,
        can_mega_evo: a.can_mega_evo,
        can_terastallize: a.can_terastallize,
    }
}
