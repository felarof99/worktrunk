//! Collectors are binary-private. Compile their source with the shared value
//! contract below so integration tests can use Cargo's freshly built mock-stub
//! binary. The production binary also compiles them against clean/mod.rs.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use worktrunk::git::{Repository, WorktreeInfo};
use worktrunk::testing::mock_commands::{MockConfig, MockResponse, mock_calls};
use worktrunk::testing::{TEST_EPOCH, TestRepo, test_tempdir};

#[path = "../../src/commands/clean/facts.rs"]
mod facts;
#[path = "../../src/commands/clean/terminals.rs"]
mod terminals;

type Timestamp = SystemTime;

#[derive(Debug, Clone)]
pub struct WorktreeFacts {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub primary: bool,
    pub bare: bool,
    pub pinned: bool,
    pub remove_requested: bool,
    pub lock_reason: Option<String>,
    pub busy: BusyState,
    pub last_touch: Timestamp,
    pub dirty: bool,
    pub tip_sha: String,
    pub status_porcelain: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BusyState {
    pub processes: Vec<ProcessInfo>,
    pub terminals: Vec<HcTerminal>,
    pub unknown: bool,
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone)]
pub struct HcTerminal {
    pub address: String,
    pub cwd: PathBuf,
    pub status: Option<TerminalStatus>,
    pub turn: u64,
    pub turn_changed_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalStatus {
    Working,
    Blocked,
    Done,
    Idle,
    Error,
}

#[derive(Debug, Clone)]
pub struct ActivitySnapshot {
    pub processes: Vec<ProcessInfo>,
    pub terminals: Vec<HcTerminal>,
    pub unknown: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct KeepState {
    pub pinned: bool,
    pub remove_requested: bool,
}

// Piece D supplies this dependency. All fact tests pass the state explicitly.
mod keep {
    pub fn load_state(
        _: &worktrunk::git::Repository,
        _: &std::path::Path,
    ) -> anyhow::Result<super::KeepState> {
        anyhow::bail!("Keep state must be supplied by the test")
    }
}

fn empty_busy() -> BusyState {
    BusyState {
        processes: vec![],
        terminals: vec![],
        unknown: false,
    }
}

fn now() -> Timestamp {
    UNIX_EPOCH + Duration::from_secs(TEST_EPOCH)
}

fn worktree(repo: &TestRepo, path: &Path) -> WorktreeInfo {
    Repository::at(repo.path())
        .unwrap()
        .list_worktrees()
        .unwrap()
        .iter()
        .find(|w| w.path == path)
        .unwrap()
        .clone()
}

fn observe(repo: &TestRepo, path: &Path) -> anyhow::Result<WorktreeFacts> {
    facts::load_with_keep_state(
        &repo.repo,
        &worktree(repo, path),
        empty_busy(),
        KeepState {
            pinned: true,
            remove_requested: true,
        },
    )
}

fn set_time(path: &Path, time: Timestamp) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

#[test]
fn tests_that_clean_facts_preserve_exact_status_and_marks() {
    let mut repo = TestRepo::with_initial_commit();
    let path = repo.add_worktree("feature");
    fs::write(path.join("file.txt"), "modified").unwrap();
    fs::write(path.join("space name"), "untracked").unwrap();
    let expected = repo
        .repo
        .worktree_at(&path)
        .run_command_output(&["--no-optional-locks", "status", "--porcelain"])
        .unwrap();
    let observed = observe(&repo, &path).unwrap();
    assert_eq!(observed.status_porcelain, expected.stdout);
    assert!(observed.dirty && observed.pinned && observed.remove_requested);
    assert_eq!(observed.path, path);
    assert_eq!(observed.branch.as_deref(), Some("feature"));
    assert_eq!(observed.tip_sha, repo.head_sha_in(&path));
    assert!(!observed.primary && !observed.bare && !observed.busy.unknown);
    assert!(observed.lock_reason.is_none());
    // The public wrapper must resolve marks, never invent an unpinned default.
    assert!(facts::load(&repo.repo, &worktree(&repo, &path), empty_busy(), now()).is_err());
}

#[test]
fn tests_that_clean_last_touch_uses_worktree_admin_and_each_dirty_file() {
    let mut repo = TestRepo::with_initial_commit();
    let path = repo.add_worktree("feature");
    fs::write(path.join("file.txt"), "modified").unwrap();
    fs::create_dir(path.join("new")).unwrap();
    fs::write(path.join("new/nested.txt"), "untracked").unwrap();
    let admin = repo.repo.worktree_at(&path).git_dir().unwrap();
    let files = [
        admin.join("index"),
        admin.join("HEAD"),
        admin.join("logs/HEAD"),
        path.join("file.txt"),
        path.join("new/nested.txt"),
    ];
    for file in &files {
        set_time(file, now());
    }
    // Each file wins in turn, including two touches within the same second.
    for (i, file) in files.iter().enumerate() {
        let time = now() + Duration::from_nanos(100_000_000 * (i as u64 + 1));
        set_time(file, time);
        let expected = fs::metadata(file).unwrap().modified().unwrap();
        assert_eq!(
            observe(&repo, &path).unwrap().last_touch,
            expected,
            "{}",
            file.display()
        );
    }
    assert_eq!(
        fs::metadata(admin.join("index"))
            .unwrap()
            .modified()
            .unwrap(),
        now() + Duration::from_nanos(100_000_000)
    );
}

#[test]
fn tests_that_clean_facts_keep_lock_reason_and_primary_identity() {
    let mut repo = TestRepo::with_initial_commit();
    let path = repo.add_worktree("feature");
    repo.lock_worktree("feature", Some("owner work"));
    assert_eq!(
        observe(&repo, &path).unwrap().lock_reason.as_deref(),
        Some("owner work")
    );
    assert!(observe(&repo, repo.path()).unwrap().primary);
}

#[test]
fn tests_that_clean_facts_read_a_fresh_tip_and_detached_head() {
    let mut repo = TestRepo::with_initial_commit();
    let path = repo.add_worktree("feature");
    let stale = worktree(&repo, &path);
    repo.run_git_in(&path, &["commit", "--allow-empty", "-m", "new tip"]);
    let facts = facts::load_with_keep_state(
        &repo.repo,
        &stale,
        empty_busy(),
        KeepState {
            pinned: false,
            remove_requested: false,
        },
    )
    .unwrap();
    assert_ne!(facts.tip_sha, stale.head);
    repo.detach_head_in_worktree("feature");
    assert!(observe(&repo, &path).unwrap().branch.is_none());
}

#[test]
fn tests_that_clean_facts_handle_renames_deletions_and_quoted_names() {
    let mut repo = TestRepo::with_initial_commit();
    let path = repo.add_worktree("feature");
    repo.run_git_in(&path, &["mv", "file.txt", "renamed file"]);
    fs::write(path.join("space and é"), "new").unwrap();
    #[cfg(unix)]
    fs::write(path.join("quotes\" and newline\n"), "new").unwrap();
    let facts = observe(&repo, &path).unwrap();
    assert!(facts.dirty);
    fs::remove_file(path.join("renamed file")).unwrap();
    assert!(observe(&repo, &path).unwrap().dirty);
}

#[test]
fn tests_that_clean_facts_fail_when_admin_or_worktree_is_unreadable() {
    let mut repo = TestRepo::with_initial_commit();
    let path = repo.add_worktree("feature");
    let admin = repo.repo.worktree_at(&path).git_dir().unwrap();
    fs::remove_file(admin.join("logs/HEAD")).unwrap();
    assert!(observe(&repo, &path).is_err());
    fs::remove_file(path.join(".git")).unwrap();
    assert!(observe(&repo, &path).is_err());
}

#[test]
fn tests_that_clean_facts_mark_linked_worktrees_of_bare_repos() {
    let repo = TestRepo::bare();
    let seed = TestRepo::with_initial_commit();
    let path = repo.path().join("linked");
    repo.run_git(&["config", "core.logAllRefUpdates", "true"]);
    repo.run_git(&["fetch", seed.path().to_str().unwrap(), "main:feature"]);
    repo.run_git(&["worktree", "add", path.to_str().unwrap(), "feature"]);
    let facts = observe(&repo, &path).unwrap();
    assert!(facts.bare);
}

fn terminal(path: &Path, status: Option<TerminalStatus>, turn: u64) -> HcTerminal {
    HcTerminal {
        address: "t1".into(),
        cwd: path.into(),
        status,
        turn,
        turn_changed_at: None,
    }
}

#[test]
fn tests_that_clean_busy_membership_uses_path_components_and_keeps_unknown() {
    let dir = test_tempdir();
    let path = dir.path().join("work");
    let sibling = dir.path().join("work-extra");
    fs::create_dir_all(path.join("nested")).unwrap();
    fs::create_dir(&sibling).unwrap();
    let activity = ActivitySnapshot {
        processes: vec![
            ProcessInfo {
                pid: 1,
                cwd: path.join("nested"),
            },
            ProcessInfo {
                pid: 2,
                cwd: sibling.clone(),
            },
        ],
        terminals: vec![terminal(&path, None, 0), terminal(&sibling, None, 0)],
        unknown: true,
    };
    let busy = terminals::busy_for(&path, &activity).unwrap();
    assert!(busy.unknown);
    assert_eq!(
        busy.processes.iter().map(|p| p.pid).collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(busy.terminals.len(), 1);
}

#[cfg(unix)]
#[test]
fn tests_that_clean_busy_membership_resolves_symlinks() {
    let dir = test_tempdir();
    let real = dir.path().join("real");
    let alias = dir.path().join("alias");
    fs::create_dir_all(real.join("nested")).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let activity = ActivitySnapshot {
        processes: vec![ProcessInfo {
            pid: 10,
            cwd: alias.join("nested"),
        }],
        terminals: vec![terminal(&alias, None, 0)],
        unknown: false,
    };
    let busy = terminals::busy_for(&real, &activity).unwrap();
    assert_eq!(busy.processes.len(), 1);
    assert_eq!(busy.terminals.len(), 1);
    assert!(!busy.unknown);
}

fn hc_json(path: &Path, status: serde_json::Value, turn: u64) -> String {
    serde_json::json!({"hidden_finished":3,"terminals":[{
        "address":"t1","cwd":path,"state":"live","status":status,"turn":turn,
        "agent":"codex","name":"agent","parent":"t0","where":"test"
    }]})
    .to_string()
}

fn fake_hc(dir: &Path, response: MockResponse) {
    MockConfig::new("hc")
        .command("ls --json", response)
        .command("kill t1", MockResponse::exit(0))
        .command("rm t1", MockResponse::exit(0))
        .command("_default", MockResponse::exit(99))
        .write(dir);
}

fn activity_case(case: &str) {
    let dir = test_tempdir();
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    // Re-enter in a child to isolate PATH, config, and the real process id.
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    worktrunk::testing::configure_cli_command(&mut child);
    let output = child
        .args([
            "--ignored",
            "--exact",
            "integration_tests::clean_facts::clean_activity_child",
            "--nocapture",
        ])
        .current_dir(dir.path())
        .env("PATH", &bin)
        .env("WORKTRUNK_CONFIG_PATH", dir.path().join("config.toml"))
        .env("WORKTRUNK_TEST_MOCK_CONFIG_DIR", &bin)
        .env("WORKTRUNK_TEST_MOCK_CALL_LOG_DIR", dir.path())
        .env("WORKTRUNK_TEST_CLEAN_CASE", case)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{case}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn tests_that_clean_accepts_the_real_hc_shape_and_conservative_statuses() {
    activity_case("shape");
}
#[test]
fn tests_that_clean_accepts_missing_hc_and_keeps_failed_observations_unknown() {
    activity_case("failure");
}
#[test]
fn tests_that_clean_tracks_turn_changes_and_dry_runs_without_writes() {
    activity_case("history");
}
#[test]
fn tests_that_clean_end_rechecks_working_status_and_turn_before_killing() {
    activity_case("end");
}
#[cfg(unix)]
#[test]
fn tests_that_clean_excludes_its_process_tree_but_keeps_ancestors() {
    activity_case("processes");
}

#[test]
#[ignore = "invoked in an isolated subprocess by the activity tests"]
fn clean_activity_child() {
    let dir = std::env::current_dir().unwrap();
    let bin = dir.join("bin");
    let history = dir.join("clean-terminal-history.json");
    #[cfg(unix)]
    MockConfig::new("lsof")
        .command(
            "_default",
            MockResponse::output(&format!(
                "p{}\nR0\ncwt\nfcwd\nn{}\n",
                std::process::id(),
                dir.display()
            )),
        )
        .write(&bin);
    let case = std::env::var("WORKTRUNK_TEST_CLEAN_CASE").unwrap();
    match case.as_str() {
        "shape" => {
            for (raw, expected) in [
                (serde_json::json!("done"), Some(TerminalStatus::Done)),
                (serde_json::json!("error"), Some(TerminalStatus::Error)),
                (serde_json::Value::Null, None),
                (
                    serde_json::json!("new-status"),
                    Some(TerminalStatus::Working),
                ),
            ] {
                fake_hc(&bin, MockResponse::output(&hc_json(&dir, raw, 2)));
                let activity = terminals::load(now(), true).unwrap();
                assert_eq!(activity.terminals.len(), 1);
                let t = &activity.terminals[0];
                assert_eq!((&*t.address, &t.cwd, t.turn), ("t1", &dir, 2));
                assert_eq!(t.status, expected);
                assert!(t.turn_changed_at.is_none());
                #[cfg(unix)]
                assert!(!activity.unknown);
            }
            fake_hc(
                &bin,
                MockResponse::output(
                    &hc_json(&dir, serde_json::json!("done"), 2)
                        .replace("\"live\"", "\"finished\""),
                ),
            );
            assert!(terminals::load(now(), true).unwrap().terminals.is_empty());
        }
        "failure" => {
            let activity = terminals::load(now(), true).unwrap();
            assert!(activity.terminals.is_empty());
            #[cfg(unix)]
            assert!(!activity.unknown);
            for response in [
                MockResponse::exit(1),
                MockResponse::output("not json"),
                MockResponse::output("[]"),
                MockResponse::output("{\"terminals\":[{}]}"),
            ] {
                fake_hc(&bin, response);
                assert!(terminals::load(now(), true).unwrap().unknown);
            }
            fake_hc(&bin, MockResponse::output("{\"terminals\":[]}"));
            #[cfg(unix)]
            {
                MockConfig::new("lsof")
                    .command("_default", MockResponse::exit(1))
                    .write(&bin);
                assert!(terminals::load(now(), true).unwrap().unknown);
            }
        }
        "history" => {
            fake_hc(
                &bin,
                MockResponse::output(&hc_json(&dir, serde_json::json!("idle"), 2)),
            );
            let first = terminals::load(now(), true).unwrap();
            assert!(first.terminals[0].turn_changed_at.is_none());
            assert!(!history.exists());
            assert!(
                terminals::load(now(), false).unwrap().terminals[0]
                    .turn_changed_at
                    .is_none()
            );
            let before = fs::read(&history).unwrap();
            let later = now() + Duration::from_secs(60);
            assert_eq!(
                terminals::load(later, true).unwrap().terminals[0].turn_changed_at,
                Some(now())
            );
            fake_hc(
                &bin,
                MockResponse::output(&hc_json(&dir, serde_json::json!("done"), 3)),
            );
            assert_eq!(
                terminals::load(later, true).unwrap().terminals[0].turn_changed_at,
                Some(later)
            );
            assert_eq!(fs::read(&history).unwrap(), before);
            assert_eq!(
                terminals::load(later, false).unwrap().terminals[0].turn_changed_at,
                Some(later)
            );
            assert_ne!(fs::read(&history).unwrap(), before);
            fake_hc(&bin, MockResponse::output("{\"terminals\":[]}"));
            terminals::load(later, false).unwrap();
            fake_hc(
                &bin,
                MockResponse::output(&hc_json(&dir, serde_json::json!("done"), 3)),
            );
            assert!(
                terminals::load(later, true).unwrap().terminals[0]
                    .turn_changed_at
                    .is_none()
            );
            fs::write(&history, "corrupt").unwrap();
            assert!(terminals::load(later, false).is_err());
            assert_eq!(fs::read_to_string(&history).unwrap(), "corrupt");
        }
        "end" => {
            for (status, turn) in [("working", 2), ("unknown-status", 2), ("done", 3)] {
                fake_hc(
                    &bin,
                    MockResponse::output(&hc_json(&dir, serde_json::json!(status), turn)),
                );
                terminals::end(&[terminal(&dir, Some(TerminalStatus::Done), 2)]).unwrap();
                assert!(
                    mock_calls(&dir, "hc")
                        .iter()
                        .all(|call| call == "ls --json")
                );
            }
            fake_hc(
                &bin,
                MockResponse::output(&hc_json(&dir, serde_json::json!("done"), 2)),
            );
            terminals::end(&[terminal(&dir, Some(TerminalStatus::Done), 2)]).unwrap();
            let calls = mock_calls(&dir, "hc");
            assert_eq!(&calls[calls.len() - 3..], ["ls --json", "kill t1", "rm t1"]);
        }
        #[cfg(unix)]
        "processes" => {
            let pid = std::process::id();
            let parent = nix::unistd::getppid().as_raw();
            let output = format!(
                "p{pid}\nR{parent}\ncwt\nfcwd\nn{}\np1000001\nR{pid}\ncgit\nfcwd\nn{}\np1000002\nR1000001\nclsof\nfcwd\nn{}\np{parent}\nR1\nczsh\nfcwd\nn{}\n",
                dir.display(),
                dir.display(),
                dir.display(),
                dir.display()
            );
            MockConfig::new("lsof")
                .command("_default", MockResponse::output(&output))
                .write(&bin);
            let activity = terminals::load(now(), true).unwrap();
            let busy = terminals::busy_for(&dir, &activity).unwrap();
            assert!(!busy.unknown);
            assert_eq!(
                busy.processes.iter().map(|p| p.pid).collect::<Vec<_>>(),
                [parent as u32]
            );
            assert_eq!(mock_calls(&dir, "lsof").len(), 1);
        }
        _ => panic!("unknown test case"),
    }
}
