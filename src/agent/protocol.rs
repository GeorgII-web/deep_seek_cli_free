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
    /// The first action block; `extra` = further blocks that are ignored
    Action { action: Action, extra: usize },
    /// A block that cannot be used; the text goes back to the model
    Invalid(String),
}

/// The first message of a conversation: the rules, then the task.
pub fn first_prompt(task: &str, cwd: &str) -> String {
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

Rules:
- One action per answer. Never write the result yourself; wait for mine.
- Commands are not interactive: no editors, no pagers, no prompts, no sudo password.
- The user may deny an action; then try another way or ask the user.
- Prefer commands that only read. Change or delete something only when the task needs it.
- When the task is done or cannot be done, answer in plain text without a block. Keep it short.

Working directory: {cwd}

Task: {task}"#
    )
}

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
    match serde_json::from_str::<Action>(first.join("\n").trim()) {
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
}
