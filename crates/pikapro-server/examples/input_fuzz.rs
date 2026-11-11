//! Robustness fuzzer for the text inputs a client controls: team import
//! (export and packed), choice strings, `|request|` JSON, player-channel
//! lines, and the battle stream's commands (`>start` and `>player` JSON,
//! choices, the rest). Valid inputs are generated from random battles,
//! then mutated; every parser runs under `catch_unwind` and is timed. A
//! panic or a slow call is printed with its input, which is also written
//! to `--out DIR` if given.
//!
//! ```sh
//! cargo run --release -p pikapro-server --example input_fuzz -- \
//!     --target all|team|choice|request|channel|stream --seconds 60 --seed 1 [--out DIR]
//! ```
//!
//! Unlike `stream_fuzz`, nothing is compared with Showdown: this looks
//! for panics, hangs and work out of proportion to the input.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use pikapro::{Battle, BattleOptions, BattleRng, Format, Seed, SideId};
use pikapro_conformance::{random_nicknames, random_team, showdown_format};
use pikapro_protocol::channel::{ChannelParser, parse_request, request_body};
use pikapro_protocol::{
    Perspective, Renderer, SetExtras, TeamSheet, choose, export_sheet, format_choice_for, import_sheet, pack_team,
    unpack_sets, unpack_team,
};
use pikapro_server::json::{Json, quote};
use pikapro_server::stream::extract;
use pikapro_server::{BattleStream, Channel, Message, PlayerStreams, StreamOptions, sets};

/// A call slower than this is reported (release build).
const SLOW: Duration = Duration::from_millis(250);
/// Mutated inputs are cut to this many bytes.
const MAX_LEN: usize = 300_000;

const TOKENS: &[&str] = &[
    "|",
    "]",
    ",",
    "\n",
    " ",
    "(",
    ")",
    " @ ",
    "@",
    "{",
    "}",
    "[",
    "]",
    "\"",
    "\\",
    ":",
    "/",
    "-",
    "+",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "24",
    "25",
    "100",
    "255",
    "256",
    "65535",
    "65536",
    "4294967296",
    "-1",
    "-0",
    "NaN",
    "Infinity",
    "1e999",
    "1e-999",
    "0x10",
    "99999999999999999999999",
    "null",
    "true",
    "false",
    "undefined",
    "p1",
    "p2",
    "p3",
    "p4",
    "p1a: ",
    "p2b: ",
    "p1c: ",
    "p2z: ",
    "p3a: ",
    "move ",
    "switch ",
    "team ",
    "pass",
    "default",
    "undo",
    "shift",
    "terastallize",
    "mega",
    "zmove",
    "dynamax",
    "max",
    "\u{0}",
    "\r",
    "\t",
    "\u{85}",
    "\u{a0}",
    "\u{feff}",
    "\u{202e}",
    "é",
    "\u{300}",
    "\u{1f600}",
    "\u{fffd}",
    "\\u0000",
    "\\ud800",
    "\\udc00",
    "\\u",
    "\"\"",
    "[]",
    "{}",
    "[[[[",
    "{{{{",
    "|split|p1",
    "|split|p2",
    "|request|",
    "|switch|",
    "|move|",
    "|-damage|",
    "|poke|",
    "|player|",
    "|teamsize|",
    "|turn|",
    "|win|",
    "|tie|",
    "|faint|",
    "|replace|",
    "|detailschange|",
    "|-formechange|",
    "|-transform|",
    "|showteam|",
    "|error|",
    "[from] ",
    "[of] ",
    "[silent]",
    "[still]",
    "/100",
    "/100y",
    "/100g",
    "/100r",
    " brn",
    " par",
    " fnt",
    " tox",
    ", L50",
    ", M",
    ", F",
    ", shiny",
    ", tera:",
    "Pikachu",
    "Ditto",
    "Zoroark",
    "Shedinja",
    "Arceus-*",
    "Urshifu-*",
    "Garchomp",
    "Protect",
    "Transform",
    "Illusion",
    "Imposter",
    "EVs: ",
    "IVs: ",
    "Level: ",
    "Ability: ",
    "Tera Type: ",
    "Nature",
    "Hidden Power [",
    "Happiness: ",
    "Shiny: Yes",
    "- ",
    "===",
    "\"rqid\":",
    "\"active\":",
    "\"side\":",
    "\"pokemon\":",
    "\"teamPreview\":true",
    "\"forceSwitch\":",
    "\"wait\":true",
    "\"moves\":",
    "\"maxChosenTeamSize\":",
    "\"condition\":",
    "\"ident\":",
    "\"details\":",
    "\"team\":",
    "\"seed\":",
    "\"formatid\":",
    "gen9customgame",
    "@@@",
    "sodium,",
    "gen5,",
    "gen9championsvgc2026regmc",
    ">start ",
    ">player p1 ",
    ">p1 ",
    ">p2 ",
    ">forcewin ",
    ">forcelose ",
    ">forcetie",
    ">tiebreak",
    ">reseed ",
    ">requestteam ",
    ">requestlog",
    ">requestexport",
    ">chat ",
    ">show-openteamsheets",
];

