//! The text protocol between the CLI and the model. The web chat has no function
//! calling, so the model asks for an action in its own tool-call markup (DSML),
//! written as text; the prompt shows it the format and the tools.

use serde::Deserialize;

/// The DSML block of tool calls, as DeepSeek's chat template writes it.
const CALLS_OPEN: &str = "<｜DSML｜function_calls>";
const CALLS_CLOSE: &str = "</｜DSML｜function_calls>";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Shell {
        cmd: String,
        #[serde(default, deserialize_with = "seconds")]
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
    /// A question for DeepSeek's own web search (Browse on): asked in a chat of its
    /// own, without the agent rules, the way the site asks it; nothing runs here
    Web {
        query: String,
        #[serde(default)]
        why: String,
    },
}

impl Action {
    pub fn why(&self) -> &str {
        match self {
            Self::Shell { why, .. }
            | Self::ReadFile { why, .. }
            | Self::WriteFile { why, .. }
            | Self::Web { why, .. } => why,
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
            Self::Web { query, .. } => format!("web search: {query}"),
        }
    }
}

/// `timeout_s` as a number, or as text with a number (a DSML parameter marked
/// `string="true"`).
fn seconds<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value {
        Number(u64),
        Text(String),
    }
    match Option::<Value>::deserialize(d)? {
        None => Ok(None),
        Some(Value::Number(n)) => Ok(Some(n)),
        Some(Value::Text(text)) => text
            .trim()
            .parse()
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    /// No tool call: the answer is the final one
    Final,
    /// The `done` call: the task ends without a text answer, because the output
    /// of the last action already is what the user asked for
    Done,
    /// The first tool call; `extra` = further calls that are ignored
    Action { action: Action, extra: usize },
    /// A call that cannot be used; the text goes back to the model
    Invalid(String),
}

/// One tool call in DSML, from its JSON object: `{"tool":"shell","cmd":"ls"}` becomes
/// the `function_calls` block with one `invoke` and a `parameter` per field (text as
/// it is with `string="true"`, other values as JSON with `string="false"`).
pub fn dsml(json: &str) -> String {
    let object: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(json).unwrap_or_default();
    let tool = object
        .get("tool")
        .and_then(|tool| tool.as_str())
        .unwrap_or_default();
    let mut text = format!("{CALLS_OPEN}\n<｜DSML｜invoke name=\"{tool}\">\n");
    for (key, value) in object.iter().filter(|(key, _)| *key != "tool") {
        let (string, value) = match value {
            serde_json::Value::String(text) => ("true", text.clone()),
            other => ("false", other.to_string()),
        };
        text.push_str(&format!(
            "<｜DSML｜parameter name=\"{key}\" string=\"{string}\">{value}</｜DSML｜parameter>\n"
        ));
    }
    text.push_str(&format!("</｜DSML｜invoke>\n{CALLS_CLOSE}"));
    text
}

/// The first message of a conversation: the rules, then the task.
pub fn first_prompt(task: &str, cwd: &str, web: bool) -> String {
    // Where web content comes from: the model's own search, or the user's machine.
    let (web_rule, web_tool) = match web {
        true => (
            "- Your web search is on. Anything on the web or anything current (the weather, news, prices, exchange rates, sports results, opening hours, a fact you are not sure of, a site to open, a page to read) you get with the web tool, then you answer in plain text. Never fetch from the internet on the user's machine: no curl, wget, wttr.in, web APIs or scripts that download. Fetch from the user's machine only when the user asks for exactly that (\"locally\", \"with curl\"), or when the address is only reachable from there (localhost, a private network).",
            "\n- web: query (what to look up, or the address to read), why. Searches the internet and reads pages on DeepSeek's side; you get what it found",
        ),
        false => (
            "- You have no web access on your side in this conversation. To read a web page, fetch it from the user's machine (for example with curl) as an action.",
            "",
        ),
    };
    format!(
        r#"You are a command line agent on the user's Linux machine. You reach the machine only through me: you write one action, I run it and send you the result.

To act, end your answer with exactly one tool call in your own tool-call format (DSML) and then stop:

{CALLS_OPEN}
<｜DSML｜invoke name="shell">
<｜DSML｜parameter name="cmd" string="true">ls -la</｜DSML｜parameter>
<｜DSML｜parameter name="why" string="true">see the files</｜DSML｜parameter>
</｜DSML｜invoke>
{CALLS_CLOSE}

A parameter with string="true" holds text exactly as it is: no quotes, no escaping, several lines allowed. string="false" holds a number.

Tools and their parameters:
- shell: cmd (the bash command), why; timeout_s (seconds, string="false") is optional
- read_file: path, why
- write_file: path, content (the whole file), why{web_tool}
- done: no parameters. Ends the task without a text answer; allowed only right after the result of an action you ran for this task

Rules:
- One tool call (one invoke) per answer. Never write the result yourself; wait for mine.
- Commands are not interactive: no editors, no pagers, no prompts, no sudo password.
- The user may deny an action; then try another way or ask the user.
- Prefer commands that only read. Change or delete something only when the task needs it.
{web_rule}
- The user sees the output of every action on the screen, exactly as you get it. When that output already is what the user asked for (a file listing, the content of a file, a status, a version), do not repeat, reformat or summarize it: send the done call as your whole answer.
- The done call is never the first answer to a task: first run the action the task needs. Each task starts from nothing, even when a similar one was done before; the user sees only what you run or write now.
- Write a text answer only when it adds something the output does not show: a conclusion, a number you worked out, an explanation the user asked for, or the reason the task cannot be done. Then answer in plain text without a tool call and keep it short.

Working directory: {cwd}

Task: {task}"#
    )
}

