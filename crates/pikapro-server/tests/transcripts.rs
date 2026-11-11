//! Golden stream transcripts: scripts recorded by
//! `examples/stream_fuzz.rs --record`, with every message Showdown's
//! `BattleStream` sent for them, replayed on pikapro-server without Node.

mod transcript;

use std::path::PathBuf;

use transcript::{Transcript, first_difference, run_channels, run_ours};

fn transcripts() -> Vec<(String, Transcript)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts");
    // Transcripts are never deleted: the manifest lists every one.
    pikapro_conformance::fixture::check_manifest(&dir, "txt")
        .unwrap_or_else(|e| panic!("{e}"))
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).expect("a transcript");
            let name = p.file_name().expect("a name").to_string_lossy().into_owned();
            let t = Transcript::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            (name, t)
        })
        .collect()
}

#[test]
fn transcripts_match_showdown() {
    let all = transcripts();
    let mut failures = Vec::new();
    for (name, t) in &all {
        let options = transcript::options(t.keep_alive, t.replay.as_deref());
        let (ours, error) = run_ours(&t.writes, options);
        if let Some(d) = first_difference(&ours, &t.messages) {
            failures.push(format!("{name}: {d}"));
        } else if error != t.error {
            failures.push(format!("{name}: error: ours {error:?}, Showdown's {:?}", t.error));
        } else if !t.channel_writes.is_empty() {
            let ours: Vec<String> = run_channels(&t.channel_writes).iter().map(|(n, c)| format!("[{n}] {c}")).collect();
            let theirs: Vec<String> = t.channels.iter().map(|(n, c)| format!("[{n}] {c}")).collect();
            if let Some(d) = first_difference(&ours, &theirs) {
                failures.push(format!("{name}: channels: {d}"));
            }
        }
    }
    assert!(failures.is_empty(), "{} of {} transcripts differ:\n{}", failures.len(), all.len(), failures.join("\n"));
}

/// The transcripts round-trip through their text form.
#[test]
fn transcript_format_round_trips() {
    for (name, t) in transcripts().into_iter().take(3) {
        assert_eq!(Transcript::parse(&t.write()).as_ref(), Ok(&t), "{name}");
    }
}

const SMALL: &str = r#">start {"formatid":"gen9customgame@@@!Team Preview","seed":"sodium,5eed"}
>player p1 {"name":"A","team":"Pikachu||choicespecs|static|thunderbolt,quickattack|Timid|,,,252,,252||||50|"}
>player p2 {"name":"B","team":"Snorlax||leftovers|thickfat|rest,bodyslam|Timid|,,,252,,252||||50|"}
>p1 move 1
>p2 move 2
>p1 move 2
>p1 undo
>p1 move 1
>p2 move 1
>chat hi
>forcewin p2
"#;

fn simulate(args: &[&str], input: &str) -> std::process::Output {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_pikapro-simulate"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("pikapro-simulate runs");
    child.stdin.take().expect("piped").write_all(input.as_bytes()).expect("write the script");
    child.wait_with_output().expect("pikapro-simulate exits")
}

/// Frames messages as Showdown's `BattleTextStream`: a newline if the
/// message lacks one, then a blank line.
fn framed(messages: &[String]) -> String {
    let mut out = String::new();
    for m in messages {
        out.push_str(m);
        if !m.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// `pikapro-simulate` frames the stream's messages as Showdown's
/// `BattleTextStream` does (a small script arrives in one read, so it is
/// one write), and ignores a last line without a newline.
#[test]
fn simulate_binary_frames_messages() {
    let out = simulate(&[], &format!("{SMALL}>p1 move 1"));
    assert!(out.status.success());
    let text = transcript::normalize(&String::from_utf8(out.stdout).expect("UTF-8"), false);
    let (messages, _) = run_ours(&[SMALL.to_string()], transcript::options(false, None));
    assert_eq!(text, framed(&messages));
    assert!(text.contains("|error|[Unavailable choice] Can't move: Pikachu's Quick Attack is disabled"));
    assert!(text.ends_with("]}\n\n"), "ends with the end log");

    let out = simulate(&["simulate-battle", "--spectate"], SMALL);
    let text = transcript::normalize(&String::from_utf8(out.stdout).expect("UTF-8"), false);
    let (messages, _) = run_ours(&[SMALL.to_string()], transcript::options(false, Some("spectator")));
    assert_eq!(text, framed(&messages));
    assert!(!text.contains("sideupdate") && text.contains("|-damage|p2a: Snorlax|"));

    // Showdown's exception: status 1, the message on stderr.
    let out = simulate(&[], ">p1 move 1\n");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Cannot read properties of null (reading 'choose')"));
}
