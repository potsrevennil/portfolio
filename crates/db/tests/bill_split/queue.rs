//! The review queue must not pull a leg out from under a split.

use db::{
    bill_split::{self, Attach},
    review::{self, Edit, StatementAccounts},
};
use rust_decimal_macros::dec;

use crate::fixture::*;

/// A line with Food 500 + Alice 500 gathered into a split, and the edit that
/// leaves it as it is.
async fn held(l: &Ledger) -> (i64, Edit) {
    let line = l.line("2026-01-10", &[(BANK, "-1000"), (FOOD, "500"), (ALICE, "500")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[line]).await.unwrap();
    let ids = l.posting_ids(line).await;
    let legs = l
        .legs(line)
        .await
        .into_iter()
        .zip(ids)
        .map(|((account, amount), id)| leg(Some(id), &account, amount))
        .collect();
    (line, Edit { narration: Some("改".into()), legs, confirm: false })
}

#[tokio::test]
async fn the_queue_refuses_to_move_or_resize_a_held_leg() {
    let l = Ledger::new().await;
    let (line, edit) = held(&l).await;
    let statements = StatementAccounts::default();

    let mut moved = edit.clone();
    moved.legs[2].account = BOB.into();
    let refused = review::edit(&l.pool, &statements, line, &moved).await.unwrap_err();
    assert!(refused.to_string().contains("先退出分帳"), "{refused}");

    let mut resized = edit.clone();
    resized.legs[1].amount = dec!(400);
    resized.legs[2].amount = dec!(600);
    assert!(review::edit(&l.pool, &statements, line, &resized).await.is_err());

    let mut removed = edit.clone();
    removed.legs[1].amount = dec!(1000);
    removed.legs.truncate(2);
    assert!(review::edit(&l.pool, &statements, line, &removed).await.is_err());

    assert_eq!(l.balance(ALICE).await, dec!(500));
}

#[tokio::test]
async fn the_queue_may_still_recategorize_the_rest_of_the_line() {
    let l = Ledger::new().await;
    let (line, edit) = held(&l).await;
    let mut other = edit.clone();
    other.legs[1].account = UNCAT_OUT.into();
    review::edit(&l.pool, &StatementAccounts::default(), line, &other).await.unwrap();
    assert_eq!(l.balance(UNCAT_OUT).await, dec!(500));
}

#[tokio::test]
async fn the_queue_refuses_to_delete_a_held_line_until_it_is_detached() {
    let l = Ledger::new().await;
    let (line, _) = held(&l).await;
    let refused = review::delete(&l.pool, line).await.unwrap_err();
    assert!(refused.to_string().contains("先退出分帳"), "{refused}");

    let split = bill_split::list(&l.pool).await.unwrap()[0].id;
    bill_split::detach(&l.pool, &l.books, split, line).await.unwrap();
    review::delete(&l.pool, line).await.unwrap();
}

#[tokio::test]
async fn your_own_share_stays_as_it_is_while_held() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(FOOD, "300"), (CASH, "-300")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::attach(&l.pool, &l.books, split, line, &Attach::Share(None)).await.unwrap();
    let ids = l.posting_ids(line).await;

    // On 未分類 a later 天天記帳 record would replace the leg under the split.
    let recategorized = Edit {
        narration: None,
        legs: vec![leg(Some(ids[0]), UNCAT_OUT, dec!(300)), leg(Some(ids[1]), CASH, dec!(-300))],
        confirm: false,
    };
    let refused = review::edit(&l.pool, &StatementAccounts::default(), line, &recategorized).await;
    assert!(refused.unwrap_err().to_string().contains("先退出分帳"));
}
