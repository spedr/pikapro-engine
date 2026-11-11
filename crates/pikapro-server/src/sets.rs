//! Showdown's set objects, as the battle keeps them in `side.team`:
//! `Teams.unpack` and `Teams.pack` (`sim/teams.ts`) over [`Json`] objects,
//! and the changes the `Pokemon` constructor makes to its set (the name,
//! the level, the EVs and IVs). The `end` log and `>requestteam` show
//! them; the engine's team is read back from their packed form.

use pikapro_data::gen9::DEX;
use pikapro_data::{Dex, SpeciesData, Stat};
use pikapro_protocol::team::{pack_name, unpack_name_guess, unpack_sets};

use crate::json::{Json, js_number};

/// `Teams.unpack(buf)`: `None` where Showdown returns `null`. A JSON
/// array in a string is packed first, as Showdown does. The fields are
/// split by pikapro-protocol's [`unpack_sets`] (at most 24 sets).
pub fn unpack(buf: &str) -> Option<Vec<Json>> {
    if buf.is_empty() {
        return None;
    }
    let owned;
    let mut buf = buf;
    if buf.starts_with('[') && buf.ends_with(']') {
        let team = Json::parse(buf).ok()?;
        owned = pack(team.as_array()?).ok()?;
        buf = &owned;
    }
    let dex: &Dex = &DEX;
    let mut team = Vec::new();
    for p in unpack_sets(buf)? {
        let mut set = Json::object();
        set.set("name", Json::str(p.name));
        let species = unpack_name(p.species, Table::Species);
        set.set("species", Json::str(if species.is_empty() { p.name.to_string() } else { species }));
        set.set("item", Json::str(unpack_name(p.item, Table::Items)));
        let species_name = set.get("species").and_then(Json::as_str).unwrap_or("");
        let ability = if ["", "0", "1", "H", "S"].contains(&p.ability) {
            // `species.abilities[ability || '0'] || (ability === '' ? '' : '!!!ERROR!!!')`.
            let data = species_by_name(dex, species_name);
            let slot = data.and_then(|s| match p.ability {
                "" | "0" => Some(s.abilities.primary),
                "1" => s.abilities.secondary,
                "H" => s.abilities.hidden,
                _ => s.abilities.special,
            });
            match slot {
                Some(a) => dex.ability(a).name.to_string(),
                None if p.ability.is_empty() => String::new(),
                None => "!!!ERROR!!!".into(),
            }
        } else {
            unpack_name(p.ability, Table::Abilities)
        };
        set.set("ability", Json::str(ability));
        set.set("moves", Json::Array(p.moves().map(|m| Json::str(unpack_name(m, Table::Moves))).collect()));
        set.set("nature", Json::str(unpack_name(p.nature, Table::Natures)));
        if let Some(evs) = p.evs() {
            set.set("evs", spread(evs));
        }
        if !p.gender.is_empty() {
            set.set("gender", Json::str(p.gender));
        }
        if let Some(ivs) = p.ivs() {
            set.set("ivs", spread(ivs));
        }
        if !p.shiny.is_empty() {
            set.set("shiny", Json::Bool(true));
        }
        if let Some(level) = p.level() {
            set.set("level", Json::Number(level));
        }
        if let Some(misc) = p.misc() {
            let m = |k: usize| misc[k].unwrap_or("");
            set.set("happiness", Json::Number(if m(0).is_empty() { 255.0 } else { js_number(m(0)) }));
            set.set("hpType", Json::str(m(1)));
            set.set("pokeball", Json::str(unpack_name(m(2), Table::Items)));
            set.set("gigantamax", Json::Bool(!m(3).is_empty()));
            set.set("dynamaxLevel", Json::Number(if m(4).is_empty() { 10.0 } else { js_number(m(4)) }));
            if let Some(tera) = misc[5] {
                set.set("teraType", Json::str(tera));
            }
        }
        team.push(set);
    }
    Some(team)
}

fn spread(values: [f64; 6]) -> Json {
    let mut out = Json::object();
    for (stat, v) in Stat::ALL.into_iter().zip(values) {
        out.set(stat.id(), Json::Number(v));
    }
    out
}

#[derive(Clone, Copy)]
enum Table {
    Species,
    Items,
    Abilities,
    Moves,
    Natures,
}

fn species_by_name<'a>(dex: &'a Dex, name: &str) -> Option<&'a SpeciesData> {
    (!name.is_empty()).then(|| dex.species_by_name(name)).flatten().map(|id| dex.species(id))
}

