//! [`SimpleHeuristicsPolicy`]: poke-env's `SimpleHeuristicsPlayer`.

use pikapro::state::SideCondition;
use pikapro::{BattleRng, Choice, LegalActions};
use pikapro_data::gen9::moves;
use pikapro_data::{BoostId, MoveCategory, MoveId, MoveTarget, Type};
use pikapro_view::PlayerView;

use super::{Avail, Ctx, Mon, Order, Restrict, SlotCtx, damage_multiplier, decide, first_max, joinable};
use crate::policy::Policy;

/// poke-env's `SimpleHeuristicsPlayer`, on the player view.
///
/// Singles (`choose_singles_move`), for the active Pokémon against the
/// foe's:
///
/// 1. switch out when a benched Pokémon has a positive matchup and the
///    active one has lost 3 Defense or Special Defense stages, 3 stages of
///    its better attacking stat, or has a matchup below -2;
/// 2. otherwise, with a move available: set an entry hazard the foe lacks
///    (while it has 3 or more Pokémon not seen fainted), or clear its own
///    side's conditions with Rapid Spin or Defog (with 2 or more of its own
///    Pokémon left);
/// 3. then the move with the best damage estimate: base power × STAB ×
///    the estimated attack / defense ratio × accuracy × expected hits ×
///    type effectiveness, Terastallizing when that improves the product of
///    offensive and defensive type matchups;
/// 4. with no move (a forced switch), the switch with the best matchup.
///
/// A matchup is the best effectiveness of the Pokémon's types on the foe
/// minus the foe's on it, ±0.1 for the faster base Speed, +0.4 × its HP
/// fraction − 0.4 × the foe's.
///
/// Doubles: the singles logic for each slot against each foe (poke-env's
/// `PseudoBattle`), aimed at that foe, keeping the better-scored of the
/// two; a commanding Tatsugiri passes, and so does a slot poke-env's
/// forced-switch rules leave out; two orders that can't go together (two
/// switches to one Pokémon, two Terastallizations) become the first order
/// and Showdown's default for the second.
///
/// poke-env's quirks are kept (no setup moves, Tera only with a known Tera
/// type, ...) and Mega Evolution is used whenever possible: see the
/// [module docs](super).
#[derive(Clone, Copy, Debug, Default)]
pub struct SimpleHeuristicsPolicy {
    setup_moves: bool,
    request_tera_type: bool,
}

impl SimpleHeuristicsPolicy {
    /// poke-env's player, quirks included.
    pub const fn new() -> SimpleHeuristicsPolicy {
        SimpleHeuristicsPolicy { setup_moves: false, request_tera_type: false }
    }

    /// Uses setup moves as poke-env's code means to (a move raising the
    /// user's stats by 2 or more stages in total, at full HP with a
    /// positive matchup, if a raised stat can still rise). poke-env never
    /// does: its test compares an enum with a string.
    pub const fn with_setup_moves(self) -> SimpleHeuristicsPolicy {
        SimpleHeuristicsPolicy { setup_moves: true, ..self }
    }

    /// Reads the player's own Tera types from the request. poke-env only
    /// knows them from an Open Team Sheet, so with closed sheets it never
    /// Terastallizes.
    pub const fn with_request_tera_type(self) -> SimpleHeuristicsPolicy {
        SimpleHeuristicsPolicy { request_tera_type: true, ..self }
    }
}

impl Policy for SimpleHeuristicsPolicy {
    fn choose(&self, view: &PlayerView<'_>, legal: &LegalActions, rng: &mut BattleRng) -> Choice {
        let cx = Ctx::new(view, self.request_tera_type);
        decide(view, legal, rng, |restrict, rng| {
            if cx.doubles {
                self.doubles(&cx, restrict, rng)
            } else {
                let s = cx.slot(0, restrict);
                [self.singles(&cx, &s, cx.own_active(0), cx.foe_active(0), rng).0, Order::Pass]
            }
        })
    }
}

const SPEED_TIER_COEFICIENT: f64 = 0.1;
const HP_FRACTION_COEFICIENT: f64 = 0.4;
const SWITCH_OUT_MATCHUP_THRESHOLD: f64 = -2.0;

