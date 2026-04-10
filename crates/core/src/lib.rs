use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Tui(TuiCommand),
    Profile(ProfileCommand),
    Auth,
    Refresh(RefreshCommand),
    Doctor(DoctorCommand),
}

pub fn command_names() -> &'static [&'static str] {
    &["tui", "profile", "auth", "refresh", "doctor"]
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorCommand {
    pub config: Option<String>,
    pub fixtures: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuiCommand {
    pub fixtures: Option<String>,
}

/// Describes the operator-triggered refresh run over configured profiles.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshCommand {
    pub config: Option<String>,
    pub fixtures: Option<String>,
    pub force: bool,
    pub now_epoch_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileCommand {
    pub config: Option<String>,
    pub action: ProfileCommandAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileCommandAction {
    List,
    Add {
        profile_id: String,
        display_name: String,
        auth_mode: String,
        account_kind: String,
        provider: String,
        membership_tier: Option<String>,
        plan_type: Option<String>,
    },
    Edit {
        profile_id: String,
        display_name: Option<String>,
        refresh_interval_secs: Option<u64>,
        membership_tier: Option<String>,
        plan_type: Option<String>,
    },
    SetDefault {
        profile_id: String,
    },
    Remove {
        profile_id: String,
    },
    Login {
        profile_id: String,
        auth_mode: String,
        secret: String,
        use_file_store: bool,
    },
    Logout {
        profile_id: String,
    },
}

/// Identifies which backend family produced a profile or quota snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    CodexProtocol,
    OpenAiApiUsage,
    Other(String),
}

/// Distinguishes ChatGPT/Codex subscription quotas from OpenAI API usage telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageFamily {
    SubscriptionQuota,
    Api,
}

/// Captures how the user authenticated the profile so UI and storage code stay aligned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    Browser,
    Headless,
    ApiKey,
}

/// Separates account identity kind from subscription tier so API-key accounts do not pretend to have ChatGPT memberships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Chatgpt,
    ApiKey,
    Other(String),
}

/// Stable, user-visible membership buckets derived from upstream `planType` values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipTier {
    Free,
    Go,
    Plus,
    Pro,
    Team,
    Edu,
    Business,
    Enterprise,
    Unknown,
    Other,
}

/// Preserves the raw upstream `planType` string even when the app falls back to an `Other` or `Unknown` bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountMembership {
    pub tier: MembershipTier,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_plan_type: Option<String>,
}

impl AccountMembership {
    /// Normalizes protocol-backed plan types into stable UI buckets while retaining the original value for forward compatibility.
    pub fn from_plan_type(plan_type: Option<&str>) -> Option<Self> {
        let raw = plan_type?.trim();
        if raw.is_empty() {
            return None;
        }

        let normalized = raw.to_ascii_lowercase();
        let tier = match normalized.as_str() {
            "free" => MembershipTier::Free,
            "go" => MembershipTier::Go,
            "plus" => MembershipTier::Plus,
            "pro" => MembershipTier::Pro,
            "team" => MembershipTier::Team,
            "edu" => MembershipTier::Edu,
            "business" | "business_plus" | "business_pro" | "self_serve_business_usage_based" => {
                MembershipTier::Business
            }
            "enterprise"
            | "enterprise_standard"
            | "enterprise_plus"
            | "enterprise_pro"
            | "enterprise_cbp_usage_based" => MembershipTier::Enterprise,
            "unknown" => MembershipTier::Unknown,
            _ => MembershipTier::Other,
        };

        Some(Self {
            tier,
            raw_plan_type: Some(raw.to_owned()),
        })
    }
}

/// Models whether a profile is healthy enough to refresh or display, without embedding provider/network errors in the domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountHealth {
    Healthy,
    Stale,
    AuthExpired,
    RateLimited,
    Degraded,
    Error,
}

