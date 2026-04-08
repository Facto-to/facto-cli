//! Interactive service selection and payment confirmation flows.
//!
//! Provides two public entry points:
//! - [`interactive_service_flow`] — full interactive flow for `facto services --interactive`
//! - [`confirm_before_pay`]       — pre-payment confirmation for `facto pay`

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::Write as _;

use crate::api::FactoApi;
use crate::config;

// ── Public API ────────────────────────────────────────────────────────────────

/// Full interactive flow for `facto services "<query>" --interactive`.
///
/// Steps:
/// 1. Search services via the discovery API.
/// 2. Display a formatted table and prompt the user to select one.
/// 3. Check balance; prompt to fund if insufficient.
/// 4. Confirm and execute payment via `POST /v1/x402/pay`.
/// 5. Print the response body to stdout.
pub async fn interactive_service_flow(
    query: Option<&str>,
    category: Option<&str>,
    api: &FactoApi,
    token: &str,
    default_pipeline_id: Option<&str>,
) -> Result<()> {
    // Step 1: search services.
    let items = search_services(query, category).await?;
    if items.is_empty() {
        bail!("No services found matching your query.");
    }

    // Step 2: display table and prompt selection.
    display_service_table(&items, query);
    let idx = prompt_selection(&items)?;
    let service = &items[idx];

    let url = service["resource"]
        .as_str()
        .or_else(|| service["url"].as_str())
        .or_else(|| service["endpoint"].as_str())
        .unwrap_or("")
        .to_string();

    let method = service["method"].as_str().unwrap_or("GET").to_string();

    // Extract price from accepts[0].maxAmountRequired (atomic USDC, 6 dec).
    let max_amount_atomic: u64 = service["accepts"]
        .get(0)
        .and_then(|a| a["maxAmountRequired"].as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| {
            service["accepts"]
                .get(0)
                .and_then(|a| a["maxAmountRequired"].as_u64())
        })
        .unwrap_or(0);

    let price_display = format_usdc_display(max_amount_atomic);

    if max_amount_atomic == 0 {
        eprintln!("  ⚠ Listed price is $0 — the service may still charge at call time.");
        eprintln!("    Use --max-amount to cap spending.\n");
    }

    // Step 3: check balance.
    let balance_resp: Value = api
        .get("/v1/x402/balance?chain_id=8453", Some(token))
        .await
        .context("Failed to fetch balance")?;

    let balance_atomic = parse_balance_atomic(&balance_resp);

    if balance_atomic >= max_amount_atomic {
        // Balance sufficient — confirm.
        let service_name = service_display_name(service);
        eprintln!();
        eprintln!("  Service : {service_name}");
        eprintln!("  Price   : {price_display}");
        eprintln!("  Balance : {}", format_usdc_display(balance_atomic));
        eprintln!();
        if !prompt_yn("Proceed with payment? [Y/n]")? {
            bail!("Payment cancelled.");
        }
    } else {
        // Balance insufficient — offer to fund.
        let deficit = max_amount_atomic.saturating_sub(balance_atomic);
        eprintln!();
        eprintln!("  Insufficient balance.");
        eprintln!("  Required : {price_display}");
        eprintln!("  Balance  : {}", format_usdc_display(balance_atomic));
        eprintln!("  Deficit  : {}", format_usdc_display(deficit));
        eprintln!();
        if !prompt_yn("Fund from Pipeline? [Y/n]")? {
            bail!("Payment cancelled — insufficient balance.");
        }
        fund_from_pipeline(api, token, default_pipeline_id, max_amount_atomic).await?;
    }

    // Step 4: execute payment.
    eprintln!("Executing payment…");
    let pay_body = serde_json::json!({
        "url": url,
        "method": method,
        "chain_id": 8453,
        "max_amount": max_amount_atomic.to_string(),
    });

    // We want the raw response body, so bypass the typed parse path.
    let pay_url = format!("{}/v1/x402/pay", config::api_url());
    let client = reqwest::Client::new();
    let resp = client
        .post(&pay_url)
        .bearer_auth(token)
        .json(&pay_body)
        .send()
        .await
        .context("POST /v1/x402/pay failed")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("Payment failed ({}): {}", status, body);
    }

    // Step 5: print response to stdout.
    println!("{body}");
    Ok(())
}

