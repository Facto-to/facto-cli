//! Real CLI process seams for the Partner CLI handoff. These drive the actual
//! binary and prove the fail-closed local paths never reach the network. The
//! live Test pairing is Root's named UAT, never a mock substitution here.

use serde_json::Value;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_facto");

fn sandbox(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "facto-cli-handoff-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).expect("sandbox home");
    fs::set_permissions(&base, Permissions::from_mode(0o755)).expect("sandbox home mode");
    base
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        // A poisoned Consumer/env endpoint must never be reachable from these
        // commands, so any leak into the output is a test failure.
        .env("FACTO_API_URL", "http://127.0.0.1:1")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .output()
        .expect("run facto")
}

fn store_dir(home: &Path) -> PathBuf {
    home.join(".facto").join("partner-cli").join("test")
}

fn parsed(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(text.trim()).unwrap_or_else(|error| panic!("JSON stdout: {error}\n{text}"))
}

fn assert_never_reaches_network_or_state(output: &Output) {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !text.contains("127.0.0.1"),
        "the Consumer endpoint leaked: {text}"
    );
    assert!(
        !text.contains("test-api.facto.to"),
        "an endpoint was echoed: {text}"
    );
    // 64 hex characters would mean a proof or hash crossed a public channel.
    let flat = text.to_string();
    assert!(
        !flat
            .as_bytes()
            .windows(64)
            .any(|window| window.iter().all(|byte| byte.is_ascii_hexdigit())),
        "credential-shaped material leaked into the output"
    );
    assert!(!flat.contains("Facto-CLI"), "the proof scheme leaked");
    let characters: Vec<char> = flat.chars().collect();
    for window in characters.windows(11) {
        let shape = window[5] == '-'
            && window
                .iter()
                .enumerate()
                .all(|(offset, c)| offset == 5 || (c.is_ascii_uppercase() && !"IO01".contains(*c)));
        assert!(!shape, "a pairing code shape leaked");
    }
}

#[test]
fn dev_target_is_refused_before_any_local_store_or_network() {
    for command in ["authorize", "session", "check-access", "logout"] {
        let home = sandbox(&command.replace('-', "_"));
        let output = run(
            &home,
            &[
                "-t", "demo", command, "--demo", "shop", "--target", "dev", "--mode", "fresh",
            ],
        );
        assert!(!output.status.success(), "{command} must fail on dev");
        let value = parsed(&output);
        assert_eq!(value["error"], "target_unsupported", "{command}");
        assert_eq!(value["authentication_verified"], false);
        assert_eq!(value["uat_status"], "NOT_RUN");
        assert_eq!(value["consumer_state_accessed"], false);
        assert_eq!(value["app_secret_transport"], false);
        assert!(
            !store_dir(&home).exists(),
            "{command} created a private store"
        );
        assert_never_reaches_network_or_state(&output);
    }
}

#[test]
fn empty_store_reads_as_missing_without_network() {
    for command in ["session", "check-access", "logout"] {
        let home = sandbox(&format!("empty_{}", command.replace('-', "_")));
        let output = run(
            &home,
            &[
                "-t", "demo", command, "--demo", "shop", "--target", "test", "--mode", "reuse",
            ],
        );
        assert!(
            !output.status.success(),
            "{command} must fail with no pairing"
        );
        let value = parsed(&output);
        assert_eq!(value["error"], "journal_missing", "{command}");
        assert_eq!(value["authentication_verified"], false, "{command}");
        assert_eq!(value["uat_status"], "NOT_RUN");
        assert_never_reaches_network_or_state(&output);
    }
}

#[test]
fn insecure_store_is_refused_without_a_post() {
    let home = sandbox("insecure");
    let dir = store_dir(&home);
    fs::create_dir_all(&dir).expect("store");
    fs::set_permissions(&dir, Permissions::from_mode(0o755)).expect("loosen store");
    for command in ["session", "check-access", "logout", "authorize"] {
        let output = run(
            &home,
            &[
                "-t", "demo", command, "--demo", "shop", "--target", "test", "--mode", "reuse",
            ],
        );
        assert!(
            !output.status.success(),
            "{command} must refuse an open store"
        );
        let value = parsed(&output);
        assert_eq!(value["error"], "unsafe_local_state", "{command}");
        assert_never_reaches_network_or_state(&output);
    }
}

#[test]
fn symlinked_store_is_refused() {
    let home = sandbox("symlink");
    let real = home.join("elsewhere");
    fs::create_dir_all(&real).expect("real");
    fs::set_permissions(&real, Permissions::from_mode(0o700)).expect("real mode");
    let facto = home.join(".facto");
    fs::create_dir_all(facto.join("partner-cli")).expect("parents");
    std::os::unix::fs::symlink(&real, facto.join("partner-cli").join("test")).expect("symlink");
    for command in ["session", "check-access", "logout", "authorize"] {
        let output = run(
            &home,
            &[
                "-t", "demo", command, "--demo", "shop", "--target", "test", "--mode", "reuse",
            ],
        );
        assert!(
            !output.status.success(),
            "{command} must refuse a linked store"
        );
        let value = parsed(&output);
        assert_eq!(value["error"], "unsafe_local_state", "{command}");
        assert_never_reaches_network_or_state(&output);
    }
}

