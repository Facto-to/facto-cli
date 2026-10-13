//! Private Test-only pairing journal for the Partner CLI handoff.
//!
//! The store holds the possession proof and the pairing code. It is separate
//! from Consumer credentials and configuration, and nothing in it may reach
//! stdout, stderr except the interactive pairing prompt, a log or any report.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

pub(crate) const JOURNAL_VERSION: u8 = 2;
pub(crate) const ALPHABET: [u8; 32] = *b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub(crate) const CODE_LENGTH: usize = 10;
const MATCHING_PREFIX: &str = "facto:partner-cli:matching:v1";
pub(crate) const REALM: &str = "test";
const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const PREFIX: &str = "authorization-";
const SUFFIX: &str = ".json";
const MAX_JOURNAL_BYTES: usize = 64 * 1024;
const PHASES: [&str; 9] = [
    "PREWRITE",
    "PENDING",
    "APPROVED",
    "ACTIVE",
    "DENIED",
    "EXPIRED",
    "REVOKED",
    "UNKNOWN",
    "RECEIPT_RECOVERED",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateError {
    HomeUnavailable,
    InsecureStore,
    InsecureFile,
    JournalMissing,
    JournalInvalid,
    IntentConflict,
    LockHeld,
    WriteFailed,
}

impl StateError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::HomeUnavailable | Self::InsecureStore | Self::WriteFailed => "unsafe_local_state",
            Self::JournalInvalid => "journal_invalid",
            Self::InsecureFile => "insecure_file",
            Self::JournalMissing => "journal_missing",
            Self::IntentConflict => "intent_conflict",
            Self::LockHeld => "concurrent_writer",
        }
    }
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for StateError {}

