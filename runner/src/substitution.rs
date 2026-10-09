//! Template substitution engine for the recipe-step dispatcher.
//!
//! Two namespaces in op-template strings:
//!
//! * **Flat tokens** — small fixed vocabulary: `{binary}`, `{image}`,
//!   `{drive}`, `{path}`, `{from}`, `{to}`, `{content}`, `{extra}`,
//!   `{tools.<name>}`. Always available.
//! * **Dotted paths** — `{scenario.<dotted.path>}` and
//!   `{step.<dotted.path>}` reach into the scenario JSON / step JSON
//!   respectively. Trailing `?` makes a path optional: missing paths
//!   yield empty strings. Checked recipe expansion rejects missing required paths.
//!
//! Two entry points:
//!
//! * [`Substitution::expand`] — replace `{path}` placeholders in a
//!   template against the context.
//! * [`Substitution::evaluate_when`] — evaluate a `when` predicate
//!   string. Returns `true` iff the dotted path resolves to a
//!   non-null, non-empty value (truthy in the JSON sense).
//!
//! Both functions are pure: the inputs are a JSON context + a
//! template / predicate string, the outputs depend only on those.
//! No I/O, no global state.

use serde_json::Value;
use std::collections::BTreeMap;

const MAX_TEMPLATE_BYTES: usize = 1_048_576;

/// Substitution context. Holds the JSON values reachable via
/// `{scenario.*}` and `{step.*}` placeholders, plus the flat tokens
/// (`{binary}`, `{image}`, etc.).
#[derive(Debug, Default)]
pub struct Substitution {
    /// Flat tokens (binary, image, drive, path, from, to, content,
    /// extra, tools.<name>). Keys are the part inside the braces,
    /// e.g. `"binary"` or `"tools.fsck"`.
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
    ///    * required → empty string in this legacy API. Recipe dispatch uses
    ///      `expand_checked` to reject missing required paths instead.
    ///
    /// Example: `"{binary} format {scenario.image} -L {step.params.label?}"`
    /// expands by substituting each `{...}` against `flat`/`scenario`/`step`.
    pub fn expand(&self, template: &str) -> String {
        // Keep the legacy missing-token behavior for callers outside recipes.
        self.expand_inner(template, false, &mut Vec::new())
            .unwrap_or_else(|_| template.to_string())
    }

    /// Expand a recipe template, rejecting missing required references and
    /// cyclic or excessively deep step-field templates. Scenario and flat
    /// values are data: their contents are never reinterpreted as templates.
    /// Double braces escape a literal placeholder: `{{step.label}}`.
    pub fn expand_checked(&self, template: &str) -> Result<String, String> {
        self.expand_inner(template, true, &mut Vec::new())
    }

    fn expand_inner(
        &self,
        template: &str,
        checked: bool,
        stack: &mut Vec<String>,
    ) -> Result<String, String> {
        if template.len() > MAX_TEMPLATE_BYTES {
            return Err("recipe template exceeds 1048576 bytes".into());
        }
        let mut out = String::with_capacity(template.len());
        let bytes = template.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if out.len() > MAX_TEMPLATE_BYTES {
                return Err("expanded recipe template exceeds 1048576 bytes".into());
            }
            if template[i..].starts_with("{{") {
                if let Some(end) = template[i + 2..].find("}}") {
                    out.push('{');
                    out.push_str(&template[i + 2..i + 2 + end]);
                    out.push('}');
                    i += end + 4;
                    continue;
                }
            }
            if bytes[i] == b'{' {
                if let Some(end_rel) = bytes[i + 1..].iter().position(|&b| b == b'}') {
                    let inner = &bytes[i + 1..i + 1 + end_rel];
                    if let Some((path, optional)) = parse_placeholder(inner) {
                        match self.lookup(&path) {
                            Some(value) => {
                                if path.starts_with("step.")
                                    && !self.flat.contains_key(&path)
                                    && self
                                        .lookup_value(&path)
                                        .is_some_and(|value| value.is_string())
                                {
                                    if stack.contains(&path) {
                                        return Err(format!(
                                            "cyclic recipe reference: {} -> {path}",
                                            stack.join(" -> ")
                                        ));
                                    }
                                    if stack.len() >= 32 {
                                        return Err(format!(
                                            "recipe reference depth exceeds 32 at {path}"
                                        ));
                                    }
                                    stack.push(path);
                                    let expanded = self.expand_inner(&value, checked, stack);
                                    stack.pop();
                                    out.push_str(&expanded?);
                                } else {
                                    out.push_str(&value);
                                }
                            }
                            None if !stack.is_empty()
                                && !["scenario.", "step.", "tools.", "vm."]
                                    .iter()
                                    .any(|prefix| path.starts_with(prefix))
                                && !matches!(
                                    path.as_str(),
                                    "binary"
                                        | "image"
                                        | "drive"
                                        | "path"
                                        | "from"
                                        | "to"
                                        | "content"
                                        | "extra"
                                        | "run_id"
                                        | "scenario_name"
                                        | "image_dir"
                                ) =>
                            {
                                // Consumer scripts can carry their own markers, e.g. {N}.
                                out.push_str(&template[i..i + end_rel + 2]);
                            }
                            None if optional || !checked => {}
                            None => return Err(format!("missing required template token: {path}")),
                        }
                        i += end_rel + 2;
                        continue;
                    }
                }
            }
            // Advancing by characters preserves literal UTF-8 in templates.
            let ch = template[i..].chars().next().expect("remaining character");
            out.push(ch);
            i += ch.len_utf8();
        }
        if out.len() > MAX_TEMPLATE_BYTES {
            return Err("expanded recipe template exceeds 1048576 bytes".into());
        }
        Ok(out)
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
        // happen to match (the flat vocabulary is small and stable).
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
    fn expands_recipe_label_reference() {
        let mut s = fixture();
        s.step = json!({"label": "{scenario.volume_params.label}"});
        assert_eq!(s.expand("-Label '{step.label}'"), "-Label 'TEST'");
    }

