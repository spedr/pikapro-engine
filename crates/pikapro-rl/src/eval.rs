//! Evaluation: how policies fare against each other.
//!
//! * [`play`]: one battle, each side choosing from its own [`PlayerView`].
//! * [`evaluate`] and [`cross_evaluate`]: win rates over **mirrored**
//!   games. Episode `e` is the battle the vector environment plays as its
//!   episode `e`: battle seed
//!   [`episode_seed`](crate::env::episode_seed)`(seed, e)` and the teams
//!   the [`Teams`] gives it. Every pairing plays it twice, once each way
//!   round (policy A as p1 with p1's team, then policy B there), with the
//!   same battle seed. Each policy thus plays both teams in both seats, and
//!   every pairing sees the same episodes, so luck of the draw cancels.
//!   A policy sees what it sees in the environment: its [`PlayerView`],
//!   with its own team.
//! * [`Record`]: wins, losses and ties with a Wilson score interval.
//!
//! The games run on a [`Pool`]'s threads, whole battles per worker.
//! Everything is deterministic and independent of the thread count: a
//! game's policies draw from streams seeded by its episode and seat, never
//! from the battle's. The stream depends on nothing else (not the pairing,
//! nor which way round the game is), so a game is played the same whatever
//! table it is part of: `evaluate(a, b)` is `evaluate(b, a).flipped()`, a
//! pairing's record doesn't depend on the other entrants or their order,
//! and every pairing shares its random numbers seat by seat (common random
//! numbers, which lowers the variance of comparisons).
//!
//! [`cross_evaluate`] is poke-env's `cross_evaluate`: every pair of
//! policies, a table of win rates.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pikapro::{
    Battle, BattleResult, BattleRng, Choice, ChoiceError, EndReason, Format, Seed, SideId, SlotAction, Team,
};
use pikapro_view::{HpReport, OwnOverlay, OwnTeam, PlayerView, PublicTracker, Unavailable, ViewConfig};

use crate::policy::Policy;
use crate::pool::{Env, Pool, PoolConfig, Slot};
use crate::vec::Teams;

/// Plays `battle` to its end, `policies[i]` choosing for side `i` from
/// its own view, drawing from `rngs[i]`. The view is the one the vector
/// environment gives a policy: a [`PublicTracker`] fed with the battle's
/// events (which must be recorded), the side's request, and its own team
/// (`teams[i]`, the sets `battle` was made with: [`OwnTeam`]). Foe HP is
/// shown as `hp` says. Events already in the battle's log are read first;
/// the log is drained as the battle goes.
///
/// A policy chooses from what its request shows, as in the vector
/// environment: `legal` is [`env::shown`](crate::env::shown), which keeps a
/// switch out of a hidden trap (unless the view shows the foe's trapping
/// ability) and a move only a foe's Imprison disables. Such a choice
/// is refused as Showdown refuses an `[Unavailable choice]`: the side's
/// request is updated with what it revealed ([`OwnOverlay::reject`]) and
/// the policy chooses again. After [`POLICY_TRIES`] refusals the side
/// plays a random choice from the engine's legal actions.
///
/// # Errors
/// If a policy's choice is rejected outright (not one its request shows).
pub fn play(
    battle: &mut Battle,
    teams: [&Team; 2],
    policies: [&dyn Policy; 2],
    rngs: &mut [BattleRng; 2],
    hp: HpReport,
) -> Result<BattleResult, String> {
    let mut own_teams = teams.map(|t| OwnTeam::from_team(t, battle.format()));
    let mut tracker = PublicTracker::new(ViewConfig::new(battle.format()).with_hp(hp));
    for e in battle.drain_events() {
        tracker.apply(&e);
    }
    while battle.result().is_none() {
        for side in SideId::BOTH {
            if battle.is_over() {
                break;
            }
            if battle.legal_actions(side).is_waiting() {
                continue;
            }
            let mut own = OwnOverlay::from_request(&battle.request_data(side));
            let team = &mut own_teams[side.index()];
            team.observe(&own);
            let rng = &mut rngs[side.index()];
            let mut tries = 0;
            loop {
                // What the request shows, as the environment masks it
                // (the engine's legal actions unless it may hide a trap or
                // a disable).
                let hides = crate::env::hides(&own);
                let legal = crate::env::shown(battle, &own, &tracker, side);
                let choice = if tries < POLICY_TRIES {
                    let view = PlayerView::new(side, &tracker, &own).with_team(team);
                    policies[side.index()].choose(&view, &legal, rng)
                } else {
                    pikapro::legal::format_actions(battle, side, None).sample(rng)
                };
                tries += 1;
                match battle.submit(side, choice) {
                    Ok(_) => break,
                    Err(ChoiceError::IllegalAction { slot }) if hides && legal.check(&choice).is_ok() => {
                        let Choice::Actions(actions) = choice else { unreachable!("a slot's action was refused") };
                        reveal_unavailable(&mut own, battle, slot as usize, actions[slot as usize]);
                    }
                    Err(e) => {
                        return Err(format!("turn {}: {side:?}'s choice {choice:?} was rejected: {e}", battle.turn()));
                    }
                }
            }
            for e in battle.drain_events() {
                tracker.apply(&e);
            }
        }
    }
    battle.result().ok_or_else(|| "the battle did not end".to_string())
}

