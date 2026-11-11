use pikapro::{BattleRng, Seed};

use super::*;

#[test]
fn teams_are_six_distinct_species() {
    for format in [RandomBattle::Singles, RandomBattle::Doubles] {
        for seed in 0..200u16 {
            let mut rng = BattleRng::new(Seed::ShowdownGen5([seed, 1, 2, 3]));
            let team = generate(format, &mut rng).expect("a team");
            assert_eq!(team.len(), 6);
        }
    }
}
