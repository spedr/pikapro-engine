//! Recorded validation cases: a team, its [`SetInfo`], and the messages
//! Showdown's validator gave for it, in a line format that needs no JSON.
//!
//! ```text
//! case championsvgc-1800042
//! format gen9championsvgc2026regmc
//! set Garchomp|Leftovers|Rough Skin|Jolly|50|2,32,0,0,0,32|31,31,31,31,31,31|Earthquake,Protect|Ground|Chompy|M|0|sp
//! expect Garchomp can't learn Spore.
//! end
//! ```
//!
//! A `set` line is `species|item|ability|nature|level|evs|ivs|moves|tera|nickname|gender|shiny|investment`
//! (`investment` is `ev` or `sp`: which [`Investment`] the stat fields are);
//! empty fields are absent values; lines starting with `# ` are comments.
//! A case may also have one `played` line per set, after its `expect`
//! lines: the set as Showdown's validator left it, which its battle plays
//! ([`played_line`]: `species|item|ability|moves|gender|level|tera`; see
//! [`Checked::team`](crate::Checked::team)).
//! The recorder is `pikapro-conformance`'s
//! `validate_fuzz --record`; `cargo test` replays the cases without Node.

use pikapro::{Investment, PokemonSet, Team};
use pikapro_data::{Dex, GenderRule, Nature, Stats, Type};

use crate::{Gender, SetInfo, ValidationFormat};

/// One recorded case.
#[derive(Clone, Debug)]
pub struct Case {
    pub name: String,
    pub format: ValidationFormat,
    pub team: Team,
    pub info: Vec<SetInfo>,
    /// Showdown's messages, in order.
    pub expect: Vec<String>,
    /// Each set as Showdown's validator left it ([`played_line`]); empty
    /// when the case wasn't recorded with them.
    pub played: Vec<String>,
}

/// A set as Showdown's validator leaves it, as a `played` line (without
/// the prefix): `species|item|ability|move,move,...|gender|level|tera`, the
/// fields validation can change that the battle reads. The gender is the
/// one the battle starts with: the set's, else the species' fixed one (`M`,
/// `F`, `N`), empty when the battle draws it; the Tera type is empty when
/// the set has none. The oracle's `validator.mjs` writes the same.
pub fn played_line(dex: &Dex, set: &PokemonSet) -> String {
    let moves: Vec<&str> = set.moves.iter().map(|&m| dex.move_(m).name).collect();
    let fixed = match dex.species(set.species).gender {
        GenderRule::Male => "M",
        GenderRule::Female => "F",
        GenderRule::Genderless => "N",
        GenderRule::Ratio { .. } => "",
    };
    [
        dex.species(set.species).name,
        set.item.map_or("", |i| dex.item(i).name),
        dex.ability(set.ability).name,
        &moves.join(","),
        set.gender.map_or(fixed, Gender::letter),
        &set.level.to_string(),
        set.tera_type.map_or("", Type::name),
    ]
    .join("|")
}

fn stats(s: &Stats<u8>) -> String {
    s.0.iter().map(u8::to_string).collect::<Vec<_>>().join(",")
}

/// A set as a `set` line (without the `set ` prefix).
pub fn set_line(dex: &Dex, set: &PokemonSet, info: &SetInfo) -> String {
    let (evs, ivs, kind) = match set.investment {
        Investment::Evs { evs, ivs } => (evs, ivs, "ev"),
        Investment::StatPoints(sp) => (sp, Stats::splat(31), "sp"),
    };
    let moves: Vec<&str> = set.moves.iter().map(|&m| dex.move_(m).name).collect();
    [
        dex.species(set.species).name.to_owned(),
        set.item.map_or(String::new(), |i| dex.item(i).name.to_owned()),
        dex.ability(set.ability).name.to_owned(),
        set.nature.name().to_owned(),
        set.level.to_string(),
        stats(&evs),
        stats(&ivs),
        moves.join(","),
        set.tera_type.map_or(String::new(), |t| t.name().to_owned()),
        info.nickname.clone().unwrap_or_default(),
        info.gender.filter(|g| *g != Gender::Genderless).map_or("", Gender::letter).to_owned(),
        if info.shiny { "1" } else { "0" }.to_owned(),
        kind.to_owned(),
    ]
    .join("|")
}

