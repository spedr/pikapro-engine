//! Golden channel fixtures: a battle's two player channels, as a
//! session was fed them in a passing check (Showdown's own streams, or our
//! renderer's in the round trip), with a digest of what the session gave
//! at each decision. `tests/channel.rs` replays them without Node or the
//! environment: the parser and session must still give the same views.
//!
//! ```text
//! # Recorded by channel_conformance. Do not edit by hand.
//! format championsvgc
//! source showdown
//! seed 5110000
//! options factored row percent
//! player p1
//! |<line>
//! ...
//! decision <digest>
//! player p2
//! ...
//! ```
//!
//! A `decision` line follows each request after which the session must
//! act: a digest (FNV-1a, 64 bits) of the request kind, the view (the
//! tracker's facts, the overlay and the player's own team), the `basic`
//! observation, the mask and `min_switches`.

use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

use pikapro::{Format, SideId};
use pikapro_protocol::BattleInfo;
use pikapro_protocol::channel::LineKind;
use pikapro_rl::session::{ChannelSession, SessionOptions};
use pikapro_rl::vec::{MaskLayout, SwitchBy};
use pikapro_view::HpReport;

use crate::view::Failure;

/// A recorded battle.
#[derive(Clone, Debug)]
pub struct Fixture {
    pub format: Format,
    /// `showdown` (Showdown's streams) or the round trip's source.
    pub source: String,
    pub seed: u64,
    pub masks: MaskLayout,
    pub switch_by: SwitchBy,
    pub hp: HpReport,
    /// Each player's lines, with the digest expected after some of them.
    pub players: [Vec<(String, Option<u64>)>; 2],
}

fn session(f: &Fixture, side: SideId) -> Result<ChannelSession, Failure> {
    // A fixture has the lines only: the session learns its team from the
    // requests.
    let options = SessionOptions::default()
        .with_masks(f.masks)
        .with_switch_by(f.switch_by)
        .with_hp(f.hp)
        .with_username(Some(pikapro_protocol::side_id(side).into()));
    let exact = BattleInfo::for_format(&f.format).report_exact_hp;
    ChannelSession::for_format(f.format.clone(), exact, options).map_err(|e| Failure::new("engine", "session", e))
}

/// FNV-1a, fed integers as little-endian bytes of a fixed width, so a
/// digest doesn't depend on the platform's pointer width (slices of
/// integers wider than a byte are fed as they lie in memory: the digests
/// are a little-endian platform's).
struct Fnv(u64);

impl Fnv {
    fn eat(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        self.eat(bytes);
    }

    fn write_u16(&mut self, i: u16) {
        self.eat(&i.to_le_bytes());
    }

    fn write_u32(&mut self, i: u32) {
        self.eat(&i.to_le_bytes());
    }

    fn write_u64(&mut self, i: u64) {
        self.eat(&i.to_le_bytes());
    }

    fn write_u128(&mut self, i: u128) {
        self.eat(&i.to_le_bytes());
    }

    fn write_usize(&mut self, i: usize) {
        self.eat(&(i as u64).to_le_bytes());
    }

    fn write_isize(&mut self, i: isize) {
        self.eat(&(i as i64).to_le_bytes());
    }
}

/// FNV-1a over what the session gives at a decision: the request kind,
/// the view (every public fact of the tracker, the overlay and the
/// player's own team), the observation, the mask and `min_switches`.
fn digest(s: &mut ChannelSession) -> u64 {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    h.eat(s.request_kind().name().as_bytes());
    let t = s.tracker();
    (t.sides, t.field, t.turn, t.phase, t.outcome, t.ots).hash(&mut h);
    s.overlay().hash(&mut h);
    s.team().hash(&mut h);
    for v in s.obs() {
        h.eat(&v.to_bits().to_le_bytes());
    }
    let mask: Vec<u8> = s.mask().iter().map(|&b| u8::from(b)).collect();
    h.eat(&mask);
    h.eat(&[s.min_switches()]);
    h.0
}

/// A fixture of a battle whose channels passed a check: the lines, and the
/// digests a session gives at their decisions now.
pub fn record(
    format: &Format,
    source: &str,
    seed: u64,
    masks: MaskLayout,
    switch_by: SwitchBy,
    hp: HpReport,
    lines: &[Vec<String>; 2],
) -> Result<String, Failure> {
    let mut f = Fixture {
        format: format.clone(),
        source: source.into(),
        seed,
        masks,
        switch_by,
        hp,
        players: [Vec::new(), Vec::new()],
    };
    for side in SideId::BOTH {
        let mut s = session(&f, side)?;
        for line in &lines[side.index()] {
            let kind = s.feed(line);
            let d = (kind == LineKind::Request && s.needs_action()).then(|| digest(&mut s));
            f.players[side.index()].push((line.clone(), d));
        }
    }
    Ok(write(&f))
}

/// The fixture with its lines kept and its digests taken again: for a
/// deliberate change to what a decision's digest covers (the view's
/// fields, the observation, the dex's ids), not to what the session reads
/// (`PIKAPRO_BLESS=1` in `tests/channel.rs`).
pub fn bless(f: &Fixture) -> Result<String, Failure> {
    let lines = f.players.clone().map(|p| p.into_iter().map(|(line, _)| line).collect::<Vec<_>>());
    record(&f.format, &f.source, f.seed, f.masks, f.switch_by, f.hp, &lines)
}

