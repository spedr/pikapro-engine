//! The PyO3 layer of `pikapro.rl` (feature `python`): the vector environment
//! (`pikapro_rl::env::VecEnv`) with numpy buffers bound once, tournament team
//! pools and evaluation. The
//! Python-facing API and its documentation are in `python/pikapro/rl/`, which
//! wraps these classes (their names start with `_`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use numpy::{Element, PyArray1, PyArrayDyn, PyArrayMethods, PyUntypedArrayMethods};
use pyo3::exceptions::{PyIndexError, PyRuntimeError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};

use pikapro::{BattleRng, Choice, Format, LegalActions, MAX_ACTIVE, Request, SideId, SlotAction};
use pikapro_protocol::{TeamSheet, export_sheet, pack_team};
use pikapro_rl::encode::Basic;
use pikapro_rl::env::{EnvConfig, OpponentSpec, OtsView, Profile, VecEnv};
use pikapro_rl::policy::Policy;
use pikapro_rl::reward::RewardPreset;
use pikapro_rl::series::SERIES_WIDTH;
use pikapro_rl::vec::{Autoreset, FinalOut, MaskLayout, Observation, Opponent, PreviewMode, StepOut, SwitchBy, Teams};
use pikapro_view::{HpReport, PlayerView};

use crate::game::Game;
use crate::python::{PyActionSpace, PyBattle, PyChoice, PyRandomTeams, arg, err, format_arg, py_choice, teams_arg};

pub(crate) fn array<'py, T: Element>(py: Python<'py>, v: Vec<T>, shape: Vec<usize>) -> PyResult<Bound<'py, PyAny>> {
    Ok(PyArray1::from_vec(py, v).reshape(shape)?.into_any())
}

// ---------------------------------------------------------------------------
// Observations.

/// What an observation writes, for Python: its name, size and column
/// names.
#[derive(Clone, Debug)]
pub(crate) struct ObsInfo {
    /// `"basic"`.
    pub(crate) kind: String,
    pub(crate) size: usize,
    /// Each value's name, by offset (`own0.hp`, `field.weather_rain`).
    pub(crate) columns: Vec<String>,
}

/// The observation named `name`: `"basic"`.
pub(crate) fn observation_arg(name: &str) -> PyResult<(Observation, ObsInfo)> {
    match name {
        "basic" => Ok((
            Observation::new(Basic),
            ObsInfo { kind: "basic".into(), size: Basic::SIZE, columns: Basic::columns() },
        )),
        other => Err(err(format!("unknown observation {other:?} (names: basic)"))),
    }
}

pub(crate) fn obs_info_dict<'py>(py: Python<'py>, info: &ObsInfo) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("kind", &info.kind)?;
    d.set_item("size", info.size)?;
    d.set_item("columns", info.columns.clone())?;
    Ok(d)
}

// ---------------------------------------------------------------------------
// The action table.

/// `format`'s factored action table (`pikapro.rl.action_table`): `columns`
/// (the names, by offset), `table` int16 `(slot_size + preview_size,
/// columns)`, `slot_size`, and `preview_orders` int16 `(preview_size,
/// bring)` (each order's team positions).
#[pyfunction]
fn _action_table<'py>(py: Python<'py>, format: &str) -> PyResult<Bound<'py, PyDict>> {
    use pikapro_rl::space::{ActionSpace, action_col};
    let format = format_arg(format)?;
    let space = ActionSpace::new(&format);
    let t = space.action_table();
    let d = PyDict::new(py);
    d.set_item("columns", action_col::NAMES.to_vec())?;
    d.set_item("slot_size", t.slot_size)?;
    let n = t.len();
    let flat: Vec<i16> = t.rows.iter().flatten().copied().collect();
    d.set_item("table", array(py, flat, vec![n, action_col::COUNT])?)?;
    let orders: Vec<i16> = (0..space.preview_size)
        .flat_map(|k| space.preview_order(k).map(|o| o.iter().map(|&p| i16::from(p)).collect::<Vec<_>>()))
        .flatten()
        .collect();
    d.set_item("preview_orders", array(py, orders, vec![space.preview_size, space.bring])?)?;
    Ok(d)
}

/// A doubles target code (0 to 4) relative to the acting `slot`: 0 no
/// target, 1 and 2 the foes in slots 0 and 1, 3 the user itself, 4 its
/// ally (codes 3 and 4 are the user's side's slots, absolutely).
#[pyfunction]
fn relative_target(
    #[pyo3(from_py_with = arg::code_slot)] slot: usize,
    #[pyo3(from_py_with = arg::code)] target: usize,
) -> PyResult<usize> {
    pikapro_rl::space::relative_target(slot, target)
        .ok_or_else(|| err(format!("no target code {target} for slot {slot} (slots 0-1, codes 0-4)")))
}

// ---------------------------------------------------------------------------
// Team pools.

/// A validated pool of teams for one format (a tournament team pool): the
/// teams that import and pass the format's validator, and the rejects in
/// Showdown's words. `pikapro.rl.teams` builds them from pokepaste lists,
/// export texts and directories.
#[pyclass(module = "pikapro._pikapro", name = "TeamPool", frozen, skip_from_py_object)]
#[derive(Debug)]
pub struct PyTeamPool {
    format: Format,
    sheets: Arc<[TeamSheet]>,
    /// The label (source) of each kept team.
    labels: Vec<String>,
    /// `(index in the input, label, problems)`.
    rejected: Vec<(usize, String, Vec<String>)>,
}

