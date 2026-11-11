//! Items: dispatch to the module that implements each one, and the item
//! lifecycle every item needs (Showdown's `setItem`, `takeItem`, `useItem`,
//! `eatItem`, `ignoringItem`).
//!
//! # Adding an item
//!
//! 1. Read the item in Showdown's `data/items.ts` (and
//!    `data/mods/champions/items.ts`). Its callbacks and their ordering
//!    are already in the generated data (`ItemData::handlers`), so the
//!    engine already collects and sorts them at every hook.
//! 2. Assign it a module in [`owner`] (`held.rs`: items that stay held;
//!    `consumed.rs`: berries and consumed items) and add one arm per
//!    callback to that module's `run`:
//!    `(items::X, Event::ModifyDamage, On) => ...`, as
//!    for abilities (`abilities/mod.rs`). A berry's `onEat` runs through
//!    [`eat_item`]; a consumed item's `onUse` through [`use_item`].
//! 3. Items checked where they matter (Damp Rock, Heavy-Duty Boots, Light
//!    Clay, ...) use [`has_item`] at the call site, which already respects
//!    Klutz, Embargo and Magic Room.
//! 4. Add it to `IMPLEMENTED_ITEMS` in `pikapro-conformance`, fuzz,
//!    record fixtures.

pub(crate) mod consumed;
pub(crate) mod held;
mod mega;

use pikapro_data::gen9::{abilities, items as i};
use pikapro_data::{Event, ItemId};

use super::abilities::has_ability;
use super::events::{self, Effect, EventCtx, Relay, Target};
use super::{actor, boosts};
use crate::battle::Battle;
use crate::event::{BattleEvent, ItemEndNote};
use crate::state::Position;

/// Which module implements an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// [`held`].
    Held,
    /// [`consumed`].
    Consumed,
    /// [`mega`]: Mega Stones and Z-Crystals.
    Mega,
    /// Not in Scarlet/Violet or Champions (Past items with effects).
    Past,
}

/// The Pokémon holding an item handler.
fn holder_of(h: &crate::resolve::handlers::Handler) -> Option<Position> {
    match h.holder {
        crate::resolve::handlers::Holder::Pokemon(p) => Some(p),
        _ => None,
    }
}

