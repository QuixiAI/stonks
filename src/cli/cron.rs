//! `stonks cron …`

use std::path::Path;

use clap::Subcommand;

use crate::config::Config;
use crate::cron;

#[derive(Debug, Subcommand)]
pub enum CronCommand {
    /// Add the stonks entry to your crontab, replacing any earlier one.
    Install,
    /// Remove the stonks entry from your crontab.
    Uninstall,
    /// Show the installed entry, if any.
    Status,
}

pub fn run(cmd: CronCommand, config_path: &Path) -> anyhow::Result<()> {
    match cmd {
        CronCommand::Install => {
            let config = Config::load(config_path)?;
            let line = cron::install(&config, config_path)?;
            println!("installed: {line}");
        }
        CronCommand::Uninstall => {
            if cron::uninstall()? {
                println!("removed");
            } else {
                println!("not installed");
            }
        }
        CronCommand::Status => match cron::installed()? {
            Some(line) => println!("installed: {line}"),
            None => println!("not installed"),
        },
    }
    Ok(())
}
