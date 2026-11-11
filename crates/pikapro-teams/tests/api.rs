//! The generator's contract: determinism, legality (by pikapro-validate; the
//! fuzzer checks Showdown's verdict), options and restrictions.

use pikapro::{Format, Investment, Team};
use pikapro_data::gen9::{items, moves, species};
use pikapro_teams::{Error, Mode, Options, Restrict, Spreads, TeamGenerator};

fn check(g: &TeamGenerator, seeds: std::ops::Range<u64>) -> Vec<Team> {
    seeds
        .map(|seed| {
            let team = g.team(seed).expect("a team");
            // A random battle's teams are Showdown's generator's, which
            // nothing validates (its sets play at levels the validator's
            // format doesn't always allow for a set's moves).
            if g.random_battle().is_some() {
                team.validate(g.format()).expect("the engine accepts it");
                return team;
            }
            if let Err(p) = g.validator().validate(&team) {
                panic!("{} {:?} seed {seed}: {:?}", g.format().id, g.mode(), g.validator().messages(&p));
            }
            team.validate(g.format()).expect("the engine accepts it");
            // Showdown's server plays a team as its validator leaves it; a
            // generated team is already that (no battle-only forme listed,
            // no forme a move or an item would set).
            assert_eq!(
                g.validator().played(&team),
                team,
                "{} {:?} seed {seed}: played as drawn",
                g.format().id,
                g.mode()
            );
            team
        })
        .collect()
}

#[test]
fn every_preset_and_mode_gives_legal_deterministic_teams() {
    for format in Format::presets() {
        for mode in Mode::ALL {
            let g = TeamGenerator::new(format, mode).expect("generator");
            let teams = check(&g, 2_300_000..2_300_020);
            assert!(teams.iter().all(|t| t.sets.len() == 6));
            assert_eq!(teams[0], g.team(2_300_000).unwrap(), "deterministic");
            assert_ne!(teams[0], teams[1]);
            // Level 50 where the format battles at 50 (VGC, BSS), the species'
            // own level in a random battle's generated teams, else 100.
            let level = if format.level == pikapro::format::LevelRule::AsSet { 100 } else { 50 };
            let sets = || teams.iter().flat_map(|t| &t.sets);
            assert_eq!(g.random_battle().is_some(), format.random_teams && mode == Mode::Sets);
            if g.random_battle().is_some() {
                assert!(sets().all(|s| (50..=100).contains(&s.level)));
                assert!(sets().any(|s| s.level != 100), "{}: every set at level 100", format.id);
            } else {
                assert!(sets().all(|s| s.level == level));
            }
            let generated = g.generate(2_300_000).unwrap();
            assert_eq!(generated.team, teams[0]);
            assert_eq!(generated.shiny.len(), 6);
        }
    }
}

/// A random battle's teams are Showdown's generator's: the team for a seed
/// is the one for its Gen 5 seed, and the generator takes no options.
#[test]
fn random_battle_teams_are_the_generators() {
    use pikapro_teams::random_battle::{self, RandomBattle};
    for (format, kind) in [
        (Format::gen9_random_battle(), RandomBattle::Singles),
        (Format::gen9_random_doubles_battle(), RandomBattle::Doubles),
    ] {
        let g = TeamGenerator::new(&format, Mode::Sets).unwrap();
        assert_eq!(g.random_battle(), Some(kind));
        for seed in [0, 7, 0x0123_4567_89ab_cdef] {
            let mut rng = pikapro::BattleRng::new(random_battle::gen5_seed(seed));
            let sets = random_battle::generate(kind, &mut rng).unwrap();
            let generated = g.generate(seed).unwrap();
            assert_eq!(generated.team, random_battle::to_team(&sets));
            assert_eq!(generated.shiny, sets.iter().map(|s| s.shiny).collect::<Vec<_>>());
        }
        let o = Options { level: Some(50), ..Options::default() };
        assert_eq!(TeamGenerator::with_options(&format, Mode::Sets, o).err(), Some(Error::RandomBattleOptions));
        assert!(
            TeamGenerator::with_options(&format, Mode::Uniform, Options { level: Some(50), ..Options::default() })
                .is_ok()
        );
    }
}

#[test]
fn teams_are_played_as_drawn() {
    for format in Format::presets() {
        for mode in Mode::ALL {
            let g = TeamGenerator::new(format, mode).expect("generator");
            check(&g, 2_310_000..2_310_300);
        }
    }
}

#[test]
fn champions_teams_use_stat_points_and_megas() {
    let g = TeamGenerator::new(&Format::champions_vgc(), Mode::Uniform).expect("generator");
    let teams = check(&g, 2_300_100..2_300_160);
    let sets = || teams.iter().flat_map(|t| &t.sets);
    assert!(sets().all(|s| matches!(s.investment, Investment::StatPoints(_)) && s.tera_type.is_none()));
    // Megas hold their own stones, and only they hold Mega Stones.
    let dex = g.format().dex;
    let megas: Vec<_> = sets().filter(|s| s.item.is_some_and(|i| !dex.item(i).mega.is_empty())).collect();
    assert!(!megas.is_empty());
    for s in megas {
        let stone = dex.item(s.item.unwrap());
        assert!(
            stone.mega.iter().any(|&(base, _)| base == s.species),
            "{} holds {}",
            dex.species(s.species).name,
            stone.name
        );
    }
}

