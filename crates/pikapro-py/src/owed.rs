//! The resets `BattlePool.step` owes with next-step autoreset: the battles
//! that ended in one step start their next episode in the next.

use std::sync::Mutex;

/// One step of next-step autoreset. Takes the resets `owed`, runs `step`,
/// then `reset`s the battles owed; returns the step's output, the battles
/// reset and the battles the step ended (`ended` of the output). When
/// `step` or `reset` fails (a failure outside the battles), the resets it
/// took stay owed, and so do those of the battles the step ended.
pub fn step_owing<S>(
    owed: &Mutex<Vec<usize>>,
    step: impl FnOnce() -> Result<S, String>,
    ended: impl FnOnce(&S) -> Vec<usize>,
    reset: impl FnOnce(&[usize]) -> Result<(), String>,
) -> Result<(S, Vec<usize>, Vec<usize>), String> {
    let pending = std::mem::take(&mut *owed.lock().unwrap_or_else(|e| e.into_inner()));
    let owe = |more: &[usize]| {
        let mut owed = owed.lock().unwrap_or_else(|e| e.into_inner());
        let mut all: Vec<usize> = pending.iter().chain(more).chain(owed.iter()).copied().collect();
        all.sort_unstable();
        all.dedup();
        *owed = all;
    };
    let out = step().inspect_err(|_| owe(&[]))?;
    let ended = ended(&out);
    if !pending.is_empty() {
        reset(&pending).inspect_err(|_| owe(&ended))?;
    }
    Ok((out, pending, ended))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owed(v: &[usize]) -> Mutex<Vec<usize>> {
        Mutex::new(v.to_vec())
    }

    #[test]
    fn a_step_resets_the_battles_owed() {
        let o = owed(&[1, 3]);
        let mut reset = Vec::new();
        let (out, pending, ended) = step_owing(
            &o,
            || Ok(7),
            |_| vec![2],
            |w| {
                reset.extend_from_slice(w);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!((out, pending, ended, reset), (7, vec![1, 3], vec![2], vec![1, 3]));
        assert!(o.lock().unwrap().is_empty(), "the caller owes the ended ones");
    }

    #[test]
    fn a_failed_step_keeps_the_resets_owed() {
        let o = owed(&[1, 3]);
        let e = step_owing(&o, || Err::<(), _>("pool".into()), |_| vec![], |_| panic!("no reset")).unwrap_err();
        assert_eq!(e, "pool");
        assert_eq!(*o.lock().unwrap(), vec![1, 3]);
    }

    #[test]
    fn a_failed_reset_keeps_those_and_owes_the_battles_that_ended() {
        let o = owed(&[3, 1]);
        let e = step_owing(&o, || Ok(()), |_| vec![0, 3], |_| Err("reset".into())).unwrap_err();
        assert_eq!(e, "reset");
        assert_eq!(*o.lock().unwrap(), vec![0, 1, 3]);
    }

    #[test]
    fn nothing_owed_resets_nothing() {
        let o = owed(&[]);
        let (_, pending, _) = step_owing(&o, || Ok(()), |_| vec![4], |_| panic!("no reset")).unwrap();
        assert!(pending.is_empty());
    }
}
