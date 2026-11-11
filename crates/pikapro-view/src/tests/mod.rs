use pikapro::testing::sample_team;
use pikapro::{Battle, BattleOptions, BattleRng, Format, Seed, SideId};
use pikapro_protocol::hp_shown;

use crate::*;

mod fuzz;
mod rules;
mod targeted;

fn battle(format: &Format, seed: u64) -> Battle {
    let teams = [sample_team(format), sample_team(format)];
    Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(seed))).unwrap()
}

/// Plays a random battle, calling `check` with the battle and the tracker
/// fed every event so far at each decision point.
fn play(format: &Format, seed: u64, mut check: impl FnMut(&Battle, &PublicTracker)) {
    let mut b = battle(format, seed);
    let mut tracker = PublicTracker::for_format(format);
    let mut policy = BattleRng::new(Seed::fast(!seed));
    tracker.apply_all(b.drain_events().as_slice());
    check(&b, &tracker);
    while !b.is_over() {
        let choices = SideId::BOTH.map(|s| {
            let legal = b.legal_actions(s);
            (!legal.is_waiting()).then(|| legal.sample(&mut policy))
        });
        for (s, c) in SideId::BOTH.into_iter().zip(choices) {
            if let Some(c) = c {
                b.submit(s, c).unwrap();
            }
        }
        for e in b.drain_events() {
            tracker.apply(&e);
        }
        check(&b, &tracker);
    }
}

#[test]
fn sizes_fit_the_budget() {
    let tracker = size_of::<PublicTracker>();
    let overlay = size_of::<OwnOverlay>();
    eprintln!(
        "sizes: tracker {tracker}, overlay {overlay}, side {}, mon {}, active {}, sheet {}",
        size_of::<PublicSide>(),
        size_of::<MonView>(),
        size_of::<ActiveView>(),
        size_of::<OpenSheet>()
    );
    // The view is `Copy` and copied often: raise these budgets deliberately.
    assert!(tracker <= 3608, "PublicTracker is {tracker} bytes");
    assert!(overlay <= 512, "OwnOverlay is {overlay} bytes");
    assert!(size_of::<MonView>() <= 80, "MonView is {} bytes", size_of::<MonView>());
}

/// What the skeleton tracks is true of the state (the sample teams have
/// no Illusion or Transform, so nothing is exempt).
#[test]
fn identity_hp_and_status_are_sound() {
    for format in [Format::champions_vgc(), Format::gen9_vgc(), Format::gen9_singles()] {
        for seed in 0..20 {
            play(&format, seed, |b, t| {
                for side in SideId::BOTH {
                    let state = b.state().side(side);
                    let public = t.side(side);
                    assert_eq!(public.fainted, state.total_fainted, "{} {seed}", format.id);
                    for slot in 0..format.active_per_side() {
                        let Some(mon) = state.active(slot) else { continue };
                        let (_, view) = public.active(slot).expect("an active slot is tracked");
                        assert_eq!(view.species, mon.species);
                        let (pct, hint) = hp_shown(format.mechanics, mon.hp, mon.max_hp());
                        assert_eq!((view.hp.hp, view.hp.max, view.hp.hint), (pct as u16, 100, hint));
                        assert_eq!(view.status, StatusKind::of(mon.status));
                        assert!(view.is_active() && view.is_revealed() && !view.is_fainted());
                    }
                }
                if let Some(r) = b.result() {
                    let expected = r.winner.map_or(Outcome::Tie, Outcome::Win);
                    assert_eq!((t.outcome, t.phase), (Some(expected), Phase::Over));
                }
            });
        }
    }
}

/// With team preview, rows are the `|poke|` order (the ids); without it,
/// the order of appearance, and nothing is known before a switch-in.
#[test]
fn rows_follow_preview_or_appearance() {
    let format = Format::champions_vgc();
    let b = battle(&format, 1);
    let mut t = PublicTracker::for_format(&format);
    t.apply_all(b.events());
    assert_eq!(t.phase, Phase::TeamPreview);
    for side in SideId::BOTH {
        assert_eq!(t.side(side).mons.len(), 6);
        let own = OwnOverlay::from_request(&b.request_data(side));
        let view = PlayerView::new(side, &t, &own);
        for m in own.mons.iter() {
            assert_eq!(view.own_row(m.id), Some(m.id));
        }
    }

    let format = Format::gen9_singles();
    let b = battle(&format, 1);
    let mut t = PublicTracker::for_format(&format);
    t.apply_all(b.events());
    assert_eq!(t.phase, Phase::Battle);
    for side in SideId::BOTH {
        let s = t.side(side);
        assert_eq!(s.mons.len(), 1, "only the lead is known");
        assert_eq!(s.active(0).map(|(row, _)| row), Some(0));
        assert_eq!(s.reveal_order.as_ref(), [0]);
    }
}

#[test]
fn overlay_shows_the_request() {
    let format = Format::champions_vgc();
    play(&format, 3, |b, t| {
        for side in SideId::BOTH {
            let data = b.request_data(side);
            let own = OwnOverlay::from_request(&data);
            assert_eq!(own.mons.len(), data.pokemon.len());
            for (m, p) in own.mons.iter().zip(data.pokemon.iter()) {
                assert_eq!((m.id, m.hp, m.max_hp, m.status), (p.id, p.hp, p.max_hp, StatusKind::of(p.status)));
            }
            let view = PlayerView::new(side, t, &own);
            assert_eq!(view.own_mons().count(), own.mons.len());
        }
    });
}

/// Exact reports keep the exact HP.
#[test]
fn exact_hp_reports() {
    let format = Format::gen9_vgc();
    let mut b = battle(&format, 5);
    let mut t = PublicTracker::new(ViewConfig::new(&format).with_hp(HpReport::Exact));
    let mut policy = BattleRng::new(Seed::fast(5));
    for _ in 0..6 {
        for s in SideId::BOTH {
            let legal = b.legal_actions(s);
            if !legal.is_waiting() {
                let c = legal.sample(&mut policy);
                b.submit(s, c).unwrap();
            }
        }
    }
    t.apply_all(b.events());
    for side in SideId::BOTH {
        for slot in 0..2 {
            if let (Some(mon), Some((_, v))) = (b.state().side(side).active(slot), t.side(side).active(slot)) {
                assert_eq!((v.hp.hp, v.hp.max), (mon.hp, mon.max_hp()));
            }
        }
    }
}
