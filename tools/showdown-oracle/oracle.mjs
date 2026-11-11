#!/usr/bin/env node
// Runs one battle on the real Pokémon Showdown simulator and prints its
// raw protocol log (`|split|` blocks included). This is the reference ("oracle") that
// pikapro-conformance diffs pikapro against.
//
// Input (JSON, from the file given as argv[2] or stdin):
//   {
//     "formatid": "gen9customgame",          // any Showdown format id
//     "seed": "gen5,0001000200030004",       // gen5 LCG seed, 4 x 16-bit hex words
//     "teams": ["<export text>", "<export text>"],
//     "choices": ["p1 team 123456", "p2 team 123456", "p1 move 1", ...]
//   }
//
// A random battle's input can give `"teamSeeds": ["<PRNG seed>", "<PRNG
// seed>"]` instead of `teams`: each player then plays the team Showdown's
// generator draws from its seed (`>player p1 {"name":"p1","seed":...}`).
//
// With `"validate": "<formatid>"`, each team is first validated by Showdown's
// `TeamValidator` for that format and played as the validator leaves it, as
// a Showdown server does (`validateForme` starts a listed battle-only forme
// as the forme it changes from). A team it rejects is an error: its problems
// go to stderr and the exit status is 3.
//
// Output: the raw log Showdown's BattleStream sends in its `update` chunks,
// one protocol line per line of stdout, `|split|pN` blocks included.
// The omniscient, spectator and player logs are derived from it the way
// `extractChannelMessages` does.
// Choice errors (|error| lines sent to a player) go to stderr as
// "p1 |error|..." and make the process exit with status 2. A choice on
// which Showdown throws (a crash in the simulator) ends the battle there:
// the log up to the throw goes to stdout, "oracle: Showdown threw: ..." to
// stderr, and the exit status is 4.
//
// Set PIKAPRO_ORACLE_TRACE=1 to interleave every PRNG draw into the log as
// `|rng|random|<args>|<result>`, with the JS call site that made it. This is
// the main tool for finding where RNG consumption diverges.
//
// Set PIKAPRO_ORACLE_REQUESTS=1 to also interleave every request Showdown sends
// a player, as `|request|<side>|<JSON>` (the `sideupdate`'s JSON verbatim),
// after the log lines sent before it.
//
// Determinism notes:
//   * Genders are not pinned. A set without one gets its species'
//     fixed gender or a draw from the battle PRNG when its side is created,
//     which pikapro reproduces.

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { BattleStream, Teams, TeamValidator } = require('pokemon-showdown');

const input = JSON.parse(readFileSync(process.argv[2] ?? 0, 'utf8'));
const { formatid, seed, teams, teamSeeds, choices, validate } = input;
const sides = teamSeeds ?? teams;
if (!formatid || !seed || !Array.isArray(sides) || sides.length !== 2 || !Array.isArray(choices)) {
  console.error('oracle: expected {formatid, seed, teams: [a, b] or teamSeeds: [a, b], choices: [...]}');
  process.exit(64);
}

function packTeam(text) {
  const sets = Teams.import(text);
  if (!sets) throw new Error(`could not parse team:\n${text}`);
  if (validate) {
    // As the server's validator does (`team-validator-async.ts`): the sets
    // it mutated, packed again.
    const problems = new TeamValidator(validate).validateTeam(sets);
    if (problems) {
      console.error(`oracle: the ${validate} validator rejects a team:\n${problems.join('\n')}`);
      process.exit(3);
    }
  }
  return Teams.pack(sets);
}

// `noCatch`: an exception reaches the write that caused it, instead of
// the stream's error buffer, which drops the log not yet read.
const stream = new BattleStream({ debug: false, noCatch: true });
const log = [];
const errors = [];
const requests = !!process.env.PIKAPRO_ORACLE_REQUESTS;

const reader = (async () => {
  for await (const chunk of stream) {
    const [type, ...rest] = chunk.split('\n');
    if (type === 'update') {
      // The raw stream, `|split|pN` blocks included (the secret line,
      // then the public one, which may be empty). Every channel of
      // `getPlayerStreams` is derived from it with `extractChannelMessages`.
      log.push(...rest);
    } else if (type === 'sideupdate') {
      const [side, ...lines] = rest;
      for (const line of lines) {
        if (line.startsWith('|error|')) errors.push(`${side} ${line}`);
        // Requests go into the log where they were sent.
        else if (requests && line.startsWith('|request|')) log.push(`|request|${side}|${line.slice(9)}`);
      }
    }
  }
})();

stream.write(`>start ${JSON.stringify({ formatid, seed })}`);
if (process.env.PIKAPRO_ORACLE_TRACE) {
  const battle = stream.battle;
  const prng = battle.prng;
  const random = prng.random.bind(prng);
  prng.random = (...args) => {
    const result = random(...args);
    const site = new Error().stack.split('\n').slice(2, 6)
      .map(l => l.trim().replace(/^at /, '').replace(/\(.*\/dist\//, '(').replace(/:\d+\)$/, ')'))
      .join(' < ');
    battle.add('rng', 'random', args.join(','), String(result), site);
    return result;
  };
}
// A random battle's players can come with a seed for the format's team
// generator instead of a team (`Battle.getTeam`).
const player = (i) => (teamSeeds ? { name: `p${i + 1}`, seed: teamSeeds[i] } : { name: `p${i + 1}`, team: packTeam(teams[i]) });
stream.write(`>player p1 ${JSON.stringify(player(0))}`);
stream.write(`>player p2 ${JSON.stringify(player(1))}`);
let thrown = null;
for (const choice of choices) {
  try {
    stream.write(`>${choice}`);
  } catch (e) {
    thrown = e;
    // What the throwing write logged before the throw (`_write` sends it
    // only when the write returns).
    stream.battle?.sendUpdates();
    break;
  }
}
stream.writeEnd();
await reader;

process.stdout.write(log.join('\n') + '\n');
if (thrown) console.error(`oracle: Showdown threw: ${thrown.stack}`);
for (const e of errors) console.error(e);
// Not `process.exit()`: that can truncate stdout when it is a pipe.
process.exitCode = thrown ? 4 : errors.length ? 2 : 0;
