mod api;
mod config;
mod interactive;
mod pay;
mod pipeline_init;

use anyhow::{bail, Context as _, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::io::Write as _;

/// Facto CLI — operator setup and DeFi-funded agent payments.
#[derive(Debug, Parser)]
#[command(
    name = "facto",
    version,
    about = "Facto CLI — operator setup and DeFi-funded agent payments"
)]
struct Cli {
    /// Output compact JSON (machine-readable).
    #[arg(short = 't', long = "terse", global = true)]
    terse: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum PipelineAction {
    /// Open the browser to create a payment pipeline.
    Create {
        /// Optional payment protocol for the creation flow.
        #[arg(long, value_parser = ["x402", "mpp"])]
        protocol: Option<String>,
        /// Jump straight to the Base x402 onboarding flow.
        #[arg(long = "x402", conflicts_with_all = ["protocol", "mpp"])]
        base_x402: bool,
        /// Jump straight to the Monad MPP onboarding flow.
        #[arg(long, conflicts_with_all = ["protocol", "base_x402"])]
        mpp: bool,
        /// Optional target chain for the creation flow.
        #[arg(long, value_parser = ["base", "monad"])]
        chain: Option<String>,
        /// Preload the create flow for a specific target payment URL.
        #[arg(long)]
        for_pay: Option<String>,
    },
    /// Show details of a specific pipeline.
    Show {
        /// Pipeline (route) ID.
        id: String,
    },
    /// Set or view the default pipeline for x402 payments.
    Default {
        /// Pipeline ID to set as default. Omit to show current default.
        id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Authenticate the operator via browser (Privy OAuth) or dev token.
    Login {
        /// Reserved for future CI/server support. Not supported for CLI user commands yet.
        #[arg(long)]
        api_key: Option<String>,
        /// HMAC signing key (required with --api-key).
        #[arg(long)]
        signing_key: Option<String>,
        /// Dev token for local testing (format: "dev:<user_id>").
        #[arg(long)]
        dev_token: Option<String>,
    },

    /// Show a diagnostic summary of the current operator account.
    Whoami,

    /// Manage DeFi pipelines: list, view details, or set default.
    #[command(visible_alias = "pipeline")]
    Pipelines {
        #[command(subcommand)]
        action: Option<PipelineAction>,
    },

    /// Withdraw USDC from a DeFi position and send to a recipient.
    Fund {
        /// Amount of USDC to withdraw (e.g. "10" or "0.50").
        #[arg(short, long)]
        amount: String,

        /// Recipient address. Must be in the pipeline's allowlist.
        /// Defaults to your own EOA (self-funding).
        /// If missing from the allowlist, CLI will offer to add it before execution.
        #[arg(short = 'r', long)]
        to: Option<String>,

        /// Pipeline (route) ID to use. If omitted, uses the first supported pipeline.
        /// Run `facto pipelines` to see available IDs.
        #[arg(short, long)]
        pipeline: Option<String>,

        /// Preview the transaction without executing it.
        #[arg(long)]
        dry_run: bool,
    },

    /// Review past funding and x402 payment activity.
    History,

    /// Call a paid API directly with automatic x402 payment handling.
    Pay {
        /// HTTP method (GET, POST, PUT, DELETE).
        method: String,
        /// Target API URL.
        url: String,
        /// Payment protocol override (auto, x402, mpp).
        #[arg(long, default_value = "auto", value_parser = ["auto", "x402", "mpp"])]
        protocol: String,
        /// Custom HTTP headers (-H "Key: Value"), can be repeated.
        #[arg(short = 'H', long = "header")]
        headers: Vec<String>,
        /// Request body for POST/PUT (JSON string).
        #[arg(long)]
        data: Option<String>,
        /// Max payment amount in USDC (human-readable, e.g. "0.50").
        #[arg(long)]
        max_amount: Option<String>,
        /// Preview payment without executing.
        #[arg(long)]
        dry_run: bool,
        /// Chain ID. If omitted, auto-detects from your active pipeline.
        #[arg(long)]
        chain: Option<u64>,
        /// Skip payment confirmation prompt.
        #[arg(short, long)]
        yes: bool,
    },

    /// Inspect server wallet USDC available for x402 payments.
    Balance {
        /// Chain ID. If omitted, auto-detects from your active pipeline.
        #[arg(long)]
        chain: Option<u64>,
    },

    /// Discover x402-enabled services for manual use or agent planning.
    Services {
        /// Search keyword to filter services (e.g. "weather", "ai", "search").
        query: Option<String>,

        /// Filter by category (ai, web, blockchain, data).
        #[arg(long)]
        category: Option<String>,

        /// Interactive mode: select a service and pay directly.
        #[arg(short, long)]
        interactive: bool,
    },

    /// Configure environment, deposit addresses, and settings.
    Config {
        /// Switch environment: dev or prod.
        #[arg(long, value_parser = ["dev", "prod"])]
        env: Option<String>,

        /// Set deposit address for cross-chain: --deposit-address <chain> <address>
        /// Example: --deposit-address arbitrum 0xfc8a...63c9
        #[arg(long, num_args = 2, value_names = ["CHAIN", "ADDRESS"])]
        deposit_address: Option<Vec<String>>,

        /// Show current configuration.
        #[arg(long)]
        show: bool,
    },

    /// Upgrade facto CLI to the latest published version.
    Upgrade {
        /// Skip confirmation prompt.
        #[arg(short, long)]
        yes: bool,
    },

    /// Clear local credentials.
    Logout,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Startup update check: only in interactive (TTY) non-terse mode,
    // skip for upgrade command itself, throttled to every 4 hours.
    if !cli.terse
        && atty::is(atty::Stream::Stderr)
        && !matches!(cli.command, Commands::Upgrade { .. })
    {
        if let Some(latest) = check_update_throttled().await {
            let current = env!("CARGO_PKG_VERSION");
            if latest != current {
                eprintln!(
                    "\x1b[36m⬆ facto v{latest} available (current: v{current}). Run `facto upgrade` to update.\x1b[0m"
                );
            }
        }
    }

    match cli.command {
        Commands::Login {
            api_key,
            signing_key,
            dev_token,
        } => cmd_login(cli.terse, api_key, signing_key, dev_token).await,
        Commands::Whoami => cmd_whoami(cli.terse).await,
        Commands::Pipelines { action } => match action {
            None => cmd_pipelines(cli.terse).await,
            Some(PipelineAction::Create {
                protocol,
                base_x402,
                mpp,
                chain,
                for_pay,
            }) => {
                cmd_pipeline_create(
                    cli.terse,
                    protocol.as_deref(),
                    base_x402,
                    mpp,
                    chain.as_deref(),
                    for_pay.as_deref(),
                )
                .await
            }
            Some(PipelineAction::Show { id }) => cmd_pipeline_show(&id, cli.terse).await,
            Some(PipelineAction::Default { id }) => {
                cmd_pipeline_default(id.as_deref(), cli.terse).await
            }
        },
        Commands::Fund {
            amount,
            to,
            pipeline,
            dry_run,
        } => {
            cmd_fund(
                &amount,
                to.as_deref(),
                pipeline.as_deref(),
                dry_run,
                cli.terse,
            )
            .await
        }
        Commands::History => cmd_history(cli.terse).await,
        Commands::Pay {
            method,
            url,
            protocol,
            headers,
            data,
            max_amount,
            dry_run,
            chain,
            yes,
        } => {
            cmd_pay(
                &method,
                &url,
                &protocol,
                &headers,
                data.as_deref(),
                max_amount.as_deref(),
                dry_run,
                chain,
                cli.terse,
                yes,
            )
            .await
        }
        Commands::Balance { chain } => cmd_balance(chain, cli.terse).await,
        Commands::Services {
            query,
            category,
            interactive,
        } => {
            if interactive {
                let (api, creds) = api::FactoApi::authenticated()?;
                ensure_user_bearer_auth(&creds)?;
                let init =
                    pipeline_init::ensure_default_pipeline(&api, &creds.token, cli.terse).await?;
                interactive::interactive_service_flow(
                    query.as_deref(),
                    category.as_deref(),
                    &api,
                    &creds.token,
                    Some(&init.pipeline_id),
                )
                .await
            } else {
                cmd_services(query.as_deref(), category.as_deref(), cli.terse).await
            }
        }
        Commands::Config {
            env,
            deposit_address,
            show,
        } => cmd_config(env, deposit_address, show, cli.terse),
        Commands::Upgrade { yes } => cmd_upgrade(yes, cli.terse).await,
        Commands::Logout => cmd_logout(),
    }
}

// ---------------------------------------------------------------------------
// API response types (login flow)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SessionCreateResponse {
    session_id: String,
    auth_url: String,
    #[serde(default)]
    frontend_url: Option<String>,
    #[serde(default)]
    pipeline_create_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionPollResponse {
    status: String,
    token: Option<String>,
    user_id: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LoginRouteStatus {
    id: String,
    chain_id: u64,
    status: String,
}

#[derive(Debug, Clone)]
struct PostLoginReadiness {
    status: &'static str,
    pipeline_id: Option<String>,
    continue_url: Option<String>,
    next_command: Option<&'static str>,
}

fn session_cli_meta(session: &SessionCreateResponse) -> Option<pipeline_init::CliMeta> {
    session.frontend_url.as_deref().map(|frontend_url| {
        let normalized = frontend_url.trim_end_matches('/').to_string();
        pipeline_init::CliMeta {
            pipeline_create_url: session
                .pipeline_create_url
                .clone()
                .unwrap_or_else(|| pipeline_init::pipeline_create_url_from_frontend(&normalized)),
            frontend_url: normalized,
        }
    })
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

async fn cmd_login(
    terse: bool,
    api_key: Option<String>,
    signing_key: Option<String>,
    dev_token: Option<String>,
) -> Result<()> {
    let preserved_deposit_addresses = config::load_credentials()
        .ok()
        .and_then(|creds| creds.deposit_addresses);

    // --- Mode 1: dev token ---
    if let Some(token) = dev_token {
        let user_id = token
            .strip_prefix("dev:")
            .ok_or_else(|| anyhow::anyhow!("Dev token must be in the format \"dev:<user_id>\""))?
            .to_string();

        config::save_credentials(&config::Credentials {
            token: token.clone(),
            user_id: user_id.clone(),
            email: None,
            expires_at: "2099-12-31T00:00:00Z".parse().unwrap(),
            api_key: None,
            signing_key: None,
            auth_mode: Some("dev".to_string()),
            deposit_addresses: preserved_deposit_addresses,
        })?;

        if terse {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "status": "authenticated",
                    "auth_mode": "dev",
                    "user_id": user_id,
                    "token": token,
                }))?
            );
        } else {
            println!("✅ Dev token saved");
            println!("User ID:    {user_id}");
            println!("Auth mode:  dev (not safe for production)");
        }
        return Ok(());
    }

    // --- Mode 2: API key + signing key ---
    if let Some(_key) = api_key {
        let _sk = signing_key
            .ok_or_else(|| anyhow::anyhow!("--signing-key is required with --api-key"))?;
        bail!(
            "API key authentication is not supported for facto-cli user commands yet. Use browser login or --dev-token."
        );
    }

    // --- Mode 3: browser-based Privy login (default) ---
    use tokio::time::{sleep, Duration};

    let facto = api::FactoApi::new();

    // 1. Create a CLI session → get session_id + auth_url
    let session: SessionCreateResponse = facto
        .post("/v1/cli/session", &serde_json::json!({}))
        .await?;

    if let Some(meta) = session_cli_meta(&session) {
        let _ = pipeline_init::cache_cli_meta(&meta);
    }

    // 2. Open the browser
    if !terse {
        println!("Opening browser for authentication...");
        println!("→ {}", session.auth_url);
    }
    open::that(&session.auth_url).ok(); // best-effort; don't abort if it fails

    // 3. Poll until authenticated
    let poll_path = format!("/v1/cli/session/{}", session.session_id);

    if !terse {
        print!("Waiting for login... ");
        std::io::stdout().flush().ok();
    }

