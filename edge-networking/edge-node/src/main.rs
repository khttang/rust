use zero_trust_core::ZeroTrustConfig;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting Authenticated Zero-Trust Edge Node...");

    // Define local paths to cryptographic identities issued by your PKI infrastructure
    let zt_env = ZeroTrustConfig {
        node_id: "sensor-node-01".to_string(),
        ca_custom_cert: "../../keys/edge-ca.crt".to_string(),
        node_cert: "../../keys/sensor-node-01.crt".to_string(),
        node_private_key: "../../keys/sensor-node-01.key".to_string()
    };

    // Compile secure configurations and open a Zenoh session over mTLS
    let config = zt_env.to_zenoh_config();
    let session = zenoh::open(config).await.map_err(|e| e.to_string())?;

    // Explicitly scope the data publication path to match the Policy Engine
    let telemetry_path = format!("edge/telemetry/{}", zt_env.node_id);
    let publisher = session.declare_publisher(&telemetry_path).await.map_err(|e| e.to_string())?;

    let mut sequence: u64 = 0;
    loop {
        let payload = format!("{{ \"seq\": {}, \"status\": \"secure\" }}", sequence);

        // Publish encrypted, authenticated data payload directly into the edge network
        publisher.put(payload).await.map_err(|e| e.to_string())?;
        println!("Securely put telemetry data on: {}", telemetry_path);

        sequence += 1;
        sleep(Duration::from_secs(5)).await;
    }
}
