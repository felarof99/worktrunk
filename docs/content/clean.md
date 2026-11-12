+++
title = "wt clean"
description = "Back up and clean worktrees."
weight = 14

[extra]
group = "Commands"
+++

<!-- ⚠️ AUTO-GENERATED from `wt clean --help-page` — edit src/cli/mod.rs to update -->

Back up and clean worktrees.

These commands are not implemented yet. The rules below describe the planned command family.

## Examples

{{ terminal(cmd="wt clean rm feature/auth|||wt clean rm feature/auth --after 10m|||wt clean sweep --dry-run|||wt clean sweep --all-repos --root ~/code --format json|||wt clean keep feature/auth|||wt clean keep feature/auth --off|||wt clean restore feature/auth") }}

## Worktrees and backups

Worktrees are addressed by branch name, with the worktree's own path as an alias resolved after the branch. Omitting BRANCH selects the current worktree where the argument is optional.

Before any removal, commits must be confirmed on the primary remote. Missing commits are pushed to `wt-backup/<branch>`; detached heads use `wt-backup/detached-<worktree folder name>`. Uncommitted changes and differing ignored `.env*` files are saved locally under `~/.wt-attic/` and never uploaded. A missing remote, failed backup, or worktree changed during backup prevents removal.

Primary and pinned worktrees are kept. Locks are respected except factory locks whose reasons start with `hc factory run` or `herdr-factory run`. Removal hooks follow the same approval rules as `wt remove`.

## Preview and JSON

`rm` and `sweep` accept `--dry-run`: print verdicts and reasons without changing anything. A preview may query the remote with `ls-remote`, but never pushes.

`--format json` prints one array. Each item has `repo`, `branch` (null for detached HEAD), `path`, `verdict` (`removed`, `kept`, `deferred`, or `would-remove`), `reason`, `backup` (`on-remote`, the pushed ref, or null), and `attic` (path or null).

## Command reference

{% terminal() %}
wt clean - Back up and clean worktrees

Usage: <b><span class=c>wt clean</span></b> <span class=c>[OPTIONS]</span> <span class=c>&lt;COMMAND&gt;</span>

<b><span class=g>Commands:</span></b>
  <b><span class=c>rm</span></b>       Back up and remove worktrees
  <b><span class=c>sweep</span></b>    Back up and remove inactive worktrees
  <b><span class=c>keep</span></b>     Pin a worktree against cleanup
  <b><span class=c>restore</span></b>  Recreate a worktree from backup

