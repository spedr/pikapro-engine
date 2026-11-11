//! A channel session against hostile or malformed channels: no request
//! makes a built-in policy panic, the lines held for the first request
//! stop at `MAX_HELD` for good, and mutated battles run
//! through the whole session (parser, tracker, overlay, own team, the
//! encoders, the masks, the policies, rejoins) without a panic.

use pikapro::testing::sample_team;
use pikapro::{Battle, BattleEvent, BattleOptions, BattleRng, Format, Seed, SideId, SlotAction};
use pikapro_protocol::{Perspective, Renderer};
use pikapro_rl::session::{ChannelSession, MAX_HELD, SessionOptions};
use pikapro_rl::vec::{MaskLayout, SwitchBy};

const POLICIES: [&str; 3] = ["random", "max_base_power", "simple_heuristics"];

/// A move request's active entry whose moves all show `disabled`.
fn disabled(name: &str) -> String {
    format!(
        r#"{{"moves":[{{"move":"{name}","id":"{}","pp":35,"maxpp":35,"target":"normal","disabled":true}}]}}"#,
        name.to_lowercase()
    )
}

fn pokemon(name: &str, active: bool) -> String {
    format!(
        r#"{{"ident":"p1: {name}","details":"{name}, L50, M","condition":"100/100","active":{active},"stats":{{"atk":1,"def":1,"spa":1,"spd":1,"spe":1}},"moves":["tackle"],"baseAbility":"static","item":"","ability":"static"}}"#
    )
}

/// A session at a move request of `format_id` whose `active` entries and
/// `pokemon` are given.
fn at_request(format_id: &str, active: &[String], pokemon: &[String], switches: &[&str]) -> ChannelSession {
    let mut s = ChannelSession::new(format_id, SessionOptions::default()).unwrap();
    s.feed("|player|p1|a|");
    s.feed("|player|p2|b|");
    s.feed(&format!(
        r#"|request|{{"active":[{}],"side":{{"name":"a","id":"p1","pokemon":[{}]}},"rqid":1}}"#,
        active.join(","),
        pokemon.join(",")
    ));
    s.feed("|start");
    for line in switches {
        s.feed(line);
    }
    s.feed("|turn|1");
    assert!(s.needs_action() && s.error().is_none(), "{:?}", s.error());
    s
}

/// Every move shown disabled, nothing on the bench: Showdown would have
/// shown Struggle, so `move 1` is the choice, for every policy.
#[test]
fn all_moves_disabled_is_struggle() {
    for policy in POLICIES {
        let mut s = at_request(
            "gen9randombattle",
            &[disabled("Tackle")],
            &[pokemon("Pikachu", true)],
            &["|switch|p1a: Pikachu|Pikachu, L50, M|100/100", "|switch|p2a: Pikachu|Pikachu, L50, M|100/100"],
        );
        assert_eq!(s.legal().slot(0), [SlotAction::Move { index: 0, target: None, gimmick: None }]);
        assert_eq!(s.choose_builtin(policy, 1).as_deref(), Ok("move 1"), "{policy}");
    }
}

/// Doubles, both slots showing every move disabled and one Pokémon on the
/// bench: each slot has Struggle, so a joint choice exists (the two
/// switches to the same Pokémon never fit together).
#[test]
fn doubles_all_moves_disabled_one_on_the_bench() {
    for policy in POLICIES {
        let mut s = at_request(
            "gen9randomdoublesbattle",
            &[disabled("Tackle"), disabled("Tackle")],
            &[pokemon("Pikachu", true), pokemon("Raichu", true), pokemon("Pichu", false)],
            &[
                "|switch|p1a: Pikachu|Pikachu, L50, M|100/100",
                "|switch|p1b: Raichu|Raichu, L50, M|100/100",
                "|switch|p2a: Pikachu|Pikachu, L50, M|100/100",
                "|switch|p2b: Raichu|Raichu, L50, M|100/100",
            ],
        );
        let text = s.choose_builtin(policy, 7_200_000).unwrap();
        assert!(!text.contains("switch 3, switch 3"), "{policy}: {text}");
    }
}

