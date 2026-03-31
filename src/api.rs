use anyhow::{Context, Result, bail};
use hmac::{Hmac, Mac};
use serde::de::DeserializeOwned;
use sha2::Sha256;

use crate::config::{self, Credentials};

/// Thin HTTP client wrapper around the Facto backend API.
pub struct FactoApi {
    client: reqwest::Client,
    base_url: String,
}

impl FactoApi {
    /// Creates a new unauthenticated API client using the configured base URL.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: config::api_url(),
        }
    }

    /// Loads stored credentials and returns both the API client and the credentials.
    pub fn authenticated() -> Result<(Self, Credentials)> {
        let creds = config::load_credentials().context("Failed to load credentials")?;
        Ok((Self::new(), creds))
    }

    /// Sends a POST request with a JSON body and deserializes the response.
    ///
    /// No `Authorization` header is attached.
    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .post(&url)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?;

        self.parse_response(resp).await
    }

    /// Sends a GET request with an optional Bearer token and deserializes the response.
    pub async fn get<T: DeserializeOwned>(&self, path: &str, token: Option<&str>) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.get(&url);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("GET {url} failed"))?;

        self.parse_response(resp).await
    }

    /// Sends a GET request authenticated via HMAC headers.
    pub async fn get_hmac<T: DeserializeOwned>(
        &self,
        path: &str,
        api_key: &str,
        signing_key: &str,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let timestamp = chrono::Utc::now().timestamp().to_string();
        let signature = hmac_sign(signing_key, &timestamp, "GET", path);
        let resp = self
            .client
            .get(&url)
            .header("X-Api-Key", api_key)
            .header("X-Timestamp", &timestamp)
            .header("X-Signature", &signature)
            .send()
            .await
            .with_context(|| format!("GET {url} failed"))?;

        self.parse_response(resp).await
    }

    /// Sends an authenticated POST request (Bearer token) with a JSON body.
    pub async fn post_authenticated<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
        token: &str,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?;

        self.parse_response(resp).await
    }

    /// Sends an authenticated POST request using HMAC headers with a JSON body.
    #[allow(dead_code)]
    pub async fn post_hmac<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
        api_key: &str,
        signing_key: &str,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let timestamp = chrono::Utc::now().timestamp().to_string();
        let signature = hmac_sign(signing_key, &timestamp, "POST", path);
        let resp = self
            .client
            .post(&url)
            .header("X-Api-Key", api_key)
            .header("X-Timestamp", &timestamp)
            .header("X-Signature", &signature)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?;

        self.parse_response(resp).await
    }

    /// Checks the HTTP status and deserializes a successful response.
    async fn parse_response<T: DeserializeOwned>(&self, resp: reqwest::Response) -> Result<T> {
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("API error ({}): {}", status, text);
        }
        resp.json::<T>()
            .await
            .context("Failed to deserialize API response")
    }
}

/// Computes an HMAC-SHA256 signature for API key authentication.
///
/// The signing string is `"{timestamp}.{METHOD}.{path}"`.
fn hmac_sign(signing_key: &str, timestamp: &str, method: &str, path: &str) -> String {
    let signing_string = format!("{timestamp}.{method}.{path}");
    let mut mac =
        Hmac::<Sha256>::new_from_slice(signing_key.as_bytes()).expect("HMAC accepts any key size");
    mac.update(signing_string.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
