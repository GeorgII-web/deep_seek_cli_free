//! Files attached to a prompt. A file dropped on the terminal arrives as a paste
//! of its path; `from_paste` turns such a paste into attachments, `warning` flags
//! files that look private (keys, passwords, tokens) before they leave the machine.
//! The upload itself is the backend's (`Backend::attach`).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Most files on one prompt, and the largest file: the limits of the DeepSeek web page.
pub const MAX_FILES: usize = 50;
pub const MAX_BYTES: u64 = 100 * 1024 * 1024;
/// How much of a file is searched for secrets.
const SCAN_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub path: PathBuf,
    /// File name without the folder: what DeepSeek and the screen get
    pub name: String,
    pub size: u64,
    /// Why the file looks private; `None` = nothing found
    pub warning: Option<String>,
}

impl Attachment {
    /// `report.pdf · 120 KB`
    pub fn label(&self) -> String {
        format!("{} · {}", self.name, size_label(self.size))
    }
}

/// What a paste of dropped files gave.
#[derive(Debug, Default, PartialEq)]
pub struct Dropped {
    pub files: Vec<Attachment>,
    /// Files that cannot be attached, with the reason
    pub skipped: Vec<String>,
}

/// The paste as dropped files: `Some` only when every word of it is an absolute
/// path (or a `file://` URI) of an existing regular file. Anything else, a
/// relative name or a folder among them, is ordinary text.
pub fn from_paste(text: &str) -> Option<Dropped> {
    // Dropped paths are short; a long paste is text.
    if text.len() > 64 * 1024 {
        return None;
    }
    let words = split_words(text.trim());
    if words.is_empty() {
        return None;
    }
    let paths: Vec<PathBuf> = words
        .iter()
        .map(|word| to_local(word))
        .collect::<Option<_>>()?;
    if !paths.iter().all(|path| path.is_file()) {
        return None;
    }
    let mut dropped = Dropped::default();
    for path in paths {
        match load(&path) {
            Ok(file) => dropped.files.push(file),
            Err(why) => dropped.skipped.push(why),
        }
    }
    Some(dropped)
}

/// The file's name, size and warning; an error text when it cannot be attached.
pub fn load(path: &Path) -> Result<Attachment, String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let size = fs::metadata(path)
        .map_err(|err| format!("{name}: {err}"))?
        .len();
    if size == 0 {
        return Err(format!("{name}: empty file"));
    }
    if size > MAX_BYTES {
        return Err(format!(
            "{name}: {} is over the limit of {}",
            size_label(size),
            size_label(MAX_BYTES)
        ));
    }
    let mut head = Vec::new();
    if let Ok(file) = fs::File::open(path) {
        let _ = file.take(SCAN_BYTES as u64).read_to_end(&mut head);
    }
    Ok(Attachment {
        warning: warning(path, &head),
        path: path.to_path_buf(),
        name,
        size,
    })
}

