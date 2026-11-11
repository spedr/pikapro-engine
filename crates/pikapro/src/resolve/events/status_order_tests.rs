//! Compare the complete lists, not just the handler that blocks. Showdown
//! sorts all handlers before invoking any, including irrelevant/suppressed
//! callbacks; selection-sort swaps make the initial order significant too.
//!
//! `findEventHandlers`: target's status/volatiles/ability/item/slot, then
//! allies (including self: Ally/Any), foes (Foe/Any), source (Source),
//! sides (On/Foe/Any), field (pseudo-weathers/weather/terrain), battle.
//! Both events use default order/priority. Speed descends before sub-order:
//! terrain 0, Uproar/Focus Punch 2, Safeguard 4, Sleep Clause 5, abilities 7.
//! No item, weather, room, major status or slot condition in these presets
//! handles either event. Sleep Clause is a rule installed as pseudo-weather.
//! Ties across the complete comparison key shuffle before any callback runs.
//!
//! Normal tests always compare against `testdata/status-handlers.txt`, including
//! in the published crate; Node is only needed to regenerate Showdown's answers:
//! `PIKAPRO_REGENERATE_STATUS_HANDLERS=1 cargo test -p pikapro --lib status_handler_lists_and_tie_draws_match_showdown`
//! Never edit the fixture by hand. Its two string tables losslessly intern the
//! handlers/draws and their lists, keeping all answers compact and readable.
//! The committed fixture is 151,361 bytes (about 148 KiB) for 2,400 cases.
use super::*;
use crate::format::{Clauses, TeamPreview};
use crate::{BattleOptions, BattleRng, Format, Investment, PokemonSet, Seed, Team};
use pikapro_data::gen9::{abilities as a, items as i, moves as m, species as s};
use std::fmt::Write as _;

fn id(name: &str) -> String {
    name.chars().filter(char::is_ascii_alphanumeric).flat_map(char::to_lowercase).collect()
}

fn describe(b: &Battle, h: &Handler) -> String {
    let (effect, prefix) = match h.effect {
        Effect::Ability(a) => (
            id(b.format.dex.ability(a).name),
            match h.callback {
                Callback::Held { prefix, .. } => format!("{prefix:?}"),
                _ => unreachable!(),
            },
        ),
        e if e == Effect::volatile(Volatiles::UPROAR) => ("uproar".into(), "Any".into()),
        e if e == Effect::volatile(Volatiles::PRIORITY_CHARGE) => ("focuspunch".into(), "On".into()),
        Effect::Side(SideCondition::Safeguard) => ("safeguard".into(), "On".into()),
        Effect::Terrain(Terrain::Misty) => ("mistyterrain".into(), "On".into()),
        Effect::Terrain(Terrain::Electric) => ("electricterrain".into(), "On".into()),
        Effect::None => ("sleepclausemod".into(), "On".into()),
        _ => panic!("unexpected status handler: {h:?}"),
    };
    let holder = match h.holder {
        Holder::Pokemon(p) => format!("p{}", p.side.index() * 2 + p.slot as usize),
        Holder::Side(s) => format!("s{}", s.index()),
        Holder::Field => "f".into(),
        _ => panic!("unexpected holder: {h:?}"),
    };
    format!(
        "{effect}:{prefix}@{holder}/{}/{}/{}/{}/{}",
        h.order.unwrap_or(0),
        h.priority,
        h.speed,
        h.sub_order,
        h.effect_order
    )
}

