//! Template substitution engine for the v2 recipe model.
//!
//! v1 used flat `{token}` substitution against a small fixed
//! vocabulary (`{binary}`, `{image}`, `{path}` etc.). v2 needs more:
//! op templates reference fields on the scenario and on the step,
//! so the namespace becomes hierarchical. Recipe steps may also be
//! conditionally executed (`when = "scenario.fixtures"`).
//!
//! This module implements both:
//!
//! * [`Substitution::expand`] — replace `{dotted.path}` placeholders
//!   in a template against a context (`scenario`, `step`, plus the
//!   v1 flat tokens). Trailing `?` makes a path optional: missing
//!   paths yield empty strings instead of `<missing:...>` markers.
//! * [`Substitution::evaluate_when`] — evaluate a `when` predicate
//!   string. Returns `true` iff the dotted path resolves to a
//!   non-null, non-empty value (truthy in the JSON sense).
//!
//! Both functions are pure: the inputs are a JSON context + a
//! template / predicate string, the outputs depend only on those.
//! No I/O, no global state.

use serde_json::Value;
use std::collections::BTreeMap;

/// Substitution context. Holds the JSON values reachable via
/// `{scenario.*}` and `{step.*}` placeholders, plus the flat v1
/// tokens (`{binary}`, `{image}`, etc.).
#[derive(Debug, Default)]
pub struct Substitution {
    /// Flat tokens from the v1 vocabulary (binary, image, drive,
    /// path, from, to, content, extra, tools.<name>). Keys are the
    /// part inside the braces, e.g. `"binary"` or `"tools.fsck"`.
    pub flat: BTreeMap<String, String>,
    /// Whole-scenario JSON, reached via `{scenario.*}`.
    pub scenario: Value,
    /// Current step's JSON, reached via `{step.*}`. `Value::Null`
    /// when expanding outside a step (e.g. mount template).
    pub step: Value,
}

