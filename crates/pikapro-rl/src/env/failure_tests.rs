//! Tests of how the vector environment takes failures and odd
//! configurations: a battle that panics or errs ends its episode alone
//! ([`EnvEndReason::Error`]), and configurations that can't play are
//! refused. Seeds from 6,200,000.

use std::collections::BTreeMap;

use pikapro::testing::sample_team;
use pikapro::{BattleRng, Choice, PokemonSet, Team};
use pikapro_data::gen9::{moves, species};
use pikapro_view::PlayerView;

use super::tests::{config, env};
use super::*;
use crate::vec::{EnvEndReason, RANDOM_ACTION, status};

const SEED: u64 = 6_200_000;

/// A built-in opponent that panics whenever it must choose.
#[derive(Debug)]
struct Boom;

impl crate::policy::Policy for Boom {
    fn choose(&self, _: &PlayerView<'_>, _: &pikapro::LegalActions, _: &mut BattleRng) -> Choice {
        panic!("boom")
    }
}

/// A built-in opponent whose choice its request never shows (a switch to
/// a team position that doesn't exist).
#[derive(Debug)]
struct Illegal;

impl crate::policy::Policy for Illegal {
    fn choose(&self, _: &PlayerView<'_>, _: &pikapro::LegalActions, _: &mut BattleRng) -> Choice {
        Choice::single(pikapro::SlotAction::Switch(40))
    }
}

/// A built-in policy's choice outside what its request shows ends the
/// battle's episode with an error, as `eval::play` returns one, instead of
/// being replaced by a random move.
#[test]
fn a_built_in_choice_the_request_does_not_show_ends_the_episode() {
    let format = Format::gen9_singles();
    let e = env(config(format, 2), 1);
    e.set_opponent(Some(&[1]), Opponent::Policy(Arc::new(Illegal))).unwrap();
    let mut b = e.buffers();
    e.reset(Some(SEED), &mut b.as_out()).unwrap();
    let actions = vec![RANDOM_ACTION; 2 * 2 * e.actions_per_player()];
    let mut errors = e.take_errors();
    for _ in 0..3 {
        if !errors.is_empty() {
            break;
        }
        e.step(&actions, &mut b.as_out()).unwrap();
        errors = e.take_errors();
    }
    assert!(!errors.is_empty(), "battle 1's opponent was never asked");
    assert!(errors.iter().all(|x| x.battle == 1 && x.message.contains("was rejected")), "{errors:?}");
    assert_eq!(b.end_reason[1], EnvEndReason::Error as u8);
    assert_ne!(b.end_reason[0], EnvEndReason::Error as u8);
}

/// Fills every buffer with values no step writes, so that a value the step
/// leaves unwritten shows.
fn garbage(b: &mut StepBuffers) {
    b.obs.fill(7.5);
    b.masks.fill(true);
    b.min_switches.fill(9);
    b.reward.fill(3.0);
    b.terminated.fill(true);
    b.truncated.fill(false);
    b.end_reason.fill(77);
    b.winner.fill(9);
    b.needs_action.fill(true);
    b.status.fill(5);
    b.agent_side.fill(7);
    b.episode.fill(777);
    b.series.fill(99);
}

/// The rows of battle `i` in `b` (every buffer but the final ones).
fn rows(b: &StepBuffers, i: usize) -> String {
    let shape = b.shape.unwrap();
    let per_series = crate::series::SERIES_WIDTH;
    format!(
        "{:?} {:?} {:?} {:?} {} {} {} {} {:?} {:?} {} {} {:?}",
        &b.obs[2 * i * shape.obs..][..2 * shape.obs],
        &b.masks[2 * i * shape.mask..][..2 * shape.mask],
        &b.min_switches[2 * i..][..2],
        &b.reward[2 * i..][..2],
        b.terminated[i],
        b.truncated[i],
        b.end_reason[i],
        b.winner[i],
        &b.needs_action[2 * i..][..2],
        &b.status[2 * i..][..2],
        b.agent_side[i],
        b.episode[i],
        b.series.get(i * per_series..(i + 1) * per_series),
    )
}

