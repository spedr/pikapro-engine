//! The public API on hand-built teams.

use pikapro::{Format, Investment, PokemonSet, Team};
use pikapro_data::gen9::{abilities, items, moves, species};
use pikapro_data::{Nature, Stats, Type};
use pikapro_validate::cases::{parse_set, set_line};
use pikapro_validate::{Invalid, ProblemKind, SetInfo, ValidationFormat, Validator, check_preset, validate_preset};

fn vgc_team() -> Team {
    let evs = Investment::Evs { evs: Stats::new(252, 4, 0, 252, 0, 0), ivs: Stats::splat(31) };
    let set = |s, a, i, ms: &[_]| {
        PokemonSet::new(s)
            .with_level(50)
            .with_ability(a)
            .with_item(i)
            .with_nature(Nature::Modest)
            .with_investment(evs)
            .with_tera_type(Type::Water)
            .with_moves(ms)
    };
    Team::new(vec![
        set(species::INCINEROAR, abilities::INTIMIDATE, items::SITRUS_BERRY, &[moves::FAKE_OUT, moves::FLARE_BLITZ]),
        set(species::RILLABOOM, abilities::GRASSY_SURGE, items::ASSAULT_VEST, &[moves::FAKE_OUT, moves::GRASSY_GLIDE]),
        set(species::AMOONGUSS, abilities::REGENERATOR, items::ROCKY_HELMET, &[moves::SPORE, moves::RAGE_POWDER]),
        set(species::URSHIFU_RAPID_STRIKE, abilities::UNSEEN_FIST, items::CHOICE_SCARF, &[moves::SURGING_STRIKES]),
        set(species::FLUTTER_MANE, abilities::PROTOSYNTHESIS, items::BOOSTER_ENERGY, &[moves::MOONBLAST]),
        set(species::MIRAIDON, abilities::HADRON_ENGINE, items::CHOICE_SPECS, &[moves::ELECTRO_DRIFT]),
    ])
}

#[test]
fn a_legal_vgc_team_passes() {
    let v = Validator::new(ValidationFormat::gen9_vgc_2025_reg_i());
    let r = v.validate(&vgc_team());
    assert!(r.is_ok(), "{:?}", r.map_err(|p| v.messages(&p)));
    assert!(validate_preset(&Format::gen9_vgc(), &vgc_team(), &[]).is_ok());
}

/// `validateSet` ends with `set.level = adjustLevel` for a valid set: Reg
/// I (Adjust Level = 50) plays a level-30 or level-100 set at 50.
#[test]
fn the_played_team_has_the_adjusted_level() {
    let mut team = vgc_team();
    team.sets[0].level = 30;
    team.sets[1].level = 100;
    let played = validate_preset(&Format::gen9_vgc(), &team, &[]).expect("valid");
    assert!(played.sets.iter().all(|s| s.level == 50));
    // Custom games (Anything Goes) keep the given level.
    let mut team = vgc_team();
    team.sets.truncate(1);
    team.sets[0].level = 30;
    let played = validate_preset(&Format::gen9_doubles(), &team, &[]).expect("valid");
    assert_eq!(played.sets[0].level, 30);
}

#[test]
fn problems_are_typed() {
    let v = Validator::new(ValidationFormat::gen9_vgc_2025_reg_i());
    let mut team = vgc_team();
    team.sets[0].moves.push(moves::SURF);
    team.sets[5].item = Some(items::SITRUS_BERRY);
    let problems = v.validate(&team).unwrap_err();
    assert_eq!(problems[0].set, Some(0));
    assert!(matches!(problems[0].kind, ProblemKind::CantLearn { .. }));
    assert_eq!(problems[1].set, None);
    assert!(matches!(problems[1].kind, ProblemKind::ItemClause { limit: 1, item } if item == items::SITRUS_BERRY));
    assert_eq!(
        v.messages(&problems),
        [
            "Incineroar can't learn Surf.",
            "You are limited to 1 of each item by Item Clause.",
            "(You have more than 1 Sitrus Berry)",
        ]
    );
}

