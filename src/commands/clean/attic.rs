//! Piece C: preserve uncommitted work locally, restore it, and expire old entries.

use std::path::Path;

use worktrunk::git::Repository;

use super::{AtticEntry, BackupResult, Timestamp, WorktreeFacts};

// TODO(clean): used by pieces E and F.
/// Save tracked.patch, untracked paths, differing ignored .env* files, and
/// metadata in one local attic entry when needed. Preserve relative paths and
/// never upload attic content; return None when there is nothing to save.
pub fn save(
    _repo: &Repository,
    _facts: &WorktreeFacts,
    _backup: &BackupResult,
    _now: Timestamp,
) -> anyhow::Result<Option<AtticEntry>> {
    anyhow::bail!("wt clean attic backup is not implemented yet")
}

// TODO(clean): used by piece C's restore command.
/// Load the newest complete attic entry for this repo and branch, preserving
/// enough metadata to restore into a newly created worktree. Absence means no
/// saved uncommitted work; malformed or unreadable entries remain errors.
pub fn load_latest(_repo: &Repository, _branch: &str) -> anyhow::Result<Option<AtticEntry>> {
    anyhow::bail!("wt clean attic lookup is not implemented yet")
}

// TODO(clean): used by piece C's restore command.
/// Re-apply an attic entry's tracked patch, untracked files, and ignored env
/// files to the restored worktree. Fail without silently overwriting existing
/// user data, and retain the attic entry if restoration fails.
pub fn apply(_entry: &AtticEntry, _worktree: &Path) -> anyhow::Result<()> {
    anyhow::bail!("wt clean attic restore is not implemented yet")
}

// TODO(clean): used by piece F, never during dry-run.
/// Remove attic entries older than 30 days relative to the supplied time and
/// return their count. Only attic entries are eligible; failures must not turn
/// into broader filesystem cleanup or removal of unrelated paths.
pub fn expire(_now: Timestamp) -> anyhow::Result<usize> {
    anyhow::bail!("wt clean attic expiry is not implemented yet")
}
