//! Soundness: every fact a [`PublicTracker`] claims holds in the battle.
//!
//! A test oracle, not part of a view: it reads the omniscient
//! [`BattleState`](pikapro::BattleState), which a view never does. Run
//! it at decision points (after `submit` returns and the new events were
//! applied); [`check_sound`] returns the first claim the state
//! contradicts.
//!
//! What is checked, for each side:
//! * every active slot: the Pokémon shown there, its HP, status, stat
//!   stages, displayed types and the public volatiles that have an engine
//!   counterpart (in both directions for the ones the log always
//!   announces);
//! * every row: fainted or not, and the facts it claims (species, level,
//!   gender, item, ability, Tera type, revealed moves);
//! * each active slot's record of the last turn (an action is a move
//!   action of the Pokémon there, a switch-in a recent one), its protect
//!   chain and the stat its Paradox ability boosts;
//! * the side's conditions, faint count, Tera and Mega use, foreseen
//!   attacks, Wish, Healing Wish and Lunar Dance; the field's weather, terrain and pseudo-weathers; the turn
//!   and the result.
//!
//! Illusion fools the tracker as it fools a client, and which records a
//! disguise touched is history this oracle doesn't have: on a side with
//! an Illusion user only the records never on the field (but the Illusion
//! user's own) are checked against the truth, and its slots are exempt
//! from the identity checks (their HP, status, stages and volatiles are
//! still their own and are checked). The records'
//! own consistency ([`check_consistent`]: `ACTIVE` exactly on the rows the
//! slots hold, `REVEALED` exactly on the rows of `reveal_order`, every row
//! a Pokémon with a level) needs no truth, and holds on every side,
//! Illusion's included.

use std::fmt;

use pikapro::state::{SideCondition, SlotCondition, Volatiles};
use pikapro::{Battle, Gimmick, SideId, Status};
use pikapro_data::MoveId;
use pikapro_data::gen9::{abilities, moves};
use pikapro_protocol::{details_species, hp_shown};

use crate::mon::{AbilityState, ItemState, MonFlags, MonView, StatusKind};
use crate::side::{PublicVolatiles as V, TurnAction, TurnRecord};
use crate::tracker::{HpReport, Outcome, PublicTracker};

/// A claim of the tracker the battle's state contradicts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub side: SideId,
    /// The active slot or the row concerned, if any.
    pub slot: Option<usize>,
    pub row: Option<u8>,
    /// What was claimed (`"item"`, `"boosts"`, ...).
    pub what: &'static str,
    /// The claim and the truth.
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.side)?;
        if let Some(slot) = self.slot {
            write!(f, " slot {slot}")?;
        }
        if let Some(row) = self.row {
            write!(f, " row {row}")?;
        }
        write!(f, ": {}: {}", self.what, self.detail)
    }
}

