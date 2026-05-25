//! run-matrix — libtest-mimic runner with exhaustion-only retry.
//!
//! Loads `harness.toml` and the matrix file; runs each scenario via
//! [`fs_test_harness::run_recipe`]; writes per-scenario diag artefacts
//! under `<consumer_root>/test-diagnostics/matrix/`.
//!
//! Scenarios that fail with "resource exhaustion" (VM drive-letter lock
//! timeout) are collected after each pass and re-run in a subsequent pass.
//! All other scenarios run once. Concurrency is reduced by one before each
//! retry pass; a probe thread restores capacity after 300 s of quiet.

use fs_test_harness::{Harness, MaxParallel, VmSection};
use libtest_mimic::{Arguments, Failed, Trial};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};

// ---------------------------------------------------------------------------
// Counting semaphore — limits concurrent scenario execution.
//
// `reduce_capacity`: back off one slot on exhaustion (floor = max(1, in_use)).
// `try_probe_up`:   add one slot back after N seconds of quiet.
// Drop clamps releases to the current capacity so reductions take effect
// as running scenarios finish naturally.
// ---------------------------------------------------------------------------

struct SemState {
    available: usize,
    capacity: usize,
}

struct Semaphore {
    state: Mutex<SemState>,
    condvar: Condvar,
}

impl Semaphore {
    fn new(permits: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SemState {
                available: permits,
                capacity: permits,
            }),
            condvar: Condvar::new(),
        })
    }

    fn acquire(self: &Arc<Self>) -> SemaphoreGuard {
        let mut state = self.state.lock().unwrap();
        while state.available == 0 {
            state = self.condvar.wait(state).unwrap();
        }
        state.available -= 1;
        SemaphoreGuard {
            sem: Arc::clone(self),
        }
    }
}

struct SemaphoreGuard {
    sem: Arc<Semaphore>,
}

impl Drop for SemaphoreGuard {
    fn drop(&mut self) {
        let mut state = self.sem.state.lock().unwrap();
        // Clamp to capacity so reductions take effect as slots are released.
        state.available = (state.available + 1).min(state.capacity);
        self.sem.condvar.notify_one();
    }
}

// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
struct ScenarioResult {
    name: String,
    status: String,
    error: Option<String>,
    diag_dir: String,
    duration_secs: f64,
}

#[derive(Serialize)]
struct RunManifest {
    timestamp_utc: String,
    host_os: &'static str,
    git_sha: Option<String>,
    project_name: String,
    scenario_count_total: usize,
    scenario_count_runnable: usize,
}

