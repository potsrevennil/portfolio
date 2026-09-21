//! The axum server: Leptos routes and server functions over one SQLite pool.

use std::{path::Path, str::FromStr};

use anyhow::{Context, Result};
use axum::Router;
use ledger::valuation::AtCost;
use leptos::prelude::*;
use leptos_axum::{generate_route_list, LeptosRoutes};
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};

use crate::app::{shell, App};

/// The database the server reads, unless `LEDGER_DATABASE_URL` names another.
/// The same default `load-journal` writes to.
const DEFAULT_DATABASE_URL: &str = "sqlite:ledger-app.db";

/// The chart config, for the reporting rules that are not in the database.
/// `LEDGER_MAPPING` overrides it.
const DEFAULT_MAPPING: &str = "ledger/mapping.toml";

pub async fn open(url: &str) -> Result<SqlitePool> {
    // Never create: a mistyped path should fail, not serve an empty ledger.
    let options = SqliteConnectOptions::from_str(url)?.create_if_missing(false);
    SqlitePool::connect_with(options).await.with_context(|| format!("opening {url}"))
}

pub fn router(options: LeptosOptions, pool: SqlitePool, at_cost: AtCost) -> Router {
    let routes = generate_route_list(App);
    let context = move || {
        provide_context(pool.clone());
        provide_context(at_cost.clone());
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
    let pool = open(&url).await?;
    let mapping = std::env::var("LEDGER_MAPPING").unwrap_or_else(|_| DEFAULT_MAPPING.into());
    let at_cost = at_cost(Path::new(&mapping))?;
    let addr = options.site_addr;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log_listening(addr);
    axum::serve(listener, router(options, pool, at_cost).into_make_service()).await?;
    Ok(())
}

/// A ledger with no mapping lists no at-cost holdings; one whose mapping
/// does not parse must not start, or it would count them silently.
pub fn at_cost(mapping: &Path) -> Result<AtCost> {
    match mapping.exists() {
        true => AtCost::load(mapping),
        false => {
            println!("{} not found: no holdings carried at cost", mapping.display());
            Ok(AtCost::default())
        }
    }
}

fn log_listening(addr: std::net::SocketAddr) {
    println!("listening on http://{addr}");
}
