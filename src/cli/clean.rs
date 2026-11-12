use std::path::PathBuf;
use std::time::Duration;

use clap::Subcommand;

use super::SwitchFormat;

pub(super) const CLEAN_HELP: &str = r#"These commands are not implemented yet. The rules below describe the planned command family.

## Examples

```console
$ wt clean rm feature/auth
$ wt clean rm feature/auth --after 10m
$ wt clean sweep --dry-run
$ wt clean sweep --all-repos --root ~/code --format json
$ wt clean keep feature/auth
$ wt clean keep feature/auth --off
$ wt clean restore feature/auth
```

## Worktrees and backups

Worktrees are addressed by branch name, with the worktree's own path as an alias resolved after the branch. Omitting BRANCH selects the current worktree where the argument is optional.

Before any removal, commits must be confirmed on the primary remote. Missing commits are pushed to `wt-backup/<branch>`; detached heads use `wt-backup/detached-<worktree folder name>`. Uncommitted changes and differing ignored `.env*` files are saved locally under `~/.wt-attic/` and never uploaded. A missing remote, failed backup, or worktree changed during backup prevents removal.

Primary and pinned worktrees are kept. Locks are respected except factory locks whose reasons start with `hc factory run` or `herdr-factory run`. Removal hooks follow the same approval rules as `wt remove`.

## Preview and JSON

`rm` and `sweep` accept `--dry-run`: print verdicts and reasons without changing anything. A preview may query the remote with `ls-remote`, but never pushes.

`--format json` prints one array. Each item has `repo`, `branch` (null for detached HEAD), `path`, `verdict` (`removed`, `kept`, `deferred`, or `would-remove`), `reason`, `backup` (`on-remote`, the pushed ref, or null), and `attic` (path or null).
<!-- subdoc: rm -->
<!-- subdoc: sweep -->
<!-- subdoc: keep -->
<!-- subdoc: restore -->"#;

#[derive(Subcommand)]
pub(crate) enum CleanCommand {
    /// Back up and remove worktrees
    #[command(
        after_long_help = r#"Not implemented yet. The planned command refuses primary and pinned worktrees. A busy worktree is backed up now and marked for removal when nothing runs in it; the calling process counts as busy. To unpin a worktree, run `wt clean keep --off` for that branch.

```console
$ wt clean rm feature/auth
$ wt clean rm feature/auth old-fix --dry-run
$ wt clean rm feature/auth --after 10m
```

With `--after`, a detached process waits and removes only if the worktree is no longer busy and has not been touched since the request. Otherwise it does nothing.

Exit status is 0 when every worktree is removed or deferred, and 1 when any worktree is refused or its backup fails. A deferred worktree goes when nothing runs in it.
"#
    )]
    Rm {
        /// Branches to remove (defaults to current)
        #[arg(value_name = "BRANCH", add = crate::completion::worktree_only_completer(), value_parser = super::non_empty_branch)]
        branches: Vec<String>,

        /// Delay removal, then recheck activity
        ///
        /// Return immediately; examples: 10m, 1h, 1d.
        #[arg(long, value_name = "DURATION", value_parser = humantime::parse_duration)]
        after: Option<Duration>,

        /// Preview without changing anything
        #[arg(long)]
        dry_run: bool,

        /// Output format
        #[arg(long, default_value = "text", help_heading = "Automation")]
        format: SwitchFormat,
    },

    /// Back up and remove inactive worktrees
    #[command(
        after_long_help = r#"Not implemented yet. The planned sweep keeps primary and pinned worktrees. A worktree silent for at least 7 days is removed after ending its non-working hc terminals. Otherwise, busy worktrees are kept, remove-requested worktrees are removed, and untouched worktrees are removed after 24 hours.

Busy means a live process has its working directory inside the worktree, or a live hc terminal was launched inside it. Last touch is the newest timestamp of the worktree's index, HEAD, logs/HEAD, and changed or untracked files. Silent means last touch is at least 7 days old and every hc terminal is not working and has not advanced its turn in 7 days. A newly observed terminal has no such history yet.

```console
$ wt clean sweep --dry-run
$ wt clean sweep --all-repos --root ~/code --root ~/work
```

`--all-repos` covers the registry of repos where any `wt clean` command has run, plus repos owning linked worktrees found up to two folders below each `--root`. Repos with unreachable primary worktrees are skipped and reported. Attic entries older than 30 days are deleted, except during a dry run.

Kept worktrees are normal results. Exit status is 0 unless the sweep itself fails.
"#
    )]
    Sweep {
        /// Include registered and discovered repos
        #[arg(long)]
        all_repos: bool,

        /// Discovery root for --all-repos (repeatable)
        #[arg(long = "root", value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
        roots: Vec<PathBuf>,

        /// Preview without changing anything
        #[arg(long)]
        dry_run: bool,

        /// Output format
        #[arg(long, default_value = "text", help_heading = "Automation")]
        format: SwitchFormat,
    },

    /// Pin a worktree against cleanup
    #[command(
        after_long_help = r#"Not implemented yet. The planned pin protects a worktree from both `wt clean rm` and `wt clean sweep`, including the 7-day rule. `--off` removes the pin.

```console
$ wt clean keep feature/auth
$ wt clean keep feature/auth --off
```
"#
    )]
    Keep {
        /// Branch to pin (defaults to current)
        #[arg(value_name = "BRANCH", add = crate::completion::worktree_only_completer(), value_parser = super::non_empty_branch)]
        branch: Option<String>,

        /// Remove the pin
        #[arg(long)]
        off: bool,
    },

    /// Recreate a worktree from backup
    #[command(
        after_long_help = r#"Not implemented yet. The planned restore uses the local branch if present, otherwise `<remote>/<branch>`, otherwise `<remote>/wt-backup/<branch>` (fetching that one ref). It re-applies the newest attic entry, if any, then prints the worktree path.

```console
$ wt clean restore feature/auth
```
"#
    )]
    Restore {
        /// Branch to restore
        #[arg(value_name = "BRANCH", add = crate::completion::branch_value_completer(), value_parser = super::non_empty_branch)]
        branch: String,
    },
}
