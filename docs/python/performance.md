# Performance

A `VecEnv` step runs in Rust, on worker threads, with the GIL released,
and writes into buffers allocated once. The engine is most of the cost.
You control how much work each call carries, how many threads share it,
and how much you encode.

## Measure first

```sh
python crates/pikapro-py/bench/bench_env.py --format championsvgc --threads 1 16 --n 1024 4096
```

It reports steps/s, battles/s and the Python wrapper's share of
random-vs-random battles.

Measured with it on championsvgc, with the sample teams, both players
observed and factored masks. The `VecEnv` figures include encoding the
`basic` observation:

| mode | threads | n | steps/s | battles/s | Python wrapper |
|---|---|---|---|---|---|
| `VecEnv` | 1 | 1,024 | 219,000 | 10,000 | 0.2% <!-- bench: performance.md VecEnv 1 thread n=1024; 2026-11-09, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-py/bench/bench_env.py --format championsvgc --threads 1 --n 1024`, median of 3 --> |
| `VecEnv` | 12 | 1,024 | 1,510,000 | 69,100 | 0.7% <!-- bench: performance.md VecEnv 12 threads n=1024; 2026-11-09, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-py/bench/bench_env.py --format championsvgc --threads 12 --n 1024`, median of 3 --> |
| `VecEnv` | 12 | 4,096 | 1,670,000 | 76,600 | 0.5% <!-- bench: performance.md VecEnv 12 threads n=4096; 2026-11-09, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-py/bench/bench_env.py --format championsvgc --threads 12 --n 4096`, median of 3 --> |
| `BattlePool.play_random` (whole battles in Rust, no views) | 12 | 4,096 | 3,110,000 | 166,000 <!-- bench: performance.md play_random 12 threads n=4096; 2026-11-09, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-py/bench/bench_env.py --format championsvgc --threads 12 --n 4096`, median of 3 --> | |
| poke-env players, in-process driver | 1 | 1 | | 359 <!-- bench: performance.md poke-env in-process; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `python -m pikapro.compat.poke_env.bench --format championsvgc --battles 500`, median of 3 --> | |