/// A further task in a conversation that already got the rules.
pub fn next_prompt(task: &str, web: bool) -> String {
    // The user can switch the web search between tasks (deep thinking turns it off).
    let web = match web {
        true => {
            "Web search is on for this task: anything on the web or current (the weather, news, prices, a page) you get with the web tool (invoke name=\"web\", parameter query: what to look up, or the address to read) and then answer in plain text; do not fetch from the internet on the user's machine (no curl, wget, web APIs) unless the task asks for exactly that. This holds even if earlier tasks of this conversation fetched pages from the machine."
        }
        false => {
            "Web search on your side is off for this task: fetch web pages from the user's machine when the task needs them."
        }
    };
    format!(
        "New task from the user. The same rules apply: one tool call in DSML ({CALLS_OPEN} … {CALLS_CLOSE}) per answer. Start with the action this task needs (or a text answer when it needs none); the done call comes only after the result of an action of this task. {web}\n\nTask: {task}"
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

/// The result of a web search for the model: the first line is the outcome the user sees.
pub fn web_result(query: &str, answer: &str) -> String {
    format!(
        "Result: searched the web\nQuery: {query}\n\n{}\n",
        answer.trim()
    )
}

/// The result when a command that would fetch a public web page on the user's machine
/// was replaced by a web search for the user's task (Browse on).
pub fn searched_instead(query: &str, answer: &str) -> String {
    format!(
        "Result: searched the web\nYour command was not run: web search is on, so nothing from the internet is fetched on the user's machine. DeepSeek's web search was asked instead.\nQuery: {query}\n\n{}\n\nAnswer the task from this in plain text, or use the web tool for more.\n",
        answer.trim()
    )
}

/// Sent when the model asks for the web tool while web search is off.
pub const WEB_OFF: &str = "Not run: web search is off in this conversation (the user turned Browse off). Fetch the page from the user's machine as an action, or answer without it.";

/// The tool calls of an answer, as JSON objects for `Action`:
/// `<｜DSML｜invoke name="shell">` with `<｜DSML｜parameter name="cmd" string="true">ls</｜DSML｜parameter>`
/// becomes `{"tool":"shell","cmd":"ls"}`. A parameter with `string="false"` is JSON
/// (a number). The bars and the words around the tags vary (the real model also sent
/// `<｜｜DSML｜｜ calls>`), so only `invoke name=`, `parameter name=` and the closing
/// tags are relied on; a call without its closing tags still counts.
fn dsml_calls(answer: &str) -> Vec<String> {
    if !answer.contains("DSML") {
        return Vec::new();
    }
    let mut calls = Vec::new();
    let mut rest = answer;
    while let Some(at) = rest.find("invoke name=\"") {
        let after = &rest[at + "invoke name=\"".len()..];
        let Some(name_end) = after.find('"') else {
            break;
        };
        let name = &after[..name_end];
        // The call ends at its closing `invoke>` tag (or the end of the answer).
        let body_end = after.find("invoke>").unwrap_or(after.len());
        let mut body = &after[name_end..body_end];
        let mut object = serde_json::Map::new();
        object.insert("tool".into(), serde_json::Value::String(name.into()));
        while let Some(at) = body.find("parameter name=\"") {
            let after = &body[at + "parameter name=\"".len()..];
            let Some(key_end) = after.find('"') else {
                break;
            };
            let key = &after[..key_end];
            let Some(open_end) = after[key_end..].find('>') else {
                break;
            };
            let attributes = &after[key_end..key_end + open_end];
            let value_start = key_end + open_end + 1;
            // The value runs to the `</` of the closing `parameter>` tag.
            let close = after[value_start..]
                .find("parameter>")
                .map_or(after.len(), |i| value_start + i);
            let value_end = after[value_start..close]
                .rfind("</")
                .map_or(close, |i| value_start + i);
            let raw = &after[value_start..value_end];
            let value = match attributes.contains("string=\"false\"") {
                true => serde_json::from_str(raw.trim())
                    .unwrap_or_else(|_| serde_json::Value::String(raw.into())),
                false => serde_json::Value::String(raw.into()),
            };
            object.insert(key.into(), value);
            body = &after[close.min(after.len())..];
        }
        calls.push(serde_json::Value::Object(object).to_string());
        rest = &after[body_end.min(after.len())..];
    }
    calls
}

/// Whether the task itself asks for fetching on the user's machine ("with curl",
/// "locally"): then a fetch is what the user wants, the model's search or not.
pub fn asks_local_fetch(task: &str) -> bool {
    let task = task.to_lowercase();
    [
        "curl",
        "wget",
        "locally",
        "local machine",
        "my machine",
        "this machine",
        "from here",
        "localhost",
        "127.0.0.1",
    ]
    .iter()
    .any(|word| task.contains(word))
}

/// Whether a shell command fetches from a public web address: an `http(s)://` URL
/// with a public host, or a bare host given to a download program (`curl wttr.in`).
/// Local addresses (localhost, private networks, `.local`, one-word hosts) do not count.
pub fn fetches_public_web(cmd: &str) -> bool {
    // The host of `user@host:port/path`; an IPv6 address keeps its brackets.
    let host_of = |text: &str| -> String {
        let authority = text
            .split(['/', '?', '#', '\'', '"', ')', ' '])
            .next()
            .unwrap_or_default();
        let host = authority.rsplit('@').next().unwrap_or_default();
        let host = match host.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or_default(),
            None => host.split(':').next().unwrap_or_default(),
        };
        host.to_lowercase()
    };
    for scheme in ["http://", "https://"] {
        for (at, _) in cmd.match_indices(scheme) {
            if is_public_host(&host_of(&cmd[at + scheme.len()..])) {
                return true;
            }
        }
    }
    const FETCHERS: [&str; 9] = [
        "curl", "wget", "http", "xh", "aria2c", "lynx", "w3m", "links", "elinks",
    ];
    let words: Vec<&str> = cmd
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&' | '(' | ')'))
        .filter(|word| !word.is_empty())
        .collect();
    for (i, word) in words.iter().enumerate() {
        let program = word.rsplit('/').next().unwrap_or(word);
        if !FETCHERS.contains(&program) {
            continue;
        }
        // The arguments up to the next option-free word that is no host: `curl -s wttr.in/x`.
        for arg in words[i + 1..].iter().take(8) {
            let arg = arg.trim_matches(['\'', '"']);
            if arg.starts_with('-') || arg.contains("://") {
                continue;
            }
            let host = host_of(arg);
            if looks_like_domain(&host) && is_public_host(&host) {
                return true;
            }
        }
    }
    false
}

/// `wttr.in`, `api.example.com`; not a file name like `out.json`.
fn looks_like_domain(host: &str) -> bool {
    const FILES: [&str; 16] = [
        "json", "txt", "html", "htm", "xml", "csv", "tar", "gz", "zip", "sh", "py", "log", "md",
        "yaml", "yml", "conf",
    ];
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && labels
            .last()
            .is_some_and(|tld| tld.chars().all(|c| c.is_ascii_alphabetic()) && !FILES.contains(tld))
}

/// A host on the internet: not localhost, a private or link-local network, `.local`
/// or `.internal`, and not a one-word name of the local network.
fn is_public_host(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']);
    if host.is_empty() || !host.contains('.') || host == "::1" {
        return false;
    }
    if [
        "localhost",
        ".localhost",
        ".local",
        ".internal",
        ".lan",
        ".home.arpa",
    ]
    .iter()
    .any(|end| host == end.trim_start_matches('.') || host.ends_with(end))
    {
        return false;
    }
    let octets: Vec<u8> = host
        .split('.')
        .filter_map(|part| part.parse().ok())
        .collect();
    if octets.len() == 4 {
        return !matches!(
            (octets[0], octets[1]),
            (127, _) | (10, _) | (0, _) | (192, 168) | (169, 254) | (172, 16..=31)
        );
    }
    true
}

/// What the model gets when it ends a task with the done call before any
/// action ran: nothing would be on the user's screen.
pub const DONE_TOO_EARLY: &str = "You sent the done call, but no action ran in this task, so the user sees nothing. Run the action the task needs now, or answer in plain text.";

/// Finds the action in an answer of the model.
pub fn parse(answer: &str) -> Parsed {
    let calls = dsml_calls(answer);
    let Some(call) = calls.first() else {
        return Parsed::Final;
    };
    let is_done = serde_json::from_str::<serde_json::Value>(call)
        .is_ok_and(|value| value.get("tool").and_then(|tool| tool.as_str()) == Some("done"));
    if is_done {
        return Parsed::Done;
    }
    match serde_json::from_str::<Action>(call) {
        Ok(action) => Parsed::Action {
            action,
            extra: calls.len() - 1,
        },
        Err(err) => Parsed::Invalid(format!(
            "Your tool call is not valid: {err}. Send one invoke of a known tool with its parameters, inside {CALLS_OPEN} … {CALLS_CLOSE}."
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
    fn one_call() {
        let answer = format!(
            "Let me look.\n\n{}\n",
            dsml(r#"{"tool":"shell","cmd":"ls -la","why":"see"}"#)
        );
        let Parsed::Action { action, extra } = parse(&answer) else {
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
        // The markup the prompt shows, as `dsml` writes it.
        assert_eq!(
            dsml(r#"{"tool":"shell","cmd":"ls","timeout_s":5}"#),
            "<｜DSML｜function_calls>\n<｜DSML｜invoke name=\"shell\">\n<｜DSML｜parameter name=\"cmd\" string=\"true\">ls</｜DSML｜parameter>\n<｜DSML｜parameter name=\"timeout_s\" string=\"false\">5</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜function_calls>"
        );
    }

    #[test]
    fn first_of_several_calls_and_unclosed_call() {
        let answer = "<｜DSML｜function_calls>\n<｜DSML｜invoke name=\"read_file\">\n<｜DSML｜parameter name=\"path\" string=\"true\">a</｜DSML｜parameter>\n</｜DSML｜invoke>\n<｜DSML｜invoke name=\"shell\">\n<｜DSML｜parameter name=\"cmd\" string=\"true\">x";
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
        assert_eq!(dsml_calls(answer)[1], r#"{"cmd":"x","tool":"shell"}"#);
    }

    #[test]
    fn write_file_keeps_its_content_as_it_is() {
        let content = "x\n```\n\"quoted\" <b>tag</b> \\n\n";
        let answer = dsml(
            &serde_json::json!({"tool": "write_file", "path": "a.md", "content": content})
                .to_string(),
        );
        let Parsed::Action { action, .. } = parse(&answer) else {
            panic!()
        };
        assert_eq!(
            action,
            Action::WriteFile {
                path: "a.md".into(),
                content: content.into(),
                why: String::new()
            }
        );
    }

    #[test]
    fn unknown_tool_and_missing_or_wrong_parameters_are_invalid() {
        for answer in [
            dsml(r#"{"tool":"rm","path":"/"}"#),
            dsml(r#"{"tool":"shell"}"#),
            dsml(r#"{"tool":"shell","cmd":"ls","color":"red"}"#),
            dsml(r#"{"tool":"shell","cmd":"ls","timeout_s":"soon"}"#),
        ] {
            let Parsed::Invalid(text) = parse(&answer) else {
                panic!("{answer}")
            };
            assert!(text.starts_with("Your tool call is not valid"));
        }
        // Seconds sent as text still count.
        assert!(matches!(
            parse(&dsml(r#"{"tool":"shell","cmd":"ls","timeout_s":"30"}"#)),
            Parsed::Action {
                action: Action::Shell {
                    timeout_s: Some(30),
                    ..
                },
                ..
            }
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
    fn done_call_ends_without_an_answer() {
        assert_eq!(parse(&dsml(r#"{"tool":"done"}"#)), Parsed::Done);
        assert_eq!(
            parse(&format!(
                "\n\n{}\n",
                dsml(r#"{"tool":"done","why":"shown"}"#)
            )),
            Parsed::Done
        );
        let prompt = first_prompt("t", "/w", true);
        assert!(prompt.contains("Your web search is on"));
        assert!(prompt.contains("- web: query"));
        assert!(!first_prompt("t", "/w", false).contains("- web:"));
        assert!(prompt.contains("<｜DSML｜invoke name=\"shell\">"));
        assert!(prompt.contains("Never fetch from the internet on the user's machine"));
        assert!(first_prompt("t", "/w", false).contains("no web access on your side"));
        assert!(prompt.contains("- done: no parameters"));
        assert!(prompt.contains("do not repeat"));
        assert!(prompt.contains("never the first answer to a task"));
        assert!(next_prompt("t", true).contains("only after the result of an action of this task"));
        assert!(next_prompt("t", true).contains("Web search is on for this task"));
        assert!(next_prompt("t", false).contains("search on your side is off"));
        assert!(next_prompt("t", true).contains("do not fetch from the internet"));
        assert!(next_prompt("t", true).contains(r#"invoke name="web""#));
        assert!(next_prompt("t", false).contains("<｜DSML｜function_calls>"));
        assert!(first_prompt("t", "/w", true).contains("weather"));
        let attached = with_files(next_prompt("t", true), &["a.pdf", "b.png"]);
        assert!(attached.ends_with("a.pdf, b.png\n\nTask: t"));
        assert_eq!(with_files("x".into(), &[]), "x");
    }

    #[test]
    fn public_web_fetches_are_recognized() {
        for cmd in [
            "curl -s 'https://wttr.in/Heidelberg?format=3'",
            "curl -sSL --max-time 20 https://example.org | head -n 40",
            "curl wttr.in/Heidelberg",
            "wget -qO- example.com",
            "python3 -c \"import urllib.request; print(urllib.request.urlopen('https://api.github.com').read())\"",
            "/usr/bin/curl -s http://1.1.1.1/",
        ] {
            assert!(fetches_public_web(cmd), "{cmd}");
        }
        for cmd in [
            "curl -s http://localhost:8080/health",
            "curl http://127.0.0.1:3000/",
            "curl -o out.json http://192.168.1.10/api",
            "wget http://printer.local/status",
            "curl http://myserver/",
            "ls -la /etc",
            "cat notes.md",
            "git remote -v",
            "curl -o result.json -s http://10.0.0.5/x",
        ] {
            assert!(!fetches_public_web(cmd), "{cmd}");
        }
    }

    #[test]
    fn a_task_can_ask_for_a_local_fetch() {
        assert!(asks_local_fetch("curl https://example.org"));
        assert!(asks_local_fetch("fetch the weather locally"));
        assert!(!asks_local_fetch("weather in Heidelberg"));
    }

    #[test]
    fn dsml_spellings_are_read_as_actions() {
        // As the real model sent it (2026-11-12).
        let answer = "\n<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"shell\">\n<｜｜DSML｜｜ parameter name=\"cmd\" string=\"true\">curl -sL 'https://ya.ru' | head -c 2000</｜｜DSML｜｜ parameter>\n<｜｜DSML｜｜ parameter name=\"why\" string=\"true\">fetch ya.ru content</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>";
        assert_eq!(
            parse(answer),
            Parsed::Action {
                action: Action::Shell {
                    cmd: "curl -sL 'https://ya.ru' | head -c 2000".into(),
                    timeout_s: None,
                    why: "fetch ya.ru content".into(),
                },
                extra: 0,
            }
        );
        // Another spelling of the tags, a number, a value over several lines, two calls.
        let answer = "<｜DSML｜function_calls>\n<｜DSML｜invoke name=\"write_file\">\n<｜DSML｜parameter name=\"path\" string=\"true\">a.txt</｜DSML｜parameter>\n<｜DSML｜parameter name=\"content\" string=\"true\">one\ntwo</｜DSML｜parameter>\n</｜DSML｜invoke>\n<｜DSML｜invoke name=\"shell\">\n<｜DSML｜parameter name=\"cmd\" string=\"true\">sleep 1</｜DSML｜parameter>\n<｜DSML｜parameter name=\"timeout_s\" string=\"false\">5</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜function_calls>";
        let Parsed::Action { action, extra } = parse(answer) else {
            panic!()
        };
        assert_eq!(extra, 1);
        assert_eq!(
            action,
            Action::WriteFile {
                path: "a.txt".into(),
                content: "one\ntwo".into(),
                why: String::new(),
            }
        );
        assert_eq!(
            dsml_calls(answer)[1],
            r#"{"cmd":"sleep 1","timeout_s":5,"tool":"shell"}"#
        );
        // The web tool and done.
        let web = "<｜DSML｜invoke name=\"web\"><｜DSML｜parameter name=\"query\" string=\"true\">weather in Heidelberg</｜DSML｜parameter></｜DSML｜invoke>";
        assert!(
            matches!(parse(web), Parsed::Action { action: Action::Web { query, .. }, .. } if query == "weather in Heidelberg")
        );
        assert_eq!(
            parse("<｜DSML｜invoke name=\"done\"></｜DSML｜invoke>"),
            Parsed::Done
        );
        assert_eq!(parse("plain text about DSML"), Parsed::Final);
        assert_eq!(parse("```bash\nls\n```"), Parsed::Final);
    }
}
