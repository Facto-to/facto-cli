//! Partner CLI Test handoff: CSPRNG possession proof, fixed-origin wire,
//! finite polling, operation receipts and the current-authorization read.
//!
//! This module never reads Consumer credentials or configuration, never sends
//! a cookie, and never accepts an App secret. Proof and pairing code live only
//! in the private journal and on the interactive terminal.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use reqwest::header::{self, HeaderName, HeaderValue};
use reqwest::{redirect, Client, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::time::Duration as StdDuration;

use crate::partner_handoff_state::{self as state, Journal};

const API_ORIGIN: &str = "https://test-api.facto.to";
const APPROVAL_ORIGIN: &str = "https://test-partner.facto.to";
const PREFIX: &str = "/api/partner-cli";
const MATCHING_PREFIX: &str = "facto:partner-cli:matching:v1";
const REALM: &str = "test";
const ALPHABET: [u8; 32] = *b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LENGTH: usize = 10;
const PROOF_BYTES: usize = 32;
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
const ACTIONS: [&str; 2] = ["workspace.access.read", "merchant.apps.read"];

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
const RECEIPT_OPERATIONS: [&str; 4] = ["create", "approve", "exchange", "revoke"];

// ── Failures ────────────────────────────────────────────────────────────────

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

// ── Entropy and identifiers ─────────────────────────────────────────────────

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
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for index in [8usize, 13, 18, 23] {
        if bytes[index] != b'-' {
            return false;
        }
    }
    bytes.iter().enumerate().all(|(index, byte)| {
        matches!(index, 8 | 13 | 18 | 23) || (byte.is_ascii_hexdigit() && byte.is_ascii_lowercase())
    })
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
    let trimmed = input.trim_matches(|c: char| c.is_ascii_whitespace());
    if !trimmed.is_ascii() {
        return None;
    }
    let upper = trimmed.to_ascii_uppercase();
    let candidate = match upper.len() {
        11 if upper.as_bytes()[5] == b'-' => format!("{}{}", &upper[..5], &upper[6..]),
        10 => upper,
        _ => return None,
    };
    if candidate.len() != CODE_LENGTH || !candidate.bytes().all(|byte| ALPHABET.contains(&byte)) {
        return None;
    }
    Some(candidate)
}

