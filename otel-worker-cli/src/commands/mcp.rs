use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use otel_worker_core::api::client::{self, ApiClient};
use otel_worker_core::api::models;
use rust_mcp_schema::schema_utils::{
    ClientJsonrpcRequest, ClientMessage, NotificationFromServer, RequestFromClient,
    RequestFromServer, ResultFromServer, RpcErrorCodes, ServerJsonrpcNotification,
    ServerJsonrpcRequest, ServerJsonrpcResponse, ServerMessage,
};
use rust_mcp_schema::{
    ClientRequest, Implementation, InitializeRequestParams, InitializeResult, JsonrpcError,
    JsonrpcErrorError, ListResourcesRequestParams, ListResourcesResult, PingRequestParams,
    ReadResourceRequestParams, ReadResourceResult, ReadResourceResultContentsItem, RequestId,
    Resource, ResourceListChangedNotification, ServerCapabilities, ServerCapabilitiesResources,
    TextResourceContents,
};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};
use url::Url;
use uuid::Uuid;

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

    let mcp_state = McpState::new(api_client);

    info!(?websocket_url, "Connecting to otel-worker websocket");
    let (mut stream, _resp) = tokio_tungstenite::connect_async(&websocket_url)
        .await
        .with_context(|| {
            format!("Unable to connect to the otel-worker websocket at {websocket_url}")
        })?;
    info!("Connected to otel-worker websocket");

    let ws_state = mcp_state.clone();
    let ws_handle = tokio::spawn(async move {
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
                    ws_state.broadcast(message).await;
                }
            }
        }
    });

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
    sessions: Arc<RwLock<HashMap<String, McpSession>>>,
}

impl McpState {
    fn new(api_client: ApiClient) -> Self {
        let sessions = Arc::new(RwLock::new(HashMap::new()));

        Self {
            api_client,
            sessions,
        }
    }

    async fn register_session(&mut self) -> (String, mpsc::Receiver<ServerMessage>) {
        loop {
            let id = Uuid::new_v4().to_string();

            let mut sessions = self.sessions.write().await;
            match sessions.entry(id.clone()) {
                Entry::Occupied(_) => continue,
                Entry::Vacant(entry) => {
                    let (mcp_session, notifications_rx) = McpSession::new();
                    entry.insert(mcp_session);
                    break (id, notifications_rx);
                }
            }
        }
    }

    async fn get_session(&mut self, session_id: impl AsRef<str>) -> Option<McpSession> {
        self.sessions.read().await.get(session_id.as_ref()).cloned()
    }

    /// Send a message to all MCP clients.
    async fn broadcast(&self, message: ServerMessage) {
        let sessions = self.sessions.read().await;

        for session in sessions.values() {
            session.send_message(message.clone()).await
        }
    }
}

#[derive(Debug, Clone)]
struct McpSession {
    notifications: mpsc::Sender<ServerMessage>,
}

impl McpSession {
    pub fn new() -> (Self, mpsc::Receiver<ServerMessage>) {
        let (notifications_tx, notifications_rx) = mpsc::channel(100);

        (
            Self {
                notifications: notifications_tx,
            },
            notifications_rx,
        )
    }

    /// Send a message to this MCP client connected to this session.
    async fn send_message(&self, message: ServerMessage) {
        if self.notifications.send(message).await.is_err() {
            warn!("A reply was send, but no client is connected");
        }
    }

    /// Send a json-rpc request to MCP client connected to this session.
    #[allow(dead_code)]
    async fn send_request(&self, request_id: RequestId, request: impl Into<RequestFromServer>) {
        let request = ServerJsonrpcRequest::new(request_id, request.into());
        let message = ServerMessage::Request(request);
        self.send_message(message).await
    }

    /// Send a json-rpc response to MCP client connected to this session.
    async fn send_response(&self, request_id: RequestId, response: impl Into<ResultFromServer>) {
        let response = ServerJsonrpcResponse::new(request_id, response.into());
        let message = ServerMessage::Response(response);
        self.send_message(message).await
    }

    /// Send a json-rpc error to MCP client connected to this session.
    #[allow(dead_code)]
    async fn send_error(&self, request_id: RequestId, error: impl Into<JsonrpcErrorError>) {
        let error = JsonrpcError::new(error.into(), request_id);
        let message = ServerMessage::Error(error);
        self.send_message(message).await
    }

