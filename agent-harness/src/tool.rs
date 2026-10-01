//! Tool type-erasure and registry.
//!
//! Tools are authored against rig's context-free [`PortableTool`] trait. The
//! registry needs to hold a heterogeneous set of them, so it stores each one
//! behind the object-safe [`DynTool`] trait. That costs one `Box` per tool at
//! registration and one boxed future per tool *call*; nothing else in the
//! harness is dynamically dispatched.

use std::{future::Future, pin::Pin};

use rig_core::{
    completion::ToolDefinition,
    tool::{IntoToolOutput, PortableTool, ToolExecutionError, ToolOutput, tool_definition},
};
use serde_json::Value;

use crate::{error::HarnessError, policy::ToolRisk, validate::validate_args};

/// Boxed future returned by [`DynTool::call`].
pub type ToolFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ToolOutput, ToolExecutionError>> + Send + 'a>>;

/// Object-safe view of a tool, implemented for every [`PortableTool`].
pub trait DynTool: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn definition(&self) -> ToolDefinition;
    fn call(&self, args: Value) -> ToolFuture<'_>;
}

impl<T> DynTool for T
where
    T: PortableTool + 'static,
    T::Args: Send,
    T::Output: Send,
{
    fn name(&self) -> &str {
        T::NAME
    }

    fn definition(&self) -> ToolDefinition {
        tool_definition(self)
    }

    fn call(&self, args: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            let args: T::Args = serde_json::from_value(args)
                .map_err(|e| ToolExecutionError::invalid_args(e.to_string()))?;
            let output = PortableTool::call(self, args)
                .await
                .map_err(|e| self.map_error(e))?;
            output.into_tool_output()
        })
    }
}

/// An ordered set of uniquely named tools, each with a declared [`ToolRisk`].
///
/// Registration order is preserved so the tool list sent to the model — and
/// therefore every request — is deterministic.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<(Box<dyn DynTool>, ToolRisk)>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool as [`ToolRisk::Mutating`], the safe default. Fails if
    /// a tool with the same name already exists.
    pub fn register<T: DynTool>(&mut self, tool: T) -> Result<(), HarnessError> {
        self.register_with_risk(tool, ToolRisk::Mutating)
    }

    /// Register a tool that only reads ([`ToolRisk::ReadOnly`]).
    pub fn register_read_only<T: DynTool>(&mut self, tool: T) -> Result<(), HarnessError> {
        self.register_with_risk(tool, ToolRisk::ReadOnly)
    }

    /// Register a tool with an explicit risk.
    pub fn register_with_risk<T: DynTool>(
        &mut self,
        tool: T,
        risk: ToolRisk,
    ) -> Result<(), HarnessError> {
        if self.get(tool.name()).is_some() {
            return Err(HarnessError::DuplicateTool(tool.name().to_owned()));
        }
        self.tools.push((Box::new(tool), risk));
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&dyn DynTool> {
        self.tools
            .iter()
            .find(|(t, _)| t.name() == name)
            .map(|(t, _)| &**t)
    }

    /// The declared risk of `name`; [`ToolRisk::Mutating`] for unknown tools.
    pub fn risk(&self, name: &str) -> ToolRisk {
        self.tools
            .iter()
            .find(|(t, _)| t.name() == name)
            .map_or(ToolRisk::Mutating, |(_, risk)| *risk)
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().map(|(t, _)| t.name())
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|(t, _)| t.definition()).collect()
    }

    /// Look up, validate and execute a tool call.
    ///
    /// Unknown tools and schema violations are reported as
    /// [`ToolExecutionError`]s rather than panics so the model can correct itself.
    pub async fn execute(&self, name: &str, args: Value) -> Result<ToolOutput, ToolExecutionError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolExecutionError::not_found(format!("unknown tool `{name}`")))?;
        validate_args(&tool.definition().parameters, &args)?;
        tool.call(args).await
    }
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.names()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Calculator, WordCount};
    use rig_core::tool::ToolErrorKind;
    use serde_json::json;

    fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.register(Calculator).unwrap();
        r.register(WordCount).unwrap();
        r
    }

    #[test]
    fn risk_defaults_to_mutating_and_is_declarable() {
        let mut r = ToolRegistry::new();
        r.register(Calculator).unwrap();
        r.register_read_only(WordCount).unwrap();
        assert_eq!(r.risk("calculator"), ToolRisk::Mutating);
        assert_eq!(r.risk("word_count"), ToolRisk::ReadOnly);
        assert_eq!(r.risk("unknown"), ToolRisk::Mutating);
        assert!(r.register_read_only(Calculator).is_err());
    }

    #[test]
    fn registry_is_send_sync_static() {
        fn assert_bounds<T: Send + Sync + 'static>() {}
        assert_bounds::<ToolRegistry>();
        assert_bounds::<Box<dyn DynTool>>();
    }

    #[test]
    fn preserves_registration_order() {
        let r = registry();
        assert_eq!(r.names().collect::<Vec<_>>(), ["calculator", "word_count"]);
        let defs = r.definitions();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "calculator");
    }

    #[test]
    fn rejects_duplicate_registration() {
        let mut r = registry();
        let err = r.register(Calculator).unwrap_err();
        assert!(matches!(err, HarnessError::DuplicateTool(name) if name == "calculator"));
        assert_eq!(r.len(), 2);
    }

    #[tokio::test]
    async fn executes_registered_tool() {
        let out = registry()
            .execute("calculator", json!({"op": "mul", "a": 6, "b": 7}))
            .await
            .unwrap();
        assert_eq!(out.as_json(), Some(&json!({"result": 42.0})));
    }

    #[tokio::test]
    async fn unknown_tool_is_not_found() {
        let err = registry().execute("nope", json!({})).await.unwrap_err();
        assert_eq!(err.kind(), ToolErrorKind::NotFound);
    }

    #[tokio::test]
    async fn schema_violation_is_invalid_args() {
        let err = registry()
            .execute("calculator", json!({"op": "add", "a": "1", "b": 2}))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ToolErrorKind::InvalidArgs);
    }

    #[tokio::test]
    async fn deserialize_failure_is_invalid_args() {
        // Passes the schema subset check but not serde (unknown enum variant).
        let err = registry()
            .execute("calculator", json!({"op": "pow", "a": 1, "b": 2}))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ToolErrorKind::InvalidArgs);
    }
}
