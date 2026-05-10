//! v2 recipe dispatcher.
//!
//! Walks `Scenario.recipe[]` in order, dispatches each step to the
//! orchestrator host (`host = "host"`) or the Windows VM (`host =
//! "vm"`) per the matching `[ops.<name>]` declaration in
//! `harness.toml`.
//!
//! The runner runs on the orchestrator (Mac / Linux / WSL2 / Windows),
//! invokes local subprocesses for host-steps, and SSHes one command
//! per vm-step.
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

    let scenario_value =
        serde_json::to_value(scenario).map_err(|e| format!("serialise scenario: {e}"))?;

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
    // Resolve op-name: prefer `op`, fallback to `type` (alternate alias).
    let op_name = step
        .get("op")
        .and_then(|v| v.as_str())
        .or_else(|| step.get("type").and_then(|v| v.as_str()))
        .ok_or_else(|| {
            format!("scenario '{scenario_name}' step {idx}: missing 'op' or 'type' field")
        })?
        .to_string();

    // Built-in transition ops: these don't appear in `harness.toml [ops]`
    // because they're harness-domain primitives, not consumer-domain
    // verbs. The runner recognises them by name and runs them via
    // `scp`/`ssh` directly. Tokens in their `src`/`dest` fields are
    // expanded via the same Substitution machinery as user-defined ops.
    if matches!(op_name.as_str(), "ship-to-vm" | "ship-to-host") {
        return run_builtin_ship(idx, step, &op_name, scenario_value, config, flat, step_dir);
    }

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
    // VM_HOST env wins over harness.toml [vm].host; same for SSH_KEY.
    // Lets the consumer's run-tests.sh source .test-env once and
    // export per-machine values without mutating the committed
    // harness.toml.
    let env_host = std::env::var("VM_HOST").ok().filter(|s| !s.is_empty());
    let env_key  = std::env::var("SSH_KEY").ok().filter(|s| !s.is_empty());

    let host_owned = env_host
        .or_else(|| vm_host.clone())
        .ok_or_else(|| "vm-step requires VM_HOST env or harness.toml [vm].host".to_string())?;
    let key_owned  = env_key.or_else(|| ssh_key.clone());

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
    if let Some(key) = &key_owned {
        cmd.args(["-i", key.as_str(), "-o", "IdentitiesOnly=yes"]);
    }
    cmd.arg(&host_owned);
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

