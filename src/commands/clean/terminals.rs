//! Observe processes and optional hc terminals once per cleanup run.
//!
//! Unix uses one batched lsof cwd listing, including parent PIDs, and the
//! existing reap parser. Exclude this process and its descendants, never its
//! ancestors. Missing or failed listings and malformed parent records mean
//! unknown activity. A process with no absolute cwd has no folder to match;
//! skip it while retaining its parent relationship for descendant exclusion.
//! Windows conservatively reports unknown process activity. Resolve worktree
//! roots, but match unresolved cwds lexically against both root names so a gone
//! or inaccessible folder outside the worktree does not make it unknown.
//!
//! hc returns an object containing live terminals; unknown status words mean
//! working. History lives in clean-terminal-history.json beside user config,
//! using the same path overrides as other user state. First sightings record
//! the current time but return no history; later observations can establish
//! silence since that first sighting. Changed turns reset the timestamp, gone
//! addresses disappear, and a changed launch directory resets identity. Failed
//! hc observations preserve history. Dry runs never create or write state.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use worktrunk::git::{CommandError, ErrorExt, WorktrunkError};
use worktrunk::shell_exec::Cmd;
use worktrunk::utils::write_atomically;

use super::{ActivitySnapshot, BusyState, HcTerminal, ProcessInfo, TerminalStatus, Timestamp};

#[derive(Deserialize)]
struct HcListing {
    terminals: Vec<TerminalEntry>,
}

#[derive(Deserialize)]
struct TerminalEntry {
    address: String,
    cwd: PathBuf,
    state: String,
    status: Option<String>,
    turn: u64,
}

#[derive(Serialize, Deserialize)]
struct TurnHistory {
    turn: u64,
    changed_at: Timestamp,
    cwd: PathBuf,
}

fn probe_timeout() -> Duration {
    Duration::from_millis(
        std::env::var("WORKTRUNK_TEST_PROBE_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5_000),
    )
}

// TODO(clean): used by pieces E and F.
/// Load live process working directories and `hc ls --json` launch directories,
/// joining terminals with their stored turn history at the supplied time. With
/// no hc on PATH, terminals are empty; unknown process activity stays busy.
/// A dry run may read history but must not update it.
pub fn load(now: Timestamp, dry_run: bool) -> anyhow::Result<ActivitySnapshot> {
    let (processes, mut unknown) = match load_processes() {
        Ok(processes) => (processes, false),
        Err(err) => {
            if let Some(signal) = err.interrupt_signal() {
                return Err(WorktrunkError::Interrupted { signal, hint: None }.into());
            }
            log::debug!("clean process inspection failed: {err:#}");
            (Vec::new(), true)
        }
    };
    let path =
        worktrunk::config::require_config_path()?.with_file_name("clean-terminal-history.json");
    let history: BTreeMap<String, TurnHistory> = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("Invalid clean terminal history")?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(err) => return Err(err).context("Failed to read clean terminal history"),
    };
    let mut terminals = match load_terminals() {
        Ok(terminals) => terminals,
        Err(err) => {
            if let Some(signal) = err.interrupt_signal() {
                return Err(WorktrunkError::Interrupted { signal, hint: None }.into());
            }
            log::debug!("clean terminal inspection failed: {err:#}");
            unknown = true;
            return Ok(ActivitySnapshot {
                processes,
                terminals: Vec::new(),
                unknown,
            });
        }
    };
    let mut updated = BTreeMap::new();
    for terminal in &mut terminals {
        terminal.turn_changed_at = history
            .get(&terminal.address)
            .filter(|previous| previous.cwd == terminal.cwd)
            .map(|previous| {
                if previous.turn == terminal.turn {
                    previous.changed_at
                } else {
                    now
                }
            });
        updated.insert(
            terminal.address.clone(),
            TurnHistory {
                turn: terminal.turn,
                changed_at: terminal.turn_changed_at.unwrap_or(now),
                cwd: terminal.cwd.clone(),
            },
        );
    }
    if !dry_run {
        let parent = path
            .parent()
            .context("Clean terminal history has no parent directory")?;
        fs::create_dir_all(parent).context("Failed to create clean state directory")?;
        write_atomically(&path, &serde_json::to_string_pretty(&updated)?)
            .context("Failed to save clean terminal history")?;
    }
    Ok(ActivitySnapshot {
        processes,
        terminals,
        unknown,
    })
}

// TODO(clean): used by pieces A, E, and F.
/// Select processes whose cwd and live terminals whose launch cwd lie inside
/// this worktree. This is a pure path-membership operation on an observation;
/// unknown activity must remain unknown rather than become an empty busy set.
/// If a cwd cannot be resolved, compare it lexically against the resolved root
/// and the supplied root. Only the worktree root itself must resolve.
pub fn busy_for(path: &Path, activity: &ActivitySnapshot) -> anyhow::Result<BusyState> {
    let root = dunce::canonicalize(path).context("Failed to resolve worktree path")?;
    let inside = |cwd: &Path| match dunce::canonicalize(cwd) {
        Ok(cwd) => cwd.starts_with(&root),
        Err(_) => cwd.starts_with(&root) || cwd.starts_with(path),
    };
    let processes = activity
        .processes
        .iter()
        .filter(|p| inside(&p.cwd))
        .cloned()
        .collect();
    let terminals = activity
        .terminals
        .iter()
        .filter(|t| inside(&t.cwd))
        .cloned()
        .collect();
    Ok(BusyState {
        processes,
        terminals,
        unknown: activity.unknown,
    })
}

