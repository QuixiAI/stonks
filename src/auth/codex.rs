//! ChatGPT (Codex) sign-in.
//!
//! With a browser available: authorization code + PKCE, redirected to a
//! listener on localhost:1455, so the user only clicks "approve". Without one
//! (SSH, no display): a short code the user enters at auth.openai.com.

use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};
use tokio::net::TcpListener;

use super::{Credential, REFRESH_SKEW_MS, Store, flow, now_ms};

/// Key of this sign-in in the credential store.
pub const PROVIDER: &str = "openai-codex";
const ORIGINATOR: &str = "pi";

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const SCOPE: &str = "openid profile email offline_access";
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";

const DEVICE_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const DEVICE_VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

const BROWSER_TIMEOUT: Duration = Duration::from_secs(300);
const DEVICE_TIMEOUT: Duration = Duration::from_secs(900);

/// Signs the user in and returns the credential to store.
pub async fn login() -> anyhow::Result<Credential> {
    if !flow::headless()
        && let Ok(listener) = TcpListener::bind(("127.0.0.1", CALLBACK_PORT)).await
    {
        return browser_login(listener).await;
    }
    device_login().await
}

async fn browser_login(listener: TcpListener) -> anyhow::Result<Credential> {
    let verifier = flow::random_token();
    let state = flow::random_token();
    let url = reqwest::Url::parse_with_params(
        AUTHORIZE_URL,
        [
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPE),
            ("code_challenge", &flow::pkce_challenge(&verifier)),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", ORIGINATOR),
        ],
    )?;
    if flow::open_browser(url.as_str()) {
        println!("Opening your browser to sign in to ChatGPT…");
        println!("If it did not open, visit:\n  {url}");
    } else {
        println!("Open this link to sign in to ChatGPT:\n  {url}");
    }
    let code = flow::wait_for_callback(listener, CALLBACK_PATH, &state, BROWSER_TIMEOUT).await?;
    exchange(&code, &verifier, REDIRECT_URI).await
}

async fn device_login() -> anyhow::Result<Credential> {
    let http = reqwest::Client::new();
    let start: Value = http
        .post(DEVICE_USERCODE_URL)
        .json(&json!({ "client_id": CLIENT_ID }))
        .send()
        .await?
        .error_for_status()
        .context("ChatGPT did not issue a sign-in code")?
        .json()
        .await?;
    let device_auth_id = start["device_auth_id"].as_str().context("no device_auth_id")?;
    let user_code = start["user_code"]
        .as_str()
        .or(start["usercode"].as_str())
        .context("no user code")?;
    let interval = interval_secs(&start["interval"]).max(3);

    println!("To sign in to ChatGPT, open:\n  {DEVICE_VERIFICATION_URL}");
    println!("and enter the code:  {user_code}");
    flow::open_browser(DEVICE_VERIFICATION_URL);

    let deadline = tokio::time::Instant::now() + DEVICE_TIMEOUT;
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if tokio::time::Instant::now() > deadline {
            bail!("the sign-in code expired; run the command again");
        }
        let response = http
            .post(DEVICE_TOKEN_URL)
            .json(&json!({ "device_auth_id": device_auth_id, "user_code": user_code }))
            .send()
            .await?;
        // 403/404 means the code has not been entered and approved yet.
        if matches!(response.status().as_u16(), 403 | 404) {
            continue;
        }
        let done: Value = response.error_for_status()?.json().await?;
        let code = done["authorization_code"].as_str().context("no authorization code")?;
        let verifier = done["code_verifier"].as_str().context("no code verifier")?;
        return exchange(code, verifier, DEVICE_REDIRECT_URI).await;
    }
}

async fn exchange(code: &str, verifier: &str, redirect_uri: &str) -> anyhow::Result<Credential> {
    token_request(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ])
    .await
    .context("ChatGPT sign-in failed at the token exchange")
}

async fn token_request(form: &[(&str, &str)]) -> anyhow::Result<Credential> {
    let response = reqwest::Client::new().post(TOKEN_URL).form(form).send().await?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or_default();
    if !status.is_success() {
        bail!("{status}: {}", body["error_description"].as_str().unwrap_or("request rejected"));
    }
    let access = body["access_token"].as_str().context("no access token")?;
    let refresh = body["refresh_token"].as_str().context("no refresh token")?;
    let expires_in = body["expires_in"].as_u64().context("no expiry")?;
    let account_id = flow::jwt_payload(access)
        .and_then(|claims| {
            claims["https://api.openai.com/auth"]["chatgpt_account_id"].as_str().map(str::to_owned)
        })
        .context("the token does not name a ChatGPT account")?;
    Ok(Credential::Codex {
        access: access.to_owned(),
        refresh: refresh.to_owned(),
        expires_ms: now_ms() + expires_in * 1000,
        account_id,
    })
}

/// Returns a currently valid `(access token, account id)`, refreshing and
/// re-saving the stored credential when it is close to expiry.
pub async fn fresh(store: &Store) -> anyhow::Result<(String, String)> {
    let _lock = store.lock()?;
    let Some(Credential::Codex { access, refresh, expires_ms, account_id }) = store.get(PROVIDER)?
    else {
        bail!("not signed in to ChatGPT; run `stonks login`");
    };
    if expires_ms > now_ms() + REFRESH_SKEW_MS {
        return Ok((access, account_id));
    }
    let renewed = token_request(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", &refresh),
        ("client_id", CLIENT_ID),
    ])
    .await
    .context("the ChatGPT sign-in has expired; run `stonks login`")?;
    store.set(PROVIDER, renewed.clone())?;
    match renewed {
        Credential::Codex { access, account_id, .. } => Ok((access, account_id)),
        _ => unreachable!("token_request returns a Codex credential"),
    }
}

/// Models this ChatGPT account can use.
pub async fn models(store: &Store) -> anyhow::Result<Vec<String>> {
    let (access, account_id) = fresh(store).await?;
    let body: Value = reqwest::Client::new()
        .get(format!("{}/models?client_version=1.0.0", crate::chatgpt::DEFAULT_BASE_URL))
        .bearer_auth(access)
        .header("ChatGPT-Account-ID", account_id)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let rows = body["models"].as_array().context("no model list")?;
    Ok(rows
        .iter()
        .filter(|row| row["visibility"].as_str().unwrap_or("list") == "list")
        .filter(|row| row["show_in_picker"] != false && row["supported_in_api"] != false)
        .filter_map(|row| row["slug"].as_str().or(row["id"].as_str()))
        .map(str::to_owned)
        .collect())
}

pub(crate) fn interval_secs(value: &Value) -> u64 {
    value.as_u64().or_else(|| value.as_str()?.parse().ok()).unwrap_or(5)
}
