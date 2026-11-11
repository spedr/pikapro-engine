# Changelog

Changes to the `pikapro` Python package. Before 1.0, a minor version may
break the API, and breaking changes are listed under "Changed (breaking)",
with what to do. The Rust
crates have their own [changelog](../../CHANGELOG.md).

## 0.1.0 (2026-11-10)

The first release, checked against Pokémon Showdown's master at commit
`a5df827`. Formats: `championsvgc`, `championsbss`, `gen9vgc`,
`gen9ou` (Smogon's `[Gen 9] OU`), `gen9randombattle`, `gen9randomdoublesbattle`, `gen9singles` and `gen9doubles`. Modules: `pikapro`, `pikapro.rl`,
`pikapro.showdown` and `pikapro.compat.poke_env`. Wheels for CPython 3.10
and later (abi3) on Linux, macOS and Windows; an sdist that builds with
Rust 1.88 or later. `ACTION_SPACE_VERSION` is 1.

### Known limitations

- No free-threaded (3.13t, 3.14t) wheels.
- The poke-env bridge follows one poke-env minor version (0.16).
