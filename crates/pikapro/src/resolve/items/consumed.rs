//! Berries, consumed items, and the abilities about items.
//!
//! Showdown references: the items in `data/items.ts` that `items::owner`
//! assigns to [`Owner::Consumed`](super::Owner::Consumed) (the berries,
//! Focus Sash, Weakness Policy, the Herbs, Eject Button, Booster Energy,
//! the seeds, ...), and the abilities `abilities::owner` assigns to
//! [`abilities::Owner::Consumed`] (Cheek Pouch, Cud Chew, Gluttony,
//! Harvest, Magician, Pickpocket, Sticky Hold, Unburden, ...). The item-moving moves (Knock Off, Trick, Bug Bite, ...)
//! live in `resolve/itemmoves.rs`.
//!
//! Adding an item: see `items/mod.rs`; an ability: `abilities/mod.rs`. One
//! arm per callback in [`run`] / [`run_ability`]; this module's
//! volatiles (bits 112..=123) get their handlers in
//! [`volatile_spec`]. The berries are in `consumed/berries.rs`.

mod berries;

pub(crate) use berries::has_on_eat;

use pikapro_data::gen9::{abilities as a, items as i, moves};
use pikapro_data::{AbilityId, BoostId, Event, EventPrefix, ItemId, MoveCategory, MoveFlags, StatBoosts};

use super::holder_of;
use crate::battle::Battle;
use crate::event::{BattleEvent, Cause, EffectName, ItemEndNote, LineArg};
use crate::resolve::abilities;
use crate::resolve::events::{self, Boosts, Effect, EventCtx, Relay, Spec, Target};
use crate::resolve::handlers::Handler;
use crate::resolve::{actor, damage_by, heal_by};
use crate::state::{Position, SwitchFlag, Terrain, Volatiles, Weather};

