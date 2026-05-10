//! `test-matrix.json` schema.
//!
//! Each scenario carries a `recipe: Vec<Step>` of typed steps. Each
//! step's `host` field (`"host"` or `"vm"`) tells the runner where to
//! dispatch it; the runner walks the recipe in order, spawning host-
//! side commands locally and VM-side commands via SSH.
//!
//! Suits filesystem drivers whose tests need to interleave host-side
//! and VM-side work (`format → write → ship → mount → chkdsk →
//! unmount → verify`).

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

    /// Ordered list of typed steps. Each step is dispatched per its
    /// `host` field by the runner. Steps are passed through to op-
    /// template substitution verbatim; the runner only inspects `host`
    /// and the op-name (`op` or `type`) to decide where and how to
    /// execute.
    ///
    /// See [`crate::config::OpDef`] for the matching `harness.toml`
    /// op declaration shape.
    #[serde(default)]
    pub recipe: Vec<Step>,

    /// Optional post-verify spec; if present, overrides the default
    /// from `harness.toml [post_verify]`. A `null` value disables the
    /// default.
    #[serde(default)]
    pub post_verify: Option<PostVerifySpec>,

    /// Optional per-scenario post-verify spec. Already declared above —
    /// re-mentioned here only to anchor the doc-comment on the
    /// `extra` field below: `post_verify` is one of several typed
    /// "known" fields. Everything *not* enumerated above lands in
    /// `extra` so v2 substitution can reach it via
    /// `{scenario.<dotted.path>}`.

    /// Catch-all for consumer-defined scenario fields the harness
    /// doesn't otherwise know about. Captures things like
    /// `volume_params`, `verdict_shape`, `fixtures`, etc. that
    /// individual fs-* drivers attach to their scenarios. Without
    /// this, those fields would be silently dropped during the
    /// serde round-trip and unreachable from
    /// `{scenario.<dotted.path>}` substitution.
    ///
    /// Agent-bookkeeping fields the harness *does* know about
    /// (`status`, `_attempts`, `_notes`, `evidence_link`) are
    /// captured separately via aliases so they can be skipped on
    /// re-serialise; everything else flattens here and round-trips
    /// untouched.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,

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

/// One step in `scenario.recipe[]`. Free-form JSON value; the
/// runner only inspects two conventional fields:
///
/// * `host` — `"host"` (orchestrator-local) or `"vm"` (Windows VM via
///   SSH). If absent, falls back to the host declared on the matching
///   `[ops.<name>]` entry in `harness.toml`.
/// * `op` (or alias `type`) — looks up the op-template in
///   `harness.toml [ops]`.
///
/// Every other field on the step is surfaced to the op-template via
/// `{step.<field>}` substitution. `scenario.<field>` substitution
/// reaches up to fields on the enclosing scenario (volume_params,
/// fixtures, etc.).
pub type Step = serde_json::Value;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PostVerifySpec {
    pub command: String,
    #[serde(default)]
    pub expect_exit: Option<i32>,
}
