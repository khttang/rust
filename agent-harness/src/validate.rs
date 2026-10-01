//! Deterministic argument validation run before any tool executes.
//!
//! This checks the subset of JSON Schema that tool definitions typically use:
//! the top-level `type: object`, `required` keys, and primitive `type`s of
//! declared properties. It is intentionally strict and side-effect free so a
//! malformed call is rejected with a precise message the model can act on.

use rig_core::tool::ToolExecutionError;
use serde_json::Value;

/// Validate `args` against a (subset of a) JSON Schema.
pub fn validate_args(schema: &Value, args: &Value) -> Result<(), ToolExecutionError> {
    if schema.get("type").and_then(Value::as_str) == Some("object") && !args.is_object() {
        return Err(ToolExecutionError::invalid_args(format!(
            "expected a JSON object of arguments, got {}",
            type_name(args)
        )));
    }

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            if args.get(key).is_none() {
                return Err(ToolExecutionError::invalid_args(format!(
                    "missing required argument `{key}`"
                )));
            }
        }
    }

    if let (Some(properties), Some(args)) = (
        schema.get("properties").and_then(Value::as_object),
        args.as_object(),
    ) {
        for (key, value) in args {
            let Some(expected) = properties
                .get(key)
                .and_then(|prop| prop.get("type"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if !matches_type(expected, value) {
                return Err(ToolExecutionError::invalid_args(format!(
                    "argument `{key}` must be of type {expected}, got {}",
                    type_name(value)
                )));
            }
        }
    }

    Ok(())
}

fn matches_type(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        // Unknown or composite types are not checked here.
        _ => true,
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::tool::ToolErrorKind;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "a": { "type": "number" },
                "op": { "type": "string" },
                "n": { "type": "integer" }
            },
            "required": ["a", "op"]
        })
    }

    #[test]
    fn accepts_valid_args() {
        assert!(validate_args(&schema(), &json!({"a": 1.5, "op": "add"})).is_ok());
        assert!(validate_args(&schema(), &json!({"a": 1, "op": "add", "n": 3})).is_ok());
    }

    #[test]
    fn rejects_non_object() {
        let err = validate_args(&schema(), &json!([1, 2])).unwrap_err();
        assert_eq!(err.kind(), ToolErrorKind::InvalidArgs);
        assert!(err.message().contains("got array"));
    }

    #[test]
    fn rejects_missing_required() {
        let err = validate_args(&schema(), &json!({"a": 1})).unwrap_err();
        assert!(err.message().contains("`op`"));
    }

    #[test]
    fn rejects_wrong_type() {
        let err = validate_args(&schema(), &json!({"a": "one", "op": "add"})).unwrap_err();
        assert!(err.message().contains("`a` must be of type number"));
        let err = validate_args(&schema(), &json!({"a": 1, "op": "add", "n": 1.5})).unwrap_err();
        assert!(err.message().contains("`n` must be of type integer"));
    }

    #[test]
    fn ignores_undeclared_properties() {
        assert!(validate_args(&schema(), &json!({"a": 1, "op": "x", "extra": true})).is_ok());
    }

    #[test]
    fn empty_schema_accepts_anything() {
        assert!(validate_args(&json!({}), &json!("anything")).is_ok());
    }
}
