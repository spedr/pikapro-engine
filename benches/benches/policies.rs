//! Criterion benchmarks of the baseline policies' choice time:
//! `cargo bench -p pikapro-bench --bench policies [-- FILTER]`.
//!
//! `policy/<format>/<policy>`: one `Policy::choose` per side asked to
//! decide, over the decision points of random self-play battles of the
//! sample teams (the view and overlays built beforehand), per decision.
//! `policy/<format>/engine` is the engine step at the same points, per
//! decision point (both sides' random choices submitted, events on), for
//! scale.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use pikapro::{BattleRng, LegalActions, Seed, SideId};
use pikapro_bench as w;
use pikapro_rl::baselines::{MaxBasePowerPolicy, SimpleHeuristicsPolicy};
use pikapro_rl::policy::{Policy, RandomPolicy};
use pikapro_view::{OwnOverlay, PlayerView, PublicTracker};

fn policies(c: &mut Criterion) {
    for id in ["championsvgc", "gen9singles"] {
        let format = w::format(id);
        let points = w::view_points(&format, &w::teams(&format), 4_730_000, 256);
        // Every (point, side) with a decision to make.
        let decisions: Vec<(PublicTracker, OwnOverlay, LegalActions, SideId)> = points
            .iter()
            .flat_map(|p| {
                let own = w::overlays(&p.battle);
                SideId::BOTH
                    .into_iter()
                    .filter(|&s| !p.battle.legal_actions(s).is_waiting())
                    .map(move |s| (p.tracker, own[s.index()], *p.battle.legal_actions(s), s))
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut group = c.benchmark_group(format!("policy/{id}"));
        group.throughput(Throughput::Elements(decisions.len() as u64));
        let entrants: [(&str, &dyn Policy); 3] =
            [("random", &RandomPolicy), ("maxbp", &MaxBasePowerPolicy), ("heuristic", &SimpleHeuristicsPolicy::new())];
        for (name, policy) in entrants {
            group.bench_function(name, |b| {
                let mut rng = BattleRng::new(Seed::fast(1));
                b.iter(|| {
                    for (tracker, own, legal, side) in &decisions {
                        black_box(policy.choose(&PlayerView::new(*side, tracker, own), legal, &mut rng));
                    }
                })
            });
        }
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
        group.finish();
    }
}

criterion_group!(benches, policies);
criterion_main!(benches);
