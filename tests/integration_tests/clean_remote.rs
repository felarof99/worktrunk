//! Real git advertisements and pushes against temporary bare remotes. Included
//! by the binary's remote module so these tests exercise its actual shared types.

use std::path::Path;
use std::time::SystemTime;

use worktrunk::git::Repository;
use worktrunk::testing::TestRepo;

use super::super::BusyState;
use super::{BackupResult, WorktreeFacts, backup, fetch_snapshot, tip_on_remote};

fn feature(branch: &str) -> TestRepo {
    let test = TestRepo::standard_main_only();
    test.run_git(&["switch", "-c", branch]);
    test.commit("local work");
    test
}

fn facts(test: &TestRepo) -> WorktreeFacts {
    let branch = test.git_output(&["branch", "--show-current"]);
    WorktreeFacts {
        path: test.root_path().to_path_buf(),
        branch: (!branch.is_empty()).then_some(branch),
        primary: false,
        bare: false,
        pinned: false,
        remove_requested: false,
        lock_reason: None,
        busy: BusyState {
            processes: vec![],
            terminals: vec![],
            unknown: false,
        },
        last_touch: SystemTime::UNIX_EPOCH,
        dirty: false,
        tip_sha: test.git_output(&["rev-parse", "HEAD"]),
        status_porcelain: vec![],
    }
}

fn git_at(test: &TestRepo, path: &Path, args: &[&str]) -> String {
    let output = test
        .git_command()
        .current_dir(path)
        .args(args.iter().copied())
        .run()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn remote_sha(test: &TestRepo, reference: &str) -> String {
    git_at(test, test.remote_path().unwrap(), &["rev-parse", reference])
}

fn assert_pushed(test: &TestRepo, reference: &str) {
    let repo = Repository::at(test.root_path()).unwrap();
    let facts = facts(test);
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert!(!tip_on_remote(&repo, &facts, &snapshot).unwrap());
    assert_eq!(
        backup(&repo, &facts, &snapshot).unwrap(),
        BackupResult::Pushed {
            reference: reference.to_string()
        }
    );
    assert_eq!(remote_sha(test, reference), facts.tip_sha);
}

fn assert_failed(result: anyhow::Result<BackupResult>, expected: &str) {
    let BackupResult::Failed { reason } = result.unwrap() else {
        panic!("backup must fail");
    };
    assert!(
        reason.contains(expected),
        "{reason:?} should contain {expected:?}"
    );
    assert!(
        !reason.contains('\n'),
        "failure reasons must fit one line: {reason:?}"
    );
}

#[test]
fn tests_that_snapshot_keeps_all_refs_and_the_advertised_default_branch() {
    let test = feature("feature");
    test.run_git(&["tag", "-a", "release", "-m", "release"]);
    test.run_git(&[
        "push",
        "origin",
        "HEAD:refs/pull/1/head",
        "refs/tags/release",
    ]);
    test.run_git_in(
        test.remote_path().unwrap(),
        &["symbolic-ref", "HEAD", "refs/heads/main"],
    );
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert_eq!(snapshot.remote, "origin");
    assert_eq!(
        snapshot.default_branch_ref.as_deref(),
        Some("refs/heads/main")
    );
    assert_eq!(snapshot.refs["HEAD"], snapshot.refs["refs/heads/main"]);
    assert_eq!(snapshot.refs["refs/pull/1/head"], facts(&test).tip_sha);
    assert_eq!(snapshot.refs["refs/tags/release^{}"], facts(&test).tip_sha);
    assert_eq!(
        snapshot.refs["refs/tags/release"],
        test.git_output(&["rev-parse", "release"])
    );
}

#[test]
fn tests_that_snapshot_uses_the_existing_primary_remote_selection() {
    let test = TestRepo::standard_main_only();
    let other = TestRepo::bare();
    test.run_git(&[
        "remote",
        "add",
        "preferred",
        other.root_path().to_str().unwrap(),
    ]);
    test.run_git(&["config", "checkout.defaultRemote", "preferred"]);
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert_eq!(snapshot.remote, "preferred");
    assert!(snapshot.refs.is_empty());
    assert_eq!(snapshot.default_branch_ref, None);
}

#[test]
fn tests_that_an_exact_branch_head_needs_no_backup_push() {
    let test = feature("feature");
    test.run_git(&["push", "origin", "feature"]);
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert!(tip_on_remote(&repo, &facts(&test), &snapshot).unwrap());
    assert_eq!(
        backup(&repo, &facts(&test), &snapshot).unwrap(),
        BackupResult::OnRemote
    );
    assert!(
        test.git_output(&["ls-remote", "origin", "refs/heads/wt-backup/*"])
            .is_empty()
    );
}

#[test]
fn tests_that_an_exact_pull_request_head_counts_as_backed_up() {
    let test = feature("feature");
    test.run_git(&["push", "origin", "HEAD:refs/pull/1/head"]);
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert_eq!(
        backup(&repo, &facts(&test), &snapshot).unwrap(),
        BackupResult::OnRemote
    );
}

#[test]
fn tests_that_ancestry_of_the_advertised_default_branch_counts() {
    let test = feature("feature");
    let saved = facts(&test);
    test.commit("later work");
    test.run_git(&["push", "origin", "HEAD:main"]);
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert!(!snapshot.refs.values().any(|sha| sha == &saved.tip_sha));
    assert_eq!(
        backup(&repo, &saved, &snapshot).unwrap(),
        BackupResult::OnRemote
    );
}

#[test]
fn tests_that_ancestry_of_another_branch_is_not_enough() {
    let test = feature("feature");
    let saved = facts(&test);
    test.commit("later work");
    test.run_git(&["push", "origin", "feature"]);
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert!(!tip_on_remote(&repo, &saved, &snapshot).unwrap());
}

#[test]
fn tests_that_a_missing_default_branch_object_is_not_fetched() {
    let test = feature("feature");
    test.run_git(&["push", "origin", "HEAD:main"]);
    let remote = test.remote_path().unwrap();
    let tree = remote_sha(&test, "main^{tree}");
    let next = git_at(
        &test,
        remote,
        &["commit-tree", &tree, "-p", "main", "-m", "remote work"],
    );
    test.run_git_in(remote, &["update-ref", "refs/heads/main", &next]);
    // A promisor remote normally permits cat-file to fetch missing objects.
    test.run_git(&["config", "remote.origin.promisor", "true"]);
    test.run_git(&["config", "extensions.partialClone", "origin"]);
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert!(
        !test
            .git_command()
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "-e", &next])
            .run()
            .unwrap()
            .status
            .success()
    );
    assert!(!tip_on_remote(&repo, &facts(&test), &snapshot).unwrap());
    assert!(
        !test
            .git_command()
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "-e", &next])
            .run()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn tests_that_a_tip_ahead_of_its_remote_branch_is_pushed_only_to_backup() {
    let test = feature("feature");
    test.run_git(&["push", "origin", "feature"]);
    let original = facts(&test).tip_sha;
    test.commit("unpushed work");
    assert_pushed(&test, "refs/heads/wt-backup/feature");
    assert_eq!(remote_sha(&test, "refs/heads/feature"), original);
}

