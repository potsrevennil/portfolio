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

mod common;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use common::{
    coverage,
    queue::{ledger, LUNCH, TAXI},
};
use fantoccini::{elements::Element, key::Key, Client, ClientBuilder, Locator};
use hyper_util::client::legacy::connect::HttpConnector;
use leptos::prelude::LeptosOptions;
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
        Self::serve(dir, pool, Default::default()).await
    }

    async fn serve(dir: TempDir, pool: SqlitePool, config: server::Config) -> Self {
        let site = site();
        let options = LeptosOptions::builder()
            .output_name("web")
            .site_root(site.path().to_string_lossy().to_string())
            .site_pkg_dir("pkg")
            .build();
        let market = common::market::beside(dir.path()).await;
        let router = server::router(options, pool.clone(), market, config);
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
    /// Waits for the tab's name, which the client sets once it has the page.
    async fn until_title(&self, title: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.browser.title().await.unwrap() != title {
            assert!(Instant::now() < deadline, "{}", self.browser.title().await.unwrap());
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

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

    /// The 對帳進度 bar whose account is labelled `label`.
    async fn account(&self, label: &str) -> Element {
        for row in self.all(".account").await {
            // textContent, not the text shown: a hidden bar has none.
            let name = row.find(Locator::Css(".name")).await.unwrap();
            let name = name.prop("textContent").await.unwrap().unwrap_or_default();
            if name.starts_with(label) {
                return row;
            }
        }
        panic!("no account {label:?}:\n{}", self.text().await);
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
    enter_cash_by_hand().await;
    show_the_accounts_at_zero().await;
    explore_the_charts().await;
    open_an_account().await;
    set_a_statement_day_and_follow_the_unchecked().await;
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

    // Taken back from the editor, it is waiting again.
    let id: i64 = sqlx::query_scalar("SELECT id FROM transactions WHERE narration = ?")
        .bind(LUNCH)
        .fetch_one(&j.pool)
        .await
        .unwrap();
    j.open(&format!("/review/{id}")).await;
    j.find("form.unconfirm button").await.click().await.unwrap();
    j.until_count("form.unconfirm", 0).await;
    // The badge refetches after the form does.
    j.until_count(".count-badge", 1).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while j.find(".count-badge").await.text().await.unwrap() != "4" {
        assert!(Instant::now() < deadline, "the count never went back to 4");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!reviewed(&j.pool, LUNCH).await);
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

async fn show_the_accounts_at_zero() {
    let j = Journey::start().await;
    j.open("/balance-sheet").await;
    j.find(".tools button").await.click().await.unwrap();
    // The emptied wallet is left out until it is asked for.
    let wallet = r#".row:has(a[href="/account?account=Assets%3AOld-Wallet"])"#;
    assert!(!j.find(wallet).await.is_displayed().await.unwrap());
    j.find(".tools input[type=checkbox]").await.click().await.unwrap();
    assert!(j.find(wallet).await.is_displayed().await.unwrap());
    j.close().await;
}

async fn explore_the_charts() {
    let journey = Journey::start().await;
    let j = &journey;
    j.open("/").await;
    let period = || async { j.find(".chart.line .readout .period").await.text().await.unwrap() };
    // What 收支分類 covers, once it reads `want`.
    let covers = |want: &'static str| async move {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let now = j.find("#breakdown .subtitle").await.text().await.unwrap();
            if now == want {
                break;
            }
            assert!(Instant::now() < deadline, "covers {now:?}, not {want:?}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    // Until the pointer comes, the readout gives the newest figures.
    assert_eq!(period().await, "至今");
    // Pressing the plot reads the month under it; the keys step through.
    j.find(".chart.line .area").await.click().await.unwrap();
    j.find(".chart.line .crosshair").await;
    let area = j.find(".chart.line .area").await;
    area.send_keys(&char::from(Key::Home).to_string()).await.unwrap();
    assert_eq!(period().await, "2024-01");
    area.send_keys(&char::from(Key::Right).to_string()).await.unwrap();
    assert_eq!(period().await, "2024-02");

    // Pressing a month of 收支 breaks that month down below.
    j.find(".chart.column a.band").await.click().await.unwrap();
    covers("2024 年 1 月").await;
    assert!(j.browser.current_url().await.unwrap().as_str().contains("period=2024-01-01"));
    // A year's chip covers the year; back returns to the month, unbroken.
    j.browser.find(Locator::LinkText("2024")).await.unwrap().click().await.unwrap();
    covers("2024 年").await;
    j.browser.back().await.unwrap();
    covers("2024 年 1 月").await;
    j.browser.back().await.unwrap();
    covers("2024 年 3 月").await;
    assert!(!j.text().await.contains("讀取失敗"));
    // The keys pick a month of 收支 too: Home, then Enter.
    j.browser
        .execute("document.querySelector('.chart.column .area').focus()", vec![])
        .await
        .unwrap();
    let column = j.find(".chart.column .area").await;
    column.send_keys(&char::from(Key::Home).to_string()).await.unwrap();
    column.send_keys(&char::from(Key::Enter).to_string()).await.unwrap();
    covers("2024 年 1 月").await;
    j.browser.back().await.unwrap();
    covers("2024 年 3 月").await;
    // 全部 and back, to a page that asked for nothing at all.
    j.browser.find(Locator::LinkText("全部")).await.unwrap().click().await.unwrap();
    covers("全部期間").await;
    j.browser.back().await.unwrap();
    covers("2024 年 3 月").await;
    assert!(!j.text().await.contains("讀取失敗"));

    // A phone's width scrolls the page down, never sideways.
    j.browser.set_window_size(375, 812).await.unwrap();
    let wide = j
        .browser
        .execute(
            "return document.documentElement.scrollWidth > document.documentElement.clientWidth",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(wide, json!(false));
    journey.close().await;
}

async fn open_an_account() {
    let j = Journey::start().await;
    j.open("/balance-sheet").await;
    j.find(".tools button").await.click().await.unwrap();
    j.browser.find(Locator::LinkText("活存")).await.unwrap().click().await.unwrap();
    j.until_text("餘額走勢").await;
    j.hydrated().await;
    assert_eq!(j.find(".account-head h1").await.text().await.unwrap(), "活存");
    j.until_title("活存 · 帳簿").await;
    j.until_text("對帳").await;
    // The trail leads up to the account above, and Back returns.
    j.find(r#".trail a[href="/account?account=Assets%3ABank"]"#).await.click().await.unwrap();
    j.until_title("銀行 · 帳簿").await;
    assert_eq!(j.find(".account-head h1").await.text().await.unwrap(), "銀行");
    j.browser.back().await.unwrap();
    j.until_title("活存 · 帳簿").await;
    // Its journal is one link away, filtered to it.
    j.browser.find(Locator::LinkText("去日記帳篩選")).await.unwrap().click().await.unwrap();
    j.hydrated().await;
    j.until_text("共 ").await;
    let url = j.browser.current_url().await.unwrap();
    assert_eq!(url.path(), "/journal");
    let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    assert_eq!(query, [("account".to_string(), "Assets:Bank:Savings".to_string())]);
    j.close().await;
}

/// From the nav to 對帳進度: give an account a statement day, then follow a
/// bar's unchecked records to the journal showing exactly those.
async fn set_a_statement_day_and_follow_the_unchecked() {
    let l = coverage::ledger().await;
    let j = Journey::serve(l.dir, l.pool, Default::default()).await;
    j.open("/").await;
    j.find(r#"header a[href="/coverage"]"#).await.click().await.unwrap();
    j.until_text("對過數單").await;
    // The emptied wallet stays out of sight until asked for.
    let wallet = j.account("舊錢包").await;
    assert!(!wallet.is_displayed().await.unwrap());
    j.find(".coverage .tools input[type=checkbox]").await.click().await.unwrap();
    assert!(wallet.is_displayed().await.unwrap());

    let fx = j.account("外幣").await;
    assert!(fx.text().await.unwrap().contains("無固定數單"));
    fx.find(Locator::XPath(".//button[text()='設定']")).await.unwrap().click().await.unwrap();
    let form = j.find(".settings form").await;
    form.find(Locator::Css("select[name=kind]"))
        .await
        .unwrap()
        .select_by_value("yearly")
        .await
        .unwrap();
    // Month and day appear for a yearly statement, at the year's end.
    j.find(".settings select[name=month]").await;
    form.find(Locator::Css("button[type=submit]")).await.unwrap().click().await.unwrap();
    j.until_text("逐年數單").await;

    let savings = j.account("活存").await;
    savings.find(Locator::Css("a.tail")).await.unwrap().click().await.unwrap();
    j.until_count("li.entry", 1).await;
    let url = j.browser.current_url().await.unwrap();
    assert!(url.as_str().contains("unverified=1"), "{url}");
    assert!(j.find("li.entry").await.text().await.unwrap().contains("2024-03-05"));
    j.close().await;
}
