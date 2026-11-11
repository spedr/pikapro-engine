#!/usr/bin/env node
// Times Pokémon Showdown's own simulator: the Showdown side of the speed
// comparison in docs/python/performance.md ("The engine").
//
//   node bench.mjs --format championsvgc --team team.txt [--battles 500]
//                  [--warmup 20] [--seed 1] [--move 0.7] [--mega 0.6]
//
// It plays `--battles` battles one after another in this Node process, after
// `--warmup` untimed ones, with the `Runner` of Showdown's
// `sim/tools/runner.js` (what Showdown's `tools/simulate` uses): a
// `BattleStream`, the per-player streams, and a `RandomPlayerAI` on each
// side reading its requests and writing its choices as text. Both players
// get the team in `--team` (Showdown export format; pikapro's sample team
// for the format is
// `python -c "import pikapro; print(pikapro.sample_team('championsvgc'))"`).
//
// The random players keep the Runner's defaults unless `--move`/`--mega`
// are given: they pick a move with probability 0.7 and otherwise switch if
// they can (so they switch voluntarily about 30% of the time), and when a
// Mega Evolution (or Dynamax) is available they take it with probability 0.6
// (Tera only then, so never in Gen 9 without a Mega). Moves, targets and
// switches are uniform. Team preview is `default` (the team's order).
// pikapro's `bench` samples uniformly over every legal choice instead, so
// the two play different battles; compare turns per second, not battles.
//
// Turns are counted from the omniscient stream's `|turn|` lines. Output is
// one JSON line: format, battles, seconds, battles/s, turns/s, average turns.
//
// Formats (pikapro's name, then the Showdown format it runs):
//   championsvgc  gen9championsvgc2026regmc
//   championsbss  gen9championsbssregmc
//   gen9vgc       gen9vgc2025regi
//   gen9ou        gen9ou
//   gen9doubles   gen9doublescustomgame
//   gen9singles   gen9customgame
// A Showdown format id is accepted as is.
//
// Rerunning the README's comparison, on a quiet machine (1-minute load
// under 1.5), three runs per format, the median of each:
//
//   python -c "import pikapro, sys; sys.stdout.write(pikapro.sample_team('championsvgc'))" > /tmp/team.txt
//   node tools/showdown-oracle/bench.mjs --format championsvgc --team /tmp/team.txt
//   cargo bench-sim --format championsvgc --battles 20000 --threads 1 --events
//
// (10,000 battles for gen9doubles and gen9singles), then update the table
// and its `<!-- bench: -->` comment in docs/python/performance.md.

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { parseArgs } from 'node:util';

const require = createRequire(import.meta.url);
const { Runner } = require('pokemon-showdown/dist/sim/tools/runner.js');
const { Teams } = require('pokemon-showdown');

const FORMATS = {
  championsvgc: 'gen9championsvgc2026regmc',
  championsbss: 'gen9championsbssregmc',
  gen9vgc: 'gen9vgc2025regi',
  gen9ou: 'gen9ou',
  gen9doubles: 'gen9doublescustomgame',
  gen9singles: 'gen9customgame',
};

const { values: opts } = parseArgs({
  options: {
    format: { type: 'string', default: 'championsvgc' },
    team: { type: 'string' },
    battles: { type: 'string', default: '500' },
    warmup: { type: 'string', default: '20' },
    seed: { type: 'string', default: '1' },
    move: { type: 'string' },
    mega: { type: 'string' },
  },
});
if (!opts.team) {
  console.error('usage: node bench.mjs --format F --team FILE [--battles N] [--warmup N] [--seed N] [--move P] [--mega P]');
  process.exit(2);
}
const format = FORMATS[opts.format] ?? opts.format;
const team = Teams.pack(Teams.import(readFileSync(opts.team, 'utf8')));
const ai = { team };
if (opts.move !== undefined) ai.move = Number(opts.move);
if (opts.mega !== undefined) ai.mega = Number(opts.mega);

// Each battle gets its own Runner, seeded from `--seed` and the battle's
// number (a gen5 seed: four 16-bit words); the Runner derives the battle's
// and the players' seeds from it. The warm-up battles come after the timed
// ones in that numbering.
const seed = Number(opts.seed);
const runnerSeed = k => [seed >>> 16, seed, k >>> 16, k].map(w => w & 0xffff);

// With `output: true` the Runner logs every chunk of the omniscient stream;
// count the turns in them instead of printing them.
let turns = 0;
const log = console.log;
console.log = chunk => {
  turns += (String(chunk).match(/^\|turn\|/gm) ?? []).length;
};
async function play(from, n) {
  for (let k = from; k < from + n; k++) {
    await new Runner({ format, prng: runnerSeed(k), output: true, p1options: ai, p2options: ai }).run();
  }
}

const battles = Number(opts.battles);
await play(battles, Number(opts.warmup));
turns = 0;
const start = process.hrtime.bigint();
await play(0, battles);
const seconds = Number(process.hrtime.bigint() - start) / 1e9;
console.log = log;
const round = (x, d) => Math.round(x * 10 ** d) / 10 ** d;
console.log(JSON.stringify({
  format,
  battles,
  seconds: round(seconds, 3),
  battles_per_second: round(battles / seconds, 1),
  turns_per_second: Math.round(turns / seconds),
  avg_turns: round(turns / battles, 1),
}));
