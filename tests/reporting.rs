//! Tests for the read-only reporting engine (`portfolio::store::query`).
//!
//! Two kinds of fixture, on purpose:
//!
//!  * **Hand-seeded** rows give a small multi-currency ledger whose every
//!    report number is worked out by hand in the comments — including a
//!    mismatched-rate `Equity:Conversions` plug that must stay out of net
//!    worth, and a seeded `stock_prices` FX quote so conversion is actually
//!    exercised.
//!  * **freeze → load** drives the real bake over a synthetic ledger and loads
//!    the journal into SQLite, so the reports are also checked against
//!    realistically-shaped data (the same pipeline `tests/freeze_journal.rs`
//!    covers), including the `Equity:Conversions` legs the bake itself inserts.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use portfolio::{
    db,
    ledger::{args::Args as BuildArgs, freeze, load},
    portfolio::portfolio::{
        AssetClass, Broker, Currency, Event, Portfolio, Security, Transaction, TransactionKind,
    },
    prices::StockPrice,
    store::query::{
        self, account_balances, holdings_report, net_worth_over_time, periodic_report, AccountType,
        Grain,
    },
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;
use tempfile::TempDir;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).expect("valid date")
}

/// A fresh, migrated, empty database in a temp dir (absolute path in the URL).
async fn fresh_db() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().expect("temp dir");
    let url = format!("sqlite:{}", dir.path().join("reporting.db").display());
    let pool = db::init_db(&url).await.expect("init db");
    (dir, pool)
}

async fn add_account(pool: &SqlitePool, path: &str, label: &str, ty: &str) -> i64 {
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
async fn add_txn(pool: &SqlitePool, date: &str, narration: &str, legs: &[(i64, &str, &str)]) {
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

async fn add_price(pool: &SqlitePool, symbol: &str, date: &str, close: f64) {
    sqlx::query("INSERT INTO stock_prices (symbol, date, close_price) VALUES (?, ?, ?)")
        .bind(symbol)
        .bind(date)
        .bind(close)
        .execute(pool)
        .await
        .expect("insert price");
}

fn balance_of<'a>(
    balances: &'a [query::AccountBalance],
    path: &str,
    currency: Currency,
) -> Option<&'a query::AccountBalance> {
    balances.iter().find(|b| b.path == path && b.currency == currency)
}

