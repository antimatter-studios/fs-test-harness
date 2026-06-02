//! run-matrix — libtest-mimic runner with multi-pass retry.
//!
//! Loads `harness.toml` and the matrix file; runs each scenario via
//! [`fs_test_harness::run_recipe`]; writes per-scenario diag artefacts
//! under `<consumer_root>/test-diagnostics/matrix/`.
//!
//! Any scenario that fails is retried up to MAX_RETRIES times. Only
//! scenarios that fail every attempt are reported as permanently broken.
//! Failure output includes the last failing step's stderr/stdout.

use fs_test_harness::{Harness, MaxParallel, VmSection};
use libtest_mimic::{Arguments, Failed, Trial};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

// ---------------------------------------------------------------------------
// Counting semaphore — limits concurrent scenario execution.
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
        .unwrap_or_else(|_| consumer_root.join("fs-test-harness.toml"));

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
    //
    // Filter resolution order:
    //   1. Empty filter → all scenarios.
    //   2. Non-empty filter that matches at least one scenario name
    //      (exact or substring) → normal filter behaviour.
    //   3. Non-empty filter that matches NO scenario → check
    //      `harness.toml [groups]` for a matching group key and expand
    //      to that group's explicit scenario list. Emits a diagnostic
    //      so the caller can see which path was taken.
    let filter = args.filter.as_deref().unwrap_or("");
    let filter_exact = args.exact;

    let apply_filter = |name: &str| -> bool {
        if filter.is_empty() {
            true
        } else if filter_exact {
            name == filter
        } else {
            name.contains(filter)
        }
    };

    // Check whether any scenario matches the filter before consuming
    // the map. If none match and a group with that name exists, use it.
    let group_set: Option<HashSet<String>> =
        if !filter.is_empty() && !harness.matrix.scenarios.keys().any(|n| apply_filter(n)) {
            if let Some(names) = harness.config.groups.get(filter) {
                eprintln!(
                    "runner: filter '{filter}' matched no scenarios — \
                 expanding group '{}' ({} scenario{})",
                    filter,
                    names.len(),
                    if names.len() == 1 { "" } else { "s" },
                );
                Some(names.iter().cloned().collect())
            } else {
                eprintln!(
                    "runner: warning: filter '{filter}' matched no scenarios \
                 and is not a defined group in harness.toml [groups]"
                );
                None
            }
        } else {
            None
        };

    let (runnable, ignored): (Vec<_>, Vec<_>) = harness
        .matrix
        .scenarios
        .into_iter()
        .filter(|(name, _)| {
            if let Some(ref gs) = group_set {
                gs.contains(name)
            } else {
                apply_filter(name)
            }
        })
        .partition(|(_, scn)| !scn.recipe.is_empty());

    let n_runnable = runnable.len();

    // When we expanded a group, the original filter string (e.g. "smoke")
    // is no longer useful — none of the expanded scenario names contain it.
    // Clear it so libtest-mimic doesn't silently re-filter and drop them.
    if group_set.is_some() {
        args.filter = None;
    }

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

    // Disk hygiene: before staging anything, reap host image directories
    // orphaned by previous runs that were killed (SIGKILL / crash / cancel)
    // before their cleanup could fire, then stake this run's claim. Each
    // run owns `{image_dir}/{run_id}/` and drops an `owner.pid` marker in
    // it; a dir whose owner pid is no longer alive is provably an orphan
    // and is removed. Without this, every cancelled full-matrix run leaves
    // tens of GiB of staged `.img` files behind and they pile up across
    // runs until the disk fills.
    if let Some(image_root) =
        resolve_image_root(harness.config.vm.image_dir.as_deref(), &consumer_root)
    {
        reap_orphaned_run_dirs(&image_root);
        claim_run_dir(&image_root, harness.run_id);
    }

    // Banner — print before any scenario work so the log has a clear start marker.
    eprintln!("================================================================");
    eprintln!(
        "{project_name} — test matrix  |  {}  |  {} scenarios  |  parallel {permits}",
        now_human_readable(),
        n_runnable
    );
    eprintln!("================================================================");

    // All progress lines use T+elapsed so log readers see how long each
    // event took relative to the start of the run, not the wall clock.
    let run_start = std::time::Instant::now();

    // Store scenarios in Arc so Trial closures can reference them across passes.
    let all_scenarios: Arc<HashMap<String, Arc<fs_test_harness::Scenario>>> = Arc::new(
        runnable
            .into_iter()
            .map(|(n, s)| (n, Arc::new(s)))
            .collect(),
    );
    let ignored_names: Vec<String> = ignored.into_iter().map(|(n, _)| n).collect();

    let config_arc = Arc::new(harness.config.clone());
    let local_config_arc = Arc::new(harness.local_config.clone());
    let cr_arc = Arc::new(consumer_root.clone());
    let run_id = harness.run_id;

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

        // Progress counter — incremented by each Trial on completion.
        let completed: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let pass_total = pending_names.len();
        let pass_label = if attempt == 0 {
            format!("pass 1/{MAX_RETRIES}")
        } else {
            format!("retry {}/{MAX_RETRIES}", attempt + 1)
        };

        // Heartbeat thread: guaranteed progress output every 30 s.
        {
            let completed_hb = Arc::clone(&completed);
            let label = pass_label.clone();
            let rs = run_start;
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(30));
                let done = completed_hb.load(Ordering::Relaxed);
                if done >= pass_total {
                    break;
                }
                eprintln!(
                    "[{}][+{}] {} — {}/{} done",
                    now_clock(),
                    fmt_elapsed(rs.elapsed().as_secs()),
                    label,
                    done,
                    pass_total,
                );
            });
        }

        let mut trials: Vec<Trial> = pending_names
            .iter()
            .map(|name| {
                let name = name.clone();
                let scn = Arc::clone(all_scenarios.get(&name).unwrap());
                let cfg = Arc::clone(&config_arc);
                let lc = Arc::clone(&local_config_arc);
                let cr = Arc::clone(&cr_arc);
                let sem = Arc::clone(&semaphore);
                let retry_set = Arc::clone(&failed_this_pass);
                let done_ctr = Arc::clone(&completed);
                let rs = run_start;

                Trial::test(name.clone(), move || {
                    let wait_start = std::time::Instant::now();
                    let diag = matrix_diag_root(&cr).join(&name);
                    let _ = std::fs::create_dir_all(&diag);

                    let _guard = sem.acquire();
                    let exec_start = std::time::Instant::now();
                    eprintln!(
                        "\n[{}][+{}] >>> {name}",
                        now_clock(),
                        fmt_elapsed(rs.elapsed().as_secs())
                    );

                    let step_start_ref =
                        std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
                    let name_cb = name.clone();
                    let outcome = fs_test_harness::run_recipe(
                        &name,
                        &scn,
                        &cfg,
                        &lc,
                        &cr,
                        &diag,
                        run_id,
                        |step_result| {
                            let step_secs = {
                                let t = step_start_ref.lock().unwrap();
                                t.elapsed().as_secs()
                            };
                            let passed = step_result.skipped
                                || (step_result.error.is_none()
                                    && step_result.exit_code == Some(step_result.expected_exit));
                            let mark = if passed { "ok  " } else { "FAIL" };
                            let detail = step_detail(step_result);
                            eprintln!(
                                "[{}][+{}]   {:02} {:<20} {}  {}{}",
                                now_clock(),
                                fmt_elapsed(rs.elapsed().as_secs()),
                                step_result.index,
                                step_result.op,
                                mark,
                                fmt_elapsed(step_secs),
                                detail,
                            );
                            *step_start_ref.lock().unwrap() = std::time::Instant::now();
                            let _ = &name_cb;
                        },
                    );
                    let exec_secs = exec_start.elapsed().as_secs();
                    let total_secs = wait_start.elapsed().as_secs_f64();

                    done_ctr.fetch_add(1, Ordering::Relaxed);

                    let (status, error) = outcome_status(&outcome, &diag);
                    let result = ScenarioResult {
                        name: name.clone(),
                        status: status.to_string(),
                        error: error.clone(),
                        diag_dir: diag.display().to_string(),
                        duration_secs: total_secs,
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
                        let reason = error.as_deref().unwrap_or("unknown error");
                        eprintln!(
                            "[{}][+{}] FAIL {name}  (total {})  — {reason}\n",
                            now_clock(),
                            fmt_elapsed(rs.elapsed().as_secs()),
                            fmt_elapsed(exec_secs),
                        );
                        retry_set.lock().unwrap().insert(name.clone());
                        return Err(Failed::from(error.unwrap_or_else(|| "failed".into())));
                    }
                    eprintln!(
                        "[{}][+{}] pass {name}  (total {})\n",
                        now_clock(),
                        fmt_elapsed(rs.elapsed().as_secs()),
                        fmt_elapsed(exec_secs),
                    );
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

        // Accounting before retry.
        eprintln!("----------------------------------------------------------------");
        eprintln!(
            "[{}][+{}] pass {}/{MAX_RETRIES} done: {} passed, {} failed",
            now_clock(),
            fmt_elapsed(run_start.elapsed().as_secs()),
            attempt + 1,
            pass_total - retry_names.len(),
            retry_names.len(),
        );
        for n in &retry_names {
            eprintln!("  ✗ {n}");
        }
        eprintln!("----------------------------------------------------------------");

        if attempt + 1 >= MAX_RETRIES {
            eprintln!(
                "runner: {} scenario(s) failed all {MAX_RETRIES} attempts — genuinely broken",
                retry_names.len()
            );
            permanently_failed.append(&mut retry_names);
            break;
        }

        eprintln!(
            "[{}][+{}] retrying {} scenario(s)…",
            now_clock(),
            fmt_elapsed(run_start.elapsed().as_secs()),
            retry_names.len(),
        );
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
    let host = match vm.host.as_deref().filter(|s| !s.is_empty()) {
        Some(h) => h.to_string(),
        None => {
            eprintln!("runner: no VM configured — defaulting max_parallel to 1");
            return 1;
        }
    };
    let key = vm
        .ssh_key
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(String::from);

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

/// Human-readable date+time for the banner (calls `date -u`; falls back to epoch secs).
fn now_human_readable() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%d %H:%M:%S UTC"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(now_iso8601)
}

/// Format a duration in seconds as `Xm Ys` or `Xs`.
/// Return a short parenthetical detail for a step — image filename for
/// ship ops, last path component of the command for others.
fn step_detail(r: &fs_test_harness::StepResult) -> String {
    if r.command.is_empty() || r.skipped {
        return String::new();
    }
    // For ship ops the command is "scp <src> <dest>"; show just the
    // source filename so it's clear what image is being transferred.
    if r.op.starts_with("ship-") {
        let src = r
            .command
            .split_whitespace()
            .find(|t| !t.starts_with('-') && *t != "scp");
        if let Some(s) = src {
            let fname = s.rsplit('/').next().unwrap_or(s);
            return format!("  ({fname})");
        }
    }
    String::new()
}

fn fmt_elapsed(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// Current wall-clock time as `HH:MM:SS` (UTC).
fn now_clock() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

// ---------------------------------------------------------------------------
// Host image-directory disk hygiene: pid-marked run dirs + orphan reaping.
//
// Each run stages disk images under `{image_dir}/{run_id}/` and drops an
// `owner.pid` marker there. A run that is killed (SIGKILL / crash / a
// cancelled background task) before its cleanup fires leaves that whole
// directory behind — tens of GiB of `.img` files for a full matrix. Across
// repeated cancelled runs these pile up until the disk fills.
//
// The fix is mechanical and self-healing: at the start of every run, before
// staging anything, scan the image root and remove any run dir whose owner
// pid is provably no longer alive. A dir we created ourselves this run is
// reclaimed by the next run once our pid dies. We never touch a dir whose
// owner is still alive (so concurrent runs are safe), and we only consider
// numeric run-id dirs so foreign content the consumer keeps in `image_dir`
// is left strictly alone.
// ---------------------------------------------------------------------------

/// Marker file, written into each run dir, naming the owning process.
const OWNER_PID_FILE: &str = "owner.pid";

/// Grace period before a run dir with no (readable) `owner.pid` marker is
/// treated as an orphan. Protects a concurrent run that has just created
/// its dir but not yet written its marker (a sub-second window in practice),
/// while still reaping legacy dirs left by harness versions predating the
/// marker.
const ORPHAN_GRACE_SECS: u64 = 300;

/// Resolve the host-side image directory the same way `build_flat_vocab`
/// does: relative paths are taken against `consumer_root`. Returns `None`
/// when no image dir is configured (nothing to manage).
fn resolve_image_root(image_dir: Option<&str>, consumer_root: &Path) -> Option<PathBuf> {
    let d = image_dir.filter(|s| !s.is_empty())?;
    let p = PathBuf::from(d);
    Some(if p.is_absolute() {
        p
    } else {
        consumer_root.join(p)
    })
}

/// True iff a process with `pid` currently exists and is signalable.
/// Uses `kill -0` — dependency-free and portable across macOS/Linux. A
/// non-success status (no such process, or EPERM after pid reuse by a
/// foreign process) is treated as "not our live process", i.e. reapable;
/// this can only ever fail to reap (leaving an orphan for the next run),
/// never wrongly reap a live run of ours.
fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Last-modification age of `path` in seconds, or `None` if unavailable.
fn dir_age_secs(path: &Path) -> Option<u64> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    mtime.elapsed().ok().map(|e| e.as_secs())
}

/// Decide whether a single run dir should be reaped. Pulled out of
/// [`reap_orphaned_run_dirs`] so the policy is unit-testable without
/// touching real process state.
///
/// `has_staging_images` is true when the dir contains at least one
/// `nfs-*.img` (our staging signature). It gates the unmarked path so we
/// never delete an unrelated numeric-named directory that merely happens
/// to live under a shared image dir like `/tmp`.
fn run_dir_is_orphan(
    owner_pid: Option<u32>,
    age_secs: Option<u64>,
    has_staging_images: bool,
) -> bool {
    match owner_pid {
        // Marked with our own marker: we own it outright — reap iff the
        // owning process is gone.
        Some(pid) => !pid_alive(pid),
        // No readable marker: only a legacy/foreign leftover from a harness
        // predating the marker. Reap only when it both carries our staging
        // signature AND is older than the grace period — so a concurrent
        // run mid-creation (marker still pending) and unrelated `/tmp`
        // directories are both left strictly alone.
        None => has_staging_images && age_secs.map(|a| a >= ORPHAN_GRACE_SECS).unwrap_or(false),
    }
}

/// True iff `dir` contains at least one `nfs-*.img` staged image.
fn has_staging_images(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_name()
            .to_str()
            .map(|n| n.starts_with("nfs-") && n.ends_with(".img"))
            .unwrap_or(false)
    })
}

/// Remove run dirs under `image_root` whose owning process is dead. Only
/// numeric (run-id) directory names are considered.
fn reap_orphaned_run_dirs(image_root: &Path) {
    let Ok(entries) = std::fs::read_dir(image_root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Run dirs are named with the millisecond run_id (u128). Anything
        // else is consumer content we must not touch.
        match path.file_name().and_then(|n| n.to_str()) {
            Some(name) if name.parse::<u128>().is_ok() => {}
            _ => continue,
        }

        let owner_pid = std::fs::read_to_string(path.join(OWNER_PID_FILE))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());

        if run_dir_is_orphan(owner_pid, dir_age_secs(&path), has_staging_images(&path))
            && std::fs::remove_dir_all(&path).is_ok()
        {
            eprintln!(
                "runner: reaped orphaned image dir {} (owner pid {})",
                path.display(),
                owner_pid
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "none".into()),
            );
        }
    }
}

