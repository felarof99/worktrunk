//! Piece D: pure cleanup verdicts with caller-supplied time and no I/O.

use super::{Timestamp, Verdict, WorktreeFacts};

// TODO(clean): used by piece F.
/// Apply the sweep table in order: primary/bare, pinned, silent seven days,
/// busy, remove-requested, recent, otherwise remove. Only the silent rule may
/// nominate terminals to end; locks and backup gates still protect removal.
pub fn sweep(_facts: &WorktreeFacts, _now: Timestamp) -> anyhow::Result<Verdict> {
    anyhow::bail!("wt clean sweep policy is not implemented yet")
}

// TODO(clean): used by piece E.
/// Apply the explicit-rm table: refuse primary and pinned worktrees, defer
/// busy worktrees after backup, otherwise remove. The caller's process counts
/// as busy; refusal reasons become rm failures. This function never backs up.
pub fn rm(_facts: &WorktreeFacts, _now: Timestamp) -> anyhow::Result<Verdict> {
    anyhow::bail!("wt clean rm policy is not implemented yet")
}
