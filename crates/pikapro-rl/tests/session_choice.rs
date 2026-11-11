//! A channel session's choice contract, the environment's: once a choice
//! answers the request, the player needn't act, its mask is all false and
//! another choice is refused, until the next request (or an `[Invalid
//! choice]`, which asks again).

use pikapro::{Format, SideId, SlotAction};
use pikapro_rl::session::{ChannelSession, SessionOptions};
use pikapro_view::OwnMoves;

mod common;

#[test]
fn a_choice_answers_the_request_until_the_next() {
    let lines = common::commander_channel();
    let mut s = ChannelSession::for_format(Format::champions_vgc(), false, SessionOptions::default()).unwrap();
    s.set_side(SideId::P1);
    let mut next = lines.iter();
    for line in next.by_ref() {
        s.feed(line);
        if s.needs_action() {
            break;
        }
    }
    assert!(s.needs_action());
    let mask = s.mask().to_vec();
    let slots = s.space().active * s.space().slot_size;
    let k = mask[slots..].iter().position(|&m| m).expect("team preview orders") as i64;
    let first = s.choice(&[k, 0]).unwrap();
    assert!(first.starts_with("team "), "{first}");

    // Answered: nothing to act on, as the environment says of a player
    // that waits.
    assert!(!s.needs_action());
    assert!(s.mask().iter().all(|&m| !m));
    assert_eq!(s.min_switches(), 0);
    assert_eq!(s.decode(&[k, 0]), None);
    let again = s.choice(&[k, 0]).unwrap_err();
    assert!(again.contains("answered"), "{again}");
    assert!(s.choose_builtin("random", 1).is_err());
    assert!(!s.legal().is_waiting(), "the request itself stands");

    // Showdown refused it as invalid: the same request asks again (unless
    // the error only says the choice came too late).
    s.feed("|error|[Invalid choice] Can't undo: It's too late to change your choice");
    assert!(!s.needs_action(), "a late error doesn't ask again");
    s.feed("|error|[Invalid choice] Can't choose for Team Preview: The choice was not valid");
    assert!(s.needs_action());
    assert_eq!(s.mask(), &mask[..]);
    assert_eq!(s.choice(&[k, 0]).unwrap(), first);
    assert!(s.mask().iter().all(|&m| !m));

    // The next request asks again.
    for line in next {
        s.feed(line);
        if s.needs_action() {
            break;
        }
    }
    assert!(s.needs_action() && s.mask().iter().any(|&m| m));
}

/// A format whose teams may be smaller than team preview's pick has no
/// action for such a preview: refused, as by the environment.
#[test]
fn formats_that_can_t_play_are_refused() {
    let format = Format { team_size: (1, 6), ..Format::gen9_vgc() };
    let e = ChannelSession::for_format(format, false, SessionOptions::default()).unwrap_err();
    assert!(e.contains("smaller than team preview's pick"), "{e}");
}

/// A session that rejoins after an `[Unavailable choice]` gets Showdown's
/// updated request (the room's last one, `update: true`), which shows what
/// the refusal taught (`trapped`, the move disabled): its overlay and
/// actions read it, and `updated` is the request's flag. Which of it the
/// refusal taught the request doesn't say, so `known_*` stay empty.
#[test]
fn a_rejoin_reads_an_updated_request() {
    let lines = common::commander_channel();
    let at = lines.iter().position(|l| l.starts_with("|request|{\"active\"")).unwrap();
    // The rejoin transcript: the log so far, then the last request.
    let log: Vec<&String> = lines[..at].iter().filter(|l| !l.starts_with("|request|")).collect();
    let refused = lines[at]
        .replacen("|request|{", "|request|{\"update\":true,", 1)
        .replacen("\"active\":[{\"moves\":", "\"active\":[{\"trapped\":true,\"moves\":", 1)
        .replacen("\"disabled\":false", "\"disabled\":true", 1);
    for (request, updated) in [(&refused, true), (&lines[at], false)] {
        let mut s = ChannelSession::for_format(Format::champions_vgc(), false, SessionOptions::default()).unwrap();
        s.set_side(SideId::P1);
        for line in &log {
            s.feed(line);
        }
        s.feed(request);
        assert!(s.needs_action() && s.error().is_none(), "{:?}", s.error());
        let own = *s.overlay();
        assert_eq!(own.updated, updated);
        assert_eq!((own.known_trapped, own.known_disabled), Default::default());
        let a = own.active[0].unwrap();
        let OwnMoves::Moves(ms) = a.moves else { panic!("{:?}", a.moves) };
        assert_eq!((a.trapped, ms[0].disabled), (updated, updated));
        let legal = s.legal();
        let switches = legal.slot(0).iter().any(|x| matches!(x, SlotAction::Switch(_)));
        let first = legal.slot(0).iter().any(|x| matches!(x, SlotAction::Move { index: 0, .. }));
        assert_eq!((switches, first), (!updated, !updated));
    }
}

