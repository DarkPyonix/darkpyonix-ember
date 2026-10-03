//! The OAuth 2.0 / OIDC pieces of Sign in with ChatGPT: PKCE, the authorization URL, callback
//! parameters, the token response and the ID token's claims. Pure functions, so they are tested
//! without a network.

use std::collections::HashMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::store::Lifetimes;
use crate::accounts::secrets::random_32;

/// The `client_id` for a first sign-in: OpenAI then registers a client for this user and returns
/// its id on the callback (developers.openai.com/siwc/token-sharing-open-source/sign-in).
pub const DYNAMIC_CLIENT: &str = "dynamic_agent_client";

/// Scopes for identity, refresh and ChatGPT plan usage (same source).
pub const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

/// The callback path. The docs require scheme, host (`127.0.0.1`, never `localhost`) and path to
/// stay the same across attempts; only the port may vary.
pub const CALLBACK_PATH: &str = "/auth/callback";

/// A PKCE verifier (43 base64url characters from 32 random bytes) and its S256 challenge.
pub struct Pkce {
    pub verifier: Zeroizing<String>,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Pkce {
        let verifier = Zeroizing::new(URL_SAFE_NO_PAD.encode(&*random_32()));
        let challenge = s256(&verifier);
        Pkce {
            verifier,
            challenge,
        }
    }
}

/// `BASE64URL(SHA256(verifier))` without padding (RFC 7636 §4.2).
pub fn s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A fresh random value for `state` or `nonce` (256 bits, base64url).
pub fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(&*random_32())
}

/// Everything that goes into the authorization URL.
pub struct AuthorizeParams<'a> {
    pub client_id: &'a str,
    /// Only on a first registration (`client_id` = [`DYNAMIC_CLIENT`]).
    pub agent_name_hint: Option<&'a str>,
    pub ext_agent_host_id: &'a str,
    pub redirect_uri: &'a str,
    pub resource: &'a str,
    pub state: &'a str,
    pub nonce: &'a str,
    pub code_challenge: &'a str,
    /// Re-sign-in only: the saved email.
    pub login_hint: Option<&'a str>,
}

pub fn authorize_url(endpoint: &str, p: &AuthorizeParams<'_>) -> anyhow::Result<String> {
    let mut q: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", p.client_id),
        ("redirect_uri", p.redirect_uri),
        ("scope", SCOPES),
        ("resource", p.resource),
        ("state", p.state),
        ("nonce", p.nonce),
        ("code_challenge", p.code_challenge),
        ("code_challenge_method", "S256"),
        ("ext_agent_host_id", p.ext_agent_host_id),
    ];
    if let Some(n) = p.agent_name_hint {
        q.push(("agent_name_hint", n));
    }
    if let Some(h) = p.login_hint {
        q.push(("login_hint", h));
    }
    Ok(reqwest::Url::parse_with_params(endpoint, &q)?.to_string())
}

/// The query of a callback URL pasted by the user (when the browser ran on another machine and
/// `127.0.0.1` did not reach this server).
pub fn callback_params(url: &str) -> anyhow::Result<HashMap<String, String>> {
    let url = reqwest::Url::parse(url.trim())?;
    anyhow::ensure!(
        url.path() == CALLBACK_PATH,
        "not a sign-in callback URL (path {} instead of {CALLBACK_PATH})",
        url.path()
    );
    Ok(url.query_pairs().into_owned().collect())
}

/// The token endpoint's JSON. Redacted `Debug`; callers move the tokens into [`Zeroizing`]
/// (see [`TokenResponse::into_tokens`]).
#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    #[allow(dead_code)]
    pub token_type: Option<String>,
    pub expires_in: Option<i64>,
    pub scope: Option<String>,
    /// Documented as a field but not its format; read as Unix seconds (or ms if that large).
    pub earliest_refresh_at: Option<serde_json::Value>,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// Refresh tokens live 30 days (token reference); each refresh renews that.
pub const REFRESH_LIFETIME_MS: i64 = 30 * 24 * 60 * 60 * 1000;

