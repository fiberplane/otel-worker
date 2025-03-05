use super::McpState;
use anyhow::{Context, Result};
use axum::extract::{MatchedPath, Query, Request, State};
use axum::middleware::{self, Next};
use axum::response::sse::Event;
use axum::response::{IntoResponse, Response, Sse};
use axum::routing::{get, post};
use axum::Json;
use futures::{Stream, StreamExt};
use http::StatusCode;
use rust_mcp_schema::schema_utils::ClientMessage;
use serde::Deserialize;
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

    let mcp_service = build_mcp_service(state);

    axum::serve(listener, mcp_service)
        .with_graceful_shutdown(shutdown_requested())
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

#[derive(Debug, Deserialize, Default)]
struct JsonRpcQuery {
    pub session_id: Option<String>,
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
        super::handle_client_message(&state, session, client_message).await;
    });

    StatusCode::ACCEPTED.into_response()
}

#[tracing::instrument(skip(state))]
async fn sse_handler(
    State(mut state): State<McpState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    debug!("MCP client connected to the SSE handler");

    let (session_id, session_notifications) = state.register_session().await;

    // This message needs to be send as soon as the client accesses the page.
    let initial_event = futures::stream::once(async move {
        Ok(Event::default()
            .event("endpoint")
            .data(format!("/messages?session_id={session_id}")))
    });

    // This stream will contain all the ServerMessages which are converted to
    // Sse Events.
    let events = ReceiverStream::new(session_notifications).map(|message| {
        Ok(Event::default()
            .event("message")
            .json_data(message)
            .expect("unable to serialize data"))
    });

    Sse::new(initial_event.chain(events)).keep_alive(
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
