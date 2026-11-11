//! Random teams (`pikapro-teams`) for battles and pools, and
//! team validation.

use std::sync::{Arc, Mutex, OnceLock};

use pikapro::{BattleRng, Format};
use pikapro_protocol::TeamSheet;
use pikapro_teams::{Mode, Options};

/// Mixed into an episode's battle seed to seed its teams, so that the
/// teams and the battle draw from different streams.
const TEAM_SEED: u64 = 0x7EA5_5EED_7EA5_5EED;

/// Both players' teams for a battle seed: p1's then p2's, drawn from one
/// RNG seeded with `seed ^ TEAM_SEED` (`TeamGenerator::seed`). [`pair`] is
/// what a pool with this generator plays in the episode with battle seed
/// `seed`. In the random battles' `Mode::Sets` that RNG is Showdown's
/// Gen 5 one: p1's team is Showdown's for the seed `gen5,<seed ^
/// TEAM_SEED in 16 hex digits>`, p2's Showdown's for the seed the PRNG
/// has after it.
pub fn pair(generator: &pikapro_teams::TeamGenerator, seed: u64) -> Result<[TeamSheet; 2], pikapro_teams::Error> {
    let mut rng = BattleRng::new(generator.seed(seed ^ TEAM_SEED));
    let mut sheet = || -> Result<TeamSheet, pikapro_teams::Error> {
        let generated = generator.generate_from(&mut rng)?;
        let mut sheet = TeamSheet::new(generated.team);
        for (extras, shiny) in sheet.extras.iter_mut().zip(generated.shiny) {
            extras.shiny = shiny;
        }
        Ok(sheet)
    };
    Ok([sheet()?, sheet()?])
}

type Cache = Mutex<Vec<(Format, Mode, Options, Arc<pikapro_teams::TeamGenerator>)>>;

/// Whether two formats have the same rules ([`Format::same_rules`]): a
/// format made from a preset keeps the preset's `id`, whatever it changed.
pub(crate) fn same_rules(a: &Format, b: &Format) -> bool {
    a.same_rules(b)
}

/// The generator for a format, mode and options, built once per process
/// and shared (a uniform generator works out each species' legal moves on
/// first use, and keeps them). Formats are told apart by their rules
/// (every field of the format), not only their id.
pub fn generator(format: &Format, mode: Mode, options: Options) -> Result<Arc<pikapro_teams::TeamGenerator>, String> {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Cache::default);
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((.., g)) = cache.iter().find(|(f, m, o, _)| same_rules(f, format) && *m == mode && *o == options) {
        return Ok(g.clone());
    }
    let g =
        Arc::new(pikapro_teams::TeamGenerator::with_options(format, mode, options.clone()).map_err(|e| e.to_string())?);
    cache.push((format.clone(), mode, options, g.clone()));
    Ok(g)
}

/// A team's problems for a preset, in Showdown's words (empty: valid):
/// Showdown's team validator where the preset has a validation format
/// (`pikapro-validate`), then the engine's structural rules.
pub fn problems(sheet: &TeamSheet, format: &Format) -> Vec<String> {
    check(sheet, format).messages
}

/// A team as a Showdown server plays it once its validator accepts it: the
/// sets as validation leaves them (`pikapro_validate::Checked::team`; a
/// listed battle-only forme such as `Floette-Mega` starts as the forme it
/// changes from and Mega Evolves in battle), the extras as given. Err: the
/// team's [`problems`].
pub fn validated(sheet: &TeamSheet, format: &Format) -> Result<TeamSheet, Vec<String>> {
    let check = check(sheet, format);
    if check.messages.is_empty() {
        Ok(TeamSheet { team: check.team, extras: sheet.extras.clone() })
    } else {
        Err(check.messages)
    }
}

/// [`validated`]'s team whether the team is valid or not (what a server
/// would play if it accepted it): the team a player gives
/// [`ChannelSession`](crate::session::ChannelSession), which the server's
/// requests then show.
pub fn played(team: &pikapro::Team, format: &Format) -> pikapro::Team {
    pikapro_validate::check_preset(format, team, &[]).team
}

fn check(sheet: &TeamSheet, format: &Format) -> pikapro_validate::PresetCheck {
    let info: Vec<pikapro_validate::SetInfo> = sheet
        .team
        .sets
        .iter()
        .enumerate()
        .map(|(i, set)| {
            let extras = sheet.extras.get(i).cloned().unwrap_or_default();
            pikapro_validate::SetInfo::new(set, &extras.name, extras.shiny)
        })
        .collect();
    pikapro_validate::check_preset(format, &sheet.team, &info)
}

/// A team that didn't make it into a pool: its index in the input and why,
/// in Showdown's words (the importer's error, or the validator's
/// problems).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejected {
    pub index: usize,
    pub problems: Vec<String>,
}

