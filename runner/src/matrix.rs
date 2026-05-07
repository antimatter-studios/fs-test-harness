//! `test-matrix.json` schema.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Matrix {
    /// Free-form metadata; the runner ignores anything except `scenarios`.
    #[serde(rename = "_format", default)]
    pub format: Option<String>,
    #[serde(rename = "_doc", default)]
    pub doc: Option<String>,
    pub scenarios: BTreeMap<String, Scenario>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Scenario {
    /// Path (relative to `[vm].image_dir`) to the test image. May be
    /// empty for scenarios that do not need a pre-existing image
    /// (e.g. format-then-mount scenarios).
    #[serde(default)]
    pub image: String,

    /// Scenario-level mount overrides. Most scenarios leave this empty
    /// and inherit `[mount]` from `harness.toml`.
    #[serde(default)]
    pub mount: Option<MountSpec>,

    /// `["--rw"]` style argv. The default mount template substitutes
    /// these into `{extra}`. Whether the scenario is RW is inferred
    /// from the presence of `--rw`.
    #[serde(default)]
    pub mount_args: Vec<String>,

    /// Ordered list of operations to execute on the mounted volume.
    #[serde(default, alias = "operations")]
    pub ops: Vec<OpSpec>,

    /// Optional post-verify spec; if present, overrides the default
    /// from `harness.toml [post_verify]`. A `null` value disables the
    /// default.
    #[serde(default)]
    pub post_verify: Option<PostVerifySpec>,

    // Agent-bookkeeping fields ignored by the runner. They live here
    // for serde round-trip preservation if a consumer ever pipes the
    // scenario through us.
    #[serde(default, skip_serializing)]
    pub status: Option<String>,
    #[serde(default, skip_serializing, rename = "_attempts")]
    pub attempts: Option<serde_json::Value>,
    #[serde(default, skip_serializing, rename = "_notes")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing)]
    pub evidence_link: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MountSpec {
    pub command: String,
    #[serde(default)]
    pub ready_line: String,
}

/// One op in `scenario.ops[]`. Free-form by design: the runner only
/// needs `type` to look up the right template; the rest of the JSON is
/// passed through to the PowerShell side, where templates substitute
/// `{path}`, `{from}`, `{to}`, `{content}`, `{extra}` as available.
pub type OpSpec = serde_json::Value;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PostVerifySpec {
    pub command: String,
    #[serde(default)]
    pub expect_exit: Option<i32>,
}
