//! Held items that stay held.
//!
//! Showdown references: the items in `data/items.ts` that `items::owner`
//! assigns to [`Owner::Held`](super::Owner::Held) (Leftovers, Life Orb, the
//! Choice items, the type-boosting items and Plates, Rocky Helmet, Assault
//! Vest, Eviolite, ...), and the `choicelock` condition in
//! `data/conditions.ts`. Items checked at their call sites (Damp Rock,
//! Heavy-Duty Boots, Light Clay, ...) use `items::has_item` there, and the
//! items without a battle effect (Poké Balls, evolution items) need nothing.
//!
//! Adding an item: see `items/mod.rs`. One arm per callback in [`run`];
//! this module's volatiles (bits 108..=111, and `choicelock`) get their
//! handlers in [`volatile_spec`].

use pikapro_data::gen9::{items as i, moves, species};
use pikapro_data::{Event, EventPrefix, ItemId, MoveCategory, MoveFlags, MoveId, StatusId, Type};

use super::{has_item, holder_of};
use crate::battle::Battle;
use crate::event::{BattleEvent, Cause, EffectName, LineArg};
use crate::resolve::events::{self, Condition, Effect, EventCtx, Relay, Spec, Target};
use crate::resolve::handlers::Handler;
use crate::resolve::manipulation::has_type;
use crate::resolve::{actor, damage_by, heal_by, hp_fraction, queue};
use crate::state::{MoveResult, PokemonState, Position, Volatiles, Weather};

/// `return this.chainModify([modifier, 4096])`.
fn chain(ev: &mut EventCtx<'_>, modifier: u32) -> Option<Relay> {
    ev.chain(modifier);
    None
}

/// `move.type`: Struggle's `onModifyMove` made it typeless (`???`).
fn move_type(ev: &EventCtx<'_>) -> Option<Type> {
    if ev.effect.move_id() == Some(moves::STRUGGLE) { None } else { ev.move_data().map(|d| d.ty) }
}

/// The holder, by its team position (on the field or not).
fn mon(battle: &Battle, pos: Position) -> Option<&PokemonState> {
    battle.state.side(pos.side).team.get(pos.slot as usize)
}

/// The holder's `baseSpecies` (Showdown's `pokemon.baseSpecies`).
fn base_species(battle: &Battle, pos: Position) -> Option<&'static pikapro_data::SpeciesData> {
    let dex = battle.format.dex;
    mon(battle, pos).map(|m| dex.species(m.base_species))
}

/// The type a type-boosting item (`onBasePower` priority 15: 4915/4096
/// for moves of its type) boosts. Plates use their `onPlate` type.
fn boosted_type(battle: &Battle, item: ItemId) -> Option<Type> {
    Some(match item {
        i::BLACK_BELT => Type::Fighting,
        i::BLACK_GLASSES => Type::Dark,
        i::CHARCOAL => Type::Fire,
        i::DRAGON_FANG => Type::Dragon,
        i::FAIRY_FEATHER => Type::Fairy,
        i::HARD_STONE => Type::Rock,
        i::MAGNET => Type::Electric,
        i::METAL_COAT => Type::Steel,
        i::MIRACLE_SEED => Type::Grass,
        i::MYSTIC_WATER => Type::Water,
        i::NEVER_MELT_ICE => Type::Ice,
        i::POISON_BARB => Type::Poison,
        i::SHARP_BEAK => Type::Flying,
        i::SILK_SCARF => Type::Normal,
        i::SILVER_POWDER => Type::Bug,
        i::SOFT_SAND => Type::Ground,
        i::SPELL_TAG => Type::Ghost,
        i::TWISTED_SPOON => Type::Psychic,
        // A Plate (Z-Crystals belong to `items::mega`).
        _ => return battle.format.dex.item(item).plate,
    })
}

/// The legendary orbs and crystals and Soul Dew (`onBasePower` priority
/// 15): the National Dex numbers of the `baseSpecies` they work for and
/// the two types they boost.
fn legendary_boost(item: ItemId) -> Option<(&'static [i16], [Type; 2])> {
    Some(match item {
        i::ADAMANT_ORB | i::ADAMANT_CRYSTAL => (&[483], [Type::Steel, Type::Dragon]),
        i::LUSTROUS_ORB | i::LUSTROUS_GLOBE => (&[484], [Type::Water, Type::Dragon]),
        i::GRISEOUS_ORB | i::GRISEOUS_CORE => (&[487], [Type::Ghost, Type::Dragon]),
        i::SOUL_DEW => (&[380, 381], [Type::Psychic, Type::Dragon]),
        _ => return None,
    })
}

/// The National Dex number whose `baseSpecies` an item can't be taken from
/// (`onTakeItem`: the Plates, the Origin items, the Rusted weapons).
fn locked_to(item: ItemId) -> Option<i16> {
    Some(match item {
        i::ADAMANT_CRYSTAL => 483,
        i::LUSTROUS_GLOBE => 484,
        i::GRISEOUS_CORE => 487,
        i::RUSTED_SWORD => 888,
        i::RUSTED_SHIELD => 889,
        i::DRACO_PLATE
        | i::DREAD_PLATE
        | i::EARTH_PLATE
        | i::FIST_PLATE
        | i::FLAME_PLATE
        | i::ICICLE_PLATE
        | i::INSECT_PLATE
        | i::IRON_PLATE
        | i::MEADOW_PLATE
        | i::MIND_PLATE
        | i::PIXIE_PLATE
        | i::SKY_PLATE
        | i::SPLASH_PLATE
        | i::SPOOKY_PLATE
        | i::STONE_PLATE
        | i::TOXIC_PLATE
        | i::ZAP_PLATE => 493,
        _ => return None,
    })
}

