//! Berries (`data/items.ts`, `isBerry`).
//!
//! A berry is eaten through `items::eat_item` (Showdown's `eatItem`): its
//! trigger (`onUpdate`, `onSourceModifyDamage`, `onDamagingHit`, ...)
//! calls it, `TryEatItem` can stop it (its own `onTryEatItem` for the
//! healing berries; Unnerve, Ripen), and its `onEat` does the effect. Bug
//! Bite, Pluck and Fling run the `Eat` callback for another Pokémon, which
//! is then `holder` here.

use pikapro_data::gen9::{abilities, items as i};
use pikapro_data::{BoostId, Event, EventPrefix, ItemId, MoveCategory, MoveFlags, Stat, Type};

use crate::battle::Battle;
use crate::event::{BattleEvent, EffectName, ItemEndNote, LineArg};
use crate::resolve::abilities::has_ability;
use crate::resolve::boosts::one_boost;
use crate::resolve::events::{Effect, EventCtx, Relay};
use crate::resolve::{actor, boosts, damage_by, heal_by, status, try_heal};
use crate::state::{Position, Status, Volatiles};

/// The resist berries: the type each one weakens (Chilan Berry: Normal,
/// whatever its effectiveness).
pub(super) fn resist_type(item: ItemId) -> Option<Type> {
    Some(match item {
        i::BABIRI_BERRY => Type::Steel,
        i::CHARTI_BERRY => Type::Rock,
        i::CHILAN_BERRY => Type::Normal,
        i::CHOPLE_BERRY => Type::Fighting,
        i::COBA_BERRY => Type::Flying,
        i::COLBUR_BERRY => Type::Dark,
        i::HABAN_BERRY => Type::Dragon,
        i::KASIB_BERRY => Type::Ghost,
        i::KEBIA_BERRY => Type::Poison,
        i::OCCA_BERRY => Type::Fire,
        i::PASSHO_BERRY => Type::Water,
        i::PAYAPA_BERRY => Type::Psychic,
        i::RINDO_BERRY => Type::Grass,
        i::ROSELI_BERRY => Type::Fairy,
        i::SHUCA_BERRY => Type::Ground,
        i::TANGA_BERRY => Type::Bug,
        i::WACAN_BERRY => Type::Electric,
        i::YACHE_BERRY => Type::Ice,
        _ => return None,
    })
}

/// The confusion berries (Figy, Wiki, Mago, Aguav, Iapapa): the stat whose
/// lowering nature makes the eater confused.
fn confusion_stat(item: ItemId) -> Option<Stat> {
    Some(match item {
        i::FIGY_BERRY => Stat::Atk,
        i::WIKI_BERRY => Stat::SpA,
        i::MAGO_BERRY => Stat::Spe,
        i::AGUAV_BERRY => Stat::SpD,
        i::IAPAPA_BERRY => Stat::Def,
        _ => return None,
    })
}

/// The stat pinch berries (Liechi, Ganlon, Salac, Petaya, Apicot).
fn pinch_boost(item: ItemId) -> Option<BoostId> {
    Some(match item {
        i::LIECHI_BERRY => BoostId::Atk,
        i::GANLON_BERRY => BoostId::Def,
        i::SALAC_BERRY => BoostId::Spe,
        i::PETAYA_BERRY => BoostId::SpA,
        i::APICOT_BERRY => BoostId::SpD,
        _ => return None,
    })
}

/// The EV-reducing berries, whose `onEat` is `false`.
pub(super) fn is_ev_berry(item: ItemId) -> bool {
    matches!(
        item,
        i::POMEG_BERRY | i::KELPSY_BERRY | i::QUALOT_BERRY | i::HONDEW_BERRY | i::GREPA_BERRY | i::TAMATO_BERRY
    )
}

/// `item.onEat` is a function (Bug Bite, Pluck, Fling and Cud Chew set
/// `ateBerry` only then).
pub(crate) fn has_on_eat(battle: &Battle, item: ItemId) -> bool {
    battle.format.dex.item(item).handler(Event::Eat, EventPrefix::On).is_some() && !is_ev_berry(item)
}

/// A pinch berry's trigger: at a quarter of max HP or less, or half with
/// Gluttony (`abilityState.gluttony`).
fn pinch(battle: &Battle, pos: Position) -> bool {
    let Some(mon) = battle.state.at(pos) else { return false };
    let (hp, max) = (mon.hp as u32, mon.max_hp() as u32);
    hp * 4 <= max
        || hp * 2 <= max
            && has_ability(battle, pos, abilities::GLUTTONY)
            && mon.volatile_counters.ability_state.gluttony
}

fn eat(battle: &mut Battle, holder: Position, item: ItemId) {
    crate::resolve::items::eat_item(battle, holder, false, None, Effect::Item(item));
}

