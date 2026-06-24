use zero_trust_core::ZeroTrustConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {

    println!("Initializing Zero-Trust Edge Gateway...");

    let zt_env = ZeroTrustConfig {
        node_id: "gateway-01".to_string(),
        ca_custom_cert: "../../keys/edge-ca.crt".to_string(),
        node_cert: "../../keys/gateway-01.crt".to_string(),
        node_private_key: "../../keys/gateway-01.key".to_string()
    };

    let config = zt_env.to_zenoh_config();
    let session = zenoh::open(config).await.map_err(|e| e.to_string())?;

    // Subscribe to all telemetry under the edge namespace
    let subscriber = session.declare_subscriber("edge/telemetry/*").await.map_err(|e| e.to_string())?;
    println!("Gateway listening on authenticated 'edge/telemetry/*' channels...");

    while let Ok(sample) = subscriber.recv_async().await {
        let origin_identity = "sensor-node-01"; // Extracted from TLS Client Certificate in production
        let resource_key = sample.key_expr().as_str();

        // Enforce the Zero-Trust check before processing any application payload
        if zt_env.validate_access(origin_identity, resource_key) {
            // Updated syntax for Zenoh 1.0.x payload retrieval
            let data = sample.payload().try_to_string().unwrap_or_default();
            println!("Verified Data Packet from [{}]: {}", resource_key, data);
        } else {
            eprintln!("SECURITY ALERT: Unauthorized access attempt rejected for key: {}", resource_key);
        }
    }

    // Explicitly return Ok to satisfy the Result return constraint
    Ok(())
}
