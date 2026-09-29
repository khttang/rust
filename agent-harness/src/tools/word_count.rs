use std::convert::Infallible;

use rig_core::tool::PortableTool;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Counts words, characters and lines in a piece of text.
#[derive(Debug, Clone, Copy, Default)]
pub struct WordCount;

#[derive(Debug, Clone, Deserialize)]
pub struct WordCountArgs {
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WordCountOutput {
    pub words: usize,
    pub chars: usize,
    pub lines: usize,
}

impl PortableTool for WordCount {
    const NAME: &'static str = "word_count";
    type Args = WordCountArgs;
    type Output = WordCountOutput;
    type Error = Infallible;

    fn description(&self) -> String {
        "Count the words, characters and lines in the given text.".to_owned()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "Text to analyse" }
            },
            "required": ["text"]
        })
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let text = args.text.as_str();
        Ok(WordCountOutput {
            words: text.split_whitespace().count(),
            chars: text.chars().count(),
            lines: text.lines().count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::DynTool;

    #[tokio::test]
    async fn counts() {
        let Ok(out) = PortableTool::call(
            &WordCount,
            WordCountArgs {
                text: "héllo world\nsecond line".into(),
            },
        )
        .await;
        assert_eq!(
            out,
            WordCountOutput {
                words: 4,
                chars: 23,
                lines: 2
            }
        );
    }

    #[tokio::test]
    async fn empty_text() {
        let Ok(out) = PortableTool::call(
            &WordCount,
            WordCountArgs {
                text: String::new(),
            },
        )
        .await;
        assert_eq!(
            out,
            WordCountOutput {
                words: 0,
                chars: 0,
                lines: 0
            }
        );
    }

    #[tokio::test]
    async fn via_dyn_tool_returns_json() {
        let out = DynTool::call(&WordCount, json!({"text": "a b c"}))
            .await
            .unwrap();
        assert_eq!(
            out.as_json(),
            Some(&json!({"words": 3, "chars": 5, "lines": 1}))
        );
    }
}