/// `Teams.unpackName`: the dex's name for it if it exists, or a readable
/// guess ([`unpack_name_guess`]).
fn unpack_name(name: &str, table: Table) -> String {
    if name.is_empty() {
        return String::new();
    }
    let dex: &Dex = &DEX;
    let found = match table {
        Table::Species => dex.species_by_name(name).map(|id| dex.species(id).name),
        Table::Items => dex.item_by_name(name).map(|id| dex.item(id).name),
        Table::Abilities => dex.ability_by_name(name).map(|id| dex.ability(id).name),
        Table::Moves => dex.move_by_name(name).map(|id| dex.move_(id).name),
        Table::Natures => pikapro_data::Nature::from_name(name).map(|n| n.name()),
    };
    match found.filter(|n| !n.is_empty()) {
        Some(found) => found.to_string(),
        None => unpack_name_guess(name),
    }
}

/// A JavaScript value as `${value}`, `undefined` for a missing key.
fn js_str(v: Option<&Json>) -> String {
    v.map_or("undefined".into(), Json::to_js_string)
}

/// `packName(value)`: `''` for a falsy value.
fn pack_value(v: Option<&Json>) -> String {
    match v {
        Some(v) if v.truthy() => pack_name(&v.to_js_string()),
        _ => String::new(),
    }
}

/// `a || b`.
fn or<'a>(a: Option<&'a Json>, b: Option<&'a Json>) -> Option<&'a Json> {
    if a.is_some_and(Json::truthy) { a } else { b }
}

/// `Teams.pack(team)`. Fails where Showdown would throw (a set that is
/// not an object, moves that are not an array).
pub fn pack(team: &[Json]) -> Result<String, String> {
    let mut buf = String::new();
    for set in team {
        if !matches!(set, Json::Object(_)) {
            return Err("Teams.pack: a set is not an object".into());
        }
        if !buf.is_empty() {
            buf.push(']');
        }
        let (name, species) = (set.get("name"), set.get("species"));
        buf.push_str(&js_str(or(name, species)));
        let id = pack_value(or(species, name));
        buf.push('|');
        if pack_value(or(name, species)) != id {
            buf.push_str(&id);
        }
        buf.push('|');
        buf.push_str(&pack_value(set.get("item")));
        buf.push('|');
        buf.push_str(&pack_value(set.get("ability")));
        buf.push('|');
        let moves = set.get("moves").and_then(Json::as_array).ok_or("Teams.pack: moves is not an array")?;
        let moves: Vec<String> = moves.iter().map(|m| pack_value(Some(m))).collect();
        buf.push_str(&moves.join(","));
        buf.push('|');
        if let Some(n) = set.get("nature").filter(|n| n.truthy()) {
            buf.push_str(&n.to_js_string());
        }
        let stat = |spread: &Json, s: &str, iv: bool| -> String {
            let v = spread.get(s);
            if iv {
                match v {
                    None => String::new(),
                    Some(Json::Number(n)) if *n == 31.0 => String::new(),
                    Some(v) => v.to_js_string(),
                }
            } else {
                match v {
                    Some(v) if v.truthy() => v.to_js_string(),
                    _ => String::new(),
                }
            }
        };
        let spread_field = |key: &str, iv: bool| -> String {
            match set.get(key).filter(|s| s.truthy()) {
                Some(s) => {
                    let parts: Vec<String> = Stat::ALL.iter().map(|k| stat(s, k.id(), iv)).collect();
                    let joined = parts.join(",");
                    if joined == ",,,,," { String::new() } else { joined }
                }
                None => String::new(),
            }
        };
        buf.push('|');
        buf.push_str(&spread_field("evs", false));
        buf.push('|');
        if let Some(g) = set.get("gender").filter(|g| g.truthy()) {
            buf.push_str(&g.to_js_string());
        }
        buf.push('|');
        buf.push_str(&spread_field("ivs", true));
        buf.push('|');
        if set.get("shiny").is_some_and(Json::truthy) {
            buf.push('S');
        }
        buf.push('|');
        if let Some(l) = set.get("level").filter(|l| l.truthy() && **l != Json::Number(100.0)) {
            buf.push_str(&l.to_js_string());
        }
        buf.push('|');
        if let Some(h) = set.get("happiness").filter(|h| **h != Json::Number(255.0)) {
            buf.push_str(&h.to_js_string());
        }
        let truthy = |k: &str| set.get(k).is_some_and(Json::truthy);
        let dynamax = set.get("dynamaxLevel").filter(|d| **d != Json::Number(10.0));
        if truthy("pokeball") || truthy("hpType") || truthy("gigantamax") || dynamax.is_some() || truthy("teraType") {
            let or_empty = |k: &str| set.get(k).filter(|v| v.truthy()).map_or(String::new(), Json::to_js_string);
            buf.push(',');
            buf.push_str(&or_empty("hpType"));
            buf.push(',');
            buf.push_str(&pack_value(set.get("pokeball")));
            buf.push(',');
            if truthy("gigantamax") {
                buf.push('G');
            }
            buf.push(',');
            if let Some(d) = dynamax {
                buf.push_str(&d.to_js_string());
            }
            buf.push(',');
            buf.push_str(&or_empty("teraType"));
        }
    }
    Ok(buf)
}

