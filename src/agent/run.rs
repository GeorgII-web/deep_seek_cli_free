//! The agent loop: prompt → answer → action → result → next prompt.

use super::interrupt::{Interrupt, Interrupts};
use super::policy::{self, Verdict};
use super::protocol::{self, Action, Parsed};
use super::tools::{self, ShellOutput};
use crate::backend::{Backend, Event};
use crate::config::Settings;
use anyhow::Result;
use std::collections::HashSet;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Yes,
    /// Yes, and do not ask again for this program or tool in this run
    Always,
    No,
    /// Run this shell command instead
    Edit(String),
}

/// What the loop shows and asks; the terminal is `ui::Terminal`, tests use a fake.
pub trait Ui {
    /// A piece of the model's answer
    fn event(&mut self, event: &Event);
    /// The answer is complete
    fn answer_end(&mut self);
    /// Asks the user about an action; `can_always` = "always" is offered
    fn approve(&mut self, action: &Action, can_always: bool) -> Decision;
    /// The action the model asked for, before any check
    fn action(&mut self, action: &Action);
    /// The action starts now (allowed or approved)
    fn running(&mut self, action: &Action);
    /// What the action returned (the text that goes to the model)
    fn result(&mut self, text: &str);
    /// A remark of the CLI itself
    fn note(&mut self, text: &str);
}

/// Runs one task: `prompt` is `protocol::first_prompt` or `next_prompt`. `always`
/// holds what the user allowed for the rest of the session.
pub async fn run<B: Backend, U: Ui>(
    backend: &mut B,
    ui: &mut U,
    mut prompt: String,
    settings: &Settings,
    yes: bool,
    always: &mut HashSet<String>,
    interrupts: &mut Interrupts,
) -> Result<()> {
    interrupts.clear();
    for _ in 0..settings.agent.max_steps {
        let answer = {
            let mut sink = |event: &Event| ui.event(event);
            tokio::select! {
                answer = backend.ask(&prompt, &mut sink) => answer?,
                why = interrupts.next() => return Err(why.into()),
            }
        };
        ui.answer_end();

        prompt = match protocol::parse(&answer) {
            Parsed::Final => return Ok(()),
            Parsed::Invalid(message) => {
                ui.note("the model sent a broken action block; asking it to repeat");
                message
            }
            Parsed::Action { action, extra } => {
                let mut result = act(action, ui, settings, yes, always, interrupts).await?;
                if extra > 0 {
                    result.push_str("\nNote: only your first action block was run. Send one block per answer.\n");
                }
                ui.result(&result);
                result
            }
        };
    }
    ui.note(&format!(
        "stopped: step limit {} reached (agent.max_steps)",
        settings.agent.max_steps
    ));
    Ok(())
}

/// Checks, asks and runs one action; returns the message for the model, or the
/// interrupt that ended the task while the action ran.
async fn act<U: Ui>(
    mut action: Action,
    ui: &mut U,
    settings: &Settings,
    yes: bool,
    always: &mut HashSet<String>,
    interrupts: &mut Interrupts,
) -> Result<String, Interrupt> {
    ui.action(&action);
    loop {
        match policy::check(&action, &settings.policy, yes, always) {
            Verdict::Deny(pattern) => {
                ui.note(&format!(
                    "denied by policy.deny (`{pattern}`): {}",
                    action.summary()
                ));
                return Ok(format!(
                    "Denied: this matches the user's deny rule `{pattern}`. It will never run; do not try it again.\n"
                ));
            }
            Verdict::Allow => break,
            Verdict::Ask => {
                let key = policy::always_key(&action);
                match ui.approve(&action, key.is_some()) {
                    Decision::Yes => break,
                    Decision::Always => {
                        always.extend(key);
                        break;
                    }
                    Decision::No => {
                        return Ok("Denied by the user. Try another way, or explain what you need and stop.\n".into());
                    }
                    // The edited command goes through the checks again.
                    Decision::Edit(cmd) => match &mut action {
                        Action::Shell { cmd: old, .. } => {
                            *old = cmd;
                            ui.action(&action);
                            // Do not ask twice: only the deny list is checked again.
                            if let Verdict::Deny(_) =
                                policy::check(&action, &settings.policy, true, always)
                            {
                                continue;
                            }
                            break;
                        }
                        _ => continue,
                    },
                }
            }
        }
    }
    ui.running(&action);
    execute(&action, settings, interrupts).await
}

