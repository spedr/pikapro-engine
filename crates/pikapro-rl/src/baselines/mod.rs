//! poke-env's baseline players, ported onto the player view: the built-in
//! opponents and yardsticks. They are faithful to
//! poke-env 0.16.1's `player/baselines.py`, so results compare with
//! poke-env's; they are not meant to be strong.
//!
//! * [`RandomPolicy`](crate::policy::RandomPolicy): `RandomPlayer`.
//! * [`MaxBasePowerPolicy`]: `MaxBasePowerPlayer`, singles and doubles
//!   (`choose_doubles_move`).
//! * [`SimpleHeuristicsPolicy`]: `SimpleHeuristicsPlayer`: matchups,
//!   switching out of bad ones, entry hazards and their removal, damage
//!   estimates, Terastallization; in doubles the singles logic per slot and
//!   foe (poke-env's `PseudoBattle`), then its Commander and forced-switch
//!   passes and `join_orders`.
//!
//! They read only a [`PlayerView`]: what the player's request and the
//! public log show, never the battle.
//!
//! # Randomness
//!
//! poke-env draws from Python's `random` at fixed points (a random target,
//! a random switch, `choose_random_move`, the team preview order). The
//! ports draw at the same points, with the same distributions, from the
//! `rng` [`Policy::choose`](crate::policy::Policy::choose) gets. Ties are broken as poke-env breaks them:
//! Python's `max` and `min` keep the first best element, in poke-env's
//! list order (moves in request order, switches in team order). Scores are
//! computed in `f64` in poke-env's operation order, so they are the same
//! floats and tie the same way.
//!
//! # The honest mask
//!
//! The engine's [`LegalActions`] can be stricter than the request: a
//! Pokémon trapped by a foe's ability nobody has seen yet is only
//! `maybe_trapped` in its request, but has no switches in `legal`; a move a
//! foe's Imprison disables is `maybe_disabled`. The policies never read
//! `legal` to decide: every candidate comes from the request
//! ([`OwnOverlay`](pikapro_view::OwnOverlay)), as poke-env's `available_moves`
//! and `available_switches` do. When the choice turns out illegal, Showdown
//! answers `[Unavailable choice]` and sends the request again with the
//! Pokémon `trapped` (or the move disabled), and poke-env chooses again;
//! the ports do the same: the rejected switch or move is removed from the
//! request and the policy chooses again. `legal` is used only to check a
//! finished choice, and to fill in the moves the engine locks the user
//! into (Showdown ignores the move named then).
//!
//! # Deviations from poke-env
//!
//! * **Mega Evolution:** poke-env's players never Mega Evolve. Here a
//!   Pokémon that can Mega Evolve does so with the move it chooses (in
//!   doubles, one per turn, the first slot's). This is the one deliberate
//!   deviation: in championsvgc a Mega left unused would make the
//!   baselines far weaker than any real player.
//! * **Data:** the format's dex (the Champions dex in championsvgc).
//!   poke-env reads its Gen 9 data in every Gen 9 format, so a move or
//!   species Champions changed differs.
//! * **A commanding Tatsugiri passes** in every policy (Showdown skips it).
//!   poke-env's `SimpleHeuristicsPlayer` passes too; its
//!   `MaxBasePowerPlayer` sends a random switch for it, which Showdown then
//!   applies to the next slot.
//!
//! poke-env's own quirks are kept, since they shape how the baselines play:
//!
//! * `SimpleHeuristicsPlayer` never uses a setup move: it compares
//!   `move.target` (an enum) with the string `"self"`, which is never
//!   equal. [`SimpleHeuristicsPolicy::with_setup_moves`] turns them on.
//! * Its stat estimate counts a +1 stage as ×2 (`2 / (2 - 1)`).
//! * It only Terastallizes when it knows its own Tera type, which poke-env
//!   learns from an Open Team Sheet (`|showteam|`) and never from the
//!   request: with closed sheets, never.
//!   [`SimpleHeuristicsPolicy::with_request_tera_type`] reads it from the
//!   request.
//! * In doubles its `get_double_target_multiplier` is computed and then
//!   unused, and a slot's order is the best of its two per-foe orders by
//!   the unscaled score.
//! * In singles, after a double KO, it matches the replacements against
//!   the fainted foe, whose Tera type poke-env keeps. The view drops a
//!   fainted Pokémon's Tera flag, as Showdown's client does; the port
//!   counts a fainted foe with a revealed Tera type as Terastallized.
//!   The same holds for its own fainted Pokémon, which a Revival Blessing
//!   chooses among: Showdown's request drops their Tera, poke-env keeps it.
//! * Any attack on a Pokémon whose first type is `???` (after Double Shock
//!   or Burn Up) is neutral, whatever its second type
//!   (`PokemonType.damage_multiplier` returns 1 first).
//!
//! # Parity
//!
//! `tools/baseline-parity/` compares the ports with poke-env's players
//! decision by decision. With the complete view, over 200 battles per
//! format:
//! * `MaxBasePowerPolicy` agrees on every decision whose outcome doesn't
//!   depend on poke-env's randomness, and on every move where only the
//!   random target does, except for a few in championsvgc (1,820 of 1,826
//!   of those: Champions' changed base powers) and one in gen9vgc
//!   (poke-env's Illusion tracking).
//! * `SimpleHeuristicsPolicy` agrees on all 9,017 such decisions in
//!   gen9singles, all 3,754 in gen9doubles and all 2,519 in gen9vgc, and on
//!   2,274 of 2,284 in championsvgc.
//!
//! The remaining gaps are Champions' changed base powers, foes that have
//! Mega Evolved (poke-env keeps their base forme) and poke-env's Illusion
//! tracking. The tool's README explains each one.
//!
//! # RandomPolicy
//!
//! [`RandomPolicy`](crate::policy::RandomPolicy) samples uniformly from
//! `legal`, as poke-env's `RandomPlayer` samples uniformly from its
//! `valid_orders` (singles) or their `join_orders` (doubles). In the vector
//! environment and in [`eval::play`](crate::eval::play) `legal` is what
//! the request shows, as poke-env's candidates are; with the engine's
//! legal set it would leave out a switch out of a hidden trap and a move a
//! hidden Imprison disables, which poke-env picks, is refused and picks
//! again. In doubles the engine lists a target per foe
//! present, where poke-env also lists targets at an empty slot a fainted
//! foe left. poke-env's team preview order is uniform too.

