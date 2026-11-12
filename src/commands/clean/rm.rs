//! Piece E: explicit removal, shared backup-then-delete path, and delayed requests.

use std::time::Duration;

use worktrunk::git::Repository;

use super::{CleanRow, RemoteSnapshot, RemovalOptions, Timestamp, Verdict, WorktreeFacts};

/// Resolve branches through Repository::resolve_worktree, defaulting to the
/// current worktree, and apply the rm policy. Back up busy requests before
/// deferring, honor delay and preview modes, and fail if any worktree is refused
/// or its backup fails. All work is still unimplemented in the skeleton.
pub(crate) fn run(
    _branches: Vec<String>,
    _after: Option<Duration>,
    _dry_run: bool,
    _format: crate::cli::SwitchFormat,
    _yes: bool,
) -> anyhow::Result<()> {
    anyhow::bail!("wt clean rm is not implemented yet")
}

// TODO(clean): used by pieces E and F.
/// Execute a verdict through remote backup, local attic backup, and exact
/// tip/status recheck before existing forced removal and local branch deletion.
/// Respect non-factory locks and approved remove-hook plans, log each removal,
/// and end terminals only for the silent-seven-days verdict. Preview writes nothing.
pub fn remove_one(
    _repo: &Repository,
    _facts: &WorktreeFacts,
    _snapshot: &RemoteSnapshot,
    _verdict: &Verdict,
    _options: RemovalOptions,
    _now: Timestamp,
) -> anyhow::Result<CleanRow> {
    anyhow::bail!("wt clean removal is not implemented yet")
}

// TODO(clean): used by piece E, never during dry-run.
/// Start a detached waiter and return immediately. Once the delay elapses,
/// remove through the shared safe path only if the worktree is not busy and
/// has not been touched since this request; otherwise do nothing.
pub fn schedule(
    _repo: &Repository,
    _facts: &WorktreeFacts,
    _after: Duration,
    _options: RemovalOptions,
    _now: Timestamp,
) -> anyhow::Result<()> {
    anyhow::bail!("wt clean delayed removal is not implemented yet")
}
