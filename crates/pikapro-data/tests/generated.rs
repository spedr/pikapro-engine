//! Spot checks of the generated tables against known Showdown values. These
//! catch generator regressions that still compile.

use pikapro_data::gen9::{abilities as a, items as i, moves as m, species as s};
use pikapro_data::{
    AbilityFlags, AbilityId, Fraction, GenderRule, MoveTarget, Nonstandard, SelfSwitch, StatBoosts, Stats, StatusId,
    Type, TypeSet, champions, gen9,
};

#[test]
fn species_rows() {
    let garchomp = gen9::DEX.species(s::GARCHOMP);
    assert_eq!(garchomp.name, "Garchomp");
    assert_eq!(garchomp.num, 445);
    assert_eq!(garchomp.types, TypeSet::dual(Type::Dragon, Type::Ground));
    assert_eq!(garchomp.base_stats, Stats::new(108, 130, 95, 80, 85, 102));
    assert_eq!(garchomp.abilities.primary, a::SAND_VEIL);
    assert_eq!(garchomp.abilities.hidden, Some(a::ROUGH_SKIN));
    assert_eq!(garchomp.weight_hg, 950);
    assert_eq!(garchomp.gender, GenderRule::Ratio { male_per_mille: 500 });
    assert_eq!(garchomp.base_species, s::GARCHOMP);

    let mega = gen9::DEX.species(s::CHARIZARD_MEGA_X);
    assert_eq!(mega.base_species, s::CHARIZARD);
    assert_eq!(mega.forme, "Mega-X");
    assert_eq!(mega.required_items, &[i::CHARIZARDITE_X]);
    assert!(mega.battle_only);

    assert_eq!(gen9::DEX.species(s::SHEDINJA).max_hp, Some(1));
    assert_eq!(gen9::DEX.species(s::GHOLDENGO).gender, GenderRule::Genderless);
}

#[test]
fn move_rows() {
    let tackle = gen9::DEX.move_(m::TACKLE);
    assert_eq!((tackle.base_power, tackle.accuracy, tackle.pp, tackle.priority), (40, Some(100), 35, 0));
    assert!(tackle.is_data_only());

    let rock_slide = gen9::DEX.move_(m::ROCK_SLIDE);
    assert_eq!(rock_slide.target, MoveTarget::AllAdjacentFoes);
    assert_eq!(rock_slide.secondaries.len(), 1);
    assert_eq!(rock_slide.secondaries[0].chance, 30);
    assert_eq!(rock_slide.secondaries[0].volatile_status, Some("flinch"));

    let fire_fang = gen9::DEX.move_(m::FIRE_FANG);
    assert_eq!(fire_fang.secondaries.len(), 2);
    assert_eq!(fire_fang.secondaries[0].status, Some(StatusId::Burn));

    let close_combat = gen9::DEX.move_(m::CLOSE_COMBAT);
    assert_eq!(close_combat.self_boosts, Some(StatBoosts::new([0, -1, 0, -1, 0, 0, 0])));
    assert_eq!(gen9::DEX.move_(m::MAKE_IT_RAIN).self_boosts, Some(StatBoosts::new([0, 0, -1, 0, 0, 0, 0])));

    assert_eq!(gen9::DEX.move_(m::U_TURN).self_switch, Some(SelfSwitch::Switch));
    assert_eq!(gen9::DEX.move_(m::DRAGON_DARTS).multihit, Some((2, 2)));
    assert_eq!(gen9::DEX.move_(m::GIGA_DRAIN).drain, Some(Fraction { numerator: 1, denominator: 2 }));
    assert_eq!(gen9::DEX.move_(m::DOUBLE_EDGE).recoil, Some(Fraction { numerator: 33, denominator: 100 }));
    assert_eq!(gen9::DEX.move_(m::WILL_O_WISP).status, Some(StatusId::Burn));

    // Callbacks are listed, not silently dropped.
    let knock_off = gen9::DEX.move_(m::KNOCK_OFF);
    assert_eq!(knock_off.unmodelled, &["onAfterHit", "onBasePower"]);
    assert!(!knock_off.is_data_only());
    assert!(gen9::DEX.move_(m::PROTECT).unmodelled.contains(&"condition"));
}

#[test]
fn ability_and_item_rows() {
    assert_eq!(gen9::DEX.ability(AbilityId::NONE).name, "No Ability");
    assert_eq!(a::NO_ABILITY, AbilityId::NONE);
    assert!(gen9::DEX.ability(a::LEVITATE).flags.contains(AbilityFlags::BREAKABLE));

    let charizardite = gen9::DEX.item(i::CHARIZARDITE_X);
    assert_eq!(charizardite.mega, &[(s::CHARIZARD, s::CHARIZARD_MEGA_X)]);
    assert_eq!(gen9::DEX.item(i::MAGEARNITE).mega.len(), 2);
    assert!(gen9::DEX.item(i::SITRUS_BERRY).is_berry);
    assert!(gen9::DEX.item(i::CHOICE_SCARF).is_choice);
}

#[test]
fn champions_shares_ids_and_overrides_fields() {
    assert_eq!(champions::DEX.species.len(), gen9::DEX.species.len());
    assert_eq!(champions::DEX.moves.len(), gen9::DEX.moves.len());
    // Showdown's `champions` mod: Protect has 5 base PP.
    assert_eq!(gen9::DEX.move_(m::PROTECT).pp, 10);
    assert_eq!(champions::DEX.move_(m::PROTECT).pp, 5);
    // Availability differs per game.
    assert_eq!(gen9::DEX.species(s::FLUTTER_MANE).nonstandard, None);
    assert_eq!(champions::DEX.species(s::FLUTTER_MANE).nonstandard, Some(Nonstandard::Past));
    assert_eq!(gen9::DEX.species(s::GENGAR_MEGA).nonstandard, Some(Nonstandard::Past));
    assert_eq!(champions::DEX.species(s::GENGAR_MEGA).nonstandard, None);
    assert_eq!(champions::DEX.move_(m::EARTHQUAKE), gen9::DEX.move_(m::EARTHQUAKE));
}

#[test]
fn names_resolve_to_constants() {
    assert_eq!(gen9::DEX.species_by_name("Flutter Mane"), Some(s::FLUTTER_MANE));
    assert_eq!(gen9::DEX.species_by_name("farfetchd"), Some(s::FARFETCHD));
    assert_eq!(gen9::DEX.move_by_name("U-turn"), Some(m::U_TURN));
    assert_eq!(gen9::DEX.item_by_name("King's Rock"), Some(i::KINGS_ROCK));
    assert_eq!(gen9::DEX.ability_by_name("Good as Gold"), Some(a::GOOD_AS_GOLD));
}
