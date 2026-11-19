# Changelog

Changes to the Rust crates. Before 1.0, a minor version may break the
API, and breaking changes are listed under "Changed (breaking)". The Python
package has its own [changelog](crates/pikapro-py/CHANGELOG.md).

## 0.2.0 (2026-11-19)

### Changed

- Checked against Pokémon Showdown's master at commit `5b88c47`
  (2026-11-18), up from `a5df827`. Battles follow Showdown's changes
  since then:
  - A Pokémon about to switch out after U-turn, Volt Switch, Eject Button
    or Emergency Exit gets Showdown's pre-switch update when the switch
    request is made, which can draw on a speed tie (issue #2).
  - A fainted Pokémon keeps its status when its side has nothing left to
    switch in (issue #2).
  - U-turn and the like don't flag their user to switch when nothing can
    come in.
  - In doubles, a switch during the turn (U-turn, Eject Button, Eject
    Pack, Emergency Exit, Roar) can't bring in the Pokémon the partner's
    switch already chose; Eject Pack and Eject Button don't activate when
    that was the only one left. Legal actions and choice checking follow.
  - No Retreat traps with a `trapped` volatile (`-activate|...|trapped`).
    A Pokémon already trapped, or a Ghost type, gets the boosts but not
    No Retreat's volatile, so it can use it again; in Champions an
    already trapped Pokémon gets the volatile silently and can't.
  - Struggle recoils off a hit that dealt no damage (Disguise, Ice Face).
  - Emergency Exit and Wimp Out don't activate when recoil or crash
    damage knocks their holder out.
  - Future Sight and Doom Desire no longer remove Protect and Endure, so
    Endure holds.
  - Baneful Bunker and Burning Bulwark poison or burn with their move as
    the cause: Poison Puppeteer confuses, and Safeguard and Flower Veil
    say they block it.
  - Destiny Bond does nothing if the attacker has fainted too.
  - Slow Start and Supreme Overlord write no end line when they are not
    in effect; a Pokémon that faints disguised by Illusion reverts its
    forme without a `detailschange` line.
  - Using Wish or a future attack on a slot whose earlier one will never
    resolve gives a hint saying so; the hint about future attacks
    starts at turn 255 (was 253).
  - Champions: Sheer Force no longer stops Eject Button, Red Card,
    Pickpocket, Berserk, Anger Shell, Color Change or Emergency Exit.
  - Champions: Last Resort is disabled until the Pokémon has used its
    other moves.
  - Champions: a Mega Sol Pokémon's Electro Shot charges in rain.
  - Champions: Encore changes the target's queued move without keeping
    its old priority or Quash order.
  - Champions: a non-Ghost's Curse aims at its user through its move
    target, and a Ghost's Curse can be redirected by Follow Me.
  - Rule lines, the Sleep Clause Mod hint and validation messages say
    "Pokemon" (`|rule|Species Clause: Limit one of each Pokemon`).
  - `gen9ou` validation reports OHKO Clause and Sleep Moves Clause moves
    as banned moves (`Breloom's move Spore is banned by Sleep Moves
    Clause.`).
  - The battle stream's input log writes a locked move or Struggle as the
    player sent it.
- Data follows Showdown: learnsets, Champions' Aura Guard (now
  standard), Darkranite and Heatranite (now Future in Champions), and
  the random-battle sets (Showdown's October balance patch). The ID
  tables are unchanged. The random-battle generator gives Weather Ball
  and Terrain Pulse their weather's and terrain's type and draws
  Squawkabilly's plumage in doubles. `pikapro-teams`' set pools for
  Champions and Gen 9 doubles are drawn again, so `Mode::Sets` teams
  differ from 0.1.0's.
- The team validator chainbreeds through up to three fathers, and a
  Shedinja's move from Ninjask needs the level Ninjask learns it at, as
  Showdown does.
- Battles simulate a few percent faster (2–6% measured, by format and
  teams) and `Battle::new` does about a tenth less work. The speed-ups
  change no results; only the Showdown changes above do.
- `pikapro-rl`'s `basic` observation is about a quarter cheaper to encode
  (same output for the same battle), and a `VecEnv` step about 7% faster.

### Changed (breaking)

- `Hint` has two new variants, `FutureMoveStuck` and `WishStuck`. `Hint`
  isn't `#[non_exhaustive]`, so an exhaustive `match` on it needs arms
  for them.
- `MoveRecord` has a new public field, `base_used_moves`: a transformed
  Pokémon's own move slots' used marks, which it gets back when the
  Transform ends. A struct literal of `MoveRecord` needs it (or
  `..Default::default()`).

### Added

- `Battle::queued_switch_ins`: the Pokémon that switches already queued
  this turn will bring in.

### Fixed

- A Pokémon able to both Terastallize and Mega Evolve in doubles (Power
  Construct Zygarde holding Zygardite, once Zygarde-Complete) no longer
  overflows its legal-action list (`StackVec capacity 32 exceeded` in
  `Battle::submit`). `MAX_SLOT_ACTIONS` is now 40, the most any slot can
  list.

## 0.1.0 (2026-11-10)

The first release, checked against Pokémon Showdown's master at commit
`a5df827`. The formats are `championsvgc`, `championsbss`, `gen9vgc`,
`gen9ou` (Smogon's `[Gen 9] OU`), Showdown's Gen 9 random battles
`gen9randombattle` and `gen9randomdoublesbattle`, and the custom games
`gen9singles` and `gen9doubles`. The crates are `pikapro`, `pikapro-data`,
`pikapro-protocol`, `pikapro-validate`, `pikapro-teams`, `pikapro-view`,
`pikapro-rl`, `pikapro-server` and `pikapro-cli`.
The minimum supported Rust version is 1.88.
