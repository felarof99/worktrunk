//! Local-only preservation of uncommitted work. Public entry points resolve
//! HOME (USERPROFILE on Windows); the inner operations take an explicit root.
//!
//! Saves build a private `.pending-*` sibling, write metadata last, then publish
//! with an exclusive rename. A failed copy never publishes a complete entry.
//! Branch names flatten slashes for storage, so lookup also checks the original
//! branch and repository path in metadata to disambiguate name collisions.
//! Changed submodules refuse backup: a gitlink patch cannot hold their files.
//!
//! Restore checks every copy destination before applying the combined HEAD
//! patch. Copies publish without replacing existing files, preserve symlinks,
//! and refuse symlink directory components. Tracked files must be clean before
//! patching. Entries survive both successful and failed restores until expiry.
//! Expiry walks exactly repo/branch/entry, never follows directory symlinks,
//! and deletes only directories containing readable, typed metadata.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, ensure};
use worktrunk::git::{CommandError, Repository};
use worktrunk::path::format_path_for_display;
use worktrunk::shell_exec::Cmd;

use super::{AtticEntry, AtticMetadata, BackupResult, Timestamp, WorktreeFacts};

const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

fn attic_root() -> anyhow::Result<PathBuf> {
    Ok(home::home_dir()
        .context("Cannot locate the home directory for the attic")?
        .join(".wt-attic"))
}

// TODO(clean): used by pieces E and F.
/// Save uncommitted files locally, publishing only after every copy succeeds.
pub fn save(
    repo: &Repository,
    facts: &WorktreeFacts,
    backup: &BackupResult,
    now: Timestamp,
) -> anyhow::Result<Option<AtticEntry>> {
    save_in(&attic_root()?, repo, facts, backup, now)
}

/// Find the newest complete entry matching both the repository and branch.
pub fn load_latest(repo: &Repository, branch: &str) -> anyhow::Result<Option<AtticEntry>> {
    load_latest_in(&attic_root()?, repo, branch)
}

/// Restore the combined patch and saved files without replacing user changes.
pub fn apply(entry: &AtticEntry, worktree: &Path) -> anyhow::Result<()> {
    ensure!(is_directory(&entry.path)?, "Attic entry is not a directory");
    let patch_path = entry.path.join("tracked.patch");
    ensure!(
        fs::symlink_metadata(&patch_path)?.is_file(),
        "Attic patch is not a regular file"
    );
    let patch = fs::read(&patch_path).context("Failed to read attic patch")?;
    let worktree = dunce::canonicalize(worktree).context("Failed to locate restored worktree")?;
    let mut files = Vec::new();
    let mut destinations = BTreeSet::new();
    for group in ["untracked", "ignored-env"] {
        let source = entry.path.join(group);
        ensure!(is_directory(&source)?, "Missing attic directory: {group}");
        for relative in leaf_paths(&source)? {
            validate_relative(&relative)?;
            ensure!(
                destinations.insert(relative.clone()),
                "Duplicate attic path: {}",
                format_path_for_display(&relative)
            );
            check_destination(&worktree, &relative)?;
            files.push((source.join(&relative), relative));
        }
    }
    if !patch.is_empty() {
        let repo = Repository::at(&worktree)?;
        let args = ["diff", "--quiet", "--no-ext-diff", "HEAD", "--"];
        let output = repo.worktree_at(&worktree).run_command_output(&args)?;
        match output.status.code() {
            Some(0) => {}
            Some(1) => anyhow::bail!("Cannot apply attic patch: worktree has tracked changes"),
            _ => return Err(CommandError::from_failed_output("git", &args, &output).into()),
        }
        for check in [true, false] {
            let mut args = vec!["apply", "--binary", "--whitespace=nowarn"];
            if check {
                args.push("--check");
            }
            let output = Cmd::new("git")
                .args(args.iter().copied())
                .current_dir(&worktree)
                .scrub_git_discovery_env()
                .stdin_bytes(patch.clone())
                .run()?;
            if !output.status.success() {
                return Err(CommandError::from_failed_output("git", &args, &output).into());
            }
        }
    }
    for (source, relative) in files {
        copy_file(&source, &worktree, &relative)?;
    }
    Ok(())
}

