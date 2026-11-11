//! `validate_fuzz`: differential fuzzing of pikapro-validate against Pokémon
//! Showdown's team validator (see docs/CONFORMANCE.md).
//!
//! ```sh
//! cargo run --release -p pikapro-conformance --bin validate_fuzz -- --format championsvgc --teams 2000
//! cargo run --release -p pikapro-conformance --bin validate_fuzz -- --battle-teams 1000
//! cargo run --release -p pikapro-conformance --bin validate_fuzz -- --record crates/pikapro-validate/tests/cases.txt
//! cargo run --release -p pikapro-conformance --bin validate_fuzz -- --formes crates/pikapro-validate/tests/formes.txt
//! cargo run --release -p pikapro-conformance --bin validate_fuzz -- --format championsvgc --pool teams.txt
//! ```
//!
//! Besides the problems, every run compares the team as each validator
//! leaves it (the species, item, ability and moves Showdown's battle
//! plays: `pikapro_validate::Checked::team`).
//!
//! Its seeds are 1,800,000–1,899,999, apart from the other tools'.

use std::collections::BTreeMap;
use std::process::ExitCode;

use pikapro::{BattleRng, Format, Seed};
use pikapro_conformance::cli::Args;
use pikapro_conformance::validate::{
    Pools, Validated, ValidationCase, forme_cases, random_case, run_showdown_validator, run_showdown_validator_played,
    team_json,
};
use pikapro_conformance::{oracle_available, random_team};
use pikapro_validate::cases::{Case, format_case, played_line, set_line};
use pikapro_validate::{SetInfo, Validator};

const USAGE: &str = "\
validate_fuzz - diff pikapro-validate against Showdown's team validator

USAGE:
    validate_fuzz [--format gen9vgc|championsvgc|championsbss|gen9singles|gen9ou|all] [--teams N] [--seed S]
                  [--batch N] [--show N] [--record FILE] [--record-max N]
                  [--battle-teams N] [--verbose]
    validate_fuzz --check FILE [--write]
    validate_fuzz [--format F|all] --formes FILE
    validate_fuzz --format F --pool FILE [--record FILE]