    loop {
        sleep(Duration::from_secs(2)).await;

        let poll: SessionPollResponse = facto.get(&poll_path, None).await?;

        match poll.status.as_str() {
            "authenticated" => {
                let token = poll.token.unwrap_or_default();
                let user_id = poll.user_id.unwrap_or_default();
                let email = poll.email.clone();

                // Save credentials
                config::save_credentials(&config::Credentials {
                    token: token.clone(),
                    user_id: user_id.clone(),
                    email: email.clone(),
                    expires_at: chrono::Utc::now() + chrono::Duration::hours(24),
                    api_key: None,
                    signing_key: None,
                    auth_mode: Some("privy".to_string()),
                    deposit_addresses: preserved_deposit_addresses.clone(),
                })?;

                if terse {
                    let readiness = detect_post_login_readiness(&facto, &token).await.ok();
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "status": "authenticated",
                            "user_id": user_id,
                            "next_command": readiness.as_ref().and_then(|state| state.next_command),
                            "continue_url": readiness.as_ref().and_then(|state| state.continue_url.as_deref()),
                        }))?
                    );
                } else {
                    println!("✅ Authenticated");
                    if let Some(e) = &email {
                        println!("Welcome, {e}");
                    }
                    if let Ok(readiness) = detect_post_login_readiness(&facto, &token).await {
                        print_post_login_readiness_hint(&readiness);
                    }
                }
                return Ok(());
            }
            "pending" => {
                if !terse {
                    print!(".");
                    std::io::stdout().flush().ok();
                }
            }
            other => {
                bail!("Unexpected session status: {other}");
            }
        }
    }
}

fn ensure_user_bearer_auth(creds: &config::Credentials) -> Result<()> {
    if matches!(creds.auth_mode.as_deref(), Some("api_key")) {
        bail!(
            "API key authentication is not supported for facto-cli user commands yet. Use browser login or --dev-token."
        );
    }
    if creds.token.trim().is_empty() {
        bail!("Missing bearer token. Run `facto login` again.");
    }
    Ok(())
}

fn cached_pipeline_create_url() -> String {
    pipeline_init::cached_or_legacy_cli_meta().pipeline_create_url
}

async fn detect_post_login_readiness(
    api: &api::FactoApi,
    token: &str,
) -> Result<PostLoginReadiness> {
    let cli_meta = pipeline_init::resolve_cli_meta(Some(api)).await;
    let routes: Vec<LoginRouteStatus> = api.get("/v1/routes/me", Some(token)).await?;
    let active_routes: Vec<LoginRouteStatus> = routes
        .into_iter()
        .filter(|route| route.status.eq_ignore_ascii_case("active"))
        .collect();

    if active_routes.is_empty() {
        return Ok(PostLoginReadiness {
            status: "needs_pipeline",
            pipeline_id: None,
            continue_url: Some(cli_meta.pipeline_create_url),
            next_command: Some("facto pipeline create"),
        });
    }

    Ok(PostLoginReadiness {
        status: "configured",
        pipeline_id: active_routes.first().map(|route| route.id.clone()),
        continue_url: None,
        next_command: None,
    })
}

fn print_post_login_readiness_hint(readiness: &PostLoginReadiness) {
    println!();
    match readiness.status {
        "needs_pipeline" => {
            println!("Operator setup incomplete.");
            println!("Create a payment pipeline:");
            println!(
                "  {}",
                readiness.next_command.unwrap_or("facto pipeline create")
            );
            if let Some(url) = readiness.continue_url.as_deref() {
                println!("  Browser: {url}");
            }
            println!(
                "Then inspect `facto balance` / `facto pipelines`, or hand control to your agent."
            );
        }
        "configured" => {
            println!("Operator setup ready.");
            if let Some(pipeline_id) = readiness.pipeline_id.as_deref() {
                println!("Pipeline: {pipeline_id}");
            }
            println!("Operator checks: `facto balance` / `facto pipelines`");
            println!("Agent handoff: https://monad-api.facto.to/SKILL.md");
        }
        _ => {}
    }
}

fn format_usdc_amount(raw: u64) -> String {
    let value = raw as f64 / 1_000_000.0;
    if value >= 1.0 {
        format!("${value:.2}")
    } else if value >= 0.01 {
        format!("${value:.4}")
    } else {
        format!("${value:.6}")
    }
}

fn extract_settlement_tx_hash(value: &serde_json::Value) -> Option<String> {
    const KEYS: &[&str] = &[
        "settlement_tx_hash",
        "settlementTxHash",
        "tx_hash",
        "txHash",
        "transaction_hash",
        "transactionHash",
    ];
    for key in KEYS {
        if let Some(tx_hash) = value.get(key).and_then(|v| v.as_str()) {
            let trimmed = tx_hash.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    match value {
        serde_json::Value::Object(map) => map.values().find_map(extract_settlement_tx_hash),
        serde_json::Value::Array(items) => items.iter().find_map(extract_settlement_tx_hash),
        _ => None,
    }
}

fn charge_settlement_tx_hash(charge: &serde_json::Value) -> Option<String> {
    charge
        .get("transaction_hash")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            charge
                .get("x402_response_body")
                .and_then(|v| v.as_str())
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|json| extract_settlement_tx_hash(&json))
        })
}

fn charge_response_success_flag(charge: &serde_json::Value) -> Option<bool> {
    charge
        .get("x402_response_body")
        .and_then(|v| v.as_str())
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|json| json.get("success").and_then(|v| v.as_bool()))
}

async fn cmd_whoami(terse: bool) -> Result<()> {
    let (api, creds) = api::FactoApi::authenticated()?;
    ensure_user_bearer_auth(&creds)?;

    let me: serde_json::Value = api
        .get("/v1/auth/me", Some(&creds.token))
        .await
        .context("Failed to fetch authenticated account")?;

    let routes: Vec<serde_json::Value> = api
        .get("/v1/routes/me", Some(&creds.token))
        .await
        .context("Failed to fetch routes")?;

    let pipeline_count = routes.len();
    let readiness = detect_post_login_readiness(&api, &creds.token).await.ok();
    let email = me["email"]
        .as_str()
        .filter(|email| !email.is_empty())
        .or(creds.email.as_deref())
        .unwrap_or("");
    let user_id = me["user_id"]
        .as_str()
        .filter(|user_id| !user_id.is_empty())
        .unwrap_or(&creds.user_id);

    if terse {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "user_id": user_id,
                "email": email,
                "pipelines": pipeline_count,
                "authenticated": true,
                "next_command": readiness.as_ref().and_then(|state| state.next_command),
                "continue_url": readiness.as_ref().and_then(|state| state.continue_url.as_deref()),
            }))?
        );
    } else {
        println!("User:          {email}");
        println!("User ID:       {user_id}");
        println!("Pipelines:     {pipeline_count}");
        println!("Authenticated: ✅");
        if let Some(readiness) = readiness.as_ref() {
            if readiness.status == "needs_pipeline" {
                println!("Base pipeline: missing");
            }
        }
    }

    Ok(())
}

fn is_active_route(route: &serde_json::Value) -> bool {
    route["status"]
        .as_str()
        .unwrap_or("")
        .eq_ignore_ascii_case("active")
}

fn resolve_chain_id_from_routes(
    explicit: Option<u64>,
    routes: &[serde_json::Value],
    preferred_pipeline_id: Option<&str>,
) -> Result<u64> {
    if let Some(id) = explicit {
        return Ok(id);
    }

    if let Some(preferred_id) = preferred_pipeline_id {
        if let Some(selected) = routes.iter().find(|route| {
            is_active_route(route) && route["id"].as_str().unwrap_or("") == preferred_id
        }) {
            return selected["chain_id"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("Pipeline has no chain_id"));
        }
    }

    let active = routes
        .iter()
        .find(|route| is_active_route(route))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No active pipeline found. Cannot auto-detect chain.\n\
                 Specify --chain <ID> explicitly, run `facto pipeline create`, or create one at {}",
                cached_pipeline_create_url()
            )
        })?;

    active["chain_id"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("Pipeline has no chain_id"))
}

async fn preferred_pipeline_id(api: &api::FactoApi, token: &str) -> Option<String> {
    if let Ok(prefs) = api
        .get::<serde_json::Value>("/v1/user/preferences", Some(token))
        .await
    {
        if let Some(default_pipeline_id) = prefs["default_pipeline_id"]
            .as_str()
            .filter(|id| !id.is_empty())
        {
            let mut cfg = config::load_config();
            if cfg.default_pipeline_id.as_deref() != Some(default_pipeline_id) {
                cfg.default_pipeline_id = Some(default_pipeline_id.to_string());
                cfg.default_pipeline_cached_at = Some(chrono::Utc::now());
                let _ = config::save_config(&cfg);
            }
            return Some(default_pipeline_id.to_string());
        }
    }

    config::load_config().default_pipeline_id
}

/// Resolves chain_id: uses explicit value if provided, otherwise prefers the
/// selected/default pipeline and falls back to the first active pipeline.
async fn resolve_chain_id(explicit: Option<u64>, api: &api::FactoApi, token: &str) -> Result<u64> {
    if let Some(id) = explicit {
        return Ok(id);
    }

    let routes: Vec<serde_json::Value> = api
        .get("/v1/routes/me", Some(token))
        .await
        .context("Failed to fetch routes for chain auto-detection")?;
    let preferred_pipeline_id = preferred_pipeline_id(api, token).await;
    resolve_chain_id_from_routes(explicit, &routes, preferred_pipeline_id.as_deref())
}

async fn cmd_balance(chain: Option<u64>, terse: bool) -> Result<()> {
    let (api, creds) = api::FactoApi::authenticated()?;
    ensure_user_bearer_auth(&creds)?;
    let chain_id = resolve_chain_id(chain, &api, &creds.token).await?;

    let resp: serde_json::Value = api
        .get(
            &format!("/v1/x402/balance?chain_id={chain_id}"),
            Some(&creds.token),
        )
        .await
        .context("failed to fetch server wallet balance")?;

    if terse {
        println!("{}", serde_json::to_string(&resp)?);
    } else {
        let wallet = resp["wallet_address"].as_str().unwrap_or("?");
        let display = resp["usdc_balance_display"].as_str().unwrap_or("$0.0000");
        let chain_name = chain_display_name(chain_id);
        println!("Server wallet:  {wallet}");
        println!("USDC balance:   {display} ({chain_name})");
    }

    Ok(())
}

