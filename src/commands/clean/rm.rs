//! Explicit cleanup and the single backup-then-delete path shared with sweep.
//!
//! A run observes activity once and lazily shares one remote advertisement.
//! Waiting and the final deletion gate take fresh observations. Hook selection
//! is frozen before execution; pre-remove hooks run before the final gate, so
//! a hook cannot invalidate the backup and still authorize forced deletion.
//! The gate compares live content with both the initial snapshot and the actual
//! saved patch/files (including primary env copies omitted from the attic).
//! File stamps bracket hashing and the slow activity probe: same-status edits
//! during either interval keep the worktree, even if its bytes are restored.
//! Directory stamps include empty and ignored directories, where a new env
//! file would change neither porcelain nor an ancestor directory's timestamp.
//! Synchronous Repository::remove_worktree (the existing Git removal fallback)
//! retains Git's lock/submodule/error checks at the original path, without the
//! trash staging fast path moving the folder before Git can reject removal.
//!
//! Delayed requests re-exec this binary through spawn_detached_exec, rooted
//! outside the requested worktree. Their private stdin protocol records the
//! original branch, tip, status and precise last-touch time. The shell binds
//! the protocol marker to its PID before exec: nested wt invocations cannot
//! inherit waiter mode. Background hooks use read-only approvals unless -y
//! was supplied. Every actual deletion still goes through remove_one.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use color_print::cformat;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use worktrunk::config::{Approvals, UserConfig};
use worktrunk::git::{CommandError, ErrorExt, HookType, Repository, WorktreeInfo, WorktrunkError};
use worktrunk::path::format_path_for_display;
use worktrunk::shell_exec::Cmd;
use worktrunk::styling::{
    eprintln, hint_message, info_message, println, progress_message, success_message,
    warning_message,
};

use crate::cli::SwitchFormat;
use crate::commands::command_executor::{CommandContext, FailureStrategy};
use crate::commands::hook_plan::{
    ApprovedHookPlan, HookPlanBuilder, execute_planned_hook, register_planned,
};
use crate::commands::hooks::HookAnnouncer;
#[cfg(unix)]
use crate::commands::process::{HookLog, InternalOp, spawn_detached_exec};
use crate::commands::template_vars::TemplateVars;
use crate::commands::worktree::hooks::PostRemoveContext;

use super::{
    BackupResult, CleanRow, KeepReason, OutputVerdict, RemoteSnapshot, RemovalOptions, Timestamp,
    Verdict, WorktreeFacts, attic, facts, keep, policy, registry, remote, terminals,
};

const DEFERRED: &str = "goes when nothing runs in it";
const WAITER_PID: &str = "WORKTRUNK_CLEAN_WAITER_PID";

fn now() -> Timestamp {
    UNIX_EPOCH + Duration::from_secs(worktrunk::utils::epoch_now())
}

fn is_waiter() -> bool {
    std::env::var(WAITER_PID).is_ok_and(|pid| pid == std::process::id().to_string())
}