pub(crate) fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub(crate) fn is_canonical_uuid(value: &str) -> bool {
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
        matches!(index, 8 | 13 | 18 | 23)
            || (byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

/// The pairing code is ten characters from the 32-glyph alphabet. It lives here
/// so the stored journal and the outgoing hash can never disagree.
pub(crate) fn normalize_pairing_code(input: &str) -> Option<String> {
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

pub(crate) fn matching_canonical_bytes(authorization_id: &str, normalized: &str) -> Vec<u8> {
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

pub(crate) fn verification_code_hash(authorization_id: &str, normalized: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(matching_canonical_bytes(authorization_id, normalized));
    hex::encode(hasher.finalize())
}

fn parses_moment(value: &str) -> bool {
    value.is_empty() || DateTime::parse_from_rfc3339(value).is_ok()
}

/// One persisted pairing. The original intent is written before every POST, so
/// a crash or a lost response can only ever be recovered by reading back the
/// same request id.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Journal {
    pub(crate) version: u8,
    pub(crate) target: String,
    pub(crate) mode: String,
    pub(crate) proof_hex: String,
    pub(crate) pairing_code: String,
    pub(crate) authorization_id: String,
    pub(crate) create_request_id: String,
    pub(crate) create_body: serde_json::Value,
    pub(crate) create_recorded: bool,
    pub(crate) phase: String,
    pub(crate) revision: u64,
    pub(crate) expires_at: String,
    pub(crate) approval: Option<serde_json::Value>,
    pub(crate) exchange_request_id: Option<String>,
    pub(crate) exchange_expected_revision: Option<u64>,
    pub(crate) exchange_recorded: bool,
    pub(crate) revoke_request_id: Option<String>,
    pub(crate) revoke_expected_revision: Option<u64>,
    pub(crate) revoke_recorded: bool,
    pub(crate) session_id: Option<String>,
    pub(crate) unknown: Vec<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

impl Journal {
    pub(crate) fn new(
        mode: &str,
        proof_hex: &str,
        pairing_code: &str,
        authorization_id: &str,
        create_body: serde_json::Value,
        now: &DateTime<Utc>,
    ) -> Self {
        let moment = now.to_rfc3339();
        Self {
            version: JOURNAL_VERSION,
            target: "test".to_string(),
            mode: mode.to_string(),
            proof_hex: proof_hex.to_string(),
            pairing_code: pairing_code.to_string(),
            authorization_id: authorization_id.to_string(),
            create_request_id: authorization_id.to_string(),
            create_body,
            create_recorded: false,
            // Nothing is authorized until the server echoes the create response.
            phase: "PREWRITE".to_string(),
            revision: 0,
            expires_at: String::new(),
            approval: None,
            exchange_request_id: None,
            exchange_expected_revision: None,
            exchange_recorded: false,
            revoke_request_id: None,
            revoke_expected_revision: None,
            revoke_recorded: false,
            session_id: None,
            unknown: Vec::new(),
            created_at: moment.clone(),
            updated_at: moment,
        }
    }

    pub(crate) fn touch(
        &mut self,
        now: &DateTime<Utc>,
        phase: &str,
        revision: u64,
        expires_at: &str,
    ) {
        self.phase = phase.to_string();
        self.revision = revision;
        if !expires_at.is_empty() {
            self.expires_at = expires_at.to_string();
        }
        self.updated_at = now.to_rfc3339();
    }

    pub(crate) fn mark_unknown(&mut self, operation: &str) {
        if !self.unknown.iter().any(|name| name == operation) {
            self.unknown.push(operation.to_string());
        }
    }

    pub(crate) fn clear_unknown(&mut self, operation: &str) {
        self.unknown.retain(|name| name != operation);
    }

    pub(crate) fn record_exchange_intent(&mut self, request_id: &str, expected_revision: u64) {
        if self.exchange_request_id.is_none() {
            self.exchange_request_id = Some(request_id.to_string());
            self.exchange_expected_revision = Some(expected_revision);
        }
    }

    pub(crate) fn record_revoke_intent(&mut self, request_id: &str, expected_revision: u64) {
        if self.revoke_request_id.is_none() {
            self.revoke_request_id = Some(request_id.to_string());
            self.revoke_expected_revision = Some(expected_revision);
        }
    }

    /// True when an original intent is still owed a receipt read. A lost create
    /// response must not be forgotten by starting a new pairing.
    pub(crate) fn has_unresolved_intent(&self) -> bool {
        !self.create_recorded || !self.unknown.is_empty() || self.pending_intents().count() > 0
    }

    /// Every original intent that was claimed but never proven recorded. These
    /// are receipt reads only; they can never become a new POST.
    pub(crate) fn pending_intents(&self) -> Vec<&'static str> {
        ["create", "exchange", "revoke"]
            .into_iter()
            .filter(|name| match *name {
                "create" => !self.create_recorded,
                "exchange" => self.exchange_request_id.is_some() && !self.exchange_recorded,
                _ => self.revoke_request_id.is_some() && !self.revoke_recorded,
            })
            .collect()
    }

    pub(crate) fn occupies_store(&self, now: &DateTime<Utc>) -> bool {
        if self.has_unresolved_intent() {
            return true;
        }
        matches!(self.phase.as_str(), "PENDING" | "APPROVED" | "ACTIVE")
            && self
                .pairing_deadline()
                .map_or(true, |deadline| deadline > *now)
    }

    pub(crate) fn pairing_deadline(&self) -> Option<DateTime<Utc>> {
        if self.expires_at.is_empty() {
            return None;
        }
        DateTime::parse_from_rfc3339(self.expires_at.as_str())
            .ok()
            .map(|moment| moment.with_timezone(&Utc))
    }

    /// Structural self-check. A malformed local object must never reach a URL,
    /// a header or an overwrite of another pairing.
    pub(crate) fn validate(&self) -> Result<(), StateError> {
        let actions = [
            "workspace.access.read".to_string(),
            "merchant.apps.read".to_string(),
        ];
        if self.version != JOURNAL_VERSION || self.target != "test" {
            return Err(StateError::JournalInvalid);
        }
        if self.mode != "fresh" && self.mode != "reuse" {
            return Err(StateError::JournalInvalid);
        }
        if !PHASES.contains(&self.phase.as_str()) {
            return Err(StateError::JournalInvalid);
        }
        if !is_lower_hex(&self.proof_hex, 64) || !is_canonical_uuid(&self.authorization_id) {
            return Err(StateError::JournalInvalid);
        }
        if self.create_request_id != self.authorization_id {
            return Err(StateError::JournalInvalid);
        }
        if normalize_pairing_code(&self.pairing_code).as_deref() != Some(self.pairing_code.as_str())
        {
            return Err(StateError::JournalInvalid);
        }
        if self.created_at.is_empty()
            || self.updated_at.is_empty()
            || !parses_moment(&self.created_at)
            || !parses_moment(&self.updated_at)
            || !parses_moment(&self.expires_at)
        {
            return Err(StateError::JournalInvalid);
        }
        // Only an intent that has no server deadline yet may leave it empty.
        if self.expires_at.is_empty() && !matches!(self.phase.as_str(), "PREWRITE" | "UNKNOWN") {
            return Err(StateError::JournalInvalid);
        }
        for id in [
            &self.exchange_request_id,
            &self.revoke_request_id,
            &self.session_id,
        ] {
            if id.as_deref().is_some_and(|value| !is_canonical_uuid(value)) {
                return Err(StateError::JournalInvalid);
            }
        }
        for revision in [
            self.exchange_expected_revision,
            self.revoke_expected_revision,
        ] {
            if revision.is_some_and(|value| value == 0) {
                return Err(StateError::JournalInvalid);
            }
        }
        let object = self
            .create_body
            .as_object()
            .ok_or(StateError::JournalInvalid)?;
        let keys: Vec<&str> = object.keys().map(String::as_str).collect();
        if keys.len() != 4 {
            return Err(StateError::JournalInvalid);
        }
        if object.get("request_id").and_then(serde_json::Value::as_str)
            != Some(&self.authorization_id)
        {
            return Err(StateError::JournalInvalid);
        }
        if object.get("mode").and_then(serde_json::Value::as_str) != Some(self.mode.as_str()) {
            return Err(StateError::JournalInvalid);
        }
        let hash = object
            .get("verification_code_hash")
            .and_then(serde_json::Value::as_str)
            .ok_or(StateError::JournalInvalid)?;
        if !is_lower_hex(hash, 64)
            || hash != &verification_code_hash(&self.authorization_id, &self.pairing_code)
        {
            return Err(StateError::JournalInvalid);
        }
        if object.get("requested_actions") != Some(&serde_json::json!(actions)) {
            return Err(StateError::JournalInvalid);
        }
        for name in &self.unknown {
            if !matches!(name.as_str(), "create" | "exchange" | "revoke") {
                return Err(StateError::JournalInvalid);
            }
        }
        if self.unknown.len() > 3 {
            return Err(StateError::JournalInvalid);
        }
        if self.create_recorded && self.phase == "PREWRITE" {
            return Err(StateError::JournalInvalid);
        }
        Ok(())
    }
}

/// Redacting debug: the proof, pairing code and original body are never
/// printable, so a stray `{:?}` or panic cannot disclose them.
impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("version", &self.version)
            .field("target", &self.target)
            .field("mode", &self.mode)
            .field("authorization_id", &self.authorization_id)
            .field("phase", &self.phase)
            .field("revision", &self.revision)
            .field("expires_at", &self.expires_at)
            .field("create_recorded", &self.create_recorded)
            .field("exchange_recorded", &self.exchange_recorded)
            .field("revoke_recorded", &self.revoke_recorded)
            .field("unknown", &self.unknown)
            .field("proof_hex", &"[REDACTED]")
            .field("pairing_code", &"[REDACTED]")
            .field("create_body", &"[REDACTED]")
            .finish()
    }
}

const LOCK_NAME: &str = ".lock";

/// A short-lived mutual-exclusion token inside the private store, created with
/// `create_new` under the 0700 chain and removed on drop. A lock left behind by
/// a dead process is never stolen: the store stays fail-closed.
pub(crate) struct StoreLock {
    path: PathBuf,
}

impl StoreLock {
    fn acquire(dir: &Path) -> Result<Self, StateError> {
        let path = dir.join(LOCK_NAME);
        let mut handle = open_exclusive(&path).map_err(|_| StateError::LockHeld)?;
        let _ = handle.write_all(format!("pid={}\n", std::process::id()).as_bytes());
        let _ = handle.sync_all();
        drop(handle);
        Ok(Self { path })
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn store_dir(home: &Path) -> PathBuf {
    home.join(".facto").join("partner-cli").join("test")
}

pub(crate) fn journal_path(dir: &Path, authorization_id: &str) -> PathBuf {
    dir.join(format!("{PREFIX}{authorization_id}{SUFFIX}"))
}

#[cfg(unix)]
fn mode_of(path: &Path) -> std::io::Result<u32> {
    fs::symlink_metadata(path).map(|meta| meta.permissions().mode() & 0o777)
}

/// Non-Unix builds cannot prove the private-store mode, so they refuse instead
/// of pretending the fence held.
#[cfg(not(unix))]
fn mode_of(path: &Path) -> std::io::Result<u32> {
    let _ = path;
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "private store modes are not verifiable here",
    ))
}

#[cfg(unix)]
fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_symlink(path: &Path) -> bool {
    let _ = path;
    false
}

fn is_private_dir(path: &Path) -> bool {
    matches!(mode_of(path), Ok(DIR_MODE))
}

/// Verify or create one private parent of the store. An existing directory with
/// weaker permissions or a symlink is refused; the Consumer `.facto` directory
/// keeps whatever historical mode it has and is only checked for indirection.
fn ensure_private_parent(dir: &Path) -> Result<(), StateError> {
    if is_symlink(dir) {
        return Err(StateError::InsecureStore);
    }
    if dir.exists() {
        if !dir.is_dir() || !is_private_dir(dir) {
            return Err(StateError::InsecureStore);
        }
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.mode(DIR_MODE);
    builder.create(dir).map_err(|_| StateError::InsecureStore)?;
    if !is_private_dir(dir) {
        let _ = fs::remove_dir(dir);
        return Err(StateError::InsecureStore);
    }
    Ok(())
}

/// Create or verify `~/.facto/partner-cli/test` and its dedicated parents.
/// Create or verify the dedicated `~/.facto/partner-cli/test` chain.
///
/// Only the two Partner-CLI directories are held to 0700. The Consumer `.facto`
/// directory is checked for indirection and otherwise left alone, so this store
/// never rewrites permissions another component owns.
pub(crate) fn ensure_store(dir: &Path) -> Result<(), StateError> {
    let partner_cli = dir.parent().ok_or(StateError::InsecureStore)?;
    let facto = partner_cli.parent().ok_or(StateError::InsecureStore)?;
    for ancestor in [facto, partner_cli, dir] {
        if is_symlink(&ancestor) {
            return Err(StateError::InsecureStore);
        }
    }
    if facto.exists() {
        if !facto.is_dir() {
            return Err(StateError::InsecureStore);
        }
    } else {
        fs::create_dir(facto).map_err(|_| StateError::InsecureStore)?;
    }
    ensure_private_parent(partner_cli)?;
    ensure_private_parent(dir)
}

fn sync_dir(dir: &Path) -> Result<(), StateError> {
    let handle = OpenOptions::new()
        .read(true)
        .open(dir)
        .map_err(|_| StateError::WriteFailed)?;
    handle.sync_all().map_err(|_| StateError::WriteFailed)
}

#[cfg(unix)]
fn open_exclusive(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(FILE_MODE);
    options.open(path)
}

#[cfg(not(unix))]
fn open_exclusive(path: &Path) -> std::io::Result<File> {
    let _ = path;
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no private store here",
    ))
}

