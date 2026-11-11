//! [`MaxBasePowerPolicy`]: poke-env's `MaxBasePowerPlayer`.

use pikapro::{BattleRng, Choice, LegalActions};
use pikapro_data::MoveTarget;
use pikapro_view::PlayerView;

use super::{Ctx, Order, Restrict, decide, first_max, random_choice};
use crate::policy::Policy;

/// poke-env's `MaxBasePowerPlayer`: the available move with the highest
/// base power (the first of equals, in request order), or a random order
/// when there is none.
///
/// In doubles (`choose_doubles_move`): a random joint choice during a
/// forced switch; otherwise per slot, a random switch when no move is
/// available, else the move with the highest base power, ×1.5 for a move
/// that hits both foes while both stand, at a random foe (or a random
/// target when it can't aim at a foe). The only deviation from poke-env
/// is Mega Evolution, used whenever it can be (see the
/// [module docs](super)).
#[derive(Clone, Copy, Debug, Default)]
pub struct MaxBasePowerPolicy;

impl Policy for MaxBasePowerPolicy {
    fn choose(&self, view: &PlayerView<'_>, legal: &LegalActions, rng: &mut BattleRng) -> Choice {
        let cx = Ctx::new(view, false);
        decide(view, legal, rng, |restrict, rng| {
            if cx.doubles { doubles(&cx, restrict, rng) } else { [singles(&cx, restrict, rng), Order::Pass] }
        })
    }
}

/// `choose_singles_move`.
fn singles(cx: &Ctx<'_, '_>, restrict: &Restrict, rng: &mut BattleRng) -> Order {
    let s = cx.slot(0, restrict);
    match first_max(&s.moves, |m| f64::from(m.info.base_power())) {
        Some(best) => s.moves[best].order(0, false, s.can_mega),
        None => cx.random_singles(&s, cx.own_active(0).is_some(), rng),
    }
}

/// `choose_doubles_move`.
fn doubles(cx: &Ctx<'_, '_>, restrict: &Restrict, rng: &mut BattleRng) -> [Order; 2] {
    if cx.force_switch().iter().any(|&f| f) {
        return cx.random_doubles(restrict, rng);
    }
    let can_double_target = cx.foe_active(0).is_some() && cx.foe_active(1).is_some();
    let mut orders = [Order::Pass; 2];
    let mut switched_in = None;
    let mut mega_used = false;
    for (slot, order) in orders.iter_mut().enumerate() {
        let s = cx.slot(slot, restrict);
        let switches: Vec<u8> = s.switches.iter().copied().filter(|&p| Some(p) != switched_in).collect();
        if cx.own_active(slot).is_none() || s.commanding {
            *order = Order::Pass;
        } else if s.moves.is_empty() && !switches.is_empty() {
            let p = random_choice(rng, &switches);
            *order = Order::Switch(p);
            switched_in = Some(p);
        } else if s.moves.is_empty() {
            *order = Order::Default;
        } else {
            let power = |m: &super::Avail| {
                let bp = f64::from(m.info.base_power());
                if matches!(m.info.target(), Some(MoveTarget::Normal | MoveTarget::Any)) || !can_double_target {
                    bp
                } else {
                    bp * 1.5
                }
            };
            let best = &s.moves[first_max(&s.moves, power).unwrap_or(0)];
            let targets = cx.possible_targets(best, slot);
            let foes: Vec<i8> = targets.iter().copied().filter(|&t| t == 1 || t == 2).collect();
            let target = if !foes.is_empty() {
                random_choice(rng, &foes)
            } else if !targets.is_empty() {
                random_choice(rng, &targets)
            } else {
                0
            };
            let mega = s.can_mega && !mega_used;
            mega_used |= mega;
            *order = best.order(target, false, mega);
        }
    }
    orders
}