/// Public volatiles and the engine volatiles they stand for. `exact`: the
/// log always announces the volatile, so the engine's having it means the
/// tracker must show it too.
const VOLATILES: &[(u64, u128, bool)] = &[
    (V::SUBSTITUTE, Volatiles::SUBSTITUTE, true),
    (V::CONFUSION, Volatiles::CONFUSION, true),
    (V::TAUNT, Volatiles::TAUNT, true),
    (V::ENCORE, Volatiles::ENCORE, true),
    (V::DISABLE, Volatiles::DISABLE, true),
    (V::LEECH_SEED, Volatiles::LEECH_SEED, true),
    (V::YAWN, Volatiles::YAWN, true),
    (V::PERISH_SONG, Volatiles::PERISH_SONG, true),
    (V::CURSE, Volatiles::CURSE, true),
    (V::ATTRACT, Volatiles::ATTRACT, true),
    (V::TORMENT, Volatiles::TORMENT, true),
    (V::SALT_CURE, Volatiles::SALT_CURE, true),
    (V::SYRUP_BOMB, Volatiles::SYRUP_BOMB, true),
    (V::THROAT_CHOP, Volatiles::THROAT_CHOP, true),
    (V::HEAL_BLOCK, Volatiles::HEAL_BLOCK, true),
    (V::IMPRISON, Volatiles::IMPRISON, true),
    (V::NO_RETREAT, Volatiles::NO_RETREAT, true),
    (V::TRAPPED, Volatiles::TRAPPED | Volatiles::OCTOLOCK, true),
    (V::PARTIALLY_TRAPPED, Volatiles::PARTIALLY_TRAPPED, true),
    (V::SMACK_DOWN, Volatiles::SMACK_DOWN, true),
    (V::MAGNET_RISE, Volatiles::MAGNET_RISE, true),
    (V::AQUA_RING, Volatiles::AQUA_RING, true),
    (V::INGRAIN, Volatiles::INGRAIN, true),
    (V::CHARGE, Volatiles::CHARGE, true),
    (V::FOCUS_ENERGY, Volatiles::FOCUS_ENERGY, true),
    (V::DRAGON_CHEER, Volatiles::DRAGON_CHEER, true),
    (V::TAR_SHOT, Volatiles::TAR_SHOT, true),
    (V::STOCKPILE, Volatiles::STOCKPILE, true),
    (V::POWER_TRICK, Volatiles::POWER_TRICK, true),
    (V::POWER_SHIFT, Volatiles::POWER_SHIFT, true),
    (V::UPROAR, Volatiles::UPROAR, true),
    (V::GLAIVE_RUSH, Volatiles::GLAIVE_RUSH, true),
    (V::TRANSFORMED, Volatiles::TRANSFORMED, true),
    (V::MIMIC, Volatiles::MIMIC, true),
    (V::ABILITY_SUPPRESSED, Volatiles::GASTRO_ACID, true),
    (V::FLASH_FIRE, Volatiles::FLASH_FIRE, true),
    (V::PARADOX_BOOST, Volatiles::PROTOSYNTHESIS | Volatiles::QUARK_DRIVE, true),
    (V::COMMANDING, Volatiles::COMMANDING, true),
    (V::COMMANDED, Volatiles::COMMANDED, true),
    (V::MUST_RECHARGE, Volatiles::MUST_RECHARGE, true),
    (V::CHARGING, Volatiles::TWO_TURN_MOVE | Volatiles::CHARGING, false),
    (V::DESTINY_BOND, Volatiles::DESTINY_BOND, false),
    (V::LOCK_ON, Volatiles::LOCK_ON, false),
    (V::MINIMIZE, Volatiles::MINIMIZE, false),
    (V::DEFENSE_CURL, Volatiles::DEFENSE_CURL, false),
];

/// The invariants a tracker keeps whatever it was fed, with no battle to
/// compare with (a channel's tracker can be checked too): per side, a row
/// is flagged [`MonFlags::ACTIVE`] exactly when a slot shows it (a
/// fainted Pokémon's slot shows it until its replacement comes in), and
/// [`MonFlags::REVEALED`] exactly when it is in
/// [`PublicSide::reveal_order`](crate::PublicSide::reveal_order), once;
/// every row is a Pokémon (a level, which a blank record lacks), and every
/// slot shows a row that exists.
pub fn check_consistent(tracker: &PublicTracker) -> Result<(), Violation> {
    for side in SideId::BOTH {
        let fail = |slot: Option<usize>, row: Option<u8>, what: &'static str, detail: String| {
            Err(Violation { side, slot, row, what, detail })
        };
        let t = tracker.side(side);
        for (slot, av) in t.slots.iter().enumerate() {
            if let Some(row) = av.row
                && t.mon(row).is_none()
            {
                return fail(Some(slot), Some(row), "slot row", format!("{} rows", t.mons.len()));
            }
        }
        for (i, &row) in t.reveal_order.iter().enumerate() {
            if t.mon(row).is_none() || t.reveal_order[..i].contains(&row) {
                return fail(None, Some(row), "reveal order", format!("{:?}", t.reveal_order.as_ref()));
            }
        }
        for (row, view) in t.mons.iter().enumerate() {
            let row = row as u8;
            let shown = t.slots.iter().any(|av| av.row == Some(row));
            if view.is_active() != shown {
                return fail(None, Some(row), "active", format!("flags {:?}, slots show it: {shown}", view.flags));
            }
            let listed = t.reveal_order.contains(&row);
            if view.is_revealed() != listed || view.is_active() && !view.is_revealed() {
                return fail(None, Some(row), "revealed", format!("flags {:?}, in reveal order: {listed}", view.flags));
            }
            if !(1..=100).contains(&view.level) {
                return fail(None, Some(row), "blank row", format!("{view:?}"));
            }
        }
    }
    Ok(())
}

/// Whether the battle ended in `endTurn`, after the next turn was counted
/// and before it started: the turn limit's tie and the Endless Battle
/// Clause. The log has no `|turn|` line for that turn, so a view is one
/// turn behind the state's turn count and its active Pokémon's turns on
/// the field.
fn ended_before_a_turn(battle: &Battle) -> bool {
    battle
        .result()
        .is_some_and(|r| matches!(r.reason, pikapro::EndReason::TurnLimit | pikapro::EndReason::EndlessBattle))
}