fn write_bytes(dir: &Path, path: &Path, bytes: &[u8], brand_new: bool) -> Result<(), StateError> {
    if is_symlink(path) {
        return Err(StateError::InsecureFile);
    }
    if brand_new {
        // create_new is the fence, and the handle it returns is the one written
        // through: a lost race reports a conflict and never overwrites an intent.
        let mut handle = open_exclusive(path).map_err(|_| StateError::IntentConflict)?;
        handle
            .write_all(bytes)
            .map_err(|_| StateError::WriteFailed)?;
        handle.flush().map_err(|_| StateError::WriteFailed)?;
        handle.sync_all().map_err(|_| StateError::WriteFailed)?;
        drop(handle);
        return verify(dir, path, bytes);
    }
    match mode_of(path) {
        Ok(FILE_MODE) | Err(_) => {}
        Ok(_) => return Err(StateError::InsecureFile),
    }
    let mut temp = path.to_path_buf();
    temp.set_extension(format!(
        "{}-{}.{}.tmp",
        SUFFIX.trim_start_matches('.'),
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    {
        // create_new plus 0600 at open time leaves no world-readable window.
        let mut handle = open_exclusive(&temp).map_err(|_| StateError::WriteFailed)?;
        handle
            .write_all(bytes)
            .map_err(|_| StateError::WriteFailed)?;
        handle.flush().map_err(|_| StateError::WriteFailed)?;
        handle.sync_all().map_err(|_| StateError::WriteFailed)?;
    }
    fs::rename(&temp, path).map_err(|_| StateError::WriteFailed)?;
    verify(dir, path, bytes)
}

fn verify(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), StateError> {
    sync_dir(dir)?;
    let read_back = fs::read(path).map_err(|_| StateError::WriteFailed)?;
    if read_back != bytes {
        return Err(StateError::WriteFailed);
    }
    match mode_of(path) {
        Ok(FILE_MODE) => Ok(()),
        Ok(_) | Err(_) => Err(StateError::InsecureFile),
    }
}

/// First durable record of a pairing. Fails if the pairing already exists, so
/// two processes can never overwrite one original intent.
pub(crate) fn create(dir: &Path, journal: &Journal) -> Result<(), StateError> {
    journal.validate()?;
    let bytes = serde_json::to_vec_pretty(journal).map_err(|_| StateError::WriteFailed)?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(StateError::JournalInvalid);
    }
    ensure_store(dir)?;
    let _guard = StoreLock::acquire(dir)?;
    // One store holds one live pairing: a parallel create cannot slip past it.
    if scan_locked(dir)?.is_some_and(|other| other.occupies_store(&Utc::now())) {
        return Err(StateError::IntentConflict);
    }
    let path = journal_path(dir, &journal.authorization_id);
    write_bytes(dir, &path, &bytes, true)
}

/// Forward-only update of the same pairing.
pub(crate) fn save(dir: &Path, journal: &Journal) -> Result<(), StateError> {
    journal.validate()?;
    ensure_store(dir)?;
    let _guard = StoreLock::acquire(dir)?;
    save_locked(dir, journal)
}

fn save_locked(dir: &Path, journal: &Journal) -> Result<(), StateError> {
    let path = journal_path(dir, &journal.authorization_id);
    let brand_new = !path.exists();
    if !brand_new {
        let previous = read(&path)?;
        check_transition(&previous, journal)?;
    }
    let bytes = serde_json::to_vec_pretty(journal).map_err(|_| StateError::WriteFailed)?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(StateError::JournalInvalid);
    }
    write_bytes(dir, &path, &bytes, brand_new)
}