/// Defines how often the app may refresh a profile and whether a user can manually override the cadence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshPolicy {
    pub refresh_interval_secs: u64,
    pub allow_manual_refresh: bool,
}

/// Reserves stable identifiers for future external account/profile switching without implementing the switch flow yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FutureSwitchBridge {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opencode_profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_account_id: Option<String>,
}

/// Canonical account metadata shared across config, storage, provider, and UI layers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProfile {
    pub id: String,
    pub display_name: String,
    pub provider: ProviderKind,
    pub auth_mode: AuthMode,
    pub account_kind: AccountKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub membership: Option<AccountMembership>,
    pub health: AccountHealth,
    pub refresh_policy: RefreshPolicy,
    pub future_switch_bridge: FutureSwitchBridge,
}

impl AccountProfile {
    /// Builds an account profile from protocol-backed identity fields while keeping account kind and membership tier distinct.
    pub fn from_protocol_identity(
        identity: ProtocolAccountIdentity,
        provider: ProviderKind,
        auth_mode: AuthMode,
        health: AccountHealth,
        refresh_policy: RefreshPolicy,
        future_switch_bridge: FutureSwitchBridge,
    ) -> Self {
        let membership = match identity.account_kind {
            AccountKind::Chatgpt => {
                AccountMembership::from_plan_type(identity.plan_type.as_deref())
            }
            AccountKind::ApiKey | AccountKind::Other(_) => None,
        };

        Self {
            id: identity.account_id,
            display_name: identity.display_name,
            provider,
            auth_mode,
            account_kind: identity.account_kind,
            membership,
            health,
            refresh_policy,
            future_switch_bridge,
        }
    }
}

/// Transport-friendly identity input used by fixture-driven normalization tests and future provider adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolAccountIdentity {
    pub account_id: String,
    pub display_name: String,
    pub account_kind: AccountKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
}

/// Severity is derived from used percent so UI code can color windows without re-encoding thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaSeverity {
    Normal,
    Warning,
    Critical,
    Exhausted,
}

impl QuotaSeverity {
    pub fn from_used_percent(used_percent: f64) -> Self {
        if used_percent >= 100.0 {
            Self::Exhausted
        } else if used_percent >= 90.0 {
            Self::Critical
        } else if used_percent >= 75.0 {
            Self::Warning
        } else {
            Self::Normal
        }
    }
}

/// Window kind drives ordering and primary/secondary selection but never discards unrecognized windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaWindowKind {
    FiveHour,
    Weekly,
    Unknown,
}

/// Canonical representation of a single quota window after protocol normalization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindow {
    pub kind: QuotaWindowKind,
    pub limit_id: String,
    pub used_percent: f64,
    pub window_duration_mins: u32,
    pub resets_at: String,
    pub label: String,
    pub severity: QuotaSeverity,
}

impl QuotaWindow {
    /// Converts a protocol window into a displayable domain object while preserving raw IDs and durations for unknown cases.
    pub fn from_protocol_window(window: ProtocolRateLimitWindow) -> Self {
        let kind = classify_window(&window.limit_id, window.window_duration_mins);
        let label = window
            .label
            .unwrap_or_else(|| fallback_window_label(kind, window.window_duration_mins));

        Self {
            kind,
            limit_id: window.limit_id,
            used_percent: window.used_percent,
            window_duration_mins: window.window_duration_mins,
            resets_at: window.resets_at,
            label,
            severity: QuotaSeverity::from_used_percent(window.used_percent),
        }
    }
}

/// Grouped snapshot used by the UI and refresh orchestration layers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub windows: Vec<QuotaWindow>,
}

impl QuotaSnapshot {
    /// Normalizes and orders windows so 5-hour and weekly quotas land in stable positions when upstream provides them.
    pub fn from_protocol_windows(windows: Vec<ProtocolRateLimitWindow>) -> Self {
        let mut normalized: Vec<_> = windows
            .into_iter()
            .map(QuotaWindow::from_protocol_window)
            .collect();
        normalized.sort_by_key(window_sort_key);
        Self {
            windows: normalized,
        }
    }

