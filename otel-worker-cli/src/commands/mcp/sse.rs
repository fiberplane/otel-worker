use super::McpState;
use anyhow::{Context, Result};
use axum::extract::{MatchedPath, Request, State};
use axum::middleware::{self, Next};
use axum::response::sse::Event;
use axum::response::{IntoResponse, Sse};
use axum::routing::{get, post};
use axum::Json;
use futures::{Stream, StreamExt};
use http::StatusCode;
use rust_mcp_schema::schema_utils::ClientMessage;
use std::process::exit;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
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

#[tracing::instrument(skip(state))]
async fn json_rpc_handler(
    State(state): State<McpState>,
    Json(client_message): Json<ClientMessage>,
) -> impl IntoResponse {
    tokio::spawn(async move {
        super::handle_client_message(&state, client_message).await;
    });

    StatusCode::ACCEPTED
}

#[tracing::instrument(skip(state))]
async fn sse_handler(
    State(state): State<McpState>,
) -> Sse<impl Stream<Item = Result<Event, BroadcastStreamRecvError>>> {
    debug!("MCP client connected to the SSE handler");

    // We will subscribe to the global sender and convert those messages into
    // a stream, which in turn gets converted into SSE events.
    let receiver = state.notifications.subscribe();

    // This message needs to be send as soon as the client accesses the page.
    let initial_event =
        futures::stream::once(async { Ok(Event::default().event("endpoint").data("/messages")) });

    let events = BroadcastStream::new(receiver).map(|message| {
        message.map(|message| {
            Event::default()
                .event("message")
                .json_data(message)
                .expect("unable to serialize data")
        })
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
