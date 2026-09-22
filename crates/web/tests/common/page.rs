//! Reading rendered pages: the text a reader sees, and the router serving them.

use std::future::Future;

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;

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

    pub async fn page(&self, path: &str) -> String {
        let request = Request::get(path).body(Body::empty()).unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        assert!(response.status().is_success(), "{path}: {}", response.status());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(body.to_vec()).unwrap()
    }
}
