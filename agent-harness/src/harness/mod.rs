//! Model runtime and adaptive memory.

pub mod memory;
pub mod runtime;

pub use memory::{AdaptiveMemoryLayer, CompactionPolicy, CompactionReport};
pub use runtime::{HostedProviderRuntime, ModelRuntime};
