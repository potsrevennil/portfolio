//! The axum server: Leptos routes and server functions over one SQLite pool.

use std::path::Path;

use anyhow::Result;
use axum::Router;
use db::{
    market::{Market, Rates},
    review::StatementAccounts,
    SqlitePool,
};
use ledger::{accounts::Chart, investments::Investments, valuation::AtCost};
use leptos::prelude::*;
use leptos_axum::{generate_route_list, LeptosRoutes};

use crate::app::{shell, App};

/// The database the server reads, unless `LEDGER_DATABASE_URL` names another.
/// The same default `load-journal` writes to.
const DEFAULT_DATABASE_URL: &str = "sqlite:ledger-app.db";

/// The chart config, for the reporting rules that are not in the database.
/// `LEDGER_MAPPING` overrides it.
const DEFAULT_MAPPING: &str = "ledger/mapping.toml";

/// The reporting rules `mapping.toml` sets beside the database.
#[derive(Debug, Default, Clone)]
pub struct Config {
    pub at_cost: AtCost,
    pub investments: Investments,
    pub statements: StatementAccounts,
}

impl Config {
    /// A ledger with no mapping has no rules: nothing carried at cost, no
    /// investment results apart, and only accounts with a statement imported
    /// count as statement accounts. A mapping that does not parse must stop
    /// the server, or its rules would be dropped silently.
    pub fn load(mapping: &Path) -> Result<Self> {
        match mapping.exists() {
            true => Ok(Config {
                at_cost: AtCost::load(mapping)?,
                investments: Investments::load(mapping)?,
                statements: StatementAccounts::from(&Chart::load(mapping)?),
            }),
            false => {
                println!("{} not found: no reporting rules", mapping.display());
                Ok(Config::default())
            }
        }
    }
}

pub fn router(options: LeptosOptions, pool: SqlitePool, market: Market, config: Config) -> Router {
    let routes = generate_route_list(App);
    let context = move || {
        provide_context(pool.clone());
        provide_context(market.clone());
        provide_context(config.at_cost.clone());
        provide_context(config.investments.clone());
        provide_context(config.statements.clone());
    };
    Router::new()
        .leptos_routes_with_context(&options, routes, context, {
            let options = options.clone();
            move || shell(options.clone())
        })
        .fallback(leptos_axum::file_and_error_handler(shell))
        .with_state(options)
}

pub async fn run() -> Result<()> {
    let options = get_configuration(None)?.leptos_options;
    let url = std::env::var("LEDGER_DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.into());
    let pool = db::connect(&url).await?;
    let market_url =
        std::env::var("LEDGER_MARKET_URL").unwrap_or_else(|_| db::market::DEFAULT_URL.into());
    let rates = Rates { ledger: pool.clone(), market: Market::open(&market_url).await? };
    match crate::rates::Config::from_env()? {
        Some(config) => crate::rates::start(rates, config),
        None => println!("LEDGER_RATES=off: rates are not fetched"),
    }
    // Pages only read the cache; fetches write it.
    let market = Market::read_only(&market_url).await?;
    let mapping = std::env::var("LEDGER_MAPPING").unwrap_or_else(|_| DEFAULT_MAPPING.into());
    let config = Config::load(Path::new(&mapping))?;
    let addr = options.site_addr;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log_listening(addr);
    let app = router(options, pool, market, config);
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

fn log_listening(addr: std::net::SocketAddr) {
    println!("listening on http://{addr}");
}
