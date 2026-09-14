use anyhow::Result;
use clap::Parser;
use portfolio::cli::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    Cli::parse().run().await
}
