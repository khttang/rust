use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::net::SocketAddr;

#[derive(Serialize)]
struct GreetingPayload {
    identified: bool,
    name: String,
    audio_url: String, // Path or command to stream greeting back
}

#[tokio::main]
async fn main() {
    let app = Router::new().route("/biometrics", get(ws_handler));
    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    println!("Edge-AI Server listening on {}", addr);
    
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    println!("ESP32-S3 Client connected!");

    while let Some(Ok(msg)) = socket.next().await {
        match msg {
            Message::Binary(bytes) => {
                // Milestone 1 Process logic:
                // 1. Parse payload (e.g., first N bytes = JPEG frame, rest = PCM audio)
                // 2. Run facial matching and voiceprint verification
                
                // Mock success response:
                let response = GreetingPayload {
                    identified: true,
                    name: "Alex".to_string(),
                    audio_url: "/static/greet_alex.pcm".to_string(),
                };
                
                let json_resp = serde_json::to_string(&response).unwrap();
                let _ = socket.send(Message::Text(json_resp.into())).await;
            }
            _ => {}
        }
    }
}