// TODO(clean): used by piece F, never during dry-run.
/// Delete only recognized entries at least thirty days old, without following links.
pub fn expire(now: Timestamp) -> anyhow::Result<usize> {
    expire_in(&attic_root()?, now)
}

fn save_in(
    root: &Path,
    repo: &Repository,
    facts: &WorktreeFacts,
    backup: &BackupResult,
    now: Timestamp,
) -> anyhow::Result<Option<AtticEntry>> {
    reject_changed_submodules(repo, &facts.path)?;
    let patch = git_bytes(
        repo,
        &facts.path,
        &[
            "diff",
            "--binary",
            "--no-color",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
        ],
    )?;
    let untracked = git_paths(
        repo,
        &facts.path,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    let ignored = git_paths(
        repo,
        &facts.path,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let primary = repo.primary_worktree()?;
    let mut env_files = Vec::new();
    for relative in ignored {
        if !relative
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".env"))
        {
            continue;
        }
        let differs = match &primary {
            Some(primary) => !same_file(&facts.path.join(&relative), &primary.join(&relative))?,
            None => true,
        };
        if differs {
            env_files.push(relative);
        }
    }
    if patch.is_empty()
        && untracked.is_empty()
        && env_files.is_empty()
        && facts.status_porcelain.is_empty()
    {
        return Ok(None);
    }
    let metadata = AtticMetadata {
        repo: repo.repo_path()?.to_path_buf(),
        branch: facts.branch.clone(),
        tip_sha: facts.tip_sha.clone(),
        worktree_path: facts.path.clone(),
        time: now,
        backup_ref: match backup {
            BackupResult::Pushed { reference } => Some(reference.clone()),
            BackupResult::OnRemote => None,
            BackupResult::Failed { reason } => {
                anyhow::bail!("Cannot save attic entry after failed backup: {reason}")
            }
        },
    };
    let branch = entry_branch(&metadata)?;
    let parent = branch_directory(root, repo, &branch)?;
    ensure_directory(root)?;
    ensure_directory(
        parent
            .parent()
            .context("Missing attic repository directory")?,
    )?;
    ensure_directory(&parent)?;
    let pending = tempfile::Builder::new()
        .prefix(".pending-")
        .tempdir_in(&parent)
        .context("Failed to create pending attic entry")?;
    for (group, paths) in [("untracked", untracked), ("ignored-env", env_files)] {
        let destination = pending.path().join(group);
        ensure_directory(&destination)?;
        for relative in paths {
            validate_relative(&relative)?;
            // Validate the source ancestry too: an intervening symlink must
            // not turn a local file into a copy from outside this worktree.
            check_parents(&facts.path, &relative, false)?;
            copy_file(&facts.path.join(&relative), &destination, &relative)?;
        }
    }
    let mut patch_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(pending.path().join("tracked.patch"))?;
    patch_file.write_all(&patch)?;
    patch_file.sync_all()?;
    worktrunk::utils::write_atomically(
        &pending.path().join("meta.json"),
        &serde_json::to_string_pretty(&metadata)?,
    )?;
    let elapsed = now
        .duration_since(UNIX_EPOCH)
        .context("Attic time precedes the Unix epoch")?;
    let timestamp =
        chrono::DateTime::from_timestamp(elapsed.as_secs().try_into()?, elapsed.subsec_nanos())
            .context("Attic time is out of range")?
            .format("%Y%m%dT%H%M%S%.9fZ")
            .to_string();
    // Exclusive publication also protects simultaneous saves and a fixed test clock.
    for suffix in 0u64.. {
        let name = if suffix == 0 {
            timestamp.clone()
        } else {
            format!("{timestamp}-{suffix:020}")
        };
        let path = parent.join(name);
        match renamore::rename_exclusive(pending.path(), &path) {
            Ok(()) => return Ok(Some(AtticEntry { path, metadata })),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("Failed to publish attic entry"),
        }
    }
    anyhow::bail!("No available attic entry name")
}