/// Runs `item`'s callback for `ev.event` / `ev.prefix` on its holder.
pub(super) fn run(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position, item: ItemId) -> Option<Relay> {
    use EventPrefix::{Any, Foe, On, Source};
    if battle.format.dex.item(item).is_berry {
        return berries::run(battle, ev, holder, item);
    }
    match (item, ev.event, ev.prefix) {
        // `onDamage` (priority -40): a move's hit that would knock out the
        // holder at full HP leaves it at 1 HP, using the sash.
        (i::FOCUS_SASH, Event::Damage, On) => {
            let damage = ev.value();
            let mon = battle.state.at(holder)?;
            let (hp, max_hp) = (mon.hp as i32, mon.max_hp() as i32);
            if hp == max_hp
                && damage >= hp
                && ev.effect.is_move()
                && super::use_item(battle, holder, None, Effect::Item(item))
            {
                return Some(Relay::Num(hp - 1));
            }
            None
        }
        // Mental Herb: `onUpdate` cures the first of these volatiles it
        // finds, and with it all the others.
        (i::MENTAL_HERB, Event::Update, On) => {
            let first = mental_herb_first(battle, holder)?;
            if super::use_item(battle, holder, None, Effect::Item(item)) {
                mental_herb_cure(battle, holder, first);
            }
            None
        }
        // White Herb: `onStart` (also on any switch-in, Mega Evolution, move and
        // at the end of the turn) uses it when a stat is lowered; `onUse`
        // resets the lowered stats. Champions inherits the base item.
        (i::WHITE_HERB, Event::Start, On)
        | (i::WHITE_HERB, Event::SwitchIn | Event::AfterMove | Event::AfterMega, Any)
        | (i::WHITE_HERB, Event::Residual, On) => {
            white_herb_start(battle, holder, item);
            None
        }
        (i::WHITE_HERB, Event::Use, On) => {
            let bits =
                battle.state.side(holder.side).team[holder.slot as usize].volatile_counters.item_state.white_herb;
            let mon = &mut battle.state.side_mut(holder.side).team[holder.slot as usize];
            for id in BoostId::ALL {
                if bits & (1 << id as u8) != 0 {
                    *crate::resolve::boosts::stage_mut(&mut mon.boosts, id) = 0;
                }
            }
            let a = actor(battle, holder);
            battle.emit(BattleEvent::ClearNegativeBoost { target: a, silent: true });
            None
        }
        // Air Balloon: announced on switching in; popped by a hit (on its
        // substitute too), which is not using it.
        (i::AIR_BALLOON, Event::Start, On) => {
            if !super::ignoring_item(battle, holder) && !crate::resolve::gravity(battle) {
                let a = actor(battle, holder);
                battle.emit(BattleEvent::ItemShow { actor: a, item, cause: None });
            }
            None
        }
        (i::AIR_BALLOON, Event::DamagingHit, On) => {
            pop_balloon(battle, holder, item);
            None
        }
        (i::AIR_BALLOON, Event::AfterSubDamage, On) => {
            if ev.effect.is_move() {
                pop_balloon(battle, holder, item);
            }
            None
        }
        // The items used when hit by a move of their type (their `boosts`).
        (i::ABSORB_BULB | i::CELL_BATTERY | i::LUMINOUS_MOSS | i::SNOWBALL, Event::DamagingHit, On) => {
            let ty = match item {
                i::CELL_BATTERY => pikapro_data::Type::Electric,
                i::SNOWBALL => pikapro_data::Type::Ice,
                _ => pikapro_data::Type::Water,
            };
            if ev.move_data()?.ty == ty {
                super::use_item(battle, holder, None, Effect::Item(item));
            }
            None
        }
        // Weakness Policy: a super-effective hit that is not fixed damage.
        (i::WEAKNESS_POLICY, Event::DamagingHit, On) => {
            let data = ev.move_data()?;
            let fixed = data.unmodelled.contains(&"damage") || data.unmodelled.contains(&"damageCallback");
            if !fixed && battle.move_state.hit_data(holder).type_mod > 0 {
                super::use_item(battle, holder, None, Effect::Item(item));
            }
            None
        }
        // Adrenaline Orb: `onAfterBoost` from Intimidate (if Attack was named
        // and Speed can rise).
        (i::ADRENALINE_ORB, Event::AfterBoost, On) => {
            let Relay::Boosts(b) = ev.relay else { return None };
            if battle.state.at(holder)?.boosts.spe == 6 || b.get(BoostId::Atk) == Some(0) {
                return None;
            }
            if ev.effect == Effect::Ability(a::INTIMIDATE) {
                super::use_item(battle, holder, None, Effect::Item(item));
            }
            None
        }
        // Throat Spray: after its holder's sound move.
        (i::THROAT_SPRAY, Event::AfterMoveSecondarySelf, On) => {
            if ev.move_data()?.flags.contains(MoveFlags::SOUND) {
                super::use_item(battle, holder, None, Effect::Item(item));
            }
            None
        }
        // Eject Button: `onAfterMoveSecondary` (priority 2) after a foe's
        // damaging move, its holder switches out (and the attacker's own
        // switch is cancelled, except in Champions).
        (i::EJECT_BUTTON, Event::AfterMoveSecondary, On) => {
            let source = ev.source?;
            let data = ev.move_data()?;
            let hp = battle.state.at(holder)?.hp;
            if source == holder
                || hp == 0
                || data.category == MoveCategory::Status
                || data.flags.contains(MoveFlags::FUTURE_MOVE)
            {
                return None;
            }
            // (`isSkyDropped()`: Sky Drop is Past in Gen 9.)
            let mon = battle.state.at(holder)?;
            if crate::resolve::switching::can_switch(battle, holder.side) == 0
                || mon.force_switch_flag
                || mon.being_called_back
                || mon.volatiles.has(Volatiles::COMMANDING | Volatiles::COMMANDED)
            {
                return None;
            }
            if abilities::all_active(battle)
                .any(|p| battle.state.side(p.side).team[p.slot as usize].switch_flag.is_true())
            {
                return None;
            }
            set_switch_flag(battle, holder, SwitchFlag::True);
            if super::use_item(battle, holder, None, Effect::Item(item)) {
                // Champions' override (`data/mods/champions/items.ts`) has
                // no `source.switchFlag = false`: a U-turn user still
                // switches out.
                if battle.format.mechanics != crate::format::Mechanics::Champions {
                    set_switch_flag(battle, source, SwitchFlag::None);
                }
            } else {
                set_switch_flag(battle, holder, SwitchFlag::None);
            }
            None
        }
        // Eject Pack: a lowered stat readies it; it is used after the
        // switch-ins, the move or the end of the turn, if its holder can
        // switch out.
        (i::EJECT_PACK, Event::AfterBoost, On) => {
            let Relay::Boosts(b) = ev.relay else { return None };
            let parting_shot = battle.active_move.is_some_and(|(_, m)| m == moves::PARTING_SHOT);
            if item_state(battle, holder).eject || parting_shot {
                return None;
            }
            if b.iter().any(|(_, n)| n < 0) {
                battle.state.side_mut(holder.side).team[holder.slot as usize].volatile_counters.item_state.eject = true;
            }
            None
        }
        (i::EJECT_PACK, Event::SwitchIn | Event::AfterMove | Event::AfterMega, Any)
        | (i::EJECT_PACK, Event::Residual, On) => {
            if item_state(battle, holder).eject {
                super::use_item(battle, holder, None, Effect::Item(item));
                // `delete this.effectState.eject`: a
                // held-back eject (`onUseItem` refused) doesn't fire again.
                // After a use the item's state is gone anyway.
                battle.state.side_mut(holder.side).team[holder.slot as usize].volatile_counters.item_state.eject =
                    false;
            }
            None
        }
        (i::EJECT_PACK, Event::UseItem, On) => {
            let mon = battle.state.at(holder)?;
            if crate::resolve::switching::can_switch(battle, holder.side) == 0
                || mon.volatiles.has(Volatiles::COMMANDING)
                || mon.volatiles.has(Volatiles::COMMANDED)
                || abilities::all_active(battle)
                    .any(|p| battle.state.side(p.side).team[p.slot as usize].switch_flag.is_true())
            {
                return Some(Relay::False);
            }
            Some(Relay::True)
        }
        (i::EJECT_PACK, Event::Use, On) => {
            set_switch_flag(battle, holder, SwitchFlag::True);
            None
        }
        // Red Card: `onAfterMoveSecondary` drags out the attacker.
        (i::RED_CARD, Event::AfterMoveSecondary, On) => {
            let source = ev.source?;
            let data = ev.move_data()?;
            let (source_hp, hp) = (battle.state.at(source).map_or(0, |m| m.hp), battle.state.at(holder)?.hp);
            if source == holder || source_hp == 0 || hp == 0 || data.category == MoveCategory::Status {
                return None;
            }
            let source_flag = battle.state.at(source).is_some_and(|m| m.force_switch_flag);
            if !events::is_active(battle, source)
                || crate::resolve::switching::can_switch(battle, source.side) == 0
                || source_flag
                || battle.state.at(holder)?.force_switch_flag
            {
                return None;
            }
            if super::use_item(battle, holder, Some(source), Effect::Item(item)) {
                let move_id = ev.effect.move_id()?;
                let relay =
                    events::move_event(battle, Event::DragOut, source, Some(holder), move_id, data, Relay::True);
                if relay.is_truthy()
                    && let Some(m) = battle.state.at_mut(source)
                {
                    m.force_switch_flag = true;
                }
            }
            None
        }
        // The Terrain Seeds: used on switching in on their terrain, or when
        // it starts.
        (i::ELECTRIC_SEED | i::GRASSY_SEED | i::MISTY_SEED | i::PSYCHIC_SEED, Event::Start, On) => {
            if !super::ignoring_item(battle, holder) && battle.state.field.terrain == Some(seed_terrain(item)) {
                super::use_item(battle, holder, None, Effect::Item(item));
            }
            None
        }
        (i::ELECTRIC_SEED | i::GRASSY_SEED | i::MISTY_SEED | i::PSYCHIC_SEED, Event::TerrainChange, On) => {
            if battle.state.field.terrain == Some(seed_terrain(item)) {
                super::use_item(battle, holder, None, Effect::Item(item));
            }
            None
        }
        // Room Service: used under Trick Room.
        (i::ROOM_SERVICE, Event::Start, On) => {
            if !super::ignoring_item(battle, holder) && battle.state.field.trick_room_turns > 0 {
                super::use_item(battle, holder, None, Effect::Item(item));
            }
            None
        }
        (i::ROOM_SERVICE, Event::PseudoWeatherChange, Any) => {
            if battle.state.field.trick_room_turns > 0 {
                super::use_item(battle, holder, Some(holder), Effect::Item(item));
            }
            None
        }
        // Normal Gem: `onSourceTryPrimaryHit` powers up its holder's Normal
        // move (the `gem` volatile).
        (i::NORMAL_GEM, Event::TryPrimaryHit, Source) => {
            let data = ev.move_data()?;
            if ev.target_pos() == Some(holder)
                || data.category == MoveCategory::Status
                || data.flags.contains(MoveFlags::PLEDGE_COMBO)
            {
                return None;
            }
            // Struggle's type is `???`.
            let struggle = ev.effect.move_id() == Some(moves::STRUGGLE);
            if data.ty == pikapro_data::Type::Normal
                && !struggle
                && super::use_item(battle, holder, None, Effect::Item(item))
            {
                add_gem(battle, holder);
            }
            None
        }
        // Power Herb: `onChargeMove` skips the charging turn.
        (i::POWER_HERB, Event::ChargeMove, On) => {
            if super::use_item(battle, holder, None, Effect::Item(item)) {
                // `attrLastMove('[still]')` (then the attack is shown by
                // `-anim`).
                battle.flush_rng_trace();
                battle.attr_last_move(crate::event::MoveAttr::Still);
                // `addMove('-anim', pokemon, move.name, target)`.
                if let (Some(move_id), Some(target)) = (ev.effect.move_id(), ev.source) {
                    super::super::multiturn::anim(battle, holder, move_id, target);
                }
                return Some(Relay::False);
            }
            None
        }
        // Mirror Herb: copies the foes' stat rises, after the switch-ins,
        // Terastallization, the move or the end of the turn.
        (i::MIRROR_HERB, Event::AfterBoost, Foe) => {
            if matches!(ev.effect, Effect::Ability(a::OPPORTUNIST) | Effect::Item(i::MIRROR_HERB)) {
                return None;
            }
            let Relay::Boosts(b) = ev.relay else { return None };
            let state = &mut battle.state.side_mut(holder.side).team[holder.slot as usize].volatile_counters.item_state;
            let mut plus = Boosts { changes: state.mirror_herb_boosts, keys: state.mirror_herb_keys };
            for (id, n) in b.iter() {
                if n > 0 {
                    plus.set(id, plus.get(id).unwrap_or(0) + n);
                    state.mirror_herb_ready = true;
                }
            }
            state.mirror_herb_boosts = plus.changes;
            state.mirror_herb_keys = plus.keys;
            None
        }
        (i::MIRROR_HERB, Event::SwitchIn | Event::AfterMove | Event::AfterMega | Event::AfterTerastallization, Any)
        | (i::MIRROR_HERB, Event::Residual, On) => {
            if item_state(battle, holder).mirror_herb_ready {
                super::use_item(battle, holder, None, Effect::Item(item));
            }
            None
        }
        (i::MIRROR_HERB, Event::Use, On) => {
            let state = item_state(battle, holder);
            let b = Boosts { changes: state.mirror_herb_boosts, keys: state.mirror_herb_keys };
            crate::resolve::boosts::boost_from(battle, holder, named(b), ev.source, Effect::Item(item), false, false);
            None
        }
        // Sticky Barb: hurts its holder each turn, and sticks to an
        // itemless attacker that makes contact.
        (i::STICKY_BARB, Event::Residual, On) => {
            let max = battle.state.at(holder)?.max_hp() as u32;
            damage_by(battle, holder, super::super::hp_fraction(max, 1, 8), None, Effect::Item(item));
            None
        }
        (i::STICKY_BARB, Event::Hit, On) => {
            let source = ev.source?;
            let data = *ev.move_data()?;
            if source == holder || battle.state.at(source).is_none_or(|m| m.item.is_some()) {
                return None;
            }
            if abilities::makes_contact(battle, &data, source, holder, None)
                && let Some(barb) = super::take_item(battle, holder, None)
            {
                super::set_item(battle, source, Some(barb), None, Effect::None);
            }
            None
        }
        // Booster Energy (`onSwitchInPriority` -2): `onStart` marks it
        // started and runs `onUpdate`, which uses it for Protosynthesis
        // outside sun or Quark Drive outside Electric Terrain.
        (i::BOOSTER_ENERGY, Event::Start, On) => {
            battle.state.side_mut(holder.side).team[holder.slot as usize]
                .volatile_counters
                .item_state
                .booster_started = true;
            booster_energy_update(battle, holder);
            None
        }
        (i::BOOSTER_ENERGY, Event::Update, On) => {
            booster_energy_update(battle, holder);
            None
        }
        // `onTakeItem`: not from a Paradox Pokémon.
        (i::BOOSTER_ENERGY, Event::TakeItem, On) => {
            let species = battle.state.side(holder.side).team[holder.slot as usize].base_species;
            Some(Relay::from_bool(!battle.format.dex.species(species).paradox))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Item helpers.

/// [`super::update_idle`] for this module's items.
pub(super) fn update_idle(battle: &Battle, holder: Position, item: ItemId) -> bool {
    if battle.format.dex.item(item).is_berry {
        return berries::update_eats(battle, holder, item) == Some(false);
    }
    match item {
        i::MENTAL_HERB => mental_herb_first(battle, holder).is_none(),
        i::BOOSTER_ENERGY => {
            battle.state.side(holder.side).team.get(holder.slot as usize).is_some_and(booster_energy_waits)
        }
        _ => false,
    }
}

/// Booster Energy's `onUpdate` does nothing: the holder's Protosynthesis or
/// Quark Drive hasn't started yet, or it is transformed.
fn booster_energy_waits(mon: &crate::state::PokemonState) -> bool {
    !mon.volatile_counters.item_state.booster_started || mon.volatiles.has(Volatiles::TRANSFORMED)
}

/// Booster Energy's `onUpdate`.
fn booster_energy_update(battle: &mut Battle, holder: Position) {
    if booster_energy_waits(&battle.state.side(holder.side).team[holder.slot as usize]) {
        return;
    }
    let effect = Effect::Item(i::BOOSTER_ENERGY);
    if abilities::has_ability(battle, holder, a::PROTOSYNTHESIS)
        && abilities::effective_weather(battle) != Some(Weather::Sun)
        && super::use_item(battle, holder, None, effect)
    {
        abilities::switchin::booster_energy(battle, holder, false);
    }
    if abilities::has_ability(battle, holder, a::QUARK_DRIVE)
        && battle.state.field.terrain != Some(Terrain::Electric)
        && super::use_item(battle, holder, None, effect)
    {
        abilities::switchin::booster_energy(battle, holder, true);
    }
}

fn item_state(battle: &Battle, pos: Position) -> crate::state::ItemState {
    battle.state.side(pos.side).team[pos.slot as usize].volatile_counters.item_state
}

fn set_switch_flag(battle: &mut Battle, pos: Position, flag: SwitchFlag) {
    if let Some(m) = battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) {
        m.switch_flag = flag;
    }
}

/// A boosts object as the `StatBoosts` `boost` applies: its named stats in
/// its key order.
fn named(b: Boosts) -> StatBoosts {
    let mut stages = [0; 7];
    for (id, n) in b.iter() {
        stages[id as usize] = n;
    }
    StatBoosts { stages, order: b.changes.order }
}

/// White Herb's `onStart`: the lowered stats are noted and the herb used.
fn white_herb_start(battle: &mut Battle, holder: Position, item: ItemId) {
    let Some(mon) = battle.state.side(holder.side).team.get(holder.slot as usize) else { return };
    let mut bits = 0u8;
    for id in BoostId::ALL {
        if mon.boosts.get(id) < 0 {
            bits |= 1 << id as u8;
        }
    }
    battle.state.side_mut(holder.side).team[holder.slot as usize].volatile_counters.item_state.white_herb = bits;
    if bits != 0 {
        super::use_item(battle, holder, None, Effect::Item(item));
    }
    if let Some(m) = battle.state.side_mut(holder.side).team.get_mut(holder.slot as usize) {
        m.volatile_counters.item_state.white_herb = 0;
    }
}

/// Air Balloon pops: `-enditem`, the item is gone (not used: no
/// `lastItem`), and `AfterUseItem` runs.
fn pop_balloon(battle: &mut Battle, holder: Position, item: ItemId) {
    let a = actor(battle, holder);
    battle.emit(BattleEvent::ItemEnd { actor: a, item, note: ItemEndNote::None, from: None, of: None });
    lose_item(battle, holder);
    let mut ev = EventCtx::new(Event::AfterUseItem, Target::Pokemon(holder), None, Effect::None, Relay::Item(item));
    events::run_event(battle, &mut ev, None);
}

/// `pokemon.item = ''` and `clearEffectState(itemState)`.
pub(crate) fn lose_item(battle: &mut Battle, pos: Position) {
    if let Some(m) = battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) {
        m.item = None;
        m.volatile_counters.item_order = 0;
        m.volatile_counters.item_state = Default::default();
    }
}

/// The terrain a Terrain Seed reacts to.
fn seed_terrain(item: ItemId) -> Terrain {
    match item {
        i::ELECTRIC_SEED => Terrain::Electric,
        i::GRASSY_SEED => Terrain::Grassy,
        i::MISTY_SEED => Terrain::Misty,
        _ => Terrain::Psychic,
    }
}

/// Showdown's `removeVolatile` for the volatiles Mental Herb, Persim Berry
/// and Lum Berry cure (with their `-end` messages).
pub(crate) fn remove_volatile(battle: &mut Battle, pos: Position, flag: u128) -> bool {
    match flag {
        Volatiles::CONFUSION | Volatiles::TAUNT | Volatiles::ENCORE | Volatiles::DISABLE => {
            crate::resolve::volatiles::remove_volatile(battle, pos, flag)
        }
        _ => crate::resolve::conditions::remove(battle, pos, flag),
    }
}

/// Micle Berry's `onEat`: `addVolatile('micleberry')` (2 turns).
fn add_micle(battle: &mut Battle, pos: Position) {
    let effect = Effect::Item(i::MICLE_BERRY);
    if events::add_volatile_flag(battle, pos, Volatiles::MICLE_BERRY, "micleberry", None, effect)
        && let Some(m) = battle.state.at_mut(pos)
    {
        m.volatile_counters.micle_berry = 2;
    }
}

/// A Gem's `source.addVolatile('gem')` (1 turn). `gem` has
/// `affectsFainted`: a user that fainted using its move (Self-Destruct)
/// still gets it.
fn add_gem(battle: &mut Battle, pos: Position) {
    let effect = Effect::Item(i::NORMAL_GEM);
    if battle.state.at(pos).is_none_or(|m| m.volatiles.has(Volatiles::GEM)) {
        return;
    }
    if events::immunity_event(battle, pos, "gem") != Some(true) {
        return;
    }
    let mut ev = EventCtx::new(Event::TryAddVolatile, Target::Pokemon(pos), Some(pos), effect, Relay::Id("gem"));
    if !events::run_event(battle, &mut ev, None).is_truthy() {
        return;
    }
    if let Some(m) = battle.state.at_mut(pos) {
        m.volatiles.add(Volatiles::GEM);
        m.volatile_counters.gem = 1;
    }
}

// ---------------------------------------------------------------------------
// Abilities.

/// Runs this module's `ability`'s callback for `ev.event` /
/// `ev.prefix` on its holder.
pub(crate) fn run_ability(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    use EventPrefix::{Ally, On, Source};
    let mon = *battle.state.side(holder.side).team.get(holder.slot as usize)?;
    match (ability, ev.event, ev.prefix) {
        // Cheek Pouch: `onEatItem` heals a third of max HP.
        (a::CHEEK_POUCH, Event::EatItem, On) => {
            heal_by(
                battle,
                holder,
                super::super::hp_fraction(mon.max_hp() as u32, 1, 3),
                ev.source,
                Effect::Ability(ability),
            );
            None
        }
        // Cud Chew: a berry it ate (not by Bug Bite or Pluck) is eaten
        // again at the end of the next turn.
        (a::CUD_CHEW, Event::EatItem, On) => {
            let Relay::Item(item) = ev.relay else { return None };
            let stolen = matches!(ev.effect, Effect::Move(moves::BUG_BITE | moves::PLUCK));
            if battle.format.dex.item(item).is_berry && !stolen {
                let last = battle.queue.peek().is_none();
                let state =
                    &mut battle.state.side_mut(holder.side).team[holder.slot as usize].volatile_counters.ability_state;
                state.cud_chew_berry = Some(item);
                state.cud_chew_counter = if last { 1 } else { 2 };
            }
            None
        }
        (a::CUD_CHEW, Event::Residual, On) => {
            let state = mon.volatile_counters.ability_state;
            let berry = state.cud_chew_berry?;
            if mon.hp == 0 {
                return None;
            }
            let counter = state.cud_chew_counter.saturating_sub(1);
            battle.state.side_mut(holder.side).team[holder.slot as usize]
                .volatile_counters
                .ability_state
                .cud_chew_counter = counter;
            if counter == 0 {
                abilities::activate(battle, holder, ability);
                let a = actor(battle, holder);
                battle.emit(BattleEvent::ItemEnd {
                    actor: a,
                    item: berry,
                    note: ItemEndNote::Eat,
                    from: None,
                    of: None,
                });
                if events::single_event_item(battle, Event::Eat, holder, berry, None, Effect::None, Relay::True)
                    .is_truthy()
                {
                    let mut ev =
                        EventCtx::new(Event::EatItem, Target::Pokemon(holder), None, Effect::None, Relay::Item(berry));
                    events::run_event(battle, &mut ev, None);
                }
                let on_eat = has_on_eat(battle, berry);
                let m = &mut battle.state.side_mut(holder.side).team[holder.slot as usize];
                if on_eat {
                    m.ate_berry = true;
                }
                m.volatile_counters.ability_state.cud_chew_berry = None;
                m.volatile_counters.ability_state.cud_chew_counter = 0;
            }
            None
        }
        // Gluttony: `abilityState.gluttony` (the pinch berries check it).
        (a::GLUTTONY, Event::Start | Event::Damage, On) => {
            if let Some(m) = battle.state.side_mut(holder.side).team.get_mut(holder.slot as usize) {
                m.volatile_counters.ability_state.gluttony = true;
            }
            None
        }
        // Harvest: at the end of the turn (always in sun, else half the
        // time), the berry it ate grows back.
        (a::HARVEST, Event::Residual, On) => {
            let sun = matches!(abilities::effective_weather(battle), Some(Weather::Sun | Weather::HarshSun));
            if sun || battle.rng.chance(1, 2) {
                let mon = *battle.state.at(holder)?;
                let berry = mon.last_item.filter(|&it| battle.format.dex.item(it).is_berry);
                if let (true, None, Some(berry)) = (mon.hp > 0, mon.item, berry) {
                    super::set_item(battle, holder, Some(berry), None, Effect::None);
                    battle.state.side_mut(holder.side).team[holder.slot as usize].last_item = None;
                    let a = actor(battle, holder);
                    let cause = Some(Cause::ability(ability));
                    battle.emit(BattleEvent::ItemShow { actor: a, item: berry, cause });
                }
            }
            None
        }
        // Magician: after its damaging move, it steals the item of the
        // first target hit (by speed) that has one.
        (a::MAGICIAN, Event::AfterMoveSecondarySelf, On) => {
            let data = ev.move_data()?;
            let move_id = ev.effect.move_id()?;
            let mask = match battle.move_state.smart_hit_target {
                Some(t) => 1 << (t.side.index() * 2 + t.slot as usize),
                None => mon.record.hit_targets?,
            };
            if mon.switch_flag.is_true()
                || mon.item.is_some()
                || mon.volatiles.has(Volatiles::GEM)
                || move_id == moves::FLING
                || data.category == MoveCategory::Status
            {
                return None;
            }
            // `move.hitTargets` is in the move's target order (the user's
            // allies, then its foes: `getMoveTargets`), which the tie
            // shuffle of `speedSort` depends on.
            let mut targets = crate::util::StackVec::<(Position, i32), 4>::new();
            for side in [holder.side, holder.side.foe()] {
                for slot in 0..battle.format.active_per_side() {
                    let p = Position::new(side, slot as u8);
                    if mask & (1 << (side.index() * 2 + slot)) != 0 {
                        targets.push((p, battle.state.side(side).team[slot].speed));
                    }
                }
            }
            crate::resolve::queue::speed_sort_by_speed(&mut targets, &mut battle.rng);
            // `this.speedSort(hitTargets)` sorts `move.hitTargets` itself:
            // Sparkling Aria's `onAfterMove` cures in this order (the only
            // later reader of the order).
            if move_id == moves::SPARKLING_ARIA {
                let nibble = |p: Position| 1 + (p.side.index() * 2 + p.slot as usize) as u16;
                // (Its targets: at most the user's ally and two foes.)
                battle.order.hit_order = targets
                    .iter()
                    .take(3)
                    .enumerate()
                    .fold(nibble(holder), |packed, (i, &(p, _))| packed | nibble(p) << (4 * (i + 1)));
            }
            for &(target, _) in targets.iter() {
                if target == holder {
                    continue;
                }
                let Some(item) = super::take_item(battle, target, Some(holder)) else { continue };
                if !super::set_item(battle, holder, Some(item), None, Effect::None) {
                    restore_item(battle, target, item);
                    continue;
                }
                let (h, t) = (actor(battle, holder), actor(battle, target));
                battle.emit(BattleEvent::ItemShow { actor: h, item, cause: Some(Cause::ability(ability).of(t)) });
                return None;
            }
            None
        }
        // Pickpocket: steals the item of an attacker that made contact.
        (a::PICKPOCKET, Event::AfterMoveSecondary, On) => {
            let source = ev.source?;
            let data = ev.move_data()?;
            if source == holder || !data.flags.contains(MoveFlags::CONTACT) {
                return None;
            }
            let source_flag = battle.state.side(source.side).team[source.slot as usize].switch_flag;
            if mon.item.is_some()
                || mon.switch_flag != SwitchFlag::None
                || mon.force_switch_flag
                || source_flag.is_true()
            {
                return None;
            }
            let item = super::take_item(battle, source, Some(holder))?;
            if !super::set_item(battle, holder, Some(item), None, Effect::None) {
                restore_item(battle, source, item);
                return None;
            }
            let s = actor(battle, source);
            let h = actor(battle, holder);
            battle.emit(BattleEvent::ItemEnd {
                actor: s,
                item,
                note: ItemEndNote::Silent,
                from: Some(EffectName::Ability(ability)),
                of: Some(s),
            });
            battle.emit(BattleEvent::ItemShow { actor: h, item, cause: Some(Cause::ability(ability).of(s)) });
            None
        }
        // Pickup: at the end of the turn, an itemless holder picks up the
        // item an adjacent Pokémon used this turn (a random one).
        (a::PICKUP, Event::Residual, On) => {
            if mon.item.is_some() {
                return None;
            }
            let candidates: crate::util::StackVec<Position, 4> = abilities::all_active(battle)
                .filter(|&p| {
                    let m = &battle.state.side(p.side).team[p.slot as usize];
                    m.last_item.is_some() && m.used_item_this_turn && adjacent(battle, holder, p)
                })
                .collect();
            if candidates.is_empty() {
                return None;
            }
            let target = *battle.rng.sample(&candidates);
            let item = battle.state.side(target.side).team[target.slot as usize].last_item?;
            battle.state.side_mut(target.side).team[target.slot as usize].last_item = None;
            let h = actor(battle, holder);
            battle.emit(BattleEvent::ItemShow { actor: h, item, cause: Some(Cause::ability(ability)) });
            super::set_item(battle, holder, Some(item), None, Effect::None);
            None
        }
        // Ripen: berries' healing and boosts are doubled, a resist berry
        // halves the damage twice.
        (a::RIPEN, Event::TryHeal, On) => {
            let Effect::Item(item) = ev.effect else { return None };
            if item == i::LEFTOVERS {
                abilities::activate(battle, holder, ability);
            }
            if battle.format.dex.item(item).is_berry {
                return ev.chain_modify(2, 1);
            }
            None
        }
        (a::RIPEN, Event::ChangeBoost, On) => {
            let Effect::Item(item) = ev.effect else { return None };
            let Relay::Boosts(mut b) = ev.relay else { return None };
            if !battle.format.dex.item(item).is_berry {
                return None;
            }
            for id in BoostId::ALL {
                if let Some(n) = b.get(id) {
                    b.set(id, n * 2);
                }
            }
            Some(Relay::Boosts(b))
        }
        (a::RIPEN, Event::ModifyDamage, Source) => {
            if mon.volatile_counters.ability_state.berry_weaken {
                battle.state.side_mut(holder.side).team[holder.slot as usize]
                    .volatile_counters
                    .ability_state
                    .berry_weaken = false;
                return ev.chain_modify(1, 2);
            }
            None
        }
        (a::RIPEN, Event::TryEatItem, On) => {
            abilities::activate(battle, holder, ability);
            None
        }
        (a::RIPEN, Event::EatItem, On) => {
            let Relay::Item(item) = ev.relay else { return None };
            let weaken = berries::resist_type(item).is_some();
            battle.state.side_mut(holder.side).team[holder.slot as usize]
                .volatile_counters
                .ability_state
                .berry_weaken = weaken;
            None
        }
        // Sticky Hold: its item can't be taken by another Pokémon (or
        // knocked off).
        (a::STICKY_HOLD, Event::TakeItem, On) => {
            if mon.hp == 0 || mon.item == Some(i::STICKY_BARB) {
                return None;
            }
            let knock_off = battle.active_move.is_some_and(|(_, m)| m == moves::KNOCK_OFF);
            if ev.source.is_some_and(|s| s != holder) || knock_off {
                abilities::activate(battle, holder, ability);
                return Some(Relay::False);
            }
            None
        }
        // Symbiosis: an ally that used up its item gets the holder's.
        (a::SYMBIOSIS, Event::AfterUseItem, Ally) => {
            let pokemon = ev.target_pos()?;
            if battle.state.side(pokemon.side).team[pokemon.slot as usize].switch_flag != SwitchFlag::None {
                return None;
            }
            let item = super::take_item(battle, holder, None)?;
            let effect = Effect::Ability(ability);
            let kept = events::single_event_item(
                battle,
                Event::TakeItem,
                pokemon,
                item,
                Some(holder),
                effect,
                Relay::Item(item),
            );
            if !kept.is_truthy() || !super::set_item(battle, pokemon, Some(item), None, Effect::None) {
                restore_item(battle, holder, item);
                return None;
            }
            let (h, p) = (actor(battle, holder), actor(battle, pokemon));
            battle.emit(BattleEvent::ActivateEffect {
                actor: h,
                effect: EffectName::Ability(ability),
                args: [Some(LineArg::Item(item)), None],
                of: Some(p),
            });
            None
        }
        // Unburden: losing its item doubles its Speed (a volatile).
        (a::UNBURDEN, Event::AfterUseItem | Event::TakeItem, On) => {
            if ev.event == Event::AfterUseItem && ev.target_pos() != Some(holder) {
                return None;
            }
            let effect = Effect::Ability(ability);
            events::add_volatile_flag(battle, holder, Volatiles::UNBURDEN, "unburden", None, effect);
            None
        }
        (a::UNBURDEN, Event::End, On) => {
            events::remove_volatile_flag(battle, holder, Volatiles::UNBURDEN);
            None
        }
        _ => None,
    }
}

/// `pokemon.item = item.id`: a taken item is put back (its state stays
/// cleared).
pub(crate) fn restore_item(battle: &mut Battle, pos: Position, item: ItemId) {
    if let Some(m) = battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) {
        m.item = Some(item);
    }
}

