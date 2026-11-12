//! User-level repo registry and bounded, read-only discovery for all-repo sweeps.
//!
//! `clean-repositories.json` sits beside the resolved user config, following
//! its XDG/APPDATA and explicit path overrides. It contains a sorted array of
//! canonical repository paths. Registration locks a sibling file, merges the
//! current repo into the latest array, and replaces the file atomically.
//! Missing entries remain registered so an unmounted volume can return later.
//!
//! Selection always includes the current repo. All-repo selection adds the
//! registry and owners of linked worktrees at root, root/child, or
//! root/child/grandchild. Ordinary folders and main-worktree `.git` directories
//! do not discover repositories. Nothing in selection writes, fetches, or
//! infers that an absent directory can be deleted. Failed lookups are returned
//! to the caller for reporting. Bare repositories keep their bare-directory
//! identity; policy protects them without default-branch discovery or network.

use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::Context;
use worktrunk::config::require_config_path;
use worktrunk::git::{ErrorExt, Repository, WorktrunkError};
use worktrunk::path::format_path_for_display;
use worktrunk::utils::write_atomically;

use super::keep::{load_json, lock_state};

/// Repositories that can be inspected, and paths that could not be inspected.
#[derive(Debug, Default)]
pub struct RepositorySelection {
    pub repositories: Vec<PathBuf>,
    pub skipped: Vec<SkippedRepository>,
}

/// A failed repository or discovery-root lookup for the caller to report.
#[derive(Debug)]
pub struct SkippedRepository {
    pub path: PathBuf,
    pub reason: String,
}

// TODO(clean): used by pieces C, D, E, and F, except dry runs.
/// Register the current repo in user-level state whenever a clean command
/// runs. Preserve existing entries and use the repo's atomic-write conventions;
/// dry runs consult the registry without registering or otherwise changing it.
pub fn register(repo: &Repository) -> anyhow::Result<()> {
    register_at(repo, &registry_path()?)
}

// TODO(clean): used by piece F.
/// Select the current repo, or with all_repos combine registered repos and
/// owners of linked worktrees found at most two folders below each root.
/// Deduplicate repos; unreachable primary worktrees are skipped and reported,
/// never interpreted as safe filesystem cleanup targets.
pub fn load_repositories(
    current: &Repository,
    all_repos: bool,
    roots: &[PathBuf],
) -> anyhow::Result<RepositorySelection> {
    // The single-repo path must not depend on user-level state availability.
    let path = if all_repos {
        registry_path()?
    } else {
        PathBuf::new()
    };
    load_repositories_at(current, all_repos, roots, &path)
}

fn registry_path() -> anyhow::Result<PathBuf> {
    Ok(require_config_path()?.with_file_name("clean-repositories.json"))
}

fn register_at(repo: &Repository, registry_path: &Path) -> anyhow::Result<()> {
    let path = repository_path(repo)?;
    let parent = registry_path
        .parent()
        .context("Repository registry has no parent directory")?;
    fs::create_dir_all(parent).context("Failed to create repository registry directory")?;
    let _lock = lock_state(registry_path)?;
    let mut repos: BTreeSet<PathBuf> = load_json(registry_path)?.unwrap_or_default();
    // Normalize still-reachable aliases, preserving absent entries verbatim.
    repos = repos
        .into_iter()
        .map(|path| dunce::canonicalize(&path).unwrap_or(path))
        .collect();
    repos.insert(path);
    write_atomically(registry_path, &serde_json::to_string(&repos)?).with_context(|| {
        format!(
            "Failed to save repository registry @ {}",
            format_path_for_display(registry_path)
        )
    })
}