mod heuristics;
mod max_power;

use pikapro::state::SideCondition;
use pikapro::util::StackVec;
use pikapro::{BattleRng, Choice, ChoiceError, Gimmick, LegalActions, MAX_ACTIVE, MAX_MOVES, MAX_TEAM, Request};
use pikapro::{SlotAction, Target};
use pikapro_data::gen9::moves;
use pikapro_data::{BoostId, Dex, Effectiveness, MoveCategory, MoveData, MoveId, MoveTarget, SpeciesId, Type, TypeSet};
use pikapro_view::{
    ActiveView, MonFlags, MonView, OwnMon, OwnMoves, OwnRequest, PlayerView, PublicSide, PublicVolatiles,
};

pub use heuristics::SimpleHeuristicsPolicy;
pub use max_power::MaxBasePowerPolicy;

#[cfg(test)]
mod tests;

/// poke-env's target positions (`DoubleBattle`): foes `1` and `2`, own
/// slots `-1` and `-2`, `0` for none.
pub(crate) type Pos = i8;

/// A move as poke-env's `Move` sees it: its data entry.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MoveInfo {
    pub id: MoveId,
    /// `None` for `recharge`, which has no entry (poke-env's stand-in: 0
    /// base power, Normal, Special, accuracy 1, no target).
    pub data: Option<&'static MoveData>,
}

impl MoveInfo {
    fn new(dex: &'static Dex, id: MoveId) -> MoveInfo {
        MoveInfo { id, data: Some(dex.move_(id)) }
    }

    const RECHARGE: MoveInfo = MoveInfo { id: MoveId(0), data: None };

    fn is_recharge(&self) -> bool {
        self.data.is_none()
    }

    pub fn base_power(&self) -> u32 {
        self.data.map_or(0, |d| u32::from(d.base_power))
    }

    pub fn ty(&self) -> Type {
        self.data.map_or(Type::Normal, |d| d.ty)
    }

    pub fn category(&self) -> MoveCategory {
        self.data.map_or(MoveCategory::Special, |d| d.category)
    }

