//! Lifecycle, legality and termination.

mod common;

use common::{new_battle, play_random};
use pikapro::data::gen9::{moves as m, species as s};
use pikapro::{
    Battle, BattleEvent, BattleOptions, Choice, ChoiceError, EndReason, Format, PokemonSet, Progress, Request, Seed,
    SideId, SlotAction, Target, Team, TeamError,
};

#[test]
fn random_battles_terminate_in_every_format() {
    for format in Format::presets() {
        for seed in 0..200 {
            let mut battle = new_battle(format.clone(), seed);
            play_random(&mut battle, seed ^ 0xABCD);
            let result = battle.result().expect("battle over");
            assert!(result.turns <= format.turn_limit);
            if result.reason == EndReason::AllFainted
                && let Some(w) = result.winner
            {
                assert_eq!(battle.state().side(w.foe()).pokemon_left(), 0);
            }
        }
    }
}

#[test]
fn team_preview_brings_the_chosen_pokemon_in_order() {
    let mut battle = new_battle(Format::gen9_vgc(), 1);
    assert_eq!(battle.request(SideId::P1), Request::TeamPreview { bring: 4 });
    assert_eq!(battle.turn(), 0);
    assert_eq!(battle.submit(SideId::P1, Choice::team(&[5, 4, 3, 2])), Ok(Progress::Waiting));
    assert_eq!(battle.request(SideId::P1), Request::Wait);
    assert_eq!(battle.submit(SideId::P2, Choice::team(&[0, 1, 2, 3])), Ok(Progress::Resolved));
    let p1 = battle.state().side(SideId::P1);
    assert_eq!(p1.team.len(), 4);
    assert_eq!(p1.team[0].species, s::AMOONGUSS);
    assert_eq!(p1.active, [Some(0), Some(1)]);
    assert_eq!(battle.turn(), 1);
    assert_eq!(battle.request(SideId::P1), Request::Move);
    // VGC level cap applies.
    assert!(p1.team.iter().all(|p| p.level == 50));
}

/// Team preview without a picked team size (gen9ou): every Pokémon is
/// brought, in the chosen order, for a team of any size.
#[test]
fn a_team_preview_without_a_picked_size_brings_the_whole_team() {
    use pikapro::request::RequestKind;
    let format = Format::gen9_ou();
    let six = pikapro::testing::sample_team(&format);
    let level = six.sets[0].level;
    let mut three = six.clone();
    three.sets.truncate(3);
    let ids = |battle: &Battle, side| battle.state().side(side).team.iter().map(|p| p.id).collect::<Vec<_>>();
    let options = BattleOptions::new(Seed::fast(1));
    let mut battle = Battle::new(format, [six, three], options).unwrap();
    // Each side brings what it has.
    assert_eq!(battle.request(SideId::P1), Request::TeamPreview { bring: 6 });
    assert_eq!(battle.request(SideId::P2), Request::TeamPreview { bring: 3 });
    assert_eq!(battle.request_data(SideId::P1).kind, RequestKind::TeamPreview { max_chosen_team_size: None });
    assert!(battle.events().contains(&BattleEvent::TeamPreview { bring: None }));
    assert_eq!(battle.legal_actions(SideId::P2).joint().len(), 6);
    // A choice orders the whole team: a shorter one is not an order.
    assert_eq!(battle.submit(SideId::P1, Choice::team(&[2, 0, 1])), Err(ChoiceError::InvalidTeamOrder));
    assert_eq!(battle.submit(SideId::P2, Choice::team(&[2, 0, 1, 3])), Err(ChoiceError::InvalidTeamOrder));
    battle.submit(SideId::P1, Choice::team(&[2, 0, 1, 5, 4, 3])).unwrap();
    assert_eq!(battle.submit(SideId::P2, Choice::team(&[1, 2, 0])), Ok(Progress::Resolved));
    assert_eq!(ids(&battle, SideId::P1), [2, 0, 1, 5, 4, 3]);
    assert_eq!(ids(&battle, SideId::P2), [1, 2, 0]);
    assert_eq!(battle.state().side(SideId::P1).active[0], Some(0));
    let sizes: Vec<_> = battle
        .events()
        .iter()
        .filter_map(|e| match e {
            BattleEvent::TeamSize { size, .. } => Some(*size),
            _ => None,
        })
        .collect();
    assert_eq!(sizes, [6, 3]);
    // Set levels stand.
    assert!(battle.state().side(SideId::P1).team.iter().all(|p| p.level == level));
    assert_eq!(battle.request(SideId::P1), Request::Move);
    play_random(&mut battle, 7);
    assert!(battle.result().is_some());
}