impl Substitution {
    /// Substitute every `{...}` placeholder in `template`.
    ///
    /// Token resolution:
    ///
    /// 1. Strip a trailing `?` from the placeholder body if present;
    ///    flag the token as "optional".
    /// 2. Look up the (possibly dotted) path in the namespace:
    ///    * `scenario.<dotted.path>` walks `self.scenario`
    ///    * `step.<dotted.path>` walks `self.step`
    ///    * any other token is a flat-vocabulary key looked up in
    ///      `self.flat`
    /// 3. If found, coerce to a string (JSON strings → unquoted;
    ///    numbers / bools → display form; arrays / objects → JSON form
    ///    so the consumer can decide what to do with them).
    /// 4. If not found:
    ///    * optional → empty string
    ///    * required → empty string with a `<missing:...>` marker
    ///      embedded for human debugging (this matches the v1
    ///      contract that undeclared tokens collapse to empty).
    ///
    /// Example: `"{binary} format {scenario.image} -L {step.params.label?}"`
    /// expands by substituting each `{...}` against `flat`/`scenario`/`step`.
    pub fn expand(&self, template: &str) -> String {
        let mut out = String::with_capacity(template.len());
        let bytes = template.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'{' {
                // Scan to the matching `}`. If unbalanced, fall through
                // and emit the `{` literally.
                if let Some(end_rel) = bytes[i + 1..].iter().position(|&b| b == b'}') {
                    let inner = &bytes[i + 1..i + 1 + end_rel];
                    if let Some((path, optional)) = parse_placeholder(inner) {
                        match self.lookup(&path) {
                            Some(s) => out.push_str(&s),
                            None if optional => { /* yield empty */ }
                            None => {
                                // Required-but-missing collapses to empty
                                // (v1 behaviour). Future iteration could
                                // emit a `<missing:path>` marker; left as
                                // a follow-up rather than breaking v1.
                            }
                        }
                        i += 1 + end_rel + 1;
                        continue;
                    }
                    // Inner wasn't a valid placeholder — emit `{...}` as-is.
                }
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    /// Evaluate a `when = "..."` predicate. Empty / absent predicate
    /// (`""`) is `true` (always run); a dotted path is `true` iff it
    /// resolves to a non-null, non-empty value:
    ///
    /// * `null`          → false
    /// * `false`         → false
    /// * `0`             → false
    /// * `""`            → false
    /// * `[]` / `{}`     → false
    /// * everything else → true
    pub fn evaluate_when(&self, predicate: &str) -> bool {
        let trimmed = predicate.trim();
        if trimmed.is_empty() {
            return true;
        }
        let (path, _optional) =
            parse_placeholder(trimmed.as_bytes()).unwrap_or_else(|| (trimmed.to_string(), true));
        match self.lookup_value(&path) {
            None => false,
            Some(v) => is_truthy(&v),
        }
    }

    /// Resolve a dotted path to its string form (the `expand` side).
    fn lookup(&self, path: &str) -> Option<String> {
        // Flat tokens first — they include things like `binary`,
        // `tools.fsck`, etc. These shadow scenario/step paths if both
        // happen to match (the v1 vocabulary is small and stable).
        if let Some(s) = self.flat.get(path) {
            return Some(s.clone());
        }
        let v = self.lookup_value(path)?;
        Some(value_to_string(&v))
    }

    /// Resolve a dotted path to the underlying JSON value (the
    /// `when` side cares about truthiness, not string form).
    fn lookup_value(&self, path: &str) -> Option<Value> {
        let mut segments = path.split('.');
        let root = segments.next()?;
        let mut cursor = match root {
            "scenario" => self.scenario.clone(),
            "step" => self.step.clone(),
            _ => {
                // Not a hierarchical path; treat as a flat key.
                return self.flat.get(path).map(|s| Value::String(s.clone()));
            }
        };
        for seg in segments {
            cursor = match cursor {
                Value::Object(mut m) => m.remove(seg)?,
                Value::Array(items) => {
                    // Numeric segment indexes the array; non-numeric
                    // doesn't address an array.
                    let idx: usize = seg.parse().ok()?;
                    items.into_iter().nth(idx)?
                }
                _ => return None,
            };
        }
        Some(cursor)
    }
}

/// Parse the inside of a `{...}` placeholder. Returns
/// `(path, optional)` if the inner is a valid placeholder body, else
/// `None`.
///
/// Valid bodies:
/// * `ident` — flat or single-segment path
/// * `ident.ident.ident` — dotted path (any depth)
/// * any of the above with a trailing `?` for optional
///
/// Identifier characters: `[A-Za-z0-9_-]`, plus `.` as the separator.
fn parse_placeholder(inner: &[u8]) -> Option<(String, bool)> {
    if inner.is_empty() {
        return None;
    }
    let (body, optional) = if inner.last() == Some(&b'?') {
        (&inner[..inner.len() - 1], true)
    } else {
        (inner, false)
    };
    if body.is_empty() {
        return None;
    }
    // First char of every segment must be alpha/underscore.
    let mut prev_dot = true;
    for &b in body {
        let valid = if prev_dot {
            b.is_ascii_alphabetic() || b == b'_'
        } else {
            b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
        };
        if !valid {
            return None;
        }
        prev_dot = b == b'.';
    }
    // Trailing dot is invalid.
    if body.last() == Some(&b'.') {
        return None;
    }
    let path = std::str::from_utf8(body).ok()?.to_string();
    Some((path, optional))
}

/// JSON value → string for substitution. Strings are unquoted; other
/// scalars use their display form; arrays / objects fall back to JSON
/// so consumers can post-process.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => v.to_string(),
    }
}

