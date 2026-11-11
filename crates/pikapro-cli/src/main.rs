//! `pikapro`: run, inspect and benchmark battles from the command line.
//!
//! Dependency-free on purpose (no clap); the argument surface is small.

use std::io::Write;
use std::process::ExitCode;
use std::time::Instant;

use pikapro::testing::sample_team;
use pikapro::{Battle, BattleOptions, BattleRng, Format, Seed, SideId, Team, TeamPreview};
use pikapro_protocol::{Perspective, Renderer, TeamSheet, format_choice, import_sheet};

const USAGE: &str = "\
pikapro - Pokémon battle simulator

USAGE:
    pikapro formats
    pikapro play  [--format ID] [--seed N] [--perspective omniscient|p1|p2|spectator]
               [--team1 FILE] [--team2 FILE] [--requests]
    pikapro bench [--format ID] [--seed N] [--battles N] [--threads N] [--events]
               [--team1 FILE] [--team2 FILE]
    pikapro validate [--format ID] FILE

The format defaults to championsvgc. Both players choose uniformly random
legal actions. Teams default to the built-in sample team; FILE is Showdown
export format. `play` prints the log of the chosen perspective
(omniscient by default) and each choice its viewer may see as
`><side> <choice>`: both players' for omniscient, one player's for p1
or p2, none for spectator. `--requests` also prints those players'
requests, as `|request|<side>|<JSON>`.
`validate` checks a team with Showdown's team validator (pikapro-validate)
and the engine's rules. gen9vgc and championsvgc validate as their VGC
regulations, championsbss as Champions BSS Reg M-C, gen9ou as [Gen 9] OU;
gen9singles, gen9doubles (custom games) and the random battles as Anything
Goes with Species Clause.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("formats") => formats(),
        Some("play") => Opts::parse("play", &args[1..]).and_then(|o| play(&o)),
        Some("bench") => Opts::parse("bench", &args[1..]).and_then(|o| bench(&o)),
        Some("validate") => validate(&args[1..]),
        Some("-h" | "--help" | "help") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Opts {
    format: Format,
    seed: u64,
    battles: u64,
    threads: usize,
    events: bool,
    perspective: Perspective,
    teams: [Option<String>; 2],
    /// `play` prints every request.
    requests: bool,
}

/// The flags each command takes (another is an error, not ignored).
fn takes(cmd: &str, flag: &str) -> bool {
    match cmd {
        "play" => matches!(flag, "--format" | "--seed" | "--perspective" | "--team1" | "--team2" | "--requests"),
        "bench" => {
            matches!(flag, "--format" | "--seed" | "--battles" | "--threads" | "--events" | "--team1" | "--team2")
        }
        _ => flag == "--format",
    }
}

impl Opts {
    fn parse(cmd: &str, args: &[String]) -> Result<Opts, String> {
        let mut o = Opts {
            format: Format::champions_vgc(),
            seed: 0,
            battles: 10_000,
            threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
            events: false,
            perspective: Perspective::Omniscient,
            teams: [None, None],
            requests: false,
        };
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            if flag.starts_with("--") && !takes(cmd, flag) {
                return Err(format!("{cmd} doesn't take {flag}\n\n{USAGE}"));
            }
            let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
            let num = |v: &str| v.parse::<u64>().map_err(|_| format!("{flag}: bad number {v:?}"));
            match flag.as_str() {
                "--format" => {
                    let id = value()?;
                    o.format = Format::by_id(id).ok_or_else(|| format!("unknown format {id:?}"))?;
                }
                "--seed" => o.seed = num(value()?)?,
                "--battles" => o.battles = num(value()?)?,
                "--threads" => o.threads = num(value()?)?.max(1) as usize,
                "--events" => o.events = true,
                "--requests" => o.requests = true,
                "--team1" => o.teams[0] = Some(read(value()?)?),
                "--team2" => o.teams[1] = Some(read(value()?)?),
                "--perspective" => {
                    o.perspective = match value()?.as_str() {
                        "omniscient" => Perspective::Omniscient,
                        "p1" => Perspective::Player(SideId::P1),
                        "p2" => Perspective::Player(SideId::P2),
                        "spectator" => Perspective::Spectator,
                        v => return Err(format!("unknown perspective {v:?}")),
                    }
                }
                other => return Err(format!("unknown flag {other:?}")),
            }
        }
        Ok(o)
    }

    /// The teams with their nicknames and shininess (the sample team has
    /// none).
    fn sheets(&self) -> Result<[TeamSheet; 2], String> {
        let load = |text: &Option<String>| match text {
            Some(t) => import_sheet(t, &self.format).map_err(|e| e.to_string()),
            None => Ok(TeamSheet::new(sample_team(&self.format))),
        };
        Ok([load(&self.teams[0])?, load(&self.teams[1])?])
    }

    fn teams(&self) -> Result<[Team; 2], String> {
        Ok(self.sheets()?.map(|s| s.team))
    }
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

