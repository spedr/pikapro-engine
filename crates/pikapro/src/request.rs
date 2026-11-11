//! What the battle is waiting for from a side.
//!
//! [`Request`] is the kind of decision; [`RequestData`] is everything
//! Showdown's `|request|` JSON tells that player (`battle.getRequests`,
//! `side.getRequestData`, `pokemon.getMoveRequestData` and
//! `getSwitchRequestData`), as typed, text-free data.
//! `pikapro_protocol::request` renders it to Showdown's JSON.

use pikapro_data::{AbilityId, ItemId, MoveId, MoveTarget, SpeciesId, Stat, Stats, Type};

use crate::battle::Battle;
use crate::format::{Gimmick, Mechanics};
use crate::resolve;
use crate::state::{
    MAX_ACTIVE, MAX_MOVES, MAX_TEAM, PokemonState, Position, SideId, SideState, SlotCondition, Status, Volatiles,
};
use crate::util::StackVec;

/// What a side must decide now ([`Battle::request`]). Answer it with a
/// [`Choice`](crate::Choice) through [`Battle::submit`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Request {
    /// Nothing to decide (the other side is choosing, or the battle is over).
    #[default]
    Wait,
    /// Choose which Pokémon to bring, and their order.
    TeamPreview {
        /// How many Pokémon to bring: the format's picked team size
        /// ([`TeamPreview::Pick`](crate::TeamPreview::Pick); a smaller
        /// team brings what it has), or the side's whole team
        /// ([`TeamPreview::All`](crate::TeamPreview::All)).
        bring: u8,
    },
    /// Choose an action for every active slot.
    Move,
    /// Replace Pokémon (Showdown's `forceSwitch` request); `slots[i]` is
    /// true if slot `i` must switch. Issued at the end of a turn for fainted
    /// Pokémon, and mid-turn for a Pokémon that used U-turn, Volt Switch,
    /// ... (only that side decides; the turn resumes afterwards).
    ForceSwitch {
        /// Which active slots must switch.
        slots: [bool; MAX_ACTIVE],
    },
}

// ---------------------------------------------------------------------------
// The request's data.

/// One side's request as Showdown sends it (`battle.getRequests`): the kind
/// of decision, the per-slot move data, and the side's team.
///
/// Once the side's choice is complete (submitted or entirely automatic passes),
/// [`Battle::request`] and the legal actions say [`Request::Wait`], but this
/// still describes what Showdown sent, with [`answered`](Self::answered) set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestData {
    /// The side the request is for.
    pub side: SideId,
    /// The decision it asks for.
    pub kind: RequestKind,
    /// The side's choice is complete (submitted or entirely automatic passes)
    /// and it waits for the other side: [`Battle::request`] is [`Request::Wait`].
    pub answered: bool,
    /// Showdown's `noCancel`: every format has Cancel Mod, so it is set
    /// when this side is the only one with a decision to make.
    pub no_cancel: bool,
    /// `side.pokemon`, in Showdown's team order (`team[slot]` is the Pokémon
    /// in `slot`).
    pub pokemon: StackVec<PokemonRequest, MAX_TEAM>,
}

/// The decision a [`RequestData`] asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RequestKind {
    /// `wait: true`.
    #[default]
    Wait,
    /// `teamPreview: true` and `maxChosenTeamSize` (the Pokémon brought).
    TeamPreview {
        /// The Pokémon to bring; `None` without a picked team size
        /// ([`TeamPreview::All`](crate::TeamPreview::All): the whole team,
        /// and Showdown's request has no `maxChosenTeamSize`).
        max_chosen_team_size: Option<u8>,
    },
    /// `active`: one entry per slot, `None` for a slot no Pokémon ever
    /// filled (JSON `null`). A fainted Pokémon that was not replaced keeps
    /// its entry, as in Showdown.
    Move {
        /// One entry per active slot.
        active: StackVec<Option<ActiveRequest>, MAX_ACTIVE>,
    },
    /// `forceSwitch`: whether each slot must switch.
    ForceSwitch {
        /// One entry per active slot.
        slots: StackVec<bool, MAX_ACTIVE>,
    },
}