fn matching_canonical_bytes(authorization_id: &str, normalized: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MATCHING_PREFIX.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(REALM.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(authorization_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(normalized.as_bytes());
    bytes
}

fn verification_code_hash(authorization_id: &str, normalized: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(matching_canonical_bytes(authorization_id, normalized));
    hex::encode(hasher.finalize())
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
    response: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ErrorWire {
    code: String,
    message: String,
}

// ── Wire validation ─────────────────────────────────────────────────────────

fn require(value: &str, expected: &str) -> Result<(), Failure> {
    if value == expected {
        Ok(())
    } else {
        Err(Failure::Local("wire_invalid"))
    }
}

fn require_test(value: &str) -> Result<(), Failure> {
    require(value, "test")
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

fn require_phase(status: &str) -> Result<(), Failure> {
    if PHASES.contains(&status) {
        Ok(())
    } else {
        Err(Failure::Local("wire_invalid"))
    }
}

fn require_uuid(value: &str) -> Result<(), Failure> {
    if is_uuid(value) {
        Ok(())
    } else {
        Err(Failure::Local("wire_invalid"))
    }
}

fn parse_deadline(value: &str) -> Result<DateTime<Utc>, Failure> {
    DateTime::parse_from_rfc3339(value)
        .map(|moment| moment.with_timezone(&Utc))
        .map_err(|_| Failure::Local("wire_invalid"))
}

fn approval_page_uri(authorization_id: &str) -> String {
    format!("{APPROVAL_ORIGIN}/cli/approve?authorization_id={authorization_id}")
}

fn server_code(code: &str) -> Result<Failure, Failure> {
    SERVER_ERROR_CODES
        .iter()
        .find(|known| **known == code)
        .copied()
        .map(Failure::Server)
        .ok_or(Failure::Local("wire_invalid"))
}

// ── Transport ───────────────────────────────────────────────────────────────

struct Wire {
    status: StatusCode,
    body: Vec<u8>,
}

fn client() -> Result<Client, Failure> {
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| Failure::Local("transport_unavailable"))
}

/// The dedicated proof scheme and locator only. An empty request id means the
/// operation has no idempotency key (reads), so the header is absent.
fn proof_headers(
    proof: &str,
    request_id: &str,
    locator: &str,
) -> Result<Vec<(HeaderName, HeaderValue)>, Failure> {
    let valid_proof = proof.len() == 64
        && proof
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if !valid_proof {
        return Err(Failure::Local("unsafe_local_state"));
    }
    if !request_id.is_empty() {
        require_uuid(request_id)?;
    }
    require_uuid(locator)?;
    let mut headers = vec![
        (
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("{PROOF_SCHEME}{proof}"))
                .map_err(|_| Failure::Local("wire_invalid"))?,
        ),
        (
            HeaderName::from_static("x-facto-cli-request-id"),
            HeaderValue::from_str(locator).map_err(|_| Failure::Local("wire_invalid"))?,
        ),
    ];
    if !request_id.is_empty() {
        headers.push((
            HeaderName::from_static("idempotency-key"),
            HeaderValue::from_str(request_id).map_err(|_| Failure::Local("wire_invalid"))?,
        ));
    }
    Ok(headers)
}

fn target_header() -> (HeaderName, HeaderValue) {
    (
        HeaderName::from_static("x-facto-target"),
        HeaderValue::from_static("test"),
    )
}

fn with_headers(
    builder: RequestBuilder,
    headers: Vec<(HeaderName, HeaderValue)>,
) -> RequestBuilder {
    headers.into_iter().fold(builder, |builder, (name, value)| {
        builder.header(name, value)
    })
}

async fn send(builder: RequestBuilder) -> Result<Wire, Failure> {
    let handle = client()?;
    let request = builder
        .build()
        .map_err(|_| Failure::Local("transport_unavailable"))?;
    let mut response = handle
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
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(Failure::Local("response_too_large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Wire { status, body })
}

async fn post_json<T: Serialize>(
    proof: &str,
    request_id: &str,
    locator: &str,
    path: &str,
    body: &T,
    expect: StatusCode,
) -> Result<Vec<u8>, Failure> {
    let headers = proof_headers(proof, request_id, locator)?
        .into_iter()
        .chain([target_header()])
        .collect();
    let builder = with_headers(client()?.post(format!("{API_ORIGIN}{path}")), headers)
        .header(header::CONTENT_TYPE, "application/json")
        .json(body);
    let wire = send(builder).await?;
    decode(wire, expect)
}

async fn get_proof(proof: &str, locator: &str, path: &str) -> Result<Vec<u8>, Failure> {
    let headers = proof_headers(proof, "", locator)?;
    let wire = send(with_headers(
        client()?.get(format!("{API_ORIGIN}{path}")),
        headers,
    ))
    .await?;
    decode(wire, StatusCode::OK)
}

fn decode(wire: Wire, expect: StatusCode) -> Result<Vec<u8>, Failure> {
    if wire.status == expect {
        return Ok(wire.body);
    }
    if let Ok(error) = serde_json::from_slice::<ErrorWire>(&wire.body) {
        if error.message.len() <= 512 {
            return Err(server_code(&error.code)?);
        }
    }
    Err(Failure::Local("http_rejected"))
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Failure> {
    serde_json::from_slice(body).map_err(|_| Failure::Local("wire_invalid"))
}

// ── Response validation against the contract ────────────────────────────────

fn validate_create(view: CreateResponse, request_id: &str) -> Result<CreateResponse, Failure> {
    require(&view.status, "PENDING")?;
    require_uuid(&view.authorization_id).and_then(|_| require_uuid(&view.request_id))?;
    if view.authorization_id != request_id || view.request_id != request_id {
        return Err(Failure::Local("wire_invalid"));
    }
    if view.revision != 1 || view.poll_interval_seconds != POLL_SECONDS {
        return Err(Failure::Local("wire_invalid"));
    }
    require_test(&view.target_environment).and_then(|_| require(&view.realm, REALM))?;
    if view.verification_uri != approval_page_uri(&view.authorization_id) {
        return Err(Failure::Local("wire_invalid"));
    }
    let now = Utc::now();
    let deadline = parse_deadline(&view.expires_at)?;
    if deadline <= now || deadline > now + Duration::seconds(PENDING_LIMIT + CLOCK_MARGIN) {
        return Err(Failure::Local("wire_invalid"));
    }
    Ok(view)
}

fn validate_snapshot(snapshot: &ApprovalSnapshot) -> Result<(), Failure> {
    require(&snapshot.environment, "dev")
        .and_then(|_| require_actions(&snapshot.granted_actions))?;
    for field in [
        &snapshot.partner_user_id,
        &snapshot.organization_id,
        &snapshot.membership_id,
        &snapshot.partner_role_id,
        &snapshot.context_id,
    ] {
        require_uuid(field)?;
    }
    let apps_ok = snapshot.authorized_app_ids.len() <= 100
        && snapshot
            .authorized_app_ids
            .iter()
            .all(|id| !id.is_empty() && id.len() <= 128);
    if !apps_ok {
        return Err(Failure::Local("wire_invalid"));
    }
    parse_deadline(&snapshot.expires_at)?;
    Ok(())
}

fn validate_view(
    view: AuthorizationView,
    authorization_id: &str,
) -> Result<AuthorizationView, Failure> {
    require_uuid(&view.authorization_id)?;
    if view.authorization_id != authorization_id {
        return Err(Failure::Local("wire_invalid"));
    }
    require_test(&view.target_environment).and_then(|_| require(&view.realm, REALM))?;
    require_phase(&view.status)?;
    if view.revision == 0 {
        return Err(Failure::Local("wire_invalid"));
    }
    parse_deadline(&view.expires_at)?;
    if let Some(snapshot) = &view.approval {
        validate_snapshot(snapshot)?;
    }
    Ok(view)
}

fn validate_exchange(
    response: ExchangeResponse,
    authorization_id: &str,
    request_id: &str,
) -> Result<ExchangeResponse, Failure> {
    require(&response.operation, "partner_cli.exchange")?;
    require(&response.status, "ACTIVE")?;
    require_uuid(&response.session_id).and_then(|_| require_uuid(&response.request_id))?;
    if response.authorization_id != authorization_id || response.request_id != request_id {
        return Err(Failure::Local("wire_invalid"));
    }
    if response.revision <= response.previous_revision || response.previous_revision == 0 {
        return Err(Failure::Local("wire_invalid"));
    }
    let snapshot = response
        .approval
        .as_ref()
        .ok_or(Failure::Local("wire_invalid"))?;
    validate_snapshot(snapshot)?;
    let now = Utc::now();
    let deadline = parse_deadline(&response.expires_at)?;
    if deadline <= now {
        return Err(Failure::Local("cli_expired"));
    }
    let bounded = deadline <= now + Duration::seconds(ACTIVE_LIMIT + CLOCK_MARGIN)
        && deadline <= now + Duration::seconds(ABSOLUTE_PARENT_LIMIT + CLOCK_MARGIN);
    if !bounded {
        return Err(Failure::Local("wire_invalid"));
    }
    Ok(response)
}

fn validate_revocation(
    response: RevocationResponse,
    authorization_id: &str,
    request_id: &str,
) -> Result<RevocationResponse, Failure> {
    require(&response.operation, "partner_cli.revoke")?;
    require(&response.status, "REVOKED")?;
    if response.authorization_id != authorization_id || response.request_id != request_id {
        return Err(Failure::Local("wire_invalid"));
    }
    if response.revision <= response.previous_revision {
        return Err(Failure::Local("wire_invalid"));
    }
    if let Some(snapshot) = &response.approval {
        validate_snapshot(snapshot)?;
    }
    Ok(response)
}

fn validate_workspace(
    workspace: WorkspaceResponse,
    journal: &Journal,
    snapshot: &ApprovalSnapshot,
) -> Result<WorkspaceResponse, Failure> {
    require(&workspace.environment, "dev")
        .and_then(|_| require_test(&workspace.target_environment))
        .and_then(|_| require(&workspace.realm, REALM))
        .and_then(|_| require_actions(&workspace.granted_actions))?;
    require_uuid(&workspace.session_id)?;
    if workspace.session_id != journal.authorization_id {
        return Err(Failure::Local("wire_invalid"));
    }
    let actors_match = workspace.partner_user_id == snapshot.partner_user_id
        && workspace.organization_id == snapshot.organization_id
        && workspace.membership_id == snapshot.membership_id
        && workspace.partner_role_id == snapshot.partner_role_id;
    if !actors_match {
        return Err(Failure::Local("cli_scope_changed"));
    }
    if workspace.authorized_app_ids.len() > 100 {
        return Err(Failure::Local("wire_invalid"));
    }
    // The current read may never be wider than the approved ceiling.
    let within_ceiling = workspace
        .authorized_app_ids
        .iter()
        .all(|id| snapshot.authorized_app_ids.contains(id))
        && workspace.apps.iter().all(|app| {
            !app.app_id.is_empty() && workspace.authorized_app_ids.contains(&app.app_id)
        });
    if !within_ceiling {
        return Err(Failure::Local("cli_scope_changed"));
    }
    if parse_deadline(&workspace.expires_at)? <= Utc::now() {
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
    if receipt.request_id != request_id || !RECEIPT_OPERATIONS.contains(&receipt.operation.as_str())
    {
        return Err(Failure::Local("wire_invalid"));
    }
    if receipt.operation != operation {
        return Err(Failure::Local("wire_invalid"));
    }
    match (receipt.status.as_str(), receipt.response) {
        ("UNKNOWN", None) => Ok(None),
        ("SUCCEEDED", Some(value)) => Ok(Some(value)),
        _ => Err(Failure::Local("wire_invalid")),
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
        text.push_str(&format!(
            "Authentication verified: {}. Business UAT: NOT_RUN. No App secret was read, sent or stored.\n",
            self.authentication_verified
        ));
        text
    }
}

fn emit(mut report: Report, outcome: Result<(), Failure>, terse: bool) -> Result<()> {
    if let Err(failure) = outcome {
        report = report.failed(&failure);
        println!("{}", report.render(terse));
        return Err(anyhow::anyhow!(failure.code()));
    }
    println!("{}", report.succeeded().render(terse));
    Ok(())
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
    state::write(dir, journal).map_err(|error| Failure::Local(error.code()))
}

fn load_current() -> Result<(std::path::PathBuf, Journal), Failure> {
    let dir = open_store()?;
    let journal = state::current(&dir).map_err(|error| Failure::Local(error.code()))?;
    Ok((dir, journal))
}

async fn read_authorization(journal: &Journal) -> Result<AuthorizationView, Failure> {
    let path = format!("{PREFIX}/authorizations/{}", journal.authorization_id);
    let body = get_proof(&journal.proof_hex, &journal.authorization_id, &path).await?;
    validate_view(
        parse::<AuthorizationView>(&body)?,
        &journal.authorization_id,
    )
}

async fn read_receipt(
    journal: &Journal,
    operation: &str,
    request_id: &str,
) -> Result<Option<serde_json::Value>, Failure> {
    let path = format!(
        "{}/authorizations/{}/requests/{operation}/{request_id}",
        PREFIX, journal.authorization_id
    );
    let body = get_proof(&journal.proof_hex, &journal.authorization_id, &path).await?;
    decode_receipt(&body, operation, request_id)
}

async fn workspace_read(
    journal: &Journal,
    snapshot: &ApprovalSnapshot,
) -> Result<WorkspaceResponse, Failure> {
    let body = get_proof(
        &journal.proof_hex,
        &journal.authorization_id,
        &format!("{PREFIX}/session/workspace"),
    )
    .await?;
    let workspace: WorkspaceResponse = parse(&body)?;
    validate_workspace(workspace, journal, snapshot)
}

fn announce_pairing(uri: &str, code: &str, terse: bool) {
    // The pairing code is never part of the JSON report; terse mode keeps it on
    // the terminal error stream so machine output stays credential-free.
    if terse {
        eprintln!("Pairing code for {uri}: {code}");
    } else {
        println!("Open the approval page: {uri}");
        println!("Pairing code to type on the Partner page: {code}");
    }
}

async fn wait_for_approval(
    dir: &std::path::Path,
    journal: &mut Journal,
    deadline: DateTime<Utc>,
) -> Result<AuthorizationView, Failure> {
    let mut transport_failures = 0_u32;
    loop {
        if Utc::now() >= deadline {
            journal.touch(&Utc::now(), "EXPIRED", journal.revision, "");
            let _ = persist(dir, journal);
            return Err(Failure::Local("cli_expired"));
        }
        match read_authorization(journal).await {
            Ok(view) => {
                transport_failures = 0;
                journal.touch(&Utc::now(), &view.status, view.revision, &view.expires_at);
                let _ = persist(dir, journal);
                match view.status.as_str() {
                    "APPROVED" => return Ok(view),
                    "PENDING" => {}
                    "DENIED" => return Err(Failure::Local("cli_denied")),
                    "EXPIRED" => return Err(Failure::Local("cli_expired")),
                    "REVOKED" => return Err(Failure::Local("cli_revoked")),
                    // Already exchanged by another run of the same pairing.
                    "ACTIVE" => return Err(Failure::Local("cli_already_exchanged")),
                    other => require_phase(other)?,
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

// ── Commands ────────────────────────────────────────────────────────────────

pub(crate) async fn authorize(target: &str, mode: &str, terse: bool) -> Result<()> {
    let mut report = Report::new("partner_cli_authorize", mode, "precheck");
    let outcome = authorize_run(target, mode, &mut report).await;
    emit(report, outcome, terse)
}

async fn authorize_run(target: &str, mode: &str, report: &mut Report) -> Result<(), Failure> {
    reject_target(target)?;
    let dir = open_store()?;
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
    let mut journal = Journal::new(
        mode,
        &proof,
        &authorization_id,
        serde_json::to_value(&body).map_err(|_| Failure::Local("unsafe_local_state"))?,
        &Utc::now(),
    );
    // The pairing must be durable before the single create POST is issued.
    persist(&dir, &journal)?;

    report.stage = "create";
    report.authorization_id = Some(authorization_id.clone());
    report.verification_uri = Some(approval_page_uri(&authorization_id));
    let path = format!("{PREFIX}/authorizations");
    let created = match post_json(
        &proof,
        &authorization_id,
        &authorization_id,
        &path,
        &body,
        StatusCode::CREATED,
    )
    .await
    {
        Ok(bytes) => validate_create(parse::<CreateResponse>(&bytes)?, &authorization_id)?,
        Err(failure) if failure == Failure::Local("transport_unavailable") => {
            journal.mark_unknown("create");
            journal.touch(&Utc::now(), "UNKNOWN", journal.revision, "");
            persist(&dir, &journal)?;
            return Err(recover_create(&dir, &mut journal, &authorization_id).await);
        }
        Err(failure) => return Err(failure),
    };

    journal.create_recorded = true;
    journal.touch(
        &Utc::now().to_rfc3339(),
        &created.status,
        created.revision,
        &created.expires_at,
    );
    persist(&dir, &journal)?;

    report.stage = "pending";
    report.phase = Some(created.status.clone());
    report.revision = Some(created.revision);
    report.expires_at = Some(created.expires_at.clone());
    announce_pairing(
        &created.verification_uri,
        &display_pairing_code(&normalized),
        false,
    );
    let _ = open::that(&created.verification_uri);

    let deadline = parse_deadline(&created.expires_at)?;
    let view = wait_for_approval(&dir, &mut journal, deadline).await?;
    let snapshot = view.approval.ok_or(Failure::Local("wire_invalid"))?;

    report.stage = "exchange";
    let exchange_request_id = new_uuid_v4()?;
    journal.exchange_request_id = Some(exchange_request_id.clone());
    persist(&dir, &journal)?;
    let exchange_body = RevisionRequest {
        request_id: &exchange_request_id,
        expected_revision: view.revision,
    };
    let exchange_path = format!("{PREFIX}/authorizations/{authorization_id}/exchanges");
    let exchanged = match post_json(
        &proof,
        &exchange_request_id,
        &authorization_id,
        &exchange_path,
        &exchange_body,
        StatusCode::OK,
    )
    .await
    {
        Ok(bytes) => validate_exchange(
            parse::<ExchangeResponse>(&bytes)?,
            &authorization_id,
            &exchange_request_id,
        )?,
        Err(failure) if failure == Failure::Local("transport_unavailable") => {
            journal.mark_unknown("exchange");
            persist(&dir, &journal)?;
            return Err(recover_exchange(&dir, &mut journal, &exchange_request_id).await);
        }
        Err(failure) => return Err(failure),
    };

    journal.exchange_recorded = true;
    journal.unknown.retain(|name| name != "exchange");
    journal.session_id = Some(exchanged.session_id.clone());
    journal.touch(
        &Utc::now().to_rfc3339(),
        &exchanged.status,
        exchanged.revision,
        &exchanged.expires_at,
    );
    persist(&dir, &journal)?;

    report.stage = "workspace";
    report.session_id = Some(exchanged.session_id.clone());
    report.phase = Some(exchanged.status.clone());
    report.revision = Some(exchanged.revision);
    report.expires_at = Some(exchanged.expires_at.clone());
    report.granted_actions = Some(snapshot.granted_actions.clone());
    report.authorized_app_ids = Some(snapshot.authorized_app_ids.clone());
    let workspace = workspace_read(&journal, &snapshot).await?;
    report.apps = Some(workspace.apps.clone());
    report.authentication_verified = true;
    Ok(())
}

async fn recover_create(
    dir: &std::path::Path,
    journal: &mut Journal,
    authorization_id: &str,
) -> Failure {
    match read_receipt(journal, "create", authorization_id).await {
        Ok(Some(value)) => match serde_json::from_value::<CreateResponse>(value) {
            Ok(created) if validate_create(created, authorization_id).is_ok() => {
                journal.create_recorded = true;
                journal.unknown.retain(|name| name != "create");
                journal.touch(&Utc::now(), "PENDING", 1, "");
                let _ = persist(dir, journal);
                Failure::Local("receipt_succeeded")
            }
            _ => Failure::Local("wire_invalid"),
        },
        Ok(None) => Failure::Local("receipt_unknown"),
        Err(failure) => failure,
    }
}

async fn recover_exchange(
    dir: &std::path::Path,
    journal: &mut Journal,
    request_id: &str,
) -> Failure {
    match read_receipt(journal, "exchange", request_id).await {
        Ok(Some(_)) => {
            journal.exchange_recorded = true;
            journal.unknown.retain(|name| name != "exchange");
            let _ = persist(dir, journal);
            Failure::Local("receipt_succeeded")
        }
        Ok(None) => Failure::Local("receipt_unknown"),
        Err(failure) => failure,
    }
}

pub(crate) async fn session(target: &str, mode: &str, terse: bool) -> Result<()> {
    let mut report = Report::new("partner_cli_session", mode, "precheck");
    let outcome = session_run(target, &mut report).await;
    emit(report, outcome, terse)
}

async fn session_run(target: &str, report: &mut Report) -> Result<(), Failure> {
    reject_target(target)?;
    let (dir, mut journal) = load_current()?;
    report.stage = "authorization";
    report.authorization_id = Some(journal.authorization_id.clone());
    report.verification_uri = Some(approval_page_uri(&journal.authorization_id));

    // Recovery reads only: no create, exchange or refresh side effect here.
    for operation in journal.unknown.clone() {
        if !RECEIPT_OPERATIONS.contains(&operation.as_str()) {
            return Err(Failure::Local("journal_invalid"));
        }
        let request_id = match operation.as_str() {
            "create" => journal.create_request_id.clone(),
            "exchange" => journal.exchange_request_id.clone(),
            "revoke" => journal.revoke_request_id.clone(),
            _ => None,
        };
        if let Some(request_id) = request_id {
            if read_receipt(&journal, &operation, &request_id)
                .await?
                .is_some()
            {
                journal.unknown.retain(|name| *name != operation);
                persist(&dir, &journal)?;
            }
        }
    }

    let view = match read_authorization(&journal).await {
        Ok(view) => view,
        // A record the server no longer returns is unknown, never a proof of
        // failure, and never triggers a new create POST.
        Err(failure) if failure == Failure::Local("http_rejected") => {
            report.phase = Some(journal.phase.clone());
            return Err(Failure::Local("receipt_unknown"));
        }
        Err(failure) => return Err(failure),
    };
    report.phase = Some(view.status.clone());
    report.revision = Some(view.revision);
    report.expires_at = Some(view.expires_at.clone());
    journal.touch(&Utc::now(), &view.status, view.revision, &view.expires_at);
    persist(&dir, &journal)?;
    if let Some(snapshot) = &view.approval {
        report.granted_actions = Some(snapshot.granted_actions.clone());
        report.authorized_app_ids = Some(snapshot.authorized_app_ids.clone());
    }
    Ok(())
}

pub(crate) async fn check_access(target: &str, mode: &str, terse: bool) -> Result<()> {
    let mut report = Report::new("partner_cli_check_access", mode, "precheck");
    let outcome = check_access_run(target, &mut report).await;
    emit(report, outcome, terse)
}

async fn check_access_run(target: &str, report: &mut Report) -> Result<(), Failure> {
    reject_target(target)?;
    let (dir, mut journal) = load_current()?;
    report.authorization_id = Some(journal.authorization_id.clone());
    // Only this explicit current read may set authentication_verified; a stored
    // ACTIVE phase or a historical receipt never does.
    let view = read_authorization(&journal).await?;
    journal.touch(&Utc::now(), &view.status, view.revision, &view.expires_at);
    persist(&dir, &journal)?;
    report.phase = Some(view.status.clone());
    report.revision = Some(view.revision);
    report.stage = "workspace";
    if view.status != "ACTIVE" {
        return Err(Failure::Local("cli_pending"));
    }
    let snapshot = view.approval.ok_or(Failure::Local("wire_invalid"))?;
    let workspace = workspace_read(&journal, &snapshot).await?;
    report.session_id = Some(workspace.session_id.clone());
    report.expires_at = Some(workspace.expires_at.clone());
    report.granted_actions = Some(workspace.granted_actions.clone());
    report.authorized_app_ids = Some(workspace.authorized_app_ids.clone());
    report.apps = Some(workspace.apps.clone());
    report.authentication_verified = true;
    Ok(())
}

pub(crate) async fn logout(target: &str, mode: &str, terse: bool) -> Result<()> {
    let mut report = Report::new("partner_cli_logout", mode, "precheck");
    let outcome = logout_run(target, &mut report).await;
    emit(report, outcome, terse)
}

async fn logout_run(target: &str, report: &mut Report) -> Result<(), Failure> {
    reject_target(target)?;
    let (dir, mut journal) = load_current()?;
    report.authorization_id = Some(journal.authorization_id.clone());
    report.phase = Some(journal.phase.clone());
    report.stage = "revocation";
    let revoke_request_id = match journal.revoke_request_id.clone() {
        Some(existing) => existing,
        None => {
            let fresh = new_uuid_v4()?;
            journal.revoke_request_id = Some(fresh.clone());
            persist(&dir, &journal)?;
            fresh
        }
    };
    let body = RevisionRequest {
        request_id: &revoke_request_id,
        expected_revision: journal.revision,
    };
    let path = format!(
        "{}/authorizations/{}/revocations",
        PREFIX, journal.authorization_id
    );
    match post_json(
        &journal.proof_hex,
        &revoke_request_id,
        &journal.authorization_id,
        &path,
        &body,
        StatusCode::OK,
    )
    .await
    {
        Ok(bytes) => {
            let response = validate_revocation(
                parse::<RevocationResponse>(&bytes)?,
                &journal.authorization_id,
                &revoke_request_id,
            )?;
            journal.revoke_recorded = true;
            journal.unknown.retain(|name| name != "revoke");
            journal.touch(&Utc::now(), &response.status, response.revision, "");
            persist(&dir, &journal)?;
            report.phase = Some(response.status.clone());
            report.revision = Some(response.revision);
        }
        // An unknown revocation keeps the recovery journal and never resends
        // under a different key.
        Err(failure) if failure == Failure::Local("transport_unavailable") => {
            journal.mark_unknown("revoke");
            persist(&dir, &journal)?;
            return Err(Failure::Local("receipt_unknown"));
        }
        Err(Failure::Server("cli_revoked")) => {
            journal.touch(&Utc::now(), "REVOKED", journal.revision, "");
            persist(&dir, &journal)?;
            report.phase = Some("REVOKED".to_string());
        }
        Err(failure) => return Err(failure),
    }
    Ok(())
}

// ── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    const FIXTURE_ID: &str = "11111111-1111-4111-8111-111111111111";

    fn snapshot(app_ids: &[&str]) -> ApprovalSnapshot {
        ApprovalSnapshot {
            partner_user_id: "22222222-2222-4222-8222-222222222222".into(),
            organization_id: "33333333-3333-4333-8333-333333333333".into(),
            membership_id: "44444444-4444-4444-8444-444444444444".into(),
            partner_role_id: "55555555-5555-4555-8555-555555555555".into(),
            context_id: "66666666-6666-4666-8666-666666666666".into(),
            environment: "dev".into(),
            authorization_revision: Some(2),
            granted_actions: ACTIONS.iter().map(|action| action.to_string()).collect(),
            authorized_app_ids: app_ids.iter().map(|id| id.to_string()).collect(),
            expires_at: "2026-10-13T00:20:00Z".into(),
        }
    }

    fn journal_for(authorization_id: &str) -> Journal {
        Journal::new(
            "reuse",
            &"a".repeat(64),
            authorization_id,
            serde_json::json!({}),
            &Utc::now(),
        )
    }

    fn created_response() -> CreateResponse {
        CreateResponse {
            authorization_id: FIXTURE_ID.into(),
            request_id: FIXTURE_ID.into(),
            target_environment: "test".into(),
            realm: "test".into(),
            status: "PENDING".into(),
            revision: 1,
            expires_at: (Utc::now() + Duration::seconds(300)).to_rfc3339(),
            poll_interval_seconds: 2,
            verification_uri: approval_page_uri(FIXTURE_ID),
        }
    }

    fn workspace_base() -> WorkspaceResponse {
        let approval = snapshot(&["mapp_one"]);
        WorkspaceResponse {
            session_id: FIXTURE_ID.into(),
            target_environment: "test".into(),
            realm: "test".into(),
            partner_user_id: approval.partner_user_id,
            organization_id: approval.organization_id,
            membership_id: approval.membership_id,
            partner_role_id: approval.partner_role_id,
            environment: "dev".into(),
            authorization_revision: Some(2),
            granted_actions: ACTIONS.iter().map(|action| action.to_string()).collect(),
            authorized_app_ids: vec!["mapp_one".into()],
            expires_at: (Utc::now() + Duration::seconds(120)).to_rfc3339(),
            apps: vec![CliApp {
                app_id: "mapp_one".into(),
                display_name: "Shop".into(),
                status: "active".into(),
            }],
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
    fn pairing_code_normalization_accepts_only_the_fixed_shape() {
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
            "abcdefghi",
        ] {
            assert_eq!(
                normalize_pairing_code(invalid),
                None,
                "{invalid} must be rejected"
            );
        }
    }

    #[test]
    fn alphabet_is_the_contract_thirty_two_set_without_ambiguous_glyphs() {
        let text = String::from_utf8_lossy(&ALPHABET);
        assert_eq!(text, "ABCDEFGHJKLMNPQRSTUVWXYZ23456789");
        for ambiguous in ['I', 'O', '0', '1'] {
            assert!(!text.contains(ambiguous), "{ambiguous} is excluded");
        }
    }

    #[test]
    fn generated_identifiers_keep_their_shape() {
        for _ in 0..32 {
            let id = new_uuid_v4().expect("uuid");
            assert!(is_uuid(&id), "{id}");
            assert_eq!(&id[14..15], "4");
            assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
            let code = new_pairing_code().expect("code");
            assert_eq!(code.len(), CODE_LENGTH);
            assert_eq!(display_pairing_code(&code).len(), CODE_LENGTH + 1);
            assert_eq!(
                normalize_pairing_code(&code).as_deref(),
                Some(code.as_str())
            );
        }
        let proof = new_proof().expect("proof");
        assert_eq!(proof.len(), 64);
        assert!(proof
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        assert!(!is_uuid("11111111-1111-4111-8111-11111111111G"));
    }

    #[test]
    fn dev_target_is_rejected_before_any_local_or_network_work() {
        assert_eq!(reject_target("test"), Ok(()));
        assert_eq!(
            reject_target("dev").unwrap_err(),
            Failure::Local("target_unsupported")
        );
        for command in ["authorize", "session", "check-access", "logout"] {
            let parsed = crate::Cli::try_parse_from([
                "facto", "demo", command, "--demo", "shop", "--target", "dev", "--mode", "fresh",
            ])
            .expect("dev selection parses");
            assert!(matches!(parsed.command, crate::Commands::Demo { .. }));
        }
        assert!(crate::Cli::try_parse_from([
            "facto",
            "demo",
            "authorize",
            "--demo",
            "shop",
            "--target",
            "test"
        ])
        .is_err());
        assert!(crate::Cli::try_parse_from([
            "facto",
            "demo",
            "authorize",
            "--demo",
            "orbit",
            "--target",
            "test",
            "--mode",
            "fresh"
        ])
        .is_err());
        assert!(crate::Cli::try_parse_from([
            "facto",
            "demo",
            "logout",
            "--demo",
            "shop",
            "--target",
            "production",
            "--mode",
            "fresh"
        ])
        .is_err());
    }

    #[test]
    fn create_response_validation_rejects_any_deviation() {
        let good = created_response();
        assert_eq!(
            validate_create(good.clone(), FIXTURE_ID).expect("valid"),
            good
        );
        let cases = [
            CreateResponse {
                authorization_id: "22222222-2222-4222-8222-222222222222".into(),
                ..good.clone()
            },
            CreateResponse {
                status: "ACTIVE".into(),
                ..good.clone()
            },
            CreateResponse {
                revision: 2,
                ..good.clone()
            },
            CreateResponse {
                poll_interval_seconds: 1,
                ..good.clone()
            },
            CreateResponse {
                realm: "external".into(),
                ..good.clone()
            },
            CreateResponse {
                target_environment: "dev".into(),
                ..good.clone()
            },
            CreateResponse {
                verification_uri: format!("{API_ORIGIN}/cli/approve?authorization_id={FIXTURE_ID}"),
                ..good.clone()
            },
            CreateResponse {
                expires_at: (Utc::now() - Duration::seconds(1)).to_rfc3339(),
                ..good.clone()
            },
            CreateResponse {
                expires_at: (Utc::now() + Duration::seconds(PENDING_LIMIT + 600)).to_rfc3339(),
                ..good.clone()
            },
        ];
        for case in cases {
            assert!(validate_create(case, FIXTURE_ID).is_err());
        }
    }

    #[test]
    fn wire_dto_rejects_unknown_fields_and_enforces_action_order() {
        let mut value = serde_json::json!({
            "request_id": FIXTURE_ID,
            "verification_code_hash": "1cba9a1db5d50072c42010d9727b800d8ebf20ac7ce7efab3bd947b37c900901",
            "requested_actions": ACTIONS,
            "mode": "reuse"
        });
        let back: serde_json::Value = serde_json::from_value::<CreateRequestOwned>(value.clone())
            .expect("round trip")
            .to_value();
        assert_eq!(back, value);
        value["token"] = serde_json::json!("never");
        assert!(serde_json::from_value::<CreateRequestOwned>(value).is_err());

        let reordered = serde_json::json!({
            "request_id": FIXTURE_ID,
            "verification_code_hash": "1cba9a1db5d50072c42010d9727b800d8ebf20ac7ce7efab3bd947b37c900901",
            "requested_actions": ["merchant.apps.read", "workspace.access.read"],
            "mode": "reuse"
        });
        let parsed = serde_json::from_value::<CreateRequestOwned>(reordered).expect("wire parses");
        assert!(require_actions(&parsed.requested_actions).is_err());
        assert!(require_actions(
            &ACTIONS
                .iter()
                .map(|action| action.to_string())
                .collect::<Vec<String>>()
        )
        .is_ok());
        assert!(require_actions(&["workspace.access.read".to_string()]).is_err());
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct CreateRequestOwned {
        request_id: String,
        verification_code_hash: String,
        requested_actions: Vec<String>,
        mode: String,
    }

    impl CreateRequestOwned {
        fn to_value(&self) -> serde_json::Value {
            serde_json::json!({
                "request_id": self.request_id,
                "verification_code_hash": self.verification_code_hash,
                "requested_actions": self.requested_actions,
                "mode": self.mode
            })
        }
    }

    #[test]
    fn current_read_may_never_exceed_the_approved_ceiling() {
        let journal = journal_for(FIXTURE_ID);
        assert_eq!(
            validate_workspace(workspace_base(), &journal, &snapshot(&["mapp_one"]))
                .expect("valid"),
            workspace_base()
        );
        let wide = WorkspaceResponse {
            authorized_app_ids: vec!["mapp_two".into()],
            apps: vec![],
            ..workspace_base()
        };
        assert_eq!(
            validate_workspace(wide, &journal, &snapshot(&["mapp_one"])).unwrap_err(),
            Failure::Local("cli_scope_changed")
        );
        let extra_app = WorkspaceResponse {
            apps: vec![CliApp {
                app_id: "mapp_two".into(),
                display_name: "x".into(),
                status: "active".into(),
            }],
            ..workspace_base()
        };
        assert_eq!(
            validate_workspace(extra_app, &journal, &snapshot(&["mapp_one"])).unwrap_err(),
            Failure::Local("cli_scope_changed")
        );
        let swapped_actor = WorkspaceResponse {
            partner_user_id: "99999999-9999-4999-8999-999999999999".into(),
            ..workspace_base()
        };
        assert_eq!(
            validate_workspace(swapped_actor, &journal, &snapshot(&["mapp_one"])).unwrap_err(),
            Failure::Local("cli_scope_changed")
        );
        for mutation in [
            WorkspaceResponse {
                realm: "external".into(),
                ..workspace_base()
            },
            WorkspaceResponse {
                environment: "test".into(),
                ..workspace_base()
            },
            WorkspaceResponse {
                session_id: "66666666-6666-4666-8666-666666666666".into(),
                ..workspace_base()
            },
            WorkspaceResponse {
                granted_actions: vec!["merchant.apps.read".into(), "workspace.access.read".into()],
                ..workspace_base()
            },
            WorkspaceResponse {
                expires_at: (Utc::now() - Duration::seconds(1)).to_rfc3339(),
                ..workspace_base()
            },
        ] {
            assert!(validate_workspace(mutation, &journal, &snapshot(&["mapp_one"])).is_err());
        }
    }

    #[test]
    fn exchange_and_revocation_are_bound_to_their_request_ids() {
        let request_id = "77777777-7777-4777-8777-777777777777";
        let exchange = ExchangeResponse {
            request_id: request_id.into(),
            operation: "partner_cli.exchange".into(),
            authorization_id: FIXTURE_ID.into(),
            status: "ACTIVE".into(),
            previous_revision: 2,
            revision: 3,
            approval: Some(snapshot(&["mapp_one"])),
            session_id: FIXTURE_ID.into(),
            expires_at: (Utc::now() + Duration::seconds(120)).to_rfc3339(),
        };
        assert!(validate_exchange(exchange.clone(), FIXTURE_ID, request_id).is_ok());
        for mutation in [
            ExchangeResponse {
                request_id: "88888888-8888-4888-8888-888888888888".into(),
                ..exchange.clone()
            },
            ExchangeResponse {
                status: "PENDING".into(),
                ..exchange.clone()
            },
            ExchangeResponse {
                revision: 2,
                ..exchange.clone()
            },
            ExchangeResponse {
                previous_revision: 0,
                ..exchange.clone()
            },
            ExchangeResponse {
                approval: None,
                ..exchange.clone()
            },
            ExchangeResponse {
                expires_at: (Utc::now() + Duration::seconds(ACTIVE_LIMIT + 3600)).to_rfc3339(),
                ..exchange.clone()
            },
            ExchangeResponse {
                expires_at: (Utc::now() - Duration::seconds(1)).to_rfc3339(),
                ..exchange.clone()
            },
        ] {
            assert!(validate_exchange(mutation, FIXTURE_ID, request_id).is_err());
        }

        let revoke_id = "99999999-9999-4999-8999-999999999999";
        let revoke = RevocationResponse {
            request_id: revoke_id.into(),
            operation: "partner_cli.revoke".into(),
            authorization_id: FIXTURE_ID.into(),
            status: "REVOKED".into(),
            previous_revision: 3,
            revision: 4,
            approval: Some(snapshot(&["mapp_one"])),
        };
        assert!(validate_revocation(revoke.clone(), FIXTURE_ID, revoke_id).is_ok());
        assert!(validate_revocation(
            RevocationResponse {
                operation: "partner_cli.exchange".into(),
                ..revoke.clone()
            },
            FIXTURE_ID,
            revoke_id
        )
        .is_err());
        assert!(validate_revocation(
            RevocationResponse {
                revision: 3,
                ..revoke.clone()
            },
            FIXTURE_ID,
            revoke_id
        )
        .is_err());
        assert!(
            validate_revocation(revoke, FIXTURE_ID, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
                .is_err()
        );
    }

    #[test]
    fn receipt_decoding_keeps_unknown_separate_from_success() {
        let unknown = serde_json::json!({"request_id": FIXTURE_ID, "operation": "approve", "status": "UNKNOWN", "response": null});
        assert_eq!(
            decode_receipt(unknown.to_string().as_bytes(), "approve", FIXTURE_ID).expect("unknown"),
            None
        );
        let succeeded = serde_json::json!({"request_id": FIXTURE_ID, "operation": "exchange", "status": "SUCCEEDED", "response": {"a": 1}});
        assert!(
            decode_receipt(succeeded.to_string().as_bytes(), "exchange", FIXTURE_ID)
                .expect("value")
                .is_some()
        );
        for bad in [
            serde_json::json!({"request_id": FIXTURE_ID, "operation": "approve", "status": "FAILED", "response": null}),
            serde_json::json!({"request_id": FIXTURE_ID, "operation": "exchange", "status": "SUCCEEDED", "response": null}),
            serde_json::json!({"request_id": FIXTURE_ID, "operation": "delete", "status": "UNKNOWN", "response": null}),
            serde_json::json!({"request_id": FIXTURE_ID, "operation": "create", "status": "UNKNOWN", "response": {}}),
            serde_json::json!({"request_id": "22222222-2222-4222-8222-222222222222", "operation": "create", "status": "UNKNOWN", "response": null}),
            serde_json::json!({"request_id": FIXTURE_ID, "operation": "approve", "status": "SUCCEEDED", "response": {}, "proof": "x"}),
        ] {
            assert!(decode_receipt(bad.to_string().as_bytes(), "approve", FIXTURE_ID).is_err());
        }
    }

    #[test]
    fn server_codes_come_only_from_the_closed_set() {
        assert_eq!(
            server_code("cli_pairing_mismatch").expect("known"),
            Failure::Server("cli_pairing_mismatch")
        );
        assert_eq!(
            server_code("recent_mfa_required").expect("known"),
            Failure::Server("recent_mfa_required")
        );
        assert_eq!(
            server_code("auto_approved").unwrap_err(),
            Failure::Local("wire_invalid")
        );
        for code in SERVER_ERROR_CODES {
            assert!(require_phase(code).is_err(), "{code} is not a phase");
        }
        for phase in PHASES {
            assert!(require_phase(phase).is_ok());
        }
        assert!(require_phase("SUCCEEDED").is_err());
    }

    #[test]
    fn http_error_bodies_map_to_their_code_and_never_to_success() {
        let body = serde_json::json!({"code": "cli_rate_limited", "message": "slow down"});
        let wire = Wire {
            status: StatusCode::TOO_MANY_REQUESTS,
            body: body.to_string().into_bytes(),
        };
        assert_eq!(
            decode(wire, StatusCode::OK).unwrap_err(),
            Failure::Server("cli_rate_limited")
        );

        let unknown = serde_json::json!({"code": "please_retry_post", "message": "x"});
        let wire = Wire {
            status: StatusCode::BAD_GATEWAY,
            body: unknown.to_string().into_bytes(),
        };
        assert_eq!(
            decode(wire, StatusCode::CREATED).unwrap_err(),
            Failure::Local("http_rejected")
        );

        let empty = Wire {
            status: StatusCode::NOT_FOUND,
            body: Vec::new(),
        };
        assert_eq!(
            decode(empty, StatusCode::OK).unwrap_err(),
            Failure::Local("http_rejected")
        );
    }

    #[test]
    fn requests_use_only_the_dedicated_proof_scheme_and_fixed_test_origin() {
        let proof = "a".repeat(64);
        let request = with_headers(
            client()
                .expect("client")
                .post(format!("{API_ORIGIN}{PREFIX}/authorizations")),
            proof_headers(&proof, FIXTURE_ID, FIXTURE_ID)
                .expect("headers")
                .into_iter()
                .chain([target_header()])
                .collect(),
        )
        .json(&CreateRequest {
            request_id: FIXTURE_ID,
            verification_code_hash: &"1".repeat(64),
            requested_actions: ACTIONS,
            mode: "reuse",
        })
        .build()
        .expect("request");

        assert_eq!(
            request.url().as_str(),
            format!("{API_ORIGIN}{PREFIX}/authorizations")
        );
        assert_eq!(request.url().scheme(), "https");
        assert_eq!(
            request
                .headers()
                .get(header::AUTHORIZATION)
                .expect("auth")
                .to_str()
                .expect("ascii"),
            format!("Facto-CLI {proof}")
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
        assert!(request.body().is_some());

        let read = with_headers(
            client()
                .expect("client")
                .get(format!("{API_ORIGIN}{PREFIX}/session/workspace")),
            proof_headers(&proof, "", FIXTURE_ID).expect("get headers"),
        )
        .build()
        .expect("read request");
        assert_eq!(read.method(), reqwest::Method::GET);
        assert!(!read.headers().contains_key("idempotency-key"));
        assert!(!read.headers().contains_key("x-facto-target"));
        assert!(!read.headers().contains_key(header::COOKIE));

        for weak in ["short", &"A".repeat(64), &"g".repeat(64), ""] {
            assert!(
                proof_headers(weak, FIXTURE_ID, FIXTURE_ID).is_err(),
                "{weak} is not a proof"
            );
        }
        assert!(proof_headers(&proof, "not-a-uuid", FIXTURE_ID).is_err());
        assert!(proof_headers(&proof, FIXTURE_ID, "not-a-uuid").is_err());
    }

    #[test]
    fn reports_never_serialize_proof_or_pairing_code() {
        let mut report = Report::new("partner_cli_authorize", "fresh", "workspace");
        report.authorization_id = Some(FIXTURE_ID.into());
        report.session_id = Some(FIXTURE_ID.into());
        report.verification_uri = Some(approval_page_uri(FIXTURE_ID));
        report.authentication_verified = true;
        let json = serde_json::to_string(&report.clone().succeeded()).expect("json");
        for forbidden in [
            "proof",
            "a".repeat(64),
            "matching",
            "cookie",
            "secret",
            "ABCDEFGHJK",
        ] {
            assert!(
                !json.contains(forbidden.as_str()),
                "{forbidden} leaked into the report"
            );
        }
        assert!(json.contains("\"uat_status\":\"NOT_RUN\""));
        assert!(json.contains("\"consumer_state_accessed\":false"));
        assert!(json.contains("\"app_secret_transport\":false"));
        assert!(json.contains("\"authentication_verified\":true"));
        assert!(json.contains("\"target\":\"test\""));

        let human = report.render(false);
        assert!(human.contains("Business UAT: NOT_RUN"));
        assert!(!human.contains("Pairing code"));

        let failed = report.failed(&Failure::Server("cli_pairing_mismatch"));
        assert_eq!(failed.error.as_deref(), Some("cli_pairing_mismatch"));
        assert!(!serde_json::to_string(&failed)
            .expect("json")
            .contains("a".repeat(64).as_str()));
    }

    #[test]
    fn pairing_code_channel_is_never_the_json_report() {
        let code = display_pairing_code("ABCDEFGHJK");
        assert_eq!(code, "ABCDE-FGHIJK");
        let json = serde_json::to_string(&Report::new("partner_cli_authorize", "fresh", "pending"))
            .expect("json");
        assert!(!json.contains(&code));
        assert!(!json.contains("ABCDEFGHJK"));
    }

    #[test]
    fn approval_page_locator_is_the_only_pairing_parameter() {
        let uri = approval_page_uri(FIXTURE_ID);
        assert_eq!(uri, "https://test-partner.facto.to/cli/approve?authorization_id=11111111-1111-4111-8111-111111111111");
        assert!(!uri.contains("code"));
        assert!(!uri.contains("proof"));
        assert!(!uri.contains(API_ORIGIN));
    }

    #[test]
    fn limits_match_the_delivered_fixture_policy() {
        assert_eq!(PENDING_LIMIT, 600);
        assert_eq!(POLL_SECONDS, 2);
        assert_eq!(ACTIVE_LIMIT, 900);
        assert_eq!(ABSOLUTE_PARENT_LIMIT, 28_800);
        assert_eq!(MAX_RESPONSE_BYTES, 65_536);
        assert_eq!(REQUEST_TIMEOUT, StdDuration::from_secs(8));
        assert_eq!(ALPHABET.len(), 32);
        assert_eq!(ACTIONS, ["workspace.access.read", "merchant.apps.read"]);
        assert_eq!(SERVER_ERROR_CODES.len(), 21);
        assert_eq!(RECEIPT_OPERATIONS.len(), 4);
        assert_eq!(PHASES.len(), 6);
    }

    #[test]
    fn entropy_is_read_from_the_os_device_only() {
        let first = random_bytes::<16>().expect("entropy");
        let second = random_bytes::<16>().expect("entropy");
        assert_ne!(first, second, "successive draws must differ");
        assert!(
            File::open("/dev/urandom-not-a-device").is_err(),
            "a missing device must fail closed"
        );
    }
}