/// Seeds a two-currency ledger (TWD savings + USD brokerage cash, a credit
/// card, income, expenses) plus an `Equity:Conversions` FX plug booked at a
/// rate (90) different from the reporting rate (30), and a `TWD=X` = 30 FX
/// quote. Base currency is TWD throughout.
async fn seed_multicurrency(pool: &SqlitePool) {
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

#[tokio::test]
async fn balances_are_per_account_per_currency_and_date_filtered() {
    let (_dir, pool) = fresh_db().await;
    seed_multicurrency(&pool).await;

    // As of the opening date, only opening balances exist.
    let opening = account_balances(&pool, d(2026, 1, 1)).await.unwrap();
    assert_eq!(
        balance_of(&opening, "Assets:Cathay:Savings", Currency::TWD).unwrap().amount,
        dec!(100000)
    );
    assert_eq!(balance_of(&opening, "Assets:IB:Cash", Currency::USD).unwrap().amount, dec!(1000));

    // Mid-January: salary counted, the 10th's lunch counted, February excluded.
    let mid = account_balances(&pool, d(2026, 1, 12)).await.unwrap();
    assert_eq!(
        balance_of(&mid, "Assets:Cathay:Savings", Currency::TWD).unwrap().amount,
        dec!(149700)
    );
    assert!(balance_of(&mid, "Expenses:Food", Currency::USD).is_none(), "Feb USD leg leaked early");

    // End state: every leg applied.
    let end = account_balances(&pool, d(2026, 3, 1)).await.unwrap();
    // Savings: 100000 + 50000 - 300 - 3000 = 146700.
    assert_eq!(
        balance_of(&end, "Assets:Cathay:Savings", Currency::TWD).unwrap().amount,
        dec!(146700)
    );
    // IB cash: 1000 - 20 + 90 = 1070 USD.
    assert_eq!(balance_of(&end, "Assets:IB:Cash", Currency::USD).unwrap().amount, dec!(1070));
    // Credit card: 0 - 1000 = -1000 TWD (credit-normal, negative).
    assert_eq!(
        balance_of(&end, "Liabilities:CreditCard", Currency::TWD).unwrap().amount,
        dec!(-1000)
    );
    // Food carries two currencies at once.
    assert_eq!(balance_of(&end, "Expenses:Food", Currency::TWD).unwrap().amount, dec!(1300));
    assert_eq!(balance_of(&end, "Expenses:Food", Currency::USD).unwrap().amount, dec!(20));
    // The equity plug is present (and non-netting), so its exclusion is observable.
    assert_eq!(balance_of(&end, "Equity:Conversions", Currency::TWD).unwrap().amount, dec!(3000));
    assert_eq!(balance_of(&end, "Equity:Conversions", Currency::USD).unwrap().amount, dec!(-90));

    // Metadata round-trips.
    let food_twd = balance_of(&end, "Expenses:Food", Currency::TWD).unwrap();
    assert_eq!(food_twd.account_type, AccountType::Expense);
    assert_eq!(food_twd.label, "Food");
    assert!(!food_twd.closed);
}

#[tokio::test]
async fn net_worth_converts_currencies_and_excludes_equity() {
    let (_dir, pool) = fresh_db().await;
    seed_multicurrency(&pool).await;

    // Monthly over Jan–Feb: a point at each month end, in TWD.
    let series =
        net_worth_over_time(&pool, Currency::TWD, d(2026, 1, 1), d(2026, 2, 28), Grain::Month)
            .await
            .unwrap();
    assert_eq!(series.base, Currency::TWD);
    assert_eq!(series.points.len(), 2);

    // End of January (February legs excluded):
    //   assets   = savings 149700 + IB 1000 USD * 30 = 149700 + 30000 = 179700
    //   liabilities = 0 ; net = 179700
    let jan = &series.points[0];
    assert_eq!(jan.date, d(2026, 1, 31));
    assert_eq!(jan.assets, dec!(179700));
    assert_eq!(jan.liabilities, dec!(0));
    assert_eq!(jan.net, dec!(179700));

    // End of February:
    //   assets = savings 146700 + IB 1070 USD * 30 = 146700 + 32100 = 178800
    //   liabilities = credit card -1000 ; net = 177800
    // The Equity:Conversions plug (+3000 TWD, -90 USD → +300 TWD at rate 30) is
    // excluded; including it would have made net 178100.
    let feb = &series.points[1];
    assert_eq!(feb.date, d(2026, 2, 28));
    assert_eq!(feb.assets, dec!(178800));
    assert_eq!(feb.liabilities, dec!(-1000));
    assert_eq!(feb.net, dec!(177800));
}

#[tokio::test]
async fn periodic_report_groups_income_and_expense_across_currencies() {
    let (_dir, pool) = fresh_db().await;
    seed_multicurrency(&pool).await;

    let report = periodic_report(&pool, Currency::TWD, d(2026, 1, 1), d(2026, 2, 28), Grain::Month)
        .await
        .unwrap();
    assert_eq!(report.periods.len(), 2);

    // January: salary 50000 income (negated from its credit-normal -50000);
    // expense is the 300 TWD lunch.
    let jan = &report.periods[0];
    assert_eq!((jan.start, jan.end), (d(2026, 1, 1), d(2026, 1, 31)));
    assert_eq!(jan.income, dec!(50000));
    assert_eq!(jan.expense, dec!(300));
    assert_eq!(jan.net, dec!(49700));

    // February: no income; expense = 20 USD * 30 + 1000 TWD = 1600. The FX
    // conversion transaction has no income/expense leg, so it never appears.
    let feb = &report.periods[1];
    assert_eq!((feb.start, feb.end), (d(2026, 2, 1), d(2026, 2, 28)));
    assert_eq!(feb.income, dec!(0));
    assert_eq!(feb.expense, dec!(1600));
    assert_eq!(feb.net, dec!(-1600));
}

#[tokio::test]
async fn periodic_report_clamps_partial_period_bounds_to_the_range() {
    let (_dir, pool) = fresh_db().await;
    seed_multicurrency(&pool).await;

    // Jan 6 – Feb 18: the first and last months are partial. Each period's
    // reported bounds must clamp to the range, and postings outside it (salary
    // Jan 5, groceries Feb 20) must not count.
    let report = periodic_report(&pool, Currency::TWD, d(2026, 1, 6), d(2026, 2, 18), Grain::Month)
        .await
        .unwrap();
    assert_eq!(report.periods.len(), 2);

    let jan = &report.periods[0];
    assert_eq!((jan.start, jan.end), (d(2026, 1, 6), d(2026, 1, 31)));
    assert_eq!(jan.income, dec!(0)); // salary Jan 5 falls before the range
    assert_eq!(jan.expense, dec!(300)); // lunch Jan 10

    let feb = &report.periods[1];
    assert_eq!((feb.start, feb.end), (d(2026, 2, 1), d(2026, 2, 18)));
    assert_eq!(feb.expense, dec!(600)); // coffee 20 USD * 30; groceries Feb 20
                                        // excluded
}

#[tokio::test]
async fn quarter_week_and_day_grains_bucket_by_their_own_bounds() {
    let (_dir, pool) = fresh_db().await;
    seed_multicurrency(&pool).await;

    // Q1 holds all of January and February; Q2 is empty but still reported.
    let report =
        periodic_report(&pool, Currency::TWD, d(2026, 1, 1), d(2026, 6, 30), Grain::Quarter)
            .await
            .unwrap();
    let bounds: Vec<(NaiveDate, NaiveDate)> =
        report.periods.iter().map(|p| (p.start, p.end)).collect();
    assert_eq!(bounds, [(d(2026, 1, 1), d(2026, 3, 31)), (d(2026, 4, 1), d(2026, 6, 30))]);
    assert_eq!((report.periods[0].income, report.periods[0].expense), (dec!(50000), dec!(1900)));
    assert_eq!((report.periods[1].income, report.periods[1].expense), (dec!(0), dec!(0)));

    // Weeks start on Monday: 2026-01-10 is a Saturday, in the week of the 5th,
    // which also holds the salary.
    let report = periodic_report(&pool, Currency::TWD, d(2026, 1, 5), d(2026, 1, 18), Grain::Week)
        .await
        .unwrap();
    let weeks: Vec<(NaiveDate, NaiveDate, Decimal, Decimal)> =
        report.periods.iter().map(|p| (p.start, p.end, p.income, p.expense)).collect();
    assert_eq!(weeks, [
        (d(2026, 1, 5), d(2026, 1, 11), dec!(50000), dec!(300)),
        (d(2026, 1, 12), d(2026, 1, 18), dec!(0), dec!(0))
    ]);

    // One period per day; only the 10th has the lunch.
    let report = periodic_report(&pool, Currency::TWD, d(2026, 1, 9), d(2026, 1, 11), Grain::Day)
        .await
        .unwrap();
    let expenses: Vec<(NaiveDate, Decimal)> =
        report.periods.iter().map(|p| (p.start, p.expense)).collect();
    assert_eq!(expenses, [
        (d(2026, 1, 9), dec!(0)),
        (d(2026, 1, 10), dec!(300)),
        (d(2026, 1, 11), dec!(0))
    ]);
}

/// A posting older than every recorded quote converts at the earliest one
/// the report can see, rather than dropping out.
#[tokio::test]
async fn a_date_before_every_quote_converts_at_the_earliest() {
    let (_dir, pool) = fresh_db().await;
    let wallet = add_account(&pool, "Assets:USD-Wallet", "Wallet", "asset").await;
    let food = add_account(&pool, "Expenses:Food", "Food", "expense").await;
    add_txn(&pool, "2026-01-10", "lunch abroad", &[(food, "10", "USD"), (wallet, "-10", "USD")])
        .await;
    add_price(&pool, "TWD=X", "2026-03-01", 30.0).await;
    add_price(&pool, "TWD=X", "2026-03-15", 40.0).await;

    let report = periodic_report(&pool, Currency::TWD, d(2026, 1, 1), d(2026, 3, 31), Grain::Month)
        .await
        .unwrap();
    assert_eq!(report.periods[0].expense, dec!(300));
}

/// A ledger held only in the base currency needs no rates at all.
#[tokio::test]
async fn a_base_currency_ledger_reports_without_rates() {
    let (_dir, pool) = fresh_db().await;
    let cash = add_account(&pool, "Assets:Cash", "Cash", "asset").await;
    let food = add_account(&pool, "Expenses:Food", "Food", "expense").await;
    add_txn(&pool, "2026-01-10", "lunch", &[(food, "120", "TWD"), (cash, "-120", "TWD")]).await;

    let report = periodic_report(&pool, Currency::TWD, d(2026, 1, 1), d(2026, 1, 31), Grain::Month)
        .await
        .unwrap();
    assert_eq!(report.periods[0].expense, dec!(120));
}

/// A posting whose account is gone would drop out of every report without a
/// trace, so loading refuses it.
#[tokio::test]
async fn a_posting_naming_a_missing_account_fails_the_load() {
    let (_dir, pool) = fresh_db().await;
    let cash = add_account(&pool, "Assets:Cash", "Cash", "asset").await;
    let food = add_account(&pool, "Expenses:Food", "Food", "expense").await;
    add_txn(&pool, "2026-01-10", "lunch", &[(food, "120", "TWD"), (cash, "-120", "TWD")]).await;
    // Only a database with its foreign keys off can hold such a row.
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF").execute(&mut *conn).await.unwrap();
    sqlx::query("UPDATE postings SET account_id = 999 WHERE account_id = ?")
        .bind(food)
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);

    let err = account_balances(&pool, d(2026, 1, 31)).await.expect_err("an orphan posting");
    assert!(format!("{err:#}").contains("account id 999"), "{err:#}");
}

