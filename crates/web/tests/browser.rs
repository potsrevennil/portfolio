//! Journeys through the review pages in a real, headless Chromium: the
//! server over the synthetic queue ledger, the hydrated wasm client, and a
//! person clicking. The rendered-HTML tests prove the markup; these prove it
//! works once the script has loaded.
//!
//! Gated: they run only with `WEB_BROWSER_TESTS=1`, and need
//! - the client built: `cargo leptos build` puts it under `target/site`
//!   (`LEPTOS_SITE_ROOT` names another);
//! - `chromedriver` on PATH, or at `CHROMEDRIVER`, matching the browser;
//! - the browser at `CHROME`, if chromedriver would not find it itself.
//!
//! ```text
//! cargo leptos build
//! WEB_BROWSER_TESTS=1 cargo test -p web --test browser
//! ```

#[path = "common/queue.rs"]
mod queue;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use db::pairing::{self, Ambiguity};
use fantoccini::{elements::Element, Client, ClientBuilder, Locator};
use hyper_util::client::legacy::connect::HttpConnector;
use ledger::valuation::AtCost;
use ledger_types::currency::Currency;
use leptos::prelude::LeptosOptions;
use queue::{ledger, COFFEE, LUNCH, TAXI};
use rust_decimal_macros::dec;
use serde_json::json;
use sqlx::SqlitePool;
use tempfile::TempDir;
use web::server;

fn enabled() -> bool {
    match std::env::var("WEB_BROWSER_TESTS").as_deref() {
        Ok("1") => true,
        _ => {
            eprintln!("skipped: set WEB_BROWSER_TESTS=1 to drive a browser");
            false
        }
    }
}

/// The client as `cargo leptos build` left it, under the plain names the
/// server's options give it: the build may have hashed them.
fn site() -> TempDir {
    let root = std::env::var_os("LEPTOS_SITE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/site"));
    let pkg = root.join("pkg");
    let entries = std::fs::read_dir(&pkg)
        .unwrap_or_else(|e| panic!("{}: {e}; run `cargo leptos build` first", pkg.display()));
    let site = TempDir::new().unwrap();
    std::fs::create_dir(site.path().join("pkg")).unwrap();
    for entry in entries {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let plain = match path.extension().and_then(|e| e.to_str()) {
            _ if !name.starts_with("web") => continue,
            Some("js") => "web.js",
            Some("wasm") => "web_bg.wasm",
            Some("css") => "web.css",
            _ => continue,
        };
        std::fs::copy(&path, site.path().join("pkg").join(plain)).unwrap();
    }
    site
}

/// chromedriver on a free port, killed when dropped.
struct Driver {
    child: Child,
    port: u16,
}

impl Driver {
    fn start() -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let binary = std::env::var("CHROMEDRIVER").unwrap_or_else(|_| "chromedriver".into());
        let child = Command::new(&binary)
            .arg(format!("--port={port}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("{binary}: {e}; set CHROMEDRIVER"));
        Driver { child, port }
    }
}

impl Drop for Driver {
    fn drop(&mut self) { let _ = self.child.kill(); }
}

/// A fresh ledger served on a free port, and a browser pointed at it.
struct Journey {
    browser: Client,
    base: String,
    pool: SqlitePool,
    _dirs: (TempDir, TempDir),
    _driver: Driver,
}

impl Journey {
    async fn start() -> Self {
        let (dir, pool) = ledger().await;
        let site = site();
        let options = LeptosOptions::builder()
            .output_name("web")
            .site_root(site.path().to_string_lossy().to_string())
            .site_pkg_dir("pkg")
            .build();
        let router = server::router(options, pool.clone(), AtCost::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router.into_make_service()).await });

        let driver = Driver::start();
        let mut chrome = json!({
            "args": ["--headless=new", "--no-sandbox", "--disable-gpu", "--window-size=1280,900"],
        });
        if let Ok(binary) = std::env::var("CHROME") {
            chrome["binary"] = json!(binary);
        }
        let mut caps = serde_json::Map::new();
        caps.insert("goog:chromeOptions".into(), chrome);
        let url = format!("http://127.0.0.1:{}", driver.port);
        let deadline = Instant::now() + Duration::from_secs(20);
        let browser = loop {
            match ClientBuilder::new(HttpConnector::new())
                .capabilities(caps.clone())
                .connect(&url)
                .await
            {
                Ok(browser) => break browser,
                Err(e) if Instant::now() > deadline => panic!("chromedriver at {url}: {e}"),
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        };
        Journey {
            browser,
            base: format!("http://{addr}"),
            pool,
            _dirs: (dir, site),
            _driver: driver,
        }
    }

