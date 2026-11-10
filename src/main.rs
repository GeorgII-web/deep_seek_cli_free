mod agent;
mod auth;
mod backend;
mod cli;
mod config;
mod deepseek;
mod ui;

use agent::interrupt::Interrupt;
use agent::protocol;
use anyhow::{Result, bail};
use auth::Auth;
use clap::Parser;
use cli::{Cli, Command};
use config::{Paths, Settings};
use deepseek::client::{Client, TurnOpts};
use deepseek::pow;
use deepseek::session::Conversation;
use std::collections::HashSet;
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
        Some(Command::Chat) => chat(&paths, &settings, &cli::RunArgs::default()).await,
        None if cli.run.task.is_empty() => chat(&paths, &settings, &cli.run).await,
        None => {
            let mut conversation = conversation(&paths, &settings, &cli.run)?;
            let prompt = protocol::first_prompt(&cli.run.task.join(" "), &cwd());
            let mut terminal = ui::Terminal::new(None, cli.run.yes);
            let mode = terminal.mode();
            let mut interrupts = terminal.interrupts();
            let mut always = HashSet::new();
            terminal.begin();
            let result = agent::run::run(
                &mut conversation,
                &mut terminal,
                prompt,
                &settings,
                &mode,
                &mut always,
                &mut interrupts,
            )
            .await;
            terminal.finish(&result);
            // The finish line already showed the error; the terminal must be restored before the exit.
            drop(terminal);
            match result {
                Ok(()) => Ok(()),
                Err(err) if err.is::<Interrupt>() => std::process::exit(130),
                Err(_) => std::process::exit(1),
            }
        }
    }
}

fn cwd() -> String {
    std::env::current_dir()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default()
}

/// Interactive mode: every input is a task in the same conversation, so the model
/// remembers the earlier ones. What the user allowed with "always" holds until the exit.
async fn chat(paths: &Paths, settings: &Settings, run: &cli::RunArgs) -> Result<()> {
    let mut conversation = conversation(paths, settings, run)?;
    let mut terminal = ui::Terminal::new(Some(paths.history()), run.yes);
    let mode = terminal.mode();
    let mut interrupts = terminal.interrupts();
    let mut always = HashSet::new();
    let mut first = true;
    let model = match (run.expert, run.think || settings.deepseek.thinking) {
        (true, _) => "expert".to_string(),
        (false, true) => format!("{} + thinking", settings.deepseek.model_type),
        (false, false) => settings.deepseek.model_type.clone(),
    };
    terminal.open_chat(&model, &cwd());
    while let Some(line) = terminal.input() {
        let prompt = match line.as_str() {
            "" => continue,
            "/exit" | "/quit" => break,
            "/help" => {
                terminal.help();
                continue;
            }
            "/mode" => {
                terminal.toggle_mode();
                continue;
            }
            "/new" => {
                conversation = self::conversation(paths, settings, run)?;
                first = true;
                terminal.info("New conversation: the model starts from zero.");
                continue;
            }
            // A path like /etc/hosts is a task, a single word after the slash a command.
            command if command.starts_with('/') && !command[1..].contains(['/', ' ']) => {
                terminal.error(&format!("unknown command `{command}`; see `/help`"));
                continue;
            }
            task if first => protocol::first_prompt(task, &cwd()),
            task => protocol::next_prompt(task),
        };
        first = false;
        terminal.begin();
        // A failed or stopped task does not end the chat; Ctrl-C twice does.
        let result = agent::run::run(
            &mut conversation,
            &mut terminal,
            prompt,
            settings,
            &mode,
            &mut always,
            &mut interrupts,
        )
        .await;
        terminal.finish(&result);
        if matches!(
            result.as_ref().err().and_then(|err| err.downcast_ref()),
            Some(Interrupt::Quit)
        ) {
            break;
        }
    }
    Ok(())
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

fn conversation(paths: &Paths, settings: &Settings, run: &cli::RunArgs) -> Result<Conversation> {
    let auth = Auth::require(paths)?;
    let client = Client::new(&auth, &settings.deepseek)?;
    let opts = TurnOpts {
        model_type: match run.expert {
            true => "expert".into(),
            false => settings.deepseek.model_type.clone(),
        },
        thinking: run.think || settings.deepseek.thinking,
        search: run.search || settings.deepseek.search,
    };
    Ok(Conversation::new(
        client,
        paths.clone(),
        settings.throttle.clone(),
        opts,
    ))
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
    let mut conversation = conversation(paths, settings, &cli::RunArgs::default())?;

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