A step is one decision round of one battle; a championsvgc battle takes
about 22 of them here. For the engine alone, see
[the engine](#the-engine).

## Threads

- `threads=` defaults to every core. If torch runs on the CPU, split the
  cores (`torch.set_num_threads(k)` and `threads=cores - k`);
  oversubscribing slows both.
- A step returns when the slowest shard finishes. Battles are split into
  `shards_per_thread` (8) shards per thread that threads claim as they
  go; raise it if your battles are uneven.
- Idle workers spin for `spin_us` (50 µs) before they sleep. Raise it if
  your policy's inference between steps is short, lower it to leave the
  cores to other work.
- With both players observed, one thread keeps about 50% of the raw
  engine's speed (60% with a built-in opponent). Twelve threads keep
  38–44% of the 12-thread engine's. On this machine more battles per
  call helped: `n` = 16,384 was 14% faster than 1,024.
  <!-- bench: performance.md share of engine speed, 1 and 12 threads; 2026-11-09, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-py/bench/bench_env.py --format championsvgc --threads 1 --n 1024` and `--threads 12 --n 1024 4096 16384`, `cargo run --release -p pikapro-bench --bin pikapro-env-split -- --threads 1 --n 256` against `cargo bench-sim` in the same session (20,300 and 180,000 battles/s), medians of 3 -->

## Battles per call

`n` is how much work a call carries. Too small and the per-call costs
dominate; too large and your policy's batch and the rollout memory grow.
The Python wrapper measured 0.2–0.7% of a step at `n ≥ 1,024`
<!-- bench: Python wrapper share; 2026-11-09, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-py/bench/bench_env.py --format championsvgc --threads 1 --n 1024` and `--threads 12 --n 1024 4096 16384` (the wrapper column), medians of 3 -->.
`bench_env.py --n` tells you where your machine stops gaining.

## Buffers and copies

- A step allocates nothing. `out.obs`, `out.masks` and the rest are the
  environment's own arrays, overwritten by the next step: **copy what you
  keep**, and don't copy what you don't.
- `env.bind(obs=..., masks=...)` makes steps write straight into your
  arrays, for instance numpy views of pinned torch tensors, which a
  non-blocking copy then moves to the GPU. `env.buffer_specs` maps each
  buffer's name to its shape and dtype. The observation is the
  `float32` buffer `obs`:

  ```python
  env = rl.VecEnv("championsvgc", n=4096)
  obs = torch.empty(env.buffer_specs["obs"][0], dtype=torch.float32).pin_memory()
  env.bind(obs=obs.numpy())
  out = env.step(actions)                 # writes into obs
  x = obs.to("cuda", non_blocking=True)
  ```

- An `int64`, C-contiguous action array of the full `(n, 2, per)` shape
  goes to Rust without a copy.
- `GymVectorEnv(..., copy=False)` returns views instead of copies (the
  default copies, as Gymnasium expects).

## What to encode

Encoding runs on the worker threads.

- A built-in opponent isn't encoded unless `observe_builtin=True`.
- `BattleView` and `embed_all` run Python for every battle at every
  step. An [encoder in Rust](encoders.md#your-own-encoder-in-rust) runs
  on the worker threads, as `basic` does.

## Masks

Factored masks (the default) have `active * slot_size + preview_size`
bools per player: 454 in VGC, 94 in gen9doubles. Joint masks have one
bool per joint choice, enumerated each step: 2,569 in VGC (5.7 times
more) and 2,209 in gen9doubles (23.5 times more). Use factored masks
with an autoregressive or per-slot policy, joint masks when your
algorithm needs one `Discrete` action.

## Beyond one process

For more parallelism than one machine's cores, run one `VecEnv` per
process (or per machine) with different base seeds; neighbouring bases
don't repeat each other's episodes.

## The simulator from Python

Random self-play with events off (`crates/pikapro-py/bench/bench_pool.py`),
each mode's speed as a share of `pikapro bench` in the same run, on
1 / 8 threads, with 64 battles per thread:

| mode | gen9vgc 1 / 8 | championsvgc 1 / 8 | gen9singles 1 / 8 |
|---|---|---|---|
| `pool.play_random` (the whole run in one call) | 101% / 95% | 100% / 93% | 101% / 97% <!-- bench: performance.md play_random share; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-py/bench/bench_pool.py --format F --threads 1` (20,000 battles) and `--threads 8 --battles 100000`, F = gen9vgc, championsvgc, gen9singles, median of 3 --> |
| `pool.step` + `sample_random` (a Python call per step) | 89% / 72% | 87% / 70% | 82% / 56% <!-- bench: performance.md pool step share; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-py/bench/bench_pool.py --format F --threads 1` (20,000 battles) and `--threads 8 --battles 100000`, F = gen9vgc, championsvgc, gen9singles, median of 3 --> |
| `pool.step` + numpy policy (`sample_slots`) | 39% / 12% | 38% / 11% | 66% / 37% <!-- bench: performance.md numpy policy share; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-py/bench/bench_pool.py --format F --threads 1` (20,000 battles) and `--threads 8 --battles 100000`, F = gen9vgc, championsvgc, gen9singles, median of 3 --> |
| `Battle` loop (a Python call per choice, no pool) | 57% | 58% | 58% <!-- bench: performance.md Battle loop share; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-py/bench/bench_pool.py --format F --threads 1` (20,000 battles) and `--threads 8 --battles 100000`, F = gen9vgc, championsvgc, gen9singles, median of 3 --> |

## The engine

The engine alone, random against random, with the event log off
(`pikapro bench`). The last column compares turns per second with
Showdown's simulator on one thread, event log on.

| format | 1 thread | 12 threads | against Showdown |
|---|---|---|---|
| championsvgc | ~20,800 battles/s <!-- bench: engine championsvgc 1 thread; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `cargo bench-sim --format championsvgc --battles 20000 --threads 1`, median of 3 --> | ~189,000 battles/s <!-- bench: engine championsvgc 12 threads; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `cargo bench-sim --format championsvgc --battles 200000 --threads 12`, median of 3 --> | ~295× |
| gen9vgc | ~24,000 battles/s <!-- bench: engine gen9vgc 1 thread; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `cargo bench-sim --format gen9vgc --battles 20000 --threads 1`, median of 3 --> | ~212,000 battles/s <!-- bench: engine gen9vgc 12 threads; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `cargo bench-sim --format gen9vgc --battles 200000 --threads 12`, median of 3 --> | ~274× |
| gen9doubles | ~13,700 battles/s <!-- bench: engine gen9doubles 1 thread; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `cargo bench-sim --format gen9doubles --battles 10000 --threads 1`, median of 3 --> | ~123,000 battles/s <!-- bench: engine gen9doubles 12 threads; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `cargo bench-sim --format gen9doubles --battles 100000 --threads 12`, median of 3 --> | ~274× |
| gen9singles | ~9,780 battles/s <!-- bench: engine gen9singles 1 thread; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `cargo bench-sim --format gen9singles --battles 10000 --threads 1`, median of 3 --> | ~86,800 battles/s <!-- bench: engine gen9singles 12 threads; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `cargo bench-sim --format gen9singles --battles 100000 --threads 12`, median of 3 --> | ~254× |

<!-- bench: Showdown simulator 1 thread; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, Node 22.18, `node tools/showdown-oracle/bench.mjs --format F --team <pikapro's sample team for F>` (the Runner of Showdown's `sim/tools/runner.js`, its default RandomPlayerAI options, 500 battles in sequence after 20 warm-up battles, turns counted from the `|turn|` lines), against `cargo bench-sim --format F --battles N --threads 1 --events` (N = 20,000, or 10,000 for gen9doubles and gen9singles), median of 3 -->

## Against Showdown's simulator

One thread, in turns per second (the two sides' random players don't
play the same battles):

| format (Showdown's) | Showdown | pikapro, `--events` | ratio |
|---|---|---|---|
| championsvgc (`gen9championsvgc2026regmc`) | ~1,030 turns/s | ~305,500 turns/s | ~295× |
| gen9vgc (`gen9vgc2025regi`) | ~980 turns/s | ~269,900 turns/s | ~274× |
| gen9doubles (`gen9doublescustomgame`) | ~1,010 turns/s | ~277,500 turns/s | ~274× |
| gen9singles (`gen9customgame`) | ~3,100 turns/s | ~785,500 turns/s | ~254× |

<!-- bench: Showdown simulator 1 thread; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, Node 22.18, `node tools/showdown-oracle/bench.mjs --format F --team <pikapro's sample team for F>` (the Runner of Showdown's `sim/tools/runner.js`, its default RandomPlayerAI options, 500 battles in sequence after 20 warm-up battles, turns counted from the `|turn|` lines), against `cargo bench-sim --format F --battles N --threads 1 --events` (N = 20,000, or 10,000 for gen9doubles and gen9singles), median of 3 -->

## poke-env players

RandomPlayer against RandomPlayer, in battles and turns per second of
wall time, over a websocket to `pikapro-showdown-server` and in process
(`pikapro.compat.poke_env`). poke-env's own parsing and choosing is most
of the cost:

| format | websocket, 1 pair × 1 battle | in-process, 1 process | websocket, 4 pairs × 8 | in-process, 4 processes |
|---|---|---|---|---|
| `championsvgc` | 174 b/s, 1,560 t/s | 359 b/s, 3,020 t/s | 732 b/s, 6,580 t/s | 1,250 b/s, 10,400 t/s <!-- bench: performance.md throughput championsvgc; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, websocket: `crates/pikapro-showdown-server/tests/python/throughput.py --format gen9championsvgc2026regmc --battles 300 [--pairs 4 --concurrent 8]` against `pikapro-showdown-server` (release-server, `--max-lines-per-second 0`); in-process: `python -m pikapro.compat.poke_env.bench --format championsvgc --battles 500` and `--battles 2000 --processes 4`; median of 3 --> |
| `gen9vgc` | 139 b/s, 1,640 t/s | 327 b/s, 2,920 t/s | 607 b/s, 7,090 t/s | 1,130 b/s, 10,500 t/s <!-- bench: performance.md throughput gen9vgc; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, websocket: `crates/pikapro-showdown-server/tests/python/throughput.py --format gen9vgc2025regi --battles 300 [--pairs 4 --concurrent 8]` against `pikapro-showdown-server` (release-server, `--max-lines-per-second 0`); in-process: `python -m pikapro.compat.poke_env.bench --format gen9vgc --battles 500` and `--battles 2000 --processes 4`; median of 3 --> |
| `gen9doubles` | 76.7 b/s, 1,580 t/s | 182 b/s, 2,940 t/s | 350 b/s, 7,220 t/s | 666 b/s, 10,500 t/s <!-- bench: performance.md throughput gen9doubles; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, websocket: `crates/pikapro-showdown-server/tests/python/throughput.py --format gen9doublescustomgame --battles 300 [--pairs 4 --concurrent 8]` against `pikapro-showdown-server` (release-server, `--max-lines-per-second 0`); in-process: `python -m pikapro.compat.poke_env.bench --format gen9doubles --battles 500` and `--battles 2000 --processes 4`; median of 3 --> |
| `gen9singles` | 46.2 b/s, 3,570 t/s | 161 b/s, 7,960 t/s | 231 b/s, 18,000 t/s | 592 b/s, 29,300 t/s <!-- bench: performance.md throughput gen9singles; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, websocket: `crates/pikapro-showdown-server/tests/python/throughput.py --format gen9customgame --battles 300 [--pairs 4 --concurrent 8]` against `pikapro-showdown-server` (release-server, `--max-lines-per-second 0`); in-process: `python -m pikapro.compat.poke_env.bench --format gen9singles --battles 500` and `--battles 2000 --processes 4`; median of 3 --> |

The same websocket runs against Showdown's own server, in battles per
second:

| format | 1 pair, 1 battle at a time: Showdown → pikapro-showdown-server | 4 pairs × 8 concurrent: Showdown → pikapro-showdown-server |
|---|---|---|
| `gen9championsvgc2026regmc` | 58.1 → 174 (3.0×) <!-- bench: showdown-server championsvgc 1 pair; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-showdown-server/tests/python/throughput.py --format gen9championsvgc2026regmc --battles 300`, Showdown's server (a5df827) then `pikapro-showdown-server` (release-server, `--max-lines-per-second 0`), median of 3 --> | 84.1 → 732 (8.7×) <!-- bench: showdown-server championsvgc 4x8; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-showdown-server/tests/python/throughput.py --format gen9championsvgc2026regmc --battles 300 --pairs 4 --concurrent 8`, Showdown's server (a5df827) then `pikapro-showdown-server` (release-server, `--max-lines-per-second 0`), median of 3 --> |
| `gen9vgc2025regi` | 45.9 → 139 (3.0×) <!-- bench: showdown-server gen9vgc 1 pair; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-showdown-server/tests/python/throughput.py --format gen9vgc2025regi --battles 300`, Showdown's server (a5df827) then `pikapro-showdown-server` (release-server, `--max-lines-per-second 0`), median of 3 --> | 63.2 → 607 (9.6×) <!-- bench: showdown-server gen9vgc 4x8; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-showdown-server/tests/python/throughput.py --format gen9vgc2025regi --battles 300 --pairs 4 --concurrent 8`, Showdown's server (a5df827) then `pikapro-showdown-server` (release-server, `--max-lines-per-second 0`), median of 3 --> |
| `gen9doublescustomgame` | 26.6 → 76.7 (2.9×) <!-- bench: showdown-server gen9doubles 1 pair; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-showdown-server/tests/python/throughput.py --format gen9doublescustomgame --battles 300`, Showdown's server (a5df827) then `pikapro-showdown-server` (release-server, `--max-lines-per-second 0`), median of 3 --> | 34.3 → 350 (10.2×) <!-- bench: showdown-server gen9doubles 4x8; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-showdown-server/tests/python/throughput.py --format gen9doublescustomgame --battles 300 --pairs 4 --concurrent 8`, Showdown's server (a5df827) then `pikapro-showdown-server` (release-server, `--max-lines-per-second 0`), median of 3 --> |
| `gen9customgame` | 15.6 → 46.2 (3.0×) <!-- bench: showdown-server gen9singles 1 pair; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-showdown-server/tests/python/throughput.py --format gen9customgame --battles 300`, Showdown's server (a5df827) then `pikapro-showdown-server` (release-server, `--max-lines-per-second 0`), median of 3 --> | 24.2 → 231 (9.6×) <!-- bench: showdown-server gen9singles 4x8; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, `crates/pikapro-showdown-server/tests/python/throughput.py --format gen9customgame --battles 300 --pairs 4 --concurrent 8`, Showdown's server (a5df827) then `pikapro-showdown-server` (release-server, `--max-lines-per-second 0`), median of 3 --> |

## How these were measured

Every figure is the median of three runs on an Apple M4 Pro (12 cores,
24 GB of RAM, macOS). The `<!-- bench: ... -->` comment next to each figure
in the Markdown source gives its command.

The Showdown comparison counts turns because the two sides' random players
don't play the same battles. Showdown's turns include writing and parsing
its text protocol, and pikapro's record typed events.

The figures are from a plain release build, as a build from source gets.
The PyPI wheels use profile-guided optimisation, which made `VecEnv` steps
16–18% faster <!-- bench: performance.md PGO gain, VecEnv; 2026-11-09, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, a PGO wheel (`tools/pgo_wheel.py`, then `pip wheel`) against a plain one, Python 3.13, `crates/pikapro-py/bench/bench_env.py --format championsvgc --threads 1 --n 1024` and `--threads 12 --n 1024 4096 16384`, medians of 3 interleaved rounds --> and the pool and
`Battle` loops 14–39% faster <!-- bench: performance.md PGO gain; 2026-11-08, Apple M4 Pro (12 cores, 24 GB) macOS, the 0.1.0 release tree, a PGO wheel (`tools/pgo_wheel.py`, then `pip wheel`) against a plain one, Python 3.13, `bench_pool.py --threads 1` in gen9vgc, championsvgc and gen9singles, medians of 3 interleaved rounds -->.