/// Read the stored pairing, apply one change and write it back while the lock
/// is held, so two processes starting from the same state cannot both claim the
/// right to send the same POST.
pub(crate) fn claim<T>(
    dir: &Path,
    authorization_id: &str,
    mutate: impl FnOnce(&mut Journal) -> Result<T, StateError>,
) -> Result<T, StateError> {
    ensure_store(dir)?;
    let _guard = StoreLock::acquire(dir)?;
    let path = journal_path(dir, authorization_id);
    let mut journal = read(&path)?;
    let outcome = mutate(&mut journal)?;
    journal.updated_at = Utc::now().to_rfc3339();
    journal.validate()?;
    save_locked(dir, &journal)?;
    Ok(outcome)
}

fn check_transition(previous: &Journal, journal: &Journal) -> Result<(), StateError> {
    if previous.authorization_id != journal.authorization_id
        || previous.create_request_id != journal.create_request_id
        || previous.proof_hex != journal.proof_hex
        || previous.pairing_code != journal.pairing_code
        || previous.mode != journal.mode
        || previous.created_at != journal.created_at
    {
        return Err(StateError::IntentConflict);
    }
    if journal.revision < previous.revision || journal.updated_at < previous.updated_at {
        return Err(StateError::IntentConflict);
    }
    for (stored, next) in [
        (&previous.exchange_request_id, &journal.exchange_request_id),
        (&previous.revoke_request_id, &journal.revoke_request_id),
    ] {
        if stored.is_some() && stored != next {
            return Err(StateError::IntentConflict);
        }
    }
    for (stored, next) in [
        (
            previous.exchange_expected_revision,
            journal.exchange_expected_revision,
        ),
        (
            previous.revoke_expected_revision,
            journal.revoke_expected_revision,
        ),
    ] {
        if stored.is_some() && stored != next {
            return Err(StateError::IntentConflict);
        }
    }
    // The approved ceiling is immutable once recorded.
    if previous.approval.is_some() && previous.approval != journal.approval {
        return Err(StateError::IntentConflict);
    }
    // An original intent may only be dropped together with proof it was recorded.
    for name in &previous.unknown {
        if journal.unknown.iter().any(|kept| kept == name) {
            continue;
        }
        let recorded = match name.as_str() {
            "create" => journal.create_recorded,
            "exchange" => journal.exchange_recorded,
            "revoke" => journal.revoke_recorded,
            _ => false,
        };
        if !recorded {
            return Err(StateError::IntentConflict);
        }
    }
    Ok(())
}

