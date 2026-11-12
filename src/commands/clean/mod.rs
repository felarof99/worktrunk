//! Back up worktrees before cleaning them up. This is the shared contract for
//! pieces A–F; the skeleton implements only CLI dispatch and error stubs. No
//! stub reads hc, writes state, pushes, backs up, or removes anything.
//!
//! # Addressing and observations
//!
//! `wt clean rm [BRANCH]... [--after DURATION] [--dry-run] [--format text|json]`,
//! `sweep [--all-repos] [--root DIR]... [--dry-run] [--format text|json]`,
//! `keep [BRANCH] [--off]`, and `restore <BRANCH>` form the fixed CLI. Global
//! `-C` and `-y` retain their usual meaning. Worktree names go through
//! `Repository::resolve_worktree`: branch first, worktree path as an alias;
//! omitted optional branches mean the current worktree. Restore can recreate
//! a missing worktree. Never retarget an existing worktree to another branch.
//!
//! Busy means a live process has its cwd inside the worktree, or a live hc
//! terminal was launched inside it. The process calling `rm` counts. Unknown
//! process activity keeps the worktree, including on unsupported platforms;
//! the silent-seven-days rule must not bypass a failed activity inspection.
//! Last touch is the newest mtime among the worktree admin files `index`,
//! `HEAD`, `logs/HEAD`, and every changed or untracked file from `git status`.
//! A newly created worktree has a recent last touch. Silent for 7 days means
//! last touch is at least 7 days old AND every terminal launched inside is
//! not `working` and has not advanced its turn in 7 days. Times preserve file
//! mtimes' subsecond precision, so a delayed request can detect a touch within
//! the same second. Callers pass `now` (production clock reads use
//! `utils::epoch_now`). Pure policy functions never read a clock or perform I/O.
//!
//! # Verdict tables
//!
//! Sweep considers each linked worktree, first matching row wins:
//!
//! | Condition | Verdict |
//! |---|---|
//! | Primary worktree, or repo is bare | Keep: primary |
//! | Pinned by `clean keep` | Keep: pinned |
//! | Silent for at least 7 days | Remove; end its hc terminals first |
//! | Busy | Keep: in use |
//! | Remove-requested by an earlier busy `clean rm` | Remove |
//! | Last touch less than 24 hours ago | Keep: recent |
//! | Otherwise | Remove |
//!
//! Explicit rm considers each requested worktree, first matching row wins:
//!
//! | Condition | Verdict |
//! |---|---|
//! | Primary worktree | Refuse |
//! | Pinned | Refuse; hint `wt clean keep --off` |
//! | Busy | Back up now, mark remove-requested, defer successfully |
//! | Otherwise | Remove now |
//!
//! A deferred removal means "goes when nothing runs in it". `rm` exits 0 if
//! every worktree was removed or deferred, 1 for any refusal or failed backup.
//! `sweep` exits 0 unless the sweep itself broke; keeping a worktree is normal.
//! PR merge status is never a policy input. Any remove verdict remains subject
//! to the lock and backup gates below; backup failure becomes Keep:
//! `backup failed (<why>)`. Non-factory locks become Keep: locked.
//!
//! # Backup, then delete
//!
//! 1. Select the primary remote through existing repository discovery. No
//!    remote fails the backup with "no remote".
//! 2. Capture one `git ls-remote <remote>` snapshot per repo per run. A tip is
//!    backed up if any advertised ref has its sha, or it is an ancestor of the
//!    advertised default-branch tip AND that object is local. Otherwise push
//!    `git push --force <remote> <tip>:refs/heads/wt-backup/<branch>` and confirm
//!    with `git ls-remote <remote> refs/heads/wt-backup/<branch>`. Detached HEAD
//!    uses `wt-backup/detached-<worktree folder name>`.
//! 3. If `git status --porcelain` is nonempty, or an ignored `.env*` differs
//!    from the primary worktree's copy (including no copy there), save one
//!    local attic entry under
//!    `~/.wt-attic/<repo name>/<branch, slashes as dashes>/<UTC timestamp>/`:
//!    `tracked.patch` from `git diff --binary HEAD`, `untracked/` preserving
//!    relative paths, `ignored-env/` for those env files, and `meta.json`
//!    containing repo, branch, tip sha, original path, time, and backup ref.
//!    Nothing in the attic is uploaded.
//! 4. Immediately before deletion, compare the tip sha AND the exact
//!    `git status --porcelain` output against the backed-up snapshot. If either
//!    changed, stop with "changed during backup". Any failure in steps 1–4
//!    means nothing is deleted.
//!
//! Only lock reasons starting with `hc factory run` or `herdr-factory run`
//! permit unlocking. Any other lock keeps the worktree. After backup and
//! recheck, use existing worktrunk removal machinery with force (content was
//! saved), then force-delete the local branch (tip confirmed on remote), and
//! write one log line per removal. Remove hooks run exactly as `wt remove`
//! runs them, including approval before mutation and immutable approved hook
//! plans. No direct `rm -rf` fallback; a git failure never means safe to delete.
//!
//! `--after` returns immediately and spawns a detached waiter. After the delay
//! it removes only if the worktree is not busy and has not been touched since
//! the request; otherwise it does nothing. `--dry-run` prints every verdict
//! and reason without any changes (including marks, registry/history writes,
//! terminal termination, delayed jobs, or attic expiry). It may `ls-remote`
//! but never pushes. Network detail must not gate the first terminal output.
//!
//! # hc, registry, attic, and restore
//!
//! hc is optional: absent from PATH means no terminals. `hc ls --json` returns
//! an array with `address`, launch `cwd`, optional status (`working`, `blocked`,
//! `done`, `idle`), and `turn`. User-level history stores each address's last
//! observed turn and when it changed. A terminal first seen today has no
//! seven-day history. Only sweep's silent-seven-days rule may end terminals,
//! using `hc kill <address>` then `hc rm <address>`; never end a working one.
//!
//! Every clean command registers its repo in user-level state. `--all-repos`
//! covers that registry plus repos owning linked worktrees discovered up to
//! two folders below each `--root`. An unreachable primary worktree is skipped
//! and reported. A sweep expires attic entries older than 30 days.
//!
//! Restore recreates a worktree from its local branch if present, else from
//! `<remote>/<branch>`, else from `<remote>/wt-backup/<branch>` (fetch only
//! that ref), re-applies the newest attic entry if any, and prints the path.
//! User-owned state and backup data must not be silently overwritten; follow
//! the repository's data-safety and atomic-write rules.
//!
//! `--format json` emits one array of [`CleanRow`]: `repo`, nullable `branch`,
//! `path`, `verdict` (`removed`, `kept`, `deferred`, `would-remove`), `reason`,
//! `backup` (`on-remote`, the pushed ref, or null), and `attic` (path or null).
//!
//! # File ownership
//!
//! | File | Owner | Responsibility |
//! |---|---|---|
//! | `src/cli/clean.rs` | skeleton | clap surface and help |
//! | `src/commands/clean/mod.rs` | skeleton | this contract, shared types, dispatch |
//! | `facts.rs` | A | last touch, dirty status, lock, tip, worktree facts |
//! | `terminals.rs` | A | processes, hc terminals, turn history |
//! | `remote.rs` | B | remote snapshot, tip presence, push and confirmation |
//! | `attic.rs` | C | save, load, apply, and expire attic entries |
//! | `restore.rs` | C | restore command |
//! | `policy.rs` | D | pure verdict tables |
//! | `keep.rs` | D | pins, remove-requested marks, keep command |
//! | `registry.rs` | D | registry and root discovery |
//! | `rm.rs` | E | rm command, shared remove-one path, delay |
//! | `sweep.rs` | F | sweep command |
//!
//! Each piece owns `tests/integration_tests/clean_<piece>.rs`. Tests use temp
//! repos, local bare remotes, and fake hc on PATH; never real hc, gh, or pushes.
//! External commands use `shell_exec::Cmd`. Unix-only inspection and helpers
//! need matching cfg gates and a conservative fallback on Windows/Linux.

