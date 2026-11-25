//! The live identity fixture's driver (spec 020 D-14): a real rauthy, reached
//! over its own HTTP API, and a real authorization code with PKCE login.
//!
//! `scripts/live-rauthy.sh up` starts the container and prints the variables
//! below; CI's `live` workflow does the same. A test that needs rauthy calls
//! [`LiveEnv::from_env`]: with `AICORTEX_TEST_RAUTHY_URL` unset it skips, and
//! with `AICORTEX_REQUIRE_RAUTHY=1` (the workflow sets it) a skip is a failure,
//! so a green live run means the test ran rather than that it was absent.
//!
//! The product mints no credential and accepts no static key. The registration
//! token, API key, and administrator here belong to the throwaway rauthy; the
//! access token a test uses is the one rauthy signs for a person after a PKCE
//! login. This module is written to be included by another crate's tests with
//! `#[path]` (spec 021 AC-2 needs the same login).

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};

/// The origin of the running rauthy, which opts a run in.
pub const ENV_URL: &str = "AICORTEX_TEST_RAUTHY_URL";
/// The dynamic client registration token.
pub const ENV_REG_TOKEN: &str = "AICORTEX_TEST_RAUTHY_REG_TOKEN";
/// An administrator API key, `name$secret`, for provisioning calls.
pub const ENV_API_KEY: &str = "AICORTEX_TEST_RAUTHY_API_KEY";
/// The person who completes the login.
pub const ENV_USER: &str = "AICORTEX_TEST_RAUTHY_USER";
/// That person's password.
pub const ENV_PASSWORD: &str = "AICORTEX_TEST_RAUTHY_PASSWORD";
/// Set to `1` by the live workflow: a skip becomes a failure.
pub const ENV_REQUIRE: &str = "AICORTEX_REQUIRE_RAUTHY";

/// The loopback redirect the client registers. Nothing listens on it:
/// rauthy answers the authorization request with the code in a `Location`.
pub const REDIRECT: &str = "http://127.0.0.1:9876/callback";

/// What the run is told about the rauthy it uses.
#[derive(Clone, Debug)]
pub struct LiveEnv {
    /// The origin, for example `http://localhost:8080`, no trailing slash.
    pub origin: String,
    /// The registration token.
    pub reg_token: String,
    /// The API key, `name$secret`.
    pub api_key: String,
    /// The person's email.
    pub user: String,
    /// The person's password.
    pub password: String,
}

impl LiveEnv {
    /// The run's environment, or `None` after recording a skip.
    ///
    /// # Panics
    ///
    /// When the run opted in (`ENV_URL` set) and another variable is missing,
    /// or when `ENV_REQUIRE` is `1` and `ENV_URL` is absent.
    pub fn from_env(test: &str) -> Option<Self> {
        let Ok(origin) = std::env::var(ENV_URL) else {
            assert!(
                std::env::var(ENV_REQUIRE).as_deref() != Ok("1"),
                "{ENV_REQUIRE}=1 and skipped: {test} needs {ENV_URL} \
                 (scripts/live-rauthy.sh up prints the variables)"
            );
            eprintln!("skipped: {test}: set {ENV_URL} (scripts/live-rauthy.sh up)");
            return None;
        };
        let required = |name: &str| {
            std::env::var(name)
                .unwrap_or_else(|_| panic!("{ENV_URL} is set, so {name} must be too"))
        };
        Some(Self {
            origin: origin.trim_end_matches('/').to_owned(),
            reg_token: required(ENV_REG_TOKEN),
            api_key: required(ENV_API_KEY),
            user: required(ENV_USER),
            password: required(ENV_PASSWORD),
        })
    }

    /// The socket address behind the origin, which `RAHI_RAUTHY_ADDR` wants.
    pub fn loopback_addr(&self) -> String {
        use std::net::ToSocketAddrs as _;
        let authority = self
            .origin
            .split_once("://")
            .map_or(self.origin.as_str(), |(_, rest)| rest);
        authority
            .to_socket_addrs()
            .unwrap_or_else(|err| panic!("{ENV_URL} names {authority}: {err}"))
            .min_by_key(|address| address.is_ipv6())
            .unwrap_or_else(|| panic!("{ENV_URL} names {authority}, which resolves to nothing"))
            .to_string()
    }
}

/// A live rauthy, driven over its own HTTP API.
pub struct Rauthy {
    env: LiveEnv,
    http: reqwest::Client,
}

impl Rauthy {
    /// A driver for the rauthy `env` names.
    pub fn new(env: &LiveEnv) -> Self {
        Self {
            env: env.clone(),
            // A test's loopback client to the fixture: not egress, which 010
            // B-8's scan polices in the shipped sources.
            http: reqwest::Client::default(),
        }
    }