/// Builds a portfolio holding 10 AAPL bought at 100 USD, reported in `broker`'s
/// currency. Used by the holdings tests.
fn aapl_portfolio(broker: Broker) -> Portfolio {
    let mut securities = HashMap::new();
    securities.insert("AAPL".to_string(), Security {
        symbol: "AAPL".to_string(),
        description: "Apple Inc.".to_string(),
    });
    let buy = Transaction {
        id: "1".to_string(),
        source: broker,
        asset_class: AssetClass::Stocks,
        symbol: "AAPL".to_string(),
        kind: TransactionKind::Buy,
        datetime: d(2026, 1, 5).and_hms_opt(0, 0, 0).unwrap().and_utc(),
        settle_date: None,
        quantity: dec!(10),
        price: dec!(100),
        amount: dec!(-1000),
        commission: dec!(0),
        currency: Currency::USD,
        balance: dec!(0),
    };
    let mut events = BTreeMap::new();
    events.insert(d(2026, 1, 5), Event { transactions: vec![buy], splits: vec![] });
    Portfolio::new(broker, events, securities)
}

#[tokio::test]
async fn holdings_report_projects_the_portfolio_modules_positions() {
    // A single Buy: 10 AAPL @ 100 USD, no commission. Marked at 120 USD.
    let mut securities = HashMap::new();
    securities.insert("AAPL".to_string(), Security {
        symbol: "AAPL".to_string(),
        description: "Apple Inc.".to_string(),
    });
    let buy = Transaction {
        id: "1".to_string(),
        source: Broker::InteractiveBrokers,
        asset_class: AssetClass::Stocks,
        symbol: "AAPL".to_string(),
        kind: TransactionKind::Buy,
        datetime: d(2026, 1, 5).and_hms_opt(0, 0, 0).unwrap().and_utc(),
        settle_date: None,
        quantity: dec!(10),
        price: dec!(100),
        amount: dec!(-1000),
        commission: dec!(0),
        currency: Currency::USD,
        balance: dec!(0),
    };
    let mut events = BTreeMap::new();
    events.insert(d(2026, 1, 5), Event { transactions: vec![buy], splits: vec![] });
    let mut portfolio = Portfolio::new(Broker::InteractiveBrokers, events, securities);

    let mut prices: HashMap<String, Vec<StockPrice>> = HashMap::new();
    prices.insert("AAPL".to_string(), vec![StockPrice { date: d(2026, 1, 6), close_price: 120.0 }]);

    let report = holdings_report(&mut portfolio, d(2026, 1, 31), &prices).unwrap();

    assert_eq!(report.reporting_currency, Currency::USD);
    assert_eq!(report.positions.len(), 1);
    let aapl = &report.positions[0];
    assert_eq!(aapl.symbol, "AAPL");
    assert_eq!(aapl.description, "Apple Inc.");
    assert_eq!(aapl.quantity, dec!(10));
    assert_eq!(aapl.total_cost, dec!(1000));
    assert_eq!(aapl.average_cost, dec!(100));
    assert_eq!(aapl.market_price, dec!(120));
    assert_eq!(aapl.market_value, dec!(1200));
    assert_eq!(aapl.unrealized_pnl_value, dec!(200));
    assert_eq!(aapl.realized_pnl_value, dec!(0));

    assert_eq!(report.total_cost, dec!(1000));
    assert_eq!(report.total_market_value, dec!(1200));
    assert_eq!(report.total_unrealized_pnl, dec!(200));
}