/// Choices a policy makes for one request before [`play`] (and the vector
/// environment) falls back to the engine's legal actions (each refused one
/// revealed something).
pub const POLICY_TRIES: usize = 4;

/// Showdown's `emitChoiceError` with an update (`side.chooseSwitch`,
/// `side.chooseMove`): the refused `action` of `slot` revealed a hidden
/// trap (a switch) or disable (a move), and the side's request (`own`)
/// shows it from now on ([`OwnOverlay::reject`]). A pass reveals nothing.
pub(crate) fn reveal_unavailable(own: &mut OwnOverlay, battle: &Battle, slot: usize, action: SlotAction) {
    let what = match action {
        SlotAction::Switch(_) => Unavailable::Switch,
        SlotAction::Move { index, .. } => Unavailable::Move { index },
        SlotAction::Pass => return,
    };
    own.reject(battle, slot, what);
}

/// Wins, losses and ties of one policy against another. A game that
/// fails (a panic, or a choice rejected outright) is in no count: it fails
/// the whole evaluation. (`pikapro.rl.eval` over a vector environment, where
/// a failed battle ends its episode alone, counts those apart as
/// `errors`.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Record {
    pub wins: u64,
    pub losses: u64,
    pub ties: u64,
    /// Games that ended at the turn limit (counted as ties).
    pub turn_limit: u64,
    /// Turns over all games.
    pub turns: u64,
}

impl Record {
    pub fn games(&self) -> u64 {
        self.wins + self.losses + self.ties
    }

    /// The score rate: wins plus half the ties, per game (poke-env's
    /// `cross_evaluate` counts wins only; ties are rare).
    pub fn win_rate(&self) -> f64 {
        let n = self.games();
        if n == 0 { 0.5 } else { (self.wins as f64 + 0.5 * self.ties as f64) / n as f64 }
    }

    /// An interval for [`Record::win_rate`] at the normal quantile `z`
    /// (1.96 for 95%). Wilson's interval needs win-or-lose trials, and a
    /// tie is neither: it is the Wilson score interval of the win share of
    /// the decisive games, mapped onto the score rate with the share of
    /// ties as observed (`0.5 + (x - 0.5) * decisive / games`, which maps
    /// `wins / decisive` to the score rate). Without ties it is the plain
    /// Wilson interval; `(0, 1)` when no game was decisive.
    pub fn wilson(&self, z: f64) -> (f64, f64) {
        let decisive = (self.wins + self.losses) as f64;
        if decisive == 0.0 {
            return (0.0, 1.0);
        }
        let p = self.wins as f64 / decisive;
        let z2 = z * z;
        let denominator = 1.0 + z2 / decisive;
        let center = (p + z2 / (2.0 * decisive)) / denominator;
        let half = z * (p * (1.0 - p) / decisive + z2 / (4.0 * decisive * decisive)).sqrt() / denominator;
        let (lo, hi) = ((center - half).max(0.0), (center + half).min(1.0));
        let share = decisive / self.games() as f64;
        (0.5 + (lo - 0.5) * share, 0.5 + (hi - 0.5) * share)
    }

