//! The `pikapro._pikapro` extension module (feature `python`).
//!
//! A thin layer over [`crate::game`], [`crate::pool`], [`crate::space`]
//! and [`crate::value`]: argument conversion, numpy arrays and errors. The
//! Python-facing documentation is in `python/pikapro/__init__.py` and
//! `python/pikapro/_pikapro.pyi`.

use std::sync::Arc;

use numpy::{PyArray1, PyArrayMethods, PyUntypedArrayMethods};
use pyo3::IntoPyObjectExt;
use pyo3::exceptions::{PyIndexError, PyOverflowError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyList, PyString, PyTuple};

use pikapro::{BattleRng, Choice, Format, Gimmick, MAX_TEAM, Seed, SideId, SlotAction, Target};
use pikapro_protocol::{Perspective, TeamSheet, export_sheet, pack_team};
use pikapro_rl::vec::Teams;

use crate::arrays;
use crate::game::{self, Game};
use crate::pool::{ActionMode, MaskKind, Pool, PoolConfig, Totals};
use crate::rl::array;
use crate::space::ActionSpace;
use crate::value::{Value, to_value};

pub(crate) fn err(msg: String) -> PyErr {
    PyValueError::new_err(msg)
}

/// The int arguments of the public classes and functions, for
/// `#[pyo3(from_py_with = arg::NAME)]`: one extractor per argument name.
///
/// An int the argument can't take raises `ValueError` (`IndexError` for an
/// index), and the message names the argument and, where it's narrower
/// than the Rust type's, its range. pyo3's own conversion raises
/// `OverflowError` instead, which `except ValueError` misses, and names
/// the argument only in a note, from Python 3.11 on. A value that isn't an
/// int raises pyo3's `TypeError`. Anything with `__index__` is an int, so
/// numpy ints are too, but a bool (Python's or numpy's) raises `TypeError`
/// naming the argument, as `Battle.submit` does, rather than run as 0 or 1.
pub(crate) mod arg {
    use super::*;

    /// Whether `obj` is a bool: Python's (an int subclass) or numpy's
    /// `bool_` (`numpy.bool` from numpy 2), which has `__index__`.
    pub(crate) fn is_bool(obj: &Bound<'_, PyAny>) -> bool {
        if obj.is_instance_of::<PyBool>() {
            return true;
        }
        let ty = obj.get_type();
        ty.name().is_ok_and(|n| n == "bool" || n == "bool_") && ty.module().is_ok_and(|m| m == "numpy")
    }

    /// The `TypeError` for a bool given as the int `what`.
    pub(crate) fn bool_error(what: &str) -> PyErr {
        PyTypeError::new_err(format!("{what} is an int, not a bool"))
    }

    /// `obj` as a `T`; a bool raises `bool_error(what)`, an int that
    /// doesn't fit `out_of_range(its text)`.
    pub(crate) fn int_or<'a, 'py, T: FromPyObject<'a, 'py>>(
        obj: &'a Bound<'py, PyAny>,
        what: &str,
        out_of_range: impl FnOnce(&str) -> PyErr,
    ) -> PyResult<T> {
        if is_bool(obj) {
            return Err(bool_error(what));
        }
        obj.extract::<T>().map_err(|e| {
            let e: PyErr = e.into();
            if e.is_instance_of::<PyOverflowError>(obj.py()) {
                out_of_range(&obj.str().map_or_else(|_| "the int".to_string(), |s| s.to_string()))
            } else {
                e
            }
        })
    }

    /// `None`, or `f(obj)`.
    fn opt<'a, 'py, T>(
        obj: &'a Bound<'py, PyAny>,
        f: impl FnOnce(&'a Bound<'py, PyAny>) -> PyResult<T>,
    ) -> PyResult<Option<T>> {
        if obj.is_none() { Ok(None) } else { f(obj).map(Some) }
    }

    /// An unsigned int argument `name` that takes `range` (the text).
    fn uint_in<'a, 'py, T: FromPyObject<'a, 'py>>(obj: &'a Bound<'py, PyAny>, name: &str, range: &str) -> PyResult<T> {
        int_or(obj, name, |v| PyValueError::new_err(format!("{name}={v} is out of range ({range})")))
    }

    /// An unsigned int argument `name` that takes any `T` (a seed, a
    /// count, a size: a 64-bit `T`).
    fn uint<'a, 'py, T: FromPyObject<'a, 'py>>(obj: &'a Bound<'py, PyAny>, name: &str) -> PyResult<T> {
        uint_in(obj, name, &format!("0 to 2**{} - 1", 8 * size_of::<T>()))
    }

    /// An index argument `name` (its upper bound is checked where it's used).
    fn index_of<'a, 'py, T: FromPyObject<'a, 'py>>(obj: &'a Bound<'py, PyAny>, name: &str) -> PyResult<T> {
        int_or(obj, name, |v| PyIndexError::new_err(format!("{name}={v} is out of range")))
    }

    /// A list of indices, argument `name`.
    fn index_list<T: for<'a, 'py> FromPyObject<'a, 'py>>(obj: &Bound<'_, PyAny>, name: &str) -> PyResult<Vec<T>> {
        let items: Vec<Bound<'_, PyAny>> = obj.extract()?;
        let what = format!("{name}: an item");
        items
            .iter()
            .map(|o| int_or(o, &what, |v| PyIndexError::new_err(format!("{name}: {v} is out of range"))))
            .collect()
    }

    pub(crate) fn seed(obj: &Bound<'_, PyAny>) -> PyResult<u64> {
        uint(obj, "seed")
    }

    pub(crate) fn seed_or_none(obj: &Bound<'_, PyAny>) -> PyResult<Option<u64>> {
        opt(obj, seed)
    }

    pub(crate) fn n(obj: &Bound<'_, PyAny>) -> PyResult<usize> {
        uint(obj, "n")
    }

    pub(crate) fn threads(obj: &Bound<'_, PyAny>) -> PyResult<Option<usize>> {
        opt(obj, |o| uint(o, "threads"))
    }

    pub(crate) fn shards_per_thread(obj: &Bound<'_, PyAny>) -> PyResult<usize> {
        uint(obj, "shards_per_thread")
    }

    pub(crate) fn battles(obj: &Bound<'_, PyAny>) -> PyResult<u64> {
        uint(obj, "battles")
    }

    pub(crate) fn episodes(obj: &Bound<'_, PyAny>) -> PyResult<u64> {
        uint(obj, "episodes")
    }

    pub(crate) fn batch(obj: &Bound<'_, PyAny>) -> PyResult<usize> {
        uint(obj, "batch")
    }

    pub(crate) fn wins(obj: &Bound<'_, PyAny>) -> PyResult<u64> {
        uint(obj, "wins")
    }

    pub(crate) fn losses(obj: &Bound<'_, PyAny>) -> PyResult<u64> {
        uint(obj, "losses")
    }

    pub(crate) fn ties(obj: &Bound<'_, PyAny>) -> PyResult<u64> {
        uint(obj, "ties")
    }

    /// The longest `spin_us` a pool takes: 1 s (its workers busy-wait that
    /// long for work before sleeping).
    const MAX_SPIN_US: u64 = 1_000_000;

    /// A pool's `spin_us`, as a duration.
    pub(crate) fn spin_us(obj: &Bound<'_, PyAny>) -> PyResult<std::time::Duration> {
        let refuse = |v: &str| err(format!("spin_us must be 0 to {MAX_SPIN_US} (1 s), got {v}"));
        let us: u64 = int_or(obj, "spin_us", refuse)?;
        if us > MAX_SPIN_US {
            return Err(refuse(&us.to_string()));
        }
        Ok(std::time::Duration::from_micros(us))
    }

    /// `VecEnv`'s `max_turns` (0 is refused with the reason by the config).
    pub(crate) fn max_turns(obj: &Bound<'_, PyAny>) -> PyResult<Option<u16>> {
        opt(obj, |o| uint_in(o, "max_turns", "1 to 65535"))
    }

    /// A series' number of games, argument `name`: an int that doesn't fit
    /// gets the best-of rule's message, as a value it can't take does from
    /// the config.
    fn games(obj: &Bound<'_, PyAny>, name: &str) -> PyResult<Option<u8>> {
        opt(obj, |o| {
            int_or(o, name, |v| err(format!("a series is best of 3, 5, 7 or 9 games (Showdown's rule), not {v}")))
        })
    }

    /// `VecEnv`'s `series`.
    pub(crate) fn series(obj: &Bound<'_, PyAny>) -> PyResult<Option<u8>> {
        games(obj, "series")
    }

    /// `_ChannelSeries`' `best_of`.
    pub(crate) fn best_of(obj: &Bound<'_, PyAny>) -> PyResult<Option<u8>> {
        games(obj, "best_of")
    }

    /// `RandomTeams`' `level` (the narrower bounds are checked where it's
    /// used).
    pub(crate) fn level(obj: &Bound<'_, PyAny>) -> PyResult<Option<u8>> {
        opt(obj, |o| int_or(o, "level", |v| err(format!("level must be 1 to 100, got {v}"))))
    }

    pub(crate) fn team_size(obj: &Bound<'_, PyAny>) -> PyResult<Option<usize>> {
        opt(obj, |o| uint_in(o, "team_size", "1 to 6"))
    }

    pub(crate) fn min_switches(obj: &Bound<'_, PyAny>) -> PyResult<u8> {
        uint_in(obj, "min_switches", "0 to 2")
    }

    /// A player index: 0 (the agent) or 1.
    pub(crate) fn player(obj: &Bound<'_, PyAny>) -> PyResult<usize> {
        let refuse = |v: &str| err(format!("player must be 0 or 1, got {v}"));
        match int_or::<i64>(obj, "player", refuse)? {
            p @ (0 | 1) => Ok(p as usize),
            p => Err(refuse(&p.to_string())),
        }
    }

    /// Action ints, as `VecEnv.step` takes them (checked where they're used).
    pub(crate) fn actions(obj: &Bound<'_, PyAny>) -> PyResult<Vec<i64>> {
        let items: Vec<Bound<'_, PyAny>> = obj.extract()?;
        items.iter().map(|o| int_or(o, "an action", |v| err(format!("action {v} out of range")))).collect()
    }

    /// `SlotAction`'s `index`: a move slot or a team position.
    pub(crate) fn slot_index(obj: &Bound<'_, PyAny>) -> PyResult<Option<u8>> {
        opt(obj, |o| index_of(o, "index"))
    }

    /// `SlotAction`'s `target`: Showdown's target location.
    pub(crate) fn target(obj: &Bound<'_, PyAny>) -> PyResult<Option<Target>> {
        let refuse = |v: &str| err(format!("target must be 1, 2, -1 or -2, got {v}"));
        opt(obj, |o| match int_or::<i64>(o, "target", refuse)? {
            loc @ (1 | 2) => Ok(Target::Foe(loc as u8 - 1)),
            loc @ (-2 | -1) => Ok(Target::Ally((-loc) as u8 - 1)),
            loc => Err(refuse(&loc.to_string())),
        })
    }

    /// `relative_target`'s `slot` (0 or 1; a code's slot is checked with
    /// the code).
    pub(crate) fn code_slot(obj: &Bound<'_, PyAny>) -> PyResult<usize> {
        uint_in(obj, "slot", "0 to 1")
    }

    /// `relative_target`'s `target`, a target code.
    pub(crate) fn code(obj: &Bound<'_, PyAny>) -> PyResult<usize> {
        uint_in(obj, "target", "0 to 4")
    }

    pub(crate) fn i(obj: &Bound<'_, PyAny>) -> PyResult<usize> {
        index_of(obj, "i")
    }

