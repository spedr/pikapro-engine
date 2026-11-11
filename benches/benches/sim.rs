//! Criterion benchmarks: `cargo bench -p pikapro-bench [-- FILTER]`.
//!
//! Wall-clock timings, for local comparisons (`--save-baseline` /
//! `--baseline`). `cargo bench-insns` checks instruction counts instead,
//! which don't depend on the machine's load.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use pikapro::{BattleRng, Seed, SideId};
use pikapro_bench as w;
use pikapro_protocol::{Perspective, Renderer};

/// Full battles of random self-play, events off and on (drained each step).
fn step(c: &mut Criterion) {
    for id in w::FORMATS {
        let format = w::format(id);
        let teams = w::teams(&format);
        let mut group = c.benchmark_group(format!("battle/{id}"));
        group.throughput(Throughput::Elements(1));
        for (name, events) in [("events-off", false), ("events-on", true)] {
            let mut seed = 0u64;
            group.bench_function(name, |b| {
                b.iter(|| {
                    seed += 1;
                    w::play(&format, &teams, seed, events)
                })
            });
        }
        group.finish();
    }
}

/// One decision point: both sides' choices submitted and the turn resolved.
fn decision(c: &mut Criterion) {
    let mut group = c.benchmark_group("decision");
    for id in w::FORMATS {
        let format = w::format(id);
        let snapshots = w::snapshots(&format, &w::teams(&format), 0, 256);
        let mut i = 0;
        group.bench_function(id, |b| {
            b.iter_batched_ref(
                || {
                    i = (i + 1) % snapshots.len();
                    (snapshots[i].clone(), BattleRng::new(Seed::fast(i as u64)))
                },
                |(battle, policy)| w::step(battle, policy),
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

/// `Battle::clone` and `Battle::fork` of a battle in progress.
fn fork(c: &mut Criterion) {
    let mut group = c.benchmark_group("fork");
    for id in w::FORMATS {
        let format = w::format(id);
        let snapshots = w::snapshots(&format, &w::teams(&format), 0, 64);
        let battle = &snapshots[snapshots.len() / 2];
        group.bench_function(format!("clone/{id}"), |b| b.iter(|| black_box(battle).clone()));
        let mut seed = 0;
        group.bench_function(format!("fork/{id}"), |b| {
            b.iter(|| {
                seed += 1;
                black_box(battle).fork(Seed::fast(seed))
            })
        });
    }
    group.finish();
}

/// Legal-action generation for both sides, over the decision points of a
/// few battles.
fn legal(c: &mut Criterion) {
    let mut group = c.benchmark_group("legal_actions");
    for id in w::FORMATS {
        let format = w::format(id);
        let snapshots = w::snapshots(&format, &w::teams(&format), 0, 256);
        group.throughput(Throughput::Elements(snapshots.len() as u64));
        group.bench_function(id, |b| {
            b.iter(|| {
                for battle in &snapshots {
                    black_box(w::legal_actions(battle));
                }
            })
        });
    }
    group.finish();
}

/// `Battle::request_data` plus `Renderer::write_request` for both sides.
fn requests(c: &mut Criterion) {
    let mut group = c.benchmark_group("request");
    for id in w::FORMATS {
        let format = w::format(id);
        let snapshots = w::snapshots(&format, &w::teams(&format), 0, 256);
        let renderer = Renderer::new(&format, Perspective::Player(SideId::P1));
        let mut buf = String::with_capacity(4096);
        group.throughput(Throughput::Elements(snapshots.len() as u64));
        group.bench_function(id, |b| {
            b.iter(|| {
                for battle in &snapshots {
                    black_box(w::requests(battle, &renderer, &mut buf));
                }
            })
        });
    }
    group.finish();
}

/// A whole battle's log rendered for each perspective.
fn render(c: &mut Criterion) {
    let mut group = c.benchmark_group("render");
    let format = w::format("championsvgc");
    let log = w::battle_log(&format, &w::teams(&format), 1);
    group.throughput(Throughput::Elements(log.len() as u64));
    for (name, perspective) in w::PERSPECTIVES {
        group.bench_function(name, |b| b.iter(|| w::render(&format, perspective, black_box(&log))));
    }
    group.finish();
}

/// Building a battle (validation, stats, leads or team preview) and
/// importing a team from Showdown's export format.
fn teams(c: &mut Criterion) {
    let mut group = c.benchmark_group("team");
    for id in w::FORMATS {
        let format = w::format(id);
        let teams = w::teams(&format);
        let mut seed = 0;
        group.bench_function(format!("construct/{id}"), |b| {
            b.iter(|| {
                seed += 1;
                w::construct(&format, &teams, seed)
            })
        });
    }
    let format = w::format("championsvgc");
    let text = w::team_text(&format);
    group.bench_function("import/championsvgc", |b| b.iter(|| w::import_team(&format, black_box(&text))));
    group.finish();
}

criterion_group!(benches, step, decision, fork, legal, requests, render, teams);
criterion_main!(benches);
