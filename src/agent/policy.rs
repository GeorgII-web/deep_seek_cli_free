//! Decides whether an action runs without asking, needs the user, or never runs.

use super::protocol::Action;
use crate::config::Policy;
use std::collections::HashSet;

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Allow,
    Ask,
    /// Never runs, not even with `--yes`; the text names the pattern
    Deny(String),
}

/// Characters that make a command more than "one program with arguments".
const SHELL_SYNTAX: &[char] = &[';', '&', '|', '>', '<', '$', '(', ')', '`', '\n', '{', '}'];

fn squeeze(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What "always" allows for the rest of the run: the program of a simple shell
/// command, or the tool name. `None` = this action can only be allowed once.
pub fn always_key(action: &Action) -> Option<String> {
    match action {
        Action::Shell { cmd, .. } if cmd.contains(SHELL_SYNTAX) => None,
        Action::Shell { cmd, .. } => cmd.split_whitespace().next().map(str::to_string),
        Action::ReadFile { .. } => Some("read_file".into()),
        Action::WriteFile { .. } => Some("write_file".into()),
    }
}

/// Order: deny list, `--yes`, allowed for this run, allow list, else ask.
pub fn check(action: &Action, policy: &Policy, yes: bool, always: &HashSet<String>) -> Verdict {
    let subject = match action {
        Action::Shell { cmd, .. } => squeeze(cmd),
        Action::ReadFile { path, .. } | Action::WriteFile { path, .. } => path.clone(),
    };
    for pattern in &policy.deny {
        let pattern = squeeze(pattern);
        if !pattern.is_empty() && subject.contains(&pattern) {
            return Verdict::Deny(pattern);
        }
    }
    if yes {
        return Verdict::Allow;
    }
    if always_key(action).is_some_and(|key| always.contains(&key)) {
        return Verdict::Allow;
    }
    if let Action::Shell { cmd, .. } = action
        && !cmd.contains(SHELL_SYNTAX)
    {
        // An entry is a program or a program with its first arguments ("git status").
        let allowed = policy
            .allow
            .iter()
            .map(|entry| squeeze(entry))
            .any(|entry| {
                !entry.is_empty() && (subject == entry || subject.starts_with(&format!("{entry} ")))
            });
        if allowed {
            return Verdict::Allow;
        }
    }
    Verdict::Ask
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(cmd: &str) -> Action {
        Action::Shell {
            cmd: cmd.into(),
            timeout_s: None,
            why: String::new(),
        }
    }

    fn policy() -> Policy {
        Policy {
            allow: vec!["ls".into(), "git status".into()],
            deny: vec!["rm -rf /".into(), ".ssh/id_".into()],
        }
    }

    fn verdict(action: &Action, yes: bool) -> Verdict {
        check(action, &policy(), yes, &HashSet::new())
    }

    #[test]
    fn allow_list_matches_whole_words_of_simple_commands() {
        assert_eq!(verdict(&shell("ls -la /tmp"), false), Verdict::Allow);
        assert_eq!(verdict(&shell("git  status -s"), false), Verdict::Allow);
        assert_eq!(verdict(&shell("git push"), false), Verdict::Ask);
        assert_eq!(verdict(&shell("lsblk"), false), Verdict::Ask);
        assert_eq!(verdict(&shell("ls; rm x"), false), Verdict::Ask);
        assert_eq!(verdict(&shell("ls $(rm x)"), false), Verdict::Ask);
        assert_eq!(verdict(&shell("ls > f"), false), Verdict::Ask);
    }

    #[test]
    fn deny_wins_over_yes_and_covers_paths() {
        assert_eq!(
            verdict(&shell("sudo rm  -rf /"), true),
            Verdict::Deny("rm -rf /".into())
        );
        let read = Action::ReadFile {
            path: "/home/u/.ssh/id_ed25519".into(),
            why: String::new(),
        };
        assert_eq!(verdict(&read, true), Verdict::Deny(".ssh/id_".into()));
        assert_eq!(verdict(&shell("make install"), true), Verdict::Allow);
    }

    #[test]
    fn files_ask_and_always_applies_per_program_or_tool() {
        let read = Action::ReadFile {
            path: "a".into(),
            why: String::new(),
        };
        assert_eq!(verdict(&read, false), Verdict::Ask);
        let always: HashSet<String> = ["read_file".to_string(), "cargo".to_string()].into();
        assert_eq!(check(&read, &policy(), false, &always), Verdict::Allow);
        assert_eq!(
            check(&shell("cargo build"), &policy(), false, &always),
            Verdict::Allow
        );
        assert_eq!(
            check(&shell("cargo build && rm x"), &policy(), false, &always),
            Verdict::Ask
        );
        assert_eq!(always_key(&shell("a | b")), None);
        assert_eq!(always_key(&shell("cargo test")), Some("cargo".into()));
    }
}