/// Pre-payment confirmation for `facto pay`.
///
/// Returns `true` if the caller should proceed with payment, `false` if cancelled.
pub async fn confirm_before_pay(
    api: &FactoApi,
    token: &str,
    url: &str,
    max_amount: u64,
    default_pipeline_id: Option<&str>,
    terse: bool,
) -> Result<bool> {
    // Agent / terse mode: skip all prompts.
    if terse {
        return Ok(true);
    }

    // Check balance.
    let balance_resp: Value = api
        .get("/v1/x402/balance?chain_id=8453", Some(token))
        .await
        .context("Failed to fetch balance")?;

    let balance_atomic = parse_balance_atomic(&balance_resp);

    eprintln!();
    eprintln!("  Target  : {url}");
    eprintln!("  Max cost: {}", format_usdc_display(max_amount));
    if max_amount == 0 {
        eprintln!("  ⚠ Listed as free — actual charge may differ");
    }
    eprintln!("  Balance : {}", format_usdc_display(balance_atomic));
    eprintln!();

    if balance_atomic < max_amount {
        let deficit = max_amount.saturating_sub(balance_atomic);
        eprintln!("  Deficit : {}", format_usdc_display(deficit));
        eprintln!();
        if prompt_yn("Fund from Pipeline? [Y/n]")? {
            fund_from_pipeline(api, token, default_pipeline_id, max_amount).await?;
        } else {
            return Ok(false);
        }
    }

    Ok(prompt_yn("Proceed with payment? [Y/n]")?)
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Calls the x402 discovery API and returns the items array.
async fn search_services(query: Option<&str>, category: Option<&str>) -> Result<Vec<Value>> {
    let base = config::api_url();
    let mut qs = "page_size=20&alive_only=true".to_string();
    if let Some(q) = query {
        qs.push_str(&format!("&q={}", urlencoding::encode(q)));
    }
    if let Some(c) = category {
        qs.push_str(&format!("&category={}", urlencoding::encode(c)));
    }
    let url = format!("{base}/v1/x402/discovery?{qs}");

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url} failed"))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("Discovery API error ({}): {}", status, body);
    }

    let json: Value = serde_json::from_str(&body).context("Failed to parse discovery response")?;

    // Accept both `{ items: [...] }` and bare arrays.
    let items = if let Some(arr) = json.get("items").and_then(|v| v.as_array()) {
        arr.clone()
    } else if let Some(arr) = json.as_array() {
        arr.clone()
    } else {
        vec![]
    };

    Ok(items)
}

/// Prints a formatted table of services to stderr.
fn display_service_table(items: &[Value], query: Option<&str>) {
    if let Some(q) = query {
        eprintln!("\nServices matching \"{q}\":\n");
    } else {
        eprintln!("\nAvailable services:\n");
    }

    eprintln!(
        "  {:<4} {:<36} {:<10} {:<16} {:<8}",
        "#", "Service", "Price", "Network", "Method"
    );
    eprintln!("  {}", "-".repeat(76));

    for (i, item) in items.iter().enumerate() {
        let name = service_display_name(item);
        let name_trunc = truncate_str(&name, 34);

        let price_atomic: u64 = item["accepts"]
            .get(0)
            .and_then(|a| a["maxAmountRequired"].as_str())
            .and_then(|s| s.parse().ok())
            .or_else(|| {
                item["accepts"]
                    .get(0)
                    .and_then(|a| a["maxAmountRequired"].as_u64())
            })
            .unwrap_or(0);
        let price = format_usdc_display(price_atomic);

        let network = item["network"]
            .as_str()
            .or_else(|| item["chain"].as_str())
            .unwrap_or("Base");

        let method = item["method"].as_str().unwrap_or("GET");

        eprintln!(
            "  {:<4} {:<36} {:<10} {:<16} {:<8}",
            i + 1,
            name_trunc,
            price,
            network,
            method
        );
    }
    eprintln!();
}

/// Prompts the user to enter a number corresponding to a service.
fn prompt_selection(items: &[Value]) -> Result<usize> {
    loop {
        eprint!("Select service (1–{}): ", items.len());
        std::io::stderr().flush().ok();
        let mut line = String::new();
        let bytes = std::io::stdin()
            .read_line(&mut line)
            .context("Failed to read user input")?;
        if bytes == 0 {
            bail!("No selection (stdin closed).");
        }
        let trimmed = line.trim();
        if let Ok(n) = trimmed.parse::<usize>() {
            if n >= 1 && n <= items.len() {
                return Ok(n - 1);
            }
        }
        eprintln!("Please enter a number between 1 and {}.", items.len());
    }
}

