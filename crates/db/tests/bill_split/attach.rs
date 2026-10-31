use db::{
    bill_split::{self, Attach, Repayment, Role, Status},
    review::Edit,
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::fixture::*;

/// The editor's legs for a 未分類 bank line, saved and confirmed.
async fn split_line(l: &Ledger, line: i64, legs: &[(&str, Decimal)]) -> Edit {
    Edit { narration: Some("晚餐".into()), confirm: true, ..l.rewrite(line, legs).await }
}

#[tokio::test]
async fn attaching_a_share_writes_the_legs_and_logs_them() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(BANK, "-1000"), (UNCAT_OUT, "1000")]).await;
    let split = l.per_person("晚餐").await;
    let edit =
        split_line(&l, line, &[(BANK, dec!(-1000)), (FOOD, dec!(500)), (ALICE, dec!(500))]).await;

    bill_split::attach(&l.pool, &l.books, split, line, &Attach::Share(Some(edit))).await.unwrap();

    assert_eq!(
        l.legs(line).await,
        dec_map(&[(BANK, dec!(-1000)), (FOOD, dec!(500)), (ALICE, dec!(500))])
    );
    let split = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(split.shares[0].amount, dec!(500));
    assert!(split.shares[0].rewrote);
    assert_eq!(l.events(line).await, vec!["split", "attached"]);
}