/// Ogerpon's masks (`onBasePower` priority 15 for the forme whose name
/// starts with the mask's, `onTakeItem` false for any Ogerpon).
fn mask_forme(item: ItemId) -> Option<&'static str> {
    Some(match item {
        i::CORNERSTONE_MASK => "Ogerpon-Cornerstone",
        i::HEARTHFLAME_MASK => "Ogerpon-Hearthflame",
        i::WELLSPRING_MASK => "Ogerpon-Wellspring",
        _ => return None,
    })
}

/// [`super::update_idle`] for this module's items.
pub(super) fn update_idle(battle: &Battle, holder: Position, item: ItemId) -> bool {
    match item {
        i::UTILITY_UMBRELLA => {
            battle.state.side(holder.side).team.get(holder.slot as usize).is_some_and(umbrella_update_idle)
        }
        _ => false,
    }
}

/// Utility Umbrella's `onUpdate` does nothing: it didn't end while
/// ignored.
fn umbrella_update_idle(mon: &PokemonState) -> bool {
    !mon.volatile_counters.item_state.umbrella_inactive
}

/// Runs `item`'s callback for `ev.event` / `ev.prefix` on its holder.
pub(super) fn run(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position, item: ItemId) -> Option<Relay> {
    use EventPrefix::{On, Source};
    match (item, ev.event, ev.prefix) {
        // `onResidual` (order 5, sub-order 4): heals 1/16 of max HP.
        (i::LEFTOVERS, Event::Residual, On) => {
            let max_hp = mon(battle, holder)?.max_hp() as u32;
            heal_by(battle, holder, hp_fraction(max_hp, 1, 16), None, Effect::Item(item));
            None
        }
        // `onResidual` (order 5, sub-order 4): a Poison type heals 1/16 of
        // its max HP, any other takes 1/8.
        (i::BLACK_SLUDGE, Event::Residual, On) => {
            let m = mon(battle, holder)?;
            let max_hp = m.max_hp() as u32;
            if has_type(m, Type::Poison) {
                heal_by(battle, holder, hp_fraction(max_hp, 1, 16), None, Effect::Item(item));
            } else {
                damage_by(battle, holder, hp_fraction(max_hp, 1, 8), None, Effect::Item(item));
            }
            None
        }
        // `onResidual` (order 28, sub-order 3): `trySetStatus` on the holder
        // (silent when it fails: the item has no `status`).
        (i::TOXIC_ORB | i::FLAME_ORB, Event::Residual, On) => {
            let status = if item == i::TOXIC_ORB { StatusId::Toxic } else { StatusId::Burn };
            let effect = Effect::Item(item);
            crate::resolve::status::set_status_from(battle, holder, status, Some(holder), effect, true);
            None
        }
        // The Choice items: `onStart` ends a lock (silently), `onModifyMove`
        // locks the holder into the move (`choicelock`), and each boosts a
        // stat 1.5x.
        (i::CHOICE_BAND | i::CHOICE_SCARF | i::CHOICE_SPECS, Event::Start, On) => {
            events::remove_volatile_flag(battle, holder, Volatiles::CHOICE_LOCK);
            None
        }
        (i::CHOICE_BAND | i::CHOICE_SCARF | i::CHOICE_SPECS, Event::ModifyMove, On) => {
            let move_id = ev.effect.move_id()?;
            // `choicelock`'s `onStart` fails for a move without an ID
            // (`recharge`) or bounced back (`hasBounced`).
            let lock = Volatiles::CHOICE_LOCK;
            if events::add_volatile_flag(battle, holder, lock, "choicelock", Some(holder), Effect::Item(item)) {
                if battle.move_state.has_bounced {
                    events::remove_volatile_flag(battle, holder, lock);
                } else if let Some(m) = battle.state.at_mut(holder) {
                    m.volatile_counters.choice_lock = move_id;
                }
            }
            None
        }
        (i::CHOICE_BAND, Event::ModifyAtk, On)
        | (i::CHOICE_SPECS, Event::ModifySpA, On)
        | (i::CHOICE_SCARF, Event::ModifySpe, On) => ev.chain_modify(3, 2),
        // `onModifyDamage`: 5324/4096; `onAfterMoveSecondarySelf`: 1/10 of
        // max HP of recoil after a damaging move that didn't target the
        // holder (not while it is being forced out).
        (i::LIFE_ORB, Event::ModifyDamage, On) => {
            ev.chain(5324);
            None
        }
        (i::LIFE_ORB, Event::AfterMoveSecondarySelf, On) => {
            // (After a future move, the "move" is the item: not a status.)
            let status = ev.move_data().is_some_and(|d| d.category == MoveCategory::Status);
            let m = mon(battle, holder)?;
            if ev.source != Some(holder) && !status && !m.force_switch_flag {
                let amount = hp_fraction(m.max_hp() as u32, 1, 10);
                damage_by(battle, holder, amount, Some(holder), Effect::Item(item));
            }
            None
        }
        // `onAfterMoveSecondarySelf` (priority -1): heals 1/8 of the damage
        // the move dealt (`move.totalDamage`), unless being forced out. The
        // heal's source is the event's: the move's target.
        (i::SHELL_BELL, Event::AfterMoveSecondarySelf, On) => {
            let (move_id, total) = battle.move_state.total_damage_of;
            if ev.effect.move_id() == Some(move_id) && total > 0 && !mon(battle, holder)?.force_switch_flag {
                heal_by(battle, holder, hp_fraction(total, 1, 8), ev.source, Effect::Item(item));
            }
            None
        }
        // `onModifyDamage`: 4915/4096 against a super-effective target
        // (`ModifyDamage` runs on the attacker; the target is its source).
        (i::EXPERT_BELT, Event::ModifyDamage, On) => {
            let target = ev.source?;
            if battle.move_state.hit_data(target).type_mod > 0 { chain(ev, 4915) } else { None }
        }
        // `onModifyDamage` of the Metronome item's volatile is in
        // `metronome_modify_damage`; `onStart` adds the volatile.
        (i::METRONOME, Event::Start, On) => {
            let flag = Volatiles::METRONOME;
            if events::add_volatile_flag(battle, holder, flag, "metronome", None, Effect::Item(item)) {
                // The condition's `onStart`.
                if let Some(m) = battle.state.at_mut(holder) {
                    m.volatile_counters.metronome_move = None;
                    m.volatile_counters.metronome_count = 0;
                }
            }
            None
        }
        // The type-boosting items and Plates (`onBasePower` priority 15):
        // 4915/4096 for a move of their type.
        (_, Event::BasePower, On) if boosted_type(battle, item).is_some() => {
            let ty = boosted_type(battle, item)?;
            if move_type(ev) == Some(ty) { chain(ev, 4915) } else { None }
        }
        // The legendary orbs and Soul Dew (`onBasePower` priority 15).
        (_, Event::BasePower, On) if legendary_boost(item).is_some() => {
            let (nums, types) = legendary_boost(item)?;
            let ty = move_type(ev)?;
            let num = base_species(battle, holder)?.num;
            if nums.contains(&num) && types.contains(&ty) { chain(ev, 4915) } else { None }
        }
        // Ogerpon's masks (`onBasePower` priority 15): any move of the
        // forme the mask belongs to.
        (_, Event::BasePower, On) if mask_forme(item).is_some() => {
            let forme = mask_forme(item)?;
            if base_species(battle, holder)?.name.starts_with(forme) { chain(ev, 4915) } else { None }
        }
        // `onTakeItem`: not from Ogerpon.
        (i::CORNERSTONE_MASK | i::HEARTHFLAME_MASK | i::WELLSPRING_MASK, Event::TakeItem, On) => {
            let ogerpon = base_species(battle, holder).is_some_and(|s| s.base_species == species::OGERPON);
            Some(Relay::from_bool(!ogerpon))
        }
        // `onTakeItem` of the Plates, the Origin items and the Rusted
        // weapons: not from (or by) their Pokémon.
        (_, Event::TakeItem, On) if locked_to(item).is_some() => {
            let num = locked_to(item)?;
            let taker = ev.source.and_then(|s| base_species(battle, s)).is_some_and(|s| s.num == num);
            let own = base_species(battle, holder).is_some_and(|s| s.num == num);
            Some(Relay::from_bool(!(taker || own)))
        }
        // Muscle Band and Wise Glasses (`onBasePower` priority 16):
        // 4505/4096 for a physical / special move.
        (i::MUSCLE_BAND | i::WISE_GLASSES, Event::BasePower, On) => {
            let wanted = if item == i::MUSCLE_BAND { MoveCategory::Physical } else { MoveCategory::Special };
            if ev.move_data()?.category == wanted { chain(ev, 4505) } else { None }
        }
        // Punching Glove: `onBasePower` (priority 23) 4506/4096 for a
        // punching move, and `onModifyMove` (priority 1): it no longer
        // makes contact.
        (i::PUNCHING_GLOVE, Event::BasePower, On) => {
            if ev.move_data()?.flags.contains(MoveFlags::PUNCH) {
                chain(ev, 4506)
            } else {
                None
            }
        }
        (i::PUNCHING_GLOVE, Event::ModifyMove, On) => {
            let data = ev.move_mut()?;
            if data.flags.contains(MoveFlags::PUNCH) {
                data.flags = MoveFlags(data.flags.0 & !MoveFlags::CONTACT.0);
            }
            None
        }
        // King's Rock and Razor Fang (`onModifyMove` priority -1): a
        // non-status move without a flinch secondary gets a 10% one. Sheer
        // Force's `onModifyMove` (priority 0) has deleted the move's own
        // secondaries by then.
        (i::KINGS_ROCK | i::RAZOR_FANG, Event::ModifyMove, On) => {
            let data = ev.move_data()?;
            let own = !battle.move_state.has_sheer_force
                && data.secondaries.iter().any(|s| s.volatile_status == Some("flinch"));
            if data.category != MoveCategory::Status && !own {
                battle.move_state.added_flinch = true;
            }
            None
        }
        // Loaded Dice (`onModifyMove`): `delete move.multiaccuracy`; its
        // hit counts are in `multihit_count`.
        (i::LOADED_DICE, Event::ModifyMove, On) => {
            battle.move_state.no_multiaccuracy = true;
            None
        }
        // Scope Lens and Razor Claw (`onModifyCritRatio`): +1.
        (i::SCOPE_LENS | i::RAZOR_CLAW, Event::ModifyCritRatio, On) => Some(Relay::Num(ev.value() + 1)),
        // Leek (`data/items.ts` leek; Champions only): +2 for a Pokémon
        // whose `baseSpecies.baseSpecies` is Farfetch'd or Sirfetch'd.
        (i::LEEK, Event::ModifyCritRatio, On) => {
            let base = base_species(battle, holder)?.base_species;
            if matches!(base, species::FARFETCHD | species::SIRFETCHD) {
                Some(Relay::Num(ev.value() + 2))
            } else {
                None
            }
        }
        // Bright Powder (`onModifyAccuracy` priority -2): 3686/4096 for a
        // numeric accuracy.
        (i::BRIGHT_POWDER, Event::ModifyAccuracy, On) => {
            if matches!(ev.relay, Relay::Num(_)) {
                chain(ev, 3686)
            } else {
                None
            }
        }
        // Wide Lens (`onSourceModifyAccuracy` priority -2): 4505/4096.
        (i::WIDE_LENS, Event::ModifyAccuracy, Source) => {
            if matches!(ev.relay, Relay::Num(_)) {
                chain(ev, 4505)
            } else {
                None
            }
        }
        // Zoom Lens (`onSourceModifyAccuracy` priority -2): 4915/4096 when
        // the target has no move left to use this turn (`queue.willMove`).
        (i::ZOOM_LENS, Event::ModifyAccuracy, Source) => {
            let target = ev.target_pos()?;
            if matches!(ev.relay, Relay::Num(_)) && !queue::will_move(battle, target) { chain(ev, 4915) } else { None }
        }
        // Quick Claw (`onFractionalPriority` priority -2): a 1 in 5 chance
        // (drawn only at priority 0 or below) to move first in its bracket,
        // except a status move under Mycelium Might. The relay is in
        // tenths.
        (i::QUICK_CLAW, Event::FractionalPriority, On) => {
            let status = ev.move_data().is_some_and(|d| d.category == MoveCategory::Status);
            if status
                && crate::resolve::abilities::has_ability(battle, holder, pikapro_data::gen9::abilities::MYCELIUM_MIGHT)
            {
                return None;
            }
            if ev.value() <= 0 && battle.rng.chance(1, 5) {
                let actor = actor(battle, holder);
                let effect = EffectName::Item(item);
                battle.emit(BattleEvent::ActivateEffect { actor, effect, args: [None, None], of: None });
                return Some(Relay::Num(1));
            }
            None
        }
        // Lagging Tail (`onFractionalPriority: -0.1`).
        (i::LAGGING_TAIL, Event::FractionalPriority, On) => Some(Relay::Num(-1)),
        // Focus Band (`onDamage` priority -40): a 1 in 10 chance, drawn for
        // any damage, to survive a move's hit at 1 HP.
        (i::FOCUS_BAND, Event::Damage, On) => {
            let hp = mon(battle, holder)?.hp as i32;
            if battle.rng.chance(1, 10) && ev.value() >= hp && ev.effect.is_move() {
                let actor = actor(battle, holder);
                let effect = EffectName::Item(item);
                battle.emit(BattleEvent::ActivateEffect { actor, effect, args: [None, None], of: None });
                return Some(Relay::Num(hp - 1));
            }
            None
        }
        // Big Root (`onTryHeal` priority 1): 5324/4096 for draining, Leech
        // Seed, Ingrain, Aqua Ring and Strength Sap.
        (i::BIG_ROOT, Event::TryHeal, On) => {
            let boosted = match ev.effect {
                Effect::Condition(Condition::Drain) => true,
                Effect::Volatile(bit) => {
                    let flag = 1u128 << bit;
                    flag == Volatiles::LEECH_SEED || flag == Volatiles::INGRAIN || flag == Volatiles::AQUA_RING
                }
                Effect::Move(m) => m == moves::STRENGTH_SAP,
                _ => false,
            };
            if boosted { chain(ev, 5324) } else { None }
        }
        // Light Ball (`onModifyAtk` / `onModifySpA` priority 1): 2x for a
        // Pikachu.
        (i::LIGHT_BALL, Event::ModifyAtk | Event::ModifySpA, On) => {
            let pikachu = base_species(battle, holder)?.base_species == species::PIKACHU;
            if pikachu { ev.chain_modify(2, 1) } else { None }
        }
        // Eviolite (`onModifyDef` / `onModifySpD` priority 2): 1.5x for a
        // Pokémon that can still evolve (`baseSpecies.nfe`).
        (i::EVIOLITE, Event::ModifyDef | Event::ModifySpD, On) => {
            if base_species(battle, holder)?.nfe {
                ev.chain_modify(3, 2)
            } else {
                None
            }
        }
        // Assault Vest: `onModifySpD` (priority 1) 1.5x; `onDisableMove`:
        // every status move but Me First.
        (i::ASSAULT_VEST, Event::ModifySpD, On) => ev.chain_modify(3, 2),
        (i::ASSAULT_VEST, Event::DisableMove, On) => {
            let dex = battle.format.dex;
            let status: crate::util::StackVec<MoveId, 4> = mon(battle, holder)?
                .moves
                .iter()
                .map(|s| s.id)
                .filter(|&m| dex.move_(m).category == MoveCategory::Status && m != moves::ME_FIRST)
                .collect();
            for &m in status.iter() {
                events::disable_move(battle, holder, m);
            }
            None
        }
        // Iron Ball and the Power items (`onModifySpe`): 0.5x.
        (
            i::IRON_BALL
            | i::POWER_ANKLET
            | i::POWER_BAND
            | i::POWER_BELT
            | i::POWER_BRACER
            | i::POWER_LENS
            | i::POWER_WEIGHT,
            Event::ModifySpe,
            On,
        ) => ev.chain_modify(1, 2),
        // Iron Ball (`onEffectiveness`): a grounded Flying type takes Ground
        // moves neutrally (0 for each of its types), unless Ingrain, Smack
        // Down or Gravity already ground it.
        (i::IRON_BALL, Event::Effectiveness, On) => {
            let target = ev.target_pos()?;
            let m = mon(battle, target)?;
            if m.volatiles.has(Volatiles::INGRAIN | Volatiles::SMACK_DOWN) || crate::resolve::gravity(battle) {
                return None;
            }
            if move_type(ev) == Some(Type::Ground) && has_type(m, Type::Flying) { Some(Relay::Num(0)) } else { None }
        }
        // Rocky Helmet (`onDamagingHit` order 2): a contact move's user
        // loses 1/6 of its max HP.
        (i::ROCKY_HELMET, Event::DamagingHit, On) => {
            let source = ev.source?;
            let data = *ev.move_data()?;
            if crate::resolve::abilities::makes_contact(battle, &data, source, holder, None) {
                let amount = hp_fraction(mon(battle, source)?.max_hp() as u32, 1, 6);
                damage_by(battle, source, amount, Some(holder), Effect::Item(item));
            }
            None
        }
        // Shed Shell (`onTrapPokemon` priority -10): nothing traps the
        // holder; `onMaybeTrapPokemon` clears `maybeTrapped`, which the
        // engine does not track.
        (i::SHED_SHELL, Event::TrapPokemon, On) => {
            events::release_trap(battle, holder);
            None
        }
        // Destiny Knot (`onAttract` priority -100): whoever attracted the
        // holder is attracted back.
        (i::DESTINY_KNOT, Event::Attract, On) => {
            let (target, source) = (ev.target_pos()?, ev.source?);
            if source != target && !mon(battle, source)?.volatiles.has(Volatiles::ATTRACT) {
                crate::resolve::conditions::attract_from(battle, source, target, EffectName::Item(item));
            }
            None
        }
        // Ring Target (`onNegateImmunity: false`): its types' immunities are
        // negated.
        (i::RING_TARGET, Event::NegateImmunity, On) => Some(Relay::False),
        // Float Stone (`onModifyWeight`): half the weight, truncated.
        (i::FLOAT_STONE, Event::ModifyWeight, On) => Some(Relay::Num(ev.value() / 2)),
        // Covert Cloak (`onModifySecondaries`): only the secondaries with a
        // `self` effect are kept (the mask's bits in roll order,
        // `secondaries::with_self`).
        (i::COVERT_CLOAK, Event::ModifySecondaries, On) => {
            let Relay::Secondaries(kept) = ev.relay else { return None };
            let data = ev.move_data()?;
            Some(Relay::Secondaries(kept & crate::resolve::secondaries::with_self(battle, data)))
        }
        // Clear Amulet (`onTryBoost` priority 1): drops by others are
        // removed; announced unless a secondary effect or Octolock caused
        // them.
        (i::CLEAR_AMULET, Event::TryBoost, On) => {
            let target = ev.target_pos()?;
            if ev.source == Some(target) {
                return None;
            }
            let Relay::Boosts(mut boosts) = ev.relay else { return None };
            let mut blocked = false;
            for id in pikapro_data::BoostId::ALL {
                if boosts.get(id).is_some_and(|v| v < 0) {
                    boosts.delete(id);
                    blocked = true;
                }
            }
            ev.relay = Relay::Boosts(boosts);
            if blocked
                && !events::effect_has_secondaries(battle, ev.effect)
                && ev.effect != Effect::Move(moves::OCTOLOCK)
            {
                let t = actor(battle, target);
                let cause = Cause::item(item).of(t);
                battle.emit(BattleEvent::FailFrom { target: t, what: LineArg::Text("unboost"), detail: None, cause });
            }
            None
        }
        // Safety Goggles: `onImmunity` to sandstorm, hail and powder;
        // `onTryHit`: a powder move from another Pokémon is blocked (`null`)
        // unless the holder is immune to powder anyway (Grass).
        (i::SAFETY_GOGGLES, Event::Immunity, On) => {
            matches!(ev.relay, Relay::Id("sandstorm" | "hail" | "powder")).then_some(Relay::False)
        }
        (i::SAFETY_GOGGLES, Event::TryHit, On) => {
            let target = ev.target_pos()?;
            let move_id = ev.effect.move_id()?;
            let powder = ev.move_data()?.flags.contains(MoveFlags::POWDER);
            if powder && ev.source != Some(target) && !has_type(mon(battle, target)?, Type::Grass) {
                let actor = actor(battle, target);
                let effect = EffectName::Item(item);
                let args = [Some(LineArg::Move(move_id)), None];
                battle.emit(BattleEvent::ActivateEffect { actor, effect, args, of: None });
                return Some(Relay::Null);
            }
            None
        }
        // Ability Shield (`onSetAbility`): the ability can't be changed;
        // an ability that tried (not Trace) is shown first.
        (i::ABILITY_SHIELD, Event::SetAbility, On) => {
            if let (Effect::Ability(a), Some(source)) = (ev.effect, ev.source)
                && a != pikapro_data::gen9::abilities::TRACE
            {
                crate::resolve::abilities::show(battle, source, a);
            }
            let actor = actor(battle, holder);
            battle.emit(BattleEvent::Block { actor, effect: EffectName::Item(item), arg: None, of: None });
            Some(Relay::Null)
        }
        // Utility Umbrella: its holder's weather changes when it starts
        // while ignored, when it ends, and on the `Update` after an end.
        (i::UTILITY_UMBRELLA, Event::Start, On) => {
            if super::ignoring_item(battle, holder) {
                umbrella_weather_change(battle, holder);
            }
            None
        }
        (i::UTILITY_UMBRELLA, Event::Update, On) => {
            let m = battle.state.side_mut(holder.side).team.get_mut(holder.slot as usize)?;
            if umbrella_update_idle(m) {
                return None;
            }
            m.volatile_counters.item_state.umbrella_inactive = false;
            umbrella_weather_change(battle, holder);
            None
        }
        (i::UTILITY_UMBRELLA, Event::End, On) => {
            umbrella_weather_change(battle, holder);
            // The state is replaced when the item is gone.
            let m = battle.state.side_mut(holder.side).team.get_mut(holder.slot as usize)?;
            if m.item == Some(item) {
                m.volatile_counters.item_state.umbrella_inactive = true;
            }
            None
        }
        _ => None,
    }
}