/// Showdown's `pokemon.isAdjacent(other)` for active Pokémon (up to two a
/// side, so any other one).
fn adjacent(battle: &Battle, a: Position, b: Position) -> bool {
    a != b && events::is_active(battle, a) && events::is_active(battle, b)
}

// ---------------------------------------------------------------------------
// Volatiles.

/// The volatiles Mental Herb cures, in its order.
const MENTAL_HERB_CONDITIONS: [u128; 6] = [
    Volatiles::ATTRACT,
    Volatiles::TAUNT,
    Volatiles::ENCORE,
    Volatiles::TORMENT,
    Volatiles::DISABLE,
    Volatiles::HEAL_BLOCK,
];

/// The first of Mental Herb's volatiles the Pokémon at `pos` has.
pub(crate) fn mental_herb_first(battle: &Battle, pos: Position) -> Option<u128> {
    let volatiles = battle.state.at(pos)?.volatiles;
    MENTAL_HERB_CONDITIONS.into_iter().find(|&f| volatiles.has(f))
}

/// Mental Herb's cure (its `onUpdate`, also Fling's effect), once `first`
/// was found: every one of its volatiles ends, Attract with
/// `-end|...|move: Attract|[from] item: Mental Herb` if it came first.
pub(crate) fn mental_herb_cure(battle: &mut Battle, pos: Position, first: u128) {
    for second in MENTAL_HERB_CONDITIONS {
        remove_volatile(battle, pos, second);
        if first == Volatiles::ATTRACT && second == Volatiles::ATTRACT {
            let a = actor(battle, pos);
            battle.emit(BattleEvent::EffectStartEnd {
                actor: a,
                start: false,
                what: LineArg::Text(""),
                effect: Some(EffectName::Move(moves::ATTRACT)),
                cause: Some(Cause::item(i::MENTAL_HERB)),
                silent: false,
            });
        }
    }
}

