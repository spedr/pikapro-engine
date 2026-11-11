#!/usr/bin/env node
// Runs one battle on the real Pokémon Showdown simulator and prints what
// each player is sent: Showdown's `getPlayerStreams` channels, verbatim
// (the battle's lines as `extractChannelMessages` gives that player, its
// `|request|` and `|error|` lines where they were sent). pikapro's channel
// parser reads these and must build the same view as pikapro's typed
// path (`cargo channel-conformance --showdown`).
//
// Input: the same JSON as oracle.mjs ({formatid, seed, teams, choices}).
// Output (stdout): one JSON object, {"p1": [chunk, ...], "p2": [...]},
// each chunk one message of that player's stream (lines joined by "\n"),
// in the order sent.

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { BattleStream, getPlayerStreams, Teams } = require('pokemon-showdown');

const input = JSON.parse(readFileSync(process.argv[2] ?? 0, 'utf8'));
const { formatid, seed, teams, choices } = input;
if (!formatid || !seed || !Array.isArray(teams) || teams.length !== 2 || !Array.isArray(choices)) {
  console.error('players: expected {formatid, seed, teams: [a, b], choices: [...]}');
  process.exit(64);
}

function packTeam(text) {
  const sets = Teams.import(text);
  if (!sets) throw new Error(`could not parse team:\n${text}`);
  return Teams.pack(sets);
}

const streams = getPlayerStreams(new BattleStream({ debug: false }));
const out = { p1: [], p2: [] };
const readers = ['p1', 'p2'].map(side => (async () => {
  for await (const chunk of streams[side]) out[side].push(chunk);
})());

streams.omniscient.write(`>start ${JSON.stringify({ formatid, seed })}`);
streams.omniscient.write(`>player p1 ${JSON.stringify({ name: 'p1', team: packTeam(teams[0]) })}`);
streams.omniscient.write(`>player p2 ${JSON.stringify({ name: 'p2', team: packTeam(teams[1]) })}`);
for (const choice of choices) streams.omniscient.write(`>${choice}`);
streams.omniscient.writeEnd();
await Promise.all(readers);

process.stdout.write(JSON.stringify(out) + '\n');