fn load_repositories_at(
    current: &Repository,
    all_repos: bool,
    roots: &[PathBuf],
    registry_path: &Path,
) -> anyhow::Result<RepositorySelection> {
    let mut selection = RepositorySelection::default();
    match repository_path(current) {
        Ok(path) => selection.repositories.push(path),
        Err(error) => selection.skip(current.discovery_path(), error)?,
    }
    if all_repos {
        let repos: BTreeSet<PathBuf> = load_json(registry_path)?.unwrap_or_default();
        for path in repos {
            selection.add(&path)?;
        }
        for root in roots {
            discover(root, 0, &mut selection)?;
        }
    }
    selection.repositories.sort();
    selection.repositories.dedup();
    selection.skipped.sort_by(|a, b| a.path.cmp(&b.path));
    selection.skipped.dedup_by(|a, b| a.path == b.path);
    Ok(selection)
}

/// Validate the primary directory without calling default_branch (which can
/// fetch and cache). A missing primary must never become a cleanup candidate.
fn repository_path(repo: &Repository) -> anyhow::Result<PathBuf> {
    let path = repo.repo_path()?;
    let path = dunce::canonicalize(path).with_context(|| {
        format!(
            "Cannot reach repository @ {}",
            format_path_for_display(path)
        )
    })?;
    if !repo.is_bare()? {
        repo.worktree_at(&path).ensure_holds_this_worktree()?;
    }
    Ok(path)
}

impl RepositorySelection {
    fn skip(&mut self, path: &Path, error: anyhow::Error) -> anyhow::Result<()> {
        if let Some(signal) = error.interrupt_signal() {
            return Err(WorktrunkError::Interrupted { signal, hint: None }.into());
        }
        self.skipped.push(SkippedRepository {
            path: dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            reason: format!("{error:#}"),
        });
        Ok(())
    }

    fn add(&mut self, path: &Path) -> anyhow::Result<()> {
        let result = (|| {
            let path = dunce::canonicalize(path)?;
            let repo = Repository::at(&path)?;
            // Do not let git's ancestor search turn a stale registered folder
            // inside another repo into that ancestor's cleanup request.
            if !repo.is_bare()? {
                repo.worktree_at(&path).ensure_holds_this_worktree()?;
            } else if path != repo.git_common_dir() {
                anyhow::bail!("Registered directory is not a repository root");
            }
            repository_path(&repo)
        })();
        match result {
            Ok(path) => self.repositories.push(path),
            Err(error) => self.skip(path, error)?,
        }
        Ok(())
    }
}