impl TokenResponse {
    /// The tokens, moved into zeroed-on-drop storage. `fallback_refresh` keeps the previous
    /// refresh token when a refresh response carries none.
    pub fn into_tokens(
        self,
        fallback_refresh: Option<Zeroizing<String>>,
        fallback_id: Option<Zeroizing<String>>,
    ) -> super::store::Tokens {
        super::store::Tokens {
            access_token: Zeroizing::new(self.access_token),
            refresh_token: self.refresh_token.map(Zeroizing::new).or(fallback_refresh),
            id_token: self.id_token.map(Zeroizing::new).or(fallback_id),
        }
    }

    pub fn lifetimes(&self, now_ms: i64) -> Lifetimes {
        Lifetimes {
            // Default to the documented one hour when `expires_in` is missing.
            access_expires_at: Some(now_ms + self.expires_in.unwrap_or(3600) * 1000),
            refresh_expires_at: self.refresh_token.as_ref().map(|_| now_ms + REFRESH_LIFETIME_MS),
            earliest_refresh_at: self.earliest_refresh_at.as_ref().and_then(|v| {
                let n = v.as_i64()?;
                Some(if n > 100_000_000_000 { n } else { n * 1000 })
            }),
        }
    }
}

/// The OAuth error body (`{"error": "...", "error_description": "..."}`), when there is one.
pub fn oauth_error(body: &[u8]) -> (String, Option<String>) {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    // Some servers nest it as {"error": {"code"|"type": ..., "message": ...}}.
    let err = &v["error"];
    let code = err
        .as_str()
        .or_else(|| err["code"].as_str())
        .or_else(|| err["type"].as_str())
        .unwrap_or("unknown_error")
        .to_string();
    let desc = v["error_description"]
        .as_str()
        .or_else(|| err["message"].as_str())
        .map(str::to_string);
    (code, desc)
}

/// Refresh errors after which the tokens are cleared and the user signs in again
/// (developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery).
pub fn refresh_error_is_terminal(code: &str) -> bool {
    matches!(
        code,
        "invalid_grant" | "invalid_refresh_token" | "token_expired" | "refresh_token_reused"
    )
}

/// The ID token claims Ember uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdClaims {
    pub sub: String,
    pub email: Option<String>,
}

