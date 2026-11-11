//! pikapro-teams' port of Showdown's random-battle team generator
//! (`pikapro_teams::random_battle`) diffed against Showdown's own
//! (`tools/showdown-oracle/random-teams.mjs`): the same seeds through both,
//! every set compared field by field, and for a team that differs, both
//! generators' draws side by side up to the first that differs.
//!
//! ```sh
//! cargo run --release -p pikapro-conformance --bin random_teams -- --format gen9randombattle --seeds 100000
//! ```

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::{Command, Stdio};

use pikapro::{BattleRng, Format, Seed};
use pikapro_protocol::unpack_team;
use pikapro_teams::random_battle::{self, Draw, RandomBattle};

use crate::{oracle_dir, quote};

/// Which of Showdown's PRNGs the seeds are for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedKind {
    /// `gen5,<16 hex digits>`: the seed's number as the Gen 5 LCG's state.
    Gen5,
    /// `sodium,<32 hex digits>`, as `PRNG.generateSeed()` makes them: 16
    /// bytes drawn from the seed's number, the rest of the key zero.
    Sodium,
}

/// Seed `n` of a kind, and Showdown's text for it.
pub fn seed(kind: SeedKind, n: u64) -> (Seed, String) {
    match kind {
        SeedKind::Gen5 => {
            let words = [(n >> 48) as u16, (n >> 32) as u16, (n >> 16) as u16, n as u16];
            (Seed::ShowdownGen5(words), format!("gen5,{n:016x}"))
        }
        SeedKind::Sodium => {
            let mut rng = BattleRng::new(Seed::fast(n ^ 0x50D1_0153_ED5E_ED00));
            let mut key = [0u8; 32];
            for b in &mut key[..16] {
                *b = rng.random(256) as u8;
            }
            let hex: String = key[..16].iter().map(|b| format!("{b:02x}")).collect();
            (Seed::Sodium(key), format!("sodium,{hex}"))
        }
    }
}

/// The seed of a Showdown seed text this module writes (`gen5,` and 16 hex
/// digits, or `sodium,` and hex digits, read in pairs into the key).
pub fn parse_seed(text: &str) -> Option<Seed> {
    let hex = |h: &str| h.bytes().all(|b| b.is_ascii_hexdigit()).then_some(());
    if let Some(h) = text.strip_prefix("gen5,") {
        hex(h)?;
        return (h.len() == 16).then(|| random_battle::gen5_seed(u64::from_str_radix(h, 16).expect("hex")));
    }
    let h = text.strip_prefix("sodium,")?;
    hex(h)?;
    if h.len() % 2 != 0 || h.len() > 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate().take(h.len() / 2) {
        *b = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).expect("hex");
    }
    Some(Seed::Sodium(key))
}

/// What Showdown's generator gave for a seed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleTeam {
    pub seed: String,
    /// The team ([`random_battle::team_text`]'s form) and its packed text, or the
    /// generator's error.
    pub team: Result<(String, String), String>,
    /// `(bound, result, site)` per draw, when traced.
    pub trace: Vec<(u32, u32, String)>,
}

