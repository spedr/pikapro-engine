use pikapro::{Battle, BattleOptions, BattleRng, Choice, Format, PokemonSet, Seed, SideId, SlotAction, Team};
use pikapro_data::gen9::{abilities, moves, species};
use pikapro_data::{BoostId, Type};
use pikapro_view::{HpReport, OwnOverlay, PlayerView, PublicTracker, ViewConfig};

use super::heuristics::{estimate_matchup, stat_estimation};
use super::*;
use crate::eval::play;
use crate::policy::{Policy, RandomPolicy};

fn mon(t1: Type, t2: Option<Type>, base: [u8; 6], hp: f64) -> Mon {
    Mon { t1, t2, base, boosts: [0; 7], hp, stats: [0; 5], tera_type: None, terastallized: false }
}

#[test]
fn damage_multipliers_are_poke_envs() {
    assert_eq!(damage_multiplier(Type::Ground, Type::Fire, Some(Type::Flying)), 0.0);
    assert_eq!(damage_multiplier(Type::Ice, Type::Dragon, Some(Type::Flying)), 4.0);
    assert_eq!(damage_multiplier(Type::Fire, Type::Water, Some(Type::Rock)), 0.25);
    // Stellar is neutral both ways, and a Stellar Tera makes all neutral.
    assert_eq!(damage_multiplier(Type::Stellar, Type::Fire, None), 1.0);
    assert_eq!(damage_multiplier(Type::Fighting, Type::Stellar, None), 1.0);
    // poke-env's quirk: a `???` first type (after Double Shock or Burn Up)
    // makes every attack neutral, whatever the second type.
    assert_eq!(damage_multiplier(Type::Psychic, Type::Unknown, Some(Type::Fighting)), 1.0);
    assert_eq!(damage_multiplier(Type::Flying, Type::Unknown, Some(Type::Fighting)), 1.0);
    assert_eq!(damage_multiplier(Type::Unknown, Type::Fire, None), 1.0);
    // A `???` second type is neutral (poke-env has no `???` row and raises).
    assert_eq!(damage_multiplier(Type::Water, Type::Fire, Some(Type::Unknown)), 2.0);
}

#[test]
fn estimates_are_poke_envs() {
    // Values from poke-env 0.16.1's SimpleHeuristicsPlayer.
    let mut garchomp = mon(Type::Dragon, Some(Type::Ground), [108, 130, 95, 80, 85, 102], 1.0);
    let rotom = mon(Type::Electric, Some(Type::Water), [50, 65, 107, 105, 107, 86], 0.5);
    // max(Dragon: 1, Ground: 2) - max(Electric: 0, Water: 1) + 0.1 + 0.4 - 0.2
    // (poke-env ignores Levitate).
    assert_eq!(estimate_matchup(&garchomp, &rotom), 2.0 - 1.0 + 0.1 + 1.0 * 0.4 - 0.5 * 0.4);
    assert_eq!(stat_estimation(&garchomp, BoostId::Atk), (2.0 * 130.0 + 31.0 + 5.0) * 1.0);
    garchomp.boosts[BoostId::Atk as usize] = 1;
    // +1 counts x2 (poke-env's `2 / (2 - 1)`), as +2 does.
    assert_eq!(stat_estimation(&garchomp, BoostId::Atk), 296.0 * 2.0);
    garchomp.boosts[BoostId::Atk as usize] = 2;
    assert_eq!(stat_estimation(&garchomp, BoostId::Atk), 296.0 * 2.0);
    garchomp.boosts[BoostId::Atk as usize] = -2;
    assert_eq!(stat_estimation(&garchomp, BoostId::Atk), 296.0 * 0.5);
}

#[test]
fn ties_keep_the_first() {
    assert_eq!(first_max(&[1.0, 3.0, 3.0, 2.0], |&x| x), Some(1));
    assert_eq!(first_max::<f64>(&[], |&x| x), None);
}