/// `ENTRY_HAZARDS`: the hazard a move sets.
fn entry_hazard(id: MoveId) -> Option<SideCondition> {
    match id {
        moves::SPIKES => Some(SideCondition::Spikes),
        moves::STEALTH_ROCK => Some(SideCondition::StealthRock),
        moves::STICKY_WEB => Some(SideCondition::StickyWeb),
        moves::TOXIC_SPIKES => Some(SideCondition::ToxicSpikes),
        _ => None,
    }
}

/// `ANTI_HAZARDS_MOVES`.
fn anti_hazard(id: MoveId) -> bool {
    id == moves::RAPID_SPIN || id == moves::DEFOG
}

/// `_estimate_matchup(mon, opponent)`.
pub(crate) fn estimate_matchup(mon: &Mon, opponent: &Mon) -> f64 {
    let mut score = mon.types().map(|t| opponent.damage_multiplier(t)).fold(f64::NEG_INFINITY, f64::max);
    score -= opponent.types().map(|t| mon.damage_multiplier(t)).fold(f64::NEG_INFINITY, f64::max);
    let (a, b) = (mon.base[5], opponent.base[5]);
    if a > b {
        score += SPEED_TIER_COEFICIENT;
    } else if b > a {
        score -= SPEED_TIER_COEFICIENT;
    }
    score += mon.hp * HP_FRACTION_COEFICIENT;
    score -= opponent.hp * HP_FRACTION_COEFICIENT;
    score
}

/// `_stat_estimation(mon, stat)` (a +1 stage counts ×2, as in poke-env).
pub(crate) fn stat_estimation(mon: &Mon, stat: BoostId) -> f64 {
    let b = f64::from(mon.boost(stat));
    let boost = if b > 1.0 { (2.0 + b) / 2.0 } else { 2.0 / (2.0 - b) };
    let base = f64::from(mon.base[stat as usize + 1]);
    ((2.0 * base + 31.0) + 5.0) * boost
}

/// `1 / (x or 1/8)`.
fn inverse(x: f64) -> f64 {
    1.0 / if x == 0.0 { 1.0 / 8.0 } else { x }
}

impl SimpleHeuristicsPolicy {
    /// `_should_terastallize(battle, move)`.
    fn should_terastallize(s: &SlotCtx, active: &Mon, opp: &Mon, ty: Type) -> bool {
        let Some(tera) = active.tera_type.filter(|_| s.can_tera) else { return false };
        let offensive = opp.damage_multiplier(ty);
        let defensive = opp.types().map(|t| inverse(active.damage_multiplier(t))).fold(f64::INFINITY, f64::min);
        let defensive_tera =
            opp.types().map(|t| inverse(damage_multiplier(t, tera, None))).fold(f64::INFINITY, f64::min);
        offensive * (defensive_tera / defensive) > 1.0
    }

    /// `_should_switch_out(battle)`.
    fn should_switch_out(cx: &Ctx<'_, '_>, s: &SlotCtx, active: &Mon, opp: &Mon) -> bool {
        if !s.switches.iter().any(|&p| estimate_matchup(&cx.own_mon(p as usize), opp) > 0.0) {
            return false;
        }
        let [atk, _, spa, _, _] = active.stats;
        active.boost(BoostId::Def) <= -3
            || active.boost(BoostId::SpD) <= -3
            || active.boost(BoostId::Atk) <= -3 && atk >= spa
            || active.boost(BoostId::SpA) <= -3 && atk <= spa
            || estimate_matchup(active, opp) < SWITCH_OUT_MATCHUP_THRESHOLD
    }