#[test]
fn status_handler_lists_and_tie_draws_match_showdown() {
    let mut input = String::new();
    let mut expected = Vec::new();
    let mut seed = 60_000_000u64;
    let mut tied = [0; 2];
    let abilities = [
        a::AROMA_VEIL,
        a::COMATOSE,
        a::FLOWER_VEIL,
        a::IMMUNITY,
        a::INNER_FOCUS,
        a::INSOMNIA,
        a::LEAF_GUARD,
        a::LIMBER,
        a::OWN_TEMPO,
        a::PASTEL_VEIL,
        a::PURIFYING_SALT,
        a::SHIELDS_DOWN,
        a::SWEET_VEIL,
        a::THERMAL_EXCHANGE,
        a::VITAL_SPIRIT,
        a::WATER_BUBBLE,
        a::WATER_VEIL,
    ];
    let layouts = [
        [a::PASTEL_VEIL, a::PASTEL_VEIL, a::SWEET_VEIL, a::FLOWER_VEIL],
        [a::INSOMNIA, a::SWEET_VEIL, a::AROMA_VEIL, a::OWN_TEMPO],
    ]
    .into_iter()
    .chain(abilities.map(|a| [a; 4]))
    .collect::<Vec<_>>();
    // Both mechanics, singles/doubles, and the preset with Sleep Clause.
    for preset in Format::presets() {
        for trick_room in [false, true] {
            for terrain in [None, Some(Terrain::Misty), Some(Terrain::Electric)] {
                for (layout, abilities) in layouts.iter().enumerate() {
                    // Every ability is audited; the mixed layouts also exercise
                    // unequal speeds and SV's zero/underflow boundaries.
                    let speeds: &[[u16; 4]] = if layout < 2 {
                        &[[100; 4], [70, 100, 130, 100], [8192; 4], [10000, 10000, 9999, 9999]]
                    } else {
                        &[[100; 4]]
                    };
                    for &speeds in speeds {
                        let mut format = preset.clone();
                        format.team_preview = TeamPreview::None;
                        format.team_size = (1, 6);
                        format.clauses = Clauses::default();
                        // Also exercise a rule at speed zero before Champions'
                        // negative-speed Pokémon, as in custom rulesets.
                        format.sleep_clause_mod |= layout == 0;
                        let n = format.active_per_side();
                        let make = |side: usize| {
                            Team::new(
                                (0..n)
                                    .map(|slot| {
                                        let at = side * 2 + slot;
                                        let mut set = PokemonSet::new(s::MEW)
                                            .with_level(50)
                                            .with_ability(abilities[at])
                                            .with_item(
                                                [i::LUM_BERRY, i::LIGHT_BALL, i::MENTAL_HERB, i::SAFETY_GOGGLES][at],
                                            )
                                            .with_moves(&[m::SPLASH]);
                                        if format.stat_system == crate::format::StatSystem::StatPoints {
                                            set.investment = Investment::StatPoints(pikapro_data::Stats::splat(0));
                                        }
                                        set
                                    })
                                    .collect(),
                            )
                        };
                        let mut b =
                            Battle::new(format.clone(), [make(0), make(1)], BattleOptions::new(Seed::fast(seed)))
                                .unwrap();
                        b.state.field.trick_room_turns = if trick_room { 5 } else { 0 };
                        b.state.field.terrain = terrain;
                        b.state.field.weather = Some(Weather::Sun);
                        for pw in [
                            PseudoWeather::Gravity,
                            PseudoWeather::MagicRoom,
                            PseudoWeather::WonderRoom,
                            PseudoWeather::FairyLock,
                            PseudoWeather::EchoedVoice,
                        ] {
                            b.state.field.pseudo_order.push(pw);
                        }
                        let charge = if layout % 2 == 0 { m::FOCUS_PUNCH } else { m::BEAK_BLAST };
                        for side in SideId::BOTH {
                            let side_state = b.state.side_mut(side);
                            side_state.conditions.safeguard = 5;
                            for c in [
                                SideCondition::Reflect,
                                SideCondition::Mist,
                                SideCondition::Safeguard,
                                SideCondition::LightScreen,
                            ] {
                                side_state.conditions.order.push(c);
                            }
                            for slot in 0..n {
                                side_state.conditions.slots[slot].order.push(SlotCondition::Wish);
                                let mon = &mut side_state.team[slot];
                                mon.status = Status::Burn;
                                let flags = if slot % 2 == 0 {
                                    [Volatiles::PRIORITY_CHARGE, Volatiles::UPROAR]
                                } else {
                                    [Volatiles::UPROAR, Volatiles::PRIORITY_CHARGE]
                                };
                                for flag in flags {
                                    mon.volatiles.add(flag);
                                }
                                mon.volatile_counters.priority_charge = charge;
                                mon.stats[Stat::Spe] = speeds[side.index() * 2 + slot];
                            }
                        }
                        b.conditions_changed();
                        crate::resolve::update_speed(&mut b);
                        for target in [Position::new(SideId::P1, 0), Position::new(SideId::P2, (n - 1) as u8)] {
                            let mut out = Vec::new();
                            let source = Some(Position::new(SideId::P2, 0));
                            for (event_index, event) in
                                [Event::SetStatus, Event::TryAddVolatile].into_iter().enumerate()
                            {
                                let mut list = EventHandlers::new();
                                collect_into(&mut b, event, Target::Pokemon(target), source, &mut list);
                                out.push(list.iter().map(|h| describe(&b, h)).collect::<Vec<_>>().join(","));
                                let sort_seed = Seed::ShowdownGen5([0, 0, (seed >> 16) as u16, seed as u16]);
                                b.rng = BattleRng::new(sort_seed);
                                b.rng.enable_trace();
                                speed_sort(&mut list, &mut b.rng, Handler::compare);
                                out.push(list.iter().map(|h| describe(&b, h)).collect::<Vec<_>>().join(","));
                                let draws = b.rng.take_trace();
                                let after_sort = b.rng.clone();
                                tied[event_index] += usize::from(!draws.is_empty());
                                out.push(
                                    draws
                                        .iter()
                                        .map(|(bound, result)| format!("{bound}:{result}"))
                                        .collect::<Vec<_>>()
                                        .join(","),
                                );
                                // A short-circuit (Focus Punch, Uproar, terrain,
                                // Safeguard) must still draw every sorting tie.
                                b.rng = BattleRng::new(sort_seed);
                                b.rng.enable_trace();
                                let relay = if event == Event::SetStatus {
                                    Relay::Status(StatusId::Sleep)
                                } else {
                                    Relay::Id("flinch")
                                };
                                let mut ev = EventCtx::new(
                                    event,
                                    Target::Pokemon(target),
                                    source,
                                    Effect::Move(m::SPORE),
                                    relay,
                                );
                                run_event(&mut b, &mut ev, None);
                                assert_eq!(b.rng, after_sort, "{seed} {event:?}");
                                if event == Event::TryAddVolatile {
                                    b.rng = BattleRng::new(sort_seed);
                                    crate::resolve::volatiles::add_volatile_from(
                                        &mut b,
                                        target,
                                        "flinch",
                                        source,
                                        false,
                                        Effect::Move(m::SPORE),
                                    );
                                    assert_eq!(
                                        b.rng, after_sort,
                                        "{seed}: addVolatile must sort before Focus Punch blocks"
                                    );
                                }
                            }
                            let formatid = match preset.id {
                                "championsvgc" => "gen9championsvgc2026regmc",
                                "championsbss" => "gen9championsbssregmc",
                                "gen9doubles" => "gen9doublescustomgame@@@!Team Preview",
                                "gen9vgc" => "gen9doublescustomgame@@@Picked Team Size = 4,Adjust Level Down = 50",
                                "gen9ou" => "gen9ou",
                                "gen9randombattle" => "gen9randombattle",
                                "gen9randomdoublesbattle" => "gen9randomdoublesbattle",
                                _ => "gen9customgame@@@!Team Preview",
                            };
                            writeln!(
                                input,
                                "{formatid}\t{seed}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                                u8::from(trick_room),
                                match terrain {
                                    Some(Terrain::Misty) => "mistyterrain",
                                    Some(Terrain::Electric) => "electricterrain",
                                    _ => "-",
                                },
                                abilities.map(|a| id(format.dex.ability(a).name)).join(","),
                                speeds.map(|s| s.to_string()).join(","),
                                target.side.index() * 2 + target.slot as usize,
                                if charge == m::FOCUS_PUNCH { "focuspunch" } else { "beakblast" },
                                u8::from(format.sleep_clause_mod)
                            )
                            .unwrap();
                            expected.push(out.join("\t"));
                            seed += 1;
                        }
                    }
                }
            }
        }
    }
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/status-handlers.txt");
    if std::env::var("PIKAPRO_REGENERATE_STATUS_HANDLERS").as_deref() == Ok("1") {
        let answers = oracle_answers(&input);
        assert_eq!(answers.lines().count(), expected.len(), "oracle case count");
        std::fs::write(&fixture, encode_fixture(&answers)).expect("write status handler fixture");
    }
    let text = std::fs::read_to_string(&fixture).expect("committed status handler fixture must exist");
    let actual = decode_fixture(&text);
    assert_eq!(actual.len(), expected.len(), "fixture case count");
    for ((case, expected), actual) in input.lines().zip(&expected).zip(&actual) {
        assert_eq!(expected, actual, "case: {case}");
    }
    assert!(tied.into_iter().all(|n| n > 0));
    eprintln!("{} handler-list pairs matched; tie draws in {tied:?} cases", expected.len());
}

