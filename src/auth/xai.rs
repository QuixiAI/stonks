//! Grok (xAI) sign-in with an X / SuperGrok account.
//!
//! xAI offers the device flow only: the browser is opened on a page with the
//! code already filled in, and this process polls until it is approved. The
//! access token is then used as the API key against api.x.ai.

use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::Value;

use super::codex::interval_secs;
use super::{Credential, REFRESH_SKEW_MS, Store, flow, now_ms};

/// Key of this sign-in in the credential store.
pub const PROVIDER: &str = "xai-oauth";

const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const MODELS_URL: &str = "https://api.x.ai/v1/models";

/// Signs the user in and returns the credential to store.
pub async fn login() -> anyhow::Result<Credential> {
    let http = reqwest::Client::new();
    let discovery: Value =
        http.get(DISCOVERY_URL).send().await?.error_for_status()?.json().await?;
    let device_endpoint = trusted(&discovery["device_authorization_endpoint"])?;
    let token_endpoint = trusted(&discovery["token_endpoint"])?;

    let start: Value = http
        .post(device_endpoint)
        .form(&[("client_id", CLIENT_ID), ("scope", SCOPE)])
        .send()
        .await?
        .error_for_status()
        .context("xAI did not issue a sign-in code")?
        .json()
        .await?;
    let device_code = start["device_code"].as_str().context("no device code")?;
    let user_code = start["user_code"].as_str().context("no user code")?;
    let url = trusted(if start["verification_uri_complete"].is_string() {
        &start["verification_uri_complete"]
    } else {
        &start["verification_uri"]
    })?;
    let mut interval = interval_secs(&start["interval"]).max(1);
    let expires_in = start["expires_in"].as_u64().unwrap_or(300).max(60);

    if flow::open_browser(url) {
        println!("Opening your browser to sign in to xAI…");
        println!("Confirm the code {user_code}. If the browser did not open, visit:\n  {url}");
    } else {
        println!("To sign in to xAI, open:\n  {url}\nand confirm the code:  {user_code}");
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(expires_in);
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if tokio::time::Instant::now() > deadline {
            bail!("the sign-in code expired; run the command again");
        }
        let form =
            [("grant_type", DEVICE_GRANT), ("client_id", CLIENT_ID), ("device_code", device_code)];
        match token_request(token_endpoint, &form, None).await {
            Ok(credential) => return Ok(credential),
            Err(TokenError::Pending) => {}
            Err(TokenError::SlowDown) => interval += 5,
            Err(TokenError::Other(err)) => return Err(err.context("xAI sign-in failed")),
        }
    }
}

enum TokenError {
    Pending,
    SlowDown,
    Other(anyhow::Error),
}

async fn token_request(
    token_endpoint: &str,
    form: &[(&str, &str)],
    previous_refresh: Option<&str>,
) -> Result<Credential, TokenError> {
    let other = |err: anyhow::Error| TokenError::Other(err);
    let response = reqwest::Client::new()
        .post(token_endpoint)
        .header("Accept", "application/json")
        .form(form)
        .send()
        .await
        .map_err(|err| other(err.into()))?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or_default();
    if !status.is_success() {
        return Err(match body["error"].as_str() {
            Some("authorization_pending") => TokenError::Pending,
            Some("slow_down") => TokenError::SlowDown,
            Some("access_denied" | "authorization_denied") => {
                other(anyhow::anyhow!("the sign-in was denied"))
            }
            Some("expired_token") => other(anyhow::anyhow!("the sign-in code expired")),
            error => other(anyhow::anyhow!("{status}: {}", error.unwrap_or("request rejected"))),
        });
    }
    let access = body["access_token"].as_str().context("no access token").map_err(other)?;
    let refresh = body["refresh_token"]
        .as_str()
        .or(previous_refresh)
        .context("no refresh token")
        .map_err(other)?;
    let expires_ms = match body["expires_in"].as_u64() {
        Some(seconds) => now_ms() + seconds * 1000,
        None => flow::jwt_payload(access)
            .and_then(|claims| claims["exp"].as_u64())
            .map_or(now_ms() + 3_600_000, |exp| exp * 1000),
    };
    Ok(Credential::Xai {
        access: access.to_owned(),
        refresh: refresh.to_owned(),
        expires_ms,
        token_endpoint: token_endpoint.to_owned(),
    })
}

/// Returns a currently valid access token, refreshing and re-saving the
/// stored credential when it is close to expiry.
pub async fn fresh(store: &Store) -> anyhow::Result<String> {
    let _lock = store.lock()?;
    let Some(Credential::Xai { access, refresh, expires_ms, token_endpoint }) =
        store.get(PROVIDER)?
    else {
        bail!("not signed in to xAI; run `stonks login`");
    };
    if expires_ms > now_ms() + REFRESH_SKEW_MS {
        return Ok(access);
    }
    let endpoint = trusted(&Value::String(token_endpoint))?.to_owned();
    let form =
        [("grant_type", "refresh_token"), ("client_id", CLIENT_ID), ("refresh_token", &*refresh)];
    let renewed = match token_request(&endpoint, &form, Some(&refresh)).await {
        Ok(credential) => credential,
        Err(TokenError::Other(err)) => {
            return Err(err.context("the xAI sign-in has expired; run `stonks login`"));
        }
        Err(_) => bail!("the xAI sign-in has expired; run `stonks login`"),
    };
    store.set(PROVIDER, renewed.clone())?;
    match renewed {
        Credential::Xai { access, .. } => Ok(access),
        _ => unreachable!("token_request returns an xAI credential"),
    }
}

/// Models this account can use.
pub async fn models(store: &Store) -> anyhow::Result<Vec<String>> {
    let access = fresh(store).await?;
    let body: Value = reqwest::Client::new()
        .get(MODELS_URL)
        .bearer_auth(access)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let rows = body["data"].as_array().context("no model list")?;
    Ok(rows.iter().filter_map(|row| row["id"].as_str()).map(str::to_owned).collect())
}

/// OAuth endpoints come from a discovery document; only follow them to x.ai.
fn trusted(url: &Value) -> anyhow::Result<&str> {
    let url = url.as_str().context("xAI sign-in metadata is incomplete")?;
    let parsed = reqwest::Url::parse(url)?;
    let host = parsed.host_str().unwrap_or("");
    if parsed.scheme() == "https" && (host == "x.ai" || host.ends_with(".x.ai")) {
        Ok(url)
    } else {
        bail!("xAI sign-in metadata points outside x.ai: {url}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_x_ai_hosts_are_trusted() {
        assert!(trusted(&Value::from("https://auth.x.ai/oauth2/token")).is_ok());
        assert!(trusted(&Value::from("https://x.ai.evil.example/token")).is_err());
        assert!(trusted(&Value::from("http://auth.x.ai/token")).is_err());
    }
}