/// `pokemon.getMoveRequestData()` for one active slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveRequest {
    /// The moves it can choose (`moves`).
    pub moves: MoveChoices,
    /// `maybeDisabled`: the last active Pokémon may have a move a foe's
    /// Imprison disables (its request still shows it usable).
    pub maybe_disabled: bool,
    /// `maybeLocked` (Showdown sets it with `maybeDisabled`).
    pub maybe_locked: bool,
    /// `trapped`: it can't switch out.
    pub trapped: bool,
    /// `maybeTrapped`: the last active Pokémon may be trapped by a foe's
    /// ability it can't see.
    pub maybe_trapped: bool,
    /// `canMegaEvo`.
    pub can_mega_evo: bool,
    /// `canTerastallize`: the Tera type it can Terastallize into.
    pub can_terastallize: Option<Type>,
}

/// The `moves` of an [`ActiveRequest`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MoveChoices {
    /// No move can be chosen: `[{move: "Struggle", id: "struggle", target:
    /// "randomNormal", disabled: false}]`.
    #[default]
    Struggle,
    /// Recharging: `[{move: "Recharge", id: "recharge"}]`.
    Recharge,
    /// Locked into a move (Outrage, a charging Solar Beam, Uproar, ...):
    /// `[{move, id}]` only.
    Locked(MoveId),
    /// Each move slot.
    Moves(StackVec<MoveRequest, MAX_MOVES>),
}

/// One move slot of a move request (`getMoves`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveRequest {
    /// The move.
    pub id: MoveId,
    /// PP left.
    pub pp: u8,
    /// Its maximum PP.
    pub max_pp: u8,
    /// The move's target type now (Curse from a non-Ghost targets the user,
    /// Pollen Puff under Heal Block, Terapagos-Stellar's Tera Starstorm).
    pub target: MoveTarget,
    /// It can't be chosen now (Disable, Taunt, a Choice lock, no PP, ...).
    pub disabled: bool,
}

impl Default for MoveRequest {
    fn default() -> MoveRequest {
        MoveRequest { id: MoveId::default(), pp: 0, max_pp: 0, target: MoveTarget::Normal, disabled: false }
    }
}

/// One Pokémon of `side.pokemon` (`getSwitchRequestData`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PokemonRequest {
    /// Stable identity ([`PokemonState::id`]), e.g. for a nickname table.
    pub id: u8,
    /// Showdown's `baseSpecies`, shown in `details`: the set's species, or
    /// the forme a permanent forme change made (Mega Evolution, ...).
    pub species: SpeciesId,
    /// Its level in battle.
    pub level: u8,
    /// Current HP (0 once fainted).
    pub hp: u16,
    /// Maximum HP.
    pub max_hp: u16,
    /// Its non-volatile status.
    pub status: Status,
    /// `active`: it stands in a slot (`position < side.active.length`),
    /// fainted or not; during team preview, the first Pokémon registered.
    pub active: bool,
    /// Showdown's `baseStoredStats`: the current forme's stats from the set
    /// (Power Trick, Guard Split and Transform don't change them). `Hp` is
    /// not part of the request.
    pub stats: Stats<u16>,
    /// `moves`: its move slots now (Transform, Mimic and Sketch change them).
    pub moves: StackVec<MoveId, MAX_MOVES>,
    /// `baseAbility`: the ability it has out of battle (a forme change can
    /// change it).
    pub base_ability: AbilityId,
    /// Its held item, if any.
    pub item: Option<ItemId>,
    /// `ability`: its ability now (Skill Swap, Trace, ... change it).
    pub ability: AbilityId,
    /// `commanding`: a Tatsugiri inside a Dondozo.
    pub commanding: bool,
    /// `reviving`: its slot has Revival Blessing.
    pub reviving: bool,
    /// `teraType`: the type it would Terastallize into.
    pub tera_type: Type,
    /// `terastallized`: the type it Terastallized into.
    pub terastallized: Option<Type>,
    /// Its gender, shown in `details` (its own, even under Illusion).
    pub gender: crate::team::Gender,
}

