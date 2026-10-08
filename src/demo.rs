//! Shop setup planning and public configuration checks.
//! This module never uses Consumer authentication or claims business acceptance.

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use reqwest::{header, redirect, Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::{fmt, future::Future, time::Duration};

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const SHOP_SOURCE: &str = "9e44a08d04845f6db1d108f58629374ddbd331ab";
const SDK_VERSION: &str = "0.0.1-beta.5";

#[derive(Debug, Subcommand)]
pub(super) enum Command {
    /// Prepare an offline Shop setup plan; does not create or authenticate anything.
    Plan {
        #[command(flatten)]
        selection: Selection,
    },
    /// Read only the Shop public configuration; does not verify access or UAT.
    Check {
        #[command(flatten)]
        selection: Selection,
        /// Shop HTTPS origin, for example https://shop.example (no path or credentials).
        #[arg(long)]
        demo_url: String,
    },
}

#[derive(Debug, Args)]
pub(super) struct Selection {
    /// Demo adapter. This first slice supports Shop only.
    #[arg(long, value_enum)]
    demo: Demo,
    /// Deployment target; never taken from Consumer configuration or credentials.
    #[arg(long, value_enum)]
    target: Target,
    /// New onboarding or reuse of an existing authorized setup; neither is executed here.
    #[arg(long, value_enum)]
    mode: Mode,
}

#[derive(Debug, Clone, Copy, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum Demo {
    Shop,
}

#[derive(Debug, Clone, Copy, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum Target {
    Test,
    Dev,
}

#[derive(Debug, Clone, Copy, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Fresh,
    Reuse,
}

impl Target {
    fn origins(self) -> Origins {
        match self {
            Self::Test => Origins {
                api: "https://test-api.facto.to",
                hosted: "https://test.facto.to",
                startup: "npm run dev:facto-test",
            },
            Self::Dev => Origins {
                api: "https://dev-api.facto.to",
                hosted: "https://dev.facto.to",
                startup: "npm run dev:facto-dev",
            },
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Dev => "dev",
        }
    }
}

#[derive(Serialize)]
struct Origins {
    api: &'static str,
    hosted: &'static str,
    startup: &'static str,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PublicConfiguration {
    app_id: String,
    facto_environment: PublicEnvironment,
    client_configured: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PublicEnvironment {
    name: String,
    api_base_url: String,
    hosted_connect_url: String,
}

#[derive(Serialize)]
struct Report {
    kind: &'static str,
    demo: Demo,
    target: Target,
    mode: Mode,
    status: &'static str,
    scope: &'static str,
    origins: Origins,
    expected_shop_source: &'static str,
    expected_sdk_version: &'static str,
    installation_verified: bool,
    authentication_verified: bool,
    uat_status: &'static str,
    steps: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    configuration: Option<PublicConfiguration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DemoError {
    InvalidOrigin,
    TransportUnavailable,
    HttpRejected,
    ResponseTooLarge,
    ConfigurationInvalid,
    TargetMismatch,
    ClientMissing,
}

impl DemoError {
    fn code(self) -> &'static str {
        match self {
            Self::InvalidOrigin => "demo_origin_invalid",
            Self::TransportUnavailable => "demo_transport_unavailable",
            Self::HttpRejected => "demo_http_rejected",
            Self::ResponseTooLarge => "demo_response_too_large",
            Self::ConfigurationInvalid => "demo_configuration_invalid",
            Self::TargetMismatch => "demo_target_mismatch",
            Self::ClientMissing => "demo_client_missing",
        }
    }
}

impl fmt::Display for DemoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for DemoError {}

fn report(selection: &Selection, kind: &'static str, status: &'static str) -> Report {
    Report {
        kind,
        demo: selection.demo,
        target: selection.target,
        mode: selection.mode,
        status,
        scope: "configuration_only",
        origins: selection.target.origins(),
        expected_shop_source: SHOP_SOURCE,
        expected_sdk_version: SDK_VERSION,
        installation_verified: false,
        authentication_verified: false,
        uat_status: "NOT_RUN",
        steps: Vec::new(),
        configuration: None,
        error: None,
    }
}

fn plan(selection: &Selection) -> Report {
    let mut output = report(selection, "shop_demo_plan", "prepared");
    output.steps = match selection.mode {
        Mode::Fresh => vec![
            "Owner: complete email verification and MFA in the selected Partner browser.",
            "Owner: explicitly create the organization and invite a second Developer.",
            "Developer: authenticate independently and accept the directed invitation.",
            "Developer: select the authorized application and obtain its server configuration.",
            "Install the fixed Shop SDK artifact and start Shop with the target startup command.",
            "Run demo check, then perform authenticated browser and callback UAT separately.",
        ],
        Mode::Reuse => vec![
            "Use the existing organization, membership and application in this target only.",
            "Recheck current membership and application permissions in the Partner browser.",
            "Read back any original pending operation; do not replay an unknown result.",
            "Reuse the fixed Shop SDK artifact and start Shop with the target startup command.",
            "Run demo check, then perform authenticated browser and callback UAT separately.",
        ],
    };
    output
}

fn configuration_url(origin: &str) -> std::result::Result<Url, DemoError> {
    // Reject normalization tricks (including /a/..), not just the parsed path.
    let authority = origin
        .strip_prefix("https://")
        .ok_or(DemoError::InvalidOrigin)?;
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    if authority.is_empty()
        || authority.contains(['/', '?', '#', '\\', '@'])
        || origin.trim() != origin
        || origin.chars().any(char::is_control)
    {
        return Err(DemoError::InvalidOrigin);
    }
    let mut url = Url::parse(origin).map_err(|_| DemoError::InvalidOrigin)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(DemoError::InvalidOrigin);
    }
    url.set_path("/api/facto/config");
    Ok(url)
}

