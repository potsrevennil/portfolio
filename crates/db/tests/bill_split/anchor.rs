use db::bill_split::{anchor_check, Anchor, AnchorRow, Anchored};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::fixture::*;

fn anchor(on: Anchored, currency: Currency, balance: Decimal) -> Anchor {
    Anchor { on, currency, balance }
}

#[tokio::test]
async fn a_friend_is_compared_with_all_their_accounts() {
    let l = Ledger::new().await;
    l.line("2026-01-01", &[(BANK, "-500"), (ALICE, "500")]).await;
    l.line("2026-01-02", &[(BANK, "-200"), (ALICE_GEAR, "200")]).await;

    let rows = anchor_check(
        &l.pool,
        &[anchor(Anchored::Person(l.alice), Currency::TWD, dec!(650))],
        d(2026, 12, 31),
    )
    .await
    .unwrap();

    assert_eq!(rows, vec![AnchorRow {
        on: Anchored::Person(l.alice),
        currency: Currency::TWD,
        ledger: dec!(700),
        splitwise: dec!(650),
    }]);
    assert_eq!(rows[0].diff(), dec!(50));
}

#[tokio::test]
async fn a_group_is_compared_with_its_netted_account_in_every_currency_either_side_holds() {
    let l = Ledger::new().await;
    l.netted("社團", CREW).await;
    l.line("2026-01-01", &[(BANK, "-300"), (CREW, "300")]).await;
    l.line_in("2026-01-02", &[(BANK, "-5"), (CREW, "5")], "USD").await;

    let crew = Anchored::Netted(CREW.into());
    let rows = anchor_check(
        &l.pool,
        &[
            anchor(crew.clone(), Currency::TWD, dec!(300)),
            anchor(crew.clone(), Currency::JPY, dec!(1000)),
        ],
        d(2026, 12, 31),
    )
    .await
    .unwrap();

    let diffs: Vec<(Currency, Decimal)> = rows.iter().map(|r| (r.currency, r.diff())).collect();
    assert!(diffs.contains(&(Currency::TWD, dec!(0))));
    // Splitwise leaves a zero balance out, so USD reads as zero there.
    assert!(diffs.contains(&(Currency::USD, dec!(5))));
    assert!(diffs.contains(&(Currency::JPY, dec!(-1000))));
}

#[tokio::test]
async fn the_ledger_side_stops_at_the_date_and_names_only_who_was_asked() {
    let l = Ledger::new().await;
    l.line("2026-01-01", &[(BANK, "-500"), (ALICE, "500")]).await;
    l.line("2026-06-01", &[(BANK, "-200"), (ALICE, "200")]).await;
    l.line("2026-01-01", &[(BANK, "-900"), (BOB, "900")]).await;

    let rows = anchor_check(
        &l.pool,
        &[anchor(Anchored::Person(l.alice), Currency::TWD, dec!(500))],
        d(2026, 3, 1),
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].diff(), dec!(0));
}

#[tokio::test]
async fn an_unknown_subject_is_refused() {
    let l = Ledger::new().await;
    let unknown = anchor(Anchored::Person(999), Currency::TWD, dec!(1));
    assert!(anchor_check(&l.pool, &[unknown], d(2026, 1, 1)).await.is_err());
    let unknown = anchor(Anchored::Netted("Assets:Split:Nope".into()), Currency::TWD, dec!(1));
    assert!(anchor_check(&l.pool, &[unknown], d(2026, 1, 1)).await.is_err());
}
