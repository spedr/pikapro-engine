//! Golden fixtures: a scenario plus Showdown's normalised log, stored as
//! text so conformance is checked by `cargo test` without Node.
//!
//! ```text
//! # comment
//! format gen9singles
//! seed 0001 0002 0003 0004
//! team p1
//! Garchomp
//! Ability: No Ability
//! - Earthquake
//! end
//! team p2
//! ...
//! end
//! choice p1 move 1
//! choice p2 switch 3
//! expect |start
//! expect |rng|100|89
//! ```
//!
//! Teams carry their nicknames (`Nick (Species)`). `expect`
//! lines are Showdown's raw log (`|split|` blocks included; `expect` alone
//! for an empty line) after [`normalize`](crate::normalize) with RNG draws
//! included. `request <side> <JSON>` lines, after them, are the
//! requests Showdown sent (see [`crate::requests`]); older fixtures have
//! none. Record new fixtures with
//! `cargo conformance --record crates/pikapro-conformance/fixtures`, which
//! lists them in the directory's [`MANIFEST`] and never overwrites one.
//!
//! Teams that list battle-only formes (`cargo conformance --listed`) are
//! written as listed, after a `validate <Showdown format id>` line: both
//! simulators validate them before the battle ([`Scenario::listed`]), and
//! [`parse`] plays what pikapro-validate makes of them.
//!
//! Every fixture says `genders drawn`: a set without a gender gets the
//! species' or a random one, as in Showdown.
//!
//! `team-size MIN MAX` after `format` explicitly relaxes the engine's
//! input-size constraint for simulator-only short-team probes. Showdown's
//! simulator accepts supplied teams without running its format validator.

use std::fmt::Write as _;

use pikapro::{Format, SideId, Team};
use pikapro_protocol::{export_sheet, import_sheet};

use crate::Scenario;

pub fn write(s: &Scenario, expected: &[String]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Recorded from Pokémon Showdown by pikapro-conformance. Do not edit by hand.");
    let _ = writeln!(out, "format {}", s.format.id);
    // Supplied teams go straight to Showdown's simulator, which does not
    // enforce the validator's Min Team Size rule. Explicitly opt in when
    // probing short teams without changing a preset's public constraints.
    if Format::by_id(s.format.id).is_some_and(|preset| preset.team_size != s.format.team_size) {
        let _ = writeln!(out, "team-size {} {}", s.format.team_size.0, s.format.team_size.1);
    }
    let _ = writeln!(out, "seed {}", s.seed.map(|w| format!("{w:04x}")).join(" "));
    let _ = writeln!(out, "{GENDERS_DRAWN}");
    if let Some(v) = s.validation_format() {
        let _ = writeln!(out, "validate {v}");
    }
    // Generated teams: the seeds Showdown's generator drew them from.
    if let Some(seeds) = &s.team_seeds {
        for (side, text) in SideId::BOTH.into_iter().zip(seeds) {
            let _ = writeln!(out, "team-seed {} {text}", pikapro_protocol::side_id(side));
        }
    }
    for side in SideId::BOTH {
        let _ = writeln!(out, "team {}", pikapro_protocol::side_id(side));
        // With the nicknames (`Nick (Species)`).
        out.push_str(&export_sheet(&s.listed_sheet(side), &s.format));
        out.push_str("end\n");
    }
    for (side, choice) in &s.choices {
        let _ = writeln!(out, "choice {} {choice}", pikapro_protocol::side_id(*side));
    }
    for line in expected {
        if line.is_empty() {
            let _ = writeln!(out, "expect");
        } else {
            let _ = writeln!(out, "expect {line}");
        }
    }
    out
}

const GENDERS_DRAWN: &str = "genders drawn";

