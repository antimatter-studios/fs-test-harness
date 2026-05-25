//! run-matrix -- retry-aware parallel scenario runner.
//!
//! Loads `harness.toml` (resolved from `$HARNESS_TOML` or
//! `<consumer_root>/harness.toml`) and the matrix file it points to;
//! runs each scenario via [`fs_test_harness::run_recipe`]; writes
//! per-scenario diag artefacts under `<consumer_root>/test-diagnostics/matrix/`.
//!
//! Scenarios that fail with "resource exhaustion" (VM drive-letter lock
//! timeout) are pushed to the back of a shared work queue so smaller tests
//! can run first. Concurrency is reduced by one on each exhaustion event and
//! probed back up every 300 s of quiet.

use fs_test_harness::{Harness, MaxParallel, VmSection};
use libtest_mimic::{Arguments, Trial};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Ceiling we started with; probe-up never exceeds this.
    initial_capacity: usize,
    /// Timestamp of the most recent resource-exhaustion event.
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
    /// by running scenarios, and we keep at least 1 so the run makes progress.
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

    /// Probe capacity up by one if no exhaustion in the last `grace_secs`
    /// seconds and we are below the initial ceiling. Called by the probe thread.
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
// Work queue — pending scenarios with retry counts.
//
// `remaining` counts scenarios not yet permanently settled (success,
// non-exhaustion failure, or exhaustion after MAX_RETRIES). Workers block
// on the Condvar when the deque is empty but remaining > 0 (there are
// in-flight scenarios that may be pushed back for retry).
// ---------------------------------------------------------------------------

struct WorkQueue {
    deque: VecDeque<(String, fs_test_harness::Scenario, u8)>,
    /// Unsettled scenario count. Reaches 0 when the run is complete.
    remaining: usize,
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

    // Determine parallelism. Clamp to 1..=24 (26 drive letters minus A, B).
    let runner = &harness.config.runner;
    let permits = resolve_max_parallel(&runner.max_parallel, &harness.config.vm).clamp(1, 24);
    eprintln!("runner: max_parallel={permits}");
    let semaphore = Semaphore::new(permits);

