# wt clean

Back up and clean worktrees.

Commits are confirmed on the primary remote before removal. Uncommitted work is saved locally; ignored build output is discarded.

## Examples

```bash
$ wt clean rm feature/auth
$ wt clean rm feature/auth --after 10m
$ wt clean rm feature/auth --wait 5s
$ wt clean sweep --dry-run
$ wt clean sweep --all-repos --root ~/code --format json
$ wt clean keep feature/auth
$ wt clean keep feature/auth --off
$ wt clean restore feature/auth
```

## Worktrees and backups

Worktrees are addressed by branch name, with the worktree's own path as an alias resolved after the branch. Omitting BRANCH selects the current worktree where the argument is optional.

Before any removal, commits must be confirmed on the primary remote. Missing commits are pushed to `wt-backup/<branch>`; detached heads use `wt-backup/detached-<worktree folder name>-<first 12 tip SHA characters>`. Tracked changes, untracked files, and differing ignored `.env*` files are saved locally under `~/.wt-attic/` and never uploaded. Other ignored content, including nested repositories under ignored paths, is discarded. Only `.env*` files Git can list in the worktree itself are saved. Untracked repositories and submodules with unsaved work prevent removal. A missing remote, failed backup, or worktree changed during backup prevents removal.

Primary and pinned worktrees are kept. Locks are respected except factory locks whose reasons start with `hc factory run` or `herdr-factory run`. Removal hooks follow the same approval rules as `wt remove`.

## Preview and JSON

`rm` and `sweep` accept `--dry-run`: print verdicts and reasons without changing anything. A preview checks file contents, attic safety, submodules, locks, and remote backup status. It may query the remote with `ls-remote`, but never pushes or runs hooks. A `would-remove` verdict describes the current checks; a later write, hook, or push can still fail. It does not wait for `--wait` or start a job for `--after`.

`--format json` prints one array. Each item has `repo`, `branch` (null for detached HEAD), `path`, `verdict` (`removed`, `kept`, `deferred`, or `would-remove`), `reason`, `backup` (`on-remote`, the pushed ref, or null), and `attic` (path or null). In a preview, `backup` is `on-remote` if the tip is already there; otherwise it is null and `reason` names the ref that would be pushed.

## Stored files

| Location | Contents |
|---|---|
| `~/.wt-attic/<repo>/<branch, slashes as dashes>/<UTC timestamp>/` | `tracked.patch`, `untracked/`, `ignored-env/`, and `meta.json`; local only |
| Beside user config: `clean-repositories.json` | Repositories visited by clean commands, except dry runs |
| Beside user config: `clean-terminal-history.json` | Terminal turns and the last time each changed |
| `.git/wt/clean/<unique-id>/marks.json` | Pins and remove-requested marks |
| Each worktree admin directory: `wt/clean-id.json` | Pointer to that worktree's marks |
| `.git/wt/logs/clean.jsonl` | Removal results and skipped or failed delayed requests |

Registry and mark updates use sibling lock files. Delayed-job output and remove-hook output use the usual per-branch logs under `.git/wt/logs/`. Sweeps expire attic entries at least 30 days old. Backed-up commits stay on the remote.

`rm` exits 0 when every request succeeds or is deferred, 1 on a refusal or failure. `sweep` exits 0 even when individual worktrees are kept; failures of the sweep itself exit 1. `keep` and `restore` exit 0 on success, 1 on failure. Invalid arguments exit 2.

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

Primary and pinned worktrees are refused. A busy worktree is backed up now and marked for removal when nothing runs in it; the calling process counts as busy. To unpin a worktree, run `wt clean keep --off` for that branch.

```bash
$ wt clean rm feature/auth
$ wt clean rm feature/auth old-fix --dry-run
$ wt clean rm feature/auth --after 10m
$ wt clean rm feature/auth --wait 5s
```

With `--after`, the command returns immediately. A detached process waits outside the worktree, then backs up and removes only if the worktree is no longer busy and has not been touched since the request. Otherwise it keeps the worktree and logs the reason. `--after` and `--wait` cannot be combined.

With `--wait`, the command stays in the foreground for up to the given duration until the worktree stops being busy, then backs up and removes it. If it is still busy when the wait ends, normal backup and deferral apply, with exit status 0.

Exit status is 0 when every worktree is removed or deferred, and 1 when any worktree is refused or its backup fails. A deferred worktree is marked for a later `wt clean sweep`; it is removed once nothing runs in it. There is no continuous cleanup daemon.

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

      --wait <DURATION>
          Wait in foreground until idle

          Wait at most this long, then defer if still busy; examples: 5s, 1m.

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

The sweep checks linked worktrees in order; the first matching rule wins. Every removal still requires a successful backup and the lock and content checks.

| Condition | Result |
|---|---|
| Primary worktree or bare repository | Keep |
| Pinned | Keep |
| Silent for at least 7 days | Remove; first end its non-working hc terminals |
| Busy | Keep |
| Earlier busy `clean rm` requested removal | Remove |
| Touched less than 24 hours ago | Keep |
| Otherwise | Remove |

Busy means a live process has its working directory inside the worktree, or a live hc terminal was launched inside it. Last touch is the newest timestamp of the worktree's index, HEAD, logs/HEAD, and changed or untracked files. Silent means last touch is at least 7 days old and every hc terminal is not working and has not advanced its turn in 7 days. A newly observed terminal has no such history yet. Missing `hc` means no terminals; failed process or terminal inspection keeps the worktree. PR merge status does not affect these rules.

```bash
$ wt clean sweep --dry-run
$ wt clean sweep --all-repos --root ~/code --root ~/work
```

`--all-repos` covers the registry of repos where any `wt clean` command has run, plus repos owning linked worktrees found up to two folders below each `--root`. Repos with unreachable primary worktrees are skipped and reported. Attic entries at least 30 days old are deleted, except during a dry run.

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

A pin protects a worktree from both `wt clean rm` and `wt clean sweep`, including the 7-day rule. `--off` removes the pin.

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

Restore uses the local branch if present, otherwise `<remote>/<branch>`, otherwise `<remote>/wt-backup/<branch>` (fetching that one ref). It re-applies the newest attic entry, if any, then prints the absolute worktree path. Existing worktrees are left in place. Restore refuses to overwrite local files or tracked changes, and retains the attic entry after success or failure. Creation follows the usual path configuration and hook approvals.

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
