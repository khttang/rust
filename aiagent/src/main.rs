use axum::{routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

// --- Google A2A Standard Schema Definitions ---
#[derive(Serialize, Deserialize, Debug, Clone)]
struct A2ARequest {
    message: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct A2AResponse {
    status: String,
    result: String,
}

// --- Human Custom Schema ---
#[derive(Deserialize)]
struct HumanRequest {
    prompt: String,
}

#[tokio::main]
async fn main() {
    // 1. Build the router with endpoints for both Human and Peer Agent inputs
    let app = Router::new()
        .route("/human/prompt", post(handle_human_request))
        .route("/a2a/v1/task", post(handle_agent_request));

    // 2. Start the unified REST server
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    println!("🤖 Agent REST API initialized!");
    println!("👤 Human interface: http://localhost:8080/human/prompt");
    println!("🔗 Agent-to-Agent interface: http://localhost:8080/a2a/v1/task");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// --- Shared Core Engine ---
// This processes the prompt regardless of who sent it (human or machine)
fn process_core_logic(incoming_text: &str, sender_type: &str) -> A2AResponse {
    println!("🧠 [Core Engine] Processing request from standard: {}", sender_type);

    let generated_output = match incoming_text {
        t if t.contains("status") => "All local sub-systems are healthy.".to_string(),
        _ => format!("Processed text: '{}' via unified pipeline.", incoming_text)
    };

    A2AResponse {
        status: "SUCCESS".to_string(),
        result: generated_output,
    }
}

// --- Route 1: Human Endpoint Handler ---
async fn handle_human_request(
    Json(payload): Json<HumanRequest>,
) -> Json<A2AResponse> {
    println!("\n👤 [Incoming] Received human prompt via REST.");
    
    // Pass to unified engine, mapping human text to the underlying logic
    let response = process_core_logic(&payload.prompt, "HUMAN_REST");
    
    Json(response)
}

// --- Route 2: Agent Endpoint Handler ---
async fn handle_agent_request(
    Json(payload): Json<A2ARequest>,
) -> Json<A2AResponse> {
    println!("\n📥 [Incoming] Received automated A2A network transaction.");
    
    // Pass to unified engine using the Google A2A standard field mapping
    let response = process_core_logic(&payload.message, "A2A_STANDARD");
    
    Json(response)
}