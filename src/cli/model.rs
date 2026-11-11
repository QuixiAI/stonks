//! `stonks model`: change the selected model. Pick one of the providers added
//! with `stonks login` (skipped when there is only one), then one of its models.

use std::path::Path;

use anyhow::bail;
use clap::{Args, Subcommand};
use inquire::Select;

use super::ui::{self, prompt};
use crate::auth::{Credential, Store};
use crate::config::{Config, Llm};
use crate::providers::{self, AuthMethod, Provider};
use crate::llm;

#[derive(Debug, Args)]
pub struct ModelArgs {
    #[command(subcommand)]
    command: Option<ModelCommand>,
    /// Provider id (e.g. `openai`, `openrouter`); skips the provider menu.
    #[arg(long)]
    provider: Option<String>,
    /// Auth method, for a provider added both ways.
    #[arg(long, value_enum)]
    auth: Option<AuthMethod>,
    /// Model name; skips the model menu.
    #[arg(long)]
    model: Option<String>,
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    /// Show the selected provider and model, and check that they work.
    Status,
}

/// One way of reaching a provider that the user has added.
struct Account {
    provider: Provider,
    method: AuthMethod,
    base_url: String,
}

impl Account {
    fn llm(&self, model: &str) -> Llm {
        Llm {
            provider: self.provider.id.to_owned(),
            auth: self.method,
            model: model.to_owned(),
            base_url: self.base_url.clone(),
        }
    }

    fn label(&self) -> String {
        ui::label(self.provider, self.method)
    }

    fn is(&self, llm: &Llm) -> bool {
        self.provider.id == llm.provider && self.method == llm.auth
    }
}

pub async fn run(args: ModelArgs, config_path: &Path) -> anyhow::Result<()> {
    let config = Config::load_or_default(config_path)?;
    let store = Store::new(&config.paths.credentials);
    if let Some(ModelCommand::Status) = args.command {
        return status(&config, &store).await;
    }

    let mut accounts = accounts(&store)?;
    if accounts.is_empty() {
        bail!("no providers have been added yet; run `stonks login`");
    }
    if let Some(id) = &args.provider {
        accounts.retain(|account| account.provider.id == id);
    }
    if let Some(method) = args.auth {
        accounts.retain(|account| account.method == method);
    }
    if accounts.is_empty() {
        bail!("that provider has not been added; run `stonks login`");
    }

    let (account, model) = match args.model {
        Some(model) => {
            // Prefer the account in use when the flags leave more than one.
            let at = accounts.iter().position(|account| account.is(&config.llm)).unwrap_or(0);
            if accounts.len() > 1 && !accounts[at].is(&config.llm) {
                bail!("more than one provider is added; pass --provider (and --auth) with --model");
            }
            (accounts.swap_remove(at), model)
        }
        None => {
            if let Some(provider) = providers::find(&config.llm.provider) {
                println!("Current: {} / {}", ui::label(provider, config.llm.auth), config.llm.model);
            }
            let account = match accounts.len() {
                1 => accounts.swap_remove(0),
                _ => pick_account(accounts, &config.llm)?,
            };
            let current = if account.is(&config.llm) { config.llm.model.as_str() } else { "" };
            let model = ui::pick_model(&account.llm(""), &store, current).await?;
            (account, model)
        }
    };
    let label = account.label();
    ui::test_and_save(account.llm(&model), &label, config, &store, config_path).await
}

/// Every provider and auth method with a stored credential, in menu order.
fn accounts(store: &Store) -> anyhow::Result<Vec<Account>> {
    let mut accounts = Vec::new();
    for provider in providers::all() {
        if provider.sign_in.is_some() && store.get(ui::sign_in_key(provider))?.is_some() {
            accounts.push(Account { provider, method: AuthMethod::Oauth, base_url: String::new() });
        }
        if let Some(Credential::ApiKey { base_url, .. }) = store.get(provider.id)? {
            accounts.push(Account { provider, method: AuthMethod::ApiKey, base_url });
        }
    }
    Ok(accounts)
}

/// The provider menu: only the providers that have been added.
fn pick_account(accounts: Vec<Account>, current: &Llm) -> anyhow::Result<Account> {
    let rows: Vec<String> = accounts
        .iter()
        .map(|account| {
            if account.is(current) { format!("{}  (current)", account.label()) } else { account.label() }
        })
        .collect();
    let start = accounts.iter().position(|account| account.is(current)).unwrap_or(0);
    let choice = prompt(
        Select::new("Provider:", rows)
            .with_starting_cursor(start)
            .with_scorer(&ui::contains)
            .with_help_message(ui::HELP)
            .raw_prompt(),
    )?;
    Ok(accounts.into_iter().nth(choice.index).expect("the menu returns a listed row"))
}

async fn status(config: &Config, store: &Store) -> anyhow::Result<()> {
    let llm = &config.llm;
    let Some(provider) = providers::find(&llm.provider) else {
        println!("No model selected yet. Run `stonks login`.");
        return Ok(());
    };
    println!("Provider:    {} ({})", provider.name, provider.id);
    println!("Model:       {}", llm.model);
    if !llm.base_url.is_empty() {
        println!("Base URL:    {}", llm.base_url);
    }
    let key = if llm.auth == AuthMethod::Oauth { ui::sign_in_key(provider) } else { provider.id };
    let credential = match store.get(key)? {
        Some(Credential::ApiKey { key, .. }) => format!("API key {}", ui::masked(&key)),
        Some(Credential::Codex { account_id, .. }) => {
            format!("signed in with ChatGPT (account {account_id})")
        }
        Some(Credential::Xai { .. }) => "signed in with X".to_owned(),
        None if llm.auth == AuthMethod::ApiKey => {
            "none saved (the provider's environment variable is used if set)".to_owned()
        }
        None => "not signed in".to_owned(),
    };
    println!("Credential:  {credential}");
    match llm::generate(llm, store, "", ui::TEST_PROMPT).await {
        Ok(_) => println!("Status:      working"),
        Err(err) => {
            println!("Status:      not working");
            return Err(err.context("the test call failed"));
        }
    }
    Ok(())
}
