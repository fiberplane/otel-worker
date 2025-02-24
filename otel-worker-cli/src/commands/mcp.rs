use anyhow::{bail, Context, Result};
use async_stream::try_stream;
use axum::extract::{MatchedPath, Request, State};
use axum::middleware::{self, Next};
use axum::response::sse::Event;
use axum::response::{IntoResponse, Sse};
use axum::routing::{get, post};
use axum_jrpc::error::{JsonRpcError, JsonRpcErrorReason};
use axum_jrpc::{JsonRpcExtractor, JsonRpcResponse, Value};
use futures::{Stream, StreamExt};
use http::StatusCode;
use otel_worker_core::api::client::{self, ApiClient};
use otel_worker_core::api::models::{ServerMessage, ServerMessageDetails};
use rust_mcp_schema::{
    Implementation, InitializeRequestParams, InitializeResult, ListResourcesRequestParams,
    ListResourcesResult, ReadResourceRequestParams, ReadResourceResult,
    ReadResourceResultContentsItem, Resource, ResourceListChangedNotification, ServerCapabilities,
    ServerCapabilitiesResources, TextResourceContents,
};
use std::convert::Infallible;
use std::process::exit;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::Sender;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, info_span, warn, Instrument};
use url::Url;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// The URL of the otel-worker instance
    #[arg(long, env, default_value = "http://127.0.0.1:6767")]
    pub otel_worker_url: Url,

    /// Optional authentication token for the otel-worker
    #[arg(long, env)]
    pub otel_worker_token: Option<String>,

    /// The address that the MCP server will listen on.
    #[arg(short, long, env, default_value = "127.0.0.1:3001")]
    pub listen_address: String,
}

pub async fn handle_command(args: Args) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&args.listen_address)
        .await
        .with_context(|| format!("Failed to bind to address: {}", args.listen_address))?;

    info!(
        mcp_listen_address = ?listener.local_addr().context("Failed to get local address")?,
        "Starting MCP server",
    );

    let client = client::builder(args.otel_worker_url)
        .set_bearer_token(args.otel_worker_token)
        .build();

    // This broadcast pair is used for async communication back to the MCP
    // client through SSE.
    let (sender, _) = tokio::sync::broadcast::channel(100);

    let ws_sender = sender.clone();
    let ws_handle = tokio::spawn(async move {
        let u = "ws://localhost:8787/api/ws";
        let (mut stream, _resp) = tokio_tungstenite::connect_async(u)
            .await
            .expect("should be able to connect");

        loop {
            let Some(Ok(message)) = stream.next().await else {
                warn!("websocket client has stopped");
                break;
            };

            debug!("Yay message!");

            if let Message::Text(content) = message {
                let msg: ServerMessage =
                    serde_json::from_str(&content).expect("Should be able to deserialize it");

                match msg.details {
                    ServerMessageDetails::SpanAdded(_span_added) => {
                        let data = ResourceListChangedNotification::new(None);
                        ws_sender
                            .send(
                                Event::default()
                                    .event("message")
                                    .json_data(data)
                                    .expect("serialization should just work"),
                            )
                            .ok();
                        debug!("list_changed message send");
                    }
                    _ => debug!("Irrelevant message"),
                }
            } else {
                warn!("Received non text message, ignoring");
                continue;
            }
        }
    });

    let mcp_service = build_mcp_service(client, sender);

    axum::serve(listener, mcp_service)
        .with_graceful_shutdown(shutdown_requested())
        .await?;

    ws_handle.abort();

    Ok(())
}

fn build_mcp_service(client: ApiClient, sender: Sender<Event>) -> axum::Router {
    let state = McpState { client, sender };
    axum::Router::new()
        .route("/messages", post(json_rpc_handler))
        .route("/sse", get(sse_handler))
        .layer(middleware::from_fn(log_and_metrics))
        .with_state(state)
}

#[derive(Clone)]
struct McpState {
    client: ApiClient,
    sender: Sender<Event>,
}

impl McpState {
    fn reply(&self, response: JsonRpcResponse) {
        let event = Event::default()
            .event("message")
            .json_data(response)
            .expect("unable to serialize data");

        if let Err(err) = self.sender.send(event) {
            warn!(?err, "A reply was send, but client is connected");
        }
    }
}

#[tracing::instrument(skip(state))]
async fn json_rpc_handler(
    State(state): State<McpState>,
    req: JsonRpcExtractor,
) -> impl IntoResponse {
    tokio::spawn(async move {
        let answer_id = req.get_answer_id();
        let result = match req.method() {
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
            "resources/list" => handle_resources_list(&state, req.parse_params().unwrap())
                .await
                .map(|result| JsonRpcResponse::success(answer_id.clone(), result))
                .unwrap_or_else(|err| {
                    error!(?err, "handle_resources_list");
                    JsonRpcResponse::error(
                        answer_id,
                        JsonRpcError::new(
                            JsonRpcErrorReason::InternalError,
                            "message".to_string(),
                            Value::Null,
                        ),
                    )
                }),
            "resources/read" => handle_resources_read(&state, req.parse_params().unwrap())
                .await
                .map(|result| JsonRpcResponse::success(answer_id.clone(), result))
                .unwrap_or_else(|err| {
                    error!(?err, "handle_resources_read");
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
        };

        state.reply(result);
    });

    StatusCode::ACCEPTED
}

#[tracing::instrument(skip(state))]
async fn sse_handler(
    State(state): State<McpState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    debug!("MCP client connected to the SSE handler");

    // We will subscribe to the global sender and convert those messages into
    // a stream, which in turn gets converted into SSE events.
    let mut receiver = state.sender.subscribe();
    let stream = try_stream! {
        loop {
            let recv = receiver.recv().await.expect("should work");
            yield recv;
        }
    };

    // Part of the SSE protocol is sending the location where the client needs
    // to do its POST request. This will be sent to the channel which will be
    // queued there until the client is connected to the SSE stream.
    if let Err(err) = state
        .sender
        .send(Event::default().event("endpoint").data("/messages"))
    {
        error!(?err, "unable to send initial message to MCP client");
    }

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(5))
            .text("keep-alive-text"),
    )
}

async fn handle_initialize(params: InitializeRequestParams) -> Result<InitializeResult> {
    debug!(?params, "Received initialize message");

    const MCP_VERSION: &str = "2024-11-05";

    // We only support one version for now
    if params.protocol_version != MCP_VERSION {
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
    McpState { client, .. }: &McpState,
    _params: ListResourcesRequestParams,
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
    McpState { client, .. }: &McpState,
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

/// Blocks while waiting for the SIGINT signal. This is most commonly send using
/// the keyboard shortcut ctrl+c. This is intended to be used with axum's
/// [`with_graceful_shutdown`] to trigger a graceful shutdown.
///
/// Another SIGINT listener task is spawned just before resolving this task,
/// which will forcefully exit the application. This is to prevent not being
/// able to shutdown, if the graceful shutdown doesn't work.
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

async fn log_and_metrics(req: Request, next: Next) -> impl IntoResponse {
    let start_time = Instant::now();

    let method = req.method().to_string();
    let matched_path = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string());

    let span = info_span!("http_request",
        %method,
        path = %req.uri().path(),
        matched_path = %matched_path.as_deref().unwrap_or_default(),
        version = ?req.version());

    let res = next.run(req).instrument(span.clone()).await;

    let duration = start_time.elapsed();
    let status = res.status().as_u16().to_string();
    span.in_scope(|| {
        debug!(%status, duration = %format!("{duration:.1?}"), "Request result");
    });

    res
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
