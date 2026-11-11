//! Stored credentials: API keys and OAuth tokens, one entry per provider id.
//!
//! They live in their own file (mode 0600), apart from the config, so the
//! cron job can use them without any environment set up.

pub mod codex;
pub mod flow;
pub mod xai;

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Credential {
    ApiKey {
        key: String,
        /// Set only for a custom endpoint.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        base_url: String,
    },
    Codex {
        access: String,
        refresh: String,
        expires_ms: u64,
        account_id: String,
    },
    Xai {
        access: String,
        refresh: String,
        expires_ms: u64,
        token_endpoint: String,
    },
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(path: &Path) -> Self {
        Self { path: path.to_owned() }
    }

    pub fn get(&self, provider: &str) -> anyhow::Result<Option<Credential>> {
        Ok(self.load()?.remove(provider))
    }

    pub fn set(&self, provider: &str, credential: Credential) -> anyhow::Result<()> {
        let mut all = self.load()?;
        all.insert(provider.to_owned(), credential);
        self.save(&all)
    }

    /// Holds an exclusive lock until the returned file is dropped, so two
    /// processes never refresh the same single-use refresh token at once.
    pub fn lock(&self) -> anyhow::Result<File> {
        self.create_dir()?;
        let file = File::create(self.path.with_extension("lock"))?;
        file.lock()?;
        Ok(file)
    }

    fn load(&self) -> anyhow::Result<BTreeMap<String, Credential>> {
        match fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("invalid credentials file {}", self.path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(err) => Err(err).with_context(|| format!("cannot read {}", self.path.display())),
        }
    }

    fn save(&self, all: &BTreeMap<String, Credential>) -> anyhow::Result<()> {
        self.create_dir()?;
        // Write a private temp file, then rename, so a crash never leaves a
        // half-written or world-readable credentials file.
        let tmp = self.path.with_extension("tmp");
        let _ = fs::remove_file(&tmp);
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        file.write_all(serde_json::to_string_pretty(all)?.as_bytes())?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("cannot write {}", self.path.display()))
    }

    fn create_dir(&self) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent().filter(|dir| !dir.exists()) {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        Ok(())
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Tokens are refreshed this long before they expire.
pub(crate) const REFRESH_SKEW_MS: u64 = 5 * 60 * 1000;
