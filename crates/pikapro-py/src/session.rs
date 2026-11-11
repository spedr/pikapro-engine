//! The PyO3 layer of `pikapro.rl.ChannelSession` (feature `python`): one
//! battle driven by Showdown's text (`pikapro_rl::session::ChannelSession`).
//! The Python-facing class and its documentation are in
//! `python/pikapro/rl/session.py`.

use numpy::PyArray1;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use pikapro::SideId;
use pikapro_protocol::channel::showdown_format;
use pikapro_rl::series::{Series, SeriesResult, check_best_of};
use pikapro_rl::session::{ChannelSession, SessionOptions};

use crate::python::{arg, err};
use crate::rl::{HP, MASKS, ObsInfo, SWITCH_BY, obs_info_dict, observation_arg, parse_choice_of};

#[pyclass(module = "pikapro._pikapro", name = "_ChannelSession", skip_from_py_object)]
pub(crate) struct PyChannelSession {
    s: ChannelSession,
    info: ObsInfo,
}

impl PyChannelSession {
    fn feed_one(&mut self, line: &str) {
        for line in line.split('\n') {
            self.s.feed(line);
        }
    }
}

#[pymethods]
impl PyChannelSession {
    #[new]
    #[pyo3(signature = (format_id, *, observation="basic", masks="factored", switch_by="row", username=None, hp="percent", team=None, series=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        format_id: &str,
        observation: &str,
        masks: &str,
        switch_by: &str,
        username: Option<String>,
        hp: &str,
        team: Option<&str>,
        series: Option<PyRef<'_, PyChannelSeries>>,
    ) -> PyResult<Self> {
        let f = showdown_format(format_id).ok_or_else(|| unsupported(format_id))?;
        let (observation, info) = observation_arg(observation)?;
        let options = SessionOptions::default()
            .with_observation(observation)
            .with_masks(parse_choice_of("masks", masks, &MASKS)?)
            .with_switch_by(parse_choice_of("switch_by", switch_by, &SWITCH_BY)?)
            .with_hp(parse_choice_of("hp", hp, &HP)?)
            .with_username(username)
            // The team as the player gave it (export or packed format);
            // `ChannelSession::new` plays it as the format's server does.
            .with_team(
                team.map(|t| pikapro_protocol::import_sheet(t, &f.format).map(|s| s.team))
                    .transpose()
                    .map_err(|e| err(format!("team: {e}")))?,
            );
        let mut s = ChannelSession::new(format_id, options).map_err(err)?;
        if let Some(series) = series {
            if series.format != s.format().id {
                return Err(err(format!("a {} series, not {}", series.format, s.format().id)));
            }
            s.set_series(series.s.clone()).map_err(err)?;
        }
        Ok(PyChannelSession { s, info })
    }

    /// The environment's `series` row for this player (game, own wins,
    /// the opponent's, the result: 1 this player won the series), `None`
    /// outside a series.
    #[getter]
    fn series_row(&self) -> Option<(u8, u8, u8, u8)> {
        self.s.series_row().map(|[a, b, c, d]| (a, b, c, d))
    }

    /// Feeds one line (or several joined by `\n`).
    fn feed(&mut self, line: &str) {
        self.feed_one(line);
    }

    fn feed_many(&mut self, lines: Vec<String>) {
        for line in &lines {
            self.feed_one(line);
        }
    }

    #[getter]
    fn needs_action(&self) -> bool {
        self.s.needs_action()
    }

    #[getter]
    fn request_kind(&self) -> &'static str {
        self.s.request_kind().name()
    }

    #[getter]
    fn finished(&self) -> bool {
        self.s.finished()
    }

    /// The winner's name (`None` for a tie or before the end).
    #[getter]
    fn winner(&self) -> Option<String> {
        let side = self.s.winner()?;
        let name = self.s.parser().player_name(side);
        Some(if name.is_empty() { pikapro_protocol::side_id(side).to_string() } else { name.to_string() })
    }

    #[getter]
    fn winner_side(&self) -> Option<&'static str> {
        self.s.winner().map(pikapro_protocol::side_id)
    }

    #[getter]
    fn side(&self) -> Option<&'static str> {
        self.s.side().map(pikapro_protocol::side_id)
    }

    #[getter]
    fn format(&self) -> &'static str {
        self.s.format().id
    }

    #[getter]
    fn turn(&self) -> u16 {
        self.s.tracker().turn
    }

    #[getter]
    fn unknown_lines(&self) -> Vec<String> {
        self.s.unknown_line_texts().to_vec()
    }

    #[getter]
    fn unknown_count(&self) -> u32 {
        self.s.unknown_lines()
    }

    #[getter]
    fn unknown_names(&self) -> u32 {
        self.s.parser().unknown_names()
    }

    #[getter]
    fn error(&self) -> Option<String> {
        self.s.error().map(str::to_owned)
    }

    #[getter]
    fn observation<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        obs_info_dict(py, &self.info)
    }

    #[getter]
    fn mask_width(&self) -> usize {
        self.s.options().masks.width(self.s.space())
    }

    #[getter]
    fn actions_per_player(&self) -> usize {
        self.s.options().masks.actions(self.s.space())
    }

    /// The observation: the encoder's float32 array (a fresh copy).
    fn obs<'py>(&mut self, py: Python<'py>) -> Bound<'py, PyArray1<f32>> {
        PyArray1::from_vec(py, self.s.obs().to_vec())
    }

    /// The view as plain values, for `pikapro.rl.BattleView`.
    fn _view<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        crate::view::view_dict(py, &self.s.view(), self.s.format())
    }

    fn mask<'py>(&mut self, py: Python<'py>) -> Bound<'py, PyArray1<bool>> {
        PyArray1::from_slice(py, self.s.mask())
    }

    #[getter]
    fn min_switches(&mut self) -> u8 {
        self.s.min_switches()
    }

    /// Showdown's `/choose` text for an action.
    fn choice(&mut self, #[pyo3(from_py_with = arg::actions)] actions: Vec<i64>) -> PyResult<String> {
        self.s.choice(&actions).map_err(err)
    }

    fn choose_builtin(&mut self, name: &str, #[pyo3(from_py_with = arg::seed)] seed: u64) -> PyResult<String> {
        self.s.choose_builtin(name, seed).map_err(err)
    }

    /// The series ended as the server says (`winner` `"p1"`/`"p2"`, or
    /// `tie=True`), whatever this game showed: `series_row` shows it.
    #[pyo3(signature = (winner=None, tie=false))]
    fn settle_series(&mut self, winner: Option<&str>, tie: bool) -> PyResult<()> {
        let w = settled_winner(winner, tie)?;
        self.s.settle_series(w).map_err(err)
    }

    fn __repr__(&self) -> String {
        format!(
            "ChannelSession({:?}, side={:?}, turn={}, request={:?}, needs_action={})",
            self.s.format().id,
            self.s.side().map(pikapro_protocol::side_id),
            self.s.tracker().turn,
            self.s.request_kind().name(),
            self.s.needs_action()
        )
    }
}

