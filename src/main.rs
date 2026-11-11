mod agent;
mod auth;
mod backend;
mod cli;
mod config;
mod deepseek;
mod mock;
mod model;
mod sessions;
mod ui;

use agent::interrupt::Interrupt;
use agent::protocol;
use anyhow::{Result, bail};
use auth::Auth;
use clap::Parser;
use cli::{Cli, Command};
use config::{Api, Mode, Paths, Settings};
use deepseek::client::{ApiError, Client, TurnOpts};
use deepseek::pow;
use deepseek::session::Conversation;
use model::Model;
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
            let mut conversation = model(&paths, &settings, &cli.run, settings.deepseek.api)?;
            let task = cli.run.task.join(" ");
            // With `--session` the task continues that conversation, which has the rules already.
            let prompt = match &cli.run.session {
                Some(code) => {
                    let store = sessions::Store::load(&paths);
                    let Some(chosen) = store.sessions.iter().find(|session| session.id == *code)
                    else {
                        bail!("no session `{code}` here; `dsc` then `/sessions` lists them");
                    };
                    conversation.resume(chosen.id.clone(), chosen.parent_message_id);
                    protocol::next_prompt(&task)
                }
                None => protocol::first_prompt(&task, &cwd(), web_search(&settings, &cli.run)),
            };
            let mut terminal =
                ui::Terminal::new(None, cli.run.yes || settings.policy.mode == Mode::AllowAll);
            terminal.set_model(settings.deepseek.api == Api::Mock);
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

    /// Saves which model answers (`/model`), keeping the mode of the same file.
    fn save_api(&mut self, api: Api) {
        self.sync();
        self.settings.deepseek.api = api;
        if let Err(err) = self.settings.save(self.paths) {
            eprintln!("dsc: the model choice was not saved: {err:#}");
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
    // Which model answers: the saved choice of `/model`. The mock needs no login.
    let mut api = settings.deepseek.api;
    let token_note = match api {
        Api::Deepseek => ensure_login(paths, settings, &mut terminal).await?,
        Api::Mock => None,
    };
    let mut conversation = model(paths, settings, run, api)?;
    let mode = terminal.mode();
    let mut saver = ModeSaver::new(paths, settings, mode.clone());
    let mut interrupts = terminal.interrupts();
    let mut always = HashSet::new();
    let mut first = true;
    // The conversations that can be continued, and the entry of the one in use.
    let mut store = sessions::Store::load(paths);
    let mut current: Option<sessions::Session> = None;
    terminal.set_model(api == Api::Mock);
    terminal.set_mouse(settings.ui.mouse_scroll);
    terminal.open_chat(&api_label(api, settings, run), &cwd());
    if api == Api::Mock {
        terminal.info(&mock_help());
    }
    if let Some(note) = token_note {
        terminal.error(&note);
    }
    terminal.set_title("dsc");
    // `--session=<code>`: start in that conversation, with its screen as it was left.
    if let Some(code) = &run.session {
        match store
            .sessions
            .iter()
            .find(|session| session.id == *code)
            .cloned()
        {
            Some(_) if api == Api::Mock => {
                terminal.error("sessions belong to the DeepSeek model; switch with `/model` first")
            }
            Some(chosen) => {
                conversation.resume(chosen.id.clone(), chosen.parent_message_id);
                first = false;
                let saved =
                    std::fs::read_to_string(paths.transcript(&chosen.id)).unwrap_or_default();
                terminal.show_output(&saved);
                terminal.info(&format!(
                    "Continuing `{}`. The model remembers that conversation.",
                    chosen.title
                ));
                terminal.set_title(&format!("dsc · {}", chosen.title));
                current = Some(chosen);
            }
            None => terminal.error(&format!("no session `{code}` here; `/sessions` lists them")),
        }
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
            "/permissions" => {
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
            "/model" => {
                let rows: Vec<String> = [
                    (
                        Api::Deepseek,
                        "DeepSeek   the real account: your token, every task counts against it",
                    ),
                    (
                        Api::Mock,
                        "Mock       fake answers inside the app: no network, no account",
                    ),
                ]
                .iter()
                .map(|(kind, text)| format!("{} {text}", if *kind == api { "●" } else { " " }))
                .collect();
                let chosen = match terminal.pick("Model", &rows) {
                    Some(0) => Api::Deepseek,
                    Some(_) => Api::Mock,
                    None => continue,
                };
                if chosen == api {
                    continue;
                }
                // The real account needs a working token before the switch.
                let note = match chosen {
                    Api::Deepseek => match ensure_login(paths, settings, &mut terminal).await {
                        Ok(note) => note,
                        Err(err) => {
                            terminal.error(&format!("{err:#}; still on the mock"));
                            continue;
                        }
                    },
                    Api::Mock => None,
                };
                api = chosen;
                terminal.set_model(api == Api::Mock);
                conversation = self::model(paths, settings, run, api)?;
                first = true;
                current = None;
                terminal.show_output("");
                terminal.set_title("dsc");
                saver.save_api(api);
                match api {
                    Api::Deepseek => terminal
                        .info("Model `DeepSeek`: the real account. A new conversation starts."),
                    Api::Mock => terminal.info(&mock_help()),
                }
                if let Some(note) = note {
                    terminal.error(&note);
                }
                continue;
            }
            "/sessions" if conversation.is_mock() => {
                terminal.info("The mock has no sessions. `/model` switches to DeepSeek.");
                continue;
            }
            "/sessions" => {
                if store.sessions.is_empty() {
                    terminal
                        .info("No earlier conversations yet: one is saved after its first task.");
                    continue;
                }
                let rows = store.rows(
                    sessions::now(),
                    current.as_ref().map(|session| session.id.as_str()),
                );
                if let Some(chosen) = terminal
                    .pick("Sessions", &rows)
                    .and_then(|index| store.sessions.get(index).cloned())
                {
                    // The messages live at DeepSeek: the next task continues below the last answer.
                    conversation = self::model(paths, settings, run, api)?;
                    conversation.resume(chosen.id.clone(), chosen.parent_message_id);
                    first = false;
                    // The screen as it was when that conversation was left.
                    let saved =
                        std::fs::read_to_string(paths.transcript(&chosen.id)).unwrap_or_default();
                    terminal.show_output(&saved);
                    terminal.info(&format!(
                        "Switched to `{}` ({} task{}, {}). The model remembers that conversation.",
                        chosen.title,
                        chosen.tasks,
                        if chosen.tasks == 1 { "" } else { "s" },
                        sessions::age(sessions::now(), chosen.updated)
                    ));
                    terminal.set_title(&format!("dsc · {}", chosen.title));
                    current = Some(chosen);
                }
                continue;
            }
            "/new" => {
                conversation = self::model(paths, settings, run, api)?;
                first = true;
                current = None;
                // A new conversation starts on a clean screen; the old one is saved with its session.
                terminal.show_output("");
                terminal.set_title("dsc");
                terminal
                    .info("New conversation: the model starts from zero. `/sessions` goes back.");
                continue;
            }
            // A path like /etc/hosts is a task, a single word after the slash a command.
            command if command.starts_with('/') && !command[1..].contains(['/', ' ']) => {
                terminal.error(&format!("unknown command `{command}`; see `/help`"));
                continue;
            }
            task if first => protocol::first_prompt(task, &cwd(), web_search(settings, run)),
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
            if conversation.is_mock() {
                // The mock's `dead` task shows the window; a typed token is thrown away.
                let _ = terminal
                    .token_dialog("Mock: this is how a dead token looks. Nothing is saved.");
                terminal.info("Mock: the token window was shown; nothing was saved.");
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
        // Remember where this conversation stands, so `/sessions` can come back to it.
        if let Some((id, parent_message_id)) = conversation.state() {
            let entry = current.get_or_insert_with(|| sessions::Session {
                id: id.clone(),
                parent_message_id: None,
                title: sessions::title(&line),
                cwd: cwd(),
                tasks: 0,
                updated: 0,
            });
            entry.id = id;
            entry.parent_message_id = parent_message_id;
            entry.tasks += 1;
            entry.updated = sessions::now();
            store.upsert(entry.clone());
            terminal.set_title(&format!("dsc · {}", entry.title));
            // The screen of this conversation, so a later switch can show it again.
            if let Some(output) = terminal.output() {
                let file = paths.transcript(&entry.id);
                let _ = file.parent().map(std::fs::create_dir_all);
                let _ = std::fs::write(file, output);
            }
        }
        if quit {
            break;
        }
    }
    // After the screen is given back: how to come back to exactly this conversation.
    let resume = current.map(|session| session.id);
    drop(terminal);
    if let Some(code) = resume {
        println!("\nContinue this conversation:\n  dsc --session={code}");
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

/// What the mock is and the tasks it understands: shown when the chat starts on
/// the mock and when `/model` switches to it.
fn mock_help() -> String {
    format!(
        "Model `Mock`: fake answers inside the app, no network, no account. `/model` switches to DeepSeek.\n\
         Its speed is set in `config.toml` under `[mock]` (`wait_ms`, `word_ms`). Tasks it understands:\n{}",
        mock::TASKS
    )
}

/// Whether DeepSeek's own web search is on for this run: the config, unless a flag says otherwise.
fn web_search(settings: &Settings, run: &cli::RunArgs) -> bool {
    (settings.deepseek.search || run.search) && !run.no_search
}

/// The model for a new conversation.
fn model(paths: &Paths, settings: &Settings, run: &cli::RunArgs, api: Api) -> Result<Model> {
    Ok(match api {
        Api::Deepseek => Model::Deepseek(Box::new(conversation(paths, settings, run)?)),
        Api::Mock => Model::Mock(mock::Mock::new(&settings.mock)),
    })
}

/// What the banner says about the model in use.
fn api_label(api: Api, settings: &Settings, run: &cli::RunArgs) -> String {
    match (api, run.expert, run.think || settings.deepseek.thinking) {
        (Api::Mock, ..) => "MOCK model · fake answers, no network".into(),
        (Api::Deepseek, true, _) => "DeepSeek web · expert".into(),
        (Api::Deepseek, false, true) => {
            format!("DeepSeek web · {} + thinking", settings.deepseek.model_type)
        }
        (Api::Deepseek, false, false) => format!("DeepSeek web · {}", settings.deepseek.model_type),
    }
}

/// Makes sure the real account can be used: asks for a token when there is
/// none, then checks it. Returns a note when the chat goes on without a working token.
async fn ensure_login(
    paths: &Paths,
    settings: &Settings,
    terminal: &mut ui::Terminal,
) -> Result<Option<String>> {
    if Auth::load(paths)?.is_none()
        && relogin(paths, settings, terminal, "You are not logged in yet.")?.is_none()
    {
        bail!("not logged in; run `dsc login`");
    }
    // One cheap call tells whether the saved token still works; a dead one opens the login window now.
    check_token(paths, settings, terminal).await
}

/// How long a token that worked is trusted without asking DeepSeek again.
const TOKEN_TRUST_SECS: u64 = 15 * 60;

/// Checks the saved token against DeepSeek before the chat starts: one call
/// that sends no prompt. It counts as a request for the throttle, and it is
/// skipped when the token worked in the last `TOKEN_TRUST_SECS` (starting the
/// app often must not hammer DeepSeek) or with `DSC_SKIP_TOKEN_CHECK` set
/// (driving the UI in tests). A dead token opens the login window until a token
/// works or the user cancels. Returns a note for the user when the chat starts
/// without a working token.
async fn check_token(
    paths: &Paths,
    settings: &Settings,
    terminal: &mut ui::Terminal,
) -> Result<Option<String>> {
    if std::env::var_os("DSC_SKIP_TOKEN_CHECK").is_some() {
        return Ok(None);
    }
    loop {
        if deepseek::throttle::token_ok_within(paths, TOKEN_TRUST_SECS) {
            return Ok(None);
        }
        let client = Client::new(&Auth::require(paths)?, &settings.deepseek)?;
        deepseek::throttle::acquire(paths, &settings.throttle, &mut |_| {}).await;
        let Err(err) = client.create_challenge().await else {
            deepseek::throttle::token_ok(paths);
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
        search: web_search(settings, run),
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

    if challenge.algorithm == deepseek::client::NO_POW {
        println!(
            "pow       skipped: this is the mock server ({})",
            deepseek::client::base().unwrap_or_default()
        );
        return Ok(());
    }

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