    pub(crate) fn index(obj: &Bound<'_, PyAny>) -> PyResult<usize> {
        index_of(obj, "index")
    }

    pub(crate) fn start(obj: &Bound<'_, PyAny>) -> PyResult<usize> {
        index_of(obj, "start")
    }

    pub(crate) fn k(obj: &Bound<'_, PyAny>) -> PyResult<usize> {
        index_of(obj, "k")
    }

    pub(crate) fn a0(obj: &Bound<'_, PyAny>) -> PyResult<usize> {
        index_of(obj, "a0")
    }

    pub(crate) fn a1(obj: &Bound<'_, PyAny>) -> PyResult<usize> {
        index_of(obj, "a1")
    }

    pub(crate) fn slots(obj: &Bound<'_, PyAny>) -> PyResult<Vec<usize>> {
        index_list(obj, "slots")
    }

    pub(crate) fn positions(obj: &Bound<'_, PyAny>) -> PyResult<Vec<usize>> {
        index_list(obj, "positions")
    }

    /// `Choice.team`'s `order`, team positions.
    pub(crate) fn order(obj: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
        index_list(obj, "order")
    }

    pub(crate) fn which(obj: &Bound<'_, PyAny>) -> PyResult<Option<Vec<usize>>> {
        opt(obj, |o| index_list(o, "which"))
    }

    pub(crate) fn indices(obj: &Bound<'_, PyAny>) -> PyResult<Option<Vec<usize>>> {
        opt(obj, |o| index_list(o, "indices"))
    }
}

// ---------------------------------------------------------------------------
// Arguments.

pub(crate) fn side_arg(side: &Bound<'_, PyAny>) -> PyResult<SideId> {
    let refuse = |v: &str| PyValueError::new_err(format!("side must be 0 or 1, got {v}"));
    match arg::int_or::<i64>(side, "side", refuse) {
        Ok(0) => return Ok(SideId::P1),
        Ok(1) => return Ok(SideId::P2),
        Ok(i) => return Err(refuse(&i.to_string())),
        // Out of range, or a bool.
        Err(e) if e.is_instance_of::<PyValueError>(side.py()) || arg::is_bool(side) => return Err(e),
        Err(_) => {}
    }
    match side.extract::<String>()?.to_ascii_lowercase().as_str() {
        "p1" => Ok(SideId::P1),
        "p2" => Ok(SideId::P2),
        s => Err(PyValueError::new_err(format!("side must be 0, 1, 'p1' or 'p2', got {s:?}"))),
    }
}

fn perspective_arg(p: &Bound<'_, PyAny>) -> PyResult<Perspective> {
    let Ok(name) = p.cast::<PyString>() else {
        return side_arg(p).map(Perspective::Player);
    };
    match name.to_str()?.to_ascii_lowercase().as_str() {
        "omniscient" | "omni" => Ok(Perspective::Omniscient),
        "spectator" => Ok(Perspective::Spectator),
        "p1" => Ok(Perspective::Player(SideId::P1)),
        "p2" => Ok(Perspective::Player(SideId::P2)),
        s => Err(PyValueError::new_err(format!(
            "perspective must be 'omniscient', 'p1', 'p2' or 'spectator', got {s:?}"
        ))),
    }
}

/// `Battle`'s and `Battle.fork`'s `seed`: an int (anything with
/// `__index__`, such as the numpy ints `BattlePool.seeds()` holds), four
/// 16-bit ints or 32 bytes.
fn seed_arg(seed: &Bound<'_, PyAny>) -> PyResult<Seed> {
    match arg::seed(seed) {
        Ok(s) => return Ok(Seed::fast(s)),
        // An int out of range, or a bool.
        Err(e) if e.is_instance_of::<PyValueError>(seed.py()) || arg::is_bool(seed) => return Err(e),
        Err(_) => {}
    }
    if let Ok(items) = seed.extract::<[Bound<'_, PyAny>; 4]>() {
        let mut words = [0u16; 4];
        let refuse = |v: &str| err(format!("seed words must be 0 to 65535, got {v}"));
        let mut ints = true;
        for (w, item) in words.iter_mut().zip(&items) {
            match arg::int_or::<u16>(item, "seed: a word", refuse) {
                Ok(v) => *w = v,
                Err(e) if e.is_instance_of::<PyValueError>(seed.py()) || arg::is_bool(item) => return Err(e),
                // Not an int: the message below says what a seed is.
                Err(_) => ints = false,
            }
        }
        if ints {
            return Ok(Seed::ShowdownGen5(words));
        }
    }
    if let Ok(bytes) = seed.cast::<pyo3::types::PyBytes>()
        && let Ok(key) = <[u8; 32]>::try_from(bytes.as_bytes())
    {
        return Ok(Seed::Sodium(key));
    }
    Err(PyTypeError::new_err(
        "seed must be an int (the fast backend), four 16-bit ints (Showdown's Gen 5 PRNG) \
         or 32 bytes (Showdown's default sodium PRNG)",
    ))
}

fn seed_obj(py: Python<'_>, seed: Seed) -> PyResult<Py<PyAny>> {
    match seed {
        Seed::Fast(s) => s.into_py_any(py),
        Seed::ShowdownGen5(w) => PyTuple::new(py, w)?.into_py_any(py),
        Seed::Sodium(key) => pyo3::types::PyBytes::new(py, &key).into_py_any(py),
    }
}

pub(crate) fn format_arg(format: &str) -> PyResult<Format> {
    game::format_by_id(format).map_err(err)
}

pub(crate) fn teams_arg(teams: &Bound<'_, PyAny>, format: &Format) -> PyResult<[TeamSheet; 2]> {
    let pair: Vec<String> = teams
        .extract()
        .map_err(|_| PyTypeError::new_err("teams must be two strings (Showdown export or packed format)"))?;
    let [a, b]: [String; 2] = pair.try_into().map_err(|_| PyValueError::new_err("teams must be exactly two teams"))?;
    Ok([
        game::parse_team(&a, format).map_err(|e| err(format!("p1 {e}")))?,
        game::parse_team(&b, format).map_err(|e| err(format!("p2 {e}")))?,
    ])
}

// ---------------------------------------------------------------------------
// Typed values.

/// A typed engine value: a struct, or an enum variant with fields.
///
/// Fields are attributes (`event.actor.species`) and items
/// (`event["hp"]`), and always win: the record's own members start with an
/// underscore (`_variant`, `_keys()`, `_to_dict()`), which no field does.
/// `_variant` is the Rust type or variant name (`"Switch"`, `"Actor"`,
/// `"Sleep"`). IDs are names, `None`/`Some` are `None`/the value, unit
/// variants are strings (`"P1"`, `"Burn"`, and `"None"` for an enum's own
/// `None` variant, such as `Status::None`), and a variant with unnamed
/// fields has them as `value` (one) or `values` (several). A record without
/// fields compares equal to its variant name as a string.
#[pyclass(module = "pikapro._pikapro", frozen, skip_from_py_object)]
#[derive(Debug)]
pub struct Record {
    kind: String,
    fields: Py<PyDict>,
}

#[pymethods]
impl Record {
    /// The Rust type or variant name.
    #[getter]
    fn _variant(&self) -> &str {
        &self.kind
    }

    fn __getattr__(&self, py: Python<'_>, name: &str) -> PyResult<Py<PyAny>> {
        match self.fields.bind(py).get_item(name)? {
            Some(v) => Ok(v.unbind()),
            None => Err(pyo3::exceptions::PyAttributeError::new_err(format!("{} has no field {name:?}", self.kind))),
        }
    }

    fn __getitem__(&self, py: Python<'_>, name: &str) -> PyResult<Py<PyAny>> {
        match self.fields.bind(py).get_item(name)? {
            Some(v) => Ok(v.unbind()),
            None => Err(pyo3::exceptions::PyKeyError::new_err(name.to_string())),
        }
    }

    fn __contains__(&self, py: Python<'_>, name: &str) -> PyResult<bool> {
        self.fields.bind(py).contains(name)
    }

    fn __dir__(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let mut names: Vec<String> = self.fields.bind(py).keys().extract()?;
        names.extend(["_variant".into(), "_keys".into(), "_to_dict".into()]);
        Ok(names)
    }

    /// The field names, in declaration order.
    fn _keys(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        self.fields.bind(py).keys().extract()
    }

    /// The record as plain dicts and lists, recursively, with its variant
    /// name under `"_variant"`.
    fn _to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let out = PyDict::new(py);
        out.set_item("_variant", &self.kind)?;
        for (k, v) in self.fields.bind(py).iter() {
            out.set_item(k, plain(&v)?)?;
        }
        out.into_py_any(py)
    }

    fn __eq__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<bool> {
        if let Ok(s) = other.cast::<PyString>() {
            return Ok(self.fields.bind(py).is_empty() && s.to_str()? == self.kind);
        }
        match other.cast::<Record>() {
            Ok(r) => {
                let r = r.get();
                Ok(r.kind == self.kind && self.fields.bind(py).eq(r.fields.bind(py))?)
            }
            Err(_) => Ok(false),
        }
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let mut out = format!("{}(", self.kind);
        for (i, (k, v)) in self.fields.bind(py).iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{}={}", k, v.repr()?));
        }
        out.push(')');
        Ok(out)
    }
}

/// Records as dicts, lists and tuples as lists, recursively.
fn plain(v: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let py = v.py();
    if let Ok(r) = v.cast::<Record>() {
        return r.get()._to_dict(py);
    }
    if let Ok(l) = v.cast::<PyList>() {
        let items = l.iter().map(|x| plain(&x)).collect::<PyResult<Vec<_>>>()?;
        return items.into_py_any(py);
    }
    if let Ok(t) = v.cast::<PyTuple>() {
        let items = t.iter().map(|x| plain(&x)).collect::<PyResult<Vec<_>>>()?;
        return items.into_py_any(py);
    }
    Ok(v.clone().unbind())
}

fn value_obj(py: Python<'_>, v: Value) -> PyResult<Py<PyAny>> {
    Ok(match v {
        Value::None => py.None(),
        Value::Bool(b) => b.into_py_any(py)?,
        Value::Int(i) => i.into_py_any(py)?,
        Value::Str(s) | Value::Unit(s) => s.into_py_any(py)?,
        Value::List(items) => {
            let items = items.into_iter().map(|x| value_obj(py, x)).collect::<PyResult<Vec<_>>>()?;
            PyList::new(py, items)?.into_py_any(py)?
        }
        Value::Tuple(items) => {
            let items = items.into_iter().map(|x| value_obj(py, x)).collect::<PyResult<Vec<_>>>()?;
            PyTuple::new(py, items)?.into_py_any(py)?
        }
        Value::Record { kind, fields } => {
            let dict = PyDict::new(py);
            for (k, v) in fields {
                dict.set_item(k, value_obj(py, v)?)?;
            }
            Record { kind, fields: dict.unbind() }.into_py_any(py)?
        }
        Value::Variant { kind, mut values } => {
            let dict = PyDict::new(py);
            if values.len() == 1 {
                dict.set_item("value", value_obj(py, values.pop().unwrap_or(Value::None))?)?;
            } else {
                let items = values.into_iter().map(|x| value_obj(py, x)).collect::<PyResult<Vec<_>>>()?;
                dict.set_item("values", items)?;
            }
            Record { kind, fields: dict.unbind() }.into_py_any(py)?
        }
    })
}

pub(crate) fn typed<T: core::fmt::Debug + ?Sized>(
    py: Python<'_>,
    value: &T,
    dex: &'static pikapro_data::Dex,
) -> PyResult<Py<PyAny>> {
    value_obj(py, to_value(value, dex))
}

// ---------------------------------------------------------------------------
// Actions.