/// A best-of series played one `ChannelSession` per game: the series the
/// next game is played in (wins by side), or the settled one.
#[pyclass(module = "pikapro._pikapro", name = "_ChannelSeries", skip_from_py_object)]
pub(crate) struct PyChannelSeries {
    s: Series,
    /// The pikapro format its games are read as.
    format: &'static str,
}

/// A series' end as the server gave it: `winner` (`"p1"`, `"p2"`) or a tie.
fn settled_winner(winner: Option<&str>, tie: bool) -> PyResult<Option<SideId>> {
    match (winner, tie) {
        (Some(_), true) => Err(err("a winner or a tie, not both".into())),
        (Some("p1"), false) => Ok(Some(SideId::P1)),
        (Some("p2"), false) => Ok(Some(SideId::P2)),
        (Some(w), false) => Err(err(format!("winner is 'p1' or 'p2', not {w:?}"))),
        (None, true) => Ok(None),
        (None, false) => Err(err("give the series' winner ('p1', 'p2') or tie=True".into())),
    }
}

fn result_name(r: SeriesResult) -> Option<&'static str> {
    match r {
        SeriesResult::Ongoing => None,
        SeriesResult::Player0 => Some("p1"),
        SeriesResult::Player1 => Some("p2"),
        SeriesResult::Tie => Some("tie"),
    }
}

#[pymethods]
impl PyChannelSeries {
    #[new]
    #[pyo3(signature = (format_id, best_of=None))]
    fn new(format_id: &str, #[pyo3(from_py_with = arg::best_of)] best_of: Option<u8>) -> PyResult<Self> {
        let f = showdown_format(format_id).ok_or_else(|| unsupported(format_id))?;
        let best_of = best_of.unwrap_or(f.best_of.unwrap_or(3));
        check_best_of(best_of).map_err(err)?;
        Ok(PyChannelSeries { s: Series::new(best_of), format: f.format.id })
    }