/// The module that implements `id`.
pub(crate) fn owner(id: ItemId) -> Owner {
    match id {
        // Held items that stay held.
        i::ABILITY_SHIELD
        | i::ADAMANT_CRYSTAL
        | i::ADAMANT_ORB
        | i::ASSAULT_VEST
        | i::BIG_ROOT
        | i::BLACK_BELT
        | i::BLACK_GLASSES
        | i::BLACK_SLUDGE
        | i::BRIGHT_POWDER
        | i::CHARCOAL
        | i::CHOICE_BAND
        | i::CHOICE_SCARF
        | i::CHOICE_SPECS
        | i::CLEAR_AMULET
        | i::CORNERSTONE_MASK
        | i::COVERT_CLOAK
        | i::DESTINY_KNOT
        | i::DRACO_PLATE
        | i::DRAGON_FANG
        | i::DREAD_PLATE
        | i::EARTH_PLATE
        | i::EVIOLITE
        | i::EXPERT_BELT
        | i::FAIRY_FEATHER
        | i::FIST_PLATE
        | i::FLAME_ORB
        | i::FLAME_PLATE
        | i::FLOAT_STONE
        | i::FOCUS_BAND
        | i::GRISEOUS_CORE
        | i::GRISEOUS_ORB
        | i::HARD_STONE
        | i::HEARTHFLAME_MASK
        | i::ICICLE_PLATE
        | i::INSECT_PLATE
        | i::IRON_BALL
        | i::IRON_PLATE
        | i::KINGS_ROCK
        | i::LEFTOVERS
        | i::LIFE_ORB
        | i::LIGHT_BALL
        | i::LOADED_DICE
        | i::LUSTROUS_GLOBE
        | i::LUSTROUS_ORB
        | i::MAGNET
        | i::MEADOW_PLATE
        | i::METAL_COAT
        | i::METRONOME
        | i::MIND_PLATE
        | i::MIRACLE_SEED
        | i::MUSCLE_BAND
        | i::MYSTIC_WATER
        | i::NEVER_MELT_ICE
        | i::PIXIE_PLATE
        | i::POISON_BARB
        | i::POWER_ANKLET
        | i::POWER_BAND
        | i::POWER_BELT
        | i::POWER_BRACER
        | i::POWER_LENS
        | i::POWER_WEIGHT
        | i::PUNCHING_GLOVE
        | i::QUICK_CLAW
        | i::RAZOR_CLAW
        | i::RAZOR_FANG
        | i::ROCKY_HELMET
        | i::RUSTED_SHIELD
        | i::RUSTED_SWORD
        | i::SAFETY_GOGGLES
        | i::SCOPE_LENS
        | i::SHARP_BEAK
        | i::SHED_SHELL
        | i::SHELL_BELL
        | i::SILK_SCARF
        | i::SILVER_POWDER
        | i::SKY_PLATE
        | i::SOFT_SAND
        | i::SOUL_DEW
        | i::SPELL_TAG
        | i::SPLASH_PLATE
        | i::SPOOKY_PLATE
        | i::STONE_PLATE
        | i::TOXIC_ORB
        | i::TOXIC_PLATE
        | i::TWISTED_SPOON
        | i::UTILITY_UMBRELLA
        | i::WELLSPRING_MASK
        | i::WIDE_LENS
        | i::WISE_GLASSES
        | i::ZAP_PLATE
        | i::ZOOM_LENS
        // Champions Reg M-C.
        | i::LEEK => Owner::Held,
        // Items checked at their call sites (Damp Rock's duration, ...).
        i::BINDING_BAND
        | i::DAMP_ROCK
        | i::GRIP_CLAW
        | i::HEAT_ROCK
        | i::HEAVY_DUTY_BOOTS
        | i::ICY_ROCK
        | i::LAGGING_TAIL
        | i::LIGHT_CLAY
        | i::PROTECTIVE_PADS
        | i::RING_TARGET
        | i::SMOOTH_ROCK
        | i::TERRAIN_EXTENDER => Owner::Held,
        // No battle effect (Poké Balls, evolution items, sweets, ...).
        i::BEAST_BALL
        | i::CHERISH_BALL
        | i::DIVE_BALL
        | i::DREAM_BALL
        | i::DUSK_BALL
        | i::FAST_BALL
        | i::FRIEND_BALL
        | i::GREAT_BALL
        | i::HEAL_BALL
        | i::HEAVY_BALL
        | i::LEVEL_BALL
        | i::LOVE_BALL
        | i::LURE_BALL
        | i::LUXURY_BALL
        | i::MASTER_BALL
        | i::MOON_BALL
        | i::NEST_BALL
        | i::NET_BALL
        | i::PARK_BALL
        | i::POKE_BALL
        | i::PREMIER_BALL
        | i::QUICK_BALL
        | i::REPEAT_BALL
        | i::SAFARI_BALL
        | i::SPORT_BALL
        | i::STRANGE_BALL
        | i::TIMER_BALL
        | i::ULTRA_BALL
        | i::AUSPICIOUS_ARMOR
        | i::BERRY_SWEET
        | i::BIG_NUGGET
        | i::BOTTLE_CAP
        | i::CHIPPED_POT
        | i::CLOVER_SWEET
        | i::CRACKED_POT
        | i::DAWN_STONE
        | i::DRAGON_SCALE
        | i::DUBIOUS_DISC
        | i::DUSK_STONE
        | i::ELECTIRIZER
        | i::FIRE_STONE
        | i::FLOWER_SWEET
        | i::GALARICA_CUFF
        | i::GALARICA_WREATH
        | i::GOLD_BOTTLE_CAP
        | i::ICE_STONE
        | i::LEAF_STONE
        | i::LOVE_SWEET
        | i::MAGMARIZER
        | i::MALICIOUS_ARMOR
        | i::MASTERPIECE_TEACUP
        | i::METAL_ALLOY
        | i::MOON_STONE
        | i::OVAL_STONE
        | i::PRETTY_FEATHER
        | i::PRISM_SCALE
        | i::PROTECTOR
        | i::RARE_BONE
        | i::REAPER_CLOTH
        | i::RIBBON_SWEET
        | i::SHINY_STONE
        | i::STAR_SWEET
        | i::STRAWBERRY_SWEET
        | i::SUN_STONE
        | i::SWEET_APPLE
        | i::SYRUPY_APPLE
        | i::TART_APPLE
        | i::THUNDER_STONE
        | i::UNREMARKABLE_TEACUP
        | i::UP_GRADE
        | i::WATER_STONE => Owner::Held,
        // Berries, consumed items, item and ability transfer.
        i::ABSORB_BULB
        | i::ADRENALINE_ORB
        | i::AGUAV_BERRY
        | i::AIR_BALLOON
        | i::APICOT_BERRY
        | i::ASPEAR_BERRY
        | i::BABIRI_BERRY
        | i::BLUNDER_POLICY
        | i::BOOSTER_ENERGY
        | i::CELL_BATTERY
        | i::CHARTI_BERRY
        | i::CHERI_BERRY
        | i::CHESTO_BERRY
        | i::CHILAN_BERRY
        | i::CHOPLE_BERRY
        | i::COBA_BERRY
        | i::COLBUR_BERRY
        | i::CUSTAP_BERRY
        | i::EJECT_BUTTON
        | i::EJECT_PACK
        | i::ELECTRIC_SEED
        | i::ENIGMA_BERRY
        | i::FIGY_BERRY
        | i::FOCUS_SASH
        | i::GANLON_BERRY
        | i::GRASSY_SEED
        | i::GREPA_BERRY
        | i::HABAN_BERRY
        | i::HONDEW_BERRY
        | i::IAPAPA_BERRY
        | i::JABOCA_BERRY
        | i::KASIB_BERRY
        | i::KEBIA_BERRY
        | i::KEE_BERRY
        | i::KELPSY_BERRY
        | i::LANSAT_BERRY
        | i::LEPPA_BERRY
        | i::LIECHI_BERRY
        | i::LUM_BERRY
        | i::LUMINOUS_MOSS
        | i::MAGO_BERRY
        | i::MARANGA_BERRY
        | i::MENTAL_HERB
        | i::MICLE_BERRY
        | i::MIRROR_HERB
        | i::MISTY_SEED
        | i::NORMAL_GEM
        | i::OCCA_BERRY
        | i::ORAN_BERRY
        | i::PASSHO_BERRY
        | i::PAYAPA_BERRY
        | i::PECHA_BERRY
        | i::PERSIM_BERRY
        | i::PETAYA_BERRY
        | i::POMEG_BERRY
        | i::POWER_HERB
        | i::PSYCHIC_SEED
        | i::QUALOT_BERRY
        | i::RAWST_BERRY
        | i::RED_CARD
        | i::RINDO_BERRY
        | i::ROOM_SERVICE
        | i::ROSELI_BERRY
        | i::ROWAP_BERRY
        | i::SALAC_BERRY
        | i::SHUCA_BERRY
        | i::SITRUS_BERRY
        | i::SNOWBALL
        | i::STARF_BERRY
        | i::STICKY_BARB
        | i::TAMATO_BERRY
        | i::TANGA_BERRY
        | i::THROAT_SPRAY
        | i::WACAN_BERRY
        | i::WEAKNESS_POLICY
        | i::WHITE_HERB
        | i::WIKI_BERRY
        | i::YACHE_BERRY => Owner::Consumed,
        // Mega Stones and Z-Crystals.
        i::ABOMASITE
        | i::ABSOLITE
        | i::ABSOLITE_Z
        | i::AERODACTYLITE
        | i::AGGRONITE
        | i::ALAKAZITE
        | i::ALTARIANITE
        | i::AMPHAROSITE
        | i::AUDINITE
        | i::BANETTITE
        | i::BARBARACITE
        | i::BAXCALIBRITE
        | i::BEEDRILLITE
        | i::BLASTOISINITE
        | i::BLAZIKENITE
        | i::CAMERUPTITE
        | i::CHANDELURITE
        | i::CHARIZARDITE_X
        | i::CHARIZARDITE_Y
        | i::CHESNAUGHTITE
        | i::CHIMECHITE
        | i::CLEFABLITE
        | i::CRABOMINITE
        | i::DARKRANITE
        | i::DELPHOXITE
        | i::DIANCITE
        | i::DRAGALGITE
        | i::DRAGONINITE
        | i::DRAMPANITE
        | i::EELEKTROSSITE
        | i::EMBOARITE
        | i::EXCADRITE
        | i::FALINKSITE
        | i::FERALIGITE
        | i::FLOETTITE
        | i::FROSLASSITE
        | i::GALLADITE
        | i::GARCHOMPITE
        | i::GARCHOMPITE_Z
        | i::GARDEVOIRITE
        | i::GENGARITE
        | i::GLALITITE
        | i::GLIMMORANITE
        | i::GOLISOPITE
        | i::GOLURKITE
        | i::GRENINJITE
        | i::GYARADOSITE
        | i::HAWLUCHANITE
        | i::HEATRANITE
        | i::HERACRONITE
        | i::HOUNDOOMINITE
        | i::KANGASKHANITE
        | i::LATIASITE
        | i::LATIOSITE
        | i::LOPUNNITE
        | i::LUCARIONITE
        | i::LUCARIONITE_Z
        | i::MAGEARNITE
        | i::MALAMARITE
        | i::MANECTITE
        | i::MAWILITE
        | i::MEDICHAMITE
        | i::MEGANIUMITE
        | i::MEOWSTICITE
        | i::METAGROSSITE
        | i::MEWTWONITE_X
        | i::MEWTWONITE_Y
        | i::PIDGEOTITE
        | i::PINSIRITE
        | i::PYROARITE
        | i::RAICHUNITE_X
        | i::RAICHUNITE_Y
        | i::SABLENITE
        | i::SALAMENCITE
        | i::SCEPTILITE
        | i::SCIZORITE
        | i::SCOLIPITE
        | i::SCOVILLAINITE
        | i::SCRAFTINITE
        | i::SHARPEDONITE
        | i::SKARMORITE
        | i::SLOWBRONITE
        | i::STARAPTITE
        | i::STARMINITE
        | i::STEELIXITE
        | i::SWAMPERTITE
        | i::TATSUGIRINITE
        | i::TYRANITARITE
        | i::VENUSAURITE
        | i::VICTREEBELITE
        | i::ZERAORITE
        | i::ZYGARDITE
        | i::ALORAICHIUM_Z
        | i::BUGINIUM_Z
        | i::DARKINIUM_Z
        | i::DECIDIUM_Z
        | i::DRAGONIUM_Z
        | i::EEVIUM_Z
        | i::ELECTRIUM_Z
        | i::FAIRIUM_Z
        | i::FIGHTINIUM_Z
        | i::FIRIUM_Z
        | i::FLYINIUM_Z
        | i::GHOSTIUM_Z
        | i::GRASSIUM_Z
        | i::GROUNDIUM_Z
        | i::ICIUM_Z
        | i::INCINIUM_Z
        | i::KOMMONIUM_Z
        | i::LUNALIUM_Z
        | i::LYCANIUM_Z
        | i::MARSHADIUM_Z
        | i::MEWNIUM_Z
        | i::MIMIKIUM_Z
        | i::NORMALIUM_Z
        | i::PIKANIUM_Z
        | i::PIKASHUNIUM_Z
        | i::POISONIUM_Z
        | i::PRIMARIUM_Z
        | i::PSYCHIUM_Z
        | i::ROCKIUM_Z
        | i::SNORLIUM_Z
        | i::SOLGANIUM_Z
        | i::STEELIUM_Z
        | i::TAPUNIUM_Z
        | i::ULTRANECROZIUM_Z
        | i::WATERIUM_Z => Owner::Mega,
        _ => Owner::Past,
    }
}

