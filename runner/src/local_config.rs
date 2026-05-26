//! Per-machine local configuration, loaded once from `.test-env`.
//!
//! `.test-env` is a gitignored file in the consumer root that supplies
//! machine-specific values (VM host, SSH key, image dir, …) without
//! touching committed config. Format: `KEY=VALUE` lines; blank lines
//! and `#`-prefixed comments are ignored. A missing file is not an error.
//!
//! harness.toml fields can reference `.test-env` keys directly:
//!
//!   host      = "${VM_HOST}"
//!   image_dir = "${HOST_IMAGE_DIR:-diskimages}"
//!
//! `${VAR}` expands to the value of VAR from `.test-env`, or empty if unset.
//! `${VAR:-default}` expands to the value of VAR, or `default` if unset/empty.

use std::collections::HashMap;
use std::path::Path;

/// Machine-local overrides sourced from `.test-env`.
#[derive(Debug, Clone, Default)]
pub struct LocalConfig {
    map: HashMap<String, String>,
    /// Path where the harness is vendored; used to construct `{vm.harness_root}`.
    /// Set via `HARNESS_DIR` in `.test-env`. Rarely needed outside non-standard layouts.
    pub harness_dir: Option<String>,
}

impl LocalConfig {
    /// Load from `path`. Missing file → empty map. Parse errors on
    /// individual lines are silently skipped.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Self::default(),
        };
        let map: HashMap<String, String> = text
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let (k, v) = line.split_once('=')?;
                let v = v.trim().to_owned();
                if v.is_empty() {
                    return None;
                }
                Some((k.trim().to_owned(), v))
            })
            .collect();

        let harness_dir = map.get("HARNESS_DIR").cloned();
        Self { map, harness_dir }
    }

    /// Expand `${VAR}` and `${VAR:-default}` references using `.test-env` values.
    /// Literal strings without `${` are returned unchanged.
    pub fn expand(&self, s: &str) -> String {
        let mut result = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(start) = rest.find("${") {
            result.push_str(&rest[..start]);
            rest = &rest[start + 2..];
            let end = match rest.find('}') {
                Some(i) => i,
                None => {
                    result.push_str("${");
                    continue;
                }
            };
            let expr = &rest[..end];
            rest = &rest[end + 1..];
            let (var, default) = match expr.find(":-") {
                Some(i) => (&expr[..i], Some(&expr[i + 2..])),
                None => (expr, None),
            };
            let value = self.map.get(var).map(|s| s.as_str()).unwrap_or("");
            if !value.is_empty() {
                result.push_str(value);
            } else if let Some(d) = default {
                result.push_str(d);
            }
        }
        result.push_str(rest);
        result
    }
}
