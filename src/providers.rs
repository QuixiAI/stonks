//! The providers a user can choose, in the order the menu shows them:
//! those that offer sign-in first, then OpenRouter, then the rest.

use serde::{Deserialize, Serialize};

use crate::llm::API_KEY_PROVIDERS;

/// How stonks authenticates to the chosen provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    /// Sign in with a subscription account.
    Oauth,
    #[default]
    ApiKey,
}

/// A sign-in a provider offers besides its API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignIn {
    /// ChatGPT account (Codex).
    Codex,
    /// X account (SuperGrok).
    Xai,
}

impl SignIn {
    /// The menu entry for this sign-in.
    pub fn label(self) -> &'static str {
        match self {
            SignIn::Codex => "Sign in with ChatGPT (Codex)",
            SignIn::Xai => "Sign in with X (SuperGrok)",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Provider {
    /// The value stored in `llm.provider`.
    pub id: &'static str,
    pub name: &'static str,
    /// Every provider takes an API key; a few also offer a sign-in.
    pub sign_in: Option<SignIn>,
}

/// The one provider that needs a base URL from the user.
pub const CUSTOM: &str = "openaicompatible";

pub fn all() -> Vec<Provider> {
    let mut all: Vec<Provider> = API_KEY_PROVIDERS
        .iter()
        .map(|&(id, name)| Provider {
            id,
            name,
            sign_in: match id {
                "openai" => Some(SignIn::Codex),
                "xai" => Some(SignIn::Xai),
                _ => None,
            },
        })
        .collect();
    // Stable: sign-in providers move to the top, the rest keep their order.
    all.sort_by_key(|provider| provider.sign_in.is_none());
    all
}

pub fn find(id: &str) -> Option<Provider> {
    all().into_iter().find(|provider| provider.id == id)
}
