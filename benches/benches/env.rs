//! Criterion benchmarks of the RL environment and its phases:
//! `cargo bench -p pikapro-bench --bench env [-- FILTER]`.
//!
//! * `env-phase/*`: each phase of a fused step on its own, over the
//!   decision points of a few championsvgc battles, per decision point
//!   (both sides): the engine step with events on, applying its events to
//!   the tracker, both overlays, both encodings, both factored masks.
//! * `env-pool/*`: whole calls at 1 and several threads, per battle step:
//!   `VecEnv::step` (the phases above plus the dispatch and the copy out;
//!   random self-play), and the raw pool's `sample_random` + `step` with
//!   events off.
//!
//! `pikapro-env-split` measures the same split inside `VecEnv::step`, with a
//! per-phase clock (`cargo run --release -p pikapro-bench --bin
//! pikapro-env-split`).

use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use pikapro::{BattleRng, Seed, SideId};
use pikapro_bench as w;
use pikapro_rl::encode::{Basic, Encoder};
use pikapro_rl::env::VecEnv;
use pikapro_rl::pool::{ActionMode, Pool};
use pikapro_rl::space::ActionSpace;
use pikapro_rl::tables::Tables;
use pikapro_rl::vec::MaskLayout;
use pikapro_view::PlayerView;

const FORMAT: &str = "championsvgc";

fn phases(c: &mut Criterion) {
    let format = w::format(FORMAT);
    let points = w::view_points(&format, &w::teams(&format), 0, 256);
    let batches = w::view_batches(&points);
    let space = ActionSpace::new(&format);
    let tables = Tables::for_format(&format);
    let mut group = c.benchmark_group("env-phase");
    group.throughput(Throughput::Elements(points.len() as u64));
    group.bench_function("engine", |b| {
        b.iter_batched_ref(
            || points.iter().map(|p| p.battle.clone()).collect::<Vec<_>>(),
            |battles| {
                for (i, battle) in battles.iter_mut().enumerate() {
                    w::step(battle, &mut BattleRng::new(Seed::fast(i as u64)));
                }
            },
            BatchSize::LargeInput,
        )
    });
    group.bench_function("view-apply", |b| {
        b.iter(|| {
            for (tracker, events) in &batches {
                let mut t = *tracker;
                t.apply_all(events);
                black_box(&t);
            }
        })
    });
    group.bench_function("overlay", |b| {
        b.iter(|| {
            for p in &points {
                black_box(w::overlays(&p.battle));
            }
        })
    });
    let overlays: Vec<_> = points.iter().map(|p| w::overlays(&p.battle)).collect();
    let encoder = Basic;
    let mut obs = vec![0f32; encoder.size()];
    group.bench_function("encode", |b| {
        b.iter(|| {
            for (p, own) in points.iter().zip(&overlays) {
                for side in SideId::BOTH {
                    encoder.encode(&PlayerView::new(side, &p.tracker, &own[side.index()]), &tables, &mut obs);
                    black_box(&obs);
                }
            }
        })
    });
    let mut masks = vec![false; 2 * MaskLayout::Factored.width(&space)];
    group.bench_function("masks", |b| {
        b.iter(|| {
            for p in &points {
                w::factored_masks(&p.battle, &space, &mut masks);
                black_box(&masks);
            }
        })
    });
    group.finish();
}

fn pools(c: &mut Criterion) {
    let format = w::format(FORMAT);
    let n = 256;
    let mut group = c.benchmark_group("env-pool");
    group.throughput(Throughput::Elements(n as u64));
    group.sample_size(20);
    for threads in [1, 8] {
        let config = w::env_config(&format, n, 0, 8);
        let env = VecEnv::new(config, threads).unwrap();
        let actions = w::random_actions(&env);
        let mut out = env.buffers();
        env.reset(None, &mut out.as_out()).unwrap();
        group.bench_function(format!("vec/{threads}"), |b| b.iter(|| env.step(&actions, &mut out.as_out()).unwrap()));
        let pool = Pool::new(w::pool_config(&format, n, 0, false), threads).unwrap();
        group.bench_function(format!("raw/{threads}"), |b| {
            b.iter(|| {
                let actions = pool.sample_random(ActionMode::Slots).unwrap();
                pool.step(actions, ActionMode::Slots).unwrap()
            })
        });
    }
    group.finish();
}

criterion_group!(benches, phases, pools);
criterion_main!(benches);
