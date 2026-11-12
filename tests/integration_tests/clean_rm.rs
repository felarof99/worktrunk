//! End-to-end cleanup against temporary repos, local bare remotes and fake hc.
//! Real lsof observes independently spawned processes; tests never contact hc.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use insta::assert_snapshot;
use serde_json::{Value, json};
use worktrunk::testing::mock_commands::{MockConfig, MockResponse, mock_calls};
use worktrunk::testing::set_temp_home_env;

use crate::common::{TestRepo, setup_snapshot_settings, wait_for_file, wait_for_file_content};

struct Fixture {
    repo: TestRepo,
    remote: TestRepo,
    path: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let mut repo = TestRepo::with_initial_commit();
        let remote = TestRepo::bare();
        repo.run_git(&["remote", "add", "origin", remote.path().to_str().unwrap()]);
        repo.run_git(&["push", "origin", "main"]);
        repo.run_git(&["config", "worktrunk.default-branch", "main"]);
        let path = repo.add_worktree("feature");
        let bin = repo.home_path().join("clean-bin");
        fs::create_dir(&bin).unwrap();
        fs::write(bin.join("terminals.json"), r#"{"terminals":[]}"#).unwrap();
        MockConfig::new("hc")
            .command("ls --json", MockResponse::file("terminals.json"))
            .write(&bin);
        Self {
            repo,
            remote,
            path,
            bin,
        }
    }

    fn command(&self) -> Command {
        let mut command = self.repo.wt_command();
        set_temp_home_env(&mut command, self.repo.home_path());
        let mut paths = vec![self.bin.clone()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        command
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("WORKTRUNK_TEST_MOCK_CONFIG_DIR", &self.bin)
            .env("WORKTRUNK_TEST_MOCK_CALL_LOG_DIR", &self.bin)
            .env_remove("WORKTRUNK_TEST_EPOCH")
            .env_remove("CLICOLOR_FORCE")
            .env("NO_COLOR", "1");
        command
    }

    fn rm(&self, args: &[&str]) -> Output {
        self.command()
            .args(["clean", "rm"])
            .args(args)
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str], code: i32) -> Value {
        let mut args = args.to_vec();
        args.extend(["--format", "json"]);
        rows(self.rm(&args), code)
    }

    fn unique_commit(&self) -> String {
        self.repo
            .commit_in_worktree(&self.path, "unique", "local only", "unique");
        self.repo.head_sha_in(&self.path)
    }

