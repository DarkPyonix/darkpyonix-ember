//! Rows of `chatgpt_accounts`, the encrypted token blob, the host id and usage.
//!
//! Tokens are sealed with the server's [`SecretBox`] (the FR-U5 scheme) under the associated data
//! `ember/chatgpt-tokens/v1:<account id>`, so a blob copied onto another row does not decrypt.
//! Plaintext tokens only exist in [`Tokens`]: redacted `Debug`, no `Serialize`, zeroed on drop.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::accounts::secrets::SecretBox;
use crate::accounts::DailyUsage;
use crate::store::{now_ms, Store};

const AAD_PREFIX: &str = "ember/chatgpt-tokens/v1:";

/// The scope that allows ChatGPT plan usage for Responses API requests.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";

/// A ChatGPT account as clients see it: no token material.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ChatGptAccount {
    pub id: String,
    /// Always `chatgpt`, so the account reads as one more kind next to agent accounts.
    pub kind: &'static str,
    pub label: String,
    pub email: Option<String>,
    /// The OAuth client OpenAI registered for this user (public, not a secret).
    pub client_id: String,
    pub scopes: Vec<String>,
    /// `chatgpt.tokens.use.direct` was granted.
    pub plan_usage: bool,
    /// `signed_in` | `signed_out` | `not_eligible`.
    pub status: String,
    pub access_expires_at: Option<i64>,
    pub limited_until: Option<i64>,
    pub limit_reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ChatGptAccount {
    pub fn limited_at(&self, now: i64) -> bool {
        self.limited_until.is_some_and(|t| t > now)
    }
}

/// Decrypted tokens. Never serialised to clients, redacted in `Debug`, zeroed on drop.
pub struct Tokens {
    pub access_token: Zeroizing<String>,
    pub refresh_token: Option<Zeroizing<String>>,
    pub id_token: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tokens(<redacted>)")
    }
}

/// The sealed form; private so it cannot be serialised anywhere else.
#[derive(Serialize)]
struct Blob<'a> {
    access_token: &'a str,
    refresh_token: Option<&'a str>,
    id_token: Option<&'a str>,
}

