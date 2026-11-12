//! Piece D: user-level repo registry and bounded root discovery for all-repo sweeps.

use std::path::PathBuf;

use worktrunk::git::Repository;

// TODO(clean): used by pieces C, D, E, and F, except dry runs.
/// Register the current repo in user-level state whenever a clean command
/// runs. Preserve existing entries and use the repo's atomic-write conventions;
/// dry runs consult the registry without registering or otherwise changing it.
pub fn register(_repo: &Repository) -> anyhow::Result<()> {
    anyhow::bail!("wt clean repo registration is not implemented yet")
}

// TODO(clean): used by piece F.
/// Select the current repo, or with all_repos combine registered repos and
/// owners of linked worktrees found at most two folders below each root.
/// Deduplicate repos; unreachable primary worktrees are skipped and reported,
/// never interpreted as safe filesystem cleanup targets.
pub fn load_repositories(
    _current: &Repository,
    _all_repos: bool,
    _roots: &[PathBuf],
) -> anyhow::Result<Vec<PathBuf>> {
    anyhow::bail!("wt clean repo discovery is not implemented yet")
}