    /// `move.accuracy`: `accuracy / 100`, 1 for a move that can't miss
    /// (and `recharge`'s entry says 1, so 0.01).
    pub fn accuracy(&self) -> f64 {
        match self.data {
            None => 1.0 / 100.0,
            Some(d) => d.accuracy.map_or(1.0, |a| f64::from(a) / 100.0),
        }
    }

    /// `move.expected_hits`.
    pub fn expected_hits(&self) -> f64 {
        if self.id == moves::TRIPLE_KICK || self.id == moves::TRIPLE_AXEL {
            return 1.0 + 2.0 * 0.9 + 3.0 * 0.81;
        }
        match self.data.and_then(|d| d.multihit) {
            None => 1.0,
            Some((lo, hi)) if lo == hi => f64::from(lo),
            // poke-env asserts 2-5 hits here.
            Some(_) => (2.0 + 3.0) / 3.0 + (4.0 + 5.0) / 6.0,
        }
    }

    /// `move.target` (the data's; poke-env never reads the request's).
    pub fn target(&self) -> Option<MoveTarget> {
        self.data.map(|d| d.target)
    }
}

/// One of `available_moves`: a move and the move slot that uses it
/// (`None` for a locked move, `recharge` or Struggle, which the engine
/// lists as move slot 0).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Avail {
    pub info: MoveInfo,
    pub index: Option<u8>,
}

/// A slot's order, as poke-env's `SingleBattleOrder` holds it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum Order {
    /// `/choose move ...`: the move, its target ([`Pos`]) and gimmick.
    Move { mv: AvailKey, target: Pos, tera: bool, mega: bool },
    /// `/choose switch ...`: to the Pokémon at this team position.
    Switch(u8),
    /// `/choose pass`.
    #[default]
    Pass,
    /// `/choose default`: Showdown's `autoChoose` for the slot.
    Default,
}

/// Identifies an [`Avail`] in an [`Order`] (poke-env compares orders by
/// their fields; two orders of one move share the `Move` object).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AvailKey {
    pub id: MoveId,
    pub index: Option<u8>,
    pub recharge: bool,
}

impl Avail {
    fn key(&self) -> AvailKey {
        AvailKey { id: self.info.id, index: self.index, recharge: self.info.is_recharge() }
    }

    fn order(&self, target: Pos, tera: bool, mega: bool) -> Order {
        Order::Move { mv: self.key(), target, tera, mega }
    }
}

impl Order {
    fn is_tera(&self) -> bool {
        matches!(self, Order::Move { tera: true, .. })
    }

    fn is_mega(&self) -> bool {
        matches!(self, Order::Move { mega: true, .. })
    }
}

/// What a re-sent request takes away after a rejected choice (Showdown's
/// `[Unavailable choice]`): the slot is trapped, or the move disabled.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Restrict {
    pub no_switch: [bool; MAX_ACTIVE],
    pub disabled: [[bool; MAX_MOVES]; MAX_ACTIVE],
}

/// What poke-env's battle object says about one active slot: its
/// `available_moves`, `available_switches`, `force_switch`, `trapped` and
/// gimmicks, all from the request.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SlotCtx {
    pub moves: StackVec<Avail, MAX_MOVES>,
    /// Team positions, in request order.
    pub switches: StackVec<u8, MAX_TEAM>,
    pub force_switch: bool,
    pub trapped: bool,
    pub can_tera: bool,
    pub can_mega: bool,
    /// A Tatsugiri inside its Dondozo (poke-env's `Effect.COMMANDER`).
    pub commanding: bool,
}

/// What poke-env's `Pokemon` says about a Pokémon, for the baselines.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mon {
    /// `types`: `type_1` (the Tera type once Terastallized), then `type_2`.
    pub t1: Type,
    pub t2: Option<Type>,
    /// `base_stats`: hp, atk, def, spa, spd, spe.
    pub base: [u8; 6],
    /// `boosts`, by [`BoostId`].
    pub boosts: [i8; 7],
    /// `current_hp_fraction`.
    pub hp: f64,
    /// `stats` (own Pokémon only): atk, def, spa, spd, spe.
    pub stats: [u16; 5],
    /// `tera_type` as poke-env knows it.
    pub tera_type: Option<Type>,
    pub terastallized: bool,
}