/// Read one journal object. The file name must bind the embedded authorization,
/// so a moved or forged file cannot be mistaken for another pairing.
pub(crate) fn read(path: &Path) -> Result<Journal, StateError> {
    if is_symlink(path) {
        return Err(StateError::InsecureFile);
    }
    match mode_of(path) {
        Ok(FILE_MODE) => {}
        Ok(_) | Err(_) => return Err(StateError::InsecureFile),
    }
    let bytes = fs::read(path).map_err(|_| StateError::JournalMissing)?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(StateError::JournalInvalid);
    }
    let journal: Journal =
        serde_json::from_slice(&bytes).map_err(|_| StateError::JournalInvalid)?;
    journal.validate()?;
    let expected = format!("{PREFIX}{}.json", journal.authorization_id);
    if path.file_name().and_then(|name| name.to_str()) != Some(expected.as_str()) {
        return Err(StateError::JournalInvalid);
    }
    Ok(journal)
}

/// Most recently updated pairing. session, check-access and logout take no
/// authorization identifier, so the store is the only locator.
pub(crate) fn current(dir: &Path) -> Result<Journal, StateError> {
    if !dir.exists() {
        return Err(StateError::JournalMissing);
    }
    ensure_store(dir)?;
    let _guard = StoreLock::acquire(dir)?;
    scan_locked(dir)?.ok_or(StateError::JournalMissing)
}

fn scan_locked(dir: &Path) -> Result<Option<Journal>, StateError> {
    let entries = fs::read_dir(dir).map_err(|_| StateError::InsecureStore)?;
    let mut newest: Option<Journal> = None;
    for entry in entries {
        let entry = entry.map_err(|_| StateError::InsecureStore)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == LOCK_NAME {
            continue;
        }
        // A foreign leaf means the private store is not ours any more.
        if !name.starts_with(PREFIX) || !name.ends_with(SUFFIX) {
            return Err(StateError::InsecureStore);
        }
        let journal = read(&entry.path())?;
        let replace = match &newest {
            Some(existing) => journal.updated_at > existing.updated_at,
            None => true,
        };
        if replace {
            newest = Some(journal);
        }
    }
    Ok(newest)
}

