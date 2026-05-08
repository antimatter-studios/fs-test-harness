//! `harness.toml` schema (deserialised via `serde` + `toml`).
//!
//! Two op-table shapes coexist for back-compat:
//!
//! * v1 `[ops]` — `BTreeMap<String, String>`, one command template per
//!   op-name. Implicit `host = "vm"` for every op (the v1 model spawns
//!   `run-scenario.ps1` once per scenario, all ops execute VM-side).
//! * v2 `[ops.<name>]` — structured table with `host`, `command`,
//!   `expect_exit`, `when`. Each op explicitly declares whether it runs
//!   on the orchestrator host or the VM. The v2 runner dispatches per
//!   step rather than batching the whole scenario through PowerShell.
//!
//! The harness understands both. Consumers migrate at their own pace;
//! `_format = "v2"` in `test-matrix.json` opts a scenario into recipe-
//! shaped execution.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct HarnessConfig {
    pub project: ProjectSection,
    #[serde(default)]
    pub vm: VmSection,
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
    /// Op-name -> definition. Accepts both v1 (bare command string)
    /// and v2 (table with host/command/expect_exit/when) forms.
    #[serde(default)]
    pub ops: BTreeMap<String, OpDef>,
    #[serde(default)]
    pub mount: Option<MountSection>,
    #[serde(default)]
    pub post_verify: Option<PostVerifySection>,
}

/// One declared op. Accepts either a bare command-string (v1) or a
/// table with explicit fields (v2):
///
/// ```toml
/// # v1 form — implicit host=vm, expect_exit=0, no `when`:
/// [ops]
/// ls = "{binary} ls {image} {path}"
///
/// # v2 form — explicit:
/// [ops.format]
/// host = "host"
/// command = "{binary} format {scenario.image} -L {step.params.label}"
/// expect_exit = 0
///
/// [ops.write-fixtures]
/// host = "host"
/// when = "scenario.fixtures"   # only run if scenario.fixtures present
/// command = "{binary} write-fixtures {scenario.image} ..."
/// ```
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(from = "OpDefRaw", into = "OpDefRaw")]
pub struct OpDef {
    /// Where the op runs. v1 forms default to "vm" (matches the
    /// historical "run-scenario.ps1 does everything" model).
    pub host: OpHost,
    /// Command template. Substitution tokens: `{binary}`, `{image}`,
    /// `{drive}`, `{path}`, `{from}`, `{to}`, `{content}`, `{extra}`,
    /// `{tools.<name>}`, `{scenario.<dotted.path>}`, `{step.<field>}`,
    /// and the optional-suffix `{x?}` (yields empty if missing).
    pub command: String,
    /// Expected process exit code. Default 0.
    #[serde(default)]
    pub expect_exit: Option<i32>,
    /// Conditional execution: only run this op when the dotted-path
    /// expression resolves to a non-null, non-empty value. Examples:
    /// `"scenario.fixtures"`, `"step.path"`, `"scenario.volume_params.label"`.
    /// Empty / absent => always run.
    #[serde(default)]
    pub when: Option<String>,
}

/// Where an op runs. The runner dispatches per-step on this value.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum OpHost {
    /// Orchestrator host (Mac, Linux, WSL2 — wherever the runner runs).
    Host,
    /// Windows VM reached via SSH.
    #[default]
    Vm,
}

/// Internal raw form for serde — accepts both `"<string>"` and a
/// table. The public `OpDef` normalises to the table form.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(untagged)]
enum OpDefRaw {
    /// v1: bare command string, implicit `host = "vm"`.
    BareCommand(String),
    /// v2: explicit table.
    Table {
        #[serde(default)]
        host: OpHost,
        command: String,
        #[serde(default)]
        expect_exit: Option<i32>,
        #[serde(default)]
        when: Option<String>,
    },
}

impl From<OpDefRaw> for OpDef {
    fn from(raw: OpDefRaw) -> Self {
        match raw {
            OpDefRaw::BareCommand(command) => OpDef {
                host: OpHost::Vm,
                command,
                expect_exit: None,
                when: None,
            },
            OpDefRaw::Table {
                host,
                command,
                expect_exit,
                when,
            } => OpDef {
                host,
                command,
                expect_exit,
                when,
            },
        }
    }
}

impl From<OpDef> for OpDefRaw {
    fn from(d: OpDef) -> Self {
        OpDefRaw::Table {
            host: d.host,
            command: d.command,
            expect_exit: d.expect_exit,
            when: d.when,
        }
    }
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
