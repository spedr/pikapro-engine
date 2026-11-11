//! A move's effective type and the type chart's verdict, from explicit
//! facts: the rules the engine runs (it calls these functions), for readers
//! that know only part of a battle (a player's view, an encoder).
//!
//! Every function is pure, integer-only and allocation-free. A fact left
//! `None` in an `Option` field is *unknown*: a function returns "unknown"
//! only when the rule it runs reads that fact (a Judgment with an unknown
//! item has an unknown type; a Tackle doesn't care). The engine knows
//! everything and fills every field.
//!
//! * [`own_move_type`]: the move's own `onModifyType` (Aura Wheel, Ivy
//!   Cudgel, Judgment, Raging Bull, Revelation Dance, Tera Blast, Tera
//!   Starstorm, Terrain Pulse, Weather Ball).
//! * [`ability_move_type`]: the user's `ModifyType` abilities (the -ate
//!   abilities, Normalize, Liquid Voice).
//! * [`electrify_type`]: Electrify's `ModifyType`, which runs last.
//! * [`move_type`]: the three in Showdown's order.
//! * [`type_mod`] and [`effectiveness`]: `runEffectiveness` with the move's
//!   own `onEffectiveness` (Freeze-Dry, Flying Press) and the Stellar rule,
//!   and the chart's immunities.
//! * [`move_weather`] and [`ground`]: the two facts about the user those
//!   rules read that come from several others (`effectiveWeather`,
//!   `isGrounded`).
//!
//! Not modelled, because no Gen 9 or Champions format can use them (they
//! are `Past`): Thousand Arrows' `onEffectiveness`, Multi-Attack, Techno
//! Blast, Natural Gift and Hidden Power. A pledge combination's type (Fire,
//! Grass or Water, from an ally's pledge the same turn) is transient and
//! stays the engine's (`resolve::pledges`).
//!
//! [`effectiveness`] is the type chart's verdict for the move's type
//! against the defender's types, nothing more: what the defender's side
//! adds (Tar Shot, Delta Stream, Tera Shell, an ability that grants an
//! immunity such as Levitate or Flash Fire, a grounded Flying type) and
//! what the user's ability ignores (Scrappy, Mind's Eye) aren't part of it.
//!
//! # Panics
//!
//! The functions that take a [`Dex`] index it with the IDs they are given
//! (a species, a move, a held item): an ID outside that dex (one from
//! another dex, or made up) panics, as the dex's own accessors do. IDs
//! from the dex itself, its constants or a battle on it are always in
//! range.

use pikapro_data::gen9::{abilities as a, items as i, moves as mv, species as s};
use pikapro_data::{AbilityId, Dex, Effectiveness, ItemId, MoveFlags, MoveId, SpeciesId, Type};

use crate::state::{Terrain, Weather};

/// What a fact the rule reads isn't known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Unknown;

impl core::fmt::Display for Unknown {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("a fact the rule reads isn't known")
    }
}

impl std::error::Error for Unknown {}

/// What is known of a Pokémon's effective ability (Showdown's
/// `ignoringAbility` applied: a suppressed ability is `Known(None)`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AbilityFact {
    /// This ability, or none (suppressed).
    Known(Option<AbilityId>),
    /// One of these (the species' ability slots; `None` entries are empty
    /// slots). A rule is known when every candidate gives the same result.
    OneOf([Option<AbilityId>; 4]),
    /// Anything.
    #[default]
    Unknown,
}

/// A Pokémon using a move, as far as a reader knows it (see the module
/// documentation: `None` in an `Option` field is unknown). Build it with
/// [`MoveUser::new`] and set what is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct MoveUser {
    /// Its species now: the one it transformed into, if it did
    /// (`pokemon.species`).
    pub species: SpeciesId,
    /// `getTypes()`: its two types (Terastallization and type changes
    /// applied) and the added one.
    pub types: [Option<Type>; 3],
    /// The type it Terastallized into; `None` if it hasn't.
    pub tera: Option<Type>,
    /// Its effective item (`Some(None)`: no item, or one it ignores).
    pub item: Option<Option<ItemId>>,
    /// Its effective ability.
    pub ability: AbilityFact,
    /// `isGrounded()` (Terrain Pulse).
    pub grounded: Option<bool>,
    /// It is Electrified.
    pub electrified: bool,
    /// Its effective weather for the move ([`move_weather`]).
    pub weather: Option<Option<Weather>>,
    /// The terrain.
    pub terrain: Option<Terrain>,
}