#[pymethods]
impl PyTeamPool {
    /// Imports and validates `texts` (Showdown export or packed format) for
    /// `format`. `labels` names each text (a URL, a file) in the rejects.
    #[new]
    #[pyo3(signature = (texts, format, labels=None))]
    fn new(py: Python<'_>, texts: Vec<String>, format: &str, labels: Option<Vec<String>>) -> PyResult<Self> {
        let format = format_arg(format)?;
        let labels = labels.unwrap_or_else(|| (0..texts.len()).map(|i| format!("#{i}")).collect());
        if labels.len() != texts.len() {
            return Err(err(format!("{} labels for {} teams", labels.len(), texts.len())));
        }
        let (sheets, rejected) = py.detach(|| pikapro_rl::teams::import_pool(&texts, &format));
        let bad: std::collections::BTreeSet<usize> = rejected.iter().map(|r| r.index).collect();
        let kept = (0..texts.len()).filter(|i| !bad.contains(i)).map(|i| labels[i].clone()).collect();
        let rejected = rejected.into_iter().map(|r| (r.index, labels[r.index].clone(), r.problems)).collect();
        Ok(PyTeamPool { format, sheets: sheets.into(), labels: kept, rejected })
    }

    /// The format id the pool was validated for.
    #[getter]
    fn format(&self) -> &'static str {
        self.format.id
    }

    fn __len__(&self) -> usize {
        self.sheets.len()
    }

    /// The label of each team kept, in order.
    #[getter]
    fn labels(&self) -> Vec<String> {
        self.labels.clone()
    }

    /// The teams that didn't make it: `(index, label, problems)`, problems
    /// in Showdown's words.
    #[getter]
    fn rejected(&self) -> Vec<(usize, String, Vec<String>)> {
        self.rejected.clone()
    }

    /// Team `i` in export format (or packed).
    #[pyo3(signature = (i, packed=false))]
    fn team(&self, #[pyo3(from_py_with = arg::i)] i: usize, packed: bool) -> PyResult<String> {
        let s = self.sheets.get(i).ok_or_else(|| PyIndexError::new_err(format!("team {i} out of range")))?;
        Ok(if packed { pack_team(s, &self.format) } else { export_sheet(s, &self.format) })
    }

    /// Every team, in export format (or packed).
    #[pyo3(signature = (packed=false))]
    fn teams(&self, packed: bool) -> Vec<String> {
        self.sheets
            .iter()
            .map(|s| if packed { pack_team(s, &self.format) } else { export_sheet(s, &self.format) })
            .collect()
    }

    fn __repr__(&self) -> String {
        format!("TeamPool({:?}, {} teams, {} rejected)", self.format.id, self.sheets.len(), self.rejected.len())
    }
}

/// A team source from Python: a `RandomTeams`, a `TeamPool` (both players
/// draw from it), two `TeamPool`s (one per player), a pair of team texts,
/// or a list of pairs.
fn teams_source(teams: &Bound<'_, PyAny>, format: &Format) -> PyResult<Teams> {
    let check_pool = |p: &PyTeamPool| -> PyResult<Arc<[TeamSheet]>> {
        if p.format.id != format.id {
            return Err(err(format!("a {} team pool in a {} environment", p.format.id, format.id)));
        }
        if p.sheets.is_empty() {
            return Err(err("an empty team pool".into()));
        }
        Ok(p.sheets.clone())
    };
    if let Ok(random) = teams.cast::<PyRandomTeams>() {
        let generator = random.get().generator.clone();
        if generator.format().id != format.id {
            return Err(err(format!("RandomTeams for {} in a {} environment", generator.format().id, format.id)));
        }
        return Ok(Teams::Generator(generator));
    }
    if let Ok(pool) = teams.cast::<PyTeamPool>() {
        let p = check_pool(pool.get())?;
        return Ok(Teams::PerSide([p.clone(), p]));
    }
    if let Ok([a, b]) = teams.extract::<[Bound<'_, PyTeamPool>; 2]>() {
        return Ok(Teams::PerSide([check_pool(a.get())?, check_pool(b.get())?]));
    }
    if teams.extract::<[String; 2]>().is_ok() {
        return Ok(Teams::Pairs(vec![Arc::new(teams_arg(teams, format)?)]));
    }
    let list: Vec<Bound<'_, PyAny>> = teams.extract().map_err(|_| {
        PyTypeError::new_err(
            "teams must be None, 'sets', 'uniform' or 'random', a RandomTeams, a TeamPool, two TeamPools, a pair of \
             teams or a list of pairs",
        )
    })?;
    if list.is_empty() {
        return Err(err("no teams".into()));
    }
    Ok(Teams::Pairs(list.iter().map(|p| teams_arg(p, format).map(Arc::new)).collect::<PyResult<Vec<_>>>()?))
}

// ---------------------------------------------------------------------------
// Opponents.

fn policy_by_name(name: &str) -> PyResult<Arc<dyn Policy>> {
    pikapro_rl::session::policy_by_name(name)
        .map_err(|e| err(if name == "external" { format!("{e}: external isn't a built-in policy") } else { e }))
}

fn opponent_one(name: &str) -> PyResult<Opponent> {
    match name {
        "external" => Ok(Opponent::External),
        name => Ok(Opponent::Policy(policy_by_name(name)?)),
    }
}

/// An opponent: a name (`"random"`, `"max_base_power"`,
/// `"simple_heuristics"`, `"external"`), or a mixture drawn per episode:
/// a dict `{name: weight}` or a list of `(name, weight)`.
fn opponent_arg(o: &Bound<'_, PyAny>) -> PyResult<OpponentSpec> {
    if let Ok(s) = o.cast::<PyString>() {
        return Ok(OpponentSpec::One(opponent_one(s.to_str()?)?));
    }
    let pairs: Vec<(String, f64)> = if let Ok(d) = o.cast::<PyDict>() {
        d.iter().map(|(k, v)| Ok((k.extract()?, v.extract()?))).collect::<PyResult<_>>()?
    } else {
        o.extract().map_err(|_| {
            PyTypeError::new_err("opponent must be a name, a dict {name: weight} or a list of (name, weight)")
        })?
    };
    let mix = pairs.iter().map(|(n, w)| Ok((opponent_one(n)?, *w))).collect::<PyResult<Vec<_>>>()?;
    Ok(OpponentSpec::Mix(mix.into()))
}

