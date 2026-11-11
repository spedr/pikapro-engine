//! Showdown's team formats: the export format ("PokePaste") and the packed
//! format servers use (`Teams.pack` / `Teams.unpack` in
//! `sim/teams.ts`).
//!
//! ```text
//! Chompy (Garchomp) (F) @ Choice Scarf
//! Ability: Rough Skin
//! Level: 50
//! Shiny: Yes
//! Happiness: 160
//! Pokeball: Great Ball
//! Tera Type: Ground
//! EVs: 252 Atk / 4 SpD / 252 Spe
//! Jolly Nature
//! IVs: 0 SpA
//! - Earthquake
//! - Hidden Power [Fire]
//! ```
//!
//! Everything Showdown's `Teams.import` reads is read as it reads it:
//! `Trait:` for `Ability:`, `No Item`, `~` or a missing space before a move,
//! `Hidden Power [Type]`, `Hidden Power:`, `Gigantamax: Yes`, lower-case
//! `nature`, the stat names `Dex.stats.getID` knows (a spread entry it
//! doesn't know is skipped), numbers as `Number` and `parseInt` read them,
//! `---` between sets and `===` team headers; a text that is one line with a
//! `|` (once empty lines around it are dropped) is a packed team. Where
//! Showdown would keep something no set can have, import fails instead: an
//! unknown line, species, item, ability, move, nature or type, a number the
//! set can't hold, more than 4 moves or more than 24 sets. `Level: 0` (or no
//! number) is level 100, as Showdown's validator and battle make it.
//! `Dynamax Level:` is ignored, as Showdown's import does (the packed format
//! carries it).
//!
//! Stat Points (Champions) are written as `EVs:`, where Showdown's
//! `champions` mod and the packed format keep them, and read from `EVs:` or
//! `SPs:`; IVs are then ignored.
//!
//! A set is read as written, as `Teams.import` reads it: a battle-only
//! forme listed as the species (`Floette-Mega @ Floettite`, as pastes list
//! Megas) stays that forme. A Showdown server validates a team before its
//! battle, and its validator starts such a set in the forme it changes from
//! (Floette-Eternal with Flower Veil); `pikapro-validate` does the same
//! (`check_preset`'s team). Import doesn't: an unvalidated team plays as
//! given, as Showdown's simulator plays it.
//!
//! What core does not model lives in [`SetExtras`] next to the [`Team`]:
//! nickname, shininess, happiness (Return and Frustration are not in Gen 9
//! or Champions), Poké Ball, Hidden Power type, Gigantamax and Dynamax
//! level. [`TeamSheet::nicknames`] is the table renderers take.

use core::fmt::{self, Write as _};

use pikapro::format::StatSystem;
use pikapro::{Format, Gender, Investment, PokemonSet, Team};
use pikapro_data::{Dex, Nature, SpeciesId, Stat, Stats, Type};

use crate::js;

/// The most sets a team can have: `Teams.unpack` reads no more.
pub(crate) const MAX_SETS: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportError {
    /// 1-based line number (the set's number for a packed team).
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ImportError {}

/// The fields of a Showdown set that the simulator doesn't read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetExtras {
    /// The nickname as written (`set.name`); empty for none.
    pub name: String,
    pub shiny: bool,
    /// `None`: not given (Showdown's 255).
    pub happiness: Option<u8>,
    /// The Poké Ball as written; empty for none.
    pub pokeball: String,
    /// `Hidden Power: <Type>` (`set.hpType`).
    pub hp_type: Option<Type>,
    pub gigantamax: bool,
    /// `None`: not given (Showdown's 10). Only the packed format has it.
    pub dynamax_level: Option<u8>,
}

/// A team with the fields core doesn't model, in registration order:
/// `extras[i]` belongs to `team.sets[i]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeamSheet {
    pub team: Team,
    pub extras: Vec<SetExtras>,
}

impl TeamSheet {
    /// A sheet without nicknames or other extras.
    pub fn new(team: Team) -> TeamSheet {
        let extras = vec![SetExtras::default(); team.sets.len()];
        TeamSheet { team, extras }
    }

    fn extras(&self, i: usize) -> SetExtras {
        self.extras.get(i).cloned().unwrap_or_default()
    }

    /// The name each Pokémon goes by in the battle ([`pokemon_name`]),
    /// indexed by [`PokemonState::id`](pikapro::PokemonState::id)
    /// (registration order), empty when it is its base species' name. This
    /// is the table [`Renderer::nicknames`](crate::Renderer::nicknames) and
    /// [`parse_choice_for`](crate::parse_choice_for) take.
    pub fn nicknames(&self, format: &Format) -> Vec<String> {
        let dex = format.dex;
        self.team
            .sets
            .iter()
            .enumerate()
            .map(|(i, set)| {
                let name = pokemon_name(&self.extras(i).name, set.species, dex);
                if name == dex.species(dex.species(set.species).base_species).name { String::new() } else { name }
            })
            .collect()
    }

    /// The Poké Ball each Pokémon's request shows, indexed like
    /// [`TeamSheet::nicknames`] (see [`Renderer::pokeballs`](crate::Renderer::pokeballs)).
    pub fn pokeballs(&self) -> Vec<String> {
        (0..self.team.sets.len()).map(|i| self.extras(i).pokeball).collect()
    }
}

