//! `harness.toml` schema (deserialised via `serde` + `toml`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct HarnessConfig {
    pub project: ProjectSection,
    #[serde(default)]
    pub vm: VmSection,
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
    #[serde(default)]
    pub ops: BTreeMap<String, String>,
    #[serde(default)]
    pub mount: Option<MountSection>,
    #[serde(default)]
    pub post_verify: Option<PostVerifySection>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct ProjectSection {
    /// Human-readable consumer name. Used in setup-local prompts and
    /// the diag manifest.
    pub name: String,
    /// Path (relative to harness.toml) to the consumer's binary on the
    /// VM. Substituted into `[ops]` templates as `{binary}`.
    #[serde(default)]
    pub binary: Option<String>,
    /// Path (relative to harness.toml) to the consumer's matrix file.
    /// Default: `test-matrix.json`.
    #[serde(default)]
    pub matrix_path: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct VmSection {
    /// Default `user@host` for SSH; `setup-local.sh` uses this as a prompt default.
    #[serde(default)]
    pub host: Option<String>,
    /// Path to SSH private key (relative to harness.toml or absolute).
    #[serde(default)]
    pub ssh_key: Option<String>,
    /// Remote workdir on the VM. The Mac-side scaffold tars source here.
    #[serde(default)]
    pub workdir: Option<String>,
    /// Remote dir holding test images; substituted as `{image_dir}`.
    #[serde(default)]
    pub image_dir: Option<String>,
    /// winget package IDs to install on first VM provisioning.
    #[serde(default)]
    pub packages: Vec<String>,
    /// Rustup toolchain triple to set as default on the VM.
    #[serde(default)]
    pub rust_toolchain: Option<String>,
    /// Optional PowerShell prefix prepended before the cargo invocation
    /// (e.g. `$env:LIBCLANG_PATH='C:\\Program Files\\LLVM\\bin';`).
    #[serde(default)]
    pub env_prefix: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct MountSection {
    /// Command template for spawning the mount. Substitution tokens:
    /// `{binary}`, `{image}`, `{drive}`, `{extra}`.
    pub command: String,
    /// Regex-ish substring the harness greps for in mount stdout to
    /// declare "mount ready". Empty string => don't wait.
    #[serde(default)]
    pub ready_line: String,
    /// Default `extra` substitution for non-rw scenarios.
    #[serde(default)]
    pub default_extra: Option<String>,
    /// Default `extra` substitution for rw scenarios. Typical: `--rw`.
    #[serde(default)]
    pub rw_extra: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct PostVerifySection {
    /// Default post-verify command. Tokens: `{image}`, `{drive}`.
    pub command: String,
    /// Expected exit code. Default 0.
    #[serde(default)]
    pub expect_exit: Option<i32>,
}
