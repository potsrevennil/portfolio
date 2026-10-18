//! Server functions called as the server would call them, over the queue
//! ledger. Not every test binary uses every item.
#![allow(dead_code)]

use db::review::StatementAccounts;
use leptos::{prelude::*, reactive::computed::ScopedFuture};
use sqlx::SqlitePool;
use web::{
    model::{JournalQuery, Queue},
    review::load_queue,
};

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

pub async fn queue(pool: &SqlitePool, query: JournalQuery) -> Queue {
    call(pool, || load_queue(query)).await.unwrap()
}