    /// Whether the 95% Wilson interval lies above one half: the policy is
    /// better, significantly.
    pub fn significantly_better(&self) -> bool {
        self.wilson(1.96).0 > 0.5
    }

    /// The same games from the opponent's side.
    #[must_use]
    pub fn flipped(&self) -> Record {
        Record { wins: self.losses, losses: self.wins, ..*self }
    }

    fn add(&mut self, result: &BattleResult, seat: SideId) {
        match result.winner {
            Some(w) if w == seat => self.wins += 1,
            Some(_) => self.losses += 1,
            None => self.ties += 1,
        }
        if result.reason == EndReason::TurnLimit {
            self.turn_limit += 1;
        }
        self.turns += u64::from(result.turns);
    }
}

impl fmt::Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (lo, hi) = self.wilson(1.96);
        write!(f, "{:.3} [{:.3}, {:.3}] ({}-{}-{})", self.win_rate(), lo, hi, self.wins, self.losses, self.ties)
    }
}

/// How an evaluation plays its games. [`EvalConfig::new`] has the
/// defaults; set the fields to change them.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct EvalConfig {
    pub format: Format,
    /// Where each episode's teams come from.
    pub teams: Teams,
    /// Episode `e` has battle seed
    /// [`episode_seed`](crate::env::episode_seed)`(seed, e)`, as in the
    /// vector environment.
    pub seed: u64,
    /// Episodes (each played twice per pairing, mirrored).
    pub episodes: u64,
    /// Threads playing the games (all the machine's by default).
    pub threads: usize,
    /// How the views show foe HP: percentages by default in every format,
    /// as the vector environment has it. What a Showdown channel shows is
    /// exact HP in gen9vgc and the custom games
    /// (`pikapro_protocol::BattleInfo::report_exact_hp`); set
    /// `HpReport::Exact` to match it.
    pub hp: HpReport,
    /// Battles the pool holds at once; more episodes play in rounds. 0:
    /// every episode, up to 4,096.
    pub batch: usize,
}

impl EvalConfig {
    /// `episodes` episodes of `format` on `teams`: seed 0, every thread,
    /// foe HP as percentages, every episode at once (up to 4,096).
    pub fn new(format: Format, teams: Teams, episodes: u64) -> EvalConfig {
        EvalConfig {
            format,
            teams,
            seed: 0,
            episodes,
            threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
            hp: HpReport::Percent,
            batch: 0,
        }
    }
}

/// A named policy.
pub type Entrant = (String, Arc<dyn Policy>);

/// Every pairing's results: `records[i][j]` is policy `i`'s record
/// against policy `j` (`None` on the diagonal), poke-env's
/// `cross_evaluate` table.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CrossTable {
    pub names: Vec<String>,
    pub records: Vec<Vec<Option<Record>>>,
}

impl CrossTable {
    /// Policy `i`'s record against policy `j`.
    pub fn get(&self, i: usize, j: usize) -> Option<Record> {
        self.records.get(i)?.get(j).copied().flatten()
    }
}

