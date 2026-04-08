use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Stored credentials for the authenticated Facto user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub token: String,
    pub user_id: String,
    pub email: Option<String>,
    pub expires_at: DateTime<Utc>,
    /// API key for HMAC-authenticated requests (api_key auth mode).
    #[serde(default)]
    pub api_key: Option<String>,
    /// HMAC signing key (required with api_key).
    #[serde(default)]
    pub signing_key: Option<String>,
    /// Authentication mode: "privy" | "api_key" | "dev"
    #[serde(default)]
    pub auth_mode: Option<String>,
    /// Deposit addresses per chain (e.g., {"arbitrum": "0xfc8a..."}).
    /// Set via `facto config --deposit-address arbitrum 0x...`
    #[serde(default)]
    pub deposit_addresses: Option<std::collections::HashMap<String, String>>,
}

impl Credentials {
    /// Get the deposit address for a given chain, or fall back to a default.
    pub fn deposit_address_for(&self, chain_name: &str) -> Option<&str> {
        self.deposit_addresses
            .as_ref()
            .and_then(|m| m.get(chain_name))
            .map(|s| s.as_str())
    }
}

/// Returns `~/.facto/`, creating it if it does not exist.
pub fn facto_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let dir = home.join(".facto");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create directory: {}", dir.display()))?;
    Ok(dir)
}

/// Returns the path to `~/.facto/credentials.json`.
pub fn credentials_path() -> Result<PathBuf> {
    Ok(facto_dir()?.join("credentials.json"))
}

/// Reads and parses the credentials file.
///
/// Returns an error with a helpful message if the file is missing.
pub fn load_credentials() -> Result<Credentials> {
    let path = credentials_path()?;
    let data = std::fs::read_to_string(&path)
        .with_context(|| "Not logged in. Run `facto login` first.".to_string())?;
    let creds: Credentials = serde_json::from_str(&data)
        .with_context(|| format!("Failed to parse credentials at {}", path.display()))?;
    Ok(creds)
}

/// Writes credentials to `~/.facto/credentials.json` as pretty-printed JSON.
pub fn save_credentials(creds: &Credentials) -> Result<()> {
    let path = credentials_path()?;
    let json = serde_json::to_string_pretty(creds).context("Failed to serialize credentials")?;
    std::fs::write(&path, json)
        .with_context(|| format!("Failed to write credentials to {}", path.display()))?;
    Ok(())
}

/// Deletes the credentials file if it exists.
pub fn clear_credentials() -> Result<()> {
    let path = credentials_path()?;
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("Failed to delete credentials at {}", path.display()))?;
    }
    Ok(())
}

// ── Environment config (~/.facto/config.json) ──────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Active environment: "dev" | "prod"
    #[serde(default = "default_env")]
    pub env: String,
    /// Custom API URLs per environment (overrides built-in defaults)
    #[serde(default)]
    pub api_urls: std::collections::HashMap<String, String>,
    /// Cached default pipeline ID (synced from backend).
    #[serde(default)]
    pub default_pipeline_id: Option<String>,
    /// When the pipeline cache was last refreshed.
    #[serde(default)]
    pub default_pipeline_cached_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn default_env() -> String {
    "dev".to_string()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            env: default_env(),
            api_urls: std::collections::HashMap::new(),
            default_pipeline_id: None,
            default_pipeline_cached_at: None,
        }
    }
}

/// Built-in API URLs per environment.
fn builtin_api_url(env: &str) -> &'static str {
    match env {
        "dev" => "https://monad-api.facto.to",
        "prod" => "https://api.facto.xyz",
        _ => "https://monad-api.facto.to",
    }
}

/// Returns the path to `~/.facto/config.json`.
pub fn config_path() -> Result<PathBuf> {
    Ok(facto_dir()?.join("config.json"))
}

/// Loads `~/.facto/config.json`, returns default if missing.
pub fn load_config() -> AppConfig {
    config_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Saves config to `~/.facto/config.json`.
pub fn save_config(cfg: &AppConfig) -> Result<()> {
    let path = config_path()?;
    let json = serde_json::to_string_pretty(cfg).context("Failed to serialize config")?;
    std::fs::write(&path, json)
        .with_context(|| format!("Failed to write config to {}", path.display()))?;
    Ok(())
}

/// Returns the Facto API base URL.
///
/// Priority: `FACTO_API_URL` env var > custom api_urls in config > built-in default for env.
pub fn api_url() -> String {
    if let Ok(url) = std::env::var("FACTO_API_URL") {
        return url;
    }
    let cfg = load_config();
    if let Some(url) = cfg.api_urls.get(&cfg.env) {
        return url.clone();
    }
    builtin_api_url(&cfg.env).to_string()
}
