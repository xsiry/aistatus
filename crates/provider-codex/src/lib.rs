use std::collections::BTreeMap;

use aistatus_core::{
    AccountKind, ProtocolAccountIdentity, ProtocolRateLimitWindow, QuotaSnapshot, UsageFamily,
};
use serde::Deserialize;
use thiserror::Error;

pub const SUPPORTED_SCHEMA_VERSION: &str = "v2";

#[derive(Debug, Clone, PartialEq)]
pub struct CodexProviderSnapshot {
    pub identity: ProtocolAccountIdentity,
    pub quota_snapshot: QuotaSnapshot,
    pub usage_family: UsageFamily,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CodexProviderError {
    #[error("transport failure: {0}")]
    Transport(String),
    #[error("authentication failure: {0}")]
    Auth(String),
    #[error("schema incompatibility: expected {expected}, got {found}")]
    IncompatibleSchema { expected: String, found: String },
    #[error("invalid payload: {0}")]
    InvalidPayload(String),
}

pub struct CodexProtocolAdapter;

impl CodexProtocolAdapter {
    pub fn from_json(
        account_json: &str,
        rate_limits_json: &str,
    ) -> Result<CodexProviderSnapshot, CodexProviderError> {
        let account_response: AccountEnvelope =
            serde_json::from_str(account_json).map_err(|error| {
                CodexProviderError::InvalidPayload(format!("account payload: {error}"))
            })?;
        let rate_limit_response: RateLimitsEnvelope = serde_json::from_str(rate_limits_json)
            .map_err(|error| {
                CodexProviderError::InvalidPayload(format!("rate limit payload: {error}"))
            })?;

        ensure_supported_schema(account_response.schema_version.as_deref())?;
        ensure_supported_schema(rate_limit_response.schema_version.as_deref())?;

        if let Some(error) = account_response
            .error
            .clone()
            .or(rate_limit_response.error.clone())
        {
            return Err(classify_error(error));
        }

        let account = account_response
            .account
            .ok_or_else(|| CodexProviderError::InvalidPayload("missing account object".into()))?;

        let identity = to_protocol_identity(account, &rate_limit_response)?;
        let windows = extract_windows(rate_limit_response);
        let quota_snapshot = QuotaSnapshot::from_protocol_windows(windows);

        Ok(CodexProviderSnapshot {
            identity,
            quota_snapshot,
            usage_family: UsageFamily::SubscriptionQuota,
        })
    }
}

fn ensure_supported_schema(found: Option<&str>) -> Result<(), CodexProviderError> {
    let found = found.unwrap_or(SUPPORTED_SCHEMA_VERSION);
    if found == SUPPORTED_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(CodexProviderError::IncompatibleSchema {
            expected: SUPPORTED_SCHEMA_VERSION.into(),
            found: found.into(),
        })
    }
}

fn classify_error(error: ProtocolErrorPayload) -> CodexProviderError {
    match error.code.as_str() {
        "auth_required" | "session_expired" | "unauthorized" => {
            CodexProviderError::Auth(error.message)
        }
        _ => CodexProviderError::Transport(error.message),
    }
}

fn to_protocol_identity(
    account: ProtocolAccount,
    rate_limits: &RateLimitsEnvelope,
) -> Result<ProtocolAccountIdentity, CodexProviderError> {
    match account {
        ProtocolAccount::Chatgpt { email, plan_type } => {
            let effective_plan = plan_type.or_else(|| find_snapshot_plan_type(rate_limits));
            Ok(ProtocolAccountIdentity {
                account_id: email.clone(),
                display_name: email,
                account_kind: AccountKind::Chatgpt,
                plan_type: effective_plan,
            })
        }
        ProtocolAccount::ApiKey => Ok(ProtocolAccountIdentity {
            account_id: "api-key".into(),
            display_name: "API Key".into(),
            account_kind: AccountKind::ApiKey,
            plan_type: None,
        }),
    }
}

fn find_snapshot_plan_type(rate_limits: &RateLimitsEnvelope) -> Option<String> {
    rate_limits
        .rate_limits
        .as_ref()
        .and_then(|snapshots| {
            snapshots
                .iter()
                .find_map(|snapshot| snapshot.plan_type.clone())
        })
        .or_else(|| {
            rate_limits
                .rate_limits_by_limit_id
                .as_ref()
                .and_then(|buckets| {
                    buckets
                        .values()
                        .find_map(|snapshot| snapshot.plan_type.clone())
                })
        })
}

