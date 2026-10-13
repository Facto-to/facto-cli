//! Private Test-only pairing journal for the Partner CLI handoff.
//!
//! This store holds the possession proof. It is separate from Consumer
//! credentials and configuration, and nothing in it may reach stdout, a log or
//! any report.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(crate) const JOURNAL_VERSION: u8 = 1;
const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const PREFIX: &str = "authorization-";
const SUFFIX: &str = ".json";
const MAX_JOURNAL_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateError {
    HomeUnavailable,
    InsecureStore,
    InsecureFile,
    JournalMissing,
    JournalInvalid,
    WriteFailed,
}

impl StateError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::HomeUnavailable | Self::InsecureStore | Self::WriteFailed => "unsafe_local_state",
            Self::InsecureFile | Self::JournalInvalid => "journal_invalid",
            Self::JournalMissing => "journal_missing",
        }
    }
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for StateError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Journal {
    pub(crate) version: u8,
    pub(crate) target: String,
    pub(crate) mode: String,
    pub(crate) proof_hex: String,
    pub(crate) authorization_id: String,
    pub(crate) create_request_id: String,
    pub(crate) create_body: serde_json::Value,
    pub(crate) create_recorded: bool,
    pub(crate) phase: String,
    pub(crate) revision: u64,
    pub(crate) expires_at: String,
    pub(crate) exchange_request_id: Option<String>,
    pub(crate) exchange_recorded: bool,
    pub(crate) revoke_request_id: Option<String>,
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
            authorization_id: authorization_id.to_string(),
            create_request_id: authorization_id.to_string(),
            create_body,
            create_recorded: false,
            // Nothing is authorized until the server echoes the create response.
            phase: "PREWRITE".to_string(),
            revision: 0,
            expires_at: String::new(),
            exchange_request_id: None,
            exchange_recorded: false,
            revoke_request_id: None,
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

fn home_ancestors(dir: &Path) -> Vec<PathBuf> {
    let mut chain = Vec::new();
    let mut current = Some(dir);
    while let Some(path) = current {
        chain.push(path.to_path_buf());
        current = path.parent();
    }
    chain.reverse();
    chain
}

/// Create or verify the private store. An existing store with weaker
/// permissions, or reached through a symlink, is refused rather than repaired.
pub(crate) fn ensure_store(dir: &Path) -> Result<(), StateError> {
    for ancestor in home_ancestors(dir) {
        if ancestor != dir && is_symlink(&ancestor) {
            return Err(StateError::InsecureStore);
        }
    }
    if dir.exists() {
        if is_symlink(dir) || !dir.is_dir() {
            return Err(StateError::InsecureStore);
        }
        return match mode_of(dir) {
            Ok(DIR_MODE) => Ok(()),
            Ok(_) | Err(_) => Err(StateError::InsecureStore),
        };
    }
    fs::create_dir_all(dir).map_err(|_| StateError::InsecureStore)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(DIR_MODE))
        .map_err(|_| StateError::InsecureStore)?;
    match mode_of(dir) {
        Ok(DIR_MODE) => Ok(()),
        Ok(_) | Err(_) => Err(StateError::InsecureStore),
    }
}

fn sync_dir(dir: &Path) -> Result<(), StateError> {
    let handle = OpenOptions::new()
        .read(true)
        .open(dir)
        .map_err(|_| StateError::WriteFailed)?;
    handle.sync_all().map_err(|_| StateError::WriteFailed)
}

/// create_new in the same directory, fsync, rename, directory fsync, then a
/// read-back comparison. A POST is only allowed after this returns Ok.
pub(crate) fn write(dir: &Path, journal: &Journal) -> Result<(), StateError> {
    if journal.version != JOURNAL_VERSION || journal.target != "test" {
        return Err(StateError::JournalInvalid);
    }
    let bytes = serde_json::to_vec_pretty(journal).map_err(|_| StateError::WriteFailed)?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(StateError::JournalInvalid);
    }
    let target = journal_path(dir, &journal.authorization_id);
    if target.exists() && is_symlink(&target) {
        return Err(StateError::InsecureFile);
    }
    let moment = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let mut temp = target.clone();
    temp.set_extension(format!(
        "{}.{}-{}.tmp",
        SUFFIX.trim_start_matches('.'),
        std::process::id(),
        moment
    ));
    {
        let mut file = File::create_new(&temp).map_err(|_| StateError::WriteFailed)?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(FILE_MODE))
            .map_err(|_| StateError::WriteFailed)?;
        file.write_all(&bytes)
            .map_err(|_| StateError::WriteFailed)?;
        file.flush().map_err(|_| StateError::WriteFailed)?;
        file.sync_all().map_err(|_| StateError::WriteFailed)?;
    }
    fs::rename(&temp, &target).map_err(|_| StateError::WriteFailed)?;
    sync_dir(dir)?;
    let read_back = fs::read(&target).map_err(|_| StateError::WriteFailed)?;
    if read_back != bytes {
        return Err(StateError::WriteFailed);
    }
    match mode_of(&target) {
        Ok(FILE_MODE) => Ok(()),
        Ok(_) | Err(_) => Err(StateError::InsecureFile),
    }
}

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
    if journal.version != JOURNAL_VERSION
        || journal.target != "test"
        || journal.authorization_id.is_empty()
        || journal.create_request_id != journal.authorization_id
    {
        return Err(StateError::JournalInvalid);
    }
    Ok(journal)
}