/// Resolve branch-first selectors and share observations across all requests.
pub(crate) fn run(
    branches: Vec<String>,
    after: Option<Duration>,
    wait: Option<Duration>,
    dry_run: bool,
    format: SwitchFormat,
    yes: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        after.is_none() || wait.is_none(),
        "--after and --wait cannot be used together"
    );
    if is_waiter() {
        return run_waiter();
    }
    // Also precedes default-branch discovery in bare repositories.
    eprintln!("{}", progress_message("Checking worktrees..."));
    let repo = Repository::current()?;
    let branches = if branches.is_empty() {
        vec!["@".into()]
    } else {
        branches
    };
    let mut paths = Vec::new();
    for branch in branches {
        let path = repo.require_worktree(&branch)?;
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    if !dry_run {
        registry::register(&repo)?;
    }
    let now = now();
    let activity = terminals::load(now, dry_run)?;
    let options = RemovalOptions { dry_run, yes };
    let mut snapshot: Option<Result<RemoteSnapshot, String>> = None;
    let mut rows = Vec::new();
    for path in paths {
        let worktree = find_worktree(&repo, &path)?;
        let mut row = row_for(&repo, &path, worktree.branch.clone())?;
        let result = (|| {
            let mut facts = facts::load(
                &repo,
                &worktree,
                terminals::busy_for(&path, &activity)?,
                now,
            )?;
            let mut verdict = policy::rm(&facts, now)?;
            if let Verdict::Keep(reason) = &verdict {
                row.reason = keep_reason(reason);
            } else if let Some(after) = after {
                if dry_run {
                    row.verdict = OutputVerdict::WouldRemove;
                    row.reason =
                        format!("would schedule after {}", humantime::format_duration(after));
                } else {
                    schedule(&repo, &facts, after, options, now)?;
                    row.verdict = OutputVerdict::Deferred;
                    row.reason = format!("scheduled after {}", humantime::format_duration(after));
                }
            } else {
                if let Some(wait) = wait
                    && !dry_run
                    && matches!(verdict, Verdict::Defer)
                {
                    facts = wait_until_idle(&repo, &path, wait)?;
                    verdict = policy::rm(&facts, now)?;
                }
                if let Some(preview) = preview_or_keep(&repo, &facts, &verdict, dry_run)? {
                    row = preview;
                } else {
                    if snapshot.is_none() {
                        eprintln!("{}", progress_message("Checking remote backup..."));
                        snapshot = Some(match remote::fetch_snapshot(&repo) {
                            Ok(snapshot) => Ok(snapshot),
                            Err(error) => Err(error_reason(error)?),
                        });
                    }
                    match snapshot.as_ref().context("Missing remote snapshot")? {
                        Ok(snapshot) => {
                            row = remove_one(&repo, &facts, snapshot, &verdict, options, now)?
                        }
                        Err(reason) => row.reason = format!("backup failed ({reason})"),
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(error) = result {
            row.reason = error_reason(error)?;
        }
        if format == SwitchFormat::Text {
            print_row(&row, dry_run);
        }
        rows.push(row);
    }
    if format == SwitchFormat::Json {
        crate::output::print_json(&rows)?;
    }
    if rows.iter().any(|row| row.verdict == OutputVerdict::Kept) {
        // Rows already carry every failure, including in JSON mode.
        anyhow::bail!("");
    }
    Ok(())
}

fn row_for(repo: &Repository, path: &Path, branch: Option<String>) -> anyhow::Result<CleanRow> {
    Ok(CleanRow {
        repo: repo.repo_path()?.to_path_buf(),
        branch,
        path: path.to_path_buf(),
        verdict: OutputVerdict::Kept,
        reason: String::new(),
        backup: None,
        attic: None,
    })
}

fn keep_reason(reason: &KeepReason) -> String {
    match reason {
        KeepReason::Primary => "primary".into(),
        KeepReason::Pinned => "pinned".into(),
        KeepReason::InUse => "in use".into(),
        KeepReason::Recent => "recent".into(),
        KeepReason::Locked(_) => "locked".into(),
        KeepReason::BackupFailed(reason) => format!("backup failed ({reason})"),
    }
}

fn factory_lock(reason: &str) -> bool {
    reason.starts_with("hc factory run") || reason.starts_with("herdr-factory run")
}

fn preview_or_keep(
    repo: &Repository,
    facts: &WorktreeFacts,
    verdict: &Verdict,
    dry_run: bool,
) -> anyhow::Result<Option<CleanRow>> {
    let mut row = row_for(repo, &facts.path, facts.branch.clone())?;
    if let Verdict::Keep(reason) = verdict {
        row.reason = keep_reason(reason);
    } else if facts.primary || facts.pinned {
        row.reason = if facts.primary { "primary" } else { "pinned" }.into();
    } else if facts
        .lock_reason
        .as_deref()
        .is_some_and(|reason| !factory_lock(reason))
    {
        row.reason = "locked".into();
    } else if dry_run {
        (row.verdict, row.reason) = match verdict {
            Verdict::Defer => (OutputVerdict::Deferred, DEFERRED.into()),
            Verdict::Remove { .. } => (OutputVerdict::WouldRemove, "backup before removal".into()),
            Verdict::Keep(_) => unreachable!(),
        };
    } else {
        return Ok(None);
    }
    Ok(Some(row))
}

fn find_worktree(repo: &Repository, path: &Path) -> anyhow::Result<WorktreeInfo> {
    repo.list_worktrees()?
        .iter()
        .find(|worktree| worktree.path == path)
        .cloned()
        .context("Worktree is no longer registered")
}

fn observe(repo: &Repository, path: &Path) -> anyhow::Result<WorktreeFacts> {
    // Refresh topology and branch identities, rather than the planner's cache.
    let fresh = Repository::at(repo.repo_path()?)?;
    let activity = terminals::load(now(), true)?;
    facts::load(
        &fresh,
        &find_worktree(&fresh, path)?,
        terminals::busy_for(path, &activity)?,
        now(),
    )
}

fn wait_until_idle(
    repo: &Repository,
    path: &Path,
    wait: Duration,
) -> anyhow::Result<WorktreeFacts> {
    let start = Instant::now();
    loop {
        let facts = observe(repo, path)?;
        if !matches!(policy::rm(&facts, now())?, Verdict::Defer) || start.elapsed() >= wait {
            return Ok(facts);
        }
        std::thread::sleep(Duration::from_millis(100).min(wait.saturating_sub(start.elapsed())));
    }
}

/// One verdict in, one row out. Failed backups or safety gates never delete.
pub fn remove_one(
    repo: &Repository,
    facts: &WorktreeFacts,
    snapshot: &RemoteSnapshot,
    verdict: &Verdict,
    options: RemovalOptions,
    now: Timestamp,
) -> anyhow::Result<CleanRow> {
    if let Some(row) = preview_or_keep(repo, facts, verdict, options.dry_run)? {
        return Ok(row);
    }
    let mut row = row_for(repo, &facts.path, facts.branch.clone())?;
    let content = match content_snapshot(facts, ContentSource::Worktree) {
        Ok(content) => content,
        Err(error) => {
            row.reason = format!("backup failed ({})", error_reason(error)?);
            return Ok(row);
        }
    };
    let backup = match remote::backup(repo, facts, snapshot) {
        Ok(BackupResult::Failed { reason }) => {
            row.reason = format!("backup failed ({reason})");
            return Ok(row);
        }
        Ok(backup) => backup,
        Err(error) => {
            row.reason = format!("backup failed ({})", error_reason(error)?);
            return Ok(row);
        }
    };
    row.backup = Some(match &backup {
        BackupResult::OnRemote => {
            eprintln!(
                "{}",
                info_message(cformat!("On <bold>{}</>", snapshot.remote))
            );
            "on-remote".into()
        }
        BackupResult::Pushed { reference } => {
            eprintln!(
                "{}",
                success_message(cformat!("Pushed to <bold>{reference}</> (verified)"))
            );
            reference.clone()
        }
        BackupResult::Failed { .. } => unreachable!(),
    });
    match attic::save(repo, facts, &backup, now) {
        Ok(entry) => {
            row.attic = entry.map(|entry| entry.path);
            if let Some(path) = &row.attic {
                eprintln!(
                    "{}",
                    success_message(format!(
                        "Saved local changes @ {}",
                        format_path_for_display(path)
                    ))
                );
            }
        }
        Err(error) => {
            row.reason = format!("backup failed ({})", error_reason(error)?);
            return Ok(row);
        }
    }
    if matches!(verdict, Verdict::Defer) {
        keep::set_remove_requested(repo, &facts.path, true)?;
        row.verdict = OutputVerdict::Deferred;
        row.reason = DEFERRED.into();
        return Ok(row);
    }
    let result = delete_backed_up(repo, facts, verdict, options, now, &content.hash, &mut row);
    if let Err(error) = result {
        row.reason = error_reason(error)?;
    }
    Ok(row)
}

fn git_bytes(path: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = Cmd::new("git")
        .args(args.iter().copied())
        .current_dir(path)
        .context(path.display().to_string())
        .scrub_git_discovery_env()
        .env("GIT_OPTIONAL_LOCKS", "0")
        .run()?;
    if !output.status.success() {
        return Err(CommandError::from_failed_output("git", args, &output).into());
    }
    Ok(output.stdout)
}

enum ContentSource<'a> {
    Worktree,
    Backup {
        attic: Option<&'a Path>,
        primary: &'a Path,
    },
}

#[derive(PartialEq, Eq)]
struct FileStamp {
    kind: fs::FileType,
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64, u32),
}

fn file_stamp(path: &Path) -> std::io::Result<Option<FileStamp>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(FileStamp {
        kind: metadata.file_type(),
        len: metadata.len(),
        modified: metadata.modified()?,
        #[cfg(unix)]
        identity: {
            use std::os::unix::fs::MetadataExt;
            (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
                metadata.mode(),
            )
        },
    }))
}

struct ContentSnapshot {
    hash: [u8; 32],
    stamps: BTreeMap<PathBuf, Option<FileStamp>>,
}

impl ContentSnapshot {
    fn unchanged(&self) -> bool {
        self.stamps
            .iter()
            .all(|(path, stamp)| file_stamp(path).is_ok_and(|current| current == *stamp))
    }
}

fn watch_path(
    stamps: &mut BTreeMap<PathBuf, Option<FileStamp>>,
    path: &Path,
    root: &Path,
) -> anyhow::Result<()> {
    for ancestor in path.ancestors() {
        if let std::collections::btree_map::Entry::Vacant(entry) = stamps.entry(ancestor.into()) {
            entry.insert(file_stamp(ancestor)?);
        }
        if ancestor == root {
            break;
        }
    }
    Ok(())
}

fn git_path(name: &[u8]) -> anyhow::Result<&Path> {
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStrExt;
        Path::new(std::ffi::OsStr::from_bytes(name))
    };
    #[cfg(not(unix))]
    let path = Path::new(std::str::from_utf8(name)?);
    anyhow::ensure!(
        path.components()
            .all(|component| matches!(component, std::path::Component::Normal(_))),
        "Invalid backup file path"
    );
    Ok(path)
}

