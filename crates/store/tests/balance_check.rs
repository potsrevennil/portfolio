//! The invariant gate (`store::check`) and the `balance_assertion` table, over
//! small hand-built ledgers. All figures are invented.

use chrono::NaiveDate;
use portfolio::{
    currency::Currency,
    db,
    store::{
        assertions::{self, AssertionSource, BalanceAssertion},
        check::{self, Unchecked},
    },
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::{SqliteConnection, SqlitePool};
use tempfile::TempDir;

fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

/// Savings (TWD + USD) with a child pocket, funded from income:
/// 03-01 +1000 TWD, 03-15 +200 TWD to the pocket, 04-01 -300 TWD, 04-01 +5 USD.
async fn ledger() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().unwrap();
    let url = format!("sqlite:{}", dir.path().join("check.db").display());
    let pool = db::init_db(&url).await.unwrap();
    for (path, ty) in [
        ("Assets:Bank", "asset"),
        ("Assets:Bank:Pocket", "asset"),
        ("Assets:Wallet", "asset"),
        ("Income:Salary", "income"),
    ] {
        sqlx::query("INSERT INTO accounts (path, label, type) VALUES (?, ?, ?)")
            .bind(path)
            .bind(path)
            .bind(ty)
            .execute(&pool)
            .await
            .unwrap();
    }
    let conn = &mut *pool.acquire().await.unwrap();
    post(conn, "2024-03-01", "Assets:Bank", "1000", "TWD").await;
    post(conn, "2024-03-15", "Assets:Bank:Pocket", "200", "TWD").await;
    post(conn, "2024-04-01", "Assets:Bank", "-300", "TWD").await;
    post(conn, "2024-04-01", "Assets:Bank", "5", "USD").await;
    (dir, pool)
}

