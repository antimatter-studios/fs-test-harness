//! Unit tests for the runner public surface.
//!
//! Kept in a single module so the harness builds the whole tree before
//! running anything; covers the three load-bearing paths the CI
//! `runner-unit` job relies on:
//!   1. `Harness::load` round-trips `examples/minimal/harness.toml`.
//!   2. `Matrix` deserialises both the `ops` field and the legacy
//!      `operations` alias.
//!   3. `TomlAdapter::expand_template` substitutes declared tokens and
//!      collapses undeclared ones to empty (per the schema).
//!
//! No filesystem fixtures are written -- everything either reads the
//! checked-in example or operates on in-memory strings.

use crate::{Harness, Matrix, TomlAdapter};
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

    // Op templates carried through verbatim. v1 bare-string form
    // deserialises into OpDef with the command intact and host=vm.
    let ls = harness.config.ops.get("ls").expect("ls op");
    assert_eq!(ls.command, "{binary} ls {image} {path}");
    assert_eq!(ls.host, crate::config::OpHost::Vm);
    assert!(harness.config.ops.contains_key("cat"));
    assert!(harness.config.ops.contains_key("stat"));

    // Mount section parsed.
    let mount = harness.config.mount.as_ref().expect("mount section");
    assert!(mount.command.contains("{binary} mount"));
    assert_eq!(mount.ready_line, "myfs mounted at");

    // Matrix file referenced by the toml resolved + parsed.
    assert!(harness.matrix.scenarios.contains_key("minimal-list-root"));
}

#[test]
fn matrix_deserialises_ops_and_legacy_operations_alias() {
    // Two scenarios: one uses `ops`, the other uses `operations`. Serde's
    // `alias = "operations"` means both should land in `Scenario::ops`.
    let raw = r#"
    {
      "_format": "v1",
      "_doc": "synthetic fixture for the alias test",
      "scenarios": {
        "uses-ops": {
          "image": "fixtures/a.img",
          "ops": [
            { "type": "ls", "path": "/" }
          ]
        },
        "uses-operations": {
          "image": "fixtures/b.img",
          "operations": [
            { "type": "cat", "path": "/hello.txt" },
            { "type": "stat", "path": "/hello.txt" }
          ]
        }
      }
    }
    "#;
    let matrix: Matrix = serde_json::from_str(raw).expect("matrix parses");

    let ops_scenario = matrix.scenarios.get("uses-ops").expect("uses-ops present");
    assert_eq!(ops_scenario.ops.len(), 1);
    assert_eq!(ops_scenario.ops[0]["type"], "ls");

    let alias_scenario = matrix
        .scenarios
        .get("uses-operations")
        .expect("uses-operations present");
    assert_eq!(
        alias_scenario.ops.len(),
        2,
        "legacy `operations` alias must populate ops"
    );
    assert_eq!(alias_scenario.ops[0]["type"], "cat");
    assert_eq!(alias_scenario.ops[1]["type"], "stat");
}

#[test]
fn matrix_deserialises_v2_recipe_with_host_vm_steps() {
    // v2 recipe shape: per-step host dispatch, free-form step fields,
    // mutually exclusive with the v1 `ops` array. Demonstrates the
    // common NTFS pattern of host-prefix + vm-suffix.
    let raw = r#"
    {
      "_format": "v2",
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
    let matrix: Matrix = serde_json::from_str(raw).expect("v2 matrix parses");
    let scn = matrix
        .scenarios
        .get("format-then-chkdsk")
        .expect("scenario present");
    assert!(scn.ops.is_empty(), "v2 leaves the v1 `ops` field empty");
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
    // serde so v2 substitution `{scenario.<dotted.path>}` can reach
    // them at op-template-expand time. Without `#[serde(flatten)]`
    // catch-all on Scenario, these fields are silently dropped on
    // deserialise.
    let raw = r#"
    {
      "_format": "v2",
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
    // in the JSON output so the v2 dispatcher can substitute against
    // `{scenario.volume_params.size_mib}` etc. at runtime.
    let re_emitted = serde_json::to_value(scn).expect("re-emit");
    assert_eq!(re_emitted["volume_params"]["size_mib"], 256);
    assert_eq!(re_emitted["verdict_shape"], "clean");
    assert_eq!(re_emitted["fixtures"][0]["name"], "a.txt");
}

#[test]
fn config_op_def_accepts_v1_string_and_v2_table() {
    use crate::config::{HarnessConfig, OpHost};
    // v1 + v2 mixed in the same `[ops]` table.
    let raw = r#"
[project]
name = "mixed"

[ops]
# v1 — bare string, implicit host=vm
ls = "{binary} ls {image} {path}"

# v2 — table form, explicit host=host
[ops.format]
host = "host"
command = "{binary} format {scenario.image} -L {step.params.label?}"
expect_exit = 0

# v2 — conditional op
[ops.write-fixtures]
host = "host"
when = "scenario.fixtures"
command = "{binary} write-fixtures {scenario.image}"
"#;
    let cfg: HarnessConfig = toml::from_str(raw).expect("mixed config parses");

    let ls = cfg.ops.get("ls").expect("ls present");
    assert_eq!(ls.command, "{binary} ls {image} {path}");
    assert_eq!(ls.host, OpHost::Vm, "v1 string defaults host to vm");
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

#[test]
fn expand_template_substitutes_known_and_collapses_unknown_tokens() {
    let vars = [
        ("path", "/hello.txt"),
        ("image", "/srv/images/test.img"),
        ("drive", "Z:"),
    ];

    // Known tokens get substituted.
    let out = TomlAdapter::expand_template("ls {image} {path}", &vars);
    assert_eq!(out, "ls /srv/images/test.img /hello.txt");

    // Drive substitution preserves trailing colon (Windows path safety).
    let out = TomlAdapter::expand_template("cd {drive}\\foo", &vars);
    assert_eq!(out, "cd Z:\\foo");

    // Undeclared token collapses to empty per the schema contract -- and
    // surrounding whitespace / literal text is preserved.
    let out = TomlAdapter::expand_template("run {image} {undeclared} end", &vars);
    assert_eq!(out, "run /srv/images/test.img  end");

    // Mixed: declared + undeclared in the same template.
    let out = TomlAdapter::expand_template("{drive} {path} extra={extra}", &vars);
    assert_eq!(out, "Z: /hello.txt extra=");

    // No tokens: passes through unchanged.
    let out = TomlAdapter::expand_template("plain text", &vars);
    assert_eq!(out, "plain text");
}
