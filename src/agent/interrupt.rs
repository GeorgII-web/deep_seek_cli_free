//! How the user stops a running task or leaves the app while one runs.

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Interrupt {
    /// End the current task (Esc; or Ctrl-C when there is no terminal to read keys from)
    Stop,
    /// Leave the app (Ctrl-C twice)
    Quit,
}

impl std::fmt::Display for Interrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stop => "stopped",
            Self::Quit => "quit",
        })
    }
}

impl std::error::Error for Interrupt {}

/// Source of interrupts for the agent loop.
pub struct Interrupts {
    /// Keys watched by the terminal UI; `None` = the Ctrl-C signal of the process
    keys: Option<UnboundedReceiver<Interrupt>>,
}

impl Interrupts {
    /// Without a terminal: Ctrl-C (SIGINT) stops the task.
    pub fn signal() -> Self {
        Self { keys: None }
    }

    /// With a terminal: the UI sends what the keys mean.
    pub fn channel() -> (UnboundedSender<Interrupt>, Self) {
        let (sender, receiver) = unbounded_channel();
        (
            sender,
            Self {
                keys: Some(receiver),
            },
        )
    }

    /// Forgets keys pressed before the task started.
    pub fn clear(&mut self) {
        if let Some(keys) = &mut self.keys {
            while keys.try_recv().is_ok() {}
        }
    }

    /// Waits for the next interrupt; never returns when there will be none.
    pub async fn next(&mut self) -> Interrupt {
        match &mut self.keys {
            Some(keys) => match keys.recv().await {
                Some(interrupt) => interrupt,
                None => std::future::pending().await,
            },
            None => match tokio::signal::ctrl_c().await {
                Ok(()) => Interrupt::Stop,
                Err(_) => std::future::pending().await,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn channel_delivers_and_clear_drops_old_keys() {
        let (sender, mut interrupts) = Interrupts::channel();
        sender.send(Interrupt::Stop).unwrap();
        interrupts.clear();
        sender.send(Interrupt::Quit).unwrap();
        assert_eq!(interrupts.next().await, Interrupt::Quit);
        assert_eq!(Interrupt::Stop.to_string(), "stopped");
    }
}