/// The fixture's text.
pub fn write(f: &Fixture) -> String {
    let mut out = String::from("# Recorded by channel_conformance. Do not edit by hand.\n");
    let masks = if f.masks == MaskLayout::Joint { "joint" } else { "factored" };
    let by = if f.switch_by == SwitchBy::Position { "position" } else { "row" };
    let hp = if f.hp == HpReport::Exact { "exact" } else { "percent" };
    let _ = writeln!(out, "format {}\nsource {}\nseed {}\noptions {masks} {by} {hp}", f.format.id, f.source, f.seed);
    for side in SideId::BOTH {
        let _ = writeln!(out, "player {}", pikapro_protocol::side_id(side));
        for (line, d) in &f.players[side.index()] {
            // Every line of a channel starts with `|` or is a room line
            // (`>battle-...`); an empty one is written as `|`-less `empty`.
            if line.is_empty() {
                out.push_str("empty\n");
            } else {
                let _ = writeln!(out, "line {line}");
            }
            if let Some(d) = d {
                let _ = writeln!(out, "decision {d:016x}");
            }
        }
    }
    out
}

/// Reads a fixture's text.
pub fn parse(text: &str) -> Result<Fixture, String> {
    let mut format = None;
    let (mut source, mut seed) = (String::new(), 0);
    let (mut masks, mut switch_by, mut hp) = (MaskLayout::Factored, SwitchBy::Row, HpReport::Percent);
    let mut players: [Vec<(String, Option<u64>)>; 2] = [Vec::new(), Vec::new()];
    let mut side: Option<usize> = None;
    for (n, line) in text.lines().enumerate() {
        let bad = |what: &str| format!("line {}: {what}", n + 1);
        if line.starts_with('#') {
            continue;
        }
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "format" => format = Some(Format::by_id(rest).ok_or_else(|| bad("unknown format"))?),
            "source" => source = rest.into(),
            "seed" => seed = rest.parse().map_err(|_| bad("bad seed"))?,
            "options" => {
                let o: Vec<&str> = rest.split(' ').collect();
                masks = if o.first() == Some(&"joint") { MaskLayout::Joint } else { MaskLayout::Factored };
                switch_by = if o.get(1) == Some(&"position") { SwitchBy::Position } else { SwitchBy::Row };
                hp = if o.get(2) == Some(&"exact") { HpReport::Exact } else { HpReport::Percent };
            }
            "player" => side = Some(if rest == "p2" { 1 } else { 0 }),
            "line" | "empty" => {
                let s = side.ok_or_else(|| bad("a line before a player"))?;
                players[s].push((if key == "empty" { String::new() } else { rest.into() }, None));
            }
            "decision" => {
                let s = side.ok_or_else(|| bad("a decision before a player"))?;
                let d = u64::from_str_radix(rest, 16).map_err(|_| bad("bad digest"))?;
                players[s].last_mut().ok_or_else(|| bad("a decision before a line"))?.1 = Some(d);
            }
            _ => return Err(bad("unknown key")),
        }
    }
    Ok(Fixture { format: format.ok_or("no format")?, source, seed, masks, switch_by, hp, players })
}

/// Replays a fixture: every decision's digest must be the recorded one.
/// Returns the decisions compared.
pub fn replay(f: &Fixture) -> Result<usize, Failure> {
    replay_with(f, false)
}

/// [`replay`], and with `rejoin` a session that joins the battle at each
/// decision, as Showdown lets a player rejoin its room, must give what the
/// one that read it all gives ([`check_rejoin`](crate::channel::check_rejoin)).
pub fn replay_with(f: &Fixture, rejoin: bool) -> Result<usize, Failure> {
    let options = crate::channel::Options {
        masks: f.masks,
        switch_by: f.switch_by,
        hp: f.hp,
        ..crate::channel::Options::default()
    };
    let mut n = 0;
    for side in SideId::BOTH {
        let mut s = session(f, side)?;
        let mut log = Vec::new();
        for (k, (line, want)) in f.players[side.index()].iter().enumerate() {
            log.push(line.clone());
            let kind = s.feed(line);
            if kind == LineKind::Unknown {
                return Err(Failure::new("golden", "unknown-line", format!("{side:?} line {k}: {line:?}")));
            }
            let got = (kind == LineKind::Request && s.needs_action()).then(|| digest(&mut s));
            if got != *want {
                return Err(Failure::new(
                    "golden",
                    "decision",
                    format!("{side:?} after line {k}: digest {got:x?}, recorded {want:x?}"),
                )
                .with_context(&log));
            }
            if rejoin && got.is_some() {
                crate::channel::check_rejoin(&mut s, &log, side, &options)
                    .map_err(|e| Failure::new("golden", "rejoin", format!("{} ({})", e, f.seed)))?;
            }
            n += usize::from(got.is_some());
        }
    }
    Ok(n)
}
