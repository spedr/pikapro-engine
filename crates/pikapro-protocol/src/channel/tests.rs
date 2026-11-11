//! The parser against the renderer: every line of random battles' player
//! and spectator channels, parsed and rendered again, is the same line.

use super::*;
use crate::{Perspective, Renderer};
use pikapro::testing::sample_team;
use pikapro::{Battle, BattleOptions, BattleRng, Seed};

/// Renders `events` as `renderer` would, with the parser's names by id.
fn rerender(renderer: &mut Renderer, parser: &ChannelParser, events: &[BattleEvent]) -> Vec<String> {
    for side in SideId::BOTH {
        let names: Vec<String> = (0..32u8).map(|id| parser.name_of(side, id).unwrap_or("").to_string()).collect();
        let shiny: Vec<bool> = (0..32u8).map(|id| parser.shiny_of(side, id)).collect();
        renderer.nicknames[side.index()] = names;
        renderer.shiny[side.index()] = shiny;
    }
    renderer.log(events.iter()).into_iter().filter(|l| l != "|clearpoke" && !l.starts_with("|uhtml")).collect()
}

/// Plays random battles and checks every line of each channel.
pub(crate) fn check_battles(format: &Format, seeds: core::ops::Range<u64>) {
    let perspectives = [Perspective::Player(SideId::P1), Perspective::Player(SideId::P2), Perspective::Spectator];
    for seed in seeds {
        let teams = [sample_team(format), sample_team(format)];
        let mut battle = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(seed))).unwrap();
        let mut rng = BattleRng::new(Seed::fast(seed ^ 0xABC));
        let renderers = perspectives.map(|p| Renderer::new(format, p));
        let mut again = perspectives.map(|p| Renderer::new(format, p));
        let mut parsers = perspectives.map(|p| match p {
            Perspective::Player(s) => ChannelParser::new(format, Some(s)),
            _ => ChannelParser::new(format, None),
        });
        let mut chunk: Vec<BattleEvent> = battle.drain_events().collect();
        let mut header = true;
        loop {
            for (i, r) in renderers.iter().enumerate() {
                let mut lines = if header { r.header() } else { Vec::new() };
                lines.extend(r.log(chunk.iter()));
                for line in &lines {
                    let mut events = Vec::new();
                    let kind = parsers[i].feed(line, &mut |e| events.push(e));
                    assert_ne!(kind, LineKind::Unknown, "{} seed {seed}: unknown line {line:?}", format.id);
                    if events.is_empty() {
                        continue;
                    }
                    let got = rerender(&mut again[i], &parsers[i], &events);
                    assert_eq!(got, vec![line.clone()], "{} seed {seed} {:?}: {events:?}", format.id, perspectives[i]);
                }
            }
            header = false;
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
        assert!(parsers.iter().all(|p| p.unknown_lines() == 0));
    }
}

/// A channel joined at any decision (a reconnect: the lines so far, then
/// the current request) gives the player's Pokémon their registration ids,
/// the engine's, as one read from the start does.
#[test]
fn first_request_mid_battle_gives_registration_ids() {
    for format in Format::presets() {
        let mut points = 0;
        for seed in 6_000_000..6_000_040 {
            let teams = [sample_team(format), sample_team(format)];
            let mut battle = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(seed))).unwrap();
            let mut rng = BattleRng::new(Seed::fast(seed ^ 0x77));
            let me = SideId::P1;
            let r = Renderer::new(format, Perspective::Player(me));
            let mut lines = r.header();
            let mut chunk: Vec<BattleEvent> = battle.drain_events().collect();
            loop {
                lines.extend(r.log(chunk.iter()));
                if battle.is_over() {
                    break;
                }
                let want = battle.request_data(me);
                points += 1;
                let mut p = ChannelParser::new(format, Some(me));
                let got = p.first_request(&lines, &r.request_json(&want), format).unwrap();
                let ids = |d: &pikapro::request::RequestData| d.pokemon.iter().map(|m| m.id).collect::<Vec<_>>();
                assert_eq!(ids(&got.data), ids(&want), "{} seed {seed} turn {}", format.id, battle.turn());
                for side in SideId::BOTH {
                    let legal = battle.legal_actions(side);
                    if !legal.is_waiting() {
                        let c = legal.sample(&mut rng);
                        battle.submit(side, c).unwrap();
                    }
                }
                chunk = battle.drain_events().collect();
            }
        }
        assert!(points > 100, "{}: {points} decisions", format.id);
    }
}

