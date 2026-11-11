# view-oracle: poke-env's side of the view differential

The player view (`pikapro-view`) must know what a Showdown client knows, no
more and no less. One check diffs it against
poke-env's battle tracker, fed the same channel lines and requests. This
directory is poke-env's half:

- `gen_decisions.py` plays fuzzed battles on `pikapro.Battle` (random teams,
  random legal choices) and writes **decision records**: per side and per
  request, the channel lines since the last one and the request.
- `pokeenv_view.py` feeds each stream of decision records to poke-env's
  own `Battle`/`DoubleBattle` through `pikapro.compat.poke_env.Feeder`, and
  writes poke-env's tracked state after each record, normalized to plain
  JSON (**state records**).
- [`SCHEMA.md`](SCHEMA.md) defines both formats. pikapro-conformance's
  `view_dump` writes the same decision records plus our own view projected
  onto the state schema, and `diff/diff.py` compares the two.

## Running

Needs the `pikapro` package with poke-env (see `crates/pikapro-py/README.md`):

```sh
cd crates/pikapro-py
uv venv
. .venv/bin/activate
uv pip install maturin 'poke-env>=0.16,<0.17'
maturin develop --uv
cd ../../tools/view-oracle

python gen_decisions.py --format championsvgc --battles 100 --seed 4100000 -o decisions.jsonl
python pokeenv_view.py decisions.jsonl -o poke-env.jsonl

# or piped
python gen_decisions.py --format gen9singles --battles 10 | python pokeenv_view.py - > poke-env.jsonl
```

`gen_decisions.py` options: `--format` (any preset, `pikapro.formats()`),
`--battles`, `--seed` (battle `k` uses `seed + k`), `--teams sets|uniform`
(the `pikapro.RandomTeams` mode).

`pokeenv_view.py` prints a summary to stderr (streams, records, streams
poke-env failed on, poke-env's warnings by message) and exits 1 if
poke-env raised on any stream. A failed stream gets one record with
`error` and the offending `line`.