/// This module's volatiles' handlers (bits 112..=123).
pub(crate) fn volatile_spec(flag: u128, event: Event, prefix: EventPrefix) -> Option<Spec> {
    use EventPrefix::{On, Source};
    match (flag, event, prefix) {
        (Volatiles::UNBURDEN, Event::ModifySpe, On) => Some(Spec::relay(unburden_modify_spe)),
        (Volatiles::MICLE_BERRY, Event::Accuracy, Source) => Some(Spec::relay(micle_accuracy)),
        (Volatiles::MICLE_BERRY, Event::Residual, On) => Some(Spec::plain(tick_micle)),
        (Volatiles::FLING, Event::Update, On) => Some(Spec::relay(fling_update)),
        (Volatiles::GEM, Event::BasePower, On) => Some(Spec::relay(gem_base_power).priority(14)),
        (Volatiles::GEM, Event::Residual, On) => Some(Spec::plain(tick_gem)),
        _ => None,
    }
}

/// The events [`volatile_spec`] handles for `flag` (any prefix). Keep it in
/// step with `volatile_spec` (a test checks them).
pub(crate) const fn volatile_events(flag: u128) -> u128 {
    match flag {
        Volatiles::UNBURDEN => const { pikapro_data::EventSet::of(&[Event::ModifySpe]).0 },
        Volatiles::MICLE_BERRY => const { pikapro_data::EventSet::of(&[Event::Accuracy, Event::Residual]).0 },
        Volatiles::FLING => const { pikapro_data::EventSet::of(&[Event::Update]).0 },
        Volatiles::GEM => const { pikapro_data::EventSet::of(&[Event::BasePower, Event::Residual]).0 },
        _ => 0,
    }
}