thread_local! {
    /// Inside [`Fuzzer::check`]: panics are reported there, not printed.
    static CHECKING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
}

fn boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// `text` with a few random edits, some drawn from `others`.
fn mutate(rng: &mut Rng, text: &str, others: &[String]) -> String {
    let mut s = text.to_string();
    let most = if rng.chance(4) { 12 } else { 3 };
    let edits = 1 + rng.below(most);
    for _ in 0..edits {
        let at = boundary(&s, rng.below(s.len() + 1));
        match rng.below(10) {
            0 | 1 => s.insert_str(at, TOKENS[rng.below(TOKENS.len())]),
            2 => {
                let end = boundary(&s, at + rng.below(32));
                s.replace_range(at..end, "");
            }
            3 => {
                // Repeat a piece, sometimes very many times.
                let end = boundary(&s, at + 1 + rng.below(40));
                let piece = s[at..end].to_string();
                let times = if rng.chance(8) { 1 + rng.below(20_000) } else { 1 + rng.below(4) };
                s.insert_str(end, &piece.repeat(times));
            }
            4 => {
                let end = boundary(&s, at + 1 + rng.below(8));
                s.replace_range(at..end, TOKENS[rng.below(TOKENS.len())]);
            }
            5 if !others.is_empty() => {
                let o = &others[rng.below(others.len())];
                let a = boundary(o, rng.below(o.len() + 1));
                let b = boundary(o, a + rng.below(200));
                s.insert_str(at, &o[a..b]);
            }
            6 => s.truncate(at),
            7 => {
                // A number replaced.
                if let Some(d) = s[at..].find(|c: char| c.is_ascii_digit()) {
                    let d = at + d;
                    let e = d + s[d..].find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len() - d);
                    let n =
                        ["0", "-1", "255", "256", "65536", "4294967297", "1e400", "99999999999999999999", "07", "NaN"];
                    s.replace_range(d..e, n[rng.below(n.len())]);
                }
            }
            8 => {
                // One field separator swapped for another.
                let seps = ['|', ',', ']', '\n', ' ', ':', '"'];
                if let Some(p) = s[at..].find(|c| seps.contains(&c)) {
                    let p = at + p;
                    let c = seps[rng.below(seps.len())];
                    s.replace_range(p..p + 1, &c.to_string());
                }
            }
            _ => {
                let c = char::from_u32(rng.below(0x3_0000) as u32).unwrap_or('?');
                s.insert(at, c);
            }
        }
        if s.len() > MAX_LEN {
            s.truncate(boundary(&s, MAX_LEN));
        }
    }
    s
}

struct Fuzzer {
    rng: Rng,
    out: Option<PathBuf>,
    found: usize,
    runs: u64,
}

impl Fuzzer {
    /// Runs `f` on `input`, reporting a panic or a slow call.
    fn check(&mut self, what: &str, input: &str, f: impl FnOnce()) {
        self.runs += 1;
        let start = Instant::now();
        CHECKING.with(|c| c.set(true));
        let r = catch_unwind(AssertUnwindSafe(f));
        CHECKING.with(|c| c.set(false));
        let took = start.elapsed();
        let problem = match r {
            Err(e) => Some(format!("panic: {}", pikapro_conformance::panic_message(&*e))),
            Ok(()) if took > SLOW => Some(format!("slow: {took:?} for {} bytes", input.len())),
            Ok(()) => None,
        };
        if let Some(problem) = problem {
            self.found += 1;
            let shown: String = input.chars().take(400).collect();
            println!("[{what}] {problem}\n  input ({} bytes): {shown:?}", input.len());
            if let Some(dir) = &self.out {
                let _ = std::fs::create_dir_all(dir);
                let _ = std::fs::write(dir.join(format!("{what}-{}.txt", self.found)), input);
            }
        }
    }
}