/// `onTryEatItem` of the healing berries: `runEvent('TryHeal', pokemon,
/// null, item, amount)` must let it heal.
fn try_eat_heal(battle: &mut Battle, holder: Position, item: ItemId, amount: u32) -> Option<Relay> {
    try_heal(battle, holder, amount, None, Effect::Item(item)).is_none().then_some(Relay::False)
}

/// The berries' `onUpdate`: whether the berry is eaten now (`None`: it has
/// none). The status berries: for their status (Lum Berry: any status, or
/// confusion); Persim Berry: confusion; Leppa Berry: a move out of PP;
/// Sitrus and Oran Berries: half HP or less; the confusion and pinch
/// berries, Lansat and Starf Berries: a quarter HP or less (half with
/// Gluttony, [`pinch`]).
///
/// It only reads the battle: an `Update` whose only handler would not eat
/// the berry is skipped ([`items::update_idle`](crate::resolve::items::update_idle)).
pub(super) fn update_eats(battle: &Battle, holder: Position, item: ItemId) -> Option<bool> {
    let mon = battle.state.side(holder.side).team.get(holder.slot as usize)?;
    Some(match item {
        i::CHERI_BERRY | i::CHESTO_BERRY | i::PECHA_BERRY | i::RAWST_BERRY | i::ASPEAR_BERRY => cures(item, mon.status),
        i::PERSIM_BERRY => mon.volatiles.has(Volatiles::CONFUSION),
        i::LUM_BERRY => mon.status != Status::None || mon.volatiles.has(Volatiles::CONFUSION),
        i::LEPPA_BERRY => mon.hp > 0 && mon.moves.iter().any(|m| m.pp == 0),
        i::SITRUS_BERRY | i::ORAN_BERRY => mon.hp as u32 * 2 <= mon.max_hp() as u32,
        _ if confusion_stat(item).is_some() || pinch_boost(item).is_some() => pinch(battle, holder),
        i::LANSAT_BERRY | i::STARF_BERRY => pinch(battle, holder),
        _ => return None,
    })
}