/// `unburden`'s `onModifySpe`: 2x while it holds nothing.
fn unburden_modify_spe(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let pos = holder_of(h)?;
    let mon = battle.state.side(pos.side).team.get(pos.slot as usize)?;
    if mon.item.is_none() && !abilities::ignoring_ability(battle, pos) {
        return ev.chain_modify(2, 1);
    }
    None
}

/// `micleberry`'s `onSourceAccuracy`: the next move (not an OHKO move) is
/// 1.2x as accurate, and the berry's effect ends.
fn micle_accuracy(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let pos = holder_of(h)?;
    if ev.move_data().is_some_and(|d| d.ohko) {
        return None;
    }
    let a = actor(battle, pos);
    battle.emit(BattleEvent::ItemEnd { actor: a, item: i::MICLE_BERRY, note: ItemEndNote::None, from: None, of: None });
    // `removeVolatile` does nothing without HP (after Explosion's faint).
    events::remove_volatile_flag(battle, pos, Volatiles::MICLE_BERRY);
    if let Relay::Num(_) = ev.relay {
        ev.chain(4915);
    }
    None
}

fn tick_micle(battle: &mut Battle, h: &Handler) -> bool {
    tick(battle, h, Volatiles::MICLE_BERRY, |c| &mut c.micle_berry)
}

