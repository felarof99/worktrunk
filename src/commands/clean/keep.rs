//! Pins and remove-requested marks shared by rm and sweep.
//!
//! Each worktree owns an immutable `<git-dir>/wt/clean-id.json` pointing to
//! its marks in the shared `.git/wt/clean/<unique-id>/marks.json`. The ID is
//! allocated in that shared directory and published without overwriting an
//! existing ID. Git deletes the pointer when unregistering a linked worktree;
//! a new checkout at the same path gets a different ID. Old mark directories
//! remain: a writer already waiting on their lock must never reach a replacement
//! worktree, even if Git reuses the admin directory's name.
//!
//! Reads never create state and fail closed on malformed or unreadable data.
//! Writers lock a sibling file, reload, change one flag, and atomically replace
//! the JSON. The lock stays on disk: unlinking it would split concurrent locks.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use color_print::cformat;
use fs2::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use worktrunk::git::{CommandError, Repository};
use worktrunk::path::format_path_for_display;
use worktrunk::shell_exec::Cmd;
use worktrunk::styling::{eprintln, success_message};
use worktrunk::utils::write_atomically;

use super::{KeepState, registry};

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Marks {
    pinned: bool,
    remove_requested: bool,
}

/// Resolve the optional branch through Repository::resolve_worktree (current
/// worktree when omitted), then pin it or clear the pin with --off. Register
/// the repo and preserve unrelated state when writing the mark.
pub fn run(branch: Option<&str>, off: bool) -> anyhow::Result<()> {
    let repo = Repository::current()?;
    // require_worktree delegates to the shared branch-first canonicalizer.
    let path = repo.require_worktree(branch.unwrap_or("@"))?;
    registry::register(&repo)?;
    set_pin(&repo, &path, !off)?;
    let action = if off {
        "Unpinned worktree (--off)"
    } else {
        "Pinned worktree"
    };
    eprintln!(
        "{}",
        success_message(cformat!(
            "{action} @ <bold>{}</>",
            format_path_for_display(&path)
        ))
    );
    Ok(())
}

// TODO(clean): used by piece A.
/// Load a worktree's pin and remove-requested mark without writing anything.
/// Absent marks mean false; unreadable state is an error rather than permission
/// to remove a possibly pinned worktree.
pub fn load_state(repo: &Repository, worktree: &Path) -> anyhow::Result<KeepState> {
    let identity = identity_path(repo, worktree)?;
    let marks: Marks = match marks_path(repo, &identity)? {
        Some(path) => load_json(&path)?.context("Worktree cleanup marks are missing")?,
        None => Marks::default(),
    };
    Ok(KeepState {
        pinned: marks.pinned,
        remove_requested: marks.remove_requested,
    })
}

// TODO(clean): used by piece D's keep command.
/// Set or clear this worktree's pin while preserving other worktrees' marks
/// and this worktree's remove-requested state. The pin protects both rm and
/// sweep, including sweep's silent-seven-days rule.
pub fn set_pin(repo: &Repository, worktree: &Path, pinned: bool) -> anyhow::Result<()> {
    update(repo, worktree, |marks| marks.pinned = pinned)
}

// TODO(clean): used by pieces E and F.
/// Set the removal mark only after backing up a busy rm request, or clear it
/// after completed removal. Preserve pins and unrelated worktree state; this
/// operation must never run as part of a dry-run preview.
pub fn set_remove_requested(
    repo: &Repository,
    worktree: &Path,
    requested: bool,
) -> anyhow::Result<()> {
    // Successful removal already deleted the marks with the admin directory.
    if !requested
        && matches!(fs::symlink_metadata(worktree), Err(e) if e.kind() == ErrorKind::NotFound)
    {
        return Ok(());
    }
    update(repo, worktree, |marks| marks.remove_requested = requested)
}

fn identity_path(repo: &Repository, worktree: &Path) -> anyhow::Result<PathBuf> {
    let worktree = repo.worktree_at(worktree);
    worktree.ensure_holds_this_worktree()?;
    // This lookup names the selected worktree, so neither cached discovery nor
    // an inherited GIT_DIR may redirect its marks to another checkout.
    let args = ["rev-parse", "--absolute-git-dir"];
    let output = Cmd::new("git")
        .args(args)
        .current_dir(worktree.path())
        .context(worktree.path().display().to_string())
        .scrub_git_discovery_env()
        .run()?;
    if !output.status.success() {
        return Err(CommandError::from_failed_output("git", &args, &output).into());
    }
    let dir = dunce::canonicalize(String::from_utf8(output.stdout)?.trim())
        .context("Failed to resolve worktree admin directory")?;
    Ok(dir.join("wt/clean-id.json"))
}