/// Runs `item`'s callback for `ev.event` / `ev.prefix`. `holder` holds the
/// item (Showdown's `effectState.target`). Returns the callback's result
/// (`None`: `undefined`).
pub(crate) fn run(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position, item: ItemId) -> Option<Relay> {
    match owner(item) {
        Owner::Held => held::run(battle, ev, holder, item),
        Owner::Consumed => consumed::run(battle, ev, holder, item),
        Owner::Mega => mega::run(battle, ev, holder, item),
        Owner::Past => None,
    }
}

/// Whether `item`'s `onUpdate` on its holder at `pos` would do nothing
/// now, whatever runs it (`false` when not known). It only reads the
/// battle: `eachEvent('Update')` skips a Pokémon whose only `Update`
/// handler is such an item's ([`events::each_event_run`]).
pub(crate) fn update_idle(battle: &Battle, pos: Position, item: ItemId) -> bool {
    match owner(item) {
        Owner::Held => held::update_idle(battle, pos, item),
        Owner::Consumed => consumed::update_idle(battle, pos, item),
        Owner::Mega | Owner::Past => false,
    }
}

// ---------------------------------------------------------------------------
// Suppression.

/// Showdown's `pokemon.ignoringItem()`: the item of the Pokémon at `pos`
/// does nothing (it is not active, Magic Room, Embargo, Klutz). Primal Orbs
/// always work; `ignoreKlutz` items work under Klutz.
pub(crate) fn ignoring_item(battle: &Battle, pos: Position) -> bool {
    ignoring(battle, pos, false)
}