/// Battle 1 of 4 plays against [`Boom`] from its first episode (with
/// `boom`), then (after `boom` steps) against a random opponent again; the
/// agent plays random actions. Returns every step's outputs (from buffers
/// filled with garbage before each call when `scramble`) and the errors
/// taken after each call.
fn boom_run(
    c: EnvConfig,
    threads: usize,
    boom: Option<usize>,
    scramble: bool,
) -> (Vec<StepBuffers>, Vec<Vec<BattleError>>) {
    let e = env(c, threads);
    if boom.is_some() {
        e.set_opponent(Some(&[1]), Opponent::Policy(Arc::new(Boom))).unwrap();
    }
    let actions = vec![RANDOM_ACTION; 4 * 2 * e.actions_per_player()];
    let mut b = e.buffers();
    let mut outs = Vec::new();
    let mut errors = Vec::new();
    if scramble {
        garbage(&mut b);
    }
    e.reset(Some(SEED), &mut b.as_out()).unwrap();
    outs.push(b.clone());
    errors.push(e.take_errors());
    for step in 0..40 {
        if Some(step) == boom {
            e.set_opponent(Some(&[1]), Opponent::random()).unwrap();
        }
        if scramble {
            garbage(&mut b);
        }
        e.step(&actions, &mut b.as_out()).unwrap();
        outs.push(b.clone());
        errors.push(e.take_errors());
    }
    (outs, errors)
}

#[test]
fn a_panic_ends_its_battle_s_episode_alone() {
    for format in [Format::champions_vgc(), Format::champions_bss()] {
        for autoreset in [Autoreset::NextStep, Autoreset::SameStep] {
            let c = EnvConfig { autoreset, series: Some(3), ..config(format.clone(), 4) };
            let (one, errors) = boom_run(c.clone(), 1, Some(3), true);
            // Battle 1 fails at its reset and at each of the next three
            // steps (a new episode, and a new series, each time), then
            // plays.
            for (k, b) in one.iter().enumerate() {
                let failed = k <= 3;
                assert_eq!(b.end_reason[1] == EnvEndReason::Error as u8, failed, "{} step {k}", format.id);
                assert_eq!(errors[k].len(), usize::from(failed));
                if failed {
                    let e = &errors[k][0];
                    assert_eq!((e.battle, e.episode), (1, 1 + 4 * k as u64));
                    assert!(e.message.contains("panic: boom"), "{}", e.message);
                    assert!(b.truncated[1] && !b.terminated[1] && b.winner[1] == -1);
                    assert_eq!(b.episode[1], 1 + 4 * k as u64);
                    assert!(b.needs_action[2..4] == [false; 2] && b.status[2..4] == [status::IGNORED; 2]);
                    assert!(b.reward[2..4] == [0.0; 2] && b.min_switches[2..4] == [0; 2]);
                    let mask = b.shape.unwrap().mask;
                    assert!(b.masks[2 * mask..4 * mask].iter().all(|&m| !m));
                    assert!(
                        b.obs.iter().skip(2 * b.shape.unwrap().obs).take(2 * b.shape.unwrap().obs).all(|&x| x == 0.0)
                    );
                    assert_eq!(b.series[4..8], [1, 0, 0, 0], "a new series each time");
                }
            }
            assert!(one[4].needs_action[2], "battle 1 plays again");
            // The other battles play as if nothing had happened.
            let (clean, none) = boom_run(c.clone(), 1, None, false);
            assert!(none.iter().all(Vec::is_empty));
            for (k, (a, b)) in one.iter().zip(&clean).enumerate() {
                for i in [0, 2, 3] {
                    assert_eq!(rows(a, i), rows(b, i), "{} {autoreset:?} step {k} battle {i}", format.id);
                }
            }
            // Every value is written: any thread count and write path,
            // kept or garbage-filled buffers, gives the same outputs.
            for (threads, direct, scramble) in [(3, None, false), (3, Some(0), true), (1, None, false)] {
                let c = EnvConfig { direct_bytes_per_thread: direct, shards_per_thread: 2, ..c.clone() };
                let (other, other_errors) = boom_run(c, threads, Some(3), scramble);
                for (k, (a, b)) in one.iter().zip(&other).enumerate() {
                    assert!(a == b, "{} {autoreset:?} {threads} {direct:?}: step {k} differs", format.id);
                }
                assert_eq!(errors, other_errors);
            }
        }
    }
}