impl Default for PokemonRequest {
    fn default() -> PokemonRequest {
        PokemonRequest {
            id: 0,
            species: SpeciesId::default(),
            level: 0,
            hp: 0,
            max_hp: 0,
            status: Status::None,
            active: false,
            stats: Stats::default(),
            moves: StackVec::new(),
            base_ability: AbilityId::default(),
            item: None,
            ability: AbilityId::default(),
            commanding: false,
            reviving: false,
            tera_type: Type::Normal,
            terastallized: None,
            gender: crate::team::Gender::Genderless,
        }
    }
}

impl Battle {
    /// The data of the `|request|` Showdown sends `side` for its current
    /// request (`battle.getRequests`). Pure: it reads the state and never
    /// draws. After `side` has submitted, it still describes the request
    /// that was answered, with [`RequestData::answered`] set (while
    /// [`Battle::request`] says [`Request::Wait`]).
    pub fn request_data(&self, side: SideId) -> RequestData {
        let requests = self.requests;
        let kind = match requests[side.index()] {
            Request::Wait => RequestKind::Wait,
            Request::TeamPreview { .. } => {
                RequestKind::TeamPreview { max_chosen_team_size: self.format.team_preview.picked() }
            }
            Request::Move => RequestKind::Move {
                active: (0..self.format.active_per_side()).map(|slot| self.active_request(side, slot)).collect(),
            },
            Request::ForceSwitch { slots } => {
                RequestKind::ForceSwitch { slots: slots[..self.format.active_per_side()].iter().copied().collect() }
            }
        };
        // `if (!this.supportCancel || !multipleRequestsExist) noCancel`.
        let others = requests[side.foe().index()] != Request::Wait;
        let no_cancel = kind != RequestKind::Wait && !others;
        let s = self.state.side(side);
        let pokemon = (0..s.team.len()).map(|i| self.pokemon_request(s, i)).collect();
        let answered = kind != RequestKind::Wait && self.request(side) == Request::Wait;
        RequestData { side, kind, answered, no_cancel, pokemon }
    }

    /// `getSwitchRequestData` for `side.team[index]`.
    fn pokemon_request(&self, s: &SideState, index: usize) -> PokemonRequest {
        let mon = &s.team[index];
        let active = index < self.format.active_per_side();
        let on_field = active && s.active(index).is_some();
        PokemonRequest {
            id: mon.id,
            species: mon.base_species,
            level: mon.level,
            hp: mon.hp,
            max_hp: mon.max_hp(),
            // `getHealth().secret` is `0 fnt` for a fainted Pokémon: no
            // status shows (its status stays until `checkFainted`).
            status: if mon.hp == 0 { Status::None } else { mon.status },
            active,
            stats: self.base_stored_stats(mon),
            moves: mon.moves.iter().map(|m| m.id).collect(),
            base_ability: mon.base_ability,
            item: mon.item,
            ability: mon.ability,
            // `!!this.volatiles['commanding'] && !this.fainted`.
            commanding: mon.volatiles.has(Volatiles::COMMANDING) && !mon.is_fainted(),
            // `this.isActive && !!side.slotConditions[position]['revivalblessing']`.
            reviving: on_field && s.conditions.slots[index].has(SlotCondition::RevivalBlessing),
            tera_type: self.shown_tera_type(mon),
            terastallized: mon.terastallized.then_some(mon.tera_type),
            gender: mon.gender,
        }
    }

    /// The Tera type a request shows. `getSwitchRequestData` leaves
    /// `teraType` out when `dex.currentMod.startsWith('champions')` (the
    /// mod, not the format's gimmicks: a Gen 9 format without
    /// Terastallization still sends the set's). Without it, the species'
    /// first type, which is all a client can tell, so that the request (and
    /// the views built from it) hold only what Showdown sends.
    pub fn shown_tera_type(&self, mon: &PokemonState) -> Type {
        if self.format.mechanics == Mechanics::Champions {
            self.format.dex.species(mon.base_species).types.primary
        } else {
            mon.tera_type
        }
    }

    /// Showdown's `baseStoredStats`: `setSpecies` sets them to the new
    /// forme's stats from the set, except for Transform.
    fn base_stored_stats(&self, mon: &PokemonState) -> Stats<u16> {
        // A transformed Pokémon keeps the forme it transformed in (a
        // temporary one included: Minior-Meteor).
        let species = match mon.volatile_counters.transform {
            Some(base) if mon.volatiles.has(Volatiles::TRANSFORMED) => base.own_species,
            _ => mon.species,
        };
        let base = self.format.dex.species(species).base_stats;
        let mut stats = mon.set_stats;
        for stat in [Stat::Atk, Stat::Def, Stat::SpA, Stat::SpD, Stat::Spe] {
            stats[stat] =
                crate::stats::calc_stat(stat, base[stat], mon.ivs[stat], mon.evs[stat], mon.level, mon.nature);
        }
        stats
    }