    // Probe-up thread: recover from over-aggressive backoff after 300 s of quiet.
    if permits > 1 {
        let sem_probe = Arc::clone(&semaphore);
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(300));
            sem_probe.try_probe_up(300);
        });
    }

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
    let n_ignored = ignored.len();

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

    // ---------------------------------------------------------------------------
    // Custom retry-aware executor.
    //
    // Each worker pops from a shared VecDeque. If a scenario fails with
    // "resource exhaustion" and has retries left, it is pushed to the *back*
    // of the queue so other scenarios can run first.  The semaphore guard is
    // dropped before re-queuing so the slot is available to other workers.
    // ---------------------------------------------------------------------------

    println!("\nrunning {n_runnable} tests");

    let work = Arc::new((
        Mutex::new(WorkQueue {
            deque: runnable
                .into_iter()
                .map(|(n, s)| (n, s, 0u8))
                .collect::<VecDeque<_>>(),
            remaining: n_runnable,
        }),
        Condvar::new(),
    ));

    let all_results: Arc<Mutex<Vec<ScenarioResult>>> = Arc::new(Mutex::new(Vec::new()));
    let any_failed = Arc::new(AtomicBool::new(false));
    let run_start = std::time::Instant::now();

    let config_arc = Arc::new(harness.config.clone());
    let cr_arc = Arc::new(consumer_root.clone());

    // Worker count equals initial permits; extra threads would only block on
    // the semaphore and provide no throughput benefit.
    let mut handles = Vec::new();
    for _ in 0..permits {
        let work = Arc::clone(&work);
        let sem = Arc::clone(&semaphore);
        let cfg = Arc::clone(&config_arc);
        let cr = Arc::clone(&cr_arc);
        let results = Arc::clone(&all_results);
        let failed_flag = Arc::clone(&any_failed);

        handles.push(std::thread::spawn(move || {
            // A scenario may be re-queued this many times before permanently failing.
            const MAX_RETRIES: u8 = 5;
            let (queue_mu, condvar) = &*work;

            loop {
                // Pop next scenario, or wait if in-flight scenarios may re-queue.
                let (name, scn, attempts) = {
                    let mut wq = queue_mu.lock().unwrap();
                    loop {
                        if wq.remaining == 0 {
                            return; // all scenarios settled
                        }
                        if let Some(item) = wq.deque.pop_front() {
                            break item;
                        }
                        // Queue empty but remaining > 0: an in-flight scenario
                        // might push something back. Wait for notification.
                        wq = condvar.wait(wq).unwrap();
                    }
                };

                let step_start = std::time::Instant::now();
                let diag = matrix_diag_root(&cr).join(&name);
                let _ = std::fs::create_dir_all(&diag);

                // Acquire a semaphore slot before touching the VM.
                let _guard = sem.acquire();

                let outcome = fs_test_harness::run_recipe(&name, &scn, &cfg, &cr, &diag);
                let elapsed = step_start.elapsed().as_secs_f64();

                // Resource exhaustion: re-queue at back; reduce concurrency.
                if step_diag_contains(&diag, "resource exhaustion") && attempts < MAX_RETRIES {
                    sem.reduce_capacity();
                    eprintln!(
                        "runner: {name} — resource exhaustion \
                         (attempt {}/{}), re-queuing at back of queue",
                        attempts + 1,
                        MAX_RETRIES
                    );
                    drop(_guard); // release slot before re-queuing
                    let mut wq = queue_mu.lock().unwrap();
                    wq.deque.push_back((name, scn, attempts + 1));
                    condvar.notify_one();
                    continue;
                }

                // Permanently settled — record result.
                let (status, error) = outcome_status(&outcome);
                let passed = status == "passed";
                println!("test {name} ... {}", if passed { "ok" } else { "FAILED" });
                if !passed {
                    failed_flag.store(true, Ordering::Relaxed);
                }

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
                results.lock().unwrap().push(result);

                // Decrement remaining; wake other workers if run is now complete.
                let mut wq = queue_mu.lock().unwrap();
                wq.remaining -= 1;
                if wq.remaining == 0 {
                    condvar.notify_all();
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("worker thread panicked");
    }

    let total_elapsed = run_start.elapsed().as_secs_f64();
    let _ = aggregate_results(&consumer_root);

    // Print libtest-style summary.
    let results = all_results.lock().unwrap();
    let n_passed = results.iter().filter(|r| r.status == "passed").count();
    let n_failed = results.iter().filter(|r| r.status != "passed").count();

    if any_failed.load(Ordering::Relaxed) {
        println!("\nfailures:\n");
        for r in results.iter().filter(|r| r.status != "passed") {
            println!(
                "---- {} ----\n{}\n",
                r.name,
                r.error.as_deref().unwrap_or("(no error message)")
            );
        }
        println!(
            "test result: FAILED. {n_passed} passed; {n_failed} failed; \
             {n_ignored} ignored; finished in {total_elapsed:.2}s\n"
        );
        std::process::exit(101);
    } else {
        println!(
            "\ntest result: ok. {n_passed} passed; 0 failed; \
             {n_ignored} ignored; finished in {total_elapsed:.2}s\n"
        );
    }
}

fn matrix_diag_root(consumer_root: &Path) -> PathBuf {
    consumer_root.join("test-diagnostics/matrix")
}

/// Extract a `(status, error_message)` pair from a recipe outcome.
fn outcome_status(
    outcome: &Result<fs_test_harness::RecipeResult, String>,
) -> (&'static str, Option<String>) {
    match outcome {
        Ok(r) if r.overall_passed => ("passed", None),
        Ok(r) => {
            let msg = r
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
            ("failed", Some(msg))
        }
        Err(e) => ("errored", Some(e.clone())),
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

/// Scan step diag subdirectories for `needle` in any `stderr.txt`.
/// Used to detect VM-side "resource exhaustion" without a special exit code.
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
