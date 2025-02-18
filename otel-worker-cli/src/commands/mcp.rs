use anyhow::{Context, Result};
use axum::routing::post;
use axum_jrpc::{
    error::{JsonRpcError, JsonRpcErrorReason},
    JsonRpcExtractor, JsonRpcResponse, Value,
};
use rust_mcp_schema::{
    Implementation, InitializeRequestParams, InitializeResult, ListResourcesRequest,
    ListResourcesRequestParams, ListResourcesResult, ServerCapabilities,
};
use serde::Serialize;
use std::{path::PathBuf, process::exit};
use tracing::{error, info, warn};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// The address to listen on.
    #[arg(short, long, env, default_value = "127.0.0.1:3001")]
    pub listen_address: String,

    /// otel-worker directory
    #[arg(from_global)]
    pub otel_worker_directory: Option<PathBuf>,
}

pub async fn handle_command(args: Args) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&args.listen_address)
        .await
        .with_context(|| format!("Failed to bind to address: {}", args.listen_address))?;

    info!(
        api_listen_address = ?listener.local_addr().context("Failed to get local address")?,
        "Starting server",
    );

    let app = build_mcp_service();

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_requested())
        .await?;

    Ok(())
}

fn build_mcp_service() -> axum::Router {
    axum::Router::new().route("/", post(json_rpc_handler))
}

async fn json_rpc_handler(req: JsonRpcExtractor) -> JsonRpcResponse {
    let answer_id = req.get_answer_id();
    match req.method() {
        "initialize" => handle_initialize(req.parse_params().unwrap())
            .await
            .map(|result| JsonRpcResponse::success(answer_id.clone(), result))
            .unwrap_or_else(|err| {
                error!(?err, "initialization failed");
                JsonRpcResponse::error(
                    answer_id,
                    JsonRpcError::new(
                        JsonRpcErrorReason::InternalError,
                        "message".to_string(),
                        Value::Null,
                    ),
                )
            }),
        "resources/list" => handle_resources_list(req.parse_params().unwrap())
            .await
            .map(|result| JsonRpcResponse::success(answer_id.clone(), result))
            .unwrap_or_else(|err| {
                error!(?err, "initialization failed");
                JsonRpcResponse::error(
                    answer_id,
                    JsonRpcError::new(
                        JsonRpcErrorReason::InternalError,
                        "message".to_string(),
                        Value::Null,
                    ),
                )
            }),
        method => {
            error!(?method, "RPC used a unsupported method");
            JsonRpcResponse::error(
                answer_id,
                JsonRpcError::new(
                    JsonRpcErrorReason::MethodNotFound,
                    "message".to_string(),
                    Value::Null,
                ),
            )
        }
    }
}

async fn handle_initialize(params: InitializeRequestParams) -> Result<InitializeResult> {
    const MCP_VERSION: &str = "2024-11-05";

    // We only support one version for now
    if params.protocol_version != MCP_VERSION {
        anyhow::bail!("unsupported version")
    }

    // TODO: Validate incoming request, ie. check if desired capabilities match
    //       ours.

    // TODO: Figure out which capabilities our MCP implementation will support
    let capabilities = ServerCapabilities {
        experimental: None,
        logging: None,
        prompts: None,
        resources: None,
        tools: None,
    };

    // TODO: Use better instructions
    let instructions = "Sample instructions";
    let server_info = Implementation {
        name: env!("CARGO_PKG_NAME").to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };

    let result = InitializeResult {
        capabilities,
        instructions: Some(instructions.to_string()),
        meta: None,
        protocol_version: MCP_VERSION.to_string(),
        server_info,
    };

    Ok(result)
}

async fn handle_resources_list(params: ListResourcesRequestParams) -> Result<ListResourcesResult> {
    todo!()
}

/// Blocks while waiting for the SIGINT signal. This is most commonly send using
/// the keyboard shortcut ctrl+c. This is intended to be used with axum's
/// [`with_graceful_shutdown`].
async fn shutdown_requested() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for ctrl-c");

    info!("Received SIGINT, shutting down api server");

    // Monitor for another SIGINT, and force shutdown if received.
    tokio::spawn(async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for ctrl-c");

        warn!("Received another SIGINT, forcing shutdown");
        exit(1);
    });
}