fn load_latest_in(
    root: &Path,
    repo: &Repository,
    branch: &str,
) -> anyhow::Result<Option<AtticEntry>> {
    let parent = branch_directory(root, repo, branch)?;
    if !is_directory(root)?
        || !is_directory(
            parent
                .parent()
                .context("Missing attic repository directory")?,
        )?
        || !is_directory(&parent)?
    {
        return Ok(None);
    }
    let mut latest: Option<AtticEntry> = None;
    for path in child_directories(&parent)? {
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        let Some(metadata) = read_metadata(&path)? else {
            continue;
        };
        if metadata.repo != repo.repo_path()? || entry_branch(&metadata)? != branch {
            continue;
        }
        ensure!(
            fs::symlink_metadata(path.join("tracked.patch"))?.is_file(),
            "Attic patch is not a regular file"
        );
        for group in ["untracked", "ignored-env"] {
            ensure!(
                is_directory(&path.join(group))?,
                "Incomplete attic entry @ {}",
                format_path_for_display(&path)
            );
        }
        if latest.as_ref().is_none_or(|entry| {
            metadata.time > entry.metadata.time
                || (metadata.time == entry.metadata.time && path > entry.path)
        }) {
            latest = Some(AtticEntry { path, metadata });
        }
    }
    Ok(latest)
}

fn expire_in(root: &Path, now: Timestamp) -> anyhow::Result<usize> {
    let mut count = 0;
    for repo in child_directories(root)? {
        for branch in child_directories(&repo)? {
            for entry in child_directories(&branch)? {
                if entry
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
                {
                    continue;
                }
                // Unknown or unreadable metadata never licenses a deletion.
                let Ok(Some(metadata)) = read_metadata(&entry) else {
                    continue;
                };
                if now
                    .duration_since(metadata.time)
                    .is_ok_and(|age| age >= RETENTION)
                {
                    // remove_dir_all removes symlink leaves themselves; it never
                    // traverses their targets. All three directory levels above
                    // were checked with symlink_metadata/file_type.
                    fs::remove_dir_all(&entry).with_context(|| {
                        format!(
                            "Failed to expire attic entry @ {}",
                            format_path_for_display(&entry)
                        )
                    })?;
                    count += 1;
                }
            }
        }
    }
    Ok(count)
}

fn entry_branch(metadata: &AtticMetadata) -> anyhow::Result<String> {
    match &metadata.branch {
        Some(branch) => Ok(branch.clone()),
        None => Ok(format!(
            "detached-{}",
            metadata
                .worktree_path
                .file_name()
                .context("Missing detached worktree name")?
                .to_string_lossy()
        )),
    }
}

fn branch_directory(root: &Path, repo: &Repository, branch: &str) -> anyhow::Result<PathBuf> {
    let name = repo
        .repo_path()?
        .file_name()
        .context("Missing repository name")?;
    let branch = branch.replace('/', "-");
    validate_relative(Path::new(&branch))?;
    ensure!(
        Path::new(&branch).components().count() == 1,
        "Invalid attic branch name"
    );
    Ok(root.join(name).join(branch))
}

fn git_bytes(repo: &Repository, worktree: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = repo.worktree_at(worktree).run_command_output(args)?;
    if !output.status.success() {
        return Err(CommandError::from_failed_output("git", args, &output).into());
    }
    Ok(output.stdout)
}

fn reject_changed_submodules(repo: &Repository, worktree: &Path) -> anyhow::Result<()> {
    let changes = git_bytes(
        repo,
        worktree,
        &[
            "diff",
            "--raw",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=none",
            "HEAD",
            "--",
        ],
    )?;
    // With -z and --no-renames, each raw record is a header followed by one
    // path. Inspect modes only in headers, never in arbitrary file names.
    let mut fields = changes.split(|byte| *byte == 0);
    while let Some(header) = fields.next().filter(|header| !header.is_empty()) {
        let mut modes = header.split(|byte| *byte == b' ');
        let old = modes.next();
        let new = modes.next();
        ensure!(
            old != Some(b":160000".as_slice()) && new != Some(b"160000".as_slice()),
            "Cannot save changed submodule files in an attic patch; preserve submodule work first"
        );
        fields.next().context("Incomplete raw diff record")?;
    }
    Ok(())
}