/// Runs `random-teams.mjs` on `seeds`.
pub fn run_oracle(format: RandomBattle, seeds: &[String], trace: bool) -> Result<Vec<OracleTeam>, String> {
    let list: Vec<String> = seeds.iter().map(|s| quote(s)).collect();
    let json =
        format!("{{\"formatid\":{},\"seeds\":[{}],\"trace\":{trace}}}", quote(format.format_id()), list.join(","));
    let mut child = Command::new("node")
        .arg(oracle_dir().join("random-teams.mjs"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("node: {e}"))?;
    child.stdin.take().expect("piped").write_all(json.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("random-teams.mjs failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut teams: Vec<OracleTeam> = Vec::with_capacity(seeds.len());
    for line in text.lines() {
        let mut parts = line.split('\t');
        let (kind, seed) = (parts.next().unwrap_or(""), parts.next().unwrap_or("").to_string());
        match kind {
            "team" => {
                let sets = parts.next().unwrap_or("").to_string();
                let packed = parts.next().unwrap_or("").to_string();
                teams.push(OracleTeam { seed, team: Ok((sets, packed)), trace: Vec::new() });
            }
            "error" => {
                let message = parts.next().unwrap_or("").to_string();
                teams.push(OracleTeam { seed, team: Err(message), trace: Vec::new() });
            }
            "trace" => {
                let draws = parts.next().unwrap_or("");
                let last = teams.last_mut().filter(|t| t.seed == seed).ok_or("a trace without its team")?;
                for d in draws.split(' ').filter(|d| !d.is_empty()) {
                    let mut f = d.splitn(3, ':');
                    let num = |x: Option<&str>| x.and_then(|v| v.parse::<u32>().ok()).ok_or(format!("bad draw {d}"));
                    let bound = num(f.next())?;
                    let result = num(f.next())?;
                    last.trace.push((bound, result, f.next().unwrap_or("").to_string()));
                }
            }
            other => return Err(format!("random-teams.mjs: unexpected line kind {other:?}")),
        }
    }
    if teams.len() != seeds.len() || teams.iter().zip(seeds).any(|(t, s)| t.seed != *s) {
        return Err(format!("random-teams.mjs gave {} teams for {} seeds", teams.len(), seeds.len()));
    }
    Ok(teams)
}

/// Whether the oracle's team for a seed is pikapro's: `None` if so, else
/// what differs. A team that agrees must also be what pikapro-protocol
/// reads from Showdown's packed team, with its shiny sets.
pub fn compare(format: RandomBattle, seed: Seed, oracle: &OracleTeam, game: &Format) -> Option<String> {
    let mut rng = BattleRng::new(seed);
    let generated = random_battle::generate(format, &mut rng).map_err(|e| e.message);
    let mine = generated.as_ref().map(|t| random_battle::team_text(t)).map_err(Clone::clone);
    let theirs = oracle.team.as_ref().map(|(sets, _)| sets.clone()).map_err(Clone::clone);
    if mine == theirs {
        let (Ok(sets), Ok((_, packed))) = (&generated, &oracle.team) else { return None };
        return match unpack_team(packed, game) {
            Ok(sheet) => {
                let shiny: Vec<bool> = sheet.extras.iter().map(|e| e.shiny).collect();
                let our_shiny: Vec<bool> = sets.iter().map(|s| s.shiny).collect();
                let team = random_battle::to_team(sets);
                (sheet.team != team || shiny != our_shiny)
                    .then(|| format!("Showdown's packed team reads as\n    {:?}\nnot\n    {:?}", sheet.team, team))
            }
            Err(e) => Some(format!("Showdown's packed team doesn't unpack: {e}")),
        };
    }
    let show = |r: &Result<String, String>| match r {
        Ok(t) => t.split(']').collect::<Vec<_>>().join("\n    "),
        Err(e) => format!("error: {e}"),
    };
    Some(format!("pikapro:\n    {}\nShowdown:\n    {}", show(&mine), show(&theirs)))
}

/// Both generators' draws for one seed, up to a few after the first that
/// differs.
pub fn trace_report(format: RandomBattle, seed: Seed, text: &str) -> Result<String, String> {
    let oracle = run_oracle(format, &[text.to_string()], true)?.remove(0);
    let mut rng = BattleRng::new(seed);
    let mut draws: Vec<Draw> = Vec::new();
    let _ = random_battle::generate_traced(format, &mut rng, &mut draws);
    let mut out = String::new();
    let first = (0..draws.len().max(oracle.trace.len())).find(|&i| match (draws.get(i), oracle.trace.get(i)) {
        (Some(d), Some((b, r, _))) => d.bound != *b || d.result != *r,
        _ => true,
    });
    let Some(first) = first else {
        let _ = writeln!(out, "the {} draws agree", draws.len());
        return Ok(out);
    };
    let _ = writeln!(
        out,
        "first differing draw: #{first} (pikapro {} draws, Showdown {})",
        draws.len(),
        oracle.trace.len()
    );
    for i in first.saturating_sub(12)..(first + 4).min(draws.len().max(oracle.trace.len())) {
        let ours =
            draws.get(i).map_or("-".to_string(), |d| format!("random({}) = {} at {}", d.bound, d.result, d.site));
        let theirs = oracle.trace.get(i).map_or("-".to_string(), |(b, r, s)| format!("random({b}) = {r} at {s}"));
        let mark = if i == first { ">" } else { " " };
        let _ = writeln!(out, "{mark} #{i:<4} pikapro {ours:<50} Showdown {theirs}");
    }
    Ok(out)
}

/// FNV-1a (64 bits): the digest of many teams in a team file.
pub fn digest(lines: impl IntoIterator<Item = String>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for line in lines {
        for b in line.bytes().chain(core::iter::once(b'\n')) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// The teams `--record` writes per format: these seeds in full, and a
/// digest of the `gen5` seeds `0..DIGEST_SEEDS`.
pub const RECORDED_GEN5: u64 = 16;
pub const RECORDED_SODIUM: u64 = 4;
pub const DIGEST_SEEDS: u64 = 2000;

/// `random_teams --record`: Showdown's teams for [`RECORDED_GEN5`] and
/// [`RECORDED_SODIUM`] seeds of each format, and the digest of
/// [`DIGEST_SEEDS`] more, in the file `pikapro-teams`' `tests/random_battle.rs`
/// reads. Fails (writing nothing) where pikapro's generator differs.
pub fn record_file() -> Result<String, String> {
    let mut out = String::from(
        "# Showdown's Gen 9 random-battle teams (tools/showdown-oracle/random-teams.mjs), recorded by\n\
         # `cargo run --release -p pikapro-conformance --bin random_teams -- --record FILE`. Do not edit by hand.\n\
         # team <format> <seed> <sets>: a team in full (random_battle::team_text's form).\n\
         # digest <format> <n> <hex>: FNV-1a of the teams of gen5 seeds 0..n, one per line.\n",
    );
    for format in [RandomBattle::Singles, RandomBattle::Doubles] {
        let game = Format::by_id(format.format_id()).expect("a preset");
        let mut seeds: Vec<(Seed, String)> = (0..RECORDED_GEN5).map(|n| seed(SeedKind::Gen5, n)).collect();
        seeds.extend((0..RECORDED_SODIUM).map(|n| seed(SeedKind::Sodium, n)));
        let texts: Vec<String> = seeds.iter().map(|(_, t)| t.clone()).collect();
        for ((s, text), o) in seeds.iter().zip(run_oracle(format, &texts, false)?) {
            if let Some(d) = compare(format, *s, &o, &game) {
                return Err(format!("{text}: {d}"));
            }
            let (sets, _) = o.team.map_err(|e| format!("{text}: Showdown's generator throws: {e}"))?;
            out.push_str(&format!("team {} {text} {sets}\n", format.format_id()));
        }
        let all: Vec<(Seed, String)> = (0..DIGEST_SEEDS).map(|n| seed(SeedKind::Gen5, n)).collect();
        let texts: Vec<String> = all.iter().map(|(_, t)| t.clone()).collect();
        let mut lines = Vec::new();
        for ((s, text), o) in all.iter().zip(run_oracle(format, &texts, false)?) {
            if let Some(d) = compare(format, *s, &o, &game) {
                return Err(format!("{text}: {d}"));
            }
            lines.push(o.team.map(|(sets, _)| sets).unwrap_or_else(|e| format!("error: {e}")));
        }
        out.push_str(&format!("digest {} {DIGEST_SEEDS} {:016x}\n", format.format_id(), digest(lines)));
    }
    Ok(out)
}