#[test]
fn invalid_choices_are_rejected_without_side_effects() {
    let mut battle = new_battle(Format::gen9_vgc(), 1);
    let before = battle.state().to_owned();
    assert_eq!(battle.submit(SideId::P1, Choice::team(&[0, 0, 1, 2])), Err(ChoiceError::InvalidTeamOrder));
    assert_eq!(battle.submit(SideId::P1, Choice::team(&[0, 1])), Err(ChoiceError::InvalidTeamOrder));
    let pass = Choice::single(SlotAction::Pass);
    assert_eq!(battle.submit(SideId::P1, pass), Err(ChoiceError::WrongKind));
    assert_eq!(*battle.state(), before);

    battle.submit(SideId::P1, Choice::team(&[0, 1, 2, 3])).unwrap();
    assert_eq!(battle.submit(SideId::P1, Choice::team(&[0, 1, 2, 3])), Ok(Progress::Waiting));
    battle.submit(SideId::P2, Choice::team(&[0, 1, 2, 3])).unwrap();

    let mv = |index, target| SlotAction::Move { index, target, gimmick: None };
    // Doubles single-target moves need a valid target: not the user
    // itself. (No target at all is Showdown's `default`, a random one.)
    assert_eq!(
        battle.submit(SideId::P1, Choice::doubles(mv(0, Some(Target::Ally(0))), mv(0, Some(Target::Foe(0))))),
        Err(ChoiceError::IllegalAction { slot: 0 })
    );
    // Both slots switching to the same bench Pokémon.
    assert_eq!(
        battle.submit(SideId::P1, Choice::doubles(SlotAction::Switch(2), SlotAction::Switch(2))),
        Err(ChoiceError::DuplicateSwitch)
    );
    // Two Terastallizations in one turn.
    let tera =
        |index| SlotAction::Move { index, target: Some(Target::Foe(0)), gimmick: Some(pikapro::Gimmick::Terastallize) };
    assert_eq!(battle.submit(SideId::P1, Choice::doubles(tera(0), tera(0))), Err(ChoiceError::MultipleGimmicks));
    // Mega Evolution is not legal in a Tera format.
    let mega = SlotAction::Move { index: 0, target: Some(Target::Foe(0)), gimmick: Some(pikapro::Gimmick::MegaEvolve) };
    assert_eq!(
        battle.submit(SideId::P1, Choice::doubles(mega, mv(0, Some(Target::Foe(0))))),
        Err(ChoiceError::IllegalAction { slot: 0 })
    );
}

#[test]
fn terastallization_is_once_per_battle() {
    let mut battle = new_battle(Format::gen9_singles(), 3);
    let tera = SlotAction::Move { index: 0, target: None, gimmick: Some(pikapro::Gimmick::Terastallize) };
    assert!(battle.legal_actions(SideId::P1).slot(0).contains(&tera));
    battle.submit(SideId::P1, Choice::single(tera)).unwrap();
    battle.submit(SideId::P2, Choice::single(SlotAction::Move { index: 2, target: None, gimmick: None })).unwrap();
    assert!(battle.events().iter().any(|e| matches!(e, BattleEvent::Terastallize { .. })));
    assert!(battle.state().side(SideId::P1).team[0].terastallized);
    if !battle.is_over() && battle.request(SideId::P1) == Request::Move {
        let legal = battle.legal_actions(SideId::P1);
        assert!(legal.slot(0).iter().all(|a| !matches!(a, SlotAction::Move { gimmick: Some(_), .. })));
    }
}

#[test]
fn forfeit_ends_the_battle() {
    let mut battle = new_battle(Format::gen9_singles(), 1);
    battle.forfeit(SideId::P2);
    let result = battle.result().unwrap();
    assert_eq!(result.winner, Some(SideId::P1));
    assert_eq!(result.reason, EndReason::Forfeit);
    let any = Choice::single(SlotAction::Pass);
    assert_eq!(battle.submit(SideId::P1, any), Err(ChoiceError::BattleOver));
    assert!(battle.legal_actions(SideId::P1).is_waiting());
}

#[test]
fn turn_limit_ends_in_a_tie() {
    // Two Pokémon that can only Protect never damage each other.
    let format = Format { turn_limit: 5, ..Format::gen9_singles() };
    let team = || Team::new(vec![PokemonSet::new(s::PIKACHU).with_moves(&[m::PROTECT])]);
    let mut battle = Battle::new(format, [team(), team()], BattleOptions::new(Seed::fast(1))).unwrap();
    // 16 PP of Protect outlast a 5-turn limit.
    play_random(&mut battle, 1);
    let result = battle.result().unwrap();
    assert_eq!(result.winner, None);
    assert_eq!(result.reason, EndReason::TurnLimit);
    assert_eq!(result.turns, 5);
    // The battle's turn counter is one past the limit; the result counts
    // the turns played.
    assert_eq!(battle.turn(), 6);
    // The warnings count down to the configured limit, and the last message
    // names it.
    use pikapro::event::Message;
    let messages: Vec<Message> = battle
        .events()
        .iter()
        .filter_map(|e| match e {
            BattleEvent::Message { message } => Some(*message),
            _ => None,
        })
        .collect();
    assert_eq!(messages.first(), Some(&Message::AutoTieWarning { turns_left: 4, limit: 5 }));
    assert_eq!(messages.last(), Some(&Message::TurnLimit { limit: 5 }));
}