fn marks_path(repo: &Repository, identity: &Path) -> anyhow::Result<Option<PathBuf>> {
    let Some(id) = load_json::<String>(identity)? else {
        return Ok(None);
    };
    // Only IDs this module allocates may select a state directory; an edited
    // pointer must not redirect a mark update to an arbitrary path.
    anyhow::ensure!(
        id.strip_prefix("worktree-").is_some_and(
            |suffix| !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_alphanumeric())
        ),
        "Invalid worktree cleanup identity"
    );
    Ok(Some(
        repo.wt_dir().join("clean").join(id).join("marks.json"),
    ))
}

fn create_state(repo: &Repository, identity: &Path) -> anyhow::Result<PathBuf> {
    PendingState::prepare(repo, identity)?.publish(repo, identity)
}

/// Own unpublished files until an identity has been published or another first
/// writer won. Dropping a failed preparation removes only its scratch state.
struct PendingState {
    pointer: tempfile::NamedTempFile,
    generation: tempfile::TempDir,
}

impl PendingState {
    fn prepare(repo: &Repository, identity: &Path) -> anyhow::Result<Self> {
        let parent = identity
            .parent()
            .context("Cleanup identity has no parent directory")?;
        create_state_dir(parent)?;
        let pointer = tempfile::NamedTempFile::new_in(parent)?;
        create_state_dir(&repo.wt_dir())?;
        let store = repo.wt_dir().join("clean");
        create_state_dir(&store)?;
        let generation = tempfile::Builder::new()
            .prefix("worktree-")
            .tempdir_in(&store)?;
        write_atomically(
            &generation.path().join("marks.json"),
            &serde_json::to_string(&Marks::default())?,
        )?;
        Ok(Self {
            pointer,
            generation,
        })
    }

    fn publish(mut self, repo: &Repository, identity: &Path) -> anyhow::Result<PathBuf> {
        let id = self
            .generation
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .context("Invalid cleanup state directory name")?;
        let path = self.generation.path().join("marks.json");
        // This is an unpublished, empty scratch file. Write its open handle:
        // replacing its pathname could resurrect it in a replacement worktree
        // after the original admin directory was deleted. Publication below
        // is atomic and never overwrites an existing identity.
        self.pointer.write_all(&serde_json::to_vec(id)?)?;
        self.pointer.as_file().sync_all()?;
        match self.pointer.persist_noclobber(identity) {
            Ok(_) => {
                let _ = self.generation.keep();
                Ok(path)
            }
            Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {
                let winner =
                    marks_path(repo, identity)?.context("Worktree cleanup identity disappeared")?;
                // Read the winner BEFORE checking our unique scratch pointer.
                // A replaced admin directory cannot pass this check and make
                // its new identity look like a competing first writer's ID.
                anyhow::ensure!(
                    load_json::<String>(error.file.path())?.as_deref() == Some(id),
                    "Worktree changed during cleanup state initialization"
                );
                Ok(winner)
            }
            Err(error) => Err(error.error).context("Failed to create worktree cleanup identity"),
        }
    }
}

fn create_state_dir(path: &Path) -> anyhow::Result<()> {
    // Single-level creation never resurrects a deleted Git admin directory.
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).context("Failed to create worktree cleanup state directory"),
    }
}

fn update(
    repo: &Repository,
    worktree: &Path,
    mutate: impl FnOnce(&mut Marks),
) -> anyhow::Result<()> {
    let identity = identity_path(repo, worktree)?;
    let path = match marks_path(repo, &identity)? {
        Some(path) => path,
        None => create_state(repo, &identity)?,
    };
    let _lock = lock_state(&path)?;
    let mut marks: Marks = load_json(&path)?.context("Worktree cleanup marks are missing")?;
    mutate(&mut marks);
    anyhow::ensure!(
        marks_path(repo, &identity)?.as_ref() == Some(&path),
        "Worktree changed during cleanup state update"
    );
    // A replacement after this check still cannot redirect the write: `path`
    // names this generation, never the reusable checkout/admin pathname.
    write_atomically(&path, &serde_json::to_string(&marks)?).with_context(|| {
        format!(
            "Failed to save cleanup marks @ {}",
            format_path_for_display(&path)
        )
    })
}