/// Splits a paste into words like a shell: blanks separate, `'…'` and `"…"`
/// group, a backslash escapes the next character (outside single quotes).
/// Windows paths (`C:\…`) keep their backslashes.
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut chars = text.chars().peekable();
    // A word that starts like `C:\` is a Windows path: backslashes are separators there.
    let windows = |rest: &str| {
        let b = rest.as_bytes();
        b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\'
    };
    let mut in_windows = false;
    while let Some(c) = chars.next() {
        if !started {
            let look: String = std::iter::once(c).chain(chars.clone().take(4)).collect();
            in_windows = windows(look.trim_start_matches(['"', '\'']));
        }
        match c {
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    word.push(c);
                }
            }
            '"' => {
                started = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' if !in_windows && matches!(chars.peek(), Some('"' | '\\')) => {
                            word.extend(chars.next());
                        }
                        c => word.push(c),
                    }
                }
            }
            '\\' if !in_windows => {
                started = true;
                word.extend(chars.next());
            }
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// A dropped word as a path on this machine: an absolute path, a `file://` URI,
/// or a Windows path (`C:\Users\…` → `/mnt/c/Users/…`, as WSL mounts it).
/// `None` for anything relative.
fn to_local(word: &str) -> Option<PathBuf> {
    if let Some(rest) = word.strip_prefix("file://") {
        // `file:///home/…` or `file://localhost/home/…`
        let path = rest.strip_prefix("localhost").unwrap_or(rest);
        return path
            .starts_with('/')
            .then(|| PathBuf::from(percent_decode(path)));
    }
    if word.starts_with('/') {
        return Some(PathBuf::from(word));
    }
    let b = word.as_bytes();
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
        let drive = (b[0] as char).to_ascii_lowercase();
        return Some(PathBuf::from(format!(
            "/mnt/{drive}/{}",
            word[3..].replace('\\', "/")
        )));
    }
    None
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Why a file looks private, from its path and its first bytes; `None` = nothing found.
/// A warning, not a block: the user decides.
pub fn warning(path: &Path, head: &[u8]) -> Option<String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    const FOLDERS: [&str; 7] = [
        ".ssh",
        ".gnupg",
        ".aws",
        ".kube",
        ".docker",
        "gcloud",
        ".password-store",
    ];
    for part in path.components() {
        let part = part.as_os_str().to_string_lossy();
        if FOLDERS.contains(&part.as_ref()) {
            return Some(format!("from the private folder {part}"));
        }
    }
    const NAMES: [&str; 11] = [
        ".env",
        ".netrc",
        ".pgpass",
        ".git-credentials",
        ".npmrc",
        ".pypirc",
        ".htpasswd",
        "credentials",
        "shadow",
        "gshadow",
        "auth.json",
    ];
    // SSH private keys; the `.pub` half is public.
    const KEY_FILES: [&str; 4] = ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];
    if NAMES.contains(&name.as_str())
        || name.starts_with(".env.")
        || KEY_FILES.iter().any(|key| name.starts_with(key)) && !name.ends_with(".pub")
    {
        return Some(format!("{name} usually holds secrets"));
    }
    const EXTENSIONS: [&str; 9] = [
        "pem", "key", "p12", "pfx", "jks", "keystore", "kdbx", "ppk", "ovpn",
    ];
    if let Some(ext) = Path::new(&name)
        .extension()
        .map(|ext| ext.to_string_lossy())
        && EXTENSIONS.contains(&ext.as_ref())
    {
        return Some(format!("a .{ext} file usually holds a key or passwords"));
    }
    const WORDS: [&str; 6] = [
        "secret",
        "password",
        "passwd",
        "credential",
        "private",
        "token",
    ];
    if let Some(word) = WORDS.iter().find(|word| name.contains(*word)) {
        return Some(format!("the name says \"{word}\""));
    }
    content_warning(head)
}

/// Secrets in the text itself: private keys, known token shapes, `password = …`.
fn content_warning(head: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(head);
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY") {
        return Some("contains a private key".into());
    }
    let tokens: [(&str, usize, &str); 6] = [
        ("AKIA", 16, "an AWS access key"),
        ("ghp_", 30, "a GitHub token"),
        ("github_pat_", 30, "a GitHub token"),
        ("xoxb-", 20, "a Slack token"),
        ("xoxp-", 20, "a Slack token"),
        ("sk-", 32, "an API key"),
    ];
    for (prefix, length, what) in tokens {
        for (at, _) in text.match_indices(prefix) {
            let before = text[..at].chars().next_back();
            let run = text[at + prefix.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                .count();
            if run >= length && !before.is_some_and(|c| c.is_ascii_alphanumeric()) {
                return Some(format!("contains {what}"));
            }
        }
    }
    let lower = text.to_lowercase();
    const KEYS: [&str; 9] = [
        "password",
        "passwd",
        "pwd",
        "secret",
        "api_key",
        "apikey",
        "access_key",
        "token",
        "private_key",
    ];
    for key in KEYS {
        for (at, _) in lower.match_indices(key) {
            let rest = lower[at + key.len()..].trim_start_matches(['"', '\'', ' ', '\t']);
            let Some(rest) = rest.strip_prefix([':', '=']) else {
                continue;
            };
            let value = rest.trim_start_matches([' ', '\t', '"', '\'']);
            // A value, not an empty field or a placeholder of a template.
            if value
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace() && !matches!(c, '<' | '$' | '{' | ','))
            {
                return Some(format!("contains a {key} value"));
            }
        }
    }
    None
}

