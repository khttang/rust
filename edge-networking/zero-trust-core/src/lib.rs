use zenoh::config::Config;

pub struct ZeroTrustConfig {
    pub node_id: String,
    pub ca_custom_cert: String,
    pub node_cert: String,
    pub node_private_key: String,
}

impl ZeroTrustConfig {
    /// Generates a production-grade Zenoh configuration enforcing strict mTLS.
    pub fn to_zenoh_config(&self) -> Config {
        let mut config = Config::default();

        // 1. Block unauthorized local multicast discovery via the Configuration API
        // This replaces the broken `config.scouting.set_multicast(false)` line.
        config.insert_json5("scouting/multicast/enabled", "false").unwrap();

        // 2. Inject Mutual TLS (mTLS) Cryptographic Materials into the proper link path
        let security_config = format!(
            r#"{{
                "root_ca_certificate": "{}",
                "listen_certificate": "{}",
                "listen_private_key": "{}",
                "connect_certificate": "{}",
                "connect_private_key": "{}"
            }}"#,
            self.ca_custom_cert,
            self.node_cert, self.node_private_key, // Used if listening for incoming connections
            self.node_cert, self.node_private_key  // Used when establishing outbound connections
        );

        // Apply the security configuration JSON to Zenoh's runtime properties
        config.insert_json5("transport/link/tls", &security_config).unwrap();

        config
    }

    /// Policy Engine: Enforces Zero-Trust Least-Privilege Access Control
    pub fn validate_access(&self, identity: &str, resource_path: &str) -> bool {
        // In production, match against a cryptographically signed token or local policy DB
        match (identity, resource_path) {
            ("sensor-node-01", path) if path.starts_with("edge/telemetry/") => true,
            ("gateway-01", path) if path.starts_with("edge/control/") => true,
            _ => false, // Default Deny Strategy
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /*#[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }*/
}
