use serde::{Deserialize, Serialize};
use shared::signaling::IceConfigPayload;
use std::env;
use std::fmt;
use thiserror::Error;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;

/// Wrapper that redacts its contents in `Debug` output.
/// Used to prevent credentials from appearing in logs.
/// Requirements: 7.4, 8.1, 8.2, 16.1
pub struct Sensitive<T>(pub T);

impl<T> fmt::Debug for Sensitive<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl<T: Clone> Clone for Sensitive<T> {
    fn clone(&self) -> Self {
        Sensitive(self.0.clone())
    }
}

impl<T: PartialEq> PartialEq for Sensitive<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct IceConfig {
    pub stun_urls: Vec<String>,
    pub turn_urls: Vec<String>,
    pub turn_username: String,
    /// TURN credential — redacted in Debug output to prevent log leakage.
    pub turn_credential: String,
}

impl fmt::Debug for IceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IceConfig")
            .field("stun_urls", &self.stun_urls)
            .field("turn_urls", &self.turn_urls)
            .field("turn_username", &self.turn_username)
            .field("turn_credential", &Sensitive(&self.turn_credential))
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Missing required environment variable: {0}")]
    MissingEnvVar(String),
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    JsonError(#[from] serde_json::Error),
}

fn parse_comma_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