    #[getter]
    fn best_of(&self) -> u8 {
        self.s.best_of
    }

    #[getter]
    fn game(&self) -> u8 {
        self.s.game
    }

    #[getter]
    fn wins(&self) -> (u8, u8) {
        (self.s.wins[0], self.s.wins[1])
    }

    #[getter]
    fn ties(&self) -> u8 {
        self.s.ties
    }

    #[getter]
    fn result(&self) -> Option<&'static str> {
        result_name(self.s.result)
    }

    #[getter]
    fn over(&self) -> bool {
        self.s.is_over()
    }

    #[getter]
    fn recorded(&self) -> usize {
        self.s.previous.len()
    }

    #[getter]
    fn format(&self) -> &'static str {
        self.format
    }

    /// Records a game's session: its winner (or `winner`/`tie` when the
    /// session didn't read the end) and its public record, then moves to
    /// the next game unless the series is over.
    #[pyo3(signature = (session, winner=None, tie=false))]
    fn record(&mut self, session: PyRef<'_, PyChannelSession>, winner: Option<&str>, tie: bool) -> PyResult<()> {
        let s = &session.s;
        if self.s.is_over() {
            return Err(err("the series is over".into()));
        }
        let played = s.series().ok_or_else(|| err("the session isn't a game of a series".into()))?;
        let at = |x: &Series| (x.best_of, x.game, x.wins, x.ties, x.previous.len());
        if at(played) != at(&self.s) {
            return Err(err(format!(
                "the session played game {} of another series than this one, at game {}",
                played.game, self.s.game
            )));
        }
        let given = (winner.is_some() || tie).then(|| settled_winner(winner, tie)).transpose()?;
        let recorded = match (given, s.recorded_series()) {
            (None, Some(r)) => r.clone(),
            (None, None) => {
                return Err(err("the session didn't read the game's result (no end, or a winner neither player's): \
                     give its winner (or tie=True)"
                    .into()));
            }
            (Some(w), _) => {
                let mut r = played.clone();
                r.record(w.map(SideId::index), s.tracker()).map_err(err)?;
                r
            }
        };
        self.s = recorded.next_game().unwrap_or(recorded);
        Ok(())
    }

    /// Ends the series as the server did (the series room's `|win|` or
    /// `|tie`: a forfeit, a disconnection), whatever the games' score:
    /// `winner` `"p1"`/`"p2"`, or `tie=True`. A no-op for a series already
    /// settled so.
    #[pyo3(signature = (winner=None, tie=false))]
    fn settle(&mut self, winner: Option<&str>, tie: bool) -> PyResult<()> {
        let result = match settled_winner(winner, tie)? {
            Some(SideId::P1) => SeriesResult::Player0,
            Some(SideId::P2) => SeriesResult::Player1,
            None => SeriesResult::Tie,
        };
        self.s.settle(result).map_err(err)
    }

    fn __repr__(&self) -> String {
        format!(
            "ChannelSeries(best_of={}, game={}, wins={:?}, ties={}, result={:?})",
            self.s.best_of,
            self.s.game,
            self.s.wins,
            self.s.ties,
            result_name(self.s.result)
        )
    }
}

/// The pikapro preset a Showdown format id is read as
/// (`pikapro_protocol::channel::showdown_format`), or `None` when
/// `ChannelSession` can't read it.
#[pyfunction]
fn _showdown_preset(format_id: &str) -> Option<&'static str> {
    showdown_format(format_id).map(|f| f.format.id)
}

/// The error for a Showdown format `ChannelSession` can't read, naming
/// the ones it can (`pikapro_protocol::channel::showdown_format`).
fn unsupported(format_id: &str) -> PyErr {
    err(format!(
        "unsupported Showdown format {format_id:?}: ChannelSession reads gen9championsvgc2026regmc and regmb \
         (and their bo3 series), championsvgc, gen9championsbssregmc and regmb, championsbss, \
         gen9vgc20* (and bo3), gen9ou, gen9singles, gen9doubles, gen9vgc, \
         gen9randombattle (and unrated, blitz), gen9randomdoublesbattle (as the random-battle presets), and the gen9 custom games \
         with !Team Preview (or Picked Team Size = 4 in doubles)"
    ))
}

/// Registers `_ChannelSession`, `_ChannelSeries` and `_showdown_preset` (called from `_pikapro`).
pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyChannelSession>()?;
    m.add_class::<PyChannelSeries>()?;
    m.add_function(wrap_pyfunction!(_showdown_preset, m)?)?;
    Ok(())
}