pub(crate) fn gimmick_name(g: Gimmick) -> &'static str {
    match g {
        Gimmick::Terastallize => "terastallize",
        Gimmick::MegaEvolve => "mega",
    }
}

fn gimmick_arg(name: &str) -> PyResult<Gimmick> {
    match name.to_ascii_lowercase().as_str() {
        "terastallize" | "tera" | "terastal" => Ok(Gimmick::Terastallize),
        "mega" | "megaevolve" => Ok(Gimmick::MegaEvolve),
        _ => Err(PyValueError::new_err(format!("unknown gimmick {name:?} (terastallize, mega)"))),
    }
}

/// Showdown's signed target location: `1`, `2` a foe, `-1`, `-2` an ally
/// (the user's own slot included).
fn target_loc(t: Target) -> i8 {
    match t {
        Target::Foe(n) => n as i8 + 1,
        Target::Ally(n) => -(n as i8) - 1,
    }
}

/// One active slot's action.
///
/// `SlotAction("move", 0, target=1, gimmick="terastallize")`,
/// `SlotAction("switch", 2)` (team position 2, 0-based), `SlotAction("pass")`.
/// `target` is Showdown's target location (`1`, `2`: foes; `-1`, `-2`: the
/// user's side); `gimmick` is `"terastallize"` or `"mega"`.
#[pyclass(module = "pikapro._pikapro", name = "SlotAction", frozen, eq, hash, skip_from_py_object)]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PySlotAction {
    pub(crate) inner: SlotAction,
}

#[pymethods]
impl PySlotAction {
    #[new]
    #[pyo3(signature = (kind, index=None, target=None, gimmick=None))]
    fn new(
        kind: &str,
        #[pyo3(from_py_with = arg::slot_index)] index: Option<u8>,
        #[pyo3(from_py_with = arg::target)] target: Option<Target>,
        gimmick: Option<&str>,
    ) -> PyResult<Self> {
        let inner = match kind {
            "move" => SlotAction::Move {
                index: index.ok_or_else(|| PyValueError::new_err("a move needs its index"))?,
                target,
                gimmick: gimmick.map(gimmick_arg).transpose()?,
            },
            "switch" => {
                SlotAction::Switch(index.ok_or_else(|| PyValueError::new_err("a switch needs a team position"))?)
            }
            "pass" => SlotAction::Pass,
            _ => return Err(PyValueError::new_err(format!("kind must be 'move', 'switch' or 'pass', got {kind:?}"))),
        };
        Ok(PySlotAction { inner })
    }

    /// `"move"`, `"switch"` or `"pass"`.
    #[getter]
    fn kind(&self) -> &'static str {
        match self.inner {
            SlotAction::Move { .. } => "move",
            SlotAction::Switch(_) => "switch",
            SlotAction::Pass => "pass",
        }
    }

    /// The move slot (0-based) or the team position switched to.
    #[getter]
    fn index(&self) -> Option<u8> {
        match self.inner {
            SlotAction::Move { index, .. } => Some(index),
            SlotAction::Switch(i) => Some(i),
            SlotAction::Pass => None,
        }
    }

    /// Showdown's target location of a move: `1`, `2` a foe, `-1`, `-2` the
    /// user's side; `None` for no target (or not a move).
    #[getter]
    fn target(&self) -> Option<i8> {
        match self.inner {
            SlotAction::Move { target, .. } => target.map(target_loc),
            _ => None,
        }
    }

    /// The move's gimmick, `"terastallize"` or `"mega"`; `None` for none (or
    /// not a move).
    #[getter]
    fn gimmick(&self) -> Option<&'static str> {
        match self.inner {
            SlotAction::Move { gimmick, .. } => gimmick.map(gimmick_name),
            _ => None,
        }
    }

    fn __repr__(&self) -> String {
        match self.inner {
            SlotAction::Move { index, target, gimmick } => {
                let mut s = format!("SlotAction('move', {index}");
                if let Some(t) = target {
                    s.push_str(&format!(", target={}", target_loc(t)));
                }
                if let Some(g) = gimmick {
                    s.push_str(&format!(", gimmick='{}'", gimmick_name(g)));
                }
                s + ")"
            }
            SlotAction::Switch(i) => format!("SlotAction('switch', {i})"),
            SlotAction::Pass => "SlotAction('pass')".into(),
        }
    }
}

/// A side's whole choice: one action per active slot, or a team order.
///
/// `str(choice)` is the Showdown choice string (`move 1 2, switch 3`,
/// `team 1234`).
#[pyclass(module = "pikapro._pikapro", name = "Choice", frozen, eq, hash, skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct PyChoice {
    pub(crate) inner: Choice,
    pub(crate) active: usize,
    /// The choice string where `format_choice` can't write one that
    /// parses back (a choice `default` made in doubles:
    /// `pikapro_protocol::format_choice_for`), set where the battle is at
    /// hand. Not part of the value: equality and hashing ignore it.
    pub(crate) text: Option<String>,
}

impl PartialEq for PyChoice {
    fn eq(&self, other: &Self) -> bool {
        (self.inner, self.active) == (other.inner, other.active)
    }
}

impl Eq for PyChoice {}

impl std::hash::Hash for PyChoice {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.inner, self.active).hash(state);
    }
}

#[pymethods]
impl PyChoice {
    /// A team-preview answer: team positions (0-based), the first `bring`
    /// of which are brought, in that order.
    #[staticmethod]
    fn team(#[pyo3(from_py_with = arg::order)] order: Vec<u8>) -> PyResult<Self> {
        if order.len() > MAX_TEAM {
            return Err(PyValueError::new_err("a team order has at most six positions"));
        }
        Ok(py_choice(Choice::team(&order), 2))
    }

    /// One action per active slot (one in singles, two in doubles).
    #[staticmethod]
    fn from_actions(actions: Vec<PyRef<'_, PySlotAction>>) -> PyResult<Self> {
        if actions.is_empty() || actions.len() > 2 {
            return Err(PyValueError::new_err("a choice has one or two slot actions"));
        }
        let mut a = [SlotAction::Pass; 2];
        for (slot, action) in actions.iter().enumerate() {
            a[slot] = action.inner;
        }
        Ok(PyChoice { inner: Choice::Actions(a), active: actions.len(), text: None })
    }

    /// `"actions"` or `"team"`.
    #[getter]
    fn kind(&self) -> &'static str {
        match self.inner {
            Choice::Actions(_) => "actions",
            Choice::Team(_) => "team",
        }
    }

    /// The slot actions (empty for a team order).
    #[getter(actions)]
    fn get_actions(&self) -> Vec<PySlotAction> {
        match self.inner {
            Choice::Actions(a) => a[..self.active].iter().map(|&inner| PySlotAction { inner }).collect(),
            Choice::Team(_) => Vec::new(),
        }
    }

    /// The team order (empty for actions).
    #[getter]
    fn order(&self) -> Vec<usize> {
        match self.inner {
            // Ints (a `Vec<u8>` would reach Python as `bytes`).
            Choice::Team(order) => order.iter().map(|&p| usize::from(p)).collect(),
            Choice::Actions(_) => Vec::new(),
        }
    }

    fn __str__(&self) -> String {
        self.text.clone().unwrap_or_else(|| pikapro_protocol::format_choice(&self.inner, self.active))
    }

    fn __repr__(&self) -> String {
        format!("Choice({:?})", self.__str__())
    }
}

pub(crate) fn py_choice(choice: Choice, active: usize) -> PyChoice {
    // A team order doesn't depend on the slot count: keep one value so
    // equal orders compare equal.
    let active = if matches!(choice, Choice::Team(_)) { 2 } else { active };
    PyChoice { inner: choice, active, text: None }
}

/// A format's action space: stable integers for slot actions and choices.
///
/// Slot action `i` of `slot_size`: moves first (`move * G * T + gimmick *
/// T + target`, with `T = targets` and `G = 1 + len(gimmicks)`), then
/// `switch_base + position` for the six team positions, then `pass_index`.
/// Choice `i` of `size`: `0..joint_size` are per-slot actions combined
/// (`a0 * slot_size + a1` in doubles, `a0` in singles), `joint_size..size`
/// team-preview orders in lexicographic order. See `docs/python/simulator.md`.
#[pyclass(module = "pikapro._pikapro", name = "ActionSpace", frozen, skip_from_py_object)]
#[derive(Debug)]
pub struct PyActionSpace {
    pub(crate) space: Arc<ActionSpace>,
}

#[pymethods]
impl PyActionSpace {
    #[new]
    fn new(format: &str) -> PyResult<Self> {
        Ok(PyActionSpace { space: Arc::new(ActionSpace::new(&format_arg(format)?)) })
    }

