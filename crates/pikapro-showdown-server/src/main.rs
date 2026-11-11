//! `pikapro-showdown-server`: a Showdown-compatible websocket server on
//! pikapro, for poke-env.

use std::process::ExitCode;

use pikapro_showdown_server::{Config, Limits, log, serve};

const USAGE: &str = "\
Usage: pikapro-showdown-server [OPTIONS]

A Pokémon Showdown-compatible websocket server on pikapro. poke-env
connects with ServerConfiguration(\"ws://HOST:PORT/showdown/websocket\", ...).
It has no accounts or passwords: keep it on a trusted network.

Options:
  --host HOST            Address to listen on (default 127.0.0.1, this machine only)
  --port PORT            Port (default 8000, Showdown's)
  --no-validate          Accept any team the engine can build (no Showdown
                         team validation)
  --seed N               Seed battles deterministically from N (default: a
                         fresh seed per battle, as Showdown's)
  --disconnect-grace S   Seconds before a disconnected player forfeits (default 60)
  --max-connections N    Open connections at most (default 1000)
  --max-battles-per-user N
                         Unfinished battles one user may play at once
                         (default 64; 0 for no limit)
  --max-ended-rooms N    Finished battles' rooms kept at once (default 1000;
                         0 for no limit); each player's own are kept to 16
  --max-lines-per-second N
                         Command lines one connection may send per second
                         (default 0: no limit). Lines past it are dropped,
                         battle choices included, with Showdown's throttle
                         notice: for a server exposed to untrusted clients
  -h, --help             This help
";

fn main() -> ExitCode {
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 8000;
    let mut config = Config::default();
    let mut limits = Limits::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        let parsed: Result<(), String> = match arg.as_str() {
            "--host" => value("--host").map(|v| host = v),
            "--port" => value("--port").and_then(|v| v.parse().map(|p| port = p).map_err(|e| format!("--port: {e}"))),
            "--no-validate" => {
                config.validate = false;
                Ok(())
            }
            "--seed" => value("--seed")
                .and_then(|v| v.parse().map(|s| config.seed = Some(s)).map_err(|e| format!("--seed: {e}"))),
            "--disconnect-grace" => value("--disconnect-grace").and_then(|v| {
                v.parse::<u64>()
                    .map(|s| config.disconnect_grace_ms = s.saturating_mul(1000))
                    .map_err(|e| format!("--disconnect-grace: {e}"))
            }),
            "--max-connections" => value("--max-connections").and_then(|v| {
                v.parse().map(|n| limits.max_connections = n).map_err(|e| format!("--max-connections: {e}"))
            }),
            "--max-battles-per-user" => value("--max-battles-per-user").and_then(|v| {
                v.parse().map(|n| config.max_battles_per_user = n).map_err(|e| format!("--max-battles-per-user: {e}"))
            }),
            "--max-ended-rooms" => value("--max-ended-rooms").and_then(|v| {
                v.parse().map(|n| config.max_ended_rooms = n).map_err(|e| format!("--max-ended-rooms: {e}"))
            }),
            "--max-lines-per-second" => value("--max-lines-per-second").and_then(|v| {
                v.parse().map(|n| config.max_lines_per_second = n).map_err(|e| format!("--max-lines-per-second: {e}"))
            }),
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => Err(format!("unknown argument {other:?}")),
        };
        if let Err(e) = parsed {
            eprintln!("{e}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    }
    if cfg!(panic = "abort") {
        log("built with panic = \"abort\" (the release profile): a crashing battle stops the server; \
             build with `--profile release-server` so that it only ends its room");
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("could not start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        let listener = match tokio::net::TcpListener::bind((host.as_str(), port)).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("could not listen on {host}:{port}: {e}");
                return ExitCode::FAILURE;
            }
        };
        log(&format!("listening on ws://{host}:{port}/showdown/websocket"));
        if listener.local_addr().is_ok_and(|a| !a.ip().is_loopback()) {
            log("not a loopback address: anyone who can reach it can log in as any free name (no passwords)");
        }
        serve(listener, config, limits).await;
        ExitCode::SUCCESS
    })
}