#[test]
fn lines_read_back() {
    for format in Format::presets() {
        check_battles(format, 0..30);
    }
}

#[test]
fn spectral_thief_boost_clear_reads_back() {
    let format = Format::gen9_ou();
    let mut parser = ChannelParser::new(&format, Some(SideId::P1));
    parser.feed("|switch|p1a: Thief|Mew|100/100", &mut |_| {});
    parser.feed("|switch|p2a: Victim|Mew|100/100", &mut |_| {});
    let line = "|-clearpositiveboost|p2a: Victim|p1a: Thief|move: Spectral Thief";
    let mut events = Vec::new();
    assert_eq!(parser.feed(line, &mut |e| events.push(e)), LineKind::Battle);
    assert!(matches!(events.as_slice(), [BattleEvent::ClearPositiveBoost { target, source, move_id }]
        if target.pos.side == SideId::P2 && source.pos.side == SideId::P1 && *move_id == moves::SPECTRAL_THIEF));
    let mut renderer = Renderer::new(&format, Perspective::Player(SideId::P1));
    assert_eq!(rerender(&mut renderer, &parser, &events), vec![line]);
    assert_eq!(parser.unknown_lines(), 0);
}

#[test]
fn reads_real_showdown_lines() {
    let format = Format::champions_vgc();
    let mut p = ChannelParser::new(&format, Some(SideId::P1));
    let mut events = Vec::new();
    let mut sink = |e| events.push(e);
    for line in [
        ">battle-gen9championsvgc2026regmc-1",
        "|init|battle",
        "|title|Alice vs. Bob",
        "|j|☆Alice",
        "|j| Bob",
        "|c|☆Alice|gl hf",
        "|raw|<div>hi</div>",
        "|inactive|Battle timer is ON",
        "|t:|1727000000",
        "|gametype|doubles",
        "|player|p1|Alice|60|1500",
        "|player|p2|Bob|lucas|",
        "|teamsize|p1|6",
        "|gen|9",
        "|tier|[Gen 9 Champions] VGC 2026 Reg M-C",
        "|rated|",
        "|rule|Species Clause: Limit one of each Pokémon",
        "|badge|p1|gold|vgc|1",
        "|request|",
        "|l|guest",
        "|n|Bob2|bob",
        "|html|<b>x</b>",
        "|uhtml|otsrequest|<button>",
        "|error|[Invalid choice] Can't do anything: It's not your turn",
        "|inactiveoff|Battle timer is now OFF.",
        "|something-new|x",
    ] {
        p.feed(line, &mut sink);
    }
    assert_eq!(p.unknown_lines(), 1);
    assert_eq!(p.player_name(SideId::P2), "Bob");
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(
        p.feed("|error|[Unavailable choice] Can't switch: The active Pokémon is trapped", &mut |_| {}),
        LineKind::Error { unavailable: true }
    );
    assert_eq!(request_body("|request|{\"wait\":true}"), Some("{\"wait\":true}"));
    assert_eq!(request_body("|request|"), None);
}

#[test]
fn split_blocks_take_this_players_line() {
    let format = Format::champions_vgc();
    for (me, want) in [(SideId::P1, "74/100"), (SideId::P2, "150/202")] {
        let mut p = ChannelParser::new(&format, Some(me));
        let mut events = Vec::new();
        for line in [
            "|switch|p2a: Incineroar|Incineroar, L50, M|202/202",
            "|split|p2",
            "|-damage|p2a: Incineroar|150/202",
            "|-damage|p2a: Incineroar|74/100",
        ] {
            p.feed(line, &mut |e| events.push(e));
        }
        let Some(BattleEvent::Damage { hp, max_hp, .. }) = events.last() else { panic!("{events:?}") };
        assert_eq!(format!("{hp}/{max_hp}"), want);
    }
}

/// The ids of the `|switch|` events of `lines`, by line.
fn switch_ids(format: &Format, me: SideId, lines: &[&str]) -> Vec<u8> {
    let mut p = ChannelParser::new(format, Some(me));
    let mut ids = Vec::new();
    for line in lines {
        p.feed(line, &mut |e| {
            if let BattleEvent::Switch { actor, .. } = e {
                ids.push(actor.id);
            }
        });
    }
    ids
}