impl Mon {
    pub fn types(&self) -> impl Iterator<Item = Type> + '_ {
        core::iter::once(self.t1).chain(self.t2)
    }

    pub fn has_type(&self, t: Type) -> bool {
        self.t1 == t || self.t2 == Some(t)
    }

    /// `damage_multiplier(type)`: an attack of type `attack` on it.
    pub fn damage_multiplier(&self, attack: Type) -> f64 {
        damage_multiplier(attack, self.t1, self.t2)
    }

    pub fn boost(&self, stat: BoostId) -> i8 {
        self.boosts[stat as usize]
    }
}

/// `PokemonType.damage_multiplier`: `attack` on types `t1`/`t2`. Stellar
/// and `???` are neutral both ways, and a Stellar or `???` `type_1` makes
/// everything neutral: poke-env returns 1 before it looks at `type_2`, so
/// after Double Shock or Burn Up (`[???, Fighting]`, say) any attack is
/// neutral, whatever the second type.
pub(crate) fn damage_multiplier(attack: Type, t1: Type, t2: Option<Type>) -> f64 {
    let neutral = |t: Type| t == Type::Stellar || t == Type::Unknown;
    if neutral(attack) || neutral(t1) {
        return 1.0;
    }
    // The dex's type chart; at most two types, so `n` is in -2..=2.
    match attack.against_set(TypeSet { primary: t1, secondary: t2 }) {
        Effectiveness::Immune => 0.0,
        Effectiveness::Exp(n) => f64::from(1u8 << (n + 2).clamp(0, 4)) / 4.0,
    }
}

/// The view as poke-env's battle object presents it to the baselines.
pub(crate) struct Ctx<'v, 'a> {
    pub view: &'v PlayerView<'a>,
    pub dex: &'static Dex,
    pub doubles: bool,
    /// poke-env knows its own Tera types (an Open Team Sheet was shown, or
    /// the policy reads them from the request).
    pub tera_known: bool,
}

impl<'v, 'a> Ctx<'v, 'a> {
    pub fn new(view: &'v PlayerView<'a>, request_tera: bool) -> Ctx<'v, 'a> {
        let config = view.public.config;
        Ctx {
            view,
            dex: config.dex,
            doubles: config.active > 1,
            tera_known: request_tera || view.public.ots[view.me.index()].is_some(),
        }
    }

