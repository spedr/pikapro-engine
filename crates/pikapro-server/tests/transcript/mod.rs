//! Golden stream transcripts: a script of writes and what Showdown's
//! `BattleStream` sent for it. Shared by `tests/transcripts.rs`
//! (replay without Node) and `examples/stream_fuzz.rs` (recording).
//!
//! ```text
//! # comment
//! @option keep-alive     the stream's options (also `@option replay spectator`)
//! @write                 a chunk written to the stream (its lines follow)
//! @message               a message the stream sent
//! @error                 the exception that stopped the script
//! @channel-write p1      a chunk written to a getPlayerStreams channel
//! @channel spectator     a chunk a channel read
//! ```
//!
//! A section's text is its lines up to the next `@` line, joined with
//! `\n`. A line that starts with `@` or `\`, or holds a carriage return,
//! is written escaped: a `\`, then the line with `\` doubled and `\r` as
//! `\r` (so that files keep their bytes through any line-ending handling).
//! Times in `|t:|` lines are left out, and so are the `|debug|` lines
//! Showdown writes in its custom-game formats.

#![allow(dead_code)]

use pikapro_server::{BattleStream, Channel, PlayerStreams, ReplayMode, StreamOptions};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    pub comments: Vec<String>,
    /// `StreamOptions::keep_alive`.
    pub keep_alive: bool,
    /// `StreamOptions::replay`: `omniscient` or `spectator`.
    pub replay: Option<String>,
    pub writes: Vec<String>,
    pub messages: Vec<String>,
    pub error: Option<String>,
    /// `(channel, chunk)` written through `getPlayerStreams`.
    pub channel_writes: Vec<(String, String)>,
    /// `(channel, chunk)` read, per channel in order.
    pub channels: Vec<(String, String)>,
}

