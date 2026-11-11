//! Hostile channel text through the parser into the tracker: the lines a
//! public Showdown server (or anything claiming to be one) may send. No
//! line may panic (a debug build checks every overflow), and every value
//! must stay in its contract: HP at most its max, a shown percentage in
//! `0..=100`, stages in `-6..=6`, ids below 32 and distinct within a
//! request. The identity table stays bounded, so work stays linear.

use std::time::{Duration, Instant};

use pikapro::testing::sample_team;
use pikapro::{Battle, BattleEvent, BattleOptions, BattleRng, Format, Seed, SideId};
use pikapro_protocol::channel::{ChannelParser, LineKind, request_body};
use pikapro_protocol::{Perspective, Renderer, hp_shown};
use pikapro_view::{HpReport, OwnOverlay, PublicTracker, ViewConfig};

/// Feeds `lines` to a parser and trackers (percentages and exact HP) and
/// checks the contracts after each line.
fn feed<S: AsRef<str>>(format: &Format, me: Option<SideId>, lines: &[S]) -> (ChannelParser, PublicTracker) {
    let mut p = ChannelParser::new(format, me);
    let mut trackers =
        [HpReport::Percent, HpReport::Exact].map(|hp| PublicTracker::new(ViewConfig::new(format).with_hp(hp)));
    for line in lines {
        let line = line.as_ref();
        let mut events: Vec<BattleEvent> = Vec::new();
        let kind = p.feed(line, &mut |e| events.push(e));
        if kind == LineKind::Request
            && let Some(json) = request_body(line)
            && let Ok(r) = p.request(json, format)
        {
            let ids: Vec<u8> = r.data.pokemon.iter().map(|m| m.id).collect();
            assert!(ids.iter().all(|&id| id < 32), "{line}: ids {ids:?}");
            assert!((1..ids.len()).all(|i| !ids[..i].contains(&ids[i])), "{line}: ids {ids:?}");
            let _ = OwnOverlay::from_request(&r.data);
        }
        for t in &mut trackers {
            for e in &events {
                t.apply(e);
            }
            if let Err(m) = contracts(format, t) {
                panic!("{}: after {line:?}: {m}", format.id);
            }
        }
    }
    let [t, _] = trackers;
    (p, t)
}

fn contracts(format: &Format, t: &PublicTracker) -> Result<(), String> {
    for side in SideId::BOTH {
        let s = t.side(side);
        for (row, m) in s.mons.iter().enumerate() {
            if m.hp.hp > m.hp.max {
                return Err(format!("{side:?} row {row}: HP {:?}", m.hp));
            }
            let (pct, _) = hp_shown(format.mechanics, m.hp.hp, m.hp.max);
            if pct > 100 {
                return Err(format!("{side:?} row {row}: {pct}%"));
            }
        }
        for (slot, a) in s.slots.iter().enumerate() {
            if a.hp.hp > a.hp.max {
                return Err(format!("{side:?} slot {slot}: HP {:?}", a.hp));
            }
            if a.boosts.iter().any(|b| !(-6..=6).contains(b)) {
                return Err(format!("{side:?} slot {slot}: stages {:?}", a.boosts));
            }
        }
    }
    Ok(())
}

const SWITCH: &str = "|switch|p2a: Pikachu|Pikachu, L50, M|100/100";

/// Cases that each once overflowed (a debug panic, a wrapped
/// value in release) or broke a contract.
#[test]
fn hostile_numbers() {
    let singles = Format::gen9_singles();
    let champions = Format::champions_vgc();
    feed(&singles, Some(SideId::P1), &[SWITCH, "|-unboost|p2a: Pikachu|atk|128"]);
    feed(&singles, Some(SideId::P1), &[SWITCH, "|-unboost|p2a: Pikachu|atk|255", "|-boost|p2a: Pikachu|atk|255"]);
    feed(&singles, Some(SideId::P1), &[SWITCH, "|-boost|p2a: Pikachu|atk|1", "|-boost|p2a: Pikachu|atk|127"]);
    let (_, t) = feed(&singles, Some(SideId::P1), &[SWITCH, "|-unboost|p2a: Pikachu|atk|128"]);
    assert_eq!(t.side(SideId::P2).slots[0].boosts[0], -6, "a large drop is a drop");
    let mut faints = vec![SWITCH.to_string()];
    faints.extend((0..300).map(|_| "|faint|p2a: Pikachu".to_string()));
    let (_, t) = feed(&singles, Some(SideId::P1), &faints);
    assert_eq!(t.side(SideId::P2).fainted, u8::MAX);
    feed(&champions, Some(SideId::P1), &[SWITCH, "|-damage|p2a: Pikachu|9999/100y", "|-heal|p2a: Pikachu|65535/100g"]);
    let (_, t) = feed(&singles, Some(SideId::P1), &["|switch|p2a: Pikachu|Pikachu, L50, M|500/100"]);
    assert_eq!(t.side(SideId::P2).mons[0].hp.hp, 100, "HP past the max is the max");
    feed(&singles, Some(SideId::P1), &[SWITCH, "|-damage|p2a: Pikachu|50/0", "|-sethp|p2a: Pikachu|70000/65535"]);
    // Toxic's counter after a status carried in from the other slot.
    feed(
        &singles,
        Some(SideId::P1),
        &[SWITCH, "|-status|p2a: Pikachu|tox", "|-damage|p2a: Pikachu|90/100 tox|[from] psn"],
    );
}

