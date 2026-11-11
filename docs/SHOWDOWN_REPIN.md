# Moving the Showdown pin

The conformance oracle and the generated game data come from one
Showdown commit, pinned in `tools/showdown-oracle/package.json` (see
[CONFORMANCE.md](CONFORMANCE.md)). Moving it is a deliberate change,
made on one branch, and merged only when every check below passes.

## Steps

1. **Bump the pin.** Change the commit in
   `tools/showdown-oracle/package.json` and run `npm install` there.
   `postinstall` builds Showdown's `dist/`. The lockfile's `resolved`
   must stay `git+https://` (npm may write `git+ssh://`). If Showdown
   needs a newer Node, update `engines`.
2. **Regenerate the data**: `node tools/datagen/datagen.mjs`. The
   generated headers name the new commit. A new entry inside a table
   renumbers the later IDs of that table; dex IDs aren't stable across
   versions.
3. **Follow format changes.** If a preset's Showdown format changed (a
   new regulation), update every place that names it: grep for the old
   format id (the validation formats, the format mappings in
   pikapro-protocol, pikapro-server and pikapro-showdown-server, the
   Python package's format lists, the docs).
4. **Re-bless the goldens keyed on dex IDs**, only for an ID change:
   - pikapro-teams' golden teams:
     `cargo run --release -p pikapro-conformance --bin teams_fuzz -- --teams 3 --record crates/pikapro-teams/tests/golden.txt`;
   - the Python API snapshot: `PIKAPRO_UPDATE_API=1 pytest tests/test_api_snapshot.py`
     in `crates/pikapro-py`.

   Review each diff: only ID values should change. A changed layout
   fingerprint means a layout changed, and its version must be bumped.

   The channel fixtures' digests cover the `basic` observation. If a
   data change moves them (a move's power, type or PP), take them again;
   their lines stay:
   `PIKAPRO_BLESS=1 cargo test -p pikapro-conformance --test channel`.
5. **Re-check every fixture on the new pin.** Each command replays the
   stored scenarios on Showdown, rewrites the ones that pass and reports
   the rest:
   - `cargo conformance --rerecord crates/pikapro-conformance/fixtures`;
   - `cargo view-conformance --rerecord crates/pikapro-conformance/fixtures/view`;
   - `cargo run --release -p pikapro-server --example stream_fuzz -- --rerecord crates/pikapro-server/tests/transcripts`;
   - `cargo run --release -p pikapro-conformance --bin validate_fuzz -- --check FILE --write`
     for each case file in `crates/pikapro-validate/tests/` (`--formes`
     for `formes.txt`).
6. **Fix the divergences.** Run `cargo test --workspace`, then fuzz every
   format on fresh seeds: `cargo conformance --trace`, and the view,
   channel, stream, validator and team fuzzers. Port each change from
   Showdown's source as in CONFORMANCE.md's "Workflow for fixing a
   divergence", then re-check the fixtures that failed.
7. **Replace the fixtures that can't replay.** A fixture whose recorded
   choice the new pin rejects (a move's PP changed, Metronome's list
   changed) can't be rewritten. Replace each with a fixture freshly
   recorded from a battle that covers the same mechanic (a targeted
   generator, `--target`, or a scripted fixture), and list every
   replacement in the commit message: the old fixture, why it can't
   replay, and the new one. Never delete a fixture without a
   replacement, and never edit one by hand.
8. **Gate.** Run the full gate (`tools/full-gate.sh`) and a conformance
   round per format on fresh seeds. Update the pin where the docs name it (the
   README, CONFORMANCE.md, FORMATS.md, `tools/datagen/README.md`,
   `tools/showdown-oracle/README.md`).
