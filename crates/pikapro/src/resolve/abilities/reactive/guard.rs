//! Status, volatile and stat-change protection: `SetStatus` (Immunity,
//! Insomnia, Limber, Leaf Guard, Purifying Salt, the veils, ...),
//! `TryAddVolatile` (Inner Focus, Own Tempo, Aroma Veil, the Yawn
//! blockers), `Immunity` (Magma Armor, Overcoat, Oblivious), `Update` (the
//! cures), `TryBoost` (Clear Body and the like, Mirror Armor, Guard Dog,
//! Intimidate's blockers), `ChangeBoost` (Contrary, Simple),
//! `AfterEachBoost` (Competitive, Defiant), `AfterBoost` (Rattled),
//! `AfterSetStatus` (Synchronize, Poison Puppeteer) and
//! `ModifySecondaries` (Shield Dust).

use pikapro_data::gen9::{abilities as a, species};
use pikapro_data::{AbilityId, BoostId, EventPrefix, StatBoosts, StatusId, Type};

use super::{block, boost, effect_has_status, immune, mon};
use crate::battle::Battle;
use crate::event::{BattleEvent, Cause, EffectName, LineArg};
use crate::resolve::abilities;
use crate::resolve::events::effect_has_secondaries;
use crate::resolve::events::{Boosts, Condition, Effect, EventCtx, Relay};
use crate::resolve::{actor, boosts, conditions, manipulation, status, volatiles};
use crate::state::{Position, Status, Volatiles, Weather};

/// Sun or Desolate Land for the holder (`target.effectiveWeather()`).
fn sunny(battle: &Battle, pos: Position) -> bool {
    matches!(abilities::pokemon_weather(battle, pos), Some(Weather::Sun | Weather::HarshSun))
}

/// Whether the Pokémon at `pos` has the Grass type.
fn grass(battle: &Battle, pos: Position) -> bool {
    mon(battle, pos).is_some_and(|m| manipulation::has_type(m, Type::Grass))
}

// ---------------------------------------------------------------------------
// Status.

/// `onSetStatus` / `onAllySetStatus(status, target, source, effect)`.
pub(super) fn set_status(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    use EventPrefix::{Ally, On};
    let target = ev.target_pos()?;
    let Relay::Status(s) = ev.relay else { return None };
    let poison = matches!(s, StatusId::Poison | StatusId::Toxic);
    // The status blocked, `-immune|<target>|[from] ability: X` when the
    // effect inflicts it (a status move, Synchronize).
    let refuse = |battle: &mut Battle, applies: bool| {
        if !applies {
            return None;
        }
        if effect_has_status(battle, ev.effect) {
            immune(battle, target, ability);
        }
        Some(Relay::False)
    };
    match (ability, ev.prefix) {
        (a::IMMUNITY | a::PASTEL_VEIL, On) => refuse(battle, poison),
        (a::INSOMNIA | a::VITAL_SPIRIT, On) => refuse(battle, s == StatusId::Sleep),
        (a::LIMBER, On) => refuse(battle, s == StatusId::Paralysis),
        (a::WATER_VEIL | a::THERMAL_EXCHANGE, On) => refuse(battle, s == StatusId::Burn),
        (a::LEAF_GUARD, On) => {
            let sun = sunny(battle, target);
            refuse(battle, sun)
        }
        (a::PURIFYING_SALT | a::COMATOSE, On) => refuse(battle, true),
        // An ally's poisoning: `-block` when the effect inflicts it.
        (a::PASTEL_VEIL, Ally) => {
            if !poison {
                return None;
            }
            if effect_has_status(battle, ev.effect) {
                block(battle, target, ability, holder);
            }
            Some(Relay::False)
        }
        (a::SWEET_VEIL, Ally) => {
            if s != StatusId::Sleep {
                return None;
            }
            block(battle, target, ability, holder);
            Some(Relay::Null)
        }
        // A Grass ally's status from someone else (not Yawn's sleep);
        // announced for a move without secondaries.
        (a::FLOWER_VEIL, Ally) => {
            let yawn = ev.effect == Effect::volatile(Volatiles::YAWN);
            let other = ev.source.is_some_and(|src| src != target);
            if !grass(battle, target) || !other || ev.effect == Effect::None || yawn {
                return None;
            }
            if ev.effect.move_id().is_some() && !effect_has_secondaries(battle, ev.effect) {
                block(battle, target, ability, holder);
            }
            Some(Relay::Null)
        }
        _ => None,
    }
}

