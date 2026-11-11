//! Terastallization: Showdown's `BattleActions#canTerastallize` and
//! `#terastallize` (`sim/battle-actions.ts`), and the Stellar Tera type's
//! rules in `getDamage`, `modifyDamage` and `Pokemon#runEffectiveness`.
//!
//! - A Terastallized Pokémon's types (`getTypes`) are its Tera type, except
//!   Stellar, which keeps its own types for defence
//!   ([`PokemonState::tera_types`]).
//! - Ogerpon changes into its Tera forme (Embody Aspect starts), and
//!   Terapagos-Terastal into Terapagos-Stellar (Teraform Zero runs on
//!   `AfterTerastallization`); both go back on fainting (`formeRegression`).
//! - Stellar: once per move type (every time for Terapagos-Stellar) its
//!   moves get 2x with STAB or 1.2x without, in place of STAB
//!   ([`stellar_stab`]); a Stellar move is super effective against any
//!   Terastallized target ([`typing::stellar_effective`](crate::typing::stellar_effective)).

use pikapro_data::gen9::species;
use pikapro_data::{Dex, SpeciesId, Type};

use super::events::Effect;
use super::{abilities, actor, forme, manipulation, run_mon_event};
use crate::battle::Battle;
use crate::event::BattleEvent;
use crate::format::{Format, Gimmick};
use crate::state::{PokemonState, Position, SideState, Volatiles};
use crate::team::PokemonSet;

fn base_of(dex: &Dex, species: SpeciesId) -> SpeciesId {
    dex.species(species).base_species
}

/// Showdown's `pokemon.species`: the species it transformed into, if it
/// did (`species` stays its own).
pub(crate) fn species_of(mon: &PokemonState) -> SpeciesId {
    match mon.volatile_counters.transform {
        Some(t) if mon.volatiles.has(Volatiles::TRANSFORMED) => t.species,
        _ => mon.species,
    }
}

/// `canTerastallize(pokemon)` when the Pokémon is created: `null` (never)
/// if it holds a Z-Crystal or a Mega Stone it can use (`canMegaEvo`).
pub(crate) fn blocked_at_start(format: &Format, set: &PokemonSet) -> bool {
    let Some(item) = set.item else { return false };
    let data = format.dex.item(item);
    // `canMegaEvo`: `item.megaStone[species.name]`, else
    // `item.megaStone[species.baseSpecies]` unless that is this forme.
    let base = base_of(format.dex, set.species);
    let mega = data.mega.iter().any(|&(from, into)| from == set.species || from == base && into != set.species);
    // (Rayquaza's `canMegaEvo` through Dragon Ascent needs a Past species
    // and move: no Gen 9 or Champions format has it.)
    data.is_z_crystal || mega
}

/// Whether the Pokémon can choose to Terastallize (Showdown's
/// `pokemon.canTerastallize`): not once its side has (`ally.canTerastallize
/// = null`), not after a Z-Crystal or Mega Stone at the start, and not
/// while transformed into Ogerpon or Terapagos (`transformInto` sets it to
/// `false`; `clearVolatile` restores it).
pub(crate) fn can_terastallize(format: &Format, side: &SideState, mon: &PokemonState) -> bool {
    let transformed_into_tera_species = mon.volatiles.has(Volatiles::TRANSFORMED)
        && matches!(base_of(format.dex, species_of(mon)), species::OGERPON | species::TERAPAGOS);
    !side.gimmicks_used.contains(Gimmick::Terastallize)
        && !mon.terastallized
        && !mon.tera_blocked
        && !transformed_into_tera_species
}

