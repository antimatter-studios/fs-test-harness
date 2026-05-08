//! v2 recipe dispatcher.
//!
//! Walks `Scenario.recipe[]` in order, dispatches each step to the
//! orchestrator host (`host = "host"`) or the Windows VM (`host =
//! "vm"`) per the matching `[ops.<name>]` declaration in
//! `harness.toml`.
//!
//! Where v1 spawns `run-scenario.ps1` once per scenario (all execution
//! on a single host), v2 dispatches per step. The runner runs on the
//! orchestrator (Mac / Linux / WSL2), invokes local subprocesses for
//! host-steps, and SSHes one command per vm-step. The PowerShell
//! per-scenario driver is bypassed entirely.
//!
//! Per-step diag is written under `<diag_dir>/step-<NN>-<op>/` —
//! one log file each for stdout, stderr, and a JSON record carrying
//! the resolved command, exit code, duration, and host.

use crate::config::{HarnessConfig, OpHost};
use crate::matrix::{Scenario, Step};
use crate::substitution::Substitution;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// Outcome of executing a single recipe step.
#[derive(Serialize, Debug, Clone)]
pub struct StepResult {
    pub index: usize,
    pub op: String,
    pub host: &'static str,
    pub command: String,
    pub exit_code: Option<i32>,
    pub expected_exit: i32,
    pub duration_ms: u128,
    pub skipped: bool,
    pub skip_reason: Option<String>,
    pub error: Option<String>,
}

/// Aggregate outcome of a recipe.
#[derive(Serialize, Debug, Clone)]
pub struct RecipeResult {
    pub steps: Vec<StepResult>,
    pub overall_passed: bool,
}

/// Walk `scenario.recipe` and dispatch each step.
///
/// Returns `Ok(RecipeResult)` even on step failure — the caller
/// inspects `overall_passed` to decide test verdict. Only genuine
/// runner-internal errors (failed to compose substitution context,
/// missing op declaration, IO failure on diag write) bubble up via
/// `Err`.
pub fn run_recipe(
    scenario_name: &str,
    scenario: &Scenario,
    config: &HarnessConfig,
    consumer_root: &Path,
    diag_dir: &Path,
) -> Result<RecipeResult, String> {
    if scenario.recipe.is_empty() {
        return Err("run_recipe called with empty recipe".to_string());
    }

    let scenario_value = serde_json::to_value(scenario)
        .map_err(|e| format!("serialise scenario: {e}"))?;

    // Build the flat-vocabulary substitution map once. Per-step
    // substitution clones it cheaply (BTreeMap of <100 small strings).
    let flat = build_flat_vocab(config, consumer_root)?;

    let mut step_results = Vec::with_capacity(scenario.recipe.len());
    let mut overall_passed = true;

    for (idx, step) in scenario.recipe.iter().enumerate() {
        let step_dir = diag_dir.join(format!("step-{idx:02}"));
        std::fs::create_dir_all(&step_dir)
            .map_err(|e| format!("mkdir {}: {e}", step_dir.display()))?;

        let result = run_step(
            idx,
            step,
            scenario_name,
            &scenario_value,
            config,
            &flat,
            &step_dir,
        )?;

        let _ = std::fs::write(
            step_dir.join("step.json"),
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        );

        if !result.skipped && (result.error.is_some() || !exit_matches(&result)) {
            overall_passed = false;
            step_results.push(result);
            // Fail-fast: don't run subsequent steps on a failure. The
            // recipe is a sequence; later steps presume earlier ones
            // succeeded.
            break;
        }
        step_results.push(result);
    }

    Ok(RecipeResult {
        steps: step_results,
        overall_passed,
    })
}