    /// Send a json-rpc notification to the MCP client connected to this session.
    #[allow(dead_code)]
    async fn send_notification(&self, notification: impl Into<NotificationFromServer>) {
        let notification = ServerJsonrpcNotification::new(notification.into());
        let message = ServerMessage::Notification(notification);
        self.send_message(message).await
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
    session: &McpSession,
    request_id: RequestId,
    params: InitializeRequestParams,
) -> Result<()> {
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

    let message = InitializeResult {
        capabilities,
        instructions: Some(instructions.to_string()),
        meta: None,
        protocol_version: MCP_VERSION.to_string(),
        server_info,
    };

    session.send_response(request_id, message).await;

    Ok(())
}

async fn handle_resources_list(
    McpState { api_client, .. }: &McpState,
    session: &McpSession,
    request_id: RequestId,
    _params: Option<ListResourcesRequestParams>,
) -> Result<()> {
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

    let message = ListResourcesResult {
        meta: None,
        next_cursor: None,
        resources,
    };

    session.send_response(request_id, message).await;

    Ok(())
}

async fn handle_resources_read(
    McpState { api_client, .. }: &McpState,
    session: &McpSession,
    request_id: RequestId,
    params: ReadResourceRequestParams,
) -> Result<()> {
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

    let message = ReadResourceResult {
        contents,
        meta: None,
    };

    session.send_response(request_id, message).await;

    Ok(())
}

async fn handle_ping(
    _state: &McpState,
    session: &McpSession,
    request_id: RequestId,
    _params: Option<PingRequestParams>,
) -> Result<()> {
    let message = rust_mcp_schema::Result {
        meta: None,
        extra: None,
    };

    session.send_response(request_id, message).await;

    Ok(())
}

#[derive(Debug, Default, Clone, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    #[default]
    Stdio,

    Sse,
}

async fn handle_client_message(
    state: &McpState,
    session: McpSession,
    client_message: ClientMessage,
) {
    match client_message {
        ClientMessage::Request(request) => {
            handle_client_request(state, session, request).await;
        }
        ClientMessage::Notification(notification) => {
            handle_client_notification(state, session, notification).await;
        }
        ClientMessage::Response(response) => {
            handle_client_response(state, session, response).await;
        }
        ClientMessage::Error(error) => {
            handle_client_error(state, session, error).await;
        }
    }
}

async fn handle_client_request(
    state: &McpState,
    session: McpSession,
    request: ClientJsonrpcRequest,
) {
    let request_id = request.id.clone();

    let result = match request.request {
        RequestFromClient::ClientRequest(client_request) => match client_request {
            ClientRequest::InitializeRequest(inner_request) => {
                handle_initialize(state, &session, request.id, inner_request.params).await
            }
            ClientRequest::ListResourcesRequest(inner_request) => {
                handle_resources_list(state, &session, request.id, inner_request.params).await
            }
            ClientRequest::ReadResourceRequest(inner_request) => {
                handle_resources_read(state, &session, request.id, inner_request.params).await
            }
            ClientRequest::PingRequest(inner_request) => {
                handle_ping(state, &session, request.id, inner_request.params).await
            }
            _inner_request => {
                error!(
                    method = request.method,
                    "received unsupported json-rpc message"
                );
                Err(anyhow!("received unsupported json-rpc message"))
            }
        },
        RequestFromClient::CustomRequest(_value) => {
            error!(
                method = request.method,
                "received unsupported json-rpc message"
            );
            Err(anyhow!("received unsupported json-rpc message"))
        }
    };

    if let Err(err) = result {
        session
            .send_error(
                request_id,
                JsonrpcErrorError::new(RpcErrorCodes::INTERNAL_ERROR, err.to_string(), None),
            )
            .await;
    }
}

async fn handle_client_notification(
    _state: &McpState,
    _session: McpSession,
    notification: rust_mcp_schema::schema_utils::ClientJsonrpcNotification,
) {
    info!(?notification, "Received a notification!");
}

async fn handle_client_response(
    _state: &McpState,
    _session: McpSession,
    response: rust_mcp_schema::schema_utils::ClientJsonrpcResponse,
) {
    info!(?response, "Received a response!");
}

async fn handle_client_error(_state: &McpState, _session: McpSession, error: JsonrpcError) {
    info!(?error, "Received a client error!");
}