/// Utility Umbrella's `runEvent('WeatherChange', pokemon, pokemon, item)`
/// when the field's weather is sun or rain.
fn umbrella_weather_change(battle: &mut Battle, holder: Position) {
    let weather = crate::resolve::abilities::effective_weather(battle);
    if matches!(weather, Some(Weather::Sun | Weather::Rain | Weather::HarshSun | Weather::HeavyRain)) {
        let effect = Effect::Item(i::UTILITY_UMBRELLA);
        let mut ev = EventCtx::new(Event::WeatherChange, Target::Pokemon(holder), Some(holder), effect, Relay::True);
        events::run_event(battle, &mut ev, None);
    }
}

/// This module's volatiles' handlers (`choicelock`, bits 108..=111).
pub(crate) fn volatile_spec(flag: u128, event: Event, prefix: EventPrefix) -> Option<Spec> {
    match (flag, event, prefix) {
        (Volatiles::CHOICE_LOCK, Event::BeforeMove, EventPrefix::On) => Some(Spec::relay(choice_lock_before_move)),
        (Volatiles::CHOICE_LOCK, Event::DisableMove, EventPrefix::On) => Some(Spec::relay(choice_lock_disable_move)),
        (Volatiles::METRONOME, Event::TryMove, EventPrefix::On) => Some(Spec::relay(metronome_try_move).priority(-2)),
        (Volatiles::METRONOME, Event::ModifyDamage, EventPrefix::On) => Some(Spec::relay(metronome_modify_damage)),
        _ => None,
    }
}