/// Porcelain names states, not contents: M stays M across an edit, and ?? dir/
/// hides additions inside that directory. Hash the attic's canonical patch and
/// loose-file contents; saved snapshots substitute the actual preserved files.
/// Keep stamps for a cheap recheck after activity inspection, so large loose
/// files need no second read between the busy gate and deletion.
fn content_snapshot(
    facts: &WorktreeFacts,
    source: ContentSource<'_>,
) -> anyhow::Result<ContentSnapshot> {
    let mut stamps = BTreeMap::new();
    watch_path(&mut stamps, &facts.path, &facts.path)?;
    let head = git_bytes(&facts.path, &["rev-parse", "--verify", "HEAD"])?;
    let status = git_bytes(&facts.path, &["status", "--porcelain"])?;
    anyhow::ensure!(
        std::str::from_utf8(&head)?.trim() == facts.tip_sha && status == facts.status_porcelain,
        "changed during backup"
    );
    let mut hash = Sha256::new();
    let patch = match source {
        ContentSource::Worktree => {
            // Git's file inventory omits empty directories. Watch every real
            // directory before reading content, without following symlinks or
            // entering Git's administrative directory.
            for entry in ignore::WalkBuilder::new(&facts.path)
                .standard_filters(false)
                .follow_links(false)
                .filter_entry(|entry| entry.depth() == 0 || entry.file_name() != ".git")
                .build()
            {
                let entry = entry.context("Failed to inspect worktree directories")?;
                if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                    watch_path(&mut stamps, entry.path(), &facts.path)?;
                }
            }
            let tracked = git_bytes(&facts.path, &["ls-files", "--cached", "-z"])?;
            for name in tracked
                .split(|byte| *byte == 0)
                .filter(|name| !name.is_empty())
            {
                watch_path(&mut stamps, &facts.path.join(git_path(name)?), &facts.path)?;
            }
            git_bytes(
                &facts.path,
                &[
                    "diff",
                    "--binary",
                    "--unified=3",
                    "--no-color",
                    "--src-prefix=a/",
                    "--dst-prefix=b/",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--ignore-submodules=none",
                    "--submodule=short",
                    "HEAD",
                    "--",
                ],
            )?
        }
        ContentSource::Backup {
            attic: Some(attic), ..
        } => {
            let path = attic.join("tracked.patch");
            watch_path(&mut stamps, &path, attic)?;
            fs::read(path).context("changed during backup")?
        }
        ContentSource::Backup { attic: None, .. } => Vec::new(),
    };
    hash.update((patch.len() as u64).to_le_bytes());
    hash.update(patch);
    for ignored in [false, true] {
        let mut args = vec!["ls-files", "--others", "--exclude-standard", "-z"];
        if ignored {
            args.push("--ignored");
        }
        let paths = git_bytes(&facts.path, &args)?;
        for name in paths
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
        {
            let path = git_path(name)?;
            // Directory stamps also catch additions beneath collapsed ?? dirs
            // and new env files among otherwise ignored build outputs.
            let live = facts.path.join(path);
            watch_path(
                &mut stamps,
                live.parent().context("Missing backup file parent")?,
                &facts.path,
            )?;
            if ignored
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".env"))
            {
                continue;
            }
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name);
            let (path, root) = match source {
                ContentSource::Worktree => (live, facts.path.as_path()),
                ContentSource::Backup { attic, primary } => {
                    let saved = attic.map(|attic| {
                        attic
                            .join(if ignored { "ignored-env" } else { "untracked" })
                            .join(path)
                    });
                    match saved {
                        Some(saved) if fs::symlink_metadata(&saved).is_ok() => {
                            (saved, attic.context("Missing attic path")?)
                        }
                        _ if ignored => (primary.join(path), primary),
                        _ => anyhow::bail!("changed during backup"),
                    }
                }
            };
            watch_path(&mut stamps, &path, root)?;
            let metadata = fs::symlink_metadata(&path)?;
            hash.update(metadata.len().to_le_bytes());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                hash.update(metadata.permissions().mode().to_le_bytes());
            }
            if metadata.file_type().is_symlink() {
                hash.update(fs::read_link(&path)?.as_os_str().as_encoded_bytes());
            } else {
                anyhow::ensure!(metadata.is_file(), "Cannot verify backup of a non-file");
                let mut file = fs::File::open(&path)?;
                let mut buffer = [0; 64 * 1024];
                loop {
                    let len = file.read(&mut buffer)?;
                    if len == 0 {
                        break;
                    }
                    hash.update(&buffer[..len]);
                }
            }
        }
    }
    let snapshot = ContentSnapshot {
        hash: hash.finalize().into(),
        stamps,
    };
    anyhow::ensure!(snapshot.unchanged(), "changed during backup");
    Ok(snapshot)
}

