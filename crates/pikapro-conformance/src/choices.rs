//! Choice strings against Pokémon Showdown.
//!
//! [`check_choices`] replays a random scenario on pikapro and, before
//! each recorded choice, tries a batch of other spellings for that side
//! ([`probes_for`]: moves and switches by name, targets, gimmicks,
//! `default`, `team` variants, passes, garbage). Each probe's verdict from
//! [`parse_choice_for`] (and core's validation) must equal Showdown's
//! (`tools/showdown-oracle/choices.mjs`): accepted with the same actions,
//! or rejected with the same message.

use std::io::Write as _;
use std::process::{Command, Stdio};

use pikapro::state::Volatiles;
use pikapro::{Battle, BattleRng, Choice, Format, Gimmick, Request, Seed, SideId, SlotAction, Target};
use pikapro_protocol::{format_choice, parse_choice_for};

use crate::{Scenario, new_battle, oracle_dir, quote, random_scenario, scenario_json, showdown_format};

/// The spelling [`random_scenario`] records for a sampled choice: usually
/// [`format_choice`]'s, sometimes another one Showdown reads as the same
/// actions (moves and switches by name, `+1` targets, `terastal`, gimmick
/// before target, `skip`, dropped trailing passes, extra spaces, `team` with
/// commas). Returns the text and the choice to submit: `default`, now and
/// then, stands for whatever `autoChoose` picks (in doubles, a move with no
/// target: a random one).
/// `names` is the side's nickname table (`Scenario::name_table`): a
/// switch by name uses the Pokémon's name, as Showdown reads it.
pub fn spell_choice(
    battle: &Battle,
    side: SideId,
    choice: Choice,
    names: &[String],
    rng: &mut BattleRng,
) -> (String, Choice) {
    let format = battle.format();
    let canonical = format_choice(&choice, format.active_per_side());
    let roll = rng.random(20);
    if roll == 0
        && !matches!(choice, Choice::Team(_))
        && let Ok(auto) = parse_choice_for(battle, side, "default", names)
        && battle.legal_actions(side).is_legal(&auto)
    {
        return ("default".into(), auto);
    }
    if roll >= 6 {
        return (canonical, choice);
    }
    let spelled = match choice {
        Choice::Team(order) => {
            let n: Vec<String> = order.iter().map(|i| (i + 1).to_string()).collect();
            // Brackets only for exactly the Pokémon brought: master's
            // `chooseTeam` doesn't trim a bracketed list.
            let exact = match battle.request(side) {
                Request::TeamPreview { bring } => {
                    order.len() == battle.state().side(side).team.len().min(bring as usize)
                }
                _ => false,
            };
            match rng.random(4) {
                0 => format!("team {}", n.join(", ")),
                1 => format!("team {}", n.join(",")),
                3 if exact => format!("team [{}]", n.join(", ")),
                _ => format!("team {}", n.concat()),
            }
        }
        Choice::Actions(actions) => {
            let dex = format.dex;
            let s = battle.state().side(side);
            let mut parts: Vec<String> = actions
                .iter()
                .take(format.active_per_side())
                .enumerate()
                .map(|(slot, a)| match *a {
                    SlotAction::Pass => if rng.random(2) == 0 { "pass" } else { "skip" }.to_string(),
                    SlotAction::Switch(i) => {
                        let mon = &s.team[i as usize];
                        let name = names.get(mon.id as usize).filter(|n| !n.is_empty());
                        match rng.random(3) {
                            0 => match name {
                                Some(name) => format!("switch {name}"),
                                None => {
                                    format!("switch {}", dex.species(dex.species(mon.set_species).base_species).name)
                                }
                            },
                            1 => format!("switch {}", dex.species(mon.species).name.to_lowercase()),
                            _ => format!("switch {}", i + 1),
                        }
                    }
                    SlotAction::Move { index, target, gimmick } => {
                        let mon = s.active(slot);
                        let plain = mon.is_none_or(|m| {
                            m.volatiles.has(Volatiles::MUST_RECHARGE | Volatiles::LOCKING) || !m.has_usable_move()
                        });
                        let name = match mon.map(|m| dex.move_(m.moves[index as usize].id).name) {
                            Some(n) if !plain => match rng.random(3) {
                                0 => n.to_string(),
                                1 => n.to_lowercase().replace(' ', ""),
                                _ => (index + 1).to_string(),
                            },
                            _ => (index + 1).to_string(),
                        };
                        let target = match target {
                            Some(Target::Foe(n)) if rng.random(2) == 0 => format!(" +{}", n + 1),
                            Some(Target::Foe(n)) => format!(" {}", n + 1),
                            Some(Target::Ally(n)) => format!(" -{}", n + 1),
                            None => String::new(),
                        };
                        let gimmick = match gimmick {
                            Some(Gimmick::Terastallize) if rng.random(2) == 0 => " terastal",
                            Some(Gimmick::Terastallize) => " terastallize",
                            Some(Gimmick::MegaEvolve) => " mega",
                            None => "",
                        };
                        if rng.random(2) == 0 {
                            format!("move {name}{target}{gimmick}")
                        } else {
                            format!("move {name}{gimmick}{target}")
                        }
                    }
                })
                .collect();
            if parts.len() > 1 && parts.last().is_some_and(|p| p == "pass") && rng.random(2) == 0 {
                parts.pop();
            }
            let sep = [",", ", ", " ,  "][rng.random(3) as usize];
            parts.join(sep)
        }
    };
    // Keep it only if it means the same.
    match parse_choice_for(battle, side, &spelled, names) {
        Ok(c) if c == choice => (spelled, choice),
        _ => (canonical, choice),
    }
}

