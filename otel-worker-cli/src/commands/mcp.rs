use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use otel_worker_core::api::client::ApiClient;
use otel_worker_core::api::models;
use otel_worker_core::data::models::HexEncodedId;
use rust_mcp_schema::schema_utils::{
    ClientJsonrpcRequest, ClientMessage, NotificationFromServer, RequestFromClient,
    RequestFromServer, ResultFromServer, RpcErrorCodes, ServerJsonrpcNotification,
    ServerJsonrpcRequest, ServerJsonrpcResponse, ServerMessage,
};
use rust_mcp_schema::{
    CallToolRequestParams, CallToolResult, ClientRequest, Implementation, InitializeRequestParams,
    InitializeResult, JsonrpcError, JsonrpcErrorError, ListResourcesRequestParams,
    ListResourcesResult, ListToolsRequestParams, ListToolsResult, PingRequestParams,
    ReadResourceRequestParams, ReadResourceResult, ReadResourceResultContentsItem, RequestId,
    Resource, ResourceListChangedNotification, ServerCapabilities, ServerCapabilitiesResources,
    ServerCapabilitiesTools, TextResourceContents, Tool, ToolInputSchema,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
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

    let api_client = ApiClient::builder(args.otel_worker_url)
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
                    let notification = ResourceListChangedNotification::new(None);
                    ws_state.broadcast_notification(notification).await;
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
                    let (mcp_session, messages) = McpSession::new();
                    entry.insert(mcp_session);
                    break (id, messages);
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

    /// Send a json-rpc notification to all MCP clients connected to this
    /// instance.
    #[allow(dead_code)]
    async fn broadcast_notification(&self, notification: impl Into<NotificationFromServer>) {
        let notification = ServerJsonrpcNotification::new(notification.into());
        let message = ServerMessage::Notification(notification);
        self.broadcast(message).await
    }
}

#[derive(Debug, Clone)]
struct McpSession {
    /// This channel is used to send message to the receiver which should
    /// send these message to the MCP client using the transport that is being
    /// used.
    ///
    /// The receiver channel is returned during the creation of the [`McpSession`].
    messages: mpsc::Sender<ServerMessage>,
}

impl McpSession {
    /// Create a new [`McpSession`]. The [`mpsc::Receiver`] returned will
    /// receive any message intended for the Mcp client.
    pub fn new() -> (Self, mpsc::Receiver<ServerMessage>) {
        let (messages_tx, messages_rx) = mpsc::channel(100);

        (
            Self {
                messages: messages_tx,
            },
            messages_rx,
        )
    }

    /// Send a message to this MCP client connected to this session.
    async fn send_message(&self, message: ServerMessage) {
        if self.messages.send(message).await.is_err() {
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
        tools: Some(ServerCapabilitiesTools { list_changed: None }),
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

async fn handle_tools_list(
    _state: &McpState,
    session: &McpSession,
    request_id: RequestId,
    _params: Option<ListToolsRequestParams>,
) -> Result<()> {
    let response = ListToolsResult {
        meta: None,
        next_cursor: None,
        tools: vec![Tool {
            description: Some("Retrieve the raw trace for a single trace".to_string()),
            input_schema: GetTraceParams::tool_input_schema(),
            name: "get_trace".to_string(),
        }],
    };
    session.send_response(request_id, response).await;
    Ok(())
}

async fn handle_tool_call(
    state: &McpState,
    session: &McpSession,
    request_id: RequestId,
    params: CallToolRequestParams,
) -> Result<()> {
    match params.name.as_str() {
        "get_trace" => match params.arguments.try_into() {
            Ok(params) => handle_tool_call_get_trace(state, session, request_id, params).await,
            Err(_) => {
                session
                    .send_error(request_id, JsonrpcErrorError::invalid_params())
                    .await;
                Ok(())
            }
        },
        name => {
            warn!(?name, "Unsupported tool was called");
            session
                .send_error(request_id, JsonrpcErrorError::method_not_found())
                .await;
            Ok(())
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Invalid parameters were provided")]
pub struct InvalidParameters;

/// Parameters associated with the "get_trace" tool.
pub struct GetTraceParams {
    pub trace_id: HexEncodedId,
}

impl GetTraceParams {
    /// Get the [`ToolInputSchema`] associated with [`GetTraceParams`].
    pub fn tool_input_schema() -> ToolInputSchema {
        // TODO: Add automatic schema generation based on the struct.
        let mut properties = HashMap::new();
        let trace_id_props = {
            let mut trace_id_props = serde_json::Map::new();
            trace_id_props.insert(
                "type".to_string(),
                serde_json::Value::String("string".to_string()),
            );
            trace_id_props.insert(
                "description".to_string(),
                serde_json::Value::String("The value of the trace it to retrieve".to_string()),
            );
            trace_id_props
        };
        properties.insert("trace_id".to_string(), trace_id_props);
        ToolInputSchema::new(Some(properties), vec!["trace_id".to_string()])
    }
}

impl TryFrom<Option<Map<String, Value>>> for GetTraceParams {
    type Error = InvalidParameters;

    fn try_from(map: Option<Map<String, Value>>) -> std::result::Result<Self, Self::Error> {
        // TODO: Add automatic parsing and validation based on the json schema
        match map
            .ok_or(InvalidParameters)?
            .remove("trace_id")
            .ok_or(InvalidParameters)?
        {
            Value::String(trace_id) => {
                // TODO: Implement more granular error message types.
                let trace_id = trace_id.parse().map_err(|_| InvalidParameters)?;
                Ok(GetTraceParams { trace_id })
            }
            _ => Err(InvalidParameters),
        }
    }
}

async fn handle_tool_call_get_trace(
    state: &McpState,
    session: &McpSession,
    request_id: RequestId,
    params: GetTraceParams,
) -> std::result::Result<(), anyhow::Error> {
    let trace = state.api_client.trace_get(params.trace_id).await?;
    let response = CallToolResult::text_content(
        serde_json::to_string(&trace).expect("unable to serialize to trace"),
        None,
    );
    session.send_response(request_id, response).await;
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
    session: &McpSession,
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
    session: &McpSession,
    request: ClientJsonrpcRequest,
) {
    let request_id = request.id.clone();

    let result = match request.request {
        RequestFromClient::ClientRequest(client_request) => match client_request {
            ClientRequest::InitializeRequest(inner_request) => {
                handle_initialize(state, session, request.id, inner_request.params).await
            }
            ClientRequest::ListResourcesRequest(inner_request) => {
                handle_resources_list(state, session, request.id, inner_request.params).await
            }
            ClientRequest::ReadResourceRequest(inner_request) => {
                handle_resources_read(state, session, request.id, inner_request.params).await
            }
            ClientRequest::PingRequest(inner_request) => {
                handle_ping(state, session, request.id, inner_request.params).await
            }
            ClientRequest::ListToolsRequest(inner_request) => {
                handle_tools_list(state, session, request.id, inner_request.params).await
            }
            ClientRequest::CallToolRequest(inner_request) => {
                handle_tool_call(state, session, request.id, inner_request.params).await
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
        warn!(?err, "Unable to handle client request");
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
    _session: &McpSession,
    notification: rust_mcp_schema::schema_utils::ClientJsonrpcNotification,
) {
    info!(?notification, "Received a notification!");
}

async fn handle_client_response(
    _state: &McpState,
    _session: &McpSession,
    response: rust_mcp_schema::schema_utils::ClientJsonrpcResponse,
) {
    info!(?response, "Received a response!");
}

async fn handle_client_error(_state: &McpState, _session: &McpSession, error: JsonrpcError) {
    info!(?error, "Received a client error!");
}