#[test]
fn tests_that_backup_never_follows_tags() {
    let test = feature("feature");
    test.run_git(&["tag", "-a", "private-tag", "-m", "local tag"]);
    test.run_git(&["config", "push.followTags", "true"]);
    assert_pushed(&test, "refs/heads/wt-backup/feature");
    assert!(
        test.git_output(&["ls-remote", "origin", "refs/tags/*"])
            .is_empty()
    );
}

#[test]
fn tests_that_replacement_commits_cannot_prove_remote_ancestry() {
    let test = feature("feature");
    let default_tip = test.git_output(&["rev-parse", "main"]);
    test.run_git(&["replace", "--graft", &default_tip, "HEAD"]);
    assert!(
        test.git_command()
            .args(["merge-base", "--is-ancestor", "HEAD", "main"])
            .run()
            .unwrap()
            .status
            .success()
    );
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert!(!tip_on_remote(&repo, &facts(&test), &snapshot).unwrap());
}

#[test]
fn tests_that_grafted_history_cannot_prove_remote_ancestry() {
    let test = feature("feature");
    let default_tip = test.git_output(&["rev-parse", "main"]);
    let tip = facts(&test).tip_sha;
    let grafts = test.root_path().join(".git/info/grafts");
    std::fs::write(grafts, format!("{default_tip} {tip}\n")).unwrap();
    assert!(
        test.git_command()
            .args(["merge-base", "--is-ancestor", "HEAD", "main"])
            .run()
            .unwrap()
            .status
            .success()
    );
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert!(!tip_on_remote(&repo, &facts(&test), &snapshot).unwrap());
}

#[test]
fn tests_that_invalid_detached_folder_names_fail_without_pushing() {
    let test = feature("feature");
    let mut facts = facts(&test);
    facts.branch = None;
    facts.path = test.root_path().join("work tree");
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert_failed(
        backup(&repo, &facts, &snapshot),
        "Invalid backup branch name",
    );
    assert!(
        test.git_output(&["ls-remote", "origin", "refs/heads/wt-backup/*"])
            .is_empty()
    );
}

#[test]
fn tests_that_unpublished_branches_preserve_slashes_in_the_backup_name() {
    for branch in ["feature", "feature/topic"] {
        let test = feature(branch);
        assert_pushed(&test, &format!("refs/heads/wt-backup/{branch}"));
    }
}

