//! Example tools. Each is a plain [`rig_core::tool::PortableTool`] and can be
//! registered with [`crate::ToolRegistry::register`].

mod calculator;
mod word_count;

pub use calculator::{Calculator, CalculatorArgs, CalculatorError, Operation};
pub use word_count::{WordCount, WordCountArgs, WordCountOutput};