#[test]
fn set_info_reaches_the_clauses() {
    let v = Validator::new(ValidationFormat::gen9_vgc_2025_reg_i());
    let info = vec![SetInfo { nickname: Some("Twin".into()), ..SetInfo::default() }; 2];
    let messages = v.messages(&v.validate_with(&vgc_team(), &info).unwrap_err());
    assert_eq!(messages, ["Your Pokémon must have different nicknames.", "(You have more than one Twin)"]);
}

#[test]
fn structure_is_checked_after_showdowns_rules() {
    let mut team = vgc_team();
    team.sets[0].investment = Investment::StatPoints(Stats::splat(1));
    assert!(matches!(validate_preset(&Format::gen9_vgc(), &team, &[]), Err(Invalid::Structure(_))));
    // Custom games validate as Anything Goes with Species Clause: levels
    // aren't adjusted (level 100 here), Item Clause doesn't apply, Species
    // Clause does.
    let mut team = vgc_team();
    for set in &mut team.sets {
        set.level = 100;
    }
    team.sets[1].item = team.sets[0].item;
    assert!(validate_preset(&Format::gen9_doubles(), &team, &[]).is_ok());
    team.sets[1] = team.sets[0].clone();
    assert!(matches!(validate_preset(&Format::gen9_doubles(), &team, &[]), Err(Invalid::Problems(_))));
}

/// Species Clause counts National Dex numbers, here and in the engine's
/// structural check: two Rotom formes break it.
#[test]
fn species_clause_counts_formes() {
    let mut team = vgc_team();
    let rotom = |s, i| {
        PokemonSet::new(s)
            .with_level(50)
            .with_ability(abilities::LEVITATE)
            .with_item(i)
            .with_investment(Investment::Evs { evs: Stats::new(252, 0, 0, 252, 4, 0), ivs: Stats::splat(31) })
            .with_tera_type(Type::Water)
            .with_moves(&[moves::THUNDERBOLT])
    };
    team.sets[4] = rotom(species::ROTOM_WASH, items::LEFTOVERS);
    team.sets[5] = rotom(species::ROTOM_HEAT, items::FOCUS_SASH);
    let v = Validator::new(ValidationFormat::gen9_vgc_2025_reg_i());
    let problems = v.validate(&team).unwrap_err();
    assert!(
        problems.iter().any(|p| matches!(p.kind, ProblemKind::SpeciesClause { base } if base == species::ROTOM)),
        "{:?}",
        v.messages(&problems)
    );
    assert!(matches!(validate_preset(&Format::gen9_vgc(), &team, &[]), Err(Invalid::Problems(_))));
    assert_eq!(team.validate(&Format::gen9_vgc()), Err(pikapro::TeamError::SpeciesClause { index: 5 }));
    // One Rotom is fine.
    team.sets[5] = vgc_team().sets[5].clone();
    let r = v.validate(&team);
    assert!(r.is_ok(), "{:?}", r.map_err(|p| v.messages(&p)));
}

#[test]
fn case_lines_round_trip() {
    let dex = &pikapro_data::gen9::DEX;
    let team = vgc_team();
    let info = SetInfo { nickname: Some("Kitty".into()), gender: Some(pikapro_validate::Gender::Male), shiny: true };
    let line = set_line(dex, &team.sets[0], &info);
    let (set, back) = parse_set(dex, &line).unwrap();
    assert_eq!(set, team.sets[0]);
    assert_eq!(back, info);
}