/// Valid inputs drawn from one random battle in `format`.
struct Corpus {
    format: Format,
    battle_states: Vec<(Battle, SideId)>,
    names: [Vec<String>; 2],
    choices: Vec<String>,
    teams_export: Vec<String>,
    teams_packed: Vec<String>,
    teams_json: Vec<String>,
    requests: Vec<String>,
    /// A player's channel, line by line.
    channels: Vec<(SideId, Vec<String>)>,
    script: Vec<String>,
}

fn corpus(format: &Format, seed: u64) -> Corpus {
    let mut rng = BattleRng::new(Seed::fast(seed));
    // Teams the engine takes (the generator may repeat a species).
    let teams = loop {
        let teams = [random_team(format, &mut rng), random_team(format, &mut rng)];
        if teams.iter().all(|t| t.validate(format).is_ok()) {
            break teams;
        }
    };
    let nicknames = random_nicknames(&teams, seed);
    let sheets: Vec<TeamSheet> = (0..2)
        .map(|s| TeamSheet {
            team: teams[s].clone(),
            extras: (0..teams[s].sets.len())
                .map(|i| SetExtras { name: nicknames[s].get(i).cloned().unwrap_or_default(), ..SetExtras::default() })
                .collect(),
        })
        .collect();
    let names = [sheets[0].nicknames(format), sheets[1].nicknames(format)];
    let teams_export: Vec<String> = sheets.iter().map(|s| export_sheet(s, format)).collect();
    let teams_packed: Vec<String> = sheets.iter().map(|s| pack_team(s, format)).collect();
    let teams_json: Vec<String> =
        teams_packed.iter().map(|p| Json::Array(sets::unpack(p).expect("unpacks")).stringify()).collect();

    // The engine's battle, for choices and requests.
    let mut battle = Battle::new(format.clone(), teams.clone(), BattleOptions::new(Seed::fast(seed))).unwrap();
    let mut battle_states = Vec::new();
    let mut choices = Vec::new();
    let mut requests = Vec::new();
    let renderer = Renderer::new(format, Perspective::Player(SideId::P1));
    let mut turns = 0;
    while !battle.is_over() && turns < 200 {
        turns += 1;
        for side in SideId::BOTH {
            let legal = battle.legal_actions(side);
            if !legal.is_waiting() {
                let c = legal.sample(&mut rng);
                choices.push(format_choice_for(&battle, side, &c, &names[side.index()]));
                if battle_states.len() < 40 || rng.random(8) == 0 {
                    battle_states.push((battle.clone(), side));
                }
                requests.push(renderer.request_json(&battle.request_data(side)));
                let _ = battle.submit(side, c);
            }
        }
        battle.drain_events().for_each(drop);
    }

    // The stream's script and channels.
    let formatid = showdown_format(format).expect("a Showdown format");
    let mut script = vec![format!(">start {{\"formatid\":{},\"seed\":\"sodium,{seed:016x}\"}}", quote(formatid))];
    for s in 0..2 {
        let team = if rng.random(3) == 0 { teams_json[s].clone() } else { quote(&teams_packed[s]) };
        script.push(format!(">player p{} {{\"name\":\"P{s}\",\"team\":{team}}}", s + 1));
    }
    let mut stream = BattleStream::new(StreamOptions::default());
    let mut channels = vec![(SideId::P1, Vec::new()), (SideId::P2, Vec::new())];
    let collect = |stream: &mut BattleStream, channels: &mut Vec<(SideId, Vec<String>)>| {
        for m in stream.drain() {
            match m {
                Message::Update(data) => {
                    for (side, lines) in channels.iter_mut() {
                        lines.extend(extract(&data, Perspective::Player(*side)));
                    }
                }
                Message::SideUpdate { side, data } => channels[side.index()].1.extend(data.lines().map(String::from)),
                _ => {}
            }
        }
    };
    for c in &script {
        stream.write(c).unwrap();
    }
    collect(&mut stream, &mut channels);
    let mut steps = 0;
    while !stream.battle_ended() && steps < 400 {
        steps += 1;
        let mut wrote = false;
        for side in SideId::BOTH {
            if !stream.waiting_for(side) {
                continue;
            }
            let Some(b) = stream.battle() else { break };
            let legal = b.legal_actions(side);
            if legal.is_waiting() {
                continue;
            }
            let c = legal.sample(&mut rng);
            let line = format!(">p{} {}", side.index() + 1, format_choice_for(b, side, &c, &names[side.index()]));
            let _ = stream.write(&line);
            script.push(line);
            wrote = true;
        }
        collect(&mut stream, &mut channels);
        if !wrote {
            break;
        }
    }
    Corpus {
        format: format.clone(),
        battle_states,
        names,
        choices,
        teams_export,
        teams_packed,
        teams_json,
        requests,
        channels,
        script,
    }
}