/// `onTryAddVolatile` / `onAllyTryAddVolatile(status, target, source,
/// effect)`.
pub(super) fn try_add_volatile(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    use EventPrefix::{Ally, On};
    let target = ev.target_pos()?;
    let Relay::Id(id) = ev.relay else { return None };
    match (ability, ev.prefix) {
        (a::INNER_FOCUS, On) => (id == "flinch").then_some(Relay::Null),
        (a::OWN_TEMPO, On) => (id == "confusion").then_some(Relay::Null),
        (a::INSOMNIA | a::VITAL_SPIRIT | a::PURIFYING_SALT, On) if id == "yawn" => {
            immune(battle, target, ability);
            Some(Relay::Null)
        }
        (a::LEAF_GUARD, On) if id == "yawn" && sunny(battle, target) => {
            immune(battle, target, ability);
            Some(Relay::Null)
        }
        (a::SWEET_VEIL, Ally) if id == "yawn" => {
            block(battle, target, ability, holder);
            Some(Relay::Null)
        }
        (a::FLOWER_VEIL, Ally) if id == "yawn" && grass(battle, target) => {
            block(battle, target, ability, holder);
            Some(Relay::Null)
        }
        // The mental volatiles; announced when a move causes them.
        (a::AROMA_VEIL, Ally) if matches!(id, "attract" | "disable" | "encore" | "healblock" | "taunt" | "torment") => {
            if ev.effect.is_move() {
                block(battle, target, ability, holder);
            }
            Some(Relay::Null)
        }
        _ => None,
    }
}

/// `onImmunity(type, pokemon)`.
pub(super) fn immunity(ev: &mut EventCtx<'_>, ability: AbilityId) -> Option<Relay> {
    let Relay::Id(id) = ev.relay else { return None };
    let immune = match ability {
        a::MAGMA_ARMOR => id == "frz",
        a::OVERCOAT => matches!(id, "sandstorm" | "hail" | "powder"),
        a::OBLIVIOUS => id == "attract",
        _ => false,
    };
    immune.then_some(Relay::False)
}

/// `onUpdate(pokemon)`: the cures (`-activate|<holder>|ability: X`, then
/// the status or volatile ends).
pub(super) fn update(battle: &mut Battle, holder: Position, ability: AbilityId) -> Option<Relay> {
    let m = mon(battle, holder)?;
    let status = m.status;
    let volatiles = m.volatiles;
    let cures = match ability {
        a::IMMUNITY | a::PASTEL_VEIL => matches!(status, Status::Poison | Status::Toxic { .. }),
        a::INSOMNIA | a::VITAL_SPIRIT => matches!(status, Status::Sleep { .. }),
        a::LIMBER => status == Status::Paralysis,
        a::MAGMA_ARMOR => matches!(status, Status::Freeze { .. }),
        a::WATER_VEIL | a::THERMAL_EXCHANGE => status == Status::Burn,
        a::OBLIVIOUS => {
            if volatiles.has(Volatiles::ATTRACT) {
                abilities::activate(battle, holder, ability);
                conditions::remove(battle, holder, Volatiles::ATTRACT);
                let actor = actor(battle, holder);
                battle.emit(BattleEvent::EffectStartEnd {
                    actor,
                    start: false,
                    what: LineArg::Text(""),
                    effect: Some(EffectName::Move(pikapro_data::gen9::moves::ATTRACT)),
                    cause: Some(Cause::ability(ability)),
                    silent: false,
                });
            }
            if volatiles.has(Volatiles::TAUNT) {
                abilities::activate(battle, holder, ability);
                volatiles::remove_volatile(battle, holder, Volatiles::TAUNT);
            }
            return None;
        }
        a::OWN_TEMPO => {
            if volatiles.has(Volatiles::CONFUSION) {
                abilities::activate(battle, holder, ability);
                volatiles::remove_volatile(battle, holder, Volatiles::CONFUSION);
            }
            return None;
        }
        _ => false,
    };
    if cures {
        abilities::activate(battle, holder, ability);
        status::cure_status(battle, holder);
    }
    None
}

// ---------------------------------------------------------------------------
// Stat changes.