pub mod attic;
pub mod facts;
pub mod keep;
pub mod policy;
pub mod registry;
pub mod remote;
pub mod restore;
pub mod rm;
pub mod sweep;
pub mod terminals;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cli::CleanCommand;

/// An absolute time, preserving filesystem precision and supplied by callers.
pub type Timestamp = std::time::SystemTime;

// TODO(clean): used by pieces A, D, E, and F.
/// Observed facts for one worktree, including the exact backup recheck inputs.
#[derive(Debug, Clone)]
pub struct WorktreeFacts {
    pub path: PathBuf,
    /// None denotes detached HEAD.
    pub branch: Option<String>,
    pub primary: bool,
    pub bare: bool,
    pub pinned: bool,
    pub remove_requested: bool,
    pub lock_reason: Option<String>,
    pub busy: BusyState,
    pub last_touch: Timestamp,
    pub dirty: bool,
    pub tip_sha: String,
    /// Exact `git status --porcelain` bytes captured for backup and recheck.
    pub status_porcelain: Vec<u8>,
}

// TODO(clean): used by pieces A, D, E, and F.
/// Processes and live terminals inside a worktree; unknown activity means busy.
#[derive(Debug, Clone)]
pub struct BusyState {
    pub processes: Vec<ProcessInfo>,
    pub terminals: Vec<HcTerminal>,
    pub unknown: bool,
}