    /// Opens a page and waits until the client has taken it over.
    async fn open(&self, path: &str) {
        self.browser.goto(&format!("{}{path}", self.base)).await.unwrap();
        self.hydrated().await;
    }

    async fn hydrated(&self) {
        self.browser
            .wait()
            .at_most(Duration::from_secs(20))
            .for_element(Locator::Css("body[data-hydrated]"))
            .await
            .expect("the page never hydrated: is the client built?");
    }

    async fn find(&self, css: &str) -> Element {
        self.browser
            .wait()
            .at_most(Duration::from_secs(10))
            .for_element(Locator::Css(css))
            .await
            .unwrap_or_else(|e| panic!("{css}: {e}"))
    }

    async fn all(&self, css: &str) -> Vec<Element> {
        self.browser.find_all(Locator::Css(css)).await.unwrap()
    }

    async fn text(&self) -> String { self.find("main").await.text().await.unwrap() }

    /// Waits until the page says `needle`.
    async fn until_text(&self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let text = self.text().await;
            if text.contains(needle) {
                return text;
            }
            if Instant::now() > deadline {
                panic!("{needle:?} never shown:\n{text}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Waits until `css` matches exactly `n` elements.
    async fn until_count(&self, css: &str, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let found = self.all(css).await.len();
            if found == n {
                return;
            }
            if Instant::now() > deadline {
                panic!("{css}: {found} shown, not {n}\n{}", self.text().await);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The queue row whose text holds `needle`.
    async fn row(&self, needle: &str) -> Element {
        for row in self.all("li.entry").await {
            if row.text().await.unwrap().contains(needle) {
                return row;
            }
        }
        panic!("no row with {needle:?}:\n{}", self.text().await);
    }

    async fn fill(&self, css: &str, value: &str) {
        let field = self.find(css).await;
        field.clear().await.unwrap();
        field.send_keys(value).await.unwrap();
    }

    async fn close(self) { self.browser.close().await.unwrap(); }
}

async fn reviewed(pool: &SqlitePool, narration: &str) -> bool {
    sqlx::query_scalar("SELECT reviewed != 0 FROM transactions WHERE narration = ?")
        .bind(narration)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Each journey on its own ledger, one after another: Leptos keeps
/// process-wide state that two servers rendering at once can trip over.
#[tokio::test(flavor = "multi_thread")]
async fn journeys() {
    if !enabled() {
        return;
    }
    confirm_from_the_queue().await;
    split_in_the_editor().await;
    pick_a_pairing().await;
    enter_cash_by_hand().await;
    close_and_reopen_an_account().await;
}

async fn confirm_from_the_queue() {
    let j = Journey::start().await;
    j.open("/review").await;
    j.until_count("li.entry", 4).await;
    assert_eq!(j.find(".count-badge").await.text().await.unwrap(), "4");

    let lunch = j.row(LUNCH).await;
    lunch.find(Locator::Css("button")).await.unwrap().click().await.unwrap();
    j.until_count("li.entry", 3).await;
    // The count beside the link follows without a reload.
    assert_eq!(j.find(".count-badge").await.text().await.unwrap(), "3");
    assert!(reviewed(&j.pool, LUNCH).await);
    j.close().await;
}

async fn split_in_the_editor() {
    let j = Journey::start().await;
    j.open("/review").await;
    j.row(TAXI).await.find(Locator::LinkText("修改")).await.unwrap().click().await.unwrap();
    j.hydrated().await;
    j.find(r#"select[name="legs[2][account]"]"#)
        .await
        .select_by_value("Assets:Split:Alpha:Tab")
        .await
        .unwrap();
    j.fill(r#"input[name="legs[2][amount]"]"#, "100").await;
    // The category keeps all 300: off by 100.
    j.find(r#"button[type="submit"]"#).await.click().await.unwrap();
    j.until_text("借貸不平衡").await;
    assert!(!reviewed(&j.pool, TAXI).await);

    j.fill(r#"input[name="legs[0][amount]"]"#, "200").await;
    j.find(r#"button[type="submit"]"#).await.click().await.unwrap();
    // Saved and confirmed: back to the queue, without the taxi.
    j.until_count("li.entry", 3).await;
    assert!(j.browser.current_url().await.unwrap().path().ends_with("/review"));
    assert!(reviewed(&j.pool, TAXI).await);
    let legs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM postings WHERE transaction_id = (SELECT id FROM transactions WHERE \
         narration = ?)",
    )
    .bind(TAXI)
    .fetch_one(&j.pool)
    .await
    .unwrap();
    assert_eq!(legs, 3);
    j.close().await;
}

async fn pick_a_pairing() {
    let j = Journey::start().await;
    let ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM transactions WHERE narration IN (?, ?) ORDER BY id")
            .bind(LUNCH)
            .bind(COFFEE)
            .fetch_all(&j.pool)
            .await
            .unwrap();
    let mut conn = j.pool.acquire().await.unwrap();
    pairing::record(&mut conn, &[Ambiguity {
        account: "Assets:Bank:Savings".into(),
        currency: Currency::TWD,
        statement_ref: "bank:line:1".into(),
        date: "2024-03-02".parse().unwrap(),
        amount: dec!(-150),
        description: "範例店".into(),
        candidates: ids.clone(),
    }])
    .await
    .unwrap();
    drop(conn);

    j.open("/review").await;
    j.until_text("待配對").await;
    j.find(&format!(r#"input[name="record"][value="{}"]"#, ids[1])).await.click().await.unwrap();
    j.find(".pairings button").await.click().await.unwrap();
    j.until_text("已選，下次匯入時對帳").await;
    let chosen: Option<i64> = sqlx::query_scalar("SELECT transaction_id FROM verification_choice")
        .fetch_one(&j.pool)
        .await
        .unwrap();
    assert_eq!(chosen, Some(ids[1]));
    j.close().await;
}

async fn enter_cash_by_hand() {
    let j = Journey::start().await;
    j.open("/entry").await;
    // 轉帳 asks where the money goes instead of a category, and back.
    let kinds = j.all(".kinds input").await;
    kinds[2].click().await.unwrap();
    j.find(r#"select[name="counter"]"#).await;
    j.until_count(r#"select[name="category"]"#, 0).await;
    kinds[0].click().await.unwrap();
    j.fill(r#"input[name="amount"]"#, "85").await;
    j.find(r#"select[name="account"]"#).await.select_by_value("Assets:Cash").await.unwrap();
    j.find(r#"select[name="category"]"#).await.select_by_value("Expenses:Food").await.unwrap();
    j.fill(r#"input[name="note"]"#, "豆漿").await;
    j.find(r#"button[type="submit"]"#).await.click().await.unwrap();
    j.until_text("已記下").await;
    let (source, reviewed): (String, bool) =
        sqlx::query_as("SELECT source, reviewed != 0 FROM transactions WHERE narration = '豆漿'")
            .fetch_one(&j.pool)
            .await
            .unwrap();
    assert_eq!((source.as_str(), reviewed), ("manual", true));

    // Found again in the journal, opened, and deleted.
    j.open("/journal").await;
    j.row("豆漿").await.find(Locator::LinkText("修改")).await.unwrap().click().await.unwrap();
    j.hydrated().await;
    j.find(r#"form.delete input[name="sure"]"#).await.click().await.unwrap();
    j.find("form.delete button").await.click().await.unwrap();
    j.until_count("li.entry", 10).await;
    assert!(j.browser.current_url().await.unwrap().path().ends_with("/journal"));
    assert!(!j.text().await.contains("豆漿"));
    let deleted: Option<String> =
        sqlx::query_scalar("SELECT deleted_at FROM transactions WHERE narration = '豆漿'")
            .fetch_one(&j.pool)
            .await
            .unwrap();
    assert!(deleted.is_some());
    j.close().await;
}

async fn close_and_reopen_an_account() {
    let j = Journey::start().await;
    j.open("/accounts").await;
    // Groups start folded; 全部展開 opens every one.
    let tab = r#".account-row:has(a[href="/journal?account=Assets%3ASplit%3AAlpha%3ATab"])"#;
    assert!(!j.find(tab).await.is_displayed().await.unwrap());
    j.find(".tools button").await.click().await.unwrap();
    assert!(j.find(tab).await.is_displayed().await.unwrap());

    let wallet = r#".account-row:has(a[href="/journal?account=Assets%3AOld-Wallet"])"#;
    j.find(&format!("{wallet} button")).await.click().await.unwrap();
    // Closed, it stays in the list, marked.
    j.until_count(&format!("{wallet}.closed"), 1).await;
    let row = j.find(wallet).await;
    assert!(row.text().await.unwrap().contains("已結清"));
    row.find(Locator::Css("button")).await.unwrap().click().await.unwrap();
    j.until_count(&format!("{wallet}.closed"), 0).await;
    let closed: bool =
        sqlx::query_scalar("SELECT closed != 0 FROM accounts WHERE path = 'Assets:Old-Wallet'")
            .fetch_one(&j.pool)
            .await
            .unwrap();
    assert!(!closed);
    j.close().await;
}
