//! Piece D: pins and remove-requested marks shared by rm and sweep.

use std::path::Path;

use worktrunk::git::Repository;

use super::KeepState;

/// Resolve the optional branch through Repository::resolve_worktree (current
/// worktree when omitted), then pin it or clear the pin with --off. Register
/// the repo and preserve unrelated state when writing the mark.
pub fn run(_branch: Option<&str>, _off: bool) -> anyhow::Result<()> {
    anyhow::bail!("wt clean keep is not implemented yet")
}

// TODO(clean): used by piece A.
/// Load a worktree's pin and remove-requested mark without writing anything.
/// Absent marks mean false; unreadable state is an error rather than permission
/// to remove a possibly pinned worktree.
pub fn load_state(_repo: &Repository, _worktree: &Path) -> anyhow::Result<KeepState> {
    anyhow::bail!("wt clean mark lookup is not implemented yet")
}

// TODO(clean): used by piece D's keep command.
/// Set or clear this worktree's pin while preserving other worktrees' marks
/// and this worktree's remove-requested state. The pin protects both rm and
/// sweep, including sweep's silent-seven-days rule.
pub fn set_pin(_repo: &Repository, _worktree: &Path, _pinned: bool) -> anyhow::Result<()> {
    anyhow::bail!("wt clean pin update is not implemented yet")
}

// TODO(clean): used by pieces E and F.
/// Set the removal mark only after backing up a busy rm request, or clear it
/// after completed removal. Preserve pins and unrelated worktree state; this
/// operation must never run as part of a dry-run preview.
pub fn set_remove_requested(
    _repo: &Repository,
    _worktree: &Path,
    _requested: bool,
) -> anyhow::Result<()> {
    anyhow::bail!("wt clean removal mark update is not implemented yet")
}
