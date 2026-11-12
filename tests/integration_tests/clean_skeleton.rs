//! CLI contracts for the clean command skeleton. Tests use isolated commands
//! and temporary repositories; no real hc terminal or remote is accessed.

use crate::common::{TestRepo, repo, wt_command};
use rstest::rstest;

#[test]
fn tests_that_clean_help_is_available() {
    for subcommand in [
        None,
        Some("rm"),
        Some("sweep"),
        Some("keep"),
        Some("restore"),
    ] {
        let mut cmd = wt_command();
        cmd.arg("clean");
        cmd.args(subcommand);
        let output = cmd.arg("--help").output().unwrap();
        assert!(
            output.status.success(),
            "clean {subcommand:?} --help failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn tests_that_clean_skeleton_accepts_flags_and_returns_an_error() {
    // Replace this skeleton-only contract as pieces C–F implement the handlers.
    let cases: &[&[&str]] = &[
        &["rm"],
        &["rm", "feature/auth", "--wait", "5s"],
        &[
            "rm",
            "feature/auth",
            "fix",
            "--after",
            "1h",
            "--dry-run",
            "--format",
            "json",
        ],
    ];
    for args in cases {
        let output = wt_command()
            .args(["-C", ".", "-y", "clean"])
            .args(*args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "clean {args:?}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("wt clean {} is not implemented yet", args[0])),
            "clean {args:?}: {stderr}"
        );
        assert!(output.stdout.is_empty(), "clean {args:?}: {output:?}");
    }
}

#[rstest]
fn tests_that_clean_completes_commands_and_branches(mut repo: TestRepo) {
    repo.add_worktree("feature/auth");
    repo.run_git(&["branch", "branch-only"]);

    for (words, expected, absent) in [
        (vec!["wt", ""], vec!["clean"], vec![]),
        (
            vec!["wt", "clean", ""],
            vec!["rm", "sweep", "keep", "restore"],
            vec![],
        ),
        (
            vec!["wt", "clean", "rm", ""],
            vec!["feature/auth"],
            vec!["branch-only"],
        ),
        (
            vec!["wt", "clean", "keep", ""],
            vec!["feature/auth"],
            vec!["branch-only"],
        ),
        (
            vec!["wt", "clean", "restore", ""],
            vec!["branch-only"],
            vec![],
        ),
    ] {
        let output = repo.completion_cmd(&words).output().unwrap();
        assert!(output.status.success(), "{words:?}: {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let candidates: Vec<_> = stdout
            .lines()
            .filter_map(|line| line.split('\t').next())
            .collect();
        for value in expected {
            assert!(
                candidates.contains(&value),
                "{words:?}: missing {value}: {stdout}"
            );
        }
        for value in absent {
            assert!(
                !candidates.contains(&value),
                "{words:?}: unexpected {value}: {stdout}"
            );
        }
    }
}
