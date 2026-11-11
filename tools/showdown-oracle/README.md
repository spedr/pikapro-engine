# showdown-oracle

Runs one battle on the real [Pokémon Showdown](https://github.com/smogon/pokemon-showdown)
simulator and prints its raw protocol log (the lines of Showdown's `update`
chunks, `|split|pN` blocks included, from which every player's, the
spectators' and the omniscient log are derived). `pikapro-conformance` uses it
as the reference implementation. See `docs/CONFORMANCE.md`.

```sh
npm ci                                   # installs the pinned pokemon-showdown
node oracle.mjs scenario.json            # or pipe JSON on stdin
PIKAPRO_ORACLE_TRACE=1 node oracle.mjs scenario.json   # + every PRNG draw with call site
PIKAPRO_ORACLE_REQUESTS=1 node oracle.mjs scenario.json  # + every |request| sent to a player
node teams.mjs teams.json                # Teams.import/export/pack/unpack round trips
node choices.mjs probes.json             # side.choose verdicts for probe strings
node stream.mjs cases.json               # battle-stream scripts on Showdown's BattleStream
node players.mjs scenario.json           # what each player is sent (getPlayerStreams)
node validator.mjs teams.json            # Showdown's team validator: the problems per team
node bench.mjs --format championsvgc --team team.txt   # Showdown's simulator speed (docs/python/performance.md, "The engine")
```

`teams.mjs` and `choices.mjs` are driven by pikapro-conformance
(`tests/teams.rs`, `cargo conformance --choices N`); their input formats
are described at the top of each script. `stream.mjs` runs
battle-stream command scripts on Showdown's own `BattleStream` and
`getPlayerStreams` for pikapro-server's differential fuzzer
(`cargo run --release -p pikapro-server --example stream_fuzz`).
`players.mjs` gives each player's channel for `cargo channel-conformance
--showdown`, and `validator.mjs` is the reference pikapro-validate
is diffed against (`validate_fuzz`, `teams_fuzz`).

Input:

```json
{
  "formatid": "gen9customgame",
  "seed": "gen5,0001000200030004",
  "teams": ["<Showdown export text>", "<Showdown export text>"],
  "choices": ["p1 team 123456", "p2 team 123456", "p1 move 1", "p2 switch 2"]
}
```

Output is the raw log on stdout: every line Showdown writes, not only
the omniscient view. Rejected choices are printed to stderr and the
exit status is 2. If Showdown throws on a choice, the log stops there,
stderr starts with `oracle: Showdown threw:` and the exit status is 4.

The Showdown version is pinned exactly in `package.json` because conformance
fixtures depend on it: a GitHub master commit
(`a5df8274e85b0889bf2a9b3422a08b39732374fc`, 2026-09-22), since Champions
Reg M-C isn't in an npm release. A git dependency ships TypeScript only, so
`postinstall` runs Showdown's `build` to produce `dist/`; Showdown master
needs Node 22.18 or newer. Upgrading is a deliberate change: bump the commit
(the lockfile's `resolved` must stay `git+https`),
rerun `cargo conformance` for every format, and re-record fixtures if
Showdown's behaviour changed. `docs/SHOWDOWN_REPIN.md` has the whole
procedure.
