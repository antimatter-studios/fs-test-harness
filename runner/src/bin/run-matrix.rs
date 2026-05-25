//! run-matrix -- libtest-mimic-driven scenario runner.
//!
//! Loads `harness.toml` (resolved from `$HARNESS_TOML` or
//! `<consumer_root>/harness.toml`) and the matrix file it points to;
//! turns each scenario into a libtest-mimic trial; writes per-scenario
//! diag artefacts under `<consumer_root>/test-diagnostics/matrix/`.
//!
//! Each trial walks `scenario.recipe[]` via [`fs_test_harness::run_recipe`]
//! — host-side steps spawn locally, vm-side steps go via SSH. Runnable
//! anywhere with `ssh` in `$PATH`.

use fs_test_harness::{Harness, MaxParallel, VmSection};
use libtest_mimic::{Arguments, Failed, Trial};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};

// ---------------------------------------------------------------------------
// Counting semaphore — limits concurrent scenario execution.
//
// Uses only std primitives (Mutex + Condvar) so no extra dependencies are
// needed. Permit count = resolve_max_parallel(), clamped to 1..=24.
//
// `reduce_capacity` backs off by one slot when resource exhaustion is
// detected (VM out of drive letters). The cap never drops below 1.
// Drop clamps releases to the current capacity so the reduction takes
// effect as running scenarios finish.
// ---------------------------------------------------------------------------

struct SemState {
    available: usize,
    capacity: usize,
    /// The ceiling we started with; probe-up never exceeds this.
    initial_capacity: usize,
    /// When the last resource-exhaustion event was recorded.
    last_exhaustion: Option<std::time::Instant>,
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
                initial_capacity: permits,
                last_exhaustion: None,
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

    /// Reduce capacity by one when resource exhaustion is detected.
    ///
    /// Floor is `max(1, in_use)`: we never claw back permits already held
    /// by running scenarios, and we keep at least 1 so the run makes
    /// progress. Records the event time so `try_probe_up` can backoff.
    fn reduce_capacity(&self) {
        let mut state = self.state.lock().unwrap();
        let in_use = state.capacity.saturating_sub(state.available);
        let floor = in_use.max(1);
        state.last_exhaustion = Some(std::time::Instant::now());
        if state.capacity > floor {
            state.capacity -= 1;
            state.available = state.available.min(state.capacity);
            eprintln!(
                "runner: resource exhaustion — reducing max_parallel to {} (floor={floor})",
                state.capacity
            );
        } else {
            eprintln!(
                "runner: resource exhaustion — max_parallel already at floor ({floor}), cannot reduce"
            );
        }
    }

    /// Probe capacity up by one if no exhaustion has occurred in the last
    /// `grace_secs` seconds and we are below the initial ceiling.
    /// Called by the probe thread every `grace_secs` seconds.
    fn try_probe_up(&self, grace_secs: u64) {
        let notify = {
            let mut state = self.state.lock().unwrap();
            if state.capacity >= state.initial_capacity {
                return;
            }
            let quiet_secs = state
                .last_exhaustion
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(u64::MAX);
            if quiet_secs >= grace_secs {
                state.capacity += 1;
                state.available += 1;
                state.last_exhaustion = None;
                eprintln!(
                    "runner: quiet for {quiet_secs}s — raising max_parallel to {}",
                    state.capacity
                );
                true
            } else {
                false
            }
        };
        if notify {
            self.condvar.notify_one();
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
    let args = Arguments::from_args();

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

    // Determine parallelism from [runner] config. Clamp to 1..=24:
    // Windows has 26 drive letters (A-Z); A and B are typically
    // reserved, leaving 24 available for VHD mounts.
    let runner = &harness.config.runner;
    let permits = resolve_max_parallel(&runner.max_parallel, &harness.config.vm).clamp(1, 24);
    eprintln!("runner: max_parallel={permits}");
    let semaphore = Semaphore::new(permits);

    // Probe-up thread: every 300 s without resource exhaustion, increment
    // capacity by one toward the original ceiling. This recovers from
    // over-aggressive backoff without human intervention.
    if permits > 1 {
        let sem_probe = Arc::clone(&semaphore);
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(300));
            sem_probe.try_probe_up(300);
        });
    }

    let total = harness.matrix.scenarios.len();
    let mut runnable = 0usize;

    let config_for_dispatch = harness.config.clone();
    let consumer_root_for_dispatch = harness.consumer_root.clone();
    let trials: Vec<Trial> = harness
        .matrix
        .scenarios
        .iter()
        .map(|(name, scn)| {
            let is_runnable = !scn.recipe.is_empty();
            if is_runnable {
                runnable += 1;
            }
            let body_name = name.clone();
            let scn = scn.clone();
            let cfg = config_for_dispatch.clone();
            let cr_disp = consumer_root_for_dispatch.clone();
            let sem = Arc::clone(&semaphore);
            let trial = Trial::test(name, move || {
                run_scenario(&body_name, &scn, &cfg, &cr_disp, &sem)
            });
            // Empty-recipe scenarios (planning placeholders) are
            // ignored rather than failing the run.
            if !is_runnable {
                trial.with_ignored_flag(true)
            } else {
                trial
            }
        })
        .collect();

    if !args.list {
        let _ = std::fs::remove_dir_all(matrix_diag_root(&harness.consumer_root));
    }

    let _ = write_run_manifest(&harness.consumer_root, &project_name, total, runnable);

    let conclusion = libtest_mimic::run(&args, trials);

    if !args.list {
        let _ = aggregate_results(&harness.consumer_root);
    }

    conclusion.exit();
}

