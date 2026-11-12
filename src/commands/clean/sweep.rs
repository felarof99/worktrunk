//! Piece F: orchestrate single-repo and all-repo cleanup using the shared pieces.

use std::path::PathBuf;

/// Enumerate selected repos and linked worktrees, apply the ordered pure sweep
/// policy, and use rm's shared backup-then-delete path. Report unreachable
/// primary worktrees, expire attic entries older than 30 days outside dry-run,
/// and treat kept worktrees as normal results rather than sweep failures.
pub(crate) fn run(
    _all_repos: bool,
    _roots: Vec<PathBuf>,
    _dry_run: bool,
    _format: crate::cli::SwitchFormat,
    _yes: bool,
) -> anyhow::Result<()> {
    anyhow::bail!("wt clean sweep is not implemented yet")
}