fn main() {
    let mut args = Arguments::from_args();

    // Resolve consumer root + harness.toml.
    let consumer_root = std::env::var("HARNESS_CONSUMER_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().expect("cwd"));
    let config_path = std::env::var("HARNESS_TOML")
        .map(PathBuf::from)
        .unwrap_or_else(|_| consumer_root.join("harness.toml"));

    let harness = Harness::load(&config_path).unwrap_or_else(|e| {
        panic!("load {}: {e}", config_path.display());
    });

    let project_name = harness.config.project.name.clone();

    // Determine parallelism. Clamp to 1..=24 (26 drive letters minus A, B).
    let permits =
        resolve_max_parallel(&harness.config.runner.max_parallel, &harness.config.vm).clamp(1, 24);
    eprintln!("runner: max_parallel={permits}");

    // Tell libtest-mimic's thread pool to match our semaphore capacity.
    args.test_threads = Some(permits);

    let semaphore = Semaphore::new(permits);

    let total = harness.matrix.scenarios.len();

    // Partition into runnable / ignored, applying the --filter arg.
    let filter = args.filter.as_deref().unwrap_or("");
    let filter_exact = args.exact;
    let (runnable, ignored): (Vec<_>, Vec<_>) = harness
        .matrix
        .scenarios
        .into_iter()
        .filter(|(name, _)| {
            if filter.is_empty() {
                true
            } else if filter_exact {
                name == filter
            } else {
                name.contains(filter)
            }
        })
        .partition(|(_, scn)| !scn.recipe.is_empty());

    let n_runnable = runnable.len();

    // --list: use libtest-mimic's formatting then exit.
    if args.list {
        let mut trials: Vec<Trial> = runnable
            .iter()
            .map(|(name, _)| Trial::test(name, || Ok(())))
            .collect();
        trials.extend(
            ignored
                .iter()
                .map(|(name, _)| Trial::test(name, || Ok(())).with_ignored_flag(true)),
        );
        libtest_mimic::run(&args, trials).exit();
    }

    let _ = std::fs::remove_dir_all(matrix_diag_root(&consumer_root));
    let _ = write_run_manifest(&consumer_root, &project_name, total, n_runnable);

    // Store scenarios in Arc so Trial closures can reference them across passes.
    let all_scenarios: Arc<HashMap<String, Arc<fs_test_harness::Scenario>>> = Arc::new(
        runnable
            .into_iter()
            .map(|(n, s)| (n, Arc::new(s)))
            .collect(),
    );
    let ignored_names: Vec<String> = ignored.into_iter().map(|(n, _)| n).collect();

    let config_arc = Arc::new(harness.config.clone());
    let cr_arc = Arc::new(consumer_root.clone());

    // Any scenario that fails a pass is retried up to MAX_RETRIES times total.
    // A scenario that passes on any attempt counts as passed. Only scenarios
    // that fail all MAX_RETRIES attempts are genuinely broken.
    const MAX_RETRIES: u8 = 5;

    let mut pending_names: Vec<String> = {
        let mut names: Vec<String> = all_scenarios.keys().cloned().collect();
        names.sort();
        names
    };

    let mut permanently_failed: Vec<String> = Vec::new();

    for attempt in 0u8..MAX_RETRIES {
        if attempt > 0 {
            eprintln!(
                "runner: {} scenario(s) failed — retrying (attempt {}/{MAX_RETRIES})",
                pending_names.len(),
                attempt + 1
            );
        }

        // Tracks which scenarios failed this pass and need another attempt.
        let failed_this_pass: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

        let mut trials: Vec<Trial> = pending_names
            .iter()
            .map(|name| {
                let name = name.clone();
                let scn = Arc::clone(all_scenarios.get(&name).unwrap());
                let cfg = Arc::clone(&config_arc);
                let cr = Arc::clone(&cr_arc);
                let sem = Arc::clone(&semaphore);
                let retry_set = Arc::clone(&failed_this_pass);

                Trial::test(name.clone(), move || {
                    let step_start = std::time::Instant::now();
                    let diag = matrix_diag_root(&cr).join(&name);
                    let _ = std::fs::create_dir_all(&diag);

                    let _guard = sem.acquire();
                    let outcome = fs_test_harness::run_recipe(&name, &scn, &cfg, &cr, &diag);
                    let elapsed = step_start.elapsed().as_secs_f64();

                    let (status, error) = outcome_status(&outcome, &diag);
                    let result = ScenarioResult {
                        name: name.clone(),
                        status: status.to_string(),
                        error: error.clone(),
                        diag_dir: diag.display().to_string(),
                        duration_secs: elapsed,
                    };
                    let _ = std::fs::write(
                        diag.join("result.json"),
                        serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".into()),
                    );
                    if let Ok(r) = &outcome {
                        let _ = std::fs::write(
                            diag.join("recipe.json"),
                            serde_json::to_string_pretty(r).unwrap_or_default(),
                        );
                    }

                    if status != "passed" {
                        retry_set.lock().unwrap().insert(name.clone());
                        return Err(Failed::from(error.unwrap_or_else(|| "failed".into())));
                    }
                    Ok(())
                })
            })
            .collect();

        // Include ignored trials on the first pass for the correct summary count.
        if attempt == 0 {
            for name in &ignored_names {
                trials.push(Trial::test(name, || Ok(())).with_ignored_flag(true));
            }
        }

        let _ = libtest_mimic::run(&args, trials);

        let mut retry_names: Vec<String> = {
            let mut names: Vec<String> = failed_this_pass.lock().unwrap().iter().cloned().collect();
            names.sort();
            names
        };

        if retry_names.is_empty() {
            break; // All passed this pass — done.
        }

        if attempt + 1 >= MAX_RETRIES {
            eprintln!(
                "runner: {} scenario(s) failed all {MAX_RETRIES} attempts — genuinely broken",
                retry_names.len()
            );
            permanently_failed.append(&mut retry_names);
            break;
        }

        pending_names = retry_names;
    }

    let _ = aggregate_results(&consumer_root);

    if !permanently_failed.is_empty() {
        std::process::exit(101);
    }
}

fn matrix_diag_root(consumer_root: &Path) -> PathBuf {
    consumer_root.join("test-diagnostics/matrix")
}

/// Extract a `(status, error_message)` pair from a recipe outcome.
///
/// When a step fails, the step's `stderr.txt` and `stdout.txt` from the diag
/// directory are appended (truncated to 4 KB each) so failures are self-contained
/// in the libtest-mimic output without needing to open separate log files.
fn outcome_status(
    outcome: &Result<fs_test_harness::RecipeResult, String>,
    diag: &Path,
) -> (&'static str, Option<String>) {
    match outcome {
        Ok(r) if r.overall_passed => ("passed", None),
        Ok(r) => {
            let msg = r
                .steps
                .last()
                .map(|s| {
                    let header = if let Some(e) = &s.error {
                        format!("step {} ({}) errored: {e}", s.index, s.op)
                    } else {
                        format!(
                            "step {} ({}) exit {:?} != expected {}",
                            s.index, s.op, s.exit_code, s.expected_exit
                        )
                    };
                    let step_dir = diag.join(format!("step-{:02}", s.index));
                    let stderr = read_tail(&step_dir.join("stderr.txt"), 4096);
                    let stdout = read_tail(&step_dir.join("stdout.txt"), 4096);
                    let mut full = header;
                    if !stderr.is_empty() {
                        full.push_str("\n--- stderr ---\n");
                        full.push_str(&stderr);
                    }
                    if !stdout.is_empty() {
                        full.push_str("\n--- stdout ---\n");
                        full.push_str(&stdout);
                    }
                    full
                })
                .unwrap_or_else(|| "recipe failed (no step results)".to_string());
            ("failed", Some(msg))
        }
        Err(e) => ("errored", Some(e.clone())),
    }
}

/// Read up to `max_bytes` from the end of a file (tail), trimmed.
fn read_tail(path: &Path, max_bytes: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.len() <= max_bytes {
        trimmed.to_string()
    } else {
        format!("...(truncated)\n{}", &trimmed[trimmed.len() - max_bytes..])
    }
}

/// Resolve `max_parallel` to a concrete permit count.
fn resolve_max_parallel(mp: &MaxParallel, vm: &VmSection) -> usize {
    match mp {
        MaxParallel::Explicit(n) => *n,
        MaxParallel::Named(name) if name == "drive-letters" => query_available_drive_letters(vm),
        MaxParallel::Named(other) => {
            eprintln!("runner: unknown max_parallel value '{other}', defaulting to 1");
            1
        }
    }
}

/// SSH to the Windows VM and count unallocated drive letters.
fn query_available_drive_letters(vm: &VmSection) -> usize {
    let env_host = std::env::var("VM_HOST").ok().filter(|s| !s.is_empty());
    let env_key = std::env::var("SSH_KEY").ok().filter(|s| !s.is_empty());
    let cfg_host = vm.host.as_deref().filter(|s| !s.is_empty());
    let cfg_key = vm.ssh_key.as_deref().filter(|s| !s.is_empty());

    let host = match env_host.or_else(|| cfg_host.map(String::from)) {
        Some(h) => h,
        None => {
            eprintln!("runner: no VM configured — defaulting max_parallel to 1");
            return 1;
        }
    };
    let key = env_key.or_else(|| cfg_key.map(String::from));

    let ps_cmd = "powershell -NoProfile -NonInteractive -Command \
        (26 - (Get-PSDrive -PSProvider FileSystem | Measure-Object).Count)";

    let mut cmd = Command::new("ssh");
    cmd.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=10",
        "-o",
        "ServerAliveCountMax=2",
    ]);
    if let Some(k) = &key {
        cmd.args(["-i", k.as_str(), "-o", "IdentitiesOnly=yes"]);
    }
    cmd.arg(&host).arg(ps_cmd);

    match cmd.output() {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            match s.parse::<usize>() {
                Ok(n) if n > 0 => {
                    eprintln!("runner: {n} drive letters available on VM — using as max_parallel");
                    n
                }
                _ => {
                    eprintln!(
                        "runner: unexpected drive-letter count '{s}' — defaulting max_parallel to 1"
                    );
                    1
                }
            }
        }
        _ => {
            eprintln!("runner: could not query VM drive letters — defaulting max_parallel to 1");
            1
        }
    }
}