    /// Active slots per side: 1 in singles, 2 in doubles.
    #[getter]
    fn active(&self) -> usize {
        self.space.active
    }
    /// Target choices per move (`T`): 1 in singles; 5 in doubles (none, the
    /// foes in slots 1 and 2, the user's side's slots).
    #[getter]
    fn targets(&self) -> usize {
        self.space.targets
    }
    /// The format's gimmicks after gimmick `0` (none): `["terastallize"]` in
    /// Gen 9, `["mega"]` in Champions.
    #[getter]
    fn gimmicks(&self) -> Vec<&'static str> {
        self.space.gimmicks.iter().map(|&g| gimmick_name(g)).collect()
    }
    /// Slot actions per active slot: moves with their targets and gimmicks, six
    /// switches and pass.
    #[getter]
    fn slot_size(&self) -> usize {
        self.space.slot_size
    }
    /// Choices that answer a move or forced-switch request: `slot_size **
    /// active`.
    #[getter]
    fn joint_size(&self) -> usize {
        self.space.joint_size
    }
    /// Team positions a team-preview choice names: the Pokémon brought to
    /// battle (0 without team preview), or 1 when `lead_only`.
    #[getter]
    fn bring(&self) -> usize {
        self.space.bring
    }
    /// Whether team preview brings the whole team (gen9ou): a preview
    /// choice names the lead, and the others follow in team order.
    #[getter]
    fn lead_only(&self) -> bool {
        self.space.lead_only
    }
    /// Team-preview choices: the ordered selections of `bring` of the six team
    /// positions (0 without team preview).
    #[getter]
    fn preview_size(&self) -> usize {
        self.space.preview_size
    }
    /// Every choice: `joint_size + preview_size`.
    #[getter]
    fn size(&self) -> usize {
        self.space.size
    }
    /// The slot-action index of `switch 1` (switches are `switch_base + i` for
    /// team position `i`).
    #[getter]
    fn switch_base(&self) -> usize {
        self.space.switch_base()
    }
    /// The slot-action index of `pass` (the last).
    #[getter]
    fn pass_index(&self) -> usize {
        self.space.pass_index()
    }

    /// The `Choice` of choice index `index` (`IndexError` out of range).
    fn decode(&self, #[pyo3(from_py_with = arg::index)] index: usize) -> PyResult<PyChoice> {
        let c =
            self.space.decode(index).ok_or_else(|| PyIndexError::new_err(format!("choice {index} out of range")))?;
        Ok(py_choice(c, self.space.active))
    }

    /// The choice index of `choice` (`ValueError` if the space has no such
    /// choice).
    fn encode(&self, choice: PyRef<'_, PyChoice>) -> PyResult<usize> {
        self.space.encode(&choice.inner).ok_or_else(|| err(format!("{} is not in this action space", choice.__str__())))
    }

    /// The `SlotAction` of slot-action index `index` (`IndexError` out of
    /// range).
    fn decode_slot(&self, #[pyo3(from_py_with = arg::index)] index: usize) -> PyResult<PySlotAction> {
        let a = self
            .space
            .decode_slot(index)
            .ok_or_else(|| PyIndexError::new_err(format!("slot action {index} out of range")))?;
        Ok(PySlotAction { inner: a })
    }

    /// The slot-action index of `action` (`ValueError` if the space has no such
    /// action).
    fn encode_slot(&self, action: PyRef<'_, PySlotAction>) -> PyResult<usize> {
        self.space
            .encode_slot(&action.inner)
            .ok_or_else(|| err(format!("{} is not in this action space", action.__repr__())))
    }

    /// The choice index of per-slot action indices.
    fn joint_index(&self, #[pyo3(from_py_with = arg::slots)] slots: Vec<usize>) -> PyResult<usize> {
        self.space.joint_index(&slots).ok_or_else(|| err(format!("bad slot actions {slots:?}")))
    }

    /// The per-slot action indices of a choice index below `joint_size`.
    fn split(&self, #[pyo3(from_py_with = arg::index)] index: usize) -> PyResult<Vec<usize>> {
        if index >= self.space.joint_size {
            return Err(PyIndexError::new_err(format!(
                "{index} is not a move choice (joint_size {})",
                self.space.joint_size
            )));
        }
        Ok(self.space.split_joint(index)[..self.space.active].to_vec())
    }

    /// Team-preview order `k` (`0 <= k < preview_size`).
    fn preview_order(&self, #[pyo3(from_py_with = arg::k)] k: usize) -> PyResult<Vec<usize>> {
        let order = self.space.preview_order(k).ok_or_else(|| PyIndexError::new_err(format!("{k}")))?;
        Ok(order.iter().map(|&p| usize::from(p)).collect())
    }

    /// The Showdown choice string of choice `index`.
    fn describe(&self, #[pyo3(from_py_with = arg::index)] index: usize) -> PyResult<String> {
        Ok(self.decode(index)?.__str__())
    }

    /// Whether slot actions `a0` and `a1` make a legal doubles choice given
    /// a side's factored slot masks (`active * slot_size` bools, or a whole
    /// factored mask row) and its `min_switches`: both in their slot's mask,
    /// not two switches to one Pokémon, at most one gimmick, and at least
    /// `min_switches` switches. In singles `a1` is ignored.
    fn pair_legal(
        &self,
        slot_masks: Vec<bool>,
        #[pyo3(from_py_with = arg::min_switches)] min_switches: u8,
        #[pyo3(from_py_with = arg::a0)] a0: usize,
        #[pyo3(from_py_with = arg::a1)] a1: usize,
    ) -> PyResult<bool> {
        let need = self.space.active * self.space.slot_size;
        if slot_masks.len() < need {
            return Err(err(format!("slot masks need {need} values, got {}", slot_masks.len())));
        }
        Ok(self.space.pair_legal(&slot_masks, min_switches, a0, a1))
    }

    /// Batched `pair_legal` for autoregressive policies: for factored masks
    /// `(..., W)` (W at least `active * slot_size`), `min_switches` `(...)`
    /// and first-slot actions `a0` `(...)`, the second slot's actions that
    /// complete a legal choice, bool `(..., slot_size)` (all false where
    /// `a0` itself isn't allowed).
    fn pair_mask<'py>(
        &self,
        py: Python<'py>,
        masks: &Bound<'py, PyAny>,
        min_switches: &Bound<'py, PyAny>,
        a0: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let np = py.import("numpy")?;
        let masks = np.call_method1("ascontiguousarray", (masks, "bool"))?;
        let masks = masks.cast::<numpy::PyArrayDyn<bool>>()?;
        let shape = masks.shape().to_vec();
        let (&width, lead) = shape.split_last().ok_or_else(|| err("masks need at least one axis".into()))?;
        let space = &self.space;
        let need = space.active * space.slot_size;
        if width < need {
            return Err(err(format!("masks need at least {need} values per row, got {width}")));
        }
        let rows: usize = lead.iter().product();
        let ints = |v: &Bound<'py, PyAny>, what: &str| -> PyResult<Vec<i64>> {
            let a = np.call_method1("broadcast_to", (np.call_method1("asarray", (v, "int64"))?, lead.to_vec()))?;
            let a = np.call_method1("ascontiguousarray", (a,))?;
            let a = a.cast::<numpy::PyArrayDyn<i64>>().map_err(|_| err(format!("{what} must be ints")))?;
            Ok(a.readonly().as_slice()?.to_vec())
        };
        let mins = ints(min_switches, "min_switches")?;
        let firsts = ints(a0, "a0")?;
        let masks = masks.readonly();
        let masks = masks.as_slice()?;
        let mut out = vec![false; rows * space.slot_size];
        for r in 0..rows {
            let Ok(a) = usize::try_from(firsts[r]) else { continue };
            let min = u8::try_from(mins[r].clamp(0, 2)).unwrap_or(0);
            space.pair_mask(&masks[r * width..][..need], min, a, &mut out[r * space.slot_size..][..space.slot_size]);
        }
        let mut out_shape = lead.to_vec();
        out_shape.push(space.slot_size);
        array(py, out, out_shape)
    }

    fn __repr__(&self) -> String {
        let s = &self.space;
        format!(
            "ActionSpace(active={}, slot_size={}, joint_size={}, preview_size={}, size={})",
            s.active, s.slot_size, s.joint_size, s.preview_size, s.size
        )
    }
}

// ---------------------------------------------------------------------------
// Battle.

/// One battle of `format` between two teams (Showdown's export or packed
/// format).
///
/// `seed`: an int selects the fast RNG; four 16-bit ints Showdown's Gen 5
/// PRNG; 32 bytes its default sodium PRNG (ChaCha20). The same format,
/// teams, seed and choices give the same battle, bit for bit. `log=False`
/// records no events (faster, for search). `players` names the players in
/// the log (`p1` and `p2` by default).
///
/// Each side answers its request (`request`, `request_kind`,
/// `legal_actions`, `action_mask`) with `submit`: a choice index, per-slot
/// indices, a Showdown choice string or a `Choice`. The turn resolves
/// once both sides have chosen. `fork` copies the battle, `log` renders
/// Showdown's protocol for any perspective, `events` returns typed
/// events, and `view` the player's view.
#[pyclass(module = "pikapro._pikapro", name = "Battle", skip_from_py_object)]
#[derive(Debug)]
pub struct PyBattle {
    pub(crate) game: Game,
}

impl PyBattle {
    fn dex(&self) -> &'static pikapro_data::Dex {
        self.game.battle.format().dex
    }

    fn choice_arg(&self, side: SideId, choice: &Bound<'_, PyAny>) -> PyResult<Choice> {
        let space = &self.game.space;
        if let Ok(s) = choice.cast::<PyString>() {
            return self.game.parse_choice(side, s.to_str()?).map_err(err);
        }
        if let Ok(c) = choice.cast::<PyChoice>() {
            return Ok(c.get().inner);
        }
        if let Some(i) = int_arg(choice, "a choice index")? {
            // (A team-preview choice depends on the team's size.)
            let team_len = self.game.battle.state().side(side).team.len();
            return usize::try_from(i)
                .ok()
                .and_then(|i| space.decode_for(i, team_len))
                .ok_or_else(|| PyIndexError::new_err(format!("choice {i} out of range (0 to {})", space.size - 1)));
        }
        if let Ok(items) = choice.extract::<Vec<Bound<'_, PyAny>>>() {
            let mut slots = Vec::with_capacity(items.len());
            for item in &items {
                let Some(a) = int_arg(item, "a slot action")? else {
                    return Err(PyTypeError::new_err("slot actions are ints"));
                };
                let a = usize::try_from(a).ok().filter(|&a| a < space.slot_size).ok_or_else(|| {
                    PyIndexError::new_err(format!("slot action {a} out of range (0 to {})", space.slot_size - 1))
                })?;
                slots.push(a);
            }
            let j = space
                .joint_index(&slots)
                .ok_or_else(|| err(format!("a choice has {} slot actions, got {}", space.active, slots.len())))?;
            return space.decode(j).ok_or_else(|| err(format!("bad slot actions {slots:?}")));
        }
        Err(PyTypeError::new_err("a choice is an int (choice index), a list of slot-action ints, a str or a Choice"))
    }
}

/// An int argument: `None` if `v` is no int (a str, a list, ...); an error
/// for a bool, which Python would otherwise take as 0 or 1, and
/// `IndexError` for an int (a numpy one too) past `i64`.
pub(crate) fn int_arg(v: &Bound<'_, PyAny>, what: &str) -> PyResult<Option<i64>> {
    if arg::is_bool(v) {
        return Err(arg::bool_error(what));
    }
    match v.extract::<i64>() {
        Ok(i) => Ok(Some(i)),
        Err(e) if e.is_instance_of::<PyOverflowError>(v.py()) => {
            Err(PyIndexError::new_err(format!("{what} out of range")))
        }
        Err(_) => Ok(None),
    }
}

#[pymethods]
impl PyBattle {
    #[new]
    #[pyo3(signature = (format, teams, seed=None, *, log=true, players=None))]
    fn new(
        format: &str,
        teams: &Bound<'_, PyAny>,
        seed: Option<&Bound<'_, PyAny>>,
        log: bool,
        players: Option<[String; 2]>,
    ) -> PyResult<Self> {
        let format = format_arg(format)?;
        let sheets = teams_arg(teams, &format)?;
        let seed = seed.map(seed_arg).transpose()?.unwrap_or(Seed::fast(0));
        let mut game = Game::new(format, sheets, seed, log).map_err(err)?;
        if let Some(players) = players {
            game.players = players;
        }
        Ok(PyBattle { game })
    }

    /// The players' names, as the log shows them (`|player|`, `|win|`):
    /// `["p1", "p2"]` unless given.
    #[getter]
    fn players(&self) -> Vec<String> {
        self.game.players.to_vec()
    }

    #[setter]
    fn set_players(&mut self, players: [String; 2]) {
        self.game.players = players;
    }

