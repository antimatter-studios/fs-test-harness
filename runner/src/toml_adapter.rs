//! Default [`Adapter`] implementation -- delegates everything to
//! `scripts/run-scenario.ps1` on Windows. The adapter's job is to
//! materialise the per-scenario JSON the PowerShell script consumes,
//! then parse the `VERDICT=` marker out of stdout.
//!
//! We invoke `run-scenario.ps1` once per scenario with the *whole*
//! op list embedded in the scenario JSON -- so this adapter doesn't
//! invoke `run_op` per op the way the trait suggests. The trait method
//! on this impl is therefore just a stub; the per-scenario flow is
//! kicked off by the binary's `run_scenario_inner` directly.

use crate::{config::HarnessConfig, matrix::Scenario, Adapter, OpResult};
use serde_json::json;
use std::path::{Path, PathBuf};

pub struct TomlAdapter {
    pub config: HarnessConfig,
    pub harness_root: PathBuf,
    pub consumer_root: PathBuf,
    pub image_dir: PathBuf,
}

impl TomlAdapter {
    pub fn new(
        config: HarnessConfig,
        harness_root: PathBuf,
        consumer_root: PathBuf,
        image_dir: PathBuf,
    ) -> Self {
        Self {
            config,
            harness_root,
            consumer_root,
            image_dir,
        }
    }

    /// Build the per-scenario JSON the PowerShell runner consumes.
    /// Public so the binary can write it to disk.
    pub fn build_scenario_json(&self, name: &str, scenario: &Scenario) -> serde_json::Value {
        let rw = scenario.mount_args.iter().any(|a| a == "--rw");
        let image_path = if scenario.image.is_empty() {
            String::new()
        } else {
            self.image_dir.join(&scenario.image).display().to_string()
        };

        // Resolve mount: scenario override -> harness default. Substitute
        // {binary} and {extra} now (so PS doesn't need the harness.toml).
        let (mount_command, ready_line, mount_extra) = match (&scenario.mount, &self.config.mount) {
            (Some(s), _) => (s.command.clone(), s.ready_line.clone(), "".to_string()),
            (None, Some(m)) => {
                let extra = scenario.mount_args.join(" ");
                let extra = if extra.is_empty() {
                    if rw {
                        m.rw_extra.clone().unwrap_or_default()
                    } else {
                        m.default_extra.clone().unwrap_or_default()
                    }
                } else {
                    extra
                };
                (m.command.clone(), m.ready_line.clone(), extra)
            }
            (None, None) => (String::new(), String::new(), String::new()),
        };

        // Substitute {binary} now -- PS shouldn't need to know harness.toml.
        let binary = self.config.project.binary.clone().unwrap_or_default();
        let binary_abs = if binary.is_empty() {
            String::new()
        } else if PathBuf::from(&binary).is_absolute() {
            binary.clone()
        } else {
            self.consumer_root.join(&binary).display().to_string()
        };
        let mount_command = mount_command.replace("{binary}", &binary_abs);

        // Resolve templates: substitute {binary} now; PS substitutes
        // per-op tokens at run time.
        let mut templates = serde_json::Map::new();
        for (k, v) in &self.config.ops {
            let v = v.replace("{binary}", &binary_abs);
            // {tools.name} -> the resolved tool path
            let mut v = v;
            for (tname, tpath) in &self.config.tools {
                v = v.replace(&format!("{{tools.{}}}", tname), tpath);
            }
            templates.insert(k.clone(), serde_json::Value::String(v));
        }

        // post_verify: scenario override > default. Substitute {binary} + tools.
        let post_verify = match &scenario.post_verify {
            Some(pv) => Some((pv.command.clone(), pv.expect_exit.unwrap_or(0))),
            None => self
                .config
                .post_verify
                .as_ref()
                .map(|pv| (pv.command.clone(), pv.expect_exit.unwrap_or(0))),
        };
        let post_verify = post_verify.map(|(cmd, exit)| {
            let mut cmd = cmd.replace("{binary}", &binary_abs);
            for (tname, tpath) in &self.config.tools {
                cmd = cmd.replace(&format!("{{tools.{}}}", tname), tpath);
            }
            json!({ "command": cmd, "expect_exit": exit })
        });

        json!({
            "name": name,
            "image": image_path,
            "rw": rw,
            "mount": {
                "command": mount_command,
                "ready_line": ready_line,
                "extra": mount_extra,
            },
            "ops": scenario.ops,
            "templates": serde_json::Value::Object(templates),
            "post_verify": post_verify,
        })
    }

    pub fn run_scenario_ps_path(&self) -> PathBuf {
        self.harness_root.join("scripts").join("run-scenario.ps1")
    }

    /// Substitute `{token}` placeholders in `template` against `vars`.
    ///
    /// This mirrors the `Expand-Template` PowerShell helper used by
    /// `scripts/run-scenario.ps1`: declared tokens get the supplied
    /// value (plain string-replace, so backslashes in Windows paths
    /// pass through unmolested), and any remaining `{...}` placeholder
    /// matching `[A-Za-z_][A-Za-z0-9_]*` is collapsed to the empty
    /// string. The schema treats undeclared tokens as empty.
    ///
    /// Used in unit tests as a stand-alone fixture; the production
    /// per-op substitution still happens on the Windows side.
    pub fn expand_template(template: &str, vars: &[(&str, &str)]) -> String {
        let mut out = template.to_string();
        for (k, v) in vars {
            out = out.replace(&format!("{{{}}}", k), v);
        }
        // Collapse any remaining `{ident}` placeholder to empty.
        let mut result = String::with_capacity(out.len());
        let bytes = out.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'{' {
                if let Some(end) = bytes[i + 1..].iter().position(|&b| b == b'}') {
                    let inner = &bytes[i + 1..i + 1 + end];
                    let is_ident = !inner.is_empty()
                        && (inner[0].is_ascii_alphabetic() || inner[0] == b'_')
                        && inner
                            .iter()
                            .all(|&b| b.is_ascii_alphanumeric() || b == b'_');
                    if is_ident {
                        // Skip the whole `{ident}` placeholder.
                        i += end + 2;
                        continue;
                    }
                }
            }
            result.push(bytes[i] as char);
            i += 1;
        }
        result
    }
}

impl Adapter for TomlAdapter {
    fn run_op(
        &self,
        _scenario: &Scenario,
        _op: &serde_json::Value,
        _diag_dir: &Path,
    ) -> anyhow::Result<OpResult> {
        // Per-op invocation is not the path the default adapter takes.
        // The whole-scenario flow lives in the bin entry point.
        anyhow::bail!("TomlAdapter does not support per-op invocation; the whole-scenario flow is in run-matrix.rs")
    }
}
