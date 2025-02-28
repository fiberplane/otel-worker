use anyhow::{bail, Context, Result};
use futures::StreamExt;
use otel_worker_core::api::client::{self, ApiClient};
use otel_worker_core::api::models;
use rust_mcp_schema::schema_utils::ServerMessage;
use rust_mcp_schema::{
    Implementation, InitializeRequestParams, InitializeResult, ListResourcesRequestParams,
    ListResourcesResult, ReadResourceRequestParams, ReadResourceResult,
    ReadResourceResultContentsItem, Resource, ResourceListChangedNotification, ServerCapabilities,
    ServerCapabilitiesResources, TextResourceContents,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};
use url::Url;

mod http_sse;
mod stdio;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// The URL of the otel-worker instance
    #[arg(long, env, default_value = "http://127.0.0.1:6767")]
    pub otel_worker_url: Url,

    /// Optional authentication token for the otel-worker
    #[arg(long, env)]
    pub otel_worker_token: Option<String>,

    #[arg(long, env, default_value_t = Transport::Stdio, value_enum)]
    pub transport: Transport,

    /// The address that the MCP server will listen on.
    #[arg(
        short,
        long,
        env,
        default_value = "127.0.0.1:3001",
        help_heading = "http with sse"
    )]
    pub listen_address: String,
}

pub async fn handle_command(args: Args) -> Result<()> {
    // Generate the websocket url outside of the [`tokio::task`] so we don't
    // have to worry about error handling inside the [`tokio::task`].
    let websocket_url = get_ws_url(&args.otel_worker_url)?;

    let api_client = client::builder(args.otel_worker_url)
        .set_bearer_token(args.otel_worker_token)
        .build();

    // This broadcast pair is used for async communication back to the MCP
    // client through SSE.
    let (notifications_tx, _) = broadcast::channel(100);

    let ws_sender = notifications_tx.clone();
    let ws_handle = tokio::spawn(async move {
        info!(?websocket_url, "Connecting to websocket");

        let (mut stream, _resp) = tokio_tungstenite::connect_async(websocket_url)
            .await
            .expect("should be able to connect");

        loop {
            let Some(Ok(message)) = stream.next().await else {
                warn!("websocket client has stopped");
                break;
            };

            if let Message::Text(content) = message {
                let msg: models::ServerMessage =
                    serde_json::from_str(&content).expect("Should be able to deserialize it");

                match msg.details {
                    models::ServerMessageDetails::SpanAdded(_span_added) => {
                        let data = ResourceListChangedNotification::new(None);
                        let message = ServerMessage::Notification(data.into());
                        ws_sender.send(message).ok();
                    }
                    _ => debug!("Irrelevant message"),
                }
            } else {
                warn!("Received non text message, ignoring");
                continue;
            }
        }
    });

    match args.transport {
        Transport::Stdio => stdio::serve(notifications_tx, api_client).await?,
        Transport::HttpSse => {
            http_sse::serve(&args.listen_address, notifications_tx, api_client).await?
        }
    }

    ws_handle.abort();

    Ok(())
}

/// Give the otel_work_url generate the websocket uri. This will convert the
/// scheme to the equivalent websocket scheme. The same authority will be used,
/// while the path will use a hardcoded value.
fn get_ws_url(otel_worker_url: &url::Url) -> Result<http::Uri> {
    let scheme = match otel_worker_url.scheme() {
        "http" => "ws",
        "https" => "wss",
        scheme => anyhow::bail!("unsupported scheme: {}", scheme),
    };

    let authority = otel_worker_url.authority();

    // NOTE: For now just assume that the api is hosted on the root and thus the
    // ws endpoint is nested directly there. This might need some smarts in case
    // reverse proxy are in between that rewrite the path.
    let path = "/api/ws";

    http::Uri::builder()
        .scheme(scheme)
        .authority(authority)
        .path_and_query(path)
        .build()
        .context("unable to build URI")
}

async fn handle_initialize(params: InitializeRequestParams) -> Result<InitializeResult> {
    debug!(?params, "Received initialize message");

    const MCP_VERSION: &str = "2024-11-05";

    // We only support one version for now
    if params.protocol_version != MCP_VERSION {
        debug!(?params, "unsupported version");
        anyhow::bail!("unsupported version")
    }

    // TODO: Validate incoming request, ie. check if desired capabilities match
    //       ours.

    // TODO: Set the server to be "initialized" and store the capabilities, etc

    // TODO: Figure out which capabilities our MCP implementation will support
    let capabilities = ServerCapabilities {
        experimental: None,
        logging: None,
        prompts: None,
        resources: Some(ServerCapabilitiesResources {
            list_changed: Some(true),
            subscribe: None,
        }),
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

async fn handle_resources_list(
    client: &ApiClient,
    _params: Option<ListResourcesRequestParams>,
) -> Result<ListResourcesResult> {
    let resources = client
        .trace_list()
        .await?
        .iter()
        .map(|trace| {
            let name = format!("trace {}", trace.trace_id);
            let uri = format!("trace://{}", trace.trace_id);
            Resource {
                name,
                uri,
                description: None,
                annotations: None,
                mime_type: None,
                size: None,
            }
        })
        .collect();

    let result = ListResourcesResult {
        meta: None,
        next_cursor: None,
        resources,
    };

    Ok(result)
}

async fn handle_resources_read(
    client: &ApiClient,
    params: ReadResourceRequestParams,
) -> Result<ReadResourceResult> {
    debug!(?params, "Received a call on resources_read");

    let Some((resource_type, arguments)) = params.uri.split_once("://") else {
        bail!("invalid uri");
    };

    let contents: Vec<ReadResourceResultContentsItem> = match resource_type {
        "trace" => client
            .span_list(arguments)
            .await
            .expect("Call to list spans failed")
            .iter()
            .map(|span| {
                ReadResourceResultContentsItem::TextResourceContents(TextResourceContents {
                    mime_type: Some("application/json".to_string()),
                    text: serde_json::to_string(&span)
                        .expect("should be able to serialize to json"),
                    uri: format!("span://{}", span.span_id),
                })
            })
            .collect(),
        resource_type => bail!("unknown resource type: {}", resource_type),
    };

    let result = ReadResourceResult {
        contents,
        meta: None,
    };

    Ok(result)
}

// #[cfg(test)]
// mod test {
//     use axum_jrpc::JsonRpcRequest;

// // Currently we are not able to respond to the Ping command as that doesn't
// // send a params property, which is required by serde.
//     #[test]
//     fn ping_extractor_test() {
//         let input = r#"{"method":"ping","jsonrpc":"2.0","id":1}"#;
//         let parsed: JsonRpcRequest = serde_json::from_str(input).unwrap();
//     }
// }

#[derive(Debug, Default, Clone, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    #[default]
    Stdio,

    HttpSse,
}