    /// The format id.
    #[getter]
    fn format(&self) -> &'static str {
        self.game.battle.format().id
    }

    /// The format's `ActionSpace`.
    #[getter]
    fn space(&self) -> PyActionSpace {
        PyActionSpace { space: self.game.space.clone() }
    }

    /// The battle's seed, as given: an int, four 16-bit ints or 32 bytes.
    #[getter]
    fn seed(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        seed_obj(py, self.game.battle.seed())
    }

    /// The current turn (0 before the first).
    #[getter]
    fn turn(&self) -> u16 {
        self.game.battle.turn()
    }

    /// Whether the battle has ended (`result` says how).
    #[getter]
    fn over(&self) -> bool {
        self.game.battle.is_over()
    }

    /// `0` or `1` once a side has won, else `None` (also for a tie).
    #[getter]
    fn winner(&self) -> Option<usize> {
        self.game.battle.result().and_then(|r| r.winner).map(SideId::index)
    }

    /// `BattleResult(winner=..., reason=..., turns=...)`, or `None`.
    #[getter]
    fn result(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match self.game.battle.result() {
            None => Ok(py.None()),
            Some(r) => typed(py, &r, self.dex()),
        }
    }

    /// `"wait"`, `"team_preview"`, `"move"` or `"force_switch"`.
    fn request_kind(&self, side: &Bound<'_, PyAny>) -> PyResult<&'static str> {
        let code = arrays::request_code(self.game.battle.request(side_arg(side)?));
        Ok(arrays::REQUEST_CODES[code as usize])
    }

    /// Everything Showdown's `|request|` says, as a typed record
    /// (`RequestData`). After `side` has submitted, it still describes the
    /// request it answered, with `answered` true, while `request_kind` says
    /// `"wait"`. After an `[Unavailable choice]`, it is the updated request
    /// Showdown sends (`trapped`, the move `disabled`).
    fn request(&self, py: Python<'_>, side: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        typed(py, &self.game.request_data(side_arg(side)?), self.dex())
    }

    /// The request as Showdown's `|request|` JSON (after an `[Unavailable
    /// choice]`, the updated request it sends again, with `update: true`).
    fn request_json(&self, side: &Bound<'_, PyAny>) -> PyResult<String> {
        Ok(self.game.request_json(side_arg(side)?))
    }

    /// Indices of every jointly legal choice, ascending.
    fn legal_actions(&self, side: &Bound<'_, PyAny>) -> PyResult<Vec<usize>> {
        let side = side_arg(side)?;
        let team_len = self.game.battle.state().side(side).team.len();
        Ok(self.game.space.legal_indices(self.game.battle.legal_actions(side), team_len))
    }

    /// Every jointly legal choice, typed, in index order.
    fn legal_choices(&self, side: &Bound<'_, PyAny>) -> PyResult<Vec<PyChoice>> {
        let space = &self.game.space;
        let team_len = self.game.battle.state().side(side_arg(side)?).team.len();
        Ok(self
            .legal_actions(side)?
            .into_iter()
            .filter_map(|i| space.decode_for(i, team_len))
            .map(|c| py_choice(c, space.active))
            .collect())
    }

    /// Per active slot, the slot-action indices legal on their own.
    fn slot_actions(&self, side: &Bound<'_, PyAny>) -> PyResult<Vec<Vec<usize>>> {
        let legal = self.game.battle.legal_actions(side_arg(side)?);
        let space = &self.game.space;
        Ok((0..space.active)
            .map(|slot| legal.slot(slot).iter().filter_map(|a| space.encode_slot(a)).collect())
            .collect())
    }

    /// Jointly legal choices as a bool array of `space.size`.
    fn action_mask<'py>(&self, py: Python<'py>, side: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let side = side_arg(side)?;
        let space = &self.game.space;
        let mut mask = vec![false; space.size];
        let team_len = self.game.battle.state().side(side).team.len();
        space.fill_mask(self.game.battle.legal_actions(side), team_len, &mut mask);
        array(py, mask, vec![space.size])
    }

    /// Each slot's own legal actions, a bool array `(active, slot_size)`.
    fn slot_mask<'py>(&self, py: Python<'py>, side: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let space = &self.game.space;
        let mut mask = vec![false; space.active * space.slot_size];
        space.fill_slot_mask(self.game.battle.legal_actions(side_arg(side)?), &mut mask);
        array(py, mask, vec![space.active, space.slot_size])
    }

    /// Submits a choice: a choice index, a list of per-slot action indices,
    /// a Showdown choice string (parsed as Showdown's `side.choose` does)
    /// or a `Choice`. Returns whether the battle resolved (both sides had
    /// chosen). A replacement clears the previous answer before validation,
    /// including when it raises `ValueError` for an illegal choice. The
    /// exception is a choice Showdown locks to avoid revealing hidden trapping
    /// or disabling information: it cannot be changed. The simulated battle
    /// state is unchanged by a rejection.
    fn submit(&mut self, side: &Bound<'_, PyAny>, choice: &Bound<'_, PyAny>) -> PyResult<bool> {
        let side = side_arg(side)?;
        let progress = if let Ok(s) = choice.cast::<PyString>() {
            self.game.submit_text(side, s.to_str()?)
        } else {
            let choice = self.choice_arg(side, choice)?;
            self.game.submit(side, choice)
        };
        Ok(progress.map_err(err)? == pikapro::Progress::Resolved)
    }

    /// `battle.showOpenTeamSheets()` at team preview: the `|showteam|p1|...`
    /// and `|showteam|p2|...` lines (each side's species, items, abilities,
    /// moves, levels, genders and Tera types, and natures in Champions, in
    /// Showdown's packed format), as Showdown's room sends them when both
    /// players accept open team sheets. Raises `ValueError` without team
    /// preview.
    fn open_team_sheets(&self) -> PyResult<Vec<String>> {
        self.game.open_team_sheets().map_err(err)
    }

    /// `side`'s player view as a `pikapro.rl.BattleView`
    /// (`pikapro.rl.BattleView.from_battle(battle, side)`): what the player has seen. Built from the events so far
    /// (the battle must record them) and the side's request, with the side's own team as every request so far
    /// showed it (a benched Pokémon's PP is the last its requests showed), as `pikapro.rl.VecEnv`'s view keeps it.
    /// A `Battle` doesn't know open team sheets or a series.
    fn view<'py>(slf: &Bound<'py, Self>, side: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let view = slf.py().import("pikapro.rl.view")?.getattr("BattleView")?;
        view.call_method1("from_battle", (slf, side))
    }

    /// `side`'s player view as plain values, for `pikapro.rl.BattleView`.
    fn _view<'py>(&self, py: Python<'py>, side: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
        let side = side_arg(side)?;
        let format = self.game.battle.format();
        self.game.with_view(side, |view| crate::view::view_dict(py, view, format)).map_err(err)?
    }

    /// Parses a Showdown choice string against the current request.
    fn parse_choice(&self, side: &Bound<'_, PyAny>, choice: &str) -> PyResult<PyChoice> {
        let side = side_arg(side)?;
        let c = self.game.parse_choice(side, choice).map_err(err)?;
        let mut out = py_choice(c, self.game.space.active);
        // `str()` parses back: `default` where only it writes the choice.
        let text = self.game.format_choice(side, &c);
        if text != pikapro_protocol::format_choice(&c, out.active) {
            out.text = Some(text);
        }
        Ok(out)
    }

    /// The index of a choice (any form `submit` takes).
    fn choice_index(&self, side: &Bound<'_, PyAny>, choice: &Bound<'_, PyAny>) -> PyResult<usize> {
        let c = self.choice_arg(side_arg(side)?, choice)?;
        self.game.space.encode(&c).ok_or_else(|| err("the choice is not in this action space".into()))
    }

    /// Ends the battle with `side` losing.
    fn forfeit(&mut self, side: &Bound<'_, PyAny>) -> PyResult<()> {
        self.game.battle.forfeit(side_arg(side)?);
        Ok(())
    }

    /// A copy. Without `seed` it continues with the same random stream
    /// (identical future); with one, its future randomness comes from
    /// `seed` (a determinised rollout).
    #[pyo3(signature = (seed=None))]
    fn fork(&self, seed: Option<&Bound<'_, PyAny>>) -> PyResult<PyBattle> {
        let mut game = self.game.clone();
        if let Some(seed) = seed {
            game.battle = self.game.battle.fork(seed_arg(seed)?);
        }
        Ok(PyBattle { game })
    }

    fn __copy__(&self) -> PyBattle {
        PyBattle { game: self.game.clone() }
    }

    fn __deepcopy__(&self, _memo: &Bound<'_, PyAny>) -> PyBattle {
        self.__copy__()
    }

    /// Number of events so far.
    #[getter]
    fn event_count(&self) -> usize {
        self.game.battle.events().len()
    }

    /// The typed events from `start` on (`BattleEvent` records).
    #[pyo3(signature = (start=0))]
    fn events(&self, py: Python<'_>, #[pyo3(from_py_with = arg::start)] start: usize) -> PyResult<Vec<Py<PyAny>>> {
        let events = self.game.battle.events();
        let dex = self.dex();
        events[start.min(events.len())..].iter().map(|e| typed(py, e, dex)).collect()
    }

    /// The log lines a perspective (`"omniscient"`, `"p1"`, `"p2"`,
    /// `"spectator"`) reads, for the events from `start` on. Start at 0 or
    /// at an `event_count` read between submits.
    #[pyo3(signature = (perspective=None, start=0))]
    fn log(
        &self,
        perspective: Option<&Bound<'_, PyAny>>,
        #[pyo3(from_py_with = arg::start)] start: usize,
    ) -> PyResult<Vec<String>> {
        let p = perspective.map(perspective_arg).transpose()?.unwrap_or(Perspective::Omniscient);
        Ok(self.game.log(p, start))
    }

    /// Showdown's raw stream from event `start`: every line once, with
    /// `|split|pN` blocks (the secret line, then the public one).
    #[pyo3(signature = (start=0))]
    fn raw_log(&self, #[pyo3(from_py_with = arg::start)] start: usize) -> Vec<String> {
        self.game.raw_log(start)
    }

    /// Both players' teams as registered, in Showdown's export format (or
    /// packed).
    #[pyo3(signature = (packed=false))]
    fn teams(&self, packed: bool) -> Vec<String> {
        let format = self.game.battle.format();
        self.game.sheets.iter().map(|s| if packed { pack_team(s, format) } else { export_sheet(s, format) }).collect()
    }

    /// The lines before the first event (`|t:|`, `|gametype|`).
    fn header(&self) -> Vec<String> {
        self.game.renderer(Perspective::Omniscient).header()
    }

    /// The whole battle state, typed (`BattleState`).
    fn state(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        typed(py, self.game.battle.state(), self.dex())
    }

    /// The raw state as int32 arrays: Pokémon `(2, 6, len(POKEMON_COLUMNS))`,
    /// sides `(2, len(SIDE_COLUMNS))`, field `(len(FIELD_COLUMNS),)`.
    fn arrays<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let mut p = vec![0; 2 * MAX_TEAM * arrays::POKEMON_WIDTH];
        let mut s = vec![0; 2 * arrays::SIDE_WIDTH];
        let mut f = vec![0; arrays::FIELD_WIDTH];
        arrays::fill(&self.game.battle, &mut p, &mut s, &mut f);
        PyTuple::new(
            py,
            [
                array(py, p, vec![2, MAX_TEAM, arrays::POKEMON_WIDTH])?,
                array(py, s, vec![2, arrays::SIDE_WIDTH])?,
                array(py, f, vec![arrays::FIELD_WIDTH])?,
            ],
        )
    }

    fn __repr__(&self) -> String {
        let b = &self.game.battle;
        let state = match b.result() {
            Some(r) => format!("over, winner={:?}", r.winner.map(SideId::index)),
            None => format!("turn {}", b.turn()),
        };
        format!("Battle({:?}, {state})", b.format().id)
    }
}

/// The built-in uniformly random policy (`LegalActions::sample`), with its
/// own RNG: `pikapro play --seed S` uses `RandomPolicy(S ^ 0x5EED)`.
#[pyclass(module = "pikapro._pikapro", name = "RandomPolicy", skip_from_py_object)]
#[derive(Debug)]
pub struct PyRandomPolicy {
    rng: BattleRng,
}

#[pymethods]
impl PyRandomPolicy {
    #[new]
    fn new(#[pyo3(from_py_with = arg::seed)] seed: u64) -> Self {
        PyRandomPolicy { rng: BattleRng::new(Seed::fast(seed)) }
    }

    /// A uniformly random jointly legal choice for `side`, or `None` if it
    /// has nothing to decide.
    fn choose(&mut self, battle: PyRef<'_, PyBattle>, side: &Bound<'_, PyAny>) -> PyResult<Option<PyChoice>> {
        let legal = battle.game.battle.legal_actions(side_arg(side)?);
        if legal.is_waiting() {
            return Ok(None);
        }
        Ok(Some(py_choice(legal.sample(&mut self.rng), battle.game.space.active)))
    }
}

// ---------------------------------------------------------------------------
// Pool.

fn totals_dict<'py>(py: Python<'py>, t: &Totals) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("battles", t.battles)?;
    d.set_item("turns", t.turns)?;
    d.set_item("decisions", t.decisions)?;
    d.set_item("p1_wins", t.wins[0])?;
    d.set_item("p2_wins", t.wins[1])?;
    d.set_item("ties", t.ties)?;
    Ok(d)
}

