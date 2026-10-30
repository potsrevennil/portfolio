//! `matcher::split_repayment::suggest` over a real database: what it proposes
//! can be confirmed as it stands through `bill_split::attach`.

#[path = "../../db/tests/bill_split/fixture.rs"]
mod fixture;

use db::bill_split::{self, Attach, Repayment, Status};
use fixture::*;
use import::matcher::split_repayment::suggest;
use rust_decimal_macros::dec;

#[tokio::test]
async fn a_suggestion_spans_the_persons_accounts_and_splits_and_can_be_confirmed() {
    let l = Ledger::new().await;
    let old = l.line("2026-01-10", &[(BANK, "-300"), (FOOD, "100"), (ALICE_GEAR, "200")]).await;
    let new = l.line("2026-03-10", &[(BANK, "-400"), (FOOD, "100"), (ALICE, "300")]).await;
    let (first, second) = (l.per_person("裝備").await, l.per_person("晚餐").await);
    bill_split::gather(&l.pool, &l.books, first, &[old]).await.unwrap();
    bill_split::gather(&l.pool, &l.books, second, &[new]).await.unwrap();
    let back = l.line("2026-05-01", &[(BANK, "350"), (ALICE, "-350")]).await;

    let suggestions = suggest(&l.pool, &l.books, l.alice).await.unwrap();

    let [suggestion] = suggestions.as_slice() else { panic!("{suggestions:?}") };
    assert_eq!(suggestion.repayment.transaction_id, back);
    assert_eq!(suggestion.settles, vec![(first, dec!(-200)), (second, dec!(-150))]);
    assert_eq!(suggestion.residual, dec!(0));
    // Nothing happens until it is confirmed.
    assert_eq!(bill_split::get(&l.pool, first).await.unwrap().status(), Status::Open);

    for (split, amount) in &suggestion.settles {
        let how = Attach::Repayment(Repayment { account: None, amount: Some(*amount) });
        bill_split::attach(&l.pool, &l.books, *split, back, &how).await.unwrap();
    }
    assert_eq!(bill_split::get(&l.pool, first).await.unwrap().status(), Status::Settled);
    assert_eq!(bill_split::get(&l.pool, second).await.unwrap().status(), Status::Partial);
    assert!(suggest(&l.pool, &l.books, l.alice).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_overpayment_suggests_a_residual_and_a_stranger_suggests_nothing() {
    let l = Ledger::new().await;
    let dinner = l.line("2026-01-10", &[(BANK, "-300"), (FOOD, "100"), (ALICE, "200")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[dinner]).await.unwrap();
    l.line("2026-05-01", &[(BANK, "500"), (ALICE, "-500")]).await;

    let suggestions = suggest(&l.pool, &l.books, l.alice).await.unwrap();
    assert_eq!(suggestions[0].settles, vec![(split, dec!(-200))]);
    assert_eq!(suggestions[0].residual, dec!(-300));
    assert!(suggest(&l.pool, &l.books, l.bob).await.unwrap().is_empty());
}
