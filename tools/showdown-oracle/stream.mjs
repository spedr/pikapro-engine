#!/usr/bin/env node
// Runs battle-stream scripts on Pokémon Showdown's own `BattleStream`,
// so pikapro-server's stream can be diffed against it message for
// message.
//
// Input (JSON, from the file given as argv[2] or stdin): an array of cases
//   { "writes": ["<chunk>", ...],          // each one `stream.write(chunk)`
//     "keepAlive": true, "replay": "spectator" | true,   // optional stream options
//     "channels": [["omniscient" | "p1" | "p2", "<chunk>"], ...] }  // optional
//
// Output (JSON, one entry per case):
//   { "messages": ["update\n...", "sideupdate\np1\n...", ...],
//     "error": "<message>" | null,       // an exception stops the case
//     "channels": { "omniscient": [...], "spectator": [...], "p1": [...], "p2": [...] } }
//
// `writes` go through `_write` exactly (each line run, then
// `sendUpdates`), with Showdown's exception caught rather than pushed, and
// what the stream pushes is read after each write. `channels` (when
// given) replays the case through `getPlayerStreams`: each chunk is
// written to that channel's stream (a player's gets `>pN ` prefixes), and
// every chunk each of the four channels reads is collected.
//
// Nothing is normalised here: `|t:|` times and `|debug|` lines are left to
// the caller.

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { BattleStream, getPlayerStreams } = require('pokemon-showdown');

const cases = JSON.parse(readFileSync(process.argv[2] ?? 0, 'utf8'));

function runWrites(writes, options) {
  const stream = new BattleStream(options);
  const messages = [];
  let error = null;
  for (const chunk of writes) {
    try {
      stream._writeLines(chunk);
    } catch (err) {
      error = err.message;
    }
    if (error === null && stream.battle) stream.battle.sendUpdates();
    messages.push(...stream.buf.splice(0));
    if (error !== null) break;
  }
  return { messages, error };
}

async function runChannels(chunks) {
  const stream = new BattleStream();
  const streams = getPlayerStreams(stream);
  const names = ['omniscient', 'spectator', 'p1', 'p2'];
  const out = {};
  const readers = names.map(async name => {
    out[name] = [];
    try {
      for await (const chunk of streams[name]) out[name].push(chunk);
    } catch (err) {
      out[name].push(`!!! ${err.message}`);
    }
  });
  for (const [name, chunk] of chunks) {
    await streams[name].write(chunk);
    // Let the router hand this write's output to the channels.
    await new Promise(resolve => setImmediate(resolve));
  }
  await streams.omniscient.writeEnd();
  await Promise.all(readers);
  return out;
}

const results = [];
for (const c of cases) {
  const result = runWrites(c.writes, { keepAlive: !!c.keepAlive, replay: c.replay || false });
  if (c.channels) result.channels = await runChannels(c.channels);
  results.push(result);
}
process.stdout.write(JSON.stringify(results) + '\n');
