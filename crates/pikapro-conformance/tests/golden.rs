//! Replays recorded Showdown battles (`fixtures/*.txt`) on pikapro and
//! requires identical normalised raw logs, RNG draws included, each
//! perspective's log to be the channel Showdown would send it, and, for the
//! fixtures recorded with requests, every request to match
//! Showdown's byte for byte, and the same battle with events off to reach
//! the same state after every choice (`silent_replay_differs`). No Node
//! needed.
//!
//! A failure here means a change altered behaviour that previously matched
//! Showdown exactly. Investigate with `cargo conformance --trace`.

use pikapro_conformance::{
    check_replay_channels, compare_request_lists, fixture, normalize, panic_message, render_raw, replay, requests,
    silent_replay_differs,
};

/// Runs `f`, turning a panic (a recorded choice pikapro rejects) into an
/// error, so one fixture that can't replay doesn't hide the others.
fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .map_err(|e| format!("the replay panicked: {}", panic_message(&*e)))
}

#[test]
fn golden_fixtures_match_showdown() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    // Fixtures are never deleted: the manifest lists every one.
    let paths = fixture::check_manifest(&dir, "txt").unwrap_or_else(|e| panic!("{e}"));

    let mut failures = Vec::new();
    let mut with_requests = 0;
    for path in &paths {
        let name = path.file_name().unwrap().to_string_lossy();
        let text = std::fs::read_to_string(path).expect("readable fixture");
        let (scenario, expected) = fixture::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let expected_requests = fixture::parse_requests(&text);
        // One replay for every check: with the requests when they're compared.
        let (replayed, raw) = match guarded(|| {
            let r = replay(&scenario, !expected_requests.is_empty());
            let raw = render_raw(&scenario, &r);
            (r, raw)
        }) {
            Ok(x) => x,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        let ours = normalize(&raw, true);
        if let Some(i) = (0..ours.len().max(expected.len())).find(|&i| ours.get(i) != expected.get(i)) {
            failures.push(format!(
                "{name} line {i}:\n  ours     {}\n  showdown {}",
                ours.get(i).map_or("<end>", String::as_str),
                expected.get(i).map_or("<end>", String::as_str),
            ));
            continue;
        }
        if let Some((perspective, d)) = check_replay_channels(&scenario, &replayed, &expected, true) {
            failures.push(format!("{name} {perspective:?} log line {}:\n{d}", d.index));
            continue;
        }
        match guarded(|| silent_replay_differs(&scenario, &replayed)) {
            Ok(None) => {}
            Ok(Some(d)) | Err(d) => {
                failures.push(format!("{name}: {d}"));
                continue;
            }
        }
        if !expected_requests.is_empty() {
            with_requests += 1;
            let ours = requests(&raw);
            if let Some(d) = compare_request_lists(&ours, &expected_requests) {
                failures.push(format!("{name} requests: {d}"));
            }
        }
    }
    assert!(failures.is_empty(), "{} of {} fixtures diverge:\n{}", failures.len(), paths.len(), failures.join("\n"));
    assert!(with_requests > 0, "no fixture has requests");
}