/// A case in the text format.
pub fn format_case(case: &Case) -> String {
    let mut out = format!("case {}\nformat {}\n", case.name, case.format.id);
    let default = SetInfo::default();
    for (i, set) in case.team.sets.iter().enumerate() {
        out += &format!("set {}\n", set_line(case.format.dex, set, case.info.get(i).unwrap_or(&default)));
    }
    for e in &case.expect {
        out += &format!("expect {e}\n");
    }
    for p in &case.played {
        out += &format!("played {p}\n");
    }
    out += "end\n";
    out
}

fn parse_stats(s: &str) -> Result<Stats<u8>, String> {
    let v: Vec<u8> =
        s.split(',').map(|x| x.parse().map_err(|_| format!("bad stat {x:?}"))).collect::<Result<_, _>>()?;
    let a: [u8; 6] = v.try_into().map_err(|_| format!("need 6 stats: {s:?}"))?;
    Ok(Stats(a))
}

/// Parses one `set` line.
pub fn parse_set(dex: &Dex, line: &str) -> Result<(PokemonSet, SetInfo), String> {
    let f: Vec<&str> = line.split('|').collect();
    if f.len() != 13 {
        return Err(format!("expected 13 fields: {line:?}"));
    }
    let species = dex.species_by_name(f[0]).ok_or_else(|| format!("species {:?}", f[0]))?;
    let mut set = PokemonSet::new(species);
    if !f[1].is_empty() {
        set.item = Some(dex.item_by_name(f[1]).ok_or_else(|| format!("item {:?}", f[1]))?);
    }
    set.ability = dex.ability_by_name(f[2]).ok_or_else(|| format!("ability {:?}", f[2]))?;
    set.nature = Nature::from_name(f[3]).ok_or_else(|| format!("nature {:?}", f[3]))?;
    set.level = f[4].parse().map_err(|_| format!("level {:?}", f[4]))?;
    let (evs, ivs) = (parse_stats(f[5])?, parse_stats(f[6])?);
    set.investment = match f[12] {
        "sp" => Investment::StatPoints(evs),
        _ => Investment::Evs { evs, ivs },
    };
    for m in f[7].split(',').filter(|m| !m.is_empty()) {
        set.moves.push(dex.move_by_name(m).ok_or_else(|| format!("move {m:?}"))?);
    }
    if !f[8].is_empty() {
        set.tera_type = Some(Type::from_name(f[8]).ok_or_else(|| format!("type {:?}", f[8]))?);
    }
    let info = SetInfo {
        nickname: (!f[9].is_empty()).then(|| f[9].to_owned()),
        gender: Gender::from_letter(f[10]).filter(|g| *g != Gender::Genderless),
        shiny: f[11] == "1",
    };
    Ok((set, info))
}

/// Parses a file of cases.
pub fn parse_cases(text: &str) -> Result<Vec<Case>, String> {
    let mut cases = Vec::new();
    let mut current: Option<Case> = None;
    for (n, line) in text.lines().enumerate() {
        let err = |e: String| format!("line {}: {e}", n + 1);
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        match word {
            "" | "#" => {}
            "case" => {
                current = Some(Case {
                    name: rest.to_owned(),
                    format: ValidationFormat::gen9_vgc_2025_reg_i(),
                    team: Team::default(),
                    info: Vec::new(),
                    expect: Vec::new(),
                    played: Vec::new(),
                })
            }
            "format" => {
                let c = current.as_mut().ok_or_else(|| err("format outside a case".into()))?;
                c.format = ValidationFormat::by_id(rest).ok_or_else(|| err(format!("format {rest:?}")))?;
            }
            "set" => {
                let c = current.as_mut().ok_or_else(|| err("set outside a case".into()))?;
                let (set, info) = parse_set(c.format.dex, rest).map_err(err)?;
                c.team.sets.push(set);
                c.info.push(info);
            }
            "expect" => {
                current.as_mut().ok_or_else(|| err("expect outside a case".into()))?.expect.push(rest.to_owned())
            }
            "played" => {
                current.as_mut().ok_or_else(|| err("played outside a case".into()))?.played.push(rest.to_owned())
            }
            "end" => cases.push(current.take().ok_or_else(|| err("end outside a case".into()))?),
            _ => return Err(err(format!("unknown line {line:?}"))),
        }
    }
    Ok(cases)
}
