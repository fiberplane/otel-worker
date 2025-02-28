use super::{handle_initialize, handle_resources_list, handle_resources_read};
use anyhow::{Context, Error, Result};
use axum::extract::{MatchedPath, Request, State};
use axum::middleware::{self, Next};
use axum::response::sse::Event;
use axum::response::{IntoResponse, Sse};
use axum::routing::{get, post};
use axum_jrpc::JsonRpcExtractor;
use futures::{Stream, StreamExt};
use http::StatusCode;
use otel_worker_core::api::client::ApiClient;
use rust_mcp_schema::schema_utils::{
    ResultFromServer, RpcErrorCodes, ServerJsonrpcResponse, ServerMessage,
};
use rust_mcp_schema::{JsonrpcError, RequestId};
use std::process::exit;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::broadcast::{self, Sender};
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tracing::{debug, error, info, info_span, warn, Instrument};

pub(crate) async fn serve(
    listen_address: &str,
    notifications: broadcast::Sender<ServerMessage>,
    api_client: ApiClient,
) -> Result<()> {
    let listener = TcpListener::bind(listen_address)
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

fn build_mcp_service(notifications: Sender<ServerMessage>, api_client: ApiClient) -> axum::Router {
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
    notifications: Sender<ServerMessage>,
}

impl McpState {
    fn reply(&self, message: ServerMessage) {
        if let Err(err) = self.notifications.send(message) {
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
        let result: Result<ResultFromServer> = match req.method() {
            "initialize" => handle_initialize(req.parse_params().unwrap())
                .await
                .map(Into::into),
            "resources/list" => {
                handle_resources_list(&state.api_client, req.parse_params().unwrap())
                    .await
                    .map(Into::into)
            }
            "resources/read" => {
                handle_resources_read(&state.api_client, req.parse_params().unwrap())
                    .await
                    .map(Into::into)
            }
            method => {
                error!(?method, "RPC used a unsupported method");
                Err(Error::msg("unknown method"))
            }
        };

        let id = match answer_id {
            axum_jrpc::Id::Num(val) => RequestId::Integer(val),
            axum_jrpc::Id::Str(val) => RequestId::String(val),
            axum_jrpc::Id::None(_) => panic!("id should be set"),
        };

        let response: ServerMessage = match result {
            Ok(result) => ServerMessage::Response(ServerJsonrpcResponse::new(id, result)),
            Err(_) => ServerMessage::Error(JsonrpcError::create(
                id,
                RpcErrorCodes::INTERNAL_ERROR,
                "error_message".to_string(),
                None,
            )),
        };

        state.reply(response);
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

    let events = tokio_stream::wrappers::BroadcastStream::new(receiver).map(|message| {
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