/// Showdown's `terastallize(pokemon)`, the `terastallize` action.
pub(crate) fn terastallize(battle: &mut Battle, pos: Position) {
    let dex = battle.format.dex;
    let Some(mon) = battle.state.side(pos.side).active(pos.slot as usize) else { return };
    // `pokemon.species`: a transformed Ogerpon is not one here.
    let base = base_of(dex, species_of(mon));
    let illusion = mon.illusion.and_then(|id| {
        let side = battle.state.side(pos.side);
        side.team.iter().find(|m| m.id == id).map(|m| base_of(dex, species_of(m)))
    });
    // An Ogerpon Terastallizing into a type other than its masks' crashes
    // the game: Showdown only hints, and nothing happens.
    if base == species::OGERPON
        && !matches!(mon.tera_type, Type::Fire | Type::Grass | Type::Rock | Type::Water)
        && illusion.is_none_or(|s| s == species::OGERPON)
    {
        return;
    }
    // An Illusion disguised as Ogerpon or Terapagos ends (`singleEvent('End',
    // Illusion)`).
    if matches!(illusion, Some(species::OGERPON | species::TERAPAGOS)) {
        abilities::switchin::illusion_end(battle, pos);
    }
    let side = battle.state.side_mut(pos.side);
    let mon = side.active_mut(pos.slot as usize).expect("checked above");
    let ty = mon.tera_type;
    mon.terastallized = true;
    // `addedType = ''`, `knownType = true`, `apparentType = type`
    // (Transform copies it).
    mon.volatile_counters.added_type = None;
    mon.volatile_counters.type_unknown = false;
    mon.volatile_counters.tera_apparent = true;
    manipulation::refresh_types(mon);
    // `ally.canTerastallize = null` for the whole side.
    side.gimmicks_used.insert(Gimmick::Terastallize);
    let actor = actor(battle, pos);
    battle.emit(BattleEvent::Terastallize { actor, ty });
    // Ogerpon's Tera forme (`toID(species.battleOnly || species.id)` plus
    // `tealtera` or `tera`) and Terapagos-Stellar: permanent forme changes
    // with no source, so they revert on fainting (`formeRegression`). The
    // new ability starts (Embody Aspect).
    let current = species_of(&battle.state.side(pos.side).team[pos.slot as usize]);
    let tera_forme = match current {
        species::OGERPON | species::OGERPON_TEAL_TERA => Some(species::OGERPON_TEAL_TERA),
        species::OGERPON_WELLSPRING | species::OGERPON_WELLSPRING_TERA => Some(species::OGERPON_WELLSPRING_TERA),
        species::OGERPON_HEARTHFLAME | species::OGERPON_HEARTHFLAME_TERA => Some(species::OGERPON_HEARTHFLAME_TERA),
        species::OGERPON_CORNERSTONE | species::OGERPON_CORNERSTONE_TERA => Some(species::OGERPON_CORNERSTONE_TERA),
        species::TERAPAGOS_TERASTAL => Some(species::TERAPAGOS_STELLAR),
        _ => None,
    };
    if let Some(forme) = tera_forme {
        forme::forme_change(battle, pos, forme, Effect::None, true, false);
    }
    // A Hangry Morpeko keeps its forme.
    abilities::switchin::terastallized(battle, pos);
    // `runEvent('AfterTerastallization')` (Teraform Zero, Opportunist,
    // Mirror Herb).
    run_mon_event(battle, pikapro_data::Event::AfterTerastallization, pos);
}

/// `getDamage`'s Tera check for the 60 base power floor: a move of its Tera
/// type (`hasType`), or for Stellar a type whose boost it has not used
/// (`stellarBoostedTypes`, read before this hit's `modifyDamage`). `None`
/// is Struggle's `???`, which only Stellar's check lets through.
pub(crate) fn power_floor_type(mon: &PokemonState, move_ty: Option<Type>) -> bool {
    if !mon.terastallized {
        return false;
    }
    match move_ty {
        _ if mon.tera_type == Type::Stellar => move_ty.is_none_or(|t| mon.stellar_boosted & 1 << t as u32 == 0),
        Some(t) => mon.types.contains(t),
        None => false,
    }
}