/// `pokemon.name`, as Showdown's `Pokemon` constructor names a set: its
/// base species' name when the nickname is empty or the species' own name,
/// otherwise the nickname cut to 20 UTF-16 units (a cut through a surrogate
/// pair leaves U+FFFD, as Node writes the lone half). A `|`, which no
/// Showdown set can carry in its name (the packed format splits on it and
/// the validator's `getName` removes it), becomes a space so the protocol's
/// lines stay intact.
pub fn pokemon_name(nickname: &str, species: SpeciesId, dex: &Dex) -> String {
    let data = dex.species(species);
    if nickname.is_empty() || nickname == data.name {
        return dex.species(data.base_species).name.to_string();
    }
    let mut out = String::with_capacity(nickname.len().min(80));
    let mut units = 0;
    for c in nickname.chars() {
        let w = c.len_utf16();
        if units + w > 20 {
            if units < 20 {
                out.push('\u{fffd}');
            }
            break;
        }
        out.push(if c == '|' { ' ' } else { c });
        units += w;
    }
    out
}

/// The name of the Pokémon with stable id `id` in a names table (see
/// [`TeamSheet::nicknames`]): its entry, or its base species' name.
pub(crate) fn name_ref<'a>(names: &'a [String], id: u8, species: SpeciesId, dex: &'a Dex) -> &'a str {
    match names.get(id as usize).filter(|n| !n.is_empty()) {
        Some(n) => n,
        None => dex.species(dex.species(species).base_species).name,
    }
}

/// Parses an exported (or packed) team, ignoring the extras. The result is
/// not validated against the format's rules; call [`Team::validate`].
pub fn import_team(text: &str, format: &Format) -> Result<Team, ImportError> {
    import_sheet(text, format).map(|s| s.team)
}

/// Parses a team in export format, or in packed format when the text is a
/// single line containing `|` once the empty lines before and after it are
/// dropped (Showdown's `Teams.import`).
pub fn import_sheet(text: &str, format: &Format) -> Result<TeamSheet, ImportError> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    let first = lines.iter().position(|l| !l.is_empty()).unwrap_or(lines.len());
    let last = lines.iter().rposition(|l| !l.is_empty()).map_or(first, |i| i + 1);
    if last - first == 1 && lines[first].contains('|') {
        return unpack_team(lines[first], format);
    }
    lines.truncate(last);
    let mut sheet = TeamSheet::default();
    let mut current: Option<(PokemonSet, SetExtras)> = None;
    for (n, raw) in lines.iter().enumerate().skip(first) {
        let err = |message: String| ImportError { line: n + 1, message };
        let line = js::trim(raw);
        if line.is_empty() || line == "---" {
            if let Some((set, extras)) = current.take() {
                sheet.team.sets.push(set);
                sheet.extras.push(extras);
            }
            continue;
        }
        if line.starts_with("===") {
            continue;
        }
        match current.as_mut() {
            None => {
                if sheet.team.sets.len() == MAX_SETS {
                    return Err(err(format!("more than {MAX_SETS} sets")));
                }
                current = Some(parse_header(line, format).map_err(err)?);
            }
            Some((set, extras)) => parse_line(line, set, extras, format).map_err(err)?,
        }
    }
    if let Some((set, extras)) = current {
        sheet.team.sets.push(set);
        sheet.extras.push(extras);
    }
    Ok(sheet)
}

/// A set with the format's default investment.
fn new_set(species: SpeciesId, format: &Format) -> PokemonSet {
    let mut set = PokemonSet::new(species);
    if format.stat_system == StatSystem::StatPoints {
        set.investment = Investment::StatPoints(Stats::splat(0));
    }
    set
}

/// `parseExportedTeamLine`'s first line: `Nickname (Species) (M) @ Item`.
fn parse_header(line: &str, format: &Format) -> Result<(PokemonSet, SetExtras), String> {
    let dex = format.dex;
    // `[line, item] = line.split(' @ ')`.
    let mut parts = line.split(" @ ");
    let mut line = parts.next().unwrap_or("");
    let item = parts.next();
    let mut gender = None;
    if let Some(l) = line.strip_suffix(" (M)") {
        gender = Some(Gender::Male);
        line = l;
    }
    if let Some(l) = line.strip_suffix(" (F)") {
        gender = Some(Gender::Female);
        line = l;
    }
    let mut extras = SetExtras::default();
    let species_name = match line.strip_suffix(')').filter(|_| line.contains('(')) {
        Some(inner) => {
            // `[name, species] = line.slice(0, -1).split('(')`.
            let mut parts = inner.split('(');
            extras.name = js::trim(parts.next().unwrap_or("")).to_string();
            parts.next().unwrap_or("")
        }
        None => line,
    };
    let species =
        dex.species_by_name(species_name).ok_or_else(|| format!("unknown species {}", quoted(species_name)))?;
    let mut set = new_set(species, format);
    set.gender = gender;
    if let Some(item) = item.filter(|i| !i.is_empty() && pikapro_data::to_id(i) != "noitem") {
        set.item = Some(dex.item_by_name(item).ok_or_else(|| format!("unknown item {}", quoted(item)))?);
    }
    Ok((set, extras))
}

