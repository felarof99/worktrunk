//! Piece D: pure cleanup verdicts with caller-supplied time and no I/O.

use std::time::Duration;

use super::{BusyState, KeepReason, TerminalStatus, Timestamp, Verdict, WorktreeFacts};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn old_enough(time: Timestamp, now: Timestamp, age: Duration) -> bool {
    now.duration_since(time).is_ok_and(|elapsed| elapsed >= age)
}

fn busy(activity: &BusyState) -> bool {
    activity.unknown || !activity.processes.is_empty() || !activity.terminals.is_empty()
}

/// Apply the sweep table in order: primary/bare, pinned, silent seven days,
/// busy, remove-requested, recent, otherwise remove. Only the silent rule may
/// nominate terminals to end; locks and backup gates still protect removal.
pub fn sweep(facts: &WorktreeFacts, now: Timestamp) -> anyhow::Result<Verdict> {
    if facts.primary || facts.bare {
        return Ok(Verdict::Keep(KeepReason::Primary));
    }
    if facts.pinned {
        return Ok(Verdict::Keep(KeepReason::Pinned));
    }
    let silent = !facts.busy.unknown
        && old_enough(facts.last_touch, now, DAY * 7)
        && facts.busy.terminals.iter().all(|terminal| {
            terminal.status != Some(TerminalStatus::Working)
                && terminal
                    .turn_changed_at
                    .is_some_and(|time| old_enough(time, now, DAY * 7))
        });
    if silent {
        return Ok(Verdict::Remove {
            terminals: facts.busy.terminals.clone(),
        });
    }
    if busy(&facts.busy) {
        return Ok(Verdict::Keep(KeepReason::InUse));
    }
    if facts.remove_requested {
        return Ok(Verdict::Remove { terminals: vec![] });
    }
    if !old_enough(facts.last_touch, now, DAY) {
        return Ok(Verdict::Keep(KeepReason::Recent));
    }
    Ok(Verdict::Remove { terminals: vec![] })
}

