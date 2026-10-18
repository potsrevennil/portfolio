//! Reading rendered pages: the text a reader sees, the router serving them,
//! and server functions called as the router calls them. Not every test
//! binary uses every item.
#![allow(dead_code)]

use std::future::Future;

use axum::{body::Body, http::Request};
use db::review::StatementAccounts;
use http_body_util::BodyExt;
use ledger::valuation::AtCost;
use leptos::{
    prelude::{provide_context, LeptosOptions, Owner},
    reactive::computed::ScopedFuture,
};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;
use web::server;

/// The text a reader sees: markup and scripts removed.
pub fn visible(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = match rest.starts_with("<script") {
            true => rest.find("</script>").map_or(rest.len(), |i| i + "</script>".len()),
            false => rest.find('>').map_or(rest.len(), |i| i + 1),
        };
        out.push(' ');
        rest = &rest[end..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `nth` table body's rows, each as a reader sees it.
pub fn table(html: &str, nth: usize) -> Vec<String> {
    let body = html.split("<tbody>").nth(nth + 1).expect("the table");
    let body = &body[..body.find("</tbody>").unwrap()];
    body.split("<tr").skip(1).map(|row| visible(&format!("<tr{row}"))).collect()
}

/// Calls a server function as the router would, with the ledger in context.
pub async fn call<F: Future>(pool: &SqlitePool, function: impl FnOnce() -> F) -> F::Output {
    call_with(pool, StatementAccounts::default(), function).await
}

/// [`call`], naming the accounts that have statements.
pub async fn call_with<F: Future>(
    pool: &SqlitePool,
    statements: StatementAccounts,
    function: impl FnOnce() -> F,
) -> F::Output {
    let owner = Owner::new();
    let call = owner.with(|| {
        provide_context(pool.clone());
        provide_context(AtCost::default());
        provide_context(statements);
        ScopedFuture::new(function())
    });
    call.await
}

pub fn position(text: &str, needle: &str) -> usize {
    text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in:\n{text}"))
}

/// Only one site serves at a time. Leptos keeps process-wide state, and
/// pages rendered at once on separate test runtimes can stall for good; the
/// server runs one runtime, so it never does. The global at fault is not yet
/// pinned down (separate reactive arenas did not help), so a second runtime in
/// the app, e.g. for a batch job, would need the same care.
static SERVING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A router over a fixture ledger. Tests reach pages only through it, so
/// every one takes the lock without having to know about it.
pub struct Site {
    router: axum::Router,
    _serving: tokio::sync::MutexGuard<'static, ()>,
    _dir: TempDir,
}

impl Site {
    /// Takes the lock before `build` touches Leptos.
    pub async fn new<F>(build: impl FnOnce() -> F) -> Self
    where
        F: Future<Output = (axum::Router, TempDir)>,
    {
        let serving = SERVING.lock().await;
        let (router, dir) = build().await;
        Site { router, _serving: serving, _dir: dir }
    }

    /// The app's router over the ledger `ledger` builds.
    pub async fn serving<F>(ledger: impl FnOnce() -> F) -> Self
    where
        F: Future<Output = (TempDir, SqlitePool)>,
    {
        Site::serving_with(ledger, StatementAccounts::default()).await
    }

    /// [`Site::serving`], naming the accounts that have statements.
    pub async fn serving_with<F>(ledger: impl FnOnce() -> F, statements: StatementAccounts) -> Self
    where
        F: Future<Output = (TempDir, SqlitePool)>,
    {
        Site::new(|| async {
            let (dir, pool) = ledger().await;
            let options = LeptosOptions::builder().output_name("web").build();
            (server::router(options, pool, AtCost::default(), statements), dir)
        })
        .await
    }

    pub async fn page(&self, path: &str) -> String {
        let request = Request::get(path).body(Body::empty()).unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        assert!(response.status().is_success(), "{path}: {}", response.status());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(body.to_vec()).unwrap()
    }
}
