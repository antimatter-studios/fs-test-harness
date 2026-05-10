//! Unit tests for the runner public surface.
//!
//! Kept in a single module so the harness builds the whole tree before
//! running anything; covers the load-bearing paths the CI
//! `runner-unit` job relies on:
//!   1. `Harness::load` round-trips `examples/minimal/harness.toml`.
//!   2. `Matrix` deserialises a recipe with mixed host/vm steps.
//!   3. Consumer-defined scenario fields round-trip through serde so
//!      `{scenario.<dotted.path>}` substitution can reach them.
//!   4. `harness.toml [ops]` accepts both the bare-string shorthand
//!      (sugar for `command = ..., host = "vm"`) and the full table
//!      form with `host`, `when`, `expect_exit`.
//!
//! No filesystem fixtures are written -- everything either reads the
//! checked-in example or operates on in-memory strings.

use crate::{Harness, Matrix};
use std::path::PathBuf;

/// Locate `examples/minimal/harness.toml` relative to this crate.
/// `CARGO_MANIFEST_DIR` is `runner/`, so the example lives one level up.
fn minimal_harness_toml() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("runner has a parent dir")
        .join("examples")
        .join("minimal")
        .join("harness.toml")
}

#[test]
fn harness_load_round_trips_minimal_example() {
    let path = minimal_harness_toml();
    assert!(
        path.is_file(),
        "expected example harness.toml at {}",
        path.display()
    );
    let harness = Harness::load(&path).expect("load minimal harness.toml");

    // Project section round-trips.
    assert_eq!(harness.config.project.name, "minimal-example");
    assert_eq!(
        harness.config.project.binary.as_deref(),
        Some("target/release/myfs.exe")
    );
    assert_eq!(
        harness.config.project.matrix_path.as_deref(),
        Some("test-matrix.json")
    );

    // Op templates carried through verbatim. Bare-string shorthand
    // deserialises into OpDef with the command intact and host=vm.
    let ls = harness.config.ops.get("ls").expect("ls op");
    assert_eq!(ls.command, "{binary} ls {image} {path}");
    assert_eq!(ls.host, crate::config::OpHost::Vm);
    assert!(harness.config.ops.contains_key("cat"));
    assert!(harness.config.ops.contains_key("stat"));

    // Matrix file referenced by the toml resolved + parsed.
    assert!(harness.matrix.scenarios.contains_key("minimal-list-root"));
}

#[test]
fn matrix_deserialises_recipe_with_host_vm_steps() {
    // Recipe shape: per-step host dispatch, free-form step fields.
    // Demonstrates the common NTFS pattern of host-prefix + vm-suffix.
    let raw = r#"
    {
      "scenarios": {
        "format-then-chkdsk": {
          "volume_params": { "size_mib": 256, "label": "TEST", "alloc_unit_size": 4096 },
          "recipe": [
            { "host": "host", "op": "format" },
            { "host": "host", "op": "write", "path": "/hello.txt", "content": "hi" },
            { "host": "vm",   "op": "mount" },
            { "host": "vm",   "op": "chkdsk", "verdict": "clean" },
            { "host": "vm",   "op": "unmount" }
          ]
        }
      }
    }
    "#;
    let matrix: Matrix = serde_json::from_str(raw).expect("matrix parses");
    let scn = matrix
        .scenarios
        .get("format-then-chkdsk")
        .expect("scenario present");
    assert_eq!(scn.recipe.len(), 5);
    assert_eq!(scn.recipe[0]["host"], "host");
    assert_eq!(scn.recipe[0]["op"], "format");
    assert_eq!(scn.recipe[3]["host"], "vm");
    assert_eq!(scn.recipe[3]["op"], "chkdsk");
    assert_eq!(scn.recipe[3]["verdict"], "clean");
}

#[test]
fn scenario_preserves_unknown_consumer_fields_through_round_trip() {
    // Consumer-defined fields on a scenario (volume_params, fixtures,
    // verdict_shape, custom annotations, ...) must round-trip through
    // serde so substitution `{scenario.<dotted.path>}` can reach them
    // at op-template-expand time. Without `#[serde(flatten)]` catch-
    // all on Scenario, these fields are silently dropped on
    // deserialise.
    let raw = r#"
    {
      "scenarios": {
        "consumer-data": {
          "volume_params": { "size_mib": 256, "label": "T", "alloc_unit_size": 4096 },
          "fixtures": [ { "name": "a.txt", "size": 16 } ],
          "verdict_shape": "clean",
          "recipe": [ { "op": "noop" } ]
        }
      }
    }
    "#;
    let matrix: Matrix = serde_json::from_str(raw).expect("parses");
    let scn = matrix.scenarios.get("consumer-data").expect("scenario");

    // Unknown fields land in the flatten catch-all.
    assert_eq!(
        scn.extra
            .get("volume_params")
            .and_then(|v| v.get("size_mib")),
        Some(&serde_json::json!(256))
    );
    assert_eq!(
        scn.extra.get("verdict_shape").and_then(|v| v.as_str()),
        Some("clean")
    );
    assert!(scn.extra.contains_key("fixtures"));

    // Re-serialising the typed Scenario re-emits the unknown fields
    // in the JSON output so the dispatcher can substitute against
    // `{scenario.volume_params.size_mib}` etc. at runtime.
    let re_emitted = serde_json::to_value(scn).expect("re-emit");
    assert_eq!(re_emitted["volume_params"]["size_mib"], 256);
    assert_eq!(re_emitted["verdict_shape"], "clean");
    assert_eq!(re_emitted["fixtures"][0]["name"], "a.txt");
}

#[test]
fn config_op_def_accepts_string_shorthand_and_table_form() {
    use crate::config::{HarnessConfig, OpHost};
    // Both shapes mixed in the same `[ops]` table.
    let raw = r#"
[project]
name = "mixed"

[ops]
# bare-string shorthand — implicit host=vm, no expect_exit, no when.
ls = "{binary} ls {image} {path}"

# table form — explicit host=host, expect_exit, etc.
[ops.format]
host = "host"
command = "{binary} format {scenario.image} -L {step.params.label?}"
expect_exit = 0

# table form with conditional op (when predicate)
[ops.write-fixtures]
host = "host"
when = "scenario.fixtures"
command = "{binary} write-fixtures {scenario.image}"
"#;
    let cfg: HarnessConfig = toml::from_str(raw).expect("mixed config parses");

    let ls = cfg.ops.get("ls").expect("ls present");
    assert_eq!(ls.command, "{binary} ls {image} {path}");
    assert_eq!(ls.host, OpHost::Vm, "bare string defaults host to vm");
    assert_eq!(ls.expect_exit, None);
    assert_eq!(ls.when, None);

    let format = cfg.ops.get("format").expect("format present");
    assert_eq!(format.host, OpHost::Host);
    assert_eq!(format.expect_exit, Some(0));
    assert!(format.command.contains("{step.params.label?}"));

    let wf = cfg
        .ops
        .get("write-fixtures")
        .expect("write-fixtures present");
    assert_eq!(wf.host, OpHost::Host);
    assert_eq!(wf.when.as_deref(), Some("scenario.fixtures"));
}