/// Our verdict on `text`, in `choices.mjs`'s form: `ok <actions>` or
/// `err <message>`.
pub fn verdict(battle: &Battle, side: SideId, text: &str, names: &[String]) -> String {
    match parse_choice_for(battle, side, text, names) {
        Ok(choice) => match battle.legal_actions(side).check(&choice) {
            Ok(()) => format!("ok {}", actions(battle, side, &choice)),
            Err(e) => format!("err CORE REJECTS {choice:?}: {e}"),
        },
        Err(e) => format!("err {}", e.message),
    }
}

/// `choices.mjs`'s summary of a choice: move slots (`locked`, `struggle`),
/// targets in doubles, gimmicks, switch positions, passes; `team 3,1,2`.
pub fn actions(battle: &Battle, side: SideId, choice: &Choice) -> String {
    let active = battle.format().active_per_side();
    match choice {
        Choice::Team(order) => {
            let order: Vec<String> = order.iter().map(|i| (i + 1).to_string()).collect();
            format!("team {}", order.join(","))
        }
        Choice::Actions(actions) => {
            let s = battle.state().side(side);
            let parts: Vec<String> = actions
                .iter()
                .take(active)
                .enumerate()
                .map(|(slot, a)| match *a {
                    SlotAction::Pass => "pass".to_string(),
                    SlotAction::Switch(i) => format!("switch {}", i + 1),
                    SlotAction::Move { index, target, gimmick } => {
                        let Some(mon) = s.active(slot) else { return "move ?".into() };
                        const LOCKS: u128 = Volatiles::MUST_RECHARGE | Volatiles::LOCKING;
                        if mon.volatiles.has(LOCKS) {
                            return "move locked".into();
                        }
                        if !mon.has_usable_move() {
                            return "move struggle".into();
                        }
                        let mut out = format!("move {}", index + 1);
                        match target {
                            Some(Target::Foe(n)) if active > 1 => out.push_str(&format!(" {}", n + 1)),
                            Some(Target::Ally(n)) if active > 1 => out.push_str(&format!(" -{}", n + 1)),
                            _ => {}
                        }
                        match gimmick {
                            Some(Gimmick::MegaEvolve) => out.push_str(" mega"),
                            Some(Gimmick::Terastallize) => out.push_str(" terastallize"),
                            None => {}
                        }
                        out
                    }
                })
                .collect();
            parts.join(", ")
        }
    }
}