/// The content type the upload declares, from the file name.
pub fn mime(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "doc" => "application/msword",
        "xls" => "application/vnd.ms-excel",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "txt" | "log" | "ini" | "cfg" | "conf" | "toml" | "yaml" | "yml" | "rs" | "py" | "js"
        | "ts" | "php" | "go" | "java" | "c" | "h" | "cpp" | "sh" | "sql" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// `512 B`, `12 KB`, `3.4 MB`
pub fn size_label(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    match bytes {
        b if b < KB => format!("{b} B"),
        b if b < MB => format!("{} KB", b.div_ceil(KB)),
        b => format!("{:.1} MB", b as f64 / MB as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_like_a_shell() {
        assert_eq!(split_words("/a/b /c/d"), ["/a/b", "/c/d"]);
        assert_eq!(
            split_words("'/mnt/c/My Files/x.pdf' \"/tmp/a b\""),
            ["/mnt/c/My Files/x.pdf", "/tmp/a b"]
        );
        assert_eq!(split_words(r"/tmp/a\ b.txt"), ["/tmp/a b.txt"]);
        assert_eq!(
            split_words(r#""C:\Users\Me\a b.txt" C:\x.log"#),
            [r"C:\Users\Me\a b.txt", r"C:\x.log"]
        );
        assert!(split_words("   ").is_empty());
    }

    #[test]
    fn local_paths() {
        assert_eq!(to_local("/etc/hosts"), Some(PathBuf::from("/etc/hosts")));
        assert_eq!(
            to_local(r"C:\Users\Me\a b.txt"),
            Some(PathBuf::from("/mnt/c/Users/Me/a b.txt"))
        );
        assert_eq!(
            to_local("file:///tmp/a%20b.txt"),
            Some(PathBuf::from("/tmp/a b.txt"))
        );
        assert_eq!(
            to_local("file://localhost/tmp/x"),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(to_local("Cargo.toml"), None);
        assert_eq!(to_local("./x"), None);
    }

    #[test]
    fn a_paste_of_existing_files_becomes_attachments() {
        let dir = std::env::temp_dir().join(format!("dsc-attach-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a b.txt"), dir.join("notes.md"));
        fs::write(&a, "hello\n").unwrap();
        fs::write(&b, "# notes\n").unwrap();
        let paste = format!("'{}' {}", a.display(), b.display());
        let dropped = from_paste(&paste).unwrap();
        assert_eq!(dropped.files.len(), 2);
        assert_eq!(dropped.files[0].name, "a b.txt");
        assert_eq!(dropped.files[0].size, 6);
        assert_eq!(dropped.files[0].warning, None);
        // Text, a missing file, a folder, a relative name: plain text.
        assert_eq!(from_paste("hello world"), None);
        assert_eq!(from_paste(&format!("{} /no/such/file", b.display())), None);
        assert_eq!(from_paste(&dir.display().to_string()), None);
        assert_eq!(from_paste("Cargo.toml"), None);
        // An empty file is skipped with a reason.
        let empty = dir.join("empty.txt");
        fs::write(&empty, "").unwrap();
        let dropped = from_paste(&empty.display().to_string()).unwrap();
        assert!(dropped.files.is_empty());
        assert!(dropped.skipped[0].contains("empty"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn private_files_are_flagged() {
        let flag = |path: &str, text: &str| warning(Path::new(path), text.as_bytes());
        assert!(flag("/home/u/.ssh/config", "").unwrap().contains(".ssh"));
        assert!(flag("/p/.env", "").is_some());
        assert!(flag("/p/.env.local", "").is_some());
        assert!(flag("/home/u/id_ed25519", "").is_some());
        assert!(flag("/home/u/id_rsa", "").is_some());
        assert!(flag("/tmp/server.pem", "").is_some());
        assert!(flag("/tmp/db.kdbx", "").is_some());
        assert!(flag("/tmp/my-passwords.txt", "").is_some());
        assert!(flag("/tmp/a.txt", "-----BEGIN OPENSSH PRIVATE KEY-----\nabc").is_some());
        assert!(
            flag("/tmp/a.txt", "key=AKIAABCDEFGHIJKLMNOP")
                .unwrap()
                .contains("AWS")
        );
        assert!(
            flag("/tmp/app.ini", "[db]\npassword = hunter2\n")
                .unwrap()
                .contains("password")
        );
        assert!(flag("/tmp/c.json", r#"{"api_key": "abc123"}"#).is_some());
        // Ordinary files and templates are not.
        assert_eq!(flag("/tmp/report.pdf", "%PDF-1.7"), None);
        assert_eq!(
            flag("/tmp/main.rs", "fn main() { let password_len = 3; }"),
            None
        );
        assert!(flag("/tmp/.env.example", "PASSWORD=${DB_PASSWORD}\n").is_some());
        assert_eq!(flag("/tmp/tpl.yml", "password: <set me>\n"), None);
        assert_eq!(flag("/home/u/id_rsa.pub", "ssh-rsa AAAA"), None);
        assert_eq!(
            flag("/home/u/keys/id_ed25519.pub", "ssh-ed25519 AAAA"),
            None
        );
    }

    #[test]
    fn labels_and_types() {
        assert_eq!(size_label(512), "512 B");
        assert_eq!(size_label(12 * 1024), "12 KB");
        assert_eq!(size_label(3 * 1024 * 1024 + 400_000), "3.4 MB");
        assert_eq!(mime("a.PDF"), "application/pdf");
        assert_eq!(mime("x.unknown"), "application/octet-stream");
    }
}
