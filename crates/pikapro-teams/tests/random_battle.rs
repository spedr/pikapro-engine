//! Showdown's random-battle teams, recorded by pikapro-conformance's
//! `random_teams --record` (`random_battle.txt`): the port must still give
//! each recorded team, and the same digest over many more. No Node needed;
//! `random_teams` itself diffs any number of seeds against Showdown.

use pikapro::{BattleRng, Seed};
use pikapro_teams::random_battle::{self, RandomBattle};

/// A `gen5,` or `sodium,` seed as the file writes it.
fn parse_seed(text: &str) -> Seed {
    if let Some(h) = text.strip_prefix("gen5,") {
        return random_battle::gen5_seed(u64::from_str_radix(h, 16).expect("a gen5 seed"));
    }
    let h = text.strip_prefix("sodium,").expect("a sodium seed");
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate().take(h.len() / 2) {
        *b = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).expect("hex");
    }
    Seed::Sodium(key)
}

fn team(format: RandomBattle, seed: Seed) -> String {
    match random_battle::generate(format, &mut BattleRng::new(seed)) {
        Ok(sets) => random_battle::team_text(&sets),
        Err(e) => format!("error: {e}"),
    }
}

/// FNV-1a over the lines, each ended by a newline.
fn digest(lines: impl Iterator<Item = String>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for line in lines {
        for b in line.bytes().chain(core::iter::once(b'\n')) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

#[test]
fn recorded_showdown_teams_are_generated_again() {
    let mut failures = Vec::new();
    let (mut teams, mut digests) = (0, 0);
    for line in include_str!("random_battle.txt").lines().filter(|l| !l.starts_with('#')) {
        let mut parts = line.splitn(4, ' ');
        let (kind, format) = (parts.next().unwrap(), parts.next().unwrap());
        let format = RandomBattle::for_format(format).expect("a random battle");
        match kind {
            "team" => {
                let (seed, sets) = (parts.next().unwrap(), parts.next().unwrap());
                teams += 1;
                let ours = team(format, parse_seed(seed));
                if ours != sets {
                    failures.push(format!("{} {seed}:\n  ours     {ours}\n  Showdown {sets}", format.format_id()));
                }
            }
            "digest" => {
                let n: u64 = parts.next().unwrap().parse().unwrap();
                let recorded = u64::from_str_radix(parts.next().unwrap(), 16).unwrap();
                digests += 1;
                let ours = digest((0..n).map(|s| team(format, random_battle::gen5_seed(s))));
                if ours != recorded {
                    failures.push(format!("{}: the digest of seeds 0..{n} differs", format.format_id()));
                }
            }
            other => panic!("unknown line kind {other}"),
        }
    }
    assert!(teams >= 40 && digests == 2, "random_battle.txt shrank: {teams} teams, {digests} digests");
    assert!(failures.is_empty(), "{} differ:\n{}", failures.len(), failures.join("\n"));
}
