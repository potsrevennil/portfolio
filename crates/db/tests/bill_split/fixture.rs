//! A small synthetic ledger for the engine. All names and figures are invented.
//! Not every test binary uses every item.
#![allow(dead_code)]

use std::collections::BTreeMap;

use chrono::NaiveDate;
use db::{
    assertions::{self, AssertionSource, BalanceAssertion},
    bill_split::{self, Attach, Books, Repayment, Shape, WriteOff},
    query::account_balances,
    review::{Edit, LegEdit},
    SqlitePool,
};
use ledger::accounts::Chart;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use tempfile::TempDir;

pub const BANK: &str = "Assets:Bank";
pub const CASH: &str = "Assets:Cash";
pub const ALICE: &str = "Assets:Split:Alice";
pub const ALICE_GEAR: &str = "Assets:Split:Gear:Alice";
pub const BOB: &str = "Assets:Split:Bob";
pub const CREW: &str = "Assets:Split:Crew";
pub const STRANGER: &str = "Assets:Split:Stranger";
pub const FOOD: &str = "Expenses:Food";
pub const FEES: &str = "Expenses:Fees";
pub const OPENING: &str = "Equity:Opening-Balances";
pub const UNCAT_OUT: &str = "Expenses:Uncategorized";
pub const UNCAT_IN: &str = "Income:Uncategorized";

const MAPPING: &str = r#"
[fallback]
income = "Income:Uncategorized"
expense = "Expenses:Uncategorized"

[fallback.descriptions]
"手續費" = "Expenses:Fees"

[split_accounts]
roots = ["Assets:Split"]
"#;

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

/// A repayment moved onto `account`, as much as it can pay.
pub fn to(account: &str) -> Attach {
    Attach::Repayment(Repayment { account: Some(account.into()), amount: None })
}

/// A TWD write-off on the fixture's day for them.
pub fn off(account: &str, amount: Option<Decimal>) -> WriteOff {
    WriteOff {
        account: account.into(),
        currency: Currency::TWD,
        date: d(2026, 6, 1),
        amount,
        note: Some("討無".into()),
    }
}

/// A TWD leg as the editor submits it.
pub fn leg(posting_id: Option<i64>, account: &str, amount: Decimal) -> LegEdit {
    LegEdit { posting_id, account: account.to_string(), amount, currency: Currency::TWD }
}

pub struct Ledger {
    _dir: TempDir,
    pub pool: SqlitePool,
    pub books: Books,
    pub alice: i64,
    pub bob: i64,
}