fn discover(path: &Path, depth: u8, selection: &mut RepositorySelection) -> anyhow::Result<()> {
    let result = (|| -> anyhow::Result<()> {
        match fs::metadata(path.join(".git")) {
            Ok(metadata) if metadata.is_file() => {
                let repo = Repository::at(path)?;
                let worktree = repo.worktree_at(path);
                worktree.ensure_holds_this_worktree()?;
                if worktree.is_linked()? {
                    selection.repositories.push(repository_path(&repo)?);
                }
                return Ok(());
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Cannot inspect discovery directory"),
        }
        // Read even at the boundary to report a missing/unreadable root.
        let entries = fs::read_dir(path).context("Cannot read discovery directory")?;
        if depth == 2 {
            return Ok(());
        }
        for entry in entries {
            let entry = entry.context("Cannot read discovery entry")?;
            if entry.file_name() != ".git" && entry.file_type()?.is_dir() {
                discover(&entry.path(), depth + 1, selection)?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        selection.skip(path, error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Arc, Barrier};

    use worktrunk::testing::TestRepo;

    use super::*;

    #[test]
    fn tests_that_interrupted_discovery_aborts_instead_of_skipping() {
        use worktrunk::git::{ErrorExt, WorktrunkError};
        for signal in [2, 15] {
            let mut selection = RepositorySelection::default();
            let error = WorktrunkError::ChildProcessExited {
                code: 128 + signal,
                signal: Some(signal),
                message: "interrupted git".into(),
            };
            let result = selection.skip(Path::new("/unvisited"), error.into());
            assert_eq!(result.unwrap_err().interrupt_signal(), Some(signal));
            assert!(selection.skipped.is_empty());
        }
    }

    #[test]
    fn tests_that_registry_deduplicates_worktrees_and_preserves_other_repos() {
        let mut fixture = TestRepo::with_initial_commit();
        let other = TestRepo::with_initial_commit();
        let linked = fixture.add_worktree("feature");
        let repo = Repository::at(fixture.root_path()).unwrap();
        let registry = fixture.home_path().join("clean-repositories.json");
        register_at(&repo, &registry).unwrap();
        register_at(&Repository::at(linked).unwrap(), &registry).unwrap();
        register_at(&Repository::at(other.root_path()).unwrap(), &registry).unwrap();
        let before = fs::read(&registry).unwrap();
        register_at(&repo, &registry).unwrap();
        assert_eq!(before, fs::read(&registry).unwrap());
        let selection = load_repositories_at(&repo, true, &[], &registry).unwrap();
        let mut wanted = vec![
            fixture.root_path().to_path_buf(),
            other.root_path().to_path_buf(),
        ];
        wanted.sort();
        assert_eq!(selection.repositories, wanted);
        assert!(selection.skipped.is_empty());
        assert_eq!(before, fs::read(&registry).unwrap());
    }

    #[test]
    fn tests_that_missing_registered_repositories_are_reported_without_cleanup() {
        let fixture = TestRepo::with_initial_commit();
        let other = TestRepo::with_initial_commit();
        let repo = Repository::at(fixture.root_path()).unwrap();
        let registry = fixture.home_path().join("clean-repositories.json");
        register_at(&Repository::at(other.root_path()).unwrap(), &registry).unwrap();
        let missing = other.root_path().to_path_buf();
        let moved = other.home_path().join("moved");
        fs::rename(&missing, &moved).unwrap();
        let before = fs::read(&registry).unwrap();
        let selection = load_repositories_at(&repo, true, &[], &registry).unwrap();
        assert_eq!(
            selection.repositories,
            vec![fixture.root_path().to_path_buf()]
        );
        assert_eq!(selection.skipped.len(), 1);
        assert_eq!(selection.skipped[0].path, missing);
        assert!(!selection.skipped[0].reason.is_empty());
        assert!(!missing.exists());
        assert!(moved.join(".git").is_dir());
        assert_eq!(before, fs::read(&registry).unwrap());
    }

    #[test]
    fn tests_that_discovery_reads_linked_worktrees_only_through_depth_two() {
        let fixture = TestRepo::with_initial_commit();
        let mut shallow = TestRepo::with_initial_commit();
        let mut deep = TestRepo::with_initial_commit();
        let root = tempfile::tempdir().unwrap();
        shallow.add_worktree_at_path("two", &root.path().join("group/two"));
        deep.add_worktree_at_path("three", &root.path().join("group/deeper/three"));
        let plain = root.path().join("plain");
        fs::create_dir(&plain).unwrap();
        fs::write(plain.join("precious"), "keep").unwrap();
        let repo = Repository::at(fixture.root_path()).unwrap();
        let registry = fixture.home_path().join("absent-registry.json");
        let selection = load_repositories_at(
            &repo,
            true,
            &[root.path().into(), root.path().into()],
            &registry,
        )
        .unwrap();
        let mut wanted = vec![
            fixture.root_path().to_path_buf(),
            shallow.root_path().to_path_buf(),
        ];
        wanted.sort();
        assert_eq!(selection.repositories, wanted);
        assert!(selection.skipped.is_empty());
        assert!(!registry.exists());
        assert_eq!(fs::read_to_string(plain.join("precious")).unwrap(), "keep");
    }

    #[test]
    fn tests_that_current_repo_selection_ignores_registry_and_roots() {
        let mut fixture = TestRepo::with_initial_commit();
        let linked = fixture.add_worktree("feature");
        let repo = Repository::at(linked).unwrap();
        let registry = fixture.home_path().join("corrupt-registry.json");
        fs::write(&registry, "not json").unwrap();
        let selection = load_repositories_at(
            &repo,
            false,
            &[fixture.home_path().join("missing")],
            &registry,
        )
        .unwrap();
        assert_eq!(
            selection.repositories,
            vec![fixture.root_path().to_path_buf()]
        );
        assert!(selection.skipped.is_empty());
    }

    #[test]
    fn tests_that_invalid_registry_is_an_error_and_is_not_overwritten() {
        let fixture = TestRepo::with_initial_commit();
        let repo = Repository::at(fixture.root_path()).unwrap();
        let registry = fixture.home_path().join("clean-repositories.json");
        for contents in ["not json", "{}", "[1]"] {
            fs::write(&registry, contents).unwrap();
            assert!(load_repositories_at(&repo, true, &[], &registry).is_err());
            assert!(register_at(&repo, &registry).is_err());
            assert_eq!(fs::read_to_string(&registry).unwrap(), contents);
        }
        fs::remove_file(&registry).unwrap();
        fs::create_dir(&registry).unwrap();
        assert!(load_repositories_at(&repo, true, &[], &registry).is_err());
    }

    #[test]
    fn tests_that_missing_roots_and_broken_worktrees_are_reported() {
        let fixture = TestRepo::with_initial_commit();
        let root = tempfile::tempdir().unwrap();
        let broken = root.path().join("broken");
        fs::create_dir(&broken).unwrap();
        fs::write(
            broken.join(".git"),
            "gitdir: /nonexistent/clean-policy-admin",
        )
        .unwrap();
        let missing = root.path().join("missing");
        let registry = fixture.home_path().join("absent-registry.json");
        let repo = Repository::at(fixture.root_path()).unwrap();
        let selection = load_repositories_at(
            &repo,
            true,
            &[root.path().into(), missing.clone()],
            &registry,
        )
        .unwrap();
        assert_eq!(
            selection.repositories,
            vec![fixture.root_path().to_path_buf()]
        );
        assert_eq!(selection.skipped.len(), 2);
        assert!(selection.skipped.iter().any(|s| s.path == missing));
        assert!(
            selection
                .skipped
                .iter()
                .any(|s| s.path == dunce::canonicalize(&broken).unwrap())
        );
        assert!(!missing.exists());
        assert!(broken.join(".git").is_file());
        assert!(!registry.exists());
    }

    #[cfg(unix)]
    #[test]
    fn tests_that_registry_deduplicates_real_paths() {
        let fixture = TestRepo::with_initial_commit();
        let alias = fixture.home_path().join("alias");
        std::os::unix::fs::symlink(fixture.root_path(), &alias).unwrap();
        let repo = Repository::at(&alias).unwrap();
        let registry = fixture.home_path().join("clean-repositories.json");
        fs::write(
            &registry,
            serde_json::to_string(&vec![&alias, fixture.root_path()]).unwrap(),
        )
        .unwrap();
        let selection = load_repositories_at(&repo, true, &[], &registry).unwrap();
        assert_eq!(
            selection.repositories,
            vec![fixture.root_path().to_path_buf()]
        );
        assert!(selection.skipped.is_empty());
    }

    #[test]
    fn tests_that_concurrent_registrations_preserve_both_repos() {
        let first = TestRepo::with_initial_commit();
        let second = TestRepo::with_initial_commit();
        let registry = first.home_path().join("clean-repositories.json");
        let barrier = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            for fixture in [&first, &second] {
                let barrier = barrier.clone();
                let registry = &registry;
                scope.spawn(move || {
                    let repo = Repository::at(fixture.root_path()).unwrap();
                    barrier.wait();
                    register_at(&repo, registry).unwrap();
                });
            }
        });
        let repo = Repository::at(first.root_path()).unwrap();
        let selection = load_repositories_at(&repo, true, &[], &registry).unwrap();
        let mut wanted = vec![
            first.root_path().to_path_buf(),
            second.root_path().to_path_buf(),
        ];
        wanted.sort();
        assert_eq!(selection.repositories, wanted);
    }
}