/// A forced switch that asks a revival with nothing fainted, while a
/// healthy Pokémon waits on the bench (no server sends it): nothing can be
/// revived, so the policies pass instead of panicking.
#[test]
fn an_impossible_revival_passes() {
    for policy in POLICIES {
        let mut s = ChannelSession::new("gen9randombattle", SessionOptions::default()).unwrap();
        s.feed("|player|p1|a|");
        let reviving = pokemon("Pikachu", true).replace(r#""active":true"#, r#""active":true,"reviving":true"#);
        s.feed(&format!(
            r#"|request|{{"forceSwitch":[true],"side":{{"name":"a","id":"p1","pokemon":[{reviving},{}]}},"rqid":1}}"#,
            pokemon("Raichu", false)
        ));
        s.feed("|start");
        s.feed("|switch|p1a: Pikachu|Pikachu, L50, M|100/100");
        assert!(s.needs_action() && s.error().is_none(), "{:?}", s.error());
        // Nothing can be revived: the slot passes, as Showdown accepts for
        // a forced switch with no target, instead of stalling the battle.
        assert_eq!(s.choose_builtin(policy, 1).as_deref(), Ok("pass"), "{policy}");
    }
}

/// Past `MAX_HELD` lines, the held lines are read, and so is every later
/// line as it comes; the session says the start wasn't read ahead.
#[test]
fn lines_past_max_held_are_read_as_they_come() {
    let mut s = ChannelSession::new("gen9randombattle", SessionOptions::default()).unwrap();
    for _ in 0..MAX_HELD {
        s.feed("|t:|1");
    }
    assert_eq!(s.view().public.turn, 0, "held");
    s.feed("|turn|3");
    assert_eq!(s.view().public.turn, 3, "the line past the cap is read");
    s.feed("|turn|4");
    assert_eq!(s.view().public.turn, 4, "and the next one too");
    assert!(s.error().is_some_and(|e| e.contains("read-ahead")), "{:?}", s.error());
}

/// A first request that can't be read ends the hold too.
#[test]
fn a_bad_first_request_ends_the_hold() {
    let mut s = ChannelSession::new("gen9randombattle", SessionOptions::default()).unwrap();
    s.feed("|turn|2");
    s.feed("|request|{not json");
    assert!(s.error().is_some());
    assert_eq!(s.view().public.turn, 2);
    s.feed("|turn|3");
    assert_eq!(s.view().public.turn, 3, "not held again");
}

/// A reconnect's read-ahead against a request whose six Pokémon all claim
/// Illusion, after a long history of own switches, stays fast.
/// Stray `|replace|` lines alternating two names in one slot re-revealed a
/// record each time and overflowed `reveal_order` (a panic).
#[test]
fn alternating_replaces_dont_panic() {
    let mut s = ChannelSession::new("gen9randomdoublesbattle", SessionOptions::default()).unwrap();
    s.set_side(SideId::P1);
    s.flush();
    for l in ["|player|p1|a|", "|player|p2|b|", "|start", "|switch|p2a: A|Pikachu, L50|100/100"] {
        s.feed(l);
    }
    for i in 0..20 {
        s.feed(if i % 2 == 0 { "|replace|p2a: B|Raichu, L50" } else { "|replace|p2a: C|Pichu, L50" });
    }
    s.feed("|replace|p1a: Nobody|Zoroark, L50, M");
    let _ = s.obs();
    pikapro_view::check::check_consistent(s.tracker()).unwrap();
}

#[test]
fn replay_with_six_illusion_users() {
    use pikapro_protocol::channel::ChannelParser;
    let format = Format::gen9_singles();
    let names = ["A", "B", "C", "D", "E", "F"];
    let mons: Vec<String> = names
        .iter()
        .map(|n| {
            format!(
                r#"{{"ident":"p1: {n}","details":"Zoroark, L50, M","condition":"100/100","active":{},"stats":{{"atk":1,"def":1,"spa":1,"spd":1,"spe":1}},"moves":["tackle"],"baseAbility":"illusion","item":"","ability":"illusion"}}"#,
                *n == "A"
            )
        })
        .collect();
    let json = format!(
        r#"{{"active":[{{"moves":[{{"move":"Tackle","id":"tackle","pp":35,"maxpp":35,"target":"normal"}}]}}],"side":{{"name":"a","id":"p1","pokemon":[{}]}}}}"#,
        mons.join(",")
    );
    let mut before = vec!["|player|p1|a|".to_string(), "|start".to_string()];
    for i in 0..60_000usize {
        before.push(format!("|switch|p1a: {}|Zoroark, L50, M|100/100", names[i % 6]));
    }
    let mut p = ChannelParser::new(&format, Some(SideId::P1));
    let t = std::time::Instant::now();
    assert!(p.first_request(&before, &json, &format).is_ok());
    assert!(t.elapsed() < std::time::Duration::from_secs(30), "{:?}", t.elapsed());
}

// ---------------------------------------------------------------------
// Mutated battles through whole sessions.

fn channel(format: &Format, seed: u64) -> Vec<String> {
    let teams = [sample_team(format), sample_team(format)];
    let mut battle = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(seed))).unwrap();
    let mut rng = BattleRng::new(Seed::fast(seed ^ 0x55));
    let r = Renderer::new(format, Perspective::Player(SideId::P1));
    let mut lines = r.header();
    let mut chunk: Vec<BattleEvent> = battle.drain_events().collect();
    loop {
        lines.extend(r.log(chunk.iter()));
        lines.push(format!("|request|{}", r.request_json(&battle.request_data(SideId::P1))));
        if battle.is_over() {
            break;
        }
        for side in SideId::BOTH {
            let legal = battle.legal_actions(side);
            if !legal.is_waiting() {
                let c = legal.sample(&mut rng);
                battle.submit(side, c).unwrap();
            }
        }
        chunk = battle.drain_events().collect();
    }
    lines
}

