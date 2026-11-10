//! The model the chat talks to: the real DeepSeek account or the built-in mock.

use crate::backend::{Backend, Event};
use crate::deepseek::client::Client;
use crate::deepseek::session::Conversation;
use crate::mock::Mock;
use anyhow::Result;

pub enum Model {
    Deepseek(Box<Conversation>),
    Mock(Mock),
}

impl Model {
    pub fn is_mock(&self) -> bool {
        matches!(self, Self::Mock(_))
    }

    /// A new login for the same conversation; nothing to do for the mock.
    pub fn set_client(&mut self, client: Client) {
        if let Self::Deepseek(conversation) = self {
            conversation.set_client(client);
        }
    }

    /// What is needed to continue the conversation later; the mock has nothing to continue.
    pub fn state(&self) -> Option<(String, Option<u64>)> {
        match self {
            Self::Deepseek(conversation) => conversation.state(),
            Self::Mock(_) => None,
        }
    }

    pub fn resume(&mut self, session_id: String, parent_message_id: Option<u64>) {
        if let Self::Deepseek(conversation) = self {
            conversation.resume(session_id, parent_message_id);
        }
    }
}

impl Backend for Model {
    async fn ask(&mut self, prompt: &str, sink: &mut dyn FnMut(&Event)) -> Result<String> {
        match self {
            Self::Deepseek(conversation) => conversation.ask(prompt, sink).await,
            Self::Mock(mock) => mock.ask(prompt, sink).await,
        }
    }
}