async fn cmd_pipelines(terse: bool) -> Result<()> {
    let (api, creds) = api::FactoApi::authenticated()?;
    ensure_user_bearer_auth(&creds)?;

    let routes: Vec<serde_json::Value> = api
        .get("/v1/routes/me", Some(&creds.token))
        .await
        .context("Failed to fetch routes")?;

    // Filter to active routes only
    let active_routes: Vec<&serde_json::Value> = routes
        .iter()
        .filter(|r| {
            r.get("status")
                .and_then(|v| v.as_str())
                .map(|s| s.eq_ignore_ascii_case("active"))
                .unwrap_or(false)
        })
        .collect();

    let me: serde_json::Value = api
        .get("/v1/auth/me", Some(&creds.token))
        .await
        .context("Failed to fetch authenticated user")?;

    if active_routes.is_empty() {
        if terse {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({"pipelines": []}))?
            );
        } else {
            println!(
                "No active pipelines found. Run `facto pipeline create` or create one at {}",
                pipeline_init::resolve_cli_meta(Some(&api))
                    .await
                    .pipeline_create_url
            );
        }
        return Ok(());
    }

    // Fetch real-time balance for each route via pre-check API
    // Endpoint: GET /v1/charges/pre-check/{yield_token}/{eoa_address}/{spender}?chain_id={chain_id}
    // The spender is the user's server wallet address (from /v1/auth/me or route eoa_address)
    let server_wallet = me["wallet_address"]
        .as_str()
        .filter(|a| !a.is_empty() && *a != "0x0000000000000000000000000000000000000000")
        .unwrap_or("");

    let mut enriched: Vec<serde_json::Value> = Vec::new();
    for r in &active_routes {
        let route_id = r.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let chain_id = r.get("chain_id").and_then(|v| v.as_u64()).unwrap_or(0);
        let protocol = r.get("protocol_id").and_then(|v| v.as_str()).unwrap_or("");
        let asset_symbol = r.get("asset_symbol").and_then(|v| v.as_str()).unwrap_or("");
        let asset_dec = r
            .get("asset_decimals")
            .and_then(|v| v.as_u64())
            .unwrap_or(6);
        let yield_token = r.get("yield_token").and_then(|v| v.as_str()).unwrap_or("");
        let eoa = r.get("eoa_address").and_then(|v| v.as_str()).unwrap_or("");
        let per_tx = r
            .get("spending_limit")
            .and_then(|v| v.as_str())
            .unwrap_or("0");
        let daily = r.get("daily_limit").and_then(|v| v.as_str()).unwrap_or("0");
        let status_raw = r
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Query real-time balance from chain
        let spender = if server_wallet.is_empty() {
            eoa
        } else {
            server_wallet
        };
        let pre_check_path = format!(
            "/v1/charges/pre-check/{}/{}/{}?chain_id={}",
            yield_token, eoa, spender, chain_id
        );
        let balance_human = match api
            .get::<serde_json::Value>(&pre_check_path, Some(&creds.token))
            .await
        {
            Ok(pc) => {
                let raw = pc.get("balance").and_then(|v| v.as_str()).unwrap_or("0");
                let atomic: u128 = raw.parse().unwrap_or(0);
                // Morpho returns 18-dec values regardless of asset_decimals;
                // use 18 for morpho, asset_decimals for others.
                let effective_dec = if protocol == "morpho" { 18 } else { asset_dec };
                let divisor = 10u128.pow(effective_dec as u32);
                format!("{:.2}", atomic as f64 / divisor as f64)
            }
            Err(_) => "? (pre-check failed)".to_string(),
        };

        enriched.push(serde_json::json!({
            "route_id": route_id,
            "chain_id": chain_id,
            "protocol": protocol,
            "asset_symbol": asset_symbol,
            "balance": balance_human,
            "spending_limit": per_tx,
            "daily_limit": daily,
            "status": status_raw,
        }));
    }

    if terse {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({"pipelines": enriched}))?
        );
    } else {
        for p in &enriched {
            let route_id = p["route_id"].as_str().unwrap_or("?");
            let chain_id = p["chain_id"].as_u64().unwrap_or(0);
            let protocol = p["protocol"].as_str().unwrap_or("?");
            let asset_symbol = p["asset_symbol"].as_str().unwrap_or("?");
            let balance = p["balance"].as_str().unwrap_or("0");
            let per_tx = p["spending_limit"].as_str().unwrap_or("0");
            let daily = p["daily_limit"].as_str().unwrap_or("0");
            let status_raw = p["status"].as_str().unwrap_or("?");

            let chain_label = chain_display_name(chain_id);
            let status_display = if status_raw.eq_ignore_ascii_case("active") {
                "✅ Active".to_string()
            } else {
                format!("⚠️  {status_raw}")
            };
            let fund_support = if is_fund_supported(chain_id) {
                ""
            } else {
                " (fund not yet supported)"
            };

            println!("Pipeline: {chain_label} {asset_symbol} ({route_id}){fund_support}");
            println!("  Chain:      {chain_label}");
            println!("  Protocol:   {protocol}");
            println!("  Balance:    {balance} {asset_symbol}");
            println!("  Limits:     ${per_tx} per tx / ${daily} daily");
            println!("  Status:     {status_display}");
            println!();
        }
    }

    Ok(())
}

async fn cmd_pipeline_create(
    terse: bool,
    protocol: Option<&str>,
    base_x402: bool,
    mpp: bool,
    chain: Option<&str>,
    for_pay: Option<&str>,
) -> Result<()> {
    let api = api::FactoApi::new();
    let meta = pipeline_init::resolve_cli_meta(Some(&api)).await;
    let protocol_hint = if base_x402 {
        Some("x402")
    } else if mpp {
        Some("mpp")
    } else {
        protocol.map(str::trim).filter(|value| !value.is_empty())
    };
    let explicit_chain = chain.map(str::trim).filter(|value| !value.is_empty());
    let protocol_chain = match protocol_hint {
        Some("x402") => Some("base"),
        Some("mpp") => Some("monad"),
        _ => None,
    };
    if let (Some(protocol_chain), Some(explicit_chain)) = (protocol_chain, explicit_chain) {
        if protocol_chain != explicit_chain {
            bail!("`--protocol {}` conflicts with `--chain {}`", protocol_hint.unwrap(), explicit_chain);
        }
    }
    let chain_hint = explicit_chain.or(protocol_chain);
    let url = if let Some(target_url) = for_pay {
        pipeline_init::pipeline_create_url_for_pay(
            &meta.frontend_url,
            target_url,
            chain_hint,
        )
    } else if let Some(chain) = chain_hint {
        pipeline_init::pipeline_create_url_from_frontend_and_chain(&meta.frontend_url, Some(chain))
    } else {
        meta.pipeline_create_url.clone()
    };
    if terse {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "status": "pipeline_create_required",
                "open_url": url,
                "protocol_hint": protocol_hint,
                "chain_hint": chain_hint,
                "for_pay": for_pay,
            }))?
        );
        return Ok(());
    }

    println!("Opening browser to create a compatible payment pipeline...");
    println!("→ {url}");
    println!("Complete the browser flow, then return to your terminal.");
    let _ = open::that(&url);
    Ok(())
}

async fn cmd_pipeline_show(route_id: &str, terse: bool) -> Result<()> {
    let (api, creds) = api::FactoApi::authenticated()?;
    ensure_user_bearer_auth(&creds)?;

    let route: serde_json::Value = api
        .get(&format!("/v1/routes/{route_id}"), Some(&creds.token))
        .await
        .context("Failed to fetch pipeline details")?;

    if terse {
        println!("{}", serde_json::to_string(&route)?);
    } else {
        let name = route["name"].as_str().unwrap_or("Unnamed");
        let chain_id = route["chain_id"].as_u64().unwrap_or(0);
        let protocol = route["protocol_id"].as_str().unwrap_or("?");
        let asset = route["asset_symbol"].as_str().unwrap_or("?");
        let eoa = route["eoa_address"].as_str().unwrap_or("?");
        let status = route["status"].as_str().unwrap_or("?");
        let per_tx = route["spending_limit"].as_str().unwrap_or("0");
        let daily = route["daily_limit"].as_str().unwrap_or("0");
        let spend_mode = route["spend_mode"].as_str().unwrap_or("withdraw");
        let refund = route["refund_address"].as_str().unwrap_or("(default)");

        println!("Pipeline: {} ({})", name, route_id);
        println!("  Chain:         {}", chain_display_name(chain_id));
        println!("  Protocol:      {}", protocol);
        println!("  Asset:         {}", asset);
        println!("  Spend Mode:    {}", spend_mode);
        println!("  EOA:           {}", eoa);
        println!("  Refund To:     {}", refund);
        println!("  Per-tx Limit:  ${}", per_tx);
        println!("  Daily Limit:   ${}", daily);
        println!(
            "  Status:        {}",
            if status == "active" {
                "✅ Active"
            } else {
                status
            }
        );
    }

    Ok(())
}

async fn cmd_pipeline_default(id: Option<&str>, terse: bool) -> Result<()> {
    let (api, creds) = api::FactoApi::authenticated()?;
    ensure_user_bearer_auth(&creds)?;

    match id {
        Some(pipeline_id) => {
            // Set default
            let body = serde_json::json!({ "default_pipeline_id": pipeline_id });
            let resp: serde_json::Value = api
                .put_authenticated("/v1/user/preferences", &body, &creds.token)
                .await
                .context("Failed to set default pipeline")?;

            // Update local cache
            let mut cfg = config::load_config();
            cfg.default_pipeline_id = Some(pipeline_id.to_string());
            cfg.default_pipeline_cached_at = Some(chrono::Utc::now());
            let _ = config::save_config(&cfg);

            if terse {
                println!("{}", serde_json::to_string(&resp)?);
            } else {
                println!("✓ Default pipeline set to {}", pipeline_id);
            }
        }
        None => {
            // Show current default
            let prefs: serde_json::Value = api
                .get("/v1/user/preferences", Some(&creds.token))
                .await
                .context("Failed to fetch preferences")?;

            if terse {
                println!("{}", serde_json::to_string(&prefs)?);
            } else {
                match prefs["default_pipeline_id"].as_str() {
                    Some(id) => println!("Default pipeline: {}", id),
                    None => println!("No default pipeline set. Use: facto pipelines default <ID>"),
                }
            }
        }
    }

    Ok(())
}