/// `ignoringItem(true)` for Fling: Klutz stops it even for `ignoreKlutz`
/// items.
pub(crate) fn ignoring_item_for_fling(battle: &Battle, pos: Position) -> bool {
    ignoring(battle, pos, true)
}

fn ignoring(battle: &Battle, pos: Position, fling: bool) -> bool {
    let Some(mon) = battle.state.side(pos.side).team.get(pos.slot as usize) else { return true };
    let data = mon.item.map(|it| battle.format.dex.item(it));
    if data.is_some_and(|d| d.is_primal_orb) {
        return false;
    }
    // Revived in its slot by Revival Blessing, not switched back in yet:
    // Showdown's `isActive` is false.
    if !events::is_active(battle, pos) || super::slots::off_field(battle, pos) {
        return true;
    }
    // (Embargo would ignore it too: a Past move in Gen 9 and Champions.)
    if battle.state.field.magic_room_turns > 0 {
        return true;
    }
    if fling {
        return has_ability(battle, pos, abilities::KLUTZ);
    }
    !data.is_some_and(|d| d.ignore_klutz) && has_ability(battle, pos, abilities::KLUTZ)
}

/// Showdown's `pokemon.hasItem(id)`: it holds the item and it is not
/// ignored.
#[inline]
pub(crate) fn has_item(battle: &Battle, pos: Position, id: ItemId) -> bool {
    battle.state.side(pos.side).team.get(pos.slot as usize).is_some_and(|m| m.item == Some(id))
        && !ignoring_item(battle, pos)
}