// ---------------------------------------------------------------------------
// The vector environment.

type Arr<T> = Py<PyArrayDyn<T>>;

#[derive(Debug)]
struct FinalBufs {
    obs: Arr<f32>,
    episode: Arr<u64>,
    turns: Arr<u16>,
    series: Option<Arr<u8>>,
}

/// The output buffers a step writes, numpy arrays bound once.
#[derive(Debug)]
struct Bufs {
    obs: Arr<f32>,
    masks: Arr<bool>,
    min_switches: Arr<u8>,
    reward: Arr<f32>,
    terminated: Arr<bool>,
    truncated: Arr<bool>,
    end_reason: Arr<u8>,
    winner: Arr<i8>,
    needs_action: Arr<bool>,
    status: Arr<i8>,
    agent_side: Arr<u8>,
    episode: Arr<u64>,
    series: Option<Arr<u8>>,
    fin: Option<FinalBufs>,
}

fn zeros<T: Element>(py: Python<'_>, shape: &[usize]) -> Arr<T> {
    PyArrayDyn::<T>::zeros(py, shape.to_vec(), false).unbind()
}

/// Every buffer's name, shape and dtype for an environment.
fn buffer_specs(env: &VecEnv) -> Vec<(String, Vec<usize>, &'static str)> {
    let shape = env.shape();
    let n = shape.n;
    let config = env.config();
    let obs: Vec<usize> = vec![n, 2, shape.obs];
    let mut out = vec![("obs".to_string(), obs.clone(), "float32")];
    out.extend([
        ("masks".to_string(), vec![n, 2, shape.mask], "bool"),
        ("min_switches".to_string(), vec![n, 2], "uint8"),
        ("reward".to_string(), vec![n, 2], "float32"),
        ("terminated".to_string(), vec![n], "bool"),
        ("truncated".to_string(), vec![n], "bool"),
        ("end_reason".to_string(), vec![n], "uint8"),
        ("winner".to_string(), vec![n], "int8"),
        ("needs_action".to_string(), vec![n, 2], "bool"),
        ("status".to_string(), vec![n, 2], "int8"),
        ("agent_side".to_string(), vec![n], "uint8"),
        ("episode".to_string(), vec![n], "uint64"),
    ]);
    if config.series.is_some() {
        out.push(("series".to_string(), vec![n, SERIES_WIDTH], "uint8"));
    }
    if config.autoreset == Autoreset::SameStep {
        out.push(("final_obs".to_string(), obs, "float32"));
        out.push(("final_episode".to_string(), vec![n], "uint64"));
        out.push(("final_turns".to_string(), vec![n], "uint16"));
        if config.series.is_some() {
            out.push(("final_series".to_string(), vec![n, SERIES_WIDTH], "uint8"));
        }
    }
    out
}

fn new_bufs(py: Python<'_>, env: &VecEnv) -> Bufs {
    let specs = buffer_specs(env);
    let shape_of = |name: &str| specs.iter().find(|s| s.0 == name).map(|s| s.1.clone()).unwrap_or_default();
    let series = env.config().series.is_some();
    let same_step = env.config().autoreset == Autoreset::SameStep;
    Bufs {
        obs: zeros(py, &shape_of("obs")),
        masks: zeros(py, &shape_of("masks")),
        min_switches: zeros(py, &shape_of("min_switches")),
        reward: zeros(py, &shape_of("reward")),
        terminated: zeros(py, &shape_of("terminated")),
        truncated: zeros(py, &shape_of("truncated")),
        end_reason: zeros(py, &shape_of("end_reason")),
        winner: zeros(py, &shape_of("winner")),
        needs_action: zeros(py, &shape_of("needs_action")),
        status: zeros(py, &shape_of("status")),
        agent_side: zeros(py, &shape_of("agent_side")),
        episode: zeros(py, &shape_of("episode")),
        series: series.then(|| zeros(py, &shape_of("series"))),
        fin: same_step.then(|| FinalBufs {
            obs: zeros(py, &shape_of("final_obs")),
            episode: zeros(py, &shape_of("final_episode")),
            turns: zeros(py, &shape_of("final_turns")),
            series: series.then(|| zeros(py, &shape_of("final_series"))),
        }),
    }
}

/// A buffer the caller binds: the right dtype, C-contiguous, writeable, and
/// `len` values (any shape).
fn bindable<T: Element>(a: &Bound<'_, PyAny>, name: &str, len: usize, dtype: &str) -> PyResult<Arr<T>> {
    let arr = a.cast::<PyArrayDyn<T>>().map_err(|_| {
        let got = a.getattr("dtype").and_then(|d| d.str().map(|s| s.to_string())).unwrap_or_else(|_| "?".into());
        PyTypeError::new_err(format!("{name}: expected a numpy array of {dtype}, got {got}"))
    })?;
    if !arr.is_c_contiguous() {
        return Err(err(format!("{name}: the array must be C-contiguous")));
    }
    if !arr.is_aligned() {
        return Err(err(format!("{name}: the array must be aligned (a view at an odd byte offset isn't)")));
    }
    if arr.len() != len {
        return Err(err(format!("{name}: {} values, expected {len}", arr.len())));
    }
    arr.try_readwrite().map_err(|e| err(format!("{name}: not writeable ({e})")))?;
    Ok(arr.clone().unbind())
}

/// The vector environment, with numpy output buffers bound once. See
/// `pikapro.rl.VecEnv`, which wraps it.
#[pyclass(module = "pikapro._pikapro", name = "_VecEnv", skip_from_py_object)]
pub struct PyVecEnv {
    env: VecEnv,
    info: ObsInfo,
    bufs: Bufs,
}

impl std::fmt::Debug for PyVecEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PyVecEnv").field("env", &self.env).finish()
    }
}

/// Borrows buffers mutably for one call.
macro_rules! rw {
    ($py:expr, $a:expr) => {
        $a.bind($py).try_readwrite().map_err(|e| PyRuntimeError::new_err(format!("a buffer is in use: {e}")))?
    };
}