/// A foe an `[of]` names before its switch-in holds no `|poke|` line's id
/// until its details match one: then it takes that one, not another line's.
#[test]
fn a_foe_named_early_takes_its_own_poke_line() {
    let format = Format::gen9_vgc();
    let ids = switch_ids(
        &format,
        SideId::P1,
        &[
            "|poke|p2|Incineroar, L50, M|",
            "|poke|p2|Rillaboom, L50, M|",
            "|poke|p2|Amoonguss, L50, M|",
            "|poke|p2|Urshifu-*, L50, M|",
            "|teampreview|4",
            "|start",
            "|switch|p2a: Rilla|Rillaboom, L50, M|100/100",
            "|switch|p2b: Shroom|Amoonguss, L50, M|100/100",
            "|-damage|p1a: X|50/100|[from] ability: Rough Skin|[of] p2: Cat",
            "|switch|p2a: Cat|Incineroar, L50, M|100/100",
            "|switch|p2b: Fist|Urshifu-Rapid-Strike, L50, M|100/100",
        ],
    );
    assert_eq!(ids, [1, 2, 0, 3]);
}

/// Off the field, another Pokémon of the same name with the line's max HP
/// is the one meant only where HP is exact: on a percentage channel, a
/// Champions colour hint (`/1000`) isn't another max HP.
#[test]
fn percentages_dont_tell_two_of_one_name_apart() {
    let format = Format::champions_vgc();
    let lines = [
        "|switch|p2a: Pika|Pikachu, L50, M|100/100",
        "|-damage|p2a: Pika|20/100y",
        "|switch|p2a: Pika|Raichu, L50, M|100/100",
        "|-heal|p2: Pika|30/100",
    ];
    let mut p = ChannelParser::new(&format, Some(SideId::P1));
    let mut healed = None;
    for line in lines {
        p.feed(line, &mut |e| {
            if let BattleEvent::Heal { target, .. } = e {
                healed = Some(target);
            }
        });
    }
    let target = healed.expect("a heal");
    assert_eq!(p.name_of(SideId::P2, target.id), Some("Pika"));
    assert_eq!(target.species, pikapro_data::gen9::species::PIKACHU, "the one off the field");
}

/// A hazard a move removed reads back with its cause, Spikes too:
/// `-sidestart` and hazard removal write Spikes the same way (`Spikes`;
/// Stealth Rock is `move: Stealth Rock` at its start), so only the
/// `[from]` tag tells its removal by Rapid Spin from a plain `-sideend`.
#[test]
fn a_removed_hazard_keeps_its_cause() {
    use pikapro::state::SideCondition;
    use pikapro_data::gen9::moves::RAPID_SPIN;
    let format = Format::gen9_singles();
    let mut p = ChannelParser::new(&format, Some(SideId::P1));
    let mut events = Vec::new();
    for line in [
        "|switch|p1a: Tusk|Great Tusk, L50|100/100",
        "|-sideend|p1: p1|Spikes|[from] move: Rapid Spin|[of] p1a: Tusk",
        "|-sideend|p1: p1|Stealth Rock|[from] move: Rapid Spin|[of] p1a: Tusk",
        "|-sideend|p1: p1|Spikes",
    ] {
        assert_ne!(p.feed(line, &mut |e| events.push(e)), LineKind::Unknown, "{line}");
    }
    let removed = |e: &BattleEvent| match *e {
        BattleEvent::HazardEnd { side: SideId::P1, condition, from, of } => Some((condition, from, of.is_some())),
        _ => None,
    };
    assert_eq!(removed(&events[1]), Some((SideCondition::Spikes, Some(RAPID_SPIN), true)), "{:?}", events[1]);
    assert_eq!(removed(&events[2]), Some((SideCondition::StealthRock, Some(RAPID_SPIN), true)));
    assert_eq!(events[3], BattleEvent::SideEnd { side: SideId::P1, condition: SideCondition::Spikes });
}

/// Fling's `-enditem` without an item (its Blunder Policy was used up by
/// the miss) reads back as it was written.
#[test]
fn a_fling_without_an_item_reads_back() {
    let format = Format::gen9_singles();
    let mut p = ChannelParser::new(&format, Some(SideId::P1));
    let mut events = Vec::new();
    for line in ["|switch|p1a: Scizor|Scizor, L50, M|100/100", "|-enditem|p1a: Scizor||[from] move: Fling"] {
        assert_eq!(p.feed(line, &mut |e| events.push(e)), LineKind::Battle, "{line}");
    }
    let BattleEvent::FlingNoItem { actor } = events[1] else { panic!("{:?}", events[1]) };
    assert_eq!(actor.species, pikapro_data::gen9::species::SCIZOR);
    let again = Renderer::new(&format, Perspective::Player(SideId::P1));
    assert_eq!(rerender(&mut { again }, &p, &events[1..]), ["|-enditem|p1a: Scizor||[from] move: Fling"]);
}