/// The events [`volatile_spec`] handles for `flag` (any prefix). Keep it in
/// step with `volatile_spec` (a test checks them).
pub(crate) const fn volatile_events(flag: u128) -> u128 {
    match flag {
        Volatiles::CHOICE_LOCK => const { pikapro_data::EventSet::of(&[Event::BeforeMove, Event::DisableMove]).0 },
        Volatiles::METRONOME => const { pikapro_data::EventSet::of(&[Event::TryMove, Event::ModifyDamage]).0 },
        _ => 0,
    }
}

/// Whether the Pokémon at `pos` holds a Choice item (`getItem().isChoice`,
/// suppressed or not).
fn holds_choice(battle: &Battle, pos: Position) -> bool {
    battle.state.side(pos.side).team[pos.slot as usize].item.is_some_and(|it| battle.format.dex.item(it).is_choice)
}

/// `choicelock`'s `onBeforeMove`: without a Choice item the lock ends;
/// otherwise another move than the locked one (not Struggle) fails with
/// `|move|` and `-fail`, unless the item is suppressed.
fn choice_lock_before_move(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let pos = holder_of(h)?;
    if !holds_choice(battle, pos) {
        events::remove_volatile_flag(battle, pos, Volatiles::CHOICE_LOCK);
        return None;
    }
    let move_id = ev.effect.move_id()?;
    let locked = battle.state.at(pos)?.volatile_counters.choice_lock;
    if !super::ignoring_item(battle, pos) && move_id != locked && move_id != moves::STRUGGLE {
        let user = actor(battle, pos);
        battle.emit(BattleEvent::Move { user, move_id, target: None, from: None });
        crate::resolve::moves::fail_still(battle, pos);
        return Some(Relay::False);
    }
    None
}