impl MoveUser {
    /// A user of `species` with the species' types, nothing else known
    /// (for readers; fill in what is known).
    ///
    /// # Panics
    /// If `species` is outside `dex`.
    pub fn new(dex: &Dex, species: SpeciesId) -> MoveUser {
        let t = dex.species(species).types;
        MoveUser {
            species,
            types: [Some(t.primary), t.secondary, None],
            tera: None,
            item: None,
            ability: AbilityFact::Unknown,
            grounded: None,
            electrified: false,
            weather: None,
            terrain: None,
        }
    }
}

/// Whether `move_id` has its own `onModifyType` ([`own_move_type`]).
pub const fn has_own_type(move_id: MoveId) -> bool {
    matches!(
        move_id,
        mv::AURA_WHEEL
            | mv::IVY_CUDGEL
            | mv::JUDGMENT
            | mv::RAGING_BULL
            | mv::REVELATION_DANCE
            | mv::TERA_BLAST
            | mv::TERA_STARSTORM
            | mv::TERRAIN_PULSE
            | mv::WEATHER_BALL
    )
}

/// The move's own `onModifyType` (`data/moves.ts`): `Ok(Some(t))` it makes
/// the move of type `t`, `Ok(None)` the move keeps its type, `Err` it
/// depends on a fact that isn't known.
///
/// # Panics
/// For Judgment, if the user's known item is outside `dex`.
pub fn own_move_type(dex: &Dex, move_id: MoveId, user: &MoveUser) -> Result<Option<Type>, Unknown> {
    Ok(match move_id {
        // `aurawheel.onModifyType`: `Morpeko-Hangry` makes it Dark.
        mv::AURA_WHEEL => Some(if user.species == s::MORPEKO_HANGRY { Type::Dark } else { Type::Electric }),
        // `ivycudgel.onModifyType`: by Ogerpon's mask (`species.name`).
        mv::IVY_CUDGEL => match user.species {
            s::OGERPON_WELLSPRING | s::OGERPON_WELLSPRING_TERA => Some(Type::Water),
            s::OGERPON_HEARTHFLAME | s::OGERPON_HEARTHFLAME_TERA => Some(Type::Fire),
            s::OGERPON_CORNERSTONE | s::OGERPON_CORNERSTONE_TERA => Some(Type::Rock),
            _ => None,
        },
        // `ragingbull.onModifyType`: by the Paldean Tauros breed.
        mv::RAGING_BULL => match user.species {
            s::TAUROS_PALDEA_COMBAT => Some(Type::Fighting),
            s::TAUROS_PALDEA_BLAZE => Some(Type::Fire),
            s::TAUROS_PALDEA_AQUA => Some(Type::Water),
            _ => None,
        },
        // `judgment.onModifyType`: the held plate's type (`item.onPlate`,
        // not a Z-Crystal's), unless the item is ignored.
        mv::JUDGMENT => user.item.ok_or(Unknown)?.and_then(|item| {
            let data = dex.item(item);
            if data.is_z_crystal { None } else { data.plate }
        }),
        // `revelationdance.onModifyType`: `getTypes()[0]` (the Tera type
        // once Terastallized); a `???` (Burn Up, Double Shock) gives way to
        // the next type, or stays.
        mv::REVELATION_DANCE => match user.types {
            [Some(Type::Unknown), second, added] => Some(second.or(added).unwrap_or(Type::Unknown)),
            [first, ..] => first,
        },
        // `terablast.onModifyType`: the Tera type once Terastallized.
        mv::TERA_BLAST => user.tera,
        // `terastarstorm.onModifyType`: Stellar for Terapagos-Stellar.
        mv::TERA_STARSTORM => (user.species == s::TERAPAGOS_STELLAR).then_some(Type::Stellar),
        // `terrainpulse.onModifyType`: the terrain's type, if the user is
        // grounded.
        mv::TERRAIN_PULSE => match user.terrain {
            None => None,
            Some(t) => {
                if !user.grounded.ok_or(Unknown)? {
                    return Ok(None);
                }
                Some(match t {
                    Terrain::Electric => Type::Electric,
                    Terrain::Grassy => Type::Grass,
                    Terrain::Misty => Type::Fairy,
                    Terrain::Psychic => Type::Psychic,
                })
            }
        },
        // `weatherball.onModifyType`: `pokemon.effectiveWeather()`.
        mv::WEATHER_BALL => match user.weather.ok_or(Unknown)? {
            Some(Weather::Sun | Weather::HarshSun) => Some(Type::Fire),
            Some(Weather::Rain | Weather::HeavyRain) => Some(Type::Water),
            Some(Weather::Sand) => Some(Type::Rock),
            Some(Weather::Snow) => Some(Type::Ice),
            _ => None,
        },
        _ => None,
    })
}

