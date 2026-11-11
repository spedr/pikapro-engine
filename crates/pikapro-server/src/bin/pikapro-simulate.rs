//! `pikapro-simulate`: a drop-in for `pokemon-showdown simulate-battle`.
//!
//! Reads command lines from stdin and writes the stream's messages to
//! stdout, each followed by a blank line, as Showdown's `BattleTextStream`
//! does: input is taken up to its last complete line whenever some
//! arrives (a trailing line without a newline is never run), each
//! message ends with `\n\n`.
//!
//! ```sh
//! printf '>start {"formatid":"gen9championsvgc2026regmc"}\n>player p1 {"team":"..."}\n...' | pikapro-simulate
//! ```
//!
//! Options (Showdown's): `-R` / `--replay` writes only the updates, as the
//! omniscient log; `-S` / `--spectate` as the spectators'. `-D` /
//! `--debug` is accepted and ignored. A leading `simulate-battle` argument
//! is ignored, so scripts written for `pokemon-showdown simulate-battle`
//! run unchanged. An error (Showdown's exception) is written to stderr and
//! exits with status 1, as Showdown's process does, without the output of
//! the write that failed (Showdown's process dies before writing it).

use std::io::{Read, Write};
use std::process::ExitCode;

use pikapro_server::{BattleStream, ReplayMode, StreamOptions};

fn main() -> ExitCode {
    let mut options = StreamOptions::default();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "simulate-battle" | "-D" | "--debug" => {}
            "-R" | "--replay" => options.replay = options.replay.or(Some(ReplayMode::Omniscient)),
            "-S" | "--spectate" => options.replay = Some(ReplayMode::Spectator),
            "-h" | "--help" => {
                println!(
                    "pikapro-simulate [--replay | --spectate]\n\n  Simulates a battle, taking Showdown's battle stream protocol on stdin\n  and writing its output to stdout (see `pokemon-showdown simulate-battle`)."
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("pikapro-simulate: unknown argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }
    let mut stream = BattleStream::new(options);
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                eprintln!("pikapro-simulate: {e}");
                return ExitCode::FAILURE;
            }
        };
        // Only the new bytes can hold a newline: the rest had none.
        let Some(at) = buf[..n].iter().rposition(|&b| b == b'\n') else {
            pending.extend_from_slice(&buf[..n]);
            continue;
        };
        let last = pending.len() + at;
        pending.extend_from_slice(&buf[..n]);
        let rest = pending.split_off(last + 1);
        let chunk = String::from_utf8_lossy(&pending[..last]).into_owned();
        pending = rest;
        if let Err(e) = stream.write(&chunk) {
            // Showdown's exception ends the process before its output
            // loop writes what this write pushed.
            eprintln!("Error: {e}");
            return ExitCode::FAILURE;
        }
        if write_messages(&mut stream, &mut stdout).is_err() {
            return ExitCode::FAILURE;
        }
        if stream.is_ended() {
            break;
        }
    }
    stream.write_end();
    if write_messages(&mut stream, &mut stdout).is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// `BattleTextStream._listen`: each message, a newline if it lacks one,
/// then a blank line.
fn write_messages(stream: &mut BattleStream, out: &mut impl Write) -> std::io::Result<()> {
    for message in stream.drain() {
        let mut text = message.to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
        out.write_all(text.as_bytes())?;
    }
    out.flush()
}
