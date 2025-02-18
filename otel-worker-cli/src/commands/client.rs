use anyhow::Result;
use clap::Subcommand;
use url::Url;

mod spans;
mod traces;

#[derive(clap::Args, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,

    /// Base url of the otel-worker server.
    #[arg(
        global = true,
        short,
        long,
        env,
        default_value = "http://127.0.0.1:6767"
    )]
    pub base_url: Url,

    /// Bearer token for authentication. If not provided, will try to read from AUTH_TOKEN environment variable.
    #[arg(global = true, short, long, env = "AUTH_TOKEN")]
    pub auth_token: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Interact with stored spans
    Spans(spans::Args),

    /// Interact with stored traces
    Traces(traces::Args),
}

pub async fn handle_command(args: Args) -> Result<()> {
    match args.command {
        Command::Spans(spans_args) => spans::handle_command(spans_args, args.auth_token).await,
        Command::Traces(traces_args) => traces::handle_command(traces_args, args.auth_token).await,
    }
}