async fn cmd_fund(
    amount: &str,
    recipient: Option<&str>,
    pipeline_id: Option<&str>,
    dry_run: bool,
    terse: bool,
) -> Result<()> {
    let (api, creds) = api::FactoApi::authenticated()?;
    ensure_user_bearer_auth(&creds)?;

    // Parse and validate amount
    let parsed: f64 = amount
        .parse()
        .with_context(|| format!("Invalid amount: {amount:?}"))?;
    if parsed <= 0.0 {
        bail!("Amount must be greater than 0");
    }

    // Convert to 6-decimal atomic units (USDC)
    let atomic: u64 = (parsed * 1_000_000.0) as u64;
    let atomic_str = atomic.to_string();

    // Fetch routes
    let routes: Vec<serde_json::Value> = api
        .get("/v1/routes/me", Some(&creds.token))
        .await
        .context("Failed to fetch routes")?;

    // Select pipeline: by --pipeline flag, or first supported active route
    let route: &serde_json::Value = if let Some(pid) = pipeline_id {
        // User specified a pipeline ID
        routes
            .iter()
            .find(|r| r["id"].as_str().unwrap_or("") == pid)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Pipeline '{pid}' not found. Run `facto pipelines` to see available IDs."
                )
            })?
    } else {
        // Auto-select: find first active route on a supported chain
        let supported_routes: Vec<&serde_json::Value> = routes
            .iter()
            .filter(|r| {
                let chain_id = r["chain_id"].as_u64().unwrap_or(0);
                let status = r["status"].as_str().unwrap_or("");
                is_fund_supported(chain_id) && status.eq_ignore_ascii_case("active")
            })
            .collect();

        match supported_routes.len() {
            0 => {
                let supported: Vec<String> = SUPPORTED_FUND_CHAINS
                    .iter()
                    .map(|id| chain_display_name(*id))
                    .collect();
                bail!(
                    "No active pipeline on a supported chain. Currently supported: {}.\nRun `facto pipeline create`, inspect `facto pipelines`, or create one at {}",
                    supported.join(", "),
                    pipeline_init::resolve_cli_meta(Some(&api))
                        .await
                        .pipeline_create_url
                );
            }
            1 => supported_routes[0],
            _ => {
                // Multiple supported pipelines — list them and ask user to choose
                if !terse {
                    println!("Multiple pipelines available. Use --pipeline <ID> to choose:\n");
                    for r in &supported_routes {
                        let rid = r["id"].as_str().unwrap_or("?");
                        let cid = r["chain_id"].as_u64().unwrap_or(0);
                        let proto = r["protocol_id"].as_str().unwrap_or("?");
                        let sym = r["asset_symbol"].as_str().unwrap_or("?");
                        println!("  facto fund --amount {} --pipeline {}", amount, rid);
                        println!("    → {} {} on {}\n", sym, proto, chain_display_name(cid));
                    }
                }
                bail!(
                    "Multiple pipelines available — specify --pipeline <ID>. Run `facto pipelines` to list them."
                );
            }
        }
    };

    let route_id = route["id"].as_str().unwrap_or("");
    let chain_id = route["chain_id"].as_u64().unwrap_or(0);
    let protocol_id = route["protocol_id"].as_str().unwrap_or("compound-v2");
    let yield_token = route["yield_token"].as_str().unwrap_or("");
    let eoa_address = route["eoa_address"].as_str().unwrap_or("");

    if !is_fund_supported(chain_id) {
        bail!(
            "Funding via CLI is not supported on {} yet. Supported chains: {}.",
            chain_display_name(chain_id),
            SUPPORTED_FUND_CHAINS
                .iter()
                .map(|id| chain_display_name(*id))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // Format display amount
    let display_amount = format!("{:.2}", parsed);

    // Resolve recipient:
    // 1. --to flag (explicit)
    // 2. Configured deposit address for this chain (from `facto config`)
    // 3. Default: own EOA (self-funding)
    let chain_name_lower = chain_display_name(chain_id).to_lowercase();
    let deposit_addr = creds
        .deposit_address_for(&chain_name_lower)
        .map(|s| s.to_string());

    let resolved_recipient = if let Some(r) = recipient {
        r
    } else if let Some(ref da) = deposit_addr {
        if !terse {
            println!(
                "Using configured deposit address for {}: {}",
                chain_display_name(chain_id),
                da
            );
        }
        da.as_str()
    } else {
        eoa_address
    };

    // Validate recipient address format
    if !resolved_recipient.starts_with("0x") || resolved_recipient.len() != 42 {
        bail!(
            "Invalid recipient address: {}\nMust be a 42-character 0x-prefixed Ethereum address (e.g. 0x1234...5678)",
            resolved_recipient
        );
    }

    let allowlist: Vec<serde_json::Value> = api
        .get("/v1/recipients", Some(&creds.token))
        .await
        .context("Failed to fetch recipient allowlist")?;

    let in_allowlist = allowlist.iter().any(|r| {
        r["address"]
            .as_str()
            .map(|a| a.eq_ignore_ascii_case(resolved_recipient))
            .unwrap_or(false)
    });

    if dry_run {
        if terse {
            let allowlist_action = if in_allowlist { "none" } else { "would_add" };
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "dry_run": true,
                    "amount": display_amount,
                    "route_id": route_id,
                    "chain_id": chain_id,
                    "recipient": resolved_recipient,
                    "allowlist_action": allowlist_action,
                }))?
            );
        } else {
            let dest_label = if resolved_recipient == eoa_address {
                format!("{resolved_recipient} (self)")
            } else {
                resolved_recipient.to_string()
            };
            println!("Dry run — would fund ${display_amount} USDC");
            println!("  Route:       {route_id}");
            println!("  Chain ID:    {chain_id}");
            println!("  Protocol:    {protocol_id}");
            println!("  Recipient:   {dest_label}");
            if !in_allowlist {
                println!("  Allowlist:   would add recipient before execution");
            }
        }
        return Ok(());
    }

    // Check if recipient is in user's allowlist; if not, offer to add.
    // Engine validates against user-level recipients (/v1/recipients), not route-level.
    if !in_allowlist {
        let label = if resolved_recipient == eoa_address {
            "Funding Source"
        } else {
            "CLI Added"
        };
        let add_body = serde_json::json!({ "address": resolved_recipient, "label": label });

        if terse {
            let _: serde_json::Value = api
                .post_authenticated("/v1/recipients", &add_body, &creds.token)
                .await
                .context("Failed to add recipient to allowlist")?;
        } else {
            println!(
                "⚠️  Address {} is not in your allowlist.",
                resolved_recipient
            );
            print!("   Add it and continue? [Y/n] ");
            std::io::Write::flush(&mut std::io::stdout())?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let answer = input.trim().to_lowercase();
            if answer == "n" || answer == "no" {
                println!("Cancelled.");
                return Ok(());
            }
            let _: serde_json::Value = api
                .post_authenticated("/v1/recipients", &add_body, &creds.token)
                .await
                .context("Failed to add recipient to allowlist")?;
            println!("   ✅ Added to allowlist.");
        }
    }

    let chain_name = chain_display_name(chain_id);

    // ── Pre-check: verify DeFi position has sufficient balance ──────────
    let me: serde_json::Value = api
        .get("/v1/auth/me", Some(&creds.token))
        .await
        .context("Failed to fetch authenticated user")?;
    let user_wallet = me["wallet_address"]
        .as_str()
        .filter(|a| !a.is_empty() && *a != "0x0000000000000000000000000000000000000000")
        .unwrap_or(eoa_address)
        .to_string();

    let spender = if user_wallet.is_empty() {
        eoa_address
    } else {
        &user_wallet
    };
    let asset_dec = route
        .get("asset_decimals")
        .and_then(|v| v.as_u64())
        .unwrap_or(6);
    let pre_check_path = format!(
        "/v1/charges/pre-check/{}/{}/{}?chain_id={}&amount={}",
        yield_token, eoa_address, spender, chain_id, atomic_str
    );
    let pipeline_balance_display = match api
        .get::<serde_json::Value>(&pre_check_path, Some(&creds.token))
        .await
    {
        Ok(pc) => {
            let redeemable = pc
                .get("redeemable")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let balance_raw = pc.get("balance").and_then(|v| v.as_str()).unwrap_or("0");
            let balance_atomic: u128 = balance_raw.parse().unwrap_or(0);
            // Morpho returns 18-dec values regardless of asset_decimals
            let effective_dec = if protocol_id == "morpho" {
                18
            } else {
                asset_dec
            };
            let divisor = 10u128.pow(effective_dec as u32);
            let balance_human = format!("{:.2}", balance_atomic as f64 / divisor as f64);

            // Client-side balance check: compare in human-readable units
            // (backend may have precision mismatch for Morpho 18-dec vs 6-dec amounts)
            let balance_f64: f64 = balance_human.parse().unwrap_or(0.0);
            let redeemable = redeemable && balance_f64 >= parsed;

            if !redeemable {
                let reasons = pc
                    .get("reasons")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|r| r.as_str())
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .unwrap_or_default();

                if terse {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "error": "insufficient_balance",
                            "pipeline_balance": balance_human,
                            "requested": display_amount,
                            "reasons": reasons,
                        }))?
                    );
                } else {
                    println!(
                        "❌ Insufficient balance in DeFi position.\n   Available: ${balance_human}  Requested: ${display_amount}\n   {reasons}"
                    );
                }
                bail!(
                    "Insufficient balance: available ${balance_human}, requested ${display_amount}"
                );
            }

            balance_human
        }
        Err(e) => {
            // Pre-check failed — warn but don't block (may be a new protocol without pre-check)
            if !terse {
                println!("⚠️  Could not verify pipeline balance: {e}");
                println!("   Proceeding anyway — on-chain execution will validate.");
            }
            "?".to_string()
        }
    };

    if !terse {
        println!(
            "Funding ${display_amount} USDC from {protocol_id} on {chain_name} (available: ${pipeline_balance_display}) → wallet..."
        );
    }

    // Generate a unique invoice ID for this funding operation
    let timestamp = chrono::Utc::now().format("%y%m%d-%H%M%S");
    let rand_suffix: u32 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let invoice_id = format!("CLI-FUND-{}-{:06X}", timestamp, rand_suffix & 0xFFFFFF);

    let spend_mode = fund_spend_mode(route);
    let body = serde_json::json!({
        "user_address":        user_wallet,
        "protocol_id":         protocol_id,
        "receipt_token":       yield_token,
        "underlying_amount":   atomic_str,
        "recipient_address":   resolved_recipient,
        "source_eoa_address":  eoa_address,
        "chain_id":            chain_id,
        "spend_mode":          spend_mode,
        "invoice_id":          invoice_id,
    });

    let resp: serde_json::Value = api
        .post_authenticated("/v1/charges/execute-7702", &body, &creds.token)
        .await
        .context("Failed to execute charge")?;

    let tx_hash = resp["transaction_hash"].as_str().unwrap_or("—");
    let charge_id = resp["charge_id"].as_str().unwrap_or("—");
    let status = resp["status"].as_str().unwrap_or("—");

    let explorer_base = match chain_id {
        42161 => "https://arbiscan.io",
        421614 => "https://sepolia.arbiscan.io",
        143 => "https://monad.socialscan.io",
        8453 => "https://basescan.org",
        _ => "https://etherscan.io",
    };
    let explorer_url = format!("{explorer_base}/tx/{tx_hash}");
    let cli_meta = pipeline_init::resolve_cli_meta(Some(&api)).await;
    let facto_url = pipeline_init::charge_url(&cli_meta.frontend_url, charge_id);

    // Confirm balance arrived on-chain (poll up to ~30s)
    let confirmed_balance = if resolved_recipient == user_wallet {
        // Recipient is the user's own wallet — verify balance reflects the deposit
        if !terse {
            print!("  Confirming on-chain");
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        let balance_path = format!("/v1/x402/balance?chain_id={}", chain_id);
        let mut confirmed: Option<String> = None;
        for attempt in 0..10 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
            if !terse && attempt > 0 {
                print!(".");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
            if let Ok(bal) = api
                .get::<serde_json::Value>(&balance_path, Some(&creds.token))
                .await
            {
                let raw = bal["usdc_balance"]
                    .as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                if raw >= atomic {
                    confirmed = Some(
                        bal["usdc_balance_display"]
                            .as_str()
                            .unwrap_or("?")
                            .to_string(),
                    );
                    break;
                }
            }
        }
        if !terse {
            println!();
        }
        confirmed
    } else {
        None
    };

    if terse {
        let mut json = serde_json::json!({
            "status":       status,
            "amount":       display_amount,
            "pipeline_balance": pipeline_balance_display,
            "tx_hash":      tx_hash,
            "explorer_url": explorer_url,
            "facto_url":    facto_url,
            "charge_id":    charge_id,
        });
        if let Some(ref bal) = confirmed_balance {
            json["confirmed_balance"] = serde_json::Value::String(bal.clone());
        } else if resolved_recipient == user_wallet {
            json["balance_confirmed"] = serde_json::Value::Bool(false);
        }
        println!("{}", serde_json::to_string(&json)?);
    } else {
        println!("✅ Funded");
        println!("  Amount:     ${display_amount} USDC");
        println!("  TX hash:    {tx_hash}");
        println!("  Explorer:   {explorer_url}");
        println!("  Facto:      {facto_url}");
        if let Some(bal) = confirmed_balance {
            println!("  Balance:    {bal} (confirmed)");
        }
    }

    Ok(())
}