/// Prompts a yes/no question; empty input defaults to yes.
fn prompt_yn(question: &str) -> Result<bool> {
    eprint!("{question} ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    let bytes = std::io::stdin()
        .read_line(&mut line)
        .context("Failed to read user input")?;
    if bytes == 0 {
        return Ok(false); // EOF → treat as "no"
    }
    let answer = line.trim().to_lowercase();
    Ok(answer.is_empty() || answer == "y" || answer == "yes")
}

/// Fetches the server wallet balance and returns a display string.
#[allow(dead_code)]
async fn check_balance(api: &FactoApi, token: &str) -> Result<String> {
    let resp: Value = api
        .get("/v1/x402/balance?chain_id=8453", Some(token))
        .await
        .context("Failed to fetch balance")?;
    let atomic = parse_balance_atomic(&resp);
    Ok(format_usdc_display(atomic))
}

/// Funds the server wallet from a pipeline. Handles failures by offering alternatives.
async fn fund_from_pipeline(
    api: &FactoApi,
    token: &str,
    default_pipeline_id: Option<&str>,
    min_amount_atomic: u64,
) -> Result<()> {
    // Resolve which pipeline to use.
    let pipeline_id = select_funding_pipeline(api, token, default_pipeline_id).await?;

    // Suggest $5.00 or the minimum required amount, whichever is larger.
    let suggested_atomic = min_amount_atomic.max(5_000_000); // $5.00 in 6-dec
    let suggested_human = atomic_to_human(suggested_atomic);

    eprint!("Amount to fund (Enter = ${suggested_human}): ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("Failed to read user input")?;
    let input = line.trim();

    let amount_human: f64 = if input.is_empty() {
        suggested_human
    } else {
        input
            .parse::<f64>()
            .context("Invalid amount — expected a number like 5 or 0.50")?
    };

    if amount_human <= 0.0 {
        bail!("Amount must be positive.");
    }

    // Try to execute the fund — if it fails, offer recovery options.
    match try_fund(api, token, &pipeline_id, amount_human).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            eprintln!();
            eprintln!("  ❌ Funding failed: {msg}");
            eprintln!();
            eprintln!("  Options:");
            eprintln!("    [1] Try a different pipeline");
            eprintln!("    [2] Complete authorization at {}/pipelines", crate::pipeline_init::frontend_url());
            eprintln!("    [3] Exit");
            eprintln!();

            loop {
                eprint!("  Choose (1–3): ");
                std::io::stderr().flush().ok();
                let mut choice = String::new();
                let bytes = std::io::stdin().read_line(&mut choice).unwrap_or(0);
                if bytes == 0 {
                    bail!("Funding cancelled (no input).");
                }
                match choice.trim() {
                    "1" => {
                        // List all pipelines and let user pick a different one.
                        let alt_id = select_funding_pipeline(api, token, None).await?;
                        return try_fund(api, token, &alt_id, amount_human).await;
                    }
                    "2" => {
                        let url = format!("{}/pipelines/{pipeline_id}", crate::pipeline_init::frontend_url());
                        eprintln!("  Opening: {url}");
                        let _ = open::that(&url);
                        bail!("Complete the authorization in your browser, then retry.");
                    }
                    "3" => {
                        bail!("Funding cancelled.");
                    }
                    _ => {
                        eprintln!("  Please enter 1, 2, or 3.");
                    }
                }
            }
        }
    }
}

/// Resolve which pipeline to fund from. Shows a selection list if no default or if explicit_id is None.
async fn select_funding_pipeline(
    api: &FactoApi,
    token: &str,
    preferred_id: Option<&str>,
) -> Result<String> {
    if let Some(id) = preferred_id {
        // Verify it exists.
        let route: Result<Value> = api.get(&format!("/v1/routes/{id}"), Some(token)).await;
        if let Ok(r) = route {
            if r["status"].as_str() == Some("active") {
                let name = r["name"].as_str().unwrap_or(id);
                let chain = r["chain_id"].as_u64().unwrap_or(0);
                let asset = r["asset_symbol"].as_str().unwrap_or("?");
                eprintln!("  Using pipeline: {name} ({asset} on chain {chain})");
                return Ok(id.to_string());
            }
        }
        eprintln!("  Default pipeline {id} is not available. Listing alternatives…");
    }

    // Fetch all active pipelines.
    let routes: Vec<Value> = api
        .get("/v1/routes/me", Some(token))
        .await
        .context("Failed to fetch pipelines")?;

    let active: Vec<&Value> = routes
        .iter()
        .filter(|r| r["status"].as_str() == Some("active"))
        .collect();

    if active.is_empty() {
        bail!(
            "No active pipelines found. Create one at {}/pipelines",
            crate::pipeline_init::frontend_url()
        );
    }

    eprintln!();
    eprintln!("  Available pipelines:");
    for (i, r) in active.iter().enumerate() {
        let name = r["name"].as_str().unwrap_or("unnamed");
        let chain = r["chain_id"].as_u64().unwrap_or(0);
        let asset = r["asset_symbol"].as_str().unwrap_or("?");
        let protocol = r["protocol_id"].as_str().unwrap_or("?");
        eprintln!("    [{}] {} — {} {} (chain {})", i + 1, name, protocol, asset, chain);
    }
    eprintln!();

    loop {
        eprint!("  Select pipeline (1–{}): ", active.len());
        std::io::stderr().flush().ok();
        let mut line = String::new();
        let bytes = std::io::stdin().read_line(&mut line).unwrap_or(0);
        if bytes == 0 {
            bail!("No pipeline selected (no input).");
        }
        if let Ok(n) = line.trim().parse::<usize>() {
            if n >= 1 && n <= active.len() {
                let id = active[n - 1]["id"].as_str().unwrap_or("").to_string();
                return Ok(id);
            }
        }
        eprintln!("  Please enter a number between 1 and {}.", active.len());
    }
}

