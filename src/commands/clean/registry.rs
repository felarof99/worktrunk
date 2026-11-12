//! User-level repo registry and bounded, read-only discovery for all-repo sweeps.
//!
//! `clean-repositories.json` sits beside the resolved user config, following
//! its XDG/APPDATA and explicit path overrides. It contains a sorted array of
//! canonical repository paths. Registration locks a sibling file, merges the
//! current repo into the latest array, and replaces the file atomically.
//! Missing entries remain registered so an unmounted volume can return later.
//!
//! Selection includes the current repo when available. All-repo selection adds the
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
use worktrunk::git::{CommandError, ErrorExt, Repository, WorktrunkError};
use worktrunk::path::format_path_for_display;
use worktrunk::shell_exec::Cmd;
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

/// Register the current repo in user-level state whenever a clean command
/// runs. Preserve existing entries and use the repo's atomic-write conventions;
/// dry runs consult the registry without registering or otherwise changing it.
pub fn register(repo: &Repository) -> anyhow::Result<()> {
    register_at(repo, &registry_path()?)
}

/// Select the current repo, or with all_repos combine registered repos and
/// owners of linked worktrees found at most two folders below each root.
/// Deduplicate repos; unreachable primary worktrees are skipped and reported,
/// never interpreted as safe filesystem cleanup targets.
pub fn load_repositories<'a>(
    current: impl Into<Option<&'a Repository>>,
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

fn load_repositories_at<'a>(
    current: impl Into<Option<&'a Repository>>,
    all_repos: bool,
    roots: &[PathBuf],
    registry_path: &Path,
) -> anyhow::Result<RepositorySelection> {
    let mut selection = RepositorySelection::default();
    if let Some(current) = current.into() {
        match repository_path(current) {
            Ok(path) => selection.repositories.push(path),
            Err(error) => selection.skip(current.discovery_path(), error)?,
        }
    } else {
        anyhow::ensure!(all_repos, "Run inside a repository or use --all-repos");
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
        match repository_at(path, false) {
            Ok(Some(path)) => self.repositories.push(path),
            Ok(None) => {}
            Err(error) => self.skip(path, error)?,
        }
        Ok(())
    }
}