--teams N         random mutated teams per format (default 500)
--seed S          first seed (default 1800000; this tool's range is 1,800,000-1,899,999)
--record FILE     append passing cases covering new problem kinds to FILE
                  (at most --record-max per format, default 60)
--battle-teams N  also validate N teams from the battle fuzzer's generator
                  and summarise what Showdown rejects
--check FILE      run the cases of FILE (see pikapro_validate::cases) through
                  both validators; with --write, record Showdown's messages
                  and teams as validated
--formes FILE     every forme Showdown's validateForme handles, in each
                  format's dex (validate::forme_cases), through both
                  validators; writes the cases that agree to FILE
--pool FILE       the teams of FILE (Showdown export, teams split by lines
                  starting with `===`) through both validators; --record
                  appends the ones validation changes (and both agree on),
                  one per new rewrite (listed species, item and ability)
--verbose         print the problem kinds seen
";

struct Opts {
    formats: Vec<&'static str>,
    teams: u64,
    seed: u64,
    batch: usize,
    show: usize,
    record: Option<String>,
    record_max: usize,
    battle_teams: u64,
    verbose: bool,
    check: Option<String>,
    write: bool,
    formes: Option<String>,
    pool: Option<String>,
}

fn parse() -> Result<Opts, String> {
    let mut args = Args::from_env();
    let mut o = Opts {
        formats: vec!["gen9vgc", "championsvgc"],
        teams: 500,
        seed: 1_800_000,
        batch: 500,
        show: 5,
        record: None,
        record_max: 60,
        battle_teams: 0,
        verbose: false,
        check: None,
        write: false,
        formes: None,
        pool: None,
    };
    while let Some(flag) = args.next_flag() {
        let flag = flag.as_str();
        match flag {
            "--format" => {
                o.formats = match args.value(flag).as_str() {
                    "gen9vgc" => vec!["gen9vgc"],
                    "championsvgc" => vec!["championsvgc"],
                    "championsbss" => vec!["championsbss"],
                    "gen9singles" => vec!["gen9singles"],
                    "gen9ou" => vec!["gen9ou"],
                    "all" => vec!["gen9vgc", "championsvgc", "championsbss", "gen9singles", "gen9ou"],
                    f => return Err(format!("no validation format for {f}")),
                }
            }
            "--teams" => o.teams = args.num(flag),
            "--seed" => o.seed = args.num(flag),
            "--batch" => o.batch = args.num(flag) as usize,
            "--show" => o.show = args.num(flag) as usize,
            "--record" => o.record = Some(args.value(flag)),
            "--record-max" => o.record_max = args.num(flag) as usize,
            "--battle-teams" => o.battle_teams = args.num(flag),
            "--check" => o.check = Some(args.value(flag)),
            "--formes" => o.formes = Some(args.value(flag)),
            "--pool" => o.pool = Some(args.value(flag)),
            "--write" => o.write = true,
            "--verbose" => o.verbose = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            a => return Err(format!("unknown argument {a}\n\n{USAGE}")),
        }
    }
    Ok(o)
}

/// The problem kinds of our result, for coverage-driven recording.
fn kinds(problems: &[pikapro_validate::Problem]) -> Vec<String> {
    problems.iter().map(|p| pikapro_teams::problem_name(&p.kind)).collect()
}

struct Outcome {
    case: ValidationCase,
    ours: Vec<String>,
    theirs: Vec<String>,
    /// The team as each validator leaves it (`played` lines).
    ours_played: Vec<String>,
    theirs_played: Vec<String>,
    kinds: Vec<String>,
}

impl Outcome {
    fn exact(&self) -> bool {
        self.ours == self.theirs && self.ours_played == self.theirs_played
    }
}

/// Our validator's messages and team as it leaves it.
fn ours(validator: &Validator, team: &pikapro::Team, info: &[SetInfo]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let checked = validator.check(team, info);
    let dex = validator.format().dex;
    let played = checked.team.sets.iter().map(|s| played_line(dex, s)).collect();
    (validator.messages(&checked.problems), played, kinds(&checked.problems))
}

fn run_batch(
    pools: &Pools,
    formatid: &str,
    cases: Vec<ValidationCase>,
) -> Result<Vec<Outcome>, pikapro_conformance::OracleError> {
    let dex = pools.validator.format().dex;
    let json: Vec<String> = cases.iter().map(|c| team_json(dex, &c.team, &c.info)).collect();
    let theirs = run_showdown_validator_played(formatid, &json)?;
    Ok(cases
        .into_iter()
        .zip(theirs)
        .map(|(case, Validated { problems: theirs, played: theirs_played })| {
            let (ours, ours_played, kinds) = ours(&pools.validator, &case.team, &case.info);
            Outcome { case, ours, theirs, ours_played, theirs_played, kinds }
        })
        .collect())
}

fn show(pools: &Pools, o: &Outcome, preset: &str) {
    let dex = pools.validator.format().dex;
    println!("--- {preset} seed {} (mutations: {})", o.case.seed, o.case.mutations.join(", "));
    let default = SetInfo::default();
    for (i, s) in o.case.team.sets.iter().enumerate() {
        println!("    set {}", set_line(dex, s, o.case.info.get(i).unwrap_or(&default)));
    }
    diff("", &o.ours, &o.theirs);
    diff("played ", &o.ours_played, &o.theirs_played);
}

/// Prints two lists of lines side by side, marking the differences.
fn diff(what: &str, ours: &[String], theirs: &[String]) {
    for i in 0..ours.len().max(theirs.len()) {
        let (a, b) = (ours.get(i), theirs.get(i));
        let mark = if a == b { " " } else { "!" };
        if a == b && !what.is_empty() {
            continue;
        }
        println!("  {mark} {what}ours:     {}", a.map_or("-", String::as_str));
        if a != b {
            println!("  {mark} {what}showdown: {}", b.map_or("-", String::as_str));
        }
    }
}

fn fuzz(o: &Opts, preset: &'static str, recorded: &mut Vec<String>) -> Result<(u64, u64), String> {
    let format = pikapro_validate::ValidationFormat::for_preset(preset).ok_or("no validation format")?;
    let formatid = format.id;
    let pools = Pools::new(format);
    let (mut exact, mut verdicts, mut total, mut rejected, mut lines) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut played, mut changed) = (0u64, 0u64);
    let mut kinds_seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut shown = 0;
    let mut covered: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut legal_recorded = 0;
    let recorded_before = recorded.len();
    let mut seed = o.seed;
    let end = o.seed + o.teams;
    while seed < end {
        let n = (o.batch as u64).min(end - seed);
        let cases: Vec<ValidationCase> = (seed..seed + n).map(|s| random_case(&pools, s)).collect();
        seed += n;
        for out in run_batch(&pools, formatid, cases).map_err(|e| e.to_string())? {
            total += 1;
            rejected += u64::from(!out.theirs.is_empty());
            lines += out.theirs.len() as u64;
            for k in &out.kinds {
                *kinds_seen.entry(k.clone()).or_default() += 1;
            }
            if out.ours.is_empty() == out.theirs.is_empty() {
                verdicts += 1;
            }
            played += u64::from(out.ours_played == out.theirs_played);
            let given: Vec<String> =
                out.case.team.sets.iter().map(|s| played_line(pools.validator.format().dex, s)).collect();
            changed += u64::from(given != out.theirs_played);
            if out.exact() {
                exact += 1;
                if o.record.is_some() && recorded.len() - recorded_before < o.record_max {
                    let new: Vec<&String> = out.kinds.iter().filter(|k| !covered.contains(*k)).collect();
                    let legal = out.theirs.is_empty() && legal_recorded < 2;
                    if !new.is_empty() || legal {
                        legal_recorded += usize::from(legal);
                        covered.extend(out.kinds.iter().cloned());
                        recorded.push(format_case(&Case {
                            name: format!("{preset}-{}", out.case.seed),
                            format: pools.validator.format().clone(),
                            team: out.case.team.clone(),
                            info: out.case.info.clone(),
                            expect: out.theirs.clone(),
                            played: out.theirs_played.clone(),
                        }));
                    }
                }
            } else if shown < o.show {
                shown += 1;
                show(&pools, &out, preset);
            }
        }
    }
    println!(
        "{preset} ({formatid}): seeds {}-{}: exact {exact}/{total}, verdict {verdicts}/{total}, \
         played team {played}/{total} (Showdown's validator changes {changed}) \
         (Showdown rejects {rejected}, {lines} messages, {} problem kinds)",
        o.seed,
        end - 1,
        kinds_seen.len()
    );
    if o.verbose {
        for (k, n) in &kinds_seen {
            println!("  {n:>7}  {k}");
        }
    }
    Ok((exact, total))
}

