//! Keep-command wiring; the policy tables and storage/discovery matrices live
//! beside their binary modules so they exercise the real APIs directly.

use std::fs;

use insta::assert_snapshot;
use worktrunk::git::Repository;

use crate::common::{TestRepo, setup_snapshot_settings};

#[test]
fn tests_that_clean_keep_pins_unpins_and_registers_by_branch_path_or_current() {
    let mut fixture = TestRepo::with_initial_commit();
    let path = fixture.add_worktree("feature/auth");
    let repo = Repository::at(fixture.root_path()).unwrap();
    let admin = repo.worktree_at(&path).git_dir().unwrap();
    let identity = admin.join("wt/clean-id.json");
    let registry = fixture
        .test_config_path()
        .with_file_name("clean-repositories.json");
    let _settings = setup_snapshot_settings(&fixture).bind_to_scope();
    let mut messages = vec![];
    for (args, pinned) in [
        (vec!["clean", "keep", "feature/auth"], true),
        (
            vec!["clean", "keep", path.to_str().unwrap(), "--off"],
            false,
        ),
        (vec!["-C", path.to_str().unwrap(), "clean", "keep"], true),
    ] {
        let output = fixture
            .wt_command()
            .args(args)
            .env_remove("CLICOLOR_FORCE")
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty());
        messages.push(String::from_utf8(output.stderr).unwrap());
        let id: String = serde_json::from_slice(&fs::read(&identity).unwrap()).unwrap();
        let marks = repo.wt_dir().join("clean").join(id).join("marks.json");
        let state: serde_json::Value = serde_json::from_slice(&fs::read(marks).unwrap()).unwrap();
        assert_eq!(state["pinned"], pinned);
        assert_eq!(state["remove_requested"], false);
    }
    let repos: Vec<std::path::PathBuf> =
        serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
    assert_eq!(repos, vec![fixture.root_path().to_path_buf()]);
    assert_snapshot!(messages.concat(), @r"
✓ Pinned worktree @ _REPO_.feature-auth
✓ Unpinned worktree (--off) @ _REPO_.feature-auth
✓ Pinned worktree @ _REPO_.feature-auth
");
}

#[test]
fn tests_that_clean_keep_rejects_a_branch_without_a_worktree() {
    let fixture = TestRepo::with_initial_commit();
    fixture.run_git(&["branch", "branch-only"]);
    let _settings = setup_snapshot_settings(&fixture).bind_to_scope();
    let output = fixture
        .wt_command()
        .args(["clean", "keep", "branch-only"])
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        !fixture
            .test_config_path()
            .with_file_name("clean-repositories.json")
            .exists()
    );
    assert_snapshot!(String::from_utf8_lossy(&output.stderr), @r"
✗ Branch branch-only has no worktree
↳ To create a worktree, run wt switch branch-only
");
}

#[test]
fn tests_that_clean_keep_marks_the_selected_worktree_with_inherited_git_dir() {
    let mut fixture = TestRepo::with_initial_commit();
    let path = fixture.add_worktree("feature");
    let repo = Repository::at(fixture.root_path()).unwrap();
    let identity = repo
        .worktree_at(&path)
        .git_dir()
        .unwrap()
        .join("wt/clean-id.json");
    let output = fixture
        .wt_command()
        .args(["clean", "keep", "feature"])
        .env("GIT_DIR", fixture.root_path().join(".git"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(identity.is_file(), "selected worktree was not pinned");
    assert!(!fixture.root_path().join(".git/wt/clean-id.json").exists());
}
