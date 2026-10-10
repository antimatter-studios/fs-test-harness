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
//!   yield empty strings instead of `<missing:...>` markers.
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

/// Why a template could not be expanded into a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubstitutionError {
    /// Required references that resolve to nothing, in template order.
    Unresolved(Vec<String>),
    /// A reference chain that leads back to itself, from first to repeat.
    Cycle(Vec<String>),
    /// A reference chain deeper than [`MAX_DEPTH`], from the outermost.
    TooDeep(Vec<String>),
}

/// How many references deep a value may point before expansion stops.
pub const MAX_DEPTH: usize = 8;

impl std::fmt::Display for SubstitutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubstitutionError::Unresolved(paths) => write!(
                f,
                "unresolved required reference(s): {}",
                paths
                    .iter()
                    .map(|p| format!("{{{p}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            SubstitutionError::Cycle(chain) => {
                write!(f, "reference cycle: {}", chain.join(" -> "))
            }
            SubstitutionError::TooDeep(chain) => write!(
                f,
                "references nested deeper than {MAX_DEPTH}: {}",
                chain.join(" -> ")
            ),
        }
    }
}

impl Substitution {
    /// Expand `template` completely, or say why it cannot be (#45). What
    /// the dispatcher runs: a command is built from this or not at all.
    ///
    /// * A `{scenario.*}` or `{step.*}` value that is itself a string is
    ///   expanded in turn, so a step can name a scenario value. A chain
    ///   that leads back to a reference already being expanded is a
    ///   [`SubstitutionError::Cycle`]; one deeper than [`MAX_DEPTH`] is
    ///   [`SubstitutionError::TooDeep`].
    /// * A required reference that resolves to nothing, at any depth, is
    ///   collected, and every one is reported together as
    ///   [`SubstitutionError::Unresolved`]. A `?` reference still yields
    ///   an empty string.
    /// * Literal data is left alone: a `{"literal": "..."}` value is used
    ///   verbatim, `{{` and `}}` are a literal brace, and flat tokens
    ///   (`{content}`, `{path}`, `{binary}`, ...) are never re-expanded.
    pub fn expand_checked(&self, template: &str) -> Result<String, SubstitutionError> {
        let mut missing = Vec::new();
        let out = self.expand_in(template, &mut Vec::new(), &mut missing)?;
        if missing.is_empty() {
            Ok(out)
        } else {
            Err(SubstitutionError::Unresolved(missing))
        }
    }