#[test]
fn a_non_terminal_authorize_refuses_before_any_post_or_journal() {
    // These tests run with piped stdio, so neither stream is a terminal. The
    // pairing therefore has to refuse before touching the store or the wire.
    let home = sandbox("no tty");
    for mode in ["fresh", "reuse"] {
        let output = run(
            &home,
            &[
                "-t",
                "demo",
                "authorize",
                "--demo",
                "shop",
                "--target",
                "test",
                "--mode",
                mode,
            ],
        );
        assert!(
            !output.status.success(),
            "authorize must refuse a non-terminal"
        );
        let value = parsed(&output);
        assert_eq!(value["error"], "pairing_requires_terminal");
        assert_eq!(value["authentication_verified"], false);
        assert_eq!(value["stage"], "precheck", "no stage was reached");
        assert!(
            !store_dir(&home).exists(),
            "a refused pairing creates no private store"
        );
        assert_never_reaches_network_or_state(&output);
        let both = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!both.contains("Pairing code"), "{both}");
    }
}

#[test]
fn pairing_hints_are_absent_from_every_non_terminal_channel() {
    let home = sandbox("hint fence");
    for command in ["session", "check-access", "logout"] {
        let output = run(
            &home,
            &[
                "-t", "demo", command, "--demo", "shop", "--target", "test", "--mode", "reuse",
            ],
        );
        assert!(!output.status.success(), "{command} refuses an empty store");
        let both = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !both.contains("Pairing code"),
            "{command} leaked a pairing prompt"
        );
        assert!(
            !both.contains("Facto-CLI"),
            "{command} leaked the proof scheme"
        );
        assert_never_reaches_network_or_state(&output);
    }
}
#[test]
fn argument_surface_is_closed_and_explicit() {
    let home = sandbox("args");
    // `--target dev` is the only safe way to exercise the accepted surface: the
    // Test selection of these commands would issue a real request.
    for command in ["authorize", "session", "check-access", "logout"] {
        let output = run(
            &home,
            &[
                "-t", "demo", command, "--demo", "shop", "--target", "dev", "--mode", "reuse",
            ],
        );
        assert!(!output.status.success(), "{command} parses its selection");
        assert_eq!(parsed(&output)["error"], "target_unsupported", "{command}");
    }
    for args in [
        vec!["demo", "authorize", "--demo", "shop", "--target", "test"],
        vec!["demo", "authorize", "--demo", "shop", "--mode", "fresh"],
        vec!["demo", "authorize", "--target", "test", "--mode", "fresh"],
        vec![
            "demo", "session", "--demo", "orbit", "--target", "test", "--mode", "fresh",
        ],
        vec![
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
            "demo",
            "check-access",
            "--demo",
            "shop",
            "--target",
            "test",
            "--mode",
            "reused",
        ],
        vec![
            "demo",
            "authorize",
            "--demo",
            "shop",
            "--target",
            "test",
            "--mode",
            "fresh",
            "--demo-url",
            "https://shop.example",
        ],
        vec![
            "demo", "plan", "--demo", "shop", "--target", "test", "--mode", "fresh", "--extra", "1",
        ],
    ] {
        let output = run(&home, &args);
        assert!(!output.status.success(), "{args:?} must be rejected");
        assert_never_reaches_network_or_state(&output);
    }
}

#[test]
fn human_output_names_the_handoff_and_never_a_credential() {
    let home = sandbox("human");
    let output = run(
        &home,
        &[
            "demo", "session", "--demo", "shop", "--target", "test", "--mode", "reuse",
        ],
    );
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        text.contains("Partner CLI handoff partner_cli_session"),
        "{text}"
    );
    assert!(text.contains("Business UAT: NOT_RUN"), "{text}");
    assert!(text.contains("No App secret"), "{text}");
    assert!(!text.contains("Pairing code"), "{text}");
    assert_never_reaches_network_or_state(&output);
}

#[test]
fn plan_and_check_keep_their_configuration_only_meaning() {
    let home = sandbox("regression");
    let plan = run(
        &home,
        &[
            "-t", "demo", "plan", "--demo", "shop", "--target", "test", "--mode", "fresh",
        ],
    );
    assert!(plan.status.success(), "plan must still pass");
    let value = parsed(&plan);
    assert_eq!(value["kind"], "shop_demo_plan");
    assert_eq!(value["status"], "prepared");
    assert_eq!(value["scope"], "configuration_only");
    assert_eq!(value["authentication_verified"], false);
    assert_eq!(value["uat_status"], "NOT_RUN");
    assert!(
        !store_dir(&home).exists(),
        "plan must not touch the pairing store"
    );

    let check = run(
        &home,
        &[
            "-t",
            "demo",
            "check",
            "--demo",
            "shop",
            "--target",
            "test",
            "--mode",
            "reuse",
            "--demo-url",
            "http://shop.example",
        ],
    );
    assert!(
        !check.status.success(),
        "an http origin must still be refused"
    );
    let value = parsed(&check);
    assert_eq!(value["kind"], "shop_demo_configuration_check");
    assert_eq!(value["error"], "demo_origin_invalid");
    assert_eq!(value["authentication_verified"], false);
    assert!(
        !store_dir(&home).exists(),
        "check must not touch the pairing store"
    );
    assert_never_reaches_network_or_state(&check);
}

#[test]
fn consumer_state_is_never_created_or_read() {
    let home = sandbox("consumer");
    for command in ["session", "check-access", "logout"] {
        let output = run(
            &home,
            &[
                "-t", "demo", command, "--demo", "shop", "--target", "test", "--mode", "reuse",
            ],
        );
        assert!(
            !output.status.success(),
            "{command} still refuses an empty store"
        );
        let value = parsed(&output);
        assert_eq!(value["consumer_state_accessed"], false, "{command}");
    }
    for forbidden in ["credentials.json", "config.json"] {
        assert!(
            !home.join(".facto").join(forbidden).exists(),
            "{forbidden} must not be created by the handoff commands"
        );
    }
}