/// Built-in transition op handler — `ship-to-vm` and `ship-to-host`.
///
/// Both take a `src` field (host-side or vm-side path, depending on
/// direction) and a `dest` field (the destination on the opposite
/// host). Substitution applies — `src = "{scenario.image}"` works.
///
/// Implementation: invokes `scp` with the same SSH options the
/// dispatcher uses for `ssh`. Single file or directory; consumer's
/// responsibility to provide a sensible path.
fn run_builtin_ship(
    idx: usize,
    step: &Step,
    op_name: &str,
    scenario_value: &serde_json::Value,
    config: &HarnessConfig,
    flat: &BTreeMap<String, String>,
    step_dir: &Path,
) -> Result<StepResult, String> {
    let sub = Substitution {
        flat: flat.clone(),
        scenario: scenario_value.clone(),
        step: step.clone(),
    };

    let src = step
        .get("src")
        .and_then(|v| v.as_str())
        .map(|s| sub.expand(s))
        .ok_or_else(|| format!("step {idx}: '{op_name}' requires a 'src' field"))?;
    let dest = step
        .get("dest")
        .and_then(|v| v.as_str())
        .map(|s| sub.expand(s))
        .ok_or_else(|| format!("step {idx}: '{op_name}' requires a 'dest' field"))?;

    // VM_HOST env var (set by run-tests.sh after sourcing .test-env)
    // wins over the committed harness.toml [vm].host. Lets a consumer
    // ship a maintainer-default IP in harness.toml without forcing
    // every other dev to edit-and-restore it on each run; the env
    // is the per-machine override.
    let vm_host_env = std::env::var("VM_HOST").ok().filter(|s| !s.is_empty());
    let vm_host_owned: String = vm_host_env
        .or_else(|| config.vm.host.clone())
        .ok_or_else(|| {
            format!("step {idx}: '{op_name}' requires VM_HOST env or harness.toml [vm].host")
        })?;
    let vm_host = vm_host_owned.as_str();

    let started = Instant::now();
    let mut cmd = Command::new("scp");
    cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
    if let Some(key) = &config.vm.ssh_key {
        cmd.args(["-i", key.as_str(), "-o", "IdentitiesOnly=yes"]);
    }
    cmd.arg("-r"); // tolerate directory shipping; single-file is unaffected.
    let (label, src_arg, dest_arg) = if op_name == "ship-to-vm" {
        ("ship-to-vm", src.clone(), format!("{vm_host}:{dest}"))
    } else {
        ("ship-to-host", format!("{vm_host}:{src}"), dest.clone())
    };
    cmd.arg(&src_arg).arg(&dest_arg);

    let outcome = spawn_with_diag(&mut cmd, step_dir);
    let duration = started.elapsed();

    match outcome {
        Ok(exit_code) => Ok(StepResult {
            index: idx,
            op: label.to_string(),
            host: "host", // scp itself runs on the orchestrator host.
            command: format!("scp {src_arg} {dest_arg}"),
            exit_code,
            expected_exit: 0,
            duration_ms: duration.as_millis(),
            skipped: false,
            skip_reason: None,
            error: None,
        }),
        Err(e) => Ok(StepResult {
            index: idx,
            op: label.to_string(),
            host: "host",
            command: format!("scp {src_arg} {dest_arg}"),
            exit_code: None,
            expected_exit: 0,
            duration_ms: duration.as_millis(),
            skipped: false,
            skip_reason: None,
            error: Some(e),
        }),
    }
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

/// Compose the flat-vocabulary tokens from `harness.toml`. These are
/// the values consumers reference as `{binary}`, `{tools.fsck}`, etc.
/// Recipe steps typically prefer dotted paths (`{scenario.image}`,
/// `{step.path}`); the flat surface is the small fixed vocabulary
/// every op gets for free.
fn build_flat_vocab(
    config: &HarnessConfig,
    consumer_root: &Path,
) -> Result<BTreeMap<String, String>, String> {
    let mut flat = BTreeMap::new();
    if let Some(b) = &config.project.binary {
        flat.insert("binary".to_string(), resolve_binary_path(b, consumer_root));
    }
    // `{image_dir}` resolves to (in priority order) the
    // `HARNESS_IMAGE_DIR` env var (matches the v1 override path —
    // `run-matrix.rs` reads the same var for the v1 adapter's
    // image_dir), then `[vm].image_dir`. Relative values are joined
    // against the consumer root so v2 commands can reach the host-
    // side image without each consumer having to spell the prefix
    // out per-op. Combine with `{scenario.image}` to get a full path.
    let env_dir = std::env::var("HARNESS_IMAGE_DIR")
        .ok()
        .filter(|s| !s.is_empty());
    let raw_dir = env_dir.as_deref().or(config.vm.image_dir.as_deref());
    if let Some(d) = raw_dir {
        let resolved = if PathBuf::from(d).is_absolute() {
            d.to_string()
        } else {
            consumer_root.join(d).display().to_string()
        };
        flat.insert("image_dir".to_string(), resolved);
    }

    // VM-side path tokens. Let consumers spell harness.toml command
    // templates against these instead of hard-coding maintainer-
    // specific absolute paths. Two tokens are exposed:
    //
    // * `{vm.workdir}`     — the VM-side consumer-root tar location
    //                        (= harness.toml [vm].workdir, verbatim)
    // * `{vm.harness_root}` — the VM-side path to the vendored harness
    //                        checkout. Auto-derived: workdir joined
    //                        with the harness's path RELATIVE to the
    //                        consumer root (so the same template
    //                        works whether the harness is vendored at
    //                        ./vendor/fs-test-harness/, ./harness/,
    //                        or anywhere else under the consumer).
    // VM_WORKDIR env (from .test-env via run-tests.sh) wins over the
    // committed harness.toml [vm].workdir. Same per-machine override
    // pattern as VM_HOST / SSH_KEY (see run_vm / run_builtin_ship).
    let env_workdir = std::env::var("VM_WORKDIR").ok().filter(|s| !s.is_empty());
    let workdir_owned = env_workdir.or_else(|| config.vm.workdir.clone());
    if let Some(workdir) = &workdir_owned {
        flat.insert("vm.workdir".to_string(), workdir.clone());
        // Compose harness_root from workdir + harness-relative path.
        // HARNESS_DIR env override lets a consumer vendor the harness
        // somewhere other than ./vendor/fs-test-harness/ without
        // editing every op-def template.
        let harness_dir = std::env::var("HARNESS_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "vendor/fs-test-harness".to_string());
        let vm_harness = format!("{}/{}", workdir.trim_end_matches('/'), harness_dir.trim_start_matches('/'));
        flat.insert("vm.harness_root".to_string(), vm_harness);
    }

    for (name, value) in &config.tools {
        flat.insert(format!("tools.{name}"), value.clone());
    }
    Ok(flat)
}

/// Resolve `[project] binary` against the consumer root with two
/// platform-tolerant fallbacks:
///
/// 1. **Canonicalize** the joined path. If it exists, return the
///    canonical form (Windows extended-path prefix stripped so
///    `cmd.exe` / shells parse it cleanly).
/// 2. If the path ended in `.exe` and canonicalize failed, try the
///    same path without the suffix. Lets a single `harness.toml`
///    `binary = "...\\foo.exe"` work on non-Windows hosts where
///    cargo emits the unsuffixed name.
/// 3. If both fail, return the unresolved join — downstream `Command`
///    spawn surfaces a clear "no such file" error rather than the
///    runner panicking.
fn resolve_binary_path(binary: &str, consumer_root: &Path) -> String {
    let candidate = if PathBuf::from(binary).is_absolute() {
        PathBuf::from(binary)
    } else {
        consumer_root.join(binary)
    };

    if let Ok(canon) = std::fs::canonicalize(&candidate) {
        return strip_windows_extended_prefix(canon.display().to_string());
    }

    // .exe fallback for non-Windows hosts running a config that hard-
    // codes the Windows naming.
    if !cfg!(windows) {
        if let Some(stripped) = binary.strip_suffix(".exe") {
            let alt = if PathBuf::from(stripped).is_absolute() {
                PathBuf::from(stripped)
            } else {
                consumer_root.join(stripped)
            };
            if let Ok(canon) = std::fs::canonicalize(&alt) {
                return strip_windows_extended_prefix(canon.display().to_string());
            }
        }
    }

    candidate.display().to_string()
}

fn strip_windows_extended_prefix(s: String) -> String {
    if cfg!(windows) {
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            if !rest.starts_with("UNC\\") {
                return rest.to_string();
            }
        }
    }
    s
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
            recipe,
            post_verify: None,
            extra: serde_json::Map::new(),
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

    #[test]
    fn builtin_ship_recognised_without_ops_table_entry() {
        // Empty `[ops]` — but the recipe uses `ship-to-vm`, which the
        // runner recognises as a built-in. No "op not declared" error.
        let cfg = HarnessConfig {
            project: ProjectSection {
                name: "test".into(),
                binary: None,
                matrix_path: None,
            },
            vm: crate::config::VmSection {
                // No host configured — the ship op should report a
                // clear error, not the generic "op not declared".
                host: None,
                ..Default::default()
            },
            ..Default::default()
        };
        let scn = scenario_with_recipe(vec![json!({
            "op": "ship-to-vm",
            "src": "/tmp/a",
            "dest": "/tmp/b"
        })]);
        let dir = tempdir();
        let err = run_recipe("ship-no-vm", &scn, &cfg, &dir, &dir).unwrap_err();
        assert!(
            err.contains("[vm].host") && err.contains("ship-to-vm"),
            "expected vm.host error, got: {err}"
        );
    }

    #[test]
    fn builtin_ship_requires_src_and_dest_fields() {
        let cfg = config_with_ops(&[]);
        // Missing src
        let scn = scenario_with_recipe(vec![json!({ "op": "ship-to-vm", "dest": "/x" })]);
        let dir = tempdir();
        let err = run_recipe("no-src", &scn, &cfg, &dir, &dir).unwrap_err();
        assert!(
            err.contains("'src' field"),
            "expected src error, got: {err}"
        );

        // Missing dest
        let scn = scenario_with_recipe(vec![json!({ "op": "ship-to-host", "src": "/x" })]);
        let err = run_recipe("no-dest", &scn, &cfg, &dir, &dir).unwrap_err();
        assert!(
            err.contains("'dest' field"),
            "expected dest error, got: {err}"
        );
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