async fn execute(
    action: &Action,
    settings: &Settings,
    interrupts: &mut Interrupts,
) -> Result<String, Interrupt> {
    let max = settings.agent.max_output_bytes;
    Ok(match action {
        Action::Shell { cmd, timeout_s, .. } => {
            let limit = settings.agent.command_timeout_secs;
            let timeout = Duration::from_secs(timeout_s.unwrap_or(limit).clamp(1, limit.max(1)));
            match tools::shell(cmd, timeout, interrupts).await {
                Ok(ShellOutput {
                    interrupted: Some(why),
                    ..
                }) => return Err(why),
                Ok(output) => protocol::shell_result(
                    output.exit,
                    output.timed_out,
                    &tools::cap(&output.stdout, max),
                    &tools::cap(&output.stderr, max),
                ),
                Err(err) => format!("Error: {err:#}\n"),
            }
        }
        Action::ReadFile { path, .. } => match tools::read_file(path) {
            Ok(text) => format!("--- {path} ---\n{}\n", tools::cap(&text, max)),
            Err(err) => format!("Error: {err:#}\n"),
        },
        Action::WriteFile { path, content, .. } => match tools::write_file(path, content) {
            Ok(()) => format!("Written: {path} ({} bytes)\n", content.len()),
            Err(err) => format!("Error: {err:#}\n"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Answers from a script and records the prompts it got.
    struct Script {
        answers: VecDeque<String>,
        prompts: Vec<String>,
    }

    impl Script {
        fn new(answers: &[&str]) -> Self {
            Self {
                answers: answers.iter().map(|a| a.to_string()).collect(),
                prompts: Vec::new(),
            }
        }
    }

    impl Backend for Script {
        async fn ask(&mut self, prompt: &str, sink: &mut dyn FnMut(&Event)) -> Result<String> {
            self.prompts.push(prompt.to_string());
            let answer = self
                .answers
                .pop_front()
                .expect("the script ran out of answers");
            sink(&Event::Text(answer.clone()));
            Ok(answer)
        }
    }

    #[derive(Default)]
    struct FakeUi {
        decisions: VecDeque<Decision>,
        asked: Vec<String>,
        notes: Vec<String>,
    }

    impl Ui for FakeUi {
        fn event(&mut self, _: &Event) {}
        fn answer_end(&mut self) {}
        fn approve(&mut self, action: &Action, _: bool) -> Decision {
            self.asked.push(action.summary());
            self.decisions.pop_front().expect("unexpected question")
        }
        fn action(&mut self, _: &Action) {}
        fn running(&mut self, _: &Action) {}
        fn result(&mut self, _: &str) {}
        fn note(&mut self, text: &str) {
            self.notes.push(text.to_string());
        }
    }

    fn block(json: &str) -> String {
        format!("ok\n```dsc\n{json}\n```")
    }

    async fn run_task(
        backend: &mut Script,
        ui: &mut FakeUi,
        task: &str,
        settings: &Settings,
        yes: bool,
    ) -> Result<()> {
        let prompt = protocol::first_prompt(task, "/work");
        run(
            backend,
            ui,
            prompt,
            settings,
            yes,
            &mut HashSet::new(),
            &mut Interrupts::signal(),
        )
        .await
    }

    fn settings() -> Settings {
        let mut settings = Settings::default();
        settings.policy.allow = vec!["echo".into()];
        settings.policy.deny = vec!["forbidden".into()];
        settings
    }

    #[tokio::test]
    async fn allowed_command_runs_and_its_output_goes_back() {
        let mut backend =
            Script::new(&[&block(r#"{"tool":"shell","cmd":"echo hi"}"#), "It said hi."]);
        let mut ui = FakeUi::default();
        run_task(&mut backend, &mut ui, "say hi", &settings(), false)
            .await
            .unwrap();
        assert!(backend.prompts[0].ends_with("Task: say hi"));
        assert_eq!(
            backend.prompts[1],
            "Result: exit code 0\n--- stdout ---\nhi\n"
        );
        assert!(ui.asked.is_empty());
    }

    #[tokio::test]
    async fn unknown_command_asks_no_and_always() {
        let touch = block(r#"{"tool":"shell","cmd":"true first"}"#);
        let again = block(r#"{"tool":"shell","cmd":"true second"}"#);
        let mut backend = Script::new(&[&touch, &touch, &again, "done"]);
        let mut ui = FakeUi {
            decisions: [Decision::No, Decision::Always].into(),
            ..Default::default()
        };
        run_task(&mut backend, &mut ui, "t", &settings(), false)
            .await
            .unwrap();
        assert!(backend.prompts[1].starts_with("Denied by the user"));
        assert!(backend.prompts[2].starts_with("Result: exit code 0"));
        // "always" covered the third command: only two questions.
        assert_eq!(ui.asked, ["$ true first", "$ true first"]);
    }

    #[tokio::test]
    async fn deny_list_holds_with_yes_and_after_an_edit() {
        let bad = block(r#"{"tool":"shell","cmd":"echo forbidden"}"#);
        let mut backend = Script::new(&[&bad, "ok"]);
        let mut ui = FakeUi::default();
        run_task(&mut backend, &mut ui, "t", &settings(), true)
            .await
            .unwrap();
        assert!(backend.prompts[1].starts_with("Denied: this matches"));

        let other = block(r#"{"tool":"shell","cmd":"true x"}"#);
        let mut backend = Script::new(&[&other, "ok"]);
        let mut ui = FakeUi {
            decisions: [Decision::Edit("echo forbidden".into())].into(),
            ..Default::default()
        };
        run_task(&mut backend, &mut ui, "t", &settings(), false)
            .await
            .unwrap();
        assert!(backend.prompts[1].starts_with("Denied: this matches"));
    }

    #[tokio::test]
    async fn edit_runs_the_new_command() {
        let mut backend = Script::new(&[&block(r#"{"tool":"shell","cmd":"true x"}"#), "ok"]);
        let mut ui = FakeUi {
            decisions: [Decision::Edit("printf edited".into())].into(),
            ..Default::default()
        };
        run_task(&mut backend, &mut ui, "t", &settings(), false)
            .await
            .unwrap();
        assert_eq!(
            backend.prompts[1],
            "Result: exit code 0\n--- stdout ---\nedited\n"
        );
    }

    #[tokio::test]
    async fn broken_block_is_sent_back_and_the_step_limit_stops() {
        let mut limited = settings();
        limited.agent.max_steps = 2;
        let mut backend = Script::new(&[
            "```dsc\nnot json\n```",
            &block(r#"{"tool":"shell","cmd":"echo a"}"#),
        ]);
        let mut ui = FakeUi::default();
        run_task(&mut backend, &mut ui, "t", &limited, false)
            .await
            .unwrap();
        assert!(backend.prompts[1].starts_with("Your action block is not valid"));
        assert!(ui.notes.last().unwrap().contains("step limit 2"));
    }

    #[tokio::test]
    async fn files_are_written_and_read_with_yes() {
        let file = std::env::temp_dir().join(format!("dsc-run-{}.txt", std::process::id()));
        let path = file.to_str().unwrap();
        let write = block(&format!(
            r#"{{"tool":"write_file","path":"{path}","content":"abc"}}"#
        ));
        let read = block(&format!(r#"{{"tool":"read_file","path":"{path}"}}"#));
        let mut backend = Script::new(&[&write, &read, "ok"]);
        run_task(&mut backend, &mut FakeUi::default(), "t", &settings(), true)
            .await
            .unwrap();
        assert_eq!(backend.prompts[1], format!("Written: {path} (3 bytes)\n"));
        assert_eq!(backend.prompts[2], format!("--- {path} ---\nabc\n"));
        std::fs::remove_file(file).unwrap();
    }

    #[tokio::test]
    async fn stop_during_a_command_ends_the_task() {
        let mut backend = Script::new(&[
            &block(r#"{"tool":"shell","cmd":"sleep 30"}"#),
            "never asked",
        ]);
        let (sender, mut interrupts) = Interrupts::channel();
        let mut cfg = settings();
        cfg.policy.allow = vec!["sleep".into()];
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            sender.send(Interrupt::Stop).unwrap();
            sender
        });
        let prompt = protocol::first_prompt("t", "/work");
        let err = run(
            &mut backend,
            &mut FakeUi::default(),
            prompt,
            &cfg,
            false,
            &mut HashSet::new(),
            &mut interrupts,
        )
        .await
        .unwrap_err();
        assert_eq!(err.downcast_ref::<Interrupt>(), Some(&Interrupt::Stop));
        assert_eq!(backend.prompts.len(), 1);
        drop(stopper.await);
    }

    #[tokio::test]
    async fn always_and_the_conversation_carry_over_to_the_next_task() {
        let first = block(r#"{"tool":"shell","cmd":"true one"}"#);
        let second = block(r#"{"tool":"shell","cmd":"true two"}"#);
        let mut backend = Script::new(&[&first, "done 1", &second, "done 2"]);
        let mut ui = FakeUi {
            decisions: [Decision::Always].into(),
            ..Default::default()
        };
        let mut always = HashSet::new();
        let cfg = settings();
        run(
            &mut backend,
            &mut ui,
            protocol::first_prompt("a", "/work"),
            &cfg,
            false,
            &mut always,
            &mut Interrupts::signal(),
        )
        .await
        .unwrap();
        run(
            &mut backend,
            &mut ui,
            protocol::next_prompt("b"),
            &cfg,
            false,
            &mut always,
            &mut Interrupts::signal(),
        )
        .await
        .unwrap();
        assert!(backend.prompts[2].starts_with("New task from the user."));
        assert!(backend.prompts[2].ends_with("Task: b"));
        assert_eq!(ui.asked.len(), 1);
    }
}
