//! The checks' own tests: they pass on battles the view gets right, and
//! fail on a view made wrong or leaky on purpose.

use pikapro::{Format, SideId};
use pikapro_data::gen9::items;
use pikapro_view::{ItemState, ViewConfig};

use super::sound;
use super::*;

fn setup(format: Format, source: Source, seed: u64) -> Setup {
    Generator::new(&format, source).expect("a generator").setup(seed)
}

/// gen9singles hides `|poke|` lines (no team preview): a tracker told
/// otherwise reads the engine's `Poke` events, which show nothing.
fn leaky(format: Format, seed: u64) -> Setup {
    let mut s = setup(format, Source::Sets, seed);
    let mut config = ViewConfig::new(&s.format);
    config.team_preview = true;
    s.view = Some(config);
    s
}

#[test]
fn the_checks_pass_battles_the_view_tracks() {
    for format in [Format::champions_vgc(), Format::gen9_singles()] {
        for seed in 4_500_000..4_500_003 {
            let s = setup(format.clone(), Source::Sets, seed);
            let played = play(&s, Driver::random(seed), Checks::ALL, |_| Ok(()))
                .unwrap_or_else(|f| panic!("{} {seed}:\n{f}", format.id));
            assert!(played.finished && played.changes > 0);
            meta::run(&s, 2).unwrap_or_else(|f| panic!("{} {seed}:\n{f}", format.id));
        }
    }
}

#[test]
fn an_event_that_shows_nothing_must_not_change_the_view() {
    let s = leaky(Format::gen9_singles(), 4_500_000);
    let f = play(&s, Driver::random(s.seed), Checks::ALL, |_| Ok(())).expect_err("the leak is caught");
    assert_eq!((f.check, f.fact.as_str()), ("leak", "Poke"), "{f}");
}

#[test]
fn a_reordered_team_must_not_show() {
    let s = leaky(Format::gen9_singles(), 4_500_001);
    let f = meta::run_kinds(&s, 2, &[meta::Kind::Order]).expect_err("the leak is caught");
    assert_eq!(f.check, "metamorphic", "{f}");
    // Rows in registration order break the team-order rule too.
    let mut order = Ok(());
    play(&s, Driver::random(s.seed), Checks::NONE, |u| {
        if u.index == 1 {
            order = sound::team_order(&s.format, u.tracker, &[0; 2]);
        }
        Ok(())
    })
    .expect("plays");
    assert_eq!(order.expect_err("rows leak the order").check, "team-order");
}

#[test]
fn soundness_catches_wrong_claims() {
    let s = setup(Format::champions_vgc(), Source::Sets, 4_500_002);
    let mut caught = Vec::new();
    play(&s, Driver::random(s.seed), Checks::NONE, |u| {
        let Some((row, rec)) = u.tracker.side(SideId::P1).active(0) else { return Ok(()) };
        if u.index != 3 {
            return Ok(());
        }
        let truth = u.battle.state().side(SideId::P1).active(0).expect("filled").item;
        let wrong = if truth == Some(items::LEFTOVERS) { items::SITRUS_BERRY } else { items::LEFTOVERS };
        let mut t = *u.tracker;
        t.sides[0].mons[row as usize].item = ItemState::Known(wrong);
        caught.push(sound::check(u.battle, &t, &mut [0; 2], &[]).expect_err("item"));
        let mut t = *u.tracker;
        t.sides[0].mons[row as usize].hp.hp = rec.hp.hp.wrapping_add(1);
        caught.push(sound::check(u.battle, &t, &mut [0; 2], &[]).expect_err("hp"));
        let mut t = *u.tracker;
        t.sides[0].slots[0].boosts[0] = 6;
        caught.push(sound::check(u.battle, &t, &mut [0; 2], &[]).expect_err("boost"));
        Ok(())
    })
    .expect("plays");
    let facts: Vec<&str> = caught.iter().map(|f| f.fact.as_str()).collect();
    assert_eq!(facts, ["item", "hp", "boosts"]);
}