#[test]
fn holdings_totals_convert_at_the_as_of_rate_not_the_newest() {
    // A US stock reported in TWD, with USD->TWD rising after as_of.
    let mut portfolio = aapl_portfolio(Broker::Cathay); // reports in TWD
    let mut prices: HashMap<String, Vec<StockPrice>> = HashMap::new();
    prices.insert("AAPL".to_string(), vec![StockPrice { date: d(2026, 1, 6), close_price: 120.0 }]);
    prices.insert("TWD=X".to_string(), vec![
        StockPrice { date: d(2026, 1, 1), close_price: 30.0 }, // in effect on as_of
        StockPrice { date: d(2026, 6, 1), close_price: 35.0 }, // newer — must NOT be used
    ]);

    let report = holdings_report(&mut portfolio, d(2026, 1, 31), &prices).unwrap();

    assert_eq!(report.reporting_currency, Currency::TWD);
    // 10 * 120 = 1200 USD, converted at the Jan rate 30 (not the newer 35).
    assert_eq!(report.total_market_value, dec!(36000));
    assert_eq!(report.total_cost, dec!(30000));
}

#[test]
fn holdings_report_preserves_the_callers_daily_statements() {
    let mut portfolio = aapl_portfolio(Broker::InteractiveBrokers);
    let mut prices: HashMap<String, Vec<StockPrice>> = HashMap::new();
    prices.insert("AAPL".to_string(), vec![StockPrice { date: d(2026, 1, 6), close_price: 120.0 }]);

    // The caller builds statements over its own range first.
    portfolio.generate_daily_statements(d(2026, 1, 5), d(2026, 1, 10), &prices);
    let kept: Vec<_> = portfolio.daily_statements.keys().copied().collect();
    assert!(!kept.is_empty());

    let _ = holdings_report(&mut portfolio, d(2026, 1, 31), &prices).unwrap();

    assert_eq!(
        portfolio.daily_statements.keys().copied().collect::<Vec<_>>(),
        kept,
        "holdings_report must leave the caller's daily_statements untouched"
    );
    assert!(
        !portfolio.daily_statements.contains_key(&d(2026, 1, 31)),
        "the as_of statement leaked into the caller's map"
    );
}