/// Foreign repos must not inherit the invoking repo's GIT_DIR/GIT_WORK_TREE.
/// Repository::at intentionally preserves those for the caller's own context,
/// so discovery uses scrubbed Cmd probes and Git's structured worktree listing.
fn git_at(path: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Cmd::new("git")
        .args(args.iter().copied())
        .current_dir(path)
        .context(path.display().to_string())
        .scrub_git_discovery_env()
        .run()?;
    if !output.status.success() {
        return Err(CommandError::from_failed_output("git", args, &output).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn common_dir_at(path: &Path) -> anyhow::Result<PathBuf> {
    let directory = git_at(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Ok(dunce::canonicalize(directory.trim())?)
}

fn listed_path(record: &str) -> anyhow::Result<PathBuf> {
    let field = record.split('\0').next().context("Empty worktree record")?;
    Ok(PathBuf::from(
        field
            .strip_prefix("worktree ")
            .context("Missing worktree path")?,
    ))
}

fn repository_at(path: &Path, linked_only: bool) -> anyhow::Result<Option<PathBuf>> {
    let path = dunce::canonicalize(path)?;
    let listing = git_at(&path, &["worktree", "list", "--porcelain", "-z"])?;
    // NUL records preserve spaces and newlines in worktree paths. Only the
    // path and bare fields matter here; the library's full parser is private.
    let mut records = listing.split("\0\0").filter(|record| !record.is_empty());
    let first = records
        .next()
        .context("Repository has no worktree listing")?;
    let bare = first.split('\0').any(|field| field == "bare");
    let common = common_dir_at(&path)?;
    let mut primary = listed_path(first)?;
    // Git lists a submodule's common directory as its main worktree. Resolve
    // core.worktree from that git directory, as Repository::repo_path does.
    if !bare && dunce::canonicalize(&primary)? == common {
        primary = PathBuf::from(git_at(&common, &["rev-parse", "--show-toplevel"])?.trim());
    }
    let primary = dunce::canonicalize(&primary).with_context(|| {
        format!(
            "Cannot reach primary worktree @ {}",
            format_path_for_display(&primary)
        )
    })?;
    let mut linked = false;
    for record in records {
        if dunce::canonicalize(listed_path(record)?).is_ok_and(|listed| listed == path) {
            linked = true;
        }
    }
    // Refuse ancestor-search answers for a stale registered plain directory.
    anyhow::ensure!(
        path == primary || linked,
        "Directory is not a registered worktree"
    );
    if !bare || linked {
        fs::metadata(path.join(".git")).context("Worktree Git entry is missing")?;
    }
    if linked_only && !linked {
        return Ok(None);
    }
    if !bare {
        fs::metadata(primary.join(".git")).context("Primary worktree Git entry is missing")?;
        anyhow::ensure!(
            common_dir_at(&primary)? == common,
            "Primary worktree belongs to a different repository"
        );
    }
    Ok(Some(primary))
}

fn discover(path: &Path, depth: u8, selection: &mut RepositorySelection) -> anyhow::Result<()> {
    let result = (|| -> anyhow::Result<()> {
        match fs::metadata(path.join(".git")) {
            Ok(metadata) if metadata.is_file() => match repository_at(path, true) {
                Ok(Some(repo)) => selection.repositories.push(repo),
                Ok(None) => {}
                Err(error) => selection.skip(path, error)?,
            },
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
    fn tests_that_discovery_includes_nested_linked_worktrees() {
        let fixture = TestRepo::with_initial_commit();
        let mut outer = TestRepo::with_initial_commit();
        let mut inner = TestRepo::with_initial_commit();
        let root = tempfile::tempdir().unwrap();
        let outer_path = outer.add_worktree_at_path("outer", &root.path().join("outer"));
        inner.add_worktree_at_path("inner", &outer_path.join("inner"));
        let repo = Repository::at(fixture.root_path()).unwrap();
        let registry = fixture.home_path().join("absent-registry.json");
        let selection =
            load_repositories_at(&repo, true, &[root.path().into()], &registry).unwrap();
        let mut expected = vec![
            fixture.root_path().to_path_buf(),
            outer.root_path().to_path_buf(),
            inner.root_path().to_path_buf(),
        ];
        expected.sort();
        assert_eq!(selection.repositories, expected);
        assert!(selection.skipped.is_empty());
    }

    #[test]
    fn tests_that_registry_ignores_inherited_git_context() {
        const CHILD: &str = "WORKTRUNK_TEST_CLEAN_REGISTRY_CONTEXT";
        if let Some(root) = std::env::var_os(CHILD) {
            // Enable the harness git-config floor in this child too. Fixtures
            // scrub inherited git vars, while the production calls below see
            // GIT_DIR exactly as a git alias or hook would supply it.
            let _isolation = TestRepo::new();
            let root = PathBuf::from(root);
            let current = root.join("current");
            let other = root.join("other");
            let discovered = root.join("discovered");
            let repo = Repository::at(&current).unwrap();
            let selection = load_repositories_at(
                &repo,
                true,
                &[root.join("scan")],
                &root.join("registry.json"),
            )
            .unwrap();
            let mut expected = vec![current, other, discovered]
                .into_iter()
                .map(|path| dunce::canonicalize(path).unwrap())
                .collect::<Vec<_>>();
            expected.sort();
            assert_eq!(selection.repositories, expected);
            assert!(selection.skipped.is_empty());
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let current = TestRepo::at(&root.path().join("current"));
        let other = TestRepo::at(&root.path().join("other"));
        let mut discovered = TestRepo::at(&root.path().join("discovered"));
        discovered.commit("initial");
        discovered.add_worktree_at_path("feature", &root.path().join("scan/feature"));
        fs::write(
            root.path().join("registry.json"),
            serde_json::to_string(&vec![other.root_path()]).unwrap(),
        )
        .unwrap();
        let output = worktrunk::shell_exec::Cmd::new(
            std::env::current_exe().unwrap().to_string_lossy(),
        )
        .args([
            "--exact",
            "commands::clean::registry::tests::tests_that_registry_ignores_inherited_git_context",
            "--nocapture",
        ])
        .env(CHILD, root.path())
        .env("GIT_DIR", current.root_path().join(".git"))
        .run()
        .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
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
    fn tests_that_bare_registry_entries_are_local_and_plain_folders_are_skipped() {
        let fixture = TestRepo::with_initial_commit();
        let bare = TestRepo::bare();
        let repo = Repository::at(fixture.root_path()).unwrap();
        let registry = fixture.home_path().join("clean-repositories.json");
        let plain = fixture.root_path().join("plain");
        fs::create_dir(&plain).unwrap();
        fs::write(
            &registry,
            serde_json::to_string(&vec![bare.root_path(), plain.as_path()]).unwrap(),
        )
        .unwrap();
        let before = fs::read(bare.root_path().join("config")).unwrap();
        let selection = load_repositories_at(&repo, true, &[], &registry).unwrap();
        let mut expected = vec![
            fixture.root_path().to_path_buf(),
            bare.root_path().to_path_buf(),
        ];
        expected.sort();
        assert_eq!(selection.repositories, expected);
        assert_eq!(selection.skipped.len(), 1);
        assert_eq!(selection.skipped[0].path, plain);
        assert_eq!(fs::read(bare.root_path().join("config")).unwrap(), before);
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