/// The opened form. Its strings are moved (not copied) into [`Zeroizing`] right away.
#[derive(Deserialize)]
struct OpenedBlob {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

/// Token lifetimes, Unix ms.
#[derive(Debug, Clone, Copy, Default)]
pub struct Lifetimes {
    pub access_expires_at: Option<i64>,
    pub refresh_expires_at: Option<i64>,
    pub earliest_refresh_at: Option<i64>,
}

fn aad(id: &str) -> Vec<u8> {
    format!("{AAD_PREFIX}{id}").into_bytes()
}

fn seal(secrets: &SecretBox, id: &str, t: &Tokens) -> (Vec<u8>, Vec<u8>) {
    let json = Zeroizing::new(
        serde_json::to_vec(&Blob {
            access_token: &t.access_token,
            refresh_token: t.refresh_token.as_deref().map(String::as_str),
            id_token: t.id_token.as_deref().map(String::as_str),
        })
        .expect("serialising strings does not fail"),
    );
    secrets.seal(&json, &aad(id))
}

const COLUMNS: &str = "id, label, email, client_id, scopes, plan_usage, status, access_expires_at, \
                       limited_until, limit_reason, created_at, updated_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatGptAccount> {
    let scopes: String = r.get(4)?;
    Ok(ChatGptAccount {
        id: r.get(0)?,
        kind: "chatgpt",
        label: r.get(1)?,
        email: r.get(2)?,
        client_id: r.get(3)?,
        scopes: scopes.split_whitespace().map(str::to_string).collect(),
        plan_usage: r.get(5)?,
        status: r.get(6)?,
        access_expires_at: r.get(7)?,
        limited_until: r.get(8)?,
        limit_reason: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

pub fn get(store: &Store, id: &str) -> anyhow::Result<Option<ChatGptAccount>> {
    Ok(store
        .conn()
        .query_row(
            &format!("SELECT {COLUMNS} FROM chatgpt_accounts WHERE id = ?1"),
            params![id],
            row,
        )
        .optional()?)
}

pub fn find(store: &Store, client_id: &str, subject: &str) -> anyhow::Result<Option<ChatGptAccount>> {
    Ok(store
        .conn()
        .query_row(
            &format!("SELECT {COLUMNS} FROM chatgpt_accounts WHERE client_id = ?1 AND subject = ?2"),
            params![client_id, subject],
            row,
        )
        .optional()?)
}

pub fn subject_of(store: &Store, id: &str) -> anyhow::Result<Option<String>> {
    Ok(store
        .conn()
        .query_row(
            "SELECT subject FROM chatgpt_accounts WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn list(store: &Store) -> anyhow::Result<Vec<ChatGptAccount>> {
    let conn = store.conn();
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM chatgpt_accounts ORDER BY created_at, rowid"
    ))?;
    let rows = stmt.query_map([], row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The fields a successful sign-in writes.
pub struct SignedIn<'a> {
    pub label: &'a str,
    pub client_id: &'a str,
    pub subject: &'a str,
    pub email: Option<&'a str>,
    pub scopes: &'a str,
    pub tokens: &'a Tokens,
    pub lifetimes: Lifetimes,
}

/// Insert a new account, or update `existing` (re-sign-in of the same identity).
pub fn save_sign_in(
    store: &Store,
    secrets: &SecretBox,
    existing: Option<&str>,
    s: SignedIn<'_>,
) -> anyhow::Result<ChatGptAccount> {
    let now = now_ms();
    let plan_usage = s.scopes.split_whitespace().any(|x| x == PLAN_SCOPE);
    let id = match existing {
        Some(id) => id.to_string(),
        None => uuid::Uuid::new_v4().to_string(),
    };
    let (nonce, ct) = seal(secrets, &id, s.tokens);
    let conn = store.conn();
    if existing.is_some() {
        conn.execute(
            "UPDATE chatgpt_accounts SET email = COALESCE(?2, email), scopes = ?3, plan_usage = ?4,
                 status = 'signed_in', access_expires_at = ?5, refresh_expires_at = ?6,
                 earliest_refresh_at = ?7, token_nonce = ?8, token_ciphertext = ?9, updated_at = ?10
             WHERE id = ?1",
            params![
                id,
                s.email,
                s.scopes,
                plan_usage,
                s.lifetimes.access_expires_at,
                s.lifetimes.refresh_expires_at,
                s.lifetimes.earliest_refresh_at,
                nonce,
                ct,
                now
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO chatgpt_accounts (id, label, client_id, subject, email, scopes, plan_usage,
                 status, access_expires_at, refresh_expires_at, earliest_refresh_at, token_nonce,
                 token_ciphertext, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'signed_in', ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
            params![
                id,
                s.label,
                s.client_id,
                s.subject,
                s.email,
                s.scopes,
                plan_usage,
                s.lifetimes.access_expires_at,
                s.lifetimes.refresh_expires_at,
                s.lifetimes.earliest_refresh_at,
                nonce,
                ct,
                now
            ],
        )?;
    }
    drop(conn);
    Ok(get(store, &id)?.expect("just written"))
}

/// Replace the tokens after a refresh.
pub fn save_refresh(
    store: &Store,
    secrets: &SecretBox,
    id: &str,
    tokens: &Tokens,
    lifetimes: Lifetimes,
    scopes: Option<&str>,
) -> anyhow::Result<()> {
    let (nonce, ct) = seal(secrets, id, tokens);
    store.conn().execute(
        "UPDATE chatgpt_accounts SET token_nonce = ?2, token_ciphertext = ?3, access_expires_at = ?4,
             refresh_expires_at = COALESCE(?5, refresh_expires_at), earliest_refresh_at = ?6,
             scopes = COALESCE(?7, scopes),
             plan_usage = CASE WHEN ?7 IS NULL THEN plan_usage ELSE ?8 END,
             updated_at = ?9
         WHERE id = ?1",
        params![
            id,
            nonce,
            ct,
            lifetimes.access_expires_at,
            lifetimes.refresh_expires_at,
            lifetimes.earliest_refresh_at,
            scopes,
            scopes.is_some_and(|s| s.split_whitespace().any(|x| x == PLAN_SCOPE)),
            now_ms()
        ],
    )?;
    Ok(())
}

/// The stored tokens and lifetimes, decrypted. `None` if the account has no tokens (signed out).
pub fn tokens(
    store: &Store,
    secrets: &SecretBox,
    id: &str,
) -> anyhow::Result<Option<(Tokens, Lifetimes)>> {
    type Row = (Option<Vec<u8>>, Option<Vec<u8>>, Option<i64>, Option<i64>, Option<i64>);
    let r: Option<Row> = store
        .conn()
        .query_row(
            "SELECT token_nonce, token_ciphertext, access_expires_at, refresh_expires_at,
                    earliest_refresh_at
             FROM chatgpt_accounts WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((Some(nonce), Some(ct), access, refresh, earliest)) = r else {
        return Ok(None);
    };
    let pt = secrets
        .open(&nonce, &ct, &aad(id))
        .map_err(|_| anyhow::anyhow!("ChatGPT tokens of {id} failed to decrypt"))?;
    let blob: OpenedBlob = serde_json::from_slice(&pt)
        .map_err(|_| anyhow::anyhow!("ChatGPT tokens of {id} are corrupt"))?;
    let tokens = Tokens {
        access_token: Zeroizing::new(blob.access_token),
        refresh_token: blob.refresh_token.map(Zeroizing::new),
        id_token: blob.id_token.map(Zeroizing::new),
    };
    Ok(Some((
        tokens,
        Lifetimes {
            access_expires_at: access,
            refresh_expires_at: refresh,
            earliest_refresh_at: earliest,
        },
    )))
}

/// Forget the tokens and set `status` (`signed_out` after a dead refresh token or revocation).
pub fn clear_tokens(store: &Store, id: &str, status: &str) -> anyhow::Result<()> {
    store.conn().execute(
        "UPDATE chatgpt_accounts SET token_nonce = NULL, token_ciphertext = NULL,
             access_expires_at = NULL, status = ?2, updated_at = ?3
         WHERE id = ?1",
        params![id, status, now_ms()],
    )?;
    Ok(())
}

pub fn set_status(store: &Store, id: &str, status: &str) -> anyhow::Result<()> {
    store.conn().execute(
        "UPDATE chatgpt_accounts SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, status, now_ms()],
    )?;
    Ok(())
}

pub fn set_limit(store: &Store, id: &str, until: i64, reason: &str) -> anyhow::Result<()> {
    store.conn().execute(
        "UPDATE chatgpt_accounts SET limited_until = ?2, limit_reason = ?3 WHERE id = ?1",
        params![id, until, reason],
    )?;
    Ok(())
}

pub fn clear_limit(store: &Store, id: &str) -> anyhow::Result<()> {
    store.conn().execute(
        "UPDATE chatgpt_accounts SET limited_until = NULL, limit_reason = NULL WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

pub fn delete(store: &Store, id: &str) -> anyhow::Result<bool> {
    let conn = store.conn();
    conn.execute("DELETE FROM chatgpt_usage WHERE account_id = ?1", params![id])?;
    Ok(conn.execute("DELETE FROM chatgpt_accounts WHERE id = ?1", params![id])? > 0)
}

/// This server's `ext_agent_host_id`, created (as a `urn:uuid:`) on first use and then stable.
pub fn host_id(store: &Store) -> anyhow::Result<String> {
    let conn = store.conn();
    conn.execute(
        "INSERT OR IGNORE INTO chatgpt_host (id, host_id) VALUES (1, ?1)",
        params![format!("urn:uuid:{}", uuid::Uuid::new_v4())],
    )?;
    Ok(conn.query_row("SELECT host_id FROM chatgpt_host WHERE id = 1", [], |r| {
        r.get(0)
    })?)
}

/// Add one `response.completed` usage report to today's (UTC) row.
pub fn record_usage(store: &Store, id: &str, input: u64, output: u64) -> anyhow::Result<()> {
    store.conn().execute(
        "INSERT INTO chatgpt_usage (account_id, day, input_tokens, output_tokens, reports)
         VALUES (?1, date(?2 / 1000, 'unixepoch'), ?3, ?4, 1)
         ON CONFLICT (account_id, day) DO UPDATE SET
             input_tokens = input_tokens + excluded.input_tokens,
             output_tokens = output_tokens + excluded.output_tokens,
             reports = reports + 1",
        params![id, now_ms(), input as i64, output as i64],
    )?;
    Ok(())
}

/// Per-day usage of ChatGPT accounts since `since_ms`, in the agent accounts' shape.
pub fn usage_since(store: &Store, since_ms: i64) -> anyhow::Result<Vec<DailyUsage>> {
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT account_id, day, input_tokens, output_tokens, reports FROM chatgpt_usage
         WHERE day >= date(?1 / 1000, 'unixepoch') ORDER BY day, account_id",
    )?;
    let rows = stmt.query_map(params![since_ms], |r| {
        Ok(DailyUsage {
            account_id: Some(r.get(0)?),
            day: r.get(1)?,
            input_tokens: r.get::<_, i64>(2)? as u64,
            output_tokens: r.get::<_, i64>(3)? as u64,
            reports: r.get::<_, i64>(4)? as u64,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(a: &str, r: &str, i: &str) -> Tokens {
        Tokens {
            access_token: Zeroizing::new(a.into()),
            refresh_token: Some(Zeroizing::new(r.into())),
            id_token: Some(Zeroizing::new(i.into())),
        }
    }

    fn sign_in(store: &Store, sb: &SecretBox, sub: &str, t: &Tokens) -> ChatGptAccount {
        save_sign_in(
            store,
            sb,
            None,
            SignedIn {
                label: "me",
                client_id: "client-1",
                subject: sub,
                email: Some("me@example.com"),
                scopes: "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
                tokens: t,
                lifetimes: Lifetimes::default(),
            },
        )
        .unwrap()
    }

    #[test]
    fn tokens_are_encrypted_bound_to_the_row_and_never_listed() {
        let store = Store::open_in_memory().unwrap();
        let sb = SecretBox::ephemeral();
        let a = sign_in(&store, &sb, "sub-a", &tokens("AT-SECRET-A", "RT-SECRET-A", "IDT-A"));
        let b = sign_in(&store, &sb, "sub-b", &tokens("AT-SECRET-B", "RT-SECRET-B", "IDT-B"));
        assert!(a.plan_usage);

        let (t, _) = super::tokens(&store, &sb, &a.id).unwrap().unwrap();
        assert_eq!(t.access_token.as_str(), "AT-SECRET-A");
        assert_eq!(t.refresh_token.as_deref().map(String::as_str), Some("RT-SECRET-A"));
        assert_eq!(format!("{t:?}"), "Tokens(<redacted>)");

        // Nothing in the database holds a token in plaintext.
        let dump: Vec<Vec<u8>> = {
            let conn = store.conn();
            let mut stmt = conn
                .prepare(
                    "SELECT token_ciphertext, CAST(label || scopes || client_id || subject AS BLOB)
                     FROM chatgpt_accounts",
                )
                .unwrap();
            let rows = stmt
                .query_map([], |r| Ok([r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?]))
                .unwrap();
            rows.flat_map(|r| r.unwrap()).collect()
        };
        for d in &dump {
            for needle in [&b"SECRET"[..], b"IDT-"] {
                assert!(!d.windows(needle.len()).any(|w| w == needle));
            }
        }

        // Listing (what clients get) carries no token material.
        let json = serde_json::to_string(&list(&store).unwrap()).unwrap();
        assert!(!json.contains("SECRET") && !json.contains("IDT-"), "{json}");
        assert!(!json.contains("token\""), "{json}");

        // A blob swapped onto another row does not decrypt.
        store
            .conn()
            .execute_batch(&format!(
                "UPDATE chatgpt_accounts SET
                   token_nonce = (SELECT token_nonce FROM chatgpt_accounts WHERE id = '{b}'),
                   token_ciphertext = (SELECT token_ciphertext FROM chatgpt_accounts WHERE id = '{b}')
                 WHERE id = '{a}'",
                a = a.id,
                b = b.id
            ))
            .unwrap();
        assert!(super::tokens(&store, &sb, &a.id).is_err());
        assert!(super::tokens(&store, &SecretBox::ephemeral(), &b.id).is_err());

        // Clearing leaves no tokens.
        clear_tokens(&store, &b.id, "signed_out").unwrap();
        assert!(super::tokens(&store, &sb, &b.id).unwrap().is_none());
        assert_eq!(get(&store, &b.id).unwrap().unwrap().status, "signed_out");
    }

    #[test]
    fn host_id_is_stable_urn_uuid() {
        let store = Store::open_in_memory().unwrap();
        let h = host_id(&store).unwrap();
        assert!(h.starts_with("urn:uuid:"), "{h}");
        assert_eq!(host_id(&store).unwrap(), h);
    }

    #[test]
    fn usage_accumulates_per_day() {
        let store = Store::open_in_memory().unwrap();
        record_usage(&store, "x", 10, 1).unwrap();
        record_usage(&store, "x", 5, 2).unwrap();
        let rows = usage_since(&store, now_ms() - 86_400_000).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].input_tokens, rows[0].output_tokens, rows[0].reports), (15, 3, 2));
        assert_eq!(rows[0].account_id.as_deref(), Some("x"));
    }
}