    /// The request issued to `side`, whether or not it has answered it
    /// ([`Battle::request`] says [`Request::Wait`] once it has): what
    /// [`RequestData::kind`] describes.
    pub fn issued_request(&self, side: SideId) -> Request {
        self.requests[side.index()]
    }

    /// `pokemon.getMoveRequestData()` for the Pokémon in `slot` (`None`
    /// when no Pokémon ever stood there, or `slot` isn't an active slot of
    /// the format): one entry of a move request's [`RequestKind::Move`],
    /// without building the rest of [`Battle::request_data`] (for callers
    /// that read the Pokémon from the state).
    pub fn active_request(&self, side: SideId, slot: usize) -> Option<ActiveRequest> {
        if slot >= self.format.active_per_side() {
            return None;
        }
        let s = self.state.side(side);
        let mon = s.team.get(slot)?;
        let pos = Position::new(side, slot as u8);
        let alive = s.active(slot).is_some();
        // `getLockedMove()`: `priorityEvent('LockMove')`.
        let locked = if !alive {
            None
        } else if mon.volatiles.has(Volatiles::MUST_RECHARGE) {
            Some(MoveChoices::Recharge)
        } else {
            resolve::locked_move(self, pos).map(MoveChoices::Locked)
        };
        let hard_locked = locked.is_some();
        // `isLastActive()`: active, and no later slot has a Pokémon that
        // has not fainted.
        let last_active = alive && (slot + 1..self.format.active_per_side()).all(|i| s.active(i).is_none());
        // `battle.canSwitch(side)`: a Pokémon that has not fainted outside
        // the active slots.
        let can_switch_in = s.team.iter().skip(self.format.active_per_side()).any(|p| !p.is_fainted());
        let moves = locked.unwrap_or_else(|| self.move_choices(pos, mon, alive, last_active));
        let mut data = ActiveRequest { moves, ..ActiveRequest::default() };
        if hard_locked || !last_active {
            // `this.trapped` (a locked Pokémon is trapped); a hidden trap
            // (a foe's Shadow Tag) shows too.
            let trapped = hard_locked || alive && resolve::is_trapped(self, pos);
            data.trapped = (hard_locked || can_switch_in) && trapped;
        } else {
            // `maybeLocked = maybeLocked || maybeDisabled`: only Imprison
            // sets either, and it sets `maybeDisabled`.
            data.maybe_disabled = self.foe_imprisons(pos);
            data.maybe_locked = data.maybe_disabled;
            if can_switch_in {
                if resolve::is_visibly_trapped(self, pos) {
                    data.trapped = true;
                } else {
                    data.maybe_trapped = self.maybe_trapped(pos);
                }
            }
        }
        // `if (!lockedMove)`: Struggle counts as one.
        if matches!(data.moves, MoveChoices::Moves(_)) {
            // Pokemon.getMoveRequestData trusts canMegaEvo, including a
            // supplied Past Mega Stone in a Gen 9 format.
            data.can_mega_evo = mon.can_mega_evo.is_some();
            let tera = self.format.gimmicks.contains(Gimmick::Terastallize)
                && resolve::tera::can_terastallize(&self.format, s, mon);
            data.can_terastallize = tera.then_some(mon.tera_type);
        }
        Some(data)
    }

