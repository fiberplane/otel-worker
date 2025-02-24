use anyhow::{Context, Result};
use async_stream::try_stream;
use axum::extract::{MatchedPath, Request, State};
use axum::middleware::{self, Next};
use axum::response::sse::Event;
use axum::response::{IntoResponse, Sse};
use axum::routing::{get, post};
use axum_jrpc::error::{JsonRpcError, JsonRpcErrorReason};
use axum_jrpc::{JsonRpcExtractor, JsonRpcResponse, Value};
use futures::Stream;
use http::StatusCode;
use otel_worker_core::api::client::ApiClient;
use std::convert::Infallible;
use std::process::exit;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::{self, Sender};
use tracing::{debug, error, info, info_span, warn, Instrument};

use super::{handle_initialize, handle_resources_list, handle_resources_read};

pub async fn serve(
    listen_address: &str,
    notifications: broadcast::Sender<Event>,
    api_client: ApiClient,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(listen_address)
        .await
        .with_context(|| format!("Failed to bind to address: {}", listen_address))?;

    info!(
        mcp_listen_address = ?listener.local_addr().context("Failed to get local address")?,
        "Starting MCP server",
    );

    let mcp_service = build_mcp_service(notifications, api_client);

    axum::serve(listener, mcp_service)
        .with_graceful_shutdown(shutdown_requested())
        .await?;

    Ok(())
}

pub fn build_mcp_service(notifications: Sender<Event>, api_client: ApiClient) -> axum::Router {
    let state = McpState {
        api_client,
        notifications,
    };
    axum::Router::new()
        .route("/messages", post(json_rpc_handler))
        .route("/sse", get(sse_handler))
        .layer(middleware::from_fn(log_and_metrics))
        .with_state(state)
}

#[derive(Clone)]
struct McpState {
    api_client: ApiClient,
    notifications: Sender<Event>,
}

impl McpState {
    fn reply(&self, response: JsonRpcResponse) {
        let event = Event::default()
            .event("message")
            .json_data(response)
            .expect("unable to serialize data");

        if let Err(err) = self.notifications.send(event) {
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
            "resources/list" => {
                handle_resources_list(&state.api_client, req.parse_params().unwrap())
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
                    })
            }
            "resources/read" => {
                handle_resources_read(&state.api_client, req.parse_params().unwrap())
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
                    })
            }
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
    let mut receiver = state.notifications.subscribe();
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
        .notifications
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