fn run_step(
    idx: usize,
    step: &Step,
    scenario_name: &str,
    scenario_value: &serde_json::Value,
    config: &HarnessConfig,
    flat: &BTreeMap<String, String>,
    step_dir: &Path,
) -> Result<StepResult, String> {
    // Resolve op-name: `op` field, fallback `type` (matches v1 alias).
    let op_name = step
        .get("op")
        .and_then(|v| v.as_str())
        .or_else(|| step.get("type").and_then(|v| v.as_str()))
        .ok_or_else(|| {
            format!("scenario '{scenario_name}' step {idx}: missing 'op' or 'type' field")
        })?
        .to_string();

    let op_def = config.ops.get(&op_name).ok_or_else(|| {
        format!(
            "scenario '{scenario_name}' step {idx}: op '{op_name}' not declared in harness.toml [ops]"
        )
    })?;

    // Per-step host override: the step's `host` field wins, else the
    // op-def's host. Unknown values fall back to op-def.
    let host = step
        .get("host")
        .and_then(|v| v.as_str())
        .and_then(|s| match s {
            "host" => Some(OpHost::Host),
            "vm" => Some(OpHost::Vm),
            _ => None,
        })
        .unwrap_or(op_def.host);

    let sub = Substitution {
        flat: flat.clone(),
        scenario: scenario_value.clone(),
        step: step.clone(),
    };

    // `when` predicate: skip when false.
    if let Some(when) = &op_def.when {
        if !sub.evaluate_when(when) {
            return Ok(StepResult {
                index: idx,
                op: op_name,
                host: host_name(host),
                command: String::new(),
                exit_code: None,
                expected_exit: op_def.expect_exit.unwrap_or(0),
                duration_ms: 0,
                skipped: true,
                skip_reason: Some(format!("when={when} false")),
                error: None,
            });
        }
    }

    let command = sub.expand(&op_def.command);
    let expected_exit = op_def.expect_exit.unwrap_or(0);

    let started = Instant::now();
    let outcome = match host {
        OpHost::Host => run_local(&command, step_dir),
        OpHost::Vm => run_vm(&command, &config.vm.host, &config.vm.ssh_key, step_dir),
    };
    let duration = started.elapsed();

    match outcome {
        Ok(exit_code) => Ok(StepResult {
            index: idx,
            op: op_name,
            host: host_name(host),
            command,
            exit_code,
            expected_exit,
            duration_ms: duration.as_millis(),
            skipped: false,
            skip_reason: None,
            error: None,
        }),
        Err(e) => Ok(StepResult {
            index: idx,
            op: op_name,
            host: host_name(host),
            command,
            exit_code: None,
            expected_exit,
            duration_ms: duration.as_millis(),
            skipped: false,
            skip_reason: None,
            error: Some(e),
        }),
    }
}

fn run_local(command: &str, step_dir: &Path) -> Result<Option<i32>, String> {
    // POSIX shell on Unix; cmd.exe on Windows. The latter would only
    // be used if someone runs the orchestrator on Windows directly
    // (not the typical scaffolding flow), so the shell choice is
    // best-effort here.
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };

    spawn_with_diag(&mut cmd, step_dir)
}

fn run_vm(
    command: &str,
    vm_host: &Option<String>,
    ssh_key: &Option<String>,
    step_dir: &Path,
) -> Result<Option<i32>, String> {
    let host = vm_host
        .as_deref()
        .ok_or_else(|| "vm-step requires harness.toml [vm].host".to_string())?;

    let mut cmd = Command::new("ssh");
    cmd.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=4",
    ]);
    if let Some(key) = ssh_key {
        cmd.args(["-i", key, "-o", "IdentitiesOnly=yes"]);
    }
    cmd.arg(host);
    cmd.arg(command);

    spawn_with_diag(&mut cmd, step_dir)
}

/// Run a `Command` and capture stdout/stderr to `step_dir`.
fn spawn_with_diag(cmd: &mut Command, step_dir: &Path) -> Result<Option<i32>, String> {
    let output = cmd.output().map_err(|e| format!("spawn: {e}"))?;
    let _ = std::fs::write(step_dir.join("stdout.txt"), &output.stdout);
    let _ = std::fs::write(step_dir.join("stderr.txt"), &output.stderr);
    Ok(output.status.code())
}

fn host_name(h: OpHost) -> &'static str {
    match h {
        OpHost::Host => "host",
        OpHost::Vm => "vm",
    }
}

fn exit_matches(r: &StepResult) -> bool {
    r.exit_code == Some(r.expected_exit)
}