// --- freeze → load: reports over realistically-shaped data ----------------

const MAPPING: &str = r#"
[institution]
app_account            = "國泰"
primary                = "Assets:Cathay:Savings"
settlement             = "Assets:Cathay:Investment"
settlement_app_account = "券商"
clearing               = "Assets:Cathay:Clearing"

[institution.accounts]
"111111111111" = "Assets:Cathay:Savings"

[fallback]
income  = "Income:Uncategorized"
expense = "Expenses:Uncategorized"

[expenses]
"飲食" = "Expenses:Food"

[income]
"投資" = "Income:Investment"

[accounts]
"現金" = "Assets:Cash"
"美金" = "Assets:USD-Wallet"
"國泰" = "Assets:Cathay"
"券商" = "Assets:Securities:ETF"
"起鼓" = "Equity:Opening-Balances"
"#;

// Unified transactions input (the T2b format). The opening is a `config`
// transfer from the opening-equity label 起鼓 into the account; the rest are
// the app's own income/expense/transfer rows.
const TRANSACTIONS: &str = "\
id,status,date,posted_date,kind,amount,currency,account,counter_account,counter_amount,\
                            counter_currency,category,major_category,member,tags,note,\
                            source_party,source_file,source_id,origin,correction_note,updated_at
o:1,active,2022-01-01,,transfer,1000,TWD,起鼓,現金,1000,TWD,,,,,,config,m,,added,,
a:1,active,2022-02-01,,expense,200,TWD,國泰,,,,飲食,,自己,,,app,f,uuid-food-2022,raw,,
a:2,active,2022-04-01,,income,500,TWD,現金,,,,投資,,自己,,,app,f,uuid-cash-income,raw,,
a:3,active,2022-05-01,,transfer,3000,TWD,國泰,券商,3000,TWD,,,,,,app,f,uuid-etf,raw,,
a:4,active,2023-01-01,,transfer,300,TWD,現金,美金,10,USD,,,,,,app,f,uuid-fx,raw,,
a:5,active,2024-06-01,,expense,100,TWD,國泰,,,,飲食,,自己,,,app,f,uuid-food-2024,raw,,
";

