//! Damage variants: multi-hit, drain, recoil, healing, self-destruct, OHKO
//! and fixed damage. Exact behaviour is checked against Showdown by the
//! conformance fuzzer; these tests pin the basic shapes.

mod common;

use pikapro::data::MoveId;
use pikapro::data::gen9::{moves as m, species as s};
use pikapro::event::{DamageSource, HealSource};
use pikapro::{Battle, BattleEvent, BattleOptions, Format, Request, Seed, SideId, Team};

fn battle(p1: (pikapro::data::SpeciesId, MoveId), p2: (pikapro::data::SpeciesId, MoveId), seed: u64) -> Battle {
    let team = |(sp, mv)| Team::new(vec![common::mon(sp, 50, &[mv])]);
    let mut battle = Battle::new(Format::gen9_singles(), [team(p1), team(p2)], BattleOptions::new(Seed::fast(seed)))
        .expect("valid teams");
    while battle.request(SideId::P1) != Request::Move {
        for side in SideId::BOTH {
            let legal = battle.legal_actions(side);
            if !legal.is_waiting() {
                let choice = legal.sample(&mut pikapro::BattleRng::new(Seed::fast(0)));
                battle.submit(side, choice).unwrap();
            }
        }
    }
    battle
}

/// Both sides use their first move; returns the events of that turn.
fn turn(battle: &mut Battle) -> Vec<BattleEvent> {
    let before = battle.events().len();
    for side in SideId::BOTH {
        battle.submit(side, common::move_1()).unwrap();
    }
    battle.events()[before..].to_vec()
}

fn damage_to(events: &[BattleEvent], side: SideId, source: DamageSource) -> Vec<u16> {
    events
        .iter()
        .filter_map(|e| match *e {
            BattleEvent::Damage { target, hp, source: src, .. } if target.pos.side == side && src == source => Some(hp),
            _ => None,
        })
        .collect()
}

#[test]
fn multi_hit_moves_report_their_hit_count() {
    let mut counts = [0; 6];
    for seed in 0..60 {
        // Blissey survives any number of Bullet Seeds from Snorlax.
        let mut b = battle((s::SNORLAX, m::BULLET_SEED), (s::BLISSEY, m::SPLASH), seed);
        let events = turn(&mut b);
        let hits = damage_to(&events, SideId::P2, DamageSource::Move).len();
        let reported = events.iter().find_map(|e| match *e {
            BattleEvent::HitCount { hits, .. } => Some(hits as usize),
            _ => None,
        });
        assert_eq!(reported, Some(hits), "seed {seed}");
        counts[hits] += 1;
    }
    assert!(counts[2] > 0 && counts[3] > 0 && counts[..2].iter().all(|&c| c == 0), "{counts:?}");
}

#[test]
fn seismic_toss_deals_the_users_level() {
    let mut b = battle((s::SNORLAX, m::SEISMIC_TOSS), (s::BLISSEY, m::SPLASH), 1);
    let max = b.state().side(SideId::P2).team[0].max_hp();
    let events = turn(&mut b);
    assert_eq!(damage_to(&events, SideId::P2, DamageSource::Move), [max - 50]);
}

#[test]
fn recover_fails_at_full_hp_and_heals_half_otherwise() {
    let mut b = battle((s::SNORLAX, m::SEISMIC_TOSS), (s::BLISSEY, m::RECOVER), 1);
    let max = b.state().side(SideId::P2).team[0].max_hp();
    // Blissey is faster than Snorlax: Recover fails, then Seismic Toss hits.
    let events = turn(&mut b);
    assert!(events.iter().any(|e| matches!(e, BattleEvent::HealFail { .. })));
    let events = turn(&mut b);
    let healed = events.iter().find_map(|e| match *e {
        BattleEvent::Heal { hp, source: HealSource::Move, .. } => Some(hp),
        _ => None,
    });
    assert_eq!(healed, Some(max));
}

#[test]
fn drain_and_recoil_follow_the_damage_dealt() {
    let mut b = battle((s::SNORLAX, m::DOUBLE_EDGE), (s::BLISSEY, m::SEISMIC_TOSS), 3);
    let events = turn(&mut b);
    let dealt = b.state().side(SideId::P2).team[0].max_hp() - damage_to(&events, SideId::P2, DamageSource::Move)[0];
    let snorlax = b.state().side(SideId::P1).team[0];
    let after_toss = snorlax.max_hp() - 50;
    let recoil = (dealt as u32 * 33 * 2 + 100) / 200;
    assert_eq!(damage_to(&events, SideId::P1, DamageSource::MoveRecoil), [after_toss - recoil as u16]);

    let mut b = battle((s::SNORLAX, m::DRAIN_PUNCH), (s::BLISSEY, m::SEISMIC_TOSS), 3);
    let events = turn(&mut b);
    assert!(events.iter().any(|e| matches!(e, BattleEvent::Heal { source: HealSource::Drain { .. }, .. })));
}

#[test]
fn explosion_faints_the_user_even_against_an_immune_target() {
    let mut b = battle((s::SNORLAX, m::EXPLOSION), (s::GENGAR, m::SPLASH), 1);
    let events = turn(&mut b);
    assert!(events.iter().any(|e| matches!(e, BattleEvent::Immune { .. })));
    assert!(events.iter().any(|e| matches!(e, BattleEvent::Faint { actor } if actor.pos.side == SideId::P1)));
    assert_eq!(b.result().and_then(|r| r.winner), Some(SideId::P2));
}

#[test]
fn sheer_cold_cannot_affect_ice_types() {
    let mut b = battle((s::SNORLAX, m::SHEER_COLD), (s::GLALIE, m::SPLASH), 1);
    let events = turn(&mut b);
    assert!(events.iter().any(|e| matches!(e, BattleEvent::OhkoImmune { .. })));
}
