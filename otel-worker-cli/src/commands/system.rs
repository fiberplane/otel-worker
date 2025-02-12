use anyhow::Result;
use clap::Subcommand;
use otel_worker_core::config::find_otel_worker_dir;

mod database;

#[derive(clap::Args, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Low level interaction with the database.
    Database(database::Args),

    /// Print the path to the otel-worker directory.
    OtelWorkerDirectory,
}

pub async fn handle_command(args: Args) -> Result<()> {
    match args.command {
        Command::Database(args) => database::handle_command(args).await,
        Command::OtelWorkerDirectory => handle_otel_worker_directory_command().await,
    }
}

pub async fn handle_otel_worker_directory_command() -> Result<()> {
    let otel_worker_directory = find_otel_worker_dir();
    match otel_worker_directory {
        Some(path) => eprintln!("otel worker directory found: {}", path.display()),
        None => eprintln!("otel worker directory not found"),
    }
    Ok(())
}
