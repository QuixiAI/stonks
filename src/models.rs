//! Model lists for the picker. Sign-in providers and custom endpoints are
//! asked directly; everything else comes from the public models.dev catalog.

use anyhow::Context;
use serde_json::Value;

use crate::auth::{self, Credential, Store};
use crate::config::Llm;
use crate::providers::{AuthMethod, CUSTOM};

const CATALOG_URL: &str = "https://models.dev/api.json";

/// Model names for `llm.provider`, most relevant first.
pub async fn list(llm: &Llm, store: &Store) -> anyhow::Result<Vec<String>> {
    match (llm.provider.as_str(), llm.auth) {
        ("openai", AuthMethod::Oauth) => auth::codex::models(store).await,
        ("xai", AuthMethod::Oauth) => match auth::xai::models(store).await {
            Ok(models) if !models.is_empty() => Ok(models),
            _ => catalog("xai").await,
        },
        (CUSTOM, _) => endpoint(llm, store).await,
        (provider, _) => catalog(provider).await,
    }
}

/// Newest models first, from models.dev.
async fn catalog(provider: &str) -> anyhow::Result<Vec<String>> {
    let all: Value = reqwest::get(CATALOG_URL).await?.error_for_status()?.json().await?;
    let models = all[provider]["models"].as_object().context("no catalog entry")?;
    let mut rows: Vec<(&str, &str)> = models
        .iter()
        .map(|(id, model)| (model["release_date"].as_str().unwrap_or(""), id.as_str()))
        .collect();
    rows.sort_by(|a, b| b.cmp(a));
    Ok(rows.into_iter().map(|(_, id)| id.to_owned()).collect())
}

/// `GET {base_url}/models` on an OpenAI-compatible server.
async fn endpoint(llm: &Llm, store: &Store) -> anyhow::Result<Vec<String>> {
    let url = format!("{}/models", llm.base_url.trim_end_matches('/'));
    let mut request = reqwest::Client::new().get(url);
    if let Some(Credential::ApiKey { key, .. }) = store.get(&llm.provider)? {
        request = request.bearer_auth(key);
    }
    let body: Value = request.send().await?.error_for_status()?.json().await?;
    let rows = body["data"].as_array().context("no model list")?;
    Ok(rows.iter().filter_map(|row| row["id"].as_str()).map(str::to_owned).collect())
}