/// The final buffers of a failed battle hold a zeroed observation.
#[test]
fn a_failed_battle_s_final_observation_is_zeros() {
    let c = EnvConfig { autoreset: Autoreset::SameStep, ..config(Format::champions_vgc(), 2) };
    let e = env(c, 1);
    e.set_opponent(Some(&[1]), Opponent::Policy(Arc::new(Boom))).unwrap();
    let mut b = e.buffers();
    b.final_obs.fill(-1.0);
    b.final_episode.fill(99);
    e.reset(Some(SEED + 2), &mut b.as_out()).unwrap();
    let obs = 2 * e.shape().obs;
    assert!(b.final_obs[obs..2 * obs].iter().all(|&x| x == 0.0));
    assert!(b.final_obs[..obs].iter().all(|&x| x == -1.0), "battle 0 didn't end");
    assert_eq!(b.final_episode, [99, 1]);
    assert_eq!(e.take_errors().len(), 1);
    assert!(e.take_errors().is_empty(), "taken");
}

/// An encoder that panics on turn 2, after the battle has moved.
#[derive(Debug)]
struct BoomAtTurnTwo;

impl crate::encode::Encoder for BoomAtTurnTwo {
    fn size(&self) -> usize {
        crate::encode::Basic.size()
    }

    fn encode(&self, view: &PlayerView<'_>, tables: &crate::tables::Tables, out: &mut [f32]) {
        assert!(view.turn() != 2, "encoder boom");
        crate::encode::Basic.encode(view, tables, out);
    }
}

#[test]
fn an_encoder_panic_ends_the_episode() {
    let c = EnvConfig {
        observation: Observation::new(BoomAtTurnTwo),
        opponent: OpponentSpec::One(Opponent::External),
        ..config(Format::gen9_singles(), 3)
    };
    for threads in [1, 2] {
        let e = env(c.clone(), threads);
        let mut b = e.buffers();
        e.reset(Some(SEED + 3), &mut b.as_out()).unwrap();
        let mut failed = 0;
        for _ in 0..30 {
            e.step(&[RANDOM_ACTION; 3 * 2], &mut b.as_out()).unwrap();
            for i in 0..3 {
                if b.end_reason[i] == EnvEndReason::Error as u8 {
                    failed += 1;
                    assert!(
                        b.obs[2 * i * b.shape.unwrap().obs..][..2 * b.shape.unwrap().obs].iter().all(|&x| x == 0.0)
                    );
                }
            }
            let errors = e.take_errors();
            assert!(errors.iter().all(|e| e.message.contains("encoder boom")), "{errors:?}");
            assert!(errors.windows(2).all(|w| w[0].battle < w[1].battle), "in battle order");
        }
        assert!(failed >= 6, "{failed}");
    }
}

/// Each episode's terminal row, by episode: its end reason, winner,
/// rewards and last observation.
type Ends = BTreeMap<u64, (u8, i8, [f32; 2], Vec<f32>)>;

