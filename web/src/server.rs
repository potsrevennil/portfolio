//! The axum server: Leptos routes and server functions over one SQLite pool.

use std::str::FromStr;

use anyhow::{Context, Result};
use axum::Router;
use leptos::prelude::*;
use leptos_axum::{generate_route_list, LeptosRoutes};
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};

use crate::app::{shell, App};

/// The database the server reads, unless `LEDGER_DATABASE_URL` names another.
/// The same default `load-journal` writes to.
const DEFAULT_DATABASE_URL: &str = "sqlite:ledger-app.db";

pub async fn open(url: &str) -> Result<SqlitePool> {
    // Never create: a mistyped path should fail, not serve an empty ledger.
    let options = SqliteConnectOptions::from_str(url)?.create_if_missing(false);
    SqlitePool::connect_with(options).await.with_context(|| format!("opening {url}"))
}

pub fn router(options: LeptosOptions, pool: SqlitePool) -> Router {
    let routes = generate_route_list(App);
    let context = move || provide_context(pool.clone());
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
    let addr = options.site_addr;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log_listening(addr);
    axum::serve(listener, router(options, pool).into_make_service()).await?;
    Ok(())
}

fn log_listening(addr: std::net::SocketAddr) {
    println!("listening on http://{addr}");
}
