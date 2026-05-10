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

use fs_test_harness::Harness;
use libtest_mimic::{Arguments, Failed, Trial};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

// Mount lifecycle is process-global on Windows (drive letter assignment,
// WinFsp host singletons), so serialise scenarios within a single
// `run-matrix` invocation. Concurrent invocations on different VMs are
// independent.
static MOUNT_LOCK: Mutex<()> = Mutex::new(());

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
            let trial = Trial::test(name, move || run_scenario(&body_name, &scn, &cfg, &cr_disp));
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
) -> Result<(), Failed> {
    let started = std::time::Instant::now();
    let diag = matrix_diag_root(consumer_root).join(name);
    std::fs::create_dir_all(&diag).map_err(|e| Failed::from(format!("mkdir diag: {e}")))?;

    // Hold the global mount lock — vm-steps in a recipe can include a
    // long-running mount lifecycle, and Windows drive-letter assignment
    // is process-global on the VM. Cheaper to serialise here than to
    // track per-recipe.
    let _guard = MOUNT_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let outcome = fs_test_harness::run_recipe(name, scn, config, consumer_root, &diag);
    let elapsed = started.elapsed().as_secs_f64();

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