async fn cmd_history(terse: bool) -> Result<()> {
    let (api, creds) = api::FactoApi::authenticated()?;
    ensure_user_bearer_auth(&creds)?;

    // Fetch user's wallet address from whoami/routes to get the correct address
    let routes: Vec<serde_json::Value> = api
        .get("/v1/routes/me", Some(&creds.token))
        .await
        .context("Failed to fetch routes")?;

    // Resolve user's server wallet address (charges are indexed by user_address = server wallet)
    let me: serde_json::Value = api
        .get("/v1/auth/me", Some(&creds.token))
        .await
        .context("Failed to fetch authenticated user")?;
    let user_wallet = me["wallet_address"]
        .as_str()
        .filter(|a| !a.is_empty() && *a != "0x0000000000000000000000000000000000000000")
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // Fallback: try eoa_address from routes
            routes
                .first()
                .and_then(|r| r.get("eoa_address"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        });

    // Fetch charge history using /v1/charges/user/{address}
    let charges: serde_json::Value = api
        .get(
            &format!("/v1/charges/user/{}", user_wallet),
            Some(&creds.token),
        )
        .await
        .context("Failed to fetch charge history")?;

    // Handle both response shapes: {"charges": [...]} or [...]
    let charge_array: Vec<serde_json::Value> = if charges.is_object() {
        charges
            .get("charges")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    } else if charges.is_array() {
        charges.as_array().cloned().unwrap_or_default()
    } else {
        Vec::new()
    };

    if charge_array.is_empty() {
        println!("No funding history found.");
        return Ok(());
    }

    if terse {
        // Raw JSON output
        println!("{}", serde_json::to_string(&charge_array)?);
    } else {
        // Human-readable table format
        println!("  {:<19} {:<11} {:<15} Status", "Date", "Amount", "TX Hash");
        println!("  {}", "─".repeat(66));

        for charge in &charge_array {
            let charge_type = charge
                .get("charge_type")
                .and_then(|v| v.as_str())
                .unwrap_or("fund");

            let type_icon = match charge_type {
                "x402" => "🌐",
                _ => "💰",
            };

            let api_url_detail = if charge_type == "x402" {
                charge
                    .get("x402_api_url")
                    .and_then(|v| v.as_str())
                    .map(|url| {
                        if url.len() > 30 {
                            format!(" → {}...", &url[..27])
                        } else {
                            format!(" → {url}")
                        }
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            };

            let created_at = charge
                .get("created_at")
                .and_then(|v| v.as_str())
                .unwrap_or("—");

            // Truncate created_at to first 16 chars (YYYY-MM-DD HH:MM)
            let created_at_display = if created_at.len() >= 16 {
                &created_at[..16]
            } else {
                created_at
            };

            // Parse underlying_amount as u64 and convert to human-readable
            let amount_display = charge
                .get("underlying_amount")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<u64>().ok())
                .map(format_usdc_amount)
                .unwrap_or_else(|| "—".to_string());

            // Shorten transaction_hash: first 6 + "..." + last 4 (if > 14 chars)
            let settlement_tx_hash = charge_settlement_tx_hash(charge);
            let tx_hash_display = settlement_tx_hash
                .as_deref()
                .map(|hash| {
                    if hash.len() > 14 {
                        format!("{}...{}", &hash[..6], &hash[hash.len() - 4..])
                    } else {
                        hash.to_string()
                    }
                })
                .unwrap_or_else(|| "—".to_string());

            // Map status to icon
            let status_display = charge
                .get("display_status")
                .or_else(|| charge.get("status"))
                .and_then(|v| v.as_str())
                .map(|s| match s.to_lowercase().as_str() {
                    "success" | "charged" | "completed" => "✅ Confirmed".to_string(),
                    "processing" | "pending" => "⏳ Processing".to_string(),
                    other => format!("❌ {other}"),
                })
                .unwrap_or_else(|| "—".to_string());

            let status_display = if charge_type == "x402" {
                match charge_response_success_flag(charge) {
                    Some(false) => "❌ failed".to_string(),
                    _ if settlement_tx_hash.is_some() => status_display,
                    _ if status_display == "✅ Confirmed" => "⏳ Settling".to_string(),
                    _ => status_display,
                }
            } else {
                status_display
            };

            println!(
                "{} {:<19} {:<11} {:<15} {}{}",
                type_icon,
                created_at_display,
                amount_display,
                tx_hash_display,
                status_display,
                api_url_detail
            );
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn cmd_pay(
    method: &str,
    url: &str,
    protocol: &str,
    headers: &[String],
    data: Option<&str>,
    max_amount: Option<&str>,
    dry_run: bool,
    chain: Option<u64>,
    terse: bool,
    yes: bool,
) -> Result<()> {
    let (api, creds) = api::FactoApi::authenticated()?;
    ensure_user_bearer_auth(&creds)?;

    let protocol_mode = pay::ProtocolMode::parse(Some(protocol))?;
    let selected_pipeline_id = preferred_pipeline_id(&api, &creds.token).await;
    let parsed_data = parse_request_body(data)?;
    let header_map = parse_request_headers(headers);
    let max_amount_atomic = parse_usdc_amount(max_amount);

    let resolve_request = pay::PayResolveRequest {
        url: url.to_string(),
        method: method.to_uppercase(),
        headers: header_map.clone(),
        body: parsed_data.clone(),
        protocol: match protocol_mode {
            pay::ProtocolMode::Auto => None,
            _ => Some(protocol_mode.as_str().to_string()),
        },
        max_amount: max_amount.map(|value| value.to_string()),
        chain_id: chain,
    };

    let resolve = pay::resolve_payment(&api, &creds.token, &resolve_request).await?;

    let execution_pipeline_id = if resolve.requires_payment {
        resolve_execution_pipeline(
            &resolve,
            selected_pipeline_id.as_deref(),
            !dry_run && !terse,
        )
        .ok()
    } else {
        None
    };

    let mut resolved_chain_id = chain.or(resolve.chain_id);
    if resolved_chain_id.is_none() && resolve.requires_payment {
        resolved_chain_id = Some(resolve_chain_id(None, &api, &creds.token).await?);
    }

    if dry_run {
        if terse {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "dry_run": true,
                    "protocol": resolve.protocol,
                    "requires_payment": resolve.requires_payment,
                    "payment_method": resolve.payment_method,
                    "chain_id": resolved_chain_id,
                    "quoted_amount": resolve.quoted_amount,
                    "compatible_pipeline_ids": resolve.compatible_pipeline_ids,
                    "recommended_execution_pipeline_id": resolve.recommended_execution_pipeline_id,
                    "selected_pipeline_id": selected_pipeline_id,
                    "execution_pipeline_id": execution_pipeline_id,
                    "create_pipeline_url": resolve.create_pipeline_url,
                }))?
            );
        } else {
            println!("Dry run — resolved payment target:");
            println!("  URL:      {url}");
            println!("  Method:   {}", method.to_uppercase());
            println!("  Protocol: {}", resolve.protocol);
            if let Some(method_label) = resolve.payment_method.as_deref() {
                println!("  Method ID: {method_label}");
            }
            if let Some(chain_id) = resolved_chain_id {
                println!("  Chain:    {} ({chain_id})", chain_display_name(chain_id));
            }
            if let Some(amount) = resolve.quoted_amount.as_deref().or(max_amount) {
                println!("  Max pay:  {amount}");
            }
            if let Some(execution_pipeline_id) = execution_pipeline_id.as_deref() {
                println!("  Exec pipe: {execution_pipeline_id}");
            }
            if let Some(url) = resolve.create_pipeline_url.as_deref() {
                println!("  Create:   {url}");
            }
        }
        return Ok(());
    }

    if !resolve.requires_payment {
        let resp = execute_free_request(url, method, &header_map, parsed_data.as_ref()).await?;
        print_payment_result(
            &resp,
            terse,
            selected_pipeline_id.as_deref(),
            None,
            Some(resolve.protocol.as_str()),
        )?;
        return Ok(());
    }

    let execution_pipeline_id = match execution_pipeline_id {
        Some(id) => id,
        None => {
            let reason_hint = pay::explain_resolve_reason(resolve.reason.as_deref());
            if let Some(url) = resolve.create_pipeline_url.as_deref() {
                if terse {
                    if let Some(reason_hint) = reason_hint {
                        bail!("{reason_hint} Create one at {url} or run `facto pipelines create`.");
                    }
                    bail!(
                        "No compatible pipeline available. Create one at {url} or run `facto pipelines create`."
                    );
                }
                println!(
                    "{}",
                    reason_hint.unwrap_or("No compatible pipeline is currently available.")
                );
                println!("Create one now: {url}");
                bail!("No compatible pipeline available");
            }
            bail!("No compatible pipeline available");
        }
    };

    let quoted_amount = resolve
        .quoted_amount
        .as_deref()
        .or(max_amount)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let chain_id = resolved_chain_id.unwrap_or(8453);
    let supports_prefund = resolve.protocol == "x402"
        || (resolve.protocol == "mpp" && resolve.can_auto_fund.unwrap_or(false));
    let confirmed = if supports_prefund {
        interactive::confirm_before_pay(
            &api,
            &creds.token,
            url,
            quoted_amount,
            Some(execution_pipeline_id.as_str()),
            terse,
            chain_id,
            &resolve.protocol,
            yes || terse,
            !yes && !terse,
        )
        .await?
    } else if yes || terse {
        true
    } else {
        interactive::confirm_payment_only(
            url,
            quoted_amount,
            Some(execution_pipeline_id.as_str()),
            &resolve.protocol,
        )?
    };
    if !confirmed {
        return Ok(());
    }

    if let Some(notice) =
        pay::temporary_pipeline_notice(selected_pipeline_id.as_deref(), &execution_pipeline_id)
    {
        if !terse {
            println!("{notice}");
        }
    }

    let chain_id = resolved_chain_id.unwrap_or(8453);
    let max_amount_atomic = max_amount_atomic.or_else(|| resolve.quoted_amount.clone());
    let protocol_name = resolve.protocol.clone();
    let body = build_payment_request_body(
        url,
        method,
        header_map,
        parsed_data,
        chain_id,
        max_amount_atomic.clone(),
        protocol_name.clone(),
        resolve.payment_method.clone(),
        selected_pipeline_id.clone(),
        Some(execution_pipeline_id.clone()),
    );

    let endpoint = pay::payment_endpoint(protocol_name.as_str()).unwrap_or("/v1/x402/pay");
    let resp: serde_json::Value = api
        .post_authenticated(endpoint, &body, &creds.token)
        .await
        .with_context(|| format!("{} request failed", endpoint.trim_start_matches('/')))
        .context("payment request failed")?;

    print_payment_result(
        &resp,
        terse,
        selected_pipeline_id.as_deref(),
        Some(execution_pipeline_id.as_str()),
        Some(protocol_name.as_str()),
    )?;

    Ok(())
}

fn build_payment_request_body(
    url: &str,
    method: &str,
    header_map: std::collections::HashMap<String, String>,
    parsed_data: Option<serde_json::Value>,
    chain_id: u64,
    max_amount_atomic: Option<String>,
    protocol_name: String,
    payment_method: Option<String>,
    selected_pipeline_id: Option<String>,
    execution_pipeline_id: Option<String>,
) -> serde_json::Value {
    serde_json::json!({
        "url": url,
        "method": method.to_uppercase(),
        "headers": header_map,
        "body": parsed_data,
        "chainId": chain_id,
        "maxAmount": max_amount_atomic,
        "protocol": protocol_name,
        "paymentMethod": payment_method,
        "selectedPipelineId": selected_pipeline_id,
        "executionPipelineId": execution_pipeline_id,
    })
}