macro_rules! slice {
    ($g:expr) => {
        $g.as_slice_mut().map_err(|e| err(format!("a buffer is not contiguous: {e}")))?
    };
}

impl PyVecEnv {
    /// One call: a step with `actions`, or a reset.
    fn run(&self, py: Python<'_>, actions: Option<&[i64]>, seed: Option<u64>) -> PyResult<()> {
        let b = &self.bufs;
        let mut obs = rw!(py, b.obs);
        let mut masks = rw!(py, b.masks);
        let mut min_switches = rw!(py, b.min_switches);
        let mut reward = rw!(py, b.reward);
        let mut terminated = rw!(py, b.terminated);
        let mut truncated = rw!(py, b.truncated);
        let mut end_reason = rw!(py, b.end_reason);
        let mut winner = rw!(py, b.winner);
        let mut needs_action = rw!(py, b.needs_action);
        let mut status = rw!(py, b.status);
        let mut agent_side = rw!(py, b.agent_side);
        let mut episode = rw!(py, b.episode);
        let mut series = match &b.series {
            Some(s) => Some(rw!(py, s)),
            None => None,
        };
        let mut fin = match &b.fin {
            Some(f) => Some((
                rw!(py, f.obs),
                rw!(py, f.episode),
                rw!(py, f.turns),
                match &f.series {
                    Some(s) => Some(rw!(py, s)),
                    None => None,
                },
            )),
            None => None,
        };
        let final_out = match &mut fin {
            Some((o, e, t, s)) => Some(FinalOut {
                obs: slice!(o),
                episode: slice!(e),
                turns: slice!(t),
                series: match s {
                    Some(s) => Some(slice!(s)),
                    None => None,
                },
            }),
            None => None,
        };
        let mut out = StepOut {
            obs: slice!(obs),
            masks: slice!(masks),
            min_switches: slice!(min_switches),
            reward: slice!(reward),
            terminated: slice!(terminated),
            truncated: slice!(truncated),
            end_reason: slice!(end_reason),
            winner: slice!(winner),
            needs_action: slice!(needs_action),
            status: slice!(status),
            agent_side: slice!(agent_side),
            episode: slice!(episode),
            series: match &mut series {
                Some(s) => Some(slice!(s)),
                None => None,
            },
            final_out,
        };
        let env = &self.env;
        let result = py.detach(|| match actions {
            Some(a) => env.step(a, &mut out),
            None => env.reset(seed, &mut out),
        });
        result.map_err(PyRuntimeError::new_err)
    }

    fn check_battle(&self, i: usize) -> PyResult<()> {
        if i >= self.env.config().n {
            return Err(PyIndexError::new_err(format!("battle {i} out of range (n = {})", self.env.config().n)));
        }
        Ok(())
    }
}

/// The names of the `masks`, `switch_by` and `hp` options.
pub(crate) const MASKS: [(&str, MaskLayout); 2] = [("factored", MaskLayout::Factored), ("joint", MaskLayout::Joint)];
pub(crate) const SWITCH_BY: [(&str, SwitchBy); 2] = [("row", SwitchBy::Row), ("position", SwitchBy::Position)];
pub(crate) const HP: [(&str, HpReport); 2] = [("percent", HpReport::Percent), ("exact", HpReport::Exact)];

pub(crate) fn parse_choice_of<T: Copy>(name: &str, value: &str, options: &[(&str, T)]) -> PyResult<T> {
    options.iter().find(|(n, _)| *n == value).map(|(_, v)| *v).ok_or_else(|| {
        let names: Vec<&str> = options.iter().map(|(n, _)| *n).collect();
        err(format!("{name} must be one of {}, got {value:?}", names.join(", ")))
    })
}

