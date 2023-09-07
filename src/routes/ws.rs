use std::sync::Arc;

use anyhow::Error;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
};
use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use tokio::sync::Mutex;

use crate::{
    app::SharedState,
    commands::{
        types::{CommandError, CommandSuccess},
        TextCommand,
    },
};

pub async fn handler(ws: WebSocketUpgrade, State(state): State<SharedState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket: WebSocket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: SharedState) {
    let (sender, mut receiver) = socket.split();
    let sender = Arc::new(Mutex::new(sender));

    while let Some(Ok(msg)) = receiver.next().await {
        match msg {
            Message::Text(text) => {
                tokio::spawn(handle_command_text(state.clone(), sender.clone(), text));
            }
            Message::Binary(bytes) => {
                tokio::spawn(handle_command_bytes(state.clone(), sender.clone(), bytes));
            }
            Message::Close(_) => break,
            _ => (),
        }
    }
}

async fn handle_command_text(
    state: SharedState,
    sender: Arc<Mutex<SplitSink<WebSocket, Message>>>,
    payload: String,
) {
    match serde_json::from_str::<TextCommand>(payload.as_str()) {
        Ok(command) => match command.handle(state).await {
            Ok(v) => {
                sender
                    .lock()
                    .await
                    .send(Message::Text(
                        serde_json::to_string(&CommandSuccess::new(command.echo_id, v)).unwrap(),
                    ))
                    .await
                    .ok();
            }
            Err(e) => {
                sender
                    .lock()
                    .await
                    .send(Message::Text(
                        serde_json::to_string(&CommandError::new(command.echo_id, e)).unwrap(),
                    ))
                    .await
                    .ok();
            }
        },
        Err(_) => {
            sender
                .lock()
                .await
                .send(Message::Text(
                    serde_json::to_string(&CommandError::new_raw(Error::msg(
                        "Malformed JSON payload. A payload must include echo_id, command and data!",
                    )))
                    .unwrap(),
                ))
                .await
                .ok();
        }
    }
}

async fn handle_command_bytes(
    state: SharedState,
    sender: Arc<Mutex<SplitSink<WebSocket, Message>>>,
    payload: Vec<u8>,
) {
    match bson::from_slice::<TextCommand>(&payload) {
        Ok(command) => match command.handle(state).await {
            Ok(v) => {
                sender
                    .lock()
                    .await
                    .send(Message::Binary(
                        bson::to_vec(&CommandSuccess::new(command.echo_id, v)).unwrap(),
                    ))
                    .await
                    .ok();
            }
            Err(e) => {
                sender
                    .lock()
                    .await
                    .send(Message::Binary(
                        bson::to_vec(&CommandError::new(command.echo_id, e)).unwrap(),
                    ))
                    .await
                    .ok();
            }
        },
        Err(_) => {
            sender
                .lock()
                .await
                .send(Message::Binary(
                    bson::to_vec(&CommandError::new_raw(Error::msg(
                        "Malformed JSON payload. A payload must include echo_id, command and data!",
                    )))
                    .unwrap(),
                ))
                .await
                .ok();
        }
    }
}