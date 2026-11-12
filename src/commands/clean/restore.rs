//! Piece C: restore a removed worktree and its newest local attic entry.

/// Recreate a worktree from the local branch, then the normal remote branch,
/// then the fetched wt-backup ref, in that order. Apply the latest attic entry
/// if present and print the path; never retarget an existing worktree.
pub fn run(_branch: &str, _yes: bool) -> anyhow::Result<()> {
    anyhow::bail!("wt clean restore is not implemented yet")
}
