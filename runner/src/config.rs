//! `harness.toml` schema (deserialised via `serde` + `toml`).
//!
//! Two equivalent op-declaration shapes:
//!
//! * Bare-string shorthand — `ls = "{binary} ls {image} {path}"`.
//!   Sugar for `{ host = "vm", command = <string>, expect_exit = 0,
//!   when = None }`. Convenient for simple ops.
//! * Table form — `[ops.<name>]` with `host`, `command`,
//!   `expect_exit`, `when`. Required when the op runs on the
//!   orchestrator host rather than the VM, when a non-zero exit is
//!   expected, or when a `when` predicate gates the op.
//!
//! Both deserialise to the same `OpDef` struct.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct HarnessConfig {
    pub project: ProjectSection,
    #[serde(default)]
    pub vm: VmSection,
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
    /// Op-name -> definition. Accepts both the bare-string shorthand
    /// (sugar for `command = ..., host = "vm"`) and the full table
    /// form with `host`, `command`, `expect_exit`, `when`.
    #[serde(default)]
    pub ops: BTreeMap<String, OpDef>,
    #[serde(default)]
    pub post_verify: Option<PostVerifySection>,
}

/// One declared op. Accepts either a bare command-string (shorthand)
/// or a table with explicit fields:
///
/// ```toml
/// # bare-string shorthand — implicit host=vm, expect_exit=0, no `when`:
/// [ops]
/// ls = "{binary} ls {image} {path}"
///
/// # table form — explicit:
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
    /// Where the op runs. The bare-string shorthand defaults to "vm"
    /// (matches the typical "this op runs against the mounted volume
    /// on the test VM" pattern); the table form must declare `host`
    /// explicitly.
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
    /// Bare-string shorthand, implicit `host = "vm"`.
    BareCommand(String),
    /// Explicit table form.
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
    /// Human-readable consumer name. Used in `run-tests.sh` bootstrap
    /// prompts and the diag manifest.
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
    /// Default `user@host` for SSH; `run-tests.sh` uses this as a prompt default
    /// during the first-run bootstrap.
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
    /// winget packages to install on first VM provisioning. Each
    /// entry is either a bare PkgId string or a table
    /// `{ id = "PkgId", custom_args = "..." }` where custom_args is
    /// forwarded to the underlying installer via winget's --override
    /// flag (e.g. `ADDLOCAL=F.Main,F.User,F.Developer` for WinFsp's
    /// dev pack). Consumed by `setup-windows-vm.ps1`.
    #[serde(default)]
    pub packages: Vec<PackageSpec>,
    /// Rustup toolchain triple to set as default on the VM.
    #[serde(default)]
    pub rust_toolchain: Option<String>,
    /// Optional PowerShell prefix prepended before the cargo invocation
    /// (e.g. `$env:LIBCLANG_PATH='C:\\Program Files\\LLVM\\bin';`).
    #[serde(default)]
    pub env_prefix: Option<String>,
}

/// A `[vm.packages]` entry. Either a bare PkgId string or a table
/// with `id` + optional `custom_args`. Serde's `untagged` enum
/// resolution matches by structure.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum PackageSpec {
    /// `"PkgId"` -- default features.
    Bare(String),
    /// `{ id = "PkgId", custom_args = "..." }` -- custom_args is
    /// forwarded to the underlying installer via winget --override.
    Table {
        id: String,
        #[serde(default)]
        custom_args: Option<String>,
    },
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct PostVerifySection {
    /// Default post-verify command. Tokens: `{image}`, `{drive}`.
    pub command: String,
    /// Expected exit code. Default 0.
    #[serde(default)]
    pub expect_exit: Option<i32>,
}
