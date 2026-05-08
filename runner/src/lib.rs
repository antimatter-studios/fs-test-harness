//! fs-test-harness runner library.
//!
//! Public API:
//! - [`Harness`]: loaded `harness.toml` + matrix.json.
//! - [`Scenario`], [`OpSpec`]: data shape produced from the matrix.
//! - [`Adapter`]: pluggable behaviour for ops and lifecycle hooks.
//! - [`TomlAdapter`]: default impl driven entirely by `harness.toml`'s
//!   `[ops]` template table; used by the `run-matrix` binary.
//!
//! Most consumers will not need to touch this crate at all -- they
//! will run the `run-matrix` binary and configure behaviour via
//! `harness.toml`. Consumers that need real Rust for an op (FFI probe,
//! in-process mount test, etc) can `impl Adapter for MyDriver` and
//! drive the loop themselves.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod config;
mod matrix;
mod report;
mod substitution;
mod toml_adapter;

#[cfg(test)]
mod tests;

pub use config::{HarnessConfig, OpDef, OpHost};
pub use matrix::{Matrix, MountSpec, OpSpec, PostVerifySpec, Scenario, Step};
pub use report::{RunReport, ScenarioResult};
pub use substitution::Substitution;
pub use toml_adapter::TomlAdapter;

/// Loaded view of `harness.toml` + the consumer's matrix file.
pub struct Harness {
    pub config: HarnessConfig,
    pub matrix: Matrix,
    pub config_path: PathBuf,
    pub matrix_path: PathBuf,
    pub consumer_root: PathBuf,
}

/// Outcome of running a single op.
#[derive(Serialize, Deserialize, Clone)]
pub struct OpResult {
    pub ok: bool,
    pub error: Option<String>,
    pub output: serde_json::Value,
    pub duration_ms: u64,
}

/// The pluggable behaviour for executing scenarios.
///
/// The default [`TomlAdapter`] is driven by `harness.toml`'s `[ops]`
/// template table and is good enough for any project whose driver is a
/// CLI binary. For projects that need real Rust (FFI, in-process
/// mount), implement this trait directly.
pub trait Adapter {
    /// Optional: invoked once before the scenario's ops run. Default no-op.
    fn pre_fixture(&self, _scenario: &Scenario, _diag_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    /// Run a single op against `scenario`. Implementations should write
    /// any per-op artefacts under `diag_dir`.
    fn run_op(&self, scenario: &Scenario, op: &OpSpec, diag_dir: &Path)
        -> anyhow::Result<OpResult>;

    /// Optional: invoked once after the scenario's ops succeed (and
    /// before any post-verify hook). Default no-op.
    fn post_verify(&self, _scenario: &Scenario, _diag_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }
}

impl Harness {
    /// Load the harness from `harness.toml` and the matrix file it
    /// points to (relative to the toml's parent dir, defaulting to
    /// `test-matrix.json`).
    pub fn load(config_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let config_path = config_path.as_ref().to_path_buf();
        let config_text = std::fs::read_to_string(&config_path)?;
        let config: HarnessConfig = toml::from_str(&config_text)?;
        let consumer_root = config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let matrix_rel = config
            .project
            .matrix_path
            .clone()
            .unwrap_or_else(|| "test-matrix.json".to_string());
        let matrix_path = consumer_root.join(&matrix_rel);
        let raw = std::fs::read_to_string(&matrix_path)?;
        let matrix: Matrix = serde_json::from_str(&raw)?;
        Ok(Self {
            config,
            matrix,
            config_path,
            matrix_path,
            consumer_root,
        })
    }

    /// Iterate scenarios in declaration order.
    pub fn scenarios(&self) -> &BTreeMap<String, Scenario> {
        &self.matrix.scenarios
    }
}