pub fn parse(text: &str) -> Result<(Scenario, Vec<String>), String> {
    let mut format = None;
    let mut seed = None;
    let mut teams = [None, None];
    let mut choices = Vec::new();
    let mut nicknames = [Vec::new(), Vec::new()];
    let mut shiny = [Vec::new(), Vec::new()];
    let mut expected = Vec::new();
    let mut validate = None;
    let mut genders_drawn = false;
    let mut team_seeds: [Option<String>; 2] = [None, None];
    let mut lines = text.lines().enumerate();
    while let Some((n, line)) = lines.next() {
        let err = |m: &str| format!("line {}: {m}", n + 1);
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "format" => format = Some(Format::by_id(rest).ok_or_else(|| err("unknown format"))?),
            "team-size" => {
                let f = format.as_mut().ok_or_else(|| err("team-size before format"))?;
                let sizes = rest
                    .split_whitespace()
                    .map(str::parse::<u8>)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| err("team-size needs min and max in 1..=6"))?;
                if sizes.len() != 2 || sizes[0] == 0 || sizes[0] > sizes[1] || sizes[1] > 6 {
                    return Err(err("team-size needs min and max in 1..=6"));
                }
                f.team_size = (sizes[0], sizes[1]);
            }
            "seed" => {
                let words: Vec<u16> = rest.split(' ').filter_map(|w| u16::from_str_radix(w, 16).ok()).collect();
                seed = Some(<[u16; 4]>::try_from(words).map_err(|_| err("seed needs 4 hex words"))?);
            }
            "team" => {
                let format = format.as_ref().ok_or_else(|| err("team before format"))?;
                let mut body = String::new();
                for (_, l) in lines.by_ref() {
                    if l == "end" {
                        break;
                    }
                    body.push_str(l);
                    body.push('\n');
                }
                let sheet = import_sheet(&body, format).map_err(|e| err(&e.to_string()))?;
                let side = side(rest).ok_or_else(|| err("bad side"))?.index();
                nicknames[side] = sheet.extras.iter().map(|e| e.name.clone()).collect();
                shiny[side] = sheet.extras.iter().map(|e| e.shiny).collect();
                teams[side] = Some(sheet.team);
            }
            "choice" => {
                let (who, choice) = rest.split_once(' ').ok_or_else(|| err("bad choice"))?;
                choices.push((side(who).ok_or_else(|| err("bad side"))?, choice.to_string()));
            }
            "expect" => expected.push(rest.to_string()),
            "validate" => {
                validate = Some(
                    pikapro_validate::ValidationFormat::by_id(rest).ok_or_else(|| err("unknown validation format"))?,
                )
            }
            "genders" if line == GENDERS_DRAWN => genders_drawn = true,
            "team-seed" => {
                let (who, text) = rest.split_once(' ').ok_or_else(|| err("bad team-seed"))?;
                team_seeds[side(who).ok_or_else(|| err("bad side"))?.index()] = Some(text.to_string());
            }
            // Read by `parse_requests`.
            "request" => {}
            _ => return Err(err("unknown key")),
        }
    }
    let [Some(t1), Some(t2)] = teams else { return Err("missing team".into()) };
    let format = format.ok_or("missing format")?;
    if !genders_drawn {
        return Err(format!("missing `{GENDERS_DRAWN}`"));
    }
    // Listed teams: played as pikapro-validate leaves them.
    let (teams, listed) = match validate {
        Some(v) => {
            if v.id != pikapro_validate::ValidationFormat::for_preset(format.id).map_or("", |p| p.id) {
                return Err(format!("validate {}: not {}'s validation format", v.id, format.id));
            }
            let validator = pikapro_validate::Validator::new(v);
            ([validator.played(&t1), validator.played(&t2)], Some([t1, t2]))
        }
        None => ([t1, t2], None),
    };
    let seed = seed.ok_or("missing seed")?;
    let team_seeds = match team_seeds {
        [None, None] => None,
        [Some(a), Some(b)] => Some([a, b]),
        _ => return Err("a `team-seed` for one side only".into()),
    };
    if let Some(seeds) = &team_seeds {
        check_generated(&format, seeds, &teams, &shiny)?;
    }
    let scenario = Scenario { format, seed, teams, choices, nicknames, shiny, listed, team_seeds };
    Ok((scenario, expected))
}

/// A fixture's generated teams must be what pikapro-teams' generator
/// draws for their seeds (the export text, shiny sets included).
fn check_generated(
    format: &Format,
    seeds: &[String; 2],
    teams: &[Team; 2],
    shiny: &[Vec<bool>; 2],
) -> Result<(), String> {
    let kind = pikapro_teams::random_battle::RandomBattle::for_format(format.id)
        .ok_or_else(|| format!("team-seed in {}, not a random battle", format.id))?;
    for i in 0..2 {
        let seed = crate::random_teams::parse_seed(&seeds[i]).ok_or_else(|| format!("bad team-seed {}", seeds[i]))?;
        let mut rng = pikapro::BattleRng::new(seed);
        let sets = pikapro_teams::random_battle::generate(kind, &mut rng).map_err(|e| e.message)?;
        let sheet = |team: Team, shiny: Vec<bool>| {
            let mut sheet = pikapro_protocol::TeamSheet::new(team);
            for (e, s) in sheet.extras.iter_mut().zip(shiny) {
                e.shiny = s;
            }
            export_sheet(&sheet, format)
        };
        let generated = sheet(pikapro_teams::random_battle::to_team(&sets), sets.iter().map(|s| s.shiny).collect());
        let recorded = sheet(teams[i].clone(), shiny[i].clone());
        if generated != recorded {
            return Err(format!(
                "p{}'s team isn't the generator's for {}:\n{generated}\nnot the recorded\n{recorded}",
                i + 1,
                seeds[i]
            ));
        }
    }
    Ok(())
}