/// Plays `steps` random steps of `e` from a reset; returns every episode's
/// terminal row (asserting none ends twice) and the errors. With same-step
/// autoreset, also checks the rows of a step whose next episode failed to
/// start: the new episode's rows say nobody is asked, the ended one's end
/// stands.
fn ends(e: &VecEnv, steps: usize) -> (Ends, Vec<BattleError>) {
    let same = e.config().autoreset == Autoreset::SameStep;
    let (n, size) = (e.shape().n, e.shape().obs);
    let actions = vec![RANDOM_ACTION; n * 2 * e.actions_per_player()];
    let mut b = e.buffers();
    let mut ends = Ends::new();
    e.reset(Some(SEED + 7), &mut b.as_out()).unwrap();
    let mut errors = e.take_errors();
    for _ in 0..steps {
        e.step(&actions, &mut b.as_out()).unwrap();
        let new = e.take_errors();
        for i in 0..n {
            if b.end_reason[i] == 0 {
                continue;
            }
            let (episode, obs) = if same { (b.final_episode[i], &b.final_obs) } else { (b.episode[i], &b.obs) };
            let row = (
                b.end_reason[i],
                b.winner[i],
                [b.reward[2 * i], b.reward[2 * i + 1]],
                obs[2 * i * size..][..2 * size].to_vec(),
            );
            assert!(ends.insert(episode, row).is_none(), "episode {episode} ended twice");
        }
        for err in &new {
            let i = err.battle;
            assert_eq!(b.episode[i], err.episode, "the failed episode's number");
            assert!(
                b.needs_action[2 * i..][..2] == [false; 2]
                    && b.obs[2 * i * size..][..2 * size].iter().all(|&x| x == 0.0)
            );
            if same && b.end_reason[i] != EnvEndReason::Error as u8 {
                // The episode before ended this step, and its row stands.
                assert!(b.end_reason[i] != 0 && b.final_episode[i] != err.episode, "{err:?}");
            }
        }
        errors.extend(new);
    }
    (ends, errors)
}

/// Checks a run whose episodes `failing` fail to start (`ends(e)` of it)
/// against the same run where they don't (`clean`): every other episode
/// ends exactly as there (its result, reward and last observation), each
/// failed one is reported once, under its own number, and never ends
/// otherwise (with next-step autoreset its row is the error's; with
/// same-step autoreset it has none).
fn check_skipped(failed: (Ends, Vec<BattleError>), clean: &Ends, same: bool, failing: impl Fn(u64) -> bool) {
    let (ends, errors) = failed;
    let mut reported = std::collections::BTreeSet::new();
    for err in &errors {
        assert!(failing(err.episode), "{err:?}");
        assert!(reported.insert(err.episode), "episode {} reported twice", err.episode);
    }
    let mut compared = 0;
    for (e, row) in &ends {
        if failing(*e) {
            assert!(!same, "episode {e}: a same-step start failure has no end row");
            assert!(reported.contains(e));
            assert_eq!((row.0, row.1, row.2), (EnvEndReason::Error as u8, -1, [0.0; 2]), "episode {e}");
            assert!(row.3.iter().all(|&x| x == 0.0));
        } else if let Some(want) = clean.get(e) {
            assert!(row == want, "episode {e}: {row:?} != {want:?}");
            compared += 1;
        }
    }
    assert!(compared >= 6 && reported.len() >= 3, "{compared} compared, {} failed", reported.len());
}

/// An episode that can't start (an error when its battle is made) is
/// skipped: reported once under its own number, and the episode before it
/// keeps its end, winner, reward and last observation, with either
/// autoreset.
#[test]
fn an_episode_that_fails_to_start_is_skipped() {
    let odd = |e: u64| e % 2 == 1;
    for autoreset in [Autoreset::NextStep, Autoreset::SameStep] {
        let c = EnvConfig { autoreset, ..config(Format::gen9_singles(), 3) };
        let (clean, _) = ends(&env(c.clone(), 1), 1200);
        for threads in [1, 2] {
            let mut e = env(c.clone(), threads);
            Arc::get_mut(&mut e.ctx).unwrap().fail_start = Some(|e| e % 2 == 1);
            let failed = ends(&e, 400);
            assert!(failed.1.iter().all(|err| err.message.contains("can't start (test)")));
            check_skipped(failed, &clean, autoreset == Autoreset::SameStep, odd);
        }
    }
}

/// A built-in opponent that panics when its team has one Pokémon, and
/// plays as [`RandomPolicy`](crate::policy::RandomPolicy) otherwise.
#[derive(Debug)]
struct BoomAlone;