/// `Utils.clampIntRange`.
fn clamp_int(v: Option<&Json>, min: f64, max: f64) -> Json {
    let n = match v {
        Some(Json::Number(n)) => n.floor(),
        _ => 0.0,
    };
    Json::Number(if n < min {
        min
    } else if n > max {
        max
    } else {
        n
    })
}

/// A generated set as Showdown's random-battle generator returns it
/// (`randomSet`'s object, its keys in its order): moves by ID, the
/// ability and item by name.
pub fn generated(set: &pikapro_teams::random_battle::RandomSet) -> Json {
    let dex = &pikapro_data::gen9::DEX;
    let stats = |s: &pikapro_data::Stats<u8>| {
        let mut o = Json::object();
        for k in Stat::ALL {
            o.set(k.id(), Json::Number(f64::from(s[k])));
        }
        o
    };
    let gender = match set.gender {
        pikapro::Gender::Male => "M",
        pikapro::Gender::Female => "F",
        pikapro::Gender::Genderless => "N",
    };
    let mut o = Json::object();
    o.set("name", Json::str(dex.species(set.name).name));
    o.set("species", Json::str(dex.species(set.species).name));
    o.set("speciesId", Json::str(pikapro_data::to_id(dex.species(set.species_id).name)));
    o.set("gender", Json::str(gender));
    o.set("shiny", Json::Bool(set.shiny));
    o.set("level", Json::Number(f64::from(set.level)));
    o.set("moves", Json::Array(set.moves.iter().map(|&m| Json::str(pikapro_data::to_id(dex.move_(m).name))).collect()));
    o.set("ability", Json::str(dex.ability(set.ability).name));
    o.set("evs", stats(&set.evs));
    o.set("ivs", stats(&set.ivs));
    o.set("item", Json::str(set.item.map_or("", |i| dex.item(i).name)));
    o.set("teraType", Json::str(set.tera_type.name()));
    o.set("role", Json::str(set.role));
    o
}