/// `modifyDamage`'s STAB for a Pokémon Terastallized into Stellar, in
/// 4096ths, in place of STAB and `ModifySTAB` (`None` for anyone else).
/// The first move of each type (every move of it for Terapagos-Stellar),
/// and every later hit and target of that move (`move.stellarBoosted`),
/// get 2x with STAB or 1.2x (`[4915, 4096]`) without; other moves keep
/// their STAB.
pub(crate) fn stellar_stab(battle: &mut Battle, pos: Position, move_ty: Type, is_stab: bool) -> Option<u32> {
    let mon = &battle.state.side(pos.side).team[pos.slot as usize];
    if !mon.terastallized || mon.tera_type != Type::Stellar {
        return None;
    }
    let (boosted, own_species) = (mon.stellar_boosted, mon.species);
    let bit = 1 << move_ty as u32;
    if boosted & bit != 0 && !battle.move_state.stellar_boosted {
        return Some(if is_stab { 6144 } else { 4096 });
    }
    battle.move_state.stellar_boosted = true;
    if own_species != species::TERAPAGOS_STELLAR {
        battle.state.side_mut(pos.side).team[pos.slot as usize].stellar_boosted |= bit;
    }
    Some(if is_stab { 8192 } else { 4915 })
}

#[cfg(test)]
mod tests {
    use pikapro_data::gen9::{abilities as a, items as i, moves as m, species as s};
    use pikapro_data::{MoveId, SpeciesId, Type, TypeSet};

    use crate::battle::Battle;
    use crate::choice::{Choice, SlotAction};
    use crate::event::BattleEvent;
    use crate::format::Gimmick;
    use crate::state::{Position, SideId};
    use crate::team::{PokemonSet, Team};
    use crate::{BattleOptions, Format, Seed};

    const P1: Position = Position::new(SideId::P1, 0);
    const P2: Position = Position::new(SideId::P2, 0);

    fn set(species: SpeciesId, moves: &[MoveId]) -> PokemonSet {
        PokemonSet::new(species).with_level(50).with_ability(a::NO_ABILITY).with_moves(moves)
    }