#[pymethods]
impl PyVecEnv {
    #[new]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (format, n, teams, *, threads=None, seed=0, opponent=None, reward="terminal", ko_weight=0.1,
        max_turns=None, observation="basic", masks="factored", switch_by="row", autoreset="next_step", ots=0.0,
        ots_view="both", randomize_sides=false, series=None, observe_builtin=false, preview="learned", hp="percent", record=false,
        shards_per_thread=8, spin_us=Duration::from_micros(50)))]
    fn new(
        py: Python<'_>,
        format: &str,
        #[pyo3(from_py_with = arg::n)] n: usize,
        teams: &Bound<'_, PyAny>,
        #[pyo3(from_py_with = arg::threads)] threads: Option<usize>,
        #[pyo3(from_py_with = arg::seed)] seed: u64,
        opponent: Option<&Bound<'_, PyAny>>,
        reward: &str,
        ko_weight: f32,
        #[pyo3(from_py_with = arg::max_turns)] max_turns: Option<u16>,
        observation: &str,
        masks: &str,
        switch_by: &str,
        autoreset: &str,
        ots: f64,
        ots_view: &str,
        randomize_sides: bool,
        #[pyo3(from_py_with = arg::series)] series: Option<u8>,
        observe_builtin: bool,
        preview: &str,
        hp: &str,
        record: bool,
        #[pyo3(from_py_with = arg::shards_per_thread)] shards_per_thread: usize,
        #[pyo3(from_py_with = arg::spin_us)] spin_us: Duration,
    ) -> PyResult<Self> {
        let format = format_arg(format)?;
        if n == 0 {
            return Err(err("n must be at least 1".into()));
        }
        let teams = teams_source(teams, &format)?;
        let (observation, info) = observation_arg(observation)?;
        let mut config = EnvConfig::new(format, n, teams);
        config.seed = seed;
        if let Some(o) = opponent {
            config.opponent = opponent_arg(o)?;
        }
        config.reward = match reward {
            "terminal" => RewardPreset::Terminal,
            "ko" if !ko_weight.is_finite() => return Err(err(format!("ko_weight must be finite, got {ko_weight}"))),
            "ko" => RewardPreset::Ko { weight: ko_weight },
            other => return Err(err(format!("reward must be 'terminal' or 'ko', got {other:?}"))),
        };
        config.max_turns = max_turns;
        config.observation = observation;
        config.masks = parse_choice_of("masks", masks, &MASKS)?;
        config.switch_by = parse_choice_of("switch_by", switch_by, &SWITCH_BY)?;
        config.autoreset = parse_choice_of(
            "autoreset",
            autoreset,
            &[("next_step", Autoreset::NextStep), ("same_step", Autoreset::SameStep)],
        )?;
        config.preview = parse_choice_of(
            "preview",
            preview,
            &[("learned", PreviewMode::Learned), ("random", PreviewMode::Random), ("fixed", PreviewMode::Fixed)],
        )?;
        config.hp = parse_choice_of("hp", hp, &HP)?;
        config.ots = ots;
        let views = [("both", OtsView::Both), ("player0", OtsView::Player0)];
        config.ots_view = parse_choice_of("ots_view", ots_view, &views)?;
        config.randomize_sides = randomize_sides;
        config.series = series;
        config.observe_builtin = observe_builtin;
        config.record = record;
        config.shards_per_thread = shards_per_thread.max(1);
        config.spin = spin_us;
        let threads = threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
        let env = py.detach(|| VecEnv::new(config, threads)).map_err(err)?;
        let bufs = new_bufs(py, &env);
        Ok(PyVecEnv { env, info, bufs })
    }

    #[getter]
    fn n(&self) -> usize {
        self.env.config().n
    }

    #[getter]
    fn threads(&self) -> usize {
        self.env.threads()
    }

    #[getter]
    fn format(&self) -> &'static str {
        self.env.config().format.id
    }

    #[getter]
    fn space(&self) -> PyActionSpace {
        PyActionSpace { space: Arc::new(self.env.space().clone()) }
    }

    /// Action values per battle and player: `active` slot actions with
    /// factored masks, one choice index with joint masks.
    #[getter]
    fn actions_per_player(&self) -> usize {
        self.env.actions_per_player()
    }

    /// Mask values per player.
    #[getter]
    fn mask_width(&self) -> usize {
        self.env.shape().mask
    }

    /// The observation: `kind`, `size` and `columns` (each value's name).
    #[getter]
    fn observation<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        obs_info_dict(py, &self.info)
    }

    /// The configuration, as the names `pikapro.rl.VecEnv` takes (it adds
    /// the options this class doesn't keep).
    #[getter]
    fn config<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let c = self.env.config();
        let d = PyDict::new(py);
        d.set_item("format", c.format.id)?;
        d.set_item("n", c.n)?;
        // The base seed of the episodes under way (`reset(seed=)` changes it).
        d.set_item("seed", self.env.seed())?;
        d.set_item(
            "reward",
            match c.reward {
                RewardPreset::Terminal => "terminal",
                RewardPreset::Ko { .. } => "ko",
            },
        )?;
        if let RewardPreset::Ko { weight } = c.reward {
            d.set_item("ko_weight", weight)?;
        }
        d.set_item("max_turns", c.max_turns)?;
        d.set_item("masks", if c.masks == MaskLayout::Joint { "joint" } else { "factored" })?;
        d.set_item("switch_by", if c.switch_by == SwitchBy::Row { "row" } else { "position" })?;
        d.set_item("autoreset", if c.autoreset == Autoreset::SameStep { "same_step" } else { "next_step" })?;
        d.set_item(
            "preview",
            match c.preview {
                PreviewMode::Learned => "learned",
                PreviewMode::Random => "random",
                PreviewMode::Fixed => "fixed",
            },
        )?;
        d.set_item("hp", if c.hp == HpReport::Exact { "exact" } else { "percent" })?;
        d.set_item("ots", c.ots)?;
        d.set_item(
            "ots_view",
            match c.ots_view {
                OtsView::Player0 => "player0",
                _ => "both",
            },
        )?;
        d.set_item("side_randomization", c.randomize_sides)?;
        d.set_item("series", c.series)?;
        d.set_item("observe_builtin", c.observe_builtin)?;
        d.set_item("record", c.record)?;
        Ok(d)
    }

    /// Every buffer's `(name, shape, dtype)`.
    fn buffer_specs(&self) -> Vec<(String, Vec<usize>, &'static str)> {
        buffer_specs(&self.env)
    }

    /// The bound buffers, by name (the arrays themselves: a step writes
    /// into them).
    fn buffers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let b = &self.bufs;
        let d = PyDict::new(py);
        d.set_item("obs", b.obs.bind(py))?;
        d.set_item("masks", b.masks.bind(py))?;
        d.set_item("min_switches", b.min_switches.bind(py))?;
        d.set_item("reward", b.reward.bind(py))?;
        d.set_item("terminated", b.terminated.bind(py))?;
        d.set_item("truncated", b.truncated.bind(py))?;
        d.set_item("end_reason", b.end_reason.bind(py))?;
        d.set_item("winner", b.winner.bind(py))?;
        d.set_item("needs_action", b.needs_action.bind(py))?;
        d.set_item("status", b.status.bind(py))?;
        d.set_item("agent_side", b.agent_side.bind(py))?;
        d.set_item("episode", b.episode.bind(py))?;
        if let Some(s) = &b.series {
            d.set_item("series", s.bind(py))?;
        }
        if let Some(f) = &b.fin {
            d.set_item("final_obs", f.obs.bind(py))?;
            d.set_item("final_episode", f.episode.bind(py))?;
            d.set_item("final_turns", f.turns.bind(py))?;
            if let Some(s) = &f.series {
                d.set_item("final_series", s.bind(py))?;
            }
        }
        Ok(d)
    }

    /// Binds the buffer `name` to `array` (its dtype, C-contiguous,
    /// writeable, the buffer's number of values in any shape): steps write
    /// there from now on, with no copy (a numpy view of pinned torch memory
    /// works).
    fn bind(&mut self, name: &str, array: &Bound<'_, PyAny>) -> PyResult<()> {
        let specs = buffer_specs(&self.env);
        let Some((_, shape, dtype)) = specs.iter().find(|s| s.0 == name) else {
            let names: Vec<&str> = specs.iter().map(|s| s.0.as_str()).collect();
            return Err(err(format!("no buffer {name:?} (buffers: {})", names.join(", "))));
        };
        let len: usize = shape.iter().product();
        let b = &mut self.bufs;
        let missing = || err(format!("no buffer {name:?}"));
        match name {
            "obs" => b.obs = bindable(array, name, len, dtype)?,
            "masks" => b.masks = bindable(array, name, len, dtype)?,
            "min_switches" => b.min_switches = bindable(array, name, len, dtype)?,
            "reward" => b.reward = bindable(array, name, len, dtype)?,
            "terminated" => b.terminated = bindable(array, name, len, dtype)?,
            "truncated" => b.truncated = bindable(array, name, len, dtype)?,
            "end_reason" => b.end_reason = bindable(array, name, len, dtype)?,
            "winner" => b.winner = bindable(array, name, len, dtype)?,
            "needs_action" => b.needs_action = bindable(array, name, len, dtype)?,
            "status" => b.status = bindable(array, name, len, dtype)?,
            "agent_side" => b.agent_side = bindable(array, name, len, dtype)?,
            "episode" => b.episode = bindable(array, name, len, dtype)?,
            "series" => b.series = Some(bindable(array, name, len, dtype)?),
            _ => {
                let f = b.fin.as_mut().ok_or_else(missing)?;
                match name {
                    "final_obs" => f.obs = bindable(array, name, len, dtype)?,
                    "final_episode" => f.episode = bindable(array, name, len, dtype)?,
                    "final_turns" => f.turns = bindable(array, name, len, dtype)?,
                    "final_series" => f.series = Some(bindable(array, name, len, dtype)?),
                    _ => return Err(missing()),
                }
            }
        }
        Ok(())
    }

    /// Starts a new episode in every battle and writes the first
    /// observations into the buffers (see `pikapro_rl::env::VecEnv::reset`).
    #[pyo3(signature = (seed=None))]
    fn reset(&self, py: Python<'_>, #[pyo3(from_py_with = arg::seed_or_none)] seed: Option<u64>) -> PyResult<()> {
        self.run(py, None, seed)
    }

    /// One request round of every battle: `actions`, int64, C-contiguous,
    /// `n * 2 * actions_per_player` values by battle and player (any
    /// shape). The outputs go into the buffers.
    fn step(&self, py: Python<'_>, actions: &Bound<'_, PyAny>) -> PyResult<()> {
        let want = self.env.config().n * 2 * self.env.actions_per_player();
        let arr = actions
            .cast::<PyArrayDyn<i64>>()
            .map_err(|_| PyTypeError::new_err("actions must be a numpy int64 array (pikapro.rl.VecEnv converts)"))?;
        if arr.len() != want {
            return Err(err(format!("expected {want} actions, got {}", arr.len())));
        }
        let ro = arr.try_readonly().map_err(|e| PyRuntimeError::new_err(format!("actions: {e}")))?;
        let slice = ro.as_slice().map_err(|_| err("actions must be C-contiguous".into()))?;
        self.run(py, Some(slice), None)
    }

    /// Changes the opponent of the given battles (all when `None`), from
    /// each one's next episode on.
    #[pyo3(signature = (opponent, which=None))]
    fn set_opponent(
        &self,
        opponent: &Bound<'_, PyAny>,
        #[pyo3(from_py_with = arg::which)] which: Option<Vec<usize>>,
    ) -> PyResult<()> {
        let o = opponent_arg(opponent)?;
        self.env.set_opponent(which.as_deref(), o).map_err(err)
    }

    /// Replaces entries of the environment's list of team pairs (made with
    /// ``teams=[(p1, p2), ...]``): ``pairs[j]`` (two team texts, export or
    /// packed) goes to position ``positions[j]`` of the list, for the
    /// episodes started from now on (an episode under way keeps its
    /// teams). The list keeps its length, so episode ``e`` still plays
    /// position ``e % len``. A team that doesn't parse or that the format
    /// refuses, or a position out of range, raises and changes nothing.
    fn set_pairs(
        &self,
        py: Python<'_>,
        #[pyo3(from_py_with = arg::positions)] positions: Vec<usize>,
        pairs: Vec<(String, String)>,
    ) -> PyResult<()> {
        if positions.len() != pairs.len() {
            return Err(err(format!("{} positions for {} pairs", positions.len(), pairs.len())));
        }
        let format = &self.env.config().format;
        // Parsing (most of the cost) runs without the GIL.
        py.detach(|| {
            let updates = positions
                .into_iter()
                .zip(&pairs)
                .map(|(k, (a, b))| {
                    let p1 = crate::game::parse_team(a, format).map_err(|e| format!("pair for {k}: p1 {e}"))?;
                    let p2 = crate::game::parse_team(b, format).map_err(|e| format!("pair for {k}: p2 {e}"))?;
                    Ok((k, Arc::new([p1, p2])))
                })
                .collect::<Result<Vec<_>, String>>()?;
            self.env.set_pairs(updates)
        })
        .map_err(err)
    }

    /// Battle `i`'s current episode's record (`previous`: the one before),
    /// with `record=True`: a dict of `episode`, `seed`, `teams` (p1's and
    /// p2's, packed), `agent_side` and `choices` (`(side, Choice)` in
    /// order), or `None`.
    #[pyo3(signature = (i, previous=false))]
    fn record<'py>(
        &self,
        py: Python<'py>,
        #[pyo3(from_py_with = arg::i)] i: usize,
        previous: bool,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        self.check_battle(i)?;
        let Some(r) = self.env.record(i, previous) else { return Ok(None) };
        let format = &self.env.config().format;
        let active = self.env.space().active;
        let d = PyDict::new(py);
        d.set_item("episode", r.episode)?;
        d.set_item("seed", r.seed)?;
        d.set_item("teams", r.sheets.iter().map(|s| pack_team(s, format)).collect::<Vec<_>>())?;
        d.set_item("agent_side", r.agent_side.index())?;
        let choices = PyList::empty(py);
        for (side, c) in &r.choices {
            choices.append((side.index(), py_choice(*c, active)))?;
        }
        d.set_item("choices", choices)?;
        Ok(Some(d))
    }

    /// A copy of battle `i` as a `pikapro.Battle` (its event log holds only
    /// what the environment hasn't consumed yet).
    fn battle(&self, py: Python<'_>, #[pyo3(from_py_with = arg::i)] i: usize) -> PyResult<PyBattle> {
        self.check_battle(i)?;
        let space = Arc::new(self.env.space().clone());
        let env = &self.env;
        let game = py.detach(|| {
            env.with_env(i, |e| {
                let mut game = Game::from_battle(e.battle.clone(), space, e.sheets.clone());
                // The environment's memory of each side's own team.
                game.team = SideId::BOTH.map(|side| *e.slot.team(side));
                game
            })
        });
        game.map(|game| PyBattle { game }).ok_or_else(|| PyIndexError::new_err(format!("battle {i}")))
    }

    /// Player `player`'s current view of battle `i` as plain values, for
    /// `pikapro.rl.BattleView`.
    fn _view<'py>(
        &self,
        py: Python<'py>,
        #[pyo3(from_py_with = arg::i)] i: usize,
        #[pyo3(from_py_with = arg::player)] player: usize,
    ) -> PyResult<Bound<'py, PyDict>> {
        self.check_battle(i)?;
        let format = &self.env.config().format;
        self.env
            .with_env(i, |e| {
                // As the environment's own encoders see it (with the series
                // this game is part of).
                let view = e.slot.view(e.slot.side_of(player));
                crate::view::view_dict(py, &view, format)
            })
            .ok_or_else(|| PyIndexError::new_err(format!("battle {i}")))?
    }

    /// The side (0 p1, 1 p2) player `player` plays in battle `i`'s episode.
    fn side_of(
        &self,
        #[pyo3(from_py_with = arg::i)] i: usize,
        #[pyo3(from_py_with = arg::player)] player: usize,
    ) -> PyResult<usize> {
        self.check_battle(i)?;
        self.env.with_env(i, |e| e.slot.side_of(player).index()).ok_or_else(|| PyIndexError::new_err("battle"))
    }

    /// The choice actions (`actions_per_player` values, as `step` takes
    /// them) make for player `player`'s current request in battle `i`, as
    /// a `Choice` (`str()` is Showdown's choice string, switches as team
    /// positions). Doesn't check the mask.
    fn decode(
        &self,
        #[pyo3(from_py_with = arg::i)] i: usize,
        #[pyo3(from_py_with = arg::player)] player: usize,
        #[pyo3(from_py_with = arg::actions)] actions: Vec<i64>,
    ) -> PyResult<PyChoice> {
        self.check_battle(i)?;
        let per = self.env.actions_per_player();
        if actions.len() != per {
            return Err(err(format!("expected {per} actions, got {}", actions.len())));
        }
        let space = self.env.space();
        let layout = self.env.config().masks;
        let by_row = self.env.config().switch_by == SwitchBy::Row;
        let result = self.env.with_env(i, |e| -> Result<Choice, String> {
            let side = e.slot.side_of(player);
            let idx = |a: i64| usize::try_from(a).map_err(|_| format!("action {a} out of range"));
            match (e.battle.request(side), layout) {
                (Request::Wait, _) => Err("the player has nothing to decide".into()),
                (Request::TeamPreview { .. }, l) => {
                    let k = idx(actions[0])?;
                    let k = if l == MaskLayout::Joint {
                        k.checked_sub(space.joint_size).ok_or("not a team order")?
                    } else {
                        k
                    };
                    let team_len = e.battle.state().side(side).team.len();
                    space.preview_choice(k, team_len).ok_or_else(|| format!("order {k} out of range"))
                }
                (_, l) => {
                    let mut slots = [0usize; MAX_ACTIVE];
                    if l == MaskLayout::Joint {
                        let j = idx(actions[0])?;
                        if j >= space.joint_size {
                            return Err(format!("choice {j} is a team order"));
                        }
                        slots = space.split_joint(j);
                    } else {
                        for (s, &a) in slots.iter_mut().zip(&actions) {
                            *s = idx(a)?;
                        }
                    }
                    let mut out = [SlotAction::Pass; MAX_ACTIVE];
                    for (o, &a) in out.iter_mut().zip(&slots[..space.active]) {
                        *o = match space.decode_slot(a).ok_or_else(|| format!("slot action {a} out of range"))? {
                            SlotAction::Switch(k) if by_row => {
                                let own = e.slot.own(side);
                                SlotAction::Switch(
                                    own.by_id(k).map(|(pos, _)| pos as u8).ok_or("no Pokémon in that row")?,
                                )
                            }
                            other => other,
                        };
                    }
                    Ok(Choice::Actions(out))
                }
            }
        });
        let choice = result.ok_or_else(|| PyIndexError::new_err("battle"))?.map_err(err)?;
        Ok(py_choice(choice, space.active))
    }

    /// The battles that failed since the last call (`end_reason` 5), in
    /// order: `(battle, episode, message)`.
    fn take_errors(&self) -> Vec<(usize, u64, String)> {
        self.env.take_errors().into_iter().map(|e| (e.battle, e.episode, e.message)).collect()
    }

    /// Turns the per-phase clock on or off.
    fn set_profile(&self, on: bool) {
        self.env.set_profile(on);
    }

    /// Nanoseconds per phase since the last call (with `set_profile`).
    fn take_profile<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let p: Profile = self.env.take_profile();
        let d = PyDict::new(py);
        for (k, v) in [
            ("engine", p.engine),
            ("view", p.view),
            ("overlay", p.overlay),
            ("encode", p.encode),
            ("masks", p.masks),
            ("other", p.other),
            ("wall", p.wall),
            ("copy", p.copy),
            ("wait", p.wait),
            ("steps", p.steps),
            ("decisions", p.decisions),
            ("battles", p.battles),
        ] {
            d.set_item(k, v)?;
        }
        Ok(d)
    }

    fn __repr__(&self) -> String {
        let c = self.env.config();
        format!(
            "VecEnv({:?}, n={}, threads={}, observation={:?})",
            c.format.id,
            c.n,
            self.env.threads(),
            self.info.kind
        )
    }
}

