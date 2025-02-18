use anyhow::Result;
use clap::Subcommand;
use otel_worker_core::api::client::ApiClient;
use std::io::stdout;
use url::Url;

#[derive(clap::Args, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Get a single span
    Get(GetArgs),

    /// List all spans for a single trace
    List(ListArgs),

    /// Delete a single span
    Delete(DeleteArgs),
}

pub async fn handle_command(args: Args, auth_token: Option<String>) -> Result<()> {
    match args.command {
        Command::Get(args) => handle_get(args, auth_token).await,
        Command::List(args) => handle_list(args, auth_token).await,
        Command::Delete(args) => handle_delete(args, auth_token).await,
    }
}

#[derive(clap::Args, Debug)]
pub struct GetArgs {
    /// TraceID - hex encoded
    pub trace_id: String,

    /// SpanID - hex encoded
    pub span_id: String,

    /// Base url of the otel-worker server.
    #[arg(from_global)]
    pub base_url: Url,
}

async fn handle_get(args: GetArgs, auth_token: Option<String>) -> Result<()> {
    let mut api_client = ApiClient::new(args.base_url.clone());
    if let Some(token) = auth_token {
        api_client = api_client.with_bearer_token(token);
    }

    let result = api_client.span_get(args.trace_id, args.span_id).await?;

    serde_json::to_writer_pretty(stdout(), &result)?;

    Ok(())
}

#[derive(clap::Args, Debug)]
pub struct ListArgs {
    /// TraceID - hex encoded
    pub trace_id: String,

    /// Base url of the otel-worker server.
    #[arg(from_global)]
    pub base_url: Url,
}

async fn handle_list(args: ListArgs, auth_token: Option<String>) -> Result<()> {
    let mut api_client = ApiClient::new(args.base_url.clone());
    if let Some(token) = auth_token {
        api_client = api_client.with_bearer_token(token);
    }

    let result = api_client.span_list(args.trace_id).await?;

    serde_json::to_writer_pretty(stdout(), &result)?;

    Ok(())
}

#[derive(clap::Args, Debug)]
pub struct DeleteArgs {
    /// TraceID - hex encoded
    pub trace_id: String,

    /// SpanID - hex encoded
    pub span_id: String,

    /// Base url of the otel-worker server.
    #[arg(from_global)]
    pub base_url: Url,
}

async fn handle_delete(args: DeleteArgs, auth_token: Option<String>) -> Result<()> {
    let mut api_client = ApiClient::new(args.base_url.clone());
    if let Some(token) = auth_token {
        api_client = api_client.with_bearer_token(token);
    }

    api_client.span_delete(args.trace_id, args.span_id).await?;

    Ok(())
}