fn parse_line(line: &str, set: &mut PokemonSet, extras: &mut SetExtras, format: &Format) -> Result<(), String> {
    let dex = format.dex;
    if let Some(v) = line.strip_prefix("Ability: ").or_else(|| line.strip_prefix("Trait: ")) {
        set.ability = dex.ability_by_name(v).ok_or_else(|| format!("unknown ability {}", quoted(v)))?;
    } else if line == "Shiny: Yes" {
        extras.shiny = true;
    } else if let Some(v) = line.strip_prefix("Level: ") {
        // `set.level = +line`; the validator makes a falsy level its
        // default (100), and so does the `Pokemon` constructor.
        let level = js::number(v);
        set.level = if level == 0.0 || level.is_nan() { 100 } else { small(level, "level")? };
    } else if let Some(v) = line.strip_prefix("Happiness: ") {
        extras.happiness = Some(small(js::number(v), "happiness")?);
    } else if let Some(v) = line.strip_prefix("Pokeball: ") {
        extras.pokeball = v.to_string();
    } else if let Some(v) = line.strip_prefix("Hidden Power: ") {
        extras.hp_type = Some(type_named(v)?);
    } else if let Some(v) = line.strip_prefix("Tera Type: ") {
        set.tera_type = Some(type_named(v)?);
    } else if line == "Gigantamax: Yes" {
        extras.gigantamax = true;
    } else if line.starts_with("Dynamax Level: ") {
        // Showdown's import ignores it (only the packed format keeps it).
    } else if let Some(v) = line.strip_prefix("EVs: ") {
        let spread = parse_spread(v, 0)?;
        set.investment = match set.investment {
            Investment::Evs { ivs, .. } => Investment::Evs { evs: spread, ivs },
            // Showdown's `champions` mod reads Stat Points from `EVs`.
            Investment::StatPoints(_) => Investment::StatPoints(spread),
        };
    } else if let Some(v) = line.strip_prefix("SPs: ") {
        if format.stat_system != StatSystem::StatPoints {
            return Err("SPs given for an EV format".into());
        }
        set.investment = Investment::StatPoints(parse_spread(v, 0)?);
    } else if let Some(v) = line.strip_prefix("IVs: ") {
        let ivs = parse_spread(v, 31)?;
        if let Investment::Evs { evs, .. } = set.investment {
            set.investment = Investment::Evs { evs, ivs };
        }
    } else if let Some(name) = nature_line(line) {
        // `if (line !== 'undefined') set.nature = line`.
        if name != "undefined" {
            set.nature = Nature::from_name(name).ok_or_else(|| format!("unknown nature {}", quoted(name)))?;
        }
    } else if let Some(mv) = line.strip_prefix('-').or_else(|| line.strip_prefix('~')) {
        let mut name = mv.strip_prefix(' ').unwrap_or(mv).to_string();
        if let Some(rest) = name.strip_prefix("Hidden Power [") {
            // `'Hidden Power ' + line.slice(14, -1)`: the last character
            // goes, whether or not it is `]`.
            let mut ty = rest.chars();
            ty.next_back();
            name = format!("Hidden Power {}", ty.as_str());
        }
        // Showdown's import: Frustration without a happiness means 0.
        if name == "Frustration" && extras.happiness.is_none() {
            extras.happiness = Some(0);
        }
        let id = dex.move_by_name(&name).ok_or_else(|| format!("unknown move {}", quoted(&name)))?;
        if set.moves.is_full() {
            return Err("more than 4 moves".into());
        }
        set.moves.push(id);
    } else {
        return Err(format!("unrecognised line {}", quoted(line)));
    }
    Ok(())
}

/// A value in an error message: quoted, and cut to 40 characters (inputs
/// can be megabytes long).
fn quoted(value: &str) -> String {
    match value.char_indices().nth(40) {
        Some((i, _)) => format!("{:?}...", &value[..i]),
        None => format!("{value:?}"),
    }
}

/// A number a set keeps in a byte: a whole number from 0 to 255.
fn small(v: f64, what: &str) -> Result<u8, String> {
    if v.fract() == 0.0 && (0.0..=255.0).contains(&v) {
        Ok(v as u8)
    } else {
        Err(format!("bad {what} {}", js::number_to_string(v)))
    }
}

/// `/^[A-Za-z]+ (N|n)ature/`: the nature's name.
fn nature_line(line: &str) -> Option<&str> {
    let letters = line.bytes().take_while(u8::is_ascii_alphabetic).count();
    let rest = &line[letters..];
    (letters > 0 && (rest.starts_with(" Nature") || rest.starts_with(" nature"))).then(|| &line[..letters])
}

fn type_named(name: &str) -> Result<Type, String> {
    Type::from_name(name).ok_or_else(|| format!("unknown type {}", quoted(name)))
}

/// `Dex.stats.getID`: `HP`, `Atk`, `SpA`, `Spe`, `Attack`, `Special
/// Defense`, `SpAtk`, ... (and `Spd`, case-sensitively, is Speed).
fn stat_named(name: &str) -> Option<Stat> {
    if name == "Spd" {
        return Some(Stat::Spe);
    }
    Some(match pikapro_data::NameId::new(name)?.as_str() {
        "hp" | "hitpoints" => Stat::Hp,
        "atk" | "attack" => Stat::Atk,
        "def" | "defense" => Stat::Def,
        "spa" | "specialattack" | "spatk" | "spattack" | "specialatk" | "special" | "spc" => Stat::SpA,
        "spd" | "specialdefense" | "spdef" | "spdefense" | "specialdef" => Stat::SpD,
        "spe" | "speed" => Stat::Spe,
        _ => return None,
    })
}

/// `EVs:` / `IVs:` as `parseExportedTeamLine` reads them: every stat
/// `default`, then for each `/`-separated entry `[value, stat] =
/// entry.trim().split(' ')`, skipped if `stat` names no stat, else
/// `parseInt(value)` (`NaN`: 0 for EVs, 31 for IVs; `set.evs[stat] || 0`
/// is what the battle reads).
fn parse_spread(text: &str, default: u8) -> Result<Stats<u8>, String> {
    let mut stats = Stats::splat(default);
    for entry in text.split('/') {
        let mut words = js::trim(entry).split(' ');
        let value = words.next().unwrap_or("");
        let Some(stat) = words.next().and_then(stat_named) else { continue };
        stats[stat] = match js::parse_int(value, 0) {
            None => default,
            Some(v) => small(v, "stat value")?,
        };
    }
    Ok(stats)
}