/// Showdown's validator returns the team with its names cleaned
/// (`Dex.getName`, a nickname naming its own species becomes the base
/// species), and the battle plays those.
#[test]
fn validation_cleans_the_names() {
    let v = Validator::new(ValidationFormat::gen9_vgc_2025_reg_i());
    let nick = |n: &str| SetInfo { nickname: Some(n.into()), ..SetInfo::default() };
    let info = [nick("A|B  [c],"), nick("rillaboom"), SetInfo::default(), nick("Amoon\u{300}\u{301}\u{302}"), nick("")];
    let checked = v.check(&vgc_team(), &info);
    assert!(checked.problems.is_empty(), "{:?}", v.messages(&checked.problems));
    assert_eq!(checked.names, ["A B c", "Rillaboom", "Amoonguss", "Amoon", "Flutter Mane", "Miraidon"]);
    let preset = check_preset(&Format::gen9_vgc(), &vgc_team(), &info);
    assert!(preset.messages.is_empty());
    assert_eq!(preset.names, checked.names);
    let mut team = vgc_team();
    team.sets[0].moves.push(moves::SURF);
    assert_eq!(check_preset(&Format::gen9_vgc(), &team, &[]).messages, ["Incineroar can't learn Surf."]);
}

/// Expected: the `set.name` Showdown 0.11.11's validator leaves Incineroar
/// with (a lone surrogate from its cut is U+FFFD here), and whether it
/// finds problems.
#[test]
fn names_are_cleaned_as_showdown_cleans_them() {
    let v = Validator::new(ValidationFormat::gen9_vgc_2025_reg_i());
    let cases = [
        ("A|B  [c],", "A B c", true),
        ("  lead\ttrail  ", "lead trail", true),
        ("Zoë\u{85}x", "Zoë\u{85}x", true),
        ("\u{feff}BOM\u{feff}", "BOM", true),
        ("Amoon\u{300}\u{301}\u{302}", "Amoon", true),
        ("a\u{300}\u{301}b", "a\u{300}\u{301}b", true),
        ("x\u{239b}y\u{23b9}z", "xyz", true),
        ("12345678901234567\u{1f600}", "12345678901234567\u{fffd}", false),
        ("1234567890123456 xyz", "1234567890123456 x", false),
        ("incineroar", "Incineroar", true),
        ("INCINEROAR", "Incineroar", true),
        ("Incineroar-Gmax", "Incineroar-Gmax", true),
        ("\u{202e}evil", "evil", true),
        ("[]", "Incineroar", true),
        ("Mr. Nice Guy", "Mr. Nice Guy", true),
        ("Litten", "Litten", false),
        ("tab\tin", "tab in", true),
    ];
    for (nickname, name, valid) in cases {
        let info = [SetInfo { nickname: Some(nickname.into()), ..SetInfo::default() }];
        let checked = v.check(&vgc_team(), &info);
        assert_eq!(checked.names[0], name, "{nickname:?}");
        assert_eq!(checked.problems.is_empty(), valid, "{nickname:?}: {:?}", v.messages(&checked.problems));
    }
}

/// `pikapro::testing::sample_team` is legal in its own preset's
/// validation format (Reg M-C, Reg I, Anything Goes with Species Clause),
/// so a team pool built from sample teams isn't empty.
#[test]
fn sample_teams_are_legal_in_their_presets() {
    for format in Format::presets() {
        let team = pikapro::testing::sample_team(format);
        let check = check_preset(format, &team, &[]);
        assert!(check.messages.is_empty(), "{}: {:?}", format.id, check.messages);
        assert!(validate_preset(format, &team, &[]).is_ok(), "{}", format.id);
    }
}

