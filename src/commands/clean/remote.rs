//! Establish that each tip is backed up on the primary remote.
//!
//! The caller captures one advertisement after its first local progress output.
//! Only exact advertised SHAs or locally proven default-branch ancestry count;
//! a missing object is never fetched to answer this question. Pushes use the
//! captured tip, only in `refs/heads/wt-backup/`, and require a fresh exact-SHA
//! confirmation. Failed reads, pushes, and confirmations never authorize removal.
//! Local replacement refs and grafts cannot supply ancestry evidence. Git's
//! automatic tag and submodule pushes cannot widen the backup namespace.
//! Advertised symbolic backup refs are refused: pushing to one follows its
//! destination and can overwrite a normal branch even with an explicit refspec.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, bail};
use worktrunk::git::{CommandError, ErrorExt, Repository, WorktrunkError};
use worktrunk::shell_exec::Cmd;

use super::{BackupResult, RemoteSnapshot, WorktreeFacts};

#[cfg(test)]
#[path = "../../../tests/integration_tests/clean_remote.rs"]
mod clean_remote;

// Match repository default-branch detection for reads. Uploading commits needs
// more time, but a stalled backup must still return with the worktree intact.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const PUSH_TIMEOUT: Duration = Duration::from_secs(60);

fn git_command(repo: &Repository) -> Cmd {
    Cmd::new("git")
        .current_dir(repo.discovery_path())
        .context(repo.discovery_path().to_string_lossy())
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env(
            "GIT_GRAFT_FILE",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
}

fn remote_command(repo: &Repository, args: &[&str], timeout: Duration) -> anyhow::Result<String> {
    let output = git_command(repo)
        .args(args.iter().copied())
        .timeout(timeout)
        .run()
        .with_context(|| format!("Could not run git {}", args[0]))?;
    if !output.status.success() {
        return Err(CommandError::from_failed_output("git", args, &output).into());
    }
    String::from_utf8(output.stdout).context("Remote advertisement is not UTF-8")
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_snapshot(remote: String, advertisement: &str) -> anyhow::Result<RemoteSnapshot> {
    let mut refs = BTreeMap::new();
    let mut default_branch_ref = None;
    for line in advertisement.lines() {
        let (value, reference) = line
            .split_once('\t')
            .context("Malformed remote advertisement")?;
        if let Some(symref) = value.strip_prefix("ref: ") {
            if reference.starts_with("refs/heads/wt-backup/") {
                bail!(
                    "Backup ref {reference} is symbolic; replace it with a direct ref before cleanup"
                );
            }
            if reference == "HEAD" && symref.starts_with("refs/heads/") {
                default_branch_ref = Some(symref.to_string());
            }
        } else if is_object_id(value) {
            refs.insert(reference.to_string(), value.to_string());
        } else {
            bail!("Malformed object ID in remote advertisement");
        }
    }
    Ok(RemoteSnapshot {
        remote,
        refs,
        default_branch_ref,
    })
}

/// Select the repo's primary remote and capture its advertised refs once per
/// run. No remote fails with "no remote"; discovery must not block the first
/// local progress output. Later confirmations query only the pushed backup ref.
pub fn fetch_snapshot(repo: &Repository) -> anyhow::Result<RemoteSnapshot> {
    let remote = repo.primary_remote().context("no remote")?;
    let advertisement = remote_command(
        repo,
        &["ls-remote", "--symref", "--", &remote],
        READ_TIMEOUT,
    )
    .with_context(|| format!("Could not read remote {remote}; check remote access and retry"))?;
    parse_snapshot(remote, &advertisement)
}

/// Test whether the exact tip is advertised, or is an ancestor of the advertised
/// default-branch tip whose object is local. This reads only the snapshot and
/// local git objects; it never fetches an object to prove ancestry.
pub fn tip_on_remote(
    repo: &Repository,
    facts: &WorktreeFacts,
    snapshot: &RemoteSnapshot,
) -> anyhow::Result<bool> {
    if !is_object_id(&facts.tip_sha) {
        bail!("Invalid worktree tip; read the worktree again");
    }
    if snapshot.refs.values().any(|sha| sha == &facts.tip_sha) {
        return Ok(true);
    }
    let Some(default_tip) = snapshot
        .default_branch_ref
        .as_ref()
        .and_then(|reference| snapshot.refs.get(reference))
    else {
        return Ok(false);
    };
    if !is_object_id(default_tip) {
        bail!("Invalid remote default-branch tip");
    }
    let commit = format!("{default_tip}^{{commit}}");
    let args = ["cat-file", "-e", &commit];
    let local = git_command(repo)
        .args(args)
        .run()
        .context("Could not check the local default-branch object")?;
    if !local.status.success() {
        if local.status.code().is_none() {
            return Err(CommandError::from_failed_output("git", &args, &local).into());
        }
        return Ok(false);
    }
    let args = ["merge-base", "--is-ancestor", &facts.tip_sha, default_tip];
    let ancestry = git_command(repo)
        .args(args)
        .run()
        .context("Could not check commit ancestry")?;
    match ancestry.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(CommandError::from_failed_output("git", &args, &ancestry).into()),
    }
}

/// Confirm an already-advertised tip or force-push it to the branch's wt-backup
/// ref and verify that ref with ls-remote. Detached heads use their worktree
/// folder name and first twelve tip SHA characters, so equal folder names do
/// not overwrite different commits. Push or confirmation failure prevents deletion.
pub fn backup(
    repo: &Repository,
    facts: &WorktreeFacts,
    snapshot: &RemoteSnapshot,
) -> anyhow::Result<BackupResult> {
    if tip_on_remote(repo, facts, snapshot)? {
        return Ok(BackupResult::OnRemote);
    }
    let reference = match backup_reference(repo, facts) {
        Ok(reference) => reference,
        Err(error) => {
            if let Some(signal) = error.interrupt_signal() {
                return Err(WorktrunkError::Interrupted { signal, hint: None }.into());
            }
            return Ok(BackupResult::Failed {
                reason: error.to_string(),
            });
        }
    };
    let refspec = format!("{}:{reference}", facts.tip_sha);
    if let Err(error) = remote_command(
        repo,
        &[
            "push",
            "--force",
            "--no-follow-tags",
            "--recurse-submodules=no",
            "--",
            &snapshot.remote,
            &refspec,
        ],
        PUSH_TIMEOUT,
    ) {
        return failed_operation(error, "push", &snapshot.remote, PUSH_TIMEOUT);
    }
    let confirmed = match remote_command(
        repo,
        &["ls-remote", "--", &snapshot.remote, &reference],
        READ_TIMEOUT,
    )
    .and_then(|advertisement| parse_snapshot(snapshot.remote.clone(), &advertisement))
    {
        Ok(confirmed) => confirmed,
        Err(error) => {
            return failed_operation(error, "confirmation", &snapshot.remote, READ_TIMEOUT);
        }
    };
    if confirmed.refs.get(&reference) != Some(&facts.tip_sha) {
        return Ok(BackupResult::Failed {
            reason: format!(
                "Backup confirmation for {reference} did not match the worktree tip; retry after other pushes finish"
            ),
        });
    }
    Ok(BackupResult::Pushed { reference })
}

/// Resolve and validate the exact push destination for both backup and preview.
pub(super) fn backup_reference(repo: &Repository, facts: &WorktreeFacts) -> anyhow::Result<String> {
    if !is_object_id(&facts.tip_sha) {
        bail!("Invalid worktree tip; read the worktree again");
    }
    let branch = match &facts.branch {
        Some(branch) => branch.clone(),
        None => {
            let folder = facts
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .context("Detached worktree has no UTF-8 folder name")?;
            format!("detached-{folder}-{}", &facts.tip_sha[..12])
        }
    };
    let reference = format!("refs/heads/wt-backup/{branch}");
    if !repo.run_command_check(&["check-ref-format", &reference])? {
        bail!("Invalid backup branch name; rename the branch or detached worktree folder");
    }
    Ok(reference)
}

fn failed_operation(
    error: anyhow::Error,
    operation: &str,
    remote: &str,
    timeout: Duration,
) -> anyhow::Result<BackupResult> {
    if let Some(signal) = error.interrupt_signal() {
        return Err(WorktrunkError::Interrupted { signal, hint: None }.into());
    }
    let timed_out = error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::TimedOut)
    });
    let reason = if timed_out {
        format!(
            "Backup {operation} to {remote} timed out after {}s; check the connection and retry",
            timeout.as_secs()
        )
    } else {
        format!(
            "Backup {operation} to {remote} failed; check remote access, write permissions, and server hooks"
        )
    };
    Ok(BackupResult::Failed { reason })
}