struct WireResponse {
    status: StatusCode,
    content_type: Option<String>,
    body: Vec<u8>,
}

fn request(url: Url) -> std::result::Result<(Client, reqwest::Request), DemoError> {
    let client = Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| DemoError::TransportUnavailable)?;
    let request = client
        .get(url)
        .header(header::ACCEPT, "application/json")
        .build()
        .map_err(|_| DemoError::TransportUnavailable)?;
    Ok((client, request))
}

async fn fetch_configuration(url: Url) -> std::result::Result<WireResponse, DemoError> {
    let (client, request) = request(url)?;
    let mut response = client
        .execute(request)
        .await
        .map_err(|_| DemoError::TransportUnavailable)?;
    let status = response.status();
    if status != StatusCode::OK {
        // No body is read or emitted on HTTP errors, including redirects.
        return Err(DemoError::HttpRejected);
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err(DemoError::ResponseTooLarge);
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| DemoError::TransportUnavailable)?
    {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(DemoError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(WireResponse {
        status,
        content_type,
        body,
    })
}

fn validate_configuration(
    target: Target,
    wire: WireResponse,
) -> std::result::Result<PublicConfiguration, DemoError> {
    if wire.status != StatusCode::OK {
        return Err(DemoError::HttpRejected);
    }
    if wire.body.len() > MAX_RESPONSE_BYTES {
        return Err(DemoError::ResponseTooLarge);
    }
    let content_type = wire.content_type.ok_or(DemoError::ConfigurationInvalid)?;
    if content_type.split(';').next().map(str::trim) != Some("application/json") {
        return Err(DemoError::ConfigurationInvalid);
    }
    // Unknown fields are rejected. Never serialize an untrusted whole payload
    // or include a deserializer/transport error in CLI output.
    let config: PublicConfiguration =
        serde_json::from_slice(&wire.body).map_err(|_| DemoError::ConfigurationInvalid)?;
    if config.app_id.is_empty()
        || config.app_id.len() > 256
        || !config
            .app_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    {
        return Err(DemoError::ConfigurationInvalid);
    }
    let origins = target.origins();
    let matching_name = match target {
        Target::Test => config.facto_environment.name == "test",
        Target::Dev => matches!(config.facto_environment.name.as_str(), "dev" | "sandbox"),
    };
    if !matching_name
        || config.facto_environment.api_base_url != origins.api
        || config.facto_environment.hosted_connect_url != origins.hosted
    {
        return Err(DemoError::TargetMismatch);
    }
    if !config.client_configured {
        return Err(DemoError::ClientMissing);
    }
    Ok(config)
}

// The test seam supplies a response, never a session or weaker TLS policy.
async fn check_with_fetch<F, Fut>(
    selection: &Selection,
    origin: &str,
    fetch: F,
) -> std::result::Result<PublicConfiguration, DemoError>
where
    F: FnOnce(Url) -> Fut,
    Fut: Future<Output = std::result::Result<WireResponse, DemoError>>,
{
    let url = configuration_url(origin)?;
    validate_configuration(selection.target, fetch(url).await?)
}

fn render(output: &Report, terse: bool) -> Result<String> {
    if terse {
        return Ok(serde_json::to_string(output)?);
    }
    let mut text = format!(
        "Shop demo {}: {} (target: {}, mode: {}).\n",
        if output.kind == "shop_demo_plan" {
            "plan"
        } else {
            "configuration check"
        },
        output.status,
        output.target.name(),
        match output.mode {
            Mode::Fresh => "fresh",
            Mode::Reuse => "reuse",
        },
    );
    if let Some(error) = output.error {
        text.push_str(&format!("Reason: {error}.\n"));
    }
    text.push_str(&format!(
        "API: {}\nHosted: {}\nStartup: {}\nExpected SDK: {}\n",
        output.origins.api,
        output.origins.hosted,
        output.origins.startup,
        output.expected_sdk_version,
    ));
    if let Some(config) = &output.configuration {
        text.push_str(&format!(
            "Public app ID: {}\nClient configured: {}\n",
            config.app_id, config.client_configured,
        ));
    }
    for (index, step) in output.steps.iter().enumerate() {
        text.push_str(&format!("{}. {step}\n", index + 1));
    }
    text.push_str("Configuration only. Authentication, installed/served versions and UAT remain unverified.\n");
    Ok(text)
}

pub(super) async fn run(command: &Command, terse: bool) -> Result<()> {
    let (output, failure) = match command {
        Command::Plan { selection } => (plan(selection), None),
        Command::Check {
            selection,
            demo_url,
        } => {
            let mut output = report(
                selection,
                "shop_demo_configuration_check",
                "configuration_checked",
            );
            match check_with_fetch(selection, demo_url, fetch_configuration).await {
                Ok(config) => {
                    output.configuration = Some(config);
                    (output, None)
                }
                Err(error) => {
                    output.status = "failed";
                    output.error = Some(error.code());
                    (output, Some(error))
                }
            }
        }
    };
    println!("{}", render(&output, terse)?);
    match failure {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use reqwest::Method;
    use serde_json::{json, Value};

    fn selection(target: Target, mode: Mode) -> Selection {
        Selection {
            demo: Demo::Shop,
            target,
            mode,
        }
    }

    fn payload(target: Target) -> Value {
        let origins = target.origins();
        json!({
            "appId": "mapp_example",
            "factoEnvironment": {
                "name": target.name(),
                "apiBaseUrl": origins.api,
                "hostedConnectUrl": origins.hosted,
            },
            "clientConfigured": true,
        })
    }

    fn wire(value: Value) -> WireResponse {
        WireResponse {
            status: StatusCode::OK,
            content_type: Some("application/json; charset=utf-8".into()),
            body: serde_json::to_vec(&value).expect("fixture JSON"),
        }
    }

    #[test]
    fn arguments_require_explicit_target_mode_and_shop() {
        for args in [
            vec!["facto", "demo", "plan"],
            vec![
                "facto", "demo", "plan", "--demo", "shop", "--target", "test",
            ],
            vec!["facto", "demo", "plan", "--demo", "shop", "--mode", "fresh"],
            vec![
                "facto", "demo", "plan", "--demo", "orbit", "--target", "test", "--mode", "fresh",
            ],
            vec![
                "facto",
                "demo",
                "plan",
                "--demo",
                "shop",
                "--target",
                "production",
                "--mode",
                "fresh",
            ],
            vec![
                "facto", "demo", "check", "--demo", "shop", "--target", "test", "--mode", "fresh",
            ],
        ] {
            assert!(crate::Cli::try_parse_from(args).is_err());
        }
        for target in ["test", "dev"] {
            for mode in ["fresh", "reuse"] {
                let parsed = crate::Cli::try_parse_from([
                    "facto", "demo", "plan", "--demo", "shop", "--target", target, "--mode", mode,
                ])
                .expect("valid selection");
                assert!(matches!(parsed.command, crate::Commands::Demo { .. }));
            }
        }
    }

    #[test]
    fn plans_are_safe_english_and_explicitly_not_acceptance() {
        let output = plan(&selection(Target::Test, Mode::Fresh));
        let value: Value =
            serde_json::from_str(&render(&output, true).expect("JSON")).expect("report");
        assert_eq!(value["status"], "prepared");
        assert_eq!(value["origins"]["api"], "https://test-api.facto.to");
        assert_eq!(value["expected_sdk_version"], SDK_VERSION);
        assert_eq!(value["uat_status"], "NOT_RUN");
        assert_eq!(value["authentication_verified"], false);
        assert_eq!(value["installation_verified"], false);
        assert!(value.get("configuration").is_none());
        let human = render(&output, false).expect("human");
        assert!(human.starts_with("Shop demo plan: prepared (target: test, mode: fresh)."));
        assert!(human.contains("authenticate independently"));
        assert!(human.contains("UAT remain unverified"));
        assert!(!human.contains("PASS"));
        let reuse = render(&plan(&selection(Target::Dev, Mode::Reuse)), false).expect("reuse");
        assert!(reuse.contains("do not replay an unknown result"));
        assert!(reuse.contains("npm run dev:facto-dev"));
    }

    #[test]
    fn only_literal_https_origins_are_accepted() {
        for origin in [
            "http://shop.example",
            "https://user:secret@shop.example",
            "https://user@shop.example",
            "https://@shop.example",
            "https://shop.example/path",
            "https://shop.example/a/..",
            "https://shop.example//",
            "https://shop.example?token=secret",
            "https://shop.example#secret",
            " https://shop.example",
            "https://shop.example\\path",
        ] {
            assert_eq!(
                configuration_url(origin).unwrap_err(),
                DemoError::InvalidOrigin
            );
        }
        for origin in [
            "https://shop.example",
            "https://shop.example/",
            "https://shop.example:8443",
        ] {
            let url = configuration_url(origin).expect("origin");
            assert_eq!(url.path(), "/api/facto/config");
            assert_eq!(url.scheme(), "https");
            assert_eq!(url.query(), None);
        }
    }

    #[test]
    fn public_get_request_contains_no_cookie_or_authorization() {
        let (_, request) =
            request(configuration_url("https://shop.example").expect("origin")).expect("request");
        assert_eq!(request.method(), Method::GET);
        assert_eq!(request.url().path(), "/api/facto/config");
        assert_eq!(
            request.headers().get(header::ACCEPT).expect("accept"),
            "application/json"
        );
        assert!(!request.headers().contains_key(header::AUTHORIZATION));
        assert!(!request.headers().contains_key(header::COOKIE));
        assert!(!request.headers().contains_key(header::PROXY_AUTHORIZATION));
        assert!(request.body().is_none());
    }

    #[test]
    fn target_and_required_configuration_fail_closed() {
        assert!(validate_configuration(Target::Test, wire(payload(Target::Test))).is_ok());
        assert!(validate_configuration(Target::Dev, wire(payload(Target::Dev))).is_ok());
        let mut sandbox = payload(Target::Dev);
        sandbox["factoEnvironment"]["name"] = json!("sandbox");
        assert!(validate_configuration(Target::Dev, wire(sandbox)).is_ok());
        assert_eq!(
            validate_configuration(Target::Test, wire(payload(Target::Dev))).unwrap_err(),
            DemoError::TargetMismatch
        );
        for field in ["name", "apiBaseUrl", "hostedConnectUrl"] {
            let mut mismatch = payload(Target::Test);
            mismatch["factoEnvironment"][field] = json!("unexpected");
            assert_eq!(
                validate_configuration(Target::Test, wire(mismatch)).unwrap_err(),
                DemoError::TargetMismatch
            );
        }
        let mut missing = payload(Target::Test);
        missing.as_object_mut().expect("object").remove("appId");
        assert_eq!(
            validate_configuration(Target::Test, wire(missing)).unwrap_err(),
            DemoError::ConfigurationInvalid
        );
        let mut incomplete = payload(Target::Test);
        incomplete["clientConfigured"] = json!(false);
        assert_eq!(
            validate_configuration(Target::Test, wire(incomplete)).unwrap_err(),
            DemoError::ClientMissing
        );
    }

    #[test]
    fn unknown_fields_and_secret_bearing_payloads_are_never_projected() {
        for extra in ["secret", "realm", "credentials", "token"] {
            let mut unknown = payload(Target::Test);
            unknown[extra] = json!("do-not-display-me");
            let error = validate_configuration(Target::Test, wire(unknown)).unwrap_err();
            assert_eq!(error, DemoError::ConfigurationInvalid);
            assert!(!error.to_string().contains("do-not-display-me"));
        }
        let mut nested = payload(Target::Test);
        nested["factoEnvironment"]["secret"] = json!("do-not-display-me");
        assert_eq!(
            validate_configuration(Target::Test, wire(nested)).unwrap_err(),
            DemoError::ConfigurationInvalid
        );
        let mut unsafe_id = payload(Target::Test);
        unsafe_id["appId"] = json!("app\nsecret");
        assert_eq!(
            validate_configuration(Target::Test, wire(unsafe_id)).unwrap_err(),
            DemoError::ConfigurationInvalid
        );
    }

    #[test]
    fn http_errors_content_type_size_and_invalid_json_have_safe_errors() {
        let mut redirect = wire(json!({"secret":"do-not-display"}));
        redirect.status = StatusCode::FOUND;
        assert_eq!(
            validate_configuration(Target::Test, redirect).unwrap_err(),
            DemoError::HttpRejected
        );
        let mut html = wire(payload(Target::Test));
        html.content_type = Some("text/html".into());
        assert_eq!(
            validate_configuration(Target::Test, html).unwrap_err(),
            DemoError::ConfigurationInvalid
        );
        let too_large = WireResponse {
            status: StatusCode::OK,
            content_type: Some("application/json".into()),
            body: vec![b'x'; MAX_RESPONSE_BYTES + 1],
        };
        assert_eq!(
            validate_configuration(Target::Test, too_large).unwrap_err(),
            DemoError::ResponseTooLarge
        );
        let invalid = WireResponse {
            status: StatusCode::OK,
            content_type: Some("application/json".into()),
            body: b"raw secret error".to_vec(),
        };
        assert_eq!(
            validate_configuration(Target::Test, invalid).unwrap_err(),
            DemoError::ConfigurationInvalid
        );
    }

    #[tokio::test]
    async fn injected_transport_uses_the_same_origin_and_cannot_make_invalid_input_a_request() {
        let selected = selection(Target::Test, Mode::Reuse);
        let config = check_with_fetch(&selected, "https://shop.example", |url| async move {
            assert_eq!(url.as_str(), "https://shop.example/api/facto/config");
            Ok(wire(payload(Target::Test)))
        })
        .await
        .expect("configuration");
        assert_eq!(config.app_id, "mapp_example");
        let called = std::sync::atomic::AtomicBool::new(false);
        let result = check_with_fetch(&selected, "http://shop.example", |_| async {
            called.store(true, std::sync::atomic::Ordering::SeqCst);
            Err(DemoError::HttpRejected)
        })
        .await;
        assert_eq!(result.unwrap_err(), DemoError::InvalidOrigin);
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        let error = check_with_fetch(&selected, "https://shop.example", |_| async {
            Err(DemoError::TransportUnavailable)
        })
        .await
        .unwrap_err();
        assert_eq!(error, DemoError::TransportUnavailable);
    }
}
