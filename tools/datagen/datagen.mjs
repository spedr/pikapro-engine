#!/usr/bin/env node
// Generates pikapro-data's tables from Pokémon Showdown's data.
//
//   node tools/datagen/datagen.mjs          write the generated files
//   node tools/datagen/datagen.mjs --check  exit 1 if any is out of date
//
// It writes crates/pikapro-data/src/{gen9,champions}.rs (the dexes),
// crates/pikapro-data/src/legality/{gen9,champions}.rs (the legality
// tables) and crates/pikapro-teams/src/sets/*.rs (the random sets).
//
// Uses the Showdown version pinned by tools/showdown-oracle (run `npm ci`
// there first), so the data and the conformance oracle always agree.
//
// Both dexes share one ID space: an entry is included if it is standard,
// Past, Future or Unobtainable in either dex (CAP, LGPE and Custom entries,
// Gigantamax formes, Z-Moves and Max Moves are left out). gen9.rs holds the
// full tables; champions.rs patches only the fields that differ.

import { readFileSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..', '..');
const oracle = join(here, '..', 'showdown-oracle');
const require = createRequire(join(oracle, 'package.json'));
const { Dex, toID } = require('pokemon-showdown');
// The pin is a GitHub commit (tools/showdown-oracle/package.json), and a
// commit's package.json keeps the last npm release's version, so the version
// names the installed commit: `0.11.11+a5df827` (semver build metadata).
function pinnedShowdownVersion() {
  const { version } = require('pokemon-showdown/package.json');
  const installed = JSON.parse(readFileSync(join(oracle, 'node_modules', '.package-lock.json'), 'utf8'));
  const resolved = installed.packages['node_modules/pokemon-showdown']?.resolved ?? '';
  const commit = /#([0-9a-f]{40})$/.exec(resolved);
  return commit ? `${version}+${commit[1].slice(0, 7)}` : version;
}
const showdownVersion = pinnedShowdownVersion();

const DEXES = { gen9: Dex.forGen(9), champions: Dex.mod('champions') };
const ALLOWED = new Set([undefined, null, 'Past', 'Future', 'Unobtainable']);
const NONSTANDARD = { Past: 'Some(Past)', Future: 'Some(Future)', Unobtainable: 'Some(Unobtainable)' };
const TYPES = ['Normal', 'Fire', 'Water', 'Electric', 'Grass', 'Ice', 'Fighting', 'Poison', 'Ground',
  'Flying', 'Psychic', 'Bug', 'Rock', 'Ghost', 'Dragon', 'Dark', 'Steel', 'Fairy'];
const STATUS = { brn: 'Burn', par: 'Paralysis', slp: 'Sleep', frz: 'Freeze', psn: 'Poison', tox: 'Toxic' };
const TARGETS = { normal: 'Normal', adjacentFoe: 'AdjacentFoe', adjacentAlly: 'AdjacentAlly',
  adjacentAllyOrSelf: 'AdjacentAllyOrSelf', any: 'Any', allAdjacentFoes: 'AllAdjacentFoes',
  allAdjacent: 'AllAdjacent', self: 'User', allies: 'Allies', allySide: 'AllySide', foeSide: 'FoeSide',
  all: 'All', randomNormal: 'RandomNormal', allyTeam: 'AllyTeam', scripted: 'Scripted' };
// Showdown flag name -> `MoveFlags` constant.
const MOVE_FLAGS = { allyanim: 'ALLY_ANIM', bypasssub: 'BYPASS_SUB', bite: 'BITE', bullet: 'BULLET',
  cantusetwice: 'CANT_USE_TWICE', charge: 'CHARGE', contact: 'CONTACT', dance: 'DANCE', defrost: 'DEFROST',
  distance: 'DISTANCE', failcopycat: 'FAIL_COPYCAT', failencore: 'FAIL_ENCORE', failinstruct: 'FAIL_INSTRUCT',
  failmefirst: 'FAIL_ME_FIRST', failmimic: 'FAIL_MIMIC', futuremove: 'FUTURE_MOVE', gravity: 'GRAVITY',
  heal: 'HEAL', metronome: 'METRONOME', minimize: 'MINIMIZE', mirror: 'MIRROR', mustpressure: 'MUST_PRESSURE',
  noassist: 'NO_ASSIST', nonsky: 'NON_SKY', noparentalbond: 'NO_PARENTAL_BOND', nosketch: 'NO_SKETCH',
  nosleeptalk: 'NO_SLEEP_TALK', pledgecombo: 'PLEDGE_COMBO', powder: 'POWDER', protect: 'PROTECT',
  pulse: 'PULSE', punch: 'PUNCH', recharge: 'RECHARGE', reflectable: 'REFLECTABLE', slicing: 'SLICING',
  snatch: 'SNATCH', sound: 'SOUND', wind: 'WIND' };
const ABILITY_FLAGS = { breakable: 'BREAKABLE', cantsuppress: 'CANT_SUPPRESS', failroleplay: 'FAIL_ROLE_PLAY',
  failskillswap: 'FAIL_SKILL_SWAP', noentrain: 'NO_ENTRAIN', noreceiver: 'NO_RECEIVER', notrace: 'NO_TRACE',
  notransform: 'NO_TRANSFORM' };
const STAT_ORDER = ['atk', 'def', 'spa', 'spd', 'spe', 'accuracy', 'evasion'];
// Showdown events abilities and items handle, spelled as the `Event`
// variants in crates/pikapro-data/src/dex.rs (same order).
const EVENTS = ['Accuracy', 'AfterBoost', 'AfterEachBoost', 'AfterFaint', 'AfterMega', 'AfterMove',
  'AfterMoveSecondary', 'AfterMoveSecondarySelf', 'AfterSetStatus', 'AfterSubDamage', 'AfterTerastallization',
  'AfterUseItem', 'Attract', 'BasePower', 'BeforeMove', 'BeforeSwitchIn', 'ChangeBoost', 'ChargeMove', 'CheckShow',
  'CriticalHit', 'Damage', 'DamagingHit', 'DeductPP', 'DisableMove', 'DragOut', 'Eat', 'EatItem', 'Effectiveness',
  'EmergencyExit', 'End', 'Faint', 'Flinch', 'FractionalPriority', 'Hit', 'HitProtect', 'Immunity',
  'Invulnerability', 'MaybeTrapPokemon', 'ModifyAccuracy', 'ModifyAtk', 'ModifyBoost', 'ModifyCritRatio',
  'ModifyDamage', 'ModifyDef', 'ModifyMove', 'ModifyPriority', 'ModifySTAB', 'ModifySecondaries', 'ModifySpA',
  'ModifySpD', 'ModifySpe', 'ModifyType', 'ModifyWeight', 'NegateImmunity', 'PrepareHit', 'PseudoWeatherChange',
  'RedirectTarget', 'Residual', 'SetAbility', 'SetStatus', 'SetWeather', 'SideConditionStart', 'Start', 'SwitchIn',
  'SwitchOut', 'TakeItem', 'TerrainChange', 'TrapPokemon', 'TryAddVolatile', 'TryBoost', 'TryEatItem', 'TryHeal',
  'TryHit', 'TryHitSide', 'TryMove', 'TryPrimaryHit', 'Update', 'Use', 'UseItem', 'Weather', 'WeatherChange',
  'WeatherModifyDamage'];
const EVENT_SET = new Set(EVENTS);
const EVENT_PREFIXES = ['Source', 'Ally', 'Foe', 'Any'];
// `on*` keys that are data, not callbacks (read through their own fields).
const DATA_CALLBACKS = new Set(['onPlate', 'onMemory', 'onDrive']);

// Move fields the schema represents, and pure metadata to ignore. Anything
// else (including every callback) is listed in `unmodelled`.
const MOVE_FIELDS = new Set(['num', 'name', 'accuracy', 'basePower', 'category', 'pp', 'priority', 'flags',
  'target', 'type', 'critRatio', 'multihit', 'drain', 'recoil', 'heal', 'status', 'volatileStatus', 'boosts',
  'self', 'secondary', 'secondaries', 'selfSwitch', 'forceSwitch', 'ohko', 'selfdestruct', 'sideCondition',
  'slotCondition', 'weather', 'terrain', 'pseudoWeather', 'isNonstandard']);
const MOVE_METADATA = new Set(['contestType', 'zMove', 'maxMove', 'desc', 'shortDesc', 'gen', 'tags']);
const SECONDARY_FIELDS = new Set(['chance', 'status', 'volatileStatus', 'boosts', 'self']);

function fail(message) {
  console.error(`datagen: ${message}`);
  process.exit(1);
}

// ---------------------------------------------------------------- ID space

function included(kind, entry) {
  if (!entry?.exists || !ALLOWED.has(entry.isNonstandard)) return false;
  if (kind === 'species') return entry.forme !== 'Gmax' && !entry.forme?.endsWith('-Gmax');
  if (kind === 'moves') return !entry.isZ && !entry.isMax;
  return true;
}

// Ordered IDs per kind: the base dex's data order, then mod-only entries.
function idSpace(kind, table) {
  const ids = [];
  const seen = new Set();
  for (const dex of Object.values(DEXES)) {
    for (const id of Object.keys(dex.data[table])) {
      if (seen.has(id)) continue;
      if (Object.values(DEXES).some((d) => included(kind, d[kind].get(id)))) {
        ids.push(id);
        seen.add(id);
      }
    }
  }
  return ids;
}

// The Gen 9 random-battle data (`data/random-battles/gen9/`): the
// generator's sets, by species ID in Showdown's key order.
const RANDOM_BATTLE_DIR = join(dirname(require.resolve('pokemon-showdown/package.json')), 'dist', 'data', 'random-battles', 'gen9');
const RANDOM_BATTLE_TABLES = {
  singles: JSON.parse(readFileSync(join(RANDOM_BATTLE_DIR, 'sets.json'), 'utf8')),
  doubles: JSON.parse(readFileSync(join(RANDOM_BATTLE_DIR, 'doubles-sets.json'), 'utf8')),
};

// Cosmetic formes Showdown makes up from a species' `cosmeticFormes` when
// asked for them (`Sawsbuck-Summer`: `dex.species.get` copies Sawsbuck)
// have no Pokedex entry. Those the Gen 9 random-battle generator can draw
// (`getForme` samples a species' cosmetic formes) are added after every
// other species, so the other IDs stay as they were.
function drawnCosmeticFormes(ids) {
  const dex = DEXES.gen9;
  const seen = new Set(ids);
  const added = [];
  for (const table of Object.values(RANDOM_BATTLE_TABLES)) {
    for (const key of Object.keys(table)) {
      const species = dex.species.get(key);
      if (!species.exists) fail(`random-battle species ${key} does not exist`);
      if (typeof species.battleOnly === 'string') continue;
      for (const forme of species.cosmeticFormes ?? []) {
        const id = toID(forme);
        if (seen.has(id)) continue;
        if (!included('species', dex.species.get(id))) fail(`cosmetic forme ${forme} is not standard`);
        seen.add(id);
        added.push(id);
      }
    }
  }
  return added;
}

const SPECIES_IDS = idSpace('species', 'Pokedex');
const ADDED_COSMETIC_FORMES = drawnCosmeticFormes(SPECIES_IDS);
const IDS = {
  species: [...SPECIES_IDS, ...ADDED_COSMETIC_FORMES],
  moves: idSpace('moves', 'Moves'),
  abilities: idSpace('abilities', 'Abilities'),
  items: idSpace('items', 'Items'),
};
if (IDS.abilities[0] !== 'noability') fail('"No Ability" must be ability 0');
const INDEX = Object.fromEntries(
  Object.entries(IDS).map(([kind, ids]) => [kind, new Map(ids.map((id, i) => [id, i]))]),
);

function ref(kind, nameOrId, ctx) {
  const id = toID(nameOrId);
  const index = INDEX[kind].get(id);
  if (index === undefined) fail(`${ctx}: ${kind} "${nameOrId}" is not in the ID space`);
  return index;
}

// ------------------------------------------------------------ rust helpers

const str = (s) => JSON.stringify(s);
const opt = (v, f = (x) => x) => (v === undefined || v === null ? 'None' : `Some(${f(v)})`);
const frac = ([n, d]) => `Fraction { numerator: ${n}, denominator: ${d} }`;
// Showdown applies and announces boosts in key order (Shell Smash: def, spd,
// atk, spa, spe), so a non-canonical order is recorded.
const boosts = (b) => {
  const stages = `[${STAT_ORDER.map((s) => b[s] ?? 0).join(', ')}]`;
  for (const k of Object.keys(b)) if (!STAT_ORDER.includes(k)) fail(`unknown boost ${k}`);
  const keys = Object.keys(b).map((s) => STAT_ORDER.indexOf(s));
  if (keys.every((s, i) => i === 0 || keys[i - 1] < s)) return `B::new(${stages})`;
  const order = [...keys, ...[...STAT_ORDER.keys()].filter((s) => !keys.includes(s))];
  return `B::ordered(${stages}, [${order.join(', ')}])`;
};
const strs = (list) => (list.length ? `&[${list.map(str).join(', ')}]` : '&[]');

function type(t, ctx) {
  if (!TYPES.includes(t)) fail(`${ctx}: unknown type ${t}`);
  return t;
}

function status(s, ctx) {
  if (!STATUS[s]) fail(`${ctx}: unknown status ${s}`);
  return `St::${STATUS[s]}`;
}

/** UPPER_SNAKE constant from a display name ("Farfetch’d" -> FARFETCHD). */
function constName(name) {
  const c = name.normalize('NFD').replace(/[̀-ͯ]/g, '').replace(/['’.:%]/g, '')
    .replace(/[^A-Za-z0-9]+/g, '_').replace(/^_|_$/g, '').toUpperCase();
  if (!/^[A-Z]/.test(c)) fail(`cannot make a constant from "${name}"`);
  return c;
}

// -------------------------------------------------------------------- rows
// Each row is a list of [field, rust expression] so dexes can be diffed.

function speciesRow(dex, id) {
  const s = dex.species.get(id);
  const ctx = `species ${s.name}`;
  const t = s.types.map((x) => type(x, ctx));
  const a = s.abilities;
  const ab = (n) => opt(n, (x) => ref('abilities', x, ctx));
  let gender;
  if (s.gender === 'M') gender = 'GenderRule::Male';
  else if (s.gender === 'F') gender = 'GenderRule::Female';
  else if (s.gender === 'N') gender = 'GenderRule::Genderless';
  else gender = `GenderRule::Ratio { male_per_mille: ${Math.round((s.genderRatio?.M ?? 0.5) * 1000)} }`;
  const required = [...(s.requiredItems ?? (s.requiredItem ? [s.requiredItem] : []))]
    .map((i) => `ItemId(${ref('items', i, ctx)})`);
  const st = s.baseStats;
  return [
    ['name', str(s.name)],
    ['num', s.num],
    ['types', t.length === 1 ? `TypeSet::mono(${t[0]})` : `TypeSet::dual(${t[0]}, ${t[1]})`],
    ['base_stats', `Stats::new(${st.hp}, ${st.atk}, ${st.def}, ${st.spa}, ${st.spd}, ${st.spe})`],
    ['abilities', `slots(${ref('abilities', a[0], ctx)}, ${ab(a[1])}, ${ab(a.H)}, ${ab(a.S)})`],
    ['weight_hg', Math.round(s.weightkg * 10)],
    ['gender', gender],
    ['base_species', `SpeciesId(${ref('species', s.baseSpecies, ctx)})`],
    ['forme', str(s.forme ?? '')],
    ['required_items', required.length ? `&[${required.join(', ')}]` : '&[]'],
    ['battle_only', String(!!s.battleOnly)],
    ['required_tera_type', opt(s.requiredTeraType, (x) => (x === 'Stellar' ? 'TeraType::Stellar' : `TeraType::Type(${type(x, ctx)})`))],
    ['max_hp', opt(s.maxHP)],
    ['nfe', String(!!s.nfe)],
    ['paradox', String((s.tags || []).includes('Paradox'))],
    ['nonstandard', NONSTANDARD[s.isNonstandard] ?? 'None'],
  ];
}

function secondaryExpr(sec, ctx) {
  const unmodelled = Object.keys(sec).filter((k) => !SECONDARY_FIELDS.has(k) || typeof sec[k] === 'function');
  let selfBoosts = 'None';
  if (sec.self) {
    const extra = Object.keys(sec.self).filter((k) => k !== 'boosts' || typeof sec.self[k] === 'function');
    if (extra.length) unmodelled.push('self');
    if (sec.self.boosts) selfBoosts = `Some(${boosts(sec.self.boosts)})`;
  }
  return `Secondary { chance: ${sec.chance ?? 100}, status: ${opt(sec.status, (x) => status(x, ctx))}, `
    + `volatile_status: ${opt(sec.volatileStatus, str)}, boosts: ${opt(sec.boosts, boosts)}, `
    + `self_boosts: ${selfBoosts}, unmodelled: ${strs(unmodelled)} }`;
}

function moveRow(dex, id) {
  const raw = dex.data.Moves[id];
  const m = dex.moves.get(id);
  const ctx = `move ${m.name}`;
  const unmodelled = Object.keys(raw)
    .filter((k) => typeof raw[k] === 'function' || (!MOVE_FIELDS.has(k) && !MOVE_METADATA.has(k)))
    .sort();
  const flags = Object.keys(m.flags).sort().map((f) => {
    if (!MOVE_FLAGS[f]) fail(`${ctx}: unknown flag ${f}`);
    return `F::${MOVE_FLAGS[f]}`;
  });
  let selfBoosts = 'None';
  let selfVolatile = 'None';
  if (raw.self) {
    const extra = Object.keys(raw.self).filter((k) => !['boosts', 'volatileStatus'].includes(k) || typeof raw.self[k] === 'function');
    if (extra.length && !unmodelled.includes('self')) unmodelled.push('self');
    if (raw.self.boosts) selfBoosts = `Some(${boosts(raw.self.boosts)})`;
    if (raw.self.volatileStatus) selfVolatile = `Some(${str(raw.self.volatileStatus)})`;
  }
  if (typeof raw.ohko === 'string') unmodelled.push('ohko');
  if (raw.selfdestruct && raw.selfdestruct !== 'always') unmodelled.push('selfdestruct');
  const secondaries = (m.secondaries ?? []).map((s) => secondaryExpr(s, ctx));
  const multihit = Array.isArray(m.multihit) ? m.multihit : m.multihit ? [m.multihit, m.multihit] : null;
  const selfSwitch = { true: 'Switch', copyvolatile: 'CopyVolatile', shedtail: 'ShedTail' }[String(m.selfSwitch)];
  if (m.selfSwitch && !selfSwitch) fail(`${ctx}: unknown selfSwitch ${m.selfSwitch}`);
  return [
    ['name', str(m.name)],
    ['num', m.num],
    ['ty', type(m.type, ctx)],
    ['category', m.category],
    ['base_power', m.basePower],
    ['accuracy', m.accuracy === true ? 'None' : `Some(${m.accuracy})`],
    ['pp', m.pp],
    ['priority', m.priority],
    ['target', `T::${TARGETS[m.target] ?? fail(`${ctx}: unknown target ${m.target}`)}`],
    ['flags', flags.length ? `F::of(&[${flags.join(', ')}])` : 'F::NONE'],
    ['crit_ratio', m.critRatio ?? 1],
    ['multihit', multihit ? `Some((${multihit[0]}, ${multihit[1]}))` : 'None'],
    ['drain', opt(m.drain, frac)],
    ['recoil', opt(m.recoil, frac)],
    ['heal', opt(m.heal, frac)],
    ['status', opt(m.status, (x) => status(x, ctx))],
    ['volatile_status', opt(m.volatileStatus, str)],
    ['boosts', opt(m.boosts, boosts)],
    ['self_boosts', selfBoosts],
    ['self_volatile', selfVolatile],
    ['secondaries', secondaries.length ? `&[${secondaries.join(', ')}]` : '&[]'],
    ['self_switch', selfSwitch ? `Some(SelfSwitch::${selfSwitch})` : 'None'],
    ['force_switch', String(!!m.forceSwitch)],
    ['ohko', String(!!m.ohko)],
    ['self_destruct', String(!!m.selfdestruct)],
    ['side_condition', opt(m.sideCondition, str)],
    ['slot_condition', opt(m.slotCondition, str)],
    ['weather', opt(m.weather, str)],
    ['terrain', opt(m.terrain, str)],
    ['pseudo_weather', opt(m.pseudoWeather, str)],
    ['nonstandard', NONSTANDARD[m.isNonstandard] ?? 'None'],
    ['unmodelled', strs(unmodelled)],
  ];
}

/**
 * The event callbacks of an ability or item, with the ordering data
 * Showdown's `resolvePriority` reads (`on<Callback>Order`, `...Priority`,
 * `...SubOrder`; 0 counts as absent, as there). Also the `SwitchIn` handler
 * `getCallback` makes of `onStart` when there is neither `onSwitchIn` nor
 * `onAnySwitchIn`.
 */
function eventHandlers(entry, ctx) {
  const has = (k) => entry[k] !== undefined && entry[k] !== null;
  const ordering = (callback) => {
    let h = `H::new(E::${callback.event}, P::${callback.prefix})`;
    const order = entry[`${callback.key}Order`] || 0;
    const priority = entry[`${callback.key}Priority`] || 0;
    const subOrder = entry[`${callback.key}SubOrder`] || 0;
    for (const [what, v, min, max] of [['order', order, 0, 255], ['priority', priority, -128, 127], ['subOrder', subOrder, 0, 255]]) {
      if (!Number.isInteger(v) || v < min || v > max) fail(`${ctx}: ${callback.key} ${what} ${v} out of range`);
    }
    if (order) h += `.order(${order})`;
    if (priority) h += `.priority(${priority})`;
    if (subOrder) h += `.sub_order(${subOrder})`;
    return h;
  };
  const out = [];
  const events = new Set();
  const prefixed = new Set();
  for (const key of Object.keys(entry)) {
    if (!/^on[A-Z]/.test(key) || !has(key) || DATA_CALLBACKS.has(key)) continue;
    // An ordering key of a callback the entry has (or of the `onStart`
    // fallback below).
    const base = key.replace(/(SubOrder|Priority|Order)$/, '');
    if (base !== key && (has(base) || base === 'onSwitchIn')) continue;
    let event = key.slice(2);
    let prefix = 'On';
    for (const p of EVENT_PREFIXES) {
      if (event.startsWith(p) && EVENT_SET.has(event.slice(p.length))) {
        prefix = p;
        event = event.slice(p.length);
        break;
      }
    }
    if (!EVENT_SET.has(event)) fail(`${ctx}: unknown event callback ${key}`);
    out.push(ordering({ key, event, prefix }));
    events.add(event);
    if (prefix !== 'On') prefixed.add(event);
  }
  if (has('onStart') && !has('onSwitchIn') && !has('onAnySwitchIn')) {
    out.push(`${ordering({ key: 'onSwitchIn', event: 'SwitchIn', prefix: 'On' })}.via_start()`);
    events.add('SwitchIn');
  }
  const setOf = (s) => {
    const list = [...s].sort((a, b) => EVENTS.indexOf(a) - EVENTS.indexOf(b)).map((e) => `E::${e}`);
    return list.length ? `EventSet::of(&[${list.join(', ')}])` : 'EventSet::EMPTY';
  };
  return [
    ['handlers', out.length ? `&[${out.join(', ')}]` : '&[]'],
    ['events', setOf(events)],
    ['events_prefixed', setOf(prefixed)],
  ];
}

function abilityRow(dex, id) {
  const a = dex.abilities.get(id);
  const flags = Object.keys(a.flags ?? {}).map((f) => {
    if (!ABILITY_FLAGS[f]) fail(`ability ${a.name}: unknown flag ${f}`);
    return `AbilityFlags::${ABILITY_FLAGS[f]}.0`;
  });
  return [
    ['name', str(a.name)],
    ['num', a.num],
    ['flags', flags.length ? `AbilityFlags(${flags.join(' | ')})` : 'AbilityFlags(0)'],
    ['nonstandard', NONSTANDARD[a.isNonstandard] ?? 'None'],
    ...eventHandlers(a, `ability ${a.name}`),
    ['suppress_weather', String(!!a.suppressWeather)],
  ];
}

function itemRow(dex, id) {
  const it = dex.items.get(id);
  const ctx = `item ${it.name}`;
  const mega = Object.entries(it.megaStone ?? {})
    .map(([base, forme]) => `(SpeciesId(${ref('species', base, ctx)}), SpeciesId(${ref('species', forme, ctx)}))`);
  return [
    ['name', str(it.name)],
    ['num', it.num],
    ['fling_power', opt(it.fling?.basePower)],
    ['is_berry', String(!!it.isBerry)],
    ['is_gem', String(!!it.isGem)],
    ['is_choice', String(!!it.isChoice)],
    ['mega', mega.length ? `&[${mega.join(', ')}]` : '&[]'],
    ['nonstandard', NONSTANDARD[it.isNonstandard] ?? 'None'],
    ...eventHandlers(it, ctx),
    ['ignore_klutz', String(!!it.ignoreKlutz)],
    ['is_primal_orb', String(!!it.isPrimalOrb)],
    ['is_pokeball', String(!!it.isPokeball)],
    ['boosts', opt(it.boosts, boosts)],
    ['plate', opt(it.onPlate, (x) => type(x, ctx))],
    ['is_z_crystal', String(!!it.zMove)],
  ];
}

// --------------------------------------------------------------- emitters

const KINDS = [
  { kind: 'species', table: 'SPECIES_TABLE', rowType: 'SpeciesData', row: speciesRow, idType: 'SpeciesId' },
  { kind: 'moves', table: 'MOVE_TABLE', rowType: 'MoveData', row: moveRow, idType: 'MoveId' },
  { kind: 'abilities', table: 'ABILITY_TABLE', rowType: 'AbilityData', row: abilityRow, idType: 'AbilityId' },
  { kind: 'items', table: 'ITEM_TABLE', rowType: 'ItemData', row: itemRow, idType: 'ItemId' },
];

// Field values a blank row has; rows list only the fields that differ and
// fill the rest from the blank (`..BLANK_*`).
const BLANKS = {
  species: { forme: '""', required_items: '&[]', battle_only: 'false', required_tera_type: 'None', max_hp: 'None',
    nfe: 'false', paradox: 'false', nonstandard: 'None' },
  moves: { accuracy: 'None', priority: '0', flags: 'F::NONE', crit_ratio: '1', multihit: 'None', drain: 'None',
    recoil: 'None', heal: 'None', status: 'None', volatile_status: 'None', boosts: 'None', self_boosts: 'None',
    self_volatile: 'None', secondaries: '&[]', self_switch: 'None', force_switch: 'false', ohko: 'false',
    self_destruct: 'false', side_condition: 'None', slot_condition: 'None', weather: 'None', terrain: 'None',
    pseudo_weather: 'None', nonstandard: 'None', unmodelled: '&[]' },
  abilities: { flags: 'AbilityFlags(0)', nonstandard: 'None', handlers: '&[]', events: 'EventSet::EMPTY', events_prefixed: 'EventSet::EMPTY',
    suppress_weather: 'false' },
  items: { fling_power: 'None', is_berry: 'false', is_gem: 'false', is_choice: 'false', mega: '&[]',
    nonstandard: 'None', handlers: '&[]', events: 'EventSet::EMPTY', events_prefixed: 'EventSet::EMPTY', ignore_klutz: 'false', is_primal_orb: 'false',
    is_pokeball: 'false', boosts: 'None', plate: 'None', is_z_crystal: 'false' },
};
const BLANK_NAME = { species: 'BLANK_SPECIES', moves: 'BLANK_MOVE', abilities: 'BLANK_ABILITY', items: 'BLANK_ITEM' };

function rowExpr(spec, fields) {
  const blank = BLANKS[spec.kind];
  const set = fields.filter(([k, v]) => blank[k] !== String(v));
  const rest = set.length < fields.length ? `, ..${BLANK_NAME[spec.kind]}` : '';
  return `${spec.rowType} { ${set.map(([k, v]) => `${k}: ${v}`).join(', ')}${rest} }`;
}

// Placeholders for the fields every row sets.
const REQUIRED = {
  species: { name: '""', num: '0', types: 'TypeSet::mono(Normal)', base_stats: 'Stats::splat(0)',
    abilities: 'slots(0, None, None, None)', weight_hg: '0', gender: 'GenderRule::Genderless',
    base_species: 'SpeciesId(0)' },
  moves: { name: '""', num: '0', ty: 'Normal', category: 'Status', base_power: '0', pp: '0', target: 'T::Normal' },
  abilities: { name: '""', num: '0' },
  items: { name: '""', num: '0' },
};

/** The blank row every generated row is completed from. */
function blankRow(spec) {
  const fields = Object.entries({ ...REQUIRED[spec.kind], ...BLANKS[spec.kind] }).map(([k, v]) => `${k}: ${v}`);
  return `const ${BLANK_NAME[spec.kind]}: ${spec.rowType} = ${spec.rowType} { ${fields.join(', ')} };\n`;
}

// A dex's view of an entry it does not include (e.g. only standard in the
// other dex): take the other dex's row but keep this dex's availability.
function rowFor(dexName, spec, id) {
  const dex = DEXES[dexName];
  const entry = dex[spec.kind].get(id);
  if (entry?.exists) return spec.row(dex, id);
  const [other] = Object.entries(DEXES).filter(([n, d]) => n !== dexName && d[spec.kind].get(id)?.exists);
  if (!other) fail(`${spec.kind} ${id} exists in no dex`);
  return spec.row(other[1], id).map(([k, v]) => (k === 'nonstandard' ? [k, 'Some(Unobtainable)'] : [k, v]));
}

const HEADER = (what) => `// @generated by tools/datagen/datagen.mjs from pokemon-showdown ${showdownVersion}.
// Do not edit by hand: run \`node tools/datagen/datagen.mjs\`.
//
// ${what}
//
// Data from Pokémon Showdown (https://github.com/smogon/pokemon-showdown),
// MIT License, Copyright (c) 2011-2026 Guangcong Luo and other contributors.
`;

function genGen9() {
  let out = HEADER('Generation 9 (Scarlet/Violet) tables. Entries not in Scarlet/Violet are\n// tagged `nonstandard`.');
  out += `
use crate::MoveCategory::*;
use crate::Nonstandard::*;
use crate::Type::*;
use crate::{
    AbilityData, AbilityFlags, AbilityId, AbilitySlots, Dex, Event as E, EventHandler as H, EventPrefix as P, EventSet,
    Fraction, GenderRule, ItemData, ItemId, MoveData, MoveFlags as F, MoveTarget as T, NameIndex, Secondary,
    SelfSwitch, SpeciesData, SpeciesId, StatBoosts as B, Stats, StatusId as St, TeraType, TypeSet,
};

/// The Scarlet/Violet dex (Showdown's \`gen9\`).
pub static DEX: Dex = Dex {
    name: "gen9",
    species: &SPECIES_TABLE,
    moves: &MOVE_TABLE,
    abilities: &ABILITY_TABLE,
    items: &ITEM_TABLE,
    names: &NAMES,
};
`;
  out += '\n';
  for (const spec of KINDS) out += blankRow(spec);
  out += `
/// Ability slots from ability indices.
pub(crate) const fn slots(primary: u16, secondary: Option<u16>, hidden: Option<u16>, special: Option<u16>) -> AbilitySlots {
    const fn id(i: Option<u16>) -> Option<AbilityId> {
        match i {
            Some(i) => Some(AbilityId(i)),
            None => None,
        }
    }
    AbilitySlots { primary: AbilityId(primary), secondary: id(secondary), hidden: id(hidden), special: id(special) }
}
`;
  for (const spec of KINDS) {
    const ids = IDS[spec.kind];
    out += `\n/// The ${spec.kind} table by value, so variant dexes can derive patched copies at\n/// compile time. Each dex materialises it once, in a \`static\`.\n#[rustfmt::skip]\n#[allow(clippy::large_const_arrays)]\npub(crate) const ${spec.table}: [${spec.rowType}; ${ids.length}] = [\n`;
    for (const id of ids) out += `    ${rowExpr(spec, rowFor('gen9', spec, id))},\n`;
    out += '];\n';
  }
  for (const spec of KINDS) {
    const mod = spec.kind;
    out += `\n/// The ${ID_DOC[mod]} IDs of both dexes, one constant per entry, named after it.\n#[rustfmt::skip]\npub mod ${mod} {\n    use crate::${spec.idType};\n`;
    const seen = new Map();
    IDS[spec.kind].forEach((id, i) => {
      const name = displayName(spec.kind, id);
      const c = constName(name);
      if (seen.has(c)) fail(`${spec.kind}: constant ${c} for both ${seen.get(c)} and ${name}`);
      seen.set(c, name);
      out += `    /// ${name}\n    pub const ${c}: ${spec.idType} = ${spec.idType}(${i});\n`;
    });
    out += '}\n';
  }
  out += genNameIndex();
  out += genTypeChartCheck();
  return out;
}

/** The kind of entry each ID module holds, for its doc. */
const ID_DOC = { species: 'species', moves: 'move', abilities: 'ability', items: 'item' };

/** An entry's display name (the other dex's for an entry only it has). */
function displayName(kind, id) {
  return DEXES.gen9[kind].get(id).name || DEXES.champions[kind].get(id).name;
}

// Each table's rows sorted by their names' `toID`, which the dex's name
// lookups binary-search. IDs must be unique and at most `MAX_ID_LEN` long.
const MAX_ID_LEN = 64;
function genNameIndex() {
  let out = `\n/// Each table's rows in the order of their names' Showdown IDs (\`toID\`), for\n/// [\`Dex\`]'s name lookups. Both dexes use it: their names are the same.\n#[rustfmt::skip]\npub static NAMES: NameIndex = NameIndex {\n`;
  for (const spec of KINDS) {
    const rows = IDS[spec.kind].map((id, i) => [toID(displayName(spec.kind, id)), i]);
    rows.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
    rows.forEach(([id], k) => {
      if (!id || id.length > MAX_ID_LEN) fail(`${spec.kind}: name id ${JSON.stringify(id)} is empty or too long`);
      if (k > 0 && rows[k - 1][0] === id) fail(`${spec.kind}: two names with the id ${id}`);
    });
    out += `    ${spec.kind}: &[\n${numbers(rows.map(([, i]) => i), '        ')}\n    ],\n`;
  }
  return out + '};\n';
}

function genTypeChartCheck() {
  const code = { 0: 'N', 1: 'S', 2: 'R', 3: 'I' };
  const rows = TYPES.map((atk) => `        [${TYPES.map((def) => code[DEXES.gen9.types.get(def).damageTaken[atk]]).join(', ')}],`);
  return `
/// Showdown's type chart, \`[attacker][defender]\`, to check the hand-written
/// chart in \`types.rs\` against.
#[cfg(test)]
#[rustfmt::skip]
mod type_chart_check {
    use crate::{Effectiveness, Type};

    const N: Effectiveness = Effectiveness::NEUTRAL;
    const S: Effectiveness = Effectiveness::Exp(1);
    const R: Effectiveness = Effectiveness::Exp(-1);
    const I: Effectiveness = Effectiveness::Immune;

    const SHOWDOWN: [[Effectiveness; 18]; 18] = [
${rows.join('\n')}
    ];

    #[test]
    fn type_chart_matches_showdown() {
        for a in Type::ALL {
            for d in Type::ALL {
                assert_eq!(a.against(d), SHOWDOWN[a as usize][d as usize], "{a:?} vs {d:?}");
            }
        }
    }
}
`;
}

function genChampions() {
  let out = HEADER("Pokémon Champions tables: the Gen 9 tables with the fields that Showdown's\n// `champions` mod changes. IDs are shared with `gen9`.");
  out += `
#![allow(unused_imports)]

use crate::MoveCategory::*;
use crate::Nonstandard::*;
use crate::Type::*;
use crate::gen9;
use crate::{
    AbilityData, AbilityFlags, AbilityId, AbilitySlots, Dex, Event as E, EventHandler as H, EventPrefix as P, EventSet,
    Fraction, GenderRule, ItemData, ItemId, MoveData, MoveFlags as F, MoveTarget as T, Secondary, SelfSwitch,
    SpeciesData, SpeciesId, StatBoosts as B, Stats, StatusId as St, TeraType, TypeSet,
};

use crate::gen9::slots;

pub use crate::gen9::{abilities, items, moves, species};

/// The Pokémon Champions dex (Showdown's \`champions\` mod).
pub static DEX: Dex = Dex {
    name: "champions",
    species: &SPECIES_TABLE,
    moves: &MOVE_TABLE,
    abilities: &ABILITY_TABLE,
    items: &ITEM_TABLE,
    names: &gen9::NAMES,
};
`;
  for (const spec of KINDS) {
    const ids = IDS[spec.kind];
    const name = spec.table;
    const patches = [];
    ids.forEach((id, i) => {
      const base = new Map(rowFor('gen9', spec, id));
      for (const [field, value] of rowFor('champions', spec, id)) {
        if (base.get(field) === value) continue;
        // The dexes share \`gen9::NAMES\`.
        if (field === 'name') fail(`${spec.kind} ${id}: its name differs between the dexes`);
        patches.push(`    t[${i}].${field} = ${value};`);
      }
    });
    out += `\n/// ${patches.length} field overrides.\n#[rustfmt::skip]\nstatic ${name}: [${spec.rowType}; ${ids.length}] = {\n    let mut t = gen9::${name};\n${patches.join('\n')}${patches.length ? '\n' : ''}    t\n};\n`;
  }
  return out;
}

// ---------------------------------------------------------------- legality
// Team-validation data (pikapro-data's `legality` module, feature `legality`):
// what Showdown's `sim/team-validator.ts` reads besides the battle tables.

const EGG_GROUPS = ['Monster', 'Water 1', 'Bug', 'Flying', 'Field', 'Fairy', 'Grass', 'Human-Like', 'Water 3',
  'Mineral', 'Amorphous', 'Water 2', 'Ditto', 'Dragon', 'Undiscovered'];
const SPECIES_TAGS = { Mythical: 'MYTHICAL', 'Restricted Legendary': 'RESTRICTED_LEGENDARY',
  'Sub-Legendary': 'SUB_LEGENDARY', 'Ultra Beast': 'ULTRA_BEAST', Paradox: 'PARADOX' };
// Tags validation never reads.
const IGNORED_TAGS = new Set(['True Past', 'Past Unobtainable', 'Pokestar']);
const GO_SOURCES = { wild: 'WILD', egg: 'EGG', '12kmegg': 'EGG_12KM', raid: 'RAID', research: 'RESEARCH',
  giovanni: 'GIOVANNI', notrade: 'NO_TRADE', specialtrade: 'SPECIAL_TRADE', noshiny: 'NO_SHINY' };
const SOURCE_KINDS = 'LMTRESDV';
// The event fields Showdown has (any other stops generation). `abilities`
// (Gen 5 and earlier), `moves` (the learnset's `S` sources), `pokeball` and
// `source` aren't emitted: validation never reads them in Gen 9.
const EVENT_FIELDS = new Set(['generation', 'level', 'shiny', 'gender', 'nature', 'ivs', 'perfectIVs', 'isHidden',
  'emeraldEventEgg', 'moves', 'pokeball', 'abilities', 'source']);
const HOME_DEXES = { gen8bdsp: Dex.mod('gen8bdsp'), gen8legends: Dex.mod('gen8legends') };

/** Showdown's learnset source string packed as `gen << 12 | kind << 9 | value`. */
function packSource(source, ctx) {
  const m = /^(\d)([LMTRESDV])(\d*)$/.exec(source);
  if (!m) fail(`${ctx}: unknown learnset source ${source}`);
  const value = m[3] === '' ? 0 : Number(m[3]);
  if (value > 511 || (m[2] !== 'L' && m[2] !== 'S' && m[3] !== '')) fail(`${ctx}: source ${source} out of range`);
  return (Number(m[1]) << 12) | (SOURCE_KINDS.indexOf(m[2]) << 9) | value;
}

/**
 * The lowest level at which `move` can be relearned through HOME from a
 * BDSP or Legends: Arceus learnset (`getExternalLearnsetData` in
 * `checkCanLearn`), or null.
 */
function homeLevel(speciesId, move) {
  let best = null;
  for (const [mod, dex] of Object.entries(HOME_DEXES)) {
    if (mod === 'gen8bdsp' && ['nincada', 'spinda'].includes(speciesId)) continue;
    if (dex.species.get(speciesId).isNonstandard) continue;
    for (const source of dex.species.getLearnsetData(speciesId).learnset?.[move] ?? []) {
      if (source.charAt(1) === 'S') continue;
      const level = source.charAt(1) === 'L' ? Number(source.slice(2)) : 0;
      if (best === null || level < best) best = level;
    }
  }
  return best;
}

/** A packed learnset stream (see `LearnsetIter`), or null without a `learnset`. */
function learnsetStream(dex, id) {
  const learnset = dex.species.getLearnsetData(id).learnset;
  if (!learnset) return null;
  const out = [];
  const entries = Object.entries(learnset)
    .filter(([move]) => INDEX.moves.has(move))
    .sort(([a], [b]) => INDEX.moves.get(a) - INDEX.moves.get(b));
  for (const [move, sources] of entries) {
    const ctx = `learnset ${id} ${move}`;
    if (sources.length > 255) fail(`${ctx}: too many sources`);
    // \`checkCanLearn\` only looks at HOME for a move with a Gen 9 non-event
    // source (\`canUseHomeRelearner\`).
    const home = sources.some((s) => s.charAt(0) === '9' && s.charAt(1) !== 'S') ? homeLevel(id, move) : null;
    out.push(INDEX.moves.get(move), sources.length | ((home === null ? 0 : home + 1) << 8));
    for (const s of sources) out.push(packSource(s, ctx));
  }
  return out;
}

function eventExpr(e, ctx) {
  for (const k of Object.keys(e)) if (!EVENT_FIELDS.has(k)) fail(`${ctx}: unknown event field ${k}`);
  const shiny = e.shiny === true ? 'Always' : e.shiny === 1 ? 'Maybe' : e.shiny ? fail(`${ctx}: shiny ${e.shiny}`) : 'Never';
  const gender = e.gender === 'M' ? 'Some(EventGender::Male)' : e.gender === 'F' ? 'Some(EventGender::Female)'
    : e.gender ? fail(`${ctx}: gender ${e.gender}`) : 'None';
  let nature = 'None';
  if (e.nature) {
    if (!DEXES.gen9.natures.get(e.nature).exists) fail(`${ctx}: nature ${e.nature}`);
    nature = `Some(Nature::${DEXES.gen9.natures.get(e.nature).name})`;
  }
  let ivs = 'None';
  if (e.ivs) {
    for (const k of Object.keys(e.ivs)) if (!['hp', 'atk', 'def', 'spa', 'spd', 'spe'].includes(k)) fail(`${ctx}: iv ${k}`);
    ivs = `Some([${['hp', 'atk', 'def', 'spa', 'spd', 'spe'].map((s) => e.ivs[s] ?? 'X').join(', ')}])`;
  }
  const fields = [['level', e.level ?? 0, 0], ['shiny', `EventShiny::${shiny}`, 'EventShiny::Never'],
    ['gender', gender, 'None'], ['nature', nature, 'None'], ['ivs', ivs, 'None'],
    ['perfect_ivs', e.perfectIVs ?? 0, 0], ['is_hidden', !!e.isHidden, false],
    ['emerald_event_egg', !!e.emeraldEventEgg, false]].filter(([, v, blank]) => v !== blank);
  if (!fields.length) return `EventInfo::blank(${e.generation})`;
  return `EventInfo { ${fields.map(([k, v]) => `${k}: ${v}`).join(', ')}, ..EventInfo::blank(${e.generation}) }`;
}

// Showdown's tiers (`species.tier`, `species.doublesTier`: formats-data
// resolved through the base species), which tier tags read (`-tag:uber` in
// [Gen 9] OU). Every value its `TierTypes` allows; an unknown one fails.
// A species without a doubles tier has its singles tier there.
const TIERS = { 'AG': 'Ag', 'Uber': 'Uber', '(Uber)': 'UberUnranked', 'OU': 'Ou', '(OU)': 'OuUnranked', 'UUBL': 'Uubl',
  'UU': 'Uu', 'RUBL': 'Rubl', 'RU': 'Ru', 'NUBL': 'Nubl', 'NU': 'Nu', '(NU)': 'NuUnranked', 'PUBL': 'Publ', 'PU': 'Pu',
  '(PU)': 'PuUnranked', 'ZUBL': 'Zubl', 'ZU': 'Zu', 'NFE': 'Nfe', 'LC': 'Lc', 'DUber': 'DUber',
  '(DUber)': 'DUberUnranked', 'DOU': 'Dou', '(DOU)': 'DouUnranked', 'DBL': 'Dbl', 'DUU': 'Duu', '(DUU)': 'DuuUnranked',
  'Unreleased': 'Unreleased', 'Illegal': 'Illegal', 'CAP': 'Cap', 'CAP NFE': 'CapNfe', 'CAP LC': 'CapLc' };

/** A species' legality row; the learnset is referenced as `LS_<index>`. */
function speciesLegalityRow(dex, id, index, learnsetName) {
  const s = dex.species.get(id);
  const ctx = `species ${s.name}`;
  const sp = (name) => opt(name ? name : null, (x) => `SpeciesId(${ref('species', x, ctx)})`);
  if (s.maleOnlyHidden || s.unreleasedHidden) fail(`${ctx}: maleOnlyHidden / unreleasedHidden are not modelled`);
  let battleOnly = 'BattleOnly::No';
  if (Array.isArray(s.battleOnly)) battleOnly = 'BattleOnly::Several';
  else if (s.battleOnly) battleOnly = `BattleOnly::Species(SpeciesId(${ref('species', s.battleOnly, ctx)}))`;
  const tags = s.tags.filter((t) => !IGNORED_TAGS.has(t)).map((t) => SPECIES_TAGS[t] ?? fail(`${ctx}: unknown tag ${t}`));
  const groups = s.eggGroups.map((g) => (EGG_GROUPS.includes(g) ? EGG_GROUPS.indexOf(g) : fail(`${ctx}: egg group ${g}`)));
  if (s.eggGroups.includes('Undiscovered') && s.eggGroups.length > 1) fail(`${ctx}: Undiscovered with another group`);
  const data = dex.species.getLearnsetData(id);
  const events = (data.eventData ?? []).map((e, i) => eventExpr(e, `${ctx} event ${i}`));
  if (data.eventOnly && !data.eventData && !s.forme) fail(`${ctx}: event-only without events`);
  const go = dex.species.getPokemonGoData(id);
  let goExpr = 'None';
  if (go) {
    if (!go.encounters) fail(`${ctx}: Pokémon GO data without encounters`);
    const bits = go.encounters.map((e) => GO_SOURCES[e] ?? fail(`${ctx}: GO encounter ${e}`));
    goExpr = `Some(GoSources(${bits.map((b) => `GoSources::${b}.0`).join(' | ') || '0'}))`;
  }
  const lgpe = Object.entries(go?.LGPERestrictiveMoves ?? {}).map(([m, l]) => `(MoveId(${ref('moves', m, ctx)}), ${l})`);
  const evoMove = s.evoType === 'levelMove' ? `Some(MoveId(${ref('moves', s.evoMove, ctx)}))` : 'None';
  if (s.evoRegion && !['Alola', 'Galar'].includes(s.evoRegion)) fail(`${ctx}: evoRegion ${s.evoRegion}`);
  return [
    ['generation', s.gen],
    ['prevo', sp(s.prevo)],
    ['first_evo', sp(s.evos[0])],
    ['evo_level', s.evoLevel ?? 0],
    ['evo_move', evoMove],
    ['evo_region', opt(s.evoRegion, (r) => `EvoRegion::${r}`)],
    ['changes_from', sp(s.changesFrom)],
    ['base_forme', str(s.baseForme ?? '')],
    ['battle_only', battleOnly],
    ['required_ability', opt(s.requiredAbility, (a) => `AbilityId(${ref('abilities', a, ctx)})`)],
    ['required_move', opt(s.requiredMove, (m) => `MoveId(${ref('moves', m, ctx)})`)],
    ['required_item', opt(s.requiredItem, (i) => `ItemId(${ref('items', i, ctx)})`)],
    ['tags', tags.length ? `SpeciesTags(${tags.map((t) => `SpeciesTags::${t}.0`).join(' | ')})` : 'SpeciesTags(0)'],
    ['tier', `Tier::${TIERS[s.tier] ?? fail(`${ctx}: unknown tier ${s.tier}`)}`],
    ['doubles_tier', `Tier::${TIERS[s.doublesTier] ?? fail(`${ctx}: unknown doubles tier ${s.doublesTier}`)}`],
    ['egg_groups', `EggGroups(${groups.reduce((a, g) => a | (1 << g), 0)})`],
    ['can_hatch', String(!!s.canHatch)],
    ['mother', sp(s.mother)],
    ['learnset', learnsetName ? `Some(Learnset(&${learnsetName}))` : 'None'],
    ['events', events.length ? `&[${events.join(', ')}]` : '&[]'],
    ['event_only', String(!!data.eventOnly)],
    ['go', goExpr],
    ['lgpe_restrictive_moves', lgpe.length ? `&[${lgpe.join(', ')}]` : '&[]'],
  ];
}

const LEGALITY_BLANK = { prevo: 'None', first_evo: 'None', evo_level: '0', evo_move: 'None', evo_region: 'None',
  changes_from: 'None', base_forme: '""', battle_only: 'BattleOnly::No', required_ability: 'None',
  required_move: 'None', required_item: 'None', tags: 'SpeciesTags(0)', tier: 'Tier::Illegal',
  doubles_tier: 'Tier::Illegal', egg_groups: 'EggGroups(0)',
  can_hatch: 'false', mother: 'None', learnset: 'None', events: '&[]', event_only: 'false', go: 'None',
  lgpe_restrictive_moves: '&[]' };

function legalityRowExpr(fields) {
  const set = fields.filter(([k, v]) => LEGALITY_BLANK[k] !== String(v));
  return `SpeciesLegality { ${set.map(([k, v]) => `${k}: ${v}`).join(', ')}, ..BLANK }`;
}

/** Wraps a list of numbers into lines of at most ~110 characters. */
function numbers(list, indent) {
  const lines = [];
  let line = '';
  for (const n of list) {
    const item = `${n},`;
    if (line && line.length + item.length + 1 > 110 - indent.length) {
      lines.push(indent + line);
      line = '';
    }
    line += (line ? ' ' : '') + item;
  }
  if (line) lines.push(indent + line);
  return lines.join('\n');
}

// A dex's view of a species it does not include is taken from the other
// dex, as for the battle tables.
function legalityDexFor(dexName, id) {
  if (DEXES[dexName].species.get(id)?.exists) return DEXES[dexName];
  const other = Object.entries(DEXES).find(([n, d]) => n !== dexName && d.species.get(id)?.exists);
  if (!other) fail(`species ${id} exists in no dex`);
  return other[1];
}

function legalityTables(dexName) {
  const learnsets = IDS.species.map((id) => learnsetStream(legalityDexFor(dexName, id), id));
  const rows = IDS.species.map((id, i) => speciesLegalityRow(legalityDexFor(dexName, id), id, i, learnsets[i] ? `LS_${i}` : null));
  const moves = IDS.moves.map((id) => {
    const dex = DEXES[dexName].moves.get(id).exists ? DEXES[dexName] : DEXES.gen9;
    const m = dex.moves.get(id);
    return `MoveLegality { id: MoveId(${ref('moves', m.id, `move ${m.name}`)}), generation: ${m.gen} }`;
  });
  const items = IDS.items.map((id) => {
    const dex = DEXES[dexName].items.get(id).exists ? DEXES[dexName] : DEXES.gen9;
    const it = dex.items.get(id);
    const forme = it.forcedForme && INDEX.species.has(toID(it.forcedForme)) ? `Some(SpeciesId(${INDEX.species.get(toID(it.forcedForme))}))` : 'None';
    if (it.forcedForme && forme === 'None' && it.isNonstandard !== 'CAP') fail(`item ${it.name}: forced forme ${it.forcedForme}`);
    return `ItemLegality { forced_forme: ${forme} }`;
  });
  return { learnsets, rows, moves, items };
}

const LEGALITY_IMPORTS = `use crate::Nature;
use crate::legality::{
    BattleOnly, EggGroups, EventGender, EventInfo, EventShiny, EvoRegion, GoSources, ItemLegality, Learnset, Legality,
    MoveLegality, SpeciesLegality, SpeciesTags, Tier,
};
use crate::{AbilityId, ItemId, MoveId, SpeciesId};
`;
const LEGALITY_X = `
/// An IV an event does not fix.
#[allow(dead_code)]
const X: u8 = EventInfo::NO_IV;
`;

function genLegalityGen9(t) {
  let out = HEADER('Team-legality tables for the Gen 9 dex (feature `legality`): learnsets,\n// events and the species fields Showdown\'s team validator reads.');
  out += `\n${LEGALITY_IMPORTS}${LEGALITY_X}
/// The Scarlet/Violet legality tables.
pub static LEGALITY: Legality = Legality { name: "gen9", species: &SPECIES, moves: &MOVES, items: &ITEMS };

const BLANK: SpeciesLegality = SpeciesLegality { generation: 0, ${Object.entries(LEGALITY_BLANK).map(([k, v]) => `${k}: ${v}`).join(', ')} };

/// The species table by value, so the Champions dex can patch a copy.
#[rustfmt::skip]
#[allow(clippy::large_const_arrays)]
pub(crate) const SPECIES_TABLE: [SpeciesLegality; ${IDS.species.length}] = [
${t.rows.map((r) => `    ${legalityRowExpr(r)},`).join('\n')}
];

static SPECIES: [SpeciesLegality; ${IDS.species.length}] = SPECIES_TABLE;

#[rustfmt::skip]
pub(crate) static MOVES: [MoveLegality; ${IDS.moves.length}] = [
${t.moves.map((m) => `    ${m},`).join('\n')}
];

#[rustfmt::skip]
pub(crate) static ITEMS: [ItemLegality; ${IDS.items.length}] = [
${t.items.map((m) => `    ${m},`).join('\n')}
];
`;
  t.learnsets.forEach((ls, i) => {
    if (!ls) return;
    out += `\n/// ${IDS.species[i]}\n#[rustfmt::skip]\npub(crate) static LS_${i}: [u16; ${ls.length}] = [\n${numbers(ls, '    ')}\n];\n`;
  });
  return out;
}

function genLegalityChampions(t, base) {
  let out = HEADER('Team-legality tables for the Champions dex (feature `legality`): the Gen 9\n// tables with what the `champions` mod changes. IDs are shared with `gen9`.');
  const patches = [];
  const statics = [];
  t.rows.forEach((row, i) => {
    const baseRow = new Map(base.rows[i]);
    const sameLearnset = JSON.stringify(t.learnsets[i]) === JSON.stringify(base.learnsets[i]);
    for (const [field, value] of row) {
      if (field === 'learnset') {
        if (sameLearnset) continue;
        if (t.learnsets[i]) statics.push(`\n/// ${IDS.species[i]}\n#[rustfmt::skip]\nstatic LS_${i}: [u16; ${t.learnsets[i].length}] = [\n${numbers(t.learnsets[i], '    ')}\n];\n`);
        patches.push(`    t[${i}].learnset = ${value};`);
      } else if (baseRow.get(field) !== value) {
        patches.push(`    t[${i}].${field} = ${value};`);
      }
    }
  });
  const moveDiff = t.moves.some((m, i) => m !== base.moves[i]);
  const itemDiff = t.items.some((m, i) => m !== base.items[i]);
  out += `\n#![allow(unused_imports)]\n\n${LEGALITY_IMPORTS}use crate::legality::gen9;\n${LEGALITY_X}

/// The Pokémon Champions legality tables.
pub static LEGALITY: Legality = Legality { name: "champions", species: &SPECIES, moves: ${moveDiff ? '&MOVES' : '&gen9::MOVES'}, items: ${itemDiff ? '&ITEMS' : '&gen9::ITEMS'} };

/// ${patches.length} field overrides.
#[rustfmt::skip]
static SPECIES: [SpeciesLegality; ${IDS.species.length}] = {
    let mut t = gen9::SPECIES_TABLE;
${patches.join('\n')}${patches.length ? '\n' : ''}    t
};
`;
  if (moveDiff) out += `\n#[rustfmt::skip]\nstatic MOVES: [MoveLegality; ${IDS.moves.length}] = [\n${t.moves.map((m) => `    ${m},`).join('\n')}\n];\n`;
  if (itemDiff) out += `\n#[rustfmt::skip]\nstatic ITEMS: [ItemLegality; ${IDS.items.length}] = [\n${t.items.map((m) => `    ${m},`).join('\n')}\n];\n`;
  // Unchanged learnsets stay the Gen 9 statics.
  out += statics.join('');
  return out;
}

/** Formats Rust source with the repository's rustfmt configuration. */
function rustfmt(source) {
  return execFileSync('rustfmt', ['--edition', '2024', '--config-path', join(root, 'rustfmt.toml'), '--emit', 'stdout'], {
    input: source,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
}

const LEGALITY = { gen9: legalityTables('gen9'), champions: legalityTables('champions') };

// ------------------------------------------------------- random-battle sets
// Competitive-style sets for pikapro-teams (`crates/pikapro-teams/src/sets/`).
// Showdown's own random-battle generators (`data/random-battles/{gen9,
// champions}/teams.js`) draw sets from its random-battle data (roles,
// movepools, abilities, Tera types) and pick the moves, ability and item;
// each species is drawn RANDOM_SET_DRAWS times from a fixed seed and the
// distinct sets that Showdown's validator accepts in the target validation
// format, at the level they're used (100 under Anything Goes, where levels
// aren't adjusted, so a random-battle set's balance level would fail the
// learn-level checks; 50 in Champions), are kept. Levels, EVs, natures and
// genders are not kept: pikapro-teams
// sets them for the format it builds teams for (the flags below keep what
// the generator's spread says about the set).

const { Teams, TeamValidator } = require('pokemon-showdown');

const RANDOM_SET_SOURCES = [
  { file: 'gen9', format: 'gen9randombattle', doubles: false, validate: 'gen9anythinggoes@@@Species Clause', level: 100, dex: 'gen9' },
  { file: 'gen9doubles', format: 'gen9randomdoublesbattle', doubles: true, validate: 'gen9anythinggoes@@@Species Clause', level: 100, dex: 'gen9' },
  { file: 'champions', format: 'gen9championsrandombattle', doubles: false, validate: 'gen9championsvgc2026regmc', level: 50, dex: 'champions' },
];
const RANDOM_SET_DRAWS = 32;
const RANDOM_SET_SEED = [0x2300, 0x0000, 0x5e75, 0x7eab];
// Row layout: species, item, ability, four moves, Tera type | role << 5, flags.
const NONE_ID = 0xffff;
const TERA_NAMES = [...TYPES, 'Stellar'];
const NO_TERA = 31;
// Flags: the generator zeroed Attack (no move uses it) / Speed (Trick Room,
// Gyro Ball, Metal Burst).
const FLAG_NO_ATTACK = 1;
const FLAG_NO_SPEED = 2;

// Whether `randomSet(species, {}, true)` has a set to draw: Showdown's
// `RandomTeams.randomSet` (data/random-battles/gen9/teams.ts) drops
// Protosynthesis / Quark Drive "Fast Bulky Setup" sets for a lead and then
// samples the rest, which throws when none is left (Iron Thorns has only
// that set). `randomTeam` never leads with such a species; neither do we.
function hasLeadSet(entry) {
  return entry.sets.some((set) => !((set.abilities.includes('Protosynthesis') || set.abilities.includes('Quark Drive'))
    && set.role === 'Fast Bulky Setup'));
}

function harvestRandomSets(src) {
  const dex = DEXES[src.dex];
  const generator = Teams.getGenerator(src.format, RANDOM_SET_SEED);
  const table = src.doubles ? generator.randomDoublesSets : generator.randomSets;
  const validator = new TeamValidator(src.validate);
  const roles = [];
  const rows = [];
  let distinct = 0;
  const rejected = new Map();
  for (const key of Object.keys(table)) {
    const seen = new Set();
    const canLead = hasLeadSet(table[key]);
    for (let i = 0; i < RANDOM_SET_DRAWS; i++) {
      const set = generator.randomSet(key, {}, canLead && i % 4 === 0, src.doubles);
      const ctx = `${src.format} ${key}`;
      // The forme a set starts as (Megas: the base species with the stone);
      // cosmetic formes outside the ID space, and those added for the
      // random-battle generator (`ADDED_COSMETIC_FORMES`), are their base
      // species, as these tables have always had them.
      let species = dex.species.get(set.species);
      if (!INDEX.species.has(species.id) || ADDED_COSMETIC_FORMES.includes(species.id)) {
        species = dex.species.get(species.baseSpecies);
      }
      const speciesIndex = ref('species', species.id, ctx);
      const moves = set.moves.map((m) => ref('moves', m, ctx));
      if (moves.length < 1 || moves.length > 4) fail(`${ctx}: ${moves.length} moves`);
      const item = set.item ? ref('items', set.item, ctx) : NONE_ID;
      const ability = ref('abilities', set.ability, ctx);
      const tera = set.teraType ? TERA_NAMES.indexOf(set.teraType) : NO_TERA;
      if (tera < 0) fail(`${ctx}: Tera type ${set.teraType}`);
      const k = [speciesIndex, item, ability, [...moves].sort((a, b) => a - b).join(','), tera].join('|');
      if (seen.has(k)) continue;
      seen.add(k);
      distinct++;
      const problems = validator.validateSet({
        name: '', species: species.name, item: set.item ?? '', ability: set.ability, moves: set.moves,
        nature: '', evs: set.evs, ivs: { hp: 31, atk: 31, def: 31, spa: 31, spd: 31, spe: 31 },
        level: src.level, gender: '', shiny: false, teraType: set.teraType,
      }, {});
      if (problems?.length) {
        const why = problems[0].replace(/^.*?(is tagged|can't|is only|must|needs|is banned|does not)/, '$1');
        rejected.set(why, (rejected.get(why) ?? 0) + 1);
        continue;
      }
      if (!roles.includes(set.role)) roles.push(set.role);
      const flags = (set.evs.atk === 0 ? FLAG_NO_ATTACK : 0) | (set.evs.spe === 0 ? FLAG_NO_SPEED : 0);
      const row = [speciesIndex, item, ability, ...moves, ...Array(4 - moves.length).fill(NONE_ID),
        tera | (roles.indexOf(set.role) << 5), flags];
      rows.push({ row, name: species.name });
    }
  }
  if (roles.length > 2047) fail('too many roles');
  return { src, roles, rows, distinct, rejected };
}

function genRandomSets(h) {
  const { src } = h;
  const rejected = [...h.rejected.entries()].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1));
  let out = HEADER(`Competitive-style sets for pikapro-teams: what Showdown's \`${src.format}\` generator draws\n// (${RANDOM_SET_DRAWS} draws per species), kept if Showdown validates them in \`${src.validate}\`.`);
  out += `
use super::SetTable;

/// ${h.rows.length} sets kept of ${h.distinct} distinct ones drawn. Rejected by \`${src.validate}\`:
${rejected.length ? rejected.map(([why, n]) => `/// - ${n}: ${why.replace(/\*/g, '')}`).join('\n') : '/// none.'}
pub static SETS: SetTable = SetTable {
    source: ${str(src.format)},
    validated_as: ${str(src.validate)},
    draws: ${RANDOM_SET_DRAWS},
    distinct: ${h.distinct},
    roles: &[${h.roles.map(str).join(', ')}],
    rows: &ROWS,
};

#[rustfmt::skip]
static ROWS: [[u16; 9]; ${h.rows.length}] = [
${h.rows.map(({ row, name }) => `    [${row.join(', ')}], // ${name}`).join('\n')}
];
`;
  return out;
}

const RANDOM_SETS = RANDOM_SET_SOURCES.map(harvestRandomSets);

// ------------------------------------------------- random-battle generator
// What pikapro-teams' port of Showdown's Gen 9 random-battle generator
// (`random_battle`, from `data/random-battles/gen9/teams.ts`) reads:
// `sets.json` and `doubles-sets.json` in their key order (the order
// `getPokemonPool` sees them in), and the fields of each species and move
// the generator queries, as Showdown's dex objects have them. The
// generator's logic is in the port, not here.

// `dex.types.names()`: the type chart's order, Stellar included.
const RB_TYPES = DEXES.gen9.types.names();
for (const t of RB_TYPES) if (t !== 'Stellar') type(t, 'random-battle types');

function rbType(name, ctx) {
  const i = RB_TYPES.indexOf(name);
  if (i < 0) fail(`${ctx}: unknown type ${name}`);
  return i;
}

// Move flags the generator reads (`queryMoves`, `randomMoveset`,
// `randomSet`), each a JavaScript truthiness test on Showdown's `Move`.
const RB_MOVE_FLAGS = [
  ['BP_CALLBACK', (m) => !!m.basePowerCallback],
  ['DAMAGE', (m) => !!(m.damage || m.damageCallback)],
  ['MULTIHIT_TO_5', (m) => Array.isArray(m.multihit) && m.multihit[1] === 5],
  ['RECOIL', (m) => !!(m.recoil || m.hasCrashDamage)],
  ['DRAIN', (m) => !!m.drain],
  ['SECONDARY', (m) => !!(m.secondary || m.hasSheerForceBoost)],
  ['BITE', (m) => !!m.flags.bite],
  ['PUNCH', (m) => !!m.flags.punch],
  ['SOUND', (m) => !!m.flags.sound],
];
const RB_CATEGORIES = { Physical: 0, Special: 1, Status: 2 };

function genRandomBattle() {
  const dex = DEXES.gen9;
  const roles = [];
  const species = [];
  const speciesIndex = new Map();
  const speciesFacts = (id, ctx) => {
    if (speciesIndex.has(id)) return speciesIndex.get(id);
    const s = dex.species.get(id);
    if (!s.exists) fail(`${ctx}: no species ${id}`);
    const sp = (x) => `SpeciesId(${ref('species', x, ctx)})`;
    const list = (xs, f) => `&[${(xs ?? []).map(f).join(', ')}]`;
    const st = s.baseStats;
    const gender = { M: "b'M'", F: "b'F'", N: "b'N'" }[s.gender] ?? '0';
    if (s.gender && gender === '0') fail(`${ctx}: gender ${s.gender}`);
    species.push(`S { id: ${sp(s.id)}, base_species: ${sp(s.baseSpecies)}, types: ${list(s.types, (t) => rbType(t, ctx))}, `
      + `base_stats: [${st.hp}, ${st.atk}, ${st.def}, ${st.spa}, ${st.spd}, ${st.spe}], `
      + `abilities: ${list(Object.values(s.abilities), (a) => `AbilityId(${ref('abilities', a, ctx)})`)}, gender: ${gender}, nfe: ${!!s.nfe}, `
      + `required_items: ${list(s.requiredItems, (i) => `ItemId(${ref('items', i, ctx)})`)}, `
      + `required_move: ${opt(s.requiredMove, (m) => `MoveId(${ref('moves', m, ctx)})`)}, `
      + `battle_only: ${typeof s.battleOnly === 'string' ? `Some(${sp(s.battleOnly)})` : (s.battleOnly ? fail(`${ctx}: battleOnly ${s.battleOnly}`) : 'None')}, `
      + `cosmetic_formes: ${list(s.cosmeticFormes, sp)}, `
      // Undefined, or (\`Outside\`) some are outside the ID space
      // (Pikachu-Starter).
      + `other_formes: ${!s.otherFormes ? 'O::Undefined' : s.otherFormes.every((f) => INDEX.species.has(toID(f))) ? `O::Ids(${list(s.otherFormes, sp)})` : 'O::Outside'} }, // ${s.name}`);
    speciesIndex.set(id, species.length - 1);
    return species.length - 1;
  };
  const tables = {};
  for (const [name, table] of Object.entries(RANDOM_BATTLE_TABLES)) {
    const entries = [];
    for (const key of Object.keys(table)) {
      const ctx = `random-battle ${name} ${key}`;
      const entry = table[key];
      for (const k of Object.keys(entry)) if (!['level', 'sets'].includes(k)) fail(`${ctx}: field ${k}`);
      if (toID(key) !== key || dex.species.get(key).id !== key) fail(`${ctx}: not a species ID`);
      const sets = entry.sets.map((set, i) => {
        const sctx = `${ctx} set ${i}`;
        for (const k of Object.keys(set)) if (!['role', 'movepool', 'abilities', 'teraTypes'].includes(k)) fail(`${sctx}: field ${k}`);
        if (!Array.isArray(set.movepool) || !Array.isArray(set.abilities) || !Array.isArray(set.teraTypes)) {
          fail(`${sctx}: movepool, abilities and teraTypes must be arrays`);
        }
        if (!roles.includes(set.role)) roles.push(set.role);
        const moves = set.movepool.map((m) => `MoveId(${ref('moves', dex.moves.get(m).id, sctx)})`);
        const abilities = set.abilities.map((a) => `AbilityId(${ref('abilities', a, sctx)})`);
        const tera = set.teraTypes.map((t) => rbType(t, sctx));
        return `Set { role: ${roles.indexOf(set.role)}, movepool: &[${moves.join(', ')}], abilities: &[${abilities.join(', ')}], tera_types: &[${tera.join(', ')}] }`;
      });
      if (!Number.isInteger(entry.level ?? 0)) fail(`${ctx}: level ${entry.level}`);
      entries.push(`    E { species: ${speciesFacts(key, ctx)}, level: ${entry.level ?? 0}, sets: &[${sets.join(', ')}] }, // ${dex.species.get(key).name}`);
    }
    tables[name] = entries;
  }
  const moves = IDS.moves.map((id) => {
    const m = dex.moves.get(id).exists ? dex.moves.get(id) : DEXES.champions.moves.get(id);
    const ctx = `random-battle move ${m.name}`;
    const flags = RB_MOVE_FLAGS.filter(([, f]) => f(m)).map(([n]) => `F::${n}.0`);
    const acc = m.accuracy === true ? 0 : m.accuracy;
    if (!Number.isInteger(acc) || acc < 0 || acc > 255) fail(`${ctx}: accuracy ${m.accuracy}`);
    if (!(m.category in RB_CATEGORIES)) fail(`${ctx}: category ${m.category}`);
    return `    M { category: ${RB_CATEGORIES[m.category]}, ty: ${rbType(m.type, ctx)}, base_power: ${m.basePower}, priority: ${m.priority}, `
      + `accuracy: ${acc}, flags: F(${flags.length ? flags.join(' | ') : '0'}) }, // ${m.name}`;
  });
  // `cachedStatusMoves`: every Status move of `dex.moves.all()`, in its
  // order. Those outside the ID space are left out: no movepool has one
  // (`ref` above fails on a movepool move outside it).
  const statusMoves = dex.moves.all().filter((m) => m.category === 'Status' && INDEX.moves.has(m.id))
    .map((m) => `MoveId(${INDEX.moves.get(m.id)})`);
  const chart = RB_TYPES.map((a) => `[${RB_TYPES.map((d) => dex.getEffectiveness(a, d)).join(', ')}]`);
  let out = HEADER("The Gen 9 random-battle generator's data (`data/random-battles/gen9/`: `sets.json`,\n// `doubles-sets.json`) and the species and move fields it reads, for pikapro-teams' port of it.");
  out += `
use pikapro_data::{AbilityId, ItemId, MoveId, SpeciesId, Type};

use super::{Entry as E, MoveFacts as M, MoveFlags as F, OtherFormes as O, Set, SpeciesFacts as S};

/// \`dex.types.names()\`, Showdown's type order (Stellar included).
pub(crate) static TYPES: [Type; ${RB_TYPES.length}] = [${RB_TYPES.map((t) => `Type::${t}`).join(', ')}];

/// \`dex.getEffectiveness(TYPES[a], TYPES[d])\`: 1 (super effective), -1
/// (resisted) or 0 (neutral, and immune, which it doesn't tell apart).
#[rustfmt::skip]
pub(crate) static EFFECTIVENESS: [[i8; ${RB_TYPES.length}]; ${RB_TYPES.length}] = [${chart.join(', ')}];

/// The cosmetic formes the generator can draw that Showdown has no Pokédex
/// entry for (it makes them up from \`cosmeticFormes\`): added to the ID
/// space after every other species for the generator.
pub static ADDED_FORMES: [SpeciesId; ${ADDED_COSMETIC_FORMES.length}] = [${ADDED_COSMETIC_FORMES.map((id) => `SpeciesId(${INDEX.species.get(id)})`).join(', ')}];

/// The sets' roles, by \`Set::role\`.
pub(crate) static ROLES: [&str; ${roles.length}] = [${roles.map(str).join(', ')}];

/// Every species of the tables, by \`Entry::species\`.
#[rustfmt::skip]
pub(crate) static SPECIES: [S; ${species.length}] = [
${species.map((x) => `    ${x}`).join('\n')}
];

/// \`sets.json\`, in its key order.
#[rustfmt::skip]
pub(crate) static SINGLES: [E; ${tables.singles.length}] = [
${tables.singles.join('\n')}
];

/// \`doubles-sets.json\`, in its key order.
#[rustfmt::skip]
pub(crate) static DOUBLES: [E; ${tables.doubles.length}] = [
${tables.doubles.join('\n')}
];

/// Every move of the ID space, by \`MoveId\`.
#[rustfmt::skip]
pub(crate) static MOVES: [M; ${moves.length}] = [
${moves.join('\n')}
];

/// \`cachedStatusMoves\`.
#[rustfmt::skip]
pub(crate) static STATUS_MOVES: [MoveId; ${statusMoves.length}] = [
${numbers(statusMoves, '    ')}
];
`;
  return out;
}

const outputs = {
  ...Object.fromEntries(RANDOM_SETS.map((h) => [
    join(root, `crates/pikapro-teams/src/sets/${h.src.file}.rs`), rustfmt(genRandomSets(h)),
  ])),
  [join(root, 'crates/pikapro-teams/src/random_battle/data.rs')]: rustfmt(genRandomBattle()),
  [join(root, 'crates/pikapro-data/src/gen9.rs')]: rustfmt(genGen9()),
  [join(root, 'crates/pikapro-data/src/champions.rs')]: rustfmt(genChampions()),
  [join(root, 'crates/pikapro-data/src/legality/gen9.rs')]: rustfmt(genLegalityGen9(LEGALITY.gen9)),
  [join(root, 'crates/pikapro-data/src/legality/champions.rs')]: rustfmt(genLegalityChampions(LEGALITY.champions, LEGALITY.gen9)),
};

const check = process.argv.includes('--check');
let stale = false;
for (const [path, content] of Object.entries(outputs)) {
  let current = '';
  try {
    current = readFileSync(path, 'utf8');
  } catch {}
  if (current === content) continue;
  if (check) {
    console.error(`datagen: ${path} is out of date`);
    stale = true;
  } else {
    writeFileSync(path, content);
    console.log(`wrote ${path}`);
  }
}
const counts = Object.entries(IDS).map(([k, v]) => `${v.length} ${k}`).join(', ');
console.log(`datagen: ${counts} (pokemon-showdown ${showdownVersion})${check && !stale ? ', up to date' : ''}`);
process.exitCode = stale ? 1 : 0;