/// The scripted Endless Battle Clause scenarios (`gen9ou-scripted-endless-*`,
/// each a battle of 100 turns or more, recorded from Showdown): one per
/// branch of `maybeTriggerEndlessBattleClause` and per place Showdown
/// writes a staleness. [`golden_fixtures_match_showdown`] replays them;
/// this says what each is there for, by how Showdown ended it.
#[test]
fn endless_battle_scenarios_end_as_recorded() {
    const P1_LOSES: &str = "|-message|p1's team started with the rudimentary means to perform restorative \
                            berry-cycling and thus loses.";
    const P2_LOSES: &str = "|-message|p2's team started with the rudimentary means to perform restorative \
                            berry-cycling and thus loses.";
    const BOTH: &str =
        "|-message|Each side's team started with the rudimentary means to perform restorative berry-cycling.";
    // The last lines of the log (after `|upkeep`, blank lines left out).
    let scenarios: [(&str, &[&str]); 18] = [
        // Both heal each other with Heal Pulse (staleness: external), neither
        // can switch; the side that started with a berry and Pickup, or
        // Harvest, loses.
        ("pickup-side-loses", &[P1_LOSES, "|win|p2"]),
        ("harvest-side-loses", &[P2_LOSES, "|win|p1"]),
        // A Leppa Berry eaten when Splash runs out and got back with
        // Recycle, three times; both teams could cycle: a tie.
        ("leppa-recycle-loop-both-cycle", &[BOTH, "|tie"]),
        ("neither-cycles-tie", &["|tie"]),
        // Not ended: one side stale; both stale by their own doing only
        // (an Oran Berry got back and eaten); both able to switch to a
        // Pokémon that isn't stale; a volatile staleness (Skill Swap)
        // ended by switching out.
        ("one-side-stale-goes-on", &["|turn|104"]),
        ("internal-only-goes-on", &["|turn|104"]),
        ("both-can-switch-goes-on", &["|turn|104"]),
        ("switching-ends-volatile-staleness", &["|turn|104"]),
        // Staleness by its own doing on one side, by the foe's Skill Swap
        // on the other.
        ("internal-and-skill-swap", &[P1_LOSES, "|win|p2"]),
        // A Pokémon on the bench counts for its team's berry and cycle;
        // a rooted Pokémon (Ingrain) can't switch.
        ("benched-cycler-loses", &[P2_LOSES, "|win|p1"]),
        ("trapped-side-cannot-switch", &[P1_LOSES, "|win|p2"]),
        // The same team in two team-preview orders: the last Pokémon
        // looked at decides whether the team holds a berry.
        ("bench-order-berry-last-loses", &[P1_LOSES, "|win|p2"]),
        ("bench-order-recycle-last-tie", &["|tie"]),
        // A Leppa Berry eaten by a foe's Bug Bite or Pluck, or flung at a
        // foe; Entrainment and Floral Healing; Wandering Spirit.
        ("bug-bite-leppa-berry", &[P2_LOSES, "|win|p1"]),
        ("pluck-leppa-berry", &["|tie"]),
        ("fling-leppa-berry", &[P1_LOSES, "|win|p2"]),
        ("entrainment-and-floral-healing", &["|tie"]),
        ("wandering-spirit", &["|tie"]),
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    for (name, ending) in scenarios {
        let path = dir.join(format!("gen9ou-scripted-endless-{name}.txt"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let (scenario, expected) = fixture::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(scenario.format.endless_battle_clause, "{name}");
        let lines: Vec<&str> = expected
            .iter()
            .map(String::as_str)
            .filter(|l| !l.starts_with("|rng|") && !l.is_empty() && *l != "|")
            .collect();
        let upkeep = lines.iter().rposition(|l| *l == "|upkeep").expect("an upkeep");
        assert_eq!(&lines[upkeep + 1..], ending, "{name}");
        // The clause looks at the end of turn 100: the last turn started
        // is 100 where it ended the battle.
        let last_turn = lines.iter().rev().find_map(|l| l.strip_prefix("|turn|")).expect("a turn");
        assert_eq!(last_turn, if ending == ["|turn|104"] { "104" } else { "100" }, "{name}");
    }
}

/// Random battles reach the Endless Battle Clause too: the `endless`
/// target's teams (`conformance --target endless`, `view-conformance
/// --source endless`) heal their foes and cycle berries, and a good part
/// of their battles end by the clause, in every one of its three ways.
/// (This counts pikapro's own results; the fuzzers compare such battles
/// with Showdown's.)
#[test]
fn the_endless_target_reaches_the_clause() {
    use pikapro::{EndReason, Format};
    use pikapro_conformance::view::Target;
    let format = Format::gen9_ou();
    let (mut won, mut tied, mut both) = (0, 0, 0);
    for seed in 7_700_000..7_700_040 {
        let (scenario, landed) = pikapro_conformance::scenario_as(&format, seed, Some(Target::Endless), false);
        assert!(landed, "{seed}");
        let replayed = replay(&scenario, false);
        use pikapro::event::Message;
        let has = |wanted: fn(&Message) -> bool| {
            let found =
                |e: &pikapro::BattleEvent| matches!(e, pikapro::BattleEvent::Message { message } if wanted(message));
            replayed.battle.events().iter().any(found)
        };
        let loses = has(|m| matches!(m, Message::BerryCycling { .. }));
        let both_cycle = has(|m| matches!(m, Message::BerryCyclingBoth));
        match replayed.battle.result() {
            Some(r) if r.reason == EndReason::EndlessBattle => {
                assert!(r.turns >= 100, "{seed}: {r:?}");
                // A side loses with the message that says why, and only
                // then.
                assert_eq!(r.winner.is_some(), loses, "{seed}");
                match (r.winner.is_some(), both_cycle) {
                    (true, _) => won += 1,
                    (false, true) => both += 1,
                    (false, false) => tied += 1,
                }
            }
            _ => assert!(!loses && !both_cycle, "{seed}"),
        }
    }
    println!("of 40 battles: {won} lost by a berry cycler, {both} tied with two, {tied} tied with none");
    assert!(
        won >= 2 && both >= 2 && tied >= 1,
        "{won} battles lost by a berry cycler, {both} tied with two, {tied} tied with none"
    );
    // The target has nothing to force in a format without the clause.
    let (_, landed) =
        pikapro_conformance::scenario_as(&Format::gen9_singles(), 7_700_000, Some(Target::Endless), false);
    assert!(!landed);
}