    /// `getMoves(null, isLastActive)`: every move slot, or Struggle when
    /// none can be chosen. A move only a foe's Imprison disables (a hidden
    /// disable) shows as usable to the last active Pokémon.
    fn move_choices(&self, pos: Position, mon: &PokemonState, alive: bool, last_active: bool) -> MoveChoices {
        let dex = self.format.dex;
        let mut moves = StackVec::<MoveRequest, MAX_MOVES>::new();
        // `request_shows_usable` of each move, at once.
        let usable = if alive { resolve::request_shows_usable_moves(self, pos) } else { 0 };
        for (i, m) in mon.moves.iter().enumerate() {
            let disabled = if alive {
                usable & (1 << i) == 0 || !last_active && m.disabled
            } else {
                self.fainted_move_disabled(mon, m.id) || m.pp == 0
            };
            let target = resolve::request_target(mon, m.id, m.target.unwrap_or(dex.move_(m.id).target));
            moves.push(MoveRequest { id: m.id, pp: m.pp, max_pp: m.max_pp, target, disabled });
        }
        if moves.iter().all(|m| m.disabled) { MoveChoices::Struggle } else { MoveChoices::Moves(moves) }
    }

    /// `moveSlot.disabled` of a fainted Pokémon still in its slot: the end
    /// of the turn reset it and ran `DisableMove` on it. For a Pokémon that
    /// is not active, `findEventHandlers` collects only the field's
    /// handlers (Gravity), not its own or its foes' (no Imprison); then each
    /// move's own `onDisableMove` runs (Belch, Stuff Cheeks; the Champions
    /// mod's Fake Out and First Impression). `lastMove` is gone, so no
    /// `cantusetwice`.
    fn fainted_move_disabled(&self, mon: &PokemonState, move_id: MoveId) -> bool {
        use pikapro_data::MoveFlags;
        let data = self.format.dex.move_(move_id);
        resolve::gravity(self) && data.flags.contains(MoveFlags::GRAVITY)
            || resolve::own_disable_move(&self.format, mon, move_id)
    }