<b><span class=g>Options:</span></b>
  <b><span class=c>-h</span></b>, <b><span class=c>--help</span></b>
          Print help (see a summary with &#39;-h&#39;)

<b><span class=g>Global Options:</span></b>
  <b><span class=c>-C</span></b><span class=c> &lt;path&gt;</span>
          Working directory for this command

      <b><span class=c>--config</span></b><span class=c> &lt;path&gt;</span>
          User config file path

      <b><span class=c>--config-set</span></b><span class=c> &lt;toml&gt;</span>
          Override config with inline TOML, e.g. --config-set list.full=true (repeatable)

  <b><span class=c>-v</span></b>, <b><span class=c>--verbose</span></b><span class=c>...</span>
          Verbose output (-v: info logs + hook/alias template variables on stderr; -vv: also debug
          logs and raw subprocess output written to .git/wt/logs/). Set WORKTRUNK_VERBOSE=0|1|2 to
          apply the same level everywhere — including shell completion, which no flag can reach

  <b><span class=c>-y</span></b>, <b><span class=c>--yes</span></b>
          Skip approval prompts
{% end %}

# Subcommands

## wt clean rm

Back up and remove worktrees.

Not implemented yet. The planned command refuses primary and pinned worktrees. A busy worktree is backed up now and marked for removal when nothing runs in it; the calling process counts as busy. To unpin a worktree, run `wt clean keep --off` for that branch.

{{ terminal(cmd="wt clean rm feature/auth|||wt clean rm feature/auth old-fix --dry-run|||wt clean rm feature/auth --after 10m") }}

With `--after`, a detached process waits and removes only if the worktree is no longer busy and has not been touched since the request. Otherwise it does nothing.

Exit status is 0 when every worktree is removed or deferred, and 1 when any worktree is refused or its backup fails. A deferred worktree goes when nothing runs in it.

### Command reference

{% terminal() %}
wt clean rm - Back up and remove worktrees

Usage: <b><span class=c>wt clean rm</span></b> <span class=c>[OPTIONS]</span> <span class=c>[BRANCH]...</span>

<b><span class=g>Arguments:</span></b>
  <span class=c>[BRANCH]...</span>
          Branches to remove (defaults to current)

<b><span class=g>Options:</span></b>
      <b><span class=c>--after</span></b><span class=c> &lt;DURATION&gt;</span>
          Delay removal, then recheck activity

          Return immediately; examples: 10m, 1h, 1d.

      <b><span class=c>--dry-run</span></b>
          Preview without changing anything

  <b><span class=c>-h</span></b>, <b><span class=c>--help</span></b>
          Print help (see a summary with &#39;-h&#39;)

<b><span class=g>Automation:</span></b>
      <b><span class=c>--format</span></b><span class=c> &lt;FORMAT&gt;</span>
          Output format

          [default: text]
          [possible values: text, json]

<b><span class=g>Global Options:</span></b>
  <b><span class=c>-C</span></b><span class=c> &lt;path&gt;</span>
          Working directory for this command

      <b><span class=c>--config</span></b><span class=c> &lt;path&gt;</span>
          User config file path

      <b><span class=c>--config-set</span></b><span class=c> &lt;toml&gt;</span>
          Override config with inline TOML, e.g. --config-set list.full=true (repeatable)

  <b><span class=c>-v</span></b>, <b><span class=c>--verbose</span></b><span class=c>...</span>
          Verbose output (-v: info logs + hook/alias template variables on stderr; -vv: also debug
          logs and raw subprocess output written to .git/wt/logs/). Set WORKTRUNK_VERBOSE=0|1|2 to
          apply the same level everywhere — including shell completion, which no flag can reach

  <b><span class=c>-y</span></b>, <b><span class=c>--yes</span></b>
          Skip approval prompts
{% end %}

## wt clean sweep

Back up and remove inactive worktrees.

Not implemented yet. The planned sweep keeps primary and pinned worktrees. A worktree silent for at least 7 days is removed after ending its non-working hc terminals. Otherwise, busy worktrees are kept, remove-requested worktrees are removed, and untouched worktrees are removed after 24 hours.

Busy means a live process has its working directory inside the worktree, or a live hc terminal was launched inside it. Last touch is the newest timestamp of the worktree's index, HEAD, logs/HEAD, and changed or untracked files. Silent means last touch is at least 7 days old and every hc terminal is not working and has not advanced its turn in 7 days. A newly observed terminal has no such history yet.

{{ terminal(cmd="wt clean sweep --dry-run|||wt clean sweep --all-repos --root ~/code --root ~/work") }}

`--all-repos` covers the registry of repos where any `wt clean` command has run, plus repos owning linked worktrees found up to two folders below each `--root`. Repos with unreachable primary worktrees are skipped and reported. Attic entries older than 30 days are deleted, except during a dry run.

Kept worktrees are normal results. Exit status is 0 unless the sweep itself fails.

### Command reference

{% terminal() %}
wt clean sweep - Back up and remove inactive worktrees

Usage: <b><span class=c>wt clean sweep</span></b> <span class=c>[OPTIONS]</span>

<b><span class=g>Options:</span></b>
      <b><span class=c>--all-repos</span></b>
          Include registered and discovered repos

      <b><span class=c>--root</span></b><span class=c> &lt;DIR&gt;</span>
          Discovery root for --all-repos (repeatable)

      <b><span class=c>--dry-run</span></b>
          Preview without changing anything

  <b><span class=c>-h</span></b>, <b><span class=c>--help</span></b>
          Print help (see a summary with &#39;-h&#39;)

<b><span class=g>Automation:</span></b>
      <b><span class=c>--format</span></b><span class=c> &lt;FORMAT&gt;</span>
          Output format

          [default: text]
          [possible values: text, json]

<b><span class=g>Global Options:</span></b>
  <b><span class=c>-C</span></b><span class=c> &lt;path&gt;</span>
          Working directory for this command

      <b><span class=c>--config</span></b><span class=c> &lt;path&gt;</span>
          User config file path

      <b><span class=c>--config-set</span></b><span class=c> &lt;toml&gt;</span>
          Override config with inline TOML, e.g. --config-set list.full=true (repeatable)

  <b><span class=c>-v</span></b>, <b><span class=c>--verbose</span></b><span class=c>...</span>
          Verbose output (-v: info logs + hook/alias template variables on stderr; -vv: also debug
          logs and raw subprocess output written to .git/wt/logs/). Set WORKTRUNK_VERBOSE=0|1|2 to
          apply the same level everywhere — including shell completion, which no flag can reach

  <b><span class=c>-y</span></b>, <b><span class=c>--yes</span></b>
          Skip approval prompts
{% end %}

## wt clean keep

Pin a worktree against cleanup.

Not implemented yet. The planned pin protects a worktree from both `wt clean rm` and `wt clean sweep`, including the 7-day rule. `--off` removes the pin.

{{ terminal(cmd="wt clean keep feature/auth|||wt clean keep feature/auth --off") }}

### Command reference

{% terminal() %}
wt clean keep - Pin a worktree against cleanup

Usage: <b><span class=c>wt clean keep</span></b> <span class=c>[OPTIONS]</span> <span class=c>[BRANCH]</span>

<b><span class=g>Arguments:</span></b>
  <span class=c>[BRANCH]</span>
          Branch to pin (defaults to current)

<b><span class=g>Options:</span></b>
      <b><span class=c>--off</span></b>
          Remove the pin

  <b><span class=c>-h</span></b>, <b><span class=c>--help</span></b>
          Print help (see a summary with &#39;-h&#39;)

<b><span class=g>Global Options:</span></b>
  <b><span class=c>-C</span></b><span class=c> &lt;path&gt;</span>
          Working directory for this command

      <b><span class=c>--config</span></b><span class=c> &lt;path&gt;</span>
          User config file path

      <b><span class=c>--config-set</span></b><span class=c> &lt;toml&gt;</span>
          Override config with inline TOML, e.g. --config-set list.full=true (repeatable)

  <b><span class=c>-v</span></b>, <b><span class=c>--verbose</span></b><span class=c>...</span>
          Verbose output (-v: info logs + hook/alias template variables on stderr; -vv: also debug
          logs and raw subprocess output written to .git/wt/logs/). Set WORKTRUNK_VERBOSE=0|1|2 to
          apply the same level everywhere — including shell completion, which no flag can reach

  <b><span class=c>-y</span></b>, <b><span class=c>--yes</span></b>
          Skip approval prompts
{% end %}

## wt clean restore

Recreate a worktree from backup.

Not implemented yet. The planned restore uses the local branch if present, otherwise `<remote>/<branch>`, otherwise `<remote>/wt-backup/<branch>` (fetching that one ref). It re-applies the newest attic entry, if any, then prints the worktree path.

{{ terminal(cmd="wt clean restore feature/auth") }}

### Command reference

{% terminal() %}
wt clean restore - Recreate a worktree from backup

Usage: <b><span class=c>wt clean restore</span></b> <span class=c>[OPTIONS]</span> <span class=c>&lt;BRANCH&gt;</span>

<b><span class=g>Arguments:</span></b>
  <span class=c>&lt;BRANCH&gt;</span>
          Branch to restore

<b><span class=g>Options:</span></b>
  <b><span class=c>-h</span></b>, <b><span class=c>--help</span></b>
          Print help (see a summary with &#39;-h&#39;)

<b><span class=g>Global Options:</span></b>
  <b><span class=c>-C</span></b><span class=c> &lt;path&gt;</span>
          Working directory for this command

      <b><span class=c>--config</span></b><span class=c> &lt;path&gt;</span>
          User config file path

      <b><span class=c>--config-set</span></b><span class=c> &lt;toml&gt;</span>
          Override config with inline TOML, e.g. --config-set list.full=true (repeatable)

  <b><span class=c>-v</span></b>, <b><span class=c>--verbose</span></b><span class=c>...</span>
          Verbose output (-v: info logs + hook/alias template variables on stderr; -vv: also debug
          logs and raw subprocess output written to .git/wt/logs/). Set WORKTRUNK_VERBOSE=0|1|2 to
          apply the same level everywhere — including shell completion, which no flag can reach

  <b><span class=c>-y</span></b>, <b><span class=c>--yes</span></b>
          Skip approval prompts
{% end %}

<!-- END AUTO-GENERATED -->