    /// `choose_singles_move` for the slot `s` of `active` against
    /// `opponent`: the order and its score.
    fn singles(
        &self,
        cx: &Ctx<'_, '_>,
        s: &SlotCtx,
        active: Option<Mon>,
        opponent: Option<Mon>,
        rng: &mut BattleRng,
    ) -> (Order, f64) {
        let (Some(active), Some(opp)) = (active, opponent) else {
            return (cx.random_singles(s, active.is_some(), rng), 0.0);
        };
        let physical_ratio = stat_estimation(&active, BoostId::Atk) / stat_estimation(&opp, BoostId::Def);
        let special_ratio = stat_estimation(&active, BoostId::SpA) / stat_estimation(&opp, BoostId::SpD);

        if !s.moves.is_empty() && (s.switches.is_empty() || !Self::should_switch_out(cx, s, &active, &opp)) {
            let n_remaining = cx.own_remaining();
            let n_opp_remaining = 6usize.saturating_sub(cx.foe_fainted());
            let (mine, foes) = (cx.view.my_side(), cx.view.foe_side());
            for m in s.moves.iter() {
                // Setting a hazard the foe lacks, or clearing one's side.
                let set =
                    n_opp_remaining >= 3 && entry_hazard(m.info.id).is_some_and(|c| foes.condition(c).layers == 0);
                let clear = Ctx::any_condition(mine) && anti_hazard(m.info.id) && n_remaining >= 2;
                if set || clear {
                    return (m.order(0, false, false), 0.0);
                }
            }
            if self.setup_moves && active.hp == 1.0 && estimate_matchup(&active, &opp) > 0.0 {
                for m in s.moves.iter() {
                    if Self::is_setup(m, &active) {
                        return (m.order(0, false, false), 0.0);
                    }
                }
            }
            let score = |m: &Avail| {
                let stab = if active.has_type(m.info.ty()) { 1.5 } else { 1.0 };
                let ratio = if m.info.category() == MoveCategory::Physical { physical_ratio } else { special_ratio };
                f64::from(m.info.base_power())
                    * stab
                    * ratio
                    * m.info.accuracy()
                    * m.info.expected_hits()
                    * opp.damage_multiplier(m.info.ty())
            };
            let best = first_max(&s.moves, score).unwrap_or(0);
            let m = &s.moves[best];
            let tera = Self::should_terastallize(s, &active, &opp, m.info.ty());
            return (m.order(0, tera, s.can_mega && !tera), score(m));
        }
        if !s.switches.is_empty() {
            let mons: Vec<Mon> = s.switches.iter().map(|&p| cx.own_mon(p as usize)).collect();
            let best = first_max(&mons, |m| estimate_matchup(m, &opp)).unwrap_or(0);
            return (Order::Switch(s.switches[best]), 0.0);
        }
        (cx.random_singles(s, true, rng), 0.0)
    }

    /// The setup test poke-env means: boosts summing to 2 or more, on the
    /// user, one of the raised stats below +6.
    fn is_setup(m: &Avail, active: &Mon) -> bool {
        let Some(boosts) = m.info.data.and_then(|d| d.boosts) else { return false };
        let sum: i32 = boosts.stages.iter().map(|&b| i32::from(b)).sum();
        let raised = boosts.iter().filter(|&(_, v)| v > 0).map(|(id, _)| active.boost(id)).min();
        sum >= 2 && m.info.target() == Some(MoveTarget::User) && raised.is_some_and(|b| b < 6)
    }

    /// `choose_move` for a `DoubleBattle`.
    fn doubles(&self, cx: &Ctx<'_, '_>, restrict: &Restrict, rng: &mut BattleRng) -> [Order; 2] {
        let force = cx.force_switch();
        let mut orders = [Order::Pass; 2];
        for (slot, order) in orders.iter_mut().enumerate() {
            let s = cx.slot(slot, restrict);
            let mon = cx.own_active(slot);
            if mon.is_some() && s.commanding || mon.is_none() && s.switches.is_empty() {
                *order = Order::Pass;
                continue;
            }
            let mut results = [(Order::Pass, 0.0); 2];
            for (opp_id, result) in results.iter_mut().enumerate() {
                *result = self.singles(cx, &s, mon, cx.foe_active(opp_id), rng);
            }
            if mon.is_some() {
                for (i, (order, _)) in results.iter_mut().enumerate() {
                    if let Order::Move { mv, target, .. } = order {
                        let Some(m) = s.moves.iter().find(|m| m.key() == *mv) else { continue };
                        let possible = cx.possible_targets(m, slot);
                        let t = i as i8 + 1;
                        *target = if possible.contains(&t) { t } else { possible.first().copied().unwrap_or(0) };
                    }
                }
            }
            let best = if results[1].1 > results[0].1 { 1 } else { 0 };
            let pass = slot == 0 && force == [false, true]
                || slot == 1 && force == [true, false]
                || slot == 1 && force == [true, true] && s.switches.len() == 1;
            *order = if pass { Order::Pass } else { results[best].0 };
        }
        // Mega Evolution (the deviation): one per turn, the first slot's.
        if orders[0].is_mega()
            && let Order::Move { mega, .. } = &mut orders[1]
        {
            *mega = false;
        }
        if joinable(orders[0], orders[1]) { orders } else { [orders[0], Order::Default] }
    }
}
