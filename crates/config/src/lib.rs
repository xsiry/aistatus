use aistatus_core::AccountProfile;
use serde::{Deserialize, Serialize};

/// Versioned plain-text config that stores profile metadata and secret references, but never secret payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlainConfig {
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile_id: Option<String>,
    pub profiles: Vec<ConfiguredProfile>,
}

impl PlainConfig {
    /// Parses the human-editable TOML config while keeping secret material in a separate contract.
    pub fn from_toml_str(input: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(input)
    }

    /// Serializes the human-editable TOML config. Secret references remain opaque identifiers only.
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }
}

/// Couples a canonical domain profile with the opaque handle used to look up its secrets elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfiguredProfile {
    pub profile: AccountProfile,
    pub secret_ref: SecretRef,
}

/// Opaque secret lookup target that may point at a keychain entry, file-backed store, or other secret backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SecretRef {
    Keychain {
        service: String,
        account: String,
    },
    File {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        key_path: Option<String>,
    },
    Managed {
        id: String,
    },
}

/// Separate serialized contract for secret/session material that must never live in plain TOML config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMaterial {
    pub version: u32,
    pub entries: Vec<SecretEntry>,
}

impl SecretMaterial {
    /// Serializes secret payloads for secure-store or sidecar JSON usage outside the plain config path.
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Parses previously stored secret payloads from JSON.
    pub fn from_json_str(input: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(input)
    }
}

/// Associates one profile ID with its auth payload in the separate secret material contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretEntry {
    pub profile_id: String,
    pub secret: AuthSecret,
}

/// Secret/session payload variants for the three supported auth modes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthSecret {
    BrowserSession { session_payload: String },
    HeadlessSession { session_payload: String },
    ApiKey { api_key: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use aistatus_core::{AccountKind, MembershipTier, ProviderKind};

    #[test]
    fn config_roundtrip_plain_config_excludes_secret_payloads() {
        let fixture = include_str!("../tests/fixtures/sample-config.toml");
        let config =
            PlainConfig::from_toml_str(fixture).expect("plain config fixture should parse");

        assert_eq!(config.version, 1);
        assert_eq!(
            config.default_profile_id.as_deref(),
            Some("acct-chatgpt-plus")
        );
        assert_eq!(config.profiles.len(), 2);
        assert_eq!(
            config.profiles[0].profile.provider,
            ProviderKind::CodexProtocol
        );
        assert_eq!(
            config.profiles[0].profile.account_kind,
            AccountKind::Chatgpt
        );
        assert_eq!(
            config.profiles[0]
                .profile
                .membership
                .as_ref()
                .map(|membership| &membership.tier),
            Some(&MembershipTier::Plus)
        );

        let rendered = config
            .to_toml_string()
            .expect("plain config should serialize");
        assert!(rendered.contains("secret_ref"));
        assert!(rendered.contains("codex-desktop"));
        assert!(!rendered.contains("sk-live-123"));
        assert!(!rendered.contains("session-cookie-value"));
    }

    #[test]
    fn config_roundtrip_secret_material_stays_in_separate_json_contract() {
        let fixture = include_str!("../tests/fixtures/sample-secrets.json");
        let secrets = SecretMaterial::from_json_str(fixture).expect("secret fixture should parse");

        assert_eq!(secrets.version, 1);
        assert_eq!(secrets.entries.len(), 2);

        let rendered = secrets
            .to_json_string()
            .expect("secret material should serialize");
        assert!(rendered.contains("sk-live-123"));
        assert!(rendered.contains("session-cookie-value"));
    }
}