/// `pikapro validate [--format ID] FILE`: prints the team's problems in
/// Showdown's words, or "valid".
fn validate(args: &[String]) -> Result<(), String> {
    let (flags, file) = match args.split_last() {
        Some((file, flags)) if !file.starts_with("--") => (flags, file),
        _ => return Err(format!("validate needs a team file\n\n{USAGE}")),
    };
    let o = Opts::parse("validate", flags)?;
    // Nicknames and shininess matter to the validator too.
    let sheet = import_sheet(&read(file)?, &o.format).map_err(|e| e.to_string())?;
    let info: Vec<pikapro_validate::SetInfo> = sheet
        .team
        .sets
        .iter()
        .zip(&sheet.extras)
        .map(|(set, extras)| pikapro_validate::SetInfo::new(set, &extras.name, extras.shiny))
        .collect();
    let messages = pikapro_validate::check_preset(&o.format, &sheet.team, &info).messages;
    if messages.is_empty() {
        println!("valid");
        return Ok(());
    }
    for line in &messages {
        println!("{line}");
    }
    Err(format!("{} problem(s)", messages.len()))
}

fn formats() -> Result<(), String> {
    for f in Format::presets() {
        let preview = match f.team_preview {
            TeamPreview::None => "none".to_string(),
            TeamPreview::All => "bring all".to_string(),
            TeamPreview::Pick(n) => format!("bring {n}"),
        };
        println!("{:<14} {:?}, {:?}, team preview: {preview}, levels: {:?}", f.id, f.game_type, f.mechanics, f.level);
    }
    Ok(())
}

/// Whether `perspective` may see `side`'s choices and requests.
fn sees(perspective: Perspective, side: SideId) -> bool {
    match perspective {
        Perspective::Omniscient => true,
        Perspective::Player(me) => me == side,
        Perspective::Spectator => false,
    }
}

/// One random-vs-random battle, printed as a Showdown protocol log with the
/// submitted choices as `>p1 ...` lines (the simulator's input syntax), of
/// the players the perspective may see. A reader that stops early (`pikapro
/// play | head`) ends it quietly.
fn play(o: &Opts) -> Result<(), String> {
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    match play_to(o, &mut out).and_then(|()| out.flush()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e.to_string()),
        _ => Ok(()),
    }
}