/// The held item of the Pokémon at `pos`, unless it is ignored.
pub(crate) fn effective_item(battle: &Battle, pos: Position) -> Option<ItemId> {
    let item = battle.state.side(pos.side).team.get(pos.slot as usize)?.item?;
    (!ignoring_item(battle, pos)).then_some(item)
}

// ---------------------------------------------------------------------------
// Lifecycle.

/// A new `itemState` for the Pokémon at `pos` (switching in, `setItem`):
/// the next `effectOrder` if it holds an item (Showdown's
/// `initEffectState` numbers only states with an ID), and fresh item data.
pub(crate) fn new_item_state(battle: &mut Battle, pos: Position) {
    let holds = battle.state.side(pos.side).team.get(pos.slot as usize).is_some_and(|m| m.item.is_some());
    let order = if holds {
        battle.order.effect_order += 1;
        battle.order.effect_order
    } else {
        0
    };
    if let Some(mon) = battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) {
        mon.volatile_counters.item_order = order;
        mon.volatile_counters.item_state = Default::default();
    }
}

/// Showdown's `clearEffectState(itemState)`: the item is gone.
fn clear_item_state(battle: &mut Battle, pos: Position) {
    if let Some(mon) = battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) {
        mon.volatile_counters.item_order = 0;
        mon.volatile_counters.item_state = Default::default();
    }
}