/// Checks every claim of `tracker` (fed all of `battle`'s events so far)
/// against `battle`'s state, after [`check_consistent`]; the first false
/// one.
pub fn check_sound(tracker: &PublicTracker, battle: &Battle) -> Result<(), Violation> {
    check_consistent(tracker)?;
    let state = battle.state();
    let format = battle.format();
    for side in SideId::BOTH {
        let fail = |slot: Option<usize>, row: Option<u8>, what: &'static str, detail: String| {
            Err(Violation { side, slot, row, what, detail })
        };
        let s = state.side(side);
        let t = tracker.side(side);
        // Fainted now: Revival Blessing takes a faint back.
        let fainted = s.team.iter().filter(|m| m.is_fainted()).count() as u8;
        if t.fainted != fainted {
            return fail(None, None, "fainted", format!("{} vs {fainted}", t.fainted));
        }
        if t.tera_used != s.gimmicks_used.contains(Gimmick::Terastallize) {
            return fail(None, None, "tera_used", format!("{}", t.tera_used));
        }
        if t.mega_used && !s.gimmicks_used.contains(Gimmick::MegaEvolve) {
            return fail(None, None, "mega_used", String::new());
        }
        for c in SideCondition::ALL {
            let view = t.condition(c);
            let truth = s.conditions.turns(c);
            let ok = if c.is_hazard() { view.layers == truth } else { (view.layers > 0) == (truth > 0) };
            if !ok {
                return fail(None, None, "condition", format!("{c:?}: {view:?} vs {truth}"));
            }
        }
        for slot in 0..format.active_per_side() {
            let view = t.future[slot].move_id;
            let truth = s.future_moves[slot].map(|f| f.move_id);
            if view != truth {
                return fail(Some(slot), None, "future", format!("{view:?} vs {truth:?}"));
            }
        }
        // Wish, Healing Wish, Lunar Dance.
        for slot in 0..format.active_per_side() {
            let view = t.slot_conditions[slot];
            let truth = &s.conditions.slots[slot];
            let pairs = [
                ("wish", view.wish.is_some(), SlotCondition::Wish),
                ("healing wish", view.healing_wish, SlotCondition::HealingWish),
                ("lunar dance", view.lunar_dance, SlotCondition::LunarDance),
            ];
            for (what, shown, c) in pairs {
                // A Wish ends in the residuals (`onResidualOrder: 4`), silently
                // when its slot's Pokémon fainted; a battle that ends in the
                // residuals shows no `|upkeep` telling whether it got there
                // (a burn ends it after the Wish ended).
                if c == SlotCondition::Wish && battle.is_over() {
                    continue;
                }
                if shown != truth.has(c) {
                    return fail(Some(slot), None, what, format!("{view:?}"));
                }
            }
        }
        let illusion_side = s.team.iter().any(|m| m.base_ability == abilities::ILLUSION);

        // Active slots.
        for slot in 0..format.active_per_side() {
            let Some(mon) = s.active(slot) else { continue };
            if mon.is_fainted() {
                continue;
            }
            let av = &t.slots[slot];
            let Some((row, view)) = t.active(slot) else {
                return fail(Some(slot), None, "active", "an empty slot".into());
            };
            // An Illusion showing the Pokémon in the other slot shares its
            // record: identity checks don't apply to either.
            let shared = (0..format.active_per_side()).any(|o| o != slot && t.slots[o].row == Some(row));
            // A side with an Illusion user may have records that mix a
            // disguise with the Pokémon behind it, which only the history
            // tells apart (`pikapro-conformance` keeps it).
            let disguised = mon.illusion.is_some() || shared || illusion_side;
            let shown_id = mon.illusion.unwrap_or(mon.id);
            if t.row_of(shown_id) != Some(row) {
                return fail(Some(slot), Some(row), "active row", format!("shows id {:?}", t.id_of(row)));
            }
            if !view.is_active() || !view.is_revealed() || view.is_fainted() && !disguised {
                return fail(Some(slot), Some(row), "active flags", format!("{:?}", view.flags));
            }
            let hp = match tracker.config.hp {
                HpReport::Exact => (mon.hp, mon.max_hp(), pikapro_protocol::HpHint::None),
                HpReport::Percent => {
                    let (pct, hint) = hp_shown(format.mechanics, mon.hp, mon.max_hp());
                    (pct as u16, 100, hint)
                }
            };
            if (av.hp.hp, av.hp.max, av.hp.hint) != hp || !shared && view.hp != av.hp {
                return fail(Some(slot), Some(row), "hp", format!("{:?} / {:?} vs {hp:?}", av.hp, view.hp));
            }
            if av.status != StatusKind::of(mon.status) || !shared && view.status != av.status {
                return fail(Some(slot), Some(row), "status", format!("{:?} vs {:?}", view.status, mon.status));
            }
            // Observed: Magic Guard hides the residuals that raise it. A
            // record an Illusion shared counts the disguise's too.
            if let Status::Toxic { counter } = mon.status
                && !disguised
                && view.status_turns > counter
            {
                return fail(Some(slot), Some(row), "toxic turns", format!("{} vs {counter}", view.status_turns));
            }
            let b = mon.boosts;
            let truth = [b.atk, b.def, b.spa, b.spd, b.spe, b.accuracy, b.evasion];
            if av.boosts != truth {
                return fail(Some(slot), Some(row), "boosts", format!("{:?} vs {truth:?}", av.boosts));
            }
            for &(bit, engine, exact) in VOLATILES {
                let (shown, has) = (av.volatiles.has(bit), mon.volatiles.has(engine));
                if shown && !has || exact && has && !shown {
                    return fail(
                        Some(slot),
                        Some(row),
                        "volatile",
                        format!("bit {} shown {shown}", bit.trailing_zeros()),
                    );
                }
            }
            let c = mon.volatile_counters;
            // The last turn's record of the slot: an action (a move or a
            // `cant`) is a move action since it came in, a switch-in is
            // recent.
            let last = t.last_turn[slot];
            let order_ok = if last.acted() {
                (1..=2 * format.active_per_side() as u8).contains(&last.order)
            } else {
                last.order == 0
            };
            let record_ok = order_ok
                && match last.action {
                    TurnAction::Move | TurnAction::Cant => mon.active_move_actions > 0 && mon.active_turns > 0,
                    // (A battle ended at the end of a turn by the turn
                    // limit or the Endless Battle Clause has counted the
                    // turn that didn't start, as Showdown's `endTurn`
                    // does before either.)
                    TurnAction::Switch => mon.active_turns <= 1 + u16::from(ended_before_a_turn(battle)),
                    TurnAction::None => last == TurnRecord::default() || last.taken > 0,
                };
            if !record_ok {
                let detail = format!("{last:?}, {} move actions, {} turns", mon.active_move_actions, mon.active_turns);
                return fail(Some(slot), Some(row), "last turn", detail);
            }
            // Both chances stop falling at 3^6. A protection that `willAct`
            // fails keeps Showdown's `stall`; the view ends it at the
            // `-fail` (a failed roll's reads the same) and takes that back
            // at the residuals if nothing acted after it. A battle that ends
            // first leaves the two apart.
            if av.counters.protect_chain != mon.protect_counter.min(6) && !battle.is_over() {
                return fail(
                    Some(slot),
                    Some(row),
                    "protect chain",
                    format!("{} vs {}", av.counters.protect_chain, mon.protect_counter),
                );
            }
            if av.volatiles.has(V::PARADOX_BOOST)
                && let Some(b) = av.boosted_stat
                && b as u8 != c.paradox_stat
            {
                return fail(Some(slot), Some(row), "paradox stat", format!("{b:?} vs {}", c.paradox_stat));
            }
            // Before its first residual the engine's count is one higher
            // than the `perish3` shown.
            if av.volatiles.has(V::PERISH_SONG) && av.counters.perish != c.perish_song && c.perish_song != 4 {
                return fail(Some(slot), Some(row), "perish", format!("{} vs {}", av.counters.perish, c.perish_song));
            }
            // The engine's layer count outlives the volatile through Baton
            // Pass (which doesn't pass Stockpile: `noCopy`); only the
            // volatile's layers are real.
            let layers = if mon.volatiles.has(Volatiles::STOCKPILE) { c.stockpile_layers } else { 0 };
            if av.counters.stockpile != layers {
                return fail(Some(slot), Some(row), "stockpile", format!("{}", av.counters.stockpile));
            }
            if av.added_type != c.added_type {
                return fail(Some(slot), Some(row), "added type", format!("{:?} vs {:?}", av.added_type, c.added_type));
            }
            if av.volatiles.has(V::DISABLE) && av.disabled_move.is_some_and(|m| m != c.disable_move) {
                return fail(Some(slot), Some(row), "disabled move", format!("{:?}", av.disabled_move));
            }
            if av.volatiles.has(V::ENCORE) && av.encore_move.is_some_and(|m| m != c.encore_move) {
                return fail(Some(slot), Some(row), "encore move", format!("{:?}", av.encore_move));
            }
            // Leech Seed drains into the seeder's slot (`sourceSlot`).
            let seeder = (c.leech_seed_slot.side, c.leech_seed_slot.slot);
            if av.volatiles.has(V::LEECH_SEED) && av.leech_seed_slot != Some(seeder) {
                return fail(
                    Some(slot),
                    Some(row),
                    "leech seed slot",
                    format!("{:?} vs {seeder:?}", av.leech_seed_slot),
                );
            }
            // A partial trap's source and move (an Illusion user's `[of]`
            // names its disguise).
            let (by, id) = c.partially_trapped_by;
            let source_disguised = state.side(by).team.iter().any(|m| m.base_ability == abilities::ILLUSION);
            let trap = tracker.side(by).row_of(id).map(|r| (by, r, c.partially_trapped_move));
            if av.volatiles.has(V::PARTIALLY_TRAPPED) && !source_disguised && av.partial_trap != trap {
                return fail(Some(slot), Some(row), "partial trap", format!("{:?} vs {trap:?}", av.partial_trap));
            }
            // A Mega Evolution after Transform changes the engine's
            // species (`setSpecies`), not whom it transformed into.
            if let (Some(tv), Some(base)) = (av.transformed, c.transform)
                && tv.species != details_species(base.species)
                && !view.flags.has(MonFlags::MEGA)
            {
                return fail(Some(slot), Some(row), "transformed into", format!("{:?}", tv.species));
            }
            // A win can cut the residuals short, before Roost ended.
            if !disguised && !battle.is_over() {
                let types = tracker.types(side, slot).expect("an active slot");
                if types != mon.types {
                    return fail(Some(slot), Some(row), "types", format!("{types:?} vs {:?}", mon.types));
                }
                let own = c.transform.map_or(mon.species, |base| base.own_species);
                if view.species != details_species(own) {
                    return fail(Some(slot), Some(row), "species", format!("{:?} vs {:?}", view.species, own));
                }
                if let AbilityState::Known(a) = view.ability
                    && a != mon.ability
                {
                    let name = |a: pikapro_data::AbilityId| format.dex.ability(a).name;
                    return fail(Some(slot), Some(row), "ability", format!("{} vs {}", name(a), name(mon.ability)));
                }
            }
        }

        // Rows. Only an Illusion leaves a disguise behind.
        if let Some(row) = t.mons.iter().position(|m| m.flags.has(MonFlags::WAS_DISGUISE))
            && !illusion_side
        {
            return fail(None, Some(row as u8), "was disguise", "a side without Illusion".into());
        }
        for (row, view) in t.mons.iter().enumerate() {
            let row = row as u8;
            let Some(id) = t.id_of(row) else {
                return fail(None, Some(row), "row", "no id".into());
            };
            // Pokémon left out at team preview are not in the battle.
            let Some(mon) = s.team.iter().find(|m| m.id == id) else {
                if view.is_revealed() {
                    return fail(None, Some(row), "row", format!("no Pokémon {id}"));
                }
                continue;
            };
            // On a side with an Illusion user, only the records that were
            // never on the field hold nothing a disguise did: not the
            // Illusion user's own (it may have fainted disguised), nor one
            // a disguise left behind.
            if illusion_side
                && (view.is_revealed()
                    || view.flags.has(MonFlags::WAS_DISGUISE)
                    || mon.base_ability == abilities::ILLUSION)
            {
                continue;
            }
            if let Err((what, detail)) = check_row(view, mon, battle) {
                return fail(None, Some(row), what, detail);
            }
        }
    }
    let f = &state.field;
    let field = &tracker.field;
    // A turn-limit tie comes after `endTurn` counted one more turn than
    // the log shows (`It is turn 1000`); the result keeps the limit. The
    // Endless Battle Clause ends a battle at the same point: no `|turn|`
    // line for the turn `endTurn` counted.
    let turn = if ended_before_a_turn(battle) { battle.result().map_or(state.turn, |r| r.turns) } else { state.turn };
    let checks = [
        ("weather", field.weather == f.weather),
        ("terrain", field.terrain == f.terrain),
        ("trick room", field.trick_room.is_some() == (f.trick_room_turns > 0)),
        ("gravity", field.gravity.is_some() == (f.gravity_turns > 0)),
        ("magic room", field.magic_room.is_some() == (f.magic_room_turns > 0)),
        ("wonder room", field.wonder_room.is_some() == (f.wonder_room_turns > 0)),
        ("fairy lock", field.fairy_lock == (f.fairy_lock_turns > 0)),
        ("turn", tracker.turn == turn),
    ];
    for (what, ok) in checks {
        if !ok {
            return Err(Violation { side: SideId::P1, slot: None, row: None, what, detail: format!("{field:?}") });
        }
    }
    if let Some(r) = battle.result() {
        let expected = r.winner.map_or(Outcome::Tie, Outcome::Win);
        if tracker.outcome != Some(expected) {
            let detail = format!("{:?} vs {expected:?}", tracker.outcome);
            return Err(Violation { side: SideId::P1, slot: None, row: None, what: "outcome", detail });
        }
    }
    Ok(())
}

