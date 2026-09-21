use anyhow::Result;
use clap::Parser;
use cli::Cli;
use env_logger::Env;

mod cli;

#[tokio::main]
async fn main() -> Result<()> {
    // Our own progress at info and everyone's warnings by default, overridable
    // with RUST_LOG. Plain `env_logger::init()` shows only errors, which would
    // hide the diagnostics these commands emit.
    env_logger::Builder::from_env(Env::default().default_filter_or(
        "warn,portfolio=info,db=info,ledger=info,store=info,import=info,prices=info",
    ))
    .init();
    Cli::parse().run().await
}