/// Moves whose own type the -ate abilities and Normalize leave alone
/// (their `noModifyType` lists, without the `Past` moves).
const NO_MODIFY_TYPE: [MoveId; 4] = [mv::JUDGMENT, mv::REVELATION_DANCE, mv::TERRAIN_PULSE, mv::WEATHER_BALL];

/// The abilities with a `ModifyType` handler on the user's move.
const TYPE_ABILITIES: [AbilityId; 7] =
    [a::AERILATE, a::DRAGONIZE, a::GALVANIZE, a::PIXILATE, a::REFRIGERATE, a::NORMALIZE, a::LIQUID_VOICE];

/// One ability's `onModifyType` on a move of type `ty` (so far): the type
/// it sets, if any.
///
/// * The -ate abilities (`aerilate`, `dragonize`, `galvanize`, `pixilate`,
///   `refrigerate`, priority -1): a Normal move becomes their type, but
///   the `noModifyType` moves and a Terastallized user's Tera Blast.
///   Struggle isn't in their lists: in Showdown it is `???` by then
///   (`struggle.onModifyMove`), which isn't Normal. The engine keeps its
///   data type, Normal, and treats it as typeless, so it is excluded here.
/// * `normalize` (priority 1): every move becomes Normal, but the same and
///   Struggle (in its list).
/// * `liquidvoice` (priority -1): a sound move becomes Water.
///
/// # Panics
/// For Liquid Voice, if `move_id` is outside `dex`.
pub fn ability_type(ability: AbilityId, dex: &Dex, move_id: MoveId, ty: Type, terastallized: bool) -> Option<Type> {
    // Only asked of the abilities below (most calls are for others).
    let excluded =
        || move_id == mv::STRUGGLE || NO_MODIFY_TYPE.contains(&move_id) || move_id == mv::TERA_BLAST && terastallized;
    match ability {
        a::AERILATE | a::DRAGONIZE | a::GALVANIZE | a::PIXILATE | a::REFRIGERATE
            if ty == Type::Normal && !excluded() =>
        {
            Some(match ability {
                a::AERILATE => Type::Flying,
                a::DRAGONIZE => Type::Dragon,
                a::GALVANIZE => Type::Electric,
                a::PIXILATE => Type::Fairy,
                _ => Type::Ice,
            })
        }
        a::NORMALIZE if !excluded() => Some(Type::Normal),
        a::LIQUID_VOICE if dex.move_(move_id).flags.contains(MoveFlags::SOUND) => Some(Type::Water),
        _ => None,
    }
}

/// The user's `ModifyType` ability ([`ability_type`]) on a move of type
/// `ty`, as far as its ability is known: `Ok(Some(t))` it sets `t`,
/// `Ok(None)` it changes nothing, `Err` the possible abilities disagree
/// ([`AbilityFact::Unknown`] counts every such ability as possible).
///
/// # Panics
/// If Liquid Voice may be the user's ability and `move_id` is outside
/// `dex` ([`ability_type`]).
pub fn ability_move_type(dex: &Dex, move_id: MoveId, ty: Type, user: &MoveUser) -> Result<Option<Type>, Unknown> {
    let tera = user.tera.is_some();
    let rule = |ability: Option<AbilityId>| ability.and_then(|ab| ability_type(ab, dex, move_id, ty, tera));
    let agree = |mut candidates: core::slice::Iter<'_, Option<AbilityId>>| {
        let first = rule(*candidates.next()?);
        candidates.all(|&c| rule(c) == first).then_some(first)
    };
    match user.ability {
        AbilityFact::Known(ability) => Ok(rule(ability)),
        AbilityFact::OneOf(slots) => {
            let mut candidates = [None; 4];
            let mut n = 0;
            for ab in slots.into_iter().flatten() {
                candidates[n] = Some(ab);
                n += 1;
            }
            if n == 0 {
                return Err(Unknown);
            }
            agree(candidates[..n].iter()).ok_or(Unknown)
        }
        AbilityFact::Unknown => {
            // No ability, or any of the ones that could change the type.
            let mut candidates = [None; TYPE_ABILITIES.len() + 1];
            for (c, ab) in candidates[1..].iter_mut().zip(TYPE_ABILITIES) {
                *c = Some(ab);
            }
            agree(candidates.iter()).ok_or(Unknown)
        }
    }
}