const SAVINGS_STATEMENT: &str = "\
查詢期間,(自 2024/01/01 至 2024/12/31)
111111111111 活存
幣別：TWD
交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註
2024/06/01,2024/06/01,午餐,100,,4900,,
2024/03/01,2024/03/01,薪水,,5000,5000,,
";

const MANUAL: &str = "\
date,account,contra,amount,currency,payee,narration,tags
2024-07-01,Assets:Petty-Cash,Income:Gifts,600,TWD,紅包,cash gift,
";

/// Freezes the synthetic ledger and loads the journal into a fresh SQLite,
/// returning the temp dir (kept alive) and a pool over the loaded database.
async fn freeze_and_load() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().expect("temp dir");
    let root = dir.path();
    let write = |name: &str, contents: &str| {
        std::fs::write(root.join(name), contents).unwrap_or_else(|e| panic!("write {name}: {e}"));
    };
    write("mapping.toml", MAPPING);
    write("transactions.csv", TRANSACTIONS);
    write("savings.csv", SAVINGS_STATEMENT);
    write("manual.csv", MANUAL);

    let build = BuildArgs {
        cathay_statements: vec![root.join("savings.csv")],
        daily_income_expense: None,
        daily_transfers: None,
        transactions: Some(root.join("transactions.csv")),
        backfill: true,
        ledger_dir: root.to_path_buf(),
    };
    let freeze_args = freeze::FreezeArgs { journal: root.join("journal.csv"), build };
    let frozen = freeze::run(&freeze_args).expect("freeze");
    assert!(frozen.ok(), "fixture did not reconcile:\n{frozen}");
    assert_eq!(frozen.conversions, 2, "cross-currency plug not inserted:\n{frozen}");

    let url = format!("sqlite:{}", root.join("ledger-app.db").display());
    let load_args = load::Args {
        journal: root.join("journal.csv"),
        database_url: url.clone(),
        mapping: root.join("mapping.toml"),
    };
    load::run(&load_args).await.expect("load");
    let pool = db::init_db(&url).await.expect("open loaded db");
    (dir, pool)
}

#[tokio::test]
async fn reports_over_frozen_history_exclude_the_conversions_plug() {
    let (_dir, pool) = freeze_and_load().await;
    let as_of = d(2024, 12, 31);
    let balances = account_balances(&pool, as_of).await.unwrap();

    // Assets:Cash is touched only by its opening (1000), the 2022 income (+500)
    // and the 2023 FX transfer out (-300): 1200 TWD.
    assert_eq!(balance_of(&balances, "Assets:Cash", Currency::TWD).unwrap().amount, dec!(1200));
    // The FX transfer's other side lands 10 USD in the USD wallet.
    assert_eq!(balance_of(&balances, "Assets:USD-Wallet", Currency::USD).unwrap().amount, dec!(10));

    // The bake inserts Equity:Conversions legs to plug the cross-currency
    // transfer (no @@ price column yet). It must be present but non-zero...
    let equity_nonzero =
        balances.iter().any(|b| b.account_type == AccountType::Equity && !b.amount.is_zero());
    assert!(equity_nonzero, "expected a non-zero Equity:Conversions balance to exclude");

    // ...and excluded from net worth. Recompute it from the asset and liability
    // rows (USD at 30) and require the report to match, which it only can if
    // equity/income/expense were all left out.
    add_price(&pool, "TWD=X", "2020-01-01", 30.0).await;
    let rate = |b: &query::AccountBalance| match b.currency {
        Currency::USD => dec!(30),
        _ => Decimal::ONE,
    };
    let expected_assets: Decimal = balances
        .iter()
        .filter(|b| b.account_type == AccountType::Asset)
        .map(|b| b.amount * rate(b))
        .sum();
    let expected_liabilities: Decimal = balances
        .iter()
        .filter(|b| b.account_type == AccountType::Liability)
        .map(|b| b.amount * rate(b))
        .sum();

    let series = net_worth_over_time(&pool, Currency::TWD, as_of, as_of, Grain::Day).await.unwrap();
    let point = &series.points[0];
    assert_eq!(point.assets, expected_assets);
    assert_eq!(point.liabilities, expected_liabilities);
    assert_eq!(point.net, expected_assets + expected_liabilities);
}

