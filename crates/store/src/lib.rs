use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use aistatus_config::{AuthSecret, SecretRef};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use directories::ProjectDirs;
use keyring::Entry;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const ENCRYPTED_FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub secrets_dir: PathBuf,
    pub master_key_file: PathBuf,
    pub doctor_log_file: PathBuf,
}

impl AppPaths {
    pub fn resolve(app_name: &str) -> Result<Self, StoreError> {
        let project_dirs = ProjectDirs::from("dev", "xsiry", app_name)
            .ok_or_else(|| StoreError::PathResolution("unable to resolve project dirs".into()))?;

        let config_dir = project_dirs.config_dir().to_path_buf();
        let data_dir = project_dirs.data_local_dir().to_path_buf();
        let secrets_dir = data_dir.join("secrets");

        Ok(Self {
            config_file: config_dir.join("config.toml"),
            master_key_file: data_dir.join("master.key"),
            doctor_log_file: data_dir.join("codex-login.log"),
            config_dir,
            data_dir,
            secrets_dir,
        })
    }

    pub fn ensure(&self) -> Result<(), StoreError> {
        ensure_dir(&self.config_dir)?;
        ensure_dir(&self.data_dir)?;
        ensure_dir(&self.secrets_dir)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreProbe {
    pub available: bool,
    pub detail: String,
}

pub trait SecretStore {
    fn write_secret(&self, profile_id: &str, secret: &AuthSecret) -> Result<SecretRef, StoreError>;
    fn read_secret(
        &self,
        secret_ref: &SecretRef,
        profile_id: &str,
    ) -> Result<AuthSecret, StoreError>;
    fn delete_secret(&self, secret_ref: &SecretRef, profile_id: &str) -> Result<(), StoreError>;
    fn probe(&self, secret_ref: Option<&SecretRef>) -> StoreProbe;
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("path resolution failed: {0}")]
    PathResolution(String),
    #[error("io failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("json failure: {0}")]
    Json(#[from] serde_json::Error),
    #[error("keyring failure: {0}")]
    Keyring(String),
    #[error("encryption failure")]
    Encryption,
    #[error("decryption failure")]
    Decryption,
    #[error("missing secret entry for profile `{0}`")]
    MissingSecret(String),
    #[error("invalid secret reference kind for this backend")]
    InvalidSecretRef,
    #[error("invalid master key length: expected 32 bytes, got {0}")]
    InvalidMasterKey(usize),
}

#[derive(Debug, Clone)]
pub struct FileSecretStore {
    root_dir: PathBuf,
    master_key_path: PathBuf,
}

impl FileSecretStore {
    pub fn new(root_dir: impl Into<PathBuf>, master_key_path: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            master_key_path: master_key_path.into(),
        }
    }

    pub fn from_app_paths(paths: &AppPaths) -> Self {
        Self::new(paths.secrets_dir.clone(), paths.master_key_file.clone())
    }

    fn entry_path(&self, profile_id: &str) -> PathBuf {
        let encoded = URL_SAFE_NO_PAD.encode(profile_id.as_bytes());
        self.root_dir.join(format!("{encoded}.secret.json"))
    }

    fn load_or_create_master_key(&self) -> Result<[u8; 32], StoreError> {
        if self.master_key_path.exists() {
            let bytes = fs::read(&self.master_key_path)?;
            if bytes.len() != 32 {
                return Err(StoreError::InvalidMasterKey(bytes.len()));
            }

            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            return Ok(key);
        }

        if let Some(parent) = self.master_key_path.parent() {
            ensure_dir(parent)?;
        }

        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        write_private_file(&self.master_key_path, &key)?;
        Ok(key)
    }

    fn encrypt_secret(&self, secret: &AuthSecret) -> Result<EncryptedSecretFile, StoreError> {
        let serialized = serde_json::to_vec(secret)?;
        let key = self.load_or_create_master_key()?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));

        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, serialized.as_ref())
            .map_err(|_| StoreError::Encryption)?;

        Ok(EncryptedSecretFile {
            version: ENCRYPTED_FILE_VERSION,
            nonce: URL_SAFE_NO_PAD.encode(nonce_bytes),
            ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
        })
    }

    fn decrypt_secret(&self, envelope: EncryptedSecretFile) -> Result<AuthSecret, StoreError> {
        if envelope.version != ENCRYPTED_FILE_VERSION {
            return Err(StoreError::Decryption);
        }

        let key = self.load_or_create_master_key()?;
        let nonce_bytes = URL_SAFE_NO_PAD
            .decode(envelope.nonce)
            .map_err(|_| StoreError::Decryption)?;
        if nonce_bytes.len() != 12 {
            return Err(StoreError::Decryption);
        }
        let ciphertext = URL_SAFE_NO_PAD
            .decode(envelope.ciphertext)
            .map_err(|_| StoreError::Decryption)?;

        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&nonce_bytes), ciphertext.as_ref())
            .map_err(|_| StoreError::Decryption)?;

        serde_json::from_slice(&plaintext).map_err(StoreError::from)
    }
}

