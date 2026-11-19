# Changelog

Changes to the `pikapro` Python package. Before 1.0, a minor version may
break the API, and breaking changes are listed under "Changed (breaking)",
with what to do. The Rust
crates have their own [changelog](../../CHANGELOG.md).

## 0.2.0 (2026-11-19)

### Changed

- Checked against Pokémon Showdown's master at commit `5b88c47`
  (2026-11-18), up from `a5df827`. Battles follow Showdown's changes
  since then, among them:
  - A Pokémon about to switch out after U-turn, Volt Switch, Eject Button
    or Emergency Exit gets Showdown's pre-switch update when the switch
    request is made, which can draw on a speed tie, and a fainted Pokémon
    keeps its status when its side has nothing left to switch in (issue
    #2).
  - In doubles, a switch during the turn can't bring in the Pokémon the
    partner's switch already chose; the legal actions follow.
  - No Retreat traps with a `trapped` volatile and, if the Pokémon is
    already trapped or a Ghost type, doesn't stop it being used again
    (in Champions it does, silently).
  - Struggle recoils off a hit that dealt no damage; Future Sight no
    longer removes Endure; Baneful Bunker's poison counts as its move's
    (Poison Puppeteer confuses); Emergency Exit doesn't activate when
    recoil knocks its holder out.
  - Champions: Sheer Force no longer stops Eject Button, Red Card,
    Pickpocket, Berserk, Anger Shell, Color Change or Emergency Exit;
    Last Resort is disabled until the other moves were used; a Mega Sol
    Electro Shot charges in rain; Encore and Curse follow Showdown's
    fixes.
  - Rule lines and validation messages say "Pokemon".
- `validate` follows Showdown's validator: `gen9ou` reports OHKO Clause
  and Sleep Moves Clause moves as banned moves, and chainbreeding goes
  through up to three fathers.
- The random-battle sets follow Showdown's October balance patch, and
  the team pools for Champions and Gen 9 doubles are drawn again, so
  generated teams for a seed differ from 0.1.0's.
- A `VecEnv` step is about 7% faster and battles simulate a few percent
  faster (2–6% measured); the speed-ups change no results, only the
  Showdown changes above do. The `basic` observation is about a quarter
  cheaper to encode, with the same output for the same battle.

### Fixed

- A Pokémon able to both Terastallize and Mega Evolve in doubles (Power
  Construct Zygarde holding Zygardite, once Zygarde-Complete) no longer
  stops the battle with a `RuntimeError` (`StackVec capacity 32
  exceeded`) when its legal actions are listed.
- `python -m pikapro.showdown --log-json` no longer drops the "challenge
  interrupted", "challenge too soon" and "from_user ignored" records
  (their `user` field clashed with the record's own). The other player
  is now `opponent` in the two challenge records and `from_user` in the
  third.

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