// ---------------------------------------------------------------------------
// Evaluation.

/// Every built-in policy against every other over `episodes` mirrored
/// episodes (`pikapro_rl::eval::cross_evaluate`): `records[i][j]` is policy
/// `i`'s `(wins, losses, ties, turn_limit, turns)` against `j` (`None` on
/// the diagonal).
#[pyfunction]
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
#[pyo3(signature = (policies, format, teams, episodes, seed=0, threads=None, hp="percent", batch=0))]
fn _cross_evaluate(
    py: Python<'_>,
    policies: Vec<String>,
    format: &str,
    teams: &Bound<'_, PyAny>,
    #[pyo3(from_py_with = arg::episodes)] episodes: u64,
    #[pyo3(from_py_with = arg::seed)] seed: u64,
    #[pyo3(from_py_with = arg::threads)] threads: Option<usize>,
    hp: &str,
    #[pyo3(from_py_with = arg::batch)] batch: usize,
) -> PyResult<Vec<Vec<Option<(u64, u64, u64, u64, u64)>>>> {
    let format = format_arg(format)?;
    let teams = teams_source(teams, &format)?;
    let stop = Arc::new(AtomicBool::new(false));
    let entrants: Vec<pikapro_rl::eval::Entrant> = policies
        .iter()
        .map(|p| {
            let inner = policy_by_name(p)?;
            Ok((p.clone(), Arc::new(Interruptible { inner, stop: stop.clone() }) as Arc<dyn Policy>))
        })
        .collect::<PyResult<_>>()?;
    let mut config = pikapro_rl::eval::EvalConfig::new(format, teams, episodes);
    config.seed = seed;
    if let Some(t) = threads {
        config.threads = t.max(1);
    }
    config.hp = parse_choice_of("hp", hp, &HP)?;
    config.batch = batch;
    let mut signal: Option<PyErr> = None;
    let result = py.detach(|| {
        std::thread::scope(|s| {
            let (done, finished) = std::sync::mpsc::channel();
            let (entrants, config) = (&entrants, &config);
            let run = s.spawn(move || {
                let table = pikapro_rl::eval::cross_evaluate(entrants, config);
                let _ = done.send(());
                table
            });
            // Python runs signal handlers on its main thread only: this
            // thread checks them while the evaluation runs on the other.
            while let Err(RecvTimeoutError::Timeout) = finished.recv_timeout(Duration::from_millis(50)) {
                if signal.is_none()
                    && let Err(e) = Python::attach(|py| py.check_signals())
                {
                    signal = Some(e);
                    stop.store(true, Ordering::Relaxed);
                }
            }
            run.join().unwrap_or_else(|payload| std::panic::resume_unwind(payload))
        })
    });
    if let Some(e) = signal {
        return Err(e);
    }
    let table = result.map_err(PyRuntimeError::new_err)?;
    Ok(table
        .records
        .iter()
        .map(|row| row.iter().map(|r| r.map(|r| (r.wins, r.losses, r.ties, r.turn_limit, r.turns))).collect())
        .collect())
}