/// Whether the ability `ability` may change a move's type
/// ([`ability_move_type`]): `false` means it leaves every move's type as
/// it is, so a move without a type rule of its own ([`has_own_type`]) used
/// by a user that isn't Electrified has its data's type ([`move_type`];
/// Struggle, typeless, aside).
pub fn may_modify_type(ability: &AbilityFact) -> bool {
    let changes = |ab: &Option<AbilityId>| ab.is_some_and(|ab| TYPE_ABILITIES.contains(&ab));
    match ability {
        AbilityFact::Known(ab) => changes(ab),
        // No candidate at all is unknown ([`ability_move_type`]'s `Err`).
        AbilityFact::OneOf(slots) => slots.iter().all(Option::is_none) || slots.iter().any(changes),
        AbilityFact::Unknown => true,
    }
}

/// Electrify's `onModifyType` (priority -2, after the abilities): every
/// move but Struggle becomes Electric.
pub const fn electrify_type(move_id: MoveId) -> Option<Type> {
    if move_id.0 == mv::STRUGGLE.0 { None } else { Some(Type::Electric) }
}

/// The type `move_id` has when `user` uses it: its own `onModifyType`, then
/// `runEvent('ModifyType')` (the user's ability, then Electrify), as
/// `useMoveInner` runs them. `None` when a fact it depends on isn't known.
///
/// Struggle is typeless (`Type::Unknown`, Showdown's `???`): its
/// `onModifyMove` sets `???` between the move's own `ModifyType` and
/// `runEvent('ModifyType')`, where no -ate ability (Normal moves only),
/// Normalize or Electrify (both skip Struggle) changes it.
///
/// # Panics
/// If `move_id` is outside `dex`, or for Judgment if the user's known item
/// is ([`own_move_type`]).
pub fn move_type(dex: &Dex, move_id: MoveId, user: &MoveUser) -> Option<Type> {
    if move_id == mv::STRUGGLE {
        return Some(Type::Unknown);
    }
    if user.electrified
        && let Some(t) = electrify_type(move_id)
    {
        return Some(t);
    }
    let base = dex.move_(move_id).ty;
    let ty = own_move_type(dex, move_id, user).ok()?.unwrap_or(base);
    Some(ability_move_type(dex, move_id, ty, user).ok()?.unwrap_or(ty))
}

/// `getEffectiveness` of a move of type `move_ty` against one defending
/// type, with the move's own `onEffectiveness` (`freezedry`: 1 against
/// Water; `flyingpress`: its Fighting's plus Flying's), as
/// `runEffectiveness` sums it: the chart's exponent, 0 for a type immune to
/// it (immunity is [`effectiveness`]'s, `runImmunity`).
pub fn type_mod(move_id: MoveId, move_ty: Type, defend: Type) -> i8 {
    let exp = |attack: Type| match attack.against(defend) {
        Effectiveness::Exp(n) => n,
        Effectiveness::Immune => 0,
    };
    match move_id {
        mv::FREEZE_DRY if defend == Type::Water => 1,
        mv::FLYING_PRESS => exp(move_ty) + exp(Type::Flying),
        _ => exp(move_ty),
    }
}

/// `runEffectiveness`'s Stellar rule: a Stellar move against a
/// Terastallized target is super effective (`totalTypeMod = 1`), without
/// the per-type sum.
pub const fn stellar_effective(move_ty: Type, terastallized: bool) -> bool {
    terastallized && matches!(move_ty, Type::Stellar)
}

/// The type chart's verdict for `move_id` of type `move_ty` against a
/// defender whose `getTypes()` is `defend` (two types and the added one):
/// [`Effectiveness::Immune`] if one of them is immune to the move's type
/// (`runImmunity`, by the chart), else the exponent `runEffectiveness`
/// sums ([`type_mod`], [`stellar_effective`]): `Exp(-2)` for a quarter,
/// `Exp(2)` for four times. Every fact it reads is given: the verdict is
/// always known.
pub fn effectiveness(move_id: MoveId, move_ty: Type, defend: [Option<Type>; 3], terastallized: bool) -> Effectiveness {
    if !has_own_effectiveness(move_id) {
        return chart_effectiveness(move_ty, defend, terastallized);
    }
    if stellar_effective(move_ty, terastallized) {
        return Effectiveness::Exp(1);
    }
    let types = defend.into_iter().flatten();
    if types.clone().any(|t| move_ty.against(t) == Effectiveness::Immune) {
        return Effectiveness::Immune;
    }
    Effectiveness::Exp(types.map(|t| type_mod(move_id, move_ty, t)).sum())
}