/// `|win|` names a player as its `|player|` line did (Showdown compares
/// user ids); a name that is neither player's is no winner.
#[test]
fn a_winner_is_named_by_its_player_line() {
    let format = Format::gen9_singles();
    let mut p = ChannelParser::new(&format, Some(SideId::P1));
    p.feed("|player|p1|Alice|60|1500", &mut |_| {});
    p.feed("|player|p2|Bob Two|lucas|", &mut |_| {});
    let mut wins = Vec::new();
    assert_eq!(p.feed("|win|bobtwo", &mut |e| wins.push(e)), LineKind::End(Some(SideId::P2)));
    assert_eq!(p.feed("|win|Carol", &mut |e| wins.push(e)), LineKind::EndUnknown, "not a tie");
    assert_eq!(wins.len(), 1, "no event for an unknown winner");
    assert_eq!(p.unknown_lines(), 1);
    let mut q = ChannelParser::new(&format, Some(SideId::P1));
    assert_eq!(q.feed("|win|p2", &mut |_| {}), LineKind::End(Some(SideId::P2)));
}

/// Plain custom games have team preview on Showdown, which the presets
/// they'd map to don't: only their `!Team Preview` forms are read.
#[test]
fn custom_games_need_no_team_preview() {
    let id = |s: &str| showdown_format(s).map(|f| (f.format.id, f.exact_hp));
    assert_eq!(id("gen9customgame"), None);
    assert_eq!(id("gen9doublescustomgame"), None);
    assert_eq!(id("gen9customgame@@@!Team Preview"), Some(("gen9singles", true)));
    assert_eq!(id("gen9doublescustomgame@@@!Team Preview"), Some(("gen9doubles", true)));
    assert_eq!(id("gen9doublescustomgame@@@Picked Team Size = 4,Adjust Level Down = 50"), Some(("gen9vgc", true)));
    assert_eq!(id("gen9singles"), Some(("gen9singles", true)));
    assert_eq!(id("gen9randombattle"), Some(("gen9randombattle", false)));
}

/// The series formats: Showdown's `Best of = 3` rule, with the sheets
/// the format shows (`Force Open Team Sheets` on the Champions Bo3 ladder).
#[test]
fn series_formats_say_their_length_and_sheets() {
    let info = |s: &str| showdown_format(s).map(|f| (f.format.id, f.best_of, f.open_sheets));
    assert_eq!(info("gen9championsvgc2026regmcbo3"), Some(("championsvgc", Some(3), OpenSheets::Forced)));
    assert_eq!(info("gen9championsvgc2026regmbbo3"), Some(("championsvgc", Some(3), OpenSheets::Forced)));
    assert_eq!(info("gen9championsvgc2026regmc"), Some(("championsvgc", None, OpenSheets::Offered)));
    assert_eq!(info("gen9vgc2025regi"), Some(("gen9vgc", None, OpenSheets::Offered)));
    assert_eq!(info("gen9vgc2025regibo3"), Some(("gen9vgc", Some(3), OpenSheets::Offered)));
    assert_eq!(info("gen9vgc2025regi@@@Best of = 5"), Some(("gen9vgc", Some(5), OpenSheets::Offered)));
    assert_eq!(info("gen9vgc2025regi@@@Best of = 4"), Some(("gen9vgc", None, OpenSheets::Offered)));
    assert_eq!(info("championsvgc"), Some(("championsvgc", None, OpenSheets::No)));
    assert_eq!(info("gen9randombattle"), Some(("gen9randombattle", None, OpenSheets::No)));
}

/// Which formats' servers validate teams (and so start a listed
/// battle-only forme as the forme it changes from).
#[test]
fn ladder_formats_validate_teams() {
    let validated = |s: &str| showdown_format(s).map(|f| f.validated);
    for id in ["gen9championsvgc2026regmc", "gen9championsvgc2026regmbbo3", "gen9vgc2025regi", "gen9vgc2026regfbo3"] {
        assert_eq!(validated(id), Some(true), "{id}");
    }
    assert_eq!(validated("championsvgc"), Some(true));
    assert_eq!(validated("gen9vgc"), Some(true));
    for id in ["gen9singles", "gen9doubles", "gen9customgame@@@!Team Preview", "gen9randombattle"] {
        assert_eq!(validated(id), Some(false), "{id}");
    }
    assert_eq!(validated("gen9doublescustomgame@@@Picked Team Size = 4,Adjust Level Down = 50"), Some(false));
}

