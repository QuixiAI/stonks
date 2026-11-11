//! `stonks login`: add a provider (sign in, or enter its API key), then pick
//! one of its models. The selection is saved once a test call works.

use std::path::Path;

use anyhow::{Context, bail};
use clap::Args;
use inquire::{Password, PasswordDisplayMode, Select, Text};

use super::ui::{self, prompt};
use crate::auth::{self, Credential, Store};
use crate::config::{Config, Llm};
use crate::providers::{self, AuthMethod, CUSTOM, Provider, SignIn};

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Provider id (e.g. `openai`, `xai`, `openrouter`); skips the provider menu.
    #[arg(long)]
    provider: Option<String>,
    /// Auth method; skips that menu for providers that offer a sign-in.
    #[arg(long, value_enum)]
    auth: Option<AuthMethod>,
    /// Model name; skips the model menu.
    #[arg(long)]
    model: Option<String>,
    /// Sign in again, or re-enter the API key, even if one is saved.
    #[arg(long)]
    force: bool,
}

pub async fn run(args: LoginArgs, config_path: &Path) -> anyhow::Result<()> {
    let config = Config::load_or_default(config_path)?;
    let store = Store::new(&config.paths.credentials);

    let provider = match &args.provider {
        Some(id) => providers::find(id).with_context(|| format!("unknown provider: {id}"))?,
        None => pick_provider(&config.llm, &store)?,
    };
    let method = match (provider.sign_in, args.auth) {
        (None, Some(AuthMethod::Oauth)) => bail!("{} has no sign-in; use an API key", provider.name),
        (None, _) => AuthMethod::ApiKey,
        (Some(_), Some(method)) => method,
        (Some(sign_in), None) => pick_auth(sign_in, &config.llm, provider)?,
    };

    let base_url = authenticate(provider, method, &store, args.force).await?;

    let same = config.llm.provider == provider.id && config.llm.auth == method;
    let mut llm = Llm { provider: provider.id.to_owned(), auth: method, model: String::new(), base_url };
    llm.model = match args.model {
        Some(model) => model,
        None => ui::pick_model(&llm, &store, if same { &config.llm.model } else { "" }).await?,
    };
    ui::test_and_save(llm, &ui::label(provider, method), config, &store, config_path).await
}

fn pick_provider(current: &Llm, store: &Store) -> anyhow::Result<Provider> {
    let all = providers::all();
    let rows: Vec<String> = all
        .iter()
        .map(|provider| {
            let how = if provider.sign_in.is_some() { "sign in or API key" } else { "API key" };
            let added = matches!(store.get(provider.id), Ok(Some(_)))
                || matches!(store.get(ui::sign_in_key(*provider)), Ok(Some(_)));
            let mut row = format!("{:<28} {how}", provider.name);
            if added {
                row.push_str("  (added)");
            }
            row
        })
        .collect();
    let start = all.iter().position(|provider| provider.id == current.provider).unwrap_or(0);
    let choice = prompt(
        Select::new("Provider:", rows)
            .with_starting_cursor(start)
            .with_page_size(12)
            .with_scorer(&ui::contains)
            .with_help_message(ui::HELP)
            .raw_prompt(),
    )?;
    Ok(all[choice.index])
}

/// The second menu, for providers that offer a sign-in besides an API key.
fn pick_auth(sign_in: SignIn, current: &Llm, provider: Provider) -> anyhow::Result<AuthMethod> {
    let rows = vec![sign_in.label(), "API key"];
    let on_key = current.provider == provider.id && current.auth == AuthMethod::ApiKey;
    let choice = prompt(
        Select::new("Auth method:", rows).with_starting_cursor(usize::from(on_key)).raw_prompt(),
    )?;
    Ok(if choice.index == 0 { AuthMethod::Oauth } else { AuthMethod::ApiKey })
}

/// Makes sure a usable credential for `provider` is stored. Returns the base
/// URL to use, which is empty except for a custom endpoint.
async fn authenticate(
    provider: Provider,
    method: AuthMethod,
    store: &Store,
    force: bool,
) -> anyhow::Result<String> {
    let sign_in = if method == AuthMethod::Oauth { provider.sign_in } else { None };
    match sign_in {
        Some(SignIn::Codex) => {
            let saved = store.get(auth::codex::PROVIDER)?;
            if !force && saved.is_some() && auth::codex::fresh(store).await.is_ok() {
                println!("Already signed in to ChatGPT.");
            } else {
                store.set(auth::codex::PROVIDER, auth::codex::login().await?)?;
                println!("Signed in to ChatGPT.");
            }
        }
        Some(SignIn::Xai) => {
            let saved = store.get(auth::xai::PROVIDER)?;
            if !force && saved.is_some() && auth::xai::fresh(store).await.is_ok() {
                println!("Already signed in to xAI.");
            } else {
                store.set(auth::xai::PROVIDER, auth::xai::login().await?)?;
                println!("Signed in to xAI.");
            }
        }
        None => return api_key(provider, store, force),
    }
    Ok(String::new())
}

fn api_key(provider: Provider, store: &Store, force: bool) -> anyhow::Result<String> {
    let (saved_key, saved_url) = match store.get(provider.id)? {
        Some(Credential::ApiKey { key, base_url }) => (Some(key), base_url),
        _ => (None, String::new()),
    };
    let custom = provider.id == CUSTOM;
    let base_url = if custom {
        let default = if saved_url.is_empty() { "http://localhost:11434/v1" } else { &saved_url };
        prompt(Text::new("Base URL:").with_default(default).prompt())?.trim().to_owned()
    } else {
        String::new()
    };

    let key = match saved_key {
        Some(key) if !force => {
            println!("Using the saved {} key ({}).", provider.name, ui::masked(&key));
            key
        }
        _ => {
            let label = if custom {
                format!("{} API key (Enter if none):", provider.name)
            } else {
                format!("{} API key:", provider.name)
            };
            let key = prompt(
                Password::new(&label)
                    .with_display_mode(PasswordDisplayMode::Masked)
                    .without_confirmation()
                    .prompt(),
            )?;
            match key.trim() {
                "" if custom => "none".to_owned(),
                "" => bail!("no key entered"),
                key => key.to_owned(),
            }
        }
    };
    store.set(provider.id, Credential::ApiKey { key, base_url: base_url.clone() })?;
    Ok(base_url)
}
