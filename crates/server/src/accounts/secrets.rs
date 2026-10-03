//! API-key providers with keys encrypted at rest (SPEC FR-U5).
//!
//! # Scheme
//!
//! * **Cipher:** XChaCha20-Poly1305 (RustCrypto `chacha20poly1305`), an AEAD with a 192-bit nonce,
//!   so a fresh random nonce per encryption (from the OS RNG) never realistically repeats.
//! * **Key:** 32 random bytes in `<data dir>/secret.key`, created once with mode `0600` (`O_EXCL`,
//!   so two servers never race to different keys). The server refuses to start if the file is
//!   readable by group or others, or is not exactly 32 bytes.
//! * **Binding:** the associated data is `ember/api-provider-key/v1:<provider id>`, so a ciphertext
//!   copied onto another provider's row fails to decrypt instead of yielding the wrong key.
//! * **Storage:** `api_providers.key_nonce` (24 bytes) and `key_ciphertext` (key + 16-byte tag).
//!
//! The database alone does not reveal keys; the database plus `secret.key` does. That is the
//! intended boundary: it protects copies of `ember.db` (backups, exports, bug reports), not a
//! compromised server account. A keychain/KMS-held key can replace the key file later without a
//! schema change.
//!
//! # Never leaked
//!
//! Plaintext keys only exist in [`ApiKey`], which has a redacting `Debug`, no `Serialize`, and is
//! zeroed on drop. [`ApiProvider`] (what list/create return) carries no key material at all, not
//! even a suffix. Keys are not events, so they never reach transcripts or the push channel.

use std::io::Write;
use std::path::Path;

use anyhow::Context;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use zeroize::Zeroizing;

use crate::store::{now_ms, Store};

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const AAD_PREFIX: &str = "ember/api-provider-key/v1:";

/// The server's at-rest encryption key.
pub struct SecretBox {
    cipher: XChaCha20Poly1305,
}