/// `-fail|<target>|unboost|<stat>|[from] ability: X|[of] <target>`.
fn fail_unboost(battle: &mut Battle, target: Position, stat: Option<&'static str>, ability: AbilityId) {
    let a = actor(battle, target);
    battle.emit(BattleEvent::FailFrom {
        target: a,
        what: LineArg::Text("unboost"),
        detail: stat.map(LineArg::Text),
        cause: Cause::ability(ability).of(a),
    });
}

/// The named stats of a boosts object and their changes, in key order
/// (`for (i in boost)` over a snapshot).
fn entries(b: &Boosts) -> ([(BoostId, i8); 7], usize) {
    let mut out = [(BoostId::Atk, 0); 7];
    let mut n = 0;
    for entry in b.iter() {
        out[n] = entry;
        n += 1;
    }
    (out, n)
}

/// Deletes the drops of a boosts object; whether there were any.
fn delete_drops(b: &mut Boosts) -> bool {
    let mut any = false;
    for id in BoostId::ALL {
        if b.get(id).is_some_and(|v| v < 0) {
            b.delete(id);
            any = true;
        }
    }
    any
}

/// `onTryBoost` / `onAllyTryBoost(boost, target, source, effect)`.
pub(super) fn try_boost(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    use EventPrefix::{Ally, On};
    let target = ev.target_pos()?;
    let Relay::Boosts(mut b) = ev.relay else { return None };
    let source = ev.source;
    let by_self = source == Some(target);
    let effect = ev.effect;
    let secondaries = effect_has_secondaries(battle, effect);
    // Big Pecks, Clear Body, Full Metal Body and White Smoke stay silent for
    // Octolock's drops (`effect.id !== 'octolock'`).
    let octolock = effect == Effect::Move(pikapro_data::gen9::moves::OCTOLOCK);
    let intimidate = effect == Effect::Ability(a::INTIMIDATE) && b.get(BoostId::Atk).is_some_and(|v| v != 0);
    match (ability, ev.prefix) {
        // One stat's drops (announced unless a secondary effect caused them).
        (a::BIG_PECKS | a::HYPER_CUTTER | a::KEEN_EYE, On) => {
            if by_self {
                return None;
            }
            // Showdown names the stat by its id (`def`, `atk`, `accuracy`).
            let id = match ability {
                a::BIG_PECKS => BoostId::Def,
                a::HYPER_CUTTER => BoostId::Atk,
                _ => BoostId::Accuracy,
            };
            if b.get(id).is_some_and(|v| v < 0) {
                b.delete(id);
                if !(secondaries || octolock && ability == a::BIG_PECKS) {
                    fail_unboost(battle, target, Some(id.id()), ability);
                }
            }
            Some(Relay::Boosts(b))
        }
        // Every drop.
        (a::CLEAR_BODY | a::WHITE_SMOKE | a::FULL_METAL_BODY, On) => {
            if by_self {
                return None;
            }
            if delete_drops(&mut b) && !secondaries && !octolock {
                fail_unboost(battle, target, None, ability);
            }
            Some(Relay::Boosts(b))
        }
        // Intimidate's Attack drop.
        (a::INNER_FOCUS | a::OBLIVIOUS | a::OWN_TEMPO, On) => {
            if !intimidate {
                return None;
            }
            b.delete(BoostId::Atk);
            fail_unboost(battle, target, Some(BoostId::Atk.id()), ability);
            Some(Relay::Boosts(b))
        }
        // Priority 2: Intimidate raises Attack instead.
        (a::GUARD_DOG, On) => {
            if !intimidate {
                return None;
            }
            b.delete(BoostId::Atk);
            let atk = StatBoosts::new([1, 0, 0, 0, 0, 0, 0]);
            boosts::boost_from(battle, target, atk, Some(target), Effect::Ability(ability), false, true);
            Some(Relay::Boosts(b))
        }
        // Each drop (not at -6 already) goes back to its source.
        (a::MIRROR_ARMOR, On) => {
            let source = source?;
            if by_self || effect == Effect::Ability(a::MIRROR_ARMOR) {
                return None;
            }
            let (named, n) = entries(&b);
            for &(id, v) in &named[..n] {
                if v >= 0 {
                    continue;
                }
                if mon(battle, target)?.boosts.get(id) == -6 {
                    continue;
                }
                b.delete(id);
                if mon(battle, source).is_some_and(|m| m.hp > 0) {
                    abilities::show(battle, target, ability);
                    let reflected = boosts::one_boost(id, v);
                    boosts::boost_from(battle, source, reflected, Some(target), Effect::Ability(ability), true, false);
                }
            }
            Some(Relay::Boosts(b))
        }
        // A Grass ally's drops caused by someone else.
        (a::FLOWER_VEIL, Ally) => {
            if by_self || !grass(battle, target) {
                return None;
            }
            if delete_drops(&mut b) && !secondaries {
                block(battle, target, ability, holder);
            }
            Some(Relay::Boosts(b))
        }
        _ => None,
    }
}