    /// The foes of `pos` that have not fainted (`pokemon.foes()`).
    fn foes(&self, pos: Position) -> impl Iterator<Item = &PokemonState> + '_ {
        let foes = self.state.side(pos.side.foe());
        (0..self.format.active_per_side()).filter_map(move |i| foes.active(i))
    }

    /// Imprison's `onFoeDisableMove` ran for the Pokémon at the end of the
    /// turn: it sets `maybeDisabled` whatever the moves.
    fn foe_imprisons(&self, pos: Position) -> bool {
        self.foes(pos).any(|f| f.volatiles.has(Volatiles::IMPRISON))
    }

    /// `maybeTrapped` as the end of the turn left it: `runEvent
    /// ('MaybeTrapPokemon')` (unless it is a known Ghost), whose handlers are
    /// the foes' Shadow Tag, Arena Trap and Magnet Pull and its own Shed
    /// Shell (priority -10, last), then `FoeMaybeTrapPokemon` for every
    /// ability of each foe's species (`endTurn`). The event's sort draws;
    /// the engine runs it at the end of the turn (`trap_pokemon_event`), so
    /// this only reads the outcome.
    fn maybe_trapped(&self, pos: Position) -> bool {
        let Some(mon) = self.state.at(pos) else { return false };
        let known = !mon.volatile_counters.type_unknown;
        let ghost = resolve::manipulation::has_type(mon, Type::Ghost);
        // The foes standing: an index loop (a filtered range iterator
        // cost more).
        let foe_side = pos.side.foe();
        let active = self.format.active_per_side() as u8;
        let mut maybe = false;
        if !known || !ghost {
            for i in 0..active {
                let foe = Position::new(foe_side, i);
                let Some(source) = self.state.at(foe) else { continue };
                let ability = source.ability;
                // (The cheap check first: both only read.)
                if self.foe_maybe_traps(pos, ability) && resolve::abilities::has_ability(self, foe, ability) {
                    maybe = true;
                }
            }
            // Shed Shell's `onMaybeTrapPokemon`, and Champions' Run Away's
            // (its dex's Run Away has the handler).
            if maybe && resolve::items::has_item(self, pos, pikapro_data::gen9::items::SHED_SHELL) {
                maybe = false;
            }
            let run_away = pikapro_data::gen9::abilities::RUN_AWAY;
            if maybe
                && self.format.dex.ability(run_away).events.contains(pikapro_data::Event::MaybeTrapPokemon)
                && resolve::abilities::has_ability(self, pos, run_away)
            {
                maybe = false;
            }
        }
        // `for (const source of pokemon.foes())`: each ability of the foe's
        // species (or the one its Illusion shows, or the one it transformed
        // into), whether or not it has it (Showdown compares the ability's
        // name with the Pokémon's ability ID, so none is skipped), when the
        // format has Obtainable Abilities (`Format::obtainable_abilities`).
        // A `singleEvent` on the trapped Pokémon: nothing when its own
        // ability is suppressed.
        // (Its own ability's suppression is looked at only for an ability
        // that would trap: both only read.)
        // (A known Ghost is never trapped: the loop reads only, so it is
        // skipped whole.)
        if self.format.obtainable_abilities && !(known && ghost) {
            for i in 0..active {
                let foe = Position::new(foe_side, i);
                let Some(source) = self.state.at(foe) else { continue };
                let shown = source.illusion.and_then(|id| self.state.side(foe.side).team.iter().find(|m| m.id == id));
                // `(source.illusion || source).species`: the species it
                // transformed into, if it did.
                let slots = self.format.dex.species(resolve::tera::species_of(shown.unwrap_or(source))).abilities;
                for ability in [Some(slots.primary), slots.secondary, slots.hidden, slots.special].into_iter().flatten()
                {
                    // `if (ruleTable.has('-ability:' + ability.id)) continue;`
                    // (`banned_trapping_abilities` is empty in most formats.)
                    if self.format.banned_trapping_abilities.contains(&ability) {
                        continue;
                    }
                    if self.foe_maybe_traps(pos, ability) && !resolve::abilities::ignoring_ability(self, pos) {
                        maybe = true;
                    }
                }
            }
        }
        maybe
    }

    /// A foe's `onFoeMaybeTrapPokemon` for `ability`: whether it sets the
    /// Pokémon at `pos`'s `maybeTrapped` (every foe is adjacent in these
    /// formats).
    fn foe_maybe_traps(&self, pos: Position, ability: AbilityId) -> bool {
        use pikapro_data::gen9::abilities as a;
        // (The ability first: most foes' trap nothing.)
        if !matches!(ability, a::ARENA_TRAP | a::MAGNET_PULL | a::SHADOW_TAG) {
            return false;
        }
        let Some(mon) = self.state.at(pos) else { return false };
        let unknown = mon.volatile_counters.type_unknown;
        match ability {
            // `pokemon.isGrounded(!pokemon.knownType)`.
            a::ARENA_TRAP => resolve::grounded(self, pos, unknown) == Some(true),
            a::MAGNET_PULL => unknown || resolve::manipulation::has_type(mon, Type::Steel),
            a::SHADOW_TAG => !resolve::abilities::has_ability(self, pos, a::SHADOW_TAG),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RequestKind;
    use crate::testing::sample_team;
    use crate::{Battle, BattleOptions, Format, Request, Seed, SideId};

    #[test]
    fn an_answered_request_is_flagged() {
        let format = Format::gen9_vgc();
        let teams = [sample_team(&format), sample_team(&format)];
        let mut battle = Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap();
        let data = battle.request_data(SideId::P1);
        assert_eq!(data.kind, RequestKind::TeamPreview { max_chosen_team_size: Some(4) });
        assert!(!data.answered);
        let choice = battle.legal_actions(SideId::P1).joint()[0];
        battle.submit(SideId::P1, choice).unwrap();
        assert_eq!(battle.request(SideId::P1), Request::Wait);
        let data = battle.request_data(SideId::P1);
        assert!(data.answered, "{data:?}");
        assert_eq!(data.kind, RequestKind::TeamPreview { max_chosen_team_size: Some(4) });
        assert!(!battle.request_data(SideId::P2).answered);
        let choice = battle.legal_actions(SideId::P2).joint()[0];
        battle.submit(SideId::P2, choice).unwrap();
        assert!(SideId::BOTH.iter().all(|&s| !battle.request_data(s).answered));
    }

    /// Only the format's active slots have an entry: a benched Pokémon
    /// has none.
    #[test]
    fn active_request_is_for_active_slots() {
        for format in [Format::gen9_singles(), Format::gen9_doubles()] {
            let active = format.active_per_side();
            let teams = [sample_team(&format), sample_team(&format)];
            let battle = Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap();
            for slot in 0..6 {
                assert_eq!(battle.active_request(SideId::P1, slot).is_some(), slot < active, "slot {slot}");
            }
        }
    }
}
