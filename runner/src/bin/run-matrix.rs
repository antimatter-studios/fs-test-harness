//! run-matrix -- libtest-mimic-driven scenario runner.
//!
//! Loads `harness.toml` (resolved from `$HARNESS_TOML` or
//! `<consumer_root>/harness.toml`) and the matrix file it points to;
//! turns each scenario into a libtest-mimic trial; writes per-scenario
//! diag artefacts under `<consumer_root>/test-diagnostics/matrix/`.
//!
//! Each trial:
//!   1. Builds the scenario JSON via [`TomlAdapter::build_scenario_json`].
//!   2. Spawns `powershell -File <scripts/run-scenario.ps1> -ScenarioJson ...`.
//!   3. Parses `VERDICT=` from stdout to decide pass/fail.
//!
//! On non-Windows hosts, trials are marked ignored. The binary still
//! compile-checks so a bad change can't slip through.

use fs_test_harness::{Harness, TomlAdapter};
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

    // Image dir: env override > harness.toml [vm.image_dir] > "".
    let image_dir = std::env::var("HARNESS_IMAGE_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| harness.config.vm.image_dir.clone())
        .map(PathBuf::from)
        .unwrap_or_default();

    // Harness root: assume we're running from the harness/runner crate
    // OR the consumer linked us. CARGO_MANIFEST_DIR points at this
    // crate's Cargo.toml, so the harness root is one level up.
    let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.to_path_buf())
        .expect("harness root resolvable");

    let project_name = harness.config.project.name.clone();

    let total = harness.matrix.scenarios.len();
    let mut runnable = 0usize;

    // Build a TomlAdapter; reused across trials (cheap to clone).
    let adapter = std::sync::Arc::new(TomlAdapter::new(
        harness.config.clone(),
        harness_root.clone(),
        harness.consumer_root.clone(),
        image_dir.clone(),
    ));

    let trials: Vec<Trial> = harness
        .matrix
        .scenarios
        .iter()
        .map(|(name, scn)| {
            let is_runnable = !scn.ops.is_empty();
            if is_runnable {
                runnable += 1;
            }
            let body_name = name.clone();
            let scn = scn.clone();
            let adapter = adapter.clone();
            let consumer_root = harness.consumer_root.clone();
            let trial = Trial::test(name, move || {
                run_scenario(&body_name, &scn, &adapter, &consumer_root)
            });
            if !is_runnable || !cfg!(target_os = "windows") {
                trial.with_ignored_flag(true)
            } else {
                trial
            }
        })
        .collect();

    if !args.list {
        let _ = std::fs::remove_dir_all(matrix_diag_root(&harness.consumer_root));
    }

    let _ = write_run_manifest(
        &harness.consumer_root,
        &project_name,
        total,
        runnable,
    );

    let conclusion = libtest_mimic::run(&args, trials);

    if !args.list {
        let _ = aggregate_results(&harness.consumer_root);
    }

    conclusion.exit();
}

fn matrix_diag_root(consumer_root: &Path) -> PathBuf {
    consumer_root.join("test-diagnostics/matrix")
}

fn run_scenario(
    name: &str,
    scn: &fs_test_harness::Scenario,
    adapter: &TomlAdapter,
    consumer_root: &Path,
) -> Result<(), Failed> {
    let started = std::time::Instant::now();
    let diag = matrix_diag_root(consumer_root).join(name);
    std::fs::create_dir_all(&diag).map_err(|e| Failed::from(format!("mkdir diag: {e}")))?;

    let outcome = run_scenario_inner(name, scn, adapter, &diag);
    let elapsed = started.elapsed().as_secs_f64();

    let result = match &outcome {
        Ok(()) => ScenarioResult {
            name: name.to_string(),
            status: "passed".into(),
            error: None,
            diag_dir: diag.display().to_string(),
            duration_secs: elapsed,
        },
        Err(e) => ScenarioResult {
            name: name.to_string(),
            status: classify_error(e).into(),
            error: Some(e.clone()),
            diag_dir: diag.display().to_string(),
            duration_secs: elapsed,
        },
    };
    let _ = std::fs::write(
        diag.join("result.json"),
        serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".into()),
    );

    match outcome {
        Ok(()) => Ok(()),
        Err(msg) => Err(Failed::from(format!("{msg} (diag at {})", diag.display()))),
    }
}

fn classify_error(e: &str) -> &'static str {
    if e.contains("VERDICT=failed") {
        "failed"
    } else {
        "errored"
    }
}

fn run_scenario_inner(
    name: &str,
    scn: &fs_test_harness::Scenario,
    adapter: &TomlAdapter,
    diag: &Path,
) -> Result<(), String> {
    // Verify the image (if any) exists before we light up the mount.
    if !scn.image.is_empty() {
        let image_abs = adapter.image_dir.join(&scn.image);
        if !image_abs.is_file() {
            return Err(format!(
                "test image not found: {} (set HARNESS_IMAGE_DIR or [vm.image_dir])",
                image_abs.display()
            ));
        }
    }

    // Materialise the per-scenario JSON the PS runner consumes.
    let scenario_json = adapter.build_scenario_json(name, scn);
    let scenario_json_path = diag.join("scenario.json");
    std::fs::write(
        &scenario_json_path,
        serde_json::to_string_pretty(&scenario_json).unwrap_or_else(|_| "{}".into()),
    )
    .map_err(|e| format!("write scenario.json: {e}"))?;

    let _guard = MOUNT_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let ps_script = adapter.run_scenario_ps_path();
    if !ps_script.is_file() {
        return Err(format!("run-scenario.ps1 not found at {}", ps_script.display()));
    }
    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ])
    .arg(&ps_script)
    .args(["-ScenarioName", name])
    .args([
        "-ScenarioJson",
        &scenario_json_path.display().to_string(),
    ])
    .args(["-Diag", &diag.display().to_string()]);

    let output = cmd
        .output()
        .map_err(|e| format!("spawn run-scenario.ps1: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::write(diag.join("ps-stdout.txt"), &stdout);
    let _ = std::fs::write(diag.join("ps-stderr.txt"), &stderr);

    if !output.status.success() {
        return Err(format!(
            "run-scenario.ps1 exit {:?}: {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    let verdict = stdout
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("VERDICT="))
        .unwrap_or("unknown");
    match verdict {
        "passed" => Ok(()),
        "failed" => {
            let err = stdout
                .lines()
                .rev()
                .find_map(|l| l.trim().strip_prefix("ERROR="))
                .unwrap_or("(no ERROR= marker)");
            Err(format!("VERDICT=failed: {err}"))
        }
        "errored" => {
            let err = stdout
                .lines()
                .rev()
                .find_map(|l| l.trim().strip_prefix("ERROR="))
                .unwrap_or("(no ERROR= marker)");
            Err(format!("VERDICT=errored: {err}"))
        }
        other => Err(format!(
            "VERDICT={other}: unexpected verdict marker; ps-stdout.txt has full output"
        )),
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
