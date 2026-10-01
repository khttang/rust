//! Parsing CBMC's `--json-ui` output.
//!
//! CBMC prints a JSON array of messages. The parts used here:
//!
//! * `{"program": "CBMC 6.6.0 (cbmc-6.6.0)"}`: the version.
//! * `{"messageType": "ERROR", "messageText": ..., "sourceLocation": ...}`:
//!   parse and type errors.
//! * `{"result": [{"property", "description", "status", "sourceLocation",
//!   "trace"?}]}`: one entry per property; `trace` (with `--trace`) on failures.
//! * `{"cProverStatus": "success" | "failure"}`: the overall verdict.
//!
//! Format checked against CBMC 6.6.0 (fixtures in `tests/fixtures`).

use serde::Serialize;
use serde_json::Value;

/// Counterexample steps kept for the model; earlier ones are summarized.
pub const MAX_TRACE_STEPS: usize = 30;

/// Overall result of a CBMC run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Outcome {
    /// Every property holds up to the loop bound.
    Verified,
    /// At least one property fails; see the counterexamples.
    Failed,
    /// CBMC could not analyse the program (e.g. a parse error).
    Error,
    /// CBMC did not finish within the time limit.
    TimedOut,
}

/// CBMC's verdict on one property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PropertyStatus {
    Success,
    Failure,
    /// Any other status CBMC reports (e.g. `UNKNOWN`, `NOT CHECKED`).
    Other,
}

/// What a property checks, from its CBMC id (`<function>.<kind>.<n>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PropertyKind {
    /// A user `assert` / `__CPROVER_assert`.
    Assertion,
    /// Arithmetic overflow.
    Overflow,
    /// Array bounds.
    Bounds,
    /// Pointer validity.
    Pointer,
    /// Division by zero.
    DivisionByZero,
    /// Undefined shift.
    Shift,
    /// The loop bound (`--unwind`) was too small to explore every iteration.
    /// Not a bug in the code: raise the bound.
    Unwinding,
    Other,
}

impl PropertyKind {
    fn from_id(id: &str) -> Self {
        let mut parts = id.split('.');
        let _function = parts.next();
        match parts.next().unwrap_or_default() {
            // Names as CBMC 6.6.0 emits them (see the `kinds` fixture).
            "assertion" => Self::Assertion,
            "overflow" => Self::Overflow,
            "array_bounds" => Self::Bounds,
            "pointer_dereference" => Self::Pointer,
            "division-by-zero" => Self::DivisionByZero,
            "undefined-shift" => Self::Shift,
            "unwind" => Self::Unwinding,
            _ => Self::Other,
        }
    }
}

/// A source position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Location {
    pub file: String,
    pub function: Option<String>,
    pub line: Option<u32>,
}

impl Location {
    fn from_json(value: &Value) -> Option<Self> {
        let file = value.get("file")?.as_str()?.to_owned();
        Some(Self {
            file,
            function: value
                .get("function")
                .and_then(Value::as_str)
                .map(str::to_owned),
            line: value
                .get("line")
                .and_then(Value::as_str)
                .and_then(|l| l.parse().ok()),
        })
    }

    /// CBMC's own library and built-ins live in files named `<...>`.
    fn is_internal(&self) -> bool {
        self.file.starts_with('<')
    }
}

/// One step of a counterexample, as shown to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum TraceStep {
    /// A nondeterministic input chosen by CBMC (e.g. a function argument).
    Input {
        name: String,
        value: String,
        line: Option<u32>,
    },
    /// A variable assignment.
    Assign {
        name: String,
        value: String,
        line: Option<u32>,
    },
    /// Entering a function.
    Call { function: String, line: Option<u32> },
    /// The violated property.
    Failure { reason: String, line: Option<u32> },
}

/// A shortened counterexample: the last [`MAX_TRACE_STEPS`] visible steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Counterexample {
    pub steps: Vec<TraceStep>,
    /// Visible steps dropped from the start to keep the trace short.
    pub omitted: usize,
}

/// One property and its verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Property {
    pub id: String,
    pub description: String,
    pub kind: PropertyKind,
    pub status: PropertyStatus,
    pub location: Option<Location>,
    pub counterexample: Option<Counterexample>,
}

/// An error message from CBMC (e.g. a syntax error).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    pub text: String,
    pub location: Option<Location>,
}

/// A parsed CBMC run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CbmcReport {
    pub outcome: Outcome,
    /// e.g. `CBMC 6.6.0 (cbmc-6.6.0)`.
    pub version: Option<String>,
    pub properties: Vec<Property>,
    pub errors: Vec<Message>,
}

