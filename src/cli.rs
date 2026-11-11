use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// Agent that runs commands on this machine, driven by the free DeepSeek web account.
#[derive(Parser, Debug)]
#[command(name = "dsc", version, args_conflicts_with_subcommands = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub run: RunArgs,
}

#[derive(Args, Debug, Default)]
pub struct RunArgs {
    /// The task for the agent, e.g. `dsc "how much disk space is free"`
    pub task: Vec<String>,

    /// Run allowed and unknown commands without asking (the deny list still applies)
    #[arg(short, long)]
    pub yes: bool,

    /// Reasoning mode
    #[arg(long)]
    pub think: bool,

    /// Web search on DeepSeek's side on (the default; see `deepseek.search` in the config)
    #[arg(long, conflicts_with = "no_search")]
    pub search: bool,

    /// Web search on DeepSeek's side off: web pages are then fetched from this machine
    #[arg(long)]
    pub no_search: bool,

    /// The "expert" model instead of the fast one
    #[arg(long)]
    pub expert: bool,

    /// Continue an earlier conversation: the code that `dsc` prints when it exits (also in `/sessions`)
    #[arg(long, value_name = "CODE")]
    pub session: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Interactive conversation: several tasks, one after another (also the default without a task)
    Chat,
    /// Store the DeepSeek web token
    Login {
        /// Read FreeDeepseekAPI's deepseek-auth.json
        #[arg(long, conflicts_with = "token")]
        import: Option<PathBuf>,
        /// Paste the token on a hidden prompt
        #[arg(long)]
        token: bool,
    },
    /// Remove the stored token
    Logout,
    /// Check paths, token and the DeepSeek calls step by step
    Doctor,
}