#[test]
fn golden_fixtures_round_trip() {
    let s = setup(Format::champions_vgc(), Source::Sets, 4_500_000);
    let (facts, played) = golden::facts(&s, Driver::random(s.seed), Checks::ALL).expect("passes");
    let f = golden::Fixture {
        setup: s,
        source: "sets".into(),
        checks: Checks::ALL,
        meta: 1,
        choices: played.choices,
        records: facts[0].len(),
        facts,
    };
    let text = golden::write(&f);
    let back = golden::parse(&text).expect("parses");
    assert_eq!(back.facts, f.facts);
    assert_eq!(back.choices, f.choices);
    golden::replay(&back).expect("replays");
    assert_eq!(golden::write(&back), text);
}

#[test]
fn scripted_choices_can_include_or_omit_automatic_passes() {
    use pikapro::data::gen9::{abilities as a, moves as m, species as s};
    use pikapro::{PokemonSet, Request, SlotAction};

    for automatic_side in SideId::BOTH {
        let mut format = Format::gen9_doubles();
        format.team_preview = pikapro::TeamPreview::None;
        let pair = Team::new(vec![
            PokemonSet::new(s::TATSUGIRI).with_ability(a::COMMANDER).with_moves(&[m::SPLASH]),
            PokemonSet::new(s::DONDOZO).with_ability(a::UNAWARE).with_moves(&[m::DOODLE, m::MEMENTO]),
        ]);
        let others = Team::new(vec![PokemonSet::new(s::MEW).with_ability(a::NO_GUARD).with_moves(&[m::SPLASH]); 2]);
        let teams = if automatic_side == SideId::P1 { [pair, others] } else { [others, pair] };
        let setup = Setup { format, seed: 1, sheets: teams.map(TeamSheet::new), view: None };
        let names = setup.names();
        let mut battle = setup.battle();
        let splash = Choice::Actions([SlotAction::Move { index: 0, target: None, gimmick: None }; 2]);
        for text in ["pass, move 1 1", "pass, move 2 1"] {
            battle.submit(automatic_side.foe(), splash).unwrap();
            let choice = parse_choice_for(&battle, automatic_side, text, &names[automatic_side.index()]).unwrap();
            assert_eq!(battle.submit(automatic_side, choice), Ok(Progress::Resolved));
        }
        assert_eq!(battle.request(automatic_side), Request::Wait);
        for text in ["move 1", "switch 1", "team 12", "pass, move 1", "pass, pass, pass"] {
            assert!(parse_choice_for(&battle, automatic_side, text, &names[automatic_side.index()]).is_err(), "{text}");
        }
        for pass in [None, Some("pass, pass"), Some("pass"), Some("skip"), Some("default"), Some("auto")] {
            let mut b = battle.clone();
            let choices = SideId::BOTH
                .into_iter()
                .filter_map(|side| {
                    let text = if side == automatic_side { pass? } else { "move 1, move 1" };
                    Some((side, text.to_string()))
                })
                .collect::<Vec<_>>();
            let mut driver = Driver::script(choices.iter().chain(&choices).cloned().collect());
            let mut log = Vec::new();
            for turn in battle.turn() + 1..=battle.turn() + 2 {
                assert_eq!(driver.advance(&mut b, &names, &mut log), Ok(true), "{automatic_side:?} {pass:?}");
                assert_eq!(b.turn(), turn);
                assert_eq!(b.request(automatic_side), Request::Wait);
                assert!(b.legal_actions(automatic_side).joint().is_empty());
            }
            assert_eq!(log.len(), 2 * choices.len());
            assert_eq!(Driver::random(1).advance(&mut b, &names, &mut log), Ok(true));
            assert_eq!(log.last().unwrap().0, automatic_side.foe());
            assert_eq!(b.turn(), battle.turn() + 3);
        }
    }
}

#[test]
fn records_follow_the_schema() {
    let s = setup(Format::gen9_singles(), Source::Sets, 4_500_000);
    let (d, result) = dump::dump(&s, Driver::random(s.seed), Checks::NONE);
    let played = result.expect("plays");
    assert_eq!(d.records, 2 * played.updates);
    let first = d.decisions.lines().next().expect("a record");
    assert!(first.starts_with(r#"{"format":"gen9singles","seed":4500000,"side":"p1","index":0,"turn":"#));
    assert!(first.contains(r#","lines":["|t:|"#));
    assert!(first.contains(r#""request":"{"#) && first.contains(r#""teams":["#));
    let last = d.decisions.lines().last().expect("a record");
    assert!(last.contains(r#""request":null"#));
    let state = d.states.lines().next().expect("a record");
    assert!(state.contains(r#""state":{"role":"p1","turn":1,"#));
}