/// Writes a team with its extras in export format, as `Teams.export`
/// writes it (without its trailing spaces). Stat Points are written as
/// `EVs:`, as Showdown writes and reads them.
pub fn export_sheet(sheet: &TeamSheet, format: &Format) -> String {
    let dex = format.dex;
    let mut out = String::new();
    for (i, set) in sheet.team.sets.iter().enumerate() {
        let extras = sheet.extras(i);
        if i > 0 {
            out.push('\n');
        }
        let species = dex.species(set.species).name;
        if !extras.name.is_empty() && extras.name != species {
            let _ = write!(out, "{} ({species})", extras.name);
        } else {
            out.push_str(species);
        }
        if let Some(g @ (Gender::Male | Gender::Female)) = set.gender {
            let _ = write!(out, " ({})", g.letter());
        }
        if let Some(item) = set.item {
            let _ = write!(out, " @ {}", dex.item(item).name);
        }
        out.push('\n');
        let _ = writeln!(out, "Ability: {}", dex.ability(set.ability).name);
        if set.level != 100 {
            let _ = writeln!(out, "Level: {}", set.level);
        }
        if extras.shiny {
            out.push_str("Shiny: Yes\n");
        }
        if let Some(h) = extras.happiness.filter(|&h| h != 255) {
            let _ = writeln!(out, "Happiness: {h}");
        }
        if !extras.pokeball.is_empty() {
            let _ = writeln!(out, "Pokeball: {}", extras.pokeball);
        }
        if let Some(t) = extras.hp_type {
            let _ = writeln!(out, "Hidden Power: {}", t.name());
        }
        if let Some(d) = extras.dynamax_level.filter(|&d| d != 10) {
            let _ = writeln!(out, "Dynamax Level: {d}");
        }
        if extras.gigantamax {
            out.push_str("Gigantamax: Yes\n");
        }
        if let Some(t) = set.tera_type {
            let _ = writeln!(out, "Tera Type: {}", t.name());
        }
        let (evs, ivs) = match set.investment {
            Investment::Evs { evs, ivs } => (evs, Some(ivs)),
            Investment::StatPoints(sp) => (sp, None),
        };
        write_spread(&mut out, "EVs", &evs, 0);
        let _ = writeln!(out, "{} Nature", set.nature.name());
        if let Some(ivs) = ivs {
            write_spread(&mut out, "IVs", &ivs, 31);
        }
        for &m in set.moves.iter() {
            let name = dex.move_(m).name;
            match name.strip_prefix("Hidden Power ") {
                Some(ty) if !ty.starts_with('[') => {
                    let _ = writeln!(out, "- Hidden Power [{ty}]");
                }
                _ => {
                    let _ = writeln!(out, "- {name}");
                }
            }
        }
    }
    out
}

/// `Dex.stats.shortNames`.
const STAT_NAMES: [&str; 6] = ["HP", "Atk", "Def", "SpA", "SpD", "Spe"];

fn write_spread(out: &mut String, label: &str, stats: &Stats<u8>, default: u8) {
    let parts: Vec<String> = Stat::ALL
        .iter()
        .filter(|&&s| stats[s] != default)
        .map(|&s| format!("{} {}", stats[s], STAT_NAMES[s as usize]))
        .collect();
    if !parts.is_empty() {
        let _ = writeln!(out, "{label}: {}", parts.join(" / "));
    }
}

/// `Teams.packName`: the name without anything but ASCII letters and digits.
pub fn pack_name(name: &str) -> String {
    name.chars().filter(char::is_ascii_alphanumeric).collect()
}

/// `Teams.unpackName`'s guess for a name the dex doesn't have:
/// `name.replace(/([0-9]+)/g, ' $1 ').replace(/([A-Z])/g, ' $1')
/// .replace(/[ ][ ]/g, ' ').trim()`.
pub fn unpack_name_guess(name: &str) -> String {
    let mut spaced = String::with_capacity(name.len() + 8);
    let mut in_digits = false;
    for c in name.chars() {
        let digit = c.is_ascii_digit();
        if digit != in_digits {
            spaced.push(' ');
        }
        in_digits = digit;
        if c.is_ascii_uppercase() {
            spaced.push(' ');
        }
        spaced.push(c);
    }
    if in_digits {
        spaced.push(' ');
    }
    // `.replace(/[ ][ ]/g, ' ')`: each pair once, left to right.
    let mut out = String::with_capacity(spaced.len());
    let mut chars = spaced.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == ' ' && chars.peek() == Some(&' ') {
            chars.next();
        }
    }
    js::trim(&out).to_string()
}