/// Most recently updated pairing in this store. session, check-access and
/// logout take no authorization identifier, so the store is the only locator.
pub(crate) fn current(dir: &Path) -> Result<Journal, StateError> {
    if !dir.exists() {
        return Err(StateError::JournalMissing);
    }
    ensure_store(dir)?;
    let entries = fs::read_dir(dir).map_err(|_| StateError::InsecureStore)?;
    let mut newest: Option<Journal> = None;
    for entry in entries {
        let entry = entry.map_err(|_| StateError::InsecureStore)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // A foreign leaf means the private store is not ours any more.
        if !name.starts_with(PREFIX)
            || !name.ends_with(SUFFIX)
            || name == format!("{PREFIX}{SUFFIX}")
        {
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
    newest.ok_or(StateError::JournalMissing)
}

pub(crate) fn home_dir() -> Result<PathBuf, StateError> {
    dirs::home_dir().ok_or(StateError::HomeUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn temp_store(tag: &str) -> PathBuf {
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

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 13, 0, 0, 0).unwrap()
    }

    fn journal(id: &str) -> Journal {
        let mut entry = Journal::new(
            "reuse",
            &"a".repeat(64),
            id,
            serde_json::json!({"request_id": id}),
            &now(),
        );
        entry.create_recorded = true;
        entry.phase = "PENDING".to_string();
        entry.revision = 1;
        entry.expires_at = "2026-10-13T00:10:00+00:00".to_string();
        entry
    }

    #[cfg(unix)]
    #[test]
    fn store_is_created_private_and_rejects_weaker_modes() {
        let dir = store_dir(&temp_store("mode"));
        ensure_store(&dir).expect("store");
        assert_eq!(mode_of(&dir).expect("mode"), DIR_MODE);

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("loosen");
        assert_eq!(ensure_store(&dir).unwrap_err(), StateError::InsecureStore);
    }

    #[cfg(unix)]
    #[test]
    fn round_trip_keeps_bytes_and_rejects_extra_fields() {
        let dir = store_dir(&temp_store("round"));
        ensure_store(&dir).expect("store");
        let id = "11111111-1111-4111-8111-111111111111";
        let entry = journal(id);
        write(&dir, &entry).expect("write");
        assert_eq!(read(&journal_path(&dir, id)).expect("read"), entry);
        assert_eq!(mode_of(&journal_path(&dir, id)).expect("mode"), FILE_MODE);

        let mut polluted = serde_json::to_value(&entry).expect("value");
        polluted["consumer_token"] = serde_json::json!("never");
        fs::write(journal_path(&dir, id), polluted.to_string()).expect("pollute");
        fs::set_permissions(
            journal_path(&dir, id),
            fs::Permissions::from_mode(FILE_MODE),
        )
        .expect("reperm");
        assert_eq!(
            read(&journal_path(&dir, id)).unwrap_err(),
            StateError::JournalInvalid
        );

        let mut tampered = serde_json::to_value(&entry).expect("value");
        tampered["target"] = serde_json::json!("dev");
        fs::write(journal_path(&dir, id), tampered.to_string()).expect("tamper");
        fs::set_permissions(
            journal_path(&dir, id),
            fs::Permissions::from_mode(FILE_MODE),
        )
        .expect("reperm");
        assert_eq!(
            read(&journal_path(&dir, id)).unwrap_err(),
            StateError::JournalInvalid
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_journal_and_store_are_refused() {
        let dir = store_dir(&temp_store("symlink"));
        ensure_store(&dir).expect("store");
        let id = "22222222-2222-4222-8222-222222222222";
        write(&dir, &journal(id)).expect("write");
        let real = journal_path(&dir, id);
        let decoy = dir.join("decoy.json");
        fs::write(&decoy, b"{}").expect("decoy");
        fs::remove_file(&real).expect("remove");
        std::os::unix::fs::symlink(&decoy, &real).expect("symlink");
        assert_eq!(read(&real).unwrap_err(), StateError::InsecureFile);

        let outer = temp_store("symlink-outer");
        let linked = outer.join("linked");
        fs::create_dir_all(store_dir(&outer).parent().unwrap()).expect("parents");
        std::os::unix::fs::symlink(fs::canonicalize(store_dir(&outer)).expect("real"), &linked)
            .expect("link store");
        assert_eq!(
            ensure_store(&linked).unwrap_err(),
            StateError::InsecureStore
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_mode_weakening_is_refused() {
        let dir = store_dir(&temp_store("filemode"));
        ensure_store(&dir).expect("store");
        let id = "33333333-3333-4333-8333-333333333333";
        write(&dir, &journal(id)).expect("write");
        fs::set_permissions(journal_path(&dir, id), fs::Permissions::from_mode(0o644))
            .expect("loosen");
        assert_eq!(
            read(&journal_path(&dir, id)).unwrap_err(),
            StateError::InsecureFile
        );
    }

    #[cfg(unix)]
    #[test]
    fn current_picks_newest_update_and_empty_store_is_missing() {
        let dir = store_dir(&temp_store("current"));
        assert_eq!(current(&dir).unwrap_err(), StateError::JournalMissing);
        ensure_store(&dir).expect("store");
        assert_eq!(current(&dir).unwrap_err(), StateError::JournalMissing);
        let mut older = journal("44444444-4444-4444-8444-444444444444");
        older.updated_at = "2026-10-13T01:00:00+00:00".to_string();
        let mut newer = journal("55555555-5555-4555-8555-555555555555");
        newer.updated_at = "2026-10-13T02:00:00+00:00".to_string();
        write(&dir, &older).expect("older");
        write(&dir, &newer).expect("newer");
        assert_eq!(
            current(&dir).expect("current").authorization_id,
            newer.authorization_id
        );
    }

    #[cfg(unix)]
    #[test]
    fn foreign_leaf_and_oversize_journal_fail_closed() {
        let dir = store_dir(&temp_store("foreign"));
        ensure_store(&dir).expect("store");
        fs::write(dir.join("stray.txt"), b"x").expect("stray");
        assert_eq!(current(&dir).unwrap_err(), StateError::InsecureStore);

        let clean = store_dir(&temp_store("big"));
        ensure_store(&clean).expect("store");
        let id = "66666666-6666-4666-8666-666666666666";
        fs::write(journal_path(&clean, id), vec![b' '; MAX_JOURNAL_BYTES + 1]).expect("big");
        fs::set_permissions(
            journal_path(&clean, id),
            fs::Permissions::from_mode(FILE_MODE),
        )
        .expect("perm");
        assert_eq!(
            read(&journal_path(&clean, id)).unwrap_err(),
            StateError::JournalInvalid
        );

        assert!(write(&clean, &journal("77777777-7777-4777-8777-777777777777")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn journal_stores_no_consumer_or_credential_field() {
        let dir = store_dir(&temp_store("shape"));
        ensure_store(&dir).expect("store");
        let id = "88888888-8888-4888-8888-888888888888";
        write(&dir, &journal(id)).expect("write");
        let text = fs::read_to_string(journal_path(&dir, id)).expect("text");
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
            text.contains("proof_hex"),
            "the proof lives in the private file"
        );
        assert!(
            !text.contains("ABCDEFGHJK"),
            "the pairing code is never persisted"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unknown_operations_are_recorded_once() {
        let dir = store_dir(&temp_store("unknown"));
        ensure_store(&dir).expect("store");
        let id = "99999999-9999-4999-8999-999999999999";
        let mut entry = journal(id);
        entry.mark_unknown("create");
        entry.mark_unknown("create");
        entry.mark_unknown("exchange");
        assert_eq!(
            entry.unknown,
            vec!["create".to_string(), "exchange".to_string()]
        );
        write(&dir, &entry).expect("write");
        assert_eq!(
            read(&journal_path(&dir, id)).expect("read").unknown,
            entry.unknown
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_leaves_no_temp_file_behind_after_success() {
        let dir = store_dir(&temp_store("temp"));
        ensure_store(&dir).expect("store");
        let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        write(&dir, &journal(id)).expect("write");
        let leftovers: Vec<String> = fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files must be renamed away: {leftovers:?}"
        );
    }
}
