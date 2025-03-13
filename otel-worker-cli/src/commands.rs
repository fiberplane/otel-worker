use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use url::Url;

pub mod client;
pub mod debug;
pub mod dev;
pub mod mcp;
pub mod system;
mod util;

/// otel-worker - store and query traces
#[derive(Parser, Debug)]
#[command(name = "otel-worker-cli", version)]
pub struct Args {
    #[command(subcommand)]
    command: Command,

    /// Enable tracing
    #[arg(long, default_value_t = false, global = true, help_heading = "global")]
    pub enable_tracing: bool,

    /// Endpoint of the OTLP collector.
    #[clap(
        long,
        env,
        default_value = "http://localhost:4317",
        global = true,
        help_heading = "global"
    )]
    pub otlp_endpoint: Url,

    /// Change the otel-worker directory.
    ///
    /// By default otel-worker will search for the first `.otel-worker`
    /// directory in the current directory and its ancestors. If it wasn't found
    /// it will create a `.otel-worker` directory in the current directory.
    #[arg(long, env, global = true, help_heading = "global")]
    pub otel_worker_directory: Option<PathBuf>,

    /// Changes the log level to DEBUG for the otel-worker components and sets
    /// the log level to info for all other components.
    ///
    /// Note that this will get ignored if `$RUST_LOG` is set.
    #[arg(short, long, env, global = true, help_heading = "global")]
    pub debug: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// A cli client to interact with a running otel-worker server.
    Client(client::Args),

    /// Debug related commands.
    #[clap(hide = true)]
    Debug(debug::Args),

    /// Start a local dev server.
    #[clap(aliases = &["up", "d", "start"])]
    Dev(dev::Args),

    /// System related commands.
    System(system::Args),

    /// Start a MCP server that will interact with a upstream otel-worker.
    Mcp(mcp::Args),
}

pub async fn handle_command(args: Args) -> Result<()> {
    match args.command {
        Command::Client(args) => client::handle_command(args).await,
        Command::Debug(args) => debug::handle_command(args).await,
        Command::Dev(args) => dev::handle_command(args).await,
        Command::System(args) => system::handle_command(args).await,
        Command::Mcp(args) => mcp::handle_command(args).await,
    }
}
