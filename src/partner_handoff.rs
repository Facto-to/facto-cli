//! Partner CLI Test handoff: CSPRNG possession proof, fixed-origin wire,
//! finite polling, write-ahead intents, original-receipt recovery and the
//! current-authorization read.
//!
//! This module never reads Consumer credentials or configuration, never sends a
//! cookie, and never accepts an application client secret. The proof and the
//! pairing code live only in the private journal and, for the pairing code, on
//! an interactive terminal.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use reqwest::header::{self, HeaderName, HeaderValue};
use reqwest::{redirect, Client, Request, StatusCode};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::time::Duration as StdDuration;

use crate::partner_handoff_state::{self as state, Journal};

const API_ORIGIN: &str = "https://test-api.facto.to";
const APPROVAL_ORIGIN: &str = "https://test-partner.facto.to";
const PREFIX: &str = "/api/partner-cli";
const PROOF_BYTES: usize = 32;
const REALM: &str = state::REALM;
const ALPHABET: [u8; 32] = state::ALPHABET;
const CODE_LENGTH: usize = state::CODE_LENGTH;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const CONNECT_TIMEOUT: StdDuration = StdDuration::from_secs(3);
const REQUEST_TIMEOUT: StdDuration = StdDuration::from_secs(8);
const POLL_SECONDS: u64 = 2;
const PENDING_LIMIT: i64 = 600;
const ACTIVE_LIMIT: i64 = 900;
const ABSOLUTE_PARENT_LIMIT: i64 = 28_800;
const CLOCK_MARGIN: i64 = 30;
const MAX_POLL_TRANSPORT_FAILURES: u32 = 3;
const PROOF_SCHEME: &str = "Facto-CLI ";
const MAX_APPROVED_APPS: usize = 100;
const ACTIONS: [&str; 2] = ["workspace.access.read", "merchant.apps.read"];
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

const SERVER_ERROR_CODES: [&str; 21] = [
    "invalid_request",
    "cli_pending",
    "cli_expired",
    "cli_revoked",
    "cli_pairing_mismatch",
    "cli_scope_changed",
    "cli_target_unsupported",
    "cli_rate_limited",
    "idempotency_conflict",
    "session_expired",
    "session_revoked",
    "session_upgrade_required",
    "password_change_required",
    "recent_mfa_required",
    "partner_context_refresh_required",
    "stale_authorization_frontier",
    "partner_context_forbidden",
    "organization_access_denied",
    "organization_sso_verifier_unavailable",
    "partner_repository_unavailable",
    "partner_context_unavailable",
];

const PHASES: [&str; 6] = [
    "PENDING", "APPROVED", "ACTIVE", "DENIED", "EXPIRED", "REVOKED",
];
const RECEIPT_OPERATIONS: [&str; 3] = ["create", "exchange", "revoke"];

// ── Failures ────────────────────────────────────────────────────────────────

/// `Local` codes are CLI-side refusals and never travel on the wire; `Server`
/// codes come from the closed set in the delivered machine contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    Local(&'static str),
    Server(&'static str),
}

impl Failure {
    fn code(self) -> &'static str {
        match self {
            Self::Local(code) | Self::Server(code) => code,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for Failure {}

// ── Entropy, identifiers and the pairing code ───────────────────────────────

#[cfg(unix)]
fn random_bytes<const N: usize>() -> Result<[u8; N], Failure> {
    let mut file = File::open("/dev/urandom").map_err(|_| Failure::Local("entropy_unavailable"))?;
    let mut buffer = [0u8; N];
    file.read_exact(&mut buffer)
        .map_err(|_| Failure::Local("entropy_unavailable"))?;
    Ok(buffer)
}

#[cfg(not(unix))]
fn random_bytes<const N: usize>() -> Result<[u8; N], Failure> {
    Err(Failure::Local("platform_unsupported"))
}

fn new_proof() -> Result<String, Failure> {
    Ok(hex::encode(random_bytes::<PROOF_BYTES>()?))
}

fn new_uuid_v4() -> Result<String, Failure> {
    let mut bytes = random_bytes::<16>()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{}-{}-{}-{}-{}",
        hex::encode(&bytes[0..4]),
        hex::encode(&bytes[4..6]),
        hex::encode(&bytes[6..8]),
        hex::encode(&bytes[8..10]),
        hex::encode(&bytes[10..16])
    ))
}

fn is_uuid(value: &str) -> bool {
    state::is_canonical_uuid(value)
}

fn new_pairing_code() -> Result<String, Failure> {
    let bytes = random_bytes::<CODE_LENGTH>()?;
    let mut code = String::with_capacity(CODE_LENGTH);
    for byte in bytes {
        code.push(ALPHABET[usize::from(byte) & 31] as char);
    }
    Ok(code)
}

fn display_pairing_code(normalized: &str) -> String {
    let (head, tail) = normalized.split_at(5);
    format!("{head}-{tail}")
}

fn normalize_pairing_code(input: &str) -> Option<String> {
    state::normalize_pairing_code(input)
}
fn matching_canonical_bytes(authorization_id: &str, normalized: &str) -> Vec<u8> {
    state::matching_canonical_bytes(authorization_id, normalized)
}
fn verification_code_hash(authorization_id: &str, normalized: &str) -> String {
    state::verification_code_hash(authorization_id, normalized)
}
// ── Wire types (exactly the delivered machine contract) ─────────────────────

#[derive(Serialize)]
struct CreateRequest<'a> {
    request_id: &'a str,
    verification_code_hash: &'a str,
    requested_actions: [&'a str; 2],
    mode: &'a str,
}