impl Ledger {
    /// A ledger with the accounts above; Alice owns her two, Bob his one.
    pub async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let url = format!("sqlite:{}", dir.path().join("split.db").display());
        let pool = db::init_db(&url).await.unwrap();
        for (path, ty) in [
            (BANK, "asset"),
            (CASH, "asset"),
            ("Assets:Split", "asset"),
            (ALICE, "asset"),
            (BOB, "asset"),
            (STRANGER, "asset"),
            (FOOD, "expense"),
            (FEES, "expense"),
            (UNCAT_OUT, "expense"),
            (UNCAT_IN, "income"),
            (OPENING, "equity"),
        ] {
            sqlx::query("INSERT INTO accounts (path, label, type) VALUES (?, ?, ?)")
                .bind(path)
                .bind(path)
                .bind(ty)
                .execute(&pool)
                .await
                .unwrap();
        }
        let chart: Chart = toml::from_str(MAPPING).unwrap();
        let books = Books::from(&chart);
        let alice = bill_split::add_person(&pool, "Alice").await.unwrap();
        let bob = bill_split::add_person(&pool, "Bob").await.unwrap();
        bill_split::assign(&pool, &books, alice, ALICE).await.unwrap();
        bill_split::assign(&pool, &books, alice, ALICE_GEAR).await.unwrap();
        bill_split::assign(&pool, &books, bob, BOB).await.unwrap();
        Ledger { _dir: dir, pool, books, alice, bob }
    }

    /// An imported line with these legs; its id.
    pub async fn line(&self, date: &str, legs: &[(&str, &str)]) -> i64 {
        self.line_in(date, legs, "TWD").await
    }

    pub async fn line_in(&self, date: &str, legs: &[(&str, &str)], currency: &str) -> i64 {
        let id = sqlx::query(
            "INSERT INTO transactions (date, source, reviewed) VALUES (?, 'import', 1)",
        )
        .bind(date)
        .execute(&self.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        for (path, amount) in legs {
            sqlx::query(
                "INSERT INTO postings (transaction_id, account_id, amount, currency)
                 SELECT ?, id, ?, ? FROM accounts WHERE path = ?",
            )
            .bind(id)
            .bind(amount)
            .bind(currency)
            .bind(path)
            .execute(&self.pool)
            .await
            .unwrap();
        }
        id
    }

    pub async fn per_person(&self, title: &str) -> i64 {
        bill_split::create(&self.pool, &self.books, title, &Shape::PerPerson).await.unwrap()
    }

    pub async fn netted(&self, title: &str, account: &str) -> i64 {
        let shape = Shape::Netted { account: account.into() };
        bill_split::create(&self.pool, &self.books, title, &shape).await.unwrap()
    }

    /// A transaction's posting ids, in posting order.
    pub async fn posting_ids(&self, transaction: i64) -> Vec<i64> {
        sqlx::query_scalar("SELECT id FROM postings WHERE transaction_id = ? ORDER BY id")
            .bind(transaction)
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }

    /// The edit rewriting a line's postings to `legs` in order, adding any past
    /// them. Unconfirmed, and without a note.
    pub async fn rewrite(&self, line: i64, legs: &[(&str, Decimal)]) -> Edit {
        let held = self.posting_ids(line).await;
        let legs = legs
            .iter()
            .enumerate()
            .map(|(i, (account, amount))| leg(held.get(i).copied(), account, *amount))
            .collect();
        Edit { narration: None, legs, confirm: false }
    }

    /// A transaction's legs as `(account, amount)`, in posting order.
    pub async fn legs(&self, transaction: i64) -> Vec<(String, Decimal)> {
        let mut conn = self.pool.acquire().await.unwrap();
        let legs = db::events::snapshot(&mut conn, transaction).await.unwrap().legs;
        legs.into_iter().map(|l| (l.account, l.amount)).collect()
    }

    /// Every account's balance.
    pub async fn balances(&self) -> BTreeMap<(String, Currency), Decimal> {
        account_balances(&self.pool, d(2100, 1, 1))
            .await
            .unwrap()
            .into_iter()
            .map(|b| ((b.path, b.currency), b.amount))
            .collect()
    }

    pub async fn balance(&self, path: &str) -> Decimal {
        self.balances().await.get(&(path.to_string(), Currency::TWD)).copied().unwrap_or_default()
    }

    pub async fn events(&self, transaction: i64) -> Vec<String> {
        db::events::history(&self.pool, transaction)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.kind.to_string())
            .collect()
    }

    pub async fn reviewed(&self, transaction: i64) -> bool {
        db::events::snapshot(&mut *self.pool.acquire().await.unwrap(), transaction)
            .await
            .unwrap()
            .reviewed
    }

    /// A statement closing on `account`, so a write past it trips the gate.
    pub async fn statement_closes(&self, account: &str, date: NaiveDate, closing: Decimal) {
        let assertion = BalanceAssertion {
            source: AssertionSource::Statement,
            account: account.into(),
            currency: Currency::TWD,
            period_start: None,
            opening: None,
            period_end: date,
            closing,
        };
        let mut db = self.pool.begin().await.unwrap();
        assertions::insert(&mut db, &assertion).await.unwrap();
        db.commit().await.unwrap();
    }
}

/// Expected legs, in the shape [`Ledger::legs`] returns.
pub fn dec_map(pairs: &[(&str, Decimal)]) -> Vec<(String, Decimal)> {
    pairs.iter().map(|(p, a)| (p.to_string(), *a)).collect()
}