    /// A request with the `User-Agent` rauthy requires: it refuses a login
    /// with an empty one.
    fn req(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .header("user-agent", "aicortex-live")
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.env.origin)
    }

    fn json_body(builder: reqwest::RequestBuilder, body: &Value) -> reqwest::RequestBuilder {
        builder
            .header("content-type", "application/json")
            .body(body.to_string())
    }

    /// Create a scope, tolerating one that is already there.
    pub async fn create_scope(&self, scope: &str) {
        let request = self
            .req(reqwest::Method::POST, self.url("/auth/v1/scopes"))
            .header("authorization", format!("API-Key {}", self.env.api_key));
        let answer = Self::json_body(request, &json!({ "scope": scope }))
            .send()
            .await
            .expect("rauthy answers the scope request");
        let status = answer.status();
        assert!(
            status.is_success() || status == reqwest::StatusCode::BAD_REQUEST,
            "creating the scope {scope} answered {status}: {}",
            answer.text().await.unwrap_or_default()
        );
    }

    /// Register a public client through RFC 7591 dynamic registration.
    pub async fn register(&self, name: &str) -> String {
        let request = self
            .req(reqwest::Method::POST, self.url("/auth/v1/clients_dyn"))
            .header("authorization", format!("Bearer {}", self.env.reg_token));
        let answer = Self::json_body(
            request,
            &json!({
                "client_name": name,
                "redirect_uris": [REDIRECT],
                "grant_types": ["authorization_code", "refresh_token"],
                "token_endpoint_auth_method": "none",
            }),
        )
        .send()
        .await
        .expect("rauthy answers the registration");
        let status = answer.status();
        let body = answer.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "dynamic registration answered {status}: {body}"
        );
        let body: Value = serde_json::from_str(&body).expect("the registration is json");
        let id = body["client_id"].as_str().expect("a client id");
        assert!(
            id.starts_with("dyn$"),
            "the client was dynamically registered"
        );
        id.to_owned()
    }

    /// Bind the client to `audiences` and grant it `scopes`. rauthy signs
    /// with `EdDSA` unless told otherwise and the chassis verifies RS256, and
    /// the pinned rauthy cannot be asked for an audience by a dynamic client,
    /// so the administrator sets `default_aud` (rahi 037 D-7).
    pub async fn configure_client(&self, client_id: &str, audiences: &[&str], scopes: &[&str]) {
        let mut all = vec!["openid", "profile", "email", "groups"];
        all.extend_from_slice(scopes);
        let mut defaults = vec!["openid"];
        defaults.extend_from_slice(scopes);
        let path = format!("/auth/v1/clients/{}", client_id.replace('$', "%24"));
        let request = self
            .req(reqwest::Method::PUT, self.url(&path))
            .header("authorization", format!("API-Key {}", self.env.api_key));
        let answer = Self::json_body(
            request,
            &json!({
                "id": client_id,
                "name": "aicortex-live",
                "enabled": true,
                "confidential": false,
                "redirect_uris": [REDIRECT],
                "flows_enabled": ["authorization_code", "refresh_token"],
                "access_token_alg": "RS256",
                "id_token_alg": "RS256",
                "auth_code_lifetime": 60,
                "access_token_lifetime": 300,
                "scopes": all,
                "default_scopes": defaults,
                "challenges": ["S256"],
                "force_mfa": false,
                "claims_at_root": false,
                "default_aud": audiences,
            }),
        )
        .send()
        .await
        .expect("rauthy answers the client update");
        let status = answer.status();
        assert!(
            status.is_success(),
            "administering the client answered {status}: {}",
            answer.text().await.unwrap_or_default()
        );
        let saved: Value = serde_json::from_str(
            &self
                .req(reqwest::Method::GET, self.url(&path))
                .header("authorization", format!("API-Key {}", self.env.api_key))
                .send()
                .await
                .expect("rauthy answers the client read")
                .text()
                .await
                .expect("the client reads"),
        )
        .expect("the client is json");
        let expected = if audiences.is_empty() {
            Value::Null
        } else {
            json!(audiences)
        };
        assert_eq!(saved["default_aud"], expected, "the audience persisted");
        assert_eq!(saved["access_token_alg"], json!("RS256"));
        assert_eq!(saved["challenges"], json!(["S256"]));
        assert_eq!(saved["confidential"], json!(false));
    }

    /// A full authorization code with PKCE login as the environment's person,
    /// returning the access token rauthy signed.
    pub async fn login(&self, client_id: &str, scopes: &[&str]) -> String {
        let verifier = pkce_verifier();
        let code = self.authorize(client_id, scopes, &verifier).await;
        self.exchange(client_id, &code, &verifier).await
    }

    async fn session(&self) -> (String, String) {
        let answer = self
            .req(reqwest::Method::POST, self.url("/auth/v1/oidc/session"))
            .send()
            .await
            .expect("rauthy answers the session request");
        let cookie = answer
            .headers()
            .get("set-cookie")
            .expect("a new session sets a cookie")
            .to_str()
            .expect("the cookie is ascii")
            .split(';')
            .next()
            .expect("a cookie has a first pair")
            .to_owned();
        let body: Value = serde_json::from_str(&answer.text().await.expect("the session reads"))
            .expect("the session is json");
        let csrf = body["csrf_token"]
            .as_str()
            .expect("a csrf token")
            .to_owned();
        (cookie, csrf)
    }

    /// Fetch rauthy's proof of work challenge and solve it: the smallest
    /// counter whose SHA-256 opens with `difficulty` zero bits.
    async fn proof_of_work(&self) -> String {
        let challenge = self
            .req(reqwest::Method::POST, self.url("/auth/v1/pow"))
            .send()
            .await
            .expect("rauthy answers the proof of work request")
            .text()
            .await
            .expect("the challenge is text");
        let challenge = challenge.trim().to_owned();
        let difficulty: u32 = challenge
            .get(2..4)
            .and_then(|field| field.parse().ok())
            .expect("the challenge states its difficulty");
        for counter in 0_u64.. {
            let attempt = format!("{challenge}{counter}");
            if leading_zero_bits(digest(&SHA256, attempt.as_bytes()).as_ref()) >= difficulty {
                return attempt;
            }
        }
        unreachable!("the counter is unbounded")
    }

    async fn authorize(&self, client_id: &str, scopes: &[&str], verifier: &str) -> String {
        let (cookie, csrf) = self.session().await;
        let mut requested = vec!["openid"];
        requested.extend_from_slice(scopes);
        let request = self
            .req(reqwest::Method::POST, self.url("/auth/v1/oidc/authorize"))
            .header("cookie", cookie)
            .header("x-csrf-token", csrf);
        let answer = Self::json_body(
            request,
            &json!({
                "email": self.env.user,
                "password": self.env.password,
                "pow": self.proof_of_work().await,
                "client_id": client_id,
                "redirect_uri": REDIRECT,
                "scopes": requested,
                "code_challenge": URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes())),
                "code_challenge_method": "S256",
            }),
        )
        .send()
        .await
        .expect("rauthy answers the authorization request");
        let status = answer.status();
        let location = answer
            .headers()
            .get("location")
            .map(|value| value.to_str().expect("ascii").to_owned());
        assert!(
            status.is_success(),
            "the authorization request answered {status}: {}",
            answer.text().await.unwrap_or_default()
        );
        let location = location.expect("a completed login redirects to the loopback");
        assert_eq!(location.split('?').next(), Some(REDIRECT));
        location
            .split_once('?')
            .and_then(|(_, query)| {
                query
                    .split('&')
                    .find_map(|pair| pair.strip_prefix("code=").map(str::to_owned))
            })
            .expect("the redirect carries the authorization code")
    }

    async fn exchange(&self, client_id: &str, code: &str, verifier: &str) -> String {
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", client_id),
            ("redirect_uri", REDIRECT),
            ("code_verifier", verifier),
        ]
        .iter()
        .map(|(key, value)| format!("{key}={}", form_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
        let answer = self
            .req(reqwest::Method::POST, self.url("/auth/v1/oidc/token"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form)
            .send()
            .await
            .expect("rauthy answers the token request");
        let status = answer.status();
        let body = answer.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "the token request answered {status}: {body}"
        );
        let body: Value = serde_json::from_str(&body).expect("the token response is json");
        body["access_token"]
            .as_str()
            .expect("an access token")
            .to_owned()
    }
}