    fn singles(p1: PokemonSet, p2: PokemonSet) -> Battle {
        let teams = [Team::new(vec![p1]), Team::new(vec![p2])];
        Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(1))).unwrap()
    }

    /// P1 uses its first move, Terastallizing; P2 uses its first move.
    fn tera_turn(b: &mut Battle) {
        let tera = SlotAction::Move { index: 0, target: None, gimmick: Some(Gimmick::Terastallize) };
        b.submit(SideId::P1, Choice::single(tera)).unwrap();
        let plain = SlotAction::Move { index: 0, target: None, gimmick: None };
        b.submit(SideId::P2, Choice::single(plain)).unwrap();
    }

    fn damage_to_p2(b: &Battle) -> Vec<u16> {
        let mut hp = b.state.at(P2).unwrap().max_hp();
        let mut out = Vec::new();
        for e in b.events() {
            if let BattleEvent::Damage { target, hp: now, .. } = e
                && target.pos == P2
            {
                out.push(hp - now);
                hp = *now;
            }
        }
        out
    }

    /// Terastallizing into Stellar keeps the Pokémon's own types, and
    /// shows `tera:Stellar`.
    #[test]
    fn stellar_keeps_types() {
        let mut b = singles(set(s::GARCHOMP, &[m::SPLASH]).with_tera_type(Type::Stellar), set(s::MEW, &[m::SPLASH]));
        tera_turn(&mut b);
        let mon = b.state.at(P1).unwrap();
        assert!(mon.terastallized);
        assert_eq!(mon.types, TypeSet::dual(Type::Dragon, Type::Ground));
        assert!(b.events().iter().any(|e| matches!(e, BattleEvent::Terastallize { ty: Type::Stellar, .. })));
    }

    /// Stellar Tera Blast: 100 power, Stellar type, lowers the user's
    /// Attack and Special Attack.
    #[test]
    fn stellar_tera_blast() {
        let mut b =
            singles(set(s::PORYGON2, &[m::TERA_BLAST]).with_tera_type(Type::Stellar), set(s::CHANSEY, &[m::SPLASH]));
        tera_turn(&mut b);
        let mon = b.state.at(P1).unwrap();
        assert_eq!((mon.boosts.atk, mon.boosts.spa), (-1, -1));
        // Its type (Stellar) has used its boost.
        assert_ne!(mon.stellar_boosted & 1 << Type::Stellar as u32, 0);
    }

    /// The Stellar boost is once per type (1.2x for a non-STAB type), and
    /// a Stellar move is super effective against a Terastallized target.
    #[test]
    fn stellar_boost_once_per_type() {
        let mut b =
            singles(set(s::PORYGON2, &[m::FLAMETHROWER]).with_tera_type(Type::Stellar), set(s::BLISSEY, &[m::SPLASH]));
        tera_turn(&mut b);
        let plain = SlotAction::Move { index: 0, target: None, gimmick: None };
        b.submit(SideId::P1, Choice::single(plain)).unwrap();
        b.submit(SideId::P2, Choice::single(plain)).unwrap();
        let mon = b.state.at(P1).unwrap();
        assert_eq!(mon.stellar_boosted, 1 << Type::Fire as u32);
        let hits = damage_to_p2(&b);
        assert_eq!(hits.len(), 2);
        // Boosted once: 1.2x at the lowest roll (85%) beats 1x at the highest.
        assert!(hits[0] > hits[1], "{hits:?}");
    }

    /// Ogerpon changes into its Tera forme, and Embody Aspect raises a
    /// stat; fainting reverts it.
    #[test]
    fn ogerpon_tera_forme() {
        let ogerpon = set(s::OGERPON_WELLSPRING, &[m::SPLASH])
            .with_ability(a::WATER_ABSORB)
            .with_item(i::WELLSPRING_MASK)
            .with_tera_type(Type::Water);
        let mut b = singles(ogerpon, set(s::MEW, &[m::SPLASH]));
        tera_turn(&mut b);
        let mon = b.state.at(P1).unwrap();
        assert_eq!((mon.species, mon.base_species), (s::OGERPON_WELLSPRING_TERA, s::OGERPON_WELLSPRING_TERA));
        assert_eq!((mon.ability, mon.boosts.spd), (a::EMBODY_ASPECT_WELLSPRING, 1));
        assert_eq!(mon.forme_regression, Some(s::OGERPON_WELLSPRING));
    }

    /// An Ogerpon without a set Tera type gets its first type, as in
    /// Showdown's simulator (the validator fills in the forced one); a Tera
    /// type other than the forced one is rejected.
    #[test]
    fn ogerpon_forced_tera_type() {
        let b = singles(set(s::OGERPON_HEARTHFLAME, &[m::SPLASH]), set(s::MEW, &[m::SPLASH]));
        assert_eq!(b.state.at(P1).unwrap().tera_type, Type::Grass);
        let wrong = set(s::OGERPON_HEARTHFLAME, &[m::SPLASH]).with_tera_type(Type::Water);
        let teams = [Team::new(vec![wrong]), Team::new(vec![set(s::MEW, &[m::SPLASH])])];
        let err = Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(1))).err();
        assert_eq!(err, Some(crate::team::TeamError::Set { index: 0, kind: crate::team::SetError::TeraType }));
    }

    /// Terapagos-Terastal becomes Terapagos-Stellar, whose Teraform Zero
    /// clears the weather.
    #[test]
    fn terapagos_stellar() {
        let terapagos = set(s::TERAPAGOS, &[m::SPLASH]).with_ability(a::TERA_SHIFT).with_tera_type(Type::Stellar);
        let mut b = singles(terapagos, set(s::MEW, &[m::RAIN_DANCE]));
        let plain = SlotAction::Move { index: 0, target: None, gimmick: None };
        b.submit(SideId::P1, Choice::single(plain)).unwrap();
        b.submit(SideId::P2, Choice::single(plain)).unwrap();
        assert!(b.state.field.weather.is_some());
        tera_turn(&mut b);
        let mon = b.state.at(P1).unwrap();
        assert_eq!((mon.species, mon.ability, mon.tera_type), (s::TERAPAGOS_STELLAR, a::TERAFORM_ZERO, Type::Stellar));
        assert!(b.events().iter().any(|e| matches!(e, BattleEvent::DetailsChange { .. })));
    }

    /// A Z-Crystal holder can't Terastallize.
    #[test]
    fn z_crystal_blocks_tera() {
        let b = singles(set(s::MEW, &[m::SPLASH]).with_item(i::NORMALIUM_Z), set(s::MEW, &[m::SPLASH]));
        assert!(
            b.legal_actions(SideId::P1)
                .slot(0)
                .iter()
                .all(|a| !matches!(a, SlotAction::Move { gimmick: Some(Gimmick::Terastallize), .. }))
        );
    }
}