/// Validates the battle fuzzer's random teams and summarises Showdown's
/// problems by message shape.
fn battle_teams(o: &Opts, preset: &'static str) -> Result<(), String> {
    let format = Format::by_id(preset).ok_or("no preset")?;
    let vformat = pikapro_validate::ValidationFormat::for_preset(preset).ok_or("no validation format")?;
    let validator = Validator::new(vformat.clone());
    let dex = vformat.dex;
    let mut teams = Vec::new();
    for seed in o.seed..o.seed + o.battle_teams {
        let mut rng = BattleRng::new(Seed::fast(seed));
        teams.push(random_team(&format, &mut rng));
    }
    let json: Vec<String> = teams.iter().map(|t| team_json(dex, t, &[])).collect();
    let theirs = run_showdown_validator(vformat.id, &json).map_err(|e| e.to_string())?;
    let mut rejected = 0;
    let mut agree = 0;
    let mut shapes: BTreeMap<String, usize> = BTreeMap::new();
    for (team, theirs) in teams.iter().zip(&theirs) {
        let ours = validator.messages(&validator.validate(team).err().unwrap_or_default());
        if &ours == theirs {
            agree += 1;
        }
        if !theirs.is_empty() {
            rejected += 1;
        }
        for p in theirs {
            *shapes.entry(shape(p)).or_default() += 1;
        }
    }
    let n = teams.len();
    println!(
        "{preset}: battle-fuzzer teams (seeds {}-): Showdown rejects {rejected}/{n}; pikapro-validate matches {agree}/{n}",
        o.seed
    );
    let mut by_count: Vec<(String, usize)> = shapes.into_iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(&a.1));
    for (s, c) in by_count.iter().take(25) {
        println!("  {c:>6}  {s}");
    }
    Ok(())
}