#[test]
fn formes_hold_their_required_items() {
    let g = TeamGenerator::new(&Format::gen9_vgc(), Mode::Uniform).expect("generator");
    let dex = g.format().dex;
    let mut seen = 0;
    for team in check(&g, 2_300_200..2_300_400) {
        for s in &team.sets {
            let required = dex.species(s.species).required_items;
            if !required.is_empty() {
                seen += 1;
                assert!(s.item.is_some_and(|i| required.contains(&i)), "{}", dex.species(s.species).name);
            }
            if s.species == species::OGERPON_WELLSPRING {
                assert_eq!(s.item, Some(items::WELLSPRING_MASK));
            }
            if let Some(t) = dex.species(s.species).required_tera_type {
                assert_eq!(s.tera_type, Some(t.ty()));
            }
        }
    }
    assert!(seen > 0, "no forme with a required item in 200 teams");
}

#[test]
fn restrictions_and_options_apply() {
    let allowed = vec![moves::PROTECT, moves::TACKLE, moves::REST, moves::SUBSTITUTE, moves::FACADE, moves::ENDURE];
    let options = Options {
        level: Some(77),
        team_size: Some(4),
        restrict: Restrict { moves: Some(allowed.clone()), ..Restrict::default() },
        ..Options::default()
    };
    let g = TeamGenerator::with_options(&Format::gen9_singles(), Mode::Uniform, options).expect("generator");
    for team in check(&g, 2_300_500..2_300_510) {
        assert_eq!(team.sets.len(), 4);
        for s in &team.sets {
            assert_eq!(s.level, 77);
            assert!(s.moves.iter().all(|m| allowed.contains(m)));
        }
    }
    let options = Options { spreads: Spreads::Showdown, ..Options::default() };
    let g = TeamGenerator::with_options(&Format::gen9_vgc(), Mode::Sets, options).expect("generator");
    for team in check(&g, 2_300_600..2_300_610) {
        for s in &team.sets {
            let Investment::Evs { evs, .. } = s.investment else { panic!("EVs") };
            assert!(evs.0.iter().all(|&e| e == 85 || e == 0));
        }
    }
}

/// Level 50 in a format that allows level 100 without adjusting it (the
/// custom games): Showdown rejects an EV total that is a multiple of 4, so
/// the generator adds one EV.
#[test]
fn level_50_custom_game_teams_are_legal() {
    for format in [pikapro::Format::gen9_singles(), pikapro::Format::gen9_doubles()] {
        for mode in [pikapro_teams::Mode::Uniform, pikapro_teams::Mode::Sets] {
            let options = pikapro_teams::Options { level: Some(50), ..Default::default() };
            let g = pikapro_teams::TeamGenerator::with_options(&format, mode, options).expect("generator");
            for seed in 0..50 {
                assert!(g.validator().validate(&g.team(seed).unwrap()).is_ok(), "{} {} {seed}", format.id, mode.name());
            }
        }
    }
}

fn restricted(restrict: Restrict) -> Options {
    Options { restrict, ..Options::default() }
}

/// Item restrictions in uniform mode: a set holds no item once every
/// allowed item is taken (Item Clause), or when none is allowed.
#[test]
fn uniform_sets_go_without_items_when_none_is_free() {
    let only = |items: Vec<pikapro_data::ItemId>| restricted(Restrict { items: Some(items), ..Restrict::default() });
    let g = TeamGenerator::with_options(&Format::gen9_vgc(), Mode::Uniform, only(vec![items::LEFTOVERS])).unwrap();
    for team in check(&g, 0..10) {
        let held: Vec<_> = team.sets.iter().filter_map(|s| s.item).collect();
        assert_eq!(held, [items::LEFTOVERS], "Item Clause: one holder");
    }
    for format in [Format::gen9_vgc(), Format::gen9_singles()] {
        let g = TeamGenerator::with_options(&format, Mode::Uniform, only(vec![])).unwrap();
        for team in check(&g, 0..5) {
            assert!(team.sets.iter().all(|s| s.item.is_none()));
        }
    }
}

/// Options no team satisfies are errors when the generator is built, and
/// a generator whose every draw fails gives an error, not a panic.
#[test]
fn impossible_options_are_errors() {
    let vgc = Format::gen9_vgc();
    for mode in Mode::ALL {
        for level in [0, 101, 200] {
            let o = Options { level: Some(level), ..Options::default() };
            let e = TeamGenerator::with_options(&vgc, mode, o).err();
            assert_eq!(e, Some(Error::Level { level, min: 1, max: 100 }), "{mode:?}");
        }
        // Six restricted legendaries, at most two per team (Reg I).
        let legends = vec![
            species::KYOGRE,
            species::GROUDON,
            species::DIALGA,
            species::PALKIA,
            species::LUGIA,
            species::HO_OH,
            species::MEWTWO,
        ];
        let o = restricted(Restrict { species: Some(legends), ..Restrict::default() });
        let e = TeamGenerator::with_options(&vgc, mode, o).err();
        assert_eq!(e, Some(Error::TeamRules { available: 2, team_size: 6 }), "{mode:?}");
    }
    // Random-battle sets all hold items: under Item Clause, only one set
    // can hold the one allowed item.
    let o = restricted(Restrict { items: Some(vec![items::LEFTOVERS]), ..Restrict::default() });
    let e = TeamGenerator::with_options(&vgc, Mode::Sets, o).err();
    assert!(matches!(e, Some(Error::TeamRules { team_size: 6, .. })), "{e:?}");
    // Species that can learn none of the allowed moves: every draw fails.
    let o = restricted(Restrict {
        species: Some(vec![species::KYOGRE, species::GROUDON, species::LUGIA, species::HO_OH]),
        moves: Some(vec![moves::SPACIAL_REND]),
        ..Restrict::default()
    });
    let o = Options { team_size: Some(4), ..o };
    let g = TeamGenerator::with_options(&Format::gen9_singles(), Mode::Uniform, o).unwrap();
    assert!(matches!(g.team(1), Err(Error::NoTeam { .. })));
}