fn matrix_diag_root(consumer_root: &Path) -> PathBuf {
    consumer_root.join("test-diagnostics/matrix")
}

/// Walk `scenario.recipe` via the per-step dispatcher; record per-step
/// diag + the aggregate verdict; return a libtest verdict.
fn run_scenario(
    name: &str,
    scn: &fs_test_harness::Scenario,
    config: &fs_test_harness::HarnessConfig,
    consumer_root: &Path,
    semaphore: &Arc<Semaphore>,
) -> Result<(), Failed> {
    let started = std::time::Instant::now();
    let diag = matrix_diag_root(consumer_root).join(name);
    std::fs::create_dir_all(&diag).map_err(|e| Failed::from(format!("mkdir diag: {e}")))?;

    // Acquire a slot from the semaphore before touching the VM.
    // Acquire a permit from the pool. Blocks if max_parallel concurrent
    // scenarios are already running, released when _guard is dropped.
    let _guard = semaphore.acquire();

    let outcome = fs_test_harness::run_recipe(name, scn, config, consumer_root, &diag);
    let elapsed = started.elapsed().as_secs_f64();

    // Backpressure: if the VM rejected this scenario due to drive-letter
    // exhaustion, reduce concurrency so subsequent scenarios don't pile up.
    if step_diag_contains(&diag, "resource exhaustion") {
        semaphore.reduce_capacity();
    }

    let (status, error) = match &outcome {
        Ok(r) if r.overall_passed => ("passed", None),
        Ok(r) => {
            let last_failed = r
                .steps
                .last()
                .map(|s| {
                    if let Some(e) = &s.error {
                        format!("step {} ({}) errored: {e}", s.index, s.op)
                    } else {
                        format!(
                            "step {} ({}) exit {:?} != expected {}",
                            s.index, s.op, s.exit_code, s.expected_exit
                        )
                    }
                })
                .unwrap_or_else(|| "recipe failed (no step results)".to_string());
            ("failed", Some(last_failed))
        }
        Err(e) => ("errored", Some(e.clone())),
    };

    let result = ScenarioResult {
        name: name.to_string(),
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

    match (status, error) {
        ("passed", _) => Ok(()),
        (_, Some(msg)) => Err(Failed::from(format!("{msg} (diag at {})", diag.display()))),
        _ => Err(Failed::from(format!(
            "recipe failed (diag at {})",
            diag.display()
        ))),
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
///
/// Each concurrently running scenario that calls a win-* op holds one
/// Windows drive letter for the duration of the VHD mount. Counting
/// unused letters at startup gives the natural parallelism upper bound.
///
/// Falls back to 4 if the VM is unreachable or the query fails.
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

    // Count file-system drive letters currently in use; subtract from 26
    // to get those available for new VHD mounts.
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

/// Scan step diag subdirectories under `scenario_diag` for `needle` in
/// any `stderr.txt`. Used to detect VM-side "resource exhaustion" messages
/// from the drive-letter lock without requiring a special exit code.
fn step_diag_contains(scenario_diag: &Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(scenario_diag) else {
        return false;
    };
    for entry in entries.flatten() {
        let stderr = entry.path().join("stderr.txt");
        if let Ok(text) = std::fs::read_to_string(&stderr) {
            if text.contains(needle) {
                return true;
            }
        }
    }
    false
}

fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}
