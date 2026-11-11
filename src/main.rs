use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    stonks::cli::Cli::parse().run().await
}