// TODO(clean): used by piece A.
/// One process's identity and observed working directory.
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub cwd: PathBuf,
}

// TODO(clean): used by pieces A, D, E, and F.
/// One live hc terminal; cwd is its launch directory, not a later shell cwd.
#[derive(Debug, Clone)]
pub struct HcTerminal {
    pub address: String,
    pub cwd: PathBuf,
    pub status: Option<TerminalStatus>,
    pub turn: u64,
    /// None means no known history; it cannot establish seven days of silence.
    pub turn_changed_at: Option<Timestamp>,
}

// TODO(clean): used by pieces A and D.
/// Status values in the hc protocol; missing status stays None on the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalStatus {
    Working,
    Blocked,
    Done,
    Idle,
}

// TODO(clean): used by piece A.
/// A run's process and terminal observations before filtering by worktree path.
#[derive(Debug, Clone)]
pub struct ActivitySnapshot {
    pub processes: Vec<ProcessInfo>,
    pub terminals: Vec<HcTerminal>,
    pub unknown: bool,
}

// TODO(clean): used by pieces D, E, and F.
/// A policy decision; Remove still requires backup and the final safety checks.
#[derive(Debug, Clone)]
pub enum Verdict {
    Keep(KeepReason),
    Remove { terminals: Vec<HcTerminal> },
    Defer,
}

// TODO(clean): used by pieces D, E, and F.
/// Reasons a worktree is kept; rm treats these as refusals rather than success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepReason {
    Primary,
    Pinned,
    InUse,
    Recent,
    Locked(String),
    BackupFailed(String),
}

// TODO(clean): used by pieces B, C, E, and F.
/// Outcome of confirming the tip on a remote, independent of the local attic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupResult {
    OnRemote,
    Pushed { reference: String },
    Failed { reason: String },
}

// TODO(clean): used by pieces B, E, and F.
/// One remote advertisement, keyed by full ref name; values are object IDs.
#[derive(Debug, Clone)]
pub struct RemoteSnapshot {
    pub remote: String,
    pub refs: BTreeMap<String, String>,
    /// Full advertised default-branch ref, when known.
    pub default_branch_ref: Option<String>,
}

// TODO(clean): used by pieces C, E, and F.
/// An attic entry's directory and metadata; its files never leave this machine.
#[derive(Debug, Clone)]
pub struct AtticEntry {
    pub path: PathBuf,
    pub metadata: AtticMetadata,
}

// TODO(clean): used by piece C.
/// Metadata stored with tracked.patch, untracked/, and ignored-env/ in the attic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtticMetadata {
    pub repo: PathBuf,
    pub branch: Option<String>,
    pub tip_sha: String,
    pub worktree_path: PathBuf,
    pub time: Timestamp,
    pub backup_ref: Option<String>,
}

// TODO(clean): used by pieces A and D.
/// Persistent cleanup marks loaded for a worktree.
#[derive(Debug, Clone, Copy)]
pub struct KeepState {
    pub pinned: bool,
    pub remove_requested: bool,
}

// TODO(clean): used by pieces E and F.
/// Shared removal controls; yes governs hook approval, never backup safety.
#[derive(Debug, Clone, Copy)]
pub struct RemovalOptions {
    pub dry_run: bool,
    pub yes: bool,
}

// TODO(clean): used by pieces E and F.
/// One item in the clean JSON array; optional fields serialize as explicit null.
#[derive(Debug, Clone, Serialize)]
pub struct CleanRow {
    pub repo: PathBuf,
    pub branch: Option<String>,
    pub path: PathBuf,
    pub verdict: OutputVerdict,
    pub reason: String,
    /// `on-remote`, the confirmed pushed ref, or None.
    pub backup: Option<String>,
    pub attic: Option<PathBuf>,
}

// TODO(clean): used by pieces E and F.
/// The four stable verdict strings in clean JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputVerdict {
    Removed,
    Kept,
    Deferred,
    WouldRemove,
}

/// Route parsed arguments to the owning piece without discovering or mutating a repo.
pub(crate) fn dispatch(command: CleanCommand, yes: bool) -> anyhow::Result<()> {
    match command {
        CleanCommand::Rm {
            branches,
            after,
            dry_run,
            format,
        } => rm::run(branches, after, dry_run, format, yes),
        CleanCommand::Sweep {
            all_repos,
            roots,
            dry_run,
            format,
        } => sweep::run(all_repos, roots, dry_run, format, yes),
        CleanCommand::Keep { branch, off } => keep::run(branch.as_deref(), off),
        CleanCommand::Restore { branch } => restore::run(&branch, yes),
    }
}