/// Other spellings for `side`'s decision, `real` being the recorded one;
/// `n` of them at random.
pub fn probes_for(battle: &Battle, side: SideId, real: &str, n: usize, rng: &mut BattleRng) -> Vec<String> {
    let format = battle.format();
    let dex = format.dex;
    let active = format.active_per_side();
    let s = battle.state().side(side);
    let mut c: Vec<String> = Vec::new();
    let push = |c: &mut Vec<String>, v: &str| c.push(v.to_string());
    let id = pikapro_data::to_id;
    let base_name = |i: usize| dex.species(dex.species(s.team[i].set_species).base_species).name;
    for v in ["default", "auto", "pass", "skip", "", "dance", "Move 1", "shift", "move 1, move 1, move 1"] {
        push(&mut c, v);
    }
    match battle.request(side) {
        Request::Wait => return Vec::new(),
        Request::TeamPreview { .. } => {
            for v in [
                "team",
                "team 1",
                "team 21",
                "team 2, 1",
                "team 1 2",
                "team 9",
                "team 11",
                "team 0",
                "team 3,1,4,2,5,6",
                "team 654321",
                "team 4321",
                "team 1,1",
                "team1",
                "team 12, ",
                " team 12",
                "move 1",
                "switch 2",
                // Numbers as JavaScript's `parseInt` reads them.
                "team 0x2, 1",
                "team 2abc, 1",
                "team 99999999999999999999999, 1",
                "team \u{1f600}1",
                "team 21\u{feff}",
                "team \u{85}2, 1",
                "\u{feff}team 2",
                // Master's bracketed `TEAMSPEC` (`Side.chooseTeam`): always
                // comma-separated, never trimmed to the team size.
                "team [2, 1]",
                "team [21]",
                "team []",
                "team [ ]",
                "team [1,2,3,4,5,6]",
                "team [ 2 , 1 ]",
                "team [2,1",
                "team 2,1]",
                "team [1,1]",
                "team [0x2]",
                "team [7]",
                "team[1]",
                "team [[1]]",
            ] {
                push(&mut c, v);
            }
        }
        Request::Move | Request::ForceSwitch { .. } => {
            for v in ["move", "switch", "switch 0", "move 0", "move 5", "testfight", "pass, pass", "switch 1, switch 1"]
            {
                push(&mut c, v);
            }
            // Numbers and white space as JavaScript reads them.
            for v in [
                "switch 0x3",
                "switch 3abc",
                "switch 1e1",
                "switch -0",
                "switch 99999999999999999999999",
                "switch \u{85}3",
                "switch \u{feff}3",
                "\u{feff}switch 3\u{3000}",
                "move 99999999999999999999",
                "move 01",
                "move 1\u{3000}1",
                "move 1\u{a0}-1",
                "move 1 \u{85}1",
            ] {
                push(&mut c, v);
            }
            for k in 1..=7 {
                c.push(format!("switch {k}"));
            }
            for i in 0..s.team.len() {
                let species = dex.species(s.team[i].species).name;
                c.push(format!("switch {}", base_name(i)));
                c.push(format!("switch {}", base_name(i).to_uppercase()));
                c.push(format!("switch {}", id(species)));
            }
            // Per active slot: its moves by number and name, with targets and
            // gimmicks, put in place of that slot's part of the real choice.
            let parts: Vec<&str> = real.split(',').map(str::trim).collect();
            for slot in 0..active {
                let Some(mon) = s.active(slot) else { continue };
                let mut variants: Vec<String> = Vec::new();
                for (j, m) in mon.moves.iter().enumerate() {
                    let name = dex.move_(m.id).name;
                    for spelled in [format!("{}", j + 1), name.to_string(), id(name)] {
                        for target in ["", " 1", " 2", " -1", " -2", " +1", " 3"] {
                            for gimmick in ["", " terastallize", " terastal", " mega", " dynamax"] {
                                variants.push(format!("move {spelled}{target}{gimmick}"));
                            }
                        }
                    }
                }
                variants.push("move recharge".into());
                variants.push("move struggle".into());
                variants.push("move 1 zmove".into());
                variants.push("move 1 megax".into());
                variants.push("move 1 ultra".into());
                variants.push("move 1 1 2".into());
                variants.push("move 1 mega terastallize".into());
                rng.shuffle(&mut variants);
                for v in variants.into_iter().take(8) {
                    let mut p: Vec<String> = parts.iter().map(|x| x.to_string()).collect();
                    if slot < p.len() {
                        p[slot] = v.clone();
                    }
                    c.push(p.join(", "));
                    c.push(v);
                }
            }
            if active > 1 && parts.len() > 1 {
                c.push(format!("{}, {}", parts[1], parts[0]));
                c.push(format!("{}, default", parts[0]));
                c.push(format!("default, {}", parts[1]));
                c.push(parts[0].to_string());
            }
        }
    }
    c.push(format!("  {}  ", real.replace(',', " ,  ")));
    rng.shuffle(&mut c);
    c.truncate(n);
    c
}

