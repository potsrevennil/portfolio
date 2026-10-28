//! The period-end walks charts are drawn from (`db::series`), over the
//! hand-seeded ledger.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use db::{
    market::Market,
    query::{self, LedgerData, PriceTable},
};
use ledger::valuation::AtCost;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;

#[path = "common/market.rs"]
mod market;
#[path = "common/seed.rs"]
mod seed;
use seed::{add_account, add_txn, d, fresh_db, seed_multicurrency};

/// The ledger and its rates up to `end`.
async fn snapshot(pool: &SqlitePool, market: &Market, end: NaiveDate) -> (LedgerData, PriceTable) {
    let data = LedgerData::load(pool).await.unwrap();
    let prices =
        query::load_fx_prices(market, &data.currencies(), Currency::TWD, end).await.unwrap();
    (data, prices)
}

#[tokio::test]
async fn worth_points_agree_with_net_worth_and_keep_unpriced_amounts_apart() {
    let (_dir, pool, market) = fresh_db().await;
    seed_multicurrency(&pool, &market).await;
    let cash = add_account(&pool, "Assets:Cash:Yen", "Yen", "asset").await;
    let opening: i64 =
        sqlx::query_scalar("SELECT id FROM accounts WHERE path = 'Equity:Opening-Balances'")
            .fetch_one(&pool)
            .await
            .unwrap();
    // No JPY rate is recorded: February is incomplete, January is not.
    add_txn(&pool, "2026-02-01", "yen", &[(cash, "5000", "JPY"), (opening, "-5000", "JPY")]).await;

    let (data, prices) = snapshot(&pool, &market, d(2026, 2, 28)).await;
    let points = data.worth_points(
        Currency::TWD,
        &AtCost::default(),
        d(2026, 1, 1),
        d(2026, 2, 20),
        &prices,
    );
    assert_eq!(points.len(), 2);
    let jan = &points[0];
    assert_eq!(jan.date, d(2026, 1, 31));
    assert_eq!(jan.assets.value, dec!(179700));
    assert!(jan.net().is_complete());
    // The last point is on the range's end: the 20th's card spend counts, the
    // 25th's conversion does not. 149700 - 0 + 980 USD * 30 = 179100.
    let feb = &points[1];
    assert_eq!(feb.date, d(2026, 2, 20));
    assert_eq!(feb.assets.value, dec!(179100));
    assert_eq!(feb.liabilities.value, dec!(-1000));
    assert_eq!(feb.net().value, dec!(178100));
    assert_eq!(feb.net().unpriced, BTreeMap::from([(Currency::JPY, dec!(5000))]));
}

#[tokio::test]
async fn flows_are_per_account_and_converted_at_each_postings_date() {
    let (_dir, pool, market) = fresh_db().await;
    seed_multicurrency(&pool, &market).await;
    let (data, prices) = snapshot(&pool, &market, d(2026, 3, 31)).await;
    let flows = data.flows(Currency::TWD, d(2026, 1, 6), d(2026, 3, 31), &prices);
    let accounts = |i: usize| -> Vec<(&str, Decimal)> {
        flows[i].accounts.iter().map(|(path, v)| (path.as_str(), v.value)).collect()
    };
    assert_eq!(flows.len(), 3);
    // The range clamps the first period; the salary on the 5th is outside it.
    assert_eq!((flows[0].start, flows[0].end), (d(2026, 1, 6), d(2026, 1, 31)));
    assert_eq!(accounts(0), [("Expenses:Food", dec!(300))]);
    // 20 USD at 30 and 1000 TWD; the conversion has no flow leg.
    assert_eq!(accounts(1), [("Expenses:Food", dec!(1600))]);
    // A quiet month is listed, empty.
    assert!(flows[2].accounts.is_empty());

    let income = data.flows(Currency::TWD, d(2026, 1, 1), d(2026, 1, 31), &prices);
    assert_eq!(income[0].accounts["Income:Salary"].value, dec!(-50000));
}
