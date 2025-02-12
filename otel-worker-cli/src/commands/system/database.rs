use anyhow::Result;
use clap::Subcommand;
use otel_worker_core::config::{find_otel_worker_dir, DATABASE_FILENAME};
use std::path::PathBuf;
use tracing::{error, info, warn};

#[derive(clap::Args, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,

    /// otel worker directory
    #[arg(from_global)]
    pub otel_worker_directory: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Delete the database files from the otel-worker directory.
    Delete,
}

pub async fn handle_command(args: Args) -> Result<()> {
    match args.command {
        Command::Delete => handle_delete_database(args).await,
    }
}

pub async fn handle_delete_database(args: Args) -> Result<()> {
    let Some(otel_worker_directory) = args.otel_worker_directory.or_else(find_otel_worker_dir)
    else {
        warn!("Unable to find otel-worker directory, skipped deleting database");
        return Ok(());
    };

    match tokio::fs::remove_file(otel_worker_directory.join(DATABASE_FILENAME)).await {
        Ok(_) => info!("Database deleted"),
        Err(err) => error!(?err, "Failed to delete database"),
    };

    Ok(())
}