fn play_to(o: &Opts, out: &mut impl Write) -> std::io::Result<()> {
    let format = o.format.clone();
    let active = format.active_per_side();
    let sheets = o.sheets().map_err(std::io::Error::other)?;
    let renderer = Renderer::new(&format, o.perspective).with_sheets([&sheets[0], &sheets[1]], &format);
    let teams = sheets.map(|s| s.team);
    let mut battle = Battle::new(format, teams, BattleOptions::new(Seed::fast(o.seed)))
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut policy = BattleRng::new(Seed::fast(o.seed ^ 0x5EED));
    for line in renderer.header() {
        writeln!(out, "{line}")?;
    }
    let requests = |out: &mut dyn Write, battle: &Battle| -> std::io::Result<()> {
        if o.requests && !battle.is_over() {
            for side in SideId::BOTH.into_iter().filter(|&s| sees(o.perspective, s)) {
                writeln!(
                    out,
                    "|request|{}|{}",
                    pikapro_protocol::side_id(side),
                    renderer.request_json(&battle.request_data(side))
                )?;
            }
        }
        Ok(())
    };
    write!(out, "{}", renderer.lines(battle.drain_events().as_slice()))?;
    requests(out, &battle)?;
    while !battle.is_over() {
        for side in SideId::BOTH {
            let legal = battle.legal_actions(side);
            if legal.is_waiting() {
                continue;
            }
            let choice = legal.sample(&mut policy);
            if sees(o.perspective, side) {
                writeln!(out, ">{} {}", pikapro_protocol::side_id(side), format_choice(&choice, active))?;
            }
            battle.submit(side, choice).map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        write!(out, "{}", renderer.lines(battle.drain_events().as_slice()))?;
        requests(out, &battle)?;
    }
    Ok(())
}

#[derive(Default)]
struct Totals {
    battles: u64,
    turns: u64,
    decisions: u64,
    wins: [u64; 2],
    ties: u64,
}

fn bench(o: &Opts) -> Result<(), String> {
    let teams = o.teams()?;
    // Validate once up front so worker threads can unwrap.
    Battle::new(o.format.clone(), teams.clone(), BattleOptions::new(Seed::fast(0))).map_err(|e| e.to_string())?;
    let threads = o.threads.min(o.battles.max(1) as usize);
    println!(
        "format {}, {} battles, {threads} threads, events {}",
        o.format.id,
        o.battles,
        if o.events { "on" } else { "off" }
    );

    let start = Instant::now();
    let totals: Vec<Totals> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let teams = teams.clone();
                scope.spawn(move || {
                    let mut totals = Totals::default();
                    let mut i = t as u64;
                    while i < o.battles {
                        run_one(o, &teams, o.seed.wrapping_add(i), &mut totals);
                        i += threads as u64;
                    }
                    totals
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("worker panicked")).collect()
    });
    let elapsed = start.elapsed().as_secs_f64();

    let mut sum = Totals::default();
    for t in totals {
        sum.battles += t.battles;
        sum.turns += t.turns;
        sum.decisions += t.decisions;
        sum.wins[0] += t.wins[0];
        sum.wins[1] += t.wins[1];
        sum.ties += t.ties;
    }
    let per_sec = |n: u64| n as f64 / elapsed;
    println!("elapsed        {elapsed:.3} s");
    println!("battles/s      {:.0}", per_sec(sum.battles));
    println!("turns/s        {:.0}", per_sec(sum.turns));
    println!("decisions/s    {:.0}", per_sec(sum.decisions));
    println!("avg turns      {:.1}", sum.turns as f64 / sum.battles.max(1) as f64);
    println!("p1/p2/tie      {}/{}/{}", sum.wins[0], sum.wins[1], sum.ties);
    Ok(())
}

fn run_one(o: &Opts, teams: &[Team; 2], seed: u64, totals: &mut Totals) {
    let mut options = BattleOptions::new(Seed::fast(seed));
    options.log_events = o.events;
    let mut battle = Battle::new(o.format.clone(), teams.clone(), options).expect("teams validated");
    let mut policy = BattleRng::new(Seed::fast(!seed));
    while !battle.is_over() {
        for side in SideId::BOTH {
            let legal = battle.legal_actions(side);
            if legal.is_waiting() {
                continue;
            }
            battle.submit(side, legal.sample(&mut policy)).expect("sampled choice is legal");
            totals.decisions += 1;
        }
        if o.events {
            battle.drain_events().for_each(drop);
        }
    }
    let result = battle.result().expect("battle ended");
    totals.battles += 1;
    totals.turns += result.turns as u64;
    match result.winner {
        Some(side) => totals.wins[side.index()] += 1,
        None => totals.ties += 1,
    }
}