/// A message with its names replaced, to group them.
fn shape(p: &str) -> String {
    for (needle, label) in [
        (" can't learn ", "<X> can't learn <move>."),
        ("'s move ", "<X>'s move <move> <reason>"),
        (" is tagged ", "<X> is tagged <tag>, which is banned"),
        (" does not exist in Gen", "<X> does not exist in Gen 9."),
        (" has exactly 0 ", "<X> has exactly 0 EVs/SPs"),
        (" can't have ", "<X> can't have <ability>."),
        ("'s moves ", "<X>'s moves <moves> are incompatible."),
        (" is only obtainable from", "<X> is only obtainable from events"),
        (" must be at least level", "<X> must be at least level <n> ..."),
        (" transforms in-battle", "<X> transforms in-battle with <Y> ..."),
        (" needs to hold ", "<X> needs to hold <item> ..."),
        (" not obtainable without hacking", "<X> is not obtainable without hacking or glitches"),
        (" must have at least three perfect IVs", "<X> must have at least three perfect IVs ..."),
    ] {
        if p.contains(needle) {
            return label.to_owned();
        }
    }
    p.to_owned()
}

/// Runs the cases of a case file through both validators; with `write`,
/// replaces each case's `expect` lines with Showdown's messages.
fn check_file(path: &str, write: bool) -> Result<bool, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut cases = pikapro_validate::cases::parse_cases(&text)?;
    let mut ok = true;
    for case in &mut cases {
        let dex = case.format.dex;
        let json = team_json(dex, &case.team, &case.info);
        let Validated { problems: theirs, played: theirs_played } =
            run_showdown_validator_played(case.format.id, &[json]).map_err(|e| e.to_string())?.remove(0);
        let validator = Validator::new(case.format.clone());
        let (ours, ours_played, _) = ours(&validator, &case.team, &case.info);
        let same = ours == theirs && ours_played == theirs_played;
        let status = if same { "ok  " } else { "DIFF" };
        ok &= same;
        println!("{status} {} ({} messages)", case.name, theirs.len());
        if !same {
            diff("", &ours, &theirs);
            diff("played ", &ours_played, &theirs_played);
        } else if !write && (case.expect != theirs || (!case.played.is_empty() && case.played != theirs_played)) {
            println!("  (recorded expectations differ from Showdown's)");
            ok = false;
        }
        case.expect = theirs;
        case.played = theirs_played;
    }
    if write {
        // Keep the file's header comment.
        let header: String =
            text.lines().take_while(|l| l.starts_with('#') || l.is_empty()).map(|l| format!("{l}\n")).collect();
        let out: String = header + &cases.iter().map(format_case).collect::<String>();
        std::fs::write(path, out).map_err(|e| format!("{path}: {e}"))?;
        println!("wrote {} cases to {path}", cases.len());
    }
    Ok(ok)
}