fn fuzz_team(f: &mut Fuzzer, c: &Corpus, n: usize) {
    let others: Vec<String> = c.teams_export.iter().chain(&c.teams_packed).cloned().collect();
    for _ in 0..n {
        let base = &others[f.rng.below(others.len())];
        let text = mutate(&mut f.rng, base, &others);
        let format = &c.format;
        f.check("team", &text, || {
            let _ = unpack_sets(&text);
            for sheet in [unpack_team(&text, format).ok(), import_sheet(&text, format).ok()].into_iter().flatten() {
                let packed = pack_team(&sheet, format);
                let _ = unpack_team(&packed, format);
                let _ = import_sheet(&export_sheet(&sheet, format), format);
                let _ = sheet.nicknames(format);
                let _ = sheet.team.validate(format);
            }
        });
        // The stream's own reader of packed teams and JSON sets.
        f.check("sets", &text, || {
            if let Some(mut sets) = sets::unpack(&text) {
                for set in &mut sets {
                    let _ = sets::construct(set, format.dex);
                }
                let _ = sets::pack(&sets);
            }
        });
    }
    for _ in 0..n / 4 {
        let base = &c.teams_json[f.rng.below(2)];
        let text = mutate(&mut f.rng, base, &c.teams_json);
        f.check("json-sets", &text, || {
            if let Ok(Json::Array(mut sets)) = Json::parse(&text) {
                for set in &mut sets {
                    let _ = sets::construct(set, c.format.dex);
                }
                let _ = sets::pack(&sets);
            }
        });
    }
}

fn fuzz_choice(f: &mut Fuzzer, c: &Corpus, n: usize) {
    if c.battle_states.is_empty() {
        return;
    }
    for _ in 0..n {
        let (battle, side) = &c.battle_states[f.rng.below(c.battle_states.len())];
        let base = &c.choices[f.rng.below(c.choices.len())];
        let text = mutate(&mut f.rng, base, &c.choices);
        let names = &c.names[side.index()];
        f.check("choice", &text, || {
            let _ = choose(battle, *side, &text, names, None);
        });
    }
}

fn fuzz_request(f: &mut Fuzzer, c: &Corpus, n: usize) {
    for _ in 0..n {
        let base = &c.requests[f.rng.below(c.requests.len())];
        let text = mutate(&mut f.rng, base, &c.requests);
        f.check("request", &text, || {
            let _ = parse_request(&text, &c.format);
            let mut p = ChannelParser::new(&c.format, Some(SideId::P1));
            let _ = p.request(&text, &c.format);
            let _ = Json::parse(&text);
        });
    }
}

fn fuzz_channel(f: &mut Fuzzer, c: &Corpus, n: usize) {
    let all: Vec<String> = c.channels.iter().flat_map(|(_, l)| l.iter().cloned()).collect();
    if all.is_empty() {
        return;
    }
    for _ in 0..n {
        let (side, lines) = &c.channels[f.rng.below(2)];
        // A channel with a few lines changed, dropped, repeated or moved.
        let mut mutated: Vec<String> = Vec::with_capacity(lines.len());
        let rate = 1 + f.rng.below(40);
        for line in lines {
            match f.rng.below(rate * 4) {
                0 => mutated.push(mutate(&mut f.rng, line, &all)),
                1 => {}
                2 => {
                    mutated.push(line.clone());
                    mutated.push(all[f.rng.below(all.len())].clone());
                }
                3 => mutated.extend(std::iter::repeat_n(line.clone(), 1 + f.rng.below(50))),
                _ => mutated.push(line.clone()),
            }
        }
        let me = if f.rng.chance(4) { None } else { Some(*side) };
        let joined = mutated.join("\n");
        let format = &c.format;
        let cut = f.rng.below(mutated.len() + 1);
        f.check("channel", &joined, || {
            let mut p = ChannelParser::new(format, me);
            p.set_exact_hp(me.is_some());
            for line in &mutated {
                match request_body(line) {
                    Some(json) => {
                        let _ = p.request(json, format);
                    }
                    None => {
                        let _ = p.feed(line, &mut |_| {});
                    }
                }
            }
            // A reconnect: the lines so far, then a request.
            if let Some(json) = mutated[cut..].iter().find_map(|l| request_body(l)) {
                let mut q = ChannelParser::new(format, me);
                let _ = q.first_request(&mutated[..cut], json, format);
            }
        });
    }
}