/// Whether `move_id` has an `onEffectiveness` of its own ([`type_mod`]):
/// Freeze-Dry, Flying Press. Any other move's verdict is
/// [`chart_effectiveness`]'s, which depends only on its type.
pub const fn has_own_effectiveness(move_id: MoveId) -> bool {
    matches!(move_id, mv::FREEZE_DRY | mv::FLYING_PRESS)
}

/// [`effectiveness`] for a move without an `onEffectiveness` of its own
/// ([`has_own_effectiveness`]): the chart's verdict for type `move_ty`
/// (a reader can work it out once per type and defender).
pub fn chart_effectiveness(move_ty: Type, defend: [Option<Type>; 3], terastallized: bool) -> Effectiveness {
    if stellar_effective(move_ty, terastallized) {
        return Effectiveness::Exp(1);
    }
    let mut total = 0;
    for t in defend.into_iter().flatten() {
        match move_ty.against(t) {
            Effectiveness::Immune => return Effectiveness::Immune,
            Effectiveness::Exp(n) => total += n,
        }
    }
    Effectiveness::Exp(total)
}

/// Utility Umbrella's part of `pokemon.effectiveWeather()`: sun and rain
/// don't reach its holder.
pub const fn umbrella_hides(weather: Weather) -> bool {
    matches!(weather, Weather::Sun | Weather::Rain | Weather::HarshSun | Weather::HeavyRain)
}

/// `pokemon.effectiveWeather()` for the user of a move (Weather Ball):
/// `weather` is the field's effective weather (`field.effectiveWeather()`:
/// none while an active Air Lock or Cloud Nine works), `item` the user's
/// effective item, `mega_sol` whether it has Mega Sol (which makes it sun
/// for its moves). `None` when an unknown fact decides it.
pub fn move_weather(
    weather: Option<Weather>,
    item: Option<Option<ItemId>>,
    mega_sol: Option<bool>,
) -> Option<Option<Weather>> {
    let without = match weather {
        Some(w) if umbrella_hides(w) => match item {
            None => return mega_sol.and_then(|m| m.then_some(Some(Weather::Sun))),
            Some(Some(i::UTILITY_UMBRELLA)) => None,
            Some(_) => Some(w),
        },
        w => w,
    };
    match mega_sol {
        Some(true) => Some(Some(Weather::Sun)),
        Some(false) => Some(without),
        None if without == Some(Weather::Sun) => Some(without),
        None => None,
    }
}

/// Where `isGrounded` puts a Pokémon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ground {
    /// On the ground: Ground moves and terrains reach it.
    Grounded,
    /// A Flying type, Magnet Rise, Air Balloon.
    Airborne,
    /// Levitate (which `runImmunity` announces).
    Levitating,
}

/// What `isGrounded` reads (`None`: unknown).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GroundFacts {
    /// Gravity is up.
    pub gravity: bool,
    /// It has Ingrain up.
    pub ingrain: bool,
    /// It was hit by Smack Down or Thousand Arrows.
    pub smack_down: bool,
    /// It has Magnet Rise up.
    pub magnet_rise: bool,
    /// It has the Flying type (`hasType`; not while roosting), unless the
    /// question negates immunity.
    pub flying: bool,
    /// Its effective item.
    pub item: Option<Option<ItemId>>,
    /// Its Levitate works (the ability, not suppressed).
    pub levitate: Option<bool>,
}

/// Showdown's `pokemon.isGrounded()`: Gravity, Ingrain, Smack Down and an
/// Iron Ball ground it; else a Flying type is in the air, Levitate
/// levitates, Magnet Rise and an Air Balloon lift it. `None` when an
/// unknown fact decides it.
pub fn ground(f: &GroundFacts) -> Option<Ground> {
    ground_by(f, |g| g)
}

/// `pokemon.isGrounded()` as a yes or no (Levitate is in the air), as
/// Terrain Pulse reads it: [`MoveUser::grounded`].
pub fn is_grounded(f: &GroundFacts) -> Option<bool> {
    ground_by(f, |g| g == Ground::Grounded)
}