/// `--formes`: [`forme_cases`] in each format, through both validators;
/// the agreeing cases are written to `path` (with Showdown's messages and
/// teams). Fails if any disagrees.
fn formes(o: &Opts, path: &str) -> Result<bool, String> {
    let mut text = String::from(
        "# Every forme Showdown's validateForme handles, in each validation format's\n\
         # dex, listed in a lone set (pikapro_conformance::validate::forme_cases):\n\
         # battle-only formes (Megas, Primals, Mimikyu-Busted, ...), formes an item\n\
         # or a move makes, with their required item, ability and move and without.\n\
         # `played` lines are the sets as Showdown's validator left them, which its\n\
         # battle plays. Recorded with\n\
         #   validate_fuzz --formes crates/pikapro-validate/tests/formes.txt\n\
         # (gen9vgc and championsvgc; --format all adds the custom games'\n\
         # Anything Goes).\n\
         # Never edit an expect or played line by hand.\n\n",
    );
    let mut ok = true;
    for preset in &o.formats {
        let format = pikapro_validate::ValidationFormat::for_preset(preset).ok_or("no validation format")?;
        let pools = Pools::new(format.clone());
        let cases = forme_cases(&pools);
        let dex = format.dex;
        let json: Vec<String> = cases.iter().map(|(_, t)| team_json(dex, t, &[])).collect();
        let theirs = run_showdown_validator_played(format.id, &json).map_err(|e| e.to_string())?;
        let (mut exact, mut changed, mut rejected) = (0, 0, 0);
        for ((name, team), Validated { problems, played }) in cases.iter().zip(theirs) {
            let (ours, ours_played, _) = ours(&pools.validator, team, &[]);
            let given: Vec<String> = team.sets.iter().map(|s| played_line(dex, s)).collect();
            changed += usize::from(given != played);
            // Beyond the team size.
            rejected += usize::from(problems.len() > 1);
            if ours == problems && ours_played == played {
                exact += 1;
                text += &format_case(&Case {
                    name: name.clone(),
                    format: format.clone(),
                    team: team.clone(),
                    info: Vec::new(),
                    expect: problems,
                    played,
                });
            } else {
                ok = false;
                println!("--- {name}");
                for s in &team.sets {
                    println!("    set {}", set_line(dex, s, &SetInfo::default()));
                }
                diff("", &ours, &problems);
                diff("played ", &ours_played, &played);
            }
        }
        println!(
            "{preset} ({}): formes: exact {exact}/{} (Showdown's validator changes {changed} sets, rejects {rejected})",
            format.id,
            cases.len()
        );
    }
    std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
    Ok(ok)
}

