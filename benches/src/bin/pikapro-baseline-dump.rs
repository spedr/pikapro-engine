//! `pikapro-baseline-dump`: the Rust half of the baseline parity check
//! (`tools/baseline-parity/`).
//!
//! Plays battles and, at every decision, writes what our
//! `MaxBasePowerPolicy` and `SimpleHeuristicsPolicy` would choose from the
//! side's view, then submits a *driver* policy's choice so the battles
//! reach varied states. `tools/baseline-parity/parity.py` replays the
//! same battles on `pikapro.Battle`, asks poke-env's players at the same
//! decisions, and compares.
//!
//! ```text
//! cargo run --release -p pikapro-bench --bin pikapro-baseline-dump -- --format championsvgc
//!     [--battles 100] [--seed 4710000] [--driver mix|random|maxbp|heuristic] [--teams sets|uniform]
//!     [-o decisions.jsonl]
//! ```
//!
//! One JSON object per line and decision: `seed`, `side` (0 or 1), `n`
//! (the battle's decision count so far), `turn`, `kind` (`move`,
//! `switch` or `preview`), `maxbp` and `heuristic` (our choices, as
//! Showdown choice strings), `driver` (the choice submitted). A battle's
//! first line also has `format` and `teams` (both teams in Showdown's
//! export format; battle `k` has seed `seed + k`, `pikapro.Battle(format,
//! teams, seed=seed)`). Views show foe HP as the format's channel does
//! (exact in the custom games and gen9vgc, percentages in championsvgc).
//!
//! The driver `mix` plays side 1 as the heuristics and side 2 as max base
//! power, each with a random choice a quarter of the time.

use std::io::Write;

use pikapro::{Battle, BattleOptions, BattleRng, Choice, Format, Request, Seed, SideId};
use pikapro_protocol::{BattleInfo, export_sheet, format_choice_for, import_sheet, json_string};
use pikapro_rl::baselines::{MaxBasePowerPolicy, SimpleHeuristicsPolicy};
use pikapro_rl::policy::{Policy, RandomPolicy};
use pikapro_view::{HpReport, OwnOverlay, PlayerView, PublicTracker, ViewConfig};

struct Args {
    format: String,
    battles: u64,
    seed: u64,
    driver: String,
    teams: String,
    out: String,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        format: "championsvgc".into(),
        battles: 100,
        seed: 4_710_000,
        driver: "mix".into(),
        teams: "sets".into(),
        out: "-".into(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--format" => a.format = value()?,
            "--battles" => a.battles = value()?.parse().map_err(|_| "bad --battles")?,
            "--seed" => a.seed = value()?.parse().map_err(|_| "bad --seed")?,
            "--driver" => a.driver = value()?,
            "--teams" => a.teams = value()?,
            "-o" | "--out" => a.out = value()?,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if !["mix", "random", "maxbp", "heuristic"].contains(&a.driver.as_str()) {
        return Err(format!("unknown driver {:?}", a.driver));
    }
    Ok(a)
}

/// A seed for one of a decision's RNGs.
fn mix(seed: u64, side: usize, n: u64, what: u64) -> Seed {
    let x =
        seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ n.wrapping_mul(0xBF58_476D_1CE4_E5B9) ^ (what << 1 | side as u64);
    Seed::fast(x)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("pikapro-baseline-dump: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let a = args()?;
    let format = Format::by_id(&a.format).ok_or(format!("unknown format {:?}", a.format))?;
    let mode = pikapro_teams::Mode::from_name(&a.teams).ok_or(format!("unknown team mode {:?}", a.teams))?;
    let generator = pikapro_rl::teams::generator(&format, mode, pikapro_teams::Options::default())?;
    let hp = if BattleInfo::for_format(&format).report_exact_hp { HpReport::Exact } else { HpReport::Percent };
    let mut out: Box<dyn Write> = if a.out == "-" {
        Box::new(std::io::BufWriter::new(std::io::stdout().lock()))
    } else {
        Box::new(std::io::BufWriter::new(std::fs::File::create(&a.out).map_err(|e| format!("{}: {e}", a.out))?))
    };
    let (maxbp, heuristic) = (MaxBasePowerPolicy, SimpleHeuristicsPolicy::new());
    let mut decisions = 0u64;
    for k in 0..a.battles {
        let seed = a.seed + k;
        let sheets = pikapro_rl::teams::pair(&generator, seed).map_err(|e| e.to_string())?;
        // What Python's pikapro.Battle is given: the exported texts.
        let texts = sheets.each_ref().map(|s| export_sheet(s, &format));
        let reread = [0, 1].map(|i| import_sheet(&texts[i], &format).map(|s| s.team));
        let [Ok(t1), Ok(t2)] = reread else { return Err(format!("seed {seed}: a team doesn't read back")) };
        let mut battle = Battle::new(format.clone(), [t1, t2], BattleOptions::new(Seed::fast(seed)))
            .map_err(|e| format!("seed {seed}: {e}"))?;
        let mut tracker = PublicTracker::new(ViewConfig::new(&format).with_hp(hp));
        for e in battle.drain_events() {
            tracker.apply(&e);
        }
        let mut drivers = [0, 1].map(|s| BattleRng::new(mix(seed, s, 0, 7)));
        let mut n = 0u64;
        while !battle.is_over() {
            for side in SideId::BOTH {
                if battle.is_over() {
                    break;
                }
                let legal = *battle.legal_actions(side);
                if legal.is_waiting() {
                    continue;
                }
                let s = side.index();
                let own = OwnOverlay::from_request(&battle.request_data(side));
                let view = PlayerView::new(side, &tracker, &own);
                let ours = |p: &dyn Policy, what| p.choose(&view, &legal, &mut BattleRng::new(mix(seed, s, n, what)));
                let (m, h) = (ours(&maxbp, 1), ours(&heuristic, 2));
                let driver = match a.driver.as_str() {
                    "random" => RandomPolicy.choose(&view, &legal, &mut drivers[s]),
                    "maxbp" => m,
                    "heuristic" => h,
                    _ => {
                        if drivers[s].random(4) == 0 {
                            RandomPolicy.choose(&view, &legal, &mut drivers[s])
                        } else if s == 0 {
                            h
                        } else {
                            m
                        }
                    }
                };
                let kind = match legal.request() {
                    Request::TeamPreview { .. } => "preview",
                    Request::ForceSwitch { .. } => "switch",
                    _ => "move",
                };
                // What parses back: `default` where only it writes the choice.
                let text = |c: &Choice| json_string(&format_choice_for(&battle, side, c, &[]));
                let mut line = format!(
                    "{{\"seed\":{seed},\"side\":{s},\"n\":{n},\"turn\":{},\"kind\":\"{kind}\",\"maxbp\":{},\"heuristic\":{},\"driver\":{}",
                    battle.turn(),
                    text(&m),
                    text(&h),
                    text(&driver)
                );
                if n == 0 {
                    line.push_str(&format!(
                        ",\"format\":{},\"teams\":[{},{}]",
                        json_string(format.id),
                        json_string(&texts[0]),
                        json_string(&texts[1])
                    ));
                }
                line.push('}');
                writeln!(out, "{line}").map_err(|e| e.to_string())?;
                battle.submit(side, driver).map_err(|e| format!("seed {seed}: driver choice rejected: {e}"))?;
                for e in battle.drain_events() {
                    tracker.apply(&e);
                }
                n += 1;
            }
        }
        decisions += n;
    }
    out.flush().map_err(|e| e.to_string())?;
    eprintln!("{} battles, {decisions} decisions", a.battles);
    Ok(())
}
