//! Pieces shared by the OAuth sign-in flows: browser launch, the loopback
//! callback listener, PKCE, and JWT payload decoding.

use std::time::Duration;

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// True when this machine cannot show a browser to the user (SSH session, or
/// Linux without a display). Sign-in then uses a code the user enters on
/// another device.
pub fn headless() -> bool {
    let set = |name| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    set("SSH_CONNECTION")
        || set("SSH_TTY")
        || (cfg!(target_os = "linux") && !set("DISPLAY") && !set("WAYLAND_DISPLAY"))
}

/// Opens `url` in the user's browser. Returns whether a browser was launched.
pub fn open_browser(url: &str) -> bool {
    !headless() && open::that_detached(url).is_ok()
}

pub fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

/// The PKCE S256 challenge for `verifier`.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Decodes the (unverified) payload of a JWT.
pub fn jwt_payload(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?).ok()
}

/// Waits for the browser to be redirected to `path` on `listener` and returns
/// the authorization code, after checking `state`.
pub async fn wait_for_callback(
    listener: TcpListener,
    path: &str,
    state: &str,
    timeout: Duration,
) -> anyhow::Result<String> {
    tokio::time::timeout(timeout, async {
        loop {
            let (mut socket, _) = listener.accept().await?;
            let mut buf = vec![0u8; 8192];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            // Request line: `GET /auth/callback?code=…&state=… HTTP/1.1`
            let target = request.split_whitespace().nth(1).unwrap_or("");
            let outcome = parse_callback(target, path, state);
            let (status, body) = match &outcome {
                Some(Ok(_)) => ("200 OK", "Signed in. You can close this tab and return to stonks."),
                Some(Err(_)) => ("400 Bad Request", "Sign-in failed. Return to stonks for details."),
                None => ("404 Not Found", "Not found."),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
            // Browsers also ask for /favicon.ico and the like; keep waiting.
            if let Some(result) = outcome {
                return result;
            }
        }
    })
    .await
    .context("timed out waiting for the browser sign-in")?
}

/// `None` when the request is not for the callback path at all.
fn parse_callback(target: &str, path: &str, state: &str) -> Option<anyhow::Result<String>> {
    let url = reqwest::Url::parse(&format!("http://localhost{target}")).ok()?;
    if url.path() != path {
        return None;
    }
    let param = |name: &str| {
        url.query_pairs().find(|(key, _)| key == name).map(|(_, value)| value.into_owned())
    };
    Some((|| {
        if let Some(error) = param("error") {
            let detail = param("error_description").unwrap_or_default();
            bail!("sign-in was not completed: {error} {detail}");
        }
        if param("state").as_deref() != Some(state) {
            bail!("sign-in response did not match this attempt (state mismatch)");
        }
        param("code").context("sign-in response had no authorization code")
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_parsing() {
        let ok = parse_callback("/auth/callback?code=abc&state=s1", "/auth/callback", "s1");
        assert_eq!(ok.unwrap().unwrap(), "abc");
        assert!(parse_callback("/favicon.ico", "/auth/callback", "s1").is_none());
        let wrong_state = parse_callback("/auth/callback?code=abc&state=zz", "/auth/callback", "s1");
        assert!(wrong_state.unwrap().is_err());
        let denied = parse_callback("/auth/callback?error=access_denied", "/auth/callback", "s1");
        assert!(denied.unwrap().is_err());
    }

    #[test]
    fn pkce_matches_rfc7636_example() {
        let challenge = pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }
}