    fn own(&self) -> &'v pikapro_view::OwnOverlay {
        self.view.own
    }

    /// `battle.force_switch`, per slot.
    pub fn force_switch(&self) -> [bool; MAX_ACTIVE] {
        match self.own().request {
            OwnRequest::ForceSwitch { slots } => slots,
            _ => [false; MAX_ACTIVE],
        }
    }

    /// The public record of the player's own Pokémon `m`.
    fn own_record(&self, m: &OwnMon) -> Option<&'v MonView> {
        let row = self.view.own_row(m.id)?;
        self.view.my_side().mon(row)
    }

    /// Own Pokémon at team position `pos` (benched: no boosts).
    pub fn own_mon(&self, pos: usize) -> Mon {
        self.own_mon_in(pos, None)
    }

    fn own_mon_in(&self, pos: usize, slot: Option<&ActiveView>) -> Mon {
        let m = &self.own().mons[pos];
        let record = self.own_record(m);
        // poke-env keeps the species the request's details show, changed
        // by `-formechange` while it is out: the public record's once it
        // has been seen.
        let species = record.filter(|r| r.is_revealed()).map_or(m.species, |r| r.species);
        // Showdown's request drops a fainted Pokémon's Tera (`faint`
        // deletes `terastallized`), but poke-env's `faint()` keeps it: a
        // Revival Blessing's candidates are scored with the Tera type they
        // fainted with, as fainted foes are (`foe_active`). With closed
        // sheets the public record knows a Tera type only from its
        // `-terastallize`.
        let me = self.view.me.index();
        let tera = m.terastallized.or_else(|| {
            let fainted_tera = record.filter(|r| r.is_fainted() && self.view.public.ots[me].is_none());
            fainted_tera.and_then(|r| r.tera_type)
        });
        let known_tera = tera.or(if self.tera_known { Some(m.tera_type) } else { None });
        let mut mon = self.mon(species, slot, tera, known_tera);
        mon.hp = if m.hp == 0 || m.max_hp == 0 { 0.0 } else { f64::from(m.hp) / f64::from(m.max_hp) };
        mon.stats = m.stats;
        mon
    }

    /// `battle.active_pokemon` (singles) or `battle.active_pokemon[slot]`
    /// (doubles, where a fainted Pokémon is `None`).
    pub fn own_active(&self, slot: usize) -> Option<Mon> {
        let m = self.own().mons.get(slot).filter(|m| m.active)?;
        if self.doubles && m.hp == 0 {
            return None;
        }
        Some(self.own_mon_in(slot, Some(&self.view.my_side().slots[slot])))
    }

    /// `battle.opponent_active_pokemon` (singles: kept while fainted) or
    /// `[slot]` (doubles: `None` once fainted).
    pub fn foe_active(&self, slot: usize) -> Option<Mon> {
        let side = self.view.foe_side();
        let (_, record) = side.active(slot)?;
        if self.doubles && record.is_fainted() {
            return None;
        }
        // poke-env's `faint()` keeps the Tera type (the view drops the
        // flag, as Showdown's client does), and a singles battle still
        // matches replacements against the fainted foe. With closed sheets
        // a fainted foe's Tera type can only come from its
        // `-terastallize`; with open ones this can't be told.
        let foe = self.view.me.foe().index();
        let terastallized = record.flags.has(MonFlags::TERASTALLIZED)
            || record.is_fainted() && record.tera_type.is_some() && self.view.public.ots[foe].is_none();
        let tera = if terastallized { record.tera_type } else { None };
        let mut mon = self.mon(record.species, Some(&side.slots[slot]), tera, record.tera_type);
        mon.hp = if record.hp.hp == 0 || record.hp.max == 0 {
            0.0
        } else {
            f64::from(record.hp.hp) / f64::from(record.hp.max)
        };
        Some(mon)
    }

    /// A Pokémon of `species` in `slot` (boosts, changed types, Transform),
    /// Terastallized into `tera`.
    fn mon(&self, species: SpeciesId, slot: Option<&ActiveView>, tera: Option<Type>, known: Option<Type>) -> Mon {
        let transformed = slot.and_then(|s| s.transformed).map(|t| self.dex.species(t.species));
        let data = self.dex.species(species);
        let base = transformed.unwrap_or(data).base_stats.0;
        let (t1, t2) = if let Some(t) = tera {
            (t, None)
        } else if let Some(ts) = slot.and_then(|s| s.types) {
            (ts.primary, ts.secondary)
        } else {
            let ts = transformed.unwrap_or(data).types;
            (ts.primary, ts.secondary)
        };
        Mon {
            t1,
            t2,
            base,
            boosts: slot.map_or([0; 7], |s| s.boosts),
            hp: 0.0,
            stats: [0; 5],
            tera_type: known,
            terastallized: tera.is_some(),
        }
    }

    /// poke-env's per-slot request facts, less what `restrict` took away.
    pub fn slot(&self, slot: usize, restrict: &Restrict) -> SlotCtx {
        let own = self.own();
        let mut out = SlotCtx { force_switch: self.force_switch()[slot], ..SlotCtx::default() };
        let mon = own.mons.get(slot).filter(|m| m.active);
        out.commanding = mon.is_some_and(|m| m.commanding);
        if let (OwnRequest::Move, Some(active)) = (own.request, own.active[slot]) {
            out.trapped = active.trapped;
            out.can_tera = active.can_terastallize.is_some();
            out.can_mega = active.can_mega_evo;
            if mon.is_some_and(|m| m.hp > 0) && !out.commanding {
                match active.moves {
                    OwnMoves::Moves(list) => {
                        for (i, m) in list.iter().enumerate() {
                            if !m.disabled && !restrict.disabled[slot].get(i).copied().unwrap_or(false) {
                                out.moves.push(Avail { info: MoveInfo::new(self.dex, m.id), index: Some(i as u8) });
                            }
                        }
                    }
                    OwnMoves::Locked(id) => out.moves.push(Avail { info: MoveInfo::new(self.dex, id), index: None }),
                    OwnMoves::Recharge => out.moves.push(Avail { info: MoveInfo::RECHARGE, index: None }),
                    OwnMoves::Struggle => {
                        out.moves.push(Avail { info: MoveInfo::new(self.dex, moves::STRUGGLE), index: None })
                    }
                }
            }
        }
        if restrict.no_switch[slot] {
            out.trapped = true;
        }
        if !out.trapped {
            let reviving = own.mons.iter().any(|m| m.reviving);
            for (i, m) in own.mons.iter().enumerate() {
                if if reviving { m.hp == 0 } else { !m.active && m.hp > 0 } {
                    out.switches.push(i as u8);
                }
            }
        }
        out
    }

    /// Whether poke-env's `side_conditions` (or the foe's) is non-empty:
    /// the conditions Showdown announces with `-sidestart` (Wide Guard and
    /// Quick Guard are `-singleturn`).
    pub fn any_condition(side: &PublicSide) -> bool {
        SideCondition::ALL.iter().any(|&c| {
            !matches!(c, SideCondition::WideGuard | SideCondition::QuickGuard) && side.condition(c).layers > 0
        })
    }

    /// `len([m for m in battle.team.values() if not m.fainted])`: poke-env's
    /// team keeps every Pokémon of the first request (all six at team
    /// preview, brought or not).
    pub fn own_remaining(&self) -> usize {
        let own = self.own();
        let team = own.mons.len().max(self.view.my_side().mons.len());
        team.saturating_sub(own.mons.iter().filter(|m| m.hp == 0).count())
    }

    /// Foe Pokémon seen fainted.
    pub fn foe_fainted(&self) -> usize {
        self.view.foe_side().mons.iter().filter(|m| m.is_fainted()).count()
    }

    /// `get_possible_showdown_targets(move, active_pokemon[slot])`.
    pub fn possible_targets(&self, mv: &Avail, slot: usize) -> StackVec<Pos, 3> {
        let mut out = StackVec::new();
        if mv.info.is_recharge() || mv.info.id == moves::STRUGGLE {
            out.push(0);
            return out;
        }
        let ally: Pos = if slot == 0 { -2 } else { -1 };
        let own_slot = &self.view.my_side().slots[slot];
        let user = self.own_active(slot);
        let terastallized = user.is_some_and(|u| u.terastallized);
        let candidates: &[Pos] = if mv.info.id == moves::CURSE && !user.is_some_and(|u| u.has_type(Type::Ghost)) {
            &[0]
        } else if mv.info.id == moves::POLLEN_PUFF && own_slot.volatiles.has(PublicVolatiles::HEAL_BLOCK) {
            &[1, 2]
        } else if mv.info.id == moves::TERA_STARSTORM && terastallized && user.is_some_and(|u| u.t1 == Type::Stellar) {
            &[0]
        } else {
            match mv.info.target() {
                Some(MoveTarget::AdjacentAlly) => &[ally],
                Some(MoveTarget::AdjacentAllyOrSelf) => {
                    if slot == 0 {
                        &[-2, -1]
                    } else {
                        &[-1, -2]
                    }
                }
                Some(MoveTarget::AdjacentFoe) => &[1, 2],
                Some(MoveTarget::Any | MoveTarget::Normal) => {
                    if slot == 0 {
                        &[-2, 1, 2]
                    } else {
                        &[-1, 1, 2]
                    }
                }
                _ => &[0],
            }
        };
        let (mine, foes) = (self.view.my_side(), self.view.foe_side());
        for &t in candidates {
            let keep = match t {
                0 => true,
                -1 | -2 => mine.slots[(-t - 1) as usize].row.is_some(),
                _ => foes.slots[(t - 1) as usize].row.is_some(),
            };
            if keep {
                out.push(t);
            }
        }
        out
    }

    /// `Battle.valid_orders` for a `PseudoBattle` of `slot` (or the singles
    /// battle): switches unless trapped, then (with a Pokémon and no forced
    /// switch) each move, then with Mega Evolution, then with Tera.
    pub fn singles_valid_orders(&self, s: &SlotCtx, active: bool) -> Vec<Order> {
        let mut out: Vec<Order> = Vec::new();
        if !s.trapped {
            out.extend(s.switches.iter().map(|&p| Order::Switch(p)));
        }
        if !active || s.force_switch {
            return out;
        }
        out.extend(s.moves.iter().map(|m| m.order(0, false, false)));
        if s.can_mega {
            out.extend(s.moves.iter().map(|m| m.order(0, false, true)));
        }
        if s.can_tera {
            out.extend(s.moves.iter().map(|m| m.order(0, true, false)));
        }
        out
    }

    /// `Player.choose_random_singles_move` on that battle.
    pub fn random_singles(&self, s: &SlotCtx, active: bool, rng: &mut BattleRng) -> Order {
        let orders = self.singles_valid_orders(s, active);
        if orders.is_empty() {
            return Order::Default;
        }
        orders[random_index(rng, orders.len())]
    }

    /// `DoubleBattle.valid_orders[slot]`.
    fn doubles_valid_orders(&self, slot: usize, restrict: &Restrict) -> Vec<Order> {
        let force = self.force_switch();
        let mut out = Vec::new();
        if force.iter().any(|&f| f) && !force[slot] {
            out.push(Order::Pass);
            return out;
        }
        let s = self.slot(slot, restrict);
        if !s.trapped {
            out.extend(s.switches.iter().map(|&p| Order::Switch(p)));
        }
        if force.iter().all(|&f| f) && self.slot(0, restrict).switches.len() == 1 {
            out.push(Order::Pass);
            return out;
        }
        if self.own_active(slot).is_some() && !s.force_switch {
            for (tera, mega) in [(false, false), (false, true), (true, false)] {
                if (mega && !s.can_mega) || (tera && !s.can_tera) {
                    continue;
                }
                for m in s.moves.iter() {
                    for &t in self.possible_targets(m, slot).iter() {
                        out.push(m.order(t, tera, mega));
                    }
                }
            }
        }
        out
    }

    /// `Player.choose_random_doubles_move`: uniform over `join_orders` of
    /// both slots' valid orders.
    pub fn random_doubles(&self, restrict: &Restrict, rng: &mut BattleRng) -> [Order; 2] {
        let mut first = self.doubles_valid_orders(0, restrict);
        let mut second = self.doubles_valid_orders(1, restrict);
        if first.is_empty() {
            first.push(Order::Pass);
        }
        if second.is_empty() {
            second.push(Order::Pass);
        }
        let mut joined = Vec::new();
        for &a in &first {
            for &b in &second {
                if joinable(a, b) {
                    joined.push([a, b]);
                }
            }
        }
        if joined.is_empty() {
            return [Order::Default, Order::Default];
        }
        joined[random_index(rng, joined.len())]
    }
}

