#![allow(dead_code)]

use pikapro::data::{MoveId, SpeciesId};
use pikapro::testing::sample_team;
use pikapro::{Battle, BattleOptions, BattleRng, Choice, Format, PokemonSet, Seed, SideId, SlotAction, Team};

/// A set of `species` at `level` with `moves`.
pub fn mon(species: SpeciesId, level: u8, moves: &[MoveId]) -> PokemonSet {
    PokemonSet::new(species).with_level(level).with_moves(moves)
}

/// A gen9singles battle between two teams.
pub fn singles(p1: Vec<PokemonSet>, p2: Vec<PokemonSet>, seed: u64) -> Battle {
    Battle::new(Format::gen9_singles(), [Team::new(p1), Team::new(p2)], BattleOptions::new(Seed::fast(seed))).unwrap()
}

/// Move `index`, without a target or a gimmick.
pub fn move_action(index: u8) -> SlotAction {
    SlotAction::Move { index, target: None, gimmick: None }
}

/// A singles choice of the first move.
pub fn move_1() -> Choice {
    Choice::single(move_action(0))
}

/// Drops the events so far, then both sides choose.
pub fn turn(battle: &mut Battle, p1: Choice, p2: Choice) {
    battle.drain_events().for_each(drop);
    battle.submit(SideId::P1, p1).unwrap();
    battle.submit(SideId::P2, p2).unwrap();
}

pub fn new_battle(format: Format, seed: u64) -> Battle {
    let teams = [sample_team(&format), sample_team(&format)];
    Battle::new(format, teams, BattleOptions::new(Seed::fast(seed))).expect("sample teams are valid")
}

/// Plays random legal choices until the battle ends, checking invariants
/// after every resolution. Returns the number of decisions made.
pub fn play_random(battle: &mut Battle, policy_seed: u64) -> usize {
    let mut rng = BattleRng::new(Seed::fast(policy_seed));
    let mut decisions = 0;
    while !battle.is_over() {
        let mut submitted = false;
        for side in SideId::BOTH {
            let legal = battle.legal_actions(side);
            if legal.is_waiting() {
                continue;
            }
            let choice = legal.sample(&mut rng);
            assert!(legal.is_legal(&choice), "sampled an illegal choice: {choice:?}");
            battle.submit(side, choice).expect("legal choice accepted");
            submitted = true;
            decisions += 1;
        }
        assert!(submitted || battle.is_over(), "no side can act but the battle is not over");
        check_invariants(battle);
        assert!(decisions < 100_000, "battle did not terminate");
    }
    decisions
}

pub fn check_invariants(battle: &Battle) {
    let active = battle.format().active_per_side();
    for side in SideId::BOTH {
        let s = battle.state().side(side);
        for mon in s.team.iter() {
            assert!(mon.hp <= mon.max_hp(), "hp exceeds max");
            for m in mon.moves.iter() {
                assert!(m.pp <= m.max_pp);
            }
        }
        for slot in 0..active {
            if let Some(i) = s.active[slot] {
                assert!((i as usize) < s.team.len());
                assert!(!s.team[i as usize].is_fainted(), "fainted Pokémon left active");
                let other = 1 - slot;
                assert!(other >= active || s.active[other] != Some(i), "same Pokémon in two slots");
            }
        }
    }
}
