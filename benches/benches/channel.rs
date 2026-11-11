//! Criterion benchmarks of the player-channel parser and session:
//! `cargo bench -p pikapro-bench --bench channel [-- FILTER]`, over player 1's
//! channels of a few random battles per format (our renderer's lines and
//! requests, in Showdown's order).
//!
//! * `channel/<format>/lines`: every line into a `ChannelParser` (events
//!   to a sink that drops them), per line;
//! * `channel/<format>/lines+tracker`: the same, the events applied to a
//!   `PublicTracker`, per line;
//! * `channel/<format>/requests`: the requests' JSON read into
//!   `RequestData` (`parse_request`), per request;
//! * `channel/<format>/session`: a `ChannelSession` fed the whole channel,
//!   the observation and the mask read at each decision: the cost of
//!   playing from text, per decision.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use pikapro::{BattleRng, Format, Seed, SideId};
use pikapro_bench as w;
use pikapro_protocol::channel::{ChannelParser, parse_request, request_body};
use pikapro_protocol::{BattleInfo, Perspective, Renderer};
use pikapro_rl::session::{ChannelSession, SessionOptions};
use pikapro_view::PublicTracker;

/// Player 1's channel of `n` random battles.
fn channels(format: &Format, n: u64) -> Vec<Vec<String>> {
    let teams = w::teams(format);
    let renderer = Renderer::new(format, Perspective::Player(SideId::P1));
    (0..n)
        .map(|seed| {
            let mut battle = w::new_battle(format, &teams, 5_150_000 + seed, true);
            let mut rng = BattleRng::new(Seed::fast(seed ^ 0xC4A7));
            let mut lines = renderer.header();
            let mut chunk: Vec<_> = battle.drain_events().collect();
            loop {
                lines.extend(renderer.log(chunk.iter()));
                if battle.is_over() {
                    break;
                }
                let mut data = battle.request_data(SideId::P1);
                data.answered = false;
                lines.push(format!("|request|{}", renderer.request_json(&data)));
                for side in SideId::BOTH {
                    let legal = battle.legal_actions(side);
                    if !legal.is_waiting() {
                        let c = legal.sample(&mut rng);
                        battle.submit(side, c).expect("a legal choice");
                    }
                }
                chunk = battle.drain_events().collect();
            }
            lines
        })
        .collect()
}

fn channel(c: &mut Criterion) {
    for id in ["championsvgc", "gen9singles"] {
        let format = w::format(id);
        let battles = channels(&format, 16);
        let lines: usize = battles.iter().map(Vec::len).sum();
        let requests: Vec<&str> = battles.iter().flatten().filter_map(|l| request_body(l)).collect();
        let exact = BattleInfo::for_format(&format).report_exact_hp;
        let mut group = c.benchmark_group(format!("channel/{id}"));

        group.throughput(Throughput::Elements(lines as u64));
        group.bench_function("lines", |b| {
            b.iter(|| {
                for battle in &battles {
                    let mut p = ChannelParser::new(&format, Some(SideId::P1));
                    for line in battle {
                        black_box(p.feed(line, &mut |e| {
                            black_box(e);
                        }));
                    }
                }
            })
        });
        group.bench_function("lines+tracker", |b| {
            b.iter(|| {
                for battle in &battles {
                    let mut p = ChannelParser::new(&format, Some(SideId::P1));
                    let mut t = PublicTracker::for_format(&format);
                    for line in battle {
                        p.feed(line, &mut |e| t.apply(&e));
                    }
                    black_box(&t);
                }
            })
        });

        group.throughput(Throughput::Elements(requests.len() as u64));
        group.bench_function("requests", |b| {
            b.iter(|| {
                for json in &requests {
                    black_box(parse_request(json, &format).expect("a request"));
                }
            })
        });

        let mut decisions = 0u64;
        for battle in &battles {
            let mut s =
                ChannelSession::for_format(format.clone(), exact, SessionOptions::default()).expect("a session");
            s.set_side(SideId::P1);
            for line in battle {
                s.feed(line);
                decisions += u64::from(request_body(line).is_some() && s.needs_action());
            }
        }
        group.throughput(Throughput::Elements(decisions));
        group.bench_function("session", |b| {
            b.iter(|| {
                for battle in &battles {
                    let mut s = ChannelSession::for_format(format.clone(), exact, SessionOptions::default())
                        .expect("a session");
                    s.set_side(SideId::P1);
                    for line in battle {
                        s.feed(line);
                        if request_body(line).is_some() && s.needs_action() {
                            black_box(s.obs());
                            black_box(s.mask());
                        }
                    }
                }
            })
        });
        group.finish();
    }
}

criterion_group!(benches, channel);
criterion_main!(benches);
