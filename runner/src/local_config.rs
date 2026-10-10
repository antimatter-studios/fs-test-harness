//! Per-machine local configuration, loaded once from `.test-env`.
//!
//! `.test-env` is a gitignored file in the consumer root that supplies
//! machine-specific values (VM host, SSH key, image dir, …) without
//! touching committed config. Format: `KEY=VALUE` lines; blank lines
//! and `#`-prefixed comments are ignored. A missing file is not an error.
//!
//! `run-tests.sh` writes the file as a sourceable shell script —
//! `export VM_HOST="user@host"` — so a leading `export` is dropped and a
//! value wrapped in one matching pair of quotes is unwrapped. Without
//! that, the file the harness writes itself would resolve `${VM_HOST}` to
//! nothing and every vm-step would fail with "requires [vm].host".
//!
//! harness.toml fields can reference `.test-env` keys directly:
//!
//!   host      = "${VM_HOST}"
//!   image_dir = "${HOST_IMAGE_DIR:-diskimages}"
//!
//! `${VAR}` expands to the value of VAR from `.test-env`, or empty if unset.
//! `${VAR:-default}` expands to the value of VAR, or `default` if unset/empty.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Strip one matching pair of surrounding `"` or `'` quotes.
fn unquote(v: &str) -> &str {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return &v[1..v.len() - 1];
        }
    }
    v
}

/// A host directory the VM also sees, e.g. through virtiofs: the path
/// `VM_SHARE_HOST_DIR` on this machine is `VM_SHARE_GUEST_DIR` in the VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub host: PathBuf,
    /// VM-side root, trailing separators trimmed (`Z:/` becomes `Z:`).
    pub guest: String,
}

/// Machine-local overrides sourced from `.test-env`.
#[derive(Debug, Clone, Default)]
pub struct LocalConfig {
    map: HashMap<String, String>,
    /// Harness checkout path relative to the consumer root; used to construct
    /// `{vm.harness_root}`. Set via `HARNESS_DIR` in `.test-env`. Defaults to the
    /// sibling checkout `../fs-windows-test-harness`, so this is rarely needed
    /// outside non-standard layouts.
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
                let line = line.strip_prefix("export ").unwrap_or(line);
                let (k, v) = line.split_once('=')?;
                let v = unquote(v.trim()).to_owned();
                if v.is_empty() {
                    return None;
                }
                Some((k.trim().to_owned(), v))
            })
            .collect();

        let harness_dir = map.get("HARNESS_DIR").cloned();
        Self { map, harness_dir }
    }

    /// The optional shared directory. Both keys or neither: half a share
    /// would silently fall back to scp, so it is an error instead.
    pub fn share(&self) -> Result<Option<Share>, String> {
        match (
            self.map.get("VM_SHARE_HOST_DIR"),
            self.map.get("VM_SHARE_GUEST_DIR"),
        ) {
            (None, None) => Ok(None),
            (Some(host), Some(guest)) => {
                let host = PathBuf::from(host);
                if !host.is_absolute() {
                    return Err(format!(
                        "VM_SHARE_HOST_DIR must be an absolute path: {}",
                        host.display()
                    ));
                }
                Ok(Some(Share {
                    host,
                    guest: guest.trim_end_matches(['/', '\\']).to_string(),
                }))
            }
            _ => Err(
                "VM_SHARE_HOST_DIR and VM_SHARE_GUEST_DIR must be set together in .test-env"
                    .to_string(),
            ),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> LocalConfig {
        // A counter, not the clock. Tests run in parallel threads of ONE
        // process, so the pid is shared, and macOS's SystemTime ticks in
        // microseconds: two tests starting together got the same path, and
        // whichever finished first deleted the other's file (`remove_file`
        // then failed with NotFound, about one run in two).
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "fs-windows-test-harness-local-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&path, text).unwrap();
        let lc = LocalConfig::load(&path);
        std::fs::remove_file(&path).unwrap();
        lc
    }

    #[test]
    fn a_share_needs_both_paths_and_an_absolute_host_dir() {
        assert_eq!(load("VM_HOST=h\n").share(), Ok(None));
        let share = load("VM_SHARE_HOST_DIR=/srv/images\nVM_SHARE_GUEST_DIR=Z:/\n")
            .share()
            .unwrap()
            .unwrap();
        assert_eq!(share.host, std::path::PathBuf::from("/srv/images"));
        assert_eq!(share.guest, "Z:", "trailing separators are trimmed");
        for half in [
            "VM_SHARE_HOST_DIR=/srv/images\n",
            "VM_SHARE_GUEST_DIR=Z:\\\n",
        ] {
            let err = load(half).share().unwrap_err();
            assert!(
                err.contains("VM_SHARE_HOST_DIR") && err.contains("VM_SHARE_GUEST_DIR"),
                "{err}"
            );
        }
        let err = load("VM_SHARE_HOST_DIR=images\nVM_SHARE_GUEST_DIR=Z:\n")
            .share()
            .unwrap_err();
        assert!(err.contains("absolute"), "{err}");
    }

    #[test]
    fn reads_the_file_run_tests_sh_writes() {
        // Verbatim shape of run-tests.sh `write_env_file`.
        let lc = load(concat!(
            "# Generated by fs-windows-test-harness/scripts/run-tests.sh\n",
            "export VM_HOST=\"runneradmin@localhost\"\n",
            "export VM_WORKDIR=\"C:/fswth-vm/smoke-consumer\"\n",
            "export VM_IMAGE_DIR=\"\"\n",
            "export SSH_OPTS=\"-i /k -o IdentitiesOnly=yes\"\n",
            "export SSH_KEY=\"/k\"\n",
        ));
        assert_eq!(lc.expand("${VM_HOST}"), "runneradmin@localhost");
        assert_eq!(lc.expand("${VM_WORKDIR}"), "C:/fswth-vm/smoke-consumer");
        assert_eq!(lc.expand("${SSH_OPTS}"), "-i /k -o IdentitiesOnly=yes");
        // An empty quoted value is unset, so the default applies.
        assert_eq!(lc.expand("${VM_IMAGE_DIR:-images}"), "images");
    }

    #[test]
    fn reads_plain_and_single_quoted_assignments() {
        let lc = load("VM_HOST = user@vm\nSSH_KEY='/a b/key'\nHARNESS_DIR=tools/h\n");
        assert_eq!(lc.expand("${VM_HOST}"), "user@vm");
        assert_eq!(lc.expand("${SSH_KEY}"), "/a b/key");
        assert_eq!(lc.harness_dir.as_deref(), Some("tools/h"));
    }

    #[test]
    fn a_lone_quote_is_kept() {
        let lc = load("A=\"\nB=x\"\n");
        assert_eq!(lc.expand("${A}"), "\"");
        assert_eq!(lc.expand("${B}"), "x\"");
    }
}
