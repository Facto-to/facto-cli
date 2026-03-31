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

/// Returns the Facto API base URL.
///
/// Reads `FACTO_API_URL` from the environment; falls back to the production URL.
pub fn api_url() -> String {
    std::env::var("FACTO_API_URL").unwrap_or_else(|_| "https://api.facto.xyz".to_string())
}