fn action_mode(mode: &str) -> PyResult<ActionMode> {
    match mode {
        "choice" => Ok(ActionMode::Choice),
        "slots" => Ok(ActionMode::Slots),
        _ => Err(PyValueError::new_err(format!("mode must be 'choice' or 'slots', got {mode:?}"))),
    }
}

/// When a [`PyBattlePool`] battle that ended starts its next episode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PoolReset {
    /// In the step that ended it; its terminal state goes to
    /// `final_state()`.
    SameStep,
    /// In the next step (Gymnasium's `NEXT_STEP`), which ignores its
    /// actions.
    NextStep,
    /// Never: it stays over until `reset()`.
    Off,
}

/// The terminal rows of the battles that ended in the last step (same-step
/// autoreset).
#[derive(Debug, Default)]
struct FinalState {
    pokemon: Vec<i32>,
    sides: Vec<i32>,
    field: Vec<i32>,
    episode: Vec<u64>,
    turns: Vec<u16>,
    ended: Vec<bool>,
}

/// `n` battles of `format` stepped together on Rust threads, with the GIL
/// released: masks, actions and the raw (omniscient) state as numpy arrays.
///
/// The masks and `sample_random` are what each side's request shows, as
/// `pikapro.rl.VecEnv`'s masks: a trap by a foe's unseen ability, or a move
/// only a foe's Imprison disables, stays allowed until a choice reveals it
/// (`step` status 2, Showdown's `[Unavailable choice]`); the side's masks
/// show it from then on, until its next request. `state()` is omniscient.
///
/// `teams` is one pair of team texts, a list of pairs (episode `e` plays
/// pair `e % len`), or a `RandomTeams` (fresh teams every episode);
/// `"sets"` (or `"random"`) and `"uniform"` stand for `RandomTeams(format,
/// mode)`, as in `pikapro.rl.VecEnv`.
/// A battle that fails (an engine panic) ends its episode alone, with end
/// reason 5, and the others go on; `take_errors()` says why.
/// Battle `i`'s `k`-th episode is episode `e = i + k * n`, played with
/// seed `pikapro_rl::env::episode_seed(seed, e)` (a hash of the two, as
/// `pikapro.rl.VecEnv`'s; `seeds()` lists the current ones), whatever the
/// number of `threads`. `autoreset` is `"next_step"` (the default, as
/// `pikapro.rl.VecEnv`'s: an ended battle stays over for the step that
/// ended it and starts its next episode at the next step, which ignores
/// its actions), `"same_step"` (an ended battle starts its next episode
/// within the step; `final_state` keeps the terminal state), or `None` or
/// `"off"` (the same: an ended battle stays over until `reset()`). For
/// the reinforcement-learning environment over player views, see
/// `pikapro.rl.VecEnv`.
#[pyclass(module = "pikapro._pikapro", name = "BattlePool", skip_from_py_object)]
#[derive(Debug)]
pub struct PyBattlePool {
    pool: Pool,
    reset: PoolReset,
    /// Battles that ended in the last step and start their next episode in
    /// the next (next-step autoreset).
    pending: std::sync::Mutex<Vec<usize>>,
    last: std::sync::Mutex<FinalState>,
}

fn end_reason_code(reason: pikapro::EndReason) -> u8 {
    pikapro_rl::vec::EnvEndReason::of(&pikapro::BattleResult { winner: None, reason, turns: 0 }) as u8
}