#[tokio::test]
async fn a_repayment_may_be_partial_and_the_rest_stays_open() {
    let l = Ledger::new().await;
    let dinner = l.line("2026-01-10", &[(BANK, "-1000"), (FOOD, "500"), (ALICE, "500")]).await;
    let part = l.line("2026-05-01", &[(BANK, "200"), (ALICE, "-200")]).await;
    let rest = l.line("2026-09-01", &[(BANK, "300"), (ALICE, "-300")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[dinner]).await.unwrap();

    let how = Attach::Repayment(Repayment::default());
    bill_split::attach(&l.pool, &l.books, split, part, &how).await.unwrap();
    assert_eq!(bill_split::get(&l.pool, split).await.unwrap().status(), Status::Partial);

    bill_split::attach(&l.pool, &l.books, split, rest, &how).await.unwrap();
    let settled = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(settled.status(), Status::Settled);

    // A settled split takes no more.
    let late = l.line("2026-10-01", &[(BANK, "100"), (ALICE, "-100")]).await;
    let refused = bill_split::attach(&l.pool, &l.books, split, late, &how).await.unwrap_err();
    assert!(refused.to_string().contains("結清"), "{refused}");
}

#[tokio::test]
async fn a_repayment_can_land_on_any_of_a_persons_accounts() {
    let l = Ledger::new().await;
    let trip = l.line("2026-01-10", &[(BANK, "-500"), (FOOD, "200"), (ALICE_GEAR, "300")]).await;
    let repaid = l.line("2026-02-01", &[(BANK, "300"), (ALICE, "-300")]).await;
    let split = l.per_person("裝備").await;
    bill_split::gather(&l.pool, &l.books, split, &[trip, repaid]).await.unwrap();

    let split = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(split.status(), Status::Settled);
    // Each account keeps its own real event; only the person nets out.
    assert_eq!(l.balance(ALICE_GEAR).await, dec!(300));
    assert_eq!(l.balance(ALICE).await, dec!(-300));
}

#[tokio::test]
async fn an_overpayment_stays_on_the_account_unallocated() {
    let l = Ledger::new().await;
    let dinner = l.line("2026-01-10", &[(BANK, "-600"), (ALICE, "600")]).await;
    let over = l.line("2026-02-01", &[(BANK, "1000"), (ALICE, "-1000")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::attach(&l.pool, &l.books, split, dinner, &Attach::Share(None)).await.unwrap();
    bill_split::attach(&l.pool, &l.books, split, over, &Attach::Repayment(Repayment::default()))
        .await
        .unwrap();

    let split = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(split.status(), Status::Settled);
    let repayment = split.shares.iter().find(|s| s.role == Role::Repayment).unwrap();
    assert_eq!(repayment.amount, dec!(-600));
    assert_eq!(repayment.posting_amount, dec!(-1000));
    assert_eq!(l.balance(ALICE).await, dec!(-400));
}

#[tokio::test]
async fn one_repayment_can_cover_two_splits() {
    let l = Ledger::new().await;
    let one = l.line("2026-01-10", &[(BANK, "-600"), (FOOD, "300"), (ALICE, "300")]).await;
    let two = l.line("2026-01-20", &[(BANK, "-400"), (FOOD, "200"), (ALICE, "200")]).await;
    let both = l.line("2026-02-01", &[(BANK, "500"), (ALICE, "-500")]).await;
    let first = l.per_person("一").await;
    let second = l.per_person("二").await;
    bill_split::gather(&l.pool, &l.books, first, &[one]).await.unwrap();
    bill_split::gather(&l.pool, &l.books, second, &[two]).await.unwrap();

    let how = Attach::Repayment(Repayment::default());
    bill_split::attach(&l.pool, &l.books, first, both, &how).await.unwrap();
    bill_split::attach(&l.pool, &l.books, second, both, &how).await.unwrap();

    for split in [first, second] {
        assert_eq!(bill_split::get(&l.pool, split).await.unwrap().status(), Status::Settled);
    }
}

#[tokio::test]
async fn an_explicit_amount_is_capped_by_what_is_owed() {
    let l = Ledger::new().await;
    let dinner = l.line("2026-01-10", &[(BANK, "-600"), (FOOD, "100"), (ALICE, "500")]).await;
    let line = l.line("2026-02-01", &[(BANK, "1000"), (ALICE, "-1000")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[dinner]).await.unwrap();

    let too_much = Attach::Repayment(Repayment { account: None, amount: Some(dec!(-501)) });
    assert!(bill_split::attach(&l.pool, &l.books, split, line, &too_much).await.is_err());

    let part = Attach::Repayment(Repayment { account: None, amount: Some(dec!(-200)) });
    bill_split::attach(&l.pool, &l.books, split, line, &part).await.unwrap();
    let split = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(split.owed()[&(l.alice, Currency::TWD)], dec!(300));
}

#[tokio::test]
async fn a_repayment_moves_an_uncategorized_leg_to_the_account_the_user_names() {
    let l = Ledger::new().await;
    let dinner = l.line("2026-01-10", &[(BANK, "-600"), (FOOD, "100"), (ALICE, "500")]).await;
    let line = l.line("2026-02-01", &[(BANK, "500"), (UNCAT_IN, "-500")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[dinner]).await.unwrap();

    let how = to(ALICE);
    bill_split::attach(&l.pool, &l.books, split, line, &how).await.unwrap();

    assert_eq!(l.legs(line).await, dec_map(&[(BANK, dec!(500)), (ALICE, dec!(-500))]));
    assert_eq!(bill_split::get(&l.pool, split).await.unwrap().status(), Status::Settled);
    assert_eq!(l.events(line).await, vec!["edited", "attached"]);
}

#[tokio::test]
async fn a_repayment_is_refused_on_a_stranger_or_a_line_with_nothing_to_move() {
    let l = Ledger::new().await;
    let dinner = l.line("2026-01-10", &[(BANK, "-600"), (FOOD, "100"), (ALICE, "500")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[dinner]).await.unwrap();

    let line = l.line("2026-02-01", &[(BANK, "500"), (UNCAT_IN, "-500")]).await;
    let stranger = to(STRANGER);
    assert!(bill_split::attach(&l.pool, &l.books, split, line, &stranger).await.is_err());

    let bobs = to(BOB);
    let refused = bill_split::attach(&l.pool, &l.books, split, line, &bobs).await.unwrap_err();
    assert!(refused.to_string().contains("對得上"), "{refused}");
    // Refused means rolled back: the line keeps its 未分類 leg.
    assert_eq!(l.legs(line).await, dec_map(&[(BANK, dec!(500)), (UNCAT_IN, dec!(-500))]));

    let food = l.line("2026-02-02", &[(BANK, "-50"), (FOOD, "50")]).await;
    let alice = to(ALICE);
    let refused = bill_split::attach(&l.pool, &l.books, split, food, &alice).await.unwrap_err();
    assert!(refused.to_string().contains("未分類"), "{refused}");
}

#[tokio::test]
async fn a_named_account_narrows_a_repayment_to_that_leg() {
    let l = Ledger::new().await;
    let share = Attach::Share(None);
    let one = l.line("2026-01-10", &[(BANK, "-300"), (ALICE, "300")]).await;
    let two = l.line("2026-01-11", &[(BANK, "-200"), (BOB, "200")]).await;
    let both = l.line("2026-02-01", &[(BANK, "500"), (ALICE, "-300"), (BOB, "-200")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::attach(&l.pool, &l.books, split, one, &share).await.unwrap();
    bill_split::attach(&l.pool, &l.books, split, two, &share).await.unwrap();

    let alice = to(ALICE);
    bill_split::attach(&l.pool, &l.books, split, both, &alice).await.unwrap();

    let split = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(split.status(), Status::Partial);
    assert_eq!(split.owed()[&(l.bob, Currency::TWD)], dec!(200));
    assert_eq!(split.owed()[&(l.alice, Currency::TWD)], dec!(0));
}

#[tokio::test]
async fn the_other_leg_of_a_line_can_join_the_same_split_later() {
    let l = Ledger::new().await;
    let share = Attach::Share(None);
    let one = l.line("2026-01-10", &[(BANK, "-300"), (ALICE, "300")]).await;
    let two = l.line("2026-01-11", &[(BANK, "-200"), (BOB, "200")]).await;
    let both = l.line("2026-02-01", &[(BANK, "500"), (ALICE, "-300"), (BOB, "-200")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::attach(&l.pool, &l.books, split, one, &share).await.unwrap();
    bill_split::attach(&l.pool, &l.books, split, two, &share).await.unwrap();

    for account in [ALICE, BOB] {
        let how = to(account);
        bill_split::attach(&l.pool, &l.books, split, both, &how).await.unwrap();
    }
    assert_eq!(bill_split::get(&l.pool, split).await.unwrap().status(), Status::Settled);

    // Either leg again is a line it already holds.
    let again = to(ALICE);
    let refused = bill_split::attach(&l.pool, &l.books, split, both, &again).await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_netted_repayment_needs_no_person_and_only_moves_the_balance() {
    let l = Ledger::new().await;
    let split = l.netted("社團", CREW).await;
    let out = l.line("2026-01-10", &[(BANK, "-300"), (FOOD, "100"), (CREW, "200")]).await;
    let back = l.line("2026-02-01", &[(BANK, "150"), (CREW, "-150")]).await;
    bill_split::gather(&l.pool, &l.books, split, &[out]).await.unwrap();
    bill_split::attach(&l.pool, &l.books, split, back, &Attach::Repayment(Repayment::default()))
        .await
        .unwrap();

    let split = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(split.status(), Status::BalanceOnly);
    assert_eq!(split.balance[&Currency::TWD], dec!(50));
}

#[tokio::test]
async fn an_attach_that_breaks_a_statement_closing_is_refused_and_leaves_no_link() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(BANK, "-1000"), (UNCAT_OUT, "1000")]).await;
    l.statement_closes(BANK, d(2026, 1, 31), dec!(-1000)).await;
    let split = l.per_person("晚餐").await;
    let edit = split_line(&l, line, &[(BANK, dec!(-900)), (ALICE, dec!(900))]).await;

    let refused =
        bill_split::attach(&l.pool, &l.books, split, line, &Attach::Share(Some(edit))).await;
    assert!(refused.unwrap_err().to_string().contains("沒有儲存"));
    assert!(bill_split::get(&l.pool, split).await.unwrap().shares.is_empty());
    assert_eq!(l.balance(BANK).await, dec!(-1000));
}

#[tokio::test]
async fn attachable_ranks_people_then_fit_then_recency() {
    let l = Ledger::new().await;
    let old = l.line("2025-01-10", &[(BANK, "-400"), (ALICE, "400")]).await;
    let new = l.line("2026-01-10", &[(BANK, "-999"), (ALICE, "999")]).await;
    let bobs = l.line("2026-02-10", &[(BANK, "-400"), (BOB, "400")]).await;
    let settled = l.line("2026-03-01", &[(BANK, "-400"), (ALICE, "400")]).await;
    let paid = l.line("2026-03-02", &[(BANK, "400"), (ALICE, "-400")]).await;
    let (fits, recent, other, done) = (
        l.per_person("舊的").await,
        l.per_person("新的").await,
        l.per_person("別人").await,
        l.per_person("結清").await,
    );
    let share = Attach::Share(None);
    bill_split::attach(&l.pool, &l.books, fits, old, &share).await.unwrap();
    bill_split::attach(&l.pool, &l.books, recent, new, &share).await.unwrap();
    bill_split::attach(&l.pool, &l.books, other, bobs, &share).await.unwrap();
    bill_split::attach(&l.pool, &l.books, done, settled, &share).await.unwrap();
    bill_split::gather(&l.pool, &l.books, done, &[paid]).await.unwrap();

    let line = l.line("2026-04-01", &[(BANK, "400"), (ALICE, "-400")]).await;
    let ranked = bill_split::attachable(&l.pool, line).await.unwrap();
    let order: Vec<i64> = ranked.iter().map(|c| c.split.id).collect();
    assert_eq!(order, vec![fits, recent, other]);
    assert!(ranked[0].people && ranked[0].fit);
    assert!(ranked[1].people && !ranked[1].fit);
    assert!(!ranked[2].people && ranked[2].fit);
}
