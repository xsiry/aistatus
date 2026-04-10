pub mod cli;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aistatus_config::{AuthSecret, ConfiguredProfile, PlainConfig, SecretMaterial, SecretRef};
use aistatus_core::{
    AccountHealth, AccountProfile, Command, ProviderKind, QuotaSnapshot, RefreshCommand,
    RefreshPolicy, UsageFamily,
};
use aistatus_provider_codex::{CodexProtocolAdapter, CodexProviderError, CodexProviderSnapshot};
use aistatus_store::{AppPaths, FileSecretStore, KeyringSecretStore, SecretStore, StoreError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const CHATGPT_ACCOUNT_SUCCESS_FIXTURE: &str =
    include_str!("../../provider-codex/tests/fixtures/account-success.json");
const RATE_LIMITS_SUCCESS_FIXTURE: &str =
    include_str!("../../provider-codex/tests/fixtures/rate-limits-success.json");
const API_KEY_ACCOUNT_SUCCESS_FIXTURE: &str =
    r#"{"schemaVersion":"v2","account":{"type":"apiKey"}}"#;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefreshedProfile {
    pub profile: AccountProfile,
    pub snapshot: Option<QuotaSnapshot>,
    pub usage_family: UsageFamily,
    pub status: RefreshStatus,
    pub last_error: Option<String>,
    pub last_updated_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshStatus {
    Healthy,
    Stale,
    AuthRequired,
    RateLimited,
    Error,
    Refreshing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshRequest {
    pub profile_id: String,
    pub now_epoch_secs: u64,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshDecision {
    pub should_start: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefreshState {
    pub profiles: BTreeMap<String, RefreshedProfile>,
}

impl RefreshState {
    pub fn new(configured_profiles: &[ConfiguredProfile]) -> Self {
        let profiles = configured_profiles
            .iter()
            .map(|configured| {
                (
                    configured.profile.id.clone(),
                    RefreshedProfile {
                        profile: configured.profile.clone(),
                        snapshot: None,
                        usage_family: default_usage_family(&configured.profile.provider),
                        status: RefreshStatus::Stale,
                        last_error: None,
                        last_updated_at: None,
                    },
                )
            })
            .collect();
        Self { profiles }
    }

    pub fn should_start_refresh(
        &self,
        request: &RefreshRequest,
        in_flight: bool,
    ) -> RefreshDecision {
        if in_flight {
            return RefreshDecision {
                should_start: false,
                reason: "refresh already in flight".into(),
            };
        }

        let Some(profile) = self.profiles.get(&request.profile_id) else {
            return RefreshDecision {
                should_start: false,
                reason: "unknown profile".into(),
            };
        };

        if request.force {
            if !profile.profile.refresh_policy.allow_manual_refresh {
                return RefreshDecision {
                    should_start: false,
                    reason: "manual refresh disabled by policy".into(),
                };
            }

            return RefreshDecision {
                should_start: true,
                reason: "manual refresh requested".into(),
            };
        }

        if let Some(last_updated_at) = profile.last_updated_at {
            let due_at = last_updated_at
                .saturating_add(profile.profile.refresh_policy.refresh_interval_secs);
            if request.now_epoch_secs < due_at {
                return RefreshDecision {
                    should_start: false,
                    reason: format!("next refresh due at {due_at}"),
                };
            }
        }

        RefreshDecision {
            should_start: true,
            reason: "refresh interval elapsed".into(),
        }
    }

    pub fn apply_codex_success(
        &mut self,
        profile_id: &str,
        now_epoch_secs: u64,
        snapshot: CodexProviderSnapshot,
    ) -> Result<(), AppError> {
        let refreshed = self
            .profiles
            .get_mut(profile_id)
            .ok_or_else(|| AppError::UnknownProfile(profile_id.to_owned()))?;

        let upstream_profile = AccountProfile::from_protocol_identity(
            snapshot.identity,
            ProviderKind::CodexProtocol,
            refreshed.profile.auth_mode.clone(),
            AccountHealth::Healthy,
            refreshed.profile.refresh_policy.clone(),
            refreshed.profile.future_switch_bridge.clone(),
        );
        refreshed.profile.display_name = upstream_profile.display_name;
        refreshed.profile.account_kind = upstream_profile.account_kind;
        refreshed.profile.membership = upstream_profile.membership;
        refreshed.profile.health = upstream_profile.health;
        refreshed.profile.provider = upstream_profile.provider;
        refreshed.profile.future_switch_bridge.codex_account_id = upstream_profile
            .future_switch_bridge
            .codex_account_id
            .or(Some(upstream_profile.id));
        refreshed.usage_family = snapshot.usage_family;
        refreshed.snapshot = Some(snapshot.quota_snapshot);
        refreshed.status = RefreshStatus::Healthy;
        refreshed.last_error = None;
        refreshed.last_updated_at = Some(now_epoch_secs);
        Ok(())
    }

    pub fn apply_codex_failure(
        &mut self,
        profile_id: &str,
        error: CodexProviderError,
    ) -> Result<(), AppError> {
        let refreshed = self
            .profiles
            .get_mut(profile_id)
            .ok_or_else(|| AppError::UnknownProfile(profile_id.to_owned()))?;

        let (status, health) = match &error {
            CodexProviderError::Auth(_) => {
                (RefreshStatus::AuthRequired, AccountHealth::AuthExpired)
            }
            CodexProviderError::Transport(message)
                if message.to_ascii_lowercase().contains("rate limit") =>
            {
                (RefreshStatus::RateLimited, AccountHealth::RateLimited)
            }
            CodexProviderError::Transport(_) => (RefreshStatus::Stale, AccountHealth::Stale),
            CodexProviderError::IncompatibleSchema { .. } => {
                (RefreshStatus::Error, AccountHealth::Degraded)
            }
            CodexProviderError::InvalidPayload(_) => (RefreshStatus::Error, AccountHealth::Error),
        };

        refreshed.status = status;
        refreshed.profile.health = health;
        refreshed.last_error = Some(error.to_string());
        Ok(())
    }

    pub fn mark_refreshing(&mut self, profile_id: &str) -> Result<(), AppError> {
        let refreshed = self
            .profiles
            .get_mut(profile_id)
            .ok_or_else(|| AppError::UnknownProfile(profile_id.to_owned()))?;
        refreshed.status = RefreshStatus::Refreshing;
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AppError {
    #[error("unknown profile `{0}`")]
    UnknownProfile(String),
}

/// Operator-facing report for one `refresh` command run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshCommandOutput {
    pub lines: Vec<String>,
}

impl RefreshCommandOutput {
    pub fn render(&self) -> String {
        self.lines.join("\n")
    }
}

/// Structured refresh result shared by the CLI and TUI so both surfaces reuse the same
/// orchestration path and per-profile feedback.
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshRun {
    pub state: RefreshState,
    pub output: RefreshCommandOutput,
    pub profile_lines: BTreeMap<String, String>,
    pub now_epoch_secs: u64,
}

#[derive(Debug, Error)]
pub enum RefreshCommandError {
    #[error("io failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("config parse failure: {0}")]
    ConfigToml(#[from] toml::de::Error),
    #[error("secret material parse failure: {0}")]
    SecretJson(#[from] serde_json::Error),
    #[error("secret store failure: {0}")]
    Store(#[from] StoreError),
    #[error("refresh orchestration failure: {0}")]
    App(#[from] AppError),
}

/// Runs the minimal fixture/config-backed refresh flow through the orchestration state machine.
pub fn run_refresh_command(
    command: &RefreshCommand,
) -> Result<RefreshCommandOutput, RefreshCommandError> {
    Ok(run_refresh_cycle(command)?.output)
}

/// Replays the existing refresh orchestration and returns both the operator-facing output and
/// the updated refresh state for interactive surfaces.
pub fn run_refresh_cycle(command: &RefreshCommand) -> Result<RefreshRun, RefreshCommandError> {
    let fixture_root = command
        .fixtures
        .as_ref()
        .map(|fixture| workspace_root().join(".sisyphus/fixtures").join(fixture));
    let config_path = resolve_refresh_config_path(command, fixture_root.as_deref());
    let input = fs::read_to_string(&config_path)?;
    let config = PlainConfig::from_toml_str(&input)?;
    let secret_material = resolve_secret_material(&config_path, fixture_root.as_deref())?;
    let now_epoch_secs = command.now_epoch_secs.unwrap_or_else(current_epoch_secs);

    let mut state = RefreshState::new(&config.profiles);
    let mut profile_lines = BTreeMap::new();
    let mut lines = vec![
        format!("source: {}", config_path.display()),
        format!("profiles: {}", config.profiles.len()),
    ];

    for configured in &config.profiles {
        let request = RefreshRequest {
            profile_id: configured.profile.id.clone(),
            now_epoch_secs,
            force: command.force,
        };
        let decision = state.should_start_refresh(&request, false);
        if !decision.should_start {
            let line = render_skipped_profile_line(configured, &decision);
            profile_lines.insert(configured.profile.id.clone(), line.clone());
            lines.push(line);
            continue;
        }

        state.mark_refreshing(&configured.profile.id)?;
        execute_refresh_profile(
            &mut state,
            configured,
            secret_material.as_ref(),
            now_epoch_secs,
        )?;
        let refreshed = state
            .profiles
            .get(&configured.profile.id)
            .ok_or_else(|| AppError::UnknownProfile(configured.profile.id.clone()))?;
        let line = render_refreshed_profile_line(refreshed, &decision.reason);
        profile_lines.insert(configured.profile.id.clone(), line.clone());
        lines.push(line);
    }

    Ok(RefreshRun {
        state,
        output: RefreshCommandOutput { lines },
        profile_lines,
        now_epoch_secs,
    })
}

pub fn dispatch_command(
    command: &Command,
) -> Option<Result<RefreshCommandOutput, RefreshCommandError>> {
    match command {
        Command::Refresh(refresh) => Some(run_refresh_command(refresh)),
        _ => None,
    }
}

pub fn clamp_refresh_policy(policy: &RefreshPolicy) -> RefreshPolicy {
    let refresh_interval_secs = policy.refresh_interval_secs.clamp(60, 3600);
    RefreshPolicy {
        refresh_interval_secs,
        allow_manual_refresh: policy.allow_manual_refresh,
    }
}

fn default_usage_family(provider: &ProviderKind) -> UsageFamily {
    match provider {
        ProviderKind::OpenAiApiUsage => UsageFamily::Api,
        ProviderKind::CodexProtocol | ProviderKind::Other(_) => UsageFamily::SubscriptionQuota,
    }
}

fn resolve_refresh_config_path(command: &RefreshCommand, fixture_root: Option<&Path>) -> PathBuf {
    command
        .config
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| fixture_root.map(|root| root.join("config.toml")))
        .unwrap_or_else(|| workspace_root().join(".sisyphus/fixtures/sample-config.toml"))
}

fn resolve_secret_material(
    config_path: &Path,
    fixture_root: Option<&Path>,
) -> Result<Option<SecretMaterial>, RefreshCommandError> {
    let candidate_paths = [
        fixture_root.map(|root| root.join("secrets.json")),
        infer_secret_material_path(config_path),
    ];

    for candidate in candidate_paths.into_iter().flatten() {
        if candidate.exists() {
            let input = fs::read_to_string(candidate)?;
            return Ok(Some(SecretMaterial::from_json_str(&input)?));
        }
    }

    Ok(None)
}

fn infer_secret_material_path(config_path: &Path) -> Option<PathBuf> {
    let file_name = config_path.file_name()?.to_string_lossy();
    let sibling = if file_name.contains("config") {
        file_name.replace("config.toml", "secrets.json")
    } else {
        format!(
            "{}.secrets.json",
            config_path.file_stem()?.to_string_lossy()
        )
    };
    Some(config_path.with_file_name(sibling))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn execute_refresh_profile(
    state: &mut RefreshState,
    configured: &ConfiguredProfile,
    secret_material: Option<&SecretMaterial>,
    now_epoch_secs: u64,
) -> Result<(), RefreshCommandError> {
    let app_paths = AppPaths::resolve("aistatus")?;
    let file_store = FileSecretStore::from_app_paths(&app_paths);
    let keyring_store = KeyringSecretStore::new("aistatus");

    if let Some(error) =
        validate_refresh_secret(configured, secret_material, &file_store, &keyring_store)
    {
        state.apply_codex_failure(&configured.profile.id, error)?;
        return Ok(());
    }

    // V1 keeps the refresh command runnable by replaying protocol fixtures through the real
    // adapter/orchestrator path until live transport wiring is added in a later pass.
    let provider_result = match configured.profile.provider {
        ProviderKind::CodexProtocol => load_fixture_snapshot_for_profile(configured),
        ProviderKind::OpenAiApiUsage | ProviderKind::Other(_) => {
            Err(CodexProviderError::InvalidPayload(format!(
                "refresh provider `{}` is not supported yet",
                render_provider(&configured.profile.provider)
            )))
        }
    };

    match provider_result {
        Ok(snapshot) => {
            state.apply_codex_success(&configured.profile.id, now_epoch_secs, snapshot)?
        }
        Err(error) => state.apply_codex_failure(&configured.profile.id, error)?,
    }

    Ok(())
}

fn validate_refresh_secret(
    configured: &ConfiguredProfile,
    secret_material: Option<&SecretMaterial>,
    file_store: &FileSecretStore,
    keyring_store: &KeyringSecretStore,
) -> Option<CodexProviderError> {
    let secret = if let Some(secret_material) = secret_material {
        match secret_material
            .entries
            .iter()
            .find(|entry| entry.profile_id == configured.profile.id)
        {
            Some(entry) => entry.secret.clone(),
            None => {
                return Some(CodexProviderError::Auth(
                    "missing secret entry in sidecar material".into(),
                ));
            }
        }
    } else {
        match &configured.secret_ref {
            SecretRef::File { .. } => {
                match file_store.read_secret(&configured.secret_ref, &configured.profile.id) {
                    Ok(secret) => secret,
                    Err(error) => return Some(map_store_error_to_auth(error)),
                }
            }
            SecretRef::Keychain { .. } => {
                match keyring_store.read_secret(&configured.secret_ref, &configured.profile.id) {
                    Ok(secret) => secret,
                    Err(error) => return Some(map_store_error_to_auth(error)),
                }
            }
            SecretRef::Managed { id } => {
                return Some(CodexProviderError::Auth(format!(
                    "managed secret ref `{id}` is unavailable without sidecar material"
                )));
            }
        }
    };

    validate_secret_payload(&secret)
}

fn map_store_error_to_auth(error: StoreError) -> CodexProviderError {
    CodexProviderError::Auth(error.to_string())
}

fn validate_secret_payload(secret: &AuthSecret) -> Option<CodexProviderError> {
    let message = match secret {
        AuthSecret::BrowserSession { session_payload }
        | AuthSecret::HeadlessSession { session_payload }
            if session_payload.trim().is_empty() =>
        {
            Some("empty browser session payload")
        }
        AuthSecret::ApiKey { api_key } if api_key.trim().is_empty() => Some("empty api key"),
        _ => None,
    }?;

    Some(CodexProviderError::Auth(message.into()))
}

fn load_fixture_snapshot_for_profile(
    configured: &ConfiguredProfile,
) -> Result<CodexProviderSnapshot, CodexProviderError> {
    let account_json = match configured.profile.account_kind {
        aistatus_core::AccountKind::ApiKey => API_KEY_ACCOUNT_SUCCESS_FIXTURE,
        _ => CHATGPT_ACCOUNT_SUCCESS_FIXTURE,
    };
    CodexProtocolAdapter::from_json(account_json, RATE_LIMITS_SUCCESS_FIXTURE)
}

fn render_skipped_profile_line(
    configured: &ConfiguredProfile,
    decision: &RefreshDecision,
) -> String {
    format!(
        "skipped: {} | {} | {}",
        configured.profile.id, configured.profile.display_name, decision.reason
    )
}

fn render_refreshed_profile_line(profile: &RefreshedProfile, decision_reason: &str) -> String {
    let snapshot = profile
        .snapshot
        .as_ref()
        .map(render_snapshot_summary)
        .unwrap_or_else(|| "no quota snapshot".into());

    let error_detail = profile
        .last_error
        .as_ref()
        .map(|error| format!(" | error={error}"))
        .unwrap_or_default();

    format!(
        "{}: {} | {} | decision={} | {}{}",
        render_refresh_status(&profile.status),
        profile.profile.id,
        profile.profile.display_name,
        decision_reason,
        snapshot,
        error_detail
    )
}

fn render_snapshot_summary(snapshot: &QuotaSnapshot) -> String {
    let primary = snapshot
        .primary_window()
        .map(|window| {
            format!(
                "{} {:.1}% -> {}",
                window.label, window.used_percent, window.resets_at
            )
        })
        .unwrap_or_else(|| "no 5h window".into());
    let secondary = snapshot
        .secondary_window()
        .map(|window| {
            format!(
                "{} {:.1}% -> {}",
                window.label, window.used_percent, window.resets_at
            )
        })
        .unwrap_or_else(|| "no weekly window".into());
    format!("{primary} | {secondary}")
}

fn render_refresh_status(status: &RefreshStatus) -> &'static str {
    match status {
        RefreshStatus::Healthy => "healthy",
        RefreshStatus::Stale => "stale",
        RefreshStatus::AuthRequired => "auth_required",
        RefreshStatus::RateLimited => "rate_limited",
        RefreshStatus::Error => "error",
        RefreshStatus::Refreshing => "refreshing",
    }
}

fn render_provider(provider: &ProviderKind) -> &str {
    match provider {
        ProviderKind::CodexProtocol => "codex_protocol",
        ProviderKind::OpenAiApiUsage => "openai_api_usage",
        ProviderKind::Other(_) => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aistatus_core::{
        AccountHealth, AccountKind, AuthMode, FutureSwitchBridge, ProtocolAccountIdentity,
        ProtocolRateLimitWindow,
    };
    use aistatus_store::FileSecretStore;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn configured_profile(id: &str, interval: u64) -> ConfiguredProfile {
        ConfiguredProfile {
            profile: AccountProfile {
                id: id.to_owned(),
                display_name: format!("Profile {id}"),
                provider: ProviderKind::CodexProtocol,
                auth_mode: AuthMode::Browser,
                account_kind: AccountKind::Chatgpt,
                membership: None,
                health: AccountHealth::Stale,
                refresh_policy: RefreshPolicy {
                    refresh_interval_secs: interval,
                    allow_manual_refresh: true,
                },
                future_switch_bridge: FutureSwitchBridge::default(),
            },
            secret_ref: aistatus_config::SecretRef::Managed {
                id: format!("managed-{id}"),
            },
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("aistatus-app-{label}-{nanos}"))
    }

    fn write_config(config_path: &Path, configured: &ConfiguredProfile) {
        let config = PlainConfig {
            version: 1,
            default_profile_id: Some(configured.profile.id.clone()),
            profiles: vec![configured.clone()],
        };
        fs::write(
            config_path,
            config
                .to_toml_string()
                .expect("config fixture should serialize"),
        )
        .expect("config fixture should write");
    }

    fn success_snapshot() -> CodexProviderSnapshot {
        CodexProviderSnapshot {
            identity: ProtocolAccountIdentity {
                account_id: "acct-1".into(),
                display_name: "acct-1@example.com".into(),
                account_kind: AccountKind::Chatgpt,
                plan_type: Some("plus".into()),
            },
            quota_snapshot: QuotaSnapshot::from_protocol_windows(vec![
                ProtocolRateLimitWindow {
                    limit_id: "codex-5h".into(),
                    label: None,
                    used_percent: 32.0,
                    window_duration_mins: 300,
                    resets_at: "2026-04-10T05:00:00Z".into(),
                },
                ProtocolRateLimitWindow {
                    limit_id: "codex-weekly".into(),
                    label: None,
                    used_percent: 10.0,
                    window_duration_mins: 10_080,
                    resets_at: "2026-04-14T00:00:00Z".into(),
                },
            ]),
            usage_family: UsageFamily::SubscriptionQuota,
        }
    }

    #[test]
    fn refresh_orchestrator_preserves_last_good_snapshot_on_transport_failure() {
        let mut state = RefreshState::new(&[configured_profile("acct-1", 300)]);
        state
            .apply_codex_success("acct-1", 1_000, success_snapshot())
            .expect("success should apply");
        let previous_snapshot = state.profiles["acct-1"].snapshot.clone();

        state
            .apply_codex_failure(
                "acct-1",
                CodexProviderError::Transport("network timeout".into()),
            )
            .expect("failure should apply");

        assert_eq!(state.profiles["acct-1"].status, RefreshStatus::Stale);
        assert_eq!(state.profiles["acct-1"].snapshot, previous_snapshot);
    }

    #[test]
    fn refresh_orchestrator_preserves_local_profile_id_on_success() {
        let mut state = RefreshState::new(&[configured_profile("local-acct-1", 300)]);
        let snapshot = CodexProviderSnapshot {
            identity: ProtocolAccountIdentity {
                account_id: "upstream-acct-99".into(),
                display_name: "remote@example.com".into(),
                account_kind: AccountKind::Chatgpt,
                plan_type: Some("pro".into()),
            },
            ..success_snapshot()
        };

        state
            .apply_codex_success("local-acct-1", 1_000, snapshot)
            .expect("success should apply");

        let refreshed = &state.profiles["local-acct-1"];
        assert_eq!(refreshed.profile.id, "local-acct-1");
        assert_eq!(refreshed.profile.display_name, "remote@example.com");
        assert_eq!(
            refreshed
                .profile
                .future_switch_bridge
                .codex_account_id
                .as_deref(),
            Some("upstream-acct-99")
        );
        assert_eq!(refreshed.status, RefreshStatus::Healthy);
        assert!(refreshed.last_error.is_none());
        assert_eq!(refreshed.last_updated_at, Some(1_000));
    }

    #[test]
    fn refresh_orchestrator_clamps_over_aggressive_polling() {
        let clamped = clamp_refresh_policy(&RefreshPolicy {
            refresh_interval_secs: 5,
            allow_manual_refresh: true,
        });
        assert_eq!(clamped.refresh_interval_secs, 60);
    }

    #[test]
    fn refresh_orchestrator_debounces_in_flight_manual_refresh() {
        let state = RefreshState::new(&[configured_profile("acct-1", 300)]);
        let decision = state.should_start_refresh(
            &RefreshRequest {
                profile_id: "acct-1".into(),
                now_epoch_secs: 1_000,
                force: true,
            },
            true,
        );

        assert!(!decision.should_start);
        assert_eq!(decision.reason, "refresh already in flight");
    }

    #[test]
    fn refresh_orchestrator_blocks_manual_refresh_when_policy_disallows_it() {
        let mut profile = configured_profile("acct-1", 300);
        profile.profile.refresh_policy.allow_manual_refresh = false;
        let state = RefreshState::new(&[profile]);

        let decision = state.should_start_refresh(
            &RefreshRequest {
                profile_id: "acct-1".into(),
                now_epoch_secs: 1_000,
                force: true,
            },
            false,
        );

        assert!(!decision.should_start);
        assert_eq!(decision.reason, "manual refresh disabled by policy");
    }

    #[test]
    fn refresh_orchestrator_allows_scheduled_refresh_when_due() {
        let state = RefreshState::new(&[configured_profile("acct-1", 300)]);

        let decision = state.should_start_refresh(
            &RefreshRequest {
                profile_id: "acct-1".into(),
                now_epoch_secs: 1_000,
                force: false,
            },
            false,
        );

        assert!(decision.should_start);
        assert_eq!(decision.reason, "refresh interval elapsed");
    }

    #[test]
    fn refresh_orchestrator_marks_auth_failures_as_auth_required() {
        let mut state = RefreshState::new(&[configured_profile("acct-1", 300)]);
        state
            .apply_codex_failure("acct-1", CodexProviderError::Auth("session expired".into()))
            .expect("failure should apply");

        assert_eq!(state.profiles["acct-1"].status, RefreshStatus::AuthRequired);
        assert_eq!(
            state.profiles["acct-1"].profile.health,
            AccountHealth::AuthExpired
        );
    }

    #[test]
    fn refresh_command_runs_real_fixture_backed_flow() {
        let output = run_refresh_command(&RefreshCommand {
            config: Some(
                workspace_root()
                    .join(".sisyphus/fixtures/sample-config.toml")
                    .display()
                    .to_string(),
            ),
            fixtures: None,
            force: true,
            now_epoch_secs: Some(1_000),
        })
        .expect("refresh command should succeed");

        let rendered = output.render();
        assert!(rendered.contains("source: "));
        assert!(rendered.contains("profiles: 2"));
        assert!(rendered.contains("healthy: acct-chatgpt-plus"));
        assert!(rendered.contains("healthy: acct-api-key"));
        assert!(rendered.contains("decision=manual refresh requested"));
        assert!(rendered.contains("42.5% -> 2026-04-10T05:00:00Z"));
        assert!(!rendered.contains("refresh scaffold"));
    }

    #[test]
    fn refresh_command_surfaces_fixture_auth_failures() {
        let output = run_refresh_command(&RefreshCommand {
            config: None,
            fixtures: Some("corrupted-session".into()),
            force: true,
            now_epoch_secs: Some(1_000),
        })
        .expect("refresh command should still render failures");

        let rendered = output.render();
        assert!(rendered.contains("auth_required: acct-corrupt"));
        assert!(rendered.contains("error=authentication failure: empty browser session payload"));
    }

    #[test]
    fn refresh_cycle_returns_state_and_profile_feedback_for_tui() {
        let run = run_refresh_cycle(&RefreshCommand {
            config: None,
            fixtures: Some("corrupted-session".into()),
            force: true,
            now_epoch_secs: Some(1_000),
        })
        .expect("refresh cycle should still return structured failures");

        assert_eq!(run.now_epoch_secs, 1_000);
        assert_eq!(
            run.state.profiles["acct-corrupt"].status,
            RefreshStatus::AuthRequired
        );
        assert!(run
            .profile_lines
            .get("acct-corrupt")
            .is_some_and(|line| line.contains("auth_required: acct-corrupt")));
    }

    #[test]
    fn managed_secret_without_sidecar_does_not_refresh_as_healthy() {
        let root = temp_dir("managed-without-sidecar");
        fs::create_dir_all(&root).expect("temp root should exist");
        let config_path = root.join("config.toml");
        let configured = configured_profile("acct-managed", 300);
        write_config(&config_path, &configured);

        let run = run_refresh_cycle(&RefreshCommand {
            config: Some(config_path.display().to_string()),
            fixtures: None,
            force: true,
            now_epoch_secs: Some(1_000),
        })
        .expect("refresh cycle should render auth failures");

        let refreshed = &run.state.profiles["acct-managed"];
        assert_eq!(refreshed.status, RefreshStatus::AuthRequired);
        assert_eq!(refreshed.profile.health, AccountHealth::AuthExpired);
        assert!(refreshed.snapshot.is_none());
        assert!(refreshed
            .last_error
            .as_ref()
            .is_some_and(|error| error.contains("managed secret ref")));
        assert!(run.output.render().contains("auth_required: acct-managed"));
    }

    #[test]
    fn file_backend_read_failure_surfaces_as_refresh_failure() {
        let root = temp_dir("file-backend-failure");
        fs::create_dir_all(&root).expect("temp root should exist");
        let store = FileSecretStore::new(root.join("secrets"), root.join("master.key"));
        let secret_ref = store
            .write_secret(
                "acct-file",
                &AuthSecret::BrowserSession {
                    session_payload: "cookie=value".into(),
                },
            )
            .expect("secret should save");

        let secret_path = match &secret_ref {
            SecretRef::File { path, .. } => PathBuf::from(path),
            _ => panic!("expected file secret ref"),
        };
        fs::remove_file(&secret_path)
            .expect("secret file should be removed to simulate backend failure");

        let configured = ConfiguredProfile {
            secret_ref,
            ..configured_profile("acct-file", 300)
        };
        let config_path = root.join("config.toml");
        write_config(&config_path, &configured);

        let run = run_refresh_cycle(&RefreshCommand {
            config: Some(config_path.display().to_string()),
            fixtures: None,
            force: true,
            now_epoch_secs: Some(1_000),
        })
        .expect("refresh cycle should render backend failures");

        let refreshed = &run.state.profiles["acct-file"];
        assert_eq!(refreshed.status, RefreshStatus::AuthRequired);
        assert_eq!(refreshed.profile.health, AccountHealth::AuthExpired);
        assert!(refreshed.snapshot.is_none());
        assert!(refreshed
            .last_error
            .as_ref()
            .is_some_and(|error| error.contains("io failure")));
        assert!(run.output.render().contains("auth_required: acct-file"));
    }
}
