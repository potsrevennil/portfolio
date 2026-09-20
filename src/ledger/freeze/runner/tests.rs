//! Unit tests for the freeze tool's pure transformation logic. The end-to-end
//! freeze → journal → load pipeline is exercised in `tests/freeze_journal.rs`.

use rust_decimal_macros::dec;

use super::*;

/// A stand-in date for the balancing tests, which do not depend on it.
fn day() -> NaiveDate { NaiveDate::from_ymd_opt(2024, 1, 1).unwrap() }

fn explicit(account: &str, amount: Decimal, currency: Currency) -> Posting {
    Posting::new(account, amount, currency)
}

#[test]
fn a_balanced_single_currency_transaction_needs_no_conversion() {
    let postings = balance_postings(
        &[
            explicit("Expenses:Food", dec!(120), Currency::TWD),
            explicit("Assets:Cash", dec!(-120), Currency::TWD),
        ],
        day(),
        &None,
        None,
    )
    .expect("balances");
    assert_eq!(postings.len(), 2, "no conversion leg should be added");
    assert!(postings.iter().all(|p| p.account != CONVERSIONS));
}

#[test]
fn two_postings_on_one_account_are_both_kept() {
    // A 錯誤更正 (bank error correction) reversal books a debit and its
    // correction on the same bank account in one transaction, netting to zero.
    // The balancer must keep both legs, not merge or reject them.
    let postings = balance_postings(
        &[
            explicit("Assets:Cathay:Savings", dec!(-500), Currency::TWD),
            explicit("Assets:Cathay:Savings", dec!(500), Currency::TWD),
        ],
        day(),
        &None,
        None,
    )
    .expect("balances");
    assert_eq!(postings.len(), 2, "both same-account legs are kept");
    assert!(postings.iter().all(|p| p.account == "Assets:Cathay:Savings"));
    assert!(postings.iter().all(|p| p.account != CONVERSIONS), "same currency needs no plug");
}

#[test]
fn a_cross_currency_transfer_is_plugged_through_conversions() {
    let postings = balance_postings(
        &[
            explicit("Assets:Cash", dec!(-300), Currency::TWD),
            explicit("Assets:USD-Wallet", dec!(10), Currency::USD),
        ],
        day(),
        &None,
        None,
    )
    .expect("balances");
    // Two originals plus one conversion leg per currency.
    assert_eq!(postings.len(), 4);
    let mut per_currency: BTreeMap<Currency, Decimal> = BTreeMap::new();
    for p in &postings {
        *per_currency.entry(p.currency).or_default() += p.amount;
    }
    assert!(per_currency.values().all(|v| v.is_zero()), "every currency must net to zero");
    assert_eq!(postings.iter().filter(|p| p.account == CONVERSIONS).count(), 2);
}

#[test]
fn a_single_currency_imbalance_is_rejected_not_masked() {
    let err = balance_postings(
        &[
            explicit("Expenses:Food", dec!(100), Currency::TWD),
            explicit("Assets:Cash", dec!(-50), Currency::TWD),
        ],
        day(),
        &None,
        None,
    )
    .expect_err("a genuine imbalance must fail loudly");
    assert!(format!("{err:#}").contains("does not balance"), "got: {err:#}");
}

#[test]
fn an_inferred_leg_is_filled_from_the_others() {
    let postings = balance_postings(
        &[explicit("Expenses:Food", dec!(100), Currency::TWD), Posting::inferred("Assets:Cash")],
        day(),
        &None,
        None,
    )
    .expect("balances");
    let cash = postings.iter().find(|p| p.account == "Assets:Cash").unwrap();
    assert_eq!(cash.amount, dec!(-100));
    assert_eq!(cash.currency, Currency::TWD);
}

#[test]
fn a_securities_posting_is_tagged_as_placeholder() {
    let postings = balance_postings(
        &[
            explicit("Assets:Broker:Holdings", dec!(3000), Currency::TWD),
            explicit("Assets:Cathay:Savings", dec!(-3000), Currency::TWD),
        ],
        day(),
        &Some("investment".into()),
        Some("Assets:Broker:Holdings"),
    )
    .expect("balances");
    let etf = postings.iter().find(|p| p.account == "Assets:Broker:Holdings").unwrap();
    let tags = etf.tags.as_deref().unwrap();
    assert!(tags.contains(PLACEHOLDER_TAG), "placeholder marker missing: {tags}");
    assert!(tags.contains("investment"), "transaction tag lost: {tags}");
    let savings = postings.iter().find(|p| p.account == "Assets:Cathay:Savings").unwrap();
    assert!(!savings.tags.as_deref().unwrap().contains(PLACEHOLDER_TAG));
}

