use anyhow::{bail, Context, Result};
use futures::StreamExt;
use otel_worker_core::api::client::{self, ApiClient};
use otel_worker_core::api::models;
use rust_mcp_schema::schema_utils::{
    ClientJsonrpcRequest, ClientMessage, RequestFromClient, ResultFromServer, RpcErrorCodes,
    ServerJsonrpcResponse, ServerMessage,
};
use rust_mcp_schema::{
    ClientRequest, Implementation, InitializeRequestParams, InitializeResult, JsonrpcError,
    ListResourcesRequestParams, ListResourcesResult, PingRequestParams, ReadResourceRequestParams,
    ReadResourceResult, ReadResourceResultContentsItem, Resource, ResourceListChangedNotification,
    ServerCapabilities, ServerCapabilitiesResources, TextResourceContents,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};
use url::Url;

mod sse;
mod stdio;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// The URL of the otel-worker instance
    #[arg(long, env, default_value = "http://127.0.0.1:6767")]
    pub otel_worker_url: Url,

    /// Optional authentication token for the otel-worker
    #[arg(long, env)]
    pub otel_worker_token: Option<String>,

    /// The type of transport used to communicate between this MCP server and
    /// the MCP client.
    #[arg(long, env, default_value_t = Transport::Stdio, value_enum)]
    pub transport: Transport,

    /// The address that the MCP server will listen on. Only available with SSE
    /// transport
    #[arg(
        short,
        long,
        env,
        default_value = "127.0.0.1:3001",
        help_heading = "SSE"
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

    // This broadcast channel is used for async communication back to the MCP
    // client.
    let (notifications, _) = broadcast::channel(100);

    let ws_sender = notifications.clone();
    let ws_handle = tokio::spawn(async move {
        info!(?websocket_url, "Connecting to otel-worker websocket");

        let (mut stream, _resp) = tokio_tungstenite::connect_async(websocket_url)
            .await
            .expect("should be able to connect");

        info!("Connected to otel-worker websocket");

        loop {
            let Some(Ok(message)) = stream.next().await else {
                warn!("websocket client has stopped");
                break;
            };

            if let Message::Text(content) = message {
                let msg: models::ServerMessage =
                    serde_json::from_str(&content).expect("Should be able to deserialize it");

                // We are only interested in 1 event, so we just ignore everything else
                if let models::ServerMessageDetails::SpanAdded(_span_added) = msg.details {
                    let data = ResourceListChangedNotification::new(None);
                    let message = ServerMessage::Notification(data.into());
                    ws_sender.send(message).ok();
                }
            }
        }
    });

    let mcp_state = McpState::new(api_client, notifications);

    match args.transport {
        Transport::Stdio => stdio::serve(mcp_state).await?,
        Transport::Sse => sse::serve(&args.listen_address, mcp_state).await?,
    }

    ws_handle.abort();

    Ok(())
}

#[derive(Clone)]
struct McpState {
    api_client: ApiClient,
    notifications: broadcast::Sender<ServerMessage>,
}

impl McpState {
    fn new(api_client: ApiClient, notifications: broadcast::Sender<ServerMessage>) -> Self {
        Self {
            api_client,
            notifications,
        }
    }

    /// Send a message to the MCP client using [`notifications`].
    fn reply(&self, message: ServerMessage) {
        if self.notifications.send(message).is_err() {
            warn!("A reply was send, but no client is connected");
        }
    }
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
    // reverse proxy is used to expose the otel-worker.
    let path = "/api/ws";

    http::Uri::builder()
        .scheme(scheme)
        .authority(authority)
        .path_and_query(path)
        .build()
        .context("unable to build otel-worker websocket URI")
}

async fn handle_initialize(
    _state: &McpState,
    params: InitializeRequestParams,
) -> Result<InitializeResult> {
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
    McpState { api_client, .. }: &McpState,
    _params: Option<ListResourcesRequestParams>,
) -> Result<ListResourcesResult> {
    let resources = api_client
        .trace_list(Some(50), None)
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
    McpState { api_client, .. }: &McpState,
    params: ReadResourceRequestParams,
) -> Result<ReadResourceResult> {
    debug!(?params, "Received a call on resources_read");

    let Some((resource_type, arguments)) = params.uri.split_once("://") else {
        bail!("invalid uri");
    };

    let contents: Vec<ReadResourceResultContentsItem> = match resource_type {
        "trace" => api_client
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

async fn handle_ping(
    _state: &McpState,
    _params: Option<PingRequestParams>,
) -> Result<rust_mcp_schema::Result> {
    Ok(rust_mcp_schema::Result {
        meta: None,
        extra: None,
    })
}

#[derive(Debug, Default, Clone, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    #[default]
    Stdio,

    Sse,
}

async fn handle_client_message(state: &McpState, client_message: ClientMessage) {
    match client_message {
        ClientMessage::Request(request) => {
            handle_client_request(state, request).await;
        }
        ClientMessage::Notification(notification) => {
            handle_client_notification(notification).await;
        }
        ClientMessage::Response(response) => {
            handle_client_response(response).await;
        }
        ClientMessage::Error(error) => {
            handle_client_error(error).await;
        }
    }
}

async fn handle_client_request(state: &McpState, request: ClientJsonrpcRequest) {
    let result: Result<ResultFromServer> = match request.request {
        RequestFromClient::ClientRequest(client_request) => match client_request {
            ClientRequest::InitializeRequest(inner_request) => {
                handle_initialize(state, inner_request.params)
                    .await
                    .map(Into::into)
            }
            ClientRequest::ListResourcesRequest(inner_request) => {
                handle_resources_list(state, inner_request.params)
                    .await
                    .map(Into::into)
            }
            ClientRequest::ReadResourceRequest(inner_request) => {
                handle_resources_read(state, inner_request.params)
                    .await
                    .map(Into::into)
            }
            ClientRequest::PingRequest(inner_request) => handle_ping(state, inner_request.params)
                .await
                .map(Into::into),
            _inner_request => {
                error!(method = request.method, "Received unsupported requests");
                return;
            }
        },
        RequestFromClient::CustomRequest(_value) => {
            error!("received unsupported custom message");
            return;
        }
    };

    let response = match result {
        Ok(result) => ServerMessage::Response(ServerJsonrpcResponse::new(request.id, result)),
        Err(_err) => ServerMessage::Error(JsonrpcError::create(
            request.id,
            RpcErrorCodes::INTERNAL_ERROR,
            "error_message".to_string(),
            None,
        )),
    };

    state.reply(response);
}

async fn handle_client_notification(
    notification: rust_mcp_schema::schema_utils::ClientJsonrpcNotification,
) {
    info!(?notification, "Received a notification!");
}

async fn handle_client_response(response: rust_mcp_schema::schema_utils::ClientJsonrpcResponse) {
    info!(?response, "Received a response!");
}

async fn handle_client_error(error: JsonrpcError) {
    info!(?error, "Received a client error!");
}