#[pymethods]
impl PyBattlePool {
    #[new]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(
        signature = (format, teams, n, seed=0, threads=None, *, log=false, autoreset=Some("next_step"), spin_us=std::time::Duration::from_micros(100)),
        text_signature = "(format, teams, n, seed=0, threads=None, *, log=False, autoreset='next_step', spin_us=100)"
    )]
    fn new(
        py: Python<'_>,
        format: &str,
        teams: &Bound<'_, PyAny>,
        #[pyo3(from_py_with = arg::n)] n: usize,
        #[pyo3(from_py_with = arg::seed)] seed: u64,
        #[pyo3(from_py_with = arg::threads)] threads: Option<usize>,
        log: bool,
        autoreset: Option<&str>,
        #[pyo3(from_py_with = arg::spin_us)] spin_us: std::time::Duration,
    ) -> PyResult<Self> {
        let reset = match autoreset {
            Some("same_step") => PoolReset::SameStep,
            Some("next_step") => PoolReset::NextStep,
            None | Some("off") => PoolReset::Off,
            Some(other) => {
                return Err(PyValueError::new_err(format!(
                    "autoreset must be 'same_step', 'next_step', 'off' or None, got {other:?}"
                )));
            }
        };
        let format = format_arg(format)?;
        // Random teams per episode, one pair, or a list of pairs (episode
        // `e` plays pair `e % len`).
        let source = if let Ok(name) = teams.cast::<PyString>() {
            Teams::Generator(named_teams(py, &format, name.to_str()?)?)
        } else if let Ok(random) = teams.cast::<PyRandomTeams>() {
            let generator = random.get().generator.clone();
            if generator.format().id != format.id {
                return Err(err(format!("RandomTeams for {} in a {} pool", generator.format().id, format.id)));
            }
            Teams::Generator(generator)
        } else if teams.extract::<[String; 2]>().is_ok() {
            Teams::Pairs(vec![Arc::new(teams_arg(teams, &format)?)])
        } else {
            let list: Vec<Bound<'_, PyAny>> = teams.extract()?;
            Teams::Pairs(list.iter().map(|p| teams_arg(p, &format).map(Arc::new)).collect::<PyResult<Vec<_>>>()?)
        };
        let threads = threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
        let mut config = PoolConfig::new(format, n, source);
        config.seed = seed;
        config.log = log;
        // The resets are this wrapper's (to keep terminal states).
        config.auto_reset = false;
        config.spin = spin_us;
        let pool = py.detach(|| Pool::new(config, threads)).map_err(err)?;
        Ok(PyBattlePool {
            pool,
            reset,
            pending: std::sync::Mutex::new(Vec::new()),
            last: std::sync::Mutex::new(FinalState::default()),
        })
    }

    /// `"same_step"`, `"next_step"` or `None` (also for `"off"`).
    #[getter]
    fn autoreset(&self) -> Option<&'static str> {
        match self.reset {
            PoolReset::SameStep => Some("same_step"),
            PoolReset::NextStep => Some("next_step"),
            PoolReset::Off => None,
        }
    }

    /// The terminal state of the battles that ended in the last `step`
    /// (with same-step autoreset, whose next episode has already started):
    /// a dict of `pokemon` `(n, 2, 6, len(POKEMON_COLUMNS))`, `sides` `(n, 2,
    /// len(SIDE_COLUMNS))` and `field` `(n, len(FIELD_COLUMNS))` int32 (as
    /// `state()`), `episode` uint64 `(n,)`, `turns` uint16 `(n,)` and
    /// `ended` bool `(n,)`; only the rows `ended` flags are set.
    fn final_state<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let n = self.pool.len();
        let last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let fit = |v: &Vec<i32>, len: usize| if v.len() == len { v.clone() } else { vec![0; len] };
        let d = PyDict::new(py);
        d.set_item(
            "pokemon",
            array(
                py,
                fit(&last.pokemon, n * 2 * MAX_TEAM * arrays::POKEMON_WIDTH),
                vec![n, 2, MAX_TEAM, arrays::POKEMON_WIDTH],
            )?,
        )?;
        d.set_item("sides", array(py, fit(&last.sides, n * 2 * arrays::SIDE_WIDTH), vec![n, 2, arrays::SIDE_WIDTH])?)?;
        d.set_item("field", array(py, fit(&last.field, n * arrays::FIELD_WIDTH), vec![n, arrays::FIELD_WIDTH])?)?;
        let or = |v: &Vec<u64>| if v.len() == n { v.clone() } else { vec![0; n] };
        d.set_item("episode", PyArray1::from_vec(py, or(&last.episode)))?;
        let turns = if last.turns.len() == n { last.turns.clone() } else { vec![0; n] };
        d.set_item("turns", PyArray1::from_vec(py, turns))?;
        let ended = if last.ended.len() == n { last.ended.clone() } else { vec![false; n] };
        d.set_item("ended", PyArray1::from_vec(py, ended))?;
        Ok(d)
    }

    /// The number of battles.
    #[getter]
    fn n(&self) -> usize {
        self.pool.len()
    }

    fn __len__(&self) -> usize {
        self.pool.len()
    }

    /// The number of threads stepping the battles.
    #[getter]
    fn threads(&self) -> usize {
        self.pool.threads()
    }

    /// The format id.
    #[getter]
    fn format(&self) -> &'static str {
        self.pool.config().format.id
    }

    /// The format's `ActionSpace`.
    #[getter]
    fn space(&self) -> PyActionSpace {
        PyActionSpace { space: Arc::new(self.pool.space().clone()) }
    }

    /// Submits one action per battle and side: an int array `(n, 2)` of
    /// choice indices, or `(n, 2, active)` of slot-action indices (at team
    /// preview, slot 0 holds the preview order's index within
    /// `0..preview_size`). Values for sides with nothing to decide are
    /// ignored. Returns `(status, done, winner, end_reason)`: int8 `(n, 2)`
    /// (`1` submitted, `0` not asked, `-1` rejected, battle unchanged, `2`
    /// unavailable: the request showed it but hid why the battle refuses
    /// it, nothing submitted, and the masks now show it), bool
    /// `(n,)` (the battle ended in this step), int8 `(n,)` (`-1` not over,
    /// `0`/`1` the winner, `2` a tie) and uint8 `(n,)` (`0` not over, `1`
    /// all fainted, `2` the turn limit, `3` a forfeit, `5` an error), the
    /// last three for the episode that ended. With same-step autoreset its
    /// next episode has already started (its terminal state is in
    /// `final_state()`); with next-step autoreset it starts in the next
    /// step, which ignores the battle's actions.
    ///
    /// A battle that fails (an engine panic, or an episode that can't
    /// start) doesn't raise: its episode ends with end reason 5, no winner
    /// and statuses 0 (a failed start is reported at the step after), the
    /// other battles' results are returned, and `take_errors()` says what
    /// went wrong.
    fn step<'py>(&self, py: Python<'py>, actions: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
        let np = py.import("numpy")?;
        let arr = np.call_method1("asarray", (actions,))?;
        // Integers only: `int64` would truncate floats (1.9 to 1) and take
        // bools as 0 and 1.
        let dtype_kind: String = arr.getattr("dtype")?.getattr("kind")?.extract()?;
        if dtype_kind != "i" && dtype_kind != "u" {
            let dtype = arr.getattr("dtype")?.str()?;
            return Err(PyTypeError::new_err(format!("actions must be an integer array, got dtype {dtype}")));
        }
        let arr = np.call_method1("ascontiguousarray", (arr, "int64"))?;
        let arr = arr.cast::<numpy::PyArrayDyn<i64>>()?;
        let n = self.pool.len();
        let active = self.pool.space().active;
        let shape = arr.shape().to_vec();
        let mode = if shape == [n, 2] {
            ActionMode::Choice
        } else if shape == [n, 2, active] {
            ActionMode::Slots
        } else {
            return Err(PyValueError::new_err(format!(
                "actions must have shape ({n}, 2) (choices) or ({n}, 2, {active}) (slot actions), got {shape:?}"
            )));
        };
        let actions = arr.readonly().as_slice()?.to_vec();
        let pool = &self.pool;
        let reset = self.reset;
        let (out, end_reason, last) = py
            .detach(|| {
                // Next step: the battles that ended last time start again now
                // (their actions were ignored: over battles aren't asked).
                // One that can't start fails, and the next step reports it.
                let (mut out, pending, ended) = crate::owed::step_owing(
                    &self.pending,
                    || pool.step(actions, mode),
                    |out| (0..out.done.len()).filter(|&i| out.done[i]).collect(),
                    |owed| pool.reset(Some(owed.to_vec())),
                )?;
                for &i in &pending {
                    out.winner[i] = -1;
                }
                let mut end_reason = vec![0u8; out.done.len()];
                let mut last = FinalState::default();
                if reset == PoolReset::SameStep {
                    let (p, s, f) = (2 * MAX_TEAM * arrays::POKEMON_WIDTH, 2 * arrays::SIDE_WIDTH, arrays::FIELD_WIDTH);
                    let n = out.done.len();
                    last = FinalState {
                        pokemon: vec![0; n * p],
                        sides: vec![0; n * s],
                        field: vec![0; n * f],
                        episode: vec![0; n],
                        turns: vec![0; n],
                        ended: out.done.clone(),
                    };
                }
                for &i in &ended {
                    if out.failed[i] {
                        // No state to keep: the terminal rows stay zeros.
                        end_reason[i] = pikapro_rl::vec::EnvEndReason::Error as u8;
                        continue;
                    }
                    pool.with_env(i, |env| {
                        if let Some(r) = env.battle.result() {
                            end_reason[i] = end_reason_code(r.reason);
                        }
                        if reset == PoolReset::SameStep {
                            let (p, s, f) =
                                (2 * MAX_TEAM * arrays::POKEMON_WIDTH, 2 * arrays::SIDE_WIDTH, arrays::FIELD_WIDTH);
                            arrays::fill(
                                &env.battle,
                                &mut last.pokemon[i * p..][..p],
                                &mut last.sides[i * s..][..s],
                                &mut last.field[i * f..][..f],
                            );
                            last.episode[i] = env.episode;
                            last.turns[i] = env.battle.turn();
                        }
                    });
                }
                match reset {
                    PoolReset::SameStep if !ended.is_empty() => pool.reset(Some(ended))?,
                    PoolReset::NextStep => *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = ended,
                    _ => {}
                }
                Ok::<_, String>((out, end_reason, last))
            })
            .map_err(PyRuntimeError::new_err)?;
        if reset == PoolReset::SameStep {
            *self.last.lock().unwrap_or_else(|e| e.into_inner()) = last;
        }
        PyTuple::new(
            py,
            [
                PyArray1::from_vec(py, out.status).reshape([n, 2])?.into_any(),
                PyArray1::from_vec(py, out.done).into_any(),
                PyArray1::from_vec(py, out.winner).into_any(),
                PyArray1::from_vec(py, end_reason).into_any(),
            ],
        )
    }

    /// Masks of the actions each side's request shows, bool: `"choice"`
    /// `(n, 2, size)` (jointly legal
    /// choices), `"slots"` `(n, 2, active, slot_size)` (each slot's own
    /// actions; all false at team preview), `"preview"` `(n, 2,
    /// preview_size)` (team-preview orders).
    #[pyo3(signature = (kind="choice"))]
    fn masks<'py>(&self, py: Python<'py>, kind: &str) -> PyResult<Bound<'py, PyAny>> {
        let space = self.pool.space();
        let n = self.pool.len();
        let (kind, shape) = match kind {
            "choice" => (MaskKind::Choice, vec![n, 2, space.size]),
            "slots" => (MaskKind::Slots, vec![n, 2, space.active, space.slot_size]),
            "preview" => (MaskKind::Preview, vec![n, 2, space.preview_size]),
            _ => {
                return Err(PyValueError::new_err(format!(
                    "kind must be 'choice', 'slots' or 'preview', got {kind:?}"
                )));
            }
        };
        let pool = &self.pool;
        let mask = py.detach(|| pool.masks(kind)).map_err(PyRuntimeError::new_err)?;
        array(py, mask, shape)
    }

    /// Each side's request, int8 `(n, 2)`: `0` wait, `1` team preview, `2`
    /// move, `3` forced switch (`REQUEST_CODES`).
    fn requests<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let pool = &self.pool;
        let r = py.detach(|| pool.requests()).map_err(PyRuntimeError::new_err)?;
        Ok(PyArray1::from_vec(py, r).reshape([self.pool.len(), 2])?.into_any())
    }

    /// Uniformly random actions from the built-in policy among the ones
    /// the masks allow, in the shape `step` takes for `mode` (`"choice"` or
    /// `"slots"`); `-1` for sides with nothing to decide.
    #[pyo3(signature = (mode="choice"))]
    fn sample_random<'py>(&self, py: Python<'py>, mode: &str) -> PyResult<Bound<'py, PyAny>> {
        let mode = action_mode(mode)?;
        let pool = &self.pool;
        let a = py.detach(|| pool.sample_random(mode)).map_err(PyRuntimeError::new_err)?;
        let n = self.pool.len();
        let shape = match mode {
            ActionMode::Choice => vec![n, 2],
            ActionMode::Slots => vec![n, 2, self.pool.space().active],
        };
        Ok(PyArray1::from_vec(py, a).reshape(shape)?.into_any())
    }

    /// Plays `battles` whole battles with the built-in random policy
    /// entirely in Rust. Returns totals and `elapsed` seconds. Checks for
    /// Ctrl-C (Python's signal handlers) every few hundred battles; the
    /// battles played until then stay played. With next-step autoreset, a
    /// battle that ended in the last step starts its next episode here
    /// (if it plays any of the `battles`), not at the next step.
    fn play_random<'py>(
        &self,
        py: Python<'py>,
        #[pyo3(from_py_with = arg::battles)] battles: u64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let pool = &self.pool;
        let mut signal: Option<PyErr> = None;
        let result = py.detach(|| {
            pool.play_random(battles, || {
                Python::attach(|py| py.check_signals()).map_err(|e| {
                    signal = Some(e);
                    "interrupted".to_string()
                })
            })
        });
        if result.is_ok() || signal.is_some() {
            // The first round ran (a Ctrl-C comes after it): every battle
            // with a share of `battles` started a new episode if its last
            // was over, so the next step owes it no reset (one would drop
            // the episode now under way).
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.retain(|&i| pool.play_random_share(battles, i) == 0);
        }
        if let Some(e) = signal {
            return Err(e);
        }
        let (totals, elapsed) = result.map_err(PyRuntimeError::new_err)?;
        let d = totals_dict(py, &totals)?;
        d.set_item("elapsed", elapsed)?;
        Ok(d)
    }

    /// The raw state as int32 arrays: Pokémon `(n, 2, 6,
    /// len(POKEMON_COLUMNS))`, sides `(n, 2, len(SIDE_COLUMNS))`, field
    /// `(n, len(FIELD_COLUMNS))`.
    fn state<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let pool = &self.pool;
        let [p, s, f] = py.detach(|| pool.state()).map_err(PyRuntimeError::new_err)?;
        let n = self.pool.len();
        PyTuple::new(
            py,
            [
                array(py, p, vec![n, 2, MAX_TEAM, arrays::POKEMON_WIDTH])?,
                array(py, s, vec![n, 2, arrays::SIDE_WIDTH])?,
                array(py, f, vec![n, arrays::FIELD_WIDTH])?,
            ],
        )
    }

    /// Starts the next episode in the given battles (all by default).
    #[pyo3(signature = (indices=None))]
    fn reset(&self, py: Python<'_>, #[pyo3(from_py_with = arg::indices)] indices: Option<Vec<usize>>) -> PyResult<()> {
        let pool = &self.pool;
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            match &indices {
                None => pending.clear(),
                Some(which) => pending.retain(|i| !which.contains(i)),
            }
        }
        py.detach(|| pool.reset(indices)).map_err(err)
    }

    /// The battles that failed since the last call to this, as
    /// `(battle, episode, message)`: the episode (under way, or one that
    /// couldn't start and is skipped) ended with end reason 5 (see
    /// `step`). At most 1,024 are kept.
    fn take_errors(&self) -> Vec<(usize, u64, String)> {
        self.pool.take_errors().into_iter().map(|e| (e.battle, e.episode, e.message)).collect()
    }

    /// A copy of battle `i` as a `Battle`.
    fn battle(&self, py: Python<'_>, #[pyo3(from_py_with = arg::i)] i: usize) -> PyResult<PyBattle> {
        let pool = &self.pool;
        let space = Arc::new(pool.space().clone());
        let game =
            py.detach(|| pool.with_env(i, |env| Game::from_battle(env.battle.clone(), space, env.sheets.clone())));
        game.map(|game| PyBattle { game }).ok_or_else(|| PyIndexError::new_err(format!("battle {i} out of range")))
    }

    /// The episode each battle is playing (battle `i`'s `k`-th is `i + k *
    /// n`), uint64 `(n,)`.
    fn episodes<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        let pool = &self.pool;
        PyArray1::from_vec(py, py.detach(|| pool.episodes())).into_any()
    }

    /// Each battle's seed (hashed from the pool's seed and the episode, as
    /// `pikapro.rl.VecEnv`'s), uint64 `(n,)`:
    /// `Battle(format, teams, seed)` replays it.
    fn seeds<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        let pool = &self.pool;
        PyArray1::from_vec(py, py.detach(|| pool.seeds())).into_any()
    }

    /// Totals over every battle finished so far (`decisions`: the choices
    /// submitted in them).
    fn finished<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let pool = &self.pool;
        totals_dict(py, &py.detach(|| pool.finished()))
    }

    fn __repr__(&self) -> String {
        format!("BattlePool({:?}, n={}, threads={})", self.format(), self.pool.len(), self.pool.threads())
    }
}

// ---------------------------------------------------------------------------
// Random teams.

/// The random teams a string names as a `teams` argument, `BattlePool`'s
/// and `pikapro.rl.VecEnv`'s (through `_named_teams`): `"sets"` (or
/// `"random"`) and `"uniform"` are `RandomTeams(format, mode)`.
fn named_teams(py: Python<'_>, format: &Format, name: &str) -> PyResult<Arc<pikapro_teams::TeamGenerator>> {
    let mode = match name {
        "sets" | "random" => pikapro_teams::Mode::Sets,
        "uniform" => pikapro_teams::Mode::Uniform,
        other => {
            return Err(err(format!(
                "teams={other:?}: a string names random teams, 'sets', 'uniform' or 'random' (fixed teams aren't a \
                 string)"
            )));
        }
    };
    py.detach(|| crate::teams::generator(format, mode, pikapro_teams::Options::default())).map_err(err)
}

/// `RandomTeams` for a string `teams` argument (`named_teams`), for
/// `pikapro.rl.VecEnv`.
#[pyfunction]
fn _named_teams(py: Python<'_>, format: &str, name: &str) -> PyResult<PyRandomTeams> {
    let format = format_arg(format)?;
    Ok(PyRandomTeams { generator: named_teams(py, &format, name)? })
}

fn ids<T>(names: Option<Vec<String>>, what: &str, find: impl Fn(&str) -> Option<T>) -> PyResult<Option<Vec<T>>> {
    names
        .map(|names| {
            names
                .iter()
                .map(|n| find(n).ok_or_else(|| PyValueError::new_err(format!("unknown {what} {n:?}"))))
                .collect()
        })
        .transpose()
}