/// A tournament team pool: each text is a team in Showdown's export format
/// (a pokepaste's; the Python loader fetches and caches them), imported for
/// `format` and validated against it ([`validated`]). Returns the teams
/// that pass, in input order and as validation leaves them (a listed Mega
/// forme starts as its base forme), for
/// [`Teams::PerSide`](crate::vec::Teams), and the rejects.
pub fn import_pool<S: AsRef<str>>(texts: &[S], format: &Format) -> (Vec<TeamSheet>, Vec<Rejected>) {
    let mut pool = Vec::new();
    let mut rejected = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        match pikapro_protocol::import_sheet(text.as_ref(), format) {
            Err(e) => rejected.push(Rejected { index, problems: vec![e.to_string()] }),
            Ok(sheet) => match validated(&sheet, format) {
                Ok(sheet) => pool.push(sheet),
                Err(problems) => rejected.push(Rejected { index, problems }),
            },
        }
    }
    (pool, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::{Pool, PoolConfig};
    use crate::vec::Teams;

    #[test]
    fn tournament_pools_keep_valid_teams() {
        let format = Format::champions_vgc();
        let g = generator(&format, Mode::Sets, Options::default()).unwrap();
        let [a, b] = pair(&g, 11).unwrap();
        let good = pikapro_protocol::export_sheet(&a, &format);
        let other = pikapro_protocol::export_sheet(&b, &format);
        // A duplicate species breaks Species Clause.
        let first = good.split("\n\n").next().unwrap().to_string();
        let clause = format!("{first}\n\n{first}\n");
        let texts = [good.as_str(), "", clause.as_str(), other.as_str()];
        let (pool, rejected) = import_pool(&texts, &format);
        assert_eq!(pool, [a, b]);
        assert_eq!(rejected.iter().map(|r| r.index).collect::<Vec<_>>(), [1, 2]);
        assert!(rejected.iter().all(|r| !r.problems.is_empty()), "{rejected:?}");
    }

    /// `sample_team` with its Mega Stone holders listed as their Megas, as
    /// pastes list them.
    fn listed_megas(format: &Format) -> (pikapro::Team, pikapro::Team) {
        let sample = pikapro::testing::sample_team(format);
        let mut listed = sample.clone();
        for set in &mut listed.sets {
            let stone = set.item.map_or(&[][..], |i| format.dex.item(i).mega);
            if let Some(&(_, mega)) = stone.iter().find(|&&(base, _)| base == set.species) {
                set.species = mega;
            }
        }
        assert_ne!(listed, sample);
        (listed, sample)
    }

    /// A pool's teams are played as Showdown's validator leaves them: a
    /// listed Mega forme starts as the forme it Mega Evolves from.
    #[test]
    fn tournament_pools_play_listed_megas_as_their_base_formes() {
        let format = Format::champions_vgc();
        let (listed, sample) = listed_megas(&format);
        let text = pikapro_protocol::export_sheet(&TeamSheet::new(listed.clone()), &format);
        let (pool, rejected) = import_pool(&[text], &format);
        assert!(rejected.is_empty(), "{rejected:?}");
        assert_eq!(pool[0].team, sample);
        assert_eq!(played(&listed, &format), sample);
        let options = crate::session::SessionOptions::default().with_team(Some(listed.clone()));
        let session = crate::session::ChannelSession::new("gen9championsvgc2026regmc", options).unwrap();
        assert_eq!(*session.team(), pikapro_view::OwnTeam::from_team(&sample, &format));
        // A custom game doesn't validate: the team is played as given.
        let options = crate::session::SessionOptions::default().with_team(Some(listed.clone()));
        let session = crate::session::ChannelSession::for_format(format.clone(), false, options).unwrap();
        assert_eq!(*session.team(), pikapro_view::OwnTeam::from_team(&listed, &format));
    }

    #[test]
    fn generated_teams_are_legal_and_deterministic() {
        for format in Format::presets() {
            let g = generator(format, Mode::Sets, Options::default()).unwrap();
            assert!(Arc::ptr_eq(&g, &generator(format, Mode::Sets, Options::default()).unwrap()));
            let [a, b] = pair(&g, 7).unwrap();
            assert_eq!([a.clone(), b.clone()], pair(&g, 7).unwrap());
            assert_ne!(a.team, b.team);
            // A random battle's teams are Showdown's generator's, which
            // nothing validates.
            assert!(
                format.random_teams || problems(&a, format).is_empty(),
                "{}: {:?}",
                format.id,
                problems(&a, format)
            );
            assert!(format.random_teams || problems(&b, format).is_empty());
        }
    }

    /// A format made from a preset keeps its id: its generator is its own,
    /// and a team source checks the rules, not only the id.
    #[test]
    fn formats_are_told_apart_by_their_rules() {
        let preset = Format::gen9_singles();
        let short = Format { turn_limit: 50, ..Format::gen9_singles() };
        let g = generator(&preset, Mode::Sets, Options::default()).unwrap();
        let h = generator(&short, Mode::Sets, Options::default()).unwrap();
        assert!(!Arc::ptr_eq(&g, &h));
        assert!(Teams::Generator(g.clone()).check(&preset).is_ok());
        assert!(Teams::Generator(g).check(&short).unwrap_err().contains("rules differ"));
        assert!(Teams::Generator(h).check(&short).is_ok());
    }

    #[test]
    fn pools_play_generated_teams() {
        let format = Format::gen9_vgc();
        let g = generator(&format, Mode::Uniform, Options::default()).unwrap();
        let config = PoolConfig {
            seed: 3,
            spin: std::time::Duration::ZERO,
            ..PoolConfig::new(format, 4, Teams::Generator(g.clone()))
        };
        let pool = Pool::new(config, 2).unwrap();
        let (totals, _) = pool.play_random(8, || Ok(())).unwrap();
        assert_eq!(totals.battles, 8);
        // Each episode's teams come from its seed.
        let seed = pool.seeds()[1];
        let sheets = pool.with_env(1, |env| env.sheets.clone()).unwrap();
        assert_eq!(*sheets, pair(&g, seed).unwrap());
    }
}
