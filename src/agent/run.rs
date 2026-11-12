//! The agent loop: prompt → answer → action → result → next prompt.

use super::interrupt::{Interrupt, Interrupts};
use super::policy::{self, Verdict};
use super::protocol::{self, Action, Parsed};
use super::tools::{self, ShellOutput};
use crate::backend::{Backend, Event};
use crate::config::Settings;
use anyhow::Result;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// What a running command prints, as it arrives (any chunking)
    fn output(&mut self, piece: &str);
    /// What the action returned (the text that goes to the model)
    fn result(&mut self, text: &str);
    /// The model ended the task without a text answer: the last result is the
    /// answer and must be visible in full
    fn result_is_answer(&mut self);
    /// A remark of the CLI itself
    fn note(&mut self, text: &str);
}

/// What one task sends first, and how web fetches are treated in it.
pub struct Task {
    /// `protocol::first_prompt` or `next_prompt`
    pub prompt: String,
    /// The task as the user wrote it: the query of a web search that replaces a fetch
    pub text: String,
    /// Browse (the model's web search) is on: the `web` tool works, and unless the
    /// task asks for a local fetch, a command that fetches a public web page is
    /// replaced once per task by a web search for `text`
    pub browse: bool,
}

/// Runs a web search (`Backend::search`), shown like an action: the header, the
/// answer as its output; the caller shows the outcome. A failure ends the task like
/// a failed answer, with the block closed first.
async fn web_search<B: Backend, U: Ui>(
    backend: &mut B,
    ui: &mut U,
    action: &Action,
    interrupts: &mut Interrupts,
) -> Result<String> {
    let Action::Web { query, .. } = action else {
        anyhow::bail!("not a web search");
    };
    ui.action(action);
    ui.running(action);
    let found = {
        let mut sink = |event: &Event| {
            if let Event::Text(text) = event {
                ui.output(text);
            }
        };
        tokio::select! {
            found = backend.search(query, &mut sink) => found,
            why = interrupts.next() => return Err(why.into()),
        }
    };
    found.inspect_err(|err| ui.result(&format!("Error: web search failed: {err:#}\n")))
}