/// `choicelock`'s `onDisableMove`: without a Choice item or the locked
/// move the lock ends; otherwise every other move is disabled (unless the
/// item is suppressed).
fn choice_lock_disable_move(battle: &mut Battle, _: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let pos = holder_of(h)?;
    let locked = battle.state.at(pos)?.volatile_counters.choice_lock;
    let has_move = battle.state.at(pos)?.moves.iter().any(|s| s.id == locked);
    if !holds_choice(battle, pos) || !has_move {
        events::remove_volatile_flag(battle, pos, Volatiles::CHOICE_LOCK);
        return None;
    }
    if super::ignoring_item(battle, pos) {
        return None;
    }
    let others: crate::util::StackVec<pikapro_data::MoveId, 4> =
        battle.state.at(pos)?.moves.iter().map(|s| s.id).filter(|&m| m != locked).collect();
    for &m in others.iter() {
        events::disable_move(battle, pos, m);
    }
    None
}

/// The Metronome item's volatile's `onTryMove` (priority -2): the volatile
/// ends without the item; a move that calls another is not counted;
/// otherwise the same move again after a successful one (or on the second
/// turn of a two-turn move) counts one more consecutive use.
fn metronome_try_move(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let pos = holder_of(h)?;
    if !has_item(battle, pos, i::METRONOME) {
        events::remove_volatile_flag(battle, pos, Volatiles::METRONOME);
        return None;
    }
    let move_id = ev.effect.move_id()?;
    let dex = battle.format.dex;
    if (move_id.0 as usize) < dex.moves.len() && dex.move_(move_id).unmodelled.contains(&"callsMove") {
        return None;
    }
    let m = battle.state.at_mut(pos)?;
    let c = &mut m.volatile_counters;
    let same = c.metronome_move == Some(move_id);
    if same && c.move_last_turn == MoveResult::Succeeded {
        c.metronome_count = c.metronome_count.saturating_add(1);
    } else if m.volatiles.has(Volatiles::TWO_TURN_MOVE) {
        c.metronome_count = if same { c.metronome_count.saturating_add(1) } else { 1 };
    } else {
        c.metronome_count = 0;
    }
    c.metronome_move = Some(move_id);
    None
}

