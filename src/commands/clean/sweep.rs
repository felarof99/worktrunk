//! Daily cleanup coordinates the shared collectors, policy, and remover.
//!
//! Activity is sampled once for the whole machine. Each repository emits local
//! keep decisions as facts arrive, before its one remote advertisement. Only
//! removal candidates wait for that advertisement; each completed removal is
//! reported immediately. Work is sequential to bound load on shared machines.
//! Failed observations and removals keep their worktrees; a repository failure
//! skips that repository. Interrupts abort both loops. A dry run passes through
//! the same decisions but never registers, updates activity history, or expires
//! the attic, and the shared remover receives its read-only option.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::Context;
use color_print::cformat;
use worktrunk::git::{ErrorExt, Repository, WorktrunkError};
use worktrunk::path::format_path_for_display;
use worktrunk::styling::{
    eprintln, format_heading, info_message, println, progress_message, success_message,
    warning_message,
};

use super::{
    ActivitySnapshot, CleanRow, KeepReason, OutputVerdict, RemoteSnapshot, RemovalOptions,
    Timestamp, Verdict, WorktreeFacts, attic, facts, policy, registry, remote, rm, terminals,
};
use crate::cli::SwitchFormat;

// The remover is shared with explicit rm; injection keeps sweep tests independent
// of its backup, hook, and deletion implementation.
type Remover<'a> = dyn FnMut(
        &Repository,
        &WorktreeFacts,
        &RemoteSnapshot,
        &Verdict,
        RemovalOptions,
        Timestamp,
    ) -> anyhow::Result<CleanRow>
    + 'a;

pub(crate) fn run(
    all_repos: bool,
    roots: Vec<PathBuf>,
    dry_run: bool,
    format: SwitchFormat,
    yes: bool,
) -> anyhow::Result<()> {
    run_with_remover(
        all_repos,
        &roots,
        RemovalOptions { dry_run, yes },
        format,
        &mut rm::remove_one,
    )
}

fn run_with_remover(
    all_repos: bool,
    roots: &[PathBuf],
    options: RemovalOptions,
    format: SwitchFormat,
    remove: &mut Remover<'_>,
) -> anyhow::Result<()> {
    let current = match Repository::current() {
        Ok(repo) => Some(repo),
        Err(error) if all_repos => {
            propagate_interrupt(&error)?;
            None
        }
        Err(error) => return Err(error),
    };
    let selection = registry::load_repositories(current.as_ref(), all_repos, roots)?;
    for skipped in selection.skipped {
        report_skipped(&skipped.path, &skipped.reason);
    }
    let now = UNIX_EPOCH + Duration::from_secs(worktrunk::utils::epoch_now());
    eprintln!(
        "{}",
        progress_message(if options.dry_run {
            "Inspecting worktrees (--dry-run)..."
        } else {
            "Inspecting worktrees..."
        })
    );
    let activity = terminals::load(now, options.dry_run)?;
    let mut rows = Vec::new();
    for path in selection.repositories {
        if matches!(format, SwitchFormat::Text) {
            text_line(
                format_heading(
                    "REPOSITORY",
                    Some(&format!("@ {}", format_path_for_display(&path))),
                ),
                options.dry_run,
            );
        }
        let result = (|| {
            let repo = Repository::at(&path)?;
            // Repository::at honors inherited git context. Never let that
            // redirect a registry entry to the invoking repository.
            anyhow::ensure!(
                dunce::canonicalize(repo.repo_path()?)? == dunce::canonicalize(&path)?,
                "Git context points to a different repository"
            );
            if !options.dry_run {
                registry::register(&repo)?;
            }
            sweep_repository(&repo, &activity, options, now, remove, |row| {
                if matches!(format, SwitchFormat::Text) {
                    text_line(render_row(row), options.dry_run);
                }
            })
        })();
        match result {
            Ok(repo_rows) => rows.extend(repo_rows),
            Err(error) => {
                propagate_interrupt(&error)?;
                report_skipped(&path, &format!("{error:#}"));
            }
        }
    }
    if matches!(format, SwitchFormat::Json) {
        crate::output::print_json(&rows)?;
    }
    if !options.dry_run {
        attic::expire(now).context("Failed to expire cleanup attic entries")?;
    }
    eprintln!("{}", info_message(summary(&rows, options.dry_run)));
    Ok(())
}

