//! The list of conversations started by this app, so the chat can switch back to
//! one. DeepSeek keeps the messages; here is only what is needed to continue:
//! the remote session id and the id of the last answer, plus a title for the list.

use crate::config::Paths;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Sessions kept; the oldest are dropped.
const KEEP: usize = 50;
/// Characters of the first task shown as the title.
const TITLE_CHARS: usize = 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// The chat session id of DeepSeek
    pub id: String,
    /// Id of the last answer; the next prompt hangs below it
    pub parent_message_id: Option<u64>,
    /// The first task, shortened
    pub title: String,
    /// Working directory the conversation was started in
    pub cwd: String,
    /// Tasks sent so far
    pub tasks: u32,
    /// Unix seconds of the last task
    pub updated: u64,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A one-line title from a task.
pub fn title(task: &str) -> String {
    let line = task.lines().next().unwrap_or("").trim();
    match line.chars().count() > TITLE_CHARS {
        true => format!(
            "{}…",
            line.chars().take(TITLE_CHARS - 1).collect::<String>()
        ),
        false => line.to_string(),
    }
}

/// "just now", "5 min ago", "3 h ago", "2 d ago".
pub fn age(now: u64, then: u64) -> String {
    match now.saturating_sub(then) {
        0..=59 => "just now".into(),
        secs @ 60..=3599 => format!("{} min ago", secs / 60),
        secs @ 3600..=86_399 => format!("{} h ago", secs / 3600),
        secs => format!("{} d ago", secs / 86_400),
    }
}

/// `<home>/sessions.json`: the sessions, the most recent first.
pub struct Store {
    file: PathBuf,
    pub sessions: Vec<Session>,
}

impl Store {
    pub fn load(paths: &Paths) -> Self {
        let file = paths.sessions();
        let sessions = fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { file, sessions }
    }

    /// Adds the session or replaces the one with the same id, puts it first and saves.
    pub fn upsert(&mut self, session: Session) {
        self.sessions.retain(|other| other.id != session.id);
        self.sessions.insert(0, session);
        self.sessions.truncate(KEEP);
        let _ = fs::write(
            &self.file,
            serde_json::to_string_pretty(&self.sessions).unwrap_or_default(),
        );
    }

    /// The lines of the list: a mark for the session in use, title, tasks, age, folder.
    pub fn rows(&self, now: u64, current: Option<&str>) -> Vec<String> {
        self.sessions
            .iter()
            .map(|session| {
                let mark = if current == Some(session.id.as_str()) {
                    "●"
                } else {
                    " "
                };
                let pad = " ".repeat(TITLE_CHARS.saturating_sub(session.title.chars().count()));
                let tasks = match session.tasks {
                    1 => "1 task ".to_string(),
                    n => format!("{n} tasks"),
                };
                format!(
                    "{mark} {}{pad}  {tasks:>9}  {:>10}  {}",
                    session.title,
                    age(now, session.updated),
                    session.cwd
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, title: &str, updated: u64) -> Session {
        Session {
            id: id.into(),
            parent_message_id: Some(4),
            title: title.into(),
            cwd: "/w".into(),
            tasks: 2,
            updated,
        }
    }

    fn store(name: &str) -> (Paths, Store) {
        let paths = Paths {
            home: std::env::temp_dir().join(format!("dsc-sessions-{name}-{}", std::process::id())),
        };
        let _ = fs::remove_dir_all(&paths.home);
        fs::create_dir_all(&paths.home).unwrap();
        let store = Store::load(&paths);
        (paths, store)
    }

    #[test]
    fn upsert_puts_the_latest_first_and_survives_a_reload() {
        let (paths, mut store) = store("upsert");
        store.upsert(session("a", "first", 1));
        store.upsert(session("b", "second", 2));
        let mut again = session("a", "first", 3);
        again.tasks = 5;
        store.upsert(again);
        let loaded = Store::load(&paths);
        let ids: Vec<&str> = loaded.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(loaded.sessions[0].tasks, 5);
        assert_eq!(loaded.sessions[0].parent_message_id, Some(4));
        fs::remove_dir_all(paths.home).unwrap();
    }

    #[test]
    fn only_the_newest_sessions_are_kept() {
        let (paths, mut store) = store("keep");
        for n in 0..KEEP + 3 {
            store.upsert(session(&n.to_string(), "t", n as u64));
        }
        assert_eq!(store.sessions.len(), KEEP);
        assert_eq!(store.sessions[0].id, (KEEP + 2).to_string());
        fs::remove_dir_all(paths.home).unwrap();
    }

    #[test]
    fn titles_ages_and_rows() {
        assert_eq!(title("  list files\nand more"), "list files");
        assert_eq!(title(&"x".repeat(100)).chars().count(), TITLE_CHARS);
        assert_eq!(age(100, 90), "just now");
        assert_eq!(age(1000, 100), "15 min ago");
        assert_eq!(age(10_000, 100), "2 h ago");
        assert_eq!(age(200_000, 100), "2 d ago");

        let (paths, mut store) = store("rows");
        store.upsert(session("a", "first", 100));
        store.upsert(session("b", "second", 1000));
        let rows = store.rows(1000, Some("a"));
        assert!(rows[0].starts_with("  second"), "{rows:?}");
        assert!(
            rows[0].contains("2 tasks") && rows[0].contains("just now") && rows[0].ends_with("/w")
        );
        assert!(rows[1].starts_with("● first") && rows[1].contains("15 min ago"));
        fs::remove_dir_all(paths.home).unwrap();
    }
}