/// The Metronome item's volatile's `onModifyDamage`: 1 + 0.2 per
/// consecutive use, up to 2x.
fn metronome_modify_damage(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    const DAMAGE_MOD: [u32; 6] = [4096, 4915, 5734, 6553, 7372, 8192];
    let pos = holder_of(h)?;
    let count = mon(battle, pos)?.volatile_counters.metronome_count.min(5);
    ev.chain(DAMAGE_MOD[count as usize]);
    None
}

/// Zacian's and Zamazenta's species conditions' `onBattleStart`: holding
/// Rusted Sword / Rusted Shield (by ID, even if ignored), they become their
/// Crowned forme for the whole battle (`setSpecies`, `baseSpecies`, the
/// forme's ability as `baseAbility`, silently) and Iron Head becomes
/// Behemoth Blade / Behemoth Bash with full PP.
pub(crate) fn battle_start(battle: &mut Battle) {
    use crate::state::SideId;
    for side in SideId::BOTH {
        for index in 0..battle.state.side(side).team.len() {
            let m = &battle.state.side(side).team[index];
            let (crowned, blade) = match (m.species, m.item) {
                (species::ZACIAN, Some(i::RUSTED_SWORD)) => (species::ZACIAN_CROWNED, moves::BEHEMOTH_BLADE),
                (species::ZAMAZENTA, Some(i::RUSTED_SHIELD)) => (species::ZAMAZENTA_CROWNED, moves::BEHEMOTH_BASH),
                _ => continue,
            };
            let pos = Position::new(side, index as u8);
            crate::resolve::forme::set_species(battle, pos, crowned);
            let dex = battle.format.dex;
            let data = dex.species(crowned);
            let pp = crate::battle::boosted_pp(battle.format.mechanics, dex.move_(blade).pp);
            let m = &mut battle.state.side_mut(side).team[index];
            m.base_species = crowned;
            m.base_types = data.types;
            // `setSpecies`: `apparentType` is the crowned forme's types.
            m.species_types = data.types;
            m.volatile_counters.apparent_types = None;
            m.volatile_counters.types = None;
            crate::resolve::manipulation::refresh_types(m);
            m.set_stats = m.stats;
            m.ability = data.abilities.primary;
            m.base_ability = m.ability;
            if let Some(slot) = m.moves.iter_mut().find(|s| s.id == moves::IRON_HEAD) {
                *slot = crate::state::MoveSlot { id: blade, pp, max_pp: pp, disabled: false, target: None };
            }
            let ability = m.ability;
            events::note_held(battle, Some(ability), None);
        }
    }
}

