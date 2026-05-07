//! Diagnostic reporting types.
//!
//! The persistent contract is `manifest.json` per scenario plus
//! `results.json` aggregated across the run; together they let an
//! agent reconstruct what was tested, which op failed, and what was
//! observed without re-running anything.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ScenarioResult {
    pub name: String,
    /// One of `passed`, `failed`, `errored`, `blocked`.
    pub status: String,
    pub error: Option<String>,
    pub diag_dir: String,
    pub duration_secs: f64,
}

#[derive(Serialize, Debug)]
pub struct RunManifest {
    pub timestamp_utc: String,
    pub host_os: &'static str,
    pub git_sha: Option<String>,
    pub project_name: String,
    pub scenario_count_total: usize,
    pub scenario_count_runnable: usize,
}

/// Aggregate of all scenario results, written to `results.json` at the
/// end of a run.
#[derive(Serialize, Debug)]
pub struct RunReport {
    pub manifest: RunManifest,
    pub results: Vec<ScenarioResult>,
}