fn sweep_repository(
    repo: &Repository,
    activity: &ActivitySnapshot,
    options: RemovalOptions,
    now: Timestamp,
    remove: &mut Remover<'_>,
    mut emit: impl FnMut(&CleanRow),
) -> anyhow::Result<Vec<CleanRow>> {
    let repo_path = repo.repo_path()?;
    let bare = repo.is_bare()?;
    let mut rows = Vec::new();
    let mut candidates = Vec::new();
    for worktree in repo.list_worktrees()? {
        // The main worktree is outside sweep's scope. Bare repositories have
        // only linked worktrees; protect them without default-branch detection.
        if !bare && worktree.path == repo_path {
            continue;
        }
        let mut row = CleanRow {
            repo: repo_path.into(),
            branch: worktree.branch.clone(),
            path: worktree.path.clone(),
            verdict: OutputVerdict::Kept,
            reason: String::new(),
            backup: None,
            attic: None,
        };
        if bare {
            row.reason = keep_reason(&KeepReason::Primary);
        } else {
            let decision = (|| {
                let busy = terminals::busy_for(&worktree.path, activity)?;
                let facts = facts::load(repo, worktree, busy, now)?;
                let verdict = policy::sweep(&facts, now)?;
                Ok::<_, anyhow::Error>((facts, verdict))
            })();
            match decision {
                Ok((_, Verdict::Keep(reason))) => row.reason = keep_reason(&reason),
                Ok((facts, verdict @ Verdict::Remove { .. })) => {
                    candidates.push((facts, verdict, row));
                    continue;
                }
                Ok((_, Verdict::Defer)) => {
                    anyhow::bail!("Sweep policy returned a deferred verdict")
                }
                Err(error) => {
                    propagate_interrupt(&error)?;
                    row.reason = format!("cannot inspect ({error:#})");
                }
            }
        }
        emit(&row);
        rows.push(row);
    }
    if !candidates.is_empty() {
        let snapshot = remote::fetch_snapshot(repo);
        if let Err(error) = &snapshot {
            propagate_interrupt(error)?;
        }
        for (facts, verdict, mut row) in candidates {
            match &snapshot {
                Ok(snapshot) => match remove(repo, &facts, snapshot, &verdict, options, now) {
                    Ok(result) => row = result,
                    Err(error) => {
                        propagate_interrupt(&error)?;
                        row.reason = keep_reason(&KeepReason::BackupFailed(format!("{error:#}")));
                    }
                },
                Err(error) => {
                    row.reason = keep_reason(&KeepReason::BackupFailed(format!("{error:#}")))
                }
            }
            emit(&row);
            rows.push(row);
        }
    }
    Ok(rows)
}

fn propagate_interrupt(error: &anyhow::Error) -> anyhow::Result<()> {
    if let Some(signal) = error.interrupt_signal() {
        return Err(WorktrunkError::Interrupted { signal, hint: None }.into());
    }
    Ok(())
}

