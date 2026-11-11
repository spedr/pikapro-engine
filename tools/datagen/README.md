# datagen

Generates `pikapro-data`'s tables from Pokémon Showdown's data, using the
Showdown version pinned by `tools/showdown-oracle` (so the data and the
conformance oracle always agree). The generated files' headers name it as
the package version plus the pinned commit (`0.11.11+a5df827`).

```sh
(cd tools/showdown-oracle && npm ci)       # once
node tools/datagen/datagen.mjs             # regenerate crates/pikapro-data/src/{gen9,champions}.rs, legality/ and pikapro-teams' sets/
node tools/datagen/datagen.mjs --check     # fail if the files are out of date
```

## What it produces

- `gen9.rs`: full tables (`SPECIES_TABLE`, `MOVE_TABLE`, `ABILITY_TABLE`,
  `ITEM_TABLE`), the `DEX`, and ID constant modules (`species::GARCHOMP`,
  `moves::U_TURN`, ...). Constants are the display name in UPPER_SNAKE case,
  accents and punctuation dropped (`Farfetch’d` → `FARFETCHD`).
- `champions.rs`: the Gen 9 tables with only the fields Showdown's
  `champions` mod changes, patched in at compile time. IDs are shared.
- A test in `gen9.rs` that checks the hand-written type chart against
  Showdown's.
- `legality/gen9.rs`, `legality/champions.rs` (cargo feature `legality`,
  used by `pikapro-validate`): what Showdown's team validator reads per
  species, move and item. The generator fails on fields the validator
  port doesn't model. Learnset entries of moves outside the ID space are
  left out.
- `crates/pikapro-teams/src/sets/{gen9,gen9doubles,champions}.rs`: sets
  for `pikapro-teams`, drawn from Showdown's own random-battle generators
  from a fixed seed and kept if Showdown's validator accepts them in the
  target format.

Both dexes share one ID space: an entry is included if it is standard,
`Past`, `Future` or `Unobtainable` in either dex. CAP, LGPE and Custom entries,
Gigantamax formes, Z-Moves and Max Moves are left out. Each row records its
availability in that dex as `nonstandard`.

## Rules

- **Never edit the generated files.** Change the generator and rerun it.
- **Fail loudly.** Unknown types, flags, statuses, targets, cross-references
  and constant-name collisions stop generation. Extend the schema in
  `crates/pikapro-data/src/dex.rs` and the generator together.
- **Nothing is dropped silently.** Move fields and callbacks the schema does
  not represent are listed in `MoveData::unmodelled`; the engine uses
  `MoveData::is_data_only()` to know which moves are fully described.
- The output is deterministic: running the generator twice gives identical
  files.

Format rule tables are not generated: `pikapro-validate` transcribes the
presets' resolved tables (`node tools/showdown-oracle/validator.mjs --rules
<format>` prints them).
