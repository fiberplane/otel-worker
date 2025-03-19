use super::McpState;
use anyhow::{Context, Result};
use axum::extract::{MatchedPath, Query, Request, State};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use axum::routing::{get, post};
use axum::Json;
use futures::StreamExt;
use http::StatusCode;
use rust_mcp_schema::schema_utils::ClientMessage;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::process::exit;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, info_span, warn, Instrument};

pub(crate) async fn serve(listen_address: &str, state: McpState) -> Result<()> {
    let listener = TcpListener::bind(listen_address)
        .await
        .with_context(|| format!("Failed to bind to address: {}", listen_address))?;

    info!(
        mcp_listen_address = ?listener.local_addr().context("Failed to get local address")?,
        "Starting MCP server",
    );

    let mcp_service = build_mcp_service(state.clone());

    axum::serve(listener, mcp_service)
        .with_graceful_shutdown(shutdown_requested(state))
        .await?;

    Ok(())
}

fn build_mcp_service(state: McpState) -> axum::Router {
    axum::Router::new()
        .route("/messages", post(json_rpc_handler))
        .route("/sse", get(sse_handler))
        .layer(middleware::from_fn(log_and_metrics))
        .with_state(state)
}

/// The querystring parameter associated with the json-rpc endpoint.
#[derive(Debug, Serialize, Deserialize, Default)]
struct JsonRpcQuery {
    pub session_id: Option<String>,
}

impl JsonRpcQuery {
    fn new(session_id: Option<String>) -> Self {
        Self { session_id }
    }
}

#[tracing::instrument(skip(state))]
async fn json_rpc_handler(
    State(mut state): State<McpState>,
    Query(JsonRpcQuery { session_id, .. }): Query<JsonRpcQuery>,
    Json(client_message): Json<ClientMessage>,
) -> Response {
    // First make sure that the request actually comes with a session_id
    let Some(session_id) = session_id else {
        debug!("Received call to json-rpc endpoint without a session_id");
        return (StatusCode::BAD_REQUEST, "session_id is required").into_response();
    };

    // Then make sure the session actually exists
    let Some(session) = state.get_session(&session_id).await else {
        debug!(
            ?session_id,
            "json-rpc endpoint was called with a session_id which doesn't exists"
        );
        return (StatusCode::BAD_REQUEST, "session not found").into_response();
    };

    // Handle to message in a separate task and immediately return ACCEPTED
    tokio::spawn(async move {
        super::handle_client_message(&state, &session, client_message).await;
    });

    StatusCode::ACCEPTED.into_response()
}

#[tracing::instrument(skip(state))]
async fn sse_handler(State(mut state): State<McpState>) -> Response {
    debug!("MCP client connected to the SSE handler");

    // Do not accept anymore clients if the server is shutting down.
    if state.is_shutting_down() {
        return (StatusCode::SERVICE_UNAVAILABLE, "server is shutting down").into_response();
    }

    let (session_id, messages) = state.register_session().await;

    // This message needs to be send as soon as the client accesses the page.
    let initial_event = futures::stream::once(async move {
        let querystring = serde_urlencoded::to_string(JsonRpcQuery::new(Some(session_id)))
            .expect("querystring encoding is expected to work");
        // We need to explicitly specify the error type, since we are not
        // constructing this anywhere

        Event::default()
            .event("endpoint")
            .data(format!("/messages?{querystring}"))
    });

    // This stream will contain all the ServerMessages which are converted to
    // Sse Events.
    let events = ReceiverStream::new(messages).map(|message| {
        Event::default()
            .event("message")
            .json_data(message)
            .expect("unable to serialize data")
    });

    Sse::new(initial_event.chain(events).map(Ok::<Event, Infallible>))
        .keep_alive(KeepAlive::new())
        .into_response()
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
async fn shutdown_requested(state: McpState) {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for ctrl-c");

    info!("Received SIGINT, shutting down api server");

    state.shutdown().await;

    // Monitor for another SIGINT, and force shutdown if received.
    tokio::spawn(async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for ctrl-c");

        warn!("Received another SIGINT, forcing shutdown");
        exit(1);
    });
}