pub(crate) fn home_dir() -> Result<PathBuf, StateError> {
    dirs::home_dir().ok_or(StateError::HomeUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 13, 0, 0, 0).unwrap()
    }

    #[cfg(unix)]
    fn temp_root(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "facto-cli-journal-{tag}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("temp root");
        fs::set_permissions(&base, fs::Permissions::from_mode(DIR_MODE)).expect("temp root mode");
        base
    }

    fn body(id: &str, mode: &str) -> serde_json::Value {
        serde_json::json!({
            "request_id": id,
            "verification_code_hash": verification_code_hash(id, "ABCDEFGHJK"),
            "requested_actions": ["workspace.access.read", "merchant.apps.read"],
            "mode": mode
        })
    }

    fn journal(id: &str) -> Journal {
        Journal::new(
            "reuse",
            &"a".repeat(64),
            "ABCDEFGHJK",
            id,
            body(id, "reuse"),
            &now(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn store_and_parents_are_created_private_without_touching_consumer_facto() {
        let home = temp_root("mode");
        fs::create_dir_all(home.join(".facto")).expect("consumer facto");
        fs::set_permissions(home.join(".facto"), fs::Permissions::from_mode(0o755))
            .expect("historical mode");
        let dir = store_dir(&home);
        ensure_store(&dir).expect("store");
        assert_eq!(mode_of(&dir).expect("test mode"), DIR_MODE);
        assert_eq!(
            mode_of(&dir.parent().unwrap()).expect("partner-cli mode"),
            DIR_MODE
        );
        // The Consumer directory keeps its own historical permissions.
        assert_eq!(mode_of(&home.join(".facto")).expect("facto mode"), 0o755);

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("loosen");
        assert_eq!(ensure_store(&dir).unwrap_err(), StateError::InsecureStore);
    }

    #[cfg(unix)]
    #[test]
    fn weak_partner_cli_parent_is_refused_and_not_repaired() {
        let home = temp_root("weakparent");
        fs::create_dir_all(home.join(".facto").join("partner-cli")).expect("parents");
        fs::set_permissions(
            home.join(".facto").join("partner-cli"),
            fs::Permissions::from_mode(0o755),
        )
        .expect("weak parent");
        let dir = store_dir(&home);
        assert_eq!(ensure_store(&dir).unwrap_err(), StateError::InsecureStore);
        assert_eq!(
            mode_of(&home.join(".facto").join("partner-cli")).expect("unchanged"),
            0o755
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_store_and_parent_are_refused() {
        let home = temp_root("symlink");
        let real = home.join("elsewhere");
        fs::create_dir_all(&real).expect("real");
        fs::set_permissions(&real, fs::Permissions::from_mode(DIR_MODE)).expect("real mode");
        fs::create_dir_all(home.join(".facto").join("partner-cli")).expect("parents");
        std::os::unix::fs::symlink(&real, store_dir(&home)).expect("symlink store");
        assert_eq!(
            ensure_store(&store_dir(&home)).unwrap_err(),
            StateError::InsecureStore
        );

        let home2 = temp_root("symlink-parent");
        let target = home2.join("target");
        fs::create_dir_all(&target).expect("target");
        fs::set_permissions(&target, fs::Permissions::from_mode(DIR_MODE)).expect("target mode");
        fs::create_dir_all(home2.join(".facto")).expect("facto");
        std::os::unix::fs::symlink(&target, home2.join(".facto").join("partner-cli"))
            .expect("symlink parent");
        assert_eq!(
            ensure_store(&store_dir(&home2)).unwrap_err(),
            StateError::InsecureStore
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_is_exclusive_and_never_clobbers_an_original_intent() {
        let dir = store_dir(&temp_root("exclusive"));
        ensure_store(&dir).expect("store");
        let id = "11111111-1111-4111-8111-111111111111";
        let entry = journal(id);
        create(&dir, &entry).expect("create");
        let stored = fs::read(journal_path(&dir, id)).expect("stored");
        assert_eq!(mode_of(&journal_path(&dir, id)).expect("mode"), FILE_MODE);
        assert_eq!(
            create(&dir, &entry).unwrap_err(),
            StateError::IntentConflict
        );
        // The rejected second create left the first intent byte-identical.
        assert_eq!(
            fs::read(journal_path(&dir, id)).expect("still stored"),
            stored
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_is_created_private_without_a_world_readable_window() {
        let dir = store_dir(&temp_root("perm"));
        ensure_store(&dir).expect("store");
        let id = "22222222-2222-4222-8222-222222222222";
        create(&dir, &journal(id)).expect("create");
        let raw = fs::read(&journal_path(&dir, id)).expect("raw");
        let text = String::from_utf8(raw).expect("utf8");
        assert!(text.contains("\"proof_hex\""));
        assert!(text.contains("\"pairing_code\""));
        for forbidden in [
            "cookie",
            "client_secret",
            "signing_key",
            "privy",
            "tempo_wallet",
            "api_key",
        ] {
            assert!(
                !text.contains(forbidden),
                "{forbidden} must never be stored"
            );
        }
        assert!(
            !dir.exists()
                || fs::read_dir(&dir)
                    .expect("entries")
                    .filter_map(|entry| entry.ok())
                    .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn update_allows_forward_progress_and_rewrites_stale_state() {
        let dir = store_dir(&temp_root("cas"));
        ensure_store(&dir).expect("store");
        let id = "33333333-3333-4333-8333-333333333333";
        let mut entry = journal(id);
        create(&dir, &entry).expect("create");

        let mut later = now();
        let mut stale = entry.clone();
        stale.phase = "PENDING".to_string();
        stale.revision = 1;
        stale.created_at = Utc
            .with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
            .unwrap()
            .to_rfc3339();
        assert_eq!(save(&dir, &stale).unwrap_err(), StateError::IntentConflict);

        entry.mark_unknown("create");
        entry.touch(&later, "UNKNOWN", 1, "");
        save(&dir, &entry).expect("unknown recorded");
        assert_eq!(
            read(&journal_path(&dir, id)).expect("read").unknown,
            vec!["create".to_string()]
        );

        entry.create_recorded = true;
        entry.clear_unknown("create");
        later = Utc.with_ymd_and_hms(2026, 10, 13, 0, 5, 0).unwrap();
        entry.touch(&later, "PENDING", 1, "2026-10-13T00:15:00+00:00");
        save(&dir, &entry).expect("receipt recovered");
        assert!(read(&journal_path(&dir, id))
            .expect("read")
            .unknown
            .is_empty());

        // Dropping an unresolved intent without recording proof is refused.
        let mut forgetful = entry.clone();
        forgetful.mark_unknown("exchange");
        forgetful.touch(
            &Utc.with_ymd_and_hms(2026, 10, 13, 0, 6, 0).unwrap(),
            "UNKNOWN",
            1,
            "",
        );
        save(&dir, &forgetful).expect("exchange unknown");
        let mut silent = forgetful.clone();
        silent.clear_unknown("exchange");
        silent.touch(
            &Utc.with_ymd_and_hms(2026, 10, 13, 0, 7, 0).unwrap(),
            "ACTIVE",
            2,
            "",
        );
        assert_eq!(save(&dir, &silent).unwrap_err(), StateError::IntentConflict);
    }

    #[cfg(unix)]
    #[test]
    fn malformed_local_objects_are_refused_before_any_url_is_built() {
        let dir = store_dir(&temp_root("malformed"));
        ensure_store(&dir).expect("store");
        let id = "44444444-4444-4444-8444-444444444444";
        let path = journal_path(&dir, id);

        let cases = vec![
            ("uppercase proof", {
                let mut j = journal(id);
                j.proof_hex = "A".repeat(64);
                j
            }),
            ("short proof", {
                let mut j = journal(id);
                j.proof_hex = "a".repeat(63);
                j
            }),
            ("uppercase uuid", {
                let mut j = journal(id);
                j.authorization_id = "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA".into();
                j.create_request_id = j.authorization_id.clone();
                j
            }),
            ("body locator mismatch", {
                let mut j = journal(id);
                j.authorization_id = "11111111-1111-4111-8111-111111111111".into();
                j.create_request_id = "22222222-2222-4222-8222-222222222222".into();
                j
            }),
            ("unknown phase", {
                let mut j = journal(id);
                j.phase = "APPROVED_BY_AGENT".into();
                j
            }),
            ("bad mode", {
                let mut j = journal(id);
                j.mode = "reused".into();
                j
            }),
            ("pairing code in url shape", {
                let mut j = journal(id);
                j.pairing_code = "ABCDE-FGHIJK".into();
                j
            }),
            ("extra body field", {
                let mut j = journal(id);
                j.create_body["target"] = serde_json::json!("dev");
                j
            }),
            ("body actions reordered", {
                let mut j = journal(id);
                j.create_body["requested_actions"] =
                    serde_json::json!(["merchant.apps.read", "workspace.access.read"]);
                j
            }),
            ("body request mismatch", {
                let mut j = journal(id);
                j.create_body["request_id"] =
                    serde_json::json!("55555555-5555-4555-8555-555555555555");
                j
            }),
            ("bad timestamp", {
                let mut j = journal(id);
                j.updated_at = "tomorrow".into();
                j
            }),
            ("hash not bound to the stored code", {
                let mut j = journal(id);
                j.create_body["verification_code_hash"] = serde_json::json!("1".repeat(64));
                j
            }),
            ("empty created_at", {
                let mut j = journal(id);
                j.created_at = String::new();
                j
            }),
            ("zero expected revision", {
                let mut j = journal(id);
                j.exchange_expected_revision = Some(0);
                j
            }),
            ("unknown operation name", {
                let mut j = journal(id);
                j.unknown.push("delete".into());
                j
            }),
        ];
        for (name, case) in cases {
            assert_eq!(
                case.validate().unwrap_err(),
                StateError::JournalInvalid,
                "{name}"
            );
            fs::write(&path, serde_json::to_vec_pretty(&case).expect("bytes")).expect("write");
            fs::set_permissions(&path, fs::Permissions::from_mode(FILE_MODE)).expect("mode");
            assert_eq!(
                read(&path).unwrap_err(),
                StateError::JournalInvalid,
                "{name}"
            );
        }

        // The alphabet includes the digits 2-9, and the stored body must carry
        // the hash of exactly the code that is stored.
        let mut digits = journal(id);
        digits.pairing_code = "ABCD234567".into();
        digits.create_body["verification_code_hash"] =
            serde_json::json!(verification_code_hash(id, "ABCD234567"));
        digits.validate().expect("digits belong to the alphabet");
        for ambiguous in ["ABCDEOGHJK", "ABCDEIGHJK", "ABCDE0GHJK", "ABCDE1GHJK"] {
            let mut bad = digits.clone();
            bad.pairing_code = ambiguous.into();
            bad.create_body["verification_code_hash"] =
                serde_json::json!(verification_code_hash(id, ambiguous));
            assert_eq!(
                bad.validate().unwrap_err(),
                StateError::JournalInvalid,
                "{ambiguous}"
            );
        }
        let mut unbound = digits.clone();
        unbound.create_body["verification_code_hash"] = serde_json::json!("1".repeat(64));
        assert_eq!(
            unbound.validate().unwrap_err(),
            StateError::JournalInvalid,
            "the body hash must bind the stored code"
        );
        let mut empty_stamp = digits.clone();
        empty_stamp.created_at = String::new();
        assert_eq!(
            empty_stamp.validate().unwrap_err(),
            StateError::JournalInvalid,
            "created_at is never empty"
        );
        let mut early_expiry = digits.clone();
        early_expiry.create_recorded = false;
        early_expiry.phase = "PREWRITE".into();
        early_expiry.expires_at = String::new();
        early_expiry
            .validate()
            .expect("a PREWRITE intent may have no deadline");
    }

    #[cfg(unix)]
    #[test]
    fn filename_must_bind_the_embedded_authorization() {
        let dir = store_dir(&temp_root("binding"));
        ensure_store(&dir).expect("store");
        let id = "66666666-6666-4666-8666-666666666666";
        let other = "77777777-7777-4777-8777-777777777777";
        create(&dir, &journal(id)).expect("create");
        let moved = journal_path(&dir, other);
        fs::copy(journal_path(&dir, id), &moved).expect("copy");
        fs::set_permissions(&moved, fs::Permissions::from_mode(FILE_MODE)).expect("mode");
        assert_eq!(read(&moved).unwrap_err(), StateError::JournalInvalid);
        assert!(read(&journal_path(&dir, id)).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_and_weakened_files_are_refused() {
        let dir = store_dir(&temp_root("filemode"));
        ensure_store(&dir).expect("store");
        let id = "88888888-8888-4888-8888-888888888888";
        create(&dir, &journal(id)).expect("create");
        fs::set_permissions(journal_path(&dir, id), fs::Permissions::from_mode(0o644))
            .expect("loosen");
        assert_eq!(
            read(&journal_path(&dir, id)).unwrap_err(),
            StateError::InsecureFile
        );

        fs::set_permissions(
            journal_path(&dir, id),
            fs::Permissions::from_mode(FILE_MODE),
        )
        .expect("restore");
        let real = journal_path(&dir, id);
        let decoy = dir.join("decoy.json");
        fs::write(&decoy, b"{}").expect("decoy");
        fs::remove_file(&real).expect("remove");
        std::os::unix::fs::symlink(&decoy, &real).expect("symlink");
        assert_eq!(read(&real).unwrap_err(), StateError::InsecureFile);
    }

    #[cfg(unix)]
    #[test]
    fn oversize_and_foreign_leaf_fail_closed() {
        let dir = store_dir(&temp_root("big"));
        ensure_store(&dir).expect("store");
        let id = "99999999-9999-4999-8999-999999999999";
        fs::write(journal_path(&dir, id), vec![b' '; MAX_JOURNAL_BYTES + 1]).expect("big");
        fs::set_permissions(
            journal_path(&dir, id),
            fs::Permissions::from_mode(FILE_MODE),
        )
        .expect("mode");
        assert_eq!(
            read(&journal_path(&dir, id)).unwrap_err(),
            StateError::JournalInvalid
        );

        fs::remove_file(journal_path(&dir, id)).expect("remove");
        create(&dir, &journal(id)).expect("create");
        fs::write(dir.join("stray.txt"), b"x").expect("stray");
        assert_eq!(current(&dir).unwrap_err(), StateError::InsecureStore);
    }

    #[cfg(unix)]
    #[test]
    fn current_picks_newest_and_empty_store_is_missing() {
        let dir = store_dir(&temp_root("current"));
        assert_eq!(current(&dir).unwrap_err(), StateError::JournalMissing);
        ensure_store(&dir).expect("store");
        assert_eq!(current(&dir).unwrap_err(), StateError::JournalMissing);
        let mut older = journal("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        // A finished pairing does not occupy the store, so a second one may exist.
        older.phase = "REVOKED".into();
        older.created_at = now().to_rfc3339();
        older.updated_at = Utc
            .with_ymd_and_hms(2026, 10, 13, 1, 0, 0)
            .unwrap()
            .to_rfc3339();
        let mut newer = journal("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
        newer.created_at = now().to_rfc3339();
        newer.updated_at = Utc
            .with_ymd_and_hms(2026, 10, 13, 2, 0, 0)
            .unwrap()
            .to_rfc3339();
        create(&dir, &older).expect("older");
        create(&dir, &newer).expect("newer");
        assert_eq!(
            current(&dir).expect("current").authorization_id,
            newer.authorization_id
        );
    }

    #[cfg(unix)]
    #[test]
    fn debug_output_never_prints_proof_or_pairing_code() {
        let id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        let text = format!("{:?}", journal(id));
        assert!(!text.contains(&"a".repeat(64)), "{text}");
        assert!(!text.contains("ABCDEFGHJK"), "{text}");
        assert!(text.contains("REDACTED"), "{text}");
        assert!(text.contains(id), "the public locator stays debuggable");
    }

    #[cfg(unix)]
    #[test]
    fn unresolved_intent_survives_a_reload() {
        let dir = store_dir(&temp_root("intent"));
        ensure_store(&dir).expect("store");
        let id = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
        let mut entry = journal(id);
        create(&dir, &entry).expect("create");
        assert!(read(&journal_path(&dir, id))
            .expect("read")
            .has_unresolved_intent());
        entry.touch(&now(), "PREWRITE", 0, "");
        assert!(entry.has_unresolved_intent());
        entry.create_recorded = true;
        entry.mark_unknown("exchange");
        assert!(entry.has_unresolved_intent());
        entry.clear_unknown("exchange");
        entry.exchange_recorded = true;
        assert!(!entry.has_unresolved_intent());
        assert!(entry.pairing_deadline().is_none());
    }

    #[test]
    fn uuid_and_hex_gates_accept_every_valid_shape() {
        // The previous check rejected all-digit UUIDs.
        assert!(is_canonical_uuid("00000000-0000-4000-8000-000000000000"));
        assert!(is_canonical_uuid("0e9d2b1f-1a2b-4c3d-8e9f-0123456789ab"));
        assert!(is_canonical_uuid("ffffffff-ffff-4fff-8fff-ffffffffffff"));
        assert!(!is_canonical_uuid("00000000-0000-4000-8000-00000000000G"));
        assert!(!is_canonical_uuid("00000000-0000-4000-8000-000000000000"));
        assert!(!is_canonical_uuid("000000000000400080000000000000000000"));
        assert!(!is_canonical_uuid("00000000-0000-4000-8000-00000000000"));
        assert!(is_lower_hex(&"0".repeat(64), 64));
        assert!(!is_lower_hex(&"A".repeat(64), 64));
        assert!(parses_moment(""));
        assert!(!parses_moment("later"));
    }
}