/// [`ground`]'s rule over every value the unknown facts could have, seen
/// through `view`: known when they all agree.
fn ground_by<T: PartialEq>(f: &GroundFacts, view: impl Fn(Ground) -> T) -> Option<T> {
    let rule = |item: Option<ItemId>, levitate: bool| {
        if f.gravity || f.ingrain || f.smack_down || item == Some(i::IRON_BALL) {
            Ground::Grounded
        } else if f.flying {
            Ground::Airborne
        } else if levitate {
            Ground::Levitating
        } else if f.magnet_rise || item == Some(i::AIR_BALLOON) {
            Ground::Airborne
        } else {
            Ground::Grounded
        }
    };
    // Every fact known (the battle's own questions): the rule once.
    if let (Some(item), Some(levitate)) = (f.item, f.levitate) {
        return Some(view(rule(item, levitate)));
    }
    let items: &[Option<ItemId>] = match f.item {
        Some(item) => &[item],
        // Another item acts as none.
        None => &[None, Some(i::IRON_BALL), Some(i::AIR_BALLOON)],
    };
    let levitates: &[bool] = match f.levitate {
        Some(l) => &[l],
        None => &[false, true],
    };
    let first = view(rule(items[0], levitates[0]));
    let agree = items.iter().all(|&item| levitates.iter().all(|&l| view(rule(item, l)) == first));
    agree.then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Format;

    fn dex() -> &'static Dex {
        Format::gen9_singles().dex
    }

    fn known(species: SpeciesId) -> MoveUser {
        MoveUser {
            item: Some(None),
            ability: AbilityFact::Known(None),
            grounded: Some(true),
            weather: Some(None),
            ..MoveUser::new(dex(), species)
        }
    }

    /// [`may_modify_type`] is `false` only where [`move_type`] gives every
    /// move without a type rule of its own its data's type: each ability,
    /// no ability, each species' candidate slots, and no candidates.
    #[test]
    fn may_modify_type_is_exact() {
        let d = dex();
        let user = MoveUser { tera: Some(Type::Fire), ..known(s::PIKACHU) };
        let mut facts = vec![AbilityFact::Known(None), AbilityFact::OneOf([None; 4]), AbilityFact::Unknown];
        facts.extend((0..d.abilities.len()).map(|k| AbilityFact::Known(Some(AbilityId(k as u16)))));
        facts.extend(d.species.iter().map(|sp| {
            let x = sp.abilities;
            AbilityFact::OneOf([Some(x.primary), x.secondary, x.hidden, x.special])
        }));
        let (mut plain, mut changing) = (0, 0);
        for ability in facts {
            let user = MoveUser { ability, ..user };
            let same = (0..d.moves.len())
                .map(|k| MoveId(k as u16))
                .filter(|&id| !has_own_type(id) && id != mv::STRUGGLE)
                .all(|id| {
                    move_type(d, id, &user) == Some(d.move_(id).ty)
                        && move_type(d, id, &MoveUser { tera: None, ..user }) == Some(d.move_(id).ty)
                });
            if !may_modify_type(&ability) {
                assert!(same, "{ability:?} changes a move's type");
                plain += 1;
            } else {
                changing += 1;
            }
        }
        assert!(plain > 100 && changing >= 8, "{plain} plain, {changing} changing");
    }

    #[test]
    fn own_types() {
        let d = dex();
        let user = known(s::ARCEUS);
        assert_eq!(move_type(d, mv::TACKLE, &user), Some(Type::Normal));
        assert_eq!(
            move_type(d, mv::JUDGMENT, &MoveUser { item: Some(Some(i::FLAME_PLATE)), ..user }),
            Some(Type::Fire)
        );
        assert_eq!(move_type(d, mv::JUDGMENT, &MoveUser { item: None, ..user }), None);
        assert_eq!(move_type(d, mv::TACKLE, &MoveUser { item: None, ..user }), Some(Type::Normal));
        let tauros = known(s::TAUROS_PALDEA_BLAZE);
        assert_eq!(move_type(d, mv::RAGING_BULL, &tauros), Some(Type::Fire));
        let ogerpon = known(s::OGERPON_CORNERSTONE);
        assert_eq!(move_type(d, mv::IVY_CUDGEL, &ogerpon), Some(Type::Rock));
        assert_eq!(move_type(d, mv::IVY_CUDGEL, &known(s::OGERPON)), Some(Type::Grass));
        assert_eq!(move_type(d, mv::AURA_WHEEL, &known(s::MORPEKO_HANGRY)), Some(Type::Dark));
        assert_eq!(move_type(d, mv::TERA_BLAST, &user), Some(Type::Normal));
        assert_eq!(move_type(d, mv::TERA_BLAST, &MoveUser { tera: Some(Type::Ghost), ..user }), Some(Type::Ghost));
        assert_eq!(move_type(d, mv::TERA_STARSTORM, &known(s::TERAPAGOS_STELLAR)), Some(Type::Stellar));
        let oricorio = known(s::ORICORIO_PAU);
        assert_eq!(move_type(d, mv::REVELATION_DANCE, &oricorio), Some(Type::Psychic));
        let burnt = MoveUser { types: [Some(Type::Unknown), Some(Type::Flying), None], ..oricorio };
        assert_eq!(move_type(d, mv::REVELATION_DANCE, &burnt), Some(Type::Flying));
    }

    #[test]
    fn weather_and_terrain() {
        let d = dex();
        let user = known(s::CASTFORM);
        let with = |weather| MoveUser { weather, ..user };
        assert_eq!(move_type(d, mv::WEATHER_BALL, &with(Some(Some(Weather::Sand)))), Some(Type::Rock));
        assert_eq!(move_type(d, mv::WEATHER_BALL, &with(Some(Some(Weather::HeavyRain)))), Some(Type::Water));
        assert_eq!(move_type(d, mv::WEATHER_BALL, &with(None)), None);
        // The user's weather: an unknown item matters in sun only.
        assert_eq!(move_weather(Some(Weather::Sand), None, Some(false)), Some(Some(Weather::Sand)));
        assert_eq!(move_weather(Some(Weather::Sun), None, Some(false)), None);
        assert_eq!(move_weather(Some(Weather::Sun), Some(Some(i::UTILITY_UMBRELLA)), Some(false)), Some(None));
        assert_eq!(move_weather(Some(Weather::Rain), Some(Some(i::LEFTOVERS)), Some(false)), Some(Some(Weather::Rain)));
        assert_eq!(move_weather(None, None, Some(true)), Some(Some(Weather::Sun)));
        assert_eq!(move_weather(Some(Weather::Sun), Some(None), None), Some(Some(Weather::Sun)));
        assert_eq!(move_weather(None, Some(None), None), None);
        let pulse = |terrain, grounded| MoveUser { terrain, grounded, ..user };
        assert_eq!(move_type(d, mv::TERRAIN_PULSE, &pulse(None, None)), Some(Type::Normal));
        assert_eq!(move_type(d, mv::TERRAIN_PULSE, &pulse(Some(Terrain::Misty), None)), None);
        assert_eq!(move_type(d, mv::TERRAIN_PULSE, &pulse(Some(Terrain::Misty), Some(true))), Some(Type::Fairy));
        assert_eq!(move_type(d, mv::TERRAIN_PULSE, &pulse(Some(Terrain::Misty), Some(false))), Some(Type::Normal));
    }

    #[test]
    fn abilities_and_electrify() {
        let d = dex();
        let sylveon = MoveUser { ability: AbilityFact::Known(Some(a::PIXILATE)), ..known(s::SYLVEON) };
        assert_eq!(move_type(d, mv::HYPER_VOICE, &sylveon), Some(Type::Fairy));
        assert_eq!(move_type(d, mv::MOONBLAST, &sylveon), Some(Type::Fairy));
        assert_eq!(move_type(d, mv::WEATHER_BALL, &sylveon), Some(Type::Normal));
        let tera = MoveUser { tera: Some(Type::Normal), ..sylveon };
        assert_eq!(move_type(d, mv::TERA_BLAST, &tera), Some(Type::Normal));
        let unknown = MoveUser { ability: AbilityFact::Unknown, ..sylveon };
        assert_eq!(move_type(d, mv::HYPER_VOICE, &unknown), None, "an -ate, Normalize or Liquid Voice");
        assert_eq!(move_type(d, mv::MOONBLAST, &unknown), None, "Normalize");
        // Sylveon's abilities: Cute Charm or Pixilate.
        let slots = d.species(s::SYLVEON).abilities;
        let one_of = MoveUser {
            ability: AbilityFact::OneOf([Some(slots.primary), slots.secondary, slots.hidden, slots.special]),
            ..sylveon
        };
        assert_eq!(move_type(d, mv::MOONBLAST, &one_of), Some(Type::Fairy));
        assert_eq!(move_type(d, mv::HYPER_VOICE, &one_of), None);
        let garchomp = d.species(s::GARCHOMP).abilities;
        let chomp = MoveUser {
            ability: AbilityFact::OneOf([
                Some(garchomp.primary),
                garchomp.secondary,
                garchomp.hidden,
                garchomp.special,
            ]),
            ..known(s::GARCHOMP)
        };
        assert_eq!(move_type(d, mv::HYPER_VOICE, &chomp), Some(Type::Normal));
        let normalize = MoveUser { ability: AbilityFact::Known(Some(a::NORMALIZE)), ..known(s::DELCATTY) };
        assert_eq!(move_type(d, mv::THUNDERBOLT, &normalize), Some(Type::Normal));
        let voice = MoveUser { ability: AbilityFact::Known(Some(a::LIQUID_VOICE)), ..known(s::PRIMARINA) };
        assert_eq!(move_type(d, mv::HYPER_VOICE, &voice), Some(Type::Water));
        let electrified = MoveUser { electrified: true, item: None, ability: AbilityFact::Unknown, ..sylveon };
        assert_eq!(move_type(d, mv::JUDGMENT, &electrified), Some(Type::Electric));
        // Struggle is typeless (`???`), whatever the user.
        let struggle = |user: &MoveUser| move_type(d, mv::STRUGGLE, user);
        assert_eq!(struggle(&MoveUser { ability: AbilityFact::Known(None), ..electrified }), Some(Type::Unknown));
        for user in [sylveon, normalize, unknown] {
            assert_eq!(struggle(&user), Some(Type::Unknown));
        }
    }

    #[test]
    fn chart_verdicts() {
        use Effectiveness::{Exp, Immune};
        let water = [Some(Type::Water), None, None];
        assert_eq!(effectiveness(mv::ICE_BEAM, Type::Ice, water, false), Exp(-1));
        assert_eq!(effectiveness(mv::FREEZE_DRY, Type::Ice, water, false), Exp(1));
        let water_ground = [Some(Type::Water), Some(Type::Ground), None];
        assert_eq!(effectiveness(mv::FREEZE_DRY, Type::Ice, water_ground, false), Exp(2));
        let grass = [Some(Type::Grass), None, None];
        assert_eq!(effectiveness(mv::FLYING_PRESS, Type::Fighting, grass, false), Exp(1));
        let ghost = [Some(Type::Ghost), None, None];
        assert_eq!(effectiveness(mv::FLYING_PRESS, Type::Fighting, ghost, false), Immune);
        assert_eq!(effectiveness(mv::TERA_BLAST, Type::Stellar, ghost, true), Exp(1));
        assert_eq!(effectiveness(mv::TERA_BLAST, Type::Stellar, ghost, false), Exp(0));
        let flying_added = [Some(Type::Normal), None, Some(Type::Grass)];
        assert_eq!(effectiveness(mv::EARTHQUAKE, Type::Ground, flying_added, false), Exp(-1));
        assert_eq!(effectiveness(mv::EARTHQUAKE, Type::Ground, [Some(Type::Flying), None, None], false), Immune);
        // Struggle's `???` is neutral on everything, Ghost included.
        assert_eq!(effectiveness(mv::STRUGGLE, Type::Unknown, ghost, false), Exp(0));
    }

    #[test]
    fn grounded() {
        let base = GroundFacts { item: Some(None), levitate: Some(false), ..GroundFacts::default() };
        assert_eq!(ground(&base), Some(Ground::Grounded));
        assert_eq!(ground(&GroundFacts { flying: true, ..base }), Some(Ground::Airborne));
        assert_eq!(ground(&GroundFacts { flying: true, item: None, ..base }), None, "an Iron Ball");
        assert_eq!(ground(&GroundFacts { flying: true, item: None, gravity: true, ..base }), Some(Ground::Grounded));
        assert_eq!(ground(&GroundFacts { item: None, ..base }), None, "an Air Balloon");
        assert_eq!(ground(&GroundFacts { levitate: None, ..base }), None);
        assert_eq!(ground(&GroundFacts { levitate: Some(true), ..base }), Some(Ground::Levitating));
        assert_eq!(
            ground(&GroundFacts { item: Some(Some(i::AIR_BALLOON)), levitate: None, ..base }),
            None,
            "airborne or levitating"
        );
        let balloon = GroundFacts { item: Some(Some(i::AIR_BALLOON)), levitate: None, ..base };
        assert_eq!(is_grounded(&balloon), Some(false));
        assert_eq!(is_grounded(&GroundFacts { item: None, ..base }), None);
    }
}