/// A gen9vgc channel joined after switches changed the
/// team order, with a request listing the four brought.
#[test]
fn a_reconnect_after_team_preview_keeps_the_poke_lines_ids() {
    let format = Format::gen9_vgc();
    let me = SideId::P1;
    let teams = [sample_team(&format), sample_team(&format)];
    let mut battle = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(6_000_500))).unwrap();
    let mut rng = BattleRng::new(Seed::fast(6_000_501));
    let r = Renderer::new(&format, Perspective::Player(me));
    let mut lines = r.header();
    let mut chunk: Vec<BattleEvent> = battle.drain_events().collect();
    let mut permuted = 0;
    while !battle.is_over() && permuted < 5 {
        lines.extend(r.log(chunk.iter()));
        let team = &battle.state().side(me).team;
        if team.iter().enumerate().any(|(i, m)| m.id as usize != i) {
            permuted += 1;
            let want = battle.request_data(me);
            let mut p = ChannelParser::new(&format, None);
            let got = p.first_request(&lines, &r.request_json(&want), &format).unwrap();
            let ids = |d: &pikapro::request::RequestData| d.pokemon.iter().map(|m| m.id).collect::<Vec<_>>();
            assert_eq!(ids(&got.data), ids(&want));
            // Read as ahead of the lines, and the same ids at a request
            // read without them are positions.
            let mut q = ChannelParser::new(&format, None);
            let by_position = q.request(&r.request_json(&want), &format).unwrap();
            assert_eq!(ids(&by_position.data), (0..want.pokemon.len() as u8).collect::<Vec<_>>());
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
    assert!(permuted > 0, "the order never changed");
}

/// A request entry for `tests`: name, details, condition.
fn entry(name: &str, details: &str, condition: &str) -> String {
    format!(
        r#"{{"ident":"p1: {name}","details":"{details}","condition":"{condition}","active":false,"stats":{{"atk":1,"def":1,"spa":1,"spd":1,"spe":1}},"moves":["tackle"],"baseAbility":"static","item":"","ability":"static"}}"#
    )
}

/// A Mega's details fit the `|poke|` line of the species it came from
/// before another forme of its base species (Raichu-Mega-Y is Raichu's,
/// not Raichu-Alola's), when a rejoin's request shows the Mega.
#[test]
fn a_mega_fits_its_own_species_poke_line() {
    let format = Format::champions_vgc();
    let lines =
        ["|poke|p1|Raichu-Alola, L50, M|", "|poke|p1|Thievul, L50, M|", "|poke|p1|Raichu, L50, M|", "|teampreview|4"];
    let mons = [entry("Raichu", "Raichu-Mega-Y, L50, M", "100/153"), entry("Thievul", "Thievul, L50, M", "10/168")];
    let json = format!(r#"{{"wait":true,"side":{{"id":"p1","pokemon":[{}]}}}}"#, mons.join(","));
    let mut p = ChannelParser::new(&format, Some(SideId::P1));
    let r = p.first_request(&lines, &json, &format).unwrap();
    assert_eq!(r.data.pokemon.iter().map(|m| m.id).collect::<Vec<_>>(), [2, 1]);
}

/// Heal Bell names a benched Pokémon by name: of two of the player's own
/// of that name, the cure is of the one its request showed with the
/// status.
#[test]
fn an_off_field_cure_is_of_the_one_with_the_status() {
    let format = Format::gen9_singles();
    let mons = [
        entry("Drowzee", "Drowzee, L50, M", "147/147"),
        entry("Arceus", "Arceus, L50", "207/207"),
        entry("Arceus", "Arceus-Ground, L50", "171/198 slp"),
    ];
    let json = format!(r#"{{"wait":true,"side":{{"id":"p1","pokemon":[{}]}}}}"#, mons.join(","));
    let mut p = ChannelParser::new(&format, Some(SideId::P1));
    p.request(&json, &format).unwrap();
    let mut cured = None;
    p.feed("|switch|p1a: Drowzee|Drowzee, L50, M|147/147", &mut |_| {});
    p.feed("|-curestatus|p1: Arceus|slp|[msg]", &mut |e| {
        if let BattleEvent::CureStatus { target, .. } | BattleEvent::CureStatusFrom { target, .. } = e {
            cured = Some(target.id);
        }
    });
    assert_eq!(cured, Some(2));
}

/// A switch-in's details name a forme; of two of the player's own of one
/// name whose request shows other formes, it is the one that reached a
/// forme in battle (Ogerpon-Teal-Tera), not another forme
/// (Ogerpon-Wellspring).
#[test]
fn a_switch_in_is_the_one_in_a_battle_forme() {
    let format = Format::gen9_singles();
    let mons = [
        entry("Garchomp", "Garchomp, L50, M", "204/204"),
        entry("Ogerpon", "Ogerpon-Wellspring, L50, F", "146/160"),
        entry("Ogerpon", "Ogerpon-Teal-Tera, L50, F", "158/158"),
    ];
    let json = format!(r#"{{"wait":true,"side":{{"id":"p1","pokemon":[{}]}}}}"#, mons.join(","));
    let mut p = ChannelParser::new(&format, Some(SideId::P1));
    p.request(&json, &format).unwrap();
    let ids = {
        let mut ids = Vec::new();
        p.feed("|switch|p1a: Ogerpon|Ogerpon, L50, F|158/158", &mut |e| {
            if let BattleEvent::Switch { actor, .. } = e {
                ids.push(actor.id);
            }
        });
        ids
    };
    assert_eq!(ids, [2]);
}

/// A forme reached in battle and kept off the field (Morpeko-Hangry, as an
/// Illusion of it shows it too) is the Pokémon of that name and base
/// species, not another.
#[test]
fn a_kept_battle_forme_is_the_same_pokemon() {
    let format = Format::gen9_doubles();
    let ids = switch_ids(
        &format,
        SideId::P1,
        &[
            "|switch|p2a: Sparky|Morpeko, L50, F|100/100",
            "|switch|p2b: Nibbles|Pawmo, L50, F|100/100",
            "|switch|p2a: Kyurem|Kyurem, L50|100/100",
            "|switch|p2b: Sparky|Morpeko-Hangry, L50, F|90/100",
        ],
    );
    assert_eq!(ids, [0, 1, 2, 0]);
}

/// The first move heal (Revival Blessing's) a parser reads in `lines`:
/// its target.
fn healed(format: &Format, me: SideId, lines: &[&str]) -> Option<Actor> {
    let mut p = ChannelParser::new(format, Some(me));
    let mut healed = None;
    for line in lines {
        p.feed(line, &mut |e| {
            if let BattleEvent::MoveHeal { target, .. } = e {
                healed.get_or_insert(target);
            }
        });
    }
    healed
}

/// The heal (or any line) of a foe the battle never showed by its own name,
/// an Illusion user that fainted disguised, which Revival Blessing names:
/// its name is the species of one unclaimed `|poke|` line, whose id it takes.
#[test]
fn a_foe_named_by_its_species_takes_its_poke_line() {
    let target = healed(
        &Format::gen9_vgc(),
        SideId::P2,
        &[
            "|poke|p1|Arceus, L50|",
            "|poke|p1|Murkrow, L50, F|",
            "|poke|p1|Zoroark, L50, M|",
            "|poke|p1|Dondozo, L50, F|",
            "|teampreview|4",
            "|start",
            "|switch|p1a: Nibbles|Arceus, L50|100/100",
            "|switch|p1b: Mr. Bubbles|Murkrow, L50, F|100/100",
            "|-heal|p1: Zoroark|50/100|[from] move: Revival Blessing",
        ],
    );
    assert_eq!(target.expect("a heal").id, 2, "Zoroark's |poke| line");
}

/// A foe's Revival Blessing names one of two Pokémon of one name (not in a
/// legal team): the fainted one.
#[test]
fn a_foe_s_revival_is_of_the_fainted_one() {
    let target = healed(
        &Format::gen9_doubles(),
        SideId::P2,
        &[
            "|switch|p1a: Vivillon|Vivillon-River, L50, F|100/100",
            "|switch|p1b: X|Forretress, L50, F|100/100",
            "|switch|p1a: Vivillon|Vivillon-Continental, L50, F|100/100",
            "|faint|p1a: Vivillon",
            "|-heal|p1: Vivillon|50/100|[from] move: Revival Blessing",
        ],
    );
    assert_eq!(target.expect("a heal").id, 2, "the Continental, which fainted");
}