    #[test]
    fn checked_expansion_rejects_missing_required_but_allows_optional() {
        let s = fixture();
        assert!(s
            .expand_checked("{step.nope}")
            .unwrap_err()
            .contains("step.nope"));
        assert_eq!(s.expand_checked("a{step.nope?}b"), Ok("ab".into()));
    }

    #[test]
    fn checked_expansion_resolves_nested_optional_and_required_references() {
        let mut s = fixture();
        s.step = json!({"label": "{scenario.absent}", "optional": "{scenario.absent?}"});
        assert!(s
            .expand_checked("{step.label}")
            .unwrap_err()
            .contains("scenario.absent"));
        assert_eq!(s.expand_checked("{step.optional}"), Ok(String::new()));
    }

    #[test]
    fn checked_expansion_rejects_cycles() {
        let mut s = fixture();
        s.step = json!({"label": "{step.alias}", "alias": "{step.label}"});
        let error = s.expand_checked("{step.label}").unwrap_err();
        assert!(error.contains("step.label -> step.alias -> step.label"));
        assert_eq!(s.expand("{step.label}"), "{step.label}");
    }

    #[test]
    fn checked_expansion_bounds_reference_depth() {
        let mut s = fixture();
        let mut fields = serde_json::Map::new();
        for i in 0..32 {
            fields.insert(
                format!("field{i}"),
                json!(format!("{{step.field{}}}", i + 1)),
            );
        }
        fields.insert("field32".into(), json!("label"));
        s.step = Value::Object(fields);
        assert!(s
            .expand_checked("{step.field0}")
            .unwrap_err()
            .contains("depth exceeds 32"));
        assert_eq!(s.expand_checked("{step.field1}"), Ok("label".into()));
    }

    #[test]
    fn checked_expansion_preserves_terminal_data_and_literal_braces() {
        let mut s = fixture();
        s.scenario = json!({"label": "{step.label}"});
        s.flat.insert("tools.literal".into(), "{step.label}".into());
        s.step = json!({"label": "{{step.label}}", "object": {"label": "{step.label}"}});
        assert_eq!(
            s.expand_checked("{scenario.label} {tools.literal} {step.label}"),
            Ok("{step.label} {step.label} {step.label}".into())
        );
        assert_eq!(
            s.expand_checked("{step.object}"),
            Ok(r#"{"label":"{step.label}"}"#.into())
        );
        s.flat
            .insert("step.label".into(), "{scenario.label}".into());
        assert_eq!(
            s.expand_checked("{step.label}"),
            Ok("{scenario.label}".into())
        );
    }

    #[test]
    fn checked_expansion_preserves_unicode_and_invalid_braces() {
        let s = fixture();
        assert_eq!(
            s.expand_checked("éclipse {{binary}} {3invalid} {"),
            Ok("éclipse {binary} {3invalid} {".into())
        );
        assert_eq!(s.expand("éclipse"), "éclipse");
    }

    #[test]
    fn checked_expansion_bounds_input_and_output_size() {
        let mut s = fixture();
        assert!(s
            .expand_checked(&"x".repeat(MAX_TEMPLATE_BYTES + 1))
            .unwrap_err()
            .contains("exceeds"));
        s.flat
            .insert("large".into(), "x".repeat(MAX_TEMPLATE_BYTES));
        assert_eq!(
            s.expand_checked("{large}").unwrap().len(),
            MAX_TEMPLATE_BYTES
        );
        for template in ["{large}x", "{large}xx", "{large}{{binary}}"] {
            assert!(s.expand_checked(template).unwrap_err().contains("exceeds"));
        }
    }

    #[test]
    fn checked_expansion_preserves_consumer_batch_markers() {
        let mut s = fixture();
        s.step = json!({"path": "/file_{N}.txt", "tool": "{tools.absent}", "binary": "{binary}"});
        assert_eq!(s.expand_checked("{step.path}"), Ok("/file_{N}.txt".into()));
        assert!(s
            .expand_checked("{step.tool}")
            .unwrap_err()
            .contains("tools.absent"));
        s.flat.remove("binary");
        assert!(s
            .expand_checked("{step.binary}")
            .unwrap_err()
            .contains("binary"));
    }

    #[test]
    fn expands_flat_tokens() {
        let s = fixture();
        assert_eq!(s.expand("ls {image} {drive}"), "ls /srv/images/test.img Z:");
        assert_eq!(s.expand("{binary} format"), "/usr/local/bin/myfs format");
        assert_eq!(
            s.expand("{tools.fsck} {image}"),
            "fsck.myfs -fn /srv/images/test.img"
        );
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
        assert_eq!(
            s.expand("--label {step.params.label}"),
            "--label STEP-LABEL"
        );
        assert_eq!(
            s.expand("op={step.op} path={step.path}"),
            "op=format path=/hello.txt"
        );
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
        assert_eq!(
            s.expand("--journal {step.params.journal_mode?}"),
            "--journal "
        );
    }

    #[test]
    fn missing_required_path_collapses_to_empty() {
        // Documented contract: undeclared tokens collapse to empty.
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
