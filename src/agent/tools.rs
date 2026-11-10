//! The actions the model can ask for.

use super::interrupt::{Interrupt, Interrupts};
use anyhow::{Context, Result};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// Output kept in memory per stream; the rest is read and dropped.
const KEEP_BYTES: usize = 4 * 1024 * 1024;
/// How long output of a finished command is still collected (a background child
/// may hold the pipe open).
const DRAIN: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub struct ShellOutput {
    /// `None` = ended by a signal
    pub exit: Option<i32>,
    pub timed_out: bool,
    /// The user stopped it; the command was killed
    pub interrupted: Option<Interrupt>,
    pub stdout: String,
    pub stderr: String,
}

/// Keeps the head and the tail of `text` when it is longer than `max` bytes.
pub fn cap(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let half = max / 2;
    let mut head = half;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - half;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n[... {} bytes cut ...]\n{}",
        &text[..head],
        tail - head,
        &text[tail..]
    )
}

fn collect(
    stream: impl AsyncRead + Unpin + Send + 'static,
) -> (Arc<Mutex<Vec<u8>>>, tokio::task::JoinHandle<()>) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let target = buffer.clone();
    let task = tokio::spawn(async move {
        let mut stream = stream;
        let mut chunk = [0u8; 8192];
        while let Ok(n) = stream.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            let mut kept = target.lock().unwrap();
            let room = KEEP_BYTES.saturating_sub(kept.len());
            kept.extend_from_slice(&chunk[..n.min(room)]);
        }
    });
    (buffer, task)
}

/// Runs `cmd` in bash, in its own process group, without a terminal on stdin.
/// A timeout or an interrupt of the user kills the whole group.
pub async fn shell(
    cmd: &str,
    timeout: Duration,
    interrupts: &mut Interrupts,
) -> Result<ShellOutput> {
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .context("start bash")?;
    let group = child.id().map(|id| id as i32);
    let (out, out_task) = collect(child.stdout.take().context("stdout")?);
    let (err, err_task) = collect(child.stderr.take().context("stderr")?);

    let (mut timed_out, mut interrupted) = (false, None);
    let status = tokio::select! {
        status = child.wait() => Some(status?),
        _ = tokio::time::sleep(timeout) => { timed_out = true; None }
        why = interrupts.next() => { interrupted = Some(why); None }
    };
    let status = match status {
        Some(status) => status,
        None => {
            if let Some(group) = group {
                // SAFETY: plain syscall; the group id is the child we started.
                unsafe { libc::killpg(group, libc::SIGKILL) };
            }
            child.wait().await?
        }
    };
    let _ = tokio::time::timeout(DRAIN, async {
        let _ = out_task.await;
        let _ = err_task.await;
    })
    .await;

    let text =
        |buffer: Arc<Mutex<Vec<u8>>>| String::from_utf8_lossy(&buffer.lock().unwrap()).into_owned();
    Ok(ShellOutput {
        exit: status.code(),
        timed_out,
        interrupted,
        stdout: text(out),
        stderr: text(err),
    })
}

pub fn read_file(path: &str) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {path}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Writes the whole file; missing parent folders are created.
pub fn write_file(path: &str, content: &str) -> Result<()> {
    if let Some(parent) = Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("write {path}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn cap_keeps_head_and_tail() {
        assert_eq!(cap("short", 10), "short");
        let capped = cap(&"a".repeat(50).replace("a", "ab"), 10);
        assert!(
            capped.starts_with("ababa\n[... 90 bytes cut ...]\n"),
            "{capped}"
        );
        assert!(capped.ends_with("babab"));
        // Never cuts inside a character.
        assert!(cap(&"é".repeat(40), 11).contains("bytes cut"));
    }

    #[tokio::test]
    async fn shell_returns_code_and_both_streams() {
        let output = shell(
            "echo out; echo err >&2; exit 3",
            Duration::from_secs(10),
            &mut Interrupts::signal(),
        )
        .await
        .unwrap();
        assert_eq!(output.exit, Some(3));
        assert_eq!(
            (output.stdout.as_str(), output.stderr.as_str()),
            ("out\n", "err\n")
        );
        assert!(!output.timed_out);
    }

    #[tokio::test]
    async fn timeout_kills_the_whole_group() {
        let started = Instant::now();
        let output = shell(
            "echo begin; sleep 30 | sleep 30",
            Duration::from_millis(300),
            &mut Interrupts::signal(),
        )
        .await
        .unwrap();
        assert!(output.timed_out);
        assert_eq!(output.exit, None);
        assert_eq!(output.stdout, "begin\n");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn background_child_does_not_block() {
        let started = Instant::now();
        let output = shell(
            "sleep 20 & echo done",
            Duration::from_secs(10),
            &mut Interrupts::signal(),
        )
        .await
        .unwrap();
        assert_eq!(output.stdout, "done\n");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn interrupt_kills_the_command() {
        let (sender, mut interrupts) = Interrupts::channel();
        sender.send(Interrupt::Stop).unwrap();
        let started = Instant::now();
        let output = shell("sleep 30", Duration::from_secs(60), &mut interrupts)
            .await
            .unwrap();
        assert_eq!(output.interrupted, Some(Interrupt::Stop));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn write_then_read_creates_folders() {
        let dir = std::env::temp_dir().join(format!("dsc-tools-{}", std::process::id()));
        let file = dir.join("a/b.txt");
        write_file(file.to_str().unwrap(), "hello").unwrap();
        assert_eq!(read_file(file.to_str().unwrap()).unwrap(), "hello");
        assert!(read_file(dir.join("nope").to_str().unwrap()).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