/// `DoubleBattleOrder.join_orders`' filter for one pair.
pub(crate) fn joinable(a: Order, b: Order) -> bool {
    !(a.is_mega() && b.is_mega()
        || a.is_tera() && b.is_tera()
        || matches!((a, b), (Order::Switch(x), Order::Switch(y)) if x == y)
        || a == Order::Pass && b == Order::Pass)
}

/// `int(random.random() * n)`.
pub(crate) fn random_index(rng: &mut BattleRng, n: usize) -> usize {
    rng.random(n as u32) as usize
}

/// `random.choice(items)`.
pub(crate) fn random_choice<T: Copy>(rng: &mut BattleRng, items: &[T]) -> T {
    *rng.sample(items)
}

/// The index of the first maximum of `key` over `items` (Python's `max`).
pub(crate) fn first_max<T>(items: &[T], mut key: impl FnMut(&T) -> f64) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, item) in items.iter().enumerate() {
        let k = key(item);
        if best.is_none_or(|(_, b)| k > b) {
            best = Some((i, k));
        }
    }
    best.map(|(i, _)| i)
}

/// Answers a request with the orders `orders` computes, the way poke-env's
/// message reaches Showdown: team preview is a random order (poke-env's
/// `random_teampreview`); a choice the battle rejects for a switch or a
/// move the request allowed is chosen again without it (the module docs'
/// "honest mask"); anything else the battle rejects falls back to a random
/// legal choice.
pub(crate) fn decide(
    view: &PlayerView<'_>,
    legal: &LegalActions,
    rng: &mut BattleRng,
    mut orders: impl FnMut(&Restrict, &mut BattleRng) -> [Order; 2],
) -> Choice {
    if !matches!(legal.request(), Request::Move | Request::ForceSwitch { .. }) {
        return legal.sample(rng);
    }
    let mut restrict = Restrict::default();
    for _ in 0..8 {
        let o = orders(&restrict, rng);
        let choice = to_choice(view, legal, &o);
        match legal.check(&choice) {
            Ok(()) => return choice,
            Err(ChoiceError::IllegalAction { slot }) => {
                let slot = slot as usize;
                let Choice::Actions(actions) = choice else { break };
                match actions[slot] {
                    SlotAction::Switch(_) if !restrict.no_switch[slot] => restrict.no_switch[slot] = true,
                    SlotAction::Move { index, .. }
                        if (index as usize) < MAX_MOVES
                            && !restrict.disabled[slot][index as usize]
                            && !legal
                                .slot(slot)
                                .iter()
                                .any(|a| matches!(*a, SlotAction::Move { index: i, .. } if i == index)) =>
                    {
                        restrict.disabled[slot][index as usize] = true;
                    }
                    _ => break,
                }
            }
            Err(_) => break,
        }
    }
    legal.sample(rng)
}