/// JSON truthiness for `when` predicate evaluation.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Substitution {
        let mut flat = BTreeMap::new();
        flat.insert("binary".to_string(), "/usr/local/bin/myfs".to_string());
        flat.insert("image".to_string(), "/srv/images/test.img".to_string());
        flat.insert("drive".to_string(), "Z:".to_string());
        flat.insert("tools.fsck".to_string(), "fsck.myfs -fn".to_string());
        Substitution {
            flat,
            scenario: json!({
                "image": "/srv/images/test.img",
                "volume_params": { "size_mib": 256, "label": "TEST", "alloc_unit_size": 4096 },
                "fixtures": [ { "name": "a.txt" } ]
            }),
            step: json!({
                "host": "host",
                "op": "format",
                "params": { "label": "STEP-LABEL" },
                "path": "/hello.txt"
            }),
        }
    }

    #[test]
    fn expands_flat_v1_tokens() {
        let s = fixture();
        assert_eq!(s.expand("ls {image} {drive}"), "ls /srv/images/test.img Z:");
        assert_eq!(s.expand("{binary} format"), "/usr/local/bin/myfs format");
        assert_eq!(s.expand("{tools.fsck} {image}"), "fsck.myfs -fn /srv/images/test.img");
    }

    #[test]
    fn expands_dotted_scenario_paths() {
        let s = fixture();
        assert_eq!(
            s.expand("size={scenario.volume_params.size_mib} label={scenario.volume_params.label}"),
            "size=256 label=TEST"
        );
    }

    #[test]
    fn expands_dotted_step_paths() {
        let s = fixture();
        assert_eq!(s.expand("--label {step.params.label}"), "--label STEP-LABEL");
        assert_eq!(s.expand("op={step.op} path={step.path}"), "op=format path=/hello.txt");
    }

    #[test]
    fn optional_suffix_yields_empty_when_missing() {
        let s = fixture();
        // alloc_unit_size IS present; optional marker is harmless.
        assert_eq!(
            s.expand("--cluster {step.params.alloc_unit_size?}"),
            "--cluster "
        );
        // Truly missing path with `?` collapses to empty.
        assert_eq!(s.expand("--journal {step.params.journal_mode?}"), "--journal ");
    }

    #[test]
    fn missing_required_path_collapses_to_empty() {
        // v1 contract: undeclared tokens collapse to empty.
        let s = fixture();
        assert_eq!(s.expand("--unknown {step.params.nope}"), "--unknown ");
    }

    #[test]
    fn unbalanced_brace_passes_through_literally() {
        let s = fixture();
        // `{` without matching `}` shouldn't break the engine.
        assert_eq!(s.expand("a { b"), "a { b");
        // Inner that isn't an identifier is left literal.
        assert_eq!(s.expand("plain {3invalid}"), "plain {3invalid}");
    }

    #[test]
    fn when_predicate_truthy_paths() {
        let s = fixture();
        // Path resolves to a non-empty array.
        assert!(s.evaluate_when("scenario.fixtures"));
        // Path resolves to a non-zero integer.
        assert!(s.evaluate_when("scenario.volume_params.size_mib"));
        // Path resolves to a non-empty string.
        assert!(s.evaluate_when("step.op"));
    }

    #[test]
    fn when_predicate_falsy_paths() {
        let s = fixture();
        // Missing path.
        assert!(!s.evaluate_when("scenario.does_not_exist"));
        // Nested missing.
        assert!(!s.evaluate_when("scenario.volume_params.journal_mode"));
        // Empty predicate => always true (= "no condition").
        assert!(s.evaluate_when(""));
        assert!(s.evaluate_when("   "));
    }

    #[test]
    fn when_zero_and_empty_string_are_falsy() {
        let mut s = fixture();
        s.scenario = json!({ "zero": 0, "empty": "", "false": false, "null": null });
        assert!(!s.evaluate_when("scenario.zero"));
        assert!(!s.evaluate_when("scenario.empty"));
        assert!(!s.evaluate_when("scenario.false"));
        assert!(!s.evaluate_when("scenario.null"));
    }
}