#[test]
fn tests_that_detached_heads_use_the_worktree_folder_name() {
    let test = feature("feature");
    test.run_git(&["checkout", "--detach"]);
    assert_eq!(facts(&test).branch, None);
    let folder = test.root_path().file_name().unwrap().to_str().unwrap();
    assert_pushed(&test, &format!("refs/heads/wt-backup/detached-{folder}"));
}

#[test]
fn tests_that_existing_backup_refs_are_force_updated() {
    let test = feature("feature");
    let saved = facts(&test);
    test.commit("later backup");
    test.run_git(&["push", "origin", "HEAD:refs/heads/wt-backup/feature"]);
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert_ne!(snapshot.refs["refs/heads/wt-backup/feature"], saved.tip_sha);
    assert_eq!(
        backup(&repo, &saved, &snapshot).unwrap(),
        BackupResult::Pushed {
            reference: "refs/heads/wt-backup/feature".into()
        }
    );
    assert_eq!(
        remote_sha(&test, "refs/heads/wt-backup/feature"),
        saved.tip_sha
    );
}

#[test]
fn tests_that_no_remote_is_an_error() {
    let test = TestRepo::with_initial_commit();
    let error = fetch_snapshot(&test.repo).unwrap_err();
    assert!(error.to_string().contains("no remote"), "{error:#}");
}

#[test]
fn tests_that_an_unreachable_remote_is_an_error_not_an_empty_snapshot() {
    let test = TestRepo::standard_main_only();
    test.run_git(&[
        "remote",
        "set-url",
        "origin",
        test.root_path().join("missing").to_str().unwrap(),
    ]);
    let repo = Repository::at(test.root_path()).unwrap();
    assert!(fetch_snapshot(&repo).is_err());
}

#[test]
fn tests_that_a_remote_lost_after_the_snapshot_fails_the_backup() {
    let test = feature("feature");
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    test.run_git(&[
        "remote",
        "set-url",
        "origin",
        test.root_path().join("missing").to_str().unwrap(),
    ]);
    assert_failed(backup(&repo, &facts(&test), &snapshot), "push");
}

#[cfg(unix)]
fn hook(test: &TestRepo, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = test.remote_path().unwrap().join("hooks").join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn tests_that_a_rejected_push_is_a_failed_backup() {
    let test = feature("feature");
    hook(
        &test,
        "pre-receive",
        "#!/bin/sh
echo 'backup forbidden' >&2
exit 1
",
    );
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert_failed(backup(&repo, &facts(&test), &snapshot), "push");
    assert!(
        test.git_output(&["ls-remote", "origin", "refs/heads/wt-backup/*"])
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn tests_that_confirmation_rejects_a_different_sha() {
    let test = feature("feature");
    hook(
        &test,
        "post-receive",
        "#!/bin/sh
git update-ref refs/heads/wt-backup/feature refs/heads/main
",
    );
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert_failed(backup(&repo, &facts(&test), &snapshot), "confirm");
    assert_ne!(
        remote_sha(&test, "refs/heads/wt-backup/feature"),
        facts(&test).tip_sha
    );
}

#[cfg(unix)]
#[test]
fn tests_that_confirmation_rejects_a_missing_ref() {
    let test = feature("feature");
    hook(
        &test,
        "post-receive",
        "#!/bin/sh
git update-ref -d refs/heads/wt-backup/feature
",
    );
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    assert_failed(backup(&repo, &facts(&test), &snapshot), "confirm");
}

#[cfg(unix)]
#[test]
fn tests_that_snapshot_timeout_is_an_error() {
    let test = TestRepo::standard_main_only();
    test.run_git(&["config", "remote.origin.uploadpack", "sh -c 'sleep 120'"]);
    let repo = Repository::at(test.root_path()).unwrap();
    let start = std::time::Instant::now();
    let error = fetch_snapshot(&repo).unwrap_err();
    assert!(
        error.chain().any(|cause| cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::TimedOut)),
        "{error:#}"
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(90));
}

#[cfg(unix)]
#[test]
fn tests_that_push_timeout_is_a_failed_backup() {
    let test = feature("feature");
    test.run_git(&["config", "remote.origin.receivepack", "sh -c 'sleep 120'"]);
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    let start = std::time::Instant::now();
    assert_failed(backup(&repo, &facts(&test), &snapshot), "timed out");
    assert!(start.elapsed() < std::time::Duration::from_secs(90));
}

#[cfg(unix)]
#[test]
fn tests_that_confirmation_timeout_is_a_failed_backup() {
    let test = feature("feature");
    let repo = Repository::at(test.root_path()).unwrap();
    let snapshot = fetch_snapshot(&repo).unwrap();
    test.run_git(&["config", "remote.origin.uploadpack", "sh -c 'sleep 120'"]);
    let start = std::time::Instant::now();
    assert_failed(backup(&repo, &facts(&test), &snapshot), "timed out");
    assert!(start.elapsed() < std::time::Duration::from_secs(90));
}
