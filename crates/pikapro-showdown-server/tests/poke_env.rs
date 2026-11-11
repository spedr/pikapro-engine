//! poke-env against the server, end to end: the server binary on a free
//! port, and `tests/python/battles.py` playing RandomPlayer and
//! MaxBasePowerPlayer battles in every format, by challenge and on the
//! ladder, checking what poke-env's battle objects say.
//!
//! Needs a Python with poke-env, so it only runs when `PIKAPRO_POKE_ENV_PYTHON`
//! names one (see the README):
//!
//! ```text
//! uv venv .venv && uv pip install --python .venv poke-env
//! PIKAPRO_POKE_ENV_PYTHON=$PWD/.venv/bin/python cargo test -p pikapro-showdown-server --test poke_env
//! ```
//!
//! `PIKAPRO_POKE_ENV_BATTLES` sets the battles per run (default 20).

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn poke_env_players_battle_in_every_format() {
    let Ok(python) = std::env::var("PIKAPRO_POKE_ENV_PYTHON") else {
        eprintln!("PIKAPRO_POKE_ENV_PYTHON is not set: skipping the poke-env integration test");
        return;
    };
    let battles: usize = std::env::var("PIKAPRO_POKE_ENV_BATTLES").ok().and_then(|n| n.parse().ok()).unwrap_or(20);
    let port = free_port();
    let _server = Server(
        Command::new(env!("CARGO_BIN_EXE_pikapro-showdown-server"))
            .args(["--port", &port.to_string(), "--seed", "2200000"])
            .stderr(Stdio::null())
            .spawn()
            .expect("the server starts"),
    );
    let start = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(start.elapsed() < Duration::from_secs(10), "the server did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/python/battles.py");
    let url = format!("ws://127.0.0.1:{port}/showdown/websocket");
    let mut failures = Vec::new();
    let mut n = 0;
    for format in ["gen9championsvgc2026regmc", "gen9vgc2025regi", "gen9customgame", "gen9doublescustomgame"] {
        for mode in ["challenge", "ladder"] {
            for (p1, p2) in [("random", "random"), ("maxbp", "random")] {
                n += 1;
                let output = Command::new(&python)
                    .args([script, "--server", &url, "--format", format, "--mode", mode, "--p1", p1, "--p2", p2])
                    .args(["--battles", &battles.to_string(), "--concurrent", "4", "--name", &format!("it{n}")])
                    .args(["--timeout", "300"])
                    .output()
                    .expect("python runs");
                let stdout = String::from_utf8_lossy(&output.stdout);
                let summary = stdout.lines().rev().find(|l| l.starts_with('{')).unwrap_or("").to_string();
                eprintln!("{summary}");
                let clean = summary.contains("\"problems\": []") && summary.contains("\"log\": {}");
                if !output.status.success() || !clean {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let tail: Vec<&str> = stderr.lines().rev().take(20).collect();
                    failures.push(format!("{format} {mode} {p1}/{p2}: {summary}\n{}", tail.join("\n")));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