#[test]
fn a_move_request_completed_by_automatic_passes_needs_no_action() {
    use pikapro::{Battle, BattleOptions, Choice, PokemonSet, Seed, Target, Team, TeamPreview};
    use pikapro_data::gen9::{abilities as a, moves as m, species as p};
    use pikapro_protocol::{Perspective, Renderer};
    use pikapro_rl::session::RequestKind;
    use pikapro_rl::vec::MaskLayout;
    use pikapro_view::OwnOverlay;

    for side in SideId::BOTH {
        for commander_slot in 0..2 {
            let format = Format { team_preview: TeamPreview::None, ..Format::gen9_doubles() };
            let bystander = PokemonSet::new(p::MEW).with_ability(a::NO_GUARD).with_moves(&[m::SPLASH]);
            let dondozo = PokemonSet::new(p::DONDOZO).with_ability(a::UNAWARE).with_moves(&[m::DOODLE, m::MEMENTO]);
            let tatsugiri = PokemonSet::new(p::TATSUGIRI).with_ability(a::COMMANDER).with_moves(&[m::SPLASH]);
            let pair = if commander_slot == 0 { vec![tatsugiri, dondozo] } else { vec![dondozo, tatsugiri] };
            let mut teams = [Team::new(vec![bystander]), Team::new(pair)];
            if side == SideId::P1 {
                teams.swap(0, 1);
            }
            let mut b = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(57_002_000))).unwrap();
            for index in [0, 1] {
                b.submit(side.foe(), Choice::single(SlotAction::Move { index: 0, target: None, gimmick: None }))
                    .unwrap();
                let mut actions = [SlotAction::Pass; 2];
                actions[1 - commander_slot] = SlotAction::Move { index, target: Some(Target::Foe(0)), gimmick: None };
                b.submit(side, Choice::Actions(actions)).unwrap();
            }
            assert_eq!(b.turn(), 3);
            let data = b.request_data(side);
            assert!(data.answered);
            assert!(matches!(data.kind, pikapro::request::RequestKind::Move { .. }));
            let json = Renderer::new(&format, Perspective::Omniscient).request_json(&data);
            for masks in [MaskLayout::Factored, MaskLayout::Joint] {
                let mut s =
                    ChannelSession::for_format(format.clone(), true, SessionOptions::default().with_masks(masks))
                        .unwrap();
                s.feed(&format!("|request|{json}"));
                assert!(s.error().is_none(), "{:?}", s.error());
                assert_eq!(s.request_kind(), RequestKind::Wait);
                assert!(!s.needs_action());
                let mut expected = OwnOverlay::from_battle(&b, side);
                // The wire condition is "0 fnt", without the old maximum HP.
                for mon in expected.mons.iter_mut().filter(|m| m.hp == 0) {
                    mon.max_hp = 0;
                }
                assert_eq!(*s.overlay(), expected);
                assert!(s.mask().iter().all(|&enabled| !enabled));
                assert!(s.legal().is_waiting());
                assert!(s.legal().joint().is_empty());
                assert!(s.choose_builtin("random", 1).is_err());
            }
        }
    }
}

#[test]
fn gen9_mega_stones_do_not_offer_unencodable_rl_actions() {
    use pikapro::{Battle, BattleOptions, Choice, Gimmick, PokemonSet, Seed, Team};
    use pikapro_data::gen9::{items, moves, species};
    use pikapro_protocol::{Perspective, Renderer};
    use pikapro_view::{OwnOverlay, PublicTracker, ViewConfig};

    let format = Format::gen9_singles();
    let team = Team::new(vec![
        PokemonSet::new(species::CHARIZARD).with_item(items::CHARIZARDITE_Y).with_moves(&[moves::SPLASH]),
    ]);
    let mut battle = Battle::new(format.clone(), [team.clone(), team], BattleOptions::new(Seed::fast(1))).unwrap();
    let mega = Choice::single(SlotAction::Move { index: 0, target: None, gimmick: Some(Gimmick::MegaEvolve) });
    assert!(battle.legal_actions(SideId::P1).is_legal(&mega));
    let data = battle.request_data(SideId::P1);
    let own = OwnOverlay::from_request(&data);
    let public = PublicTracker::new(ViewConfig::new(&format));
    let legal = pikapro_rl::env::shown(&battle, &own, &public, SideId::P1);
    assert!(!legal.is_legal(&mega));
    let mut session = ChannelSession::for_format(format.clone(), true, SessionOptions::default()).unwrap();
    session.feed(&format!("|request|{}", Renderer::new(&format, Perspective::Omniscient).request_json(&data)));
    assert!(session.needs_action());
    assert!(!session.legal().is_legal(&mega));
    assert_eq!(session.legal().joint(), legal.joint());
    assert_eq!((session.space().slot_size, session.space().size), (15, 15));
    for choice in legal.joint() {
        assert!(session.space().encode(&choice).is_some(), "{choice:?}");
    }
    // The unrestricted battle API still follows Showdown's simulator.
    battle.submit(SideId::P1, mega).unwrap();
    battle.submit(SideId::P2, mega).unwrap();
    assert_eq!(battle.state().side(SideId::P1).team[0].species, species::CHARIZARD_MEGA_Y);
}
