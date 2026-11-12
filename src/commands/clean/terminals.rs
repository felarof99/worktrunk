//! Piece A: observe processes and optional hc terminals, including turn history.

use std::path::Path;

use super::{ActivitySnapshot, BusyState, HcTerminal, Timestamp};

// TODO(clean): used by pieces E and F.
/// Load live process working directories and `hc ls --json` launch directories,
/// joining terminals with their stored turn history at the supplied time. With
/// no hc on PATH, terminals are empty; unknown process activity stays busy.
/// A dry run may read history but must not update it.
pub fn load(_now: Timestamp, _dry_run: bool) -> anyhow::Result<ActivitySnapshot> {
    anyhow::bail!("wt clean activity collection is not implemented yet")
}

// TODO(clean): used by pieces A, E, and F.
/// Select processes whose cwd and live terminals whose launch cwd lie inside
/// this worktree. This is a pure path-membership operation on an observation;
/// unknown activity must remain unknown rather than become an empty busy set.
pub fn busy_for(_path: &Path, _activity: &ActivitySnapshot) -> anyhow::Result<BusyState> {
    anyhow::bail!("wt clean activity matching is not implemented yet")
}

// TODO(clean): used by pieces E and F.
/// End only terminals selected by the silent-seven-days sweep verdict, with
/// `hc kill` followed by `hc rm`. A working terminal must never be ended;
/// callers must retain that protection when observations become stale.
pub fn end(_terminals: &[HcTerminal]) -> anyhow::Result<()> {
    anyhow::bail!("wt clean terminal cleanup is not implemented yet")
}