impl crate::policy::Policy for BoomAlone {
    fn choose(&self, view: &PlayerView<'_>, legal: &pikapro::LegalActions, rng: &mut BattleRng) -> Choice {
        assert!(view.team.is_none_or(|t| t.mons.len() > 1), "alone");
        legal.sample(rng)
    }
}

/// An episode whose opponent panics while it starts (the first choice of
/// each episode with the one-Pokémon team) is skipped the same way: the
/// episode that ended before it keeps its row.
#[test]
fn a_panic_while_an_episode_starts_skips_it() {
    let format = Format::gen9_singles();
    let alone = Team::new(vec![PokemonSet::new(species::PIKACHU).with_moves(&[moves::TACKLE])]);
    let pairs = vec![
        Arc::new([TeamSheet::new(sample_team(&format)), TeamSheet::new(sample_team(&format))]),
        Arc::new([TeamSheet::new(sample_team(&format)), TeamSheet::new(alone)]),
    ];
    for autoreset in [Autoreset::NextStep, Autoreset::SameStep] {
        let c = EnvConfig { autoreset, teams: Teams::Pairs(pairs.clone()), ..config(format.clone(), 3) };
        let (clean, _) = ends(&env(c.clone(), 1), 1200);
        let c = EnvConfig { opponent: OpponentSpec::One(Opponent::Policy(Arc::new(BoomAlone))), ..c };
        for threads in [1, 2] {
            let failed = ends(&env(c.clone(), threads), 400);
            assert!(failed.1.iter().all(|err| err.message == "panic: alone"), "{:?}", failed.1);
            check_skipped(failed, &clean, autoreset == Autoreset::SameStep, |e| e % 2 == 1);
        }
    }
}

/// A team list or pool with a team that can't start a battle is refused
/// when the environment is made, whatever the seed, naming the team.
#[test]
fn a_team_that_can_t_play_is_refused_whatever_the_seed() {
    let format = Format::gen9_singles();
    let good = TeamSheet::new(sample_team(&format));
    let bad = TeamSheet::new(Team::new(Vec::new()));
    let pool: Arc<[TeamSheet]> = Arc::from(vec![good.clone(), good.clone(), good.clone(), bad.clone()]);
    for seed in 0..20 {
        let c = EnvConfig { seed, teams: Teams::PerSide([pool.clone(), pool.clone()]), ..config(format.clone(), 2) };
        let e = VecEnv::new(c, 1).unwrap_err();
        assert!(e.starts_with("player 0's pool, team 3: team has 0 Pokémon"), "{e}");
    }
    let pairs = Teams::Pairs(vec![Arc::new([good.clone(), good.clone()]), Arc::new([good.clone(), bad])]);
    let e = VecEnv::new(EnvConfig { teams: pairs, ..config(format.clone(), 2) }, 1).unwrap_err();
    assert!(e.starts_with("pair 1, player 1's team: team has 0 Pokémon"), "{e}");
    for empty in [Teams::Pairs(Vec::new()), Teams::PerSide([Arc::from(vec![]), Arc::from(vec![good])])] {
        assert!(empty.teams(0, 0).is_err(), "an error, not a panic");
        assert!(VecEnv::new(EnvConfig { teams: empty, ..config(format.clone(), 1) }, 1).is_err());
    }
}