    fn hook(&self, body: &str) {
        let hook = self.repo.path().join(".git/hooks/pre-push");
        fs::write(&hook, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn on_second_activity_probe(&self, body: &str) {
        let hc = self.bin.join("hc");
        fs::remove_file(&hc).unwrap(); // Replace the fake tool's symlink, never its binary.
        fs::write(
            &hc,
            format!(
                r#"#!/bin/sh
if [ -f {} ]; then
    {body}
fi
touch {}
printf '%s\n' '{{"terminals":[]}}'
"#,
                quote(&self.bin.join("observed")),
                quote(&self.bin.join("observed"))
            ),
        )
        .unwrap();
        fs::set_permissions(hc, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn project(&self, body: &str) {
        fs::create_dir_all(self.repo.path().join(".config")).unwrap();
        fs::write(self.repo.path().join(".config/wt.toml"), body).unwrap();
    }

    fn marks(&self) -> Value {
        let admin = self.repo.repo.worktree_at(&self.path).git_dir().unwrap();
        let id: String =
            serde_json::from_slice(&fs::read(admin.join("wt/clean-id.json")).unwrap()).unwrap();
        serde_json::from_slice(
            &fs::read(
                self.repo
                    .repo
                    .wt_dir()
                    .join("clean")
                    .join(id)
                    .join("marks.json"),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn exists_branch(&self) -> bool {
        self.repo
            .git_command()
            .args(["show-ref", "--verify", "--quiet", "refs/heads/feature"])
            .run()
            .unwrap()
            .status
            .success()
    }

    fn log(&self) -> PathBuf {
        self.repo.repo.wt_logs_dir().join("clean.jsonl")
    }
}

fn rows(output: Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let rows: Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {output:?}"));
    assert!(rows.is_array());
    rows
}

fn quote(path: &Path) -> String {
    shell_escape::unix::escape(path.to_string_lossy()).into_owned()
}

struct Process(Child);

impl Process {
    fn inside(path: &Path) -> Self {
        Self(
            Command::new("sleep")
                .arg("60")
                .current_dir(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn tests_that_clean_rm_removes_a_backed_up_worktree_and_local_branch() {
    let f = Fixture::new();
    let rows = f.json(&["feature"], 0);
    assert_eq!(
        rows,
        json!([{
            "repo": f.repo.path(), "branch": "feature", "path": f.path,
            "verdict": "removed", "reason": "backed up", "backup": "on-remote", "attic": null
        }])
    );
    assert!(!f.path.exists());
    assert!(!f.exists_branch());
    assert_eq!(fs::read_to_string(f.log()).unwrap().lines().count(), 1);
    let registry = f
        .repo
        .test_config_path()
        .with_file_name("clean-repositories.json");
    assert!(
        fs::read_to_string(registry)
            .unwrap()
            .contains(f.repo.path().to_str().unwrap())
    );
}

#[test]
fn tests_that_clean_rm_pushes_unpublished_commits_before_removing() {
    let f = Fixture::new();
    let tip = f.unique_commit();
    let rows = f.json(&["feature"], 0);
    assert_eq!(rows[0]["backup"], "refs/heads/wt-backup/feature");
    assert_eq!(
        f.remote.git_output(&["rev-parse", "wt-backup/feature"]),
        tip
    );
    assert!(!f.path.exists());
    assert!(!f.exists_branch());
}

#[test]
fn tests_that_clean_rm_saves_uncommitted_and_ignored_env_files() {
    let f = Fixture::new();
    fs::write(f.path.join("file.txt"), "edited").unwrap();
    fs::write(f.path.join("untracked"), "keep me").unwrap();
    fs::write(f.path.join(".env.local"), "secret").unwrap();
    fs::write(f.repo.path().join(".git/info/exclude"), ".env*\n").unwrap();
    let rows = f.json(&["feature"], 0);
    let attic = Path::new(rows[0]["attic"].as_str().unwrap());
    assert!(
        fs::read_to_string(attic.join("tracked.patch"))
            .unwrap()
            .contains("+edited")
    );
    assert_eq!(
        fs::read_to_string(attic.join("untracked/untracked")).unwrap(),
        "keep me"
    );
    assert_eq!(
        fs::read_to_string(attic.join("ignored-env/.env.local")).unwrap(),
        "secret"
    );
    assert!(!f.path.exists());
}

#[test]
fn tests_that_clean_rm_refuses_primary_and_pinned_without_backup() {
    let f = Fixture::new();
    assert!(
        f.command()
            .args(["clean", "keep", "feature"])
            .output()
            .unwrap()
            .status
            .success()
    );
    f.repo.run_git(&["remote", "remove", "origin"]);
    let rows = f.json(&["main", "feature"], 1);
    assert_eq!(rows[0]["reason"], "primary");
    assert_eq!(rows[1]["reason"], "pinned");
    assert_eq!(rows[0]["verdict"], "kept");
    assert_eq!(rows[1]["verdict"], "kept");
    assert!(f.path.exists() && f.repo.path().exists());
    assert!(!f.repo.home_path().join(".wt-attic").exists());
}

#[test]
fn tests_that_clean_rm_keeps_everything_without_a_remote() {
    let f = Fixture::new();
    f.repo.run_git(&["remote", "remove", "origin"]);
    let rows = f.json(&["feature"], 1);
    assert_eq!(rows[0]["reason"], "backup failed (no remote)");
    assert_eq!(rows[0]["verdict"], "kept");
    assert!(f.path.exists() && f.exists_branch());
}

#[test]
fn tests_that_clean_rm_defers_and_backs_up_a_process_inside() {
    let f = Fixture::new();
    let tip = f.unique_commit();
    fs::write(f.path.join("unsaved"), "local").unwrap();
    let _busy = Process::inside(&f.path);
    let rows = f.json(&["feature"], 0);
    assert_eq!(rows[0]["verdict"], "deferred");
    assert_eq!(rows[0]["reason"], "goes when nothing runs in it");
    assert!(rows[0]["attic"].is_string());
    assert_eq!(
        f.remote.git_output(&["rev-parse", "wt-backup/feature"]),
        tip
    );
    assert_eq!(f.marks()["remove_requested"], true);
    assert!(f.path.exists() && f.exists_branch());
}

#[test]
fn tests_that_clean_rm_waits_for_a_process_to_exit() {
    let f = Fixture::new();
    let busy = Process::inside(&f.path);
    let child = f
        .command()
        .args([
            "clean", "rm", "feature", "--wait", "20s", "--format", "json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while mock_calls(&f.bin, "hc").len() < 2 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(busy);
    let rows = rows(child.wait_with_output().unwrap(), 0);
    assert_eq!(rows[0]["verdict"], "removed");
    assert!(!f.path.exists());
}

#[test]
fn tests_that_clean_rm_defers_when_wait_expires() {
    let f = Fixture::new();
    let _busy = Process::inside(&f.path);
    let rows = f.json(&["feature", "--wait", "1ms"], 0);
    assert_eq!(rows[0]["verdict"], "deferred");
    assert_eq!(f.marks()["remove_requested"], true);
    assert!(f.path.exists());
}

#[test]
fn tests_that_clean_rm_unlocks_only_factory_locks() {
    for reason in [
        "hc factory run example",
        "herdr-factory run example",
        "owner locked",
    ] {
        let f = Fixture::new();
        f.repo.run_git(&[
            "worktree",
            "lock",
            "--reason",
            reason,
            f.path.to_str().unwrap(),
        ]);
        let factory = reason != "owner locked";
        let rows = f.json(&["feature"], if factory { 0 } else { 1 });
        assert_eq!(rows[0]["verdict"], if factory { "removed" } else { "kept" });
        assert_eq!(f.path.exists(), !factory);
        if !factory {
            assert_eq!(rows[0]["reason"], "locked");
        }
    }
}

#[test]
fn tests_that_clean_rm_keeps_a_worktree_changed_during_backup() {
    let f = Fixture::new();
    f.unique_commit();
    f.hook(&format!(
        "printf changed > {}",
        quote(&f.path.join("arrived"))
    ));
    let rows = f.json(&["feature"], 1);
    assert_eq!(rows[0]["reason"], "changed during backup");
    assert!(f.path.join("arrived").exists() && f.exists_branch());
}

#[test]
fn tests_that_clean_rm_keeps_a_worktree_that_became_busy_during_backup() {
    let f = Fixture::new();
    f.unique_commit();
    let terminals = json!({"terminals":[{"address":"fake", "cwd":f.path,"state":"live","status":"working","turn":1}]});
    let ready = f.bin.join("busy.json");
    fs::write(&ready, terminals.to_string()).unwrap();
    f.hook(&format!(
        "cp {} {}",
        quote(&ready),
        quote(&f.bin.join("terminals.json"))
    ));
    let rows = f.json(&["feature"], 1);
    assert_eq!(rows[0]["verdict"], "kept");
    assert_eq!(rows[0]["reason"], "in use");
    assert!(f.path.exists() && f.exists_branch());
}

#[test]
fn tests_that_clean_rm_dry_run_writes_nothing() {
    let f = Fixture::new();
    f.unique_commit();
    fs::write(f.path.join("untracked"), "unsaved").unwrap();
    let admin = f.repo.repo.worktree_at(&f.path).git_dir().unwrap();
    let index_time = fs::metadata(admin.join("index"))
        .unwrap()
        .modified()
        .unwrap();
    let rows = f.json(&["feature", "--dry-run"], 0);
    assert_eq!(rows[0]["verdict"], "would-remove");
    assert!(f.path.exists() && f.exists_branch());
    assert_eq!(
        fs::metadata(admin.join("index"))
            .unwrap()
            .modified()
            .unwrap(),
        index_time
    );
    assert!(!admin.join("wt/clean-id.json").exists());
    assert!(!f.repo.home_path().join(".wt-attic").exists());
    for name in ["clean-repositories.json", "clean-terminal-history.json"] {
        assert!(!f.repo.test_config_path().with_file_name(name).exists());
    }
    assert!(
        f.remote
            .git_output(&["for-each-ref", "refs/heads/wt-backup"])
            .is_empty()
    );
    assert!(!f.log().exists());
}

#[test]
fn tests_that_clean_rm_resolves_detached_paths_and_defaults_to_current() {
    for detached in [false, true] {
        let f = Fixture::new();
        let tip = f.unique_commit();
        if detached {
            f.repo.run_git_in(&f.path, &["checkout", "--detach"]);
        }
        let rows = rows(
            f.command()
                .args([
                    "-C",
                    f.path.to_str().unwrap(),
                    "clean",
                    "rm",
                    "--format",
                    "json",
                ])
                .output()
                .unwrap(),
            0,
        );
        assert_eq!(rows[0]["verdict"], "removed");
        assert!(!f.path.exists());
        if detached {
            assert!(rows[0]["branch"].is_null());
            let reference = rows[0]["backup"].as_str().unwrap();
            assert!(reference.starts_with("refs/heads/wt-backup/detached-"));
            assert_eq!(f.remote.git_output(&["rev-parse", reference]), tip);
        }
    }
}

#[test]
fn tests_that_clean_rm_requires_hook_approval_and_runs_approved_hooks() {
    let f = Fixture::new();
    let pre = f.bin.join("pre");
    let post = f.bin.join("post");
    f.project(&format!(
        "pre-remove = 'touch {}'\npost-remove = 'printf %s {{{{ branch }}}} > {}'\n",
        pre.display(),
        post.display()
    ));
    let rows = f.json(&["feature"], 1);
    assert_eq!(rows[0]["verdict"], "kept");
    assert!(!pre.exists() && !post.exists() && f.path.exists());
    let rows = f.json(&["feature", "-y"], 0);
    assert_eq!(rows[0]["verdict"], "removed");
    assert!(pre.exists());
    wait_for_file(&post);
    assert_eq!(fs::read_to_string(post).unwrap(), "feature");
}

#[test]
fn tests_that_clean_rm_rechecks_after_pre_remove_hooks() {
    let f = Fixture::new();
    f.project("pre-remove = 'touch changed-by-hook'\n");
    let rows = f.json(&["feature", "-y"], 1);
    assert_eq!(rows[0]["reason"], "changed during backup");
    assert!(f.path.join("changed-by-hook").exists() && f.exists_branch());
}

#[test]
fn tests_that_clean_rm_keeps_edits_even_when_porcelain_does_not_change() {
    for name in ["file.txt", "folder/local", ".env.local"] {
        let f = Fixture::new();
        fs::write(f.repo.path().join(".git/info/exclude"), ".env*\n").unwrap();
        let file = f.path.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "before").unwrap();
        f.project(&format!("pre-remove = 'printf after > {name}'\n"));
        let rows = f.json(&["feature", "-y"], 1);
        assert_eq!(rows[0]["reason"], "changed during backup");
        assert_eq!(fs::read_to_string(file).unwrap(), "after");
        assert!(f.exists_branch());
    }
}

#[test]
fn tests_that_clean_rm_checks_the_bytes_actually_saved_to_the_attic() {
    for name in ["file.txt", "folder/local", ".env.local"] {
        let f = Fixture::new();
        f.unique_commit();
        fs::write(f.repo.path().join(".git/info/exclude"), ".env*\n").unwrap();
        let path = f.path.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "original").unwrap();
        f.hook(&format!("printf different > {}", quote(&path)));
        f.project(&format!("pre-remove = 'printf original > {name}'\n"));
        let rows = f.json(&["feature", "-y"], 1);
        assert_eq!(rows[0]["verdict"], "kept");
        assert_eq!(rows[0]["reason"], "changed during backup");
        assert_eq!(fs::read_to_string(path).unwrap(), "original");
    }
}

#[test]
fn tests_that_clean_rm_rechecks_primary_env_copies_omitted_from_attic() {
    for during_probe in [false, true] {
        let f = Fixture::new();
        fs::write(f.repo.path().join(".git/info/exclude"), ".env*\n").unwrap();
        fs::write(f.path.join(".env.local"), "original").unwrap();
        let primary_env = f.repo.path().join(".env.local");
        fs::write(&primary_env, "original").unwrap();
        if during_probe {
            f.on_second_activity_probe(&format!("printf different > {}", quote(&primary_env)));
        } else {
            f.project(&format!(
                "pre-remove = 'printf different > {}'\n",
                primary_env.display()
            ));
        }
        let rows = f.json(&["feature", "-y"], 1);
        assert_eq!(rows[0]["reason"], "changed during backup");
        assert_eq!(
            fs::read_to_string(f.path.join(".env.local")).unwrap(),
            "original"
        );
    }
}

#[test]
fn tests_that_clean_rm_keeps_edits_made_during_the_final_activity_probe() {
    for name in ["file.txt", "folder/local", ".env.local"] {
        let f = Fixture::new();
        fs::write(f.repo.path().join(".git/info/exclude"), ".env*\n").unwrap();
        let path = f.path.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "original").unwrap();
        f.on_second_activity_probe(&format!("printf different > {}", quote(&path)));
        let rows = f.json(&["feature"], 1);
        assert_eq!(rows[0]["reason"], "changed during backup");
        assert_eq!(fs::read_to_string(path).unwrap(), "different");
    }
}

#[test]
fn tests_that_clean_rm_keeps_new_env_files_in_previously_empty_directories() {
    let f = Fixture::new();
    fs::write(f.repo.path().join(".git/info/exclude"), "ignored/\n").unwrap();
    let dir = f.path.join("ignored/empty");
    fs::create_dir_all(&dir).unwrap();
    let secret = dir.join(".env.local");
    f.on_second_activity_probe(&format!("printf secret > {}", quote(&secret)));
    let rows = f.json(&["feature"], 1);
    assert_eq!(rows[0]["reason"], "changed during backup");
    assert_eq!(fs::read_to_string(secret).unwrap(), "secret");
    assert!(f.exists_branch());
}

#[test]
fn tests_that_clean_rm_after_rechecks_activity_after_backing_up() {
    let f = Fixture::new();
    f.unique_commit();
    let terminals = json!({"terminals":[{"address":"fake", "cwd":f.path,"state":"live","status":"working","turn":1}]});
    fs::write(f.bin.join("busy.json"), terminals.to_string()).unwrap();
    f.hook(&format!(
        "cp {} {}",
        quote(&f.bin.join("busy.json")),
        quote(&f.bin.join("terminals.json"))
    ));
    f.json(&["feature", "--after", "1ms"], 0);
    wait_for_file_content(&f.log());
    assert!(f.path.exists() && f.exists_branch());
    assert!(fs::read_to_string(f.log()).unwrap().contains("in use"));
}

#[test]
fn tests_that_clean_rm_after_uses_only_approved_hooks_without_yes() {
    let f = Fixture::new();
    let marker = f.bin.join("unapproved");
    f.project(&format!("pre-remove = 'touch {}'\n", marker.display()));
    f.json(&["feature", "--after", "1ms"], 0);
    wait_for_file_content(&f.log());
    assert!(!f.path.exists());
    assert!(!marker.exists());
}

#[test]
fn tests_that_clean_rm_keeps_on_push_or_attic_failure() {
    for attic_failure in [false, true] {
        let f = Fixture::new();
        f.unique_commit();
        if attic_failure {
            fs::write(f.repo.home_path().join(".wt-attic"), "occupied").unwrap();
            fs::write(f.path.join("local"), "unsaved").unwrap();
        } else {
            f.hook("exit 1");
        }
        let rows = f.json(&["feature"], 1);
        assert_eq!(rows[0]["verdict"], "kept");
        assert!(
            rows[0]["reason"]
                .as_str()
                .unwrap()
                .starts_with("backup failed (")
        );
        assert!(f.path.exists() && f.exists_branch());
    }
}

#[test]
fn tests_that_clean_rm_shares_one_remote_snapshot_across_worktrees() {
    let mut f = Fixture::new();
    let second = f.repo.add_worktree("second");
    let trace = f.bin.join("git-trace");
    let output = f
        .command()
        .env("GIT_TRACE", &trace)
        .args(["clean", "rm", "feature", "second", "--format", "json"])
        .output()
        .unwrap();
    let rows = rows(output, 0);
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert_eq!(
        fs::read_to_string(trace)
            .unwrap()
            .matches("ls-remote --symref -- origin")
            .count(),
        1
    );
    // One initial activity snapshot plus one final gate for each deletion.
    assert_eq!(mock_calls(&f.bin, "hc").len(), 3);
    assert!(!f.path.exists() && !second.exists());
}

#[test]
fn tests_that_clean_rm_keeps_a_tip_changed_during_backup() {
    let f = Fixture::new();
    let tip = f.unique_commit();
    f.hook(&format!(
        "unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE; git -C {} commit --allow-empty -m moved",
        quote(&f.path)
    ));
    let rows = f.json(&["feature"], 1);
    assert_eq!(rows[0]["reason"], "changed during backup");
    assert!(f.path.exists() && f.exists_branch());
    assert_ne!(f.repo.head_sha_in(&f.path), tip);
    assert_eq!(
        f.remote.git_output(&["rev-parse", "wt-backup/feature"]),
        tip
    );
}

#[test]
fn tests_that_clean_rm_preserves_the_folder_when_git_removal_fails() {
    let f = Fixture::new();
    let git = which::which("git").unwrap();
    let wrapper = f.bin.join("git");
    fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
if [ "$1" = worktree ] && [ "$2" = remove ]; then
    echo 'simulated removal failure' >&2
    exit 1
fi
exec {} "$@"
"#,
            quote(&git)
        ),
    )
    .unwrap();
    fs::set_permissions(wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let rows = f.json(&["feature"], 1);
    assert_eq!(rows[0]["verdict"], "kept");
    assert_eq!(rows[0]["backup"], "on-remote");
    assert!(f.path.join("file.txt").exists() && f.exists_branch());
}

#[test]
fn tests_that_clean_rm_keeps_a_branch_checked_out_during_removal() {
    let f = Fixture::new();
    let git = which::which("git").unwrap();
    let replacement = f.repo.home_path().join("concurrent-checkout");
    let wrapper = f.bin.join("git");
    fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
if [ "$1" = worktree ] && [ "$2" = remove ]; then
    {} "$@" || exit $?
    {} worktree add --force {} feature >&2
    exit $?
fi
exec {} "$@"
"#,
            quote(&git),
            quote(&git),
            quote(&replacement),
            quote(&git)
        ),
    )
    .unwrap();
    fs::set_permissions(wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let rows = f.json(&["feature"], 1);
    assert_eq!(rows[0]["verdict"], "kept");
    assert!(
        rows[0]["reason"]
            .as_str()
            .unwrap()
            .contains("local branch kept")
    );
    assert!(replacement.join("file.txt").exists() && f.exists_branch());
}

#[test]
fn tests_that_clean_rm_dry_busy_wait_never_marks_or_waits() {
    let f = Fixture::new();
    let _busy = Process::inside(&f.path);
    let rows = f.json(&["feature", "--wait", "1h", "--dry-run"], 0);
    assert_eq!(rows[0]["verdict"], "deferred");
    assert_eq!(mock_calls(&f.bin, "hc").len(), 1);
    let admin = f.repo.repo.worktree_at(&f.path).git_dir().unwrap();
    assert!(!admin.join("wt/clean-id.json").exists());
    assert!(!f.log().exists());
}

#[test]
fn tests_that_clean_rm_unknown_activity_never_authorizes_deletion() {
    let f = Fixture::new();
    MockConfig::new("lsof")
        .command("-d cwd -F pRcn", MockResponse::exit(1))
        .write(&f.bin);
    let rows = f.json(&["feature"], 0);
    assert_eq!(rows[0]["verdict"], "deferred");
    assert!(f.path.exists() && f.exists_branch());
    assert_eq!(f.marks()["remove_requested"], true);
}

#[test]
fn tests_that_clean_rm_after_runs_outside_the_current_worktree() {
    let f = Fixture::new();
    rows(
        f.command()
            .args([
                "-C",
                f.path.to_str().unwrap(),
                "clean",
                "rm",
                "--after",
                "1ms",
                "--format",
                "json",
            ])
            .output()
            .unwrap(),
        0,
    );
    wait_for_file_content(&f.log());
    assert!(!f.path.exists() && !f.exists_branch());
}

#[test]
fn tests_that_clean_rm_after_returns_before_delay_and_removes_later() {
    let f = Fixture::new();
    let rows = f.json(&["feature", "--after", "3s"], 0);
    assert_eq!(rows[0]["verdict"], "deferred");
    assert!(rows[0]["reason"].as_str().unwrap().contains("scheduled"));
    assert!(f.path.exists(), "caller waited for the deletion");
    wait_for_file_content(&f.log());
    assert!(!f.path.exists() && !f.exists_branch());
}

#[test]
fn tests_that_clean_rm_after_keeps_touched_or_busy_worktrees() {
    for busy in [false, true] {
        let f = Fixture::new();
        f.json(&["feature", "--after", "2s"], 0);
        let _process = busy.then(|| Process::inside(&f.path));
        if !busy {
            fs::write(f.path.join("new-work"), "keep").unwrap();
        }
        wait_for_file_content(&f.log());
        let log = fs::read_to_string(f.log()).unwrap();
        assert!(
            log.contains(if busy {
                "in use"
            } else {
                "touched since request"
            }),
            "{log}"
        );
        assert!(f.path.exists() && f.exists_branch());
        assert!(!f.repo.home_path().join(".wt-attic").exists());
    }
}

#[test]
fn tests_that_clean_rm_after_dry_run_schedules_nothing() {
    let f = Fixture::new();
    let rows = f.json(&["feature", "--after", "1ms", "--dry-run"], 0);
    assert_eq!(rows[0]["verdict"], "would-remove");
    assert!(rows[0]["reason"].as_str().unwrap().contains("schedule"));
    assert!(!f.repo.repo.wt_logs_dir().exists());
    assert!(
        !f.repo
            .test_config_path()
            .with_file_name("clean-repositories.json")
            .exists()
    );
    assert!(f.path.exists());
}

#[test]
fn tests_that_clean_rm_after_refuses_primary_and_pin_before_scheduling() {
    let f = Fixture::new();
    assert!(
        f.command()
            .args(["clean", "keep", "feature"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let rows = f.json(&["main", "feature", "--after", "1ms"], 1);
    assert_eq!(rows[0]["reason"], "primary");
    assert_eq!(rows[1]["reason"], "pinned");
    assert!(!f.repo.repo.wt_logs_dir().exists());
}

#[test]
fn tests_that_clean_rm_rejects_after_with_wait() {
    let f = Fixture::new();
    let output = f.rm(&["feature", "--after", "1s", "--wait", "1s"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--after"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--wait"));
    assert!(f.path.exists());
}

#[test]
fn tests_that_clean_rm_text_reports_backup_removal_and_preview() {
    let f = Fixture::new();
    let _settings = setup_snapshot_settings(&f.repo).bind_to_scope();
    let preview = f.rm(&["feature", "--dry-run"]);
    assert!(preview.status.success(), "{preview:?}");
    assert_snapshot!(String::from_utf8_lossy(&preview.stdout), @r"○ Would remove worktree @ _REPO_.feature: backup before removal");
    let removed = f.rm(&["feature"]);
    assert!(removed.status.success(), "{removed:?}");
    assert!(removed.stdout.is_empty());
    assert_snapshot!(String::from_utf8_lossy(&removed.stderr), @r"◎ Checking worktrees...
◎ Checking remote backup...
○ On origin
✓ Removed worktree @ _REPO_.feature
✓ Deleted local branch feature");
}