fn git_paths(repo: &Repository, worktree: &Path, args: &[&str]) -> anyhow::Result<Vec<PathBuf>> {
    git_bytes(repo, worktree, args)?
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
            }
            #[cfg(not(unix))]
            {
                Ok(PathBuf::from(
                    std::str::from_utf8(bytes).context("Invalid UTF-8 path from git")?,
                ))
            }
        })
        .collect()
}

fn same_file(source: &Path, counterpart: &Path) -> anyhow::Result<bool> {
    let source_meta = fs::symlink_metadata(source)?;
    let other_meta = match fs::symlink_metadata(counterpart) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("Failed to inspect primary env file"),
    };
    if source_meta.file_type() != other_meta.file_type() {
        return Ok(false);
    }
    if source_meta.is_symlink() {
        return Ok(fs::read_link(source)? == fs::read_link(counterpart)?);
    }
    ensure!(
        source_meta.is_file(),
        "Env path is not a regular file @ {}",
        format_path_for_display(source)
    );
    Ok(fs::read(source)? == fs::read(counterpart)?)
}

fn is_directory(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| {
            format!(
                "Failed to inspect directory @ {}",
                format_path_for_display(path)
            )
        }),
    }
}

fn ensure_directory(path: &Path) -> anyhow::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            ensure!(
                is_directory(path)?,
                "Refusing non-directory or symlink @ {}",
                format_path_for_display(path)
            );
            Ok(())
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "Failed to create directory @ {}",
                format_path_for_display(path)
            )
        }),
    }
}

fn child_directories(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !is_directory(path)? {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}

fn read_metadata(entry: &Path) -> anyhow::Result<Option<AtticMetadata>> {
    let path = entry.join("meta.json");
    match fs::symlink_metadata(&path) {
        Ok(metadata) => ensure!(metadata.is_file(), "Attic metadata is not a regular file"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Failed to inspect attic metadata"),
    }
    Ok(Some(
        serde_json::from_slice(&fs::read(&path)?).with_context(|| {
            format!(
                "Failed to read attic metadata @ {}",
                format_path_for_display(&path)
            )
        })?,
    ))
}

fn leaf_paths(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut directories = vec![PathBuf::new()];
    let mut files = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(root.join(&directory))? {
            let entry = entry?;
            let relative = directory.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                directories.push(relative);
            } else {
                files.push(relative);
            }
        }
    }
    Ok(files)
}

fn validate_relative(path: &Path) -> anyhow::Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(name) if name != ".git")),
        "Unsafe attic path: {}",
        format_path_for_display(path)
    );
    Ok(())
}

fn check_parents(root: &Path, relative: &Path, create: bool) -> anyhow::Result<()> {
    validate_relative(relative)?;
    ensure!(
        is_directory(root)?,
        "Not a directory @ {}",
        format_path_for_display(root)
    );
    let mut path = root.to_path_buf();
    for part in relative.parent().into_iter().flat_map(Path::components) {
        path.push(part);
        if create {
            ensure_directory(&path)?;
        } else {
            match fs::symlink_metadata(&path) {
                Ok(metadata) => ensure!(
                    metadata.is_dir(),
                    "Refusing non-directory or symlink @ {}",
                    format_path_for_display(&path)
                ),
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Err(error) => return Err(error).context("Failed to inspect attic file parent"),
            }
        }
    }
    Ok(())
}