fn tick_gem(battle: &mut Battle, h: &Handler) -> bool {
    tick(battle, h, Volatiles::GEM, |c| &mut c.gem)
}

/// A volatile's `duration` at the end of the turn: it ends at 0.
fn tick(
    battle: &mut Battle,
    h: &Handler,
    flag: u128,
    counter: fn(&mut crate::state::VolatileCounters) -> &mut u8,
) -> bool {
    let Some(pos) = holder_of(h) else { return false };
    let Some(m) = battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) else { return false };
    if !m.volatiles.has(flag) {
        return false;
    }
    let turns = counter(&mut m.volatile_counters);
    *turns = turns.saturating_sub(1);
    if *turns == 0 {
        m.volatiles.clear(flag);
        return false;
    }
    true
}

/// `gem`'s `onBasePower` (priority 14): 5325/4096.
fn gem_base_power(_: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    ev.chain(5325);
    None
}

/// `fling`'s `onUpdate`: the flung item is gone.
///
/// ```js
/// const item = pokemon.getItem();
/// pokemon.setItem('');
/// pokemon.lastItem = item.id;
/// pokemon.usedItemThisTurn = true;
/// this.add('-enditem', pokemon, item.name, '[from] move: Fling');
/// this.runEvent('AfterUseItem', pokemon, null, null, item);
/// pokemon.removeVolatile('fling');
/// ```
///
/// All of it runs when the item is gone already (a flung Blunder Policy
/// that missed was used up by the miss): the line names no item,
/// `lastItem` is emptied and `AfterUseItem` runs again.
fn fling_update(battle: &mut Battle, _: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let pos = holder_of(h)?;
    let item = battle.state.side(pos.side).team[pos.slot as usize].item;
    super::set_item(battle, pos, None, None, Effect::None);
    let m = &mut battle.state.side_mut(pos.side).team[pos.slot as usize];
    m.last_item = item;
    m.used_item_this_turn = true;
    let a = actor(battle, pos);
    let relay = match item {
        Some(item) => {
            battle.emit(BattleEvent::ItemEnd {
                actor: a,
                item,
                note: ItemEndNote::None,
                from: Some(EffectName::Move(moves::FLING)),
                of: None,
            });
            Relay::Item(item)
        }
        None => {
            battle.emit(BattleEvent::FlingNoItem { actor: a });
            // (No `AfterUseItem` handler reads the item.)
            Relay::True
        }
    };
    let mut ev = EventCtx::new(Event::AfterUseItem, Target::Pokemon(pos), None, Effect::None, relay);
    events::run_event(battle, &mut ev, None);
    events::remove_volatile_flag(battle, pos, Volatiles::FLING);
    None
}

