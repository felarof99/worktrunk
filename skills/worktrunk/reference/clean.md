# wt clean

Back up and clean worktrees.

These commands are not implemented yet. The rules below describe the planned command family.

## Examples

```bash
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

## Command reference

```
wt clean - Back up and clean worktrees

Usage: wt clean [OPTIONS] <COMMAND>

Commands:
  rm       Back up and remove worktrees
  sweep    Back up and remove inactive worktrees
  keep     Pin a worktree against cleanup
  restore  Recreate a worktree from backup

Options:
  -h, --help
          Print help (see a summary with '-h')

Global Options:
  -C <path>
          Working directory for this command

      --config <path>
          User config file path

      --config-set <toml>
          Override config with inline TOML, e.g. --config-set list.full=true (repeatable)

  -v, --verbose...
          Verbose output (-v: info logs + hook/alias template variables on stderr; -vv: also debug
          logs and raw subprocess output written to .git/wt/logs/). Set WORKTRUNK_VERBOSE=0|1|2 to
          apply the same level everywhere — including shell completion, which no flag can reach

  -y, --yes
          Skip approval prompts
```

# Subcommands

## wt clean rm

Back up and remove worktrees.

Not implemented yet. The planned command refuses primary and pinned worktrees. A busy worktree is backed up now and marked for removal when nothing runs in it; the calling process counts as busy. To unpin a worktree, run `wt clean keep --off` for that branch.

```bash
$ wt clean rm feature/auth
$ wt clean rm feature/auth old-fix --dry-run
$ wt clean rm feature/auth --after 10m
```

With `--after`, a detached process waits and removes only if the worktree is no longer busy and has not been touched since the request. Otherwise it does nothing.

Exit status is 0 when every worktree is removed or deferred, and 1 when any worktree is refused or its backup fails. A deferred worktree goes when nothing runs in it.

### Command reference

```
wt clean rm - Back up and remove worktrees

Usage: wt clean rm [OPTIONS] [BRANCH]...

Arguments:
  [BRANCH]...
          Branches to remove (defaults to current)

Options:
      --after <DURATION>
          Delay removal, then recheck activity

          Return immediately; examples: 10m, 1h, 1d.

      --dry-run
          Preview without changing anything

  -h, --help
          Print help (see a summary with '-h')

Automation:
      --format <FORMAT>
          Output format

          [default: text]
          [possible values: text, json]

Global Options:
  -C <path>
          Working directory for this command

      --config <path>
          User config file path

      --config-set <toml>
          Override config with inline TOML, e.g. --config-set list.full=true (repeatable)

  -v, --verbose...
          Verbose output (-v: info logs + hook/alias template variables on stderr; -vv: also debug
          logs and raw subprocess output written to .git/wt/logs/). Set WORKTRUNK_VERBOSE=0|1|2 to
          apply the same level everywhere — including shell completion, which no flag can reach

  -y, --yes
          Skip approval prompts
```

## wt clean sweep

Back up and remove inactive worktrees.

Not implemented yet. The planned sweep keeps primary and pinned worktrees. A worktree silent for at least 7 days is removed after ending its non-working hc terminals. Otherwise, busy worktrees are kept, remove-requested worktrees are removed, and untouched worktrees are removed after 24 hours.

Busy means a live process has its working directory inside the worktree, or a live hc terminal was launched inside it. Last touch is the newest timestamp of the worktree's index, HEAD, logs/HEAD, and changed or untracked files. Silent means last touch is at least 7 days old and every hc terminal is not working and has not advanced its turn in 7 days. A newly observed terminal has no such history yet.

```bash
$ wt clean sweep --dry-run
$ wt clean sweep --all-repos --root ~/code --root ~/work
```

`--all-repos` covers the registry of repos where any `wt clean` command has run, plus repos owning linked worktrees found up to two folders below each `--root`. Repos with unreachable primary worktrees are skipped and reported. Attic entries older than 30 days are deleted, except during a dry run.

Kept worktrees are normal results. Exit status is 0 unless the sweep itself fails.

### Command reference

```
wt clean sweep - Back up and remove inactive worktrees

Usage: wt clean sweep [OPTIONS]

Options:
      --all-repos
          Include registered and discovered repos

      --root <DIR>
          Discovery root for --all-repos (repeatable)

      --dry-run
          Preview without changing anything

  -h, --help
          Print help (see a summary with '-h')

Automation:
      --format <FORMAT>
          Output format

          [default: text]
          [possible values: text, json]

Global Options:
  -C <path>
          Working directory for this command

      --config <path>
          User config file path

      --config-set <toml>
          Override config with inline TOML, e.g. --config-set list.full=true (repeatable)

  -v, --verbose...
          Verbose output (-v: info logs + hook/alias template variables on stderr; -vv: also debug
          logs and raw subprocess output written to .git/wt/logs/). Set WORKTRUNK_VERBOSE=0|1|2 to
          apply the same level everywhere — including shell completion, which no flag can reach

  -y, --yes
          Skip approval prompts
```

## wt clean keep

Pin a worktree against cleanup.

Not implemented yet. The planned pin protects a worktree from both `wt clean rm` and `wt clean sweep`, including the 7-day rule. `--off` removes the pin.

```bash
$ wt clean keep feature/auth
$ wt clean keep feature/auth --off
```

### Command reference

```
wt clean keep - Pin a worktree against cleanup

Usage: wt clean keep [OPTIONS] [BRANCH]

Arguments:
  [BRANCH]
          Branch to pin (defaults to current)

Options:
      --off
          Remove the pin

  -h, --help
          Print help (see a summary with '-h')

Global Options:
  -C <path>
          Working directory for this command

      --config <path>
          User config file path

      --config-set <toml>
          Override config with inline TOML, e.g. --config-set list.full=true (repeatable)

  -v, --verbose...
          Verbose output (-v: info logs + hook/alias template variables on stderr; -vv: also debug
          logs and raw subprocess output written to .git/wt/logs/). Set WORKTRUNK_VERBOSE=0|1|2 to
          apply the same level everywhere — including shell completion, which no flag can reach

  -y, --yes
          Skip approval prompts
```

## wt clean restore

Recreate a worktree from backup.

Not implemented yet. The planned restore uses the local branch if present, otherwise `<remote>/<branch>`, otherwise `<remote>/wt-backup/<branch>` (fetching that one ref). It re-applies the newest attic entry, if any, then prints the worktree path.

```bash
$ wt clean restore feature/auth
```

### Command reference

```
wt clean restore - Recreate a worktree from backup

Usage: wt clean restore [OPTIONS] <BRANCH>

Arguments:
  <BRANCH>
          Branch to restore

Options:
  -h, --help
          Print help (see a summary with '-h')

Global Options:
  -C <path>
          Working directory for this command

      --config <path>
          User config file path

      --config-set <toml>
          Override config with inline TOML, e.g. --config-set list.full=true (repeatable)

  -v, --verbose...
          Verbose output (-v: info logs + hook/alias template variables on stderr; -vv: also debug
          logs and raw subprocess output written to .git/wt/logs/). Set WORKTRUNK_VERBOSE=0|1|2 to
          apply the same level everywhere — including shell completion, which no flag can reach

  -y, --yes
          Skip approval prompts
```