/// Random teams from `pikapro-teams`, for one preset and mode.
///
/// Teams pass Showdown's validator, except for `mode="sets"` in
/// `gen9randombattle` and `gen9randomdoublesbattle`: that is Showdown's
/// random-battle team generator, ported exactly, whose teams nothing
/// validates. `team(seed)` is Showdown's team for the seed
/// `gen5,<seed in 16 hex digits>`; it takes no options.
///
/// `mode` is `"uniform"` (every legal choice drawn uniformly) or `"sets"`
/// (sets from Showdown's random-battle data). Options: `level`,
/// `team_size`, `spreads` (`"competitive"` or `"showdown"`), and lists of
/// names restricting `species`, `moves`, `abilities` and `items`. The
/// generator is built once per process for each preset, mode and options.
/// Pass it as a `BattlePool`'s `teams` for fresh teams every episode.
#[pyclass(module = "pikapro._pikapro", name = "RandomTeams", frozen, skip_from_py_object)]
#[derive(Debug)]
pub struct PyRandomTeams {
    pub(crate) generator: Arc<pikapro_teams::TeamGenerator>,
}

#[pymethods]
impl PyRandomTeams {
    #[new]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (format, mode="uniform", *, level=None, team_size=None, spreads="competitive", species=None, moves=None, abilities=None, items=None))]
    fn new(
        py: Python<'_>,
        format: &str,
        mode: &str,
        #[pyo3(from_py_with = arg::level)] level: Option<u8>,
        #[pyo3(from_py_with = arg::team_size)] team_size: Option<usize>,
        spreads: &str,
        species: Option<Vec<String>>,
        moves: Option<Vec<String>>,
        abilities: Option<Vec<String>>,
        items: Option<Vec<String>>,
    ) -> PyResult<Self> {
        let format = format_arg(format)?;
        let mode = pikapro_teams::Mode::from_name(mode)
            .ok_or_else(|| PyValueError::new_err(format!("mode must be 'uniform' or 'sets', got {mode:?}")))?;
        let spreads = match spreads {
            "competitive" => pikapro_teams::Spreads::Competitive,
            "showdown" => pikapro_teams::Spreads::Showdown,
            _ => {
                return Err(PyValueError::new_err(format!(
                    "spreads must be 'competitive' or 'showdown', got {spreads:?}"
                )));
            }
        };
        let dex = format.dex;
        let restrict = pikapro_teams::Restrict {
            species: ids(species, "species", |n| dex.species_by_name(n))?,
            moves: ids(moves, "move", |n| dex.move_by_name(n))?,
            abilities: ids(abilities, "ability", |n| dex.ability_by_name(n))?,
            items: ids(items, "item", |n| dex.item_by_name(n))?,
        };
        let options = pikapro_teams::Options { level, team_size, spreads, restrict };
        let generator = py.detach(|| crate::teams::generator(&format, mode, options)).map_err(err)?;
        Ok(PyRandomTeams { generator })
    }

    /// The format id.
    #[getter]
    fn format(&self) -> &'static str {
        self.generator.format().id
    }

    /// `"uniform"` or `"sets"`.
    #[getter]
    fn mode(&self) -> &'static str {
        self.generator.mode().name()
    }

    /// The team for `seed` (`pikapro_teams::TeamGenerator::team`), in export
    /// format (or packed).
    #[pyo3(signature = (seed, packed=false))]
    fn team(&self, py: Python<'_>, #[pyo3(from_py_with = arg::seed)] seed: u64, packed: bool) -> PyResult<String> {
        let g = &self.generator;
        let generated = py.detach(|| g.generate(seed)).map_err(|e| err(e.to_string()))?;
        let mut sheet = TeamSheet::new(generated.team);
        for (extras, shiny) in sheet.extras.iter_mut().zip(generated.shiny) {
            extras.shiny = shiny;
        }
        Ok(if packed { pack_team(&sheet, g.format()) } else { export_sheet(&sheet, g.format()) })
    }

    /// Both teams a `BattlePool` with these teams plays in the episode
    /// whose battle seed is `seed` (see `BattlePool.seeds()`).
    #[pyo3(signature = (seed, packed=false))]
    fn pair(&self, py: Python<'_>, #[pyo3(from_py_with = arg::seed)] seed: u64, packed: bool) -> PyResult<Vec<String>> {
        let g = &self.generator;
        let sheets = py.detach(|| crate::teams::pair(g, seed)).map_err(|e| err(e.to_string()))?;
        let f = g.format();
        Ok(sheets.iter().map(|s| if packed { pack_team(s, f) } else { export_sheet(s, f) }).collect())
    }

    fn __repr__(&self) -> String {
        format!("RandomTeams({:?}, {:?})", self.format(), self.mode())
    }
}

// ---------------------------------------------------------------------------
// Functions.

/// A team's problems in `format`, in Showdown's words: Showdown's team
/// validator (for gen9vgc and championsvgc their Regulations, for
/// championsbss Champions BSS Reg M-C, for gen9ou [Gen 9] OU, for the
/// custom games and the random battles Anything Goes with Species Clause), then the engine's own
/// rules. Empty if the team is
/// valid. Raises `ValueError` if the text doesn't parse.
#[pyfunction]
fn validate(team: &str, format: &str) -> PyResult<Vec<String>> {
    let format = format_arg(format)?;
    let sheet = pikapro_protocol::import_sheet(team, &format).map_err(|e| err(format!("team: {e}")))?;
    Ok(crate::teams::problems(&sheet, &format))
}

/// The preset format ids.
#[pyfunction]
fn formats() -> Vec<&'static str> {
    Format::presets().iter().map(|f| f.id).collect()
}

/// The built-in sample team for `format`, in Showdown's export format.
#[pyfunction]
fn sample_team(format: &str) -> PyResult<String> {
    let format = format_arg(format)?;
    Ok(export_sheet(&TeamSheet::new(pikapro::testing::sample_team(&format)), &format))
}

/// Validates a team (export or packed) against the format's structural
/// rules and returns it in the packed format.
#[pyfunction]
fn pack(team: &str, format: &str) -> PyResult<String> {
    let format = format_arg(format)?;
    Ok(pack_team(&game::parse_team(team, &format).map_err(err)?, &format))
}

/// A team (export or packed) in Showdown's export format.
#[pyfunction]
fn export(team: &str, format: &str) -> PyResult<String> {
    let format = format_arg(format)?;
    Ok(export_sheet(&game::parse_team(team, &format).map_err(err)?, &format))
}

/// Showdown's `toID`: the text lowercased, with everything but `a-z` and
/// `0-9` removed (`"Flabébé"` -> `"flabb"`, `"[Gen 9] VGC 2025 Reg I"` ->
/// `"gen9vgc2025regi"`). The engine's own (`pikapro_data::to_id`).
#[pyfunction]
fn to_id(text: &str) -> String {
    pikapro_data::to_id(text)
}

/// The interval of a record's score rate at the normal quantile `z`
/// (`pikapro_rl::eval::Record::wilson`): what `pikapro.rl.eval.WinRecord.wilson`
/// returns, so both languages share one implementation.
#[pyfunction]
#[pyo3(signature = (wins, losses, ties, z=1.96))]
fn _wilson(
    #[pyo3(from_py_with = arg::wins)] wins: u64,
    #[pyo3(from_py_with = arg::losses)] losses: u64,
    #[pyo3(from_py_with = arg::ties)] ties: u64,
    z: f64,
) -> PyResult<(f64, f64)> {
    // A z <= 0 gives an inverted interval, and NaN or infinity a meaningless one.
    if !(z.is_finite() && z > 0.0) {
        return Err(err(format!("z must be a positive, finite normal quantile, got {z}")));
    }
    Ok(pikapro_rl::eval::Record { wins, losses, ties, ..Default::default() }.wilson(z))
}

/// Every name of a table, indexed by ID: `"species"`, `"moves"`,
/// `"abilities"`, `"items"` or `"types"`. The ID space is shared by all
/// formats.
#[pyfunction]
#[pyo3(signature = (table, format="gen9vgc"))]
fn names(table: &str, format: &str) -> PyResult<Vec<&'static str>> {
    let dex = format_arg(format)?.dex;
    Ok(match table {
        "species" => dex.species.iter().map(|s| s.name).collect(),
        "moves" => dex.moves.iter().map(|m| m.name).collect(),
        "abilities" => dex.abilities.iter().map(|a| a.name).collect(),
        "items" => dex.items.iter().map(|i| i.name).collect(),
        // The Tera types (Stellar last), then Burn Up's and Double Shock's
        // typeless `???`: indexed by `Type`'s discriminant.
        "types" => pikapro_data::Type::TERA.iter().chain([&pikapro_data::Type::Unknown]).map(|t| t.name()).collect(),
        _ => return Err(PyValueError::new_err(format!("unknown table {table:?}"))),
    })
}

/// Panics on purpose (tests of how a Rust panic reaches Python).
#[pyfunction]
fn _panic(message: &str) {
    // `resume_unwind`: no panic hook, so no message on stderr.
    std::panic::resume_unwind(Box::new(message.to_string()));
}

/// Makes a Rust panic that reaches Python a `RuntimeError`.
///
/// PyO3 turns a panic escaping any `#[pyfunction]` or `#[pymethods]` call
/// (they all go through its trampoline, which catches the unwind) into
/// `pyo3_runtime.PanicException`, a `BaseException`: `except Exception`
/// doesn't catch it, so one engine bug on one battle would end a training
/// loop or `pikapro.showdown`'s client, with every other battle. PyO3 0.29
/// has no option to choose the exception, so its type (a heap type this
/// extension owns: every PyO3 extension creates its own) gets
/// `RuntimeError` as its base, for every call at once, including calls
/// added later. It keeps its name and the panic's message.
fn panics_are_runtime_errors(py: Python<'_>) -> PyResult<()> {
    let panic = py.get_type::<pyo3::panic::PanicException>();
    panic.setattr("__bases__", (py.get_type::<PyRuntimeError>(),))
}

#[pymodule]
fn _pikapro(m: &Bound<'_, PyModule>) -> PyResult<()> {
    panics_are_runtime_errors(m.py())?;
    m.add_class::<Record>()?;
    m.add_class::<PySlotAction>()?;
    m.add_class::<PyChoice>()?;
    m.add_class::<PyActionSpace>()?;
    m.add_class::<PyBattle>()?;
    m.add_class::<PyRandomPolicy>()?;
    m.add_class::<PyBattlePool>()?;
    m.add_class::<PyRandomTeams>()?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    m.add_function(wrap_pyfunction!(_named_teams, m)?)?;
    m.add_function(wrap_pyfunction!(formats, m)?)?;
    m.add_function(wrap_pyfunction!(sample_team, m)?)?;
    m.add_function(wrap_pyfunction!(pack, m)?)?;
    m.add_function(wrap_pyfunction!(export, m)?)?;
    m.add_function(wrap_pyfunction!(names, m)?)?;
    m.add_function(wrap_pyfunction!(to_id, m)?)?;
    m.add_function(wrap_pyfunction!(_wilson, m)?)?;
    m.add_function(wrap_pyfunction!(_panic, m)?)?;
    m.add("POKEMON_COLUMNS", arrays::POKEMON_COLUMNS.to_vec())?;
    m.add("SIDE_COLUMNS", arrays::SIDE_COLUMNS.to_vec())?;
    m.add("FIELD_COLUMNS", arrays::FIELD_COLUMNS.to_vec())?;
    m.add("STATUS_CODES", arrays::STATUS_CODES.to_vec())?;
    m.add("WEATHER_CODES", arrays::WEATHER_CODES.to_vec())?;
    m.add("TERRAIN_CODES", arrays::TERRAIN_CODES.to_vec())?;
    m.add("REQUEST_CODES", arrays::REQUEST_CODES.to_vec())?;
    m.add("VOLATILE_NAMES", crate::volatiles::volatile_names())?;
    crate::rl::register(m)?;
    crate::view::register(m)?;
    crate::session::register(m)?;
    Ok(())
}