/// Register a client for `audience`, grant it `scopes`, and log the
/// environment's person in: the one call a test usually wants.
pub async fn person_token(env: &LiveEnv, audience: &str, scopes: &[&str]) -> String {
    let rauthy = Rauthy::new(env);
    for scope in scopes {
        rauthy.create_scope(scope).await;
    }
    let client = rauthy.register("aicortex-live").await;
    rauthy.configure_client(&client, &[audience], scopes).await;
    rauthy.login(&client, scopes).await
}

/// The claims of a JWT, decoded without verifying it: for assertions about
/// what rauthy issued, never for trust. The chassis verifies the signature.
pub fn claims(token: &str) -> Value {
    let parts: Vec<_> = token.split('.').collect();
    assert_eq!(parts.len(), 3, "rauthy issues a three-part JWT");
    let decode = |part| {
        serde_json::from_slice::<Value>(&URL_SAFE_NO_PAD.decode(part).expect("base64url"))
            .expect("JWT JSON")
    };
    assert_eq!(decode(parts[0])["alg"], json!("RS256"));
    decode(parts[1])
}

fn pkce_verifier() -> String {
    let mut bytes = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("the system entropy source answers");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in bytes {
        bits += byte.leading_zeros();
        if *byte != 0 {
            break;
        }
    }
    bits
}

fn form_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