impl SecretStore for FileSecretStore {
    fn write_secret(&self, profile_id: &str, secret: &AuthSecret) -> Result<SecretRef, StoreError> {
        ensure_dir(&self.root_dir)?;
        let entry_path = self.entry_path(profile_id);
        let envelope = self.encrypt_secret(secret)?;
        let bytes = serde_json::to_vec_pretty(&envelope)?;
        write_private_file(&entry_path, &bytes)?;

        Ok(SecretRef::File {
            path: entry_path.to_string_lossy().to_string(),
            key_path: Some(self.master_key_path.to_string_lossy().to_string()),
        })
    }

    fn read_secret(
        &self,
        secret_ref: &SecretRef,
        profile_id: &str,
    ) -> Result<AuthSecret, StoreError> {
        let (path, key_path) = match secret_ref {
            SecretRef::File { path, key_path } => (PathBuf::from(path), key_path.clone()),
            _ => return Err(StoreError::InvalidSecretRef),
        };

        let scoped_store = Self::new(
            path.parent().unwrap_or(&self.root_dir),
            key_path.unwrap_or_else(|| self.master_key_path.to_string_lossy().to_string()),
        );
        let entry_path = if path.is_dir() {
            scoped_store.entry_path(profile_id)
        } else {
            path
        };
        let bytes = fs::read(entry_path)?;
        let envelope: EncryptedSecretFile = serde_json::from_slice(&bytes)?;
        scoped_store.decrypt_secret(envelope)
    }