// ---------------------------------------------------------------------------
// Items checked at their call sites.

/// `hitStepAccuracy` after a miss (not by an OHKO move): Blunder Policy
/// (`useItem`, then `boost({spe: 2}, pokemon)`: its effect is `this.effect`,
/// the battle's empty one outside an event, or Magic Bounce's `TryHit`
/// while it reflects the move: `-ability|Magic Bounce|boost` first. Dancer
/// calls `runMove` outside any event, so its copy boosts plainly).
pub(crate) fn after_miss(battle: &mut Battle, user: Position) {
    if super::has_item(battle, user, i::BLUNDER_POLICY)
        && super::use_item(battle, user, None, Effect::Item(i::BLUNDER_POLICY))
    {
        let spe = StatBoosts::new([0, 0, 0, 0, 2, 0, 0]);
        let effect = if battle.move_state.has_bounced {
            Effect::Ability(pikapro_data::gen9::abilities::MAGIC_BOUNCE)
        } else {
            Effect::Condition(events::Condition::Other)
        };
        crate::resolve::boosts::boost_from(battle, user, spe, None, effect, false, false);
    }
}

#[cfg(test)]
mod tests {
    use pikapro_data::gen9::{abilities as a, items as i, moves as m, species as s};

    use crate::event::BattleEvent;
    use crate::{Battle, BattleOptions, Choice, Format, PokemonSet, Seed, SideId, SlotAction, Team};