#[tokio::test]
async fn periodic_totals_over_frozen_history_reconcile_with_balances() {
    let (_dir, pool) = freeze_and_load().await;
    // Wide bounds so every income/expense posting is inside the period, whatever
    // exact dates the bake assigned.
    let (start, end) = (d(2020, 1, 1), d(2025, 12, 31));
    let balances = account_balances(&pool, end).await.unwrap();

    // Income and expense accounts have no opening balances and all their
    // postings fall inside the period, so the whole-period totals must
    // equal the account balances (income negated to positive revenue). They are
    // all in TWD, so no FX quote is needed.
    let expected_income: Decimal = -balances
        .iter()
        .filter(|b| b.account_type == AccountType::Income)
        .map(|b| b.amount)
        .sum::<Decimal>();
    let expected_expense: Decimal =
        balances.iter().filter(|b| b.account_type == AccountType::Expense).map(|b| b.amount).sum();

    let report = periodic_report(&pool, Currency::TWD, start, end, Grain::Year).await.unwrap();
    let total_income: Decimal = report.periods.iter().map(|b| b.income).sum();
    let total_expense: Decimal = report.periods.iter().map(|b| b.expense).sum();

    assert_eq!(total_income, expected_income);
    assert_eq!(total_expense, expected_expense);
    assert!(total_income > Decimal::ZERO, "fixture should have income");
    assert!(total_expense > Decimal::ZERO, "fixture should have expense");
}

/// A foreign balance with no FX quote must fail the report, never count 1:1.
#[tokio::test]
async fn net_worth_without_an_fx_rate_is_an_error_not_one_to_one() {
    let (_dir, pool) = freeze_and_load().await;
    let as_of = d(2024, 12, 31);
    let err = net_worth_over_time(&pool, Currency::TWD, as_of, as_of, Grain::Day)
        .await
        .expect_err("a USD balance with no USD→TWD quote must not convert at 1");
    assert!(format!("{err:#}").contains("USD"), "error should name the currency: {err:#}");
}

/// Net worth excludes income and expense, so a currency seen only there needs
/// no quote — it must not abort the report.
#[tokio::test]
async fn net_worth_ignores_a_currency_that_only_touches_flow_accounts() {
    let (_dir, pool) = fresh_db().await;
    let cash = add_account(&pool, "Assets:Cash", "Cash", "asset").await;
    let salary = add_account(&pool, "Income:Salary", "Salary", "income").await;
    let travel = add_account(&pool, "Expenses:Travel", "Travel", "expense").await;
    add_txn(&pool, "2026-01-05", "pay", &[(cash, "1000", "TWD"), (salary, "-1000", "TWD")]).await;
    // A JPY trip paid by a JPY allowance: no JPY ever reaches the balance sheet.
    add_txn(&pool, "2026-01-06", "trip", &[(travel, "500", "JPY"), (salary, "-500", "JPY")]).await;

    let as_of = d(2026, 1, 31);
    let series = net_worth_over_time(&pool, Currency::TWD, as_of, as_of, Grain::Day).await.expect(
        "a JPY flow leg with no quote must not fail a report that excludes income and expense",
    );
    assert_eq!(series.points[0].net, dec!(1000));

    // The same JPY leg does need a rate once it is inside the reported figure.
    let err = periodic_report(&pool, Currency::TWD, d(2026, 1, 1), as_of, Grain::Month)
        .await
        .expect_err("an expense report must not count 500 JPY as 500 TWD");
    assert!(format!("{err:#}").contains("JPY"), "{err:#}");
}
