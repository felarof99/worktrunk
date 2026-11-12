//! Piece A: collect one worktree's facts without making cleanup decisions.

use worktrunk::git::{Repository, WorktreeInfo};

use super::{BusyState, Timestamp, WorktreeFacts};

// TODO(clean): used by pieces E and F.
/// Load primary/bare status, pins, removal marks, lock, tip, exact porcelain
/// status, and last touch from admin files and changed/untracked paths. Attach
/// the supplied activity observation; failures must preserve the worktree.
pub fn load(
    _repo: &Repository,
    _worktree: &WorktreeInfo,
    _busy: BusyState,
    _now: Timestamp,
) -> anyhow::Result<WorktreeFacts> {
    anyhow::bail!("wt clean fact collection is not implemented yet")
}