fn parse_request_headers(headers: &[String]) -> std::collections::HashMap<String, String> {
    let mut header_map = std::collections::HashMap::new();
    for header in headers {
        if let Some((key, value)) = header.split_once(':') {
            header_map.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    header_map
}

fn parse_request_body(data: Option<&str>) -> Result<Option<serde_json::Value>> {
    Ok(data.map(|value| {
        serde_json::from_str(value).unwrap_or(serde_json::Value::String(value.to_string()))
    }))
}

fn parse_usdc_amount(raw: Option<&str>) -> Option<String> {
    raw.and_then(|value| value.parse::<f64>().ok())
        .map(|value| format!("{}", (value * 1_000_000.0) as u64))
}

fn resolve_execution_pipeline(
    resolve: &pay::PayResolveResponse,
    selected_pipeline_id: Option<&str>,
    allow_prompt: bool,
) -> Result<String> {
    match pay::choose_execution_pipeline(
        selected_pipeline_id,
        &resolve.compatible_pipeline_ids,
        resolve.recommended_execution_pipeline_id.as_deref(),
    ) {
        Ok(decision) => Ok(decision.execution_pipeline_id),
        Err(_err) if allow_prompt && !resolve.compatible_pipeline_ids.is_empty() => {
            prompt_execution_pipeline_choice(
                &resolve.compatible_pipeline_ids,
                resolve.recommended_execution_pipeline_id.as_deref(),
            )
        }
        Err(err) if resolve.compatible_pipeline_ids.is_empty() => {
            let reason_hint = pay::explain_resolve_reason(resolve.reason.as_deref());
            if let Some(url) = resolve.create_pipeline_url.as_deref() {
                if !allow_prompt {
                    if let Some(reason_hint) = reason_hint {
                        bail!("{reason_hint} Create one at {url}. ({err})");
                    }
                    bail!("No compatible pipeline available. Create one at {url}. ({err})");
                }
                println!(
                    "{}",
                    reason_hint.unwrap_or("No compatible pipeline is currently available.")
                );
                println!("Create one now: {url}");
            }
            Err(err)
        }
        Err(err) => Err(err),
    }
}

fn prompt_execution_pipeline_choice(
    compatible_pipeline_ids: &[String],
    recommended_execution_pipeline_id: Option<&str>,
) -> Result<String> {
    eprintln!();
    eprintln!("Multiple compatible pipelines found:");
    for (idx, pipeline_id) in compatible_pipeline_ids.iter().enumerate() {
        let recommended = recommended_execution_pipeline_id
            .filter(|recommended| *recommended == pipeline_id)
            .map(|_| " (recommended)")
            .unwrap_or("");
        eprintln!("  {}. {}{}", idx + 1, pipeline_id, recommended);
    }

    loop {
        eprint!(
            "Select execution pipeline (1-{}): ",
            compatible_pipeline_ids.len()
        );
        std::io::stderr().flush().ok();
        let mut line = String::new();
        let bytes = std::io::stdin().read_line(&mut line).unwrap_or(0);
        if bytes == 0 {
            bail!("No execution pipeline selected.");
        }
        if let Ok(choice) = line.trim().parse::<usize>() {
            if (1..=compatible_pipeline_ids.len()).contains(&choice) {
                return Ok(compatible_pipeline_ids[choice - 1].clone());
            }
        }
        eprintln!(
            "Please enter a number between 1 and {}.",
            compatible_pipeline_ids.len()
        );
    }
}

async fn execute_free_request(
    url: &str,
    method: &str,
    headers: &std::collections::HashMap<String, String>,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    let client = reqwest::Client::new();
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .context("invalid HTTP method for free request")?;
    let mut req = client.request(method, url);
    for (key, value) in headers {
        req = req.header(key, value);
    }
    if let Some(value) = body {
        if value.is_object() || value.is_array() {
            req = req.json(value);
        } else if let Some(text) = value.as_str() {
            req = req.body(text.to_string());
        } else {
            req = req.json(value);
        }
    }

    let resp = req
        .send()
        .await
        .with_context(|| format!("GET {url} failed"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    Ok(serde_json::json!({
        "status": "free",
        "response": {
            "status_code": status.as_u16(),
            "body": body,
        }
    }))
}

fn print_payment_result(
    resp: &serde_json::Value,
    terse: bool,
    selected_pipeline_id: Option<&str>,
    execution_pipeline_id: Option<&str>,
    protocol: Option<&str>,
) -> Result<()> {
    let status = resp["status"].as_str().unwrap_or("unknown");
    if status == "error" || status == "failed" {
        let error = resp["error"].as_str().unwrap_or("Unknown error");
        if terse {
            println!("{}", serde_json::to_string(resp)?);
        } else {
            println!("❌ {error}");
        }
        bail!("{error}");
    }

    if terse {
        let mut object = resp.as_object().cloned().unwrap_or_default();
        object.insert(
            "selected_pipeline_id".to_string(),
            selected_pipeline_id
                .map(|value| serde_json::Value::String(value.to_string()))
                .unwrap_or(serde_json::Value::Null),
        );
        object.insert(
            "execution_pipeline_id".to_string(),
            execution_pipeline_id
                .map(|value| serde_json::Value::String(value.to_string()))
                .unwrap_or(serde_json::Value::Null),
        );
        object.insert(
            "default_pipeline_unchanged".to_string(),
            serde_json::Value::Bool(true),
        );
        object.insert(
            "protocol".to_string(),
            protocol
                .map(|value| serde_json::Value::String(value.to_string()))
                .unwrap_or_else(|| {
                    object
                        .get("protocol")
                        .cloned()
                        .unwrap_or(serde_json::Value::String("none".to_string()))
                }),
        );
        println!(
            "{}",
            serde_json::to_string(&serde_json::Value::Object(object))?
        );
        return Ok(());
    }

    println!(
        "Protocol: {}",
        protocol
            .or_else(|| resp["protocol"].as_str())
            .unwrap_or("none")
    );
    if let Some(execution_pipeline_id) = execution_pipeline_id {
        println!("Execution pipeline: {execution_pipeline_id}");
    }

    if status == "paid" {
        let amount = extract_payment_amount_display(resp);
        let pay_to = extract_payment_destination(resp);
        let charge_id = resp["payment"]["charge_id"].as_str().unwrap_or("?");
        let resp_status = resp["response"]["status_code"].as_u64().unwrap_or(0);
        let payment_method = resp["payment"]["method"].as_str();
        let payment_intent = resp["payment"]["intent"].as_str();
        let reference = resp["payment"]["reference"].as_str();

        let pay_to_short = pay_to.as_deref().map(shorten_payment_destination);

        match (&amount, &pay_to_short) {
            (Some(amount), Some(pay_to_short)) => {
                println!("💳 Payment required: {amount} USDC → {pay_to_short}");
            }
            (Some(amount), None) => {
                println!("💳 Payment completed: {amount} USDC");
            }
            (None, Some(pay_to_short)) => {
                println!("💳 Payment completed → {pay_to_short}");
            }
            (None, None) => {
                println!("💳 Payment completed");
            }
        }
        println!("✅ Paid & received response ({resp_status})");
        println!();
        println!("Payment:");
        if let Some(amount) = amount {
            println!("  Amount:    {amount} USDC");
        }
        if let Some(pay_to_short) = pay_to_short {
            println!("  To:        {pay_to_short}");
        }
        if let Some(method) = payment_method {
            let intent_suffix = payment_intent.map(|intent| format!(" / {intent}")).unwrap_or_default();
            println!("  Method:    {method}{intent_suffix}");
        }
        if let Some(reference) = reference {
            println!("  Reference: {reference}");
        }
        println!("  Charge:    {charge_id}");
        println!();
        println!("Response:");
    } else {
        let resp_status = resp["response"]["status_code"].as_u64().unwrap_or(0);
        println!("✅ Response ({resp_status}) — no payment required");
        println!();
        println!("Response:");
    }

    let body = resp["response"]["body"].as_str().unwrap_or("");
    println!("{body}");
    Ok(())
}

fn extract_payment_amount_display(resp: &serde_json::Value) -> Option<String> {
    if let Some(amount_display) = resp["payment"]["amount_display"].as_str() {
        return Some(amount_display.to_string());
    }

    let raw_amount = resp["payment"]["amount"].as_str()?;
    let raw_amount = raw_amount.parse::<u128>().ok()?;
    let dollars = raw_amount as f64 / 1_000_000.0;

    Some(if dollars < 1.0 {
        format!("${:.4}", dollars)
    } else {
        format!("${:.2}", dollars)
    })
}

fn extract_payment_destination(resp: &serde_json::Value) -> Option<String> {
    resp["payment"]["pay_to"]
        .as_str()
        .or_else(|| resp["payment"]["recipient"].as_str())
        .or_else(|| resp["payment"]["merchant"].as_str())
        .map(|value| value.to_string())
}

fn shorten_payment_destination(value: &str) -> String {
    if value.len() > 18 && value.starts_with("0x") {
        format!("{}...{}", &value[..6], &value[value.len() - 4..])
    } else {
        value.to_string()
    }
}

fn format_services_terse(items: &[serde_json::Value]) -> String {
    let output: Vec<serde_json::Value> = items
        .iter()
        .map(|item| {
            let accept = item["accepts"].as_array().and_then(|a| a.first());
            let raw: u128 = accept
                .and_then(|a| a["maxAmountRequired"].as_str())
                .unwrap_or("0")
                .parse()
                .unwrap_or(0);
            let price_usd = raw as f64 / 1_000_000.0;
            let version = item["x402Version"].as_i64().unwrap_or(2);
            let method = item["method"].as_str().unwrap_or("GET");
            let mut obj = serde_json::json!({
                "url": item["resource"].as_str().unwrap_or(""),
                "name": item["name"].as_str().unwrap_or(""),
                "description": item["description"].as_str().unwrap_or(""),
                "category": item["category"].as_str().unwrap_or(""),
                "source": item["source"].as_str().unwrap_or(""),
                "price_usdc": format!("{:.4}", price_usd),
                "method": method,
                "x402_version": version,
            });
            // Include required_params and example_request when available
            if let Some(params) = item.get("requiredParams") {
                if !params.is_null() {
                    obj["required_params"] = params.clone();
                }
            }
            if let Some(example) = item["exampleRequest"].as_str() {
                obj["example"] = serde_json::Value::String(example.to_string());
            }
            if raw == 0 {
                obj["price_note"] = serde_json::Value::String(
                    "Listed as free but may charge at call time. Use --max-amount to cap."
                        .to_string(),
                );
            }
            obj
        })
        .collect();
    serde_json::to_string(&output).unwrap_or_else(|_| "[]".to_string())
}

fn format_services_human(items: &[serde_json::Value], total: i64, query: Option<&str>) -> String {
    if items.is_empty() {
        return format!(
            "No services found{}",
            query
                .map(|q| format!(" matching \"{q}\""))
                .unwrap_or_default()
        );
    }
    let mut out = format!(
        "\u{1f310} x402 Services ({total} total, showing {})\n\n",
        items.len()
    );
    for item in items {
        let name = item["name"].as_str().unwrap_or("");
        let url = item["resource"].as_str().unwrap_or("?");
        let desc = item["description"].as_str().unwrap_or("No description");
        let source = item["source"].as_str().unwrap_or("?");
        let accept = item["accepts"].as_array().and_then(|a| a.first());
        let raw: u128 = accept
            .and_then(|a| a["maxAmountRequired"].as_str())
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);
        let price_usd = raw as f64 / 1_000_000.0;
        let desc_short = if desc.len() > 60 {
            format!("{}...", &desc[..57])
        } else {
            desc.to_string()
        };
        let display_name = if name.is_empty() {
            desc_short.as_str()
        } else {
            name
        };
        let version = item["x402Version"].as_i64().unwrap_or(2);
        let version_badge = if version >= 2 { "v2" } else { "v1" };
        out.push_str(&format!(
            "  {} [{}] [{}]\n",
            display_name, source, version_badge
        ));
        out.push_str(&format!("  URL:   {url}\n"));
        if raw > 0 {
            out.push_str(&format!("  Price: ${:.4} USDC\n", price_usd));
        } else {
            out.push_str("  Price: $0 (listed) — actual charge may differ\n");
        }
        out.push('\n');
    }
    out.push_str("Usage: facto pay POST <url> --data '{...}'");
    out
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct TestEnv {
        home: PathBuf,
        prev_home: Option<OsString>,
        prev_api_url: Option<OsString>,
    }

    impl TestEnv {
        fn new(api_url: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let home = std::env::temp_dir().join(format!(
                "facto-cli-test-{}-{}",
                std::process::id(),
                unique
            ));
            std::fs::create_dir_all(home.join(".facto")).unwrap();

            let prev_home = std::env::var_os("HOME");
            let prev_api_url = std::env::var_os("FACTO_API_URL");
            std::env::set_var("HOME", &home);
            std::env::set_var("FACTO_API_URL", api_url);

            Self {
                home,
                prev_home,
                prev_api_url,
            }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            match &self.prev_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match &self.prev_api_url {
                Some(value) => std::env::set_var("FACTO_API_URL", value),
                None => std::env::remove_var("FACTO_API_URL"),
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    fn start_balance_mock_server(
        requests: Arc<Mutex<Vec<String>>>,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();

        let handle = thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut seen = 0;

            while seen < 3 && std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap();
                        let request = String::from_utf8_lossy(&buf[..n]);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("")
                            .to_string();
                        requests.lock().unwrap().push(path.clone());

                        let (status, body) = match path.as_str() {
                            "/v1/routes/me" => (
                                "200 OK",
                                serde_json::json!([
                                    {
                                        "id": "first-active",
                                        "status": "active",
                                        "chain_id": 8453
                                    },
                                    {
                                        "id": "selected-pipeline",
                                        "status": "active",
                                        "chain_id": 143
                                    }
                                ])
                                .to_string(),
                            ),
                            "/v1/user/preferences" => (
                                "200 OK",
                                serde_json::json!({
                                    "default_pipeline_id": "selected-pipeline"
                                })
                                .to_string(),
                            ),
                            "/v1/x402/balance?chain_id=143" => (
                                "200 OK",
                                serde_json::json!({
                                    "wallet_address": "0x0000000000000000000000000000000000000123",
                                    "usdc_balance": "1000000",
                                    "usdc_balance_display": "$1.0000"
                                })
                                .to_string(),
                            ),
                            _ => (
                                "404 Not Found",
                                serde_json::json!({
                                    "error": format!("unexpected path: {path}")
                                })
                                .to_string(),
                            ),
                        };

                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        stream.write_all(response.as_bytes()).unwrap();
                        seen += 1;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(err) => panic!("mock server accept failed: {err}"),
                }
            }

            assert_eq!(seen, 3, "expected 3 requests, saw {seen}");
        });

        (format!("http://{}", addr), handle)
    }

    fn start_readiness_mock_server(
        requests: Arc<Mutex<Vec<String>>>,
        expected_requests: usize,
        cli_meta_body: serde_json::Value,
        routes_body: serde_json::Value,
        me_body: serde_json::Value,
        auths_body: serde_json::Value,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();

        let handle = thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut seen = 0;

            while seen < expected_requests && std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap();
                        let request = String::from_utf8_lossy(&buf[..n]);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("")
                            .to_string();
                        requests.lock().unwrap().push(path.clone());

                        let (status, body) = match path.as_str() {
                            "/v1/cli/meta" => ("200 OK", cli_meta_body.to_string()),
                            "/v1/routes/me" => ("200 OK", routes_body.to_string()),
                            "/v1/auth/me" => ("200 OK", me_body.to_string()),
                            "/v1/authorizations/me/8453" => ("200 OK", auths_body.to_string()),
                            _ => (
                                "404 Not Found",
                                serde_json::json!({
                                    "error": format!("unexpected path: {path}")
                                })
                                .to_string(),
                            ),
                        };

                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        stream.write_all(response.as_bytes()).unwrap();
                        seen += 1;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(err) => panic!("mock server accept failed: {err}"),
                }
            }

            assert_eq!(
                seen, expected_requests,
                "expected {expected_requests} requests, saw {seen}"
            );
        });

        (format!("http://{}", addr), handle)
    }

    fn make_item(name: &str, url: &str, source: &str, amount: &str) -> serde_json::Value {
        serde_json::json!({
            "resource": url, "name": name,
            "description": format!("{name} description"),
            "category": "AI", "source": source,
            "accepts": [{"maxAmountRequired": amount, "network": "base"}],
        })
    }

    #[test]
    fn test_format_terse_output() {
        let items = vec![make_item("Svc", "https://a.com", "cdp_bazaar", "1000000")];
        let json = format_services_terse(&items);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["url"], "https://a.com");
        assert_eq!(parsed[0]["source"], "cdp_bazaar");
        assert_eq!(parsed[0]["price_usdc"], "1.0000");
    }

    #[test]
    fn test_format_terse_empty() {
        let json = format_services_terse(&[]);
        assert_eq!(json, "[]");
    }

    #[test]
    fn test_format_human_output() {
        let items = vec![make_item(
            "AI Svc",
            "https://a.com",
            "cdp_bazaar",
            "1000000",
        )];
        let out = format_services_human(&items, 1, None);
        assert!(out.contains("AI Svc [cdp_bazaar]"));
        assert!(out.contains("https://a.com"));
        assert!(out.contains("$1.0000 USDC"));
    }

    #[test]
    fn test_format_human_empty() {
        let out = format_services_human(&[], 0, None);
        assert_eq!(out, "No services found");
    }

    #[test]
    fn test_format_human_with_query() {
        let out = format_services_human(&[], 0, Some("test"));
        assert_eq!(out, "No services found matching \"test\"");
    }

    #[test]
    fn test_format_human_price_display() {
        let items = vec![make_item("Svc", "https://a.com", "x", "1000000")];
        let out = format_services_human(&items, 1, None);
        assert!(out.contains("$1.0000 USDC"));
    }

    #[test]
    fn test_format_human_truncate_desc() {
        let long_desc = "a".repeat(80);
        let item = serde_json::json!({
            "resource": "https://a.com", "name": "", "description": long_desc,
            "category": "AI", "source": "x",
            "accepts": [{"maxAmountRequired": "0"}],
        });
        let out = format_services_human(&[item], 1, None);
        assert!(out.contains("..."));
    }

    #[test]
    fn test_format_terse_zero_price() {
        let items = vec![make_item("Free", "https://f.com", "x", "0")];
        let json = format_services_terse(&items);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["price_usdc"], "0.0000");
    }

    #[test]
    fn test_body_json_object_passthrough() {
        let data = Some(r#"{"query":"bitcoin","limit":5}"#);
        let parsed: Option<serde_json::Value> = data
            .map(|s| serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.to_string())));
        // Should be an Object, not a String
        assert!(parsed.as_ref().unwrap().is_object());
        assert_eq!(parsed.as_ref().unwrap()["query"], "bitcoin");
    }

    #[test]
    fn test_body_invalid_json_fallback() {
        let data = Some("not valid json");
        let parsed: Option<serde_json::Value> = data
            .map(|s| serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.to_string())));
        // Should fallback to String
        assert!(parsed.as_ref().unwrap().is_string());
        assert_eq!(parsed.as_ref().unwrap().as_str().unwrap(), "not valid json");
    }

    #[test]
    fn test_build_payment_request_body_uses_camel_case_fields() {
        let body = build_payment_request_body(
            "https://example.com/pay",
            "post",
            HashMap::from([("x-test".to_string(), "1".to_string())]),
            Some(serde_json::json!({"hello":"world"})),
            143,
            Some("1000000".to_string()),
            "mpp".to_string(),
            Some("monad".to_string()),
            Some("route-base".to_string()),
            Some("route-monad".to_string()),
        );

        assert_eq!(body.get("chainId").and_then(|v| v.as_u64()), Some(143));
        assert_eq!(
            body.get("executionPipelineId").and_then(|v| v.as_str()),
            Some("route-monad")
        );
        assert!(body.get("chain_id").is_none());
        assert!(body.get("execution_pipeline_id").is_none());
    }

    #[test]
    fn test_format_usdc_amount_small_values() {
        assert_eq!(format_usdc_amount(1200), "$0.001200");
        assert_eq!(format_usdc_amount(12_000), "$0.0120");
        assert_eq!(format_usdc_amount(1_000_000), "$1.00");
    }

    #[test]
    fn test_extract_settlement_tx_hash_nested_payment() {
        let body = serde_json::json!({
            "status": "confirmed",
            "payment": {
                "settlement": {
                    "transactionHash": "0xabc123"
                }
            }
        });
        assert_eq!(
            extract_settlement_tx_hash(&body).as_deref(),
            Some("0xabc123")
        );
    }

    #[test]
    fn test_charge_response_success_flag_reads_x402_body() {
        let charge = serde_json::json!({
            "x402_response_body": "{\"success\":false,\"error\":\"boom\"}"
        });
        assert_eq!(charge_response_success_flag(&charge), Some(false));
    }

    #[test]
    fn test_resolve_chain_id_from_routes_prefers_selected_pipeline() {
        let routes = vec![
            serde_json::json!({
                "id": "first-active",
                "status": "active",
                "chain_id": 8453
            }),
            serde_json::json!({
                "id": "selected-pipeline",
                "status": "active",
                "chain_id": 143
            }),
        ];

        let chain_id =
            resolve_chain_id_from_routes(None, &routes, Some("selected-pipeline")).unwrap();
        assert_eq!(chain_id, 143);
    }

    #[test]
    fn test_resolve_chain_id_from_routes_falls_back_to_first_active_pipeline() {
        let routes = vec![
            serde_json::json!({
                "id": "inactive-selected",
                "status": "inactive",
                "chain_id": 8453
            }),
            serde_json::json!({
                "id": "first-active",
                "status": "active",
                "chain_id": 42161
            }),
        ];

        let chain_id =
            resolve_chain_id_from_routes(None, &routes, Some("inactive-selected")).unwrap();
        assert_eq!(chain_id, 42161);
    }

    #[test]
    fn test_pipeline_create_alias_parses() {
        let cli = Cli::parse_from(["facto", "pipeline", "create"]);
        assert!(matches!(
            cli.command,
            Commands::Pipelines {
                action: Some(PipelineAction::Create {
                    protocol: None,
                    base_x402: false,
                    mpp: false,
                    chain: None,
                    for_pay: None
                })
            }
        ));
    }

    #[test]
    fn test_pipeline_create_targeted_args_parse() {
        let cli = Cli::parse_from([
            "facto",
            "pipeline",
            "create",
            "--mpp",
            "--chain",
            "monad",
            "--for-pay",
            "https://api.example/pay",
        ]);
        assert!(matches!(
            cli.command,
            Commands::Pipelines {
                action: Some(PipelineAction::Create {
                    protocol,
                    base_x402,
                    mpp,
                    chain,
                    for_pay
                })
            } if protocol.is_none() && !base_x402 && mpp && chain.as_deref() == Some("monad") && for_pay.as_deref() == Some("https://api.example/pay")
        ));
    }

    #[test]
    fn test_pipeline_create_x402_shortcut_parses() {
        let cli = Cli::parse_from(["facto", "pipeline", "create", "--x402"]);
        assert!(matches!(
            cli.command,
            Commands::Pipelines {
                action: Some(PipelineAction::Create {
                    protocol,
                    base_x402,
                    mpp,
                    chain,
                    for_pay
                })
            } if protocol.is_none() && base_x402 && !mpp && chain.is_none() && for_pay.is_none()
        ));
    }

    #[test]
    fn test_session_cli_meta_uses_backend_pipeline_create_url() {
        let session = SessionCreateResponse {
            session_id: "sess_123".to_string(),
            auth_url: "https://user.example/cli-auth?session=sess_123".to_string(),
            frontend_url: Some("https://user.example/".to_string()),
            pipeline_create_url: Some(
                "https://user.example/custom/pipelines/create?source=cli&chain=base".to_string(),
            ),
        };

        let meta = session_cli_meta(&session).unwrap();
        assert_eq!(meta.frontend_url, "https://user.example");
        assert_eq!(
            meta.pipeline_create_url,
            "https://user.example/custom/pipelines/create?source=cli&chain=base"
        );
    }

    #[test]
    fn test_session_cli_meta_derives_pipeline_create_url_when_backend_omits_it() {
        let session = SessionCreateResponse {
            session_id: "sess_123".to_string(),
            auth_url: "https://user.example/cli-auth?session=sess_123".to_string(),
            frontend_url: Some("https://user.example/".to_string()),
            pipeline_create_url: None,
        };

        let meta = session_cli_meta(&session).unwrap();
        assert_eq!(meta.frontend_url, "https://user.example");
        assert_eq!(
            meta.pipeline_create_url,
            "https://user.example/pipelines/create?source=cli"
        );
    }

    #[test]
    fn test_detect_post_login_readiness_needs_pipeline() {
        let _guard = env_lock().lock().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (api_url, server) = start_readiness_mock_server(
            requests.clone(),
            2,
            serde_json::json!({
                "frontend_url": "https://user.example",
                "pipeline_create_url": "https://user.example/pipelines/create?source=cli"
            }),
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!([]),
        );
        let _env = TestEnv::new(&api_url);

        let readiness = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { detect_post_login_readiness(&api::FactoApi::new(), "test").await })
            .unwrap();

        server.join().unwrap();

        assert_eq!(readiness.status, "needs_pipeline");
        assert_eq!(readiness.next_command, Some("facto pipeline create"));
        assert_eq!(
            readiness.continue_url.as_deref(),
            Some("https://user.example/pipelines/create?source=cli")
        );
        assert_eq!(
            requests.lock().unwrap().clone(),
            vec!["/v1/cli/meta".to_string(), "/v1/routes/me".to_string()]
        );
    }

    #[test]
    fn test_detect_post_login_readiness_configured() {
        let _guard = env_lock().lock().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (api_url, server) = start_readiness_mock_server(
            requests.clone(),
            2,
            serde_json::json!({
                "frontend_url": "https://user.example",
                "pipeline_create_url": "https://user.example/pipelines/create?source=cli"
            }),
            serde_json::json!([
                {
                    "id": "route-1",
                    "chain_id": 8453,
                    "status": "active"
                }
            ]),
            serde_json::json!({}),
            serde_json::json!([]),
        );
        let _env = TestEnv::new(&api_url);

        let readiness = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { detect_post_login_readiness(&api::FactoApi::new(), "test").await })
            .unwrap();

        server.join().unwrap();

        assert_eq!(readiness.status, "configured");
        assert_eq!(readiness.pipeline_id.as_deref(), Some("route-1"));
    }

    #[test]
    fn test_detect_post_login_readiness_configured_with_multiple_routes() {
        let _guard = env_lock().lock().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (api_url, server) = start_readiness_mock_server(
            requests.clone(),
            2,
            serde_json::json!({
                "frontend_url": "https://user.example",
                "pipeline_create_url": "https://user.example/pipelines/create?source=cli"
            }),
            serde_json::json!([
                {
                    "id": "route-1",
                    "chain_id": 8453,
                    "status": "active"
                },
                {
                    "id": "route-2",
                    "chain_id": 8453,
                    "status": "active"
                }
            ]),
            serde_json::json!({}),
            serde_json::json!([]),
        );
        let _env = TestEnv::new(&api_url);

        let readiness = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { detect_post_login_readiness(&api::FactoApi::new(), "test").await })
            .unwrap();

        server.join().unwrap();

        assert_eq!(readiness.status, "configured");
        assert_eq!(readiness.pipeline_id.as_deref(), Some("route-1"));
        assert_eq!(readiness.next_command, None);
    }

    #[test]
    fn test_cmd_balance_uses_selected_pipeline_chain() {
        let _guard = env_lock().lock().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (api_url, server) = start_balance_mock_server(requests.clone());
        let _env = TestEnv::new(&api_url);

        config::save_credentials(&config::Credentials {
            token: "test-token".to_string(),
            user_id: "user-123".to_string(),
            email: Some("dev@facto.to".to_string()),
            expires_at: "2099-12-31T00:00:00Z".parse().unwrap(),
            api_key: None,
            signing_key: None,
            auth_mode: Some("privy".to_string()),
            deposit_addresses: None,
        })
        .unwrap();
        config::save_config(&config::AppConfig {
            env: "dev".to_string(),
            api_urls: HashMap::new(),
            default_pipeline_id: Some("stale-local-default".to_string()),
            default_pipeline_cached_at: None,
            last_update_check: None,
            frontend_url: None,
            frontend_url_cached_at: None,
        })
        .unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            cmd_balance(None, true).await.unwrap();
        });

        server.join().unwrap();

        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests[0], "/v1/routes/me");
        assert_eq!(requests[1], "/v1/user/preferences");
        assert_eq!(requests[2], "/v1/x402/balance?chain_id=143");

        let cfg = config::load_config();
        assert_eq!(
            cfg.default_pipeline_id.as_deref(),
            Some("selected-pipeline")
        );
    }

    #[test]
    fn test_ensure_user_bearer_auth_rejects_api_key_mode() {
        let creds = config::Credentials {
            token: String::new(),
            user_id: String::new(),
            email: None,
            expires_at: "2099-12-31T00:00:00Z".parse().unwrap(),
            api_key: Some("key".to_string()),
            signing_key: Some("secret".to_string()),
            auth_mode: Some("api_key".to_string()),
            deposit_addresses: None,
        };
        let err = ensure_user_bearer_auth(&creds).unwrap_err().to_string();
        assert!(err.contains("API key authentication is not supported"));
    }

    #[test]
    fn test_fund_spend_mode_uses_route_spend_mode() {
        let route = serde_json::json!({
            "spend_mode": "borrow"
        });

        assert_eq!(fund_spend_mode(&route), "borrow");
    }

    #[test]
    fn test_fund_spend_mode_defaults_to_withdraw() {
        let route = serde_json::json!({});

        assert_eq!(fund_spend_mode(&route), "withdraw");
    }
}

