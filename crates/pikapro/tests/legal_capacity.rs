//! The per-slot legal-action list is exactly as long as the longest
//! Showdown offers: a Pokémon that can both Terastallize and Mega Evolve in
//! doubles (Power Construct Zygarde holding Zygardite, once it is
//! Zygarde-Complete) lists each move and target three times.

use pikapro::data::gen9::{abilities as a, items as i, moves as m, species as s};
use pikapro::legal::MAX_SLOT_ACTIONS;
use pikapro::{Battle, BattleOptions, BattleRng, Format, PokemonSet, Seed, SideId, Team};

fn team() -> Team {
    let zygarde = PokemonSet::new(s::ZYGARDE)
        .with_level(50)
        .with_ability(a::POWER_CONSTRUCT)
        .with_item(i::ZYGARDITE)
        .with_moves(&[m::TACKLE, m::BITE, m::CRUNCH, m::DRAGON_PULSE]);
    let filler =
        |species| PokemonSet::new(species).with_level(50).with_moves(&[m::TACKLE, m::PROTECT, m::HELPING_HAND]);
    Team::new(vec![
        zygarde,
        filler(s::BLISSEY),
        filler(s::CHANSEY),
        filler(s::SNORLAX),
        filler(s::MILOTIC),
        filler(s::UMBREON),
    ])
}

/// Plays random battles to the end; returns the longest slot list seen.
fn longest_list(format: Format, seeds: std::ops::Range<u64>) -> usize {
    let mut longest = 0;
    for seed in seeds {
        let mut battle = Battle::new(format.clone(), [team(), team()], BattleOptions::new(Seed::fast(seed))).unwrap();
        let mut policy = BattleRng::new(Seed::fast(seed + 1000));
        while !battle.is_over() {
            for side in SideId::BOTH {
                let legal = battle.legal_actions(side);
                if legal.is_waiting() {
                    continue;
                }
                for slot in 0..2 {
                    longest = longest.max(legal.slot(slot).len());
                }
                battle.submit(side, legal.sample(&mut policy)).unwrap();
            }
        }
    }
    longest
}

#[test]
fn tera_and_mega_together_fill_the_list_in_doubles() {
    assert_eq!(longest_list(Format::gen9_doubles(), 0..40), MAX_SLOT_ACTIONS);
}