    /// One level of [`Self::expand_checked`]: `chain` is the references
    /// being expanded on the way here, outermost first.
    fn expand_in(
        &self,
        template: &str,
        chain: &mut Vec<String>,
        missing: &mut Vec<String>,
    ) -> Result<String, SubstitutionError> {
        let mut out = String::with_capacity(template.len());
        let bytes = template.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match (bytes[i], bytes.get(i + 1)) {
                (b'{', Some(b'{')) | (b'}', Some(b'}')) => {
                    out.push(bytes[i] as char);
                    i += 2;
                    continue;
                }
                (b'{', _) => {
                    if let Some(end_rel) = bytes[i + 1..].iter().position(|&b| b == b'}') {
                        let inner = &bytes[i + 1..i + 1 + end_rel];
                        if let Some((path, optional)) = parse_placeholder(inner) {
                            match self.resolve(&path, chain, missing)? {
                                Some(s) => out.push_str(&s),
                                None if optional => {}
                                None => {
                                    if !missing.contains(&path) {
                                        missing.push(path);
                                    }
                                }
                            }
                            i += 1 + end_rel + 1;
                            continue;
                        }
                    }
                }
                _ => {}
            }
            let ch = template[i..]
                .chars()
                .next()
                .expect("i is on a char boundary");
            out.push(ch);
            i += ch.len_utf8();
        }
        Ok(out)
    }

    /// A reference's text, expanded when it is a scenario or step string;
    /// `None` when it resolves to nothing.
    fn resolve(
        &self,
        path: &str,
        chain: &mut Vec<String>,
        missing: &mut Vec<String>,
    ) -> Result<Option<String>, SubstitutionError> {
        if let Some(s) = self.flat.get(path) {
            return Ok(Some(s.clone()));
        }
        let Some(v) = self.lookup_value(path) else {
            return Ok(None);
        };
        if let Some(literal) = v
            .as_object()
            .filter(|m| m.len() == 1)
            .and_then(|m| m.get("literal"))
            .and_then(Value::as_str)
        {
            return Ok(Some(literal.to_string()));
        }
        let Value::String(s) = v else {
            return Ok(Some(value_to_string(&v)));
        };
        let mut at = chain.clone();
        at.push(path.to_string());
        if chain.iter().any(|p| p == path) {
            return Err(SubstitutionError::Cycle(at));
        }
        if chain.len() >= MAX_DEPTH {
            return Err(SubstitutionError::TooDeep(at));
        }
        chain.push(path.to_string());
        let expanded = self.expand_in(&s, chain, missing);
        chain.pop();
        expanded.map(Some)
    }

    /// Substitute every `{...}` placeholder in `template`, in one pass and
    /// with a missing required reference silently empty. The dispatcher
    /// uses [`Self::expand_checked`]; this stays for its plain contract.
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
    ///      embedded for human debugging (matches the documented
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
                                // for now. Future iteration could emit a
                                // `<missing:path>` marker; left as a
                                // follow-up to avoid breaking existing
                                // consumers that rely on the silent-empty
                                // contract.
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
    /// The QEMU validation's shape (#45): a step value that is itself a
    /// reference. One pass handed Windows `{scenario.volume_params.label}`.
    #[test]
    fn a_reference_inside_a_resolved_value_is_expanded() {
        let mut s = fixture();
        s.step = json!({ "params": { "label": "{scenario.volume_params.label}" } });
        assert_eq!(
            s.expand_checked("-Label '{step.params.label}'"),
            Ok("-Label 'TEST'".to_string())
        );
        // Two levels: a step value naming a scenario value naming another.
        s.scenario["alias"] = json!("{scenario.volume_params.label}");
        s.step = json!({ "label": "{scenario.alias}" });
        assert_eq!(s.expand_checked("{step.label}"), Ok("TEST".to_string()));
    }

    #[test]
    fn a_reference_cycle_is_reported_not_followed() {
        let mut s = fixture();
        s.scenario = json!({ "a": "{scenario.b}", "b": "{scenario.a}" });
        match s.expand_checked("x {scenario.a}") {
            Err(SubstitutionError::Cycle(chain)) => {
                assert_eq!(chain, ["scenario.a", "scenario.b", "scenario.a"])
            }
            other => panic!("expected a cycle, got {other:?}"),
        }
    }

    #[test]
    fn a_chain_deeper_than_the_limit_is_reported() {
        let mut s = fixture();
        let mut m = serde_json::Map::new();
        for i in 0..=MAX_DEPTH {
            m.insert(format!("v{i}"), json!(format!("{{scenario.v{}}}", i + 1)));
        }
        m.insert(format!("v{}", MAX_DEPTH + 1), json!("end"));
        s.scenario = serde_json::Value::Object(m);
        assert!(matches!(
            s.expand_checked("{scenario.v0}"),
            Err(SubstitutionError::TooDeep(_))
        ));
    }

    /// A required reference that resolves to nothing is an error before
    /// any command runs, naming every one; an optional one is still empty.
    #[test]
    fn unresolved_required_references_are_reported() {
        let s = fixture();
        assert_eq!(
            s.expand_checked("a {step.params.nope} b {scenario.gone} c {step.params.maybe?}"),
            Err(SubstitutionError::Unresolved(vec![
                "step.params.nope".to_string(),
                "scenario.gone".to_string()
            ]))
        );
        assert_eq!(
            s.expand_checked("--journal {step.params.journal_mode?}"),
            Ok("--journal ".to_string())
        );
        // A missing reference reached through another is reported too.
        let mut s = fixture();
        s.step = json!({ "label": "{scenario.no_such}" });
        assert_eq!(
            s.expand_checked("{step.label}"),
            Err(SubstitutionError::Unresolved(vec![
                "scenario.no_such".to_string()
            ]))
        );
    }

    /// Data that is meant literally stays literal: a `{"literal": ...}`
    /// value, doubled braces, and flat tokens (file content, paths).
    #[test]
    fn explicitly_literal_data_is_not_expanded() {
        let mut s = fixture();
        s.step = json!({ "content": { "literal": "{scenario.image} stays" } });
        assert_eq!(
            s.expand_checked("{step.content}"),
            Ok("{scenario.image} stays".to_string())
        );
        assert_eq!(
            s.expand_checked("{{step.content}} and {{"),
            Ok("{step.content} and {".to_string())
        );
        s.flat
            .insert("content".to_string(), "{scenario.image}".to_string());
        assert_eq!(
            s.expand_checked("{content}"),
            Ok("{scenario.image}".to_string())
        );
        // Every existing one-level template still expands as before.
        let s = fixture();
        assert_eq!(
            s.expand_checked("{binary} {image} --label {step.params.label}"),
            Ok("/usr/local/bin/myfs /srv/images/test.img --label STEP-LABEL".to_string())
        );
    }
}