async fn cmd_services(query: Option<&str>, category: Option<&str>, terse: bool) -> Result<()> {
    let api_base = config::api_url();
    let client = reqwest::Client::new();

    let mut url = format!("{api_base}/v1/x402/discovery?page_size=100");
    if let Some(q) = query {
        url.push_str(&format!("&q={}", urlencoding::encode(q)));
    }
    if let Some(cat) = category {
        url.push_str(&format!("&category={}", urlencoding::encode(cat)));
    }

    let resp = client
        .get(&url)
        .send()
        .await
        .context("failed to fetch x402 services")?;
    if !resp.status().is_success() {
        bail!("discovery API returned {}", resp.status());
    }

    let data: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse discovery response")?;
    let items = data["items"].as_array().cloned().unwrap_or_default();
    let total = data["pagination"]["total"]
        .as_i64()
        .unwrap_or(items.len() as i64);

    if terse {
        println!("{}", format_services_terse(&items));
    } else {
        println!("{}", format_services_human(&items, total, query));
    }

    Ok(())
}

fn cmd_config(
    env: Option<String>,
    deposit_address: Option<Vec<String>>,
    show: bool,
    terse: bool,
) -> Result<()> {
    let mut changed = false;

    // --env: switch environment
    if let Some(ref new_env) = env {
        let mut cfg = config::load_config();
        cfg.env = new_env.clone();
        config::save_config(&cfg)?;
        changed = true;
        if terse {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "env": new_env,
                    "api_url": config::api_url(),
                }))?
            );
        } else {
            println!("✅ Environment set: {new_env}");
            println!("   API: {}", config::api_url());
        }
    }

    // --deposit-address: set per-chain deposit address
    if let Some(args) = deposit_address {
        if args.len() != 2 {
            bail!(
                "Usage: --deposit-address <chain> <address>\nExample: --deposit-address arbitrum 0xfc8a..."
            );
        }
        let chain = args[0].to_lowercase();
        let address = &args[1];

        if !address.starts_with("0x") || address.len() != 42 {
            bail!(
                "Invalid address: {address}\nMust be a 42-character 0x-prefixed Ethereum address"
            );
        }

        let mut creds = config::load_credentials()?;
        let map = creds.deposit_addresses.get_or_insert_with(Default::default);
        map.insert(chain.clone(), address.clone());
        config::save_credentials(&creds)?;
        changed = true;

        if terse {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "set": chain, "address": address
                }))?
            );
        } else {
            println!("✅ Deposit address set: {chain} → {address}");
        }
    }

    // Show config when --show or no flags given
    if show || (!changed && env.is_none()) {
        let cfg = config::load_config();
        let creds = config::load_credentials().ok();
        let addrs = creds.as_ref().and_then(|c| c.deposit_addresses.as_ref());

        if terse {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "env": cfg.env,
                    "api_url": config::api_url(),
                    "deposit_addresses": addrs,
                }))?
            );
        } else {
            println!("Environment:  {} ({})", cfg.env, config::api_url());
            println!();
            println!("Deposit Addresses:");
            match addrs {
                Some(m) if !m.is_empty() => {
                    for (chain, addr) in m {
                        println!("  {chain:<15} {addr}");
                    }
                }
                _ => {
                    println!("  (none configured)");
                }
            }
        }
    }

    Ok(())
}