/// Create this run's image dir and stamp it with our `owner.pid` marker so
/// a future run can prove ownership (and reap it if we die without cleanup).
fn claim_run_dir(image_root: &Path, run_id: u128) {
    let dir = image_root.join(run_id.to_string());
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join(OWNER_PID_FILE), std::process::id().to_string());
    }
}

#[cfg(test)]
mod disk_hygiene_tests {
    use super::*;

    #[test]
    fn resolve_image_root_handles_relative_absolute_and_missing() {
        let cr = Path::new("/consumer/root");
        assert_eq!(
            resolve_image_root(Some("diskimages"), cr),
            Some(PathBuf::from("/consumer/root/diskimages"))
        );
        assert_eq!(
            resolve_image_root(Some("/tmp/imgs"), cr),
            Some(PathBuf::from("/tmp/imgs"))
        );
        assert_eq!(resolve_image_root(None, cr), None);
        assert_eq!(resolve_image_root(Some(""), cr), None);
    }

    #[test]
    fn orphan_policy_keeps_live_and_reaps_dead() {
        // A live owner is never reaped (staging signature is irrelevant
        // for marked dirs).
        assert!(!run_dir_is_orphan(
            Some(std::process::id()),
            Some(99_999),
            true
        ));
        assert!(!run_dir_is_orphan(Some(std::process::id()), None, false));
        // A clearly-dead pid is reaped regardless of staging signature —
        // our marker proves we own it.
        assert!(run_dir_is_orphan(Some(0x7FFF_FFFE), None, false));
    }