impl CbmcReport {
    /// A run that produced no output because it was stopped.
    pub fn timed_out() -> Self {
        Self {
            outcome: Outcome::TimedOut,
            version: None,
            properties: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub fn failures(&self) -> impl Iterator<Item = &Property> {
        self.properties
            .iter()
            .filter(|p| p.status == PropertyStatus::Failure)
    }

    /// Whether any failure is only the loop bound being too small.
    pub fn bound_too_small(&self) -> bool {
        self.failures().any(|p| p.kind == PropertyKind::Unwinding)
    }
}

/// Why CBMC output could not be understood.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("CBMC output is not JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("CBMC output is not a JSON array of messages")]
    NotAnArray,

    #[error("CBMC output has neither a verdict nor an error message")]
    NoVerdict,
}

/// Parse CBMC `--json-ui` output.
pub fn parse(json: &str) -> Result<CbmcReport, ParseError> {
    let messages: Value = serde_json::from_str(json)?;
    let messages = messages.as_array().ok_or(ParseError::NotAnArray)?;

    let mut version = None;
    let mut properties = Vec::new();
    let mut errors = Vec::new();
    let mut status = None;

    for message in messages {
        if let Some(program) = message.get("program").and_then(Value::as_str) {
            version = Some(program.to_owned());
        }
        if message.get("messageType").and_then(Value::as_str) == Some("ERROR") {
            errors.push(Message {
                text: message
                    .get("messageText")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                location: message.get("sourceLocation").and_then(Location::from_json),
            });
        }
        if let Some(results) = message.get("result").and_then(Value::as_array) {
            properties.extend(results.iter().map(property));
        }
        if let Some(s) = message.get("cProverStatus").and_then(Value::as_str) {
            status = Some(s.to_owned());
        }
    }

    let outcome = match status.as_deref() {
        Some("success") => Outcome::Verified,
        Some("failure") => Outcome::Failed,
        Some(_) => Outcome::Error,
        None if !errors.is_empty() => Outcome::Error,
        None => return Err(ParseError::NoVerdict),
    };
    Ok(CbmcReport {
        outcome,
        version,
        properties,
        errors,
    })
}

fn property(value: &Value) -> Property {
    let id = value
        .get("property")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let status = match value.get("status").and_then(Value::as_str) {
        Some("SUCCESS") => PropertyStatus::Success,
        Some("FAILURE") => PropertyStatus::Failure,
        _ => PropertyStatus::Other,
    };
    Property {
        kind: PropertyKind::from_id(&id),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        location: value.get("sourceLocation").and_then(Location::from_json),
        counterexample: value
            .get("trace")
            .and_then(Value::as_array)
            .map(|steps| counterexample(steps)),
        status,
        id,
    }
}

fn counterexample(steps: &[Value]) -> Counterexample {
    let visible: Vec<TraceStep> = steps.iter().filter_map(trace_step).collect();
    let omitted = visible.len().saturating_sub(MAX_TRACE_STEPS);
    Counterexample {
        steps: visible.into_iter().skip(omitted).collect(),
        omitted,
    }
}

/// Keep only steps in user code that carry information.
fn trace_step(step: &Value) -> Option<TraceStep> {
    if step.get("hidden").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let location = step.get("sourceLocation").and_then(Location::from_json);
    if location.as_ref().is_some_and(Location::is_internal) {
        return None;
    }
    let line = location.as_ref().and_then(|l| l.line);
    let text = |v: &Value| v.get("data").and_then(Value::as_str).map(str::to_owned);
    match step.get("stepType").and_then(Value::as_str)? {
        "input" => Some(TraceStep::Input {
            name: step.get("inputID")?.as_str()?.to_owned(),
            value: step
                .get("values")
                .and_then(Value::as_array)
                .and_then(|v| v.first())
                .and_then(text)
                .unwrap_or_default(),
            line,
        }),
        "assignment" => Some(TraceStep::Assign {
            name: step.get("lhs")?.as_str()?.to_owned(),
            value: step.get("value").and_then(text).unwrap_or_default(),
            line,
        }),
        "function-call" => Some(TraceStep::Call {
            function: step
                .get("function")?
                .get("displayName")?
                .as_str()?
                .to_owned(),
            line,
        }),
        "failure" => Some(TraceStep::Failure {
            reason: step
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            line,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/cbmc-6.6.0-{name}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn verified_run() {
        let report = parse(&fixture("ok")).unwrap();
        assert_eq!(report.outcome, Outcome::Verified);
        assert_eq!(report.version.as_deref(), Some("CBMC 6.6.0 (cbmc-6.6.0)"));
        assert_eq!(report.properties.len(), 1);
        let p = &report.properties[0];
        assert_eq!(p.id, "clamp.assertion.1");
        assert_eq!(p.kind, PropertyKind::Assertion);
        assert_eq!(p.status, PropertyStatus::Success);
        assert_eq!(p.location.as_ref().unwrap().line, Some(5));
        assert!(p.counterexample.is_none());
        assert_eq!(report.failures().count(), 0);
    }

    #[test]
    fn overflow_with_counterexample() {
        let report = parse(&fixture("overflow")).unwrap();
        assert_eq!(report.outcome, Outcome::Failed);
        let failure = report.failures().next().unwrap();
        assert_eq!(failure.kind, PropertyKind::Overflow);
        assert_eq!(
            failure.description,
            "arithmetic overflow on signed * in error * gain"
        );

        let cex = failure.counterexample.as_ref().unwrap();
        assert_eq!(cex.omitted, 0);
        // Inputs and parameter assignments, then the failure; nothing internal.
        assert!(cex.steps.contains(&TraceStep::Input {
            name: "error".into(),
            value: "-1073741826".into(),
            line: Some(1),
        }));
        assert!(cex.steps.contains(&TraceStep::Assign {
            name: "gain".into(),
            value: "-2".into(),
            line: Some(1),
        }));
        assert_eq!(
            cex.steps.last(),
            Some(&TraceStep::Failure {
                reason: "arithmetic overflow on signed * in error * gain".into(),
                line: Some(2),
            })
        );
        assert!(
            !serde_json::to_string(cex).unwrap().contains("__CPROVER"),
            "internal steps are dropped"
        );
    }

    #[test]
    fn loop_bound_too_small_is_its_own_kind() {
        let report = parse(&fixture("loop")).unwrap();
        assert_eq!(report.outcome, Outcome::Failed);
        assert!(report.bound_too_small());
        let failures: Vec<_> = report.failures().collect();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].id, "sum.unwind.0");
        assert_eq!(failures[0].kind, PropertyKind::Unwinding);
        // The overflow checks on the same line passed.
        assert_eq!(report.properties.len(), 3);
    }

    #[test]
    fn parse_error_is_an_error_outcome() {
        let report = parse(&fixture("bad")).unwrap();
        assert_eq!(report.outcome, Outcome::Error);
        assert!(report.properties.is_empty());
        assert_eq!(report.errors.len(), 2);
        assert_eq!(report.errors[0].text, "syntax error before '{'");
        assert_eq!(report.errors[0].location.as_ref().unwrap().line, Some(1));
    }

    #[test]
    fn long_traces_keep_the_last_steps() {
        let step = |i: usize| {
            serde_json::json!({
                "stepType": "assignment", "hidden": false, "lhs": format!("x{i}"),
                "value": {"data": i.to_string()},
                "sourceLocation": {"file": "a.c", "line": i.to_string()}
            })
        };
        let steps: Vec<Value> = (0..MAX_TRACE_STEPS + 5).map(step).collect();
        let cex = counterexample(&steps);
        assert_eq!(cex.omitted, 5);
        assert_eq!(cex.steps.len(), MAX_TRACE_STEPS);
        assert!(matches!(&cex.steps[0], TraceStep::Assign { name, .. } if name == "x5"));
    }

    #[test]
    fn rejects_output_that_is_not_cbmc() {
        assert!(matches!(parse("not json"), Err(ParseError::Json(_))));
        assert!(matches!(parse("{}"), Err(ParseError::NotAnArray)));
        assert!(matches!(
            parse("[{\"program\": \"CBMC\"}]"),
            Err(ParseError::NoVerdict)
        ));
    }

    #[test]
    fn property_kinds_from_real_ids() {
        let report = parse(&fixture("kinds")).unwrap();
        let kind_of = |prefix: &str| {
            report
                .properties
                .iter()
                .find(|p| p.id.starts_with(prefix))
                .unwrap_or_else(|| panic!("no property {prefix}"))
                .kind
        };
        assert_eq!(kind_of("kinds.array_bounds."), PropertyKind::Bounds);
        assert_eq!(
            kind_of("kinds.division-by-zero."),
            PropertyKind::DivisionByZero
        );
        assert_eq!(kind_of("kinds.undefined-shift."), PropertyKind::Shift);
        assert_eq!(kind_of("kinds.pointer_dereference."), PropertyKind::Pointer);
        assert_eq!(kind_of("kinds.overflow."), PropertyKind::Overflow);
        assert!(
            report
                .properties
                .iter()
                .all(|p| p.kind != PropertyKind::Other)
        );
        // CBMC also reports UNKNOWN for some pointer checks.
        assert!(
            report
                .properties
                .iter()
                .any(|p| p.status == PropertyStatus::Other)
        );
        assert_eq!(PropertyKind::from_id("f.something.1"), PropertyKind::Other);
        assert_eq!(
            PropertyKind::from_id("f.assertion.1"),
            PropertyKind::Assertion
        );
    }
}