fn side(s: &str) -> Option<SideId> {
    match s {
        "p1" => Some(SideId::P1),
        "p2" => Some(SideId::P2),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Requests.

/// The `request` lines for Showdown's requests.
pub fn write_requests(requests: &[crate::SentRequest]) -> String {
    let mut out = String::new();
    for r in requests {
        let _ = writeln!(out, "request {} {}", r.side, r.json);
    }
    out
}

/// The requests a fixture recorded (`after` is not recorded).
pub fn parse_requests(text: &str) -> Vec<crate::SentRequest> {
    text.lines()
        .filter_map(|line| line.strip_prefix("request "))
        .filter_map(|rest| rest.split_once(' '))
        .map(|(side, json)| crate::SentRequest { side: side.into(), json: json.into(), after: String::new() })
        .collect()
}

/// The file in a golden fixture directory that lists its fixtures, one
/// file name per line (in the order they were recorded). [`check_manifest`]
/// requires it to match the directory exactly, so a deleted fixture or an
/// unlisted new one fails `cargo test`; the `--record` flags add what they
/// write with [`record`]. Fixtures are never deleted: when a re-pin
/// replaces one, the manifest's diff shows it.
pub const MANIFEST: &str = "MANIFEST";

/// Records a new golden fixture: writes `text` to `dir/name` and lists it
/// in the [`MANIFEST`]. `Ok(false)`, writing nothing, if `dir/name` exists:
/// `--record` never overwrites a golden (`--rerecord` rewrites them).
///
/// Safe for several processes recording into one directory: the file
/// appears whole or not at all (a hard link from a temporary file named
/// by the process, which fails if the name is taken), and the manifest
/// only grows by one line written in a single append.
pub fn record(dir: &std::path::Path, name: &str, text: &str) -> std::io::Result<bool> {
    use std::io::Write as _;
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    let linked = std::fs::hard_link(&tmp, &path);
    std::fs::remove_file(&tmp)?;
    match linked {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(e),
    }
    let mut manifest = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(MANIFEST))?;
    manifest.write_all(format!("{name}\n").as_bytes())?;
    Ok(true)
}

/// The fixtures in `dir` (files ending in `.ext`), sorted, after checking
/// them against its [`MANIFEST`]: the error names the listed fixtures that
/// are missing and the ones on disk that aren't listed.
pub fn check_manifest(dir: &std::path::Path, ext: &str) -> Result<Vec<std::path::PathBuf>, String> {
    use std::collections::BTreeSet;
    let manifest = dir.join(MANIFEST);
    let text = std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let listed: BTreeSet<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .map(|e| e.expect("a directory entry").path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == ext))
        .collect();
    paths.sort();
    let names: Vec<String> = paths.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    let on_disk: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    let few = |set: Vec<&&str>| {
        let shown: Vec<&str> = set.iter().take(10).map(|s| **s).collect();
        match set.len() - shown.len() {
            0 => shown.join(", "),
            more => format!("{} and {more} more", shown.join(", ")),
        }
    };
    let missing: Vec<_> = listed.difference(&on_disk).collect();
    let unlisted: Vec<_> = on_disk.difference(&listed).collect();
    let mut problems = Vec::new();
    if !missing.is_empty() {
        problems.push(format!(
            "{} listed fixtures are missing (fixtures are never deleted): {}",
            missing.len(),
            few(missing)
        ));
    }
    if !unlisted.is_empty() {
        problems.push(format!(
            "{} fixtures aren't listed (`--record` lists what it writes; add others by hand): {}",
            unlisted.len(),
            few(unlisted)
        ));
    }
    if problems.is_empty() { Ok(paths) } else { Err(format!("{}: {}", manifest.display(), problems.join("; "))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `record` writes a new fixture and lists it, refuses to overwrite
    /// one, and leaves no temporary file; `check_manifest` agrees.
    #[test]
    fn record_never_overwrites_and_lists_what_it_writes() {
        let dir = std::env::temp_dir().join(format!("pikapro-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST), "").unwrap();
        assert!(record(&dir, "b.txt", "one").unwrap());
        assert!(record(&dir, "a.txt", "two").unwrap());
        assert!(!record(&dir, "b.txt", "three").unwrap(), "an existing fixture");
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(dir.join(MANIFEST)).unwrap(), "b.txt\na.txt\n");
        let paths = check_manifest(&dir, "txt").unwrap();
        assert_eq!(paths, vec![dir.join("a.txt"), dir.join("b.txt")]);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3, "no temporary file left");
        std::fs::write(dir.join("c.txt"), "unlisted").unwrap();
        assert!(check_manifest(&dir, "txt").unwrap_err().contains("c.txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