/// Showdown's `pokemon.setItem(item, source, effect)`: the Pokémon holds
/// `item` (or nothing) with a new `itemState`; the old item's `End`, then
/// the new one's `Start`, run. Fails (false) without HP or off the field.
pub(crate) fn set_item(
    battle: &mut Battle,
    pos: Position,
    item: Option<ItemId>,
    source: Option<Position>,
    effect: Effect,
) -> bool {
    let Some(mon) = battle.state.side(pos.side).team.get(pos.slot as usize) else { return false };
    if mon.hp == 0 || !events::is_active(battle, pos) {
        return false;
    }
    let old = mon.item;
    super::endless::item_set(battle, pos);
    battle.state.side_mut(pos.side).team[pos.slot as usize].item = item;
    events::note_held(battle, None, item);
    new_item_state(battle, pos);
    if let Some(old) = old {
        events::single_event_item(battle, Event::End, pos, old, None, Effect::None, Relay::True);
    }
    if let Some(item) = item {
        events::single_event_item(battle, Event::Start, pos, item, source, effect, Relay::True);
    }
    true
}

/// Showdown's `pokemon.takeItem(source)`: the item is removed unless a
/// `TakeItem` handler stops it (Sticky Hold, a Mega Stone on its user,
/// ...). Returns the item taken. `source` defaults to the holder.
pub(crate) fn take_item(battle: &mut Battle, pos: Position, source: Option<Position>) -> Option<ItemId> {
    let item = battle.state.side(pos.side).team.get(pos.slot as usize)?.item?;
    let source = source.unwrap_or(pos);
    let mut ev = EventCtx::new(Event::TakeItem, Target::Pokemon(pos), Some(source), Effect::None, Relay::Item(item));
    if !events::run_event(battle, &mut ev, None).is_truthy() {
        return None;
    }
    battle.state.side_mut(pos.side).team[pos.slot as usize].item = None;
    clear_item_state(battle, pos);
    super::endless::item_taken(battle, pos);
    events::single_event_item(battle, Event::End, pos, item, None, Effect::None, Relay::True);
    Some(item)
}

/// Showdown's `pokemon.useItem(source, sourceEffect)`: a consumed item that
/// is not eaten (Focus Sash, Weakness Policy, a Gem, ...). `-enditem`,
/// its `boosts`, its `Use` callback, then it is gone (`lastItem`,
/// `usedItemThisTurn`) and `AfterUseItem` runs. `source` defaults to the
/// holder; `effect` is what makes it happen (the item itself when it uses
/// itself). Returns whether it was used.
pub(crate) fn use_item(battle: &mut Battle, pos: Position, source: Option<Position>, effect: Effect) -> bool {
    let Some(mon) = battle.state.side(pos.side).team.get(pos.slot as usize) else { return false };
    let Some(item) = mon.item else { return false };
    let data = battle.format.dex.item(item);
    if (mon.hp == 0 && !data.is_gem) || !events::is_active(battle, pos) {
        return false;
    }
    let source = source.unwrap_or(pos);
    // Another item cannot use this one on its holder's behalf.
    if matches!(effect, Effect::Item(e) if e != item) && source == pos {
        return false;
    }
    let mut ev = EventCtx::new(Event::UseItem, Target::Pokemon(pos), None, Effect::None, Relay::Item(item));
    if !events::run_event(battle, &mut ev, None).is_truthy() {
        return false;
    }
    // `-enditem`: Red Card names its target, a Gem says `[from] gem|[move]
    // <activeMove.name>` (`this.battle.activeMove!`: a Gem is used only by
    // the move it powers).
    let holder = actor(battle, pos);
    let note = match battle.active_move {
        Some((_, m)) if item != i::RED_CARD && data.is_gem => ItemEndNote::GemMove(m),
        _ => ItemEndNote::None,
    };
    let of = (item == i::RED_CARD).then(|| actor(battle, source));
    battle.emit(BattleEvent::ItemEnd { actor: holder, item, note, from: None, of });
    if let Some(b) = data.boosts {
        boosts::boost_from(battle, pos, b, Some(source), Effect::Item(item), false, false);
    }
    events::single_event_item(battle, Event::Use, pos, item, Some(source), effect, Relay::True);
    finish_consumed(battle, pos, item, false);
    true
}

