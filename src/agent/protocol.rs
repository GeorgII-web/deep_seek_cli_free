//! The text protocol between the CLI and the model. The web chat has no function
//! calling, so the model asks for an action with one fenced `dsc` block of JSON.

use serde::Deserialize;

pub const FENCE: &str = "```dsc";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Shell {
        cmd: String,
        #[serde(default)]
        timeout_s: Option<u64>,
        #[serde(default)]
        why: String,
    },
    ReadFile {
        path: String,
        #[serde(default)]
        why: String,
    },
    WriteFile {
        path: String,
        content: String,
        #[serde(default)]
        why: String,
    },
}

impl Action {
    pub fn why(&self) -> &str {
        match self {
            Self::Shell { why, .. } | Self::ReadFile { why, .. } | Self::WriteFile { why, .. } => {
                why
            }
        }
    }

    /// One line for the user: what would run.
    pub fn summary(&self) -> String {
        match self {
            Self::Shell { cmd, .. } => format!("$ {cmd}"),
            Self::ReadFile { path, .. } => format!("read {path}"),
            Self::WriteFile { path, content, .. } => {
                format!("write {path} ({} bytes)", content.len())
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    /// No action block: the answer is the final one
    Final,
    /// The `done` block: the task ends without a text answer, because the output
    /// of the last action already is what the user asked for
    Done,
    /// The first action block; `extra` = further blocks that are ignored
    Action { action: Action, extra: usize },
    /// A block that cannot be used; the text goes back to the model
    Invalid(String),
}

/// The first message of a conversation: the rules, then the task.
pub fn first_prompt(task: &str, cwd: &str, web: bool) -> String {
    // Where web content comes from: the model's own search, or the user's machine.
    let web_rule = match web {
        true => {
            "- You have web search and a browser on your side, and they are on. For anything on the web (open a site, read a page, look something up) use them and answer in text: do not run curl, wget or a browser on the user's machine. Fetch from the user's machine only when the user asks for exactly that (\"locally\", \"with curl\"), or when the address is only reachable from there (localhost, a private network)."
        }
        false => {
            "- You have no web access on your side in this conversation. To read a web page, fetch it from the user's machine (for example with curl) as an action."
        }
    };
    format!(
        r#"You are a command line agent on the user's Linux machine. You reach the machine only through me: you write one action, I run it and send you the result.

To act, end your answer with exactly one block like this and then stop:

{FENCE}
{{"tool":"shell","cmd":"ls -la","why":"see the files"}}
```

Tools (the block holds one JSON object on one line):
- {{"tool":"shell","cmd":"<bash command>","timeout_s":60,"why":"<reason>"}} runs in bash; timeout_s is optional
- {{"tool":"read_file","path":"<path>","why":"<reason>"}}
- {{"tool":"write_file","path":"<path>","content":"<whole file content>","why":"<reason>"}}
- {{"tool":"done"}} ends the task without a text answer; allowed only right after the result of an action you ran for this task

Rules:
- One action per answer. Never write the result yourself; wait for mine.
- Commands are not interactive: no editors, no pagers, no prompts, no sudo password.
- The user may deny an action; then try another way or ask the user.
- Prefer commands that only read. Change or delete something only when the task needs it.
{web_rule}
- The user sees the output of every action on the screen, exactly as you get it. When that output already is what the user asked for (a file listing, the content of a file, a status, a version), do not repeat, reformat or summarize it: send {{"tool":"done"}} as your whole answer.
- The done block is never the first answer to a task: first run the action the task needs. Each task starts from nothing, even when a similar one was done before; the user sees only what you run or write now.
- Write a text answer only when it adds something the output does not show: a conclusion, a number you worked out, an explanation the user asked for, or the reason the task cannot be done. Then answer in plain text without a block and keep it short.

Working directory: {cwd}

Task: {task}"#
    )
}

/// A further task in a conversation that already got the rules.
pub fn next_prompt(task: &str, web: bool) -> String {
    // The user can switch the web search between tasks (deep thinking turns it off).
    let web = match web {
        true => "Web search on your side is on for this task.",
        false => {
            "Web search on your side is off for this task: fetch web pages from the user's machine when the task needs them."
        }
    };
    format!(
        "New task from the user. The same rules apply: one {FENCE} block per answer. Start with the action this task needs (or a text answer when it needs none); the done block comes only after the result of an action of this task. {web}\n\nTask: {task}"
    )
}

/// The prompt with a note on the files attached to it, right before the task:
/// their content comes with the message, they are not on the machine to look for.
pub fn with_files(prompt: String, names: &[&str]) -> String {
    if names.is_empty() {
        return prompt;
    }
    let note = format!(
        "Attached files (their content comes with this message; do not look for them on the machine): {}\n\n",
        names.join(", ")
    );
    match prompt.rfind("Task: ") {
        Some(at) => format!("{}{note}{}", &prompt[..at], &prompt[at..]),
        None => format!("{note}{prompt}"),
    }
}

/// What the model gets when it ends a task with the done block before any
/// action ran: nothing would be on the user's screen.
pub const DONE_TOO_EARLY: &str = "You sent the done block, but no action ran in this task, so the user sees nothing. Run the action the task needs now, or answer in plain text.";

/// Finds the action in an answer of the model.
pub fn parse(answer: &str) -> Parsed {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in answer.lines() {
        let trimmed = line.trim();
        match &mut current {
            None if trimmed == FENCE => current = Some(Vec::new()),
            None => {}
            Some(_) if trimmed == "```" => blocks.extend(current.take()),
            Some(lines) => lines.push(line),
        }
    }
    // A block the model did not close still counts.
    blocks.extend(current);

    let Some(first) = blocks.first() else {
        return Parsed::Final;
    };
    let block = first.join("\n");
    let is_done = serde_json::from_str::<serde_json::Value>(block.trim())
        .is_ok_and(|value| value.get("tool").and_then(|tool| tool.as_str()) == Some("done"));
    if is_done {
        return Parsed::Done;
    }
    match serde_json::from_str::<Action>(block.trim()) {
        Ok(action) => Parsed::Action {
            action,
            extra: blocks.len() - 1,
        },
        Err(err) => Parsed::Invalid(format!(
            "Your action block is not valid: {err}. Send one JSON object on one line inside the {FENCE} block, with a known tool and its fields."
        )),
    }
}

/// Result of a shell action as the next message to the model.
pub fn shell_result(exit: Option<i32>, timed_out: bool, stdout: &str, stderr: &str) -> String {
    let status = match (timed_out, exit) {
        (true, _) => "timed out, killed".to_string(),
        (false, Some(code)) => format!("exit code {code}"),
        (false, None) => "killed by a signal".to_string(),
    };
    let mut text = format!("Result: {status}\n");
    for (name, body) in [("stdout", stdout), ("stderr", stderr)] {
        if !body.trim().is_empty() {
            text.push_str(&format!("--- {name} ---\n{}\n", body.trim_end()));
        }
    }
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        text.push_str("(no output)\n");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_answer_is_final() {
        assert_eq!(parse("All done.\n```bash\nls\n```"), Parsed::Final);
    }

    #[test]
    fn one_block() {
        let answer = "Let me look.\n\n```dsc\n{\"tool\":\"shell\",\"cmd\":\"ls -la\",\"why\":\"see\"}\n```\n";
        let Parsed::Action { action, extra } = parse(answer) else {
            panic!()
        };
        assert_eq!(extra, 0);
        assert_eq!(
            action,
            Action::Shell {
                cmd: "ls -la".into(),
                timeout_s: None,
                why: "see".into()
            }
        );
        assert_eq!(action.summary(), "$ ls -la");
    }

    #[test]
    fn first_of_several_blocks_and_unclosed_block() {
        let answer = "```dsc\n{\"tool\":\"read_file\",\"path\":\"a\"}\n```\ntext\n```dsc\n{\"tool\":\"shell\",\"cmd\":\"x\"}";
        let Parsed::Action { action, extra } = parse(answer) else {
            panic!()
        };
        assert_eq!(extra, 1);
        assert_eq!(
            action,
            Action::ReadFile {
                path: "a".into(),
                why: String::new()
            }
        );
    }

    #[test]
    fn write_file_keeps_fences_inside_the_content() {
        let answer = "```dsc\n{\"tool\":\"write_file\",\"path\":\"a.md\",\"content\":\"x\\n```\\ny\\n\"}\n```";
        let Parsed::Action { action, .. } = parse(answer) else {
            panic!()
        };
        assert_eq!(
            action,
            Action::WriteFile {
                path: "a.md".into(),
                content: "x\n```\ny\n".into(),
                why: String::new()
            }
        );
    }

    #[test]
    fn broken_json_and_unknown_tool_are_invalid() {
        assert!(matches!(
            parse("```dsc\n{\"tool\":\"shell\",\n```"),
            Parsed::Invalid(_)
        ));
        assert!(matches!(
            parse("```dsc\n{\"tool\":\"rm\",\"path\":\"/\"}\n```"),
            Parsed::Invalid(_)
        ));
        assert!(matches!(
            parse("```dsc\n{\"tool\":\"shell\"}\n```"),
            Parsed::Invalid(_)
        ));
    }

    #[test]
    fn shell_result_text() {
        assert_eq!(
            shell_result(Some(0), false, "a\n", ""),
            "Result: exit code 0\n--- stdout ---\na\n"
        );
        assert_eq!(
            shell_result(None, true, "", " "),
            "Result: timed out, killed\n(no output)\n"
        );
    }

    #[test]
    fn done_block_ends_without_an_answer() {
        assert_eq!(parse("```dsc\n{\"tool\":\"done\"}\n```"), Parsed::Done);
        assert_eq!(
            parse("\n\n```dsc\n{ \"tool\" : \"done\", \"why\": \"shown\" }\n```\n"),
            Parsed::Done
        );
        let prompt = first_prompt("t", "/w", true);
        assert!(prompt.contains("web search and a browser on your side"));
        assert!(prompt.contains("do not run curl"));
        assert!(first_prompt("t", "/w", false).contains("no web access on your side"));
        assert!(prompt.contains(r#"{"tool":"done"}"#));
        assert!(prompt.contains("do not repeat"));
        assert!(prompt.contains("never the first answer to a task"));
        assert!(next_prompt("t", true).contains("only after the result of an action of this task"));
        assert!(next_prompt("t", true).contains("search on your side is on"));
        assert!(next_prompt("t", false).contains("search on your side is off"));
        let attached = with_files(next_prompt("t", true), &["a.pdf", "b.png"]);
        assert!(attached.ends_with("a.pdf, b.png\n\nTask: t"));
        assert_eq!(with_files("x".into(), &[]), "x");
    }
}
