//! fs-windows-test-harness runner library.
//!
//! Public API:
//! - [`Harness`]: loaded `harness.toml` + matrix.json.
//! - [`Scenario`], [`Step`]: data shape produced from the matrix.
//! - [`run_recipe`]: walks `scenario.recipe[]` dispatching each step
//!   per its `host` field (host-side spawn or VM-side SSH).
//!
//! Most consumers will not need to touch this crate at all -- they
//! will run the `run-matrix` binary and configure behaviour via
//! `harness.toml`'s `[ops]` table.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod config;
mod dispatch;
pub mod local_config;
mod matrix;
mod report;
mod substitution;

#[cfg(test)]
mod tests;

pub use config::{HarnessConfig, MaxParallel, OpDef, OpHost, RunnerConfig, VmSection};
pub use dispatch::{
    output_with_timeout, parse_step_timeout, run_recipe, run_recipe_observed, step_timeout,
    RecipeResult, StepResult, TimedOut, DEFAULT_STEP_TIMEOUT_SECS, STEP_TIMEOUT_ENV,
};
pub use local_config::LocalConfig;
pub use matrix::{Matrix, PostVerifySpec, Scenario, Step};
pub use report::{RunReport, ScenarioResult};
pub use substitution::Substitution;

/// Loaded view of `harness.toml` + the consumer's matrix file.
pub struct Harness {
    pub config: HarnessConfig,
    pub local_config: LocalConfig,
    pub matrix: Matrix,
    pub config_path: PathBuf,
    pub matrix_path: PathBuf,
    pub consumer_root: PathBuf,
    /// Millisecond timestamp generated once at load time. Exposed as
    /// `{run_id}` in op templates so concurrent runs write to separate
    /// directories without coordinating.
    pub run_id: u128,
}

impl Harness {
    /// Load the harness from `harness.toml` and the matrix file it
    /// points to (relative to the toml's parent dir, defaulting to
    /// `test-matrix.json`).
    pub fn load(config_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let config_path = config_path.as_ref().to_path_buf();
        let config_text = std::fs::read_to_string(&config_path)?;
        let mut config: HarnessConfig = toml::from_str(&config_text)?;
        let consumer_root = config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let local_config = LocalConfig::load(&consumer_root.join(".test-env"));
        config.vm.apply(&local_config);
        let matrix_rel = config
            .project
            .matrix_path
            .clone()
            .unwrap_or_else(|| "test-matrix.json".to_string());
        let matrix_path = consumer_root.join(&matrix_rel);
        let raw = std::fs::read_to_string(&matrix_path)?;
        let matrix: Matrix = serde_json::from_str(&raw)?;
        for (name, scenario) in &matrix.scenarios {
            if scenario
                .exclusive_group
                .as_deref()
                .is_some_and(|group| group.trim().is_empty())
            {
                anyhow::bail!(
                    "{} scenario '{name}': exclusive_group must not be empty",
                    matrix_path.display()
                );
            }
        }
        let run_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        Ok(Self {
            config,
            local_config,
            matrix,
            config_path,
            matrix_path,
            consumer_root,
            run_id,
        })
    }

    /// Iterate scenarios in declaration order.
    pub fn scenarios(&self) -> &BTreeMap<String, Scenario> {
        &self.matrix.scenarios
    }
}
