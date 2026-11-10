mod auth;
mod cli;
mod config;
mod deepseek;

use anyhow::{Context, Result, bail};
use auth::Auth;
use clap::Parser;
use cli::{Cli, Command};
use config::{Paths, Settings};
use deepseek::client::{Client, TurnOpts};
use deepseek::pow;
use deepseek::session::Conversation;
use futures_util::StreamExt;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::new()?;
    let settings = Settings::load_or_create(&paths)?;

    match cli.command {
        Some(Command::Doctor) => doctor(&paths, &settings).await,
        Some(Command::Login { import, token }) => login(&paths, import, token),
        Some(Command::Logout) => {
            let removed = Auth::remove(&paths)?;
            println!(
                "{}",
                if removed {
                    "logged out"
                } else {
                    "was not logged in"
                }
            );
            Ok(())
        }
        Some(Command::Chat { .. }) => bail!("chat: not implemented yet (TASKS.md #10)"),
        None if cli.run.task.is_empty() => bail!("no task given; see `dsc --help`"),
        None => raw(&paths, &settings, &cli.run).await,
    }
}

fn login(paths: &Paths, import: Option<PathBuf>, token: bool) -> Result<()> {
    let auth = match (import, token) {
        (Some(file), _) => Auth::import(&file)?,
        (None, true) => {
            let token = rpassword::prompt_password("DeepSeek token (hidden): ")?;
            let cookie = rpassword::prompt_password("Cookie header (hidden, Enter = none): ")?;
            Auth::new(token, cookie, None)?
        }
        (None, false) => {
            bail!("browser login: not implemented yet (TASKS.md #11); use --import or --token")
        }
    };
    auth.save(paths)?;
    println!(
        "saved {}; check it with `dsc doctor`",
        paths.auth().display()
    );
    Ok(())
}

fn conversation(paths: &Paths, settings: &Settings) -> Result<Conversation> {
    let auth = Auth::require(paths)?;
    let client = Client::new(&auth, &settings.deepseek)?;
    Ok(Conversation::new(
        client,
        paths.clone(),
        settings.throttle.clone(),
    ))
}

/// Temporary until the stream parser exists: prints the raw SSE of one turn.
async fn raw(paths: &Paths, settings: &Settings, run: &cli::RunArgs) -> Result<()> {
    let mut conversation = conversation(paths, settings)?;
    let opts = TurnOpts {
        model_type: if run.expert {
            "expert".into()
        } else {
            settings.deepseek.model_type.clone()
        },
        thinking: run.think || settings.deepseek.thinking,
        search: run.search || settings.deepseek.search,
    };
    let response = conversation.send(&run.task.join(" "), &opts).await?;
    let mut body = response.bytes_stream();
    let mut out = std::io::stdout().lock();
    while let Some(chunk) = body.next().await {
        out.write_all(&chunk.context("stream broke")?)?;
    }
    Ok(())
}

/// Checks each step of a DeepSeek call without sending a prompt.
async fn doctor(paths: &Paths, settings: &Settings) -> Result<()> {
    let mark = |ok: bool| if ok { "ok" } else { "missing" };
    println!("home      {}", paths.home.display());
    println!(
        "config    {} ({})",
        paths.config().display(),
        mark(paths.config().exists())
    );
    println!(
        "auth      {} ({})",
        paths.auth().display(),
        mark(paths.auth().exists())
    );
    println!(
        "model     {} thinking={} search={}",
        settings.deepseek.model_type, settings.deepseek.thinking, settings.deepseek.search
    );
    println!(
        "throttle  gap {}+0..{} ms, max {}/hour",
        settings.throttle.min_interval_ms,
        settings.throttle.jitter_ms,
        settings.throttle.max_per_hour
    );
    if Auth::load(paths)?.is_none() {
        println!("token     not logged in; run `dsc login`");
        return Ok(());
    }
    let mut conversation = conversation(paths, settings)?;

    let challenge = match conversation.client().create_challenge().await {
        Ok(challenge) => challenge,
        Err(err) => {
            println!("token     FAILED: {err:#}");
            return Ok(());
        }
    };
    println!(
        "token     ok (challenge {}, difficulty {})",
        challenge.algorithm, challenge.difficulty
    );

    let wasm = match conversation.wasm().await {
        Ok(wasm) => wasm.to_vec(),
        Err(err) => {
            println!("wasm      FAILED: {err:#}");
            return Ok(());
        }
    };
    println!("wasm      ok ({} bytes)", wasm.len());

    let started = Instant::now();
    match tokio::task::spawn_blocking(move || pow::solve(&wasm, &challenge)).await? {
        Ok(_) => println!("pow       ok ({} ms)", started.elapsed().as_millis()),
        Err(err) => println!("pow       FAILED: {err:#}"),
    }
    Ok(())
}
