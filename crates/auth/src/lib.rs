use std::fs;
use std::path::{Path, PathBuf};

use aistatus_config::{AuthSecret, PlainConfig, SecretMaterial, SecretRef};
use aistatus_core::{
    AccountHealth, AccountKind, AccountMembership, AccountProfile, AuthMode, Command,
    DoctorCommand, FutureSwitchBridge, MembershipTier, ProfileCommand, ProfileCommandAction,
    ProviderKind, RefreshPolicy,
};
use aistatus_store::{AppPaths, FileSecretStore, KeyringSecretStore, SecretStore, StoreError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("io failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("config parse failure: {0}")]
    ConfigToml(#[from] toml::de::Error),
    #[error("config render failure: {0}")]
    ConfigTomlWrite(#[from] toml::ser::Error),
    #[error("secret material parse failure: {0}")]
    SecretJson(#[from] serde_json::Error),
    #[error("store failure: {0}")]
    Store(#[from] StoreError),
    #[error("profile `{0}` not found")]
    MissingProfile(String),
    #[error("invalid auth mode `{0}`")]
    InvalidAuthMode(String),
}

#[derive(Debug, Clone)]
pub struct ProfileRepository {
    pub config: PlainConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileCommandOutput {
    pub lines: Vec<String>,
}

impl ProfileCommandOutput {
    pub fn render(&self) -> String {
        self.lines.join("\n")
    }
}

impl ProfileRepository {
    pub fn load(path: &Path) -> Result<Self, AuthError> {
        let input = fs::read_to_string(path)?;
        let config = PlainConfig::from_toml_str(&input)?;
        Ok(Self { config })
    }

    pub fn save(&self, path: &Path) -> Result<(), AuthError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let rendered = self.config.to_toml_string()?;
        fs::write(path, rendered)?;
        Ok(())
    }

    pub fn list(&self) -> &[aistatus_config::ConfiguredProfile] {
        &self.config.profiles
    }

    pub fn set_default(&mut self, profile_id: &str) -> Result<(), AuthError> {
        if self.find(profile_id).is_none() {
            return Err(AuthError::MissingProfile(profile_id.to_owned()));
        }
        self.config.default_profile_id = Some(profile_id.to_owned());
        Ok(())
    }

    pub fn upsert(&mut self, profile: AccountProfile, secret_ref: SecretRef) {
        if let Some(existing) = self
            .config
            .profiles
            .iter_mut()
            .find(|configured| configured.profile.id == profile.id)
        {
            existing.profile = profile;
            existing.secret_ref = secret_ref;
        } else {
            self.config
                .profiles
                .push(aistatus_config::ConfiguredProfile {
                    profile,
                    secret_ref,
                });
        }
    }

    pub fn remove(
        &mut self,
        profile_id: &str,
    ) -> Result<aistatus_config::ConfiguredProfile, AuthError> {
        let index = self
            .config
            .profiles
            .iter()
            .position(|profile| profile.profile.id == profile_id)
            .ok_or_else(|| AuthError::MissingProfile(profile_id.to_owned()))?;
        Ok(self.config.profiles.remove(index))
    }

    pub fn find(&self, profile_id: &str) -> Option<&aistatus_config::ConfiguredProfile> {
        self.config
            .profiles
            .iter()
            .find(|profile| profile.profile.id == profile_id)
    }

    pub fn find_mut(
        &mut self,
        profile_id: &str,
    ) -> Option<&mut aistatus_config::ConfiguredProfile> {
        self.config
            .profiles
            .iter_mut()
            .find(|profile| profile.profile.id == profile_id)
    }
}

pub fn run_profile_command(command: &ProfileCommand) -> Result<ProfileCommandOutput, AuthError> {
    let config_path = resolve_profile_config_path(command);
    let app_paths = AppPaths::resolve("aistatus")?;
    app_paths.ensure()?;

    let mut repository = if config_path.exists() {
        ProfileRepository::load(&config_path)?
    } else {
        ProfileRepository {
            config: PlainConfig {
                version: 1,
                default_profile_id: None,
                profiles: Vec::new(),
            },
        }
    };

    match &command.action {
        ProfileCommandAction::List => Ok(render_profile_list(&repository)),
        ProfileCommandAction::Add {
            profile_id,
            display_name,
            auth_mode,
            account_kind,
            provider,
            membership_tier,
            plan_type,
        } => {
            let profile = build_profile(
                profile_id,
                display_name,
                provider,
                auth_mode,
                account_kind,
                membership_tier.as_deref(),
                plan_type.as_deref(),
            )?;
            repository.upsert(
                profile,
                SecretRef::Managed {
                    id: format!("managed-{profile_id}"),
                },
            );
            if repository.config.default_profile_id.is_none() {
                repository.config.default_profile_id = Some(profile_id.clone());
            }
            repository.save(&config_path)?;
            Ok(ProfileCommandOutput {
                lines: vec![format!("added profile `{profile_id}`")],
            })
        }
        ProfileCommandAction::Edit {
            profile_id,
            display_name,
            refresh_interval_secs,
            membership_tier,
            plan_type,
        } => {
            let configured = repository
                .find_mut(profile_id)
                .ok_or_else(|| AuthError::MissingProfile(profile_id.clone()))?;
            if let Some(display_name) = display_name {
                configured.profile.display_name = display_name.clone();
            }
            if let Some(refresh_interval_secs) = refresh_interval_secs {
                configured.profile.refresh_policy.refresh_interval_secs = *refresh_interval_secs;
            }
            if membership_tier.is_some() || plan_type.is_some() {
                configured.profile.membership =
                    build_membership(membership_tier.as_deref(), plan_type.as_deref());
            }
            repository.save(&config_path)?;
            Ok(ProfileCommandOutput {
                lines: vec![format!("updated profile `{profile_id}`")],
            })
        }
        ProfileCommandAction::SetDefault { profile_id } => {
            repository.set_default(profile_id)?;
            repository.save(&config_path)?;
            Ok(ProfileCommandOutput {
                lines: vec![format!("default profile set to `{profile_id}`")],
            })
        }
        ProfileCommandAction::Remove { profile_id } => {
            let removed = repository.remove(profile_id)?;
            if repository.config.default_profile_id.as_deref() == Some(profile_id.as_str()) {
                repository.config.default_profile_id = repository
                    .config
                    .profiles
                    .first()
                    .map(|profile| profile.profile.id.clone());
            }
            match &removed.secret_ref {
                SecretRef::Keychain { .. } => {
                    let keyring_store = KeyringSecretStore::new("aistatus");
                    let _ = keyring_store.delete_secret(&removed.secret_ref, &removed.profile.id);
                }
                SecretRef::File { .. } => {
                    let file_store = FileSecretStore::from_app_paths(&app_paths);
                    let _ = file_store.delete_secret(&removed.secret_ref, &removed.profile.id);
                }
                SecretRef::Managed { .. } => {}
            }
            repository.save(&config_path)?;
            Ok(ProfileCommandOutput {
                lines: vec![format!("removed profile `{profile_id}`")],
            })
        }
        ProfileCommandAction::Login {
            profile_id,
            auth_mode,
            secret,
            use_file_store,
        } => {
            let configured = repository
                .find_mut(profile_id)
                .ok_or_else(|| AuthError::MissingProfile(profile_id.clone()))?;
            let secret_payload = build_auth_secret(auth_mode, secret)?;
            let secret_ref = if *use_file_store {
                let file_store = FileSecretStore::from_app_paths(&app_paths);
                file_store.write_secret(profile_id, &secret_payload)?
            } else {
                let keyring_store = KeyringSecretStore::new("aistatus");
                keyring_store.write_secret(profile_id, &secret_payload)?
            };
            configured.profile.auth_mode = parse_auth_mode(auth_mode)?;
            configured.secret_ref = secret_ref;
            repository.save(&config_path)?;
            Ok(ProfileCommandOutput {
                lines: vec![format!("stored login material for `{profile_id}`")],
            })
        }
        ProfileCommandAction::Logout { profile_id } => {
            let configured = repository
                .find_mut(profile_id)
                .ok_or_else(|| AuthError::MissingProfile(profile_id.clone()))?;
            match &configured.secret_ref {
                SecretRef::Keychain { .. } => {
                    let keyring_store = KeyringSecretStore::new("aistatus");
                    let _ = keyring_store.delete_secret(&configured.secret_ref, profile_id);
                }
                SecretRef::File { .. } => {
                    let file_store = FileSecretStore::from_app_paths(&app_paths);
                    let _ = file_store.delete_secret(&configured.secret_ref, profile_id);
                }
                SecretRef::Managed { .. } => {}
            }
            configured.secret_ref = SecretRef::Managed {
                id: format!("logged-out-{profile_id}"),
            };
            repository.save(&config_path)?;
            Ok(ProfileCommandOutput {
                lines: vec![format!("cleared login material for `{profile_id}`")],
            })
        }
    }
}

pub fn migrate_secret_material_to_file_store(
    repository: &mut ProfileRepository,
    secret_material: &SecretMaterial,
    store: &FileSecretStore,
) -> Result<(), AuthError> {
    for configured in &mut repository.config.profiles {
        let secret = secret_material
            .entries
            .iter()
            .find(|entry| entry.profile_id == configured.profile.id)
            .ok_or_else(|| StoreError::MissingSecret(configured.profile.id.clone()))?;

        configured.secret_ref = store.write_secret(&configured.profile.id, &secret.secret)?;
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub overall_status: DoctorStatus,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn render(&self) -> String {
        let mut lines = vec![format!(
            "overall: {}",
            match self.overall_status {
                DoctorStatus::Ok => "ok",
                DoctorStatus::Warning => "warning",
                DoctorStatus::Error => "error",
            }
        )];

        for check in &self.checks {
            let label = match check.status {
                DoctorStatus::Ok => "ok",
                DoctorStatus::Warning => "warning",
                DoctorStatus::Error => "error",
            };
            lines.push(format!("{label}: {} - {}", check.name, check.detail));
        }

        lines.join("\n")
    }
}

pub fn run_doctor(command: &DoctorCommand) -> Result<DoctorReport, AuthError> {
    let fixture_root = command
        .fixtures
        .as_ref()
        .map(|fixture| workspace_root().join(".sisyphus/fixtures").join(fixture));

    let config_path = command
        .config
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| fixture_root.as_ref().map(|root| root.join("config.toml")))
        .unwrap_or_else(|| {
            AppPaths::resolve("aistatus")
                .expect("app paths")
                .config_file
        });

    let mut checks = Vec::new();

    let repository = match ProfileRepository::load(&config_path) {
        Ok(repository) => {
            checks.push(DoctorCheck {
                name: "config".into(),
                status: DoctorStatus::Ok,
                detail: format!("loaded {} profiles", repository.config.profiles.len()),
            });
            repository
        }
        Err(error) => {
            checks.push(DoctorCheck {
                name: "config".into(),
                status: DoctorStatus::Error,
                detail: error.to_string(),
            });
            return Ok(DoctorReport {
                overall_status: DoctorStatus::Error,
                checks,
            });
        }
    };

    let maybe_secret_material = resolve_secret_material(&config_path, fixture_root.as_deref())?;
    if let Some(secret_material) = maybe_secret_material.as_ref() {
        match validate_secret_material(&repository.config, secret_material) {
            Ok(detail) => checks.push(DoctorCheck {
                name: "secret_material".into(),
                status: DoctorStatus::Ok,
                detail,
            }),
            Err(error) => checks.push(DoctorCheck {
                name: "secret_material".into(),
                status: DoctorStatus::Error,
                detail: error.to_string(),
            }),
        }
    }

    let app_paths = AppPaths::resolve("aistatus")?;
    let file_store = FileSecretStore::from_app_paths(&app_paths);
    let keyring_store = KeyringSecretStore::new("aistatus");

    for configured in &repository.config.profiles {
        let status = validate_profile_secret(
            configured,
            maybe_secret_material.as_ref(),
            &file_store,
            &keyring_store,
        );
        checks.push(status);
    }

    let overall_status = checks.iter().fold(DoctorStatus::Ok, |current, check| {
        match (current, check.status) {
            (_, DoctorStatus::Error) => DoctorStatus::Error,
            (DoctorStatus::Ok, DoctorStatus::Warning) => DoctorStatus::Warning,
            (status, _) => status,
        }
    });

    Ok(DoctorReport {
        overall_status,
        checks,
    })
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn resolve_secret_material(
    config_path: &Path,
    fixture_root: Option<&Path>,
) -> Result<Option<SecretMaterial>, AuthError> {
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

fn validate_secret_material(
    config: &PlainConfig,
    secret_material: &SecretMaterial,
) -> Result<String, StoreError> {
    for profile in &config.profiles {
        if !secret_material
            .entries
            .iter()
            .any(|entry| entry.profile_id == profile.profile.id)
        {
            return Err(StoreError::MissingSecret(profile.profile.id.clone()));
        }
    }

    Ok(format!(
        "loaded {} secret entries for {} profiles",
        secret_material.entries.len(),
        config.profiles.len()
    ))
}

fn validate_profile_secret(
    configured: &aistatus_config::ConfiguredProfile,
    secret_material: Option<&SecretMaterial>,
    file_store: &FileSecretStore,
    keyring_store: &KeyringSecretStore,
) -> DoctorCheck {
    if let Some(secret_material) = secret_material {
        match secret_material
            .entries
            .iter()
            .find(|entry| entry.profile_id == configured.profile.id)
        {
            Some(entry) => return validate_secret_entry(&configured.profile.id, &entry.secret),
            None => {
                return DoctorCheck {
                    name: format!("profile:{}", configured.profile.id),
                    status: DoctorStatus::Error,
                    detail: "missing secret entry in sidecar material".into(),
                };
            }
        }
    }

    let load_result = match &configured.secret_ref {
        SecretRef::File { .. } => {
            file_store.read_secret(&configured.secret_ref, &configured.profile.id)
        }
        SecretRef::Keychain { .. } => {
            keyring_store.read_secret(&configured.secret_ref, &configured.profile.id)
        }
        SecretRef::Managed { id } => {
            return DoctorCheck {
                name: format!("profile:{}", configured.profile.id),
                status: DoctorStatus::Warning,
                detail: format!("managed secret ref `{id}` requires external backend"),
            };
        }
    };

    match load_result {
        Ok(secret) => validate_secret_entry(&configured.profile.id, &secret),
        Err(error) => DoctorCheck {
            name: format!("profile:{}", configured.profile.id),
            status: DoctorStatus::Error,
            detail: error.to_string(),
        },
    }
}

fn validate_secret_entry(profile_id: &str, secret: &AuthSecret) -> DoctorCheck {
    let (status, detail) = match secret {
        AuthSecret::BrowserSession { session_payload } => {
            if session_payload.trim().is_empty() {
                (
                    DoctorStatus::Error,
                    "empty browser session payload".to_owned(),
                )
            } else {
                (
                    DoctorStatus::Ok,
                    "browser session payload present".to_owned(),
                )
            }
        }
        AuthSecret::HeadlessSession { session_payload } => {
            if session_payload.trim().is_empty() {
                (
                    DoctorStatus::Error,
                    "empty headless session payload".to_owned(),
                )
            } else {
                (
                    DoctorStatus::Ok,
                    "headless session payload present".to_owned(),
                )
            }
        }
        AuthSecret::ApiKey { api_key } => {
            if api_key.trim().is_empty() {
                (DoctorStatus::Error, "empty api key".to_owned())
            } else if api_key.starts_with("sk-") {
                (DoctorStatus::Ok, "api key shape looks valid".to_owned())
            } else {
                (
                    DoctorStatus::Warning,
                    "api key is non-empty but missing `sk-` prefix".to_owned(),
                )
            }
        }
    };

    DoctorCheck {
        name: format!("profile:{profile_id}"),
        status,
        detail,
    }
}

pub fn dispatch_command(command: &Command) -> Option<Result<DoctorReport, AuthError>> {
    match command {
        Command::Doctor(doctor) => Some(run_doctor(doctor)),
        _ => None,
    }
}

pub fn dispatch_profile_command(
    command: &Command,
) -> Option<Result<ProfileCommandOutput, AuthError>> {
    match command {
        Command::Profile(profile) => Some(run_profile_command(profile)),
        _ => None,
    }
}

fn resolve_profile_config_path(command: &ProfileCommand) -> PathBuf {
    command
        .config
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            AppPaths::resolve("aistatus")
                .expect("app paths")
                .config_file
        })
}

fn render_profile_list(repository: &ProfileRepository) -> ProfileCommandOutput {
    let mut lines = Vec::new();
    for configured in repository.list() {
        let marker = if repository.config.default_profile_id.as_deref()
            == Some(configured.profile.id.as_str())
        {
            "*"
        } else {
            " "
        };
        lines.push(format!(
            "{marker} {} | {} | {} | {}",
            configured.profile.id,
            configured.profile.display_name,
            render_auth_mode(&configured.profile.auth_mode),
            render_account_kind(&configured.profile.account_kind)
        ));
    }
    ProfileCommandOutput { lines }
}

fn build_profile(
    profile_id: &str,
    display_name: &str,
    provider: &str,
    auth_mode: &str,
    account_kind: &str,
    membership_tier: Option<&str>,
    plan_type: Option<&str>,
) -> Result<AccountProfile, AuthError> {
    Ok(AccountProfile {
        id: profile_id.to_owned(),
        display_name: display_name.to_owned(),
        provider: parse_provider(provider),
        auth_mode: parse_auth_mode(auth_mode)?,
        account_kind: parse_account_kind(account_kind),
        membership: build_membership(membership_tier, plan_type),
        health: AccountHealth::Healthy,
        refresh_policy: RefreshPolicy {
            refresh_interval_secs: 300,
            allow_manual_refresh: true,
        },
        future_switch_bridge: FutureSwitchBridge::default(),
    })
}

fn build_membership(
    membership_tier: Option<&str>,
    plan_type: Option<&str>,
) -> Option<AccountMembership> {
    let raw_plan_type = plan_type.map(|value| value.to_owned());
    let tier = membership_tier
        .map(parse_membership_tier)
        .or_else(|| AccountMembership::from_plan_type(plan_type).map(|membership| membership.tier));

    tier.map(|tier| AccountMembership {
        tier,
        raw_plan_type,
    })
}

fn build_auth_secret(auth_mode: &str, secret: &str) -> Result<AuthSecret, AuthError> {
    Ok(match parse_auth_mode(auth_mode)? {
        AuthMode::Browser => AuthSecret::BrowserSession {
            session_payload: secret.to_owned(),
        },
        AuthMode::Headless => AuthSecret::HeadlessSession {
            session_payload: secret.to_owned(),
        },
        AuthMode::ApiKey => AuthSecret::ApiKey {
            api_key: secret.to_owned(),
        },
    })
}

fn parse_provider(value: &str) -> ProviderKind {
    match value {
        "codex_protocol" => ProviderKind::CodexProtocol,
        "openai_api_usage" => ProviderKind::OpenAiApiUsage,
        other => ProviderKind::Other(other.to_owned()),
    }
}

fn parse_auth_mode(value: &str) -> Result<AuthMode, AuthError> {
    match value {
        "browser" => Ok(AuthMode::Browser),
        "headless" => Ok(AuthMode::Headless),
        "api_key" => Ok(AuthMode::ApiKey),
        other => Err(AuthError::InvalidAuthMode(other.to_owned())),
    }
}

fn parse_account_kind(value: &str) -> AccountKind {
    match value {
        "chatgpt" => AccountKind::Chatgpt,
        "api_key" => AccountKind::ApiKey,
        other => AccountKind::Other(other.to_owned()),
    }
}

fn parse_membership_tier(value: &str) -> MembershipTier {
    match value {
        "free" => MembershipTier::Free,
        "go" => MembershipTier::Go,
        "plus" => MembershipTier::Plus,
        "pro" => MembershipTier::Pro,
        "team" => MembershipTier::Team,
        "edu" => MembershipTier::Edu,
        "business" => MembershipTier::Business,
        "enterprise" => MembershipTier::Enterprise,
        "unknown" => MembershipTier::Unknown,
        _ => MembershipTier::Other,
    }
}

fn render_auth_mode(value: &AuthMode) -> &'static str {
    match value {
        AuthMode::Browser => "browser",
        AuthMode::Headless => "headless",
        AuthMode::ApiKey => "api_key",
    }
}

fn render_account_kind(value: &AccountKind) -> &'static str {
    match value {
        AccountKind::Chatgpt => "chatgpt",
        AccountKind::ApiKey => "api_key",
        AccountKind::Other(_) => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aistatus_config::{ConfiguredProfile, SecretEntry, SecretRef};
    use aistatus_core::{
        AccountHealth, AccountKind, AccountMembership, AuthMode, FutureSwitchBridge,
        MembershipTier, ProviderKind, RefreshPolicy,
    };
    use aistatus_store::FileSecretStore;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("aistatus-auth-{label}-{nanos}"))
    }

    fn sample_profile(profile_id: &str) -> AccountProfile {
        AccountProfile {
            id: profile_id.to_owned(),
            display_name: "Primary".to_owned(),
            provider: ProviderKind::CodexProtocol,
            auth_mode: AuthMode::Browser,
            account_kind: AccountKind::Chatgpt,
            membership: Some(AccountMembership {
                tier: MembershipTier::Plus,
                raw_plan_type: Some("plus".to_owned()),
            }),
            health: AccountHealth::Healthy,
            refresh_policy: RefreshPolicy {
                refresh_interval_secs: 300,
                allow_manual_refresh: true,
            },
            future_switch_bridge: FutureSwitchBridge::default(),
        }
    }

    fn write_config(config_path: &Path, configured: &ConfiguredProfile) {
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent).expect("config fixture parent should exist");
        }
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

    #[test]
    fn repository_upsert_and_remove_profiles() {
        let mut repository = ProfileRepository {
            config: PlainConfig {
                version: 1,
                default_profile_id: None,
                profiles: Vec::new(),
            },
        };

        repository.upsert(
            sample_profile("acct-1"),
            SecretRef::Managed {
                id: "managed-1".into(),
            },
        );
        assert_eq!(repository.list().len(), 1);

        let removed = repository.remove("acct-1").expect("profile should exist");
        assert_eq!(removed.profile.id, "acct-1");
        assert!(repository.list().is_empty());
    }

    #[test]
    fn migration_moves_legacy_secret_material_into_file_store_refs() {
        let root = temp_dir("migration");
        let store = FileSecretStore::new(root.join("secrets"), root.join("master.key"));
        let mut repository = ProfileRepository {
            config: PlainConfig {
                version: 1,
                default_profile_id: Some("acct-1".into()),
                profiles: vec![ConfiguredProfile {
                    profile: sample_profile("acct-1"),
                    secret_ref: SecretRef::Managed {
                        id: "legacy-1".into(),
                    },
                }],
            },
        };
        let secret_material = SecretMaterial {
            version: 1,
            entries: vec![SecretEntry {
                profile_id: "acct-1".into(),
                secret: AuthSecret::BrowserSession {
                    session_payload: "cookie=value".into(),
                },
            }],
        };

        migrate_secret_material_to_file_store(&mut repository, &secret_material, &store)
            .expect("migration should succeed");

        assert!(matches!(
            repository.list()[0].secret_ref,
            SecretRef::File { .. }
        ));
    }

    #[test]
    fn doctor_reports_corrupted_fixture_as_error() {
        let report = run_doctor(&DoctorCommand {
            config: None,
            fixtures: Some("corrupted-session".into()),
        })
        .expect("doctor should produce report");

        assert_eq!(report.overall_status, DoctorStatus::Error);
        assert!(report
            .checks
            .iter()
            .any(|check| check.detail.contains("empty browser session payload")));
    }

    #[test]
    fn profile_commands_list_and_set_default_are_deterministic() {
        let root = temp_dir("profile-cmds");
        let config_path = root.join("config.toml");

        run_profile_command(&ProfileCommand {
            config: Some(config_path.to_string_lossy().to_string()),
            action: ProfileCommandAction::Add {
                profile_id: "acct-primary".into(),
                display_name: "Primary".into(),
                auth_mode: "browser".into(),
                account_kind: "chatgpt".into(),
                provider: "codex_protocol".into(),
                membership_tier: Some("plus".into()),
                plan_type: Some("plus".into()),
            },
        })
        .expect("add primary should succeed");

        run_profile_command(&ProfileCommand {
            config: Some(config_path.to_string_lossy().to_string()),
            action: ProfileCommandAction::Add {
                profile_id: "acct-secondary".into(),
                display_name: "Secondary".into(),
                auth_mode: "api_key".into(),
                account_kind: "api_key".into(),
                provider: "codex_protocol".into(),
                membership_tier: None,
                plan_type: None,
            },
        })
        .expect("add secondary should succeed");

        let list_before = run_profile_command(&ProfileCommand {
            config: Some(config_path.to_string_lossy().to_string()),
            action: ProfileCommandAction::List,
        })
        .expect("list should succeed")
        .render();
        assert!(list_before.contains("* acct-primary"));

        run_profile_command(&ProfileCommand {
            config: Some(config_path.to_string_lossy().to_string()),
            action: ProfileCommandAction::SetDefault {
                profile_id: "acct-secondary".into(),
            },
        })
        .expect("set default should succeed");

        let list_after = run_profile_command(&ProfileCommand {
            config: Some(config_path.to_string_lossy().to_string()),
            action: ProfileCommandAction::List,
        })
        .expect("list should succeed")
        .render();
        assert!(list_after.contains("* acct-secondary"));
    }

    #[test]
    fn profile_add_rejects_unknown_auth_mode_without_writing_config() {
        let root = temp_dir("profile-add-invalid-auth-mode");
        let config_path = root.join("config.toml");

        let error = run_profile_command(&ProfileCommand {
            config: Some(config_path.to_string_lossy().to_string()),
            action: ProfileCommandAction::Add {
                profile_id: "acct-invalid".into(),
                display_name: "Invalid".into(),
                auth_mode: "cookiejar".into(),
                account_kind: "chatgpt".into(),
                provider: "codex_protocol".into(),
                membership_tier: None,
                plan_type: None,
            },
        })
        .expect_err("invalid auth mode should fail");

        assert!(matches!(error, AuthError::InvalidAuthMode(mode) if mode == "cookiejar"));
        assert!(
            !config_path.exists(),
            "invalid add should not persist config"
        );
    }

    #[test]
    fn profile_login_rejects_unknown_auth_mode_without_persisting_changes() {
        let root = temp_dir("profile-login-invalid-auth-mode");
        let config_path = root.join("config.toml");
        let configured = ConfiguredProfile {
            profile: sample_profile("acct-login"),
            secret_ref: SecretRef::Managed {
                id: "managed-login".into(),
            },
        };
        write_config(&config_path, &configured);

        let error = run_profile_command(&ProfileCommand {
            config: Some(config_path.to_string_lossy().to_string()),
            action: ProfileCommandAction::Login {
                profile_id: "acct-login".into(),
                auth_mode: "cookiejar".into(),
                secret: "payload".into(),
                use_file_store: false,
            },
        })
        .expect_err("invalid auth mode should fail");

        assert!(matches!(error, AuthError::InvalidAuthMode(mode) if mode == "cookiejar"));

        let reloaded =
            ProfileRepository::load(&config_path).expect("config should remain readable");
        assert_eq!(reloaded.list()[0].profile.auth_mode, AuthMode::Browser);
        assert!(matches!(
            reloaded.list()[0].secret_ref,
            SecretRef::Managed { .. }
        ));
    }
}
