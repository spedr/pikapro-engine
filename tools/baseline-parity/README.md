# baseline-parity: our baseline players against poke-env's

`pikapro_rl::baselines` ports poke-env 0.16.1's `MaxBasePowerPlayer` and
`SimpleHeuristicsPlayer` onto the player view. This directory checks the
ports decision by decision against poke-env's own players:

1. `pikapro-baseline-dump` (Rust, `benches/src/bin/`) plays battles with a
   driver policy and, at every decision, records what our two policies
   choose from the side's `PlayerView`, and the choice it submitted.
2. `parity.py` replays each battle on `pikapro.Battle`, keeps one poke-env
   battle object per side and player in sync through
   `pikapro.compat.poke_env.Feeder` (poke-env's own parsing of the side's
   channel lines and requests), asks poke-env's players at the same
   decisions, and compares the two choices as Showdown reads them.

## Running

Needs the `pikapro` package with poke-env (see `crates/pikapro-py/README.md`).

```sh
cargo run --release -p pikapro-bench --bin pikapro-baseline-dump -- \
    --format championsvgc --battles 200 --seed 4710000 -o decisions.jsonl
python tools/baseline-parity/parity.py decisions.jsonl --mismatches mm.jsonl [--details]
```

`pikapro-baseline-dump` options: `--format`, `--battles`, `--seed` (battle
`k` has seed `seed + k`), `--teams sets|uniform` (random legal teams),
`--driver mix|random|maxbp|heuristic` (the choices submitted; `mix`, the
default, plays side 1 as our heuristics and side 2 as our max base power,
each with a random choice a quarter of the time). Views show foe HP as
the format's channel does (exact in the custom games and gen9vgc,
percentages in championsvgc), so both players know the same things.

`parity.py` writes a JSON summary per format and player; with
`--mismatches`, every disagreement (with `--details`, also the request
and poke-env's tracked state, `tools/view-oracle/SCHEMA.md`).

## How decisions are compared

* **Randomness.** poke-env draws from Python's `random` (a random target,
  a random switch, `choose_random_move`). Each decision is asked twice,
  with `random` forced to its lowest and to its highest outcomes:
  * equal answers: a **deterministic** decision; ours must be the same;
  * equal once move targets are dropped: **target-random** (max base power
    in doubles aims at a random foe); ours must be the same without
    targets;
  * otherwise **random**, only counted (`random-agree` just says ours was
    one of the two extremes).
* **Mega Evolution** is our one deliberate deviation (poke-env never Mega
  Evolves): choices are compared without it, and `mega-deviation` counts
  the deterministic decisions that differ only by it.
* **Rejections.** A poke-env choice Showdown rejects (a switch out of a
  hidden trap) is answered as the in-process driver answers it (the
  `|error|`, the updated request) and poke-env is asked again; ours makes
  the same retry internally. Both are compared on the choice finally
  accepted; `rejected-first` counts poke-env's first-try rejections.
* Team preview is random for both and not compared.

## Expected differences

Known causes of deterministic disagreement:

* In championsvgc, poke-env reads Gen 9 move data, where our players read
  the format's dex (Mountain Gale, Psyshield Bash and others have other
  base powers there).
* poke-env keeps a Mega-Evolved foe under its base forme's species, so it
  likely scores the base forme's stats.
* After `|replace|` reveals a Zoroark disguised as its ally, poke-env
  loses the real ally.

The port follows poke-env where it counts a fainted foe's revealed Tera
type as Terastallized, and where `PokemonType.damage_multiplier` returns 1
for a first type of `???`.
