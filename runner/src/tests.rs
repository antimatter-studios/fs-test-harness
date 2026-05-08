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

    // Op templates carried through verbatim.
    assert_eq!(
        harness.config.ops.get("ls").map(String::as_str),
        Some("{binary} ls {image} {path}")
    );
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