// ---------------------------------------------------------------------------
// Items checked at their call sites.

/// A weather's `durationCallback` (`source` set it): 5 turns, 8 with Heat
/// Rock (sun), Damp Rock (rain), Smooth Rock (sand) or Icy Rock (snow).
pub(crate) fn weather_duration(battle: &Battle, source: Option<Position>, weather: Weather) -> u8 {
    let rock = match weather {
        Weather::Sun => i::HEAT_ROCK,
        Weather::Rain => i::DAMP_ROCK,
        Weather::Sand => i::SMOOTH_ROCK,
        Weather::Snow => i::ICY_ROCK,
        _ => return 5,
    };
    if source.is_some_and(|s| has_item(battle, s, rock)) { 8 } else { 5 }
}

/// A terrain's `durationCallback`: 5 turns, 8 with Terrain Extender.
pub(crate) fn terrain_duration(battle: &Battle, source: Position) -> u8 {
    if has_item(battle, source, i::TERRAIN_EXTENDER) { 8 } else { 5 }
}

/// Reflect's, Light Screen's and Aurora Veil's `durationCallback`: 5
/// turns, 8 with Light Clay.
pub(crate) fn screen_duration(battle: &Battle, source: Position) -> u8 {
    if has_item(battle, source, i::LIGHT_CLAY) { 8 } else { 5 }
}

/// Loaded Dice in `hitStepMoveHitLoop`: `hits` was drawn for a 2-5 hit move
/// (below 4 it becomes `5 - random(2)`) or is Population Bomb's 10 (less
/// `random(7)`). Returns the number of hits.
pub(crate) fn multihit_count(battle: &mut Battle, user: Position, hits: u8) -> u8 {
    if !has_item(battle, user, i::LOADED_DICE) {
        return hits;
    }
    if hits == 10 {
        hits - battle.rng.random(7) as u8
    } else if hits < 4 {
        5 - battle.rng.random(2) as u8
    } else {
        hits
    }
}

/// `partiallytrapped`'s `durationCallback`: 8 turns with Grip Claw (held by
/// `source`); `None` for the usual `random(5, 7)`.
pub(crate) fn binding_duration(battle: &Battle, source: Position) -> Option<u8> {
    has_item(battle, source, i::GRIP_CLAW).then_some(8)
}

/// `partiallytrapped`'s `boundDivisor`: 6 with Binding Band (held by
/// `source`), 8 otherwise.
pub(crate) fn binding_divisor(battle: &Battle, source: Position) -> u32 {
    if has_item(battle, source, i::BINDING_BAND) { 6 } else { 8 }
}

