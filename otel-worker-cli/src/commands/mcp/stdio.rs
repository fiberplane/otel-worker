use anyhow::Result;
use otel_worker_core::api::client::ApiClient;
use rust_mcp_schema::schema_utils::{
    ClientJsonrpcRequest, ClientMessage, RequestFromClient, ResultFromServer, RpcErrorCodes,
    ServerJsonrpcResponse, ServerMessage,
};
use rust_mcp_schema::{ClientRequest, JsonrpcError};
use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::broadcast::{Receiver, Sender};
use tracing::{debug, error, info};

pub(crate) async fn serve(
    notifications_tx: Sender<ServerMessage>,
    mut notifications_rx: Receiver<ServerMessage>,
    api_client: ApiClient,
) -> Result<()> {
    // spawn two tasks, one to read lines on stdin, parse payloads, and dispatch
    // to super::*. The other has to read from notifications and serialize them
    // to stdout.
    let stdin_loop = tokio::spawn(async move {
        let mut stdin = BufReader::new(tokio::io::stdin());

        let mut line = String::new();
        loop {
            line.clear();
            if let Err(err) = stdin.read_line(&mut line).await {
                error!(?err, "unable to read a line from stdin");
                break;
            }

            error!(line, "this is the line");

            let client_message: ClientMessage =
                serde_json::from_str(&line).expect("todo: handle error state");

            match client_message {
                ClientMessage::Request(request) => {
                    handle_client_request(request, &api_client, &notifications_tx).await;
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
    });

    let stdout_loop = tokio::spawn(async move {
        loop {
            match notifications_rx.recv().await {
                Ok(message) => {
                    let message = serde_json::to_string(&message).expect("TODO: should work");
                    let mut stdout = std::io::stdout().lock();

                    stdout
                        .write_all(message.as_bytes())
                        .expect("TODO: should be able to write to stdout");
                    stdout
                        .write(b"\n")
                        .expect("TODO: should be able to write to stdout");
                    stdout.flush().expect("TODO: should be able to flush");
                    debug!("stdout loop has written the message");
                }
                Err(err) => {
                    error!(?err, "TODO: Unable to read from notifications channel");
                    break;
                }
            };
        }
    });

    let result = tokio::try_join!(stdin_loop, stdout_loop);

    match result {
        Ok(_) => info!("Everything went fine"),
        Err(err) => error!(?err, "Something went wrong!!!"),
    }

    Ok(())
}

async fn handle_client_request(
    request: ClientJsonrpcRequest,
    api_client: &ApiClient,
    notifications_tx: &Sender<ServerMessage>,
) {
    let result: Result<ResultFromServer> = match request.request {
        RequestFromClient::ClientRequest(client_request) => match client_request {
            ClientRequest::InitializeRequest(inner_request) => {
                super::handle_initialize(inner_request.params)
                    .await
                    .map(Into::into)
            }
            ClientRequest::ListResourcesRequest(inner_request) => {
                super::handle_resources_list(api_client, inner_request.params)
                    .await
                    .map(Into::into)
            }
            ClientRequest::ReadResourceRequest(inner_request) => {
                super::handle_resources_read(api_client, inner_request.params)
                    .await
                    .map(Into::into)
            }
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

    notifications_tx.send(response).ok();
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
