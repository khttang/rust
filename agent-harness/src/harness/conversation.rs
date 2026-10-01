//! Conversation transcript for multi-turn runs.

use rig_core::message::Message;

/// Ordered message history, oldest first, excluding the system preamble.
///
/// Owned by the caller and passed to [`crate::harness::AgentLoop::run`] by
/// `&mut`, so one loop can serve many independent conversations and a
/// conversation survives a model switch.
///
/// Unbounded: every turn is kept and resent. Trimming must never separate an
/// assistant tool-call message from the user message holding its results;
/// providers reject orphaned calls or results.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversation {
    messages: Vec<Message>,
}

impl Conversation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Drop every message after the first `len`. Used to roll back a failed
    /// run; a no-op if the conversation is already that short.
    pub fn truncate(&mut self, len: usize) {
        self.messages.truncate(len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_is_send_sync_static() {
        fn assert_bounds<T: Send + Sync + 'static>() {}
        assert_bounds::<Conversation>();
    }

    #[test]
    fn push_truncate_clear() {
        let mut c = Conversation::new();
        assert!(c.is_empty());
        c.push(Message::user("hi"));
        c.push(Message::assistant("hello"));
        assert_eq!(c.len(), 2);
        assert_eq!(c.messages()[0], Message::user("hi"));

        c.truncate(1);
        assert_eq!(c.messages(), &[Message::user("hi")]);
        c.truncate(5);
        assert_eq!(c.len(), 1);

        c.clear();
        assert!(c.is_empty());
    }
}
