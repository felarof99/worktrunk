//! Restore exercises the real CLI against temporary repos and local bare remotes.
//! Every child gets a temporary HOME/USERPROFILE, including its .wt-attic.

use std::fs;
use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, UNIX_EPOCH};

use insta::assert_snapshot;

use crate::common::{TEST_EPOCH, TestRepo};
use worktrunk::testing::set_temp_home_env;

fn restore(repo: &TestRepo, branch: &str, yes: bool) -> Output {
    let mut cmd = repo.wt_command();
    set_temp_home_env(&mut cmd, repo.home_path());
    cmd.env_remove("CLICOLOR_FORCE").env("NO_COLOR", "1");
    if yes {
        cmd.arg("-y");
    }
    cmd.args(["clean", "restore", branch]).output().unwrap()
}

fn fixture() -> TestRepo {
    let repo = TestRepo::with_initial_commit();
    repo.write_test_config("worktree-path = '../recovered/{{ branch | sanitize }}'");
    repo
}

fn recovered(repo: &TestRepo) -> PathBuf {
    dunce::canonicalize(repo.home_path())
        .unwrap()
        .join("recovered/feature-auth")
}

fn assert_restored(repo: &TestRepo, output: Output, tip: &str) {
    assert!(output.status.success(), "{output:?}");
    let path = recovered(repo);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        path.to_str().unwrap()
    );
    assert_eq!(
        repo.git_command()
            .current_dir(&path)
            .args(["rev-parse", "HEAD"])
            .run()
            .unwrap()
            .stdout,
        format!("{tip}\n").as_bytes()
    );
    assert_eq!(
        repo.git_command()
            .current_dir(&path)
            .args(["branch", "--show-current"])
            .run()
            .unwrap()
            .stdout,
        b"feature/auth\n"
    );
}

fn remote(repo: &TestRepo, reference: &str) -> TestRepo {
    let remote = TestRepo::bare();
    repo.run_git(&["remote", "add", "origin", remote.path().to_str().unwrap()]);
    repo.run_git(&["push", "origin", &format!("HEAD:{reference}")]);
    remote
}

#[test]
fn tests_that_restore_uses_the_local_branch_and_configured_path() {
    let repo = fixture();
    repo.run_git(&["branch", "feature/auth"]);
    let tip = repo.git_output(&["rev-parse", "feature/auth"]);
    let output = restore(&repo, "feature/auth", true);
    assert_snapshot!(String::from_utf8_lossy(&output.stderr), @r###"◎ Restoring worktree for feature/auth...
✓ Created worktree for feature/auth @ ~/recovered/feature-auth"###);
    assert_restored(&repo, output, &tip);
}

#[test]
fn tests_that_restore_prefers_the_local_branch_over_the_remote() {
    let repo = fixture();
    let _remote = remote(&repo, "refs/heads/feature/auth");
    repo.commit("local change");
    repo.run_git(&["branch", "feature/auth"]);
    let tip = repo.git_output(&["rev-parse", "feature/auth"]);
    assert_ne!(tip, repo.git_output(&["rev-parse", "origin/feature/auth"]));
    assert_restored(&repo, restore(&repo, "feature/auth", true), &tip);
}

#[test]
fn tests_that_restore_uses_the_normal_remote_branch() {
    let repo = fixture();
    let _remote = remote(&repo, "refs/heads/feature/auth");
    let tip = repo.git_output(&["rev-parse", "origin/feature/auth"]);
    assert!(
        !repo
            .git_command()
            .args(["show-ref", "--verify", "refs/heads/feature/auth"])
            .run()
            .unwrap()
            .status
            .success()
    );
    assert_restored(&repo, restore(&repo, "feature/auth", true), &tip);
}

#[test]
fn tests_that_restore_fetches_only_the_backup_ref() {
    let repo = fixture();
    let remote = remote(&repo, "refs/heads/wt-backup/feature/auth");
    repo.run_git(&[
        "update-ref",
        "-d",
        "refs/remotes/origin/wt-backup/feature/auth",
    ]);
    remote.run_git(&["branch", "unrelated", "wt-backup/feature/auth"]);
    let tip = repo.git_output(&["rev-parse", "HEAD"]);
    assert_restored(&repo, restore(&repo, "feature/auth", true), &tip);
    assert!(
        !repo
            .git_command()
            .args(["show-ref", "--verify", "refs/remotes/origin/unrelated"])
            .run()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn tests_that_restore_leaves_an_existing_worktree_unchanged() {
    let mut repo = fixture();
    let path = repo.add_worktree("feature/auth");
    fs::write(path.join("file.txt"), "keep this change").unwrap();
    let output = restore(&repo, "feature/auth", true);
    assert!(output.status.success(), "{output:?}");
    assert_snapshot!(String::from_utf8_lossy(&output.stderr), @r###"○ Worktree for feature/auth already exists @ ~/repo.feature-auth"###);
    assert_eq!(
        fs::read_to_string(path.join("file.txt")).unwrap(),
        "keep this change"
    );
    assert!(!recovered(&repo).exists());
}

#[test]
fn tests_that_restore_keeps_an_occupied_destination() {
    let repo = fixture();
    repo.run_git(&["branch", "feature/auth"]);
    let path = recovered(&repo);
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("valuable"), "keep").unwrap();
    let output = restore(&repo, "feature/auth", true);
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(fs::read_to_string(path.join("valuable")).unwrap(), "keep");
    assert!(!path.join(".git").exists());
}

#[test]
fn tests_that_restore_applies_the_latest_attic_from_the_isolated_home() {
    let repo = fixture();
    repo.run_git(&["branch", "feature/auth"]);
    let tip = repo.git_output(&["rev-parse", "HEAD"]);
    let root = repo.home_path().join(".wt-attic/repo/feature-auth");
    for (name, age, contents) in [("old", 10, "old notes"), ("new", 0, "latest notes")] {
        let entry = root.join(name);
        fs::create_dir_all(entry.join("untracked/nested")).unwrap();
        fs::create_dir_all(entry.join("ignored-env")).unwrap();
        fs::write(entry.join("tracked.patch"), "").unwrap();
        fs::write(entry.join("untracked/nested/notes"), contents).unwrap();
        let metadata = serde_json::json!({
            "repo": repo.path(), "branch": "feature/auth", "tip_sha": tip,
            "worktree_path": recovered(&repo),
            "time": UNIX_EPOCH + Duration::from_secs(TEST_EPOCH - age),
            "backup_ref": null,
        });
        fs::write(
            entry.join("meta.json"),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
    }
    assert_restored(&repo, restore(&repo, "feature/auth", true), &tip);
    assert_eq!(
        fs::read_to_string(recovered(&repo).join("nested/notes")).unwrap(),
        "latest notes"
    );
    assert!(root.join("new/meta.json").is_file());
}

#[test]
fn tests_that_restore_gates_project_hooks_before_creation() {
    let repo = fixture();
    repo.run_git(&["branch", "feature/auth"]);
    repo.write_project_config("pre-start = 'echo hook > approved-hook'");
    let output = restore(&repo, "feature/auth", false);
    assert!(!output.status.success(), "{output:?}");
    assert!(!recovered(&repo).exists());
    let output = restore(&repo, "feature/auth", true);
    assert!(output.status.success(), "{output:?}");
    assert!(recovered(&repo).join("approved-hook").is_file());
}
