mod cli;
mod config;

use anyhow::{Result, bail};
use clap::Parser;
use cli::{Cli, Command};
use config::{Paths, Settings};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::new()?;
    let settings = Settings::load_or_create(&paths)?;

    match cli.command {
        Some(Command::Doctor) => doctor(&paths, &settings),
        Some(Command::Chat { .. }) => bail!("chat: not implemented yet (TASKS.md #10)"),
        Some(Command::Login { .. }) => bail!("login: not implemented yet (TASKS.md #2)"),
        Some(Command::Logout) => bail!("logout: not implemented yet (TASKS.md #2)"),
        None if cli.run.task.is_empty() => bail!("no task given; see `dsc --help`"),
        None => bail!("agent run: not implemented yet (TASKS.md #9)"),
    }
}

fn doctor(paths: &Paths, settings: &Settings) -> Result<()> {
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
    println!("cache     {}", paths.cache().display());
    println!("sessions  {}", paths.sessions().display());
    println!(
        "model     {} thinking={} search={}",
        settings.deepseek.model_type, settings.deepseek.thinking, settings.deepseek.search
    );
    Ok(())
}