/// Attempt the actual fund operation.
async fn try_fund(api: &FactoApi, token: &str, pipeline_id: &str, amount_human: f64) -> Result<()> {
    let route: Value = api
        .get(&format!("/v1/routes/{pipeline_id}"), Some(token))
        .await
        .context("Failed to fetch pipeline details")?;

    let asset_decimals = route["asset_decimals"].as_u64().unwrap_or(6) as u32;

    let me: Value = api
        .get("/v1/auth/me", Some(token))
        .await
        .context("Failed to fetch user info")?;

    let server_wallet = me["server_wallet"]
        .as_str()
        .or_else(|| me["wallet_address"].as_str())
        .or_else(|| me["address"].as_str())
        .unwrap_or("")
        .to_string();

    if server_wallet.is_empty() {
        bail!("Could not determine server wallet address.");
    }

    let factor = 10u64.pow(asset_decimals);
    let amount_atomic = (amount_human * factor as f64).round() as u64;

    let now = chrono::Utc::now();
    let ts = now.format("%y%m%d%H%M%S");
    let rnd = rand_u16();
    let invoice_id = format!("CLI-FUND-{ts}-{rnd:04x}");

    eprintln!("  Funding ${amount_human:.2} USDC from pipeline {pipeline_id}…");

    let asset_symbol = route["asset_symbol"].as_str().unwrap_or("USDC").to_string();

    let payload = serde_json::json!({
        "route_id": pipeline_id,
        "invoice_id": invoice_id,
        "underlying_amount": amount_atomic.to_string(),
        "to": server_wallet,
        "asset_symbol": asset_symbol,
        "asset_decimals": asset_decimals,
    });

    let _: Value = api
        .post_authenticated("/v1/charges/execute-7702", &payload, token)
        .await
        .context("Funding failed")?;

    eprintln!("  Funding submitted. Waiting for balance…");
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    eprintln!("  ✓ Done.");
    Ok(())
}

/// Converts atomic USDC (6 decimals) to `"$X.XX"` display string.
pub fn format_usdc_display(atomic: u64) -> String {
    let whole = atomic / 1_000_000;
    let frac = atomic % 1_000_000;
    // Show 4 decimal places to capture sub-cent prices (e.g. $0.0020)
    let frac_4 = frac / 100; // 6 dec → 4 dec
    format!("${whole}.{frac_4:04}")
}

// ── Internal utilities ────────────────────────────────────────────────────────

/// Parses the numeric balance (atomic, 6-dec USDC) from a balance response.
fn parse_balance_atomic(resp: &Value) -> u64 {
    // The balance API returns { "usdc_balance": "945000", ... }
    for key in &["usdc_balance", "balance", "usdc"] {
        if let Some(s) = resp[key].as_str() {
            if let Ok(n) = s.parse::<u64>() {
                return n;
            }
        }
        if let Some(n) = resp[key].as_u64() {
            return n;
        }
    }
    0
}

/// Converts atomic USDC (6 dec) to a human-readable f64.
fn atomic_to_human(atomic: u64) -> f64 {
    atomic as f64 / 1_000_000.0
}

/// Returns a display name for a service entry.
fn service_display_name(item: &Value) -> String {
    item["name"]
        .as_str()
        .or_else(|| item["title"].as_str())
        .or_else(|| item["url"].as_str())
        .or_else(|| item["endpoint"].as_str())
        .unwrap_or("(unknown)")
        .to_string()
}

/// Truncates a string to at most `max_chars` characters.
fn truncate_str(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = chars[..max_chars.saturating_sub(1)].iter().collect();
        format!("{truncated}…")
    }
}

/// Simple pseudo-random u16 derived from nanosecond time (no external dependency).
fn rand_u16() -> u16 {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    (d.subsec_nanos() % 65536) as u16
}