impl fmt::Display for CrossTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self.names.iter().map(|n| n.len()).max().unwrap_or(0).max(8);
        write!(f, "{:width$}", "")?;
        for n in &self.names {
            write!(f, "  {n:>27}")?;
        }
        writeln!(f)?;
        for (i, n) in self.names.iter().enumerate() {
            write!(f, "{n:width$}")?;
            for j in 0..self.names.len() {
                match self.get(i, j) {
                    Some(r) => {
                        let (lo, hi) = r.wilson(1.96);
                        write!(f, "  {:>9.3} [{:.3}, {:.3}]", r.win_rate(), lo, hi)?
                    }
                    None => write!(f, "  {:>27}", "-")?,
                }
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

/// Policy `a`'s record against policy `b` over `config.episodes`
/// mirrored episodes.
///
/// # Errors
/// If the pool can't be built (bad teams) or a choice is rejected.
pub fn evaluate(a: Arc<dyn Policy>, b: Arc<dyn Policy>, config: &EvalConfig) -> Result<Record, String> {
    let table = cross_evaluate(&[("a".into(), a), ("b".into(), b)], config)?;
    Ok(table.get(0, 1).unwrap_or_default())
}

/// The pool's per-battle payload: nothing (the games are played on
/// copies of the episode's fresh battle).
#[derive(Clone, Copy, Debug)]
struct Fresh;

impl crate::pool::sealed::Sealed for Fresh {}

/// One game: its pairing, which way round, the result.
#[derive(Clone, Copy, Debug)]
struct Game {
    pairing: usize,
    /// `false`: the pairing's first policy is p1.
    mirrored: bool,
    result: BattleResult,
}

impl Slot for Fresh {
    type Out = Vec<Game>;

    fn start(_: &mut Battle) -> Fresh {
        Fresh
    }
}

/// Largest pool built by default: episodes beyond it play in rounds.
const MAX_POOL: u64 = 4096;

/// Every policy against every other ([`evaluate`] for each pair), on the
/// same episodes.
///
/// # Errors
/// If the pool can't be built (bad teams) or a choice is rejected.
pub fn cross_evaluate(policies: &[Entrant], config: &EvalConfig) -> Result<CrossTable, String> {
    let k = policies.len();
    let mut records = vec![vec![None; k]; k];
    let pairings: Vec<(usize, usize)> = (0..k).flat_map(|i| (i + 1..k).map(move |j| (i, j))).collect();
    if config.episodes > 0 && !pairings.is_empty() {
        let totals = run(policies, &pairings, config)?;
        for (&(i, j), r) in pairings.iter().zip(totals) {
            records[i][j] = Some(r);
            records[j][i] = Some(r.flipped());
        }
    }
    Ok(CrossTable { names: policies.iter().map(|(n, _)| n.clone()).collect(), records })
}

/// A game's policy RNG for the policy in `seat` (0: p1), from its episode's
/// battle seed alone: the same game (episode, and which policy sits where)
/// draws the same numbers whichever pairing, table position or way round
/// it is played as.
fn policy_rng(seed: u64, seat: usize) -> BattleRng {
    let k = seat as u64 + 1;
    BattleRng::new(Seed::fast(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ k.wrapping_mul(0xBF58_476D_1CE4_E5B9)))
}

fn run(policies: &[Entrant], pairings: &[(usize, usize)], config: &EvalConfig) -> Result<Vec<Record>, String> {
    let batch = if config.batch == 0 { MAX_POOL } else { config.batch as u64 };
    let n = config.episodes.min(batch) as usize;
    let pool_config = PoolConfig {
        seed: config.seed,
        log: true,
        auto_reset: false,
        spin: Duration::from_micros(200),
        ..PoolConfig::new(config.format.clone(), n, config.teams.clone())
    };
    let pool: Pool<Fresh> = Pool::with_slots(pool_config, config.threads)?;
    let policies: Arc<Vec<Arc<dyn Policy>>> = Arc::new(policies.iter().map(|(_, p)| p.clone()).collect());
    let pairings: Arc<Vec<(usize, usize)>> = Arc::new(pairings.to_vec());
    let totals = Arc::new(Mutex::new(vec![Record::default(); pairings.len()]));
    let (episodes, hp) = (config.episodes, config.hp);
    let rounds = episodes.div_ceil(n as u64);
    for round in 0..rounds {
        if round > 0 {
            pool.reset(None)?;
        }
        let (policies, pairings) = (policies.clone(), pairings.clone());
        let job = move |_: usize, envs: &mut [Env<Fresh>], out: &mut Vec<Game>, _: &PoolConfig| {
            out.clear();
            for env in envs.iter().filter(|e| e.episode < episodes) {
                // The pool's episode `e` is the battle `VecEnv` plays as its
                // episode `e`: battle seed `episode_seed(seed, e)`, and the
                // teams the source gives it.
                let battle_seed = env.seed();
                let at = |e: &str| format!("episode {} (seed {battle_seed}): {e}", env.episode);
                if let Some(why) = &env.poisoned {
                    return Err(at(why));
                }
                let teams = [&env.sheets[0].team, &env.sheets[1].team];
                for (pairing, &(a, b)) in pairings.iter().enumerate() {
                    for mirrored in [false, true] {
                        let seats = if mirrored { [b, a] } else { [a, b] };
                        let mut battle = env.battle.clone();
                        let mut rngs = [0, 1].map(|s| policy_rng(battle_seed, s));
                        let policies = [&*policies[seats[0]], &*policies[seats[1]]];
                        let result = play(&mut battle, teams, policies, &mut rngs, hp).map_err(|e| at(&e))?;
                        out.push(Game { pairing, mirrored, result });
                    }
                }
            }
            Ok(())
        };
        let totals = totals.clone();
        pool.run_shards(job, move |_, games| {
            let mut totals = totals.lock().unwrap_or_else(|e| e.into_inner());
            for g in games {
                // The pairing's first policy sits on p1 unless mirrored.
                let seat = if g.mirrored { SideId::P2 } else { SideId::P1 };
                totals[g.pairing].add(&g.result, seat);
            }
        })?;
    }
    let totals = totals.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Ok(totals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baselines::{MaxBasePowerPolicy, SimpleHeuristicsPolicy};
    use crate::policy::RandomPolicy;

    #[test]
    fn wilson_matches_the_formula() {
        let r = Record { wins: 60, losses: 40, ..Record::default() };
        let (lo, hi) = r.wilson(1.96);
        assert!((lo - 0.5020).abs() < 1e-3 && (hi - 0.6906).abs() < 1e-3, "{lo} {hi}");
        assert!(r.significantly_better());
        assert!(!r.flipped().significantly_better());
        assert_eq!(Record::default().wilson(1.96), (0.0, 1.0));
        // Ties: the decisive games' interval, pulled towards one half by
        // the tie share; it holds the score rate.
        let t = Record { wins: 60, losses: 40, ties: 100, ..Record::default() };
        let (tlo, thi) = t.wilson(1.96);
        assert!((tlo - (0.5 + (lo - 0.5) / 2.0)).abs() < 1e-12 && (thi - (0.5 + (hi - 0.5) / 2.0)).abs() < 1e-12);
        assert!(tlo < t.win_rate() && t.win_rate() < thi && t.win_rate() == 0.55);
        assert_eq!(t.significantly_better(), r.significantly_better());
        assert_eq!(Record { ties: 5, ..Record::default() }.wilson(1.96), (0.0, 1.0));
    }

    fn config(format: Format, episodes: u64, threads: usize) -> EvalConfig {
        let pair = Arc::new([0, 1].map(|_| pikapro_protocol::TeamSheet::new(pikapro::testing::sample_team(&format))));
        let mut c = EvalConfig::new(format, Teams::Pairs(vec![pair]), episodes);
        c.seed = 4_700_000;
        c.threads = threads;
        c
    }

    fn entrants() -> Vec<Entrant> {
        vec![
            ("random".into(), Arc::new(RandomPolicy) as Arc<dyn Policy>),
            ("maxbp".into(), Arc::new(MaxBasePowerPolicy)),
            ("heuristic".into(), Arc::new(SimpleHeuristicsPolicy::new())),
        ]
    }

    #[test]
    fn cross_evaluation_is_mirrored_and_independent_of_threads() {
        for format in [Format::gen9_singles(), Format::champions_vgc()] {
            let one = cross_evaluate(&entrants(), &config(format.clone(), 6, 1)).unwrap();
            let four = cross_evaluate(&entrants(), &config(format.clone(), 6, 4)).unwrap();
            assert_eq!(one.records, four.records, "{}", format.id);
            for i in 0..3 {
                assert_eq!(one.get(i, i), None);
                for j in 0..3 {
                    if i != j {
                        let r = one.get(i, j).unwrap();
                        assert_eq!(r.games(), 12);
                        assert_eq!(r, one.get(j, i).unwrap().flipped());
                    }
                }
            }
        }
    }

    /// Counts the views that carry the player's own team, and plays at
    /// random.
    #[derive(Debug, Default)]
    struct Probe {
        with_team: std::sync::atomic::AtomicUsize,
        without: std::sync::atomic::AtomicUsize,
    }

    impl Policy for Probe {
        fn choose(&self, view: &PlayerView<'_>, legal: &pikapro::LegalActions, rng: &mut BattleRng) -> Choice {
            let count = if view.team.is_some() { &self.with_team } else { &self.without };
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            legal.sample(rng)
        }
    }

    /// A policy is evaluated on the view it trains on: with its own team.
    #[test]
    fn policies_see_their_own_team() {
        let probe = Arc::new(Probe::default());
        evaluate(probe.clone(), Arc::new(RandomPolicy), &config(Format::champions_vgc(), 2, 1)).unwrap();
        let with_team = probe.with_team.load(std::sync::atomic::Ordering::Relaxed);
        assert!(with_team > 0 && probe.without.load(std::sync::atomic::Ordering::Relaxed) == 0);
    }

    /// Evaluation episode `e` is the vector environment's episode `e`:
    /// the same battle seed and teams, with random teams too.
    #[test]
    fn episodes_are_the_environment_s() {
        let format = Format::gen9_vgc();
        let g = crate::teams::generator(&format, pikapro_teams::Mode::Sets, Default::default()).unwrap();
        let teams = Teams::Generator(g);
        let seed = 4_700_200;
        let env_config = crate::env::EnvConfig {
            seed,
            record: true,
            ..crate::env::EnvConfig::new(format.clone(), 4, teams.clone())
        };
        let env = crate::env::VecEnv::new(env_config, 1).unwrap();
        env.reset(Some(seed), &mut env.buffers().as_out()).unwrap();
        let config = PoolConfig { seed, log: true, auto_reset: false, ..PoolConfig::new(format, 4, teams) };
        let pool: Pool<Fresh> = Pool::with_slots(config, 1).unwrap();
        for e in 0..4 {
            let record = env.record(e, false).unwrap();
            let episode = pool.with_env(e, |env| (env.episode, env.seed(), env.sheets.clone())).unwrap();
            assert_eq!(episode, (record.episode, record.seed, record.sheets.clone()));
        }
    }

    /// A game is the same whatever table it is played in: `evaluate(a, b)`
    /// is `evaluate(b, a).flipped()`, and a pairing's record doesn't depend
    /// on the other entrants or their order.
    #[test]
    fn records_do_not_depend_on_order_or_table() {
        for format in [Format::gen9_singles(), Format::champions_vgc()] {
            let mut c = config(format.clone(), 8, 2);
            let g = crate::teams::generator(&format, pikapro_teams::Mode::Sets, Default::default()).unwrap();
            c.teams = Teams::Generator(g);
            c.seed = 8_300_000;
            let [random, maxbp, heuristic] = <[Entrant; 3]>::try_from(entrants()).unwrap();
            let ab = evaluate(random.1.clone(), maxbp.1.clone(), &c).unwrap();
            let ba = evaluate(maxbp.1.clone(), random.1.clone(), &c).unwrap();
            assert_eq!(ab, ba.flipped(), "{}", format.id);
            let three = cross_evaluate(&[heuristic, maxbp, random], &c).unwrap();
            assert_eq!(three.get(2, 1), Some(ab), "{}", format.id);
            assert_eq!(three.get(1, 2), Some(ba), "{}", format.id);
        }
    }

    #[test]
    fn rounds_play_the_same_episodes() {
        let all = config(Format::gen9_singles(), 5, 2);
        let rounds = EvalConfig { batch: 2, ..all.clone() };
        let a = evaluate(Arc::new(RandomPolicy), Arc::new(MaxBasePowerPolicy), &all).unwrap();
        let b = evaluate(Arc::new(RandomPolicy), Arc::new(MaxBasePowerPolicy), &rounds).unwrap();
        assert_eq!((a.games(), a), (10, b));
    }
}
