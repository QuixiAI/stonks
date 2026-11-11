//! `stonks run`

use crate::bot;
use crate::config::Config;

pub async fn run(config: &Config) -> anyhow::Result<()> {
    bot::run(config).await
}
