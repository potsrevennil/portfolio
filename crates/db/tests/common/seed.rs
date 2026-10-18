//! A hand-seeded ledger in two currencies whose every report figure is
//! worked out by hand where it is used. All figures are invented. Not every
//! test binary uses every item.
#![allow(dead_code)]

use chrono::NaiveDate;
use sqlx::SqlitePool;
use tempfile::TempDir;

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).expect("valid date")
}

/// A fresh, migrated, empty database in a temp dir (absolute path in the URL).
pub async fn fresh_db() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().expect("temp dir");
    let url = format!("sqlite:{}", dir.path().join("reporting.db").display());
    let pool = db::init_db(&url).await.expect("init db");
    (dir, pool)
}

pub async fn add_account(pool: &SqlitePool, path: &str, label: &str, ty: &str) -> i64 {
    sqlx::query("INSERT INTO accounts (path, label, type, closed) VALUES (?, ?, ?, 0)")
        .bind(path)
        .bind(label)
        .bind(ty)
        .execute(pool)
        .await
        .expect("insert account")
        .last_insert_rowid()
}

/// Inserts a transaction and its legs. `legs` are `(account_id, amount,
/// currency)`.
pub async fn add_txn(pool: &SqlitePool, date: &str, narration: &str, legs: &[(i64, &str, &str)]) {
    let txn_id = sqlx::query(
        "INSERT INTO transactions (date, narration, source, reviewed) VALUES (?, ?, 'manual', 0)",
    )
    .bind(date)
    .bind(narration)
    .execute(pool)
    .await
    .expect("insert transaction")
    .last_insert_rowid();
    for (account_id, amount, currency) in legs {
        sqlx::query(
            "INSERT INTO postings (transaction_id, account_id, amount, currency) VALUES (?, ?, ?, \
             ?)",
        )
        .bind(txn_id)
        .bind(account_id)
        .bind(amount)
        .bind(currency)
        .execute(pool)
        .await
        .expect("insert posting");
    }
}

pub async fn add_price(pool: &SqlitePool, symbol: &str, date: &str, close: f64) {
    sqlx::query("INSERT INTO stock_prices (symbol, date, close_price) VALUES (?, ?, ?)")
        .bind(symbol)
        .bind(date)
        .bind(close)
        .execute(pool)
        .await
        .expect("insert price");
}

/// Seeds a two-currency ledger (TWD savings + USD brokerage cash, a credit
/// card, income, expenses) plus an `Equity:Conversions` FX plug booked at a
/// rate (90) different from the reporting rate (30), and a `TWD=X` = 30 FX
/// quote. Base currency is TWD throughout.
pub async fn seed_multicurrency(pool: &SqlitePool) {
    let savings = add_account(pool, "Assets:Cathay:Savings", "Savings", "asset").await;
    let ib_cash = add_account(pool, "Assets:IB:Cash", "IB Cash", "asset").await;
    let card = add_account(pool, "Liabilities:CreditCard", "Credit Card", "liability").await;
    let salary = add_account(pool, "Income:Salary", "Salary", "income").await;
    let food = add_account(pool, "Expenses:Food", "Food", "expense").await;
    let conv = add_account(pool, "Equity:Conversions", "Conversions", "equity").await;
    let opening_eq =
        add_account(pool, "Equity:Opening-Balances", "Opening Balances", "equity").await;

    // Openings are transactions against Equity:Opening-Balances (T2c model).
    add_txn(pool, "2026-01-01", "opening", &[
        (savings, "100000", "TWD"),
        (opening_eq, "-100000", "TWD"),
    ])
    .await;
    add_txn(pool, "2026-01-01", "opening", &[
        (ib_cash, "1000", "USD"),
        (opening_eq, "-1000", "USD"),
    ])
    .await;

    // 1 USD = 30 TWD, quoted USD->TWD as the ticker `TWD=X`.
    add_price(pool, "TWD=X", "2026-01-01", 30.0).await;

    // Salary 50000 TWD into savings.
    add_txn(pool, "2026-01-05", "salary", &[(savings, "50000", "TWD"), (salary, "-50000", "TWD")])
        .await;
    // Food 300 TWD from savings.
    add_txn(pool, "2026-01-10", "lunch", &[(food, "300", "TWD"), (savings, "-300", "TWD")]).await;
    // Food 20 USD from the USD brokerage cash (multi-currency expense).
    add_txn(pool, "2026-02-15", "coffee abroad", &[(food, "20", "USD"), (ib_cash, "-20", "USD")])
        .await;
    // Credit-card spend 1000 TWD.
    add_txn(pool, "2026-02-20", "groceries", &[(food, "1000", "TWD"), (card, "-1000", "TWD")])
        .await;
    // Cross-currency move: 3000 TWD out of savings becomes 90 USD in IB cash.
    // The Equity:Conversions plug absorbs both nominal legs; booked at 33.3
    // TWD/USD it does NOT net to zero at the reporting rate of 30, so a report
    // that wrongly folded equity into net worth would visibly differ.
    add_txn(pool, "2026-02-25", "fx to usd", &[
        (savings, "-3000", "TWD"),
        (ib_cash, "90", "USD"),
        (conv, "3000", "TWD"),
        (conv, "-90", "USD"),
    ])
    .await;
}