/// `--pool`: the teams of a file through both validators; with `record`,
/// appends the agreeing teams validation changes to it.
fn pool(o: &Opts, path: &str) -> Result<bool, String> {
    let [preset] = o.formats[..] else { return Err("--pool needs one --format".into()) };
    let format = Format::by_id(preset).ok_or("no preset")?;
    let vformat = pikapro_validate::ValidationFormat::for_preset(preset).ok_or("no validation format")?;
    let validator = Validator::new(vformat.clone());
    let dex = vformat.dex;
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut teams: Vec<(String, pikapro::Team, Vec<SetInfo>)> = Vec::new();
    let (mut label, mut body) = (String::from("#0"), String::new());
    let flush = |label: &str, body: &mut String, teams: &mut Vec<_>| {
        if body.trim().is_empty() {
            return;
        }
        match pikapro_protocol::import_sheet(body, &format) {
            Ok(sheet) => {
                let info = sheet.team.sets.iter().zip(&sheet.extras).map(|(s, e)| SetInfo::new(s, &e.name, e.shiny));
                let info = info.collect();
                teams.push((label.to_owned(), sheet.team, info));
            }
            Err(e) => println!("{label}: not imported: {e}"),
        }
        body.clear();
    };
    for line in text.lines() {
        if line.starts_with("===") {
            flush(&label, &mut body, &mut teams);
            label = line.trim_matches(|c| c == '=' || c == ' ').to_owned();
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    flush(&label, &mut body, &mut teams);
    let (mut exact, mut messages, mut played_ok, mut changed_teams, mut changed_sets, mut sets) = (0, 0, 0, 0, 0, 0);
    let mut recorded = String::new();
    let mut covered = std::collections::BTreeSet::new();
    let mut shown = 0;
    for chunk in teams.chunks(o.batch.max(1)) {
        let json: Vec<String> = chunk.iter().map(|(_, t, i)| team_json(dex, t, i)).collect();
        let theirs = run_showdown_validator_played(vformat.id, &json).map_err(|e| e.to_string())?;
        for ((label, team, info), Validated { problems, played }) in chunk.iter().zip(theirs) {
            let (ours, ours_played, _) = ours(&validator, team, info);
            let given: Vec<String> = team.sets.iter().map(|s| played_line(dex, s)).collect();
            sets += team.sets.len();
            let rewrites: Vec<(String, String)> = given
                .iter()
                .zip(&played)
                .filter(|(a, b)| a != b)
                .map(|(a, b)| (a.split('|').take(3).collect::<Vec<_>>().join("|"), b.clone()))
                .collect();
            let changed = rewrites.len();
            changed_sets += changed;
            changed_teams += usize::from(changed > 0);
            messages += usize::from(ours == problems);
            played_ok += usize::from(ours_played == played);
            if ours == problems && ours_played == played {
                exact += 1;
                // One team per new rewrite (listed species, item and
                // ability).
                let new = rewrites.iter().any(|(given, _)| !covered.contains(given));
                covered.extend(rewrites.into_iter().map(|(given, _)| given));
                if new {
                    recorded += &format_case(&Case {
                        name: format!("{preset}-pool-{}", label.split(' ').find(|w| !w.starts_with('[')).unwrap_or("")),
                        format: vformat.clone(),
                        team: team.clone(),
                        info: info.clone(),
                        expect: problems,
                        played,
                    });
                }
            } else if shown < o.show {
                shown += 1;
                println!("--- {label}");
                diff("", &ours, &problems);
                diff("played ", &ours_played, &played);
            }
        }
    }
    let n = teams.len();
    println!(
        "{preset} ({}): {path}: {n} teams, {sets} sets: exact {exact}/{n} (messages {messages}/{n}, played team \
         {played_ok}/{n}); Showdown's validator changes {changed_sets} sets in {changed_teams} teams",
        vformat.id
    );
    if let Some(out) = &o.record {
        let mut text = std::fs::read_to_string(out).unwrap_or_default();
        text += &recorded;
        std::fs::write(out, text).map_err(|e| format!("{out}: {e}"))?;
        println!(
            "recorded {} teams in {out}",
            recorded.matches("\ncase ").count() + usize::from(recorded.starts_with("case "))
        );
    }
    Ok(exact == n)
}

fn main() -> ExitCode {
    let o = match parse() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if !oracle_available() {
        eprintln!("error: node or tools/showdown-oracle/node_modules is missing (run `npm ci` there)");
        return ExitCode::FAILURE;
    }
    let special = [(&o.formes, formes as fn(&Opts, &str) -> Result<bool, String>), (&o.pool, pool)];
    for (path, run) in special {
        if let Some(path) = path {
            return match run(&o, path) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            };
        }
    }
    if let Some(path) = &o.check {
        return match check_file(path, o.write) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let mut recorded = Vec::new();
    let mut ok = true;
    for preset in &o.formats {
        match fuzz(&o, preset, &mut recorded) {
            Ok((exact, total)) => ok &= exact == total,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
        if o.battle_teams > 0
            && let Err(e) = battle_teams(&o, preset)
        {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    }
    if let Some(path) = &o.record {
        let mut text = std::fs::read_to_string(path).unwrap_or_default();
        for c in &recorded {
            text += c;
        }
        if let Err(e) = std::fs::write(path, text) {
            eprintln!("error: {path}: {e}");
            return ExitCode::FAILURE;
        }
        println!("recorded {} cases in {path}", recorded.len());
    }
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