/// Runs `choices.mjs` on the scenario with `probes` (`(choice index, side,
/// text)`); Showdown's verdict per probe.
fn showdown_verdicts(s: &Scenario, probes: &[(usize, SideId, String)]) -> Result<Vec<String>, String> {
    let formatid = showdown_format(&s.format).ok_or("no Showdown format")?;
    let mut json = scenario_json(s, formatid);
    json.pop();
    let list: Vec<String> = probes
        .iter()
        .map(|(i, side, text)| format!("[{i},\"{}\",{}]", pikapro_protocol::side_id(*side), quote(text)))
        .collect();
    json.push_str(&format!(",\"probes\":[{}]}}", list.join(",")));
    let mut child = Command::new("node")
        .arg(oracle_dir().join("choices.mjs"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child.stdin.take().expect("piped").write_all(json.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(format!("choices.mjs failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let mut verdicts = vec![String::new(); probes.len()];
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("rejected ") {
            return Err(format!("Showdown rejected recorded choice {rest}"));
        }
        let Some(rest) = line.strip_prefix("probe ") else { continue };
        let (n, v) = rest.split_once(' ').ok_or("bad probe line")?;
        verdicts[n.parse::<usize>().map_err(|e| e.to_string())?] = v.to_string();
    }
    Ok(verdicts)
}

/// One scenario's probes compared with Showdown: how many, or the
/// mismatches (at most `show`).
pub fn check_choices(format: &Format, seed: u64, per_decision: usize, show: usize) -> Result<usize, String> {
    let s = random_scenario(format, seed);
    let mut rng = BattleRng::new(Seed::fast(seed ^ 0x7743_7765));
    let mut battle = new_battle(&s.format, &s.teams, s.seed);
    let names = s.name_table();
    let mut probes = Vec::new();
    let mut ours = Vec::new();
    // The side's team at each choice, to make sense of a mismatch.
    let mut teams = Vec::new();
    for (i, (side, text)) in s.choices.iter().enumerate() {
        let dex = s.format.dex;
        let team: Vec<String> = battle
            .state()
            .side(*side)
            .team
            .iter()
            .map(|m| format!("{} {}/{}", dex.species(m.species).name, m.hp, m.max_hp()))
            .collect();
        teams.push(team.join(", "));
        for p in probes_for(&battle, *side, text, per_decision, &mut rng) {
            ours.push(verdict(&battle, *side, &p, &names[side.index()]));
            probes.push((i, *side, p));
        }
        let choice = parse_choice_for(&battle, *side, text, &names[side.index()])
            .map_err(|e| format!("recorded choice {text:?}: {e}"))?;
        battle.submit(*side, choice).map_err(|e| format!("recorded choice {text:?}: {e}"))?;
    }
    let theirs = showdown_verdicts(&s, &probes)?;
    let mut mismatches = Vec::new();
    let dump = std::env::var_os("PIKAPRO_CHOICES_DUMP").is_some();
    for (((i, side, text), ours), theirs) in probes.iter().zip(&ours).zip(&theirs) {
        if dump {
            println!("{i} {side:?} {text:?}\n  ours {ours}\n  sd   {theirs}");
        }
        if ours != theirs {
            mismatches.push(format!(
                "  choice {i} ({}) {:?}\n    ours {ours}\n    sd   {theirs}\n    team {}",
                pikapro_protocol::side_id(*side),
                text,
                teams[*i]
            ));
        }
    }
    if mismatches.is_empty() {
        Ok(probes.len())
    } else {
        let n = mismatches.len();
        mismatches.truncate(show);
        Err(format!("{n} of {} probes differ:\n{}", probes.len(), mismatches.join("\n")))
    }
}
