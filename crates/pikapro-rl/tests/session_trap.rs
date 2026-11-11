//! A trap the request hides (`maybeTrapped`) but the view shows: a foe's
//! revealed Shadow Tag (Gengar-Mega's only ability) keeps our last active
//! Pokémon in, and Showdown refuses its switches ("Can't switch: The active
//! Pokémon is trapped"). The session's actions (`shown_legal`) drop them,
//! as the engine's own legal actions do; an unrevealed Shadow Tag
//! (Wobbuffet's, one of two abilities) and a Ghost type keep them.

use pikapro::{
    Battle, BattleOptions, Choice, Format, Gimmick, Investment, PokemonSet, Request, Seed, SideId, SlotAction, Team,
};
use pikapro_data::gen9::{abilities, items, moves, species};
use pikapro_data::{SpeciesId, Stats};
use pikapro_rl::session::{revealed_traps, shown_legal};
use pikapro_view::{OwnOverlay, PublicTracker, ViewConfig};

fn set(s: SpeciesId) -> PokemonSet {
    PokemonSet::new(s)
        .with_level(50)
        .with_investment(Investment::StatPoints(Stats([0; 6])))
        .with_moves(&[moves::SPLASH, moves::PROTECT])
}

fn splash(gimmick: Option<Gimmick>) -> SlotAction {
    SlotAction::Move { index: 0, target: None, gimmick }
}

fn has_switch(list: &[SlotAction]) -> bool {
    list.iter().any(|a| matches!(a, SlotAction::Switch(_)))
}

/// A Champions doubles battle after one turn in which p2's lead `foe`
/// (Mega Evolving with `mega`) faced p1's `ours` (Snorlax in slot a, `ours`
/// in slot b): the battle, p1's view and its overlay.
fn after_one_turn(ours: SpeciesId, foe: PokemonSet, mega: bool) -> (Battle, PublicTracker, OwnOverlay) {
    let format = Format::champions_vgc();
    let p1 = Team::new(vec![
        set(species::SNORLAX),
        set(ours),
        set(species::LAPRAS),
        set(species::RAICHU),
        set(species::PICHU),
        set(species::EEVEE),
    ]);
    let p2 = Team::new(vec![
        foe,
        set(species::SNORLAX),
        set(species::PIKACHU),
        set(species::RAICHU),
        set(species::PICHU),
        set(species::EEVEE),
    ]);
    let mut battle = Battle::new(format, [p1, p2], BattleOptions::new(Seed::fast(7))).unwrap();
    let mut tracker = PublicTracker::new(ViewConfig::new(battle.format()));
    for side in SideId::BOTH {
        if let Request::TeamPreview { .. } = battle.request(side) {
            battle.submit(side, Choice::team(&[0, 1, 2, 3])).unwrap();
        }
    }
    battle.submit(SideId::P1, Choice::doubles(splash(None), splash(None))).unwrap();
    let g = mega.then_some(Gimmick::MegaEvolve);
    battle.submit(SideId::P2, Choice::doubles(splash(g), splash(None))).unwrap();
    for e in battle.drain_events() {
        tracker.apply(&e);
    }
    let own = OwnOverlay::from_request(&battle.request_data(SideId::P1));
    (battle, tracker, own)
}

#[test]
fn a_revealed_shadow_tag_drops_the_last_active_pokemon_s_switches() {
    let gengar = set(species::GENGAR).with_item(items::GENGARITE).with_ability(abilities::CURSED_BODY);
    let (battle, tracker, own) = after_one_turn(species::PIKACHU, gengar, true);
    let format = battle.format();
    // Slot b is the last active Pokémon: its request says `maybeTrapped`.
    let b = own.active[1].unwrap();
    assert!(b.maybe_trapped && !b.trapped, "{b:?}");
    // The engine traps both (its legal actions have no switch).
    assert!(!has_switch(battle.legal_actions(SideId::P1).slot(1)));
    assert_eq!(revealed_traps(format, &own, &tracker, SideId::P1), [false, true]);
    let (legal, _) = shown_legal(format, &own, &tracker, SideId::P1);
    assert!(!has_switch(legal.slot(0)) && !has_switch(legal.slot(1)));
}

#[test]
fn an_unrevealed_shadow_tag_keeps_them() {
    let wobbuffet = set(species::WOBBUFFET).with_ability(abilities::SHADOW_TAG);
    let (battle, tracker, own) = after_one_turn(species::PIKACHU, wobbuffet, false);
    let format = battle.format();
    let b = own.active[1].unwrap();
    assert!(b.maybe_trapped && !b.trapped, "{b:?}");
    // Trapped in truth, but nothing showed it: as Showdown's request.
    assert!(!has_switch(battle.legal_actions(SideId::P1).slot(1)));
    assert_eq!(revealed_traps(format, &own, &tracker, SideId::P1), [false, false]);
    let (legal, _) = shown_legal(format, &own, &tracker, SideId::P1);
    assert!(has_switch(legal.slot(1)));
}

#[test]
fn a_ghost_type_is_not_trapped() {
    let gengar = set(species::GENGAR).with_item(items::GENGARITE).with_ability(abilities::CURSED_BODY);
    let (battle, tracker, own) = after_one_turn(species::SABLEYE, gengar, true);
    let format = battle.format();
    assert!(has_switch(battle.legal_actions(SideId::P1).slot(1)));
    assert_eq!(revealed_traps(format, &own, &tracker, SideId::P1), [false, false]);
    let (legal, _) = shown_legal(format, &own, &tracker, SideId::P1);
    assert!(has_switch(legal.slot(1)));
}
