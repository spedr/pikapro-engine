//! Replays recorded validation cases: teams with the messages Showdown's
//! validator gave for them (recorded by `validate_fuzz`; see the headers of
//! the case files). No Node needed.

use pikapro_validate::Validator;
use pikapro_validate::cases::{parse_cases, played_line};

fn replay(file: &str, text: &str) -> usize {
    let cases = parse_cases(text).unwrap_or_else(|e| panic!("{file}: {e}"));
    let mut failures = Vec::new();
    for case in &cases {
        let validator = Validator::new(case.format.clone());
        let checked = validator.check(&case.team, &case.info);
        let ours = validator.messages(&checked.problems);
        if ours != case.expect {
            failures.push(format!("{}:\n  ours:     {ours:#?}\n  showdown: {:#?}", case.name, case.expect));
        }
        // The team as validation leaves it, which the battle plays.
        let played: Vec<String> = checked.team.sets.iter().map(|s| played_line(case.format.dex, s)).collect();
        if !case.played.is_empty() && played != case.played {
            failures.push(format!("{} (played):\n  ours:     {played:#?}\n  showdown: {:#?}", case.name, case.played));
        }
    }
    assert!(
        failures.is_empty(),
        "{file}: {} of {} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
    cases.len()
}

#[test]
fn recorded_fuzz_cases_match_showdown() {
    let n = replay("cases.txt", include_str!("cases.txt"));
    assert!(n >= 70, "cases.txt shrank to {n} cases");
}

/// Every forme Showdown's `validateForme` handles in both dexes: the
/// messages, and the team as validation leaves it (what the battle plays).
#[test]
fn battle_only_forme_cases_match_showdown() {
    let n = replay("formes.txt", include_str!("formes.txt"));
    assert!(n >= 790, "formes.txt shrank to {n} cases");
}

#[test]
fn deep_path_cases_match_showdown() {
    let n = replay("deep.txt", include_str!("deep.txt"));
    assert!(n >= 24, "deep.txt shrank to {n} cases");
}

/// `[Gen 9] OU`: random mutated teams, and a hand-written team per rule
/// (the tier tags, the clauses' set hooks, the bans, the messages' order).
#[test]
fn ou_cases_match_showdown() {
    let n = replay("ou.txt", include_str!("ou.txt"));
    assert!(n >= 40, "ou.txt shrank to {n} cases");
    let text = include_str!("ou-rules.txt");
    let n = replay("ou-rules.txt", text);
    assert!(n >= 17, "ou-rules.txt shrank to {n} cases");
    for message in [
        " is tagged Uber, which is banned.",
        " is tagged AG, which is banned.",
        " is banned by OHKO Clause.",
        " is banned by Sleep Moves Clause.",
        "'s move Yawn is banned by Sleep Moves Clause.",
        " is banned by Evasion Moves Clause.",
        " is banned by Evasion Abilities Clause.",
        " is banned by Evasion Items Clause.",
        "'s ability Shadow Tag is banned.",
        "'s ability Arena Trap is banned.",
        "'s ability Moody is banned.",
        "'s item King's Rock is banned.",
        "'s item Razor Fang is banned.",
        "'s move Baton Pass is banned.",
        "'s move Last Respects is banned.",
        "'s move Shed Tail is banned.",
        "'s move Tera Blast is banned.",
    ] {
        assert!(text.lines().any(|l| l.starts_with("expect ") && l.ends_with(message)), "no case with {message:?}");
    }
}