#[test]
fn subtree_balance_covers_descendants_and_respects_the_cutoff() {
    let leg = |account: &str, amount: Decimal, year: i32| journal::Posting {
        group: 0,
        source: model::Source::Manual,
        date: NaiveDate::from_ymd_opt(year, 1, 1).unwrap(),
        payee: None,
        narration: String::new(),
        external_ref: None,
        account: account.into(),
        amount,
        currency: Currency::TWD,
        tags: None,
    };
    let movements = vec![
        leg("Assets:Cash", dec!(1000), 2022),
        leg("Assets:Cash", dec!(500), 2023),
        leg("Assets:Cash:Petty", dec!(25), 2025),
    ];
    // Subtree, no cutoff: parent plus descendant.
    let all = subtree_balance(&movements, "Assets:Cash", None, true);
    assert_eq!(all.get(&Currency::TWD), Some(&dec!(1525)));
    // A cutoff excludes the 2025 movement (start-of-day: strictly before).
    let mid = subtree_balance(
        &movements,
        "Assets:Cash",
        Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
        true,
    );
    assert_eq!(mid.get(&Currency::TWD), Some(&dec!(1500)));
    // Exact account only: the descendant is excluded.
    let exact = subtree_balance(&movements, "Assets:Cash", None, false);
    assert_eq!(exact.get(&Currency::TWD), Some(&dec!(1500)));
}

#[test]
fn tag_merging_covers_every_combination() {
    assert_eq!(merge_tags(&None, None), None);
    assert_eq!(merge_tags(&Some("a".into()), None), Some("a".into()));
    assert_eq!(merge_tags(&None, Some("b".into())), Some("b".into()));
    assert_eq!(merge_tags(&Some("a".into()), Some("b".into())), Some("a,b".into()));
}

#[test]
fn placeholder_and_subtree_predicates_match_only_the_right_paths() {
    // The root is the chart's securities account, whatever it is named.
    let root = Some("Assets:Broker:Holdings");
    assert!(is_placeholder("Assets:Broker:Holdings:ETF", root));
    assert!(is_placeholder("Assets:Broker:Holdings", root));
    assert!(!is_placeholder("Assets:Broker:Holdings-Old", root));
    assert!(!is_placeholder("Assets:Cash", root));
    // A chart that maps no securities account tags nothing.
    assert!(!is_placeholder("Assets:Broker:Holdings", None));
    assert!(in_subtree("Assets:Cash:Petty", "Assets:Cash"));
    assert!(in_subtree("Assets:Cash", "Assets:Cash"));
    assert!(!in_subtree("Assets:Cashew", "Assets:Cash"));
}

#[test]
fn an_account_path_parses_to_its_root() {
    assert_eq!("Assets:Cash:Petty".parse(), Ok(AccountType::Assets));
    assert_eq!("Liabilities".parse(), Ok(AccountType::Liabilities));
    assert_eq!("Nonsense:Root".parse::<AccountType>(), Err(()));
}

#[test]
fn two_inferred_legs_are_rejected() {
    let err = balance_postings(
        &[
            explicit("Expenses:Food", dec!(100), Currency::TWD),
            Posting::inferred("Assets:Cash"),
            Posting::inferred("Assets:Wallet"),
        ],
        day(),
        &None,
        None,
    )
    .expect_err("which leg takes the remainder is ambiguous");
    assert!(format!("{err:#}").contains("more than one inferred"), "got: {err:#}");
}

#[test]
fn an_inferred_leg_in_a_multi_currency_transaction_is_rejected() {
    let err = balance_postings(
        &[
            explicit("Assets:Cash", dec!(-300), Currency::TWD),
            explicit("Assets:USD-Wallet", dec!(10), Currency::USD),
            Posting::inferred("Expenses:Fees"),
        ],
        day(),
        &None,
        None,
    )
    .expect_err("the inferred leg has no single currency to take");
    assert!(format!("{err:#}").contains("multi-currency"), "got: {err:#}");
}

/// Two transactions under one (source, external_ref) cannot be loaded, so the
/// freeze refuses to write them rather than leaving the loader to fail.
#[test]
fn a_repeated_dedup_key_is_refused() {
    let txn = |account: &str| model::Transaction {
        date: day(),
        payee: String::new(),
        narration: String::new(),
        tags: Vec::new(),
        postings: vec![
            explicit(account, dec!(100), Currency::TWD),
            explicit("Assets:Cash", dec!(-100), Currency::TWD),
        ],
        source: model::Source::Tiantian,
        external_ref: Some("U-1".to_string()),
    };
    let model = model::Model {
        cathay: vec![
            model::Directive::Transaction(txn("Expenses:Food")),
            model::Directive::Transaction(txn("Expenses:Travel")),
        ],
        ..model::Model::default()
    };

    let err = reconcile(&model, &[], None).expect_err("one key, two transactions");
    assert!(format!("{err:#}").contains("(tiantian, U-1)"), "got: {err:#}");
}