/// A built-in policy that ends its game once `stop` is set (Ctrl-C during
/// `_cross_evaluate`). The pool contains the unwind (a shard that panics
/// fails with an error, not the call), so every shard stops at its next
/// choice and `cross_evaluate` returns that error; `resume_unwind` runs no
/// panic hook, so nothing is printed.
#[derive(Debug)]
struct Interruptible {
    inner: Arc<dyn Policy>,
    stop: Arc<AtomicBool>,
}

impl Policy for Interruptible {
    fn choose(&self, view: &PlayerView<'_>, legal: &LegalActions, rng: &mut BattleRng) -> Choice {
        if self.stop.load(Ordering::Relaxed) {
            std::panic::resume_unwind(Box::new("interrupted"));
        }
        self.inner.choose(view, legal, rng)
    }
}

/// Both players' teams of each of the first `episodes` episodes of a team
/// source with base seed `seed` (episode `e` has battle seed
/// `episode_seed(seed, e)`, as in `VecEnv` and `cross_evaluate`), packed:
/// what `pikapro.rl.eval` mirrors.
#[pyfunction]
fn _episode_teams(
    teams: &Bound<'_, PyAny>,
    format: &str,
    #[pyo3(from_py_with = arg::episodes)] episodes: u64,
    #[pyo3(from_py_with = arg::seed)] seed: u64,
) -> PyResult<Vec<[String; 2]>> {
    let format = format_arg(format)?;
    let source = teams_source(teams, &format)?;
    (0..episodes)
        .map(|e| {
            let pair = source.teams(e, pikapro_rl::env::episode_seed(seed, e)).map_err(err)?;
            Ok([pack_team(&pair[0], &format), pack_team(&pair[1], &format)])
        })
        .collect()
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyVecEnv>()?;
    m.add_class::<PyTeamPool>()?;
    m.add_function(wrap_pyfunction!(_action_table, m)?)?;
    m.add_function(wrap_pyfunction!(relative_target, m)?)?;
    m.add_function(wrap_pyfunction!(_cross_evaluate, m)?)?;
    m.add_function(wrap_pyfunction!(_episode_teams, m)?)?;
    Ok(())
}
