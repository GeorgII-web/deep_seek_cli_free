//! What the agent needs from a chat model; `deepseek::session::Conversation` is the
//! only implementation, tests use fakes.

use anyhow::Result;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Reasoning text (thinking mode)
    Thinking(String),
    /// Answer text
    Text(String),
}

/// One conversation: every `ask` continues below the previous answer.
#[allow(async_fn_in_trait)]
pub trait Backend {
    /// Sends `prompt`, reports the pieces to `sink` as they arrive and returns the
    /// whole answer text (without the reasoning).
    async fn ask(&mut self, prompt: &str, sink: &mut dyn FnMut(&Event)) -> Result<String>;
}
