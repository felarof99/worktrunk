//! Piece B: establish that each tip is backed up on the primary remote.

use worktrunk::git::Repository;

use super::{BackupResult, RemoteSnapshot, WorktreeFacts};

// TODO(clean): used by pieces E and F.
/// Select the repo's primary remote and capture its advertised refs once per
/// run. No remote fails with "no remote"; discovery must not block the first
/// local progress output. Later confirmations query only the pushed backup ref.
pub fn fetch_snapshot(_repo: &Repository) -> anyhow::Result<RemoteSnapshot> {
    anyhow::bail!("wt clean remote discovery is not implemented yet")
}

// TODO(clean): used by piece B and dry-run paths in pieces E and F.
/// Test whether the exact tip is advertised, or is an ancestor of the advertised
/// default-branch tip whose object is local. This reads only the snapshot and
/// local git objects; it never fetches an object to prove ancestry.
pub fn tip_on_remote(
    _repo: &Repository,
    _facts: &WorktreeFacts,
    _snapshot: &RemoteSnapshot,
) -> anyhow::Result<bool> {
    anyhow::bail!("wt clean remote tip check is not implemented yet")
}

// TODO(clean): used by pieces E and F, never during dry-run.
/// Confirm an already-advertised tip or force-push it to the branch's wt-backup
/// ref and verify that ref with ls-remote. Detached heads use their worktree
/// folder name. Any push or confirmation failure prevents local deletion.
pub fn backup(
    _repo: &Repository,
    _facts: &WorktreeFacts,
    _snapshot: &RemoteSnapshot,
) -> anyhow::Result<BackupResult> {
    anyhow::bail!("wt clean remote backup is not implemented yet")
}