fn cmd_logout() -> Result<()> {
    config::clear_credentials()?;
    println!("Logged out. Credentials cleared.");
    Ok(())
}

// ── Chain helpers ───────────────────────────────────────────────────────

/// Supported chains for `facto fund` (charge execution).
/// Other chains are visible in `pipelines` but cannot be funded via CLI yet.
const SUPPORTED_FUND_CHAINS: &[u64] = &[
    143, // Monad (x402 agentic payments)
    8453, // Base (x402 agentic payments, Aave V3)
         // 42161, // Arbitrum (cross-chain, requires bridge)
         // 42431, // Tempo Testnet
];

fn is_fund_supported(chain_id: u64) -> bool {
    SUPPORTED_FUND_CHAINS.contains(&chain_id)
}

fn fund_spend_mode(route: &serde_json::Value) -> &str {
    route["spend_mode"].as_str().unwrap_or("withdraw")
}

fn chain_display_name(chain_id: u64) -> String {
    match chain_id {
        42161 => "Arbitrum".to_string(),
        421614 => "Arbitrum Sepolia".to_string(),
        143 => "Monad".to_string(),
        8453 => "Base".to_string(),
        1329 => "Sei".to_string(),
        137 => "Polygon".to_string(),
        4326 => "MegaETH".to_string(),
        988 => "Stable Mainnet".to_string(),
        46630 => "Robinhood Testnet".to_string(),
        other => format!("Chain {other}"),
    }
}

// ---------------------------------------------------------------------------
// Self-upgrade
// ---------------------------------------------------------------------------

const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/Facto-to/facto-cli/releases/latest";
const GITHUB_DOWNLOAD_BASE: &str = "https://github.com/Facto-to/facto-cli/releases/latest/download";

/// Check GitHub for the latest release version.
/// Returns the version string (without leading "v") on success.
async fn check_latest_version() -> Option<String> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .user_agent("facto-cli")
        .build()
        .ok()?;
    let release: Release = client
        .get(GITHUB_RELEASES_API)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    Some(release.tag_name.trim_start_matches('v').to_string())
}

/// Throttled update check — at most once every 4 hours.
/// Returns Some(latest_version) only if a newer version exists.
async fn check_update_throttled() -> Option<String> {
    let cfg = config::load_config();
    if let Some(last) = cfg.last_update_check {
        let elapsed = chrono::Utc::now() - last;
        if elapsed < chrono::Duration::hours(4) {
            return None; // Checked recently, skip
        }
    }

    let latest = check_latest_version().await?;

    // Record check time regardless of result
    let mut cfg = config::load_config();
    cfg.last_update_check = Some(chrono::Utc::now());
    let _ = config::save_config(&cfg);

    let current = env!("CARGO_PKG_VERSION");
    if latest != current {
        Some(latest)
    } else {
        None
    }
}

/// Resolve the binary filename for the current OS and architecture.
fn upgrade_binary_name() -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        other => bail!("Unsupported OS: {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => bail!("Unsupported architecture: {other}"),
    };
    Ok(format!("facto-{os}-{arch}"))
}

/// Download and install the latest binary, replacing the current one.
async fn do_upgrade(latest: &str) -> Result<()> {
    let binary_name = upgrade_binary_name()?;
    let url = format!("{GITHUB_DOWNLOAD_BASE}/{binary_name}");
    let install_dir = config::facto_dir()?.join("bin");
    std::fs::create_dir_all(&install_dir)?;
    let target = install_dir.join("facto");
    let tmp = install_dir.join(".facto-upgrade-tmp");

    eprintln!("Downloading facto v{latest} ({binary_name})...");

    let client = reqwest::Client::builder()
        .user_agent("facto-cli")
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?;
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        bail!("Download failed: HTTP {} from {}", resp.status(), url);
    }
    let bytes = resp.bytes().await?;
    std::fs::write(&tmp, &bytes)
        .with_context(|| format!("Failed to write to {}", tmp.display()))?;

    // Make executable (Unix)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }

    // Atomic replace
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("Failed to replace {}", target.display()))?;

    eprintln!("✅ facto upgraded to v{latest}");
    Ok(())
}

/// `facto upgrade` command handler.
async fn cmd_upgrade(yes: bool, terse: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");

    if terse {
        // Machine-readable: just print version info as JSON
        let latest = check_latest_version()
            .await
            .unwrap_or_else(|| current.to_string());
        let up_to_date = latest == current;
        println!(
            "{}",
            serde_json::json!({
                "current": current,
                "latest": latest,
                "up_to_date": up_to_date,
            })
        );
        if up_to_date {
            return Ok(());
        }
        // In terse mode, don't actually upgrade — just report
        return Ok(());
    }

    eprintln!("Checking for updates...");
    let latest = match check_latest_version().await {
        Some(v) => v,
        None => {
            eprintln!("Could not reach GitHub to check for updates.");
            return Ok(());
        }
    };

    if latest == current {
        eprintln!("✅ Already up to date (v{current})");
        return Ok(());
    }

    eprintln!("  Current: v{current}");
    eprintln!("  Latest:  v{latest}");

    if !yes {
        eprint!("\nUpgrade to v{latest}? [Y/n] ");
        std::io::stderr().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim().to_lowercase();
        if !input.is_empty() && input != "y" && input != "yes" {
            eprintln!("Upgrade cancelled.");
            return Ok(());
        }
    }

    do_upgrade(&latest).await?;

    // Update check timestamp
    let mut cfg = config::load_config();
    cfg.last_update_check = Some(chrono::Utc::now());
    let _ = config::save_config(&cfg);

    Ok(())
}
