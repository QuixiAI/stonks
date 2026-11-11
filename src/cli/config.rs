//! `stonks config …`

use std::path::Path;

use anyhow::{Context, bail};
use clap::Subcommand;

use serde_yaml_ng::Value;

use crate::config::Config;

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Write a config file holding the defaults.
    Init {
        /// Replace an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Print the config file location.
    Path,
    /// Print the effective configuration.
    Show,
    /// Print one value, addressed by dotted key (e.g. `llm.model`).
    Get { key: String },
    /// Set one value, addressed by dotted key (e.g. `llm.model`).
    Set { key: String, value: String },
}

pub fn run(cmd: ConfigCommand, path: &Path) -> anyhow::Result<()> {
    match cmd {
        ConfigCommand::Init { force } => {
            if path.exists() && !force {
                bail!("{} already exists (use --force to replace it)", path.display());
            }
            Config::default().save(path)?;
            println!("wrote {}", path.display());
        }
        ConfigCommand::Path => println!("{}", path.display()),
        ConfigCommand::Show => print!("{}", Config::load(path)?.to_yaml()?),
        ConfigCommand::Get { key } => {
            let tree = serde_yaml_ng::to_value(Config::load(path)?)?;
            let value = lookup(&tree, &key).with_context(|| format!("no such key: {key}"))?;
            match value {
                Value::String(s) => println!("{s}"),
                other => print!("{}", serde_yaml_ng::to_string(other)?),
            }
        }
        ConfigCommand::Set { key, value } => {
            let mut tree = serde_yaml_ng::to_value(Config::load(path)?)?;
            let slot = lookup_mut(&mut tree, &key).with_context(|| format!("no such key: {key}"))?;
            *slot = parse_like(slot, &value)?;
            // Round-trip through `Config` so a bad value is rejected before it is saved.
            let config: Config = serde_yaml_ng::from_value(tree).context("invalid value")?;
            config.validate()?;
            config.save(path)?;
        }
    }
    Ok(())
}

fn lookup<'a>(root: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.').try_fold(root, |node, part| node.get(part))
}

fn lookup_mut<'a>(root: &'a mut Value, key: &str) -> Option<&'a mut Value> {
    key.split('.').try_fold(root, |node, part| node.get_mut(part))
}

/// Parses `raw` as the same type the key currently holds.
fn parse_like(current: &Value, raw: &str) -> anyhow::Result<Value> {
    Ok(match current {
        Value::String(_) => Value::String(raw.to_owned()),
        Value::Number(n) if n.is_f64() => Value::from(raw.parse::<f64>()?),
        Value::Number(_) => Value::from(raw.parse::<i64>()?),
        Value::Bool(_) => Value::Bool(raw.parse()?),
        _ => bail!("this key holds a group of settings; set one of its fields instead"),
    })
}
