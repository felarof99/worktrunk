//! Sweep CLI wiring uses isolated state and fake activity commands. Removal is
//! tested with a fake beside sweep.rs until piece E supplies the shared remover.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, UNIX_EPOCH};

#[cfg(unix)]
use insta::assert_snapshot;
use serde_json::Value;
use worktrunk::testing::mock_commands::{MockConfig, MockResponse, mock_calls};
use worktrunk::testing::set_temp_home_env;

#[cfg(unix)]
use crate::common::setup_snapshot_settings;
use crate::common::{TEST_EPOCH, TestRepo, wait_for_file};

fn command(repo: &TestRepo) -> Command {
    let bin = repo.home_path().join("sweep-bin");
    fs::create_dir_all(&bin).unwrap();
    MockConfig::new("hc")
        .command("ls", MockResponse::output(r#"{"terminals":[]}"#))
        .command("_default", MockResponse::exit(99))
        .write(&bin);
    #[cfg(unix)]
    MockConfig::new("lsof")
        .command(
            "_default",
            MockResponse::output("p1\nR0\ncinit\nfcwd\nn/\n"),
        )
        .write(&bin);
    let mut cmd = repo.wt_command();
    set_temp_home_env(&mut cmd, repo.home_path());
    let path = cmd
        .get_envs()
        .find_map(|(key, value)| (key == "PATH").then_some(value).flatten())
        .map(|path| path.to_owned())
        .unwrap_or_else(|| std::env::var_os("PATH").unwrap());
    cmd.env(
        "PATH",
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&path)))
            .unwrap(),
    )
    .env("WORKTRUNK_TEST_MOCK_CONFIG_DIR", &bin)
    .env("WORKTRUNK_TEST_MOCK_CALL_LOG_DIR", &bin)
    .env_remove("CLICOLOR_FORCE")
    .env("NO_COLOR", "1");
    cmd.args(["clean", "sweep"]);
    cmd
}