fn policies() -> [(&'static str, Box<dyn Policy>); 5] {
    [
        ("random", Box::new(RandomPolicy)),
        ("maxbp", Box::new(MaxBasePowerPolicy)),
        ("heuristic", Box::new(SimpleHeuristicsPolicy::new())),
        ("heuristic+setup", Box::new(SimpleHeuristicsPolicy::new().with_setup_moves())),
        ("heuristic+tera", Box::new(SimpleHeuristicsPolicy::new().with_request_tera_type())),
    ]
}

/// Every policy against every other finishes battles in every preset with
/// legal choices, deterministically.
#[test]
fn policies_play_every_preset() {
    let ps = policies();
    for format in Format::presets() {
        for (i, (a, pa)) in ps.iter().enumerate() {
            for (b, pb) in ps.iter().skip(i) {
                let teams = [pikapro::testing::sample_team(format), pikapro::testing::sample_team(format)];
                let seed = 4_700_100 + i as u64;
                let run = || {
                    let mut battle =
                        Battle::new(format.clone(), teams.clone(), BattleOptions::new(Seed::fast(seed))).unwrap();
                    let mut rngs = [BattleRng::new(Seed::fast(seed)), BattleRng::new(Seed::fast(!seed))];
                    play(&mut battle, [&teams[0], &teams[1]], [&**pa, &**pb], &mut rngs, HpReport::Percent).unwrap()
                };
                let (x, y) = (run(), run());
                assert_eq!((x.winner, x.turns), (y.winner, y.turns), "{} {a} vs {b}", format.id);
            }
        }
    }
}

fn battle(format: Format, p1: Vec<PokemonSet>, p2: Vec<PokemonSet>) -> Battle {
    Battle::new(format, [Team::new(p1), Team::new(p2)], BattleOptions::new(Seed::fast(4_700_000))).unwrap()
}

/// The view of `side` of a battle whose events are all unread.
fn view_of(battle: &mut Battle, side: SideId) -> (PublicTracker, OwnOverlay) {
    let mut tracker = PublicTracker::new(ViewConfig::new(battle.format()));
    for e in battle.drain_events() {
        tracker.apply(&e);
    }
    (tracker, OwnOverlay::from_request(&battle.request_data(side)))
}

fn choose(policy: &dyn Policy, battle: &mut Battle, side: SideId) -> Choice {
    let (tracker, own) = view_of(battle, side);
    let legal = *battle.legal_actions(side);
    policy.choose(&PlayerView::new(side, &tracker, &own), &legal, &mut BattleRng::new(Seed::fast(1)))
}

fn mv(index: u8) -> SlotAction {
    SlotAction::Move { index, target: None, gimmick: None }
}

#[test]
fn max_base_power_takes_the_strongest_move() {
    let set = |s| PokemonSet::new(s).with_level(50);
    let mut b = battle(
        Format::gen9_singles(),
        vec![set(species::GARCHOMP).with_moves(&[
            moves::TACKLE,
            moves::EARTHQUAKE,
            moves::DRAGON_CLAW,
            moves::SWORDS_DANCE,
        ])],
        vec![set(species::SNORLAX).with_moves(&[moves::TACKLE])],
    );
    assert_eq!(choose(&MaxBasePowerPolicy, &mut b, SideId::P1), Choice::single(mv(1)));
}

#[test]
fn heuristics_weigh_effectiveness_and_stab() {
    // Against Skarmory, Earthquake (x0) and Tackle (x0.5) lose to Dragon
    // Claw (x0.5, but Garchomp's STAB).
    let set = |s| PokemonSet::new(s).with_level(50);
    let mut b = battle(
        Format::gen9_singles(),
        vec![
            set(species::GARCHOMP).with_moves(&[moves::TACKLE, moves::EARTHQUAKE, moves::DRAGON_CLAW]),
            set(species::SNORLAX).with_moves(&[moves::TACKLE]),
        ],
        vec![set(species::SKARMORY).with_moves(&[moves::TACKLE])],
    );
    assert_eq!(choose(&SimpleHeuristicsPolicy::new(), &mut b, SideId::P1), Choice::single(mv(2)));
    // Max base power doesn't care.
    assert_eq!(choose(&MaxBasePowerPolicy, &mut b, SideId::P1), Choice::single(mv(1)));
}

#[test]
fn heuristics_set_hazards_first() {
    let set = |s| PokemonSet::new(s).with_level(50);
    let mut b = battle(
        Format::gen9_singles(),
        vec![set(species::GARCHOMP).with_moves(&[moves::EARTHQUAKE, moves::STEALTH_ROCK])],
        vec![set(species::SNORLAX).with_moves(&[moves::TACKLE])],
    );
    assert_eq!(choose(&SimpleHeuristicsPolicy::new(), &mut b, SideId::P1), Choice::single(mv(1)));
}

/// A switch the request allows but a hidden trap forbids is chosen, then
/// dropped as Showdown's `[Unavailable choice]` would make it.
#[test]
fn a_rejected_switch_is_chosen_again_without_it() {
    let set = |s| PokemonSet::new(s).with_level(50);
    let mut b = battle(
        Format::gen9_singles(),
        vec![
            set(species::PIKACHU).with_moves(&[moves::THUNDERBOLT]),
            set(species::SNORLAX).with_moves(&[moves::TACKLE]),
        ],
        vec![set(species::WOBBUFFET).with_ability(abilities::SHADOW_TAG).with_moves(&[moves::COUNTER])],
    );
    let (tracker, own) = view_of(&mut b, SideId::P1);
    let legal = *b.legal_actions(SideId::P1);
    let active = own.active[0].unwrap();
    assert!(active.maybe_trapped && !active.trapped, "the request allows a switch");
    assert!(!legal.slot(0).iter().any(|a| matches!(a, SlotAction::Switch(_))), "the battle doesn't");
    let view = PlayerView::new(SideId::P1, &tracker, &own);
    let mut tries = Vec::new();
    let choice = decide(&view, &legal, &mut BattleRng::new(Seed::fast(1)), |restrict, _| {
        tries.push(restrict.no_switch[0]);
        let s = Ctx::new(&view, false).slot(0, restrict);
        match s.switches.first() {
            Some(&p) => [Order::Switch(p), Order::Pass],
            None => [s.moves[0].order(0, false, false), Order::Pass],
        }
    });
    assert_eq!(tries, [false, true]);
    assert_eq!(choice, Choice::single(mv(0)));
}
