//! Provider-agnostic data schemas.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A unit of work for a [`crate::ModelRuntime`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTask {
    /// System instructions.
    #[serde(default)]
    pub preamble: String,
    /// The user-facing request.
    pub payload: String,
}

impl AgentTask {
    pub fn new(preamble: impl Into<String>, payload: impl Into<String>) -> Self {
        Self {
            preamble: preamble.into(),
            payload: payload.into(),
        }
    }
}

/// One flexible key-value fact. The value is any JSON so manifests can carry
/// strings, numbers, lists or nested objects without a fixed schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub key: String,
    pub value: Value,
}

impl ManifestEntry {
    pub fn new(key: impl Into<String>, value: impl Into<Value>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }

    /// The value as prompt text: strings verbatim (no quotes), anything else
    /// as compact JSON.
    pub fn value_text(&self) -> Cow<'_, str> {
        match &self.value {
            Value::String(s) => Cow::Borrowed(s),
            other => Cow::Owned(other.to_string()),
        }
    }
}

/// An ordered list of [`ManifestEntry`] values.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, entry: ManifestEntry) {
        self.entries.push(entry);
    }

    /// The last entry with `key`, so later entries override earlier ones.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.key == key)
            .map(|e| &e.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schemas_are_send_sync_static() {
        fn assert_bounds<T: Send + Sync + 'static>() {}
        assert_bounds::<AgentTask>();
        assert_bounds::<ManifestEntry>();
        assert_bounds::<Manifest>();
    }

    #[test]
    fn value_text_unquotes_strings_only() {
        assert_eq!(ManifestEntry::new("k", "zsh").value_text(), "zsh");
        assert_eq!(ManifestEntry::new("k", 8).value_text(), "8");
        assert_eq!(
            ManifestEntry::new("k", json!({"a": [1, 2]})).value_text(),
            r#"{"a":[1,2]}"#
        );
    }

    #[test]
    fn manifest_round_trips_through_json() {
        let raw = json!({"entries": [
            {"key": "os", "value": "darwin"},
            {"key": "limits", "value": {"ctx": 8192}},
        ]});
        let manifest: Manifest = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(manifest.get("limits"), Some(&json!({"ctx": 8192})));
        assert_eq!(serde_json::to_value(&manifest).unwrap(), raw);
    }

    #[test]
    fn later_entries_override() {
        let mut m = Manifest::new();
        m.push(ManifestEntry::new("os", "linux"));
        m.push(ManifestEntry::new("os", "darwin"));
        assert_eq!(m.get("os"), Some(&json!("darwin")));
        assert_eq!(m.get("missing"), None);
    }

    #[test]
    fn task_preamble_defaults_to_empty() {
        let task: AgentTask = serde_json::from_value(json!({"payload": "hi"})).unwrap();
        assert_eq!(task, AgentTask::new("", "hi"));
    }
}