/// Check the ID token's claims: issuer, audience (the issued client id), expiry and nonce.
///
/// The signature is not verified here. The token comes straight from the token endpoint over
/// TLS in the code flow, which OIDC Core §3.1.3.7 (item 6) allows in place of a signature check.
/// OpenAI's page asks for JWKS verification as well; see the module docs.
pub fn check_id_token(
    id_token: &str,
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now_ms: i64,
) -> Result<IdClaims, String> {
    let mut parts = id_token.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("ID token is not a JWT".into());
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| "ID token payload is not base64url".to_string())?;
    let c: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "ID token payload is not JSON".to_string())?;
    if c["iss"].as_str() != Some(issuer) {
        return Err(format!("ID token issuer is {}, expected {issuer}", c["iss"]));
    }
    let aud_ok = match &c["aud"] {
        serde_json::Value::String(a) => a == client_id,
        serde_json::Value::Array(a) => a.iter().any(|x| x.as_str() == Some(client_id)),
        _ => false,
    };
    if !aud_ok {
        return Err("ID token audience is not this client".into());
    }
    let exp = c["exp"].as_i64().ok_or("ID token has no exp")?;
    // One minute of clock skew.
    if exp * 1000 + 60_000 < now_ms {
        return Err("ID token has expired".into());
    }
    if c["nonce"].as_str() != Some(nonce) {
        return Err("ID token nonce does not match this sign-in".into());
    }
    let sub = c["sub"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("ID token has no sub")?
        .to_string();
    Ok(IdClaims {
        sub,
        email: c["email"].as_str().map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: serde_json::Value) -> String {
        format!(
            "{}.{}.sig",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        )
    }

    #[test]
    fn pkce_is_s256_of_a_fresh_verifier() {
        let a = Pkce::new();
        let b = Pkce::new();
        assert_ne!(a.verifier.as_str(), b.verifier.as_str());
        // RFC 7636: 43..=128 unreserved characters.
        assert_eq!(a.verifier.len(), 43);
        assert!(a
            .verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(a.challenge, s256(&a.verifier));
        // RFC 7636 Appendix B test vector.
        assert_eq!(
            s256("dBjftJeZ4CVP-mJ0kYmbizBsfVwYWvXg23ON6Z08SZw"),
            "E9Melhoa2OwvFrEMTJguCQaoeJ1M8TsZQqfY6UdVFLI"
        );
        assert_ne!(random_token(), random_token());
    }

    #[test]
    fn authorize_url_carries_the_documented_parameters() {
        let url = authorize_url(
            "https://auth.openai.com/api/accounts/authorize",
            &AuthorizeParams {
                client_id: DYNAMIC_CLIENT,
                agent_name_hint: Some("DarkPyonix Ember"),
                ext_agent_host_id: "urn:uuid:1",
                redirect_uri: "http://127.0.0.1:8740/auth/callback",
                resource: "https://api.openai.com/v1",
                state: "st",
                nonce: "no",
                code_challenge: "ch",
                login_hint: None,
            },
        )
        .unwrap();
        let u = reqwest::Url::parse(&url).unwrap();
        let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "dynamic_agent_client");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], "ch");
        assert_eq!(q["scope"], SCOPES);
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:8740/auth/callback");
        assert_eq!(q["agent_name_hint"], "DarkPyonix Ember");
        assert_eq!(q["ext_agent_host_id"], "urn:uuid:1");
        assert!(!q.contains_key("login_hint"));
    }

    #[test]
    fn pasted_callback_url() {
        let q = callback_params("http://127.0.0.1:8740/auth/callback?code=c&state=s&client_id=x")
            .unwrap();
        assert_eq!((q["code"].as_str(), q["state"].as_str()), ("c", "s"));
        assert!(callback_params("http://127.0.0.1:8740/elsewhere?code=c").is_err());
        assert!(callback_params("not a url").is_err());
    }

    #[test]
    fn id_token_claims_are_checked() {
        let now = 1_800_000_000_000;
        let good = json_claims("iss", "client", "n", now / 1000 + 3600);
        let c = check_id_token(&jwt(good.clone()), "iss", "client", "n", now).unwrap();
        assert_eq!(c.sub, "user-1");
        assert_eq!(c.email.as_deref(), Some("u@example.com"));

        assert!(check_id_token(&jwt(good.clone()), "other", "client", "n", now).is_err());
        assert!(check_id_token(&jwt(good.clone()), "iss", "other", "n", now).is_err());
        assert!(check_id_token(&jwt(good), "iss", "client", "replayed", now).is_err());
        let expired = json_claims("iss", "client", "n", now / 1000 - 3600);
        assert!(check_id_token(&jwt(expired), "iss", "client", "n", now).is_err());
        let mut arr = json_claims("iss", "client", "n", now / 1000 + 60);
        arr["aud"] = serde_json::json!(["a", "client"]);
        assert!(check_id_token(&jwt(arr), "iss", "client", "n", now).is_ok());
        assert!(check_id_token("garbage", "iss", "client", "n", now).is_err());
    }

    fn json_claims(iss: &str, aud: &str, nonce: &str, exp: i64) -> serde_json::Value {
        serde_json::json!({
            "iss": iss, "aud": aud, "nonce": nonce, "exp": exp,
            "sub": "user-1", "email": "u@example.com"
        })
    }

    #[test]
    fn token_response_debug_is_redacted_and_errors_parse() {
        let t: TokenResponse = serde_json::from_str(
            r#"{"access_token":"AT-SECRET","refresh_token":"RT-SECRET","id_token":"x.y.z",
                "token_type":"Bearer","expires_in":3600,"scope":"openid"}"#,
        )
        .unwrap();
        let dbg = format!("{t:?}");
        assert!(!dbg.contains("SECRET"), "{dbg}");
        let l = t.lifetimes(1_000);
        assert_eq!(l.access_expires_at, Some(1_000 + 3_600_000));

        assert_eq!(
            oauth_error(br#"{"error":"invalid_grant","error_description":"nope"}"#),
            ("invalid_grant".into(), Some("nope".into()))
        );
        assert_eq!(oauth_error(b"<html>").0, "unknown_error");
        assert!(refresh_error_is_terminal("refresh_token_reused"));
        assert!(!refresh_error_is_terminal("temporarily_unavailable"));
    }
}