#[test]
fn struggle_when_out_of_pp() {
    let format = Format::gen9_singles();
    // Gholdengo is immune to Tackle; Pikachu runs out of PP and must Struggle,
    // which is typeless and does hit.
    let p1 = Team::new(vec![PokemonSet::new(s::PIKACHU).with_level(5).with_moves(&[m::TACKLE])]);
    let p2 = Team::new(vec![PokemonSet::new(s::GHOLDENGO).with_moves(&[m::PROTECT])]);
    let mut battle = Battle::new(format, [p1, p2], BattleOptions::new(Seed::fast(2))).unwrap();
    play_random(&mut battle, 2);
    let struggled =
        battle.events().iter().any(|e| matches!(e, BattleEvent::Move { move_id, .. } if *move_id == m::STRUGGLE));
    assert!(struggled);
    // Struggle recoil eventually knocks Pikachu out.
    assert_eq!(battle.result().unwrap().winner, Some(SideId::P2));
}

#[test]
fn team_validation() {
    let vgc = Format::gen9_vgc();
    let mon = |sp| PokemonSet::new(sp).with_moves(&[m::TACKLE]);
    let too_small = Team::new(vec![mon(s::PIKACHU)]);
    assert!(matches!(too_small.validate(&vgc), Err(TeamError::Size { .. })));
    let dupes = Team::new(vec![mon(s::PIKACHU), mon(s::PIKACHU), mon(s::GARCHOMP), mon(s::DRAGONITE)]);
    assert_eq!(dupes.validate(&vgc), Err(TeamError::SpeciesClause { index: 1 }));
    // Species Clause compares National Dex numbers: two formes of one
    // species break it.
    let formes = Team::new(vec![mon(s::ROTOM_WASH), mon(s::GARCHOMP), mon(s::ROTOM_HEAT), mon(s::DRAGONITE)]);
    assert_eq!(formes.validate(&vgc), Err(TeamError::SpeciesClause { index: 2 }));
    let urshifu = Team::new(vec![mon(s::URSHIFU), mon(s::URSHIFU_RAPID_STRIKE), mon(s::GARCHOMP), mon(s::DRAGONITE)]);
    assert_eq!(urshifu.validate(&vgc), Err(TeamError::SpeciesClause { index: 1 }));
    let base = Team::new(vec![mon(s::ROTOM), mon(s::GARCHOMP), mon(s::DRAGONITE), mon(s::ROTOM_WASH)]);
    assert_eq!(base.validate(&vgc), Err(TeamError::SpeciesClause { index: 3 }));
    // Without the clause they are fine.
    assert!(formes.validate(&Format { clauses: Default::default(), ..Format::gen9_vgc() }).is_ok());
    // Champions requires exactly six Pokémon...
    let champions = Format::champions_vgc();
    let four = vec![mon(s::PIKACHU), mon(s::GARCHOMP), mon(s::DRAGONITE), mon(s::AMOONGUSS)];
    assert!(matches!(Team::new(four).validate(&champions), Err(TeamError::Size { .. })));
    // ...with Stat Points, not EVs.
    let six = [s::PIKACHU, s::GARCHOMP, s::DRAGONITE, s::AMOONGUSS, s::INCINEROAR, s::RILLABOOM].map(mon).to_vec();
    assert_eq!(
        Team::new(six).validate(&champions),
        Err(TeamError::Set { index: 0, kind: pikapro::team::SetError::WrongStatSystem })
    );
    // An empty team is rejected even where the format's minimum is 0, and
    // a format that brings none has no valid team.
    let open = Format { team_size: (0, 6), ..Format::gen9_singles() };
    assert_eq!(Team::new(vec![]).validate(&open), Err(TeamError::Size { len: 0, min: 1, max: 6 }));
    let none = Format { team_preview: pikapro::TeamPreview::Pick(0), ..Format::gen9_vgc() };
    let four = Team::new(vec![mon(s::PIKACHU), mon(s::GARCHOMP), mon(s::DRAGONITE), mon(s::AMOONGUSS)]);
    assert!(four.validate(&vgc).is_ok());
    assert_eq!(four.validate(&none), Err(TeamError::Size { len: 4, min: 4, max: 0 }));
}