impl SecretBox {
    /// Load `path`, creating it with a fresh random key (mode `0600`) if it does not exist.
    pub fn open_or_create(path: &Path) -> anyhow::Result<SecretBox> {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode_0600()
            .open(path)
        {
            Ok(mut f) => {
                let key = XChaCha20Poly1305::generate_key(&mut OsRng);
                f.write_all(&key)?;
                f.sync_all()?;
                return Ok(SecretBox {
                    cipher: XChaCha20Poly1305::new(&key),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
        }
        check_private(path)?;
        let bytes = Zeroizing::new(
            std::fs::read(path).with_context(|| format!("reading {}", path.display()))?,
        );
        anyhow::ensure!(
            bytes.len() == KEY_LEN,
            "{} is corrupt: expected {KEY_LEN} bytes, found {}",
            path.display(),
            bytes.len()
        );
        let cipher = XChaCha20Poly1305::new_from_slice(&bytes).expect("length checked");
        Ok(SecretBox { cipher })
    }

    /// A throwaway key, for tests and in-memory stores.
    pub fn ephemeral() -> SecretBox {
        SecretBox {
            cipher: XChaCha20Poly1305::new(&XChaCha20Poly1305::generate_key(&mut OsRng)),
        }
    }

    /// Encrypt `plaintext`, bound to `aad`. Returns `(nonce, ciphertext)`. Also used for the
    /// ChatGPT sign-in tokens (`crate::chatgpt`), with their own AAD prefix.
    pub(crate) fn seal(&self, plaintext: &[u8], aad: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ct = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .expect("XChaCha20-Poly1305 encryption does not fail for in-memory input");
        (nonce.to_vec(), ct)
    }

    pub(crate) fn open(
        &self,
        nonce: &[u8],
        ct: &[u8],
        aad: &[u8],
    ) -> anyhow::Result<Zeroizing<Vec<u8>>> {
        anyhow::ensure!(nonce.len() == NONCE_LEN, "bad nonce length");
        let pt = self
            .cipher
            .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
            .map_err(|_| {
                anyhow::anyhow!("API key failed to decrypt (wrong secret.key or tampered row)")
            })?;
        Ok(Zeroizing::new(pt))
    }
}

/// 32 bytes from the OS RNG (PKCE verifiers, OAuth `state` and `nonce`).
pub(crate) fn random_32() -> Zeroizing<[u8; KEY_LEN]> {
    let key = XChaCha20Poly1305::generate_key(&mut OsRng);
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    out.copy_from_slice(key.as_slice());
    out
}

trait Mode0600 {
    fn mode_0600(&mut self) -> &mut Self;
}

impl Mode0600 for std::fs::OpenOptions {
    #[cfg(unix)]
    fn mode_0600(&mut self) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.mode(0o600)
    }
    #[cfg(not(unix))]
    fn mode_0600(&mut self) -> &mut Self {
        self
    }
}

#[cfg(unix)]
fn check_private(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    anyhow::ensure!(
        mode & 0o077 == 0,
        "{} must not be readable by group or others (mode {:o}); run chmod 600 on it",
        path.display(),
        mode & 0o777
    );
    Ok(())
}

#[cfg(not(unix))]
fn check_private(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// A decrypted API key. Never serialised, redacted in `Debug`, zeroed on drop.
pub struct ApiKey(Zeroizing<String>);

impl ApiKey {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// An API-key provider as clients see it: no key material.
#[derive(Debug, Clone, Serialize)]
pub struct ApiProvider {
    pub id: String,
    pub label: String,
    /// e.g. `openai`, `anthropic`, `openai-compatible`.
    pub kind: String,
    pub base_url: Option<String>,
    pub created_at: i64,
}

fn aad(id: &str) -> Vec<u8> {
    format!("{AAD_PREFIX}{id}").into_bytes()
}

pub fn create_provider(
    store: &Store,
    secrets: &SecretBox,
    label: &str,
    kind: &str,
    base_url: Option<&str>,
    api_key: &str,
) -> anyhow::Result<ApiProvider> {
    anyhow::ensure!(!api_key.is_empty(), "api_key is empty");
    let id = uuid::Uuid::new_v4().to_string();
    let (nonce, ct) = secrets.seal(api_key.as_bytes(), &aad(&id));
    let created_at = now_ms();
    store.conn().execute(
        "INSERT INTO api_providers (id, label, kind, base_url, key_nonce, key_ciphertext, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, label, kind, base_url, nonce, ct, created_at],
    )?;
    Ok(ApiProvider {
        id,
        label: label.into(),
        kind: kind.into(),
        base_url: base_url.map(str::to_string),
        created_at,
    })
}

pub fn list_providers(store: &Store) -> anyhow::Result<Vec<ApiProvider>> {
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT id, label, kind, base_url, created_at FROM api_providers ORDER BY created_at",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ApiProvider {
            id: r.get(0)?,
            label: r.get(1)?,
            kind: r.get(2)?,
            base_url: r.get(3)?,
            created_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn delete_provider(store: &Store, id: &str) -> anyhow::Result<bool> {
    Ok(store
        .conn()
        .execute("DELETE FROM api_providers WHERE id = ?1", params![id])?
        > 0)
}

/// Decrypt a provider's key, for the code that calls the provider. Never send it to a client.
pub fn provider_key(
    store: &Store,
    secrets: &SecretBox,
    id: &str,
) -> anyhow::Result<Option<ApiKey>> {
    let row: Option<(Vec<u8>, Vec<u8>)> = store
        .conn()
        .query_row(
            "SELECT key_nonce, key_ciphertext FROM api_providers WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((nonce, ct)) = row else {
        return Ok(None);
    };
    let pt = secrets.open(&nonce, &ct, &aad(id))?;
    let key = String::from_utf8(pt.to_vec()).context("stored API key is not UTF-8")?;
    Ok(Some(ApiKey(Zeroizing::new(key))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_ciphertext_is_bound_to_its_row() {
        let store = Store::open_in_memory().unwrap();
        let sb = SecretBox::ephemeral();
        let a = create_provider(&store, &sb, "work", "openai", None, "sk-live-AAAA").unwrap();
        let b = create_provider(&store, &sb, "home", "anthropic", None, "sk-ant-BBBB").unwrap();
        assert_eq!(
            provider_key(&store, &sb, &a.id).unwrap().unwrap().expose(),
            "sk-live-AAAA"
        );
        assert_eq!(
            provider_key(&store, &sb, &b.id).unwrap().unwrap().expose(),
            "sk-ant-BBBB"
        );

        // Not stored in plaintext.
        let raw: Vec<u8> = store
            .conn()
            .query_row(
                "SELECT key_ciphertext FROM api_providers WHERE id = ?1",
                [&a.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!raw.windows(4).any(|w| w == b"AAAA"));

        // Swapping ciphertexts between rows fails instead of returning the other key.
        store
            .conn()
            .execute_batch(&format!(
                "UPDATE api_providers SET key_nonce = (SELECT key_nonce FROM api_providers WHERE id = '{b}'),
                   key_ciphertext = (SELECT key_ciphertext FROM api_providers WHERE id = '{b}')
                 WHERE id = '{a}'",
                a = a.id,
                b = b.id
            ))
            .unwrap();
        assert!(provider_key(&store, &sb, &a.id).is_err());
        // A different server key cannot read them.
        assert!(provider_key(&store, &SecretBox::ephemeral(), &b.id).is_err());
        assert_eq!(
            format!("{:?}", provider_key(&store, &sb, &b.id).unwrap().unwrap()),
            "ApiKey(<redacted>)"
        );
    }

    #[test]
    fn list_never_contains_key_material() {
        let store = Store::open_in_memory().unwrap();
        let sb = SecretBox::ephemeral();
        create_provider(
            &store,
            &sb,
            "p",
            "openai-compatible",
            Some("http://x"),
            "sk-SECRET-123",
        )
        .unwrap();
        let json = serde_json::to_string(&list_providers(&store).unwrap()).unwrap();
        assert!(!json.contains("SECRET"), "{json}");
        assert!(!json.contains("key"), "{json}");
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_private_and_stable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");
        let store = Store::open_in_memory().unwrap();
        let sb = SecretBox::open_or_create(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let p = create_provider(&store, &sb, "p", "openai", None, "sk-1").unwrap();
        // Reopened key decrypts what the first instance wrote.
        let again = SecretBox::open_or_create(&path).unwrap();
        assert_eq!(
            provider_key(&store, &again, &p.id)
                .unwrap()
                .unwrap()
                .expose(),
            "sk-1"
        );
        // A world-readable key file is refused.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(SecretBox::open_or_create(&path).is_err());
    }
}
