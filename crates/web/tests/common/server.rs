//! Calling the server functions over the queue ledger, directly or through
//! the router, and the database reads and edits the tests share.
#![allow(dead_code)]

use db::review::StatementAccounts;
use ledger::valuation::AtCost;
use leptos::{prelude::*, reactive::computed::ScopedFuture};
use sqlx::SqlitePool;
use web::{model::LegInput, review::save_entry, server};

use super::{page::Site, queue::ledger};

/// Runs a server function as the server would, with the pool in context.
pub async fn call<T, F>(pool: &SqlitePool, f: impl FnOnce() -> F) -> Result<T, ServerFnError>
where
    F: std::future::Future<Output = Result<T, ServerFnError>>,
{
    let owner = Owner::new();
    let call = owner.with(|| {
        provide_context(pool.clone());
        provide_context(statement_accounts());
        ScopedFuture::new(f())
    });
    call.await
}

/// What the chart would name: the savings account, and a card whose first
/// statement is still to come. Cash is neither.
pub fn statement_accounts() -> StatementAccounts {
    ["Assets:Bank:Savings", "Liabilities:Card"].into_iter().collect()
}

pub async fn site() -> Site {
    Site::new(|| async {
        let (dir, pool) = ledger().await;
        let options = LeptosOptions::builder().output_name("web").build();
        (server::router(options, pool, AtCost::default(), statement_accounts()), dir)
    })
    .await
}

pub async fn id_of(pool: &SqlitePool, narration: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM transactions WHERE narration = ?")
        .bind(narration)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// `(posting id, account, amount, origin)` in posting order.
pub async fn legs(pool: &SqlitePool, id: i64) -> Vec<(i64, String, String, Option<String>)> {
    sqlx::query_as(
        "SELECT p.id, a.path, p.amount, p.origin FROM postings p JOIN accounts a ON a.id = \
         p.account_id WHERE p.transaction_id = ? ORDER BY p.id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
}

pub fn leg(posting: Option<i64>, account: &str, amount: &str) -> LegInput {
    LegInput {
        posting: posting.map(|p| p.to_string()).unwrap_or_default(),
        account: account.into(),
        amount: amount.into(),
        currency: "TWD".into(),
    }
}

pub async fn save(
    pool: &SqlitePool,
    id: i64,
    narration: &str,
    legs: Vec<LegInput>,
    confirm: bool,
) -> Result<(), ServerFnError> {
    let narration = Some(narration.to_string());
    let confirm = confirm.then(|| "1".to_string());
    call(pool, || save_entry(id, narration, legs, confirm)).await
}
