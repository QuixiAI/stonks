//! Command-line interface: argument definitions and dispatch.

mod config;
mod cron;
mod login;
mod model;
mod run;
mod ui;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::Config;

#[derive(Debug, Parser)]
#[command(name = "stonks", version, about = "Trading bot")]
pub struct Cli {
    /// Config file to use instead of the default location.
    #[arg(long, global = true, env = "STONKS_CONFIG", value_name = "FILE")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage configuration.
    #[command(subcommand)]
    Config(config::ConfigCommand),
    /// Manage the cron job that runs the bot.
    #[command(subcommand)]
    Cron(cron::CronCommand),
    /// Add an LLM provider (sign in, or enter its API key) and pick one of its models.
    Login(login::LoginArgs),
    /// Change the selected model, from any provider you have added.
    Model(model::ModelArgs),
    /// Run one trading cycle. This is what the cron job invokes.
    Run,
}

impl Cli {
    pub async fn run(self) -> anyhow::Result<()> {
        let path = match self.config {
            Some(path) => path,
            None => Config::default_path()?,
        };
        match self.command {
            Command::Config(cmd) => config::run(cmd, &path),
            Command::Cron(cmd) => cron::run(cmd, &path),
            Command::Login(args) => login::run(args, &path).await,
            Command::Model(args) => model::run(args, &path).await,
            Command::Run => run::run(&Config::load(&path)?).await,
        }
    }
}