/// The choice Showdown makes of the orders' message.
fn to_choice(view: &PlayerView<'_>, legal: &LegalActions, orders: &[Order; 2]) -> Choice {
    let active = view.public.config.active as usize;
    let mut actions = [SlotAction::Pass; MAX_ACTIVE];
    let mut taken = 0u8;
    for slot in 0..active.min(MAX_ACTIVE) {
        let action = match orders[slot] {
            Order::Pass => SlotAction::Pass,
            Order::Switch(p) => SlotAction::Switch(p),
            Order::Move { mv, target, tera, mega } => {
                let gimmick = if tera {
                    Some(Gimmick::Terastallize)
                } else if mega {
                    Some(Gimmick::MegaEvolve)
                } else {
                    None
                };
                match mv.index {
                    Some(index) => {
                        SlotAction::Move { index, target: if active > 1 { target_of(target) } else { None }, gimmick }
                    }
                    // A locked move, recharge or Struggle: Showdown uses
                    // what the Pokémon must use, with no target or gimmick.
                    None => first_move(legal, slot),
                }
            }
            Order::Default => auto_choose(view, legal, slot, taken),
        };
        if let SlotAction::Switch(p) = action {
            taken |= 1 << p;
        }
        actions[slot] = action;
    }
    Choice::Actions(actions)
}