fn oracle_answers(input: &str) -> String {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let oracle = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/showdown-oracle");
    let mut child = Command::new("node")
        .arg(oracle.join("status-handlers.mjs"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("regeneration needs Node and tools/showdown-oracle dependencies");
    let mut stdin = child.stdin.take().unwrap();
    // Drain stdout/stderr while writing: an early Node failure must report its
    // stderr even if the large input hits BrokenPipe (or stderr fills its pipe).
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input.as_bytes()));
        let output = child.wait_with_output().expect("wait for status handler oracle");
        let written = writer.join().expect("oracle stdin writer");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "status handler oracle {}: {stderr}", output.status);
        written.unwrap_or_else(|e| panic!("status handler oracle stdin: {e}; stderr: {stderr}"));
        String::from_utf8(output.stdout).expect("oracle UTF-8")
    })
}

fn encode_fixture(answers: &str) -> String {
    use std::collections::BTreeMap;
    let mut tokens = Vec::new();
    let mut token_ids = BTreeMap::new();
    let mut lists = Vec::new();
    let mut list_ids = BTreeMap::new();
    let mut cases = Vec::new();
    for line in answers.lines() {
        let mut case = Vec::new();
        for list in line.split('\t') {
            let next = lists.len();
            let index = *list_ids.entry(list).or_insert_with(|| {
                let ids = list
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|token| {
                        let next = tokens.len();
                        token_ids
                            .entry(token)
                            .or_insert_with(|| {
                                tokens.push(token);
                                next
                            })
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                lists.push(if ids.is_empty() { "-".into() } else { ids });
                next
            });
            case.push(index.to_string());
        }
        assert_eq!(case.len(), 6, "two events, each with collected/sorted/draws");
        cases.push(case.join("\t"));
    }
    format!("status-handlers-v1\n{}\n\n{}\n\n{}\n", tokens.join("\n"), lists.join("\n"), cases.join("\n"))
}

fn decode_fixture(text: &str) -> Vec<String> {
    let mut sections = text.trim_end().split("\n\n");
    let mut tokens = sections.next().unwrap().lines();
    assert_eq!(tokens.next(), Some("status-handlers-v1"));
    let tokens = tokens.collect::<Vec<_>>();
    let lists = sections
        .next()
        .unwrap()
        .lines()
        .map(|line| {
            if line == "-" {
                String::new()
            } else {
                line.split(',').map(|i| tokens[i.parse::<usize>().unwrap()]).collect::<Vec<_>>().join(",")
            }
        })
        .collect::<Vec<_>>();
    let cases = sections
        .next()
        .unwrap()
        .lines()
        .map(|line| {
            let columns = line.split('\t').map(|i| lists[i.parse::<usize>().unwrap()].as_str()).collect::<Vec<_>>();
            assert_eq!(columns.len(), 6);
            columns.join("\t")
        })
        .collect();
    assert!(sections.next().is_none());
    cases
}
