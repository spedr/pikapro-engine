//! Criterion benchmark of the `basic` encoder, per decision point (both
//! sides encoded), over the views of a few championsvgc battles:
//! `cargo bench -p pikapro-bench --bench encode`.
//!
//! It times the path the vector environment runs, with each player's own
//! team ([`pikapro_bench::ViewPoint::teams`]), as the environment passes
//! it: [`pikapro_bench::encode_points`], the loop `cargo bench-insns`
//! counts.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use pikapro_bench as w;
use pikapro_rl::tables::Tables;

const FORMAT: &str = "championsvgc";

fn encoders(c: &mut Criterion) {
    let format = w::format(FORMAT);
    // 64 points: the views stay in cache, as in a step, where the encoder
    // reads a tracker and overlays just written.
    let points = w::view_points(&format, &w::teams(&format), 4_400_000, 64);
    let overlays: Vec<_> = points.iter().map(|p| w::overlays(&p.battle)).collect();
    let tables = Tables::for_format(&format);
    let mut group = c.benchmark_group("encode");
    group.throughput(Throughput::Elements(points.len() as u64));
    group.bench_function("basic", |b| b.iter(|| black_box(w::encode_points(&points, &overlays, &tables))));
    group.finish();
}

criterion_group!(benches, encoders);
criterion_main!(benches);
