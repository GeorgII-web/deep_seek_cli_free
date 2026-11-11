//! A fake model inside the app: scripted answers, no network, no account. Chosen
//! with `/model` (or `deepseek.api = "mock"`), for trying the app and for tests.
//! `scripts/mock-deepseek.py` plays the same parts over HTTP for the client code.

use crate::agent::protocol::FENCE;
use crate::backend::{Backend, Event};
use crate::config::MockSpeed;
use crate::deepseek::client::ApiError;
use anyhow::Result;
use std::time::Duration;

/// How a prompt starts when it is the result of an action.
const RESULT_STARTS: [&str; 7] = [
    "Result:",
    "--- ",
    "Written:",
    "Denied",
    "Error:",
    "Your action block",
    "The user interrupted",
];

/// What the mock understands; shown by `/model` and `/help`.
pub const TASKS: &str = "\
`!<command>`      asks to run the command, then ends with the silent done block
`run <command>`   asks to run it, then answers with a sentence
`browse <url>`    reads the page on the model's side and answers (nothing runs here)
`curl <url>`      the local way: asks to fetch the page with curl here, then answers
`read <path>`     asks to read the file; `write <path>` asks to write `mock` to it
`think <text>`    reasoning first, then an answer
`slow`            a long answer at a quarter of the speed (try Esc, scrolling)
`long`            80 lines; `code` = bold, inline code, a code block, a link
`broken`          an action block that is not JSON; `two` = two action blocks
`early`           the done block at once (the app asks again), then a sentence
`dead`            the \"token dead\" error; `429` = the \"too many requests\" error
anything else     `Mock answer to: <task>`";

/// `Mock::default()` answers without any pause (the unit tests); the app uses `Mock::new`.
#[derive(Default)]
pub struct Mock {
    /// What to answer to the result of the action asked for last
    after: Option<String>,
    /// The conversation's id (it starts with `sessions::MOCK_PREFIX`) and how many
    /// messages it has: enough to list it under `/sessions` and come back to it
    session: String,
    messages: u64,
    /// Deep thinking is on: every answer comes with some reasoning first
    thinking: bool,
    /// Pause before an answer starts
    wait: Duration,
    /// Pause between two pieces of an answer
    delay: Duration,
}

fn block(json: &str) -> String {
    format!("{FENCE}\n{json}\n```")
}

/// The text as one word for bash.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn json_string(text: &str) -> String {
    serde_json::Value::String(text.to_string()).to_string()
}

/// What a prompt gets: reasoning, answer, pause between the pieces.
struct Plan {
    thinking: String,
    answer: String,
    delay: Duration,
}

impl Mock {
    /// A mock that answers at the speed of the settings (`[mock]`).
    pub fn new(speed: &MockSpeed) -> Self {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        Self {
            session: format!("{}{millis}", crate::sessions::MOCK_PREFIX),
            wait: Duration::from_millis(speed.wait_ms),
            delay: Duration::from_millis(speed.word_ms),
            ..Self::default()
        }
    }

    pub fn set_thinking(&mut self, on: bool) {
        self.thinking = on;
    }

    /// What `/sessions` needs to come back: the id and the place in the conversation. `None` before the first answer.
    pub fn state(&self) -> Option<(String, Option<u64>)> {
        (self.messages > 0).then(|| (self.session.clone(), Some(self.messages)))
    }

    /// Continues an earlier mock conversation (the mock keeps no memory: only the id and the count go on).
    pub fn resume(&mut self, session: String, messages: Option<u64>) {
        self.session = session;
        self.messages = messages.unwrap_or(0);
    }