/// Compose the v1 flat-vocabulary tokens from `harness.toml`. These
/// are the values consumers reference as `{binary}`, `{tools.fsck}`,
/// etc. v2 ops typically prefer dotted-paths (`{scenario.image}`,
/// `{step.path}`) but the flat surface is preserved for back-compat
/// and convenience.
fn build_flat_vocab(
    config: &HarnessConfig,
    consumer_root: &Path,
) -> Result<BTreeMap<String, String>, String> {
    let mut flat = BTreeMap::new();
    if let Some(b) = &config.project.binary {
        let binary_abs = if PathBuf::from(b).is_absolute() {
            b.clone()
        } else {
            consumer_root.join(b).display().to_string()
        };
        flat.insert("binary".to_string(), binary_abs);
    }
    for (name, value) in &config.tools {
        flat.insert(format!("tools.{name}"), value.clone());
    }
    Ok(flat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{OpDef, OpHost as Host, ProjectSection};
    use serde_json::json;

    fn config_with_ops(ops: &[(&str, OpDef)]) -> HarnessConfig {
        let mut cfg = HarnessConfig {
            project: ProjectSection {
                name: "test".into(),
                binary: Some("/usr/bin/true".into()),
                matrix_path: None,
            },
            ..Default::default()
        };
        for (name, def) in ops {
            cfg.ops.insert(name.to_string(), def.clone());
        }
        cfg
    }

    fn scenario_with_recipe(recipe: Vec<serde_json::Value>) -> Scenario {
        Scenario {
            image: String::new(),
            mount: None,
            mount_args: vec![],
            ops: vec![],
            recipe,
            post_verify: None,
            status: None,
            attempts: None,
            notes: None,
            evidence_link: None,
        }
    }

    #[test]
    fn empty_recipe_errors() {
        let cfg = config_with_ops(&[]);
        let scn = scenario_with_recipe(vec![]);
        let dir = tempdir();
        let result = run_recipe("empty", &scn, &cfg, &dir, &dir);
        assert!(result.is_err());
    }

    #[test]
    fn host_step_runs_locally_and_records_exit() {
        let cfg = config_with_ops(&[(
            "noop",
            OpDef {
                host: Host::Host,
                command: "/usr/bin/true".into(),
                expect_exit: Some(0),
                when: None,
            },
        )]);
        let scn = scenario_with_recipe(vec![json!({ "op": "noop" })]);
        let dir = tempdir();
        let result = run_recipe("host-noop", &scn, &cfg, &dir, &dir).expect("recipe runs");
        assert!(result.overall_passed);
        assert_eq!(result.steps.len(), 1);
        assert_eq!(result.steps[0].host, "host");
        assert_eq!(result.steps[0].exit_code, Some(0));
        assert!(!result.steps[0].skipped);
    }

    #[test]
    fn step_skipped_when_predicate_false() {
        let cfg = config_with_ops(&[(
            "needs-fixtures",
            OpDef {
                host: Host::Host,
                command: "/usr/bin/false".into(), // would fail if run
                expect_exit: Some(0),
                when: Some("scenario.fixtures".into()),
            },
        )]);
        // No `fixtures` field on scenario => `when` is false => skip.
        let scn = scenario_with_recipe(vec![json!({ "op": "needs-fixtures" })]);
        let dir = tempdir();
        let result = run_recipe("skipped", &scn, &cfg, &dir, &dir).expect("recipe runs");
        assert!(result.overall_passed);
        assert_eq!(result.steps.len(), 1);
        assert!(result.steps[0].skipped);
        assert!(result.steps[0].skip_reason.is_some());
    }

    #[test]
    fn unknown_op_returns_error() {
        let cfg = config_with_ops(&[]);
        let scn = scenario_with_recipe(vec![json!({ "op": "doesnt-exist" })]);
        let dir = tempdir();
        let err = run_recipe("unknown", &scn, &cfg, &dir, &dir).unwrap_err();
        assert!(err.contains("doesnt-exist"));
    }

    #[test]
    fn step_failure_stops_recipe() {
        let cfg = config_with_ops(&[
            (
                "fail",
                OpDef {
                    host: Host::Host,
                    command: "/usr/bin/false".into(),
                    expect_exit: Some(0),
                    when: None,
                },
            ),
            (
                "after",
                OpDef {
                    host: Host::Host,
                    command: "/usr/bin/true".into(),
                    expect_exit: Some(0),
                    when: None,
                },
            ),
        ]);
        let scn = scenario_with_recipe(vec![json!({ "op": "fail" }), json!({ "op": "after" })]);
        let dir = tempdir();
        let result = run_recipe("fail-fast", &scn, &cfg, &dir, &dir).expect("recipe runs");
        assert!(!result.overall_passed);
        // Recipe should fail-fast on the first non-zero step.
        assert_eq!(result.steps.len(), 1);
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "fs-test-harness-dispatch-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