    #[test]
    fn orphan_policy_marker_grace_for_unmarked_dirs() {
        // Unmarked dirs are reaped only when they carry our staging
        // signature AND are older than the grace period.
        assert!(run_dir_is_orphan(None, Some(ORPHAN_GRACE_SECS), true));
        // Unmarked + recent → kept (concurrent run mid-creation).
        assert!(!run_dir_is_orphan(None, Some(0), true));
        assert!(!run_dir_is_orphan(None, Some(ORPHAN_GRACE_SECS - 1), true));
        // Unmarked + old but NO staging signature → kept (an unrelated
        // numeric-named dir under a shared image dir like /tmp).
        assert!(!run_dir_is_orphan(None, Some(ORPHAN_GRACE_SECS), false));
        // Unmarked + unknown age → kept (can't prove it's an orphan).
        assert!(!run_dir_is_orphan(None, None, true));
    }

    #[test]
    fn reap_removes_dead_marked_dir_and_keeps_live_and_foreign() {
        let base = std::env::temp_dir().join(format!(
            "fs-harness-reap-test-{}-{}",
            std::process::id(),
            now_clock().replace(':', "")
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // Dead-owner run dir (numeric name, owner.pid = an unused pid).
        let dead = base.join("1780000000000");
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::write(dead.join(OWNER_PID_FILE), "2147483646").unwrap();
        std::fs::write(dead.join("nfs-x.img"), b"junk").unwrap();

        // Live-owner run dir (owner.pid = us) — must survive.
        let live = base.join("1780000000001");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join(OWNER_PID_FILE), std::process::id().to_string()).unwrap();

        // Foreign, non-numeric dir — must never be touched.
        let foreign = base.join("ntfs-basic-fixture");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("keep.img"), b"important").unwrap();

        reap_orphaned_run_dirs(&base);

        assert!(!dead.exists(), "dead-owner run dir should be reaped");
        assert!(live.exists(), "live-owner run dir must survive");
        assert!(foreign.exists(), "foreign non-run-id dir must be untouched");

        let _ = std::fs::remove_dir_all(&base);
    }
}