// TODO(clean): used by pieces E and F.
/// End only terminals selected by the silent-seven-days sweep verdict, with
/// `hc kill` followed by `hc rm`. A working terminal must never be ended;
/// callers must retain that protection when observations become stale.
pub fn end(terminals: &[HcTerminal]) -> anyhow::Result<()> {
    for terminal in terminals {
        if terminal.status == Some(TerminalStatus::Working) {
            continue;
        }
        let current = load_terminals()?;
        let Some(current) = current.iter().find(|t| t.address == terminal.address) else {
            continue;
        };
        if current.status == Some(TerminalStatus::Working)
            || current.turn != terminal.turn
            || current.cwd != terminal.cwd
        {
            continue;
        }
        hc_command(&["kill", &terminal.address])?;
        hc_command(&["rm", &terminal.address])?;
    }
    Ok(())
}

fn hc_command(args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = Cmd::new("hc")
        .args(args.iter().copied())
        .timeout(probe_timeout())
        .run()?;
    if !output.status.success() {
        return Err(CommandError::from_failed_output("hc", args, &output).into());
    }
    Ok(output.stdout)
}

fn load_terminals() -> anyhow::Result<Vec<HcTerminal>> {
    match which::which("hc") {
        Ok(_) => {}
        Err(which::Error::CannotFindBinaryPath) => return Ok(Vec::new()),
        Err(err) => return Err(err).context("Failed to find hc"),
    }
    let listing: HcListing = serde_json::from_slice(&hc_command(&["ls", "--json"])?)
        .context("Invalid hc terminal listing")?;
    let mut addresses = HashSet::new();
    let mut terminals = Vec::new();
    for entry in listing.terminals {
        if entry.state != "live" {
            continue;
        }
        anyhow::ensure!(
            !entry.address.is_empty()
                && !entry.address.starts_with('-')
                && addresses.insert(entry.address.clone())
                && entry.cwd.is_absolute(),
            "Invalid hc terminal identity"
        );
        let status = entry.status.as_deref().map(|s| match s {
            "blocked" => TerminalStatus::Blocked,
            "done" => TerminalStatus::Done,
            "idle" => TerminalStatus::Idle,
            "error" => TerminalStatus::Error,
            _ => TerminalStatus::Working,
        });
        terminals.push(HcTerminal {
            address: entry.address,
            cwd: entry.cwd,
            status,
            turn: entry.turn,
            turn_changed_at: None,
        });
    }
    Ok(terminals)
}

#[cfg(unix)]
fn load_processes() -> anyhow::Result<Vec<ProcessInfo>> {
    // R adds parent PIDs to the existing reap module's batched cwd protocol.
    let args = ["-d", "cwd", "-F", "pRcn"];
    let output = Cmd::new("lsof").args(args).timeout(probe_timeout()).run()?;
    if !output.status.success() {
        return Err(CommandError::from_failed_output("lsof", &args, &output).into());
    }
    let stdout = std::str::from_utf8(&output.stdout).context("Invalid process listing")?;
    let processes = worktrunk::git::reap::parse_lsof_cwd(stdout);
    let mut parents = BTreeMap::new();
    let mut pid = None;
    let mut listed = HashSet::new();
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix('p') {
            let current = value.parse::<u32>().context("Invalid process id")?;
            anyhow::ensure!(
                current > 0 && listed.insert(current),
                "Duplicate process id"
            );
            pid = Some(current);
        } else if let Some(value) = line.strip_prefix('R') {
            let current = pid.context("Missing process id")?;
            let parent = value.parse::<u32>().context("Invalid parent process id")?;
            anyhow::ensure!(
                parents.insert(current, parent).is_none(),
                "Duplicate parent process id"
            );
        }
    }
    anyhow::ensure!(
        !listed.is_empty() && parents.len() == listed.len(),
        "Incomplete process listing"
    );
    let mut excluded = HashSet::from([std::process::id()]);
    loop {
        let before = excluded.len();
        for (&pid, parent) in &parents {
            if excluded.contains(parent) {
                excluded.insert(pid);
            }
        }
        if before == excluded.len() {
            break;
        }
    }
    Ok(processes
        .into_iter()
        .filter(|p| p.cwd.is_absolute() && !excluded.contains(&p.pid))
        .map(|p| ProcessInfo {
            pid: p.pid,
            cwd: p.cwd,
        })
        .collect())
}

#[cfg(not(unix))]
fn load_processes() -> anyhow::Result<Vec<ProcessInfo>> {
    anyhow::bail!("Process working-directory inspection is unavailable")
}
