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
use config::{Mode, Paths, Settings};
use deepseek::client::{ApiError, Client, TurnOpts};
use deepseek::pow;
use deepseek::session::Conversation;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
            let mut terminal =
                ui::Terminal::new(None, cli.run.yes || settings.policy.mode == Mode::AllowAll);
            let mode = terminal.mode();
            let mut saver = ModeSaver::new(&paths, &settings, mode.clone());
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
            saver.sync();
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

/// Writes the approval mode to `config.toml` whenever the user switched it, so
/// the next start uses it. A mode that only comes from `--yes` is not saved.
struct ModeSaver<'a> {
    paths: &'a Paths,
    settings: Settings,
    flag: Arc<AtomicBool>,
    /// The value at the start or at the last save
    last: bool,
}

impl<'a> ModeSaver<'a> {
    fn new(paths: &'a Paths, settings: &Settings, flag: Arc<AtomicBool>) -> Self {
        let last = flag.load(Ordering::Relaxed);
        Self {
            paths,
            settings: settings.clone(),
            flag,
            last,
        }
    }

    fn sync(&mut self) {
        let now = self.flag.load(Ordering::Relaxed);
        if now == self.last {
            return;
        }
        self.last = now;
        self.settings.policy.mode = if now { Mode::AllowAll } else { Mode::Ask };
        if let Err(err) = self.settings.save(self.paths) {
            eprintln!("dsc: the mode was not saved: {err:#}");
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
    let mut terminal = ui::Terminal::new(
        Some(paths.history()),
        run.yes || settings.policy.mode == Mode::AllowAll,
    );
    if Auth::load(paths)?.is_none()
        && relogin(paths, settings, &mut terminal, "You are not logged in yet.")?.is_none()
    {
        bail!("not logged in; run `dsc login`");
    }
    // One cheap call tells whether the saved token still works; a dead one opens the login window now.
    let token_note = check_token(paths, settings, &mut terminal).await?;
    let mut conversation = conversation(paths, settings, run)?;
    let mode = terminal.mode();
    let mut saver = ModeSaver::new(paths, settings, mode.clone());
    let mut interrupts = terminal.interrupts();
    let mut always = HashSet::new();
    let mut first = true;
    let model = match (run.expert, run.think || settings.deepseek.thinking) {
        (true, _) => "expert".to_string(),
        (false, true) => format!("{} + thinking", settings.deepseek.model_type),
        (false, false) => settings.deepseek.model_type.clone(),
    };
    terminal.open_chat(&model, &cwd());
    if let Some(note) = token_note {
        terminal.error(&note);
    }
    loop {
        let line = terminal.input();
        // Shift+Tab in the box may have switched the mode.
        saver.sync();
        let Some(line) = line else {
            break;
        };
        let prompt = match line.as_str() {
            "" => continue,
            "/exit" | "/quit" => break,
            "/help" => {
                terminal.help();
                continue;
            }
            "/mode" => {
                terminal.toggle_mode();
                saver.sync();
                continue;
            }
            "/login" => {
                match relogin(
                    paths,
                    settings,
                    &mut terminal,
                    "Enter a new DeepSeek token.",
                ) {
                    Ok(Some(client)) => {
                        conversation.set_client(client);
                        terminal.info("Token saved. Check it with a task or `dsc doctor`.");
                    }
                    Ok(None) => terminal.info("Cancelled: the token was not changed."),
                    Err(err) => terminal.error(&format!("{err:#}")),
                }
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
        let mut quit = false;
        // A failed or stopped task does not end the chat; Ctrl-C twice does. A dead
        // token opens the login window and, with a new token, the task runs again.
        loop {
            terminal.begin();
            let result = agent::run::run(
                &mut conversation,
                &mut terminal,
                prompt.clone(),
                settings,
                &mode,
                &mut always,
                &mut interrupts,
            )
            .await;
            terminal.finish(&result);
            saver.sync();
            let error = result.as_ref().err();
            if matches!(
                error.and_then(|err| err.downcast_ref()),
                Some(Interrupt::Quit)
            ) {
                quit = true;
                break;
            }
            if !matches!(
                error.and_then(|err| err.downcast_ref()),
                Some(ApiError::TokenDead(_))
            ) {
                break;
            }
            match relogin(
                paths,
                settings,
                &mut terminal,
                "The token is dead or expired.",
            ) {
                Ok(Some(client)) => {
                    conversation.set_client(client);
                    terminal.info("Token saved; running the task again.");
                }
                Ok(None) => {
                    terminal.info("No new token. Use `/login` when you have one.");
                    break;
                }
                Err(err) => {
                    terminal.error(&format!("{err:#}"));
                    break;
                }
            }
        }
        if quit {
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
            // The token window; without a terminal there is nothing to show it on.
            let mut terminal = ui::Terminal::new(None, false);
            match terminal.token_dialog("Enter your DeepSeek token.") {
                Some(token) => Auth::new(token, String::new(), None)?,
                None => bail!(
                    "no token entered; in a script use `dsc login --token` or `--import <file>`"
                ),
            }
        }
    };
    auth.save(paths)?;
    println!(
        "saved {}; check it with `dsc doctor`",
        paths.auth().display()
    );
    Ok(())
}

/// Checks the saved token against DeepSeek before the chat starts (the same call
/// as `doctor`: no prompt is sent, it is not throttled). A dead token opens the
/// login window until a token works or the user cancels. Returns a note for the
/// user when the chat starts without a working token.
async fn check_token(
    paths: &Paths,
    settings: &Settings,
    terminal: &mut ui::Terminal,
) -> Result<Option<String>> {
    loop {
        let client = Client::new(&Auth::require(paths)?, &settings.deepseek)?;
        let Err(err) = client.create_challenge().await else {
            return Ok(None);
        };
        if !matches!(err.downcast_ref(), Some(ApiError::TokenDead(_))) {
            // No network, a block page: not a reason to ask for a token.
            return Ok(Some(format!("the token could not be checked: {err:#}")));
        }
        if relogin(
            paths,
            settings,
            terminal,
            "The saved token is dead or expired.",
        )?
        .is_none()
        {
            return Ok(Some(
                "the token is dead: tasks will fail until you enter a new one with `/login`".into(),
            ));
        }
    }
}

/// Asks for a new token in the login window, saves it and returns a client that
/// uses it. `None` = the user cancelled. The token is written to `auth.json` only.
fn relogin(
    paths: &Paths,
    settings: &Settings,
    terminal: &mut ui::Terminal,
    reason: &str,
) -> Result<Option<Client>> {
    let Some(token) = terminal.token_dialog(reason) else {
        return Ok(None);
    };
    // The proof-of-work URL of the old login still applies.
    let wasm_url = Auth::load(paths)
        .ok()
        .flatten()
        .and_then(|old| old.wasm_url);
    let auth = Auth::new(token, String::new(), wasm_url)?;
    auth.save(paths)?;
    Ok(Some(Client::new(&auth, &settings.deepseek)?))
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