/// Heavy-Duty Boots: the Pokémon at `pos` ignores entry hazards (Toxic
/// Spikes are still absorbed by a Poison type).
pub(crate) fn hazard_immune(battle: &Battle, pos: Position) -> bool {
    has_item(battle, pos, i::HEAVY_DUTY_BOOTS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::events::run_modify;
    use crate::state::SideId;
    use crate::{BattleOptions, Format, PokemonSet, Seed, Team};
    use pikapro_data::gen9::{abilities as a, moves as m, species as s};
    use pikapro_data::{SpeciesId, Stat};

    const P1: Position = Position::new(SideId::P1, 0);
    const P2: Position = Position::new(SideId::P2, 0);

    fn set(species: SpeciesId, item: Option<ItemId>) -> PokemonSet {
        let s = PokemonSet::new(species).with_level(50).with_ability(a::NO_ABILITY).with_moves(&[m::TACKLE]);
        match item {
            Some(i) => s.with_item(i),
            None => s,
        }
    }

    fn singles(p1: PokemonSet, p2: PokemonSet) -> Battle {
        let format = Format::gen9_singles();
        let team = |set: PokemonSet| Team::new(vec![set]);
        Battle::new(format, [team(p1), team(p2)], BattleOptions::new(Seed::fast(1))).unwrap()
    }

    fn base_power(b: &mut Battle, move_id: MoveId, bp: u32) -> u32 {
        let data = *b.format.dex.move_(move_id);
        run_modify(b, Event::BasePower, P1, Some(P2), Effect::Move(move_id), bp, Some(&data), None)
    }

    /// The type-boosting items and Plates multiply their type's moves'
    /// base power by 4915/4096; Muscle Band physical moves by 4505/4096.
    #[test]
    fn type_boosters_and_plates() {
        let mut b = singles(set(s::CHARIZARD, Some(i::CHARCOAL)), set(s::BLISSEY, None));
        assert_eq!(base_power(&mut b, m::FLAMETHROWER, 90), crate::stats::modify(90, 4915));
        assert_eq!(base_power(&mut b, m::SURF, 90), 90);
        let mut b = singles(set(s::CHARIZARD, Some(i::SPLASH_PLATE)), set(s::BLISSEY, None));
        assert_eq!(base_power(&mut b, m::SURF, 90), crate::stats::modify(90, 4915));
        let mut b = singles(set(s::CHARIZARD, Some(i::MUSCLE_BAND)), set(s::BLISSEY, None));
        assert_eq!(base_power(&mut b, m::TACKLE, 40), crate::stats::modify(40, 4505));
        assert_eq!(base_power(&mut b, m::FLAMETHROWER, 90), 90);
    }

    /// Eviolite boosts the defences of a Pokémon that can still evolve.
    #[test]
    fn eviolite_needs_an_evolution() {
        let mut b = singles(set(s::CHANSEY, Some(i::EVIOLITE)), set(s::BLISSEY, Some(i::EVIOLITE)));
        let def = |b: &mut Battle, p: Position| {
            let raw = b.state.at(p).unwrap().stats[Stat::Def] as u32;
            (raw, events::get_stat(b, p, Stat::Def, false, false))
        };
        let (raw, chansey) = def(&mut b, P1);
        assert_eq!(chansey, crate::stats::modify(raw, 6144));
        let (raw, blissey) = def(&mut b, P2);
        assert_eq!(blissey, raw);
    }

    /// The weather rocks, Light Clay and Terrain Extender make their
    /// effects last 8 turns.
    #[test]
    fn duration_items() {
        let b = singles(set(s::TORKOAL, Some(i::HEAT_ROCK)), set(s::PELIPPER, Some(i::LIGHT_CLAY)));
        assert_eq!(weather_duration(&b, Some(P1), Weather::Sun), 8);
        assert_eq!(weather_duration(&b, Some(P1), Weather::Rain), 5);
        assert_eq!(weather_duration(&b, Some(P2), Weather::Rain), 5);
        assert_eq!(screen_duration(&b, P2), 8);
        assert_eq!(screen_duration(&b, P1), 5);
        assert_eq!(terrain_duration(&b, P1), 5);
    }

    /// Zacian holding Rusted Sword is crowned at the start of the battle:
    /// its species, ability and Iron Head change.
    #[test]
    fn rusted_sword_crowns_zacian() {
        let zacian = set(s::ZACIAN, Some(i::RUSTED_SWORD)).with_moves(&[m::IRON_HEAD, m::TACKLE]);
        let b = singles(zacian, set(s::ZAMAZENTA, Some(i::RUSTED_SWORD)));
        let z = b.state.at(P1).unwrap();
        assert_eq!((z.species, z.base_species), (s::ZACIAN_CROWNED, s::ZACIAN_CROWNED));
        assert_eq!(z.ability, a::INTREPID_SWORD);
        assert_eq!(z.moves[0].id, m::BEHEMOTH_BLADE);
        assert_eq!(z.moves[0].max_pp, 8);
        assert_eq!(b.state.at(P2).unwrap().species, s::ZAMAZENTA);
    }

    /// Loaded Dice: 2-5 hit moves hit 4 or 5 times, Population Bomb 4 to
    /// 10 times.
    #[test]
    fn loaded_dice_counts() {
        let mut b = singles(set(s::MAUSHOLD, Some(i::LOADED_DICE)), set(s::BLISSEY, None));
        for _ in 0..50 {
            assert!((4..=5).contains(&multihit_count(&mut b, P1, 2)));
            assert_eq!(multihit_count(&mut b, P1, 5), 5);
            assert!((4..=10).contains(&multihit_count(&mut b, P1, 10)));
        }
        assert_eq!(multihit_count(&mut b, P2, 2), 2);
    }
}