fn escape(text: &str, out: &mut String) {
    for line in text.split('\n') {
        if line.starts_with('@') || line.starts_with('\\') || line.contains('\r') {
            out.push('\\');
            out.push_str(&line.replace('\\', "\\\\").replace('\r', "\\r"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
}

fn unescape(line: &str) -> String {
    let Some(rest) = line.strip_prefix('\\') else { return line.to_string() };
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some('\\')) => {
                out.push('\\');
                chars.next();
            }
            ('\\', Some('r')) => {
                out.push('\r');
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

impl Transcript {
    pub fn write(&self) -> String {
        let mut out = String::new();
        for c in &self.comments {
            out.push_str(&format!("# {c}\n"));
        }
        if self.keep_alive {
            out.push_str("@option keep-alive\n");
        }
        if let Some(r) = &self.replay {
            out.push_str(&format!("@option replay {r}\n"));
        }
        for w in &self.writes {
            out.push_str("@write\n");
            escape(w, &mut out);
        }
        for m in &self.messages {
            out.push_str("@message\n");
            escape(m, &mut out);
        }
        if let Some(e) = &self.error {
            out.push_str("@error\n");
            escape(e, &mut out);
        }
        for (c, w) in &self.channel_writes {
            out.push_str(&format!("@channel-write {c}\n"));
            escape(w, &mut out);
        }
        for (c, m) in &self.channels {
            out.push_str(&format!("@channel {c}\n"));
            escape(m, &mut out);
        }
        out
    }

    pub fn parse(text: &str) -> Result<Transcript, String> {
        let mut t = Transcript::default();
        let mut section: Option<(String, Vec<String>)> = None;
        let flush = |t: &mut Transcript, section: Option<(String, Vec<String>)>| -> Result<(), String> {
            let Some((head, lines)) = section else { return Ok(()) };
            let body = lines.join("\n");
            let (kind, arg) = head.split_once(' ').unwrap_or((head.as_str(), ""));
            match kind {
                "@option" if arg == "keep-alive" => t.keep_alive = true,
                "@option" => t.replay = arg.strip_prefix("replay ").map(String::from),
                "@write" => t.writes.push(body),
                "@message" => t.messages.push(body),
                "@error" => t.error = Some(body),
                "@channel-write" => t.channel_writes.push((arg.to_string(), body)),
                "@channel" => t.channels.push((arg.to_string(), body)),
                _ => return Err(format!("unknown section {head}")),
            }
            Ok(())
        };
        let text = text.strip_suffix('\n').unwrap_or(text);
        for line in text.split('\n') {
            // A checkout that turned newlines into CRLF: no raw CR is ever written.
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.starts_with('@') {
                flush(&mut t, section.take())?;
                section = Some((line.to_string(), Vec::new()));
            } else if let Some((_, lines)) = &mut section {
                lines.push(unescape(line));
            } else if let Some(c) = line.strip_prefix("# ") {
                t.comments.push(c.to_string());
            }
        }
        flush(&mut t, section)?;
        Ok(t)
    }
}

/// `|t:|<time>` without the time, `|debug|` lines dropped (`debug`: a
/// custom game, where Showdown writes them), in any message or chunk.
pub fn normalize(text: &str, debug: bool) -> String {
    let lines: Vec<String> = text
        .split('\n')
        .filter(|l| !(debug && l.starts_with("|debug|")))
        .map(|l| if l.starts_with("|t:|") { "|t:|".to_string() } else { l.to_string() })
        .collect();
    lines.join("\n")
}

/// Normalises a message list; an update left empty by dropping `|debug|`
/// lines is dropped.
pub fn normalize_messages(messages: &[String], debug: bool) -> Vec<String> {
    messages
        .iter()
        .filter(|m| !(debug && m.strip_prefix("update\n").is_some_and(only_debug)))
        .map(|m| normalize(m, debug))
        .collect()
}

/// A text made only of `|debug|` lines (which pikapro-server never sends).
fn only_debug(text: &str) -> bool {
    !text.is_empty() && text.split('\n').all(|l| l.starts_with("|debug|"))
}

/// Normalises channel chunks; a chunk made only of `|debug|` lines is
/// dropped (`debug`: a custom game).
pub fn normalize_chunks(chunks: &[(String, String)], debug: bool) -> Vec<(String, String)> {
    chunks.iter().filter(|(_, c)| !(debug && only_debug(c))).map(|(n, c)| (n.clone(), normalize(c, debug))).collect()
}

fn zero_clock() -> u64 {
    0
}

/// The stream options a transcript names.
pub fn options(keep_alive: bool, replay: Option<&str>) -> StreamOptions {
    let replay = replay.map(|r| if r == "spectator" { ReplayMode::Spectator } else { ReplayMode::Omniscient });
    StreamOptions { clock: zero_clock, keep_alive, replay }
}

/// What pikapro-server sends for the transcript's writes (normalised).
pub fn run_ours(writes: &[String], options: StreamOptions) -> (Vec<String>, Option<String>) {
    let mut stream = BattleStream::new(options);
    let mut messages = Vec::new();
    let mut error = None;
    for w in writes {
        let r = stream.write(w);
        messages.extend(stream.drain().iter().map(ToString::to_string));
        if let Err(e) = r {
            error = Some(e.message);
            break;
        }
    }
    (normalize_messages(&messages, false), error)
}

pub fn channel(name: &str) -> Channel {
    match name {
        "omniscient" => Channel::Omniscient,
        "spectator" => Channel::Spectator,
        "p1" => Channel::Player(pikapro::SideId::P1),
        _ => Channel::Player(pikapro::SideId::P2),
    }
}

pub const CHANNELS: [&str; 4] = ["omniscient", "spectator", "p1", "p2"];

/// What pikapro-server's `getPlayerStreams` channels read for the writes, in
/// channel order (normalised).
pub fn run_channels(writes: &[(String, String)]) -> Vec<(String, String)> {
    let mut streams = PlayerStreams::new(BattleStream::new(options(false, None)));
    for (c, w) in writes {
        if streams.write(channel(c), w).is_err() {
            break;
        }
    }
    streams.write_end();
    let mut out = Vec::new();
    for c in CHANNELS {
        for chunk in streams.drain(channel(c)) {
            out.push((c.to_string(), normalize(&chunk, false)));
        }
    }
    out
}

/// The first difference between two lists of texts: index, and the first
/// differing line of each.
pub fn first_difference(ours: &[String], theirs: &[String]) -> Option<String> {
    for i in 0..ours.len().max(theirs.len()) {
        let (a, b) = (ours.get(i), theirs.get(i));
        if a == b {
            continue;
        }
        let (a, b) = (a.map_or("<none>", String::as_str), b.map_or("<none>", String::as_str));
        let al: Vec<&str> = a.split('\n').collect();
        let bl: Vec<&str> = b.split('\n').collect();
        let k = (0..al.len().max(bl.len())).find(|&k| al.get(k) != bl.get(k)).unwrap_or(0);
        let head = al.first().copied().unwrap_or("");
        return Some(format!(
            "message {i} ({head}), line {k}:\n  ours:   {}\n  theirs: {}",
            al.get(k).copied().unwrap_or("<none>"),
            bl.get(k).copied().unwrap_or("<none>")
        ));
    }
    None
}
