//! Collect one worktree's facts without making cleanup decisions.
//!
//! Status and HEAD are read afresh, with optional git locks disabled so the
//! observation cannot refresh the index's mtime. Preserve the ordinary porcelain
//! bytes for backup comparisons; a second NUL-delimited, fully expanded status
//! supplies filesystem paths, including individual untracked files. If ordinary
//! status hides untracked files, fail rather than provide an incomplete backup
//! comparison. Deleted files have no trustworthy timestamp, so they and other
//! failed reads abort collection, never manufacture a clean or old worktree.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use worktrunk::git::{CommandError, Repository, WorkingTree, WorktreeInfo};
use worktrunk::path::format_path_for_display;

use super::{BusyState, KeepState, Timestamp, WorktreeFacts};

/// Load primary/bare status, pins, removal marks, lock, tip, exact porcelain
/// status, and last touch from admin files and changed/untracked paths. Attach
/// the supplied activity observation; failures must preserve the worktree.
pub fn load(
    repo: &Repository,
    worktree: &WorktreeInfo,
    busy: BusyState,
    _now: Timestamp,
) -> anyhow::Result<WorktreeFacts> {
    load_with_keep_state(
        repo,
        worktree,
        busy,
        super::keep::load_state(repo, &worktree.path)?,
    )
}

/// Collect independently of the persistent marks owned by `keep`.
pub(super) fn load_with_keep_state(
    repo: &Repository,
    worktree: &WorktreeInfo,
    busy: BusyState,
    state: KeepState,
) -> anyhow::Result<WorktreeFacts> {
    let wt = repo.worktree_at(&worktree.path);
    let admin = wt.git_dir()?;
    let bare = repo.is_bare()?;
    let primary = match repo.primary_worktree()? {
        Some(primary) => dunce::canonicalize(primary)? == dunce::canonicalize(&worktree.path)?,
        None => false,
    };
    let tip_sha = wt.run_command(&["rev-parse", "--verify", "HEAD"])?;
    let tip_sha = tip_sha.trim().to_owned();
    anyhow::ensure!(!tip_sha.is_empty(), "Worktree has no HEAD commit");
    let lock_reason = match fs::read_to_string(admin.join("locked")) {
        Ok(reason) => Some(reason.trim_end_matches('\n').to_owned()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).context("Failed to read worktree lock"),
    };

    let mut last_touch = modified(&admin.join("HEAD"))?;
    let (status_porcelain, dirty) = if worktree.bare {
        // A bare git directory has neither an index nor working-tree status.
        (Vec::new(), false)
    } else {
        for name in ["index", "logs/HEAD"] {
            last_touch = last_touch.max(modified(&admin.join(name))?);
        }
        let status = status_bytes(&wt, &["--no-optional-locks", "status", "--porcelain"])?;
        let paths = status_bytes(
            &wt,
            &[
                "--no-optional-locks",
                "status",
                "--porcelain",
                "-z",
                "-uall",
            ],
        )?;
        let paths = changed_paths(&paths)?;
        anyhow::ensure!(
            !paths.iter().any(|p| p.untracked)
                || status
                    .split(|b| *b == b'\n')
                    .any(|line| line.starts_with(b"?? ")),
            "Git status omitted untracked files; cannot verify cleanup"
        );
        let dirty = !status.is_empty() || !paths.is_empty();
        for changed in paths {
            let path = worktree.path.join(changed.path);
            let time = fs::symlink_metadata(&path)
                .and_then(|metadata| metadata.modified())
                .with_context(|| {
                    format!(
                        "Failed to read changed file timestamp @ {}",
                        format_path_for_display(&path)
                    )
                })?;
            last_touch = last_touch.max(time);
        }
        (status, dirty)
    };
    Ok(WorktreeFacts {
        path: worktree.path.clone(),
        branch: worktree.branch.clone(),
        primary,
        bare,
        pinned: state.pinned,
        remove_requested: state.remove_requested,
        lock_reason,
        busy,
        last_touch,
        dirty,
        tip_sha,
        status_porcelain,
    })
}

fn modified(path: &Path) -> anyhow::Result<Timestamp> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .with_context(|| {
            format!(
                "Failed to read worktree timestamp @ {}",
                format_path_for_display(path)
            )
        })
}

fn status_bytes(wt: &WorkingTree<'_>, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = wt.run_command_output(args)?;
    if !output.status.success() {
        return Err(CommandError::from_failed_output("git", args, &output).into());
    }
    Ok(output.stdout)
}

struct ChangedPath {
    path: PathBuf,
    untracked: bool,
}

/// Preserve non-UTF-8 filenames on Unix. Rename sources are historical names;
/// timestamp their destinations, failing if the destination has since vanished.
fn changed_paths(status: &[u8]) -> anyhow::Result<Vec<ChangedPath>> {
    anyhow::ensure!(
        status.is_empty() || status.ends_with(&[0]),
        "Incomplete git status"
    );
    let mut entries = status.split(|b| *b == 0);
    let mut paths = Vec::new();
    while let Some(entry) = entries.next().filter(|entry| !entry.is_empty()) {
        anyhow::ensure!(
            entry.len() > 3 && entry[2] == b' ',
            "Invalid git status record"
        );
        let name = &entry[3..];
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(std::ffi::OsStr::from_bytes(name))
        };
        #[cfg(not(unix))]
        let path = PathBuf::from(std::str::from_utf8(name).context("Invalid git status path")?);
        anyhow::ensure!(
            path.is_relative()
                && path
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "Invalid git status path"
        );
        paths.push(ChangedPath {
            path,
            untracked: entry.starts_with(b"?? "),
        });
        if entry[..2].iter().any(|b| matches!(b, b'R' | b'C')) {
            anyhow::ensure!(
                entries.next().is_some_and(|source| !source.is_empty()),
                "Missing rename source in git status"
            );
        }
    }
    Ok(paths)
}