/// Apply the explicit-rm table: refuse primary and pinned worktrees, defer
/// busy worktrees after backup, otherwise remove. The caller's process counts
/// as busy; refusal reasons become rm failures. This function never backs up.
pub fn rm(facts: &WorktreeFacts, _now: Timestamp) -> anyhow::Result<Verdict> {
    if facts.primary {
        return Ok(Verdict::Keep(KeepReason::Primary));
    }
    if facts.pinned {
        return Ok(Verdict::Keep(KeepReason::Pinned));
    }
    if busy(&facts.busy) {
        return Ok(Verdict::Defer);
    }
    Ok(Verdict::Remove { terminals: vec![] })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::clean::{BusyState, HcTerminal, KeepReason, ProcessInfo, TerminalStatus};

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    fn now() -> Timestamp {
        UNIX_EPOCH + DAY * 100
    }

    fn facts() -> WorktreeFacts {
        WorktreeFacts {
            path: "/repo.feature".into(),
            branch: Some("feature".into()),
            primary: false,
            bare: false,
            pinned: false,
            remove_requested: false,
            lock_reason: None,
            busy: BusyState {
                processes: vec![],
                terminals: vec![],
                unknown: false,
            },
            last_touch: now() - DAY * 2,
            dirty: false,
            tip_sha: "abc".into(),
            status_porcelain: vec![],
        }
    }

    fn terminal(status: Option<TerminalStatus>, age: Option<Duration>) -> HcTerminal {
        HcTerminal {
            address: "t1".into(),
            cwd: "/repo.feature".into(),
            status,
            turn: 4,
            turn_changed_at: age.map(|age| now() - age),
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Expected {
        Keep(KeepReason),
        Remove(Vec<String>),
        Defer,
    }

    fn expected(verdict: Verdict) -> Expected {
        match verdict {
            Verdict::Keep(reason) => Expected::Keep(reason),
            Verdict::Remove { terminals } => {
                assert!(
                    terminals
                        .iter()
                        .all(|t| t.status != Some(TerminalStatus::Working))
                );
                Expected::Remove(terminals.into_iter().map(|t| t.address).collect())
            }
            Verdict::Defer => Expected::Defer,
        }
    }

    #[test]
    fn tests_that_sweep_rows_apply_in_order() {
        use Expected::{Keep, Remove};
        use KeepReason::{InUse, Pinned, Primary, Recent};

        // Each overlapping case proves that an earlier row wins.
        for (name, primary, bare, pinned, age, busy, requested, want) in [
            (
                "primary before pinned",
                true,
                false,
                true,
                DAY * 8,
                true,
                true,
                Keep(Primary),
            ),
            (
                "bare before pinned",
                false,
                true,
                true,
                DAY * 8,
                true,
                true,
                Keep(Primary),
            ),
            (
                "pinned before silent",
                false,
                false,
                true,
                DAY * 8,
                true,
                true,
                Keep(Pinned),
            ),
            (
                "silent before busy",
                false,
                false,
                false,
                DAY * 8,
                true,
                true,
                Remove(vec!["t1".into()]),
            ),
            (
                "busy before requested",
                false,
                false,
                false,
                DAY,
                true,
                true,
                Keep(InUse),
            ),
            (
                "requested before recent",
                false,
                false,
                false,
                DAY / 2,
                false,
                true,
                Remove(vec![]),
            ),
            (
                "recent",
                false,
                false,
                false,
                DAY / 2,
                false,
                false,
                Keep(Recent),
            ),
            (
                "otherwise",
                false,
                false,
                false,
                DAY * 2,
                false,
                false,
                Remove(vec![]),
            ),
        ] {
            let mut f = facts();
            f.primary = primary;
            f.bare = bare;
            f.pinned = pinned;
            f.last_touch = now() - age;
            f.remove_requested = requested;
            if busy {
                f.busy.processes.push(ProcessInfo {
                    pid: 1,
                    cwd: f.path.clone(),
                });
                f.busy
                    .terminals
                    .push(terminal(Some(TerminalStatus::Blocked), Some(DAY * 8)));
            }
            assert_eq!(expected(sweep(&f, now()).unwrap()), want, "{name}");
        }
    }

    #[test]
    fn tests_that_sweep_respects_exact_time_edges() {
        for (age, recent) in [
            (DAY - Duration::from_nanos(1), true),
            (DAY, false),
            (DAY + Duration::from_nanos(1), false),
        ] {
            let mut f = facts();
            f.last_touch = now() - age;
            let want = if recent {
                Expected::Keep(KeepReason::Recent)
            } else {
                Expected::Remove(vec![])
            };
            assert_eq!(expected(sweep(&f, now()).unwrap()), want, "{age:?}");
        }
        for (age, silent) in [
            (DAY * 7 - Duration::from_nanos(1), false),
            (DAY * 7, true),
            (DAY * 7 + Duration::from_nanos(1), true),
        ] {
            for edge in ["touch", "turn"] {
                let mut f = facts();
                f.last_touch = now() - if edge == "touch" { age } else { DAY * 8 };
                f.busy.terminals.push(terminal(
                    Some(TerminalStatus::Done),
                    Some(if edge == "turn" { age } else { DAY * 8 }),
                ));
                let want = if silent {
                    Expected::Remove(vec!["t1".into()])
                } else {
                    Expected::Keep(KeepReason::InUse)
                };
                assert_eq!(expected(sweep(&f, now()).unwrap()), want, "{edge}: {age:?}");
            }
        }
        let mut f = facts();
        f.last_touch = now() + DAY;
        assert_eq!(
            expected(sweep(&f, now()).unwrap()),
            Expected::Keep(KeepReason::Recent)
        );
    }

    #[test]
    fn tests_that_silence_requires_known_activity_and_every_terminal_history() {
        for (status, age, unknown, silent) in [
            (Some(TerminalStatus::Working), Some(DAY * 8), false, false),
            (Some(TerminalStatus::Blocked), None, false, false),
            (None, None, false, false),
            (Some(TerminalStatus::Idle), Some(DAY), false, false),
            (Some(TerminalStatus::Done), Some(DAY * 8), true, false),
            (Some(TerminalStatus::Blocked), Some(DAY * 8), false, true),
            (None, Some(DAY * 8), false, true),
        ] {
            let mut f = facts();
            f.last_touch = now() - DAY * 8;
            f.busy.unknown = unknown;
            let mut old = terminal(Some(TerminalStatus::Idle), Some(DAY * 8));
            old.address = "t0".into();
            f.busy.terminals = vec![old, terminal(status, age)];
            let want = if silent {
                Expected::Remove(vec!["t0".into(), "t1".into()])
            } else {
                Expected::Keep(KeepReason::InUse)
            };
            assert_eq!(
                expected(sweep(&f, now()).unwrap()),
                want,
                "{status:?}, {age:?}, unknown={unknown}"
            );
        }
        let mut f = facts();
        f.last_touch = now() - DAY * 8;
        f.busy.unknown = true;
        assert_eq!(
            expected(sweep(&f, now()).unwrap()),
            Expected::Keep(KeepReason::InUse)
        );
        f.busy.unknown = false;
        assert_eq!(
            expected(sweep(&f, now()).unwrap()),
            Expected::Remove(vec![])
        );
    }

    #[test]
    fn tests_that_rm_refuses_then_defers_busy_and_never_ends_terminals() {
        for (primary, pinned, busy, want) in [
            (true, true, true, Expected::Keep(KeepReason::Primary)),
            (false, true, true, Expected::Keep(KeepReason::Pinned)),
            (false, false, true, Expected::Defer),
            (false, false, false, Expected::Remove(vec![])),
        ] {
            for kind in ["process", "terminal", "unknown"] {
                let mut f = facts();
                f.primary = primary;
                f.pinned = pinned;
                f.last_touch = now() - DAY * 8;
                if busy {
                    match kind {
                        "process" => f.busy.processes.push(ProcessInfo {
                            pid: 1,
                            cwd: f.path.clone(),
                        }),
                        "terminal" => f
                            .busy
                            .terminals
                            .push(terminal(Some(TerminalStatus::Done), Some(DAY * 8))),
                        _ => f.busy.unknown = true,
                    }
                }
                assert_eq!(
                    expected(rm(&f, now()).unwrap()),
                    want,
                    "{primary}, {pinned}, {busy}, {kind}"
                );
            }
        }
    }
}
