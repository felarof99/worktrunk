//! Restore through the switch pipeline so path templates, collision checks,
//! and project-hook approvals retain their usual behavior. Existing worktrees
//! return before loading config, fetching, running hooks, or applying an attic.
//! A fresh repository handle after creation avoids stale worktree-list caches.

use anyhow::Context;
use color_print::cformat;
use worktrunk::config::UserConfig;
use worktrunk::git::{CommandError, Repository, ResolvedWorktree, is_valid_branch_name};
use worktrunk::path::format_path_for_display;
use worktrunk::styling::{eprintln, info_message, println, progress_message};

use crate::cli::SwitchFormat;
use crate::commands::worktree::SwitchPipeline;

use super::attic;

/// Recreate a worktree from the local branch, then the normal remote branch,
/// then the fetched wt-backup ref, in that order. Apply the latest attic entry
/// if present and print the path; never retarget an existing worktree.
pub fn run(branch: &str, yes: bool) -> anyhow::Result<()> {
    let repo = Repository::current()?;
    let branch = match repo.resolve_worktree(branch)? {
        ResolvedWorktree::Worktree { path, .. } => {
            eprintln!(
                "{}",
                info_message(cformat!(
                    "Worktree for <bold>{branch}</> already exists @ {}",
                    format_path_for_display(&path)
                ))
            );
            return Ok(());
        }
        ResolvedWorktree::BranchOnly { branch } => branch,
        ResolvedWorktree::NoWorktreeAtPath { path } => {
            anyhow::bail!("No worktree @ {}", format_path_for_display(&path))
        }
    };
    anyhow::ensure!(
        is_valid_branch_name(&branch) && !branch.starts_with('-'),
        "Invalid branch name: {branch}"
    );
    eprintln!(
        "{}",
        progress_message(cformat!("Restoring worktree for <bold>{branch}</>..."))
    );
    super::registry::register(&repo)?;
    let entry = attic::load_latest(&repo, &branch)?;
    let local = has_ref(&repo, &format!("refs/heads/{branch}"))?;
    let base = if local {
        None
    } else {
        let remote = repo
            .primary_remote()
            .context("Cannot restore branch: no remote")?;
        let normal = format!("refs/remotes/{remote}/{branch}");
        let reference = if has_ref(&repo, &normal)? {
            normal
        } else {
            let backup = format!("refs/remotes/{remote}/wt-backup/{branch}");
            let refspec = format!("+refs/heads/wt-backup/{branch}:{backup}");
            repo.run_command_delayed_stream(
                &[
                    "fetch",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--no-write-fetch-head",
                    "--refmap=",
                    "--",
                    &remote,
                    &refspec,
                ],
                0,
                None,
            )
            .context("Failed to fetch backup branch")?;
            backup
        };
        // Pin the chosen source before hooks run or another fetch changes it.
        Some(
            repo.run_command(&["rev-parse", "--verify", &format!("{reference}^{{commit}}")])?
                .trim()
                .to_string(),
        )
    };
    let mut config = UserConfig::load().context("Failed to load config")?;
    SwitchPipeline {
        repo: &repo,
        config: &mut config,
        identifier: &branch,
        create: !local,
        base: base.as_deref(),
        clobber: false,
        verify: true,
        yes,
        change_dir: false,
        format: SwitchFormat::Text,
        is_recovered: false,
        suggestion_ctx: None,
        execute: None,
        execute_args: &[],
        shell_integration_binary: None,
    }
    .run()?;
    let fresh = Repository::at(repo.discovery_path())?;
    let path = fresh
        .worktree_for_branch(&branch)?
        .context("Restored worktree is missing")?;
    if let Some(entry) = entry {
        attic::apply(&entry, &path).with_context(|| {
            format!(
                "Failed to restore local changes; attic retained @ {}",
                format_path_for_display(&entry.path)
            )
        })?;
    }
    // The path is the pipeable answer, so keep it absolute and unstyled.
    println!("{}", path.display());
    Ok(())
}

fn has_ref(repo: &Repository, reference: &str) -> anyhow::Result<bool> {
    let args = ["show-ref", "--verify", "--quiet", reference];
    let output = repo
        .worktree_at(repo.discovery_path())
        .run_command_output(&args)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(CommandError::from_failed_output("git", &args, &output).into()),
    }
}