/// `onChangeBoost`: Contrary reverses the changes, Simple doubles them.
pub(super) fn change_boost(ev: &mut EventCtx<'_>, ability: AbilityId) -> Option<Relay> {
    let Relay::Boosts(mut b) = ev.relay else { return None };
    let (named, n) = entries(&b);
    for &(id, v) in &named[..n] {
        b.set(id, if ability == a::CONTRARY { -v } else { v.saturating_mul(2) });
    }
    Some(Relay::Boosts(b))
}

/// Competitive and Defiant's `onAfterEachBoost`: a stat lowered by a foe
/// raises Special Attack / Attack by 2.
pub(super) fn after_each_boost(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    let source = ev.source?;
    if source.side == holder.side {
        return None;
    }
    let Relay::Boosts(b) = ev.relay else { return None };
    if b.iter().any(|(_, v)| v < 0) {
        let raise = if ability == a::COMPETITIVE {
            StatBoosts::new([0, 0, 2, 0, 0, 0, 0])
        } else {
            StatBoosts::new([2, 0, 0, 0, 0, 0, 0])
        };
        boosts::boost_from(battle, holder, raise, Some(holder), Effect::Ability(ability), false, true);
    }
    None
}

/// Rattled's `onAfterBoost`: Intimidate lowering Attack raises Speed.
pub(super) fn rattled_after_boost(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position) -> Option<Relay> {
    let Relay::Boosts(b) = ev.relay else { return None };
    if ev.effect == Effect::Ability(a::INTIMIDATE) && b.get(BoostId::Atk).is_some_and(|v| v != 0) {
        boost(battle, holder, StatBoosts::new([0, 0, 0, 0, 1, 0, 0]), ev.source, a::RATTLED);
    }
    None
}

// ---------------------------------------------------------------------------
// After a status.

/// Synchronize's `onAfterSetStatus`: a status from another Pokémon (not
/// sleep, freeze, or Toxic Spikes') is passed back to it.
pub(super) fn synchronize(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position) -> Option<Relay> {
    let source = ev.source?;
    let Relay::Status(s) = ev.relay else { return None };
    if source == holder || ev.effect == Effect::Side(crate::state::SideCondition::ToxicSpikes) {
        return None;
    }
    if matches!(s, StatusId::Sleep | StatusId::Freeze) {
        return None;
    }
    abilities::activate(battle, holder, a::SYNCHRONIZE);
    let effect = Effect::Condition(Condition::Synchronize);
    status::set_status_from(battle, source, s, Some(holder), effect, false);
    None
}

/// Poison Puppeteer's `onAnyAfterSetStatus`: Pecharunt's moves that poison
/// also confuse.
pub(super) fn poison_puppeteer(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position) -> Option<Relay> {
    let source = ev.source?;
    let target = ev.target_pos()?;
    let Relay::Status(s) = ev.relay else { return None };
    let species = mon(battle, source)?.species;
    if battle.format.dex.species(species).base_species != species::PECHARUNT {
        return None;
    }
    if source != holder || target == source || !matches!(ev.effect, Effect::Move(_)) {
        return None;
    }
    if matches!(s, StatusId::Poison | StatusId::Toxic) {
        let effect = Effect::Ability(a::POISON_PUPPETEER);
        volatiles::add_volatile_from(battle, target, "confusion", Some(holder), true, effect);
    }
    None
}

/// Shield Dust's `onModifySecondaries`: only the secondaries that affect
/// the user (`self`) are left (the mask's bits in roll order,
/// `secondaries::with_self`).
pub(super) fn shield_dust(battle: &Battle, ev: &mut EventCtx<'_>) -> Option<Relay> {
    let Relay::Secondaries(kept) = ev.relay else { return None };
    let data = ev.move_data()?;
    Some(Relay::Secondaries(kept & crate::resolve::secondaries::with_self(battle, data)))
}