#[test]
fn configurations_that_can_t_play_are_refused() {
    let base = config(Format::champions_vgc(), 2);
    let refused = |c: EnvConfig| VecEnv::new(c, 1).unwrap_err();
    assert!(refused(EnvConfig { max_turns: Some(0), ..base.clone() }).contains("max_turns"));
    VecEnv::new(EnvConfig { max_turns: Some(1), ..base.clone() }, 1).unwrap();
    for best_of in [0, 1, 2, 4, 6, 8, 10, 11] {
        assert!(refused(EnvConfig { series: Some(best_of), ..base.clone() }).contains("best of 3, 5, 7 or 9"));
    }
    for best_of in [3, 5, 7, 9] {
        VecEnv::new(EnvConfig { series: Some(best_of), ..base.clone() }, 1).unwrap();
    }
    // Open sheets, and a series (played with them), need team preview.
    for format in [Format::gen9_singles(), Format::gen9_doubles()] {
        let none = config(format, 2);
        VecEnv::new(none.clone(), 1).unwrap();
        for ots in [0.01, 1.0] {
            assert!(refused(EnvConfig { ots, ..none.clone() }).contains("ots needs a format with team preview"));
        }
        assert!(refused(EnvConfig { series: Some(3), ..none.clone() }).contains("need team preview"));
        // Nor does a session take a sheet there: a `|showteam|` line (which
        // Showdown doesn't send without team preview) is left out.
        let mut session =
            crate::session::ChannelSession::for_format(none.format.clone(), true, Default::default()).unwrap();
        session.set_side(pikapro::SideId::P1);
        session.flush();
        let sheet = TeamSheet::new(sample_team(&none.format));
        session.feed(&format!("|showteam|p2|{}", pikapro_protocol::pack_team(&sheet, &none.format)));
        assert!(session.error().is_none());
        assert_eq!(session.tracker().ots, [None; 2]);
    }
    // Teams smaller than team preview's pick: no order of the action space
    // could answer the preview.
    let format = Format { team_size: (1, 6), ..Format::gen9_vgc() };
    let small = Team::new(vec![
        PokemonSet::new(species::PIKACHU).with_moves(&[moves::TACKLE]),
        PokemonSet::new(species::SNORLAX).with_moves(&[moves::TACKLE]),
    ]);
    let sheets = Arc::new([TeamSheet::new(small.clone()), TeamSheet::new(small)]);
    let e = refused(EnvConfig::new(format, 1, Teams::Pairs(vec![sheets])));
    assert!(e.contains("smaller than team preview's pick of 4"), "{e}");
    for w in [f64::NAN, f64::INFINITY, -1.0] {
        let mix = OpponentSpec::Mix(Arc::from(vec![(Opponent::random(), 1.0), (Opponent::External, w)]));
        assert!(refused(EnvConfig { opponent: mix, ..base.clone() }).contains("mixture weights"));
    }
    let zeros = OpponentSpec::Mix(Arc::from(vec![(Opponent::random(), 0.0), (Opponent::External, 0.0)]));
    assert!(refused(EnvConfig { opponent: zeros, ..base.clone() }).contains("mixture weights"));
    // A ko weight that isn't finite would make every shaped reward NaN.
    for weight in [f32::NAN, f32::INFINITY] {
        let ko = crate::reward::RewardPreset::Ko { weight };
        assert!(refused(EnvConfig { reward: ko, ..base.clone() }).contains("ko reward's weight"));
    }
}

/// A reset with a seed whose buffers are refused changes nothing: not the
/// base seed, not the episodes.
#[test]
fn a_refused_reset_keeps_the_seed() {
    let e = env(config(Format::gen9_singles(), 3), 1);
    let mut b = e.buffers();
    e.reset(Some(1), &mut b.as_out()).unwrap();
    e.reset(None, &mut b.as_out()).unwrap();
    let mut wrong = StepBuffers::new(StepShape { n: 2, ..e.shape() });
    assert!(e.reset(Some(77), &mut wrong.as_out()).is_err());
    assert_eq!(e.seed(), 1);
    e.reset(None, &mut b.as_out()).unwrap();
    assert_eq!(b.episode, [6, 7, 8], "the episodes after the last reset's");
}

/// Mixture weights of any magnitude draw as their ratios do: scaled by a
/// power of two, exactly, before they are summed.
#[test]
fn mixture_weights_of_any_size_draw_alike() {
    let picks = |weights: [f64; 2]| {
        let mix =
            OpponentSpec::Mix(Arc::from(vec![(Opponent::External, weights[0]), (Opponent::random(), weights[1])]));
        let e = env(EnvConfig { opponent: mix, ..config(Format::gen9_singles(), 64) }, 1);
        e.reset(Some(SEED + 5), &mut e.buffers().as_out()).unwrap();
        (0..64).map(|i| e.with_env(i, |env| env.slot.opponent.is_external()).unwrap()).collect::<Vec<_>>()
    };
    let unit = picks([1.0, 1.0]);
    let external = unit.iter().filter(|&&x| x).count();
    assert!((16..=48).contains(&external), "{external}");
    assert_eq!(picks([1e308, 1e308]), unit, "a sum that would overflow");
    assert_eq!(picks([0.25, 0.25]), unit);
    assert_eq!(picks([1.0, 3.0]), picks([2f64.powi(900), 3.0 * 2f64.powi(900)]));
    assert_eq!(picks([1e308, 0.0]), vec![true; 64]);
    assert_eq!(picks([0.0, 1e-300]), vec![false; 64]);
}

