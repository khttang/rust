use rig_core::tool::{PortableTool, ToolExecutionError};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Basic arithmetic on two numbers.
#[derive(Debug, Clone, Copy, Default)]
pub struct Calculator;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CalculatorArgs {
    pub op: Operation,
    pub a: f64,
    pub b: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CalculatorOutput {
    pub result: f64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CalculatorError {
    #[error("division by zero")]
    DivisionByZero,
    #[error("result is not a finite number")]
    NonFinite,
}

impl PortableTool for Calculator {
    const NAME: &'static str = "calculator";
    type Args = CalculatorArgs;
    type Output = CalculatorOutput;
    type Error = CalculatorError;

    fn description(&self) -> String {
        "Perform one arithmetic operation (add, sub, mul, div) on two numbers.".to_owned()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["add", "sub", "mul", "div"] },
                "a":  { "type": "number", "description": "Left operand" },
                "b":  { "type": "number", "description": "Right operand" }
            },
            "required": ["op", "a", "b"]
        })
    }

    // These errors contain no sensitive data, so surface them verbatim to the model.
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        ToolExecutionError::invalid_args(error.to_string())
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let result = match args.op {
            Operation::Add => args.a + args.b,
            Operation::Sub => args.a - args.b,
            Operation::Mul => args.a * args.b,
            Operation::Div if args.b == 0.0 => return Err(CalculatorError::DivisionByZero),
            Operation::Div => args.a / args.b,
        };
        if !result.is_finite() {
            return Err(CalculatorError::NonFinite);
        }
        Ok(CalculatorOutput { result })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::DynTool;
    use rig_core::tool::ToolErrorKind;

    async fn calc(op: Operation, a: f64, b: f64) -> Result<f64, CalculatorError> {
        PortableTool::call(&Calculator, CalculatorArgs { op, a, b })
            .await
            .map(|o| o.result)
    }

    #[tokio::test]
    async fn arithmetic() {
        assert_eq!(calc(Operation::Add, 2.0, 3.0).await, Ok(5.0));
        assert_eq!(calc(Operation::Sub, 2.0, 3.0).await, Ok(-1.0));
        assert_eq!(calc(Operation::Mul, 2.0, 3.0).await, Ok(6.0));
        assert_eq!(calc(Operation::Div, 3.0, 2.0).await, Ok(1.5));
    }

    #[tokio::test]
    async fn division_by_zero() {
        assert_eq!(
            calc(Operation::Div, 1.0, 0.0).await,
            Err(CalculatorError::DivisionByZero)
        );
    }

    #[tokio::test]
    async fn overflow_is_non_finite() {
        assert_eq!(
            calc(Operation::Mul, f64::MAX, 2.0).await,
            Err(CalculatorError::NonFinite)
        );
    }

    #[tokio::test]
    async fn errors_are_visible_to_model() {
        let err = DynTool::call(&Calculator, json!({"op": "div", "a": 1, "b": 0}))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ToolErrorKind::InvalidArgs);
        assert_eq!(err.model_output().as_text(), Some("division by zero"));
    }

    #[test]
    fn definition() {
        let def = DynTool::definition(&Calculator);
        assert_eq!(def.name, "calculator");
        assert_eq!(def.parameters["required"], json!(["op", "a", "b"]));
    }
}