    /// Returns the primary window only when an explicit 5-hour quota exists.
    pub fn primary_window(&self) -> Option<&QuotaWindow> {
        self.windows
            .iter()
            .find(|window| window.kind == QuotaWindowKind::FiveHour)
    }

    /// Returns the secondary window only when an explicit weekly quota exists.
    pub fn secondary_window(&self) -> Option<&QuotaWindow> {
        self.windows
            .iter()
            .find(|window| window.kind == QuotaWindowKind::Weekly)
    }
}

/// Minimal protocol-backed input for a single rate-limit window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtocolRateLimitWindow {
    pub limit_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub used_percent: f64,
    pub window_duration_mins: u32,
    pub resets_at: String,
}

fn classify_window(limit_id: &str, window_duration_mins: u32) -> QuotaWindowKind {
    let normalized_limit_id = limit_id.to_ascii_lowercase();

    if window_duration_mins == 300
        || normalized_limit_id.contains("5h")
        || normalized_limit_id.contains("5_h")
        || normalized_limit_id.contains("5hr")
        || normalized_limit_id.contains("five_hour")
    {
        QuotaWindowKind::FiveHour
    } else if window_duration_mins == 10_080 || normalized_limit_id.contains("week") {
        QuotaWindowKind::Weekly
    } else {
        QuotaWindowKind::Unknown
    }
}

fn fallback_window_label(kind: QuotaWindowKind, window_duration_mins: u32) -> String {
    match kind {
        QuotaWindowKind::FiveHour => "5h".to_owned(),
        QuotaWindowKind::Weekly => "Weekly".to_owned(),
        QuotaWindowKind::Unknown => format!("{}m", window_duration_mins),
    }
}