fn mutate(line: &str, rng: &mut BattleRng) -> String {
    const BIG: [&str; 14] =
        ["0", "65535", "65536", "999999", "128", "255", "256", "-1", "127", "4294967296", "", "abc", "7", "300"];
    match rng.random(12) {
        0..=2 => {
            // One run of digits becomes another number (or text).
            let b = BIG[rng.random(BIG.len() as u32) as usize];
            let skip = rng.random(12);
            let (mut out, mut runs, mut done) = (String::new(), 0, false);
            let mut chars = line.chars().peekable();
            while let Some(c) = chars.next() {
                if c.is_ascii_digit() && !done {
                    while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                        chars.next();
                    }
                    if runs == skip {
                        out.push_str(b);
                        done = true;
                    } else {
                        out.push(c);
                    }
                    runs += 1;
                    continue;
                }
                out.push(c);
            }
            out
        }
        3 => line.replace("p2a", "p2b").replace("p1a", "p1b"),
        4 => line.replace("\"active\":false", "\"active\":true"),
        5 => line.replace("\"p1: ", "\"p1: X").replace("p1a: ", "p1a: Y"),
        6 => {
            line.replace("\"trapped\":true", "\"maybeTrapped\":true").replace("\"disabled\":false", "\"disabled\":true")
        }
        7 => line.replace("/100", "/0").replace(", L50", ", L0"),
        8 => line.replace("\"teamPreview\":true", "\"teamPreview\":true,\"maxChosenTeamSize\":300"),
        9 => line.replace("\"forceSwitch\":[true]", "\"forceSwitch\":[true,true,true]"),
        _ => {
            const EXTRA: [&str; 16] = [
                "|swap|p1a: Pikachu|1|[from] move: Ally Switch",
                "|swap|p2a: X|7",
                "|-singleturn|p1a: Pikachu|Wide Guard",
                "|-start|p2a: Pikachu|move: Octolock|[of] p1a: Pikachu",
                "|-end|p2a: Pikachu|Octolock|[partiallytrapped]|[silent]",
                "|move|p1a: Pikachu|Wish|p1a: Pikachu",
                "|move|p2b: Pikachu|Healing Wish|p2b: Pikachu",
                "|-heal|p1b: Pikachu|100/100|[from] move: Healing Wish",
                "|-heal|p1a: Pikachu|100/100|[from] move: Wish|[wisher] Zzz",
                "|replace|p1a: Zoroark|Zoroark, L50, M",
                "|sentchoice|move 1",
                "|error|[Unavailable choice] Can't switch: The active Pokémon is trapped",
                "|error|[Invalid choice] There's nothing to choose",
                "|-start|p1a: Pikachu|move: Future Sight",
                "|poke|p1|Urshifu-*, L50, M|",
                "|-hint|Future Sight missed",
            ];
            let extra = EXTRA[rng.random(EXTRA.len() as u32) as usize];
            format!("{extra}\n{line}")
        }
    }
}

fn run(format: &Format, lines: &[String], opts: &SessionOptions, rejoin: bool) {
    let mut s = ChannelSession::for_format(format.clone(), false, opts.clone()).unwrap();
    for (i, line) in lines.iter().enumerate() {
        for l in line.split('\n') {
            s.feed(l);
        }
        if rejoin && i + 1 < lines.len() && line.starts_with("|request|") && i % 7 == 0 {
            let mut r = ChannelSession::for_format(format.clone(), false, opts.clone()).unwrap();
            r.feed_many(&lines[..=i]);
            let _ = r.obs().len();
            let _ = r.mask().len();
            let _ = r.legal();
        }
        if line.starts_with("|request|") || i % 5 == 0 {
            let _ = s.obs().len();
            let _ = s.mask().len();
            let _ = s.min_switches();
            let _ = s.legal();
            if s.needs_action() {
                let _ = s.decode(&[0, 1]);
                // Any policy on any request: a choice or an error.
                let _ = s.choose_builtin(POLICIES[i % 3], i as u64);
            }
        }
    }
}

#[test]
fn hostile_sessions() {
    for format in Format::presets() {
        for seed in 7_200_000..7_200_006u64 {
            let lines = channel(format, seed);
            let mut m = BattleRng::new(Seed::fast(seed ^ 0x77));
            for round in 0..20u32 {
                let masks = if round % 2 == 0 { MaskLayout::Factored } else { MaskLayout::Joint };
                let switch_by = if round % 4 < 2 { SwitchBy::Row } else { SwitchBy::Position };
                let team = (round % 3 == 0).then(|| sample_team(format));
                let opts = SessionOptions::default().with_masks(masks).with_switch_by(switch_by).with_team(team);
                let mut mutated = lines.clone();
                for _ in 0..=(round / 2) {
                    let i = m.random(mutated.len() as u32) as usize;
                    mutated[i] = mutate(&mutated[i], &mut m);
                }
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(format, &mutated, &opts, round % 4 == 1)
                }));
                if res.is_err() {
                    let diff: Vec<_> = mutated.iter().zip(&lines).filter(|(a, b)| a != b).map(|(a, _)| a).collect();
                    panic!("{} seed {seed} round {round}: mutated lines {diff:#?}", format.id);
                }
            }
        }
    }
}
