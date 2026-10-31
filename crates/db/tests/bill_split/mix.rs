//! A group is netted or per-person, whichever way a leg arrives. 天天記帳's
//! test in `crates/import` follows a record there through the import's gate.

use anyhow::Error;
use db::{
    bill_split::Mixed,
    check,
    import::{self, Origin, Posting, Transaction},
    review::{self, Edit, Manual, StatementAccounts},
};
use ledger::{labels::Labels, model::Source};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::fixture::*;

const IN_CREW: &str = "Assets:Split:Crew:Alice";

fn refused_with(err: Error, with: &str) {
    let mixed = err.downcast::<Mixed>().unwrap();
    assert_eq!(mixed, Mixed { account: IN_CREW.into(), with: with.into() });
}

async fn netted_crew(l: &Ledger) {
    let mut db = l.pool.acquire().await.unwrap();
    import::ensure_account(&mut db, &Labels::default(), IN_CREW).await.unwrap();
    drop(db);
    l.netted("團體", CREW).await;
}

fn posting(account: &str, amount: Decimal) -> Posting {
    Posting {
        account: account.to_string(),
        amount,
        currency: Currency::TWD,
        tags: None,
        origin: Some(Origin::Rule),
    }
}

/// A netted group, and a 未分類 line with its two posting ids.
async fn crew_and_line(l: &Ledger) -> (i64, Vec<i64>) {
    netted_crew(l).await;
    let line = l.line("2026-01-10", &[(BANK, "-100"), (UNCAT_OUT, "100")]).await;
    (line, l.posting_ids(line).await)
}

#[tokio::test]
async fn a_queue_edit_cannot_move_a_leg_into_a_netted_group() {
    let l = Ledger::new().await;
    let (line, ids) = crew_and_line(&l).await;
    let moved = Edit {
        narration: None,
        legs: vec![leg(Some(ids[0]), BANK, dec!(-100)), leg(Some(ids[1]), IN_CREW, dec!(100))],
        confirm: false,
    };
    let refused = review::edit(&l.pool, &StatementAccounts::default(), line, &moved).await;
    refused_with(refused.unwrap_err(), CREW);
    assert_eq!(l.legs(line).await, dec_map(&[(BANK, dec!(-100)), (UNCAT_OUT, dec!(100))]));
}

#[tokio::test]
async fn a_queue_edit_cannot_add_a_leg_in_a_netted_group() {
    let l = Ledger::new().await;
    let (line, ids) = crew_and_line(&l).await;
    let added = Edit {
        narration: None,
        legs: vec![
            leg(Some(ids[0]), BANK, dec!(-100)),
            leg(Some(ids[1]), UNCAT_OUT, dec!(60)),
            leg(None, IN_CREW, dec!(40)),
        ],
        confirm: false,
    };
    let refused = review::edit(&l.pool, &StatementAccounts::default(), line, &added).await;
    refused_with(refused.unwrap_err(), CREW);
    assert_eq!(l.legs(line).await, dec_map(&[(BANK, dec!(-100)), (UNCAT_OUT, dec!(100))]));
}

#[tokio::test]
async fn a_group_with_owners_takes_no_leg_of_its_own() {
    let l = Ledger::new().await;
    let gear = "Assets:Split:Gear";
    let manual = Manual {
        date: d(2026, 1, 10),
        amount: dec!(100),
        currency: Currency::TWD,
        account: BANK.into(),
        category: None,
        counter: Some(gear.into()),
        note: None,
    };
    let refused = review::enter(&l.pool, &StatementAccounts::default(), &manual).await;
    let mixed = refused.unwrap_err().downcast::<Mixed>().unwrap();
    assert_eq!(mixed, Mixed { account: gear.into(), with: ALICE_GEAR.into() });

    // Its people and the netted group's own account still take legs.
    l.line("2026-01-10", &[(BANK, "-100"), (ALICE_GEAR, "100")]).await;
    l.netted("團體", CREW).await;
    let edit = Manual { counter: Some(CREW.into()), ..manual };
    review::enter(&l.pool, &StatementAccounts::default(), &edit).await.unwrap();
}

#[tokio::test]
async fn an_edit_leaves_a_leg_already_there_alone() {
    let l = Ledger::new().await;
    // Books already mixed: the fixture writes the leg past the gate.
    netted_crew(&l).await;
    let line = l.line("2026-01-10", &[(BANK, "-100"), (IN_CREW, "100")]).await;
    let ids = l.posting_ids(line).await;
    let note = Edit {
        narration: Some("改備註".into()),
        legs: vec![leg(Some(ids[0]), BANK, dec!(-100)), leg(Some(ids[1]), IN_CREW, dec!(100))],
        confirm: false,
    };
    review::edit(&l.pool, &StatementAccounts::default(), line, &note).await.unwrap();
}

#[tokio::test]
async fn an_import_cannot_gate_with_a_leg_in_a_netted_group() {
    let l = Ledger::new().await;
    l.netted("團體", CREW).await;
    let mut db = l.pool.begin().await.unwrap();
    let txn = Transaction {
        date: d(2026, 1, 10),
        payee: None,
        narration: None,
        source: Source::Tiantian,
        external_ref: Some("synthetic:1".into()),
        import_batch_id: None,
        postings: vec![posting(BANK, dec!(-100)), posting(IN_CREW, dec!(100))],
    };
    import::insert_deduped(&mut db, &Labels::default(), &txn).await.unwrap();
    refused_with(check::gate(&mut db).await.unwrap_err(), CREW);
}