/// Runs a berry's callback for `ev.event` / `ev.prefix` on its holder.
pub(super) fn run(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position, item: ItemId) -> Option<Relay> {
    use EventPrefix::{On, Source};
    let mon = battle.state.side(holder.side).team.get(holder.slot as usize)?;
    let max_hp = mon.max_hp() as u32;
    match (item, ev.event, ev.prefix) {
        // `onUpdate`: [`update_eats`].
        (_, Event::Update, On) => {
            if update_eats(battle, holder, item) == Some(true) {
                eat(battle, holder, item);
            }
            None
        }
        // The status berries: `onEat` cures their status (Lum Berry: any
        // status and confusion, also right after a status is set).
        (i::CHERI_BERRY | i::CHESTO_BERRY | i::PECHA_BERRY | i::RAWST_BERRY | i::ASPEAR_BERRY, Event::Eat, On) => {
            if cures(item, mon.status) {
                status::cure_status(battle, holder);
            }
            None
        }
        (i::PERSIM_BERRY, Event::Eat, On) => {
            crate::resolve::volatiles::remove_volatile(battle, holder, Volatiles::CONFUSION);
            None
        }
        (i::LUM_BERRY, Event::AfterSetStatus, On) => {
            eat(battle, holder, item);
            None
        }
        (i::LUM_BERRY, Event::Eat, On) => {
            status::cure_status(battle, holder);
            crate::resolve::volatiles::remove_volatile(battle, holder, Volatiles::CONFUSION);
            None
        }
        // Leppa Berry: `onEat`, a move out of PP (or else the first move
        // missing PP) gets 10 PP back, 20 with Ripen.
        (i::LEPPA_BERRY, Event::Eat, On) => {
            let index =
                mon.moves.iter().position(|m| m.pp == 0).or_else(|| mon.moves.iter().position(|m| m.pp < m.max_pp))?;
            let added = if has_ability(battle, holder, abilities::RIPEN) { 20 } else { 10 };
            let slot = &mut battle.state.side_mut(holder.side).team[holder.slot as usize].moves[index];
            slot.pp = (slot.pp + added).min(slot.max_pp);
            let move_id = slot.id;
            let a = actor(battle, holder);
            battle.emit(BattleEvent::ActivateEffect {
                actor: a,
                effect: EffectName::Item(item),
                args: [Some(LineArg::Move(move_id)), Some(LineArg::Text("[consumed]"))],
                of: None,
            });
            None
        }
        // Sitrus Berry: a quarter of max HP at half HP
        // or less; not while it couldn't heal (Heal Block).
        (i::SITRUS_BERRY, Event::TryEatItem, On) => {
            try_eat_heal(battle, holder, item, crate::resolve::hp_fraction(max_hp, 1, 4))
        }
        (i::SITRUS_BERRY, Event::Eat, On) => {
            heal_by(battle, holder, crate::resolve::hp_fraction(max_hp, 1, 4), ev.source, Effect::Item(item));
            None
        }
        // Oran Berry: 10 HP at half HP or less.
        (i::ORAN_BERRY, Event::TryEatItem, On) => try_eat_heal(battle, holder, item, 10),
        (i::ORAN_BERRY, Event::Eat, On) => {
            heal_by(battle, holder, 10, ev.source, Effect::Item(item));
            None
        }
        // The confusion berries: a third of max HP at a quarter HP (half
        // with Gluttony); confused if the nature lowers their stat.
        (_, Event::TryEatItem, On) if confusion_stat(item).is_some() => {
            try_eat_heal(battle, holder, item, crate::resolve::hp_fraction(max_hp, 1, 3))
        }
        (_, Event::Eat, On) if confusion_stat(item).is_some() => {
            // (The nature, read before healing: it doesn't change.)
            let lowered = mon.nature.modifiers().map(|(_, minus)| minus);
            heal_by(battle, holder, crate::resolve::hp_fraction(max_hp, 1, 3), ev.source, Effect::Item(item));
            if lowered == confusion_stat(item) {
                // The effect is the berry, not the move being used (if any):
                // Misty Terrain blocks it silently, and Axe Kick's longer
                // confusion doesn't apply. The move stays active: a Mold
                // Breaker move suppresses the eater's Own Tempo.
                let effect = Effect::Item(item);
                // `addVolatile('confusion')` takes the `Eat` event's source
                // (`this.battle.event.source`), so Safeguard stops the
                // confusion of a berry a foe made it eat (Fling, Teatime;
                // Teatime's source is the field: never the eater).
                let source = match ev.effect {
                    Effect::Move(m) if m == pikapro_data::gen9::moves::TEATIME => {
                        Some(Position::new(holder.side.foe(), 0))
                    }
                    _ => ev.source,
                };
                crate::resolve::volatiles::add_volatile_from(battle, holder, "confusion", source, true, effect);
            }
            None
        }
        (_, Event::Eat, On) if pinch_boost(item).is_some() => {
            let id = pinch_boost(item)?;
            boosts::boost_from(battle, holder, one_boost(id, 1), ev.source, Effect::Item(item), false, false);
            None
        }
        // Lansat Berry: Focus Energy.
        (i::LANSAT_BERRY, Event::Eat, On) => {
            crate::resolve::volatiles::add_volatile(battle, holder, "focusenergy", None, false);
            None
        }
        // Starf Berry: +2 to a random stat that can still rise.
        (i::STARF_BERRY, Event::Eat, On) => {
            let b = mon.boosts;
            let mut stats = [BoostId::Atk; 5];
            let mut n = 0;
            for id in [BoostId::Atk, BoostId::Def, BoostId::SpA, BoostId::SpD, BoostId::Spe] {
                if b.get(id) < 6 {
                    stats[n] = id;
                    n += 1;
                }
            }
            if n > 0 {
                let id = *battle.rng.sample(&stats[..n]);
                boosts::boost_from(battle, holder, one_boost(id, 2), ev.source, Effect::Item(item), false, false);
            }
            None
        }
        // Micle Berry: eaten at the end of the turn; the next move is more
        // accurate.
        (i::MICLE_BERRY, Event::Residual, On) => {
            if pinch(battle, holder) {
                eat(battle, holder, item);
            }
            None
        }
        (i::MICLE_BERRY, Event::Eat, On) => {
            super::add_micle(battle, holder);
            None
        }
        // Custap Berry: `onFractionalPriority` (priority -2): unless another
        // handler already moved it up (the relay, in tenths), the move goes
        // first in its priority bracket.
        (i::CUSTAP_BERRY, Event::FractionalPriority, On) => {
            // `recharge` has no data (no category): the berry is still eaten.
            let status = ev.move_data().is_some_and(|d| d.category == MoveCategory::Status);
            if status && has_ability(battle, holder, abilities::MYCELIUM_MIGHT) {
                return None;
            }
            if ev.value() <= 0
                && pinch(battle, holder)
                && crate::resolve::items::eat_item(battle, holder, false, None, Effect::Item(item))
            {
                let a = actor(battle, holder);
                battle.emit(BattleEvent::ActivateEffect {
                    actor: a,
                    effect: EffectName::Item(item),
                    args: [Some(LineArg::Text("[consumed]")), None],
                    of: None,
                });
                return Some(Relay::Num(1));
            }
            None
        }
        // The resist berries: `onSourceModifyDamage` halves a
        // super-effective hit of their type (Chilan Berry: any Normal hit),
        // unless a substitute takes it.
        (_, Event::ModifyDamage, Source) if resist_type(item).is_some() => {
            let data = ev.move_data()?;
            let ty = resist_type(item)?;
            let effective = item == i::CHILAN_BERRY || battle.move_state.hit_data(holder).type_mod > 0;
            // Struggle's type is `???`.
            let struggle = ev.effect.move_id() == Some(pikapro_data::gen9::moves::STRUGGLE);
            if data.ty != ty || !effective || struggle {
                return None;
            }
            let hit_sub = mon.volatiles.has(Volatiles::SUBSTITUTE)
                && !data.flags.contains(MoveFlags::BYPASS_SUB)
                && !battle.move_state.infiltrates;
            if hit_sub {
                return None;
            }
            if crate::resolve::items::eat_item(battle, holder, false, ev.target_pos(), Effect::Item(item)) {
                let a = actor(battle, holder);
                battle.emit(BattleEvent::ItemEnd { actor: a, item, note: ItemEndNote::Weaken, from: None, of: None });
                return ev.chain_modify(1, 2);
            }
            None
        }
        // Enigma Berry: `onHit` by a super-effective move heals a quarter
        // of max HP.
        (i::ENIGMA_BERRY, Event::Hit, On) => {
            if battle.move_state.hit_data(holder).type_mod > 0
                && crate::resolve::items::eat_item(battle, holder, false, None, Effect::Item(item))
            {
                heal_by(battle, holder, crate::resolve::hp_fraction(max_hp, 1, 4), ev.source, Effect::Item(item));
            }
            None
        }
        (i::ENIGMA_BERRY, Event::TryEatItem, On) => {
            try_eat_heal(battle, holder, item, crate::resolve::hp_fraction(max_hp, 1, 4))
        }
        // Jaboca and Rowap Berry: `onDamagingHit` by a physical (special)
        // move damages the attacker by 1/8 of its max HP (1/4 with Ripen).
        (i::JABOCA_BERRY | i::ROWAP_BERRY, Event::DamagingHit, On) => {
            let category = if item == i::JABOCA_BERRY { MoveCategory::Physical } else { MoveCategory::Special };
            let source = ev.source?;
            let data = ev.move_data()?;
            let up = battle.state.at(source).is_some_and(|m| m.hp > 0)
                && crate::resolve::events::is_active(battle, source)
                && !has_ability(battle, source, abilities::MAGIC_GUARD);
            if data.category == category
                && up
                && crate::resolve::items::eat_item(battle, holder, false, None, Effect::Item(item))
            {
                let divisor = if has_ability(battle, holder, abilities::RIPEN) { 4 } else { 8 };
                let amount =
                    crate::resolve::hp_fraction(battle.state.at(source).map_or(0, |m| m.max_hp() as u32), 1, divisor);
                damage_by(battle, source, amount, Some(holder), Effect::Item(item));
            }
            None
        }
        // Kee and Maranga Berry: `onAfterMoveSecondary` after a physical
        // (special) move, +1 Defense (Special Defense).
        (i::KEE_BERRY | i::MARANGA_BERRY, Event::AfterMoveSecondary, On) => {
            let data = ev.move_data()?;
            let category = if item == i::KEE_BERRY { MoveCategory::Physical } else { MoveCategory::Special };
            let present_heal = ev.effect.move_id() == Some(pikapro_data::gen9::moves::PRESENT) && data.heal.is_some();
            if data.category == category && !(item == i::KEE_BERRY && present_heal) {
                eat(battle, holder, item);
            }
            None
        }
        (i::KEE_BERRY | i::MARANGA_BERRY, Event::Eat, On) => {
            let id = if item == i::KEE_BERRY { BoostId::Def } else { BoostId::SpD };
            boosts::boost_from(battle, holder, one_boost(id, 1), ev.source, Effect::Item(item), false, false);
            None
        }
        // The EV berries: `onEat: false`.
        (_, Event::Eat, On) if is_ev_berry(item) => Some(Relay::False),
        _ => None,
    }
}

/// Whether a status berry cures `status`.
fn cures(item: ItemId, status: Status) -> bool {
    match item {
        i::CHERI_BERRY => status == Status::Paralysis,
        i::CHESTO_BERRY => matches!(status, Status::Sleep { .. }),
        i::PECHA_BERRY => matches!(status, Status::Poison | Status::Toxic { .. }),
        i::RAWST_BERRY => status == Status::Burn,
        i::ASPEAR_BERRY => matches!(status, Status::Freeze { .. }),
        _ => false,
    }
}