fn extract_windows(response: RateLimitsEnvelope) -> Vec<ProtocolRateLimitWindow> {
    let mut snapshots = response.rate_limits.unwrap_or_default();

    if let Some(by_id) = response.rate_limits_by_limit_id {
        for (limit_id, snapshot) in by_id {
            if !snapshots
                .iter()
                .any(|candidate| candidate.limit_id.as_deref() == Some(limit_id.as_str()))
            {
                snapshots.push(snapshot);
            }
        }
    }

    snapshots
        .into_iter()
        .flat_map(|snapshot| {
            let limit_id = snapshot
                .limit_id
                .unwrap_or_else(|| "unknown-limit".to_owned());
            let limit_name = snapshot.limit_name.clone();

            let mut windows = Vec::new();
            if let Some(primary) = snapshot.primary {
                windows.push(to_protocol_window(&limit_id, limit_name.clone(), primary));
            }
            if let Some(secondary) = snapshot.secondary {
                windows.push(to_protocol_window(&limit_id, limit_name, secondary));
            }
            windows
        })
        .collect()
}

fn to_protocol_window(
    limit_id: &str,
    limit_name: Option<String>,
    window: ProtocolWindow,
) -> ProtocolRateLimitWindow {
    ProtocolRateLimitWindow {
        limit_id: limit_id.to_owned(),
        label: limit_name,
        used_percent: window.used_percent,
        window_duration_mins: window.window_duration_mins,
        resets_at: window.resets_at,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountEnvelope {
    #[serde(default)]
    schema_version: Option<String>,
    #[serde(default)]
    account: Option<ProtocolAccount>,
    #[serde(default)]
    error: Option<ProtocolErrorPayload>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(tag = "type")]
enum ProtocolAccount {
    #[serde(rename = "chatgpt")]
    Chatgpt {
        email: String,
        #[serde(default)]
        plan_type: Option<String>,
    },
    #[serde(rename = "apiKey")]
    ApiKey,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitsEnvelope {
    #[serde(default)]
    schema_version: Option<String>,
    #[serde(default)]
    rate_limits: Option<Vec<ProtocolRateLimitSnapshot>>,
    #[serde(default)]
    rate_limits_by_limit_id: Option<BTreeMap<String, ProtocolRateLimitSnapshot>>,
    #[serde(default)]
    error: Option<ProtocolErrorPayload>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProtocolRateLimitSnapshot {
    #[serde(default)]
    limit_id: Option<String>,
    #[serde(default)]
    limit_name: Option<String>,
    #[serde(default)]
    primary: Option<ProtocolWindow>,
    #[serde(default)]
    secondary: Option<ProtocolWindow>,
    #[serde(default)]
    plan_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProtocolWindow {
    used_percent: f64,
    window_duration_mins: u32,
    resets_at: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ProtocolErrorPayload {
    code: String,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aistatus_core::{AccountMembership, MembershipTier, QuotaWindowKind};

    #[test]
    fn protocol_adapter_maps_success_fixture() {
        let snapshot = CodexProtocolAdapter::from_json(
            include_str!("../tests/fixtures/account-success.json"),
            include_str!("../tests/fixtures/rate-limits-success.json"),
        )
        .expect("success fixture should normalize");

        assert_eq!(snapshot.identity.account_kind, AccountKind::Chatgpt);
        assert_eq!(snapshot.identity.plan_type.as_deref(), Some("plus"));
        assert_eq!(snapshot.usage_family, UsageFamily::SubscriptionQuota);
        assert_eq!(snapshot.quota_snapshot.windows.len(), 2);
        assert_eq!(
            snapshot.quota_snapshot.windows[0].kind,
            QuotaWindowKind::FiveHour
        );
        assert_eq!(
            snapshot.quota_snapshot.windows[1].kind,
            QuotaWindowKind::Weekly
        );
    }

    #[test]
    fn protocol_adapter_flags_auth_failures() {
        let error = CodexProtocolAdapter::from_json(
            include_str!("../tests/fixtures/account-auth-expired.json"),
            include_str!("../tests/fixtures/rate-limits-auth-expired.json"),
        )
        .expect_err("auth fixture should fail");

        assert!(matches!(error, CodexProviderError::Auth(_)));
    }

    #[test]
    fn protocol_adapter_rejects_schema_drift() {
        let error = CodexProtocolAdapter::from_json(
            include_str!("../tests/fixtures/account-success.json"),
            include_str!("../tests/fixtures/rate-limits-schema-drift.json"),
        )
        .expect_err("schema drift fixture should fail");

        assert!(matches!(
            error,
            CodexProviderError::IncompatibleSchema { .. } | CodexProviderError::InvalidPayload(_)
        ));
    }

    #[test]
    fn protocol_adapter_uses_snapshot_plan_type_when_account_is_missing_one() {
        let snapshot = CodexProtocolAdapter::from_json(
            include_str!("../tests/fixtures/account-success-no-plan.json"),
            include_str!("../tests/fixtures/rate-limits-success.json"),
        )
        .expect("snapshot should fallback to rate limit plan type");

        let membership = AccountMembership::from_plan_type(snapshot.identity.plan_type.as_deref())
            .expect("membership should derive from fallback plan type");
        assert_eq!(membership.tier, MembershipTier::Plus);
    }
}