/// Showdown's packed format (`Teams.pack`), sets separated by `]`:
/// `name|species|item|ability|moves|nature|evs|gender|ivs|shiny|level|happiness,hptype,pokeball,gigantamax,dynamaxlevel,teratype`.
/// Stat Points go in the EV field.
pub fn pack_team(sheet: &TeamSheet, format: &Format) -> String {
    let dex = format.dex;
    let mut buf = String::new();
    for (i, set) in sheet.team.sets.iter().enumerate() {
        let extras = sheet.extras(i);
        if i > 0 {
            buf.push(']');
        }
        let species = dex.species(set.species).name;
        let name = if extras.name.is_empty() { species } else { extras.name.as_str() };
        buf.push_str(name);
        let id = pack_name(species);
        let _ = write!(buf, "|{}", if pack_name(name) == id { "" } else { &id });
        let _ = write!(buf, "|{}", set.item.map_or(String::new(), |it| pack_name(dex.item(it).name)));
        let _ = write!(buf, "|{}", pack_name(dex.ability(set.ability).name));
        let moves: Vec<String> = set.moves.iter().map(|&m| pack_name(dex.move_(m).name)).collect();
        let _ = write!(buf, "|{}", moves.join(","));
        let _ = write!(buf, "|{}", set.nature.name());
        let (evs, ivs) = match set.investment {
            Investment::Evs { evs, ivs } => (evs, ivs),
            Investment::StatPoints(sp) => (sp, Stats::splat(31)),
        };
        let _ = write!(buf, "|{}", pack_spread(&evs, 0));
        let _ = write!(buf, "|{}", set.gender.map_or("", Gender::letter));
        let _ = write!(buf, "|{}", pack_spread(&ivs, 31));
        let _ = write!(buf, "|{}", if extras.shiny { "S" } else { "" });
        buf.push('|');
        if set.level != 100 {
            let _ = write!(buf, "{}", set.level);
        }
        buf.push('|');
        if let Some(h) = extras.happiness.filter(|&h| h != 255) {
            let _ = write!(buf, "{h}");
        }
        let dynamax = extras.dynamax_level.filter(|&d| d != 10);
        if !extras.pokeball.is_empty()
            || extras.hp_type.is_some()
            || extras.gigantamax
            || dynamax.is_some()
            || set.tera_type.is_some()
        {
            let _ = write!(
                buf,
                ",{},{},{},{},{}",
                extras.hp_type.map_or("", Type::name),
                pack_name(&extras.pokeball),
                if extras.gigantamax { "G" } else { "" },
                dynamax.map_or(String::new(), |d| d.to_string()),
                set.tera_type.map_or("", Type::name),
            );
        }
    }
    buf
}

/// `hp,atk,def,spa,spd,spe` with `default` values left empty; empty if all
/// are the default.
fn pack_spread(stats: &Stats<u8>, default: u8) -> String {
    if Stat::ALL.iter().all(|&s| stats[s] == default) {
        return String::new();
    }
    let parts: Vec<String> =
        Stat::ALL.iter().map(|&s| if stats[s] == default { String::new() } else { stats[s].to_string() }).collect();
    parts.join(",")
}

/// One set of Showdown's packed format as `Teams.unpack` splits it, each
/// field as written. Its methods read the fields as `Teams.unpack` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackedSet<'a> {
    pub name: &'a str,
    pub species: &'a str,
    pub item: &'a str,
    pub ability: &'a str,
    pub moves: &'a str,
    pub nature: &'a str,
    pub evs: &'a str,
    pub gender: &'a str,
    pub ivs: &'a str,
    pub shiny: &'a str,
    pub level: &'a str,
    /// `happiness,hpType,pokeball,gigantamax,dynamaxLevel,teraType`, when
    /// not empty (Showdown's `misc`).
    pub misc: Option<&'a str>,
}

impl<'a> PackedSet<'a> {
    /// `moves.split(',', 24)`, empty names included.
    pub fn moves(&self) -> impl Iterator<Item = &'a str> + use<'a> {
        self.moves.split(',').take(24)
    }

    /// `set.evs`: `Number(evs[i]) || 0` per stat; `None` when the field is
    /// empty.
    pub fn evs(&self) -> Option<[f64; 6]> {
        spread(self.evs, |v| or_zero(v.map_or(f64::NAN, js::number)))
    }

    /// `set.ivs`: 31 for an empty entry, else `Number(ivs[i]) || 0` (so a
    /// missing entry is 0); `None` when the field is empty.
    pub fn ivs(&self) -> Option<[f64; 6]> {
        spread(self.ivs, |v| match v {
            Some("") => 31.0,
            v => or_zero(v.map_or(f64::NAN, js::number)),
        })
    }

    /// `set.level`: `parseInt(level)` (`NaN` possible); `None` when the
    /// field is empty.
    pub fn level(&self) -> Option<f64> {
        (!self.level.is_empty()).then(|| js::parse_int(self.level, 0).unwrap_or(f64::NAN))
    }

    /// `misc = field.split(',', 6)`: each entry, `None` past the end.
    pub fn misc(&self) -> Option<[Option<&'a str>; 6]> {
        let mut parts = self.misc?.split(',');
        Some(core::array::from_fn(|_| parts.next()))
    }
}

/// `Number(x) || 0`.
fn or_zero(v: f64) -> f64 {
    if v.is_nan() { 0.0 } else { v }
}

/// A packed spread (`split(',', 6)`), `None` when the field is empty.
fn spread(field: &str, value: impl Fn(Option<&str>) -> f64) -> Option<[f64; 6]> {
    if field.is_empty() {
        return None;
    }
    let mut parts = field.split(',');
    Some(core::array::from_fn(|_| value(parts.next())))
}

/// `Teams.unpack(buf)`'s reading of a packed team, `None` where it returns
/// `null` (an empty text, a set with too few fields, a `]` with no set
/// after it). As Showdown's, it reads at most 24 sets and ignores
/// the rest. (Showdown also packs a JSON array given to it first; this
/// does not.)
pub fn unpack_sets(buf: &str) -> Option<Vec<PackedSet<'_>>> {
    if buf.is_empty() {
        return None;
    }
    let mut sets = Vec::new();
    let mut i = 0;
    for _ in 0..MAX_SETS {
        let mut field = || {
            let j = i + buf[i..].find('|')?;
            let f = &buf[i..j];
            i = j + 1;
            Some(f)
        };
        let (name, species, item, ability, moves, nature) =
            (field()?, field()?, field()?, field()?, field()?, field()?);
        let (evs, gender, ivs, shiny, level) = (field()?, field()?, field()?, field()?, field()?);
        let end = buf[i..].find(']').map(|j| i + j);
        let misc = match end {
            None => (i < buf.len()).then(|| &buf[i..]),
            Some(j) => (i != j).then(|| &buf[i..j]),
        };
        sets.push(PackedSet { name, species, item, ability, moves, nature, evs, gender, ivs, shiny, level, misc });
        match end {
            None => break,
            Some(j) => i = j + 1,
        }
    }
    Some(sets)
}

