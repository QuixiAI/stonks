//! Configuration: one YAML file (`~/.config/stonks/config.yml` unless `--config` says
//! otherwise), managed through the CLI.
//!
//! Secrets are never stored here; API keys come from the environment.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::providers::{self, AuthMethod};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub llm: Llm,
    pub broker: Broker,
    pub cron: Cron,
    pub paths: Paths,
}

/// LLM settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Llm {
    /// A provider id from `providers::all()`; empty until `stonks login` is run.
    pub provider: String,
    /// Sign-in or API key. Only some providers offer a sign-in.
    pub auth: AuthMethod,
    pub model: String,
    /// Overrides the provider's default endpoint. Empty means the default.
    pub base_url: String,
}

/// Interactive Brokers, reached through its hosted MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Broker {
    pub mcp_url: String,
}

/// The crontab entry that invokes `stonks run`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cron {
    /// Five-field cron expression.
    pub schedule: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Paths {
    pub log: PathBuf,
    /// API keys and sign-in tokens (mode 0600).
    pub credentials: PathBuf,
}

impl Default for Broker {
    fn default() -> Self {
        Self {
            mcp_url: "https://api.ibkr.com/v1/api/mcp-public".into(),
        }
    }
}

impl Default for Cron {
    fn default() -> Self {
        Self { schedule: "*/15 * * * 1-5".into() }
    }
}

impl Default for Paths {
    fn default() -> Self {
        let data = project_dirs()
            .map(|dirs| dirs.data_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let config = project_dirs()
            .map(|dirs| dirs.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            credentials: config.join("credentials.json"),
            log: data.join("stonks.log"),
        }
    }
}

impl Config {
    pub fn default_path() -> anyhow::Result<PathBuf> {
        let dirs = project_dirs().context("cannot determine the home directory")?;
        Ok(dirs.config_dir().join("config.yml"))
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = fs::read_to_string(path).with_context(|| {
            format!("cannot read {} (run `stonks config init` to create it)", path.display())
        })?;
        let config: Self = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("invalid config in {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    /// Like `load`, but a missing file yields the defaults.
    pub fn load_or_default(path: &Path) -> anyhow::Result<Self> {
        if path.exists() { Self::load(path) } else { Ok(Self::default()) }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let provider = &self.llm.provider;
        if provider.is_empty() {
            return Ok(());
        }
        let Some(provider) = providers::find(provider) else {
            anyhow::bail!("unknown llm.provider {provider:?}; run `stonks login` to choose one");
        };
        if self.llm.auth == AuthMethod::Oauth && provider.sign_in.is_none() {
            anyhow::bail!("{} has no sign-in; run `stonks login` to choose again", provider.name);
        }
        Ok(())
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, self.to_yaml()?).with_context(|| format!("cannot write {}", path.display()))
    }

    pub fn to_yaml(&self) -> anyhow::Result<String> {
        Ok(serde_yaml_ng::to_string(self)?)
    }
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "stonks")
}