fn keep_reason(reason: &KeepReason) -> String {
    match reason {
        KeepReason::Primary => "primary".into(),
        KeepReason::Pinned => "pinned".into(),
        KeepReason::InUse => "in use".into(),
        KeepReason::Recent => "recent".into(),
        KeepReason::Locked(reason) => format!("locked ({reason})"),
        KeepReason::BackupFailed(reason) => format!("backup failed ({reason})"),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn render_row(row: &CleanRow) -> String {
    let verdict = match row.verdict {
        OutputVerdict::Removed => "removed",
        OutputVerdict::Kept => "kept",
        OutputVerdict::Deferred => "deferred",
        OutputVerdict::WouldRemove => "would-remove",
    };
    let branch = one_line(row.branch.as_deref().unwrap_or("detached HEAD"));
    let path = one_line(&format_path_for_display(&row.path));
    let reason = one_line(&row.reason);
    let message = cformat!("<bold>{verdict}</> <bold>{branch}</> @ {path} ({reason})");
    if row.verdict == OutputVerdict::Removed {
        success_message(message).to_string()
    } else {
        info_message(message).to_string()
    }
}

fn text_line(line: String, dry_run: bool) {
    if dry_run {
        println!("{line}");
    } else {
        eprintln!("{line}");
    }
}

fn report_skipped(path: &Path, reason: &str) {
    eprintln!(
        "{}",
        warning_message(format!(
            "Skipped repository @ {} ({})",
            one_line(&format_path_for_display(path)),
            one_line(reason)
        ))
    );
}

fn summary(rows: &[CleanRow], dry_run: bool) -> String {
    let removed = rows
        .iter()
        .filter(|row| {
            matches!(
                row.verdict,
                OutputVerdict::Removed | OutputVerdict::WouldRemove
            )
        })
        .count();
    let kept = rows.len() - removed;
    let labels = [
        "primary",
        "pinned",
        "in use",
        "recent",
        "locked",
        "backup failed",
        "errors",
        "deferred",
    ];
    let mut counts = [0; 8];
    for row in rows
        .iter()
        .filter(|row| matches!(row.verdict, OutputVerdict::Kept | OutputVerdict::Deferred))
    {
        let index = if row.verdict == OutputVerdict::Deferred {
            7
        } else {
            match row.reason.as_str() {
                "primary" => 0,
                "pinned" => 1,
                "in use" => 2,
                "recent" => 3,
                reason if reason.starts_with("locked") => 4,
                reason if reason.starts_with("backup failed") => 5,
                _ => 6,
            }
        };
        counts[index] += 1;
    }
    let reasons = labels
        .iter()
        .zip(counts)
        .filter(|(_, count)| *count > 0)
        .map(|(label, count)| format!("{count} {label}"))
        .collect::<Vec<_>>()
        .join(", ");
    let action = if dry_run { "would-remove" } else { "removed" };
    let mut message = format!("{action} {removed}, kept {kept}");
    if !reasons.is_empty() {
        message.push_str(&format!(" ({reasons})"));
    }
    message
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, UNIX_EPOCH};

    use worktrunk::git::ErrorExt;
    use worktrunk::testing::{TEST_EPOCH, TestRepo};

    use super::*;
    use crate::clean::{HcTerminal, OutputVerdict, TerminalStatus, keep};

    const DAY: Duration = Duration::from_secs(86_400);

    fn now() -> Timestamp {
        UNIX_EPOCH + Duration::from_secs(TEST_EPOCH)
    }

    fn quiet() -> ActivitySnapshot {
        ActivitySnapshot {
            processes: vec![],
            terminals: vec![],
            unknown: false,
        }
    }

    fn age(repo: &Repository, path: &Path, elapsed: Duration) {
        let admin = repo.worktree_at(path).git_dir().unwrap();
        for file in ["index", "HEAD", "logs/HEAD"] {
            fs::File::options()
                .write(true)
                .open(admin.join(file))
                .unwrap()
                .set_modified(now() - elapsed)
                .unwrap();
        }
    }

    fn terminal(path: &Path, status: TerminalStatus, elapsed: Duration) -> HcTerminal {
        HcTerminal {
            address: path.file_name().unwrap().to_string_lossy().into_owned(),
            cwd: path.into(),
            status: Some(status),
            turn: 3,
            turn_changed_at: Some(now() - elapsed),
        }
    }

    fn removed(
        repo: &Repository,
        facts: &WorktreeFacts,
        _: &RemoteSnapshot,
        _: &Verdict,
        options: RemovalOptions,
        _: Timestamp,
    ) -> anyhow::Result<CleanRow> {
        Ok(CleanRow {
            repo: repo.repo_path()?.into(),
            branch: facts.branch.clone(),
            path: facts.path.clone(),
            verdict: if options.dry_run {
                OutputVerdict::WouldRemove
            } else {
                OutputVerdict::Removed
            },
            reason: "inactive".into(),
            backup: Some("on-remote".into()),
            attic: None,
        })
    }

    #[test]
    fn tests_that_clean_sweep_applies_the_table_and_hands_silent_terminals_to_removal() {
        let mut fixture = TestRepo::standard_main_only();
        let mut activity = quiet();
        for (name, elapsed) in [
            ("pinned", DAY * 8),
            ("silent", DAY * 7),
            ("busy", DAY * 2),
            ("requested", DAY / 2),
            ("recent", DAY / 2),
            ("old", DAY),
        ] {
            let path = fixture.add_worktree(name);
            if name == "pinned" {
                keep::set_pin(&fixture.repo, &path, true).unwrap();
            }
            if name == "requested" {
                keep::set_remove_requested(&fixture.repo, &path, true).unwrap();
            }
            if name == "silent" {
                activity
                    .terminals
                    .push(terminal(&path, TerminalStatus::Done, DAY * 7));
            }
            if name == "busy" {
                activity
                    .terminals
                    .push(terminal(&path, TerminalStatus::Working, DAY * 8));
            }
            age(&fixture.repo, &path, elapsed);
        }
        let mut handed = vec![];
        let rows = sweep_repository(
            &fixture.repo,
            &activity,
            RemovalOptions {
                dry_run: false,
                yes: true,
            },
            now(),
            &mut |repo, facts, snapshot, verdict, options, time| {
                let Verdict::Remove { terminals } = verdict else {
                    panic!("only removal verdicts reach the remover")
                };
                handed.extend(terminals.iter().map(|terminal| terminal.address.clone()));
                removed(repo, facts, snapshot, verdict, options, time)
            },
            |_| {},
        )
        .unwrap();
        let decisions: std::collections::BTreeMap<_, _> = rows
            .iter()
            .map(|row| {
                (
                    row.branch.as_deref().unwrap(),
                    (row.verdict, row.reason.as_str()),
                )
            })
            .collect();
        assert_eq!(
            decisions,
            std::collections::BTreeMap::from([
                ("pinned", (OutputVerdict::Kept, "pinned")),
                ("silent", (OutputVerdict::Removed, "inactive")),
                ("busy", (OutputVerdict::Kept, "in use")),
                ("requested", (OutputVerdict::Removed, "inactive")),
                ("recent", (OutputVerdict::Kept, "recent")),
                ("old", (OutputVerdict::Removed, "inactive")),
            ])
        );
        assert_eq!(handed.len(), 1);
        assert!(handed[0].ends_with("silent"));
        assert!(rows.iter().all(|row| row.path != fixture.root_path()));
    }

    #[test]
    fn tests_that_clean_sweep_keeps_unreadable_facts_and_continues_after_removal_failure() {
        let mut fixture = TestRepo::standard_main_only();
        for name in ["a-broken", "b-fails", "c-works"] {
            let path = fixture.add_worktree(name);
            age(&fixture.repo, &path, DAY * 2);
            if name == "a-broken" {
                fs::remove_file(path.join("file.txt")).unwrap();
            }
        }
        let rows = sweep_repository(
            &fixture.repo,
            &quiet(),
            RemovalOptions {
                dry_run: false,
                yes: false,
            },
            now(),
            &mut |repo, facts, snapshot, verdict, options, time| {
                if facts.branch.as_deref() == Some("b-fails") {
                    anyhow::bail!("remote denied backup");
                }
                removed(repo, facts, snapshot, verdict, options, time)
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].verdict, OutputVerdict::Kept);
        assert!(
            rows[0]
                .reason
                .contains("Failed to read changed file timestamp")
        );
        assert_eq!(rows[1].verdict, OutputVerdict::Kept);
        assert!(rows[1].reason.contains("remote denied backup"));
        assert_eq!(rows[2].verdict, OutputVerdict::Removed);
    }

    #[test]
    fn tests_that_clean_sweep_interrupts_abort_the_remaining_removals() {
        let mut fixture = TestRepo::standard_main_only();
        for name in ["a", "b"] {
            let path = fixture.add_worktree(name);
            age(&fixture.repo, &path, DAY * 2);
        }
        let mut calls = 0;
        let error = sweep_repository(
            &fixture.repo,
            &quiet(),
            RemovalOptions {
                dry_run: false,
                yes: false,
            },
            now(),
            &mut |_, _, _, _, _, _| {
                calls += 1;
                Err(worktrunk::git::WorktrunkError::ChildProcessExited {
                    code: 130,
                    signal: Some(2),
                    message: "interrupt".into(),
                }
                .into())
            },
            |_| {},
        )
        .unwrap_err();
        assert_eq!(error.interrupt_signal(), Some(2));
        assert_eq!(calls, 1);
    }

    #[test]
    fn tests_that_clean_sweep_prints_local_rows_before_one_remote_snapshot() {
        let mut fixture = TestRepo::standard_main_only();
        for name in ["a-old", "b-old", "z-recent"] {
            let path = fixture.add_worktree(name);
            age(
                &fixture.repo,
                &path,
                if name == "z-recent" { DAY / 2 } else { DAY * 2 },
            );
        }
        let remote = fixture.remote_path().unwrap();
        let hidden = fixture.home_path().join("hidden-remote");
        fs::rename(remote, &hidden).unwrap();
        let calls = Cell::new(0);
        let rows = sweep_repository(
            &fixture.repo,
            &quiet(),
            RemovalOptions {
                dry_run: false,
                yes: false,
            },
            now(),
            &mut |repo, facts, snapshot, verdict, options, time| {
                assert!(snapshot.refs.values().any(|sha| sha == &facts.tip_sha));
                if calls.get() == 0 {
                    fs::rename(remote, &hidden).unwrap();
                }
                calls.set(calls.get() + 1);
                removed(repo, facts, snapshot, verdict, options, time)
            },
            |row| {
                if row.branch.as_deref() == Some("z-recent") {
                    // An early fetch fails; after the first removal a second fetch
                    // fails. Both candidates therefore require the one right read.
                    fs::rename(&hidden, remote).unwrap();
                }
            },
        )
        .unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(
            rows.iter().map(|row| row.verdict).collect::<Vec<_>>(),
            [
                OutputVerdict::Kept,
                OutputVerdict::Removed,
                OutputVerdict::Removed
            ]
        );
    }

    #[test]
    fn tests_that_clean_sweep_dry_run_passes_read_only_options_to_the_remover() {
        let mut fixture = TestRepo::standard_main_only();
        let path = fixture.add_worktree("old");
        age(&fixture.repo, &path, DAY * 2);
        let rows = sweep_repository(
            &fixture.repo,
            &quiet(),
            RemovalOptions {
                dry_run: true,
                yes: true,
            },
            now(),
            &mut |repo, facts, snapshot, verdict, options, time| {
                assert!(options.dry_run && options.yes);
                removed(repo, facts, snapshot, verdict, options, time)
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(rows[0].verdict, OutputVerdict::WouldRemove);
        assert!(path.is_dir());
    }

    #[test]
    fn tests_that_clean_sweep_keeps_missing_remote_and_unknown_activity() {
        let mut fixture = TestRepo::with_initial_commit();
        let path = fixture.add_worktree("old");
        age(&fixture.repo, &path, DAY * 8);
        for unknown in [false, true] {
            let mut activity = quiet();
            activity.unknown = unknown;
            let rows = sweep_repository(
                &fixture.repo,
                &activity,
                RemovalOptions {
                    dry_run: true,
                    yes: false,
                },
                now(),
                &mut |_, _, _, _, _, _| panic!("unsafe removal"),
                |_| {},
            )
            .unwrap();
            assert_eq!(rows[0].verdict, OutputVerdict::Kept);
            assert_eq!(
                rows[0].reason,
                if unknown {
                    "in use"
                } else {
                    "backup failed (no remote: No remotes configured)"
                }
            );
        }
    }

    #[test]
    fn tests_that_clean_sweep_excludes_the_main_worktree_and_keeps_bare_repos_without_detection() {
        let fixture = TestRepo::standard_main_only();
        let options = RemovalOptions {
            dry_run: true,
            yes: false,
        };
        let rows = sweep_repository(
            &fixture.repo,
            &quiet(),
            options,
            now(),
            &mut |_, _, _, _, _, _| panic!("primary removal"),
            |_| {},
        )
        .unwrap();
        assert!(rows.is_empty());
        let bare = fixture.remote_path().unwrap();
        let linked = fixture.home_path().join("bare-linked");
        fixture.run_git_in(
            bare,
            &[
                "worktree",
                "add",
                "-b",
                "bare-linked",
                linked.to_str().unwrap(),
                "main",
            ],
        );
        let config = fs::read(bare.join("config")).unwrap();
        let rows = sweep_repository(
            &Repository::at(bare).unwrap(),
            &quiet(),
            options,
            now(),
            &mut |_, _, _, _, _, _| panic!("bare removal"),
            |_| {},
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verdict, OutputVerdict::Kept);
        assert_eq!(rows[0].reason, "primary");
        assert_eq!(fs::read(bare.join("config")).unwrap(), config);
    }

    #[test]
    fn tests_that_clean_sweep_summary_counts_results_and_groups_backup_errors() {
        use insta::assert_snapshot;
        let mut rows = vec![];
        for (verdict, reason, count) in [
            (OutputVerdict::Removed, "inactive", 31),
            (OutputVerdict::Kept, "in use", 4),
            (OutputVerdict::Kept, "recent", 1),
            (OutputVerdict::Kept, "backup failed (no remote)", 1),
        ] {
            rows.extend((0..count).map(|_| CleanRow {
                repo: "/repo".into(),
                branch: None,
                path: "/repo.linked".into(),
                verdict,
                reason: reason.into(),
                backup: None,
                attic: None,
            }));
        }
        assert_snapshot!(summary(&rows, false), @"removed 31, kept 6 (4 in use, 1 recent, 1 backup failed)");
        for row in &mut rows {
            if row.verdict == OutputVerdict::Removed {
                row.verdict = OutputVerdict::WouldRemove;
            }
        }
        assert_snapshot!(summary(&rows, true), @"would-remove 31, kept 6 (4 in use, 1 recent, 1 backup failed)");
    }
}