    fn plan(&mut self, prompt: &str) -> Result<Plan> {
        let delay = self.delay;
        let say = |answer: String| Plan {
            thinking: String::new(),
            answer,
            delay,
        };
        if RESULT_STARTS.iter().any(|start| prompt.starts_with(start)) {
            return Ok(say(self
                .after
                .take()
                .unwrap_or_else(|| "Mock: finished.".into())));
        }
        if prompt.starts_with("You sent the done block") {
            return Ok(say("Mock: a real answer after the early done.".into()));
        }
        let task = prompt.rsplit("Task: ").next().unwrap_or(prompt).trim();
        let done = block(r#"{"tool":"done"}"#);
        let shell = |cmd: &str| {
            block(&format!(
                r#"{{"tool":"shell","cmd":{},"why":"mock: the task asked for it"}}"#,
                json_string(cmd)
            ))
        };
        if let Some(cmd) = task.strip_prefix('!') {
            self.after = Some(done);
            return Ok(say(shell(cmd.trim())));
        }
        if let Some(cmd) = task.strip_prefix("run ") {
            self.after = Some("Mock: the command finished.".into());
            return Ok(say(format!("I will run it.\n\n{}", shell(cmd))));
        }
        // "Open this site": by default the model reads it on its own side (DeepSeek's
        // web search) and just answers; nothing runs on the user's machine.
        if task == "browse" || task.starts_with("browse ") {
            let url = match task["browse".len()..].trim() {
                "" => "https://example.org",
                url => url,
            };
            return Ok(say(format!(
                "Mock: I opened {url} with the browser on my side (nothing ran on your machine).\n\n# Example page\nThis is what the page says, as the model read it. See also https://example.org/more."
            )));
        }
        // The local way, when the user asks for it: fetch the page with curl here.
        if task == "curl" || task.starts_with("curl ") {
            let url = match task["curl".len()..].trim() {
                "" => "https://example.org",
                url => url,
            };
            self.after = Some(format!(
                "Mock: that is the start of the page at {url}, fetched from your machine."
            ));
            let command = format!("curl -sSL --max-time 20 {} | head -n 40", shell_quote(url));
            return Ok(say(format!(
                "I will fetch the page from your machine.\n\n{}",
                shell(&command)
            )));
        }
        if let Some(path) = task.strip_prefix("read ") {
            self.after = Some(done);
            return Ok(say(block(&format!(
                r#"{{"tool":"read_file","path":{},"why":"mock: read"}}"#,
                json_string(path)
            ))));
        }
        if let Some(path) = task.strip_prefix("write ") {
            self.after = Some("Mock: written.".into());
            let action = format!(
                r#"{{"tool":"write_file","path":{},"content":"mock\n","why":"mock: write"}}"#,
                json_string(path)
            );
            return Ok(say(block(&action)));
        }
        Ok(match task {
            "dead" => return Err(ApiError::TokenDead("mock".into()).into()),
            "429" => return Err(ApiError::RateLimited(Some(1)).into()),
            "slow" => Plan {
                thinking: String::new(),
                answer: (1..=40).map(|n| format!("word{n}")).collect::<Vec<_>>().join(" "),
                delay: delay * 4,
            },
            "long" => say((1..=80).map(|n| format!("line {n} of the long mock answer")).collect::<Vec<_>>().join("\n")),
            "code" => say(
                "Here is **bold**, `inline code` and a link https://example.org/docs.\n\n```bash\necho \"hello\"\nls -la\n```\n\n# Heading\nDone."
                    .into(),
            ),
            "broken" => say(format!("{FENCE}\nthis is not json\n```")),
            "early" => say(block(r#"{"tool":"done"}"#)),
            "two" => {
                self.after = Some("Mock: finished.".into());
                say(format!("{}\n\n{}", shell("echo first"), shell("echo second")))
            }
            task if task.starts_with("think") => Plan {
                thinking: "The user wants a thought. Two plus two is four.".into(),
                answer: "Mock thought: 4.".into(),
                delay,
            },
            task => say(format!("Mock answer to: {task}")),
        })
    }
}

/// Words with the space or line end after them, like the pieces of a real stream.
fn pieces(text: &str) -> Vec<&str> {
    text.split_inclusive([' ', '\n']).collect()
}

impl Backend for Mock {
    async fn ask(&mut self, prompt: &str, sink: &mut dyn FnMut(&Event)) -> Result<String> {
        sink(&Event::Status("Waiting for the mock".into()));
        tokio::time::sleep(self.wait).await;
        let mut plan = self.plan(prompt)?;
        if self.thinking && plan.thinking.is_empty() {
            plan.thinking =
                "Mock reasoning: the user asks something. I look at it from two sides and decide."
                    .into();
        }
        for piece in pieces(&plan.thinking) {
            sink(&Event::Thinking(piece.to_string()));
            tokio::time::sleep(plan.delay).await;
        }
        for piece in pieces(&plan.answer) {
            sink(&Event::Text(piece.to_string()));
            tokio::time::sleep(plan.delay).await;
        }
        // A prompt and its answer.
        self.messages += 2;
        Ok(plan.answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::protocol::{self, Action, Parsed};

    async fn ask(mock: &mut Mock, prompt: &str) -> (String, String) {
        let mut seen = String::new();
        let answer = mock
            .ask(prompt, &mut |event| {
                if let Event::Text(text) = event {
                    seen.push_str(text)
                }
            })
            .await
            .unwrap();
        (answer, seen)
    }

    #[tokio::test]
    async fn bang_runs_the_command_and_ends_with_done() {
        let mut mock = Mock::default();
        let (answer, streamed) = ask(
            &mut mock,
            &protocol::first_prompt("!echo \"a b\"", "/w", true),
        )
        .await;
        assert_eq!(answer, streamed);
        let Parsed::Action { action, .. } = protocol::parse(&answer) else {
            panic!("{answer}")
        };
        assert!(matches!(action, Action::Shell { cmd, .. } if cmd == "echo \"a b\""));
        let (after, _) = ask(&mut mock, "Result: exit code 0\n").await;
        assert_eq!(protocol::parse(&after), Parsed::Done);
    }

    #[tokio::test]
    async fn browse_answers_from_the_models_side_and_curl_fetches_here() {
        let mut mock = Mock::default();
        // Browsing is the model's own business: a text answer, no action.
        let (answer, _) = ask(&mut mock, "Task: browse https://ya.ru").await;
        assert_eq!(protocol::parse(&answer), Parsed::Final);
        assert!(answer.contains("https://ya.ru") && answer.contains("on my side"));
        // The local way is asked for by name.
        let (answer, _) = ask(&mut mock, "Task: curl https://ya.ru/?a=b'c").await;
        let Parsed::Action { action, .. } = protocol::parse(&answer) else {
            panic!("{answer}")
        };
        // The address is one quoted word: nothing in it can act as shell syntax.
        assert!(
            matches!(action, Action::Shell { cmd, .. } if cmd == "curl -sSL --max-time 20 'https://ya.ru/?a=b'\\''c' | head -n 40")
        );
        assert!(
            ask(&mut mock, "Result: exit code 0\n")
                .await
                .0
                .contains("fetched from your machine")
        );
    }

    #[tokio::test]
    async fn the_mock_has_a_session_to_come_back_to() {
        let mut mock = Mock::new(&MockSpeed {
            wait_ms: 0,
            word_ms: 0,
        });
        assert_eq!(mock.state(), None);
        ask(&mut mock, "Task: hello").await;
        let (id, place) = mock.state().unwrap();
        assert!(crate::sessions::is_mock(&id));
        assert_eq!(place, Some(2));
        let mut later = Mock::new(&MockSpeed {
            wait_ms: 0,
            word_ms: 0,
        });
        later.resume(id.clone(), place);
        ask(&mut later, "Task: again").await;
        assert_eq!(later.state(), Some((id, Some(4))));
    }

    #[tokio::test]
    async fn with_thinking_on_every_answer_reasons_first() {
        let mut mock = Mock::default();
        let mut thought = String::new();
        mock.ask("Task: hello", &mut |event| {
            if let Event::Thinking(text) = event {
                thought.push_str(text)
            }
        })
        .await
        .unwrap();
        assert_eq!(thought, "");
        mock.set_thinking(true);
        let answer = mock
            .ask("Task: hello", &mut |event| {
                if let Event::Thinking(text) = event {
                    thought.push_str(text)
                }
            })
            .await
            .unwrap();
        assert!(thought.starts_with("Mock reasoning"));
        assert_eq!(answer, "Mock answer to: hello");
    }

    #[tokio::test]
    async fn plain_task_long_and_follow_up_prompt() {
        let mut mock = Mock::default();
        assert_eq!(
            ask(&mut mock, &protocol::first_prompt("hello", "/w", true))
                .await
                .0,
            "Mock answer to: hello"
        );
        assert_eq!(
            ask(&mut mock, &protocol::next_prompt("again", true))
                .await
                .0,
            "Mock answer to: again"
        );
        assert_eq!(ask(&mut mock, "Task: long").await.0.lines().count(), 80);
        assert!(matches!(
            protocol::parse(&ask(&mut mock, "Task: broken").await.0),
            Parsed::Invalid(_)
        ));
    }

    #[tokio::test]
    async fn thinking_and_the_error_tasks() {
        let mut mock = Mock::default();
        let mut thought = String::new();
        let answer = mock
            .ask("Task: think", &mut |event| {
                if let Event::Thinking(text) = event {
                    thought.push_str(text)
                }
            })
            .await
            .unwrap();
        assert_eq!(answer, "Mock thought: 4.");
        assert!(thought.contains("four"));
        let dead = mock.ask("Task: dead", &mut |_| {}).await.unwrap_err();
        assert!(matches!(dead.downcast_ref(), Some(ApiError::TokenDead(_))));
        let limited = mock.ask("Task: 429", &mut |_| {}).await.unwrap_err();
        assert!(matches!(
            limited.downcast_ref(),
            Some(ApiError::RateLimited(Some(1)))
        ));
    }
}