impl IceConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.stun_urls.is_empty() {
            return Err(ConfigError::InvalidConfig(
                "At least one STUN URL is required".to_string(),
            ));
        }

        if self.turn_urls.is_empty() {
            return Err(ConfigError::InvalidConfig(
                "At least one TURN URL is required".to_string(),
            ));
        }

        Ok(())
    }

    /// Load ICE configuration from environment variables.
    ///
    /// Returns `Ok(None)` if `WAVIS_STUN_URLS` is absent, meaning env-based ICE
    /// configuration is not enabled.
    /// Returns `Ok(Some(config))` when all required variables are present and
    /// valid.
    /// Returns `Err` if the variables are partially configured or invalid so
    /// startup fails closed.
    ///
    /// Environment variables:
    /// - `WAVIS_STUN_URLS`: Comma-separated STUN URLs such as
    ///   `stun:stun.example.com:19302`. At least one non-empty entry is
    ///   required.
    /// - `WAVIS_TURN_URLS`: Comma-separated TURN URLs such as
    ///   `turn:turn.example.com:3478`. Required when `WAVIS_STUN_URLS` is set.
    /// - `WAVIS_TURN_USERNAME`: TURN server username. Required when
    ///   `WAVIS_STUN_URLS` is set.
    /// - `WAVIS_TURN_CREDENTIAL`: TURN server password. Required when
    ///   `WAVIS_STUN_URLS` is set. Redacted by `Debug` output on `IceConfig`.
    pub(crate) fn try_from_env() -> Result<Option<Self>, ConfigError> {
        let stun_urls_str = match env::var("WAVIS_STUN_URLS") {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };

        let turn_urls_str = env::var("WAVIS_TURN_URLS")
            .map_err(|_| ConfigError::MissingEnvVar("WAVIS_TURN_URLS".to_string()))?;
        let turn_username = env::var("WAVIS_TURN_USERNAME")
            .map_err(|_| ConfigError::MissingEnvVar("WAVIS_TURN_USERNAME".to_string()))?;
        let turn_credential = env::var("WAVIS_TURN_CREDENTIAL")
            .map_err(|_| ConfigError::MissingEnvVar("WAVIS_TURN_CREDENTIAL".to_string()))?;

        let config = IceConfig {
            stun_urls: parse_comma_list(&stun_urls_str),
            turn_urls: parse_comma_list(&turn_urls_str),
            turn_username,
            turn_credential,
        };
        config.validate()?;

        Ok(Some(config))
    }

    /// Load ICE configuration from environment variables or config file.
    ///
    /// Environment variables take precedence:
    /// - WAVIS_STUN_URLS: comma-separated list of STUN server URLs
    /// - WAVIS_TURN_URLS: comma-separated list of TURN server URLs
    /// - WAVIS_TURN_USERNAME: TURN server username
    /// - WAVIS_TURN_CREDENTIAL: TURN server credential
    ///
    /// If environment variables are not set, attempts to load from config.json
    /// in the current directory.
    pub fn load() -> Result<Self, ConfigError> {
        if let Some(config) = Self::try_from_env()? {
            return Ok(config);
        }

        let config_content = std::fs::read_to_string("config.json")?;
        let config: IceConfig = serde_json::from_str(&config_content)?;
        config.validate()?;

        Ok(config)
    }

    /// Construct an `IceConfig` from a server-issued `IceConfigPayload` (from `Joined` response).
    ///
    /// Credentials are held in memory only — never written to disk.
    /// Requirements: 3.2, 3.3, 3.4
    pub fn from_server(payload: IceConfigPayload) -> Self {
        IceConfig {
            stun_urls: payload.stun_urls,
            turn_urls: payload.turn_urls,
            turn_username: payload.turn_username,
            turn_credential: payload.turn_credential,
        }
    }

    /// Convert IceConfig to webrtc-rs RTCConfiguration.
    ///
    /// Creates RTCIceServer entries for both STUN and TURN servers,
    /// with credentials applied to TURN servers.
    pub fn to_rtc_config(&self) -> RTCConfiguration {
        let mut ice_servers = Vec::new();

        // Add STUN servers
        for stun_url in &self.stun_urls {
            ice_servers.push(RTCIceServer {
                urls: vec![stun_url.clone()],
                username: String::new(),
                credential: String::new(),
                credential_type: Default::default(),
            });
        }

        // Add TURN servers with credentials
        for turn_url in &self.turn_urls {
            ice_servers.push(RTCIceServer {
                urls: vec![turn_url.clone()],
                username: self.turn_username.clone(),
                credential: self.turn_credential.clone(),
                credential_type: Default::default(),
            });
        }

        RTCConfiguration {
            ice_servers,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to ensure tests that modify env vars run sequentially
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_ice_env_vars() {
        env::remove_var("WAVIS_STUN_URLS");
        env::remove_var("WAVIS_TURN_URLS");
        env::remove_var("WAVIS_TURN_USERNAME");
        env::remove_var("WAVIS_TURN_CREDENTIAL");
    }

    #[test]
    fn test_to_rtc_config_creates_valid_configuration() {
        let config = IceConfig {
            stun_urls: vec!["stun:stun.l.google.com:19302".to_string()],
            turn_urls: vec!["turn:turn.example.com:3478".to_string()],
            turn_username: "testuser".to_string(),
            turn_credential: "testpass".to_string(),
        };

        let rtc_config = config.to_rtc_config();

        assert_eq!(rtc_config.ice_servers.len(), 2);

        // Verify STUN server
        assert_eq!(rtc_config.ice_servers[0].urls.len(), 1);
        assert_eq!(
            rtc_config.ice_servers[0].urls[0],
            "stun:stun.l.google.com:19302"
        );
        assert_eq!(rtc_config.ice_servers[0].username, "");
        assert_eq!(rtc_config.ice_servers[0].credential, "");

        // Verify TURN server
        assert_eq!(rtc_config.ice_servers[1].urls.len(), 1);
        assert_eq!(
            rtc_config.ice_servers[1].urls[0],
            "turn:turn.example.com:3478"
        );
        assert_eq!(rtc_config.ice_servers[1].username, "testuser");
        assert_eq!(rtc_config.ice_servers[1].credential, "testpass");
    }

    #[test]
    fn test_to_rtc_config_handles_multiple_servers() {
        let config = IceConfig {
            stun_urls: vec![
                "stun:stun1.example.com:19302".to_string(),
                "stun:stun2.example.com:19302".to_string(),
            ],
            turn_urls: vec![
                "turn:turn1.example.com:3478".to_string(),
                "turn:turn2.example.com:3478".to_string(),
            ],
            turn_username: "user".to_string(),
            turn_credential: "pass".to_string(),
        };

        let rtc_config = config.to_rtc_config();

        // Should have 4 ice_servers total (2 STUN + 2 TURN)
        assert_eq!(rtc_config.ice_servers.len(), 4);

        // Verify all STUN servers have no credentials
        assert_eq!(rtc_config.ice_servers[0].username, "");
        assert_eq!(rtc_config.ice_servers[1].username, "");

        // Verify all TURN servers have credentials
        assert_eq!(rtc_config.ice_servers[2].username, "user");
        assert_eq!(rtc_config.ice_servers[3].username, "user");
    }

    #[test]
    fn test_load_validates_stun_urls_required() {
        let _lock = ENV_LOCK.lock().unwrap();

        clear_ice_env_vars();
        env::set_var("WAVIS_STUN_URLS", "");
        env::set_var("WAVIS_TURN_URLS", "turn:turn.example.com:3478");
        env::set_var("WAVIS_TURN_USERNAME", "user");
        env::set_var("WAVIS_TURN_CREDENTIAL", "pass");

        let result = IceConfig::load();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidConfig(_)));

        clear_ice_env_vars();
    }

    #[test]
    fn test_load_validates_turn_urls_required() {
        let _lock = ENV_LOCK.lock().unwrap();

        clear_ice_env_vars();
        env::set_var("WAVIS_STUN_URLS", "stun:stun.example.com:19302");
        env::set_var("WAVIS_TURN_URLS", "");
        env::set_var("WAVIS_TURN_USERNAME", "user");
        env::set_var("WAVIS_TURN_CREDENTIAL", "pass");

        let result = IceConfig::load();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidConfig(_)));

        clear_ice_env_vars();
    }

    #[test]
    fn test_try_from_env_returns_none_when_not_set() {
        let _lock = ENV_LOCK.lock().unwrap();

        clear_ice_env_vars();

        let result = IceConfig::try_from_env();
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_try_from_env_missing_turn_urls() {
        let _lock = ENV_LOCK.lock().unwrap();

        clear_ice_env_vars();
        env::set_var("WAVIS_STUN_URLS", "stun:stun.example.com:19302");
        env::set_var("WAVIS_TURN_USERNAME", "user");
        env::set_var("WAVIS_TURN_CREDENTIAL", "pass");

        let result = IceConfig::try_from_env();
        assert!(matches!(
            result,
            Err(ConfigError::MissingEnvVar(ref name)) if name == "WAVIS_TURN_URLS"
        ));

        clear_ice_env_vars();
    }

    #[test]
    fn test_try_from_env_missing_turn_username() {
        let _lock = ENV_LOCK.lock().unwrap();

        clear_ice_env_vars();
        env::set_var("WAVIS_STUN_URLS", "stun:stun.example.com:19302");
        env::set_var("WAVIS_TURN_URLS", "turn:turn.example.com:3478");
        env::set_var("WAVIS_TURN_CREDENTIAL", "pass");

        let result = IceConfig::try_from_env();
        assert!(matches!(
            result,
            Err(ConfigError::MissingEnvVar(ref name)) if name == "WAVIS_TURN_USERNAME"
        ));

        clear_ice_env_vars();
    }

    #[test]
    fn test_load_from_env_success() {
        let _lock = ENV_LOCK.lock().unwrap();

        clear_ice_env_vars();
        env::set_var("WAVIS_STUN_URLS", "stun:stun.example.com:19302");
        env::set_var("WAVIS_TURN_URLS", "turn:turn.example.com:3478");
        env::set_var("WAVIS_TURN_USERNAME", "testuser");
        env::set_var("WAVIS_TURN_CREDENTIAL", "testpass");

        let config = IceConfig::try_from_env()
            .unwrap()
            .expect("env config should be present");
        assert_eq!(config.stun_urls.len(), 1);
        assert_eq!(config.stun_urls[0], "stun:stun.example.com:19302");
        assert_eq!(config.turn_urls.len(), 1);
        assert_eq!(config.turn_urls[0], "turn:turn.example.com:3478");
        assert_eq!(config.turn_username, "testuser");
        assert_eq!(config.turn_credential, "testpass");

        clear_ice_env_vars();
    }

    #[test]
    fn test_load_from_env_multiple_urls() {
        let _lock = ENV_LOCK.lock().unwrap();

        clear_ice_env_vars();
        env::set_var(
            "WAVIS_STUN_URLS",
            "stun:stun1.example.com:19302, stun:stun2.example.com:19302",
        );
        env::set_var(
            "WAVIS_TURN_URLS",
            "turn:turn1.example.com:3478, turn:turn2.example.com:3478",
        );
        env::set_var("WAVIS_TURN_USERNAME", "user");
        env::set_var("WAVIS_TURN_CREDENTIAL", "pass");

        let config = IceConfig::try_from_env()
            .unwrap()
            .expect("env config should be present");
        assert_eq!(config.stun_urls.len(), 2);
        assert_eq!(config.turn_urls.len(), 2);

        clear_ice_env_vars();
    }
}
