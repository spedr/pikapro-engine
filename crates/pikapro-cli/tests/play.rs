//! `pikapro play` with team files.

use std::process::Command;

use pikapro::Format;
use pikapro::testing::sample_team;
use pikapro_protocol::{TeamSheet, export_sheet};

#[test]
fn play_keeps_the_team_files_nicknames_and_shininess() {
    let format = Format::by_id("gen9singles").unwrap();
    let mut sheet = TeamSheet::new(sample_team(&format));
    for (i, extras) in sheet.extras.iter_mut().enumerate() {
        extras.name = format!("Nick{i}");
        extras.shiny = true;
    }
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("play-nicknames.txt");
    std::fs::write(&path, export_sheet(&sheet, &format)).unwrap();
    let file = path.to_str().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_pikapro"))
        .args(["play", "--format", "gen9singles", "--seed", "1", "--team1", file, "--team2", file])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let log = String::from_utf8(out.stdout).unwrap();
    assert!(log.contains("|switch|p1a: Nick0|"), "{log}");
    assert!(log.contains("|switch|p2a: Nick0|"), "{log}");
    assert!(log.contains(", shiny|"), "{log}");
}

#[test]
fn play_ends_quietly_when_the_reader_stops() {
    // As `pikapro play | head -1`: the reader closes stdout after the first
    // byte, while most of the log is still to come.
    let mut child = Command::new(env!("CARGO_BIN_EXE_pikapro"))
        .args(["play", "--format", "gen9singles", "--seed", "1", "--requests"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = [0u8; 1];
    std::io::Read::read_exact(child.stdout.as_mut().unwrap(), &mut first).unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && stderr.is_empty(), "{:?}: {stderr}", out.status);
}