/// A name per line can't grow the identity table (it holds at most 24 a
/// side), so each line stays cheap; ids never collide.
#[test]
fn identity_table_is_bounded() {
    let format = Format::gen9_singles();
    let lines: Vec<String> = (0..20_000).map(|i| format!("|-damage|p2: Name{i}|50/100")).collect();
    let start = Instant::now();
    let (p, _) = feed(&format, Some(SideId::P1), &lines);
    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    assert!(p.unknown_lines() > 19_000, "names past the table are counted: {}", p.unknown_lines());
    // Requests naming new Pokémon every time.
    let mut p = ChannelParser::new(&format, Some(SideId::P1));
    for round in 0..10 {
        let mons: Vec<String> = (0..6)
            .map(|i| {
                format!(
                    r#"{{"ident":"p1: N{round}_{i}","details":"Pikachu, L50","condition":"100/100","active":false,"stats":{{"atk":1,"def":1,"spa":1,"spd":1,"spe":1}},"moves":["tackle"],"baseAbility":"static","item":"","ability":"static"}}"#
                )
            })
            .collect();
        let json = format!(r#"{{"wait":true,"side":{{"id":"p1","pokemon":[{}]}}}}"#, mons.join(","));
        let ids: Vec<u8> = p.request(&json, &format).unwrap().data.pokemon.iter().map(|m| m.id).collect();
        assert!(ids.iter().all(|&id| id < 32), "round {round}: {ids:?}");
        assert!((1..ids.len()).all(|i| !ids[..i].contains(&ids[i])), "round {round}: {ids:?}");
    }
}

/// Real channels with numbers, idents and arguments mutated: no panic, no
/// broken contract.
#[test]
fn mutated_channels() {
    for format in Format::presets() {
        for seed in 6_010_000..6_010_012u64 {
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
            let mut m = BattleRng::new(Seed::fast(seed ^ 0x77));
            for round in 0..12 {
                let mut mutated = lines.clone();
                for _ in 0..=round {
                    let i = m.random(mutated.len() as u32) as usize;
                    mutated[i] = mutate(&mutated[i], &mut m);
                }
                feed(format, Some(SideId::P1), &mutated);
            }
        }
    }
}

fn mutate(line: &str, rng: &mut BattleRng) -> String {
    const BIG: [&str; 12] =
        ["0", "65535", "65536", "999999", "128", "255", "256", "-1", "127", "4294967296", "", "abc"];
    match rng.random(9) {
        0 => {
            // A run of digits replaced by a large or odd value.
            let b = BIG[rng.random(BIG.len() as u32) as usize];
            let skip = rng.random(4);
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
        1 => {
            let mut k = rng.random(line.len() as u32 + 1) as usize;
            while !line.is_char_boundary(k) {
                k -= 1;
            }
            line[..k].to_string()
        }
        2 => line.replace("p2a", "p2z").replace("p1a", "p1f"),
        3 => line.replace(": ", &format!(": {}", "Ω".repeat(rng.random(300) as usize))),
        4 => line.replace("p1", "p2"),
        5 => format!("{line}|[from] ability: Illusion|[of] p2: Zzz|[spread] p1a,p1b,p1c,p2a,p2b,p2c,p2d,p2e"),
        6 => line.replace('"', ""),
        7 => line.replace("/100", "/0").replace("/1", "/"),
        _ => line.replace(", L50", ", L0").replace(", L100", ", L255"),
    }
}

/// Random lines from the protocol's commands, idents and arguments.
#[test]
fn random_lines() {
    const CMDS: &[&str] = &[
        "switch",
        "drag",
        "replace",
        "detailschange",
        "-formechange",
        "-mega",
        "-terastallize",
        "-transform",
        "swap",
        "faint",
        "move",
        "-anim",
        "-prepare",
        "-mustrecharge",
        "cant",
        "-singleturn",
        "-singlemove",
        "-damage",
        "-heal",
        "-sethp",
        "-status",
        "-curestatus",
        "-boost",
        "-unboost",
        "-setboost",
        "-clearboost",
        "-clearallboost",
        "-clearnegativeboost",
        "-invertboost",
        "-copyboost",
        "-swapboost",
        "-crit",
        "-supereffective",
        "-immune",
        "-hitcount",
        "-miss",
        "-fail",
        "-block",
        "-activate",
        "-start",
        "-end",
        "-endability",
        "-ability",
        "-item",
        "-enditem",
        "-weather",
        "-fieldstart",
        "-fieldend",
        "-fieldactivate",
        "-sidestart",
        "-sideend",
        "-swapsideconditions",
        "-waiting",
        "-hint",
        "turn",
        "upkeep",
        "",
        "t:",
        "poke",
        "teampreview",
        "start",
        "win",
        "tie",
        "teamsize",
        "split",
        "player",
        "-message",
        "bigerror",
    ];
    const IDENTS: &[&str] = &[
        "p1a: Pikachu",
        "p2a: Pikachu",
        "p2b: Zoroark",
        "p1: Pikachu",
        "p2: Ditto",
        "p2c: X",
        "p2z: Y",
        "p1a: ",
        "p3a: Q",
        "p1",
        "p2a: Tatsugiri",
        "p2b: Dondozo",
        "p1b: Incineroar",
    ];
    const ARGS: &[&str] = &[
        "Pikachu, L50, M",
        "Zoroark, L50, F",
        "Ditto",
        "Urshifu-*, L50",
        "Terapagos-Terastal, L50, tera:Stellar",
        "100/100",
        "0 fnt",
        "50/100y",
        "20/100r",
        "65535/65535",
        "1/0",
        "999/100",
        "atk",
        "spe",
        "accuracy",
        "6",
        "-6",
        "12",
        "128",
        "255",
        "Tackle",
        "Revival Blessing",
        "Transform",
        "Protect",
        "move: Protect",
        "Substitute",
        "confusion",
        "ability: Commander",
        "ability: Illusion",
        "item: Leftovers",
        "Leftovers",
        "[from] move: Revival Blessing",
        "[from] ability: Imposter",
        "[of] p1a: Pikachu",
        "[of] p2b: Dondozo",
        "[from] item: Leftovers",
        "[silent]",
        "[spread] p1a,p1b,p2a,p2b,p2c",
        "[still]",
        "[miss]",
        "perish3",
        "stockpile3",
        "typechange",
        "Fire/Water",
        "???",
        "0",
        "1",
        "2",
        "5",
        "p1",
        "p2",
        "SunnyDay",
        "[upkeep]",
        "move: Trick Room",
        "move: Stealth Rock",
        "move: Octolock",
        "Octolock",
        "[partiallytrapped]",
        "[wisher] Pikachu",
        "quarkdrivespe",
        "[eat]",
        "Sitrus Berry",
        "[from] stealeat",
        "Skill Swap",
        "Intimidate",
        "[fail]",
    ];
    for format in Format::presets() {
        for seed in 0..40u64 {
            let mut rng = BattleRng::new(Seed::fast(6_020_000 + seed));
            let lines: Vec<String> = (0..400)
                .map(|_| {
                    let mut s = format!("|{}", CMDS[rng.random(CMDS.len() as u32) as usize]);
                    for k in 0..rng.random(6) {
                        s.push('|');
                        s.push_str(if k == 0 || rng.random(3) == 0 {
                            IDENTS[rng.random(IDENTS.len() as u32) as usize]
                        } else {
                            ARGS[rng.random(ARGS.len() as u32) as usize]
                        });
                    }
                    s
                })
                .collect();
            let me = (seed % 2 == 0).then_some(SideId::P1);
            let (_, t) = feed(format, me, &lines);
            let _ = t.types(SideId::P1, 0);
            let _ = t.types(SideId::P2, 1);
        }
    }
}
