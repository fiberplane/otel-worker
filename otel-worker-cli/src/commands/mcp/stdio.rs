use super::McpState;
use anyhow::Result;
use rust_mcp_schema::schema_utils::ClientMessage;
use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{debug, error, info};

pub(crate) async fn serve(state: McpState) -> Result<()> {
    let mut notifications_rx = state.notifications.subscribe();

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

            let client_message: ClientMessage =
                serde_json::from_str(&line).expect("todo: handle error state");

            super::handle_client_message(&state, client_message).await;
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
                        .write_all(b"\n")
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
