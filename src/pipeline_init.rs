//! Ensures the user has a default Base (chain_id=8453) pipeline before x402 commands.
//!
//! The main entry point is [`ensure_default_pipeline`], which resolves the active
//! pipeline ID through a multi-step cache → backend → interactive flow.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::api::FactoApi;
use crate::config;

/// Result of a successful pipeline resolution.
pub struct InitResult {
    pub pipeline_id: String,
}

// ── Backend response shapes ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RouteResponse {
    id: String,
    chain_id: u64,
    status: String,
}

#[derive(Debug, Deserialize)]
struct UserPreferences {
    #[serde(default)]
    default_pipeline_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct SetDefaultPipeline {
    default_pipeline_id: String,
}

// A generic wrapper used for PUT /v1/user/preferences which returns an object
// we don't need to inspect.
#[derive(Debug, Deserialize)]
struct AckResponse {}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CliMeta {
    pub frontend_url: String,
    pub pipeline_create_url: String,
}

// ── Cache TTL ─────────────────────────────────────────────────────────────

const CACHE_TTL_SECS: i64 = 3600; // 1 hour
const FRONTEND_CACHE_TTL_SECS: i64 = 3600; // 1 hour

fn cache_is_fresh(cached_at: &chrono::DateTime<chrono::Utc>) -> bool {
    let age = chrono::Utc::now().signed_duration_since(*cached_at);
    age.num_seconds() < CACHE_TTL_SECS
}

fn frontend_cache_is_fresh(cached_at: &chrono::DateTime<chrono::Utc>) -> bool {
    let age = chrono::Utc::now().signed_duration_since(*cached_at);
    age.num_seconds() < FRONTEND_CACHE_TTL_SECS
}

// ── Backend helpers ───────────────────────────────────────────────────────

/// Fetch a single route by ID; returns `None` if not found (404).
async fn fetch_route(api: &FactoApi, token: &str, route_id: &str) -> Result<Option<RouteResponse>> {
    let path = format!("/v1/routes/{route_id}");
    let result: Result<RouteResponse> = api.get(&path, Some(token)).await;
    match result {
        Ok(r) => Ok(Some(r)),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("404") || msg.contains("not found") {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

/// Returns `true` if the route is active and on Base (8453).
fn is_valid_base_route(r: &RouteResponse) -> bool {
    r.chain_id == 8453 && r.status.to_lowercase() == "active"
}

/// Fetch user preferences from the backend.
async fn fetch_preferences(api: &FactoApi, token: &str) -> Result<UserPreferences> {
    api.get("/v1/user/preferences", Some(token)).await
}

/// Fetch all routes belonging to the current user.
async fn fetch_my_routes(api: &FactoApi, token: &str) -> Result<Vec<RouteResponse>> {
    api.get("/v1/routes/me", Some(token)).await
}

/// Persist the chosen pipeline ID as the default on the backend and in local cache.
async fn set_default(api: &FactoApi, token: &str, pipeline_id: &str) -> Result<()> {
    let body = SetDefaultPipeline {
        default_pipeline_id: pipeline_id.to_string(),
    };
    let _: AckResponse = api
        .put_authenticated("/v1/user/preferences", &body, token)
        .await
        .context("Failed to set default pipeline on backend")?;

    // Update local config cache.
    let mut cfg = config::load_config();
    cfg.default_pipeline_id = Some(pipeline_id.to_string());
    cfg.default_pipeline_cached_at = Some(chrono::Utc::now());
    config::save_config(&cfg).context("Failed to save pipeline cache to config")?;

    Ok(())
}

// ── Frontend URL helper ───────────────────────────────────────────────────

fn legacy_frontend_url() -> &'static str {
    let cfg = config::load_config();
    match cfg.env.as_str() {
        "prod" => "https://facto.xyz",
        _ => "https://facto-pay-monad-advanced.vercel.app",
    }
}

fn normalize_frontend_url(frontend_url: &str) -> String {
    frontend_url.trim_end_matches('/').to_string()
}

pub fn pipeline_create_url_from_frontend(frontend_url: &str) -> String {
    format!(
        "{}/pipelines/create?source=cli&chain=base",
        normalize_frontend_url(frontend_url)
    )
}

pub fn pipeline_detail_url(frontend_url: &str, pipeline_id: &str) -> String {
    format!(
        "{}/pipelines/{}",
        normalize_frontend_url(frontend_url),
        pipeline_id
    )
}

pub fn charge_url(frontend_url: &str, charge_id: &str) -> String {
    format!(
        "{}/charges/{charge_id}",
        normalize_frontend_url(frontend_url)
    )
}

fn legacy_cli_meta() -> CliMeta {
    let frontend_url = legacy_frontend_url().to_string();
    CliMeta {
        pipeline_create_url: pipeline_create_url_from_frontend(&frontend_url),
        frontend_url,
    }
}

pub fn cached_or_legacy_cli_meta() -> CliMeta {
    let cfg = config::load_config();
    if let (Some(frontend_url), Some(cached_at)) = (
        cfg.frontend_url.as_ref(),
        cfg.frontend_url_cached_at.as_ref(),
    ) {
        if frontend_cache_is_fresh(cached_at) {
            let normalized = normalize_frontend_url(frontend_url);
            return CliMeta {
                pipeline_create_url: pipeline_create_url_from_frontend(&normalized),
                frontend_url: normalized,
            };
        }
    }
    legacy_cli_meta()
}

pub fn cache_cli_meta(meta: &CliMeta) -> Result<()> {
    let mut cfg = config::load_config();
    cfg.frontend_url = Some(normalize_frontend_url(&meta.frontend_url));
    cfg.frontend_url_cached_at = Some(chrono::Utc::now());
    config::save_config(&cfg).context("Failed to save frontend URL cache to config")
}

async fn fetch_cli_meta(api: &FactoApi) -> Result<CliMeta> {
    let meta: CliMeta = api
        .get("/v1/cli/meta", None)
        .await
        .context("Failed to fetch CLI browser surfaces")?;
    let frontend_url = normalize_frontend_url(&meta.frontend_url);
    Ok(CliMeta {
        pipeline_create_url: if meta.pipeline_create_url.trim().is_empty() {
            pipeline_create_url_from_frontend(&frontend_url)
        } else {
            meta.pipeline_create_url
        },
        frontend_url,
    })
}

pub async fn resolve_cli_meta(api: Option<&FactoApi>) -> CliMeta {
    let cfg = config::load_config();
    if let (Some(frontend_url), Some(cached_at)) = (
        cfg.frontend_url.as_ref(),
        cfg.frontend_url_cached_at.as_ref(),
    ) {
        if frontend_cache_is_fresh(cached_at) {
            let normalized = normalize_frontend_url(frontend_url);
            return CliMeta {
                pipeline_create_url: pipeline_create_url_from_frontend(&normalized),
                frontend_url: normalized,
            };
        }
    }

    if let Some(api) = api {
        if let Ok(meta) = fetch_cli_meta(api).await {
            let _ = cache_cli_meta(&meta);
            return meta;
        }
    }

    legacy_cli_meta()
}

// ── Interactive prompt helper ─────────────────────────────────────────────

fn prompt_choice(label: &str, max: usize) -> Result<usize> {
    loop {
        eprint!("{label} (1–{max}): ");
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .context("Failed to read user input")?;
        let trimmed = line.trim();
        if let Ok(n) = trimmed.parse::<usize>() {
            if n >= 1 && n <= max {
                return Ok(n - 1); // 0-based index
            }
        }
        eprintln!("Please enter a number between 1 and {max}.");
    }
}

// ── Main public function ──────────────────────────────────────────────────

/// Ensures a default Base pipeline exists, resolving it through cache → backend → interactive flow.
///
/// * `api`   — authenticated API client
/// * `token` — Bearer token for API calls
/// * `terse` — when `true`, skip interactive prompts and bail with an error instead
pub async fn ensure_default_pipeline(
    api: &FactoApi,
    token: &str,
    terse: bool,
) -> Result<InitResult> {
    // ── Step 1: check local cache ────────────────────────────────────────
    let cfg = config::load_config();
    if let (Some(cached_id), Some(cached_at)) =
        (&cfg.default_pipeline_id, &cfg.default_pipeline_cached_at)
    {
        if cache_is_fresh(cached_at) {
            // Validate the cached route is still active on the backend.
            match fetch_route(api, token, cached_id).await? {
                Some(r) if is_valid_base_route(&r) => {
                    return Ok(InitResult { pipeline_id: r.id });
                }
                _ => {
                    // Stale or invalid — clear cache and fall through.
                    let mut cfg2 = config::load_config();
                    cfg2.default_pipeline_id = None;
                    cfg2.default_pipeline_cached_at = None;
                    let _ = config::save_config(&cfg2);
                }
            }
        }
    }

    // ── Step 2: check backend user preferences ───────────────────────────
    if let Ok(prefs) = fetch_preferences(api, token).await {
        if let Some(pref_id) = prefs.default_pipeline_id {
            if let Some(r) = fetch_route(api, token, &pref_id).await? {
                if is_valid_base_route(&r) {
                    // Cache locally.
                    let mut cfg2 = config::load_config();
                    cfg2.default_pipeline_id = Some(r.id.clone());
                    cfg2.default_pipeline_cached_at = Some(chrono::Utc::now());
                    let _ = config::save_config(&cfg2);

                    return Ok(InitResult { pipeline_id: r.id });
                }
            }
        }
    }

    // ── Step 3: fetch all user routes and filter to Base ─────────────────
    let all_routes = fetch_my_routes(api, token)
        .await
        .context("Failed to fetch routes")?;

    let mut base_routes: Vec<RouteResponse> =
        all_routes.into_iter().filter(is_valid_base_route).collect();

    // ── Step 3a: no Base routes ──────────────────────────────────────────
    if base_routes.is_empty() {
        let create_url = resolve_cli_meta(Some(api)).await.pipeline_create_url;

        if terse {
            bail!(
                "No active Base pipeline found. Run `facto pipeline create` or create one at: {create_url}"
            );
        }

        // Interactive: open browser and poll.
        eprintln!("No active Base (chain_id=8453) pipeline found.");
        eprintln!("Opening browser to create one: {create_url}");
        let _ = open::that(&create_url);
        eprintln!("Waiting for a Base pipeline to appear (polling every 3s, up to 5 min)…");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let routes = fetch_my_routes(api, token).await.unwrap_or_default();
            base_routes = routes.into_iter().filter(is_valid_base_route).collect();
            if !base_routes.is_empty() {
                eprintln!("Base pipeline detected.");
                break;
            }
            if std::time::Instant::now() >= deadline {
                bail!("Timed out waiting for a Base pipeline. Create one at: {create_url}");
            }
        }
    }

    // ── Step 3b: exactly one Base route — auto-select ────────────────────
    if base_routes.len() == 1 {
        let route = base_routes.remove(0);
        eprintln!("Auto-selected Base pipeline: {}", route.id);
        set_default(api, token, &route.id).await?;
        return Ok(InitResult {
            pipeline_id: route.id,
        });
    }

    // ── Step 3c: multiple Base routes ────────────────────────────────────
    if terse {
        bail!("Multiple Base pipelines found. Specify one with --pipeline <id>.");
    }

    // Interactive: display numbered list and prompt.
    eprintln!("Multiple Base pipelines found. Select one to use as default:");
    for (i, r) in base_routes.iter().enumerate() {
        eprintln!("  {}. {}", i + 1, r.id);
    }

    let idx = prompt_choice("Enter number", base_routes.len())?;
    let chosen = base_routes.remove(idx);
    set_default(api, token, &chosen.id).await?;

    Ok(InitResult {
        pipeline_id: chosen.id,
    })
}