fn window_sort_key(window: &QuotaWindow) -> (u8, u32, String) {
    let rank = match window.kind {
        QuotaWindowKind::FiveHour => 0,
        QuotaWindowKind::Weekly => 1,
        QuotaWindowKind::Unknown => 2,
    };

    (rank, window.window_duration_mins, window.label.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct QuotaFixture {
        windows: Vec<ProtocolRateLimitWindow>,
    }

    fn default_refresh_policy() -> RefreshPolicy {
        RefreshPolicy {
            refresh_interval_secs: 300,
            allow_manual_refresh: true,
        }
    }

    #[test]
    fn quota_domain_mixed_windows_normalize_primary_and_secondary() {
        let fixture: QuotaFixture =
            serde_json::from_str(include_str!("../tests/fixtures/quota-mixed-windows.json"))
                .expect("fixture should deserialize");

        let snapshot = QuotaSnapshot::from_protocol_windows(fixture.windows);

        assert_eq!(snapshot.windows.len(), 2);
        assert_eq!(
            snapshot.primary_window().map(|window| &window.label),
            Some(&"5h".to_owned())
        );
        assert_eq!(
            snapshot.secondary_window().map(|window| &window.label),
            Some(&"Weekly".to_owned())
        );

        let primary = snapshot.primary_window().expect("5h window should exist");
        assert_eq!(primary.limit_id, "codex-5h");
        assert_eq!(primary.used_percent, 42.5);
        assert_eq!(primary.window_duration_mins, 300);
        assert_eq!(primary.resets_at, "2026-04-10T05:00:00Z");
        assert_eq!(primary.severity, QuotaSeverity::Normal);

        let secondary = snapshot
            .secondary_window()
            .expect("weekly window should exist");
        assert_eq!(secondary.limit_id, "codex-weekly");
        assert_eq!(secondary.used_percent, 15.0);
        assert_eq!(secondary.window_duration_mins, 10_080);
        assert_eq!(secondary.resets_at, "2026-04-14T00:00:00Z");
    }

    #[test]
    fn quota_domain_unknown_window_preserves_raw_display_data() {
        let fixture: QuotaFixture =
            serde_json::from_str(include_str!("../tests/fixtures/quota-unknown-window.json"))
                .expect("fixture should deserialize");

        let snapshot = QuotaSnapshot::from_protocol_windows(fixture.windows);
        let window = snapshot
            .windows
            .first()
            .expect("unknown window should exist");

        assert_eq!(window.kind, QuotaWindowKind::Unknown);
        assert_eq!(window.limit_id, "mystery-12h-limit");
        assert_eq!(window.window_duration_mins, 720);
        assert_eq!(window.label, "12h experimental");
        assert_eq!(snapshot.primary_window(), None);
        assert_eq!(snapshot.secondary_window(), None);
    }

    #[test]
    fn quota_domain_chatgpt_account_uses_known_plan_type() {
        let identity: ProtocolAccountIdentity =
            serde_json::from_str(include_str!("../tests/fixtures/account-chatgpt-plus.json"))
                .expect("fixture should deserialize");

        let profile = AccountProfile::from_protocol_identity(
            identity,
            ProviderKind::CodexProtocol,
            AuthMode::Browser,
            AccountHealth::Healthy,
            default_refresh_policy(),
            FutureSwitchBridge::default(),
        );

        assert_eq!(profile.account_kind, AccountKind::Chatgpt);
        assert_eq!(
            profile.membership,
            Some(AccountMembership {
                tier: MembershipTier::Plus,
                raw_plan_type: Some("plus".to_owned()),
            })
        );
    }

    #[test]
    fn quota_domain_api_key_account_has_no_chatgpt_membership() {
        let identity: ProtocolAccountIdentity =
            serde_json::from_str(include_str!("../tests/fixtures/account-api-key.json"))
                .expect("fixture should deserialize");

        let profile = AccountProfile::from_protocol_identity(
            identity,
            ProviderKind::CodexProtocol,
            AuthMode::ApiKey,
            AccountHealth::Healthy,
            default_refresh_policy(),
            FutureSwitchBridge::default(),
        );

        assert_eq!(profile.account_kind, AccountKind::ApiKey);
        assert_eq!(profile.membership, None);
    }

    #[test]
    fn quota_domain_unknown_plan_type_keeps_raw_value() {
        let identity: ProtocolAccountIdentity =
            serde_json::from_str(include_str!("../tests/fixtures/account-raw-plan.json"))
                .expect("fixture should deserialize");

        let profile = AccountProfile::from_protocol_identity(
            identity,
            ProviderKind::CodexProtocol,
            AuthMode::Headless,
            AccountHealth::Stale,
            default_refresh_policy(),
            FutureSwitchBridge::default(),
        );

        assert_eq!(profile.account_kind, AccountKind::Chatgpt);
        assert_eq!(
            profile.membership,
            Some(AccountMembership {
                tier: MembershipTier::Other,
                raw_plan_type: Some("galaxy".to_owned()),
            })
        );
    }

    #[test]
    fn quota_domain_unknown_literal_plan_type_maps_to_unknown_tier() {
        let membership = AccountMembership::from_plan_type(Some("unknown"));

        assert_eq!(
            membership,
            Some(AccountMembership {
                tier: MembershipTier::Unknown,
                raw_plan_type: Some("unknown".to_owned()),
            })
        );
    }

    #[test]
    fn quota_domain_usage_based_plan_types_map_to_stable_business_buckets() {
        assert_eq!(
            AccountMembership::from_plan_type(Some("self_serve_business_usage_based")),
            Some(AccountMembership {
                tier: MembershipTier::Business,
                raw_plan_type: Some("self_serve_business_usage_based".to_owned()),
            })
        );

        assert_eq!(
            AccountMembership::from_plan_type(Some("enterprise_cbp_usage_based")),
            Some(AccountMembership {
                tier: MembershipTier::Enterprise,
                raw_plan_type: Some("enterprise_cbp_usage_based".to_owned()),
            })
        );
    }
}
