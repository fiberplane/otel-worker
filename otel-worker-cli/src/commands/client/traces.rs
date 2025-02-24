use anyhow::{anyhow, Result};
use clap::Subcommand;
use otel_worker_core::api::client::ApiClient;
use std::io::stdout;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use url::Url;

#[derive(clap::Args, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Get a single trace
    Get(GetArgs),

    /// List all traces
    List(ListArgs),

    /// Delete all spans for a single trace
    Delete(DeleteArgs),
}

pub async fn handle_command(args: Args) -> Result<()> {
    match args.command {
        Command::Get(args) => handle_get(args).await,
        Command::List(args) => handle_list(args).await,
        Command::Delete(args) => handle_delete(args).await,
    }
}

#[derive(clap::Args, Debug)]
pub struct GetArgs {
    /// TraceID - hex encoded
    pub trace_id: String,

    #[arg(from_global)]
    pub base_url: Url,

    #[arg(from_global)]
    pub auth_token: Option<String>,
}

async fn handle_get(args: GetArgs) -> Result<()> {
    let mut api_client = ApiClient::new(args.base_url.clone());
    api_client.set_bearer_token(args.auth_token);

    let result = api_client.trace_get(args.trace_id).await?;

    serde_json::to_writer_pretty(stdout(), &result)?;

    Ok(())
}

#[derive(clap::Args, Debug)]
pub struct ListArgs {
    /// Optional limit to the amount of traces to be returned
    pub limit: Option<u32>,

    /// Optional timestamp from which traces that occurred before it are returned.
    /// Must be in RFC3339 format
    pub time: Option<String>,

    #[arg(from_global)]
    pub base_url: Url,

    #[arg(from_global)]
    pub auth_token: Option<String>,
}

async fn handle_list(args: ListArgs) -> Result<()> {
    let time = if let Some(time) = args.time {
        Some(
            OffsetDateTime::parse(&time, &Rfc3339)
                .map_err(|err| anyhow!("failed to parse time argument as rfc3339: {err}"))?,
        )
    } else {
        None
    };

    let mut api_client = ApiClient::new(args.base_url.clone());
    api_client.set_bearer_token(args.auth_token);

    let result = api_client.trace_list(args.limit, time).await?;

    serde_json::to_writer_pretty(stdout(), &result)?;

    Ok(())
}

#[derive(clap::Args, Debug)]
pub struct DeleteArgs {
    /// TraceID - hex encoded
    pub trace_id: String,

    #[arg(from_global)]
    pub base_url: Url,

    #[arg(from_global)]
    pub auth_token: Option<String>,
}

async fn handle_delete(args: DeleteArgs) -> Result<()> {
    let mut api_client = ApiClient::new(args.base_url.clone());
    api_client.set_bearer_token(args.auth_token);

    api_client.trace_delete(args.trace_id).await?;

    Ok(())
}