/// Shared state readers distinguish absence from unreadable or dangling files.
pub(super) fn load_json<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error)
            if error.kind() == ErrorKind::NotFound
                && matches!(fs::symlink_metadata(path), Err(e) if e.kind() == ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "Failed to read cleanup state @ {}",
                    format_path_for_display(path)
                )
            });
        }
    };
    serde_json::from_str(&content)
        .map(Some)
        .with_context(|| format!("Invalid cleanup state @ {}", format_path_for_display(path)))
}

/// The parent must already exist; callers decide which directories may be created.
pub(super) fn lock_state(path: &Path) -> anyhow::Result<File> {
    let lock = path.with_extension("json.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)
        .with_context(|| {
            format!(
                "Failed to open cleanup state lock @ {}",
                format_path_for_display(&lock)
            )
        })?;
    file.lock_exclusive().with_context(|| {
        format!(
            "Failed to lock cleanup state @ {}",
            format_path_for_display(path)
        )
    })?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};

    use worktrunk::testing::TestRepo;

    use super::*;

    fn state_file(repo: &Repository, path: &Path) -> PathBuf {
        marks_path(repo, &identity_path(repo, path).unwrap())
            .unwrap()
            .unwrap()
    }

    fn assert_marks(repo: &Repository, path: &Path, pinned: bool, requested: bool) {
        let state = load_state(repo, path).unwrap();
        assert_eq!((state.pinned, state.remove_requested), (pinned, requested));
    }

    #[test]
    fn tests_that_pins_and_removal_marks_preserve_each_other() {
        let mut fixture = TestRepo::with_initial_commit();
        let path = fixture.add_worktree("feature");
        let repo = Repository::at(fixture.root_path()).unwrap();
        assert_marks(&repo, &path, false, false);
        assert!(!identity_path(&repo, &path).unwrap().exists());
        set_pin(&repo, &path, true).unwrap();
        assert_marks(&repo, &path, true, false);
        set_remove_requested(&repo, &path, true).unwrap();
        assert_marks(&repo, &path, true, true);
        set_pin(&repo, &path, false).unwrap();
        assert_marks(&repo, &path, false, true);
        set_remove_requested(&repo, &path, false).unwrap();
        assert_marks(&repo, &path, false, false);
    }

    #[test]
    fn tests_that_worktrees_have_independent_marks() {
        let mut fixture = TestRepo::with_initial_commit();
        let first = fixture.add_worktree("first");
        let second = fixture.add_worktree("second");
        let repo = Repository::at(fixture.root_path()).unwrap();
        set_pin(&repo, &first, true).unwrap();
        set_remove_requested(&repo, &second, true).unwrap();
        set_pin(&repo, fixture.root_path(), true).unwrap();
        assert_marks(&repo, &first, true, false);
        assert_marks(&repo, &second, false, true);
        assert_marks(&repo, fixture.root_path(), true, false);
        set_pin(&repo, &second, true).unwrap();
        set_remove_requested(&repo, &second, false).unwrap();
        assert_marks(&repo, &first, true, false);
        assert_marks(&repo, &second, true, false);
    }

    #[test]
    fn tests_that_recreated_worktrees_do_not_inherit_marks() {
        let mut fixture = TestRepo::with_initial_commit();
        let path = fixture.add_worktree("feature");
        let repo = Repository::at(fixture.root_path()).unwrap();
        set_pin(&repo, &path, true).unwrap();
        set_remove_requested(&repo, &path, true).unwrap();
        let old_marks = state_file(&repo, &path);
        let identity = identity_path(&repo, &path).unwrap();
        fixture.run_git(&["worktree", "remove", path.to_str().unwrap()]);
        assert!(!identity.exists());
        // Clearing a completed removal must not recreate its admin directory.
        set_remove_requested(&repo, &path, false).unwrap();
        assert!(!identity.exists());
        fixture.run_git(&["worktree", "add", path.to_str().unwrap(), "feature"]);
        assert_marks(&repo, &path, false, false);
        assert!(
            old_marks.exists(),
            "old generations remain isolated from new worktrees"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tests_that_pending_mark_updates_cannot_reach_replacements() {
        for request_removal in [false, true] {
            let mut fixture = TestRepo::with_initial_commit();
            let path = fixture.add_worktree("old");
            let repo = Repository::at(fixture.root_path()).unwrap();
            set_pin(&repo, &path, true).unwrap();
            // The mutation callback runs while the old writer owns its lock.
            // Recreate at this causal boundary, without scheduling or sleeps.
            let result = update(&repo, &path, |marks| {
                fixture.run_git(&["worktree", "remove", path.to_str().unwrap()]);
                fixture.run_git(&[
                    "worktree",
                    "add",
                    "-b",
                    "replacement",
                    path.to_str().unwrap(),
                ]);
                set_pin(&repo, &path, true).unwrap();
                if request_removal {
                    marks.remove_requested = true;
                } else {
                    marks.pinned = false;
                }
            });
            assert_marks(&repo, &path, true, false);
            assert!(
                result.is_err(),
                "obsolete update should report the replaced worktree"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn tests_that_first_writers_refuse_a_replaced_worktree() {
        let mut fixture = TestRepo::with_initial_commit();
        let path = fixture.add_worktree("old");
        let repo = Repository::at(fixture.root_path()).unwrap();
        let identity = identity_path(&repo, &path).unwrap();
        let pending = PendingState::prepare(&repo, &identity).unwrap();
        assert!(
            !identity.exists(),
            "the old worktree has no published identity yet"
        );
        fixture.run_git(&["worktree", "remove", path.to_str().unwrap()]);
        fixture.run_git(&[
            "worktree",
            "add",
            "-b",
            "replacement",
            path.to_str().unwrap(),
        ]);
        set_pin(&repo, &path, true).unwrap();
        let before = fs::read(&identity).unwrap();
        assert!(
            pending.publish(&repo, &identity).is_err(),
            "an obsolete first writer must not adopt the replacement identity"
        );
        assert_eq!(fs::read(&identity).unwrap(), before);
        assert_marks(&repo, &path, true, false);
    }

    #[test]
    fn tests_that_corrupt_or_unreadable_marks_fail_closed() {
        let mut fixture = TestRepo::with_initial_commit();
        let path = fixture.add_worktree("feature");
        let repo = Repository::at(fixture.root_path()).unwrap();
        set_pin(&repo, &path, true).unwrap();
        let state_path = state_file(&repo, &path);
        for content in [
            "not json",
            "{}",
            r#"{"pinned":"yes","remove_requested":false}"#,
        ] {
            fs::write(&state_path, content).unwrap();
            assert!(load_state(&repo, &path).is_err());
            assert!(set_pin(&repo, &path, false).is_err());
            assert!(set_remove_requested(&repo, &path, true).is_err());
            assert_eq!(fs::read_to_string(&state_path).unwrap(), content);
        }
        fs::remove_file(&state_path).unwrap();
        fs::create_dir(&state_path).unwrap();
        assert!(load_state(&repo, &path).is_err());
        assert!(set_pin(&repo, &path, false).is_err());
    }

    #[test]
    fn tests_that_corrupt_identities_and_missing_generations_fail_closed() {
        let mut fixture = TestRepo::with_initial_commit();
        let path = fixture.add_worktree("feature");
        let repo = Repository::at(fixture.root_path()).unwrap();
        set_pin(&repo, &path, true).unwrap();
        let state = state_file(&repo, &path);
        let before = fs::read(&state).unwrap();
        let identity = identity_path(&repo, &path).unwrap();
        for contents in ["not json", r#""../other""#, r#""worktree-missing""#] {
            fs::write(&identity, contents).unwrap();
            assert!(load_state(&repo, &path).is_err());
            assert!(set_pin(&repo, &path, false).is_err());
            assert!(set_remove_requested(&repo, &path, true).is_err());
            assert_eq!(fs::read(&state).unwrap(), before);
            assert_eq!(fs::read_to_string(&identity).unwrap(), contents);
        }
    }

    #[test]
    fn tests_that_marks_reject_unrelated_or_missing_worktrees() {
        let fixture = TestRepo::with_initial_commit();
        let other = TestRepo::with_initial_commit();
        let repo = Repository::at(fixture.root_path()).unwrap();
        let plain = fixture.root_path().join("plain");
        fs::create_dir(&plain).unwrap();
        for path in [other.root_path(), plain.as_path(), &plain.join("missing")] {
            assert!(load_state(&repo, path).is_err());
            assert!(set_pin(&repo, path, true).is_err());
        }
    }

    #[test]
    fn tests_that_concurrent_mark_updates_preserve_both_flags() {
        let mut fixture = TestRepo::with_initial_commit();
        let path = fixture.add_worktree("feature");
        let repo = Repository::at(fixture.root_path()).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            for pin in [true, false] {
                let barrier = barrier.clone();
                let repo = &repo;
                let path = &path;
                scope.spawn(move || {
                    barrier.wait();
                    if pin {
                        set_pin(repo, path, true)
                    } else {
                        set_remove_requested(repo, path, true)
                    }
                    .unwrap();
                });
            }
        });
        assert_marks(&repo, &path, true, true);
    }
}