/// Showdown's `pokemon.eatItem(force, source, sourceEffect)`: the held
/// berry is eaten unless `UseItem` or `TryEatItem` stops it (`force` skips
/// `TryEatItem`: Stuff Cheeks, Teatime). `-enditem|...|[eat]`, its `Eat`
/// callback, `EatItem`, then it is gone (`lastItem`, `usedItemThisTurn`,
/// `ateBerry`) and `AfterUseItem` runs. Returns whether it was eaten.
pub(crate) fn eat_item(
    battle: &mut Battle,
    pos: Position,
    force: bool,
    source: Option<Position>,
    effect: Effect,
) -> bool {
    let Some(mon) = battle.state.side(pos.side).team.get(pos.slot as usize) else { return false };
    let Some(item) = mon.item else { return false };
    // Jaboca and Rowap Berry work after their holder fainted.
    if (mon.hp == 0 && item != i::JABOCA_BERRY && item != i::ROWAP_BERRY) || !events::is_active(battle, pos) {
        return false;
    }
    let source = source.unwrap_or(pos);
    if matches!(effect, Effect::Item(e) if e != item) && source == pos {
        return false;
    }
    let mut ev = EventCtx::new(Event::UseItem, Target::Pokemon(pos), None, Effect::None, Relay::Item(item));
    if !events::run_event(battle, &mut ev, None).is_truthy() {
        return false;
    }
    if !force {
        let mut ev = EventCtx::new(Event::TryEatItem, Target::Pokemon(pos), None, Effect::None, Relay::Item(item));
        if !events::run_event(battle, &mut ev, None).is_truthy() {
            return false;
        }
    }
    let holder = actor(battle, pos);
    battle.emit(BattleEvent::ItemEnd { actor: holder, item, note: ItemEndNote::Eat, from: None, of: None });
    events::single_event_item(battle, Event::Eat, pos, item, Some(source), effect, Relay::True);
    let mut ev = EventCtx::new(Event::EatItem, Target::Pokemon(pos), Some(source), effect, Relay::Item(item));
    events::run_event(battle, &mut ev, None);
    super::endless::item_eaten(battle, pos, item);
    finish_consumed(battle, pos, item, true);
    true
}

/// The end of `useItem` / `eatItem`: the item is gone, remembered as
/// `lastItem`, and `AfterUseItem` runs (Unburden's hook).
fn finish_consumed(battle: &mut Battle, pos: Position, item: ItemId, eaten: bool) {
    let mon = &mut battle.state.side_mut(pos.side).team[pos.slot as usize];
    mon.last_item = Some(item);
    mon.item = None;
    mon.used_item_this_turn = true;
    if eaten {
        mon.ate_berry = true;
    }
    clear_item_state(battle, pos);
    let mut ev = EventCtx::new(Event::AfterUseItem, Target::Pokemon(pos), None, Effect::None, Relay::Item(item));
    events::run_event(battle, &mut ev, None);
}
