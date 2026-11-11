//! Prompt helpers shared by `stonks login` and `stonks model`.

use std::path::Path;

use anyhow::{Context, bail};
use inquire::{Select, Text};

use crate::auth::{self, Store};
use crate::config::{Config, Llm};
use crate::{llm, models};
use crate::providers::{AuthMethod, Provider, SignIn};

pub const HELP: &str = "type to filter, ↑↓ to move, Enter to select";
pub const TEST_PROMPT: &str = "Reply with the single word: ok";

/// Menu filter: keeps the rows that contain what was typed (ignoring case;
/// several words must all appear), in their original order.
pub fn contains(input: &str, _option: &String, row: &str, index: usize) -> Option<i64> {
    let row = row.to_lowercase();
    let matches = input.to_lowercase().split_whitespace().all(|word| row.contains(word));
    // inquire shows the highest score first, so earlier rows score higher.
    matches.then_some(-(index as i64))
}

/// Turns inquire's failures into something actionable.
pub fn prompt<T>(result: Result<T, inquire::InquireError>) -> anyhow::Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(inquire::InquireError::NotTTY) => {
            bail!("this needs an interactive terminal; pass --provider and --model to skip the menus")
        }
        Err(inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted) => {
            bail!("cancelled; nothing was changed")
        }
        Err(err) => Err(err.into()),
    }
}

/// The model menu for one provider: bare model names, the current one marked
/// and preselected, with typing a name as the last choice.
pub async fn pick_model(llm: &Llm, store: &Store, current: &str) -> anyhow::Result<String> {
    const ENTER_MANUALLY: &str = "Enter a model name…";
    let models = models::list(llm, store).await.unwrap_or_default();
    if models.is_empty() {
        println!("Could not fetch a model list for this provider.");
        return enter_model(current);
    }
    let start = models.iter().position(|model| model == current).unwrap_or(0);
    let mut rows: Vec<String> = models
        .iter()
        .map(|model| if model == current { format!("{model}  (current)") } else { model.clone() })
        .collect();
    rows.push(ENTER_MANUALLY.to_owned());
    let choice = prompt(
        Select::new("Model:", rows)
            .with_starting_cursor(start)
            .with_page_size(12)
            .with_scorer(&contains)
            .with_help_message(HELP)
            .raw_prompt(),
    )?;
    match models.into_iter().nth(choice.index) {
        Some(model) => Ok(model),
        None => enter_model(current),
    }
}

pub fn enter_model(current: &str) -> anyhow::Result<String> {
    let mut text = Text::new("Model name:");
    if !current.is_empty() {
        text = text.with_default(current);
    }
    let model = prompt(text.prompt())?;
    if model.trim().is_empty() {
        bail!("no model entered");
    }
    Ok(model.trim().to_owned())
}

/// Makes a test call with `llm` and, only if it works, saves it as the
/// provider and model stonks uses.
pub async fn test_and_save(
    llm: Llm,
    label: &str,
    mut config: Config,
    store: &Store,
    config_path: &Path,
) -> anyhow::Result<()> {
    println!("Testing {label} / {}…", llm.model);
    llm::generate(&llm, store, "", TEST_PROMPT)
        .await
        .context("the test call failed, so the selected model was not changed")?;
    config.llm = llm;
    config.save(config_path)?;
    println!("Working. stonks now uses {label} / {}.", config.llm.model);
    Ok(())
}

/// Where a provider's sign-in tokens are kept in the credential store.
pub fn sign_in_key(provider: Provider) -> &'static str {
    match provider.sign_in {
        Some(SignIn::Codex) => auth::codex::PROVIDER,
        Some(SignIn::Xai) => auth::xai::PROVIDER,
        None => "",
    }
}

/// The provider's name, qualified by auth method where it offers two.
pub fn label(provider: Provider, method: AuthMethod) -> String {
    match (provider.sign_in, method) {
        (Some(SignIn::Codex), AuthMethod::Oauth) => format!("{} (ChatGPT sign-in)", provider.name),
        (Some(SignIn::Xai), AuthMethod::Oauth) => format!("{} (X sign-in)", provider.name),
        (Some(_), AuthMethod::ApiKey) => format!("{} (API key)", provider.name),
        (None, _) => provider.name.to_owned(),
    }
}

/// Shows only the last four characters of a secret.
pub fn masked(secret: &str) -> String {
    let count = secret.chars().count();
    if count > 8 { format!("…{}", secret.chars().skip(count - 4).collect::<String>()) } else { "…".to_owned() }
}

#[cfg(test)]
mod tests {
    use super::contains;

    #[test]
    fn filter_is_substring_and_keeps_order() {
        let row = "OpenRouter / anthropic/claude-sonnet-5.5".to_owned();
        assert_eq!(contains("SONNET", &row, &row, 3), Some(-3));
        assert_eq!(contains("openrouter sonnet", &row, &row, 0), Some(0));
        assert_eq!(contains("", &row, &row, 7), Some(-7));
        // Letters that merely appear in order are not a match.
        assert_eq!(contains("sol", &row, &row, 0), None);
    }
}