fn approve_hooks(
    repo: &Repository,
    facts: &WorktreeFacts,
    config: &UserConfig,
    yes: bool,
) -> anyhow::Result<ApprovedHookPlan> {
    let project = repo.load_project_config()?;
    let project_id = repo.project_identifier().ok();
    let mut builder = HookPlanBuilder::new(project.as_ref(), config, project_id.as_deref());
    builder.add(&facts.path, &[HookType::PreRemove, HookType::PostRemove]);
    let plan = builder.finish();
    if is_waiter() && !yes {
        return Ok(plan.approve_readonly(&Approvals::load()?, project_id.as_deref()));
    }
    match plan.approve(project_id.as_deref(), yes)? {
        Some(plan) => Ok(plan),
        None => {
            eprintln!(
                "{}",
                info_message("Commands declined, continuing removal without hooks")
            );
            Ok(ApprovedHookPlan::empty())
        }
    }
}

fn delete_backed_up(
    repo: &Repository,
    facts: &WorktreeFacts,
    verdict: &Verdict,
    options: RemovalOptions,
    now: Timestamp,
    content: &[u8; 32],
    row: &mut CleanRow,
) -> anyhow::Result<()> {
    let config = UserConfig::load()?;
    let plan = approve_hooks(repo, facts, &config, options.yes)?;
    let primary = repo
        .primary_worktree()?
        .context("Primary worktree is unavailable")?;
    let stable = Repository::at(&primary)?;
    let target_branch = stable.current_worktree().branch()?.unwrap_or_default();
    let vars = TemplateVars::new()
        .with_target(&target_branch)
        .with_target_worktree_path(&primary);
    let pre_repo = Repository::at(&facts.path)?;
    let context = CommandContext::new(
        &pre_repo,
        &config,
        facts.branch.as_deref(),
        &facts.path,
        false,
    );
    execute_planned_hook(
        &plan,
        &facts.path,
        &context,
        HookType::PreRemove,
        &vars.as_extra_vars(),
        FailureStrategy::FailFast,
        crate::output::pre_hook_display_path(&facts.path),
    )?;
    if let Verdict::Remove { terminals } = verdict {
        terminals::end(terminals)?;
    }
    let live = content_snapshot(facts, ContentSource::Worktree)?;
    let saved = content_snapshot(
        facts,
        ContentSource::Backup {
            attic: row.attic.as_deref(),
            primary: &primary,
        },
    )?;
    let fresh = observe(&stable, &facts.path)?;
    if fresh.tip_sha != facts.tip_sha
        || fresh.status_porcelain != facts.status_porcelain
        || fresh.branch != facts.branch
        || live.hash != *content
        || saved.hash != *content
        || (is_waiter() && fresh.last_touch > facts.last_touch)
    {
        row.reason = "changed during backup".into();
        return Ok(());
    }
    if fresh.primary || fresh.pinned {
        row.reason = if fresh.primary { "primary" } else { "pinned" }.into();
        return Ok(());
    }
    if fresh
        .lock_reason
        .as_deref()
        .is_some_and(|reason| !factory_lock(reason))
    {
        row.reason = "locked".into();
        return Ok(());
    }
    // The policy type carries terminals to end, but no reason discriminant.
    // Only a seven-day sweep can remove an initially busy worktree. Even then
    // new processes and terminals left alive by end() keep the worktree.
    let silent = now
        .duration_since(facts.last_touch)
        .is_ok_and(|age| age >= Duration::from_secs(7 * 24 * 60 * 60))
        && matches!(policy::sweep(facts, now)?, Verdict::Remove { .. });
    let new_process = fresh.busy.processes.iter().any(|process| {
        !silent
            || !facts
                .busy
                .processes
                .iter()
                .any(|old| old.pid == process.pid)
    });
    if fresh.busy.unknown || !fresh.busy.terminals.is_empty() || new_process {
        row.reason = "in use".into();
        return Ok(());
    }
    if let Some(branch) = &facts.branch {
        anyhow::ensure!(
            !stable
                .list_worktrees()?
                .iter()
                .any(|w| w.path != facts.path && w.branch.as_ref() == Some(branch)),
            "Local branch is checked out in another worktree"
        );
    }
    // An unwritable log must fail before deletion.
    let mut log = open_log(&stable)?;
    stable
        .worktree_at(&facts.path)
        .ensure_holds_this_worktree()?;
    if fresh.lock_reason.is_some() {
        stable.run_command(&[
            "worktree",
            "unlock",
            facts.path.to_str().context("Invalid worktree path")?,
        ])?;
    }
    worktrunk::git::remove::stop_fsmonitor_daemon(&stable.worktree_at(&facts.path));
    // No slow probe or content read between this stamp check and removal.
    // A write during hashing, hooks or activity inspection keeps the folder.
    anyhow::ensure!(
        live.unchanged() && saved.unchanged(),
        "changed during backup"
    );
    stable.remove_worktree(&facts.path, true)?;
    row.verdict = OutputVerdict::Removed;
    row.reason = "backed up".into();
    if let Some(branch) = &facts.branch {
        // Forced means no integration check; CAS still protects a new commit
        // published during removal. Never delete an unbacked-up tip.
        let reference = format!("refs/heads/{branch}");
        let deletion = (|| {
            let fresh = Repository::at(&primary)?;
            anyhow::ensure!(
                !fresh
                    .list_worktrees()?
                    .iter()
                    .any(|worktree| worktree.branch.as_ref() == Some(branch)),
                "Local branch is checked out in another worktree"
            );
            stable.run_command(&["update-ref", "--no-deref", "-d", &reference, &facts.tip_sha])?;
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(error) = deletion {
            row.verdict = OutputVerdict::Kept;
            row.reason = format!(
                "worktree removed; local branch kept ({})",
                error_reason(error)?
            );
            write_log(&mut log, row)?;
            return Ok(());
        }
    }
    keep::set_remove_requested(&stable, &facts.path, false)?;
    write_log(&mut log, row)?;
    let removed_branch = facts.branch.as_deref().unwrap_or("HEAD");
    let vars = PostRemoveContext::new(&facts.path, Some(&facts.tip_sha), &primary, &stable);
    let context = CommandContext::new(&stable, &config, Some(removed_branch), &primary, false);
    let mut announcer = HookAnnouncer::new(&stable, false);
    register_planned(
        &mut announcer,
        &plan,
        &facts.path,
        &context,
        HookType::PostRemove,
        &vars.extra_vars(removed_branch),
        crate::output::pre_hook_display_path(&primary),
    )?;
    announcer.flush()?;
    Ok(())
}

fn error_reason(error: anyhow::Error) -> anyhow::Result<String> {
    if let Some(signal) = error.interrupt_signal() {
        return Err(WorktrunkError::Interrupted { signal, hint: None }.into());
    }
    log::debug!("Cleanup failed: {error:#}");
    Ok(error.to_string())
}

fn print_row(row: &CleanRow, dry_run: bool) {
    let path = format_path_for_display(&row.path);
    let message = match row.verdict {
        OutputVerdict::Removed => success_message(format!("Removed worktree @ {path}")),
        OutputVerdict::Kept => warning_message(format!("Kept worktree @ {path}: {}", row.reason)),
        OutputVerdict::Deferred => {
            info_message(format!("Deferred worktree @ {path}: {}", row.reason))
        }
        OutputVerdict::WouldRemove => {
            info_message(format!("Would remove worktree @ {path}: {}", row.reason))
        }
    };
    if dry_run {
        println!("{message}");
    } else {
        eprintln!("{message}");
    }
    if row.verdict == OutputVerdict::Removed
        && let Some(branch) = &row.branch
    {
        eprintln!(
            "{}",
            success_message(cformat!("Deleted local branch <bold>{branch}</>"))
        );
    }
    if row.reason == "pinned" {
        let selector = row
            .branch
            .clone()
            .unwrap_or_else(|| row.path.to_string_lossy().into_owned());
        let command = worktrunk::styling::suggest_command("clean keep", &[&selector], &["--off"]);
        eprintln!(
            "{}",
            hint_message(cformat!(
                "To unpin the worktree, run <underline>{command}</>"
            ))
        );
    }
}

fn open_log(repo: &Repository) -> anyhow::Result<fs::File> {
    let directory = repo.wt_logs_dir();
    fs::create_dir_all(&directory)?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("clean.jsonl"))
        .context("Failed to open cleanup log")
}

fn write_log(log: &mut fs::File, row: &CleanRow) -> anyhow::Result<()> {
    let mut line = serde_json::to_vec(row)?;
    line.push(b'\n');
    log.write_all(&line).context("Failed to write cleanup log")
}

#[derive(Serialize, Deserialize)]
struct DelayedRequest {
    path: PathBuf,
    branch: Option<String>,
    tip_sha: String,
    status: Vec<u8>,
    last_touch: Timestamp,
    source: PathBuf,
    after: Duration,
    yes: bool,
}

/// Spawn in the repository's permanent directory, never in the worktree.
#[cfg(unix)]
pub fn schedule(
    repo: &Repository,
    facts: &WorktreeFacts,
    after: Duration,
    options: RemovalOptions,
    _now: Timestamp,
) -> anyhow::Result<()> {
    if options.dry_run {
        return Ok(());
    }
    anyhow::ensure!(
        !facts.primary && !facts.pinned,
        "Cannot schedule a primary or pinned worktree"
    );
    let request = DelayedRequest {
        path: facts.path.clone(),
        branch: facts.branch.clone(),
        tip_sha: facts.tip_sha.clone(),
        status: facts.status_porcelain.clone(),
        last_touch: facts.last_touch,
        source: repo.discovery_path().to_path_buf(),
        after,
        yes: options.yes,
    };
    let root = repo.repo_path()?;
    anyhow::ensure!(
        !root.starts_with(&facts.path),
        "Waiter must run outside the removed worktree"
    );
    let executable = std::env::current_exe()?;
    let command = format!(
        "unset {}; export {WAITER_PID}=$$; exec {} clean rm",
        worktrunk::shell_exec::INHERITED_GIT_PATH_VARS.join(" "),
        shell_escape::unix::escape(executable.to_string_lossy()),
    );
    spawn_detached_exec(
        repo,
        root,
        Path::new("sh"),
        &["-c", &command],
        facts.branch.as_deref().unwrap_or("detached"),
        &HookLog::internal(InternalOp::Remove),
        &serde_json::to_vec(&request)?,
    )?;
    Ok(())
}

#[cfg(not(unix))]
pub fn schedule(
    _repo: &Repository,
    _facts: &WorktreeFacts,
    _after: Duration,
    options: RemovalOptions,
    _now: Timestamp,
) -> anyhow::Result<()> {
    if options.dry_run {
        return Ok(());
    }
    anyhow::bail!("Delayed cleanup is unavailable without process working-directory inspection")
}

fn run_waiter() -> anyhow::Result<()> {
    let request: DelayedRequest = serde_json::from_reader(std::io::stdin().lock())
        .context("Invalid delayed cleanup request")?;
    std::thread::sleep(request.after);
    let repo = Repository::current()?;
    let mut row = row_for(&repo, &request.path, request.branch.clone())?;
    let result = (|| {
        let facts = observe(&repo, &request.path)?;
        if facts.branch != request.branch
            || facts.tip_sha != request.tip_sha
            || facts.status_porcelain != request.status
            || facts.last_touch > request.last_touch
        {
            row.reason = "touched since request".into();
        } else {
            let verdict = policy::rm(&facts, now())?;
            match verdict {
                Verdict::Keep(reason) => row.reason = keep_reason(&reason),
                Verdict::Defer => row.reason = "in use".into(),
                Verdict::Remove { .. } => {
                    eprintln!("{}", progress_message("Checking remote backup..."));
                    let snapshot = remote::fetch_snapshot(&repo)?;
                    let source = Repository::at(&request.source)?;
                    row = remove_one(
                        &source,
                        &facts,
                        &snapshot,
                        &verdict,
                        RemovalOptions {
                            dry_run: false,
                            yes: request.yes,
                        },
                        now(),
                    )?;
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    })();
    if let Err(error) = result {
        row.reason = error_reason(error)?;
    }
    // remove_one already logs a completed deletion. Skipped or failed delayed
    // jobs also need a durable explanation, even with no terminal watching.
    if row.verdict != OutputVerdict::Removed {
        write_log(&mut open_log(&repo)?, &row)?;
    }
    print_row(&row, false);
    Ok(())
}