fn target_of(t: Pos) -> Option<Target> {
    match t {
        1 | 2 => Some(Target::Foe((t - 1) as u8)),
        -1 | -2 => Some(Target::Ally((-t - 1) as u8)),
        _ => None,
    }
}

/// The move the engine lists first for `slot` (move slot 0 for a locked
/// move), without target or gimmick.
fn first_move(legal: &LegalActions, slot: usize) -> SlotAction {
    legal
        .slot(slot)
        .iter()
        .find_map(|a| match *a {
            SlotAction::Move { index, .. } => Some(SlotAction::Move { index, target: None, gimmick: None }),
            _ => None,
        })
        .unwrap_or(SlotAction::Pass)
}

/// Showdown's `autoChoose` for one slot: the first move the request
/// allows, with no target (a random one); in a forced switch, the first
/// healthy benched Pokémon not already switching in; otherwise a pass.
fn auto_choose(view: &PlayerView<'_>, legal: &LegalActions, slot: usize, taken: u8) -> SlotAction {
    let own = view.own;
    match own.request {
        OwnRequest::Move => {
            let alive = own.mons.get(slot).is_some_and(|m| m.active && m.hp > 0 && !m.commanding);
            match own.active[slot].filter(|_| alive).map(|a| a.moves) {
                Some(OwnMoves::Moves(list)) => {
                    list.iter().position(|m| !m.disabled).map_or(first_move(legal, slot), |i| SlotAction::Move {
                        index: i as u8,
                        target: None,
                        gimmick: None,
                    })
                }
                Some(_) => first_move(legal, slot),
                None => SlotAction::Pass,
            }
        }
        OwnRequest::ForceSwitch { slots } if slots[slot] => {
            let active = view.public.config.active as usize;
            (active..own.mons.len())
                .find(|&i| own.mons[i].hp > 0 && taken & (1 << i) == 0)
                .map_or(SlotAction::Pass, |i| SlotAction::Switch(i as u8))
        }
        _ => SlotAction::Pass,
    }
}