    /// A Dancer copy that misses: Blunder Policy boosts plainly, because
    /// Dancer calls `runMove` outside any event (only Magic Bounce's reflection
    /// runs inside its ability's handler).
    #[test]
    fn a_dancer_copy_that_misses_boosts_plainly() {
        for seed in 1..200 {
            let dancer = PokemonSet::new(s::ORICORIO)
                .with_ability(a::DANCER)
                .with_item(i::BLUNDER_POLICY)
                .with_moves(&[m::SPLASH]);
            let foe = PokemonSet::new(s::MEW).with_ability(a::SYNCHRONIZE).with_moves(&[m::FEATHER_DANCE]);
            let teams = [Team::new(vec![dancer]), Team::new(vec![foe])];
            let mut b = Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(seed))).unwrap();
            b.state.side_mut(SideId::P1).team[0].boosts.accuracy = -6;
            let act = || Choice::single(SlotAction::Move { index: 0, target: None, gimmick: None });
            b.submit(SideId::P1, act()).unwrap();
            b.submit(SideId::P2, act()).unwrap();
            if b.state.side(SideId::P1).team[0].item.is_some() {
                continue;
            }
            assert_eq!(b.state.side(SideId::P1).team[0].boosts.spe, 2);
            assert!(
                !b.events()
                    .iter()
                    .any(|e| matches!(e, BattleEvent::AbilityShow { note: crate::event::AbilityNote::Boost, .. })),
                "seed {seed}: {:?}",
                b.events()
            );
            return;
        }
        panic!("the copy never missed");
    }

    /// A flung confusion berry's effect is the berry, not the move, so
    /// Infiltrator doesn't take its confusion through Safeguard.
    #[test]
    fn safeguard_stops_a_flung_berrys_confusion_even_from_infiltrator() {
        for safeguard in [0, 5] {
            let user =
                PokemonSet::new(s::MEW).with_ability(a::INFILTRATOR).with_item(i::FIGY_BERRY).with_moves(&[m::FLING]);
            let eater = PokemonSet::new(s::BLISSEY)
                .with_ability(a::NATURAL_CURE)
                .with_nature(pikapro_data::Nature::Bold)
                .with_moves(&[m::SPLASH]);
            let teams = [Team::new(vec![user]), Team::new(vec![eater])];
            let mut b = Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(1))).unwrap();
            if safeguard > 0 {
                let holder = crate::Position::new(SideId::P2, 0);
                assert!(crate::resolve::field::add_side_condition(&mut b, SideId::P2, "safeguard", holder));
            }
            let act = || Choice::single(SlotAction::Move { index: 0, target: None, gimmick: None });
            b.submit(SideId::P1, act()).unwrap();
            b.submit(SideId::P2, act()).unwrap();
            let confused = b.state.side(SideId::P2).team[0].volatiles.has(crate::state::Volatiles::CONFUSION);
            assert_eq!(confused, safeguard == 0, "safeguard {safeguard}");
        }
    }

    /// A flung Blunder Policy that misses is used up by the miss; Fling's
    /// cleanup then runs on a Pokémon without an item, as Showdown's does:
    /// `-enditem|<pokemon>||[from] move: Fling`, and `lastItem` is emptied
    /// (conformance, gen9ou 40,814,827).
    #[test]
    fn fling_cleans_up_after_a_blunder_policy_that_missed() {
        // The first seed whose Fling misses a foe at +6 evasion.
        for seed in 1..40 {
            let flinger = PokemonSet::new(s::SCIZOR)
                .with_ability(a::TECHNICIAN)
                .with_item(i::BLUNDER_POLICY)
                .with_moves(&[m::FLING, m::SPLASH]);
            let foe = PokemonSet::new(s::BLISSEY).with_ability(a::NATURAL_CURE).with_moves(&[m::SPLASH]);
            let teams = [Team::new(vec![flinger]), Team::new(vec![foe])];
            let mut b = Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(seed))).unwrap();
            b.state.side_mut(SideId::P2).team[0].boosts.evasion = 6;
            let act = |index| Choice::single(SlotAction::Move { index, target: None, gimmick: None });
            b.submit(SideId::P1, act(0)).unwrap();
            b.submit(SideId::P2, act(0)).unwrap();
            if !b.events().iter().any(|e| matches!(e, BattleEvent::Miss { .. })) {
                continue;
            }
            let ends: Vec<&BattleEvent> = b
                .events()
                .iter()
                .filter(|e| matches!(e, BattleEvent::ItemEnd { .. } | BattleEvent::FlingNoItem { .. }))
                .collect();
            assert!(
                matches!(
                    ends[..],
                    [BattleEvent::ItemEnd { item: i::BLUNDER_POLICY, from: None, .. }, BattleEvent::FlingNoItem { .. }]
                ),
                "seed {seed}: {ends:?}"
            );
            let user = b.state.side(SideId::P1).team[0];
            assert_eq!((user.item, user.last_item, user.used_item_this_turn), (None, None, false));
            assert_eq!(user.boosts.spe, 2, "Blunder Policy");
            assert!(!user.volatiles.has(crate::state::Volatiles::FLING));
            return;
        }
        panic!("Fling never missed");
    }
}