/// Parses Showdown's packed format (`Teams.unpack`) into a typed team.
pub fn unpack_team(buf: &str, format: &Format) -> Result<TeamSheet, ImportError> {
    let sets = unpack_sets(buf).ok_or_else(|| ImportError { line: 1, message: "not a packed team".into() })?;
    let mut sheet = TeamSheet::default();
    for (n, packed) in sets.iter().enumerate() {
        let err = |message: String| ImportError { line: n + 1, message };
        let (set, extras) = unpack_set(packed, format).map_err(err)?;
        sheet.team.sets.push(set);
        sheet.extras.push(extras);
    }
    Ok(sheet)
}

fn unpack_set(p: &PackedSet<'_>, format: &Format) -> Result<(PokemonSet, SetExtras), String> {
    let dex: &Dex = format.dex;
    let mut extras = SetExtras { name: p.name.to_string(), ..SetExtras::default() };
    // `unpackName(species, Dex.species) || set.name`.
    let species_name = if p.species.is_empty() { p.name } else { p.species };
    let species =
        dex.species_by_name(species_name).ok_or_else(|| format!("unknown species {}", quoted(species_name)))?;
    let data = dex.species(species);
    if extras.name == data.name {
        extras.name.clear();
    }
    let mut set = new_set(species, format);
    if !p.item.is_empty() {
        set.item = Some(dex.item_by_name(p.item).ok_or_else(|| format!("unknown item {}", quoted(p.item)))?);
    }
    // `species.abilities[ability || '0'] || '!!!ERROR!!!'`.
    set.ability = match p.ability {
        "" | "0" => data.abilities.primary,
        "1" => data.abilities.secondary.ok_or("no ability in slot 1")?,
        "H" => data.abilities.hidden.ok_or("no hidden ability")?,
        "S" => data.abilities.special.ok_or("no special ability")?,
        name => dex.ability_by_name(name).ok_or_else(|| format!("unknown ability {}", quoted(name)))?,
    };
    for name in p.moves().filter(|m| !m.is_empty()) {
        let id = dex.move_by_name(name).ok_or_else(|| format!("unknown move {}", quoted(name)))?;
        if set.moves.is_full() {
            return Err("more than 4 moves".into());
        }
        set.moves.push(id);
    }
    if !p.nature.is_empty() {
        set.nature = Nature::from_name(p.nature).ok_or_else(|| format!("unknown nature {}", quoted(p.nature)))?;
    }
    let bytes = |values: [f64; 6]| -> Result<Stats<u8>, String> {
        let mut stats = Stats::splat(0);
        for (s, v) in Stat::ALL.into_iter().zip(values) {
            stats[s] = small(v, "stat value")?;
        }
        Ok(stats)
    };
    if let Some(evs) = p.evs() {
        let evs = bytes(evs)?;
        set.investment = match set.investment {
            Investment::Evs { ivs, .. } => Investment::Evs { evs, ivs },
            Investment::StatPoints(_) => Investment::StatPoints(evs),
        };
    }
    // Anything else is kept by Showdown but means nothing to its `Pokemon`
    // constructor.
    set.gender = Gender::from_letter(p.gender);
    if let Some(ivs) = p.ivs() {
        let ivs = bytes(ivs)?;
        if let Investment::Evs { evs, .. } = set.investment {
            set.investment = Investment::Evs { evs, ivs };
        }
    }
    extras.shiny = !p.shiny.is_empty();
    // A falsy level is the default one, for Showdown's validator and battle.
    if let Some(level) = p.level().filter(|l| *l != 0.0 && !l.is_nan()) {
        set.level = small(level, "level")?;
    }
    if let Some(misc) = p.misc() {
        let given = |i: usize| misc[i].filter(|m| !m.is_empty());
        if let Some(h) = given(0) {
            extras.happiness = Some(small(js::number(h), "happiness")?);
        }
        if let Some(t) = given(1) {
            extras.hp_type = Some(type_named(t)?);
        }
        if let Some(ball) = given(2) {
            // `unpackName(pokeball, Dex.items)`.
            extras.pokeball = match dex.item_by_name(ball) {
                Some(it) => dex.item(it).name.to_string(),
                None => unpack_name_guess(ball),
            };
        }
        extras.gigantamax = given(3).is_some();
        if let Some(d) = given(4) {
            extras.dynamax_level = Some(small(js::number(d), "dynamax level")?);
        }
        if let Some(t) = given(5) {
            set.tera_type = Some(type_named(t)?);
        }
    }
    Ok((set, extras))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro::testing::sample_team;
    use pikapro_data::gen9::{abilities, items, moves, species};

    const PASTE: &str = "\
Incineroar @ Sitrus Berry
Ability: Intimidate
Level: 50
Tera Type: Ghost
EVs: 252 HP / 4 Atk / 252 SpD
IVs: 0 Spe
Careful Nature
- Knock Off
- Protect

Pikachu
Ability: Static
- Thunderbolt
";

    #[test]
    fn imports_paste() {
        let format = Format::gen9_vgc();
        let team = import_team(PASTE, &format).unwrap();
        assert_eq!(team.sets.len(), 2);
        let inc = &team.sets[0];
        assert_eq!(format.dex.species(inc.species).name, "Incineroar");
        assert_eq!(inc.level, 50);
        assert_eq!(inc.nature, Nature::Careful);
        assert_eq!(inc.tera_type, Some(Type::Ghost));
        let Investment::Evs { evs, ivs } = inc.investment else { panic!() };
        assert_eq!(evs, Stats::new(252, 4, 0, 0, 252, 0));
        assert_eq!(ivs, Stats::new(31, 31, 31, 31, 31, 0));
        assert_eq!(inc.moves.len(), 2);
        assert_eq!(inc.gender, None);
        assert_eq!(team.sets[1].level, 100);
    }

    #[test]
    fn round_trips_every_format() {
        for format in Format::presets() {
            let team = sample_team(format);
            let text = export_sheet(&TeamSheet::new(team.clone()), format);
            assert_eq!(import_team(&text, format).unwrap(), team, "{}:\n{text}", format.id);
            let sheet = TeamSheet::new(team);
            assert_eq!(unpack_team(&pack_team(&sheet, format), format).unwrap(), sheet, "{}", format.id);
        }
    }

    const FULL: &str = "\
Chompy (Garchomp) (F) @ Choice Scarf
Ability: Rough Skin
Level: 50
Shiny: Yes
Happiness: 160
Pokeball: Great Ball
Hidden Power: Ice
Gigantamax: Yes
Tera Type: Ground
EVs: 252 Atk / 4 SpD / 252 Spe
Jolly Nature
IVs: 0 SpA
- Earthquake
- Hidden Power [Fire]
- Frustration
";

    #[test]
    fn imports_every_field() {
        let format = Format::gen9_singles();
        let sheet = import_sheet(FULL, &format).unwrap();
        let (set, extras) = (&sheet.team.sets[0], &sheet.extras[0]);
        assert_eq!(set.species, species::GARCHOMP);
        assert_eq!(set.gender, Some(Gender::Female));
        assert_eq!(set.item, Some(items::CHOICE_SCARF));
        assert_eq!(set.moves[1], moves::HIDDEN_POWER_FIRE);
        assert_eq!(extras.name, "Chompy");
        assert!(extras.shiny && extras.gigantamax);
        assert_eq!(extras.happiness, Some(160));
        assert_eq!(extras.pokeball, "Great Ball");
        assert_eq!(extras.hp_type, Some(Type::Ice));
        assert_eq!(export_sheet(&sheet, &format), FULL);
        assert_eq!(sheet.nicknames(&format), ["Chompy"]);
        let packed = pack_team(&sheet, &format);
        assert_eq!(
            packed,
            "Chompy|Garchomp|ChoiceScarf|RoughSkin|Earthquake,HiddenPowerFire,Frustration|Jolly|,252,,,4,252|F|,,,0,,|S|50|160,Ice,GreatBall,G,,Ground"
        );
        assert_eq!(unpack_team(&packed, &format).unwrap(), sheet);
        assert_eq!(import_sheet(&format!("\n{packed}\n"), &format).unwrap(), sheet);
    }

    #[test]
    fn frustration_sets_happiness_like_showdown() {
        let format = Format::gen9_singles();
        let sheet = import_sheet("Snorlax\n- Frustration\n", &format).unwrap();
        assert_eq!(sheet.extras[0].happiness, Some(0));
        let sheet = import_sheet("Snorlax\nHappiness: 7\n- Frustration\n", &format).unwrap();
        assert_eq!(sheet.extras[0].happiness, Some(7));
    }

    #[test]
    fn accepts_showdown_spellings() {
        let format = Format::gen9_singles();
        let text = "=== [gen9] Box ===\n\nGarchomp (M) @ No Item\nTrait: Rough Skin\nEVs: 4 hp / 252 Attack / 252 Spd\nadamant nature\n~Earthquake\n-Protect\n---\nGarchomp (Garchomp)\nDynamax Level: 5\n- Protect\n";
        let sheet = import_sheet(text, &format).unwrap();
        let set = &sheet.team.sets[0];
        assert_eq!((set.gender, set.item, set.nature), (Some(Gender::Male), None, Nature::Adamant));
        let Investment::Evs { evs, .. } = set.investment else { panic!() };
        assert_eq!(evs, Stats::new(4, 252, 0, 0, 0, 252));
        assert_eq!(set.moves.len(), 2);
        assert_eq!(sheet.team.sets.len(), 2);
        assert_eq!(sheet.nicknames(&format), ["", ""]);
        assert_eq!(sheet.extras[1].dynamax_level, None);
    }

    /// Expected: Showdown 0.11.11's `Teams.import`, `Teams.unpack` and the
    /// validator.
    #[test]
    fn reads_numbers_and_spreads_like_showdown() {
        let format = Format::gen9_singles();
        let evs = |text: &str| match import_team(text, &format).unwrap().sets[0].investment {
            Investment::Evs { evs, ivs } => (evs, ivs),
            Investment::StatPoints(_) => unreachable!(),
        };
        // `split(' ')`: two spaces leave the stat name empty, which is skipped.
        assert_eq!(evs("Pikachu\nEVs: 252  Atk / 4 HP\n- Protect\n").0, Stats::new(4, 0, 0, 0, 0, 0));
        // An unknown stat is skipped too; `parseInt` reads `0x10` and `12abc`.
        assert_eq!(evs("Pikachu\nEVs: 4 Atack / 0x10 Def / 12abc Spe\n- Protect\n").0, Stats::new(0, 0, 16, 0, 0, 12));
        assert_eq!(evs("Pikachu\nIVs: x Atk / 7 SpA\n- Protect\n").1, Stats::new(31, 31, 31, 7, 31, 31));
        let level =
            |l: &str| import_team(&format!("Pikachu\nLevel: {l}\n- Protect\n"), &format).map(|t| t.sets[0].level);
        assert_eq!(level("0"), Ok(100));
        assert_eq!(level("abc"), Ok(100));
        assert_eq!(level("0x32"), Ok(50));
        assert!(level("-5").is_err() && level("256").is_err() && level("5.5").is_err());
        // `Hidden Power [Fire` is "Hidden Power Fir", no move.
        assert!(import_team("Pikachu\n- Hidden Power [Fire\n", &format).is_err());
        assert!(import_team("Pikachu\nundefined Nature\n- Protect\n", &format).is_ok());

        // Packed: a missing IV is 0, `]` may be in a name, the misc field
        // keeps its first 6 entries, `S` is the special ability slot.
        let sheet = unpack_team("A]B|Pikachu|||Protect|||||||", &format).unwrap();
        assert_eq!(sheet.extras[0].name, "A]B");
        let set = |packed: &str| unpack_team(packed, &format).unwrap().team.sets.remove(0);
        let Investment::Evs { ivs, .. } = set("Pikachu||||Protect||||0,0|||").investment else { panic!() };
        assert_eq!(ivs, Stats::splat(0));
        let Investment::Evs { ivs, .. } = set("Pikachu||||Protect||||,5|||").investment else { panic!() };
        assert_eq!(ivs, Stats::new(31, 5, 0, 0, 0, 0));
        assert_eq!(set("Pikachu||||Protect|||||||,,,,,Water,extra").tera_type, Some(Type::Water));
        assert_eq!(set("Pikachu||||Protect||||||0x32|").level, 50);
        assert_eq!(set("Pikachu||||Protect||||||0|").level, 100);
        assert_eq!(set("Greninja|||S|Protect|||||||").ability, abilities::BATTLE_BOND);
        let sheet = unpack_team("Pikachu||||Protect|||||||,,PremierBall", &format).unwrap();
        assert_eq!(sheet.extras[0].pokeball, "Premier Ball");
        let sheet = unpack_team("Pikachu||||Protect|||||||,,FooBall2", &format).unwrap();
        assert_eq!(sheet.extras[0].pokeball, "Foo Ball 2");
        // A trailing `]` is `null`.
        assert!(unpack_team("Pikachu||||Protect|||||||]", &format).is_err());
    }

    #[test]
    fn packed_detection_drops_only_empty_lines() {
        let format = Format::gen9_singles();
        let packed = "Pikachu||||Protect|||||||";
        assert!(import_sheet(&format!("\n\n{packed}\n\n"), &format).is_ok());
        // A line of white space is not empty: this is export format.
        assert!(import_sheet(&format!(" \n{packed}"), &format).is_err());
    }

    #[test]
    fn teams_are_capped_at_24_sets() {
        let format = Format::gen9_singles();
        let set = "Pikachu||||Protect|||||||";
        let packed = vec![set; 30].join("]");
        assert_eq!(unpack_team(&packed, &format).unwrap().team.sets.len(), MAX_SETS);
        let text = vec!["Pikachu\n- Protect\n"; 25].join("\n");
        assert!(import_sheet(&text, &format).is_err());
        assert!(import_sheet(&vec!["Pikachu\n- Protect\n"; 24].join("\n"), &format).is_ok());
    }

    #[test]
    fn stat_points_read_from_evs_too() {
        let format = Format::champions_vgc();
        let a = import_team("Garchomp\nSPs: 32 Atk / 2 HP\n- Protect\n", &format).unwrap();
        let b = import_team("Garchomp\nEVs: 32 Atk / 2 HP\n- Protect\n", &format).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.sets[0].investment, Investment::StatPoints(Stats::new(2, 32, 0, 0, 0, 0)));
        assert!(export_sheet(&TeamSheet::new(a), &format).contains("EVs: 2 HP / 32 Atk\n"));
    }

    #[test]
    fn nicknames_follow_the_pokemon_constructor() {
        let format = Format::gen9_singles();
        let text = "Rotom (Rotom-Wash)\n- Protect\n\nA Very Long Nickname Indeed (Pikachu)\n- Protect\n\nPikachu (Pikachu)\n- Protect\n\nA|B (Pikachu)\n- Protect\n\n1234567890123456789\u{1f600} (Pikachu)\n- Protect\n";
        let sheet = import_sheet(text, &format).unwrap();
        assert_eq!(sheet.extras[3].name, "A|B");
        assert_eq!(sheet.nicknames(&format), ["", "A Very Long Nickname", "", "A B", "1234567890123456789\u{fffd}"]);
    }

    #[test]
    fn unpacks_ability_slots_and_genderless() {
        let format = Format::gen9_singles();
        assert!(unpack_team("Garchomp||||Protect|", &format).is_err());
        let sheet = unpack_team("Garchomp|||H|Protect|||N||||", &format).unwrap();
        let set = &sheet.team.sets[0];
        assert_eq!(set.ability, abilities::ROUGH_SKIN);
        assert_eq!(set.gender, Some(Gender::Genderless));
    }

    #[test]
    fn reports_errors_with_lines() {
        let format = Format::gen9_vgc();
        let e = import_team("Pikachu\n- Tackle\n- Not A Move\n", &format).unwrap_err();
        assert_eq!(e.line, 3);
        assert!(import_team("Missingno\n", &format).is_err());
        assert!(import_team("Pikachu\nSPs: 32 Atk\n", &format).is_err());
        assert!(import_team("Pikachu\nWeird: line\n", &format).is_err());
    }
}