#[test]
fn level_rules_apply_at_battle_start() {
    use pikapro::testing::sample_team;
    let levels = |format: Format, level: u8| {
        let mut team = sample_team(&format);
        team.sets[0].level = level;
        let battle = Battle::new(format, [team.clone(), team], BattleOptions::new(Seed::fast(1))).unwrap();
        battle.state().side(SideId::P1).team.iter().map(|p| p.level).collect::<Vec<_>>()
    };
    // Scarlet/Violet: adjusted down only.
    assert_eq!(levels(Format::gen9_singles(), 30)[0], 30);
    // Champions: every Pokémon at exactly 50.
    let mut champions = levels(Format::champions_vgc(), 30);
    champions.dedup();
    assert_eq!(champions, [50]);
}

#[test]
fn struggle_into_protect_takes_no_recoil() {
    // Pikachu's only move can't touch Gholdengo; once it runs out of PP it
    // struggles into a Gholdengo that only knows Protect.
    let format = Format::gen9_singles();
    let p1 = Team::new(vec![PokemonSet::new(s::PIKACHU).with_moves(&[m::EXTREME_SPEED])]);
    let p2 = Team::new(vec![PokemonSet::new(s::GHOLDENGO).with_moves(&[m::PROTECT])]);
    let mut seen = 0;
    for seed in 0..20 {
        let mut battle =
            Battle::new(format.clone(), [p1.clone(), p2.clone()], BattleOptions::new(Seed::fast(seed))).unwrap();
        play_random(&mut battle, seed);
        let events = battle.events();
        for (i, e) in events.iter().enumerate() {
            let BattleEvent::Move { move_id, .. } = e else { continue };
            if *move_id != m::STRUGGLE || !matches!(events.get(i + 1), Some(BattleEvent::Protected { .. })) {
                continue;
            }
            seen += 1;
            let rest = events[i + 1..].iter().take_while(|e| !matches!(e, BattleEvent::Upkeep));
            for e in rest {
                assert!(
                    !matches!(e, BattleEvent::Damage { source: pikapro::event::DamageSource::Recoil, .. }),
                    "seed {seed}: recoil after a fully protected Struggle"
                );
            }
        }
    }
    assert!(seen > 0, "scenario never came up");
}

#[test]
fn effectiveness_is_reported_before_crits() {
    let mut crit_after_effectiveness = 0;
    for seed in 0..100 {
        let mut battle = new_battle(Format::gen9_singles(), seed);
        play_random(&mut battle, seed);
        for pair in battle.events().windows(2) {
            match pair {
                [BattleEvent::Crit { .. }, BattleEvent::SuperEffective { .. } | BattleEvent::Resisted { .. }] => {
                    panic!("seed {seed}: crit reported before effectiveness")
                }
                [BattleEvent::SuperEffective { .. } | BattleEvent::Resisted { .. }, BattleEvent::Crit { .. }] => {
                    crit_after_effectiveness += 1
                }
                _ => {}
            }
        }
    }
    assert!(crit_after_effectiveness > 0, "scenario never came up");
}

#[test]
fn replacing_a_choice_clears_it_even_when_invalid() {
    let set = PokemonSet::new(s::MEW).with_moves(&[m::SPLASH, m::CELEBRATE]);
    let team = Team::new(vec![set]);
    let original =
        Battle::new(Format::gen9_singles(), [team.clone(), team], BattleOptions::new(Seed::fast(1))).unwrap();
    let choice = |index| Choice::single(SlotAction::Move { index, target: None, gimmick: None });
    for side in SideId::BOTH {
        let mut battle = original.clone();
        battle.submit(side, choice(0)).unwrap();
        assert_eq!(battle.submit(side, choice(8)), Err(ChoiceError::IllegalAction { slot: 0 }));
        assert_eq!(battle.request(side), Request::Move);
        assert_eq!(battle.submit(side.foe(), choice(0)), Ok(Progress::Waiting));
        assert_eq!(battle.turn(), 1, "the rejected replacement removed the first answer");
        assert_eq!(battle.submit(side, choice(1)), Ok(Progress::Resolved));
        assert_eq!(battle.state().side(side).team[0].moves[0].pp, original.state().side(side).team[0].moves[0].pp);
        assert_eq!(battle.state().side(side).team[0].moves[1].pp, original.state().side(side).team[0].moves[1].pp - 1);

        let mut battle = original.clone();
        battle.submit(side, choice(0)).unwrap();
        assert_eq!(battle.submit(side, choice(1)), Ok(Progress::Waiting));
        assert_eq!(battle.submit(side.foe(), choice(0)), Ok(Progress::Resolved));
        assert_eq!(battle.state().side(side).team[0].moves[0].pp, original.state().side(side).team[0].moves[0].pp);
        assert_eq!(battle.state().side(side).team[0].moves[1].pp, original.state().side(side).team[0].moves[1].pp - 1);
    }
}