/// The first line of a command, for a note.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// Runs one task (`Task`). `always`
/// holds what the user allowed for the rest of the session. `yes` = run without
/// asking; it is read before every action, so the mode can change mid-task.
pub async fn run<B: Backend, U: Ui>(
    backend: &mut B,
    ui: &mut U,
    task: Task,
    settings: &Settings,
    yes: &AtomicBool,
    always: &mut HashSet<String>,
    interrupts: &mut Interrupts,
) -> Result<()> {
    interrupts.clear();
    let Task {
        mut prompt,
        text,
        browse,
    } = task;
    let replace_fetch = browse && !protocol::asks_local_fetch(&text);
    // Whether an action of this task put its output on the user's screen.
    let mut shown = false;
    // A web fetch was replaced by a web search once in this task.
    let mut replaced = false;
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
            // "The output is the answer" needs an output: an action that ran in this task.
            Parsed::Done if shown => {
                ui.result_is_answer();
                return Ok(());
            }
            Parsed::Done => {
                ui.note("the model ended without doing or saying anything; asking it again");
                protocol::DONE_TOO_EARLY.to_string()
            }
            Parsed::Invalid(message) => {
                ui.note("the model sent a broken action block; asking it to repeat");
                message
            }
            // Browse is on, yet the model fetches a web page here (it does, with the agent
            // rules in the conversation): the task goes to the web search instead, as typed.
            Parsed::Action {
                action: Action::Shell { cmd, .. },
                ..
            } if replace_fetch && !replaced && protocol::fetches_public_web(&cmd) => {
                replaced = true;
                let search = Action::Web {
                    query: text.clone(),
                    why: format!("Browse is on: searched instead of `{}`", first_line(&cmd)),
                };
                let found = web_search(backend, ui, &search, interrupts).await?;
                let result = protocol::searched_instead(&text, &found);
                ui.result(&result);
                shown = true;
                result
            }
            Parsed::Action {
                action: Action::Web { .. },
                ..
            } if !browse => {
                ui.note("the model asked for a web search, but Browse is off");
                protocol::WEB_OFF.to_string()
            }
            Parsed::Action {
                action: search @ Action::Web { .. },
                ..
            } => {
                let found = web_search(backend, ui, &search, interrupts).await?;
                let Action::Web { query, .. } = &search else {
                    unreachable!()
                };
                let result = protocol::web_result(query, &found);
                ui.result(&result);
                shown = true;
                result
            }
            Parsed::Action { action, extra } => {
                let mut result = act(action, ui, settings, yes, always, interrupts).await?;
                if extra > 0 {
                    result.push_str("\nNote: only your first action block was run. Send one block per answer.\n");
                }
                ui.result(&result);
                // A denied action showed nothing.
                shown = !result.starts_with("Denied");
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
    yes: &AtomicBool,
    always: &mut HashSet<String>,
    interrupts: &mut Interrupts,
) -> Result<String, Interrupt> {
    ui.action(&action);
    loop {
        match policy::check(
            &action,
            &settings.policy,
            yes.load(Ordering::Relaxed),
            always,
        ) {
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
    execute(&action, settings, interrupts, ui).await
}

async fn execute<U: Ui>(
    action: &Action,
    settings: &Settings,
    interrupts: &mut Interrupts,
    ui: &mut U,
) -> Result<String, Interrupt> {
    let max = settings.agent.max_output_bytes;
    Ok(match action {
        Action::Shell { cmd, timeout_s, .. } => {
            let limit = settings.agent.command_timeout_secs;
            let timeout = Duration::from_secs(timeout_s.unwrap_or(limit).clamp(1, limit.max(1)));
            match tools::shell(cmd, timeout, interrupts, &mut |piece| ui.output(piece)).await {
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
        // `run` sends a web search to the backend before it gets here.
        Action::Web { .. } => "Error: a web search is not a local action\n".into(),
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
        /// The web searches asked for
        searches: Vec<String>,
    }

    impl Script {
        fn new(answers: &[&str]) -> Self {
            Self {
                answers: answers.iter().map(|a| a.to_string()).collect(),
                prompts: Vec::new(),
                searches: Vec::new(),
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

        async fn search(&mut self, query: &str, sink: &mut dyn FnMut(&Event)) -> Result<String> {
            self.searches.push(query.to_string());
            sink(&Event::Text("8 °C, overcast".into()));
            Ok("8 °C, overcast".into())
        }
    }

    #[derive(Default)]
    struct FakeUi {
        decisions: VecDeque<Decision>,
        asked: Vec<String>,
        notes: Vec<String>,
        answers_by_result: u32,
        live: String,
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
        fn output(&mut self, piece: &str) {
            self.live.push_str(piece);
        }
        fn result(&mut self, _: &str) {}
        fn result_is_answer(&mut self) {
            self.answers_by_result += 1;
        }
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
        let prompt = protocol::first_prompt(task, "/work", true);
        run(
            backend,
            ui,
            Task {
                prompt,
                text: "t".into(),
                browse: false,
            },
            settings,
            &AtomicBool::new(yes),
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
        let prompt = protocol::first_prompt("t", "/work", true);
        let err = run(
            &mut backend,
            &mut FakeUi::default(),
            Task {
                prompt,
                text: "t".into(),
                browse: false,
            },
            &cfg,
            &AtomicBool::new(false),
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
    async fn command_output_reaches_the_user_while_it_runs() {
        let mut backend = Script::new(&[&block(r#"{"tool":"shell","cmd":"echo live"}"#), "ok"]);
        let mut ui = FakeUi::default();
        run_task(&mut backend, &mut ui, "t", &settings(), false)
            .await
            .unwrap();
        assert_eq!(ui.live, "live\n");
    }

    #[tokio::test]
    async fn done_without_an_action_is_sent_back() {
        let done = block(r#"{"tool":"done"}"#);
        // First answer: done at once. Then a denied action and done again. Then a real answer.
        let denied = block(r#"{"tool":"shell","cmd":"true x"}"#);
        let mut backend = Script::new(&[&done, &denied, &done, "Here is the answer."]);
        let mut ui = FakeUi {
            decisions: [Decision::No].into(),
            ..Default::default()
        };
        run_task(&mut backend, &mut ui, "get it", &settings(), false)
            .await
            .unwrap();
        assert_eq!(backend.prompts[1], protocol::DONE_TOO_EARLY);
        assert!(backend.prompts[2].starts_with("Denied by the user"));
        assert_eq!(backend.prompts[3], protocol::DONE_TOO_EARLY);
        assert_eq!(ui.answers_by_result, 0);
        assert_eq!(ui.notes.len(), 2);
    }

    #[tokio::test]
    async fn done_block_ends_the_task_and_marks_the_result_as_the_answer() {
        let done = block(r#"{"tool":"done"}"#);
        let mut backend = Script::new(&[&block(r#"{"tool":"shell","cmd":"echo listing"}"#), &done]);
        let mut ui = FakeUi::default();
        run_task(&mut backend, &mut ui, "list", &settings(), false)
            .await
            .unwrap();
        assert_eq!(backend.prompts.len(), 2);
        assert_eq!(ui.answers_by_result, 1);
    }

    async fn web_task(backend: &mut Script, ui: &mut FakeUi, text: &str, browse: bool) {
        run(
            backend,
            ui,
            Task {
                prompt: protocol::first_prompt(text, "/work", browse),
                text: text.into(),
                browse,
            },
            &settings(),
            &AtomicBool::new(false),
            &mut HashSet::new(),
            &mut Interrupts::signal(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn with_browse_on_a_web_fetch_becomes_a_web_search_for_the_task() {
        let curl =
            block(r#"{"tool":"shell","cmd":"curl -s 'https://wttr.in/Heidelberg?format=3'"}"#);
        let mut backend = Script::new(&[&curl, "Heidelberg: 8 °C, overcast."]);
        let mut ui = FakeUi::default();
        web_task(&mut backend, &mut ui, "weather in Heidelberg", true).await;
        // Not run, not asked: the task went to the web search as typed.
        assert!(ui.asked.is_empty());
        assert_eq!(backend.searches, ["weather in Heidelberg"]);
        assert_eq!(
            backend.prompts[1],
            protocol::searched_instead("weather in Heidelberg", "8 °C, overcast")
        );
        assert!(ui.live.contains("8 °C, overcast"));
        // Once per task: a second fetch goes the normal way, the user is asked.
        let mut backend = Script::new(&[&curl, &curl, "fetched"]);
        let mut ui = FakeUi {
            decisions: [Decision::No].into(),
            ..Default::default()
        };
        web_task(&mut backend, &mut ui, "weather in Heidelberg", true).await;
        assert_eq!(ui.asked.len(), 1);
        // A task that asks for curl, or Browse off: an ordinary action.
        for (text, browse) in [
            ("weather in Heidelberg with curl", true),
            ("weather", false),
        ] {
            let mut backend = Script::new(&[&curl, "fetched"]);
            let mut ui = FakeUi {
                decisions: [Decision::No].into(),
                ..Default::default()
            };
            web_task(&mut backend, &mut ui, text, browse).await;
            assert_eq!(ui.asked.len(), 1, "{text}");
            assert!(backend.searches.is_empty());
        }
    }

    #[tokio::test]
    async fn the_web_tool_searches_with_browse_on_only() {
        let web = block(r#"{"tool":"web","query":"news today"}"#);
        let mut backend = Script::new(&[&web, "Here is the news."]);
        let mut ui = FakeUi::default();
        web_task(&mut backend, &mut ui, "news", true).await;
        assert_eq!(backend.searches, ["news today"]);
        assert_eq!(
            backend.prompts[1],
            protocol::web_result("news today", "8 °C, overcast")
        );
        assert!(ui.asked.is_empty());
        let mut backend = Script::new(&[&web, "Without it then."]);
        web_task(&mut backend, &mut FakeUi::default(), "news", false).await;
        assert!(backend.searches.is_empty());
        assert_eq!(backend.prompts[1], protocol::WEB_OFF);
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
            Task {
                prompt: protocol::first_prompt("a", "/work", true),
                text: "t".into(),
                browse: false,
            },
            &cfg,
            &AtomicBool::new(false),
            &mut always,
            &mut Interrupts::signal(),
        )
        .await
        .unwrap();
        run(
            &mut backend,
            &mut ui,
            Task {
                prompt: protocol::next_prompt("b", true),
                text: "t".into(),
                browse: false,
            },
            &cfg,
            &AtomicBool::new(false),
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