/// A two-leg transaction between `account` and Income:Salary.
async fn post(conn: &mut SqliteConnection, date: &str, account: &str, amount: &str, ccy: &str) {
    let txn = sqlx::query("INSERT INTO transactions (date, source) VALUES (?, 'manual')")
        .bind(date)
        .execute(&mut *conn)
        .await
        .unwrap()
        .last_insert_rowid();
    let negated = format!("{}", -amount.parse::<Decimal>().unwrap());
    for (path, amount) in [(account, amount), ("Income:Salary", negated.as_str())] {
        sqlx::query(
            "INSERT INTO postings (transaction_id, account_id, amount, currency) SELECT ?, id, ?, \
             ? FROM accounts WHERE path = ?",
        )
        .bind(txn)
        .bind(amount)
        .bind(ccy)
        .bind(path)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
}

fn statement(opening: Decimal, closing: Decimal) -> BalanceAssertion {
    BalanceAssertion {
        source: AssertionSource::Statement,
        account: "Assets:Bank".into(),
        currency: Currency::TWD,
        period_start: Some(d(2024, 3, 10)),
        opening: Some(opening),
        period_end: d(2024, 4, 1),
        closing,
    }
}

async fn check_with(pool: &SqlitePool, a: &[BalanceAssertion]) -> check::CheckReport {
    let conn = &mut *pool.acquire().await.unwrap();
    for a in a {
        assertions::insert(conn, a).await.unwrap();
    }
    check::check(conn).await.unwrap()
}

#[tokio::test]
async fn matching_figures_pass_and_cover_the_subtree() {
    let (_dir, pool) = ledger().await;
    // Opening = end of 03-09 (1000); closing = end of 04-01, pocket included.
    let report = check_with(&pool, &[statement(dec!(1000), dec!(900))]).await;
    assert!(report.ok(), "{report}");
    assert_eq!(report.figures(), 2, "a statement period states an opening and a closing");
    assert_eq!(report.accounts["Assets:Bank"].vouched_through, Some(d(2024, 4, 1)));
}

#[tokio::test]
async fn a_wrong_closing_fails_with_the_difference() {
    let (_dir, pool) = ledger().await;
    let report = check_with(&pool, &[statement(dec!(1000), dec!(700))]).await;
    assert!(!report.ok());
    let [m] = report.failures() else { panic!("one mismatch expected:\n{report}") };
    assert_eq!((m.as_of, m.expected, m.computed), (d(2024, 4, 1), dec!(700), dec!(900)));
    assert!(report.to_string().contains("off by 200"), "{report}");
    assert_eq!(report.accounts["Assets:Bank"].failed, 1);
}

#[tokio::test]
async fn a_wrong_opening_fails_on_the_day_before_the_period() {
    let (_dir, pool) = ledger().await;
    let report = check_with(&pool, &[statement(dec!(0), dec!(900))]).await;
    let [m] = report.failures() else { panic!("one mismatch expected:\n{report}") };
    assert_eq!((m.as_of, m.computed), (d(2024, 3, 9), dec!(1000)));
}

#[tokio::test]
async fn each_currency_is_checked_on_its_own() {
    let (_dir, pool) = ledger().await;
    let usd = |closing| BalanceAssertion {
        source: AssertionSource::Counted,
        account: "Assets:Bank".into(),
        currency: Currency::USD,
        period_start: None,
        opening: None,
        period_end: d(2024, 4, 1),
        closing,
    };
    assert!(check_with(&pool, &[usd(dec!(5))]).await.ok());
    let (_dir, pool) = ledger().await;
    assert!(!check_with(&pool, &[usd(dec!(900))]).await.ok(), "TWD must not satisfy a USD figure");
}

/// A ledger nothing vouches for is a failure, not a green check: otherwise an
/// empty assertions file would wave the whole history (or an import) through.
#[tokio::test]
async fn a_ledger_with_no_assertions_at_all_fails_the_gate() {
    let (_dir, pool) = ledger().await;
    let report = check_with(&pool, &[]).await;
    assert!(!report.ok(), "{report}");
    assert!(report.failures().is_empty(), "there is nothing to mismatch against");
    assert!(report.to_string().contains("nothing vouches"), "{report}");

    let err = check::gate(&mut *pool.acquire().await.unwrap())
        .await
        .expect_err("the import gate must refuse a ledger with no assertions");
    assert!(format!("{err:#}").contains("nothing vouches"), "{err:#}");
}

#[tokio::test]
async fn balances_nothing_vouches_for_are_listed_per_currency() {
    let (_dir, pool) = ledger().await;
    let report = check_with(&pool, &[]).await;
    assert_eq!(
        report.unchecked.iter().map(|u| (u.account.as_str(), u.currency)).collect::<Vec<_>>(),
        [
            ("Assets:Bank", Currency::TWD),
            ("Assets:Bank", Currency::USD),
            ("Assets:Bank:Pocket", Currency::TWD)
        ]
    );
    assert!(report.to_string().contains("never vouched for"), "{report}");

    // A TWD statement covers the subtree's TWD and nothing else: the USD the
    // same account holds is still unvouched for.
    let (_dir, pool) = ledger().await;
    let report = check_with(&pool, &[statement(dec!(1000), dec!(900))]).await;
    assert_eq!(
        report.unchecked,
        [Unchecked {
            account: "Assets:Bank".into(),
            currency: Currency::USD,
            vouched_through: None,
            posted_through: d(2024, 4, 1)
        }],
        "{report}"
    );
}

/// The import gate: an uncommitted write that breaks a figure fails `gate`
/// inside the writer's transaction, which then rolls back.
#[tokio::test]
async fn the_gate_sees_uncommitted_writes_and_nothing_commits() {
    let (_dir, pool) = ledger().await;
    assertions::insert(&mut *pool.acquire().await.unwrap(), &statement(dec!(1000), dec!(900)))
        .await
        .unwrap();

    let mut tx = pool.begin().await.unwrap();
    post(&mut tx, "2024-03-20", "Assets:Bank", "1", "TWD").await;
    let err = check::gate(&mut tx).await.expect_err("a duplicated line must fail the gate");
    assert!(format!("{err:#}").contains("MISMATCH Assets:Bank TWD"), "{err:#}");
    tx.rollback().await.unwrap();

    let report = check::check(&mut *pool.acquire().await.unwrap()).await.unwrap();
    assert!(report.ok(), "{report}");
}

#[tokio::test]
async fn inserting_an_assertion_is_idempotent_but_rejects_a_conflict() {
    let (_dir, pool) = ledger().await;
    let conn = &mut *pool.acquire().await.unwrap();
    let a = statement(dec!(1000), dec!(900));
    assertions::insert(conn, &a).await.unwrap();
    assertions::insert(conn, &a).await.expect("the same figures again are a no-op");
    assert_eq!(assertions::load(conn).await.unwrap(), [a]);

    let err = assertions::insert(conn, &statement(dec!(1000), dec!(901)))
        .await
        .expect_err("two closings for one period cannot both be right");
    assert!(format!("{err:#}").contains("conflicts"), "{err:#}");

    let mut unknown = statement(dec!(0), dec!(0));
    unknown.account = "Assets:Nowhere".into();
    assert!(assertions::insert(conn, &unknown).await.is_err());
}

/// An assertion only vouches for what it could have seen: postings after its
/// period leave the balance unchecked again, which is the state an account
/// whose statements stopped arriving is in.
#[tokio::test]
async fn an_assertion_older_than_the_newest_posting_does_not_vouch_for_it() {
    let (_dir, pool) = ledger().await;
    post(&mut *pool.acquire().await.unwrap(), "2024-05-01", "Assets:Bank", "50", "TWD").await;
    let report = check_with(&pool, &[statement(dec!(1000), dec!(900))]).await;

    // The figures still match: they describe a period the posting is outside of.
    // Only the account's current balance is left unvouched for.
    assert!(report.ok(), "{report}");
    let stale = report
        .unchecked
        .iter()
        .find(|u| u.account == "Assets:Bank" && u.currency == Currency::TWD)
        .expect("a balance posted to after its newest assertion is unchecked");
    assert_eq!((stale.vouched_through, stale.posted_through), (Some(d(2024, 4, 1)), d(2024, 5, 1)));
    assert!(report.to_string().contains("vouched through 2024-04-01"), "{report}");
}