    fn delete_secret(&self, secret_ref: &SecretRef, profile_id: &str) -> Result<(), StoreError> {
        let path = match secret_ref {
            SecretRef::File { path, .. } => {
                let resolved = PathBuf::from(path);
                if resolved.is_dir() {
                    self.entry_path(profile_id)
                } else {
                    resolved
                }
            }
            _ => return Err(StoreError::InvalidSecretRef),
        };

        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    fn probe(&self, secret_ref: Option<&SecretRef>) -> StoreProbe {
        let path_exists = match secret_ref {
            Some(SecretRef::File { path, key_path }) => {
                let file_ok = Path::new(path).exists();
                let key_ok = key_path
                    .as_ref()
                    .map(|candidate| Path::new(candidate).exists())
                    .unwrap_or_else(|| self.master_key_path.exists());
                file_ok && key_ok
            }
            Some(_) => false,
            None => self.root_dir.exists() || self.master_key_path.exists(),
        };

        StoreProbe {
            available: path_exists || self.root_dir.exists(),
            detail: if path_exists {
                "file-backed secret store reachable".into()
            } else {
                "file-backed secret store not initialized".into()
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    default_service: String,
}

impl KeyringSecretStore {
    pub fn new(default_service: impl Into<String>) -> Self {
        Self {
            default_service: default_service.into(),
        }
    }

    fn entry_for(&self, service: &str, account: &str) -> Result<Entry, StoreError> {
        Entry::new(service, account).map_err(|error| StoreError::Keyring(error.to_string()))
    }
}

impl SecretStore for KeyringSecretStore {
    fn write_secret(&self, profile_id: &str, secret: &AuthSecret) -> Result<SecretRef, StoreError> {
        let entry = self.entry_for(&self.default_service, profile_id)?;
        let serialized = serde_json::to_string(secret)?;
        entry
            .set_password(&serialized)
            .map_err(|error| StoreError::Keyring(error.to_string()))?;

        Ok(SecretRef::Keychain {
            service: self.default_service.clone(),
            account: profile_id.to_owned(),
        })
    }

    fn read_secret(
        &self,
        secret_ref: &SecretRef,
        _profile_id: &str,
    ) -> Result<AuthSecret, StoreError> {
        let (service, account) = match secret_ref {
            SecretRef::Keychain { service, account } => (service, account),
            _ => return Err(StoreError::InvalidSecretRef),
        };

        let entry = self.entry_for(service, account)?;
        let serialized = entry
            .get_password()
            .map_err(|error| StoreError::Keyring(error.to_string()))?;
        serde_json::from_str(&serialized).map_err(StoreError::from)
    }

    fn delete_secret(&self, secret_ref: &SecretRef, _profile_id: &str) -> Result<(), StoreError> {
        let (service, account) = match secret_ref {
            SecretRef::Keychain { service, account } => (service, account),
            _ => return Err(StoreError::InvalidSecretRef),
        };

        let entry = self.entry_for(service, account)?;
        entry
            .delete_credential()
            .map_err(|error| StoreError::Keyring(error.to_string()))
    }

    fn probe(&self, secret_ref: Option<&SecretRef>) -> StoreProbe {
        let result = match secret_ref {
            Some(SecretRef::Keychain { service, account }) => self.entry_for(service, account),
            Some(_) => Err(StoreError::InvalidSecretRef),
            None => self.entry_for(&self.default_service, "probe"),
        };

        match result {
            Ok(_) => StoreProbe {
                available: true,
                detail: "native keyring entry is addressable".into(),
            },
            Err(error) => StoreProbe {
                available: false,
                detail: error.to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EncryptedSecretFile {
    version: u32,
    nonce: String,
    ciphertext: String,
}

fn ensure_dir(path: &Path) -> Result<(), StoreError> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }

    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path)?;
    file.write_all(bytes)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("aistatus-{label}-{nanos}"))
    }

    #[test]
    fn file_secret_store_roundtrips_api_keys() {
        let root = temp_dir("store-roundtrip");
        let store = FileSecretStore::new(root.join("secrets"), root.join("master.key"));
        let secret = AuthSecret::ApiKey {
            api_key: "sk-test-123".to_owned(),
        };

        let secret_ref = store
            .write_secret("acct-api-key", &secret)
            .expect("secret should save");
        let loaded = store
            .read_secret(&secret_ref, "acct-api-key")
            .expect("secret should load");

        assert_eq!(loaded, secret);
        assert!(matches!(secret_ref, SecretRef::File { .. }));
    }

    #[test]
    fn file_secret_store_reports_corrupted_payload() {
        let root = temp_dir("store-corrupt");
        let store = FileSecretStore::new(root.join("secrets"), root.join("master.key"));
        let secret = AuthSecret::BrowserSession {
            session_payload: "cookie=value".to_owned(),
        };

        let secret_ref = store
            .write_secret("acct-browser", &secret)
            .expect("secret should save");

        let path = match &secret_ref {
            SecretRef::File { path, .. } => PathBuf::from(path),
            _ => panic!("expected file secret ref"),
        };
        fs::write(path, b"not-json").expect("should overwrite fixture with invalid data");

        let error = store
            .read_secret(&secret_ref, "acct-browser")
            .expect_err("corrupted secret should fail");

        assert!(matches!(error, StoreError::Json(_)) || matches!(error, StoreError::Decryption));
    }

    #[test]
    fn file_secret_store_rejects_invalid_nonce_length() {
        let root = temp_dir("store-invalid-nonce");
        let store = FileSecretStore::new(root.join("secrets"), root.join("master.key"));
        let secret = AuthSecret::ApiKey {
            api_key: "sk-test-123".to_owned(),
        };

        let secret_ref = store
            .write_secret("acct-api-key", &secret)
            .expect("secret should save");
        let path = match &secret_ref {
            SecretRef::File { path, .. } => PathBuf::from(path),
            _ => panic!("expected file secret ref"),
        };

        let bad_envelope = EncryptedSecretFile {
            version: ENCRYPTED_FILE_VERSION,
            nonce: URL_SAFE_NO_PAD.encode([0u8; 11]),
            ciphertext: URL_SAFE_NO_PAD.encode([1u8; 16]),
        };
        fs::write(
            path,
            serde_json::to_vec_pretty(&bad_envelope).expect("envelope should serialize"),
        )
        .expect("should write malformed envelope");

        let error = store
            .read_secret(&secret_ref, "acct-api-key")
            .expect_err("invalid nonce length should fail");

        assert!(matches!(error, StoreError::Decryption));
    }
}
