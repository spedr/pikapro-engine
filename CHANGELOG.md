# Changelog

Changes to the Rust crates. Before 1.0, a minor version may break the
API, and breaking changes are listed under "Changed (breaking)". The Python
package has its own [changelog](crates/pikapro-py/CHANGELOG.md).

## 0.1.0 (2026-11-10)

The first release, checked against Pokémon Showdown's master at commit
`a5df827`. The formats are `championsvgc`, `championsbss`, `gen9vgc`,
`gen9ou` (Smogon's `[Gen 9] OU`), Showdown's Gen 9 random battles
`gen9randombattle` and `gen9randomdoublesbattle`, and the custom games
`gen9singles` and `gen9doubles`. The crates are `pikapro`, `pikapro-data`,
`pikapro-protocol`, `pikapro-validate`, `pikapro-teams`, `pikapro-view`,
`pikapro-rl`, `pikapro-server` and `pikapro-cli`.
The minimum supported Rust version is 1.88.