/// IDs outside the dex (the ID types are public tuple structs) are errors,
/// not panics, in every validation format and preset.
#[test]
fn unknown_ids_are_errors() {
    use pikapro::{SetError, TeamError};
    use pikapro_data::{AbilityId, ItemId, MoveId, SpeciesId};
    let base = vgc_team().sets[0].clone();
    let cases = [
        (PokemonSet { species: SpeciesId(60000), ..base.clone() }, SetError::UnknownSpecies),
        (PokemonSet { ability: AbilityId(60000), ..base.clone() }, SetError::UnknownAbility),
        (PokemonSet { item: Some(ItemId(60000)), ..base.clone() }, SetError::UnknownItem),
        (base.clone().with_moves(&[moves::FAKE_OUT, MoveId(60000)]), SetError::UnknownMove),
    ];
    for (set, error) in cases {
        let mut team = vgc_team();
        team.sets[2] = set;
        for vf in ValidationFormat::all() {
            let v = Validator::new(vf.clone());
            let problems = v.validate(&team).unwrap_err();
            assert!(
                problems.iter().any(|p| p.set == Some(2)
                    && matches!(p.kind, ProblemKind::UnknownId { error: e, id: 60000 } if e == error)),
                "{}: {:?}",
                vf.id,
                v.messages(&problems)
            );
            assert!(v.messages(&problems).iter().any(|m| m.contains("ID 60000 does not exist")));
            assert_eq!(v.check(&team, &[]).names.len(), 6);
            let one = v.set_problems(&team.sets[2], &SetInfo::default());
            assert!(matches!(one[..], [ProblemKind::UnknownId { error: e, .. }] if e == error));
        }
        for format in Format::presets() {
            let expected = Invalid::Structure(TeamError::Set { index: 2, kind: error });
            assert_eq!(validate_preset(format, &team, &[]), Err(expected), "{}", format.id);
            let check = check_preset(format, &team, &[]);
            assert_eq!(check.messages.len(), 1, "{}: {:?}", format.id, check.messages);
            assert_eq!(check.names.len(), 6);
        }
    }
}

/// A team that lists its Megas' formes (as pastes do: `Charizard-Mega-Y @
/// Charizardite Y`) is played as Showdown's validator leaves it: each in the
/// forme it Mega Evolves from, a Mega's ability becoming that forme's first
/// (`validateForme`, then `validateSet`'s ability fix).
#[test]
fn listed_battle_only_formes_are_played_as_validation_leaves_them() {
    let format = Format::champions_vgc();
    let sample = pikapro::testing::sample_team(&format);
    let dex = format.dex;
    let mut listed = sample.clone();
    let mut megas = 0;
    for set in &mut listed.sets {
        let Some(&(_, mega)) = set.item.and_then(|i| dex.item(i).mega.iter().find(|&&(base, _)| base == set.species))
        else {
            continue;
        };
        set.species = mega;
        // The Mega's ability, where the base forme has its first.
        if set.ability == dex.species(set.species).abilities.primary {
            set.ability = dex.species(mega).abilities.primary;
        }
        megas += 1;
    }
    assert!(megas >= 2, "the sample team has {megas} Mega Stones");
    assert_ne!(listed, sample);
    assert_eq!(validate_preset(&format, &listed, &[]), Ok(sample.clone()));
    let check = check_preset(&format, &listed, &[]);
    assert!(check.messages.is_empty(), "{:?}", check.messages);
    assert_eq!(check.team, sample);
    let v = Validator::new(ValidationFormat::champions_vgc_2026_reg_mc());
    assert_eq!(v.played(&listed), sample);
    assert_eq!(v.played(&sample), sample, "validation leaves a validated team alone");
    // The Crowned heroes: Behemoth Blade is Iron Head until the battle
    // crowns Zacian.
    let crowned = PokemonSet::new(species::ZACIAN_CROWNED)
        .with_level(50)
        .with_ability(abilities::INTREPID_SWORD)
        .with_item(items::RUSTED_SWORD)
        .with_moves(&[moves::BEHEMOTH_BLADE, moves::PROTECT]);
    let played = Validator::new(ValidationFormat::gen9_vgc_2025_reg_i()).played(&Team::new(vec![crowned]));
    assert_eq!(played.sets[0].species, species::ZACIAN);
    assert_eq!(played.sets[0].moves[..], [moves::IRON_HEAD, moves::PROTECT]);
}