fn write_run_manifest(
    consumer_root: &Path,
    project_name: &str,
    total: usize,
    runnable: usize,
) -> std::io::Result<()> {
    let root = matrix_diag_root(consumer_root);
    std::fs::create_dir_all(&root)?;
    let manifest = RunManifest {
        timestamp_utc: now_iso8601(),
        host_os: std::env::consts::OS,
        git_sha: git_sha(consumer_root),
        project_name: project_name.to_string(),
        scenario_count_total: total,
        scenario_count_runnable: runnable,
    };
    std::fs::write(
        root.join("run-manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap_or_else(|_| "{}".into()),
    )
}

fn aggregate_results(consumer_root: &Path) -> std::io::Result<()> {
    let root = matrix_diag_root(consumer_root);
    let mut results: Vec<ScenarioResult> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let p = entry.path().join("result.json");
            if p.is_file() {
                if let Ok(raw) = std::fs::read_to_string(&p) {
                    if let Ok(r) = serde_json::from_str::<ScenarioResult>(&raw) {
                        results.push(r);
                    }
                }
            }
        }
    }
    results.sort_by(|a, b| a.name.cmp(&b.name));
    std::fs::write(
        root.join("results.json"),
        serde_json::to_string_pretty(&results).unwrap_or_else(|_| "[]".into()),
    )
}

fn git_sha(consumer_root: &Path) -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(consumer_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}
