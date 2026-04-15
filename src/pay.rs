use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::api::FactoApi;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolMode {
    Auto,
    X402,
    Mpp,
}

impl ProtocolMode {
    pub fn parse(raw: Option<&str>) -> Result<Self> {
        match raw.unwrap_or("auto") {
            "auto" => Ok(Self::Auto),
            "x402" => Ok(Self::X402),
            "mpp" => Ok(Self::Mpp),
            other => bail!("unsupported protocol: {other}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::X402 => "x402",
            Self::Mpp => "mpp",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PayResolveRequest {
    pub url: String,
    pub method: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub max_amount: Option<String>,
    #[serde(default)]
    pub chain_id: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PayResolveResponse {
    pub requires_payment: bool,
    pub protocol: String,
    #[serde(default)]
    pub payment_method: Option<String>,
    #[serde(default)]
    pub chain_id: Option<u64>,
    #[serde(default)]
    pub asset: Option<String>,
    #[serde(default)]
    pub quoted_amount: Option<String>,
    #[serde(default)]
    pub compatible_pipeline_ids: Vec<String>,
    #[serde(default)]
    pub recommended_execution_pipeline_id: Option<String>,
    #[serde(default)]
    pub create_pipeline_url: Option<String>,
    #[serde(default)]
    pub can_auto_fund: Option<bool>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionDecision {
    pub execution_pipeline_id: String,
    pub default_pipeline_changed: bool,
}

pub async fn resolve_payment(
    api: &FactoApi,
    token: &str,
    request: &PayResolveRequest,
) -> Result<PayResolveResponse> {
    api.post_authenticated("/v1/pay/resolve", request, token)
        .await
}

pub fn choose_execution_pipeline(
    selected_pipeline_id: Option<&str>,
    compatible_pipeline_ids: &[String],
    recommended_execution_pipeline_id: Option<&str>,
) -> Result<ExecutionDecision> {
    if let Some(selected) = selected_pipeline_id {
        if compatible_pipeline_ids.iter().any(|id| id == selected) {
            return Ok(ExecutionDecision {
                execution_pipeline_id: selected.to_string(),
                default_pipeline_changed: false,
            });
        }
    }

    if let Some(recommended) = recommended_execution_pipeline_id {
        if compatible_pipeline_ids.iter().any(|id| id == recommended) {
            return Ok(ExecutionDecision {
                execution_pipeline_id: recommended.to_string(),
                default_pipeline_changed: false,
            });
        }
    }

    if compatible_pipeline_ids.len() == 1 {
        return Ok(ExecutionDecision {
            execution_pipeline_id: compatible_pipeline_ids[0].clone(),
            default_pipeline_changed: false,
        });
    }

    if compatible_pipeline_ids.is_empty() {
        bail!("no compatible execution pipeline available");
    }

    bail!("multiple compatible execution pipelines available");
}

pub fn temporary_pipeline_notice(
    selected_pipeline_id: Option<&str>,
    execution_pipeline_id: &str,
) -> Option<String> {
    match selected_pipeline_id {
        Some(selected) if selected != execution_pipeline_id => Some(format!(
            "Using compatible pipeline {execution_pipeline_id} for this payment. Default pipeline remains {selected}."
        )),
        _ => None,
    }
}

pub fn payment_endpoint(protocol: &str) -> Option<&'static str> {
    match protocol {
        "x402" => Some("/v1/x402/pay"),
        "mpp" => Some("/v1/mpp/pay"),
        _ => None,
    }
}

pub fn explain_resolve_reason(reason: Option<&str>) -> Option<&'static str> {
    match reason {
        Some("no_compatible_pipeline") => Some("No compatible pipeline is currently available."),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_protocol_mode_defaults_and_overrides() {
        assert!(matches!(
            ProtocolMode::parse(None).unwrap(),
            ProtocolMode::Auto
        ));
        assert!(matches!(
            ProtocolMode::parse(Some("x402")).unwrap(),
            ProtocolMode::X402
        ));
        assert!(matches!(
            ProtocolMode::parse(Some("mpp")).unwrap(),
            ProtocolMode::Mpp
        ));
    }

    #[test]
    fn maps_payment_endpoints_by_protocol() {
        assert_eq!(payment_endpoint("x402"), Some("/v1/x402/pay"));
        assert_eq!(payment_endpoint("mpp"), Some("/v1/mpp/pay"));
        assert_eq!(payment_endpoint("none"), None);
    }

    #[test]
    fn chooses_selected_pipeline_when_compatible() {
        let selected = Some("route-base");
        let compatible = vec!["route-base".to_string(), "route-monad".to_string()];

        let decision = choose_execution_pipeline(selected, &compatible, None).unwrap();

        assert_eq!(decision.execution_pipeline_id, "route-base");
        assert!(!decision.default_pipeline_changed);
    }

    #[test]
    fn chooses_single_compatible_pipeline_when_selected_is_not_compatible() {
        let selected = Some("route-base");
        let compatible = vec!["route-monad".to_string()];

        let decision = choose_execution_pipeline(selected, &compatible, None).unwrap();

        assert_eq!(decision.execution_pipeline_id, "route-monad");
        assert!(!decision.default_pipeline_changed);
    }

    #[test]
    fn uses_recommended_pipeline_when_available() {
        let selected = Some("route-base");
        let compatible = vec!["route-monad-a".to_string(), "route-monad-b".to_string()];

        let decision =
            choose_execution_pipeline(selected, &compatible, Some("route-monad-b")).unwrap();

        assert_eq!(decision.execution_pipeline_id, "route-monad-b");
    }

    #[test]
    fn reports_temporary_pipeline_notice_only_when_execution_differs() {
        assert_eq!(
            temporary_pipeline_notice(Some("route-base"), "route-monad").as_deref(),
            Some("Using compatible pipeline route-monad for this payment. Default pipeline remains route-base.")
        );
        assert!(temporary_pipeline_notice(Some("route-base"), "route-base").is_none());
    }

    #[test]
    fn explains_no_compatible_pipeline_reason() {
        assert_eq!(
            explain_resolve_reason(Some("no_compatible_pipeline")),
            Some("No compatible pipeline is currently available.")
        );
    }
}