/// What the `Pokemon` constructor changes in its set, in its order: the
/// name (the base species' when it has none or it equals the species),
/// the level (`set.adjustLevel || set.level || 100`, within 1-9999), a
/// Hidden Power's type, then the EVs and IVs (filled in and clamped).
/// Fails as the constructor does for an unknown species or no moves.
pub fn construct(set: &mut Json, dex: &Dex) -> Result<(), String> {
    let name = set.get("name").cloned();
    let species = set.get("species").cloned();
    let species_name = or(species.as_ref(), name.as_ref()).map_or(String::new(), Json::to_js_string);
    let data = species_by_name(dex, &species_name).ok_or_else(|| format!("Unidentified species: {species_name}"))?;
    if name == species || !name.as_ref().is_some_and(Json::truthy) {
        set.set("name", Json::str(dex.species(data.base_species).name));
    }
    let level = or(set.get("adjustLevel"), set.get("level")).filter(|l| l.truthy()).cloned();
    set.set("level", clamp_int(Some(&level.unwrap_or(Json::Number(100.0))), 1.0, 9999.0));
    let moves = set.get("moves").and_then(Json::as_array).cloned().unwrap_or_default();
    if moves.is_empty() {
        let name = set.get("name").map_or(String::new(), Json::to_js_string);
        return Err(format!("Set {} has no moves", name.chars().take(20).collect::<String>()));
    }
    for m in &moves {
        let id = dex.move_by_name(&m.to_js_string());
        let Some(id) = id.filter(|_| m.truthy()) else { continue };
        let data = dex.move_(id);
        let is_hp = pack_name(data.name).to_ascii_lowercase().starts_with("hiddenpower");
        if is_hp && data.ty != pikapro_data::Type::Normal && !set.get("hpType").is_some_and(Json::truthy) {
            set.set("hpType", Json::str(data.ty.name()));
        }
    }
    const ORDER: [&str; 6] = ["hp", "atk", "def", "spe", "spa", "spd"];
    for (key, default) in [("evs", 0.0), ("ivs", 31.0)] {
        if !set.get(key).is_some_and(Json::truthy) {
            let mut s = Json::object();
            for k in Stat::ALL {
                s.set(k.id(), Json::Number(default));
            }
            set.set(key, s);
        }
    }
    for stat in ORDER {
        let evs = set.get_mut("evs").expect("set above");
        if !evs.get(stat).is_some_and(Json::truthy) {
            evs.set(stat, Json::Number(0.0));
        }
        let ivs = set.get_mut("ivs").expect("set above");
        let v = ivs.get(stat);
        if !v.is_some_and(Json::truthy) && v != Some(&Json::Number(0.0)) {
            ivs.set(stat, Json::Number(31.0));
        }
    }
    for (key, max) in [("evs", 255.0), ("ivs", 31.0)] {
        if let Some(Json::Object(entries)) = set.get_mut(key) {
            for (_, v) in entries.iter_mut() {
                *v = clamp_int(Some(v), 0.0, max);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacks_and_constructs_like_showdown() {
        let packed = "Pikachu||lightball|static|thunderbolt,quickattack|Timid|,,,252,,252||||50|";
        let mut team = unpack(packed).unwrap();
        construct(&mut team[0], &DEX).unwrap();
        assert_eq!(
            Json::Array(team.clone()).stringify(),
            r#"[{"name":"Pikachu","species":"Pikachu","item":"Light Ball","ability":"Static","moves":["Thunderbolt","Quick Attack"],"nature":"Timid","evs":{"hp":0,"atk":0,"def":0,"spa":252,"spd":0,"spe":252},"level":50,"ivs":{"hp":31,"atk":31,"def":31,"spa":31,"spd":31,"spe":31}}]"#
        );
        assert_eq!(pack(&team).unwrap(), "Pikachu||LightBall|Static|Thunderbolt,QuickAttack|Timid|,,,252,,252||||50|");
        // Expected: Showdown 0.11.11's `Teams.unpack` and `Teams.pack`.
        let nick = unpack("Sparky|Rotom-Wash||H|hydropump|||F|,0,,,,|S||,,,,,Water").unwrap();
        assert_eq!(
            Json::Array(nick.clone()).stringify(),
            r#"[{"name":"Sparky","species":"Rotom-Wash","item":"","ability":"!!!ERROR!!!","moves":["Hydro Pump"],"nature":"","gender":"F","ivs":{"hp":31,"atk":0,"def":31,"spa":31,"spd":31,"spe":31},"shiny":true,"happiness":255,"hpType":"","pokeball":"","gigantamax":false,"dynamaxLevel":10,"teraType":"Water"}]"#
        );
        assert_eq!(pack(&nick).unwrap(), "Sparky|RotomWash||ERROR|HydroPump|||F|,0,,,,|S||,,,,,Water");
        let odd = unpack("Sparky|RotomWash||0|hydropump|||F|31|S|abc|,,,,,Water]X|FooBar12x|zz|qq|a,,b|nat|1,2|||||")
            .unwrap();
        assert_eq!(
            Json::Array(odd.clone()).stringify(),
            r#"[{"name":"Sparky","species":"Rotom-Wash","item":"","ability":"Levitate","moves":["Hydro Pump"],"nature":"","gender":"F","ivs":{"hp":31,"atk":0,"def":0,"spa":0,"spd":0,"spe":0},"shiny":true,"level":null,"happiness":255,"hpType":"","pokeball":"","gigantamax":false,"dynamaxLevel":10,"teraType":"Water"},{"name":"X","species":"Foo Bar 12 x","item":"zz","ability":"qq","moves":["a","","b"],"nature":"nat","evs":{"hp":1,"atk":2,"def":0,"spa":0,"spd":0,"spe":0}}]"#
        );
        assert_eq!(
            pack(&odd).unwrap(),
            "Sparky|RotomWash||Levitate|HydroPump|||F|,0,0,0,0,0|S||,,,,,Water]X|FooBar12x|zz|qq|a,,b|nat|1,2,,,,|||||"
        );
        assert!(unpack("Pikachu|").is_none());
    }
}