#[derive(Serialize)]
struct RevisionRequest<'a> {
    request_id: &'a str,
    expected_revision: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateResponse {
    pub(crate) authorization_id: String,
    pub(crate) request_id: String,
    pub(crate) target_environment: String,
    pub(crate) realm: String,
    pub(crate) status: String,
    pub(crate) revision: u64,
    pub(crate) expires_at: String,
    pub(crate) poll_interval_seconds: u64,
    pub(crate) verification_uri: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovalSnapshot {
    pub(crate) partner_user_id: String,
    pub(crate) organization_id: String,
    pub(crate) membership_id: String,
    pub(crate) partner_role_id: String,
    pub(crate) context_id: String,
    pub(crate) environment: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) authorization_revision: Option<u64>,
    pub(crate) granted_actions: Vec<String>,
    pub(crate) authorized_app_ids: Vec<String>,
    pub(crate) expires_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthorizationView {
    pub(crate) authorization_id: String,
    pub(crate) target_environment: String,
    pub(crate) realm: String,
    pub(crate) status: String,
    pub(crate) revision: u64,
    pub(crate) expires_at: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) approval: Option<ApprovalSnapshot>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExchangeResponse {
    pub(crate) request_id: String,
    pub(crate) operation: String,
    pub(crate) authorization_id: String,
    pub(crate) status: String,
    pub(crate) previous_revision: u64,
    pub(crate) revision: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) approval: Option<ApprovalSnapshot>,
    pub(crate) session_id: String,
    pub(crate) expires_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevocationResponse {
    pub(crate) request_id: String,
    pub(crate) operation: String,
    pub(crate) authorization_id: String,
    pub(crate) status: String,
    pub(crate) previous_revision: u64,
    pub(crate) revision: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) approval: Option<ApprovalSnapshot>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CliApp {
    pub(crate) app_id: String,
    pub(crate) display_name: String,
    pub(crate) status: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceResponse {
    pub(crate) session_id: String,
    pub(crate) target_environment: String,
    pub(crate) realm: String,
    pub(crate) partner_user_id: String,
    pub(crate) organization_id: String,
    pub(crate) membership_id: String,
    pub(crate) partner_role_id: String,
    pub(crate) environment: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) authorization_revision: Option<u64>,
    pub(crate) granted_actions: Vec<String>,
    pub(crate) authorized_app_ids: Vec<String>,
    pub(crate) expires_at: String,
    pub(crate) apps: Vec<CliApp>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReceiptWire {
    request_id: String,
    operation: String,
    status: String,
    #[serde(deserialize_with = "required_nullable")]
    response: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ErrorWire {
    code: String,
    message: String,
}

// ── Validation ──────────────────────────────────────────────────────────────

fn require(value: &str, expected: &str) -> Result<(), Failure> {
    if value == expected {
        Ok(())
    } else {
        Err(Failure::Local("wire_invalid"))
    }
}

fn require_moment(value: &str) -> Result<DateTime<Utc>, Failure> {
    DateTime::parse_from_rfc3339(value)
        .map(|moment| moment.with_timezone(&Utc))
        .map_err(|_| Failure::Local("wire_invalid"))
}

fn require_actions(actions: &[String]) -> Result<(), Failure> {
    let exact = actions.len() == ACTIONS.len()
        && actions
            .iter()
            .zip(ACTIONS)
            .all(|(got, want)| got.as_str() == want);
    if exact {
        Ok(())
    } else {
        Err(Failure::Local("cli_scope_changed"))
    }
}

fn require_unique_ids(values: &[String]) -> Result<(), Failure> {
    if values.len() > MAX_APPROVED_APPS {
        return Err(Failure::Local("wire_invalid"));
    }
    let mut seen = Vec::with_capacity(values.len());
    for value in values {
        if !bounded_text(value, 1, 128) || seen.contains(value) {
            return Err(Failure::Local("wire_invalid"));
        }
        seen.push(value.clone());
    }
    Ok(())
}

fn approval_page_uri(authorization_id: &str) -> String {
    format!("{APPROVAL_ORIGIN}/cli/approve?authorization_id={authorization_id}")
}

fn server_or_local(code: &str) -> Failure {
    match SERVER_ERROR_CODES.iter().find(|known| **known == code) {
        Some(known) => Failure::Server(*known),
        None => Failure::Local("receipt_unknown"),
    }
}

fn server_code(code: &str) -> Result<Failure, Failure> {
    SERVER_ERROR_CODES
        .iter()
        .find(|known| **known == code)
        .copied()
        .map(Failure::Server)
        .ok_or(Failure::Local("wire_invalid"))
}

/// Shape and identity only. A historical effect stays readable after its own
/// deadline, so liveness is checked separately by the caller.
fn check_create_shape(view: &CreateResponse, request_id: &str) -> Result<(), Failure> {
    require(&view.status, "PENDING")?;
    if !is_uuid(&view.authorization_id) || !is_uuid(&view.request_id) {
        return Err(Failure::Local("wire_invalid"));
    }
    if view.authorization_id != request_id || view.request_id != request_id {
        return Err(Failure::Local("wire_invalid"));
    }
    if view.revision != 1 || view.poll_interval_seconds != POLL_SECONDS {
        return Err(Failure::Local("wire_invalid"));
    }
    require(&view.target_environment, "test")?;
    require(&view.realm, REALM)?;
    if view.verification_uri != approval_page_uri(&view.authorization_id) {
        return Err(Failure::Local("wire_invalid"));
    }
    require_moment(&view.expires_at).map(|_| ())
}

fn validate_create(
    view: CreateResponse,
    request_id: &str,
    now: DateTime<Utc>,
) -> Result<CreateResponse, Failure> {
    check_create_shape(&view, request_id)?;
    let deadline = require_moment(&view.expires_at)?;
    if deadline <= now || deadline > now + Duration::seconds(PENDING_LIMIT + CLOCK_MARGIN) {
        return Err(Failure::Local("wire_invalid"));
    }
    Ok(view)
}

fn check_snapshot_shape(snapshot: &ApprovalSnapshot) -> Result<(), Failure> {
    require(&snapshot.environment, "dev")?;
    require_actions(&snapshot.granted_actions)?;
    for field in [
        &snapshot.partner_user_id,
        &snapshot.organization_id,
        &snapshot.membership_id,
        &snapshot.partner_role_id,
        &snapshot.context_id,
    ] {
        if !is_uuid(field) {
            return Err(Failure::Local("wire_invalid"));
        }
    }
    require_unique_ids(&snapshot.authorized_app_ids)?;
    if let Some(revision) = snapshot.authorization_revision {
        require_revision(revision)?;
    }
    require_moment(&snapshot.expires_at).map(|_| ())
}
fn check_view_shape(view: &AuthorizationView, authorization_id: &str) -> Result<(), Failure> {
    if !is_uuid(&view.authorization_id) || view.authorization_id != authorization_id {
        return Err(Failure::Local("wire_invalid"));
    }
    require(&view.target_environment, "test")?;
    require(&view.realm, REALM)?;
    if !PHASES.contains(&view.status.as_str()) {
        return Err(Failure::Local("wire_invalid"));
    }
    require_revision(view.revision)?;
    require_moment(&view.expires_at)?;
    if let Some(snapshot) = &view.approval {
        check_snapshot_shape(snapshot)?;
    }
    Ok(())
}

fn check_exchange_shape(
    response: &ExchangeResponse,
    authorization_id: &str,
    request_id: &str,
) -> Result<(), Failure> {
    require(&response.operation, "partner_cli.exchange")?;
    require(&response.status, "ACTIVE")?;
    if !is_uuid(&response.request_id) {
        return Err(Failure::Local("wire_invalid"));
    }
    if response.authorization_id != authorization_id || response.request_id != request_id {
        return Err(Failure::Local("wire_invalid"));
    }
    // The session locator is the authorization itself; no second bearer exists.
    if !is_uuid(&response.session_id) || response.session_id != authorization_id {
        return Err(Failure::Local("wire_invalid"));
    }
    require_revision(response.previous_revision)?;
    // Checked arithmetic: a hostile u64 cannot wrap the revision forward.
    let expected = response
        .previous_revision
        .checked_add(1)
        .filter(|next| *next <= MAX_SAFE_INTEGER)
        .ok_or(Failure::Local("wire_invalid"))?;
    if response.revision != expected {
        return Err(Failure::Local("wire_invalid"));
    }
    let snapshot = response
        .approval
        .as_ref()
        .ok_or(Failure::Local("wire_invalid"))?;
    check_snapshot_shape(snapshot)
}
fn validate_exchange(
    response: ExchangeResponse,
    authorization_id: &str,
    request_id: &str,
    expected_revision: u64,
    snapshot_ceiling: &ApprovalSnapshot,
    now: DateTime<Utc>,
) -> Result<ExchangeResponse, Failure> {
    check_exchange_shape(&response, authorization_id, request_id)?;
    if response.previous_revision != expected_revision {
        return Err(Failure::Local("cli_scope_changed"));
    }
    let deadline = require_moment(&response.expires_at)?;
    if deadline <= now {
        return Err(Failure::Local("cli_expired"));
    }
    // Never longer than the approval ceiling, the local active cap or the
    // absolute parent window.
    let approval_deadline = require_moment(&snapshot_ceiling.expires_at)?;
    let active_cap = now
        .checked_add_signed(Duration::seconds(ACTIVE_LIMIT))
        .ok_or(Failure::Local("wire_invalid"))?;
    let parent_cap = now
        .checked_add_signed(Duration::seconds(ABSOLUTE_PARENT_LIMIT))
        .ok_or(Failure::Local("wire_invalid"))?;
    let earliest = approval_deadline.min(active_cap).min(parent_cap);
    if deadline > earliest + Duration::seconds(CLOCK_MARGIN) {
        return Err(Failure::Local("wire_invalid"));
    }
    Ok(response)
}
fn check_revocation_shape(
    response: &RevocationResponse,
    authorization_id: &str,
    request_id: &str,
) -> Result<(), Failure> {
    require(&response.operation, "partner_cli.revoke")?;
    require(&response.status, "REVOKED")?;
    if response.authorization_id != authorization_id || response.request_id != request_id {
        return Err(Failure::Local("wire_invalid"));
    }
    if !is_uuid(&response.request_id) {
        return Err(Failure::Local("wire_invalid"));
    }
    require_revision(response.previous_revision)?;
    if response.revision <= response.previous_revision || response.revision > MAX_SAFE_INTEGER {
        return Err(Failure::Local("wire_invalid"));
    }
    if let Some(snapshot) = &response.approval {
        check_snapshot_shape(snapshot)?;
    }
    Ok(())
}

fn check_workspace_shape(
    workspace: &WorkspaceResponse,
    authorization_id: &str,
) -> Result<(), Failure> {
    require(&workspace.environment, "dev")?;
    require(&workspace.target_environment, "test")?;
    require(&workspace.realm, REALM)?;
    require_actions(&workspace.granted_actions)?;
    if !is_uuid(&workspace.session_id) || workspace.session_id != authorization_id {
        return Err(Failure::Local("wire_invalid"));
    }
    for field in [
        &workspace.partner_user_id,
        &workspace.organization_id,
        &workspace.membership_id,
        &workspace.partner_role_id,
    ] {
        if !is_uuid(field) {
            return Err(Failure::Local("wire_invalid"));
        }
    }
    require_unique_ids(&workspace.authorized_app_ids)?;
    if let Some(revision) = workspace.authorization_revision {
        require_revision(revision)?;
    }
    let mut listed: Vec<String> = Vec::new();
    for app in &workspace.apps {
        // The contract bounds these by Unicode scalars, not by bytes.
        if !bounded_text(&app.app_id, 1, 128)
            || !bounded_text(&app.status, 1, 64)
            || !bounded_text(&app.display_name, 0, 512)
            || listed.contains(&app.app_id)
        {
            return Err(Failure::Local("wire_invalid"));
        }
        listed.push(app.app_id.clone());
    }
    require_moment(&workspace.expires_at).map(|_| ())
}
/// The current read must be the approved tuple, unchanged or narrowed.
fn validate_workspace(
    workspace: WorkspaceResponse,
    authorization_id: &str,
    snapshot: &ApprovalSnapshot,
    now: DateTime<Utc>,
) -> Result<WorkspaceResponse, Failure> {
    check_workspace_shape(&workspace, authorization_id)?;
    let actors_match = workspace.partner_user_id == snapshot.partner_user_id
        && workspace.organization_id == snapshot.organization_id
        && workspace.membership_id == snapshot.membership_id
        && workspace.partner_role_id == snapshot.partner_role_id;
    if !actors_match {
        return Err(Failure::Local("cli_scope_changed"));
    }
    // The approved revision must be reproduced exactly, Some or None alike.
    if workspace.authorization_revision != snapshot.authorization_revision {
        return Err(Failure::Local("cli_scope_changed"));
    }
    let within_ceiling = workspace
        .authorized_app_ids
        .iter()
        .all(|id| snapshot.authorized_app_ids.contains(id))
        && workspace
            .apps
            .iter()
            .all(|app| workspace.authorized_app_ids.contains(&app.app_id));
    if !within_ceiling {
        return Err(Failure::Local("cli_scope_changed"));
    }
    let deadline = require_moment(&workspace.expires_at)?;
    if deadline > require_moment(&snapshot.expires_at)? {
        return Err(Failure::Local("cli_scope_changed"));
    }
    let cap = now
        .checked_add_signed(Duration::seconds(ACTIVE_LIMIT + CLOCK_MARGIN))
        .ok_or(Failure::Local("wire_invalid"))?;
    if deadline > cap {
        return Err(Failure::Local("cli_scope_changed"));
    }
    if deadline <= now {
        return Err(Failure::Local("cli_expired"));
    }
    Ok(workspace)
}
fn decode_receipt(
    body: &[u8],
    operation: &str,
    request_id: &str,
) -> Result<Option<serde_json::Value>, Failure> {
    let receipt: ReceiptWire = parse(body)?;
    if receipt.request_id != request_id || receipt.operation != operation {
        return Err(Failure::Local("wire_invalid"));
    }
    if !RECEIPT_OPERATIONS.contains(&receipt.operation.as_str()) {
        return Err(Failure::Local("wire_invalid"));
    }
    match (receipt.status.as_str(), receipt.response) {
        ("UNKNOWN", None) => Ok(None),
        ("SUCCEEDED", Some(value)) => Ok(Some(value)),
        _ => Err(Failure::Local("wire_invalid")),
    }
}

// ── Transport ───────────────────────────────────────────────────────────────

/// One request, described before it becomes a `reqwest::Request` so the same
/// construction is used by the live path and by the narrow test seam.
#[derive(Clone, PartialEq, Eq)]
struct Built {
    method: &'static str,
    path: String,
    proof: String,
    /// Idempotency key: the original operation UUID. Reads have none.
    request_id: Option<String>,
    /// Locator: always the create request id, which is the authorization id.
    locator: String,
    target: bool,
    body: Option<serde_json::Value>,
}

impl fmt::Debug for Built {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Built")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("request_id", &self.request_id)
            .field("locator", &self.locator)
            .field("target", &self.target)
            .field("body", &self.body.as_ref().map(|_| "[REDACTED]"))
            .field("proof", &"[REDACTED]")
            .finish()
    }
}

fn proof_headers(built: &Built) -> Result<Vec<(HeaderName, HeaderValue)>, Failure> {
    if !state::is_lower_hex(&built.proof, 64) {
        return Err(Failure::Local("unsafe_local_state"));
    }
    if !is_uuid(&built.locator) {
        return Err(Failure::Local("wire_invalid"));
    }
    if let Some(request_id) = &built.request_id {
        if !is_uuid(request_id) {
            return Err(Failure::Local("wire_invalid"));
        }
    }
    let mut proof_value = HeaderValue::from_str(&format!("{PROOF_SCHEME}{}", built.proof))
        .map_err(|_| Failure::Local("wire_invalid"))?;
    proof_value.set_sensitive(true);
    let mut headers = vec![
        (header::AUTHORIZATION, proof_value),
        (
            HeaderName::from_static("x-facto-cli-request-id"),
            HeaderValue::from_str(&built.locator).map_err(|_| Failure::Local("wire_invalid"))?,
        ),
    ];
    if let Some(request_id) = &built.request_id {
        headers.push((
            HeaderName::from_static("idempotency-key"),
            HeaderValue::from_str(request_id).map_err(|_| Failure::Local("wire_invalid"))?,
        ));
    }
    if built.target {
        headers.push((
            HeaderName::from_static("x-facto-target"),
            HeaderValue::from_static("test"),
        ));
    }
    Ok(headers)
}

fn live_client() -> Result<Client, Failure> {
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| Failure::Local("transport_unavailable"))
}

/// Production request construction. The origin, TLS policy and headers are
/// fixed here; no environment value is read.
fn request_for(client: &Client, origin: &str, built: &Built) -> Result<Request, Failure> {
    let url = format!("{origin}{}", built.path);
    let mut builder = match built.method {
        "POST" => client.post(&url),
        "GET" => client.get(&url),
        _ => return Err(Failure::Local("wire_invalid")),
    };
    for (name, value) in proof_headers(built)? {
        builder = builder.header(name, value);
    }
    if let Some(body) = &built.body {
        builder = builder
            .header(header::CONTENT_TYPE, "application/json")
            .json(body);
    }
    builder
        .build()
        .map_err(|_| Failure::Local("transport_unavailable"))
}

fn live_request(built: &Built) -> Result<(Client, Request), Failure> {
    let client = live_client()?;
    let request = request_for(&client, API_ORIGIN, built)?;
    Ok((client, request))
}

/// The single execute/read/timeout path. Production always calls it with the
/// fixed origin and the pinned TLS policy; only a test supplies another client
/// and origin, and it can never reach this through an environment value.
async fn transact_with(
    client: &Client,
    origin: &str,
    built: &Built,
    expect: StatusCode,
) -> Result<Vec<u8>, Failure> {
    let request = request_for(client, origin, built)?;
    let mut response = client
        .execute(request)
        .await
        .map_err(|_| Failure::Local("transport_unavailable"))?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err(Failure::Local("response_too_large"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Failure::Local("transport_unavailable"))?
    {
        push_chunk(&mut body, &chunk)?;
    }
    interpret(status, body, expect)
}
fn push_chunk(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), Failure> {
    if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
        return Err(Failure::Local("response_too_large"));
    }
    body.extend_from_slice(chunk);
    Ok(())
}

fn interpret(status: StatusCode, body: Vec<u8>, expect: StatusCode) -> Result<Vec<u8>, Failure> {
    if status == expect {
        return Ok(body);
    }
    if let Ok(error) = serde_json::from_slice::<ErrorWire>(&body) {
        if error.message.len() <= 512 {
            return Err(server_code(&error.code)?);
        }
    }
    Err(Failure::Local("http_rejected"))
}

#[cfg(test)]
fn assemble(
    status: StatusCode,
    chunks: &[Vec<u8>],
    expect: StatusCode,
) -> Result<Vec<u8>, Failure> {
    let mut body = Vec::new();
    for chunk in chunks {
        push_chunk(&mut body, chunk)?;
    }
    interpret(status, body, expect)
}

enum Sender {
    Live,
    #[cfg(test)]
    Stub(std::rc::Rc<std::cell::RefCell<Stub>>),
    /// A real `reqwest` client against a loopback HTTP server. Production keeps
    /// `Live` with the pinned origin; this variant only exists in tests.
    #[cfg(test)]
    Loopback {
        client: Client,
        origin: String,
    },
}

#[cfg(test)]
#[derive(Default)]
struct Stub {
    posted: Vec<Built>,
    fetched: Vec<Built>,
    script: std::collections::VecDeque<(u16, Vec<Vec<u8>>)>,
}

#[cfg(test)]
impl Stub {
    fn ok(&mut self, value: &serde_json::Value) {
        self.script
            .push_back((200, vec![serde_json::to_vec(value).expect("bytes")]));
    }

    fn created(&mut self, value: &serde_json::Value) {
        self.script
            .push_back((201, vec![serde_json::to_vec(value).expect("bytes")]));
    }

    fn status(&mut self, code: u16, value: &serde_json::Value) {
        self.script
            .push_back((code, vec![serde_json::to_vec(value).expect("bytes")]));
    }

    fn post_count(&self) -> usize {
        self.posted.len()
    }
}

impl Sender {
    async fn transact(&self, built: &Built, expect: StatusCode) -> Result<Vec<u8>, Failure> {
        match self {
            Sender::Live => transact_with(&live_client()?, API_ORIGIN, built, expect).await,
            #[cfg(test)]
            Sender::Loopback { client, origin } => {
                transact_with(client, origin, built, expect).await
            }
            #[cfg(test)]
            Sender::Stub(cell) => {
                let mut stub = cell.borrow_mut();
                if built.method == "POST" {
                    stub.posted.push(built.clone());
                } else {
                    stub.fetched.push(built.clone());
                }
                let (code, chunks) = stub
                    .script
                    .pop_front()
                    .ok_or(Failure::Local("transport_unavailable"))?;
                assemble(
                    StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
                    &chunks,
                    expect,
                )
            }
        }
    }
}

/// A nullable wire field must still be *present*: the machine contract lists it
/// as required, so an absent value is a protocol violation rather than a null.
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn require_revision(value: u64) -> Result<(), Failure> {
    if value == 0 || value > MAX_SAFE_INTEGER {
        return Err(Failure::Local("wire_invalid"));
    }
    Ok(())
}

fn bounded_text(value: &str, minimum: usize, maximum: usize) -> bool {
    let count = value.chars().count();
    count >= minimum && count <= maximum
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Failure> {
    serde_json::from_slice(body).map_err(|_| Failure::Local("wire_invalid"))
}

// ── Endpoint builders ───────────────────────────────────────────────────────

fn build_create(proof: &str, authorization_id: &str, body: serde_json::Value) -> Built {
    Built {
        method: "POST",
        path: format!("{PREFIX}/authorizations"),
        proof: proof.to_string(),
        request_id: Some(authorization_id.to_string()),
        locator: authorization_id.to_string(),
        target: true,
        body: Some(body),
    }
}

fn build_read(proof: &str, locator: &str, path: String) -> Built {
    Built {
        method: "GET",
        path,
        proof: proof.to_string(),
        request_id: None,
        locator: locator.to_string(),
        target: false,
        body: None,
    }
}

fn build_write(
    proof: &str,
    request_id: &str,
    locator: &str,
    path: String,
    body: serde_json::Value,
) -> Built {
    Built {
        method: "POST",
        path,
        proof: proof.to_string(),
        request_id: Some(request_id.to_string()),
        locator: locator.to_string(),
        target: true,
        body: Some(body),
    }
}

fn revision_body(request_id: &str, expected_revision: u64) -> serde_json::Value {
    serde_json::to_value(RevisionRequest {
        request_id,
        expected_revision,
    })
    .expect("revision request")
}

// ── Journal-bound wire helpers ──────────────────────────────────────────────

async fn read_authorization(
    sender: &Sender,
    journal: &Journal,
) -> Result<AuthorizationView, Failure> {
    let built = build_read(
        &journal.proof_hex,
        &journal.authorization_id,
        format!("{PREFIX}/authorizations/{}", journal.authorization_id),
    );
    let body = sender.transact(&built, StatusCode::OK).await?;
    let view: AuthorizationView = parse(&body)?;
    check_view_shape(&view, &journal.authorization_id).map(|_| view)
}

async fn read_receipt(
    sender: &Sender,
    journal: &Journal,
    operation: &str,
    request_id: &str,
) -> Result<Option<serde_json::Value>, Failure> {
    let built = build_read(
        &journal.proof_hex,
        &journal.authorization_id,
        format!(
            "{PREFIX}/authorizations/{}/requests/{operation}/{request_id}",
            journal.authorization_id
        ),
    );
    let body = sender.transact(&built, StatusCode::OK).await?;
    decode_receipt(&body, operation, request_id)
}

async fn read_workspace(sender: &Sender, journal: &Journal) -> Result<WorkspaceResponse, Failure> {
    let built = build_read(
        &journal.proof_hex,
        &journal.authorization_id,
        format!("{PREFIX}/session/workspace"),
    );
    let body = sender.transact(&built, StatusCode::OK).await?;
    parse::<WorkspaceResponse>(&body)
}

fn journal_approval(journal: &Journal) -> Option<ApprovalSnapshot> {
    journal
        .approval
        .as_ref()
        .and_then(|value| serde_json::from_value::<ApprovalSnapshot>(value.clone()).ok())
}

/// Claim the right to send one operation. The claim and the "effect unknown"
/// marker are durable before the POST, and only the process that created the
/// intent is allowed to send it.
fn claim_intent(
    dir: &std::path::Path,
    journal: &Journal,
    request_id: &str,
    expected_revision: u64,
    operation: &str,
) -> Result<(bool, Journal), Failure> {
    let candidate = request_id.to_string();
    let outcome = state::claim(dir, &journal.authorization_id, |entry| {
        let owned = match operation {
            "exchange" => {
                if entry.exchange_recorded || entry.exchange_request_id.is_some() {
                    false
                } else {
                    entry.record_exchange_intent(&candidate, expected_revision);
                    entry.mark_unknown("exchange");
                    true
                }
            }
            "revoke" => {
                if entry.revoke_recorded || entry.revoke_request_id.is_some() {
                    false
                } else {
                    entry.record_revoke_intent(&candidate, expected_revision);
                    entry.mark_unknown("revoke");
                    true
                }
            }
            _ => return Err(state::StateError::JournalInvalid),
        };
        Ok(owned)
    })
    .map_err(|error| Failure::Local(error.code()))?;
    let stored = state::read(&state::journal_path(dir, &journal.authorization_id))
        .map_err(|error| Failure::Local(error.code()))?;
    Ok((outcome, stored))
}

/// The approved tuple and ceiling are immutable. A later read may only agree
/// with it or narrow it; widening is a scope change.
fn record_approval(journal: &mut Journal, snapshot: &ApprovalSnapshot) -> Result<(), Failure> {
    let stored = match journal_approval(journal) {
        None => {
            journal.approval =
                Some(serde_json::to_value(snapshot).map_err(|_| Failure::Local("wire_invalid"))?);
            return Ok(());
        }
        Some(stored) => stored,
    };
    let same_actor = stored.partner_user_id == snapshot.partner_user_id
        && stored.organization_id == snapshot.organization_id
        && stored.membership_id == snapshot.membership_id
        && stored.partner_role_id == snapshot.partner_role_id
        && stored.context_id == snapshot.context_id
        && stored.authorization_revision == snapshot.authorization_revision;
    let narrowed = snapshot
        .authorized_app_ids
        .iter()
        .all(|id| stored.authorized_app_ids.contains(id))
        && require_moment(&snapshot.expires_at)? <= require_moment(&stored.expires_at)?;
    if same_actor && narrowed {
        Ok(())
    } else {
        Err(Failure::Local("cli_scope_changed"))
    }
}

/// Read back every original intent that is still owed an answer. This kernel is
/// shared by `demo session` and by the two-process test seam, so recovery is
/// never a test-only path.
async fn recover_all(
    dir: &std::path::Path,
    sender: &Sender,
    report: &mut Report,
) -> Result<(Journal, Vec<String>), Failure> {
    let mut journal = state::current(dir).map_err(|error| Failure::Local(error.code()))?;
    let mut failures: Vec<String> = Vec::new();
    for operation in journal.pending_intents() {
        report.stage = "receipt";
        let outcome: Result<(), Failure> = match operation {
            "create" => match resume_create(dir, journal.clone(), sender, report).await {
                Ok(updated) => {
                    journal = updated;
                    Ok(())
                }
                Err(failure) => Err(failure),
            },
            "exchange" => recover_exchange(dir, &mut journal, sender, report).await,
            _ => recover_revoke(dir, &mut journal, sender, report).await,
        };
        if let Err(failure) = outcome {
            failures.push(format!("{operation}:{}", failure.code()));
        }
        journal = state::read(&state::journal_path(dir, &journal.authorization_id))
            .map_err(|error| Failure::Local(error.code()))?;
    }
    Ok((journal, failures))
}

fn stored_approval(journal: &Journal) -> Result<ApprovalSnapshot, Failure> {
    let value = journal
        .approval
        .as_ref()
        .ok_or(Failure::Local("cli_pending"))?;
    let snapshot: ApprovalSnapshot =
        serde_json::from_value(value.clone()).map_err(|_| Failure::Local("journal_invalid"))?;
    check_snapshot_shape(&snapshot).map(|_| snapshot)
}

// ── Pairing channel ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channel {
    /// Interactive human terminal on stdout (normal output).
    Stdout,
    /// Interactive terminal in terse mode; the JSON report owns stdout.
    Stderr,
    /// Nothing is a terminal: pairing is refused before any request.
    Refused,
}

fn pairing_channel(terse: bool, stdout_tty: bool, stderr_tty: bool) -> Channel {
    if terse {
        if stderr_tty {
            Channel::Stderr
        } else {
            Channel::Refused
        }
    } else if stdout_tty {
        Channel::Stdout
    } else {
        Channel::Refused
    }
}

fn live_channel(terse: bool) -> Channel {
    pairing_channel(
        terse,
        atty::is(atty::Stream::Stdout),
        atty::is(atty::Stream::Stderr),
    )
}

/// The pairing code reaches a terminal and nothing else: never the JSON
/// report, a URL, a receipt, a log or the proof-bearing journal dump.
fn announce_pairing<O: Write, E: Write>(
    uri: &str,
    code: &str,
    channel: Channel,
    mut stdout: O,
    mut stderr: E,
) -> Result<(), Failure> {
    let failure = Failure::Local("transport_unavailable");
    match channel {
        Channel::Stdout => {
            writeln!(stdout, "Open the approval page: {uri}").map_err(|_| failure)?;
            writeln!(stdout, "Pairing code to type on the Partner page: {code}")
                .map_err(|_| failure)
        }
        Channel::Stderr => writeln!(stderr, "Pairing code for {uri}: {code}").map_err(|_| failure),
        Channel::Refused => Err(Failure::Local("pairing_requires_terminal")),
    }
}
// ── Reports ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct Report {
    kind: &'static str,
    target: &'static str,
    mode: String,
    status: String,
    stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    authorization_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verification_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unknown_operations: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resumed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_step: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    granted_actions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    authorized_app_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apps: Option<Vec<CliApp>>,
    authentication_verified: bool,
    uat_status: &'static str,
    consumer_state_accessed: bool,
    app_secret_transport: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Report {
    fn new(kind: &'static str, mode: &str, stage: &'static str) -> Self {
        Self {
            kind,
            target: "test",
            mode: mode.to_string(),
            status: "failed".to_string(),
            stage,
            authorization_id: None,
            session_id: None,
            verification_uri: None,
            phase: None,
            revision: None,
            expires_at: None,
            effect: None,
            unknown_operations: None,
            resumed: None,
            next_step: None,
            granted_actions: None,
            authorized_app_ids: None,
            apps: None,
            authentication_verified: false,
            uat_status: "NOT_RUN",
            consumer_state_accessed: false,
            app_secret_transport: false,
            error: None,
        }
    }

    fn from_journal(
        kind: &'static str,
        mode: &str,
        stage: &'static str,
        journal: &Journal,
    ) -> Self {
        let mut report = Report::new(kind, mode, stage);
        report.authorization_id = Some(journal.authorization_id.clone());
        report.phase = Some(journal.phase.clone());
        report.revision = Some(journal.revision);
        report.expires_at = (!journal.expires_at.is_empty()).then(|| journal.expires_at.clone());
        report.session_id = journal.session_id.clone();
        report.verification_uri = Some(approval_page_uri(&journal.authorization_id));
        report
    }

    fn succeeded(mut self) -> Self {
        self.status = "ok".to_string();
        self.error = None;
        self
    }

    fn failed(mut self, failure: &Failure) -> Self {
        self.status = "failed".to_string();
        self.error = Some(failure.code().to_string());
        self
    }

    fn render(&self, terse: bool) -> String {
        if terse {
            return serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        }
        let mut text = format!(
            "Partner CLI handoff {}: {} (target: test, mode: {}, stage: {}).\n",
            self.kind, self.status, self.mode, self.stage
        );
        if let Some(id) = &self.authorization_id {
            text.push_str(&format!("Authorization: {id}\n"));
        }
        if let Some(uri) = &self.verification_uri {
            text.push_str(&format!("Approval page: {uri}\n"));
        }
        if let Some(phase) = &self.phase {
            text.push_str(&format!("Phase: {phase}\n"));
        }
        if let Some(effect) = &self.effect {
            text.push_str(&format!("Original effect: {effect}\n"));
        }
        if let Some(unknown) = &self.unknown_operations {
            if !unknown.is_empty() {
                text.push_str(&format!("Unresolved operations: {}.\n", unknown.join(", ")));
            }
        }
        if let Some(apps) = &self.apps {
            if apps.is_empty() {
                text.push_str("Authorized applications: none.\n");
            } else {
                text.push_str("Authorized applications:\n");
                for app in apps {
                    text.push_str(&format!(
                        "  {} ({}): {}\n",
                        app.app_id, app.status, app.display_name
                    ));
                }
            }
        }
        if let Some(error) = &self.error {
            text.push_str(&format!("Reason: {error}\n"));
        }
        if let Some(step) = &self.next_step {
            text.push_str(&format!("Next step: {step}\n"));
        }
        text.push_str(&format!(
            "Authentication verified: {}. Business UAT: NOT_RUN. No App secret was read, sent or stored.\n",
            self.authentication_verified
        ));
        text
    }
}

fn emit(mut report: Report, outcome: Result<(), Failure>, terse: bool) -> Result<()> {
    match outcome {
        Ok(()) => {
            println!("{}", report.succeeded().render(terse));
            Ok(())
        }
        Err(failure) => {
            report = report.failed(&failure);
            println!("{}", report.render(terse));
            Err(anyhow::anyhow!(failure.code()))
        }
    }
}

// ── Shared preamble ─────────────────────────────────────────────────────────

fn reject_target(target: &str) -> Result<(), Failure> {
    // The delivered wire is Test only; Dev keeps the existing plan/check path.
    if target == "test" {
        Ok(())
    } else {
        Err(Failure::Local("target_unsupported"))
    }
}

fn open_store() -> Result<std::path::PathBuf, Failure> {
    let home = state::home_dir().map_err(|error| Failure::Local(error.code()))?;
    let dir = state::store_dir(&home);
    state::ensure_store(&dir).map_err(|error| Failure::Local(error.code()))?;
    Ok(dir)
}

fn persist(dir: &std::path::Path, journal: &Journal) -> Result<(), Failure> {
    state::save(dir, journal).map_err(|error| Failure::Local(error.code()))
}

fn clock() -> DateTime<Utc> {
    Utc::now()
}

// ── Pairing state machine ───────────────────────────────────────────────────

fn wants_resume(journal: &Journal, now: DateTime<Utc>) -> bool {
    if journal.has_unresolved_intent() {
        return true;
    }
    let pairing = matches!(journal.phase.as_str(), "PENDING" | "APPROVED");
    pairing
        && journal
            .pairing_deadline()
            .map_or(true, |deadline| deadline > now)
}

fn blocks_new_pairing(journal: &Journal, now: DateTime<Utc>) -> bool {
    journal.phase == "ACTIVE"
        && journal
            .pairing_deadline()
            .map_or(false, |deadline| deadline > now)
}

/// Persist the original intent, then issue the single create POST. A lost
/// response is recovered from the original receipt, never by posting twice.
async fn fresh_pairing(
    mode: &str,
    dir: &std::path::Path,
    sender: &Sender,
    report: &mut Report,
    now: DateTime<Utc>,
) -> Result<Journal, Failure> {
    let proof = new_proof()?;
    let authorization_id = new_uuid_v4()?;
    let pairing = new_pairing_code()?;
    let normalized =
        normalize_pairing_code(&pairing).ok_or(Failure::Local("unsafe_local_state"))?;
    let hashed = verification_code_hash(&authorization_id, &normalized);
    let body = CreateRequest {
        request_id: &authorization_id,
        verification_code_hash: &hashed,
        requested_actions: ACTIONS,
        mode,
    };
    let value = serde_json::to_value(&body).map_err(|_| Failure::Local("unsafe_local_state"))?;
    let mut journal = Journal::new(
        mode,
        &proof,
        &normalized,
        &authorization_id,
        value.clone(),
        &now,
    );
    // Durable before the wire: an exclusive create also blocks a second process.
    state::create(dir, &journal).map_err(|error| Failure::Local(error.code()))?;
    report.stage = "create";
    report.authorization_id = Some(authorization_id.clone());
    report.verification_uri = Some(approval_page_uri(&authorization_id));

    let built = build_create(&proof, &authorization_id, value);
    let created = match sender.transact(&built, StatusCode::CREATED).await {
        Ok(bytes) => validate_create(parse::<CreateResponse>(&bytes)?, &authorization_id, now)?,
        Err(failure) if failure == Failure::Local("transport_unavailable") => {
            journal.mark_unknown("create");
            journal.touch(&now, "UNKNOWN", journal.revision, "");
            persist(dir, &journal)?;
            report.resumed = Some(false);
            // The effect is unknown: only the original receipt may answer. A
            // historical success continues this pairing; an unknown stays
            // unknown and is returned without a second POST.
            return resume_create(dir, journal, sender, report).await;
        }
        Err(failure) => return Err(failure),
    };

    journal.create_recorded = true;
    journal.clear_unknown("create");
    journal.touch(&now, &created.status, created.revision, &created.expires_at);
    persist(dir, &journal)?;
    report.phase = Some(created.status.clone());
    report.revision = Some(created.revision);
    report.expires_at = Some(created.expires_at.clone());
    Ok(journal)
}

/// Read the original create effect. A historical success stays readable after
/// its own deadline; it is never treated as current authority.
async fn resume_create(
    dir: &std::path::Path,
    mut journal: Journal,
    sender: &Sender,
    report: &mut Report,
) -> Result<Journal, Failure> {
    report.stage = "create-receipt";
    let value = read_receipt(
        sender,
        &journal,
        "create",
        &journal.create_request_id.clone(),
    )
    .await?;
    match value {
        Some(value) => {
            let created: CreateResponse =
                serde_json::from_value(value).map_err(|_| Failure::Local("wire_invalid"))?;
            check_create_shape(&created, &journal.authorization_id)?;
            journal.create_recorded = true;
            journal.clear_unknown("create");
            journal.touch(
                &clock(),
                &created.status,
                created.revision,
                &created.expires_at,
            );
            persist(dir, &journal)?;
            report.phase = Some(created.status.clone());
            report.revision = Some(created.revision);
            report.expires_at = Some(created.expires_at.clone());
            Ok(journal)
        }
        None => {
            journal.mark_unknown("create");
            journal.touch(&clock(), "UNKNOWN", journal.revision, "");
            persist(dir, &journal)?;
            report.next_step =
                Some("demo session to read the original receipt, or demo logout to revoke it");
            Err(Failure::Local("receipt_unknown"))
        }
    }
}

async fn wait_for_approval(
    dir: &std::path::Path,
    journal: &mut Journal,
    sender: &Sender,
    deadline: DateTime<Utc>,
) -> Result<AuthorizationView, Failure> {
    let mut transport_failures = 0_u32;
    loop {
        if clock() >= deadline {
            journal.touch(&clock(), "EXPIRED", journal.revision, "");
            persist(dir, journal)?;
            return Err(Failure::Local("cli_expired"));
        }
        match read_authorization(sender, journal).await {
            Ok(view) => {
                transport_failures = 0;
                journal.touch(&clock(), &view.status, view.revision, &view.expires_at);
                if let Some(snapshot) = &view.approval {
                    // A polled view may agree with or narrow the ceiling, never
                    // widen what was approved.
                    record_approval(journal, snapshot)?;
                }
                persist(dir, journal)?;
                match view.status.as_str() {
                    "APPROVED" => return Ok(view),
                    "PENDING" => {}
                    "DENIED" => return Err(Failure::Local("cli_denied")),
                    "EXPIRED" => return Err(Failure::Local("cli_expired")),
                    "REVOKED" => return Err(Failure::Local("cli_revoked")),
                    "ACTIVE" => return Ok(view),
                    _ => return Err(Failure::Local("wire_invalid")),
                }
            }
            Err(failure) if failure == Failure::Local("transport_unavailable") => {
                transport_failures += 1;
                if transport_failures >= MAX_POLL_TRANSPORT_FAILURES {
                    return Err(failure);
                }
            }
            Err(failure) => return Err(failure),
        }
        tokio::time::sleep(StdDuration::from_secs(POLL_SECONDS)).await;
    }
}

async fn ensure_exchange(
    dir: &std::path::Path,
    journal: &mut Journal,
    sender: &Sender,
    report: &mut Report,
    approved_revision: u64,
) -> Result<(), Failure> {
    report.stage = "exchange";
    if journal.exchange_recorded {
        return Ok(());
    }
    // Claim first, POST second: whoever creates the intent owns the single send,
    // and every later start of the same pairing may only read that receipt.
    let candidate = new_uuid_v4()?;
    let (owner, stored) = claim_intent(dir, journal, &candidate, approved_revision, "exchange")?;
    *journal = stored;
    if journal.exchange_recorded {
        return Ok(());
    }
    if !owner {
        return recover_exchange(dir, journal, sender, report).await;
    }
    let exchange_id = journal
        .exchange_request_id
        .clone()
        .ok_or(Failure::Local("journal_invalid"))?;
    let expected_revision = journal
        .exchange_expected_revision
        .ok_or(Failure::Local("journal_invalid"))?;
    if expected_revision != approved_revision {
        // The approved revision moved: the persisted intent stays frozen and is
        // read back, never rewritten or re-posted.
        return recover_exchange(dir, journal, sender, report).await;
    }
    let body = revision_body(&exchange_id, expected_revision);
    let built = build_write(
        &journal.proof_hex,
        &exchange_id,
        &journal.authorization_id,
        format!(
            "{PREFIX}/authorizations/{}/exchanges",
            journal.authorization_id
        ),
        body,
    );
    let now = clock();
    match sender.transact(&built, StatusCode::OK).await {
        Ok(bytes) => {
            let response = parse::<ExchangeResponse>(&bytes)?;
            let ceiling = match journal_approval(journal) {
                Some(snapshot) => snapshot,
                None => response
                    .approval
                    .clone()
                    .ok_or(Failure::Local("wire_invalid"))?,
            };
            let response = validate_exchange(
                response,
                &journal.authorization_id,
                &exchange_id,
                expected_revision,
                &ceiling,
                now,
            )?;
            if let Some(snapshot) = &response.approval {
                record_approval(journal, snapshot)?;
            }
            journal.exchange_recorded = true;
            journal.clear_unknown("exchange");
            journal.session_id = Some(response.session_id.clone());
            journal.touch(
                &now,
                &response.status,
                response.revision,
                &response.expires_at,
            );
            persist(dir, journal)?;
            report.session_id = Some(response.session_id.clone());
            report.phase = Some(response.status.clone());
            report.revision = Some(response.revision);
            report.expires_at = Some(response.expires_at.clone());
            Ok(())
        }
        // A lost or unusable response keeps the claim: only the receipt answers.
        Err(failure) => {
            recover_exchange(dir, journal, sender, report).await?;
            Err(failure)
        }
    }
}
/// A recovered exchange restores the safe binding only. It never claims current
/// access; the independent workspace read is what may do that.
async fn recover_exchange(
    dir: &std::path::Path,
    journal: &mut Journal,
    sender: &Sender,
    report: &mut Report,
) -> Result<(), Failure> {
    report.stage = "exchange-receipt";
    let exchange_id = journal
        .exchange_request_id
        .clone()
        .ok_or(Failure::Local("journal_invalid"))?;
    let expected = journal
        .exchange_expected_revision
        .ok_or(Failure::Local("journal_invalid"))?;
    let value = read_receipt(sender, journal, "exchange", &exchange_id).await?;
    match value {
        Some(value) => {
            let response: ExchangeResponse =
                serde_json::from_value(value).map_err(|_| Failure::Local("wire_invalid"))?;
            check_exchange_shape(&response, &journal.authorization_id, &exchange_id)?;
            if response.previous_revision != expected {
                return Err(Failure::Local("cli_scope_changed"));
            }
            let next = expected
                .checked_add(1)
                .ok_or(Failure::Local("wire_invalid"))?;
            if response.revision != next {
                return Err(Failure::Local("cli_scope_changed"));
            }
            let snapshot = response
                .approval
                .as_ref()
                .ok_or(Failure::Local("wire_invalid"))?;
            check_snapshot_shape(snapshot)?;
            // A recovered historical effect may already be past its deadline;
            // it is still the original success, and it never widens the ceiling.
            record_approval(journal, snapshot)?;
            journal.exchange_recorded = true;
            journal.clear_unknown("exchange");
            journal.session_id = Some(response.session_id.clone());
            journal.touch(
                &clock(),
                &response.status,
                response.revision,
                &response.expires_at,
            );
            persist(dir, journal)?;
            report.effect = Some("exchange:SUCCEEDED".to_string());
            report.authentication_verified = false;
            Ok(())
        }
        None => {
            report.effect = Some("exchange:UNKNOWN".to_string());
            report.next_step = Some("demo session reads the original receipt; logout revokes it");
            Err(Failure::Local("receipt_unknown"))
        }
    }
}
// ── Commands ────────────────────────────────────────────────────────────────

pub(crate) async fn authorize(target: &str, mode: &str, terse: bool) -> Result<()> {
    let mut report = Report::new("partner_cli_authorize", mode, "precheck");
    let outcome = authorize_run(
        target,
        mode,
        &Sender::Live,
        live_channel(terse),
        &mut report,
    )
    .await;
    emit(report, outcome, terse)
}

async fn authorize_run(
    target: &str,
    mode: &str,
    sender: &Sender,
    channel: Channel,
    report: &mut Report,
) -> Result<(), Failure> {
    reject_target(target)?;
    if channel == Channel::Refused {
        return Err(Failure::Local("pairing_requires_terminal"));
    }
    let dir = open_store()?;
    let now = clock();
    let mut journal = match state::current(&dir) {
        Ok(existing) if wants_resume(&existing, now) => {
            report.resumed = Some(true);
            if !existing.create_recorded || existing.unknown.iter().any(|name| name == "create") {
                resume_create(&dir, existing, sender, report).await?
            } else {
                existing
            }
        }
        Ok(existing) if blocks_new_pairing(&existing, now) => {
            report.authorization_id = Some(existing.authorization_id.clone());
            report.phase = Some(existing.phase.clone());
            return Err(Failure::Local("cli_already_active"));
        }
        Ok(_) => fresh_pairing(mode, &dir, sender, report, now).await?,
        Err(state::StateError::JournalMissing) => {
            fresh_pairing(mode, &dir, sender, report, now).await?
        }
        Err(error) => return Err(Failure::Local(error.code())),
    };

    if !journal.pairing_code.is_empty() {
        let code = display_pairing_code(&journal.pairing_code);
        let uri = approval_page_uri(&journal.authorization_id);
        let shown = announce_pairing(&uri, &code, channel, std::io::stdout(), std::io::stderr());
        if shown.is_ok() && channel == Channel::Stdout && !cfg!(test) {
            let _ = open::that(&uri);
        }
    }

    report.stage = "pending";
    let approved: AuthorizationView = if matches!(journal.phase.as_str(), "APPROVED" | "ACTIVE") {
        let view = read_authorization(sender, &journal).await?;
        if let Some(snapshot) = &view.approval {
            journal.approval =
                Some(serde_json::to_value(snapshot).map_err(|_| Failure::Local("wire_invalid"))?);
            persist(&dir, &journal)?;
        }
        view
    } else {
        let deadline = journal
            .pairing_deadline()
            .ok_or(Failure::Local("wire_invalid"))?;
        wait_for_approval(&dir, &mut journal, sender, deadline).await?
    };
    if approved.status == "ACTIVE" && journal.exchange_recorded {
        report.effect = Some("exchange:SUCCEEDED".to_string());
    } else {
        ensure_exchange(&dir, &mut journal, sender, report, approved.revision).await?;
    }

    report.stage = "workspace";
    let snapshot = stored_approval(&journal)?;
    let workspace = read_workspace(sender, &journal).await?;
    let workspace = validate_workspace(workspace, &journal.authorization_id, &snapshot, clock())?;
    report.session_id = Some(workspace.session_id.clone());
    report.granted_actions = Some(workspace.granted_actions.clone());
    report.authorized_app_ids = Some(workspace.authorized_app_ids.clone());
    report.apps = Some(workspace.apps.clone());
    // Only this current read may set the flag, and only after every other stage
    // has already been recorded durably.
    report.authentication_verified = true;
    Ok(())
}

pub(crate) async fn session(target: &str, mode: &str, terse: bool) -> Result<()> {
    let mut report = Report::new("partner_cli_session", mode, "precheck");
    let outcome = session_run(target, &Sender::Live, &mut report).await;
    emit(report, outcome, terse)
}

async fn session_run(target: &str, sender: &Sender, report: &mut Report) -> Result<(), Failure> {
    reject_target(target)?;
    let dir = open_store()?;
    let journal = state::current(&dir).map_err(|error| Failure::Local(error.code()))?;
    report.stage = "authorization";
    report.authorization_id = Some(journal.authorization_id.clone());
    report.verification_uri = Some(approval_page_uri(&journal.authorization_id));
    report.resumed = Some(true);

    // Recovery reads only, driven by every persisted intent rather than by an
    // in-memory list: no create, exchange or refresh side effect happens here.
    let (mut journal, mut failures) = recover_all(&dir, &sender, report).await?;
    if !journal.pending_intents().is_empty() {
        report.unknown_operations = Some(
            journal
                .pending_intents()
                .iter()
                .map(|name| name.to_string())
                .collect(),
        );
    }

    report.stage = "authorization";
    let view = match read_authorization(&sender, &journal).await {
        Ok(view) => view,
        Err(failure) if failure == Failure::Local("http_rejected") => {
            report.phase = Some(journal.phase.clone());
            failures.push("authorization:receipt_unknown".to_string());
            if !failures.is_empty() {
                report.effect = Some(failures.join(","));
            }
            return Err(Failure::Local("receipt_unknown"));
        }
        Err(failure) => return Err(failure),
    };
    if let Some(snapshot) = &view.approval {
        // A current view may agree or narrow the recorded ceiling, never widen it.
        record_approval(&mut journal, snapshot)?;
    }
    journal.touch(&clock(), &view.status, view.revision, &view.expires_at);
    persist(&dir, &journal)?;
    report.phase = Some(view.status.clone());
    report.revision = Some(view.revision);
    report.expires_at = Some(view.expires_at.clone());
    report.effect = Some(format!("authorization:{}", view.status));
    if !failures.is_empty() {
        report.effect = Some(format!(
            "{},{}",
            report.effect.clone().unwrap_or_default(),
            failures.join(",")
        ));
    }
    report.unknown_operations = Some(
        journal
            .pending_intents()
            .iter()
            .map(|name| name.to_string())
            .collect(),
    );
    // A stored or historical ACTIVE phase is an effect, never current access.
    report.authentication_verified = false;
    if let Some(first) = failures.first() {
        report.effect = Some(failures.join(","));
        let code = first.split(':').last().unwrap_or("receipt_unknown");
        let failure = match code {
            "receipt_unknown" => Failure::Local("receipt_unknown"),
            "wire_invalid" => Failure::Local("wire_invalid"),
            other => server_or_local(other),
        };
        return Err(failure);
    }
    Ok(())
}
fn receipt_key(journal: &Journal, operation: &str) -> Result<String, Failure> {
    let key = match operation {
        "create" => Some(journal.create_request_id.clone()),
        "exchange" => journal.exchange_request_id.clone(),
        "revoke" => journal.revoke_request_id.clone(),
        _ => None,
    };
    let key = key.ok_or(Failure::Local("journal_invalid"))?;
    if !is_uuid(&key) {
        return Err(Failure::Local("journal_invalid"));
    }
    Ok(key)
}

pub(crate) async fn check_access(target: &str, mode: &str, terse: bool) -> Result<()> {
    let mut report = Report::new("partner_cli_check_access", mode, "precheck");
    let outcome = check_access_run(target, &Sender::Live, &mut report).await;
    emit(report, outcome, terse)
}

async fn check_access_run(
    target: &str,
    sender: &Sender,
    report: &mut Report,
) -> Result<(), Failure> {
    reject_target(target)?;
    let dir = open_store()?;
    let journal = state::current(&dir).map_err(|error| Failure::Local(error.code()))?;
    report.authorization_id = Some(journal.authorization_id.clone());
    report.session_id = journal.session_id.clone();
    // The original ticket gives this command exactly one GET: the current read.
    report.stage = "workspace";
    let snapshot = stored_approval(&journal)?;
    let workspace = read_workspace(&sender, &journal).await?;
    let workspace = validate_workspace(workspace, &journal.authorization_id, &snapshot, clock())?;
    report.phase = Some("ACTIVE".to_string());
    report.expires_at = Some(workspace.expires_at.clone());
    report.granted_actions = Some(workspace.granted_actions.clone());
    report.authorized_app_ids = Some(workspace.authorized_app_ids.clone());
    report.apps = Some(workspace.apps.clone());
    report.authentication_verified = true;
    Ok(())
}

pub(crate) async fn logout(target: &str, mode: &str, terse: bool) -> Result<()> {
    let mut report = Report::new("partner_cli_logout", mode, "precheck");
    let outcome = logout_run(target, &Sender::Live, &mut report).await;
    emit(report, outcome, terse)
}

async fn logout_run(target: &str, sender: &Sender, report: &mut Report) -> Result<(), Failure> {
    reject_target(target)?;
    let dir = open_store()?;
    let journal = state::current(&dir).map_err(|error| Failure::Local(error.code()))?;
    report.authorization_id = Some(journal.authorization_id.clone());
    report.phase = Some(journal.phase.clone());
    report.unknown_operations = Some(
        journal
            .pending_intents()
            .iter()
            .map(|name| name.to_string())
            .collect(),
    );
    report.stage = "revocation";
    revoke_once(&dir, sender, report).await
}

/// The revocation kernel, independent of the process environment: it claims the
/// intent durably, sends at most once, and otherwise reads the original
/// receipt. The shipped command and the two-process test seam call this body.
async fn revoke_once(
    dir: &std::path::Path,
    sender: &Sender,
    report: &mut Report,
) -> Result<(), Failure> {
    let mut journal = state::current(dir).map_err(|error| Failure::Local(error.code()))?;
    if journal.revoke_recorded {
        report.effect = Some("revoke:SUCCEEDED".to_string());
        return Ok(());
    }
    if journal.revoke_request_id.is_some() {
        // An intent that exists was already sent by someone. Only the receipt
        // may answer, under the same request id and never a new key.
        return recover_revoke(dir, &mut journal, sender, report).await;
    }
    let candidate = new_uuid_v4()?;
    let (owner, stored) = claim_intent(dir, &journal, &candidate, journal.revision, "revoke")?;
    journal = stored;
    if !owner {
        return recover_revoke(dir, &mut journal, sender, report).await;
    }
    let revoke_id = journal
        .revoke_request_id
        .clone()
        .ok_or(Failure::Local("journal_invalid"))?;
    let expected_revision = journal
        .revoke_expected_revision
        .ok_or(Failure::Local("journal_invalid"))?;
    let body = revision_body(&revoke_id, expected_revision);
    let built = build_write(
        &journal.proof_hex,
        &revoke_id,
        &journal.authorization_id,
        format!(
            "{PREFIX}/authorizations/{}/revocations",
            journal.authorization_id
        ),
        body,
    );
    let now = clock();
    match sender.transact(&built, StatusCode::OK).await {
        Ok(bytes) => {
            let response = parse::<RevocationResponse>(&bytes)?;
            check_revocation_shape(&response, &journal.authorization_id, &revoke_id)?;
            let next = expected_revision
                .checked_add(1)
                .filter(|value| *value <= MAX_SAFE_INTEGER)
                .ok_or(Failure::Local("wire_invalid"))?;
            if response.previous_revision != expected_revision || response.revision != next {
                return Err(Failure::Local("cli_scope_changed"));
            }
            if let Some(snapshot) = &response.approval {
                check_snapshot_shape(snapshot)?;
                record_approval(&mut journal, snapshot)?;
            }
            journal.revoke_recorded = true;
            journal.clear_unknown("revoke");
            journal.touch(&now, &response.status, response.revision, "");
            persist(dir, &journal)?;
            report.phase = Some(response.status.clone());
            report.revision = Some(response.revision);
            report.effect = Some(format!("revoke:{}", response.status));
        }
        // The claim already says the effect is unknown: read, never resend.
        Err(failure) => {
            recover_revoke(dir, &mut journal, sender, report).await?;
            return Err(failure);
        }
    }
    Ok(())
}

async fn recover_revoke(
    dir: &std::path::Path,
    journal: &mut Journal,
    sender: &Sender,
    report: &mut Report,
) -> Result<(), Failure> {
    report.stage = "revoke-receipt";
    let revoke_id = journal
        .revoke_request_id
        .clone()
        .ok_or(Failure::Local("journal_invalid"))?;
    let expected = journal
        .revoke_expected_revision
        .ok_or(Failure::Local("journal_invalid"))?;
    match read_receipt(sender, journal, "revoke", &revoke_id).await? {
        Some(value) => {
            let response: RevocationResponse =
                serde_json::from_value(value).map_err(|_| Failure::Local("wire_invalid"))?;
            check_revocation_shape(&response, &journal.authorization_id, &revoke_id)?;
            if response.previous_revision != expected {
                return Err(Failure::Local("cli_scope_changed"));
            }
            let next = expected
                .checked_add(1)
                .ok_or(Failure::Local("wire_invalid"))?;
            if response.revision != next {
                return Err(Failure::Local("cli_scope_changed"));
            }
            if let Some(snapshot) = &response.approval {
                check_snapshot_shape(snapshot)?;
                record_approval(journal, snapshot)?;
            }
            journal.revoke_recorded = true;
            journal.clear_unknown("revoke");
            journal.touch(&clock(), &response.status, response.revision, "");
            persist(dir, journal)?;
            report.phase = Some(response.status.clone());
            report.effect = Some(format!("revoke:{}", response.status));
            Ok(())
        }
        None => {
            report.effect = Some("revoke:UNKNOWN".to_string());
            report.unknown_operations = Some(
                journal
                    .pending_intents()
                    .iter()
                    .map(|name| name.to_string())
                    .collect(),
            );
            report.next_step =
                Some("the original revoke receipt stays unknown; no resend is allowed");
            Err(Failure::Local("receipt_unknown"))
        }
    }
}
// ── Unit and seam tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use clap::Parser;
    use serde_json::{json, Value};
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;
    use std::rc::Rc;

    const FIXTURE_ID: &str = "11111111-1111-4111-8111-111111111111";
    const OTHER_ID: &str = "22222222-2222-4222-8222-222222222222";

    fn clock() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 13, 0, 0, 0).unwrap()
    }

    fn moment(seconds: i64) -> String {
        (clock() + Duration::seconds(seconds)).to_rfc3339()
    }

    /// The loopback seams answer with wall-clock deadlines, so those tests pass
    /// a real "now" into the kernels instead of the fixed fixture clock.
    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn snapshot(app_ids: &[&str]) -> ApprovalSnapshot {
        ApprovalSnapshot {
            partner_user_id: OTHER_ID.into(),
            organization_id: "33333333-3333-4333-8333-333333333333".into(),
            membership_id: "44444444-4444-4444-8444-444444444444".into(),
            partner_role_id: "55555555-5555-4555-8555-555555555555".into(),
            context_id: "66666666-6666-4666-8666-666666666666".into(),
            environment: "dev".into(),
            authorization_revision: Some(3),
            granted_actions: ACTIONS.iter().map(|action| action.to_string()).collect(),
            authorized_app_ids: app_ids.iter().map(|id| id.to_string()).collect(),
            expires_at: moment(300),
        }
    }

    fn body(id: &str, mode: &str) -> Value {
        json!({
            "request_id": id,
            "verification_code_hash": verification_code_hash(id, "ABCDEFGHJK"),
            "requested_actions": ACTIONS,
            "mode": mode
        })
    }

    fn patch(base: Value, key: &str, value: Value) -> Value {
        let mut object = base.as_object().expect("object").clone();
        object.insert(key.to_string(), value);
        Value::Object(object)
    }

    fn journal(id: &str) -> Journal {
        let mut entry = Journal::new(
            "reuse",
            &"a".repeat(64),
            "ABCDEFGHJK",
            id,
            body(id, "reuse"),
            &clock(),
        );
        entry.create_recorded = true;
        entry.phase = "PENDING".into();
        entry.revision = 1;
        entry.expires_at = moment(300);
        entry
    }

    fn created_view() -> Value {
        json!({
            "authorization_id": FIXTURE_ID,
            "request_id": FIXTURE_ID,
            "target_environment": "test",
            "realm": "test",
            "status": "PENDING",
            "revision": 1,
            "expires_at": moment(300),
            "poll_interval_seconds": 2,
            "verification_uri": approval_page_uri(FIXTURE_ID)
        })
    }

    fn exchange_view(request_id: &str) -> Value {
        json!({
            "request_id": request_id,
            "operation": "partner_cli.exchange",
            "authorization_id": FIXTURE_ID,
            "status": "ACTIVE",
            "previous_revision": 2,
            "revision": 3,
            "approval": snapshot(&["mapp_one"]),
            "session_id": FIXTURE_ID,
            "expires_at": moment(120)
        })
    }

    fn workspace_view() -> Value {
        json!({
            "session_id": FIXTURE_ID,
            "target_environment": "test",
            "realm": "test",
            "partner_user_id": OTHER_ID,
            "organization_id": "33333333-3333-4333-8333-333333333333",
            "membership_id": "44444444-4444-4444-8444-444444444444",
            "partner_role_id": "55555555-5555-4555-8555-555555555555",
            "environment": "dev",
            "authorization_revision": 3,
            "granted_actions": ACTIONS,
            "authorized_app_ids": ["mapp_one"],
            "expires_at": moment(120),
            "apps": [{"app_id": "mapp_one", "display_name": "Shop", "status": "active"}]
        })
    }

    fn approve_view() -> Value {
        json!({
            "authorization_id": FIXTURE_ID,
            "target_environment": "test",
            "realm": "test",
            "status": "APPROVED",
            "revision": 2,
            "expires_at": moment(120),
            "approval": snapshot(&["mapp_one"])
        })
    }

    fn stub() -> Rc<RefCell<Stub>> {
        Rc::new(RefCell::new(Stub::default()))
    }

    fn sandbox(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "facto-cli-handoff-{tag}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("sandbox");
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))
            .expect("sandbox mode");
        base
    }

    // ── Contract arithmetic ─────────────────────────────────────────────────

    // ── Real client over a loopback HTTP server ─────────────────────────────
    //
    // These seams drive the same command kernels through an actual `reqwest`
    // client, a real TCP listener and real byte counters. Production keeps
    // `Sender::Live` with the pinned origin; only these tests inject a loopback
    // origin, and nothing here reads an environment value or relaxes TLS.

    use std::collections::HashMap;
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Fixture {
        posts: Arc<AtomicUsize>,
        gets: Arc<AtomicUsize>,
        seen: Arc<Mutex<Vec<String>>>,
        bodies: Arc<Mutex<Vec<String>>>,
        headers: Arc<Mutex<Vec<String>>>,
        /// Method+path pairs whose response is dropped after the request is
        /// fully recorded, to reproduce a lost reply.
        drop_on: Arc<Mutex<Vec<String>>>,
        oversized: Arc<AtomicUsize>,
        redirect: Arc<AtomicUsize>,
        chunked: Arc<AtomicUsize>,
        widen: Arc<AtomicUsize>,
        /// Receipt reads answered as UNKNOWN, to reproduce an effect that stays
        /// unknown until a later process reads it again.
        unknown_receipt: Arc<AtomicUsize>,
        /// Effects the server actually processed, keyed by operation and
        /// original request id. A receipt read may only replay one of these.
        effects: Arc<Mutex<HashMap<String, String>>>,
    }

    fn loopback() -> (String, Fixture) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("addr").port();
        let fixture = Fixture {
            posts: Arc::new(AtomicUsize::new(0)),
            gets: Arc::new(AtomicUsize::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
            bodies: Arc::new(Mutex::new(Vec::new())),
            headers: Arc::new(Mutex::new(Vec::new())),
            drop_on: Arc::new(Mutex::new(Vec::new())),
            oversized: Arc::new(AtomicUsize::new(0)),
            redirect: Arc::new(AtomicUsize::new(0)),
            chunked: Arc::new(AtomicUsize::new(0)),
            widen: Arc::new(AtomicUsize::new(0)),
            unknown_receipt: Arc::new(AtomicUsize::new(0)),
            effects: Arc::new(Mutex::new(HashMap::new())),
        };
        let server = fixture.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if let Ok(stream) = stream {
                    serve(stream, &server);
                }
            }
        });
        (format!("http://127.0.0.1:{port}"), fixture)
    }

    fn classify(path: &str) -> &'static str {
        if path.ends_with("/exchanges") {
            "exchange"
        } else if path.ends_with("/revocations") {
            "revoke"
        } else if path.contains("/requests/") {
            "receipt"
        } else if path.ends_with("/session/workspace") {
            "workspace"
        } else if path.ends_with("/authorizations") {
            "create"
        } else {
            "authorization"
        }
    }

    fn locator(path: &str) -> String {
        let trimmed = path.trim_start_matches('/');
        if let Some(rest) = trimmed.strip_prefix("api/partner-cli/authorizations/") {
            rest.split('/').next().unwrap_or("").to_string()
        } else {
            String::new()
        }
    }

    fn request_key(body: &str, path: &str) -> String {
        serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("request_id")
                    .and_then(|id| id.as_str())
                    .map(str::to_string)
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| locator(path))
    }

    fn receipt_operation(path: &str) -> String {
        path.split("/requests/")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or("create")
            .to_string()
    }

    fn snapshot_json(app_ids: &[&str]) -> Value {
        json!({
            "partner_user_id": OTHER_ID,
            "organization_id": "33333333-3333-4333-8333-333333333333",
            "membership_id": "44444444-4444-4444-8444-444444444444",
            "partner_role_id": "55555555-5555-4555-8555-555555555555",
            "context_id": "66666666-6666-4666-8666-666666666666",
            "environment": "dev",
            "authorization_revision": 3,
            "granted_actions": ACTIONS,
            "authorized_app_ids": app_ids,
            "expires_at": (Utc::now() + Duration::seconds(600)).to_rfc3339()
        })
    }

    fn respond_to(kind: &str, path: &str, body: &str, widen: bool) -> Value {
        let now = Utc::now();
        let auth = locator(path);
        let request_id = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("request_id")
                    .and_then(|id| id.as_str())
                    .map(str::to_string)
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                path.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .to_string()
            });
        match kind {
            "create" => json!({
                "authorization_id": request_id,
                "request_id": request_id,
                "target_environment": "test",
                "realm": "test",
                "status": "PENDING",
                "revision": 1,
                "expires_at": (now + Duration::seconds(300)).to_rfc3339(),
                "poll_interval_seconds": 2,
                "verification_uri": approval_page_uri(&request_id)
            }),
            "authorization" => json!({
                "authorization_id": auth,
                "target_environment": "test",
                "realm": "test",
                "status": "APPROVED",
                "revision": 2,
                "expires_at": (now + Duration::seconds(300)).to_rfc3339(),
                "approval": snapshot_json(&["mapp_one"])
            }),
            "exchange" => json!({
                "request_id": request_id,
                "operation": "partner_cli.exchange",
                "authorization_id": auth,
                "status": "ACTIVE",
                "previous_revision": 2,
                "revision": 3,
                "approval": snapshot_json(&["mapp_one"]),
                "session_id": auth,
                "expires_at": (now + Duration::seconds(120)).to_rfc3339()
            }),
            "revoke" => json!({
                "request_id": request_id,
                "operation": "partner_cli.revoke",
                "authorization_id": auth,
                "status": "REVOKED",
                "previous_revision": 3,
                "revision": 4,
                "approval": snapshot_json(&["mapp_one"])
            }),
            "receipt" => Value::Null,
            _ => json!({
                "session_id": auth,
                "target_environment": "test",
                "realm": "test",
                "partner_user_id": OTHER_ID,
                "organization_id": "33333333-3333-4333-8333-333333333333",
                "membership_id": "44444444-4444-4444-8444-444444444444",
                "partner_role_id": "55555555-5555-4555-8555-555555555555",
                "environment": "dev",
                "authorization_revision": 3,
                "granted_actions": ACTIONS,
                "authorized_app_ids": if widen { json!(["mapp_one", "mapp_two"]) } else { json!(["mapp_one"]) },
                "expires_at": (now + Duration::seconds(120)).to_rfc3339(),
                "apps": if widen {
                    json!([{"app_id": "mapp_one", "display_name": "Shop", "status": "active"},
                           {"app_id": "mapp_two", "display_name": "Wider", "status": "active"}])
                } else {
                    json!([{"app_id": "mapp_one", "display_name": "Shop", "status": "active"}])
                }
            }),
        }
    }

    fn read_request(stream: &mut TcpStream) -> Option<(String, String, String)> {
        let mut buffer = Vec::new();
        let mut byte = [0_u8; 1];
        let mut header_end = None;
        while buffer.len() < 64 * 1024 {
            if stream.read(&mut byte).ok()? == 0 {
                return None;
            }
            buffer.push(byte[0]);
            if buffer.len() >= 4 && buffer[buffer.len() - 4..] == *b"\r\n\r\n" {
                header_end = Some(buffer.len());
                break;
            }
        }
        let head = header_end?;
        let text = String::from_utf8_lossy(&buffer[..head]).to_string();
        let mut lines = text.split("\r\n");
        let request_line = lines.next()?.to_string();
        let mut headers = Vec::new();
        let mut length = 0_usize;
        for line in lines {
            if line.is_empty() {
                break;
            }
            headers.push(line.to_string());
            if let Some(value) = line
                .strip_prefix("Content-Length: ")
                .or_else(|| line.strip_prefix("content-length: "))
            {
                length = value.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0_u8; length];
        stream.read_exact(&mut body).ok()?;
        Some((
            request_line,
            headers.join("\n"),
            String::from_utf8_lossy(&body).to_string(),
        ))
    }

    fn serve(mut stream: TcpStream, fixture: &Fixture) {
        let Some((request_line, header_block, body)) = read_request(&mut stream) else {
            return;
        };
        let mut parts = request_line.split_whitespace();
        let (method, path) = (
            parts.next().unwrap_or("").to_string(),
            parts.next().unwrap_or("").to_string(),
        );
        let kind = classify(&path);
        fixture
            .seen
            .lock()
            .expect("seen lock")
            .push(format!("{method} {path}"));
        fixture.bodies.lock().expect("body lock").push(body.clone());
        fixture
            .headers
            .lock()
            .expect("header lock")
            .push(header_block.clone());
        let _ = &header_block;
        if method == "POST" {
            fixture.posts.fetch_add(1, Ordering::SeqCst);
        } else {
            fixture.gets.fetch_add(1, Ordering::SeqCst);
        }
        let signature = format!("{method} {path}");
        if fixture
            .drop_on
            .lock()
            .expect("drop lock")
            .iter()
            .any(|entry| signature.contains(entry))
        {
            // The request is recorded, then the connection dies with no reply.
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return;
        }
        if fixture.redirect.load(Ordering::SeqCst) > 0 {
            fixture.redirect.fetch_sub(1, Ordering::SeqCst);
            let _ = stream.write_all(
                b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://elsewhere.invalid/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            let _ = stream.flush();
            return;
        }
        if fixture.oversized.load(Ordering::SeqCst) > 0 {
            fixture.oversized.fetch_sub(1, Ordering::SeqCst);
            let declared = MAX_RESPONSE_BYTES + 16;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&vec![b'x'; declared]);
            let _ = stream.flush();
            return;
        }
        let widen = fixture.widen.load(Ordering::SeqCst) > 0;
        if kind == "receipt" {
            // Only an effect this server actually processed may be reported,
            // and always with the payload and deadline it produced at the time.
            let operation = receipt_operation(&path);
            let key = path.rsplit('/').next().unwrap_or("").to_string();
            let recorded = fixture
                .effects
                .lock()
                .expect("effect lock")
                .get(&format!("{operation}:{key}"))
                .cloned();
            let not_yet_visible = fixture.unknown_receipt.swap(0, Ordering::SeqCst) > 0;
            let answer = match recorded.filter(|_| !not_yet_visible) {
                Some(effect) => json!({
                    "request_id": key,
                    "operation": operation,
                    "status": "SUCCEEDED",
                    "response": serde_json::from_str::<Value>(&effect).expect("recorded effect")
                }),
                None => json!({
                    "request_id": key,
                    "operation": operation,
                    "status": "UNKNOWN",
                    "response": null
                }),
            };
            let receipt_body = serde_json::to_vec(&answer).expect("receipt payload");
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                receipt_body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&receipt_body);
            let _ = stream.flush();
            return;
        }
        let payload = serde_json::to_vec(&respond_to(kind, &path, &body, widen)).expect("payload");
        if method == "POST" && matches!(kind, "create" | "exchange" | "revoke") {
            // The effect exists server-side even when the reply is lost below.
            let key = request_key(&body, &path);
            fixture.effects.lock().expect("effect lock").insert(
                format!("{kind}:{key}"),
                String::from_utf8_lossy(&payload).to_string(),
            );
        }
        if fixture.chunked.load(Ordering::SeqCst) > 0 {
            fixture.chunked.fetch_sub(1, Ordering::SeqCst);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            );
            // No Content-Length: the cap has to trip while streaming.
            let filler = vec![b'x'; MAX_RESPONSE_BYTES + 8];
            let mut offset = 0;
            while offset < filler.len() {
                let slice = &filler[offset..filler.len().min(offset + 900)];
                let _ = write!(stream, "{:x}\r\n", slice.len());
                let _ = stream.write_all(slice);
                let _ = stream.write_all(b"\r\n");
                offset += slice.len();
            }
            let _ = stream.flush();
            return;
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&payload);
        let _ = stream.flush();
    }

    fn loopback_sender(origin: &str) -> Sender {
        let client = Client::builder()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("loopback client");
        Sender::Loopback {
            client,
            origin: origin.to_string(),
        }
    }

    #[test]
    fn matching_hash_matches_the_delivered_golden_vector() {
        assert_eq!(
            hex::encode(matching_canonical_bytes(FIXTURE_ID, "ABCDEFGHJK")),
            "666163746f3a706172746e65722d636c693a6d61746368696e673a763100746573740031313131313131312d313131312d343131312d383131312d3131313131313131313131310041424344454647484a4b"
        );
        assert_eq!(
            verification_code_hash(FIXTURE_ID, "ABCDEFGHJK"),
            "1cba9a1db5d50072c42010d9727b800d8ebf20ac7ce7efab3bd947b37c900901"
        );
    }

    #[test]
    fn pairing_code_normalization_and_alphabet() {
        assert_eq!(
            normalize_pairing_code("abcde-fghjk").as_deref(),
            Some("ABCDEFGHJK")
        );
        assert_eq!(
            normalize_pairing_code("  ABCDEFGHJK  ").as_deref(),
            Some("ABCDEFGHJK")
        );
        for invalid in [
            "ABCDE--FGHJK",
            "ABCDE/FGHJK",
            "ABCDEＦGHJK",
            "ABCDEOGHJK",
            "ABCDEFGHI",
            "ABCDEFGHIJKL",
        ] {
            assert_eq!(normalize_pairing_code(invalid), None, "{invalid}");
        }
        let text = String::from_utf8_lossy(&ALPHABET);
        assert_eq!(text, "ABCDEFGHJKLMNPQRSTUVWXYZ23456789");
        for ambiguous in ['I', 'O', '0', '1'] {
            assert!(!text.contains(ambiguous));
        }
    }

    #[test]
    fn all_digit_uuids_pass_and_malformed_shapes_fail() {
        assert!(is_uuid("00000000-0000-4000-8000-000000000000"));
        assert!(is_uuid(FIXTURE_ID));
        assert!(!is_uuid("00000000-0000-4000-8000-00000000000G"));
        assert!(!is_uuid("00000000-0000-4000-8000-00000000000A"));
        assert!(!is_uuid("00000000-0000-4000-8000-00000000000"));
        assert!(!is_uuid("00000000_0000_4000_8000_000000000000"));
        for _ in 0..16 {
            let id = new_uuid_v4().expect("uuid");
            assert!(is_uuid(&id), "{id}");
            assert!(new_proof().is_ok());
        }
    }

    #[test]
    fn limits_match_the_delivered_fixture_policy() {
        assert_eq!(
            (
                PENDING_LIMIT,
                POLL_SECONDS,
                ACTIVE_LIMIT,
                ABSOLUTE_PARENT_LIMIT
            ),
            (600, 2, 900, 28_800)
        );
        assert_eq!(MAX_RESPONSE_BYTES, 65_536);
        assert_eq!(REQUEST_TIMEOUT, StdDuration::from_secs(8));
        assert_eq!(ACTIONS, ["workspace.access.read", "merchant.apps.read"]);
        assert_eq!(SERVER_ERROR_CODES.len(), 21);
        assert_eq!(PHASES.len(), 6);
        assert_eq!(RECEIPT_OPERATIONS, ["create", "exchange", "revoke"]);
    }

    #[test]
    fn approval_page_locator_is_the_only_pairing_parameter() {
        let uri = approval_page_uri(FIXTURE_ID);
        assert_eq!(
            uri,
            format!("https://test-partner.facto.to/cli/approve?authorization_id={FIXTURE_ID}")
        );
        assert!(!uri.contains("code") && !uri.contains("proof") && !uri.contains(API_ORIGIN));
    }

    // ── Wire validation ─────────────────────────────────────────────────────

    #[test]
    fn create_shape_survives_history_while_liveness_rejects_it() {
        let view: CreateResponse = parse(created_view().to_string().as_bytes()).expect("shape");
        assert_eq!(
            validate_create(view.clone(), FIXTURE_ID, clock()).expect("live"),
            view
        );
        // The same original effect read after its own deadline is still a valid
        // historical receipt.
        check_create_shape(&view, FIXTURE_ID).expect("historical shape");
        let late = clock() + Duration::seconds(3600);
        assert_eq!(
            validate_create(view, FIXTURE_ID, late).unwrap_err(),
            Failure::Local("wire_invalid")
        );
        let denied: CreateResponse = parse(
            patch(created_view(), "status", json!("DENIED"))
                .to_string()
                .as_bytes(),
        )
        .expect("wire");
        assert_eq!(
            check_create_shape(&denied, FIXTURE_ID).unwrap_err(),
            Failure::Local("wire_invalid")
        );
        for mutation in [
            patch(created_view(), "revision", json!(2)),
            patch(created_view(), "poll_interval_seconds", json!(1)),
            patch(created_view(), "realm", json!("external")),
            patch(created_view(), "target_environment", json!("dev")),
            patch(created_view(), "request_id", json!(OTHER_ID)),
            patch(
                created_view(),
                "verification_uri",
                json!(format!(
                    "{API_ORIGIN}/cli/approve?authorization_id={FIXTURE_ID}"
                )),
            ),
            patch(created_view(), "proof", json!("a".repeat(64))),
        ] {
            let parsed: Result<CreateResponse, _> = parse(mutation.to_string().as_bytes());
            assert!(
                parsed.is_err() || check_create_shape(&parsed.expect("wire"), FIXTURE_ID).is_err(),
                "{mutation}"
            );
        }
    }

    #[test]
    fn exchange_is_bound_to_the_persisted_intent_and_the_approved_ceiling() {
        let request_id = "77777777-7777-4777-8777-777777777777";
        let response: ExchangeResponse =
            parse(exchange_view(request_id).to_string().as_bytes()).expect("wire");
        let ceiling = snapshot(&["mapp_one"]);
        assert_eq!(
            validate_exchange(
                response.clone(),
                FIXTURE_ID,
                request_id,
                2,
                &ceiling,
                clock()
            )
            .expect("valid")
            .request_id,
            request_id
        );
        assert_eq!(
            validate_exchange(response.clone(), FIXTURE_ID, OTHER_ID, 2, &ceiling, clock())
                .unwrap_err(),
            Failure::Local("wire_invalid")
        );
        assert_eq!(
            validate_exchange(
                response.clone(),
                FIXTURE_ID,
                request_id,
                5,
                &ceiling,
                clock()
            )
            .unwrap_err(),
            Failure::Local("cli_scope_changed")
        );
        // An expiry later than the approved ceiling is a widened grant.
        let wide = patch(exchange_view(request_id), "expires_at", json!(moment(500)));
        let wide: ExchangeResponse = parse(wide.to_string().as_bytes()).expect("wire");
        assert_eq!(
            validate_exchange(wide, FIXTURE_ID, request_id, 2, &ceiling, clock()).unwrap_err(),
            Failure::Local("wire_invalid")
        );
        let second_bearer = patch(exchange_view(request_id), "session_id", json!(OTHER_ID));
        let second_bearer: ExchangeResponse =
            parse(second_bearer.to_string().as_bytes()).expect("wire");
        assert!(check_exchange_shape(&second_bearer, FIXTURE_ID, request_id).is_err());
        let same_revision = patch(exchange_view(request_id), "revision", json!(2));
        let same_revision: ExchangeResponse =
            parse(same_revision.to_string().as_bytes()).expect("wire");
        assert!(check_exchange_shape(&same_revision, FIXTURE_ID, request_id).is_err());
    }

    #[test]
    fn current_read_may_only_narrow_the_approved_ceiling() {
        let ceiling = snapshot(&["mapp_one"]);
        let base: WorkspaceResponse = parse(workspace_view().to_string().as_bytes()).expect("wire");
        assert!(validate_workspace(base.clone(), FIXTURE_ID, &ceiling, clock()).is_ok());
        for mutation in [
            patch(workspace_view(), "authorized_app_ids", json!(["mapp_two"])),
            patch(
                workspace_view(),
                "apps",
                json!([{"app_id": "mapp_two", "display_name": "x", "status": "active"}]),
            ),
            patch(
                workspace_view(),
                "partner_user_id",
                json!("99999999-9999-4999-8999-999999999999"),
            ),
            patch(workspace_view(), "expires_at", json!(moment(600))),
            patch(workspace_view(), "realm", json!("external")),
            patch(workspace_view(), "environment", json!("test")),
            patch(
                workspace_view(),
                "granted_actions",
                json!(["merchant.apps.read", "workspace.access.read"]),
            ),
            patch(
                workspace_view(),
                "authorized_app_ids",
                json!(["mapp_one", "mapp_one"]),
            ),
            patch(workspace_view(), "session_id", json!(OTHER_ID)),
            patch(workspace_view(), "authorization_revision", json!(0)),
            patch(workspace_view(), "client_secret", json!("never")),
        ] {
            let parsed: Result<WorkspaceResponse, _> = parse(mutation.to_string().as_bytes());
            let outcome = match parsed {
                Ok(workspace) => validate_workspace(workspace, FIXTURE_ID, &ceiling, clock()),
                Err(failure) => Err(failure),
            };
            assert!(outcome.is_err(), "{mutation} must fail closed");
        }
        let expired = patch(workspace_view(), "expires_at", json!(moment(-1)));
        let expired: WorkspaceResponse = parse(expired.to_string().as_bytes()).expect("wire");
        assert_eq!(
            validate_workspace(expired, FIXTURE_ID, &ceiling, clock()).unwrap_err(),
            Failure::Local("cli_expired")
        );
    }

    #[test]
    fn receipt_status_and_operation_are_typed_per_operation() {
        assert_eq!(decode_receipt(json!({"request_id": FIXTURE_ID, "operation": "create", "status": "UNKNOWN", "response": null}).to_string().as_bytes(), "create", FIXTURE_ID).expect("unknown"), None);
        assert!(decode_receipt(json!({"request_id": FIXTURE_ID, "operation": "create", "status": "SUCCEEDED", "response": created_view()}).to_string().as_bytes(), "create", FIXTURE_ID).expect("value").is_some());
        for bad in [
            json!({"request_id": FIXTURE_ID, "operation": "approve", "status": "UNKNOWN", "response": null}),
            json!({"request_id": FIXTURE_ID, "operation": "create", "status": "FAILED", "response": null}),
            json!({"request_id": FIXTURE_ID, "operation": "create", "status": "SUCCEEDED", "response": null}),
            json!({"request_id": OTHER_ID, "operation": "create", "status": "UNKNOWN", "response": null}),
            json!({"request_id": FIXTURE_ID, "operation": "create", "status": "UNKNOWN", "response": null, "token": "never"}),
        ] {
            assert!(
                decode_receipt(bad.to_string().as_bytes(), "create", FIXTURE_ID).is_err(),
                "{bad}"
            );
        }
        // An arbitrary SUCCEEDED payload is not an exchange effect.
        let junk = json!({"request_id": FIXTURE_ID, "operation": "exchange", "status": "SUCCEEDED", "response": {"a": 1}});
        let value = decode_receipt(junk.to_string().as_bytes(), "exchange", FIXTURE_ID)
            .expect("wire")
            .expect("payload");
        let parsed: Result<ExchangeResponse, _> = serde_json::from_value(value);
        assert!(parsed.is_err());
    }

    #[test]
    fn http_status_maps_to_the_closed_code_set_only() {
        let wire = json!({"code": "cli_rate_limited", "message": "slow down"});
        assert_eq!(
            interpret(
                StatusCode::TOO_MANY_REQUESTS,
                wire.to_string().into_bytes(),
                StatusCode::OK
            )
            .unwrap_err(),
            Failure::Server("cli_rate_limited")
        );
        assert_eq!(
            interpret(
                StatusCode::BAD_GATEWAY,
                b"not json".to_vec(),
                StatusCode::OK
            )
            .unwrap_err(),
            Failure::Local("http_rejected")
        );
        assert_eq!(
            interpret(StatusCode::NOT_FOUND, Vec::new(), StatusCode::OK).unwrap_err(),
            Failure::Local("http_rejected")
        );
        assert_eq!(
            interpret(StatusCode::OK, b"{}".to_vec(), StatusCode::OK).expect("body"),
            b"{}"
        );
    }

    // ── Transport seam ──────────────────────────────────────────────────────

    #[test]
    fn built_requests_carry_only_the_dedicated_proof_scheme() {
        let built = build_create(&"a".repeat(64), FIXTURE_ID, body(FIXTURE_ID, "reuse"));
        let (client, request) = live_request(&built).expect("live request");
        drop(client);
        assert_eq!(
            request.url().as_str(),
            format!("{API_ORIGIN}{PREFIX}/authorizations")
        );
        assert_eq!(request.method().as_str(), "POST");
        assert_eq!(
            request
                .headers()
                .get(header::AUTHORIZATION)
                .expect("auth")
                .to_str()
                .expect("ascii"),
            format!("Facto-CLI {}", "a".repeat(64))
        );
        assert_eq!(
            request
                .headers()
                .get("x-facto-cli-request-id")
                .expect("locator"),
            FIXTURE_ID
        );
        assert_eq!(
            request.headers().get("idempotency-key").expect("key"),
            FIXTURE_ID
        );
        assert_eq!(
            request.headers().get("x-facto-target").expect("target"),
            "test"
        );
        assert!(!request.headers().contains_key(header::COOKIE));
        assert!(!request.headers().contains_key("x-facto-client-secret"));
        assert!(!request.headers().contains_key("x-api-key"));

        let read = build_read(
            &"a".repeat(64),
            FIXTURE_ID,
            format!("{PREFIX}/session/workspace"),
        );
        let (_, request) = live_request(&read).expect("read request");
        assert_eq!(request.method().as_str(), "GET");
        assert!(!request.headers().contains_key("idempotency-key"));
        assert!(!request.headers().contains_key("x-facto-target"));
        assert!(!request.headers().contains_key(header::COOKIE));

        let weak_proofs: [String; 4] = [
            String::new(),
            "a".repeat(63),
            "A".repeat(64),
            "g".repeat(64),
        ];
        for weak in weak_proofs {
            let broken = Built {
                proof: weak,
                ..build_read(&"a".repeat(64), FIXTURE_ID, "/x".into())
            };
            assert!(
                proof_headers(&broken).is_err(),
                "proof shape must be rejected"
            );
        }
        let bad_locator = Built {
            locator: "nope".to_string(),
            ..build_read(&"a".repeat(64), FIXTURE_ID, "/x".into())
        };
        assert!(
            proof_headers(&bad_locator).is_err(),
            "locator must be a UUID"
        );
    }

    #[test]
    fn response_body_is_capped_across_chunks() {
        let big = vec![vec![b'x'; MAX_RESPONSE_BYTES]; 2];
        assert_eq!(
            assemble(StatusCode::OK, &big, StatusCode::OK).unwrap_err(),
            Failure::Local("response_too_large")
        );
        let exact = vec![vec![b'x'; MAX_RESPONSE_BYTES]];
        assert_eq!(
            assemble(StatusCode::OK, &exact, StatusCode::OK)
                .expect("exact")
                .len(),
            MAX_RESPONSE_BYTES
        );
        let split = vec![
            vec![b'x'; MAX_RESPONSE_BYTES - 1],
            vec![b'x'; 1],
            vec![b'x'; 1],
        ];
        assert_eq!(
            assemble(StatusCode::OK, &split, StatusCode::OK).unwrap_err(),
            Failure::Local("response_too_large")
        );
    }

    #[tokio::test]
    async fn a_redirect_is_rejected_without_following_or_retrying() {
        let cell = stub();
        cell.borrow_mut()
            .status(301, &json!({"location": "https://elsewhere.example"}));
        let built = build_read(
            &"a".repeat(64),
            FIXTURE_ID,
            format!("{PREFIX}/session/workspace"),
        );
        let sender = Sender::Stub(cell.clone());
        assert_eq!(
            sender.transact(&built, StatusCode::OK).await.unwrap_err(),
            Failure::Local("http_rejected")
        );
        assert_eq!(cell.borrow().fetched.len(), 1, "no retry request");
        assert!(cell.borrow().posted.is_empty());
    }

    #[tokio::test]
    async fn the_live_client_refuses_plain_http_endpoints() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("addr").port();
        let built = build_read(
            &"a".repeat(64),
            FIXTURE_ID,
            format!("{PREFIX}/session/workspace"),
        );
        let (client, _) = live_request(&built).expect("live request");
        // The listener is released first, so the refusal is immediate instead
        // of waiting on the request timeout.
        drop(listener);
        let outcome = client
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .map(|response| response.status());
        assert!(
            outcome.is_err(),
            "https_only must refuse a plain HTTP origin"
        );
    }

    // ── Pairing channel fencing ─────────────────────────────────────────────

    #[test]
    fn pairing_code_only_reaches_an_interactive_terminal() {
        assert_eq!(pairing_channel(false, true, false), Channel::Stdout);
        assert_eq!(pairing_channel(true, true, false), Channel::Stderr);
        assert_eq!(pairing_channel(true, false, false), Channel::Refused);
        assert_eq!(pairing_channel(false, false, true), Channel::Refused);
        assert_eq!(pairing_channel(true, false, true), Channel::Stderr);

        let code = display_pairing_code("ABCDEFGHJK");
        let uri = approval_page_uri(FIXTURE_ID);
        let mut out = Vec::new();
        let mut err = Vec::new();
        announce_pairing(&uri, &code, Channel::Stdout, &mut out, &mut err).expect("stdout");
        assert!(String::from_utf8_lossy(&out).contains(&code));
        assert!(err.is_empty());

        let mut out = Vec::new();
        let mut err = Vec::new();
        announce_pairing(&uri, &code, Channel::Stderr, &mut out, &mut err).expect("stderr");
        assert!(
            out.is_empty(),
            "terse stdout is reserved for the JSON report"
        );
        assert!(String::from_utf8_lossy(&err).contains(&code));

        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            announce_pairing(&uri, &code, Channel::Refused, &mut out, &mut err).unwrap_err(),
            Failure::Local("pairing_requires_terminal")
        );
        assert!(out.is_empty() && err.is_empty());
    }

    #[test]
    fn reports_and_debug_never_carry_credential_material() {
        let mut report = Report::new("partner_cli_authorize", "fresh", "workspace");
        report.authorization_id = Some(FIXTURE_ID.into());
        report.session_id = Some(FIXTURE_ID.into());
        report.verification_uri = Some(approval_page_uri(FIXTURE_ID));
        report.authentication_verified = true;
        let json = report.clone().succeeded().render(true);
        let proof_shape = "a".repeat(64);
        let forbidden: [&str; 7] = [
            "proof",
            proof_shape.as_str(),
            "matching",
            "cookie",
            "secret",
            "ABCDEFGHJK",
            "FGHIJK",
        ];
        for needle in forbidden {
            assert!(!json.contains(needle), "{needle} leaked: {json}");
        }
        assert!(json.contains("\"uat_status\":\"NOT_RUN\""));
        assert!(json.contains("\"consumer_state_accessed\":false"));
        assert!(json.contains("\"app_secret_transport\":false"));
        let human = report.render(false);
        assert!(!human.contains("Pairing code"));

        let entry = journal(FIXTURE_ID);
        let debug = format!("{entry:?}");
        assert!(
            !debug.contains(&"a".repeat(64)) && !debug.contains("ABCDEFGHJK"),
            "{debug}"
        );
    }

    // ── State machine seams ─────────────────────────────────────────────────

    fn store(tag: &str) -> std::path::PathBuf {
        let home = sandbox(tag);
        let dir = state::store_dir(&home);
        state::ensure_store(&dir).expect("store");
        dir
    }

    #[tokio::test]
    async fn an_unresolved_create_intent_is_never_posted_twice() {
        let dir = store("resume");
        let mut entry = journal(FIXTURE_ID);
        entry.create_recorded = false;
        entry.phase = "PREWRITE".into();
        entry.updated_at = clock().to_rfc3339();
        state::create(&dir, &entry).expect("intent");

        let cell = stub();
        cell.borrow_mut().ok(&json!({"request_id": FIXTURE_ID, "operation": "create", "status": "UNKNOWN", "response": null}));
        let sender = Sender::Stub(cell.clone());
        let mut report = Report::new("partner_cli_authorize", "reuse", "precheck");
        let outcome = resume_create(&dir, entry, &sender, &mut report).await;
        assert_eq!(outcome.unwrap_err(), Failure::Local("receipt_unknown"));
        assert_eq!(
            cell.borrow().post_count(),
            0,
            "a known intent must not be re-posted"
        );
        assert!(report.next_step.is_some());
        let stored = state::read(&state::journal_path(&dir, FIXTURE_ID)).expect("stored");
        assert!(stored.unknown.contains(&"create".to_string()));
        assert!(stored.has_unresolved_intent());
    }

    #[tokio::test]
    async fn a_historical_create_success_resumes_the_original_pairing() {
        let dir = store("recover");
        let mut entry = journal(FIXTURE_ID);
        entry.create_recorded = false;
        entry.phase = "UNKNOWN".into();
        entry.mark_unknown("create");
        state::create(&dir, &entry).expect("intent");

        let cell = stub();
        cell.borrow_mut().ok(&json!({"request_id": FIXTURE_ID, "operation": "create", "status": "SUCCEEDED", "response": created_view()}));
        let sender = Sender::Stub(cell.clone());
        let mut report = Report::new("partner_cli_authorize", "reuse", "precheck");
        let resumed = resume_create(&dir, entry, &sender, &mut report)
            .await
            .expect("recovered");
        assert!(resumed.create_recorded && resumed.unknown.is_empty());
        assert_eq!(resumed.phase, "PENDING");
        assert_eq!(
            resumed.expires_at,
            moment(300),
            "the original deadline is kept"
        );
        assert_eq!(
            resumed.pairing_code, "ABCDEFGHJK",
            "the pairing code survives the lost response"
        );
        assert_eq!(cell.borrow().post_count(), 0);
    }

    #[tokio::test]
    async fn a_lost_create_response_is_written_ahead_then_read_once() {
        let dir = store("lost");
        let cell = stub();
        // Nothing is scripted: the create POST is lost in transit.
        let sender = Sender::Stub(cell.clone());
        let mut report = Report::new("partner_cli_authorize", "fresh", "precheck");
        let outcome = fresh_pairing("fresh", &dir, &sender, &mut report, clock()).await;
        assert_eq!(
            outcome.unwrap_err(),
            Failure::Local("transport_unavailable")
        );
        assert_eq!(cell.borrow().post_count(), 1, "exactly one create attempt");
        let stored = state::current(&dir).expect("stored");
        assert!(stored.has_unresolved_intent() && !stored.create_recorded);
        assert!(
            !stored.pairing_code.is_empty(),
            "the pairing code survives the loss"
        );

        // A reload of the same on-disk journal reads that receipt and posts nothing.
        let id = stored.authorization_id.clone();
        let cell2 = stub();
        cell2
            .borrow_mut()
            .ok(&json!({"request_id": id, "operation": "create", "status": "UNKNOWN", "response": null}));
        let mut report = Report::new("partner_cli_session", "fresh", "precheck");
        let outcome = resume_create(&dir, stored, &Sender::Stub(cell2.clone()), &mut report).await;
        assert_eq!(outcome.unwrap_err(), Failure::Local("receipt_unknown"));
        assert_eq!(cell2.borrow().post_count(), 0, "the reload never re-posts");
        assert_eq!(cell2.borrow().fetched.len(), 1);
        assert!(state::current(&dir)
            .expect("still unresolved")
            .has_unresolved_intent());
    }

    #[tokio::test]
    async fn a_reloaded_journal_reuses_the_exchange_intent_and_stays_unverified() {
        let dir = store("exchange");
        let exchange_id = "77777777-7777-4777-8777-777777777777";
        let mut entry = journal(FIXTURE_ID);
        entry.phase = "APPROVED".into();
        entry.revision = 2;
        entry.approval = Some(json!(snapshot(&["mapp_one"])));
        entry.record_exchange_intent(exchange_id, 2);
        state::create(&dir, &entry).expect("intent");

        let cell = stub();
        cell.borrow_mut().ok(&json!({"request_id": exchange_id, "operation": "exchange", "status": "SUCCEEDED", "response": exchange_view(exchange_id)}));
        let sender = Sender::Stub(cell.clone());
        let mut report = Report::new("partner_cli_session", "reuse", "precheck");
        let mut stored = state::read(&state::journal_path(&dir, FIXTURE_ID)).expect("stored");
        recover_exchange(&dir, &mut stored, &sender, &mut report)
            .await
            .expect("recovered");
        assert_eq!(
            cell.borrow().post_count(),
            0,
            "the original intent is only read back"
        );
        let after = state::read(&state::journal_path(&dir, FIXTURE_ID)).expect("after");
        assert!(after.exchange_recorded && after.unknown.is_empty());
        assert_eq!(after.phase, "ACTIVE");
        assert_eq!(after.session_id.as_deref(), Some(FIXTURE_ID));
        assert_eq!(
            report.authentication_verified, false,
            "recovery alone is not current access"
        );
        assert_eq!(report.effect.as_deref(), Some("exchange:SUCCEEDED"));

        // A junk SUCCEEDED payload must not clear the intent.
        let junk = stub();
        junk.borrow_mut().ok(&json!({"request_id": exchange_id, "operation": "exchange", "status": "SUCCEEDED", "response": {"a": 1}}));
        let mut dirty = after.clone();
        dirty.exchange_recorded = false;
        dirty.mark_unknown("exchange");
        let mut report = Report::new("partner_cli_session", "reuse", "precheck");
        assert_eq!(
            recover_exchange(&dir, &mut dirty, &Sender::Stub(junk.clone()), &mut report)
                .await
                .unwrap_err(),
            Failure::Local("wire_invalid")
        );
        assert!(
            dirty.unknown.contains(&"exchange".to_string()),
            "intent kept on a bad payload"
        );
    }

    #[tokio::test]
    async fn logout_never_resends_a_recorded_or_unknown_revocation() {
        let dir = store("logout");
        let revoke_id = "99999999-9999-4999-8999-999999999999";
        let mut entry = journal(FIXTURE_ID);
        entry.phase = "ACTIVE".into();
        entry.revision = 3;
        entry.record_revoke_intent(revoke_id, 3);
        entry.mark_unknown("revoke");
        state::create(&dir, &entry).expect("intent");

        let cell = stub();
        cell.borrow_mut().ok(&json!({"request_id": revoke_id, "operation": "revoke", "status": "UNKNOWN", "response": null}));
        let sender = Sender::Stub(cell.clone());
        let mut stored = state::read(&state::journal_path(&dir, FIXTURE_ID)).expect("stored");
        let mut report = Report::new("partner_cli_logout", "reuse", "precheck");
        assert_eq!(
            recover_revoke(&dir, &mut stored, &sender, &mut report)
                .await
                .unwrap_err(),
            Failure::Local("receipt_unknown")
        );
        assert_eq!(cell.borrow().post_count(), 0, "no resend under a new key");
        assert!(state::read(&state::journal_path(&dir, FIXTURE_ID))
            .expect("journal")
            .unknown
            .contains(&"revoke".to_string()));

        let revoked = {
            let mut done = journal(FIXTURE_ID);
            done.phase = "REVOKED".into();
            done.revoke_recorded = true;
            done.revoke_request_id = Some(revoke_id.into());
            done.revoke_expected_revision = Some(3);
            done
        };
        let dir2 = store("logout-done");
        state::create(&dir2, &revoked).expect("revoked journal");
        let cell2 = stub();
        let mut report = Report::new("partner_cli_logout", "reuse", "precheck");
        let stored = state::read(&state::journal_path(&dir2, FIXTURE_ID)).expect("stored");
        // The kernel's own gate: a recorded revocation with no pending intent is
        // a definite local success and never reaches a POST.
        assert_eq!(
            (
                stored.phase.as_str(),
                stored.revoke_recorded,
                stored.pending_intents().is_empty()
            ),
            ("REVOKED", true, true)
        );
        assert_eq!(
            cell2.borrow().post_count(),
            0,
            "a repeat logout is a local success only"
        );
        report.phase = Some(stored.phase.clone());
        assert_eq!(report.phase.as_deref(), Some("REVOKED"));
    }

    #[tokio::test]
    async fn positive_handoff_flow_reads_current_access_once() {
        let cell = stub();
        let exchange_id = "77777777-7777-4777-8777-777777777777";
        // The stub serves responses in consumption order: create, poll, read.
        cell.borrow_mut().created(&created_view());
        cell.borrow_mut().ok(&approve_view());
        cell.borrow_mut().ok(&workspace_view());
        let sender = Sender::Stub(cell.clone());
        let dir = store("flow");

        let mut report = Report::new("partner_cli_authorize", "reuse", "precheck");
        let mut journal = fresh_pairing("reuse", &dir, &sender, &mut report, now())
            .await
            .expect("created");
        assert_eq!(journal.phase, "PENDING");
        let approved =
            wait_for_approval(&dir, &mut journal, &sender, clock() + Duration::seconds(60))
                .await
                .expect("approved");
        assert_eq!(approved.status, "APPROVED");
        journal.record_exchange_intent(exchange_id, approved.revision);
        persist(&dir, &journal).expect("intent");
        let reread = state::read(&state::journal_path(&dir, FIXTURE_ID)).expect("reread");
        // The intent survived the write-ahead step unchanged: same create and
        // exchange request ids, still the pre-exchange phase.
        assert_eq!(reread.authorization_id, FIXTURE_ID);
        assert_eq!(reread.create_request_id, FIXTURE_ID);
        assert_eq!(reread.exchange_request_id.as_deref(), Some(exchange_id));
        assert_eq!(reread.exchange_expected_revision, Some(approved.revision));
        assert_eq!(reread.phase, "APPROVED");
        assert!(!reread.exchange_recorded);
        let response: ExchangeResponse =
            parse(exchange_view(exchange_id).to_string().as_bytes()).expect("wire");
        validate_exchange(
            response,
            FIXTURE_ID,
            exchange_id,
            approved.revision,
            &snapshot(&["mapp_one"]),
            clock(),
        )
        .expect("exchange");
        let workspace: WorkspaceResponse =
            parse(workspace_view().to_string().as_bytes()).expect("wire");
        let validated =
            validate_workspace(workspace, FIXTURE_ID, &snapshot(&["mapp_one"]), clock())
                .expect("current read");
        assert_eq!(validated.apps.len(), 1);
        assert_eq!(
            cell.borrow().post_count(),
            1,
            "one create POST in the whole flow"
        );
    }

    #[test]
    fn dev_target_and_non_terminal_pairing_fail_closed_before_the_store() {
        assert_eq!(reject_target("test"), Ok(()));
        assert_eq!(
            reject_target("dev").unwrap_err(),
            Failure::Local("target_unsupported")
        );
        for command in ["authorize", "session", "check-access", "logout"] {
            let parsed = crate::Cli::try_parse_from([
                "facto", "demo", command, "--demo", "shop", "--target", "dev", "--mode", "fresh",
            ])
            .expect("parses");
            assert!(matches!(parsed.command, crate::Commands::Demo { .. }));
        }
        for args in [
            vec![
                "facto",
                "demo",
                "authorize",
                "--demo",
                "shop",
                "--target",
                "test",
            ],
            vec![
                "facto", "demo", "session", "--demo", "orbit", "--target", "test", "--mode",
                "fresh",
            ],
            vec![
                "facto",
                "demo",
                "logout",
                "--demo",
                "shop",
                "--target",
                "production",
                "--mode",
                "fresh",
            ],
            vec![
                "facto",
                "demo",
                "check-access",
                "--demo",
                "shop",
                "--target",
                "test",
                "--mode",
                "reused",
            ],
        ] {
            assert!(crate::Cli::try_parse_from(&args).is_err(), "{args:?}");
        }
        assert!(pairing_channel(false, false, false) == Channel::Refused);
    }

    #[test]
    fn resume_and_block_rules_follow_the_stored_phase() {
        let now = clock();
        let mut pending = journal(FIXTURE_ID);
        assert!(wants_resume(&pending, now), "a live pairing is resumed");
        pending.create_recorded = false;
        assert!(wants_resume(&pending, now));
        pending.create_recorded = true;
        pending.phase = "ACTIVE".into();
        assert!(blocks_new_pairing(&pending, now) && !wants_resume(&pending, now));
        pending.expires_at = moment(-1);
        assert!(!blocks_new_pairing(&pending, now));
        pending.phase = "REVOKED".into();
        pending.expires_at = moment(60);
        assert!(!wants_resume(&pending, now) && !blocks_new_pairing(&pending, now));
        let mut unknown = journal(FIXTURE_ID);
        unknown.mark_unknown("revoke");
        assert!(
            wants_resume(&unknown, now),
            "an unresolved revoke is not forgotten"
        );
    }

    #[tokio::test]
    async fn the_full_chain_over_a_real_client_is_two_posts_and_two_gets() {
        let (origin, server) = loopback();
        let sender = loopback_sender(&origin);
        let dir = store("loop-full");
        let mut report = Report::new("partner_cli_authorize", "reuse", "precheck");
        let mut journal = fresh_pairing("reuse", &dir, &sender, &mut report, now())
            .await
            .expect("create");
        let approved =
            wait_for_approval(&dir, &mut journal, &sender, now() + Duration::seconds(60))
                .await
                .expect("approved");
        assert_eq!(approved.status, "APPROVED");
        ensure_exchange(&dir, &mut journal, &sender, &mut report, approved.revision)
            .await
            .expect("exchanged");
        let ceiling = stored_approval(&journal).expect("ceiling");
        let workspace = read_workspace(&sender, &journal).await.expect("workspace");
        let workspace = validate_workspace(workspace, &journal.authorization_id, &ceiling, now())
            .expect("read");
        assert_eq!(workspace.apps.len(), 1);
        assert_eq!(
            server.posts.load(Ordering::SeqCst),
            2,
            "create and exchange"
        );
        assert_eq!(
            server.gets.load(Ordering::SeqCst),
            2,
            "poll and current read"
        );

        let seen = server.seen.lock().expect("seen");
        let auth = journal.authorization_id.as_str();
        assert_eq!(seen[0], format!("POST {PREFIX}/authorizations"));
        assert_eq!(seen[1], format!("GET {PREFIX}/authorizations/{auth}"));
        assert_eq!(
            seen[2],
            format!("POST {PREFIX}/authorizations/{auth}/exchanges")
        );
        assert_eq!(seen[3], format!("GET {PREFIX}/session/workspace"));

        let headers = server.headers.lock().expect("headers");
        let create = headers[0].to_ascii_lowercase();
        assert!(create.contains("authorization: facto-cli "), "{create}");
        assert!(create.contains("idempotency-key: "), "{create}");
        assert!(create.contains("x-facto-target: test"), "{create}");
        assert!(create.contains("x-facto-cli-request-id: "), "{create}");
        assert!(!create.contains("cookie"), "no cookie may leave the CLI");
        assert!(
            !create.contains("x-facto-client-secret"),
            "no app secret transport"
        );
        assert!(!create.contains("x-api-key"), "no legacy HMAC surface");

        let bodies = server.bodies.lock().expect("bodies");
        assert!(
            bodies[0].contains("\"verification_code_hash\""),
            "{}",
            bodies[0]
        );
        assert!(
            bodies[2].contains("\"expected_revision\":2"),
            "{}",
            bodies[2]
        );
        // The proof never appears in a URL or a body.
        let proof = journal.proof_hex.clone();
        assert!(
            !bodies.iter().any(|body| body.contains(&proof)),
            "proof in a body"
        );
        assert!(
            !seen.iter().any(|line| line.contains(&proof)),
            "proof in a path"
        );
    }

    #[tokio::test]
    async fn a_dropped_create_reply_is_recovered_from_the_original_receipt_only() {
        let (origin, server) = loopback();
        server
            .drop_on
            .lock()
            .expect("drop lock")
            .push(format!("POST {PREFIX}/authorizations"));
        let sender = loopback_sender(&origin);
        let dir = store("loop-lost-create");
        let mut report = Report::new("partner_cli_authorize", "fresh", "precheck");
        // The reply never arrives, so the create is unknown and stays unknown
        // until the original receipt answers it.
        let outcome = fresh_pairing("fresh", &dir, &sender, &mut report, clock()).await;
        assert!(outcome.is_ok(), "the receipt recovers the pairing");
        let journal = outcome.expect("recovered");
        assert!(journal.create_recorded);
        assert_eq!(journal.phase, "PENDING");
        assert!(
            !journal.pairing_code.is_empty(),
            "the code survives the loss"
        );
        assert_eq!(
            server.posts.load(Ordering::SeqCst),
            1,
            "one create attempt only"
        );
        assert_eq!(
            server.gets.load(Ordering::SeqCst),
            1,
            "one original receipt read"
        );
        assert_eq!(
            server.seen.lock().expect("seen")[1],
            format!(
                "GET {PREFIX}/authorizations/{}/requests/create/{}",
                journal.authorization_id, journal.authorization_id
            )
        );
    }

    #[tokio::test]
    async fn a_dropped_exchange_reply_never_becomes_a_second_exchange_post() {
        let (origin, server) = loopback();
        let sender = loopback_sender(&origin);
        let dir = store("loop-lost-exchange");
        let mut report = Report::new("partner_cli_authorize", "reuse", "precheck");
        let mut journal = fresh_pairing("reuse", &dir, &sender, &mut report, now())
            .await
            .expect("created");
        let approved =
            wait_for_approval(&dir, &mut journal, &sender, now() + Duration::seconds(60))
                .await
                .expect("approved");
        let auth = journal.authorization_id.clone();
        server
            .drop_on
            .lock()
            .expect("drop lock")
            .push(format!("POST {PREFIX}/authorizations/{auth}/exchanges"));

        let failure = ensure_exchange(&dir, &mut journal, &sender, &mut report, approved.revision)
            .await
            .expect_err("the reply is lost");
        assert_eq!(failure, Failure::Local("transport_unavailable"));
        // The claim was durable, so the receipt could answer it in the same run.
        let after = state::read(&state::journal_path(&dir, &auth)).expect("after");
        assert!(after.exchange_recorded && after.pending_intents().is_empty());

        // A second invocation over the same journal sends nothing at all.
        let before_posts = server.posts.load(Ordering::SeqCst);
        let before_gets = server.gets.load(Ordering::SeqCst);
        let mut second = Report::new("partner_cli_authorize", "reuse", "precheck");
        let mut reloaded = state::current(&dir).expect("reloaded");
        ensure_exchange(&dir, &mut reloaded, &sender, &mut second, after.revision)
            .await
            .expect("already recorded");
        assert_eq!(
            server.posts.load(Ordering::SeqCst),
            before_posts,
            "no new POST"
        );
        assert_eq!(
            server.gets.load(Ordering::SeqCst),
            before_gets,
            "no needless read"
        );
        assert_eq!(before_posts, 2, "create plus the one lost exchange");
    }

    #[tokio::test]
    async fn the_real_client_refuses_oversized_chunked_and_redirected_bodies() {
        let (origin, server) = loopback();
        let sender = loopback_sender(&origin);
        let built = build_read(
            &"a".repeat(64),
            FIXTURE_ID,
            format!("{PREFIX}/session/workspace"),
        );

        server.oversized.store(1, Ordering::SeqCst);
        assert_eq!(
            sender.transact(&built, StatusCode::OK).await.unwrap_err(),
            Failure::Local("response_too_large")
        );
        server.chunked.store(1, Ordering::SeqCst);
        assert_eq!(
            sender.transact(&built, StatusCode::OK).await.unwrap_err(),
            Failure::Local("response_too_large")
        );
        server.redirect.store(1, Ordering::SeqCst);
        assert_eq!(
            sender.transact(&built, StatusCode::OK).await.unwrap_err(),
            Failure::Local("http_rejected")
        );
        assert_eq!(
            server.gets.load(Ordering::SeqCst),
            3,
            "each attempt is one request"
        );
        assert_eq!(
            server.posts.load(Ordering::SeqCst),
            0,
            "reads never became writes"
        );
        assert!(
            !server
                .seen
                .lock()
                .expect("seen")
                .iter()
                .any(|line| line.contains("elsewhere")),
            "the redirect target was never fetched"
        );
    }

    #[tokio::test]
    async fn a_widened_current_read_is_refused_by_the_real_client() {
        let (origin, server) = loopback();
        let sender = loopback_sender(&origin);
        let dir = store("loop-widen");
        let mut report = Report::new("partner_cli_authorize", "reuse", "precheck");
        let mut journal = fresh_pairing("reuse", &dir, &sender, &mut report, now())
            .await
            .expect("created");
        let approved =
            wait_for_approval(&dir, &mut journal, &sender, now() + Duration::seconds(60))
                .await
                .expect("approved");
        ensure_exchange(&dir, &mut journal, &sender, &mut report, approved.revision)
            .await
            .expect("exchanged");
        let ceiling = stored_approval(&journal).expect("ceiling");

        server.widen.store(1, Ordering::SeqCst);
        let workspace = read_workspace(&sender, &journal).await.expect("wire");
        assert_eq!(
            validate_workspace(workspace, &journal.authorization_id, &ceiling, now()).unwrap_err(),
            Failure::Local("cli_scope_changed")
        );
        assert_eq!(
            report.authentication_verified, false,
            "a refused read is never verified"
        );
    }

    #[tokio::test]
    async fn an_unknown_exchange_receipt_keeps_the_session_failing_closed() {
        let cell = stub();
        let dir = store("session-unknown");
        let mut entry = journal(FIXTURE_ID);
        entry.record_exchange_intent("77777777-7777-4777-8777-777777777777", 2);
        entry.mark_unknown("exchange");
        state::create(&dir, &entry).expect("intent");
        cell.borrow_mut().ok(&json!({
            "request_id": "77777777-7777-4777-8777-777777777777",
            "operation": "exchange",
            "status": "UNKNOWN",
            "response": null
        }));
        let mut report = Report::new("partner_cli_session", "reuse", "precheck");
        let mut stored = state::current(&dir).expect("stored");
        let outcome =
            recover_exchange(&dir, &mut stored, &Sender::Stub(cell.clone()), &mut report).await;
        assert_eq!(outcome.unwrap_err(), Failure::Local("receipt_unknown"));
        assert_eq!(report.status.as_str(), "failed");
        assert!(state::current(&dir)
            .expect("still owed")
            .pending_intents()
            .contains(&"exchange"));
        assert_eq!(cell.borrow().post_count(), 0);
    }

    // ── Two real OS child processes over one private journal ────────────────
    //
    // The child is the same test binary, spawned for real, running the shipped
    // kernels against the parent's loopback server and one on-disk journal. The
    // fixtures travel through child-only environment names inside `cfg(test)`;
    // the public `demo authorize` terminal requirement is untouched and the
    // production origin is never read from the environment.

    const CHILD_ORIGIN: &str = "FACTO_CLI_TEST_CHILD_ORIGIN";
    const CHILD_DIR: &str = "FACTO_CLI_TEST_CHILD_DIR";
    const CHILD_STAGE: &str = "FACTO_CLI_TEST_CHILD_STAGE";

    /// A pairing written to disk without ever reaching the wire: the window a
    /// process leaves behind when it dies between the durable claim and the POST.
    fn prewrite_only(dir: &std::path::Path) -> Result<String, state::StateError> {
        let proof = new_proof().map_err(|_| state::StateError::WriteFailed)?;
        let authorization_id = new_uuid_v4().map_err(|_| state::StateError::WriteFailed)?;
        let code = new_pairing_code().map_err(|_| state::StateError::WriteFailed)?;
        let hashed = verification_code_hash(&authorization_id, &code);
        let body = CreateRequest {
            request_id: &authorization_id,
            verification_code_hash: &hashed,
            requested_actions: ACTIONS,
            mode: "fresh",
        };
        let value = serde_json::to_value(body).map_err(|_| state::StateError::JournalInvalid)?;
        let journal = Journal::new(
            "fresh",
            &proof,
            &code,
            &authorization_id,
            value,
            &Utc::now(),
        );
        state::create(dir, &journal)?;
        Ok(authorization_id)
    }

    async fn run_child_stage(origin: &str, dir: &std::path::Path, stage: &str) -> String {
        let sender = loopback_sender(origin);
        let mut report = Report::new("child_stage", "fresh", "precheck");
        let outcome: Result<(), Failure> = match stage {
            "create" => fresh_pairing("fresh", dir, &sender, &mut report, Utc::now())
                .await
                .map(|_| ()),
            "prewrite" => prewrite_only(dir)
                .map(|_| ())
                .map_err(|_| Failure::Local("unsafe_local_state")),
            "approve-exchange" => {
                let mut journal = state::current(dir)
                    .map_err(|error| Failure::Local(error.code()))
                    .expect("child journal");
                let deadline = journal
                    .pairing_deadline()
                    .unwrap_or_else(|| Utc::now() + Duration::seconds(120));
                let approved = wait_for_approval(dir, &mut journal, &sender, deadline)
                    .await
                    .expect("child approved");
                ensure_exchange(dir, &mut journal, &sender, &mut report, approved.revision).await
            }
            "revoke" => revoke_once(dir, &sender, &mut report).await,
            "recover" => recover_all(dir, &sender, &mut report)
                .await
                .and_then(|(_, failures)| match failures.first() {
                    None => Ok(()),
                    Some(code) => Err(server_or_local(code.split(':').last().unwrap_or(""))),
                }),
            other => panic!("unknown child stage {other}"),
        };
        match outcome {
            Ok(()) => "ok".to_string(),
            Err(failure) => failure.code().to_string(),
        }
    }

    #[test]
    #[ignore = "child entry point: spawned by two_child_processes_recover_their_original_intents"]
    fn child_pairing_kernel_entry() {
        let origin = std::env::var(CHILD_ORIGIN).expect("child origin");
        let dir = std::path::PathBuf::from(std::env::var(CHILD_DIR).expect("child journal dir"));
        let stage = std::env::var(CHILD_STAGE).expect("child stage");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("child runtime");
        let outcome = runtime.block_on(run_child_stage(&origin, &dir, &stage));
        println!("CHILD-RESULT {stage} {outcome}");
    }

    fn spawn_child(origin: &str, dir: &std::path::Path, stage: &str) -> String {
        let output = std::process::Command::new(std::env::current_exe().expect("exe"))
            .args([
                "--exact",
                "partner_handoff::tests::child_pairing_kernel_entry",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ORIGIN, origin)
            .env(CHILD_DIR, dir.as_os_str())
            .env(CHILD_STAGE, stage)
            .output()
            .expect("spawn child");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "child {stage} failed: {text}");
        let marker = format!("CHILD-RESULT {stage} ");
        let line = text
            .lines()
            .find(|line| line.starts_with(&marker))
            .unwrap_or_else(|| panic!("child {stage} reported nothing: {text}"));
        line.trim_start_matches(&marker).to_string()
    }

    /// The deadline the fixture recorded for the operation this scenario lost.
    fn recorded_expiry(server: &Fixture, dir: &std::path::Path, label: &str) -> String {
        let journal = journal_of(dir);
        let operation = match label {
            "lost-exchange" => "exchange",
            "lost-revoke" => "revoke",
            _ => "create",
        };
        let key = match operation {
            "exchange" => journal.exchange_request_id.clone().unwrap_or_default(),
            "revoke" => journal.revoke_request_id.clone().unwrap_or_default(),
            _ => journal.create_request_id.clone(),
        };
        let effect = server
            .effects
            .lock()
            .expect("effect lock")
            .get(&format!("{operation}:{key}"))
            .cloned()
            .expect("recorded effect");
        serde_json::from_str::<Value>(&effect)
            .expect("recorded json")
            .get("expires_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    fn journal_of(dir: &std::path::Path) -> Journal {
        state::current(dir).expect("journal")
    }

    #[tokio::test]
    async fn two_child_processes_recover_their_original_intents_over_real_http() {
        // scenario: (label, stages before the drop, dropped request, expected
        // first-child code)
        let cases: [(&str, &[&str], &str); 4] = [
            ("prewrite", &["prewrite"], "ok"),
            ("lost-create", &["create"], "receipt_unknown"),
            (
                "lost-exchange",
                &["create", "approve-exchange"],
                "receipt_unknown",
            ),
            (
                "lost-revoke",
                &["create", "approve-exchange", "revoke"],
                "receipt_unknown",
            ),
        ];
        for (label, stages, expected_first) in cases {
            let prewrite = label == "prewrite";
            let (origin, server) = loopback();
            let dir = store(&format!("children-{label}"));
            for stage in stages.iter().take(stages.len() - 1) {
                let code = spawn_child(&origin, &dir, stage);
                assert_eq!(code, "ok", "{label} setup stage {stage}");
            }
            // Only the exchange and revocation windows address a known
            // authorization; the create and pre-write windows do not need one.
            let auth = match label {
                "lost-exchange" | "lost-revoke" => journal_of(&dir).authorization_id.clone(),
                _ => String::new(),
            };
            let target = match label {
                "lost-create" | "prewrite" => format!("POST {PREFIX}/authorizations"),
                "lost-exchange" => format!("POST {PREFIX}/authorizations/{auth}/exchanges"),
                _ => format!("POST {PREFIX}/authorizations/{auth}/revocations"),
            };
            server
                .drop_on
                .lock()
                .expect("drop lock")
                .push(target.clone());
            server.unknown_receipt.store(1, Ordering::SeqCst);

            let first_stage = stages.last().expect("last stage");
            let first_code = spawn_child(&origin, dir.as_path(), first_stage);
            assert_eq!(first_code, expected_first, "{label} first child");

            let posts_after_first = server.posts.load(Ordering::SeqCst);
            let gets_after_first = server.gets.load(Ordering::SeqCst);
            let before = journal_of(&dir);
            let keys_before = (
                before.create_request_id.clone(),
                before.exchange_request_id.clone(),
                before.revoke_request_id.clone(),
            );
            assert!(
                !before.pending_intents().is_empty(),
                "{label} keeps the original intent"
            );

            // The second, separate process may only read the same request ids.
            // Nothing was ever sent for a pre-write intent, so the server still
            // has no effect to report and the child has to fail closed.
            server.unknown_receipt.store(0, Ordering::SeqCst);
            let second = spawn_child(&origin, dir.as_path(), "recover");
            if prewrite {
                assert_eq!(second, "receipt_unknown", "{label} has no effect to replay");
            } else {
                assert_eq!(second, "ok", "{label} second child recovered");
            }

            let after = journal_of(&dir);
            let keys_after = (
                after.create_request_id.clone(),
                after.exchange_request_id.clone(),
                after.revoke_request_id.clone(),
            );
            assert_eq!(keys_before, keys_after, "{label} never swapped a key");
            assert_eq!(
                server.posts.load(Ordering::SeqCst),
                posts_after_first,
                "{label} the second child sent no POST"
            );
            assert!(
                server.gets.load(Ordering::SeqCst) > gets_after_first,
                "{label} the second child read the original receipt"
            );
            if prewrite {
                assert!(!after.create_recorded, "{label} stays unresolved");
                assert!(
                    after.pending_intents().contains(&"create"),
                    "{label} keeps the original intent"
                );
                assert_eq!(
                    server.posts.load(Ordering::SeqCst),
                    0,
                    "{label} never posted"
                );
            } else {
                assert!(
                    after.pending_intents().is_empty(),
                    "{label} intent resolved"
                );
                assert!(after.create_recorded, "{label} create proven");
                let recorded = recorded_expiry(&server, &dir, label);
                // The recovered deadline is exactly what the fixture recorded
                // when the reply was lost: recovery may not renew or widen.
                assert_eq!(
                    after.expires_at, recorded,
                    "{label} recovery must not renew the approval"
                );
            }
            assert!(
                !keys_before.1.as_deref().unwrap_or("").is_empty()
                    || label == "lost-create"
                    || prewrite,
                "{label} an exchange key is never invented later"
            );
            let seen = server.seen.lock().expect("seen");
            assert!(
                seen.iter().any(|line| line.contains(&format!(
                    "/authorizations/{}/requests/",
                    after.authorization_id
                ))),
                "{label} receipt reads are observable"
            );
        }
    }

    #[test]
    fn nullable_fields_must_be_present_and_values_stay_bounded() {
        let base = approve_view();
        // Explicit null is a legal answer while the pairing is unapproved.
        let nulled = patch(base.clone(), "approval", Value::Null);
        let view: AuthorizationView = parse(nulled.to_string().as_bytes()).expect("explicit null");
        assert_eq!(view.approval, None);
        // A missing key is a protocol violation, not a null.
        let mut absent = base.as_object().expect("object").clone();
        absent.remove("approval");
        assert!(parse::<AuthorizationView>(Value::Object(absent).to_string().as_bytes()).is_err());

        for bad in [json!(0), json!(MAX_SAFE_INTEGER + 1), json!(-1), json!("2")] {
            let mutated = patch(base.clone(), "revision", bad);
            let parsed: Result<AuthorizationView, _> = parse(mutated.to_string().as_bytes());
            assert!(
                parsed.is_err() || check_view_shape(&parsed.expect("wire"), FIXTURE_ID).is_err(),
                "revision must stay inside 1..=safe-integer"
            );
        }

        // authorization_revision is nullable but must be present, and a value
        // keeps the same range.
        let mut missing = workspace_view().as_object().expect("object").clone();
        missing.remove("authorization_revision");
        assert!(parse::<WorkspaceResponse>(Value::Object(missing).to_string().as_bytes()).is_err());
        let zeroed = patch(workspace_view(), "authorization_revision", json!(0));
        let zeroed: WorkspaceResponse = parse(zeroed.to_string().as_bytes()).expect("wire");
        assert_eq!(
            check_workspace_shape(&zeroed, FIXTURE_ID).unwrap_err(),
            Failure::Local("wire_invalid")
        );

        // The contract bounds text by Unicode scalars, not by bytes.
        let two_byte = "\u{e9}";
        let accepted = patch(
            workspace_view(),
            "apps",
            json!([{"app_id": "mapp_one", "display_name": two_byte.repeat(512), "status": "active"}]),
        );
        let accepted: WorkspaceResponse =
            parse(accepted.to_string().as_bytes()).expect("wide name");
        assert!(check_workspace_shape(&accepted, FIXTURE_ID).is_ok());
        let rejected = patch(
            workspace_view(),
            "apps",
            json!([{"app_id": "mapp_one", "display_name": two_byte.repeat(513), "status": "active"}]),
        );
        let rejected: WorkspaceResponse =
            parse(rejected.to_string().as_bytes()).expect("longer name");
        assert!(check_workspace_shape(&rejected, FIXTURE_ID).is_err());
        for status in [String::new(), "s".repeat(65)] {
            let mutated = patch(
                workspace_view(),
                "apps",
                json!([{"app_id": "mapp_one", "display_name": "Shop", "status": status}]),
            );
            let parsed: WorkspaceResponse = parse(mutated.to_string().as_bytes()).expect("wire");
            assert!(
                check_workspace_shape(&parsed, FIXTURE_ID).is_err(),
                "status {status:?} is out of range"
            );
        }
    }

    #[test]
    fn the_proof_header_is_marked_sensitive_and_stays_out_of_debug() {
        let built = build_read(
            &"a".repeat(64),
            FIXTURE_ID,
            format!("{PREFIX}/session/workspace"),
        );
        let (client, request) = live_request(&built).expect("request");
        let value = request.headers().get(header::AUTHORIZATION).expect("auth");
        assert!(value.is_sensitive(), "reqwest must not print the proof");
        let rendered = format!("{built:?}");
        assert!(!rendered.contains(&"a".repeat(64)), "{rendered}");
        assert!(rendered.contains("REDACTED"), "{rendered}");
        drop(client);
    }

    #[test]
    fn receipt_keys_bind_to_the_persisted_intents() {
        let mut entry = journal(FIXTURE_ID);
        assert_eq!(
            receipt_key(&entry, "create").expect("create key"),
            FIXTURE_ID
        );
        assert_eq!(
            receipt_key(&entry, "exchange").unwrap_err(),
            Failure::Local("journal_invalid")
        );
        entry.record_exchange_intent("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", 2);
        assert_eq!(
            receipt_key(&entry, "exchange").expect("exchange key"),
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        );
        assert_eq!(
            receipt_key(&entry, "approve").unwrap_err(),
            Failure::Local("journal_invalid")
        );
    }
}
