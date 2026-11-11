//! One bot cycle. This is what the cron job runs.

use crate::config::Config;

pub async fn run(_config: &Config) -> anyhow::Result<()> {
    anyhow::bail!("the trading cycle is not implemented yet")
}