fn fuzz_stream(f: &mut Fuzzer, c: &Corpus, n: usize) {
    for _ in 0..n {
        let mut script = c.script.clone();
        let edits = 1 + f.rng.below(6);
        for _ in 0..edits {
            let i = f.rng.below(script.len());
            match f.rng.below(6) {
                0 | 1 => script[i] = mutate(&mut f.rng, &script[i], &c.script),
                2 => {
                    let k = f.rng.below(c.script.len());
                    let line = mutate(&mut f.rng, &c.script[k], &c.script);
                    script.insert(i, line);
                }
                3 => {
                    script.remove(i);
                    if script.is_empty() {
                        script.push(String::new());
                    }
                }
                4 => {
                    let t = TOKENS.iter().filter(|t| t.starts_with('>')).collect::<Vec<_>>();
                    let cmd = t[f.rng.below(t.len())];
                    let arg = mutate(&mut f.rng, "p1", &c.script);
                    script.insert(i, format!("{cmd}{arg}"));
                }
                _ => {
                    let j = f.rng.below(script.len());
                    script.swap(i, j);
                }
            }
        }
        let batch = f.rng.chance(3);
        let players = f.rng.chance(4);
        let joined = script.join("\n");
        f.check("stream", &joined, || {
            if players {
                let mut ps = PlayerStreams::new(BattleStream::new(StreamOptions::default()));
                for line in &script {
                    let _ = match (line.strip_prefix(">p1 "), line.strip_prefix(">p2 ")) {
                        (Some(choice), _) => ps.write(Channel::Player(SideId::P1), choice),
                        (_, Some(choice)) => ps.write(Channel::Player(SideId::P2), choice),
                        _ => ps.write(Channel::Omniscient, line),
                    };
                    for ch in Channel::ALL {
                        let _ = ps.drain(ch);
                    }
                }
                return;
            }
            let mut s = BattleStream::new(StreamOptions::default());
            if batch {
                let _ = s.write(&joined);
                let _ = s.drain();
            } else {
                for line in &script {
                    let _ = s.write(line);
                    let _ = s.drain();
                }
            }
            s.write_end();
            let _ = s.drain();
        });
    }
}

fn main() {
    let mut target = "all".to_string();
    let mut seconds = 60u64;
    let mut seed = 1u64;
    let mut out = None;
    let mut args = pikapro_conformance::cli::Args::from_env();
    while let Some(flag) = args.next_flag() {
        let flag = flag.as_str();
        match flag {
            "--target" => target = args.value(flag),
            "--seconds" => seconds = args.num(flag),
            "--seed" => seed = args.num(flag),
            "--out" => out = Some(PathBuf::from(args.value(flag))),
            other => panic!("unknown argument {other}"),
        }
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !CHECKING.with(|c| c.get()) {
            default_hook(info);
        }
    }));
    let mut f = Fuzzer { rng: Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1), out, found: 0, runs: 0 };
    let start = Instant::now();
    let formats = Format::presets();
    let mut round = 0u64;
    while start.elapsed() < Duration::from_secs(seconds) {
        let format = &formats[(round % formats.len() as u64) as usize];
        let c = corpus(format, seed.wrapping_mul(1_000_003).wrapping_add(round));
        round += 1;
        let all = target == "all";
        if all || target == "team" {
            fuzz_team(&mut f, &c, 300);
        }
        if all || target == "choice" {
            fuzz_choice(&mut f, &c, 1000);
        }
        if all || target == "request" {
            fuzz_request(&mut f, &c, 500);
        }
        if all || target == "channel" {
            fuzz_channel(&mut f, &c, 30);
        }
        if all || target == "stream" {
            fuzz_stream(&mut f, &c, 30);
        }
    }
    println!("{target}: {} runs on {round} battles in {:?}, {} problems", f.runs, start.elapsed(), f.found);
}