fn check_destination(root: &Path, relative: &Path) -> anyhow::Result<()> {
    check_parents(root, relative, false)?;
    let dest = root.join(relative);
    match fs::symlink_metadata(&dest) {
        Ok(_) => anyhow::bail!(
            "Refusing to overwrite existing file @ {}",
            format_path_for_display(&dest)
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("Failed to inspect restore destination"),
    }
}

fn copy_file(source: &Path, root: &Path, relative: &Path) -> anyhow::Result<()> {
    check_destination(root, relative)?;
    check_parents(root, relative, true)?;
    let dest = root.join(relative);
    let metadata = fs::symlink_metadata(source).with_context(|| {
        format!(
            "Failed to inspect file @ {}",
            format_path_for_display(source)
        )
    })?;
    if metadata.is_symlink() {
        let target = fs::read_link(source)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &dest)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTypeExt;
            if metadata.file_type().is_symlink_dir() {
                std::os::windows::fs::symlink_dir(target, &dest)?;
            } else {
                std::os::windows::fs::symlink_file(target, &dest)?;
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = target;
            anyhow::bail!("Symlink copying is unsupported on this platform");
        }
    } else {
        ensure!(
            metadata.is_file(),
            "Cannot copy non-regular file @ {}",
            format_path_for_display(source)
        );
        let mut source_file = fs::File::open(source).with_context(|| {
            format!("Failed to copy file @ {}", format_path_for_display(source))
        })?;
        let mut temp = tempfile::NamedTempFile::new_in(
            dest.parent().context("Missing copy destination parent")?,
        )?;
        io::copy(&mut source_file, &mut temp)?;
        temp.as_file().set_permissions(metadata.permissions())?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(&dest)
            .map_err(|error| error.error)
            .context("Failed to publish copied attic file")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};
    use worktrunk::testing::{TEST_EPOCH, TestRepo};

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    fn now() -> Timestamp {
        UNIX_EPOCH + Duration::from_secs(TEST_EPOCH)
    }

    fn root(repo: &TestRepo) -> PathBuf {
        repo.home_path().join("attic")
    }

    fn facts(repo: &TestRepo, path: &Path, branch: &str) -> WorktreeFacts {
        let wt = repo.repo.worktree_at(path);
        WorktreeFacts {
            path: path.to_path_buf(),
            branch: Some(branch.into()),
            primary: false,
            bare: false,
            pinned: false,
            remove_requested: false,
            lock_reason: None,
            busy: super::super::BusyState {
                processes: vec![],
                terminals: vec![],
                unknown: false,
            },
            last_touch: now(),
            dirty: true,
            tip_sha: wt
                .run_command(&["rev-parse", "HEAD"])
                .unwrap()
                .trim()
                .into(),
            status_porcelain: wt
                .run_command_output(&["status", "--porcelain"])
                .unwrap()
                .stdout,
        }
    }

    fn saved(repo: &TestRepo, path: &Path, branch: &str, time: Timestamp) -> AtticEntry {
        save_in(
            &root(repo),
            &repo.repo,
            &facts(repo, path, branch),
            &BackupResult::OnRemote,
            time,
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn tests_that_nothing_to_save_creates_no_attic() {
        let repo = TestRepo::with_initial_commit();
        assert!(
            save_in(
                &root(&repo),
                &repo.repo,
                &facts(&repo, repo.path(), "main"),
                &BackupResult::OnRemote,
                now()
            )
            .unwrap()
            .is_none()
        );
        assert!(!root(&repo).exists());
    }

    #[test]
    fn tests_that_tracked_changes_and_binary_files_round_trip() {
        let mut repo = TestRepo::with_initial_commit();
        fs::write(repo.path().join("deleted"), "remove me").unwrap();
        fs::write(repo.path().join("renamed"), "rename me").unwrap();
        fs::write(repo.path().join("binary"), [0, 1, 2, 0, 255]).unwrap();
        repo.run_git(&["add", "."]);
        repo.run_git(&["commit", "-m", "base files"]);
        let worktree = repo.add_worktree("feature/auth");
        fs::write(worktree.join("file.txt"), "staged\n").unwrap();
        repo.run_git_in(&worktree, &["add", "file.txt"]);
        fs::write(worktree.join("file.txt"), "staged\nunstaged\n").unwrap();
        fs::write(worktree.join("added"), "staged new file").unwrap();
        repo.run_git_in(&worktree, &["add", "added"]);
        fs::remove_file(worktree.join("deleted")).unwrap();
        repo.run_git_in(&worktree, &["mv", "renamed", "new-name"]);
        fs::write(worktree.join("binary"), [0, 3, 255, 0, 128, 7]).unwrap();
        let entry = saved(&repo, &worktree, "feature/auth", now());
        assert_eq!(
            entry.path.parent().unwrap().file_name().unwrap(),
            "feature-auth"
        );
        let dest = repo.add_worktree("restored");
        apply(&entry, &dest).unwrap();
        assert_eq!(
            fs::read(dest.join("file.txt")).unwrap(),
            b"staged\nunstaged\n"
        );
        assert_eq!(fs::read(dest.join("added")).unwrap(), b"staged new file");
        assert_eq!(
            fs::read(dest.join("binary")).unwrap(),
            [0, 3, 255, 0, 128, 7]
        );
        assert!(!dest.join("deleted").exists());
        assert!(!dest.join("renamed").exists());
        assert_eq!(fs::read(dest.join("new-name")).unwrap(), b"rename me");
        assert!(entry.path.join("meta.json").is_file());
    }

    #[test]
    fn tests_that_git_display_settings_cannot_corrupt_the_patch() {
        let mut repo = TestRepo::with_initial_commit();
        let path = repo.add_worktree("feature/auth");
        repo.run_git(&["config", "color.ui", "always"]);
        repo.run_git(&["config", "diff.noprefix", "true"]);
        fs::write(path.join("file.txt"), "saved change").unwrap();
        let entry = saved(&repo, &path, "feature/auth", now());
        let dest = repo.add_worktree("restored");
        apply(&entry, &dest).unwrap();
        assert_eq!(fs::read(dest.join("file.txt")).unwrap(), b"saved change");
    }

    #[test]
    fn tests_that_submodule_changes_prevent_an_incomplete_backup() {
        let repo = TestRepo::with_initial_commit();
        let submodule = TestRepo::with_initial_commit();
        repo.run_git(&[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            submodule.path().to_str().unwrap(),
            "sub",
        ]);
        repo.run_git(&["commit", "-am", "add submodule"]);
        fs::write(
            repo.path().join("sub/file.txt"),
            "uncommitted submodule data",
        )
        .unwrap();
        let result = save_in(
            &root(&repo),
            &repo.repo,
            &facts(&repo, repo.path(), "main"),
            &BackupResult::OnRemote,
            now(),
        );
        assert!(
            result.is_err(),
            "A gitlink patch cannot preserve the submodule's dirty files"
        );
        assert!(!root(&repo).exists());
    }

    #[test]
    fn tests_that_nested_untracked_files_round_trip() {
        let mut repo = TestRepo::with_initial_commit();
        let path = repo.add_worktree("feature/auth");
        fs::create_dir_all(path.join("nested/deep")).unwrap();
        fs::write(path.join("nested/deep/notes with spaces"), "notes").unwrap();
        let entry = saved(&repo, &path, "feature/auth", now());
        let dest = repo.add_worktree("restored");
        apply(&entry, &dest).unwrap();
        assert_eq!(
            fs::read(dest.join("nested/deep/notes with spaces")).unwrap(),
            b"notes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tests_that_symlinks_and_unusual_paths_round_trip_without_following() {
        use std::os::unix::fs::symlink;
        let mut repo = TestRepo::with_initial_commit();
        let path = repo.add_worktree("feature/auth");
        symlink("missing-target", path.join("link")).unwrap();
        let name = "odd-☃\nname";
        fs::write(path.join(name), "bytes").unwrap();
        let entry = saved(&repo, &path, "feature/auth", now());
        let dest = repo.add_worktree("restored");
        apply(&entry, &dest).unwrap();
        assert_eq!(
            fs::read_link(dest.join("link")).unwrap(),
            Path::new("missing-target")
        );
        assert_eq!(fs::read(dest.join(name)).unwrap(), b"bytes");
    }

    #[test]
    fn tests_that_only_differing_or_missing_ignored_env_files_are_saved() {
        let mut repo = TestRepo::with_initial_commit();
        fs::write(repo.path().join(".gitignore"), ".env*\nignored.log\n").unwrap();
        repo.run_git(&["add", ".gitignore"]);
        repo.run_git(&["commit", "-m", "ignore env"]);
        fs::write(repo.path().join(".env.local"), "primary").unwrap();
        fs::write(repo.path().join(".env.same"), "same").unwrap();
        let path = repo.add_worktree("feature/auth");
        fs::write(path.join(".env.local"), "local secret").unwrap();
        fs::write(path.join(".env.same"), "same").unwrap();
        fs::create_dir(path.join("nested")).unwrap();
        fs::write(path.join("nested/.env.missing"), "new secret").unwrap();
        fs::write(path.join("ignored.log"), "not an env file").unwrap();
        let entry = saved(&repo, &path, "feature/auth", now());
        assert!(!entry.path.join("ignored-env/.env.same").exists());
        assert!(!entry.path.join("ignored-env/ignored.log").exists());
        let dest = repo.add_worktree("restored");
        apply(&entry, &dest).unwrap();
        assert_eq!(fs::read(dest.join(".env.local")).unwrap(), b"local secret");
        assert_eq!(
            fs::read(dest.join("nested/.env.missing")).unwrap(),
            b"new secret"
        );
        fs::write(path.join(".env.local"), "primary").unwrap();
        fs::remove_file(path.join("nested/.env.missing")).unwrap();
        assert!(
            save_in(
                &root(&repo),
                &repo.repo,
                &facts(&repo, &path, "feature/auth"),
                &BackupResult::OnRemote,
                now() + DAY
            )
            .unwrap()
            .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn tests_that_a_copy_failure_never_publishes_an_entry() {
        use std::os::unix::fs::PermissionsExt;
        let mut repo = TestRepo::with_initial_commit();
        let path = repo.add_worktree("feature/auth");
        let file = path.join("unreadable");
        fs::write(&file, "keep me").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = fs::read(&file).is_err();
        let result = save_in(
            &root(&repo),
            &repo.repo,
            &facts(&repo, &path, "feature/auth"),
            &BackupResult::OnRemote,
            now(),
        );
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        if !unreadable {
            return;
        } // Root bypasses permissions, as in the repo's other permission tests.
        assert!(result.is_err());
        assert!(
            load_latest_in(&root(&repo), &repo.repo, "feature/auth")
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read(&file).unwrap(), b"keep me");
    }

    #[test]
    fn tests_that_apply_refuses_existing_files_before_changing_tracked_files() {
        let mut repo = TestRepo::with_initial_commit();
        let path = repo.add_worktree("feature/auth");
        fs::write(path.join("notes"), "saved notes").unwrap();
        fs::write(path.join("file.txt"), "saved tracked").unwrap();
        let entry = saved(&repo, &path, "feature/auth", now());
        let dest = repo.add_worktree("restored");
        fs::write(dest.join("notes"), "existing notes").unwrap();
        assert!(apply(&entry, &dest).is_err());
        assert_eq!(fs::read(dest.join("notes")).unwrap(), b"existing notes");
        assert_eq!(fs::read(dest.join("file.txt")).unwrap(), b"hello");
        assert!(entry.path.join("meta.json").is_file());
    }

    #[test]
    fn tests_that_apply_preserves_dirty_tracked_files() {
        let mut repo = TestRepo::with_initial_commit();
        let path = repo.add_worktree("feature/auth");
        fs::write(path.join("file.txt"), "saved tracked").unwrap();
        let entry = saved(&repo, &path, "feature/auth", now());
        let dest = repo.add_worktree("restored");
        fs::write(dest.join("file.txt"), "existing tracked").unwrap();
        assert!(apply(&entry, &dest).is_err());
        assert_eq!(
            fs::read(dest.join("file.txt")).unwrap(),
            b"existing tracked"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tests_that_apply_refuses_symlink_parents_and_dangling_destinations() {
        use std::os::unix::fs::symlink;
        let mut repo = TestRepo::with_initial_commit();
        let path = repo.add_worktree("feature/auth");
        fs::create_dir(path.join("nested")).unwrap();
        fs::write(path.join("nested/notes"), "saved").unwrap();
        let entry = saved(&repo, &path, "feature/auth", now());
        let dest = repo.add_worktree("restored");
        symlink(repo.path(), dest.join("nested")).unwrap();
        assert!(apply(&entry, &dest).is_err());
        assert!(!repo.path().join("notes").exists());
        fs::remove_file(dest.join("nested")).unwrap();
        fs::create_dir(dest.join("nested")).unwrap();
        symlink("missing", dest.join("nested/notes")).unwrap();
        assert!(apply(&entry, &dest).is_err());
        assert_eq!(
            fs::read_link(dest.join("nested/notes")).unwrap(),
            Path::new("missing")
        );
    }

    #[test]
    fn tests_that_latest_ignores_incomplete_entries_and_disambiguates_branches() {
        let mut repo = TestRepo::with_initial_commit();
        let path = repo.add_worktree("feature/auth");
        fs::write(path.join("notes"), "old").unwrap();
        let old = saved(&repo, &path, "feature/auth", now() - DAY);
        fs::write(path.join("notes"), "new").unwrap();
        let new = saved(&repo, &path, "feature/auth", now());
        let collision_path = repo.home_path().join("other-worktree");
        let collision = repo.add_worktree_at_path("feature-auth", &collision_path);
        fs::write(collision.join("notes"), "other branch").unwrap();
        saved(&repo, &collision, "feature-auth", now() + DAY);
        let pending = new.path.parent().unwrap().join(".pending-crash");
        fs::create_dir(&pending).unwrap();
        fs::copy(old.path.join("meta.json"), pending.join("meta.json")).unwrap();
        assert_eq!(
            load_latest_in(&root(&repo), &repo.repo, "feature/auth")
                .unwrap()
                .unwrap()
                .path,
            new.path
        );
    }

    #[test]
    fn tests_that_expiry_uses_the_30_day_boundary_and_keeps_unrelated_folders() {
        let mut repo = TestRepo::with_initial_commit();
        let path = repo.add_worktree("feature/auth");
        fs::write(path.join("notes"), "saved").unwrap();
        let young = saved(&repo, &path, "feature/auth", now() - DAY * 29);
        let old = saved(&repo, &path, "feature/auth", now() - DAY * 30);
        let unknown = old.path.parent().unwrap().join("unrelated");
        fs::create_dir(&unknown).unwrap();
        fs::write(unknown.join("precious"), "keep").unwrap();
        let invalid = old.path.parent().unwrap().join("invalid");
        fs::create_dir(&invalid).unwrap();
        fs::write(invalid.join("meta.json"), "invalid json").unwrap();
        assert_eq!(expire_in(&root(&repo), now()).unwrap(), 1);
        assert!(!old.path.exists());
        assert!(young.path.exists());
        assert!(unknown.join("precious").exists());
        assert!(invalid.exists());
    }

    #[cfg(unix)]
    #[test]
    fn tests_that_expiry_never_follows_symlinks_outside_the_attic() {
        use std::os::unix::fs::symlink;
        let mut repo = TestRepo::with_initial_commit();
        let path = repo.add_worktree("feature/auth");
        fs::write(path.join("notes"), "saved").unwrap();
        let entry = saved(&repo, &path, "feature/auth", now() - DAY * 30);
        let outside = repo.home_path().join("outside");
        fs::rename(&entry.path, &outside).unwrap();
        symlink(&outside, &entry.path).unwrap();
        assert_eq!(expire_in(&root(&repo), now()).unwrap(), 0);
        assert!(outside.join("meta.json").exists());
        assert_eq!(expire_in(&entry.path, now()).unwrap(), 0);
        assert!(outside.join("meta.json").exists());
    }
}