/// The facts one record claims about `mon`: the row checks of
/// [`check_sound`], which on a side with an Illusion user checks only the
/// rows never on the field (`pikapro-conformance`, which keeps the
/// history, checks the rows there that never showed a disguise).
pub fn check_row(view: &MonView, mon: &pikapro::PokemonState, battle: &Battle) -> Result<(), (&'static str, String)> {
    let err = |what, detail: String| Err((what, detail));
    if view.is_fainted() != mon.is_fainted() && (view.is_revealed() || view.flags.has(MonFlags::PREVIEWED)) {
        return err("fainted", format!("{:?}", view.flags));
    }
    if view.is_revealed() {
        // The species the details show (Greninja-Bond as Greninja).
        if view.details_species != details_species(mon.base_species) {
            return err("details species", format!("{:?} vs {:?}", view.details_species, mon.base_species));
        }
        if !view.is_active() && view.species != details_species(mon.species) {
            return err("benched species", format!("{:?} vs {:?}", view.species, mon.species));
        }
        if (view.level, view.gender) != (mon.level, mon.gender) {
            return err("level/gender", format!("{:?}", (view.level, view.gender)));
        }
    } else if !view.flags.has(MonFlags::FORME_HIDDEN)
        && view.flags.has(MonFlags::PREVIEWED)
        && view.species != details_species(mon.species)
    {
        return err("preview species", format!("{:?} vs {:?}", view.species, mon.species));
    }
    match view.item {
        ItemState::Known(i) if mon.item != Some(i) => {
            let name = |i: pikapro_data::ItemId| battle.format().dex.item(i).name;
            return err("item", format!("{} vs {:?}", name(i), mon.item.map(name)));
        }
        ItemState::NoItem | ItemState::Gone { .. } if mon.item.is_some() => {
            return err("item", format!("{:?} vs {:?}", view.item, mon.item));
        }
        _ => {}
    }
    if let AbilityState::Known(a) = view.base_ability
        && a != mon.base_ability
    {
        let name = |a: pikapro_data::AbilityId| battle.format().dex.ability(a).name;
        return err("base ability", format!("{} vs {}", name(a), name(mon.base_ability)));
    }
    if !view.is_active()
        && let AbilityState::Known(a) = view.ability
        && a != mon.ability
    {
        let name = |a: pikapro_data::AbilityId| battle.format().dex.ability(a).name;
        return err("benched ability", format!("{} vs {}", name(a), name(mon.ability)));
    }
    if let Some(t) = view.tera_type
        && t != mon.tera_type
    {
        return err("tera type", format!("{t:?} vs {:?}", mon.tera_type));
    }
    if !mon.is_fainted() && view.is_revealed() && view.flags.has(MonFlags::TERASTALLIZED) != mon.terastallized {
        return err("terastallized", format!("{:?}", view.flags));
    }
    let c = mon.volatile_counters;
    let own: &[pikapro::MoveSlot] = match &c.transform {
        Some(base) => &base.moves,
        None => &mon.moves,
    };
    let has = |id: MoveId| {
        own.iter().any(|m| m.id == id)
            || mon.volatiles.has(Volatiles::MIMIC) && c.mimic_original.id == id
            || id == moves::SKETCH
    };
    for m in view.moves.iter() {
        if !has(m.id) {
            let name = battle.format().dex.move_(m.id).name;
            return err("move", format!("{name} not in {:?}", own.iter().map(|m| m.id).collect::<Vec<_>>()));
        }
    }
    Ok(())
}