/// Too many shards per thread is at most one shard per battle, not an
/// overflow into a pool without shards.
#[test]
fn shards_per_thread_is_clamped() {
    let base = config(Format::gen9_singles(), 4);
    let expected = super::tests::play(&env(base.clone(), 2), 20, SEED + 6);
    for shards in [0, usize::MAX / 2 + 1, usize::MAX] {
        let e = env(EnvConfig { shards_per_thread: shards, ..base.clone() }, 2);
        assert!(e.with_env(3, |_| ()).is_some());
        assert!(super::tests::play(&e, 20, SEED + 6) == expected, "{shards}");
    }
}

/// `set_pairs` replaces entries of a pair list for the episodes started
/// after it (the one under way keeps its teams), keeps `e % len`, and
/// refuses (changing nothing) a position out of range, a team that can't
/// play or another team source.
#[test]
fn set_pairs_changes_the_next_episodes_teams() {
    let format = Format::gen9_singles();
    let one = |s| TeamSheet::new(Team::new(vec![PokemonSet::new(s).with_moves(&[moves::TACKLE])]));
    let a = TeamSheet::new(sample_team(&format));
    let pair = |x: &TeamSheet, y: &TeamSheet| Arc::new([x.clone(), y.clone()]);
    let pairs = vec![pair(&a, &a), pair(&a, &a)];
    let e = env(EnvConfig { teams: Teams::Pairs(pairs), ..config(format.clone(), 1) }, 1);
    let mut b = e.buffers();
    e.reset(Some(SEED + 11), &mut b.as_out()).unwrap();
    let (pika, eevee) = (one(species::PIKACHU), one(species::EEVEE));
    e.set_pairs(vec![(1, pair(&pika, &eevee)), (0, pair(&eevee, &pika))]).unwrap();
    let sheets = |e: &VecEnv| e.with_env(0, |env| (env.episode, env.sheets.clone())).unwrap();
    assert_eq!(sheets(&e), (0, pair(&a, &a)), "the episode under way keeps its teams");
    let bad = TeamSheet::new(Team::new(Vec::new()));
    assert!(e.set_pairs(vec![(0, pair(&a, &a)), (2, pair(&a, &a))]).unwrap_err().contains("out of range"));
    assert!(e.set_pairs(vec![(0, pair(&a, &a)), (1, pair(&a, &bad))]).unwrap_err().contains("player 1's team"));
    let actions = vec![RANDOM_ACTION; 2 * e.actions_per_player()];
    let mut seen = Vec::new();
    for _ in 0..2000 {
        e.step(&actions, &mut b.as_out()).unwrap();
        let (ep, s) = sheets(&e);
        if seen.last().map(|(x, _)| *x) != Some(ep) {
            seen.push((ep, s));
        }
        if seen.len() == 3 {
            break;
        }
    }
    assert_eq!(seen.len(), 3, "three episodes");
    assert_eq!(seen[1], (1, pair(&pika, &eevee)));
    assert_eq!(seen[2], (2, pair(&eevee, &pika)), "the refused calls changed nothing");
    let pool: Arc<[TeamSheet]> = Arc::from(vec![a.clone()]);
    let e = env(EnvConfig { teams: Teams::PerSide([pool.clone(), pool]), ..config(format, 1) }, 1);
    assert!(e.set_pairs(vec![(0, pair(&a, &a))]).unwrap_err().contains("list of team pairs"));
}