#[test]
fn tests_that_clean_sweep_all_repos_works_outside_every_repo() {
    let fixture = TestRepo::with_initial_commit();
    let output = command(&fixture)
        .current_dir(fixture.home_path())
        .args(["--all-repos", "--dry-run", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        serde_json::json!([])
    );
    assert!(
        !fixture
            .test_config_path()
            .with_file_name("clean-repositories.json")
            .exists()
    );
    assert!(
        !fixture
            .test_config_path()
            .with_file_name("clean-terminal-history.json")
            .exists()
    );
}

#[test]
fn tests_that_clean_sweep_reports_linked_worktrees_as_one_json_array() {
    let mut fixture = TestRepo::with_initial_commit();
    let path = fixture.add_worktree("recent");
    let output = command(&fixture)
        .args(["--dry-run", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        rows,
        serde_json::json!([{
            "repo": fixture.root_path(),
            "branch": "recent",
            "path": path,
            "verdict": "kept",
            "reason": if cfg!(unix) { "recent" } else { "in use" },
            "backup": null,
            "attic": null
        }])
    );
}

fn registry(fixture: &TestRepo, repositories: &[&Path]) -> PathBuf {
    let path = fixture
        .test_config_path()
        .with_file_name("clean-repositories.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(repositories).unwrap()).unwrap();
    path
}

fn attic_entry(fixture: &TestRepo, days: u64) -> PathBuf {
    let entry = fixture
        .home_path()
        .join(format!(".wt-attic/project/feature/{days}"));
    fs::create_dir_all(&entry).unwrap();
    let metadata = serde_json::json!({
        "repo": fixture.root_path(), "branch": "feature",
        "tip_sha": fixture.head_sha(), "worktree_path": fixture.root_path(),
        "time": UNIX_EPOCH + Duration::from_secs(TEST_EPOCH - days * 86_400),
        "backup_ref": null,
    });
    fs::write(
        entry.join("meta.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    fs::write(entry.join("saved"), "local backup").unwrap();
    entry
}

fn contents(path: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    if !path.exists() {
        return vec![];
    }
    if path.is_file() {
        return vec![(
            path.into(),
            fs::read(path).unwrap(),
            fs::metadata(path).unwrap().modified().unwrap(),
        )];
    }
    let mut files: Vec<_> = fs::read_dir(path)
        .unwrap()
        .flat_map(|entry| contents(&entry.unwrap().path()))
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

#[test]
fn tests_that_clean_sweep_dry_run_leaves_registry_history_attic_and_index_untouched() {
    let mut fixture = TestRepo::with_initial_commit();
    let linked = fixture.add_worktree("recent");
    let registry = registry(&fixture, &[fixture.root_path()]);
    let history = registry.with_file_name("clean-terminal-history.json");
    fs::write(
        &history,
        serde_json::to_vec(&serde_json::json!({"old-terminal": {
            "turn": 4, "cwd": linked, "changed_at": UNIX_EPOCH + Duration::from_secs(TEST_EPOCH)
        }}))
        .unwrap(),
    )
    .unwrap();
    let old = attic_entry(&fixture, 31);
    let paths = [
        registry,
        history,
        old,
        fixture.repo.worktree_at(&linked).git_dir().unwrap(),
        fixture.repo.wt_dir(),
    ];
    let before: Vec<_> = paths.iter().map(|path| contents(path)).collect();
    let output = command(&fixture).arg("--dry-run").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let after: Vec<_> = paths.iter().map(|path| contents(path)).collect();
    assert_eq!(before, after);
    let bin = fixture.home_path().join("sweep-bin");
    assert_eq!(mock_calls(&bin, "hc"), ["ls --json"]);
}

#[test]
fn tests_that_clean_sweep_real_run_registers_and_expires_only_old_attic_entries() {
    let fixture = TestRepo::with_initial_commit();
    let old = attic_entry(&fixture, 31);
    let new = attic_entry(&fixture, 29);
    let output = command(&fixture).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!old.exists());
    assert!(new.join("saved").exists());
    let registry = fixture
        .test_config_path()
        .with_file_name("clean-repositories.json");
    let registered: Vec<PathBuf> = serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
    assert_eq!(registered, [fixture.root_path()]);
    assert!(
        registry
            .with_file_name("clean-terminal-history.json")
            .exists()
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn tests_that_clean_sweep_combines_registered_and_discovered_repos_and_reports_skips() {
    let mut registered = TestRepo::with_initial_commit();
    registered.add_worktree("registered");
    let mut discovered = TestRepo::with_initial_commit();
    let root = registered.home_path().join("root");
    fs::create_dir_all(root.join("group")).unwrap();
    let linked = discovered.add_worktree_at_path("discovered", &root.join("group/discovered"));
    let missing = dunce::canonicalize(registered.home_path())
        .unwrap()
        .join("missing");
    registry(&registered, &[registered.root_path(), &missing]);
    let output = command(&registered)
        .current_dir(registered.home_path())
        .args(["--all-repos", "--root"])
        .arg(&root)
        .args(["--dry-run", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|row| row["branch"] == "discovered" && row["path"] == serde_json::json!(linked))
    );
    assert!(rows.iter().any(|row| row["branch"] == "registered"));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("Skipped repository @ ~/missing"),
        "{stderr}"
    );
    let bin = registered.home_path().join("sweep-bin");
    assert_eq!(mock_calls(&bin, "hc"), ["ls --json"]);
    #[cfg(unix)]
    assert_eq!(mock_calls(&bin, "lsof"), ["-d cwd -F pRcn"]);
}

#[test]
fn tests_that_clean_sweep_continues_when_a_selected_repository_disappears() {
    let mut first = TestRepo::with_initial_commit();
    first.add_worktree("first");
    let mut second = TestRepo::with_initial_commit();
    second.add_worktree("second");
    registry(&first, &[first.root_path(), second.root_path()]);
    let mut cmd = command(&first);
    cmd.current_dir(first.home_path())
        .args(["--all-repos", "--dry-run", "--format", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let bin = first.home_path().join("sweep-bin");
    MockConfig::new("hc")
        .command(
            "ls",
            MockResponse::output(r#"{"terminals":[]}"#).wait_for_file("release"),
        )
        .command("_default", MockResponse::exit(99))
        .write(&bin);
    let child = cmd.spawn().unwrap();
    wait_for_file(&bin.join("hc.calls"));
    // Selection is complete before activity starts; this is a genuine failure
    // in the sweep loop, not a missing entry filtered out by the registry.
    let (gone, survivor) = if first.root_path() < second.root_path() {
        (&first, &second)
    } else {
        (&second, &first)
    };
    fs::rename(gone.root_path(), gone.home_path().join("moved-primary")).unwrap();
    fs::write(bin.join("release"), "ready").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["repo"], serde_json::json!(survivor.root_path()));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Skipped repository")
    );
}

#[cfg(unix)]
#[test]
fn tests_that_clean_sweep_text_groups_rows_and_closes_with_counts() {
    let mut fixture = TestRepo::with_initial_commit();
    fixture.add_worktree("recent");
    let _settings = setup_snapshot_settings(&fixture).bind_to_scope();
    let output = command(&fixture).arg("--dry-run").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_snapshot!(String::from_utf8_lossy(&output.stdout), @r"
REPOSITORY @ _REPO_
○ kept recent @ _REPO_.recent (recent)
");
    assert_snapshot!(String::from_utf8_lossy(&output.stderr), @r"
◎ Inspecting worktrees (--dry-run)...
○ would-remove 0, kept 1 (1 recent)
");
}

#[test]
fn tests_that_clean_sweep_never_uses_an_inherited_git_dir_for_another_repo() {
    let mut first = TestRepo::with_initial_commit();
    first.add_worktree("first");
    let mut second = TestRepo::with_initial_commit();
    second.add_worktree("second");
    registry(&first, &[first.root_path(), second.root_path()]);
    let output = command(&first)
        .env("GIT_DIR", first.root_path().join(".git"))
        .args(["--all-repos", "--dry-run", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["repo"], serde_json::json!(first.root_path()));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Git context points to a different repository")
    );
}

#[test]
fn tests_that_clean_sweep_exits_unsuccessfully_when_shared_state_cannot_be_read() {
    for filename in ["clean-repositories.json", "clean-terminal-history.json"] {
        let fixture = TestRepo::with_initial_commit();
        let path = fixture.test_config_path().with_file_name(filename);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "corrupt").unwrap();
        let output = command(&fixture)
            .args(["--all-repos", "--dry-run", "--format", "json"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(fs::read_to_string(path).unwrap(), "corrupt");
    }
}

#[test]
fn tests_that_clean_sweep_exits_unsuccessfully_when_expiry_fails() {
    let fixture = TestRepo::with_initial_commit();
    let invalid_home = fixture.home_path().join("not-a-directory");
    fs::write(&invalid_home, "preserve").unwrap();
    let mut cmd = command(&fixture);
    set_temp_home_env(&mut cmd, &invalid_home);
    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Failed to expire cleanup attic entries")
    );
    assert_eq!(fs::read_to_string(invalid_home).unwrap(), "preserve");
}

#[cfg(unix)]
#[test]
fn tests_that_clean_sweep_keeps_worktrees_when_git_path_overrides_make_facts_ambiguous() {
    let mut fixture = TestRepo::with_initial_commit();
    let linked = fixture.add_worktree("fresh");
    let old = UNIX_EPOCH + Duration::from_secs(TEST_EPOCH - 2 * 86_400);
    for file in ["HEAD", "index", "logs/HEAD"] {
        fs::File::options()
            .write(true)
            .open(fixture.root_path().join(".git").join(file))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let output = command(&fixture)
        .args(["--dry-run", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows[0]["reason"], "recent");
    for (variable, path) in [
        ("GIT_DIR", fixture.root_path().join(".git")),
        ("GIT_WORK_TREE", fixture.root_path().into()),
    ] {
        let output = command(&fixture)
            .env(variable, path)
            .args(["--dry-run", "--format", "json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["path"], serde_json::json!(linked));
        assert_eq!(rows[0]["verdict"], "kept");
        let reason = rows[0]["reason"].as_str().unwrap();
        assert!(
            reason.contains("Git path overrides prevent worktree inspection")
                && reason.contains(variable),
            "{reason}"
        );
    }
}

#[test]
fn tests_that_clean_sweep_resolves_relative_roots_under_global_c() {
    let mut caller = TestRepo::with_initial_commit();
    let mut selected = TestRepo::with_initial_commit();
    for (fixture, branch) in [(&mut caller, "caller"), (&mut selected, "selected")] {
        let root = fixture.home_path().join("root");
        fs::create_dir(&root).unwrap();
        fixture.add_worktree_at_path(branch, &root.join(branch));
    }
    let output = command(&caller)
        .current_dir(caller.home_path())
        .arg("-C")
        .arg(selected.home_path())
        .args([
            "--all-repos",
            "--root",
            "root",
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["repo"], serde_json::json!(selected.root_path()));
    assert_eq!(rows[0]["branch"], "selected");
}

#[cfg(unix)]
#[rstest::rstest]
#[case("hc", "INT", 130)]
#[case("hc", "TERM", 143)]
#[case("lsof", "INT", 130)]
#[case("lsof", "TERM", 143)]
fn tests_that_clean_sweep_stops_without_writes_after_an_activity_interrupt(
    #[case] probe: &str,
    #[case] signal: &str,
    #[case] status: i32,
) {
    use std::os::unix::fs::PermissionsExt;
    let fixture = TestRepo::with_initial_commit();
    let old = attic_entry(&fixture, 31);
    let mut cmd = command(&fixture);
    let path = fixture.home_path().join("sweep-bin").join(probe);
    // Replace only the fixture's link, never write through it to the wt binary.
    fs::remove_file(&path).unwrap();
    fs::write(
        &path,
        format!(
            "#!/bin/sh
kill -{signal} $$
"
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let output = cmd.args(["--format", "json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(old.join("saved").exists());
    for file in ["clean-repositories.json", "clean-terminal-history.json"] {
        assert!(!fixture.test_config_path().with_file_name(file).exists());
    }
}
