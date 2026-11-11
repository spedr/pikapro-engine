//! Replays recorded teams: each case is a team pikapro-teams generated
//! (`<preset>-<mode>-<seed>`, default options) with Showdown's verdict on
//! it, recorded by `teams_fuzz --record` (see the header of golden.txt).
//! The generator must still give exactly that team, and pikapro-validate must
//! still agree with Showdown. No Node needed.
//!
//! A deliberate change to what the generator draws changes these teams:
//! re-record them (and rerun the fuzzer).

use std::collections::BTreeMap;

use pikapro::Format;
use pikapro_teams::{Mode, TeamGenerator};
use pikapro_validate::SetInfo;
use pikapro_validate::cases::{parse_cases, set_line};

#[test]
fn recorded_teams_are_generated_again_and_valid() {
    let cases = parse_cases(include_str!("golden.txt")).expect("golden.txt parses");
    assert!(cases.len() >= 24, "golden.txt shrank to {} cases", cases.len());
    let mut generators: BTreeMap<(String, &str), TeamGenerator> = BTreeMap::new();
    let mut failures = Vec::new();
    for case in &cases {
        let mut parts = case.name.rsplitn(3, '-');
        let seed: u64 = parts.next().and_then(|s| s.parse().ok()).expect("seed");
        let mode = parts.next().and_then(Mode::from_name).expect("mode");
        let preset = parts.next().expect("preset").to_owned();
        let g = generators
            .entry((preset.clone(), mode.name()))
            .or_insert_with(|| TeamGenerator::new(&Format::by_id(&preset).expect("preset"), mode).expect("generator"));
        assert_eq!(g.validation_format().id, case.format.id, "{}", case.name);
        let dex = case.format.dex;
        let team = g.team(seed).expect("a team");
        let line = |s: &pikapro::PokemonSet| set_line(dex, s, &SetInfo { gender: s.gender, ..SetInfo::default() });
        let ours: Vec<String> = team.sets.iter().map(line).collect();
        let recorded: Vec<String> =
            case.team.sets.iter().zip(&case.info).map(|(s, info)| set_line(dex, s, info)).collect();
        if ours != recorded {
            failures.push(format!(
                "{}: generated\n  {}\nrecorded\n  {}",
                case.name,
                ours.join("\n  "),
                recorded.join("\n  ")
            ));
            continue;
        }
        let problems = g.validator().validate_with(&case.team, &case.info).err().unwrap_or_default();
        let messages = g.validator().messages(&problems);
        if messages != case.expect {
            failures.push(format!("{}: pikapro-validate {messages:?}, Showdown {:?}", case.name, case.expect));
        }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
}
