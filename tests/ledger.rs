//! Per-record corrections to the 天天記帳 import.

use std::io::Write;

use portfolio::ledger::{accounts::Chart, daily};
use rust_decimal_macros::dec;
use tempfile::NamedTempFile;

/// The sections `Chart::load` insists on. They name accounts the build reaches
/// by role, so a config missing them would otherwise produce a ledger full of
/// empty account names and fail far from the cause.
const REQUIRED: &str = r#"
[institution]
app_account = "銀行"
primary = "Assets:Bank:Savings"
settlement = "Assets:Bank:Investment"
settlement_app_account = "證券"
clearing = "Assets:Bank:Clearing"
[institution.accounts]
"123456789012" = "Assets:Bank:Savings"
[fallback]
income = "Income:Uncategorized"
expense = "Expenses:Uncategorized"
"#;

fn temp(contents: &str) -> anyhow::Result<NamedTempFile> {
    let mut file = NamedTempFile::new()?;
    file.write_all(contents.as_bytes())?;
    Ok(file)
}

/// Corrections are keyed by the UUID in the export's twelfth column. Nothing
/// else in the row identifies it, and if that index ever drifts the correction
/// stops applying silently — the record just reverts to its app category.
#[test]
fn flow_records_carry_their_app_uuid() -> anyhow::Result<()> {
    let income_expense = temp(concat!(
        "日期,類別,大類別,金額,幣別,成員,帳戶,標籤,備註,收支區分,上次更新,UUID\n",
        "20240101,其他,,250,USD,自己,交易所,,,收,2024-01-01 00:00:00,",
        "00000000-0000-4000-8000-000000000001\n",
    ))?;
    let transfers = temp("日期,從帳戶,轉出金額,幣別,到帳戶,轉入金額,幣別,標籤,備註,上次更新,UUID\n")?;

    let entries = daily::load_entries(
        income_expense.path().to_str().unwrap(),
        transfers.path().to_str().unwrap(),
    )?;

    let [daily::Entry::Flow { id, category, .. }] = entries.as_slice() else {
        panic!("expected one 收支 record, got {:?}", entries);
    };
    assert_eq!(id, "00000000-0000-4000-8000-000000000001");
    assert_eq!(category, "其他");
    Ok(())
}

/// The subject's view of a record must keep the currency the record was written
/// in. The parser this replaced never read 幣別, so a foreign-currency record
/// against a TWD-statement account was booked as TWD — silently, because the
/// two legs still balanced against each other.
#[test]
fn the_subject_view_keeps_each_side_in_its_own_currency() -> anyhow::Result<()> {
    let income_expense = temp(concat!(
        "日期,類別,大類別,金額,幣別,成員,帳戶,標籤,備註,收支區分,上次更新,UUID\n",
        "20240101,其他,,500,USD,自己,銀行,,,收,2024-01-01 00:00:00,A\n",
    ))?;
    let transfers = temp(concat!(
        "日期,從帳戶,轉出金額,幣別,到帳戶,轉入金額,幣別,標籤,備註,上次更新,UUID\n",
        "20240417,銀行,3000,TWD,交易所,100.00,USD,,,2024-04-17 00:00:00,B\n",
    ))?;

    let entries = daily::load_entries(
        income_expense.path().to_str().unwrap(),
        transfers.path().to_str().unwrap(),
    )?;

    let bank = daily::view(&entries, "銀行");
    let [flow, sent] = &bank[..] else {
        panic!("expected the bank to see two records, got {bank:?}");
    };
    assert_eq!(flow.currency, "USD", "收支 currency is dropped");
    assert_eq!(flow.far, None, "a category has no far side");
    assert_eq!(sent.currency, "TWD", "the sending side is in what it sent");
    assert_eq!(sent.far, Some((dec!(100.00), "USD".into())), "far side lost");

    // The same row, seen from the other account: the two sides swap.
    let exchange = daily::view(&entries, "交易所");
    let [received] = &exchange[..] else {
        panic!("expected the exchange to see one record, got {exchange:?}");
    };
    assert_eq!(received.delta, dec!(100.00));
    assert_eq!(received.currency, "USD");
    assert_eq!(received.far, Some((dec!(3000), "TWD".into())));
    Ok(())
}

/// An empty account means "deliberately not corrected", matching how the
/// category tables treat one.
#[test]
fn blank_override_account_is_ignored() -> anyhow::Result<()> {
    let mapping = temp(&format!(
        "{REQUIRED}\n\
         [overrides]\n\
         \"KEEP\" = {{ account = \"Expenses:Investment:Loss\", narration = \"虧損\" }}\n\
         \"DROP\" = {{ account = \"\" }}\n"
    ))?;

    let chart = Chart::load(mapping.path().to_str().unwrap())?;
    let kept = chart.override_for("KEEP").expect("KEEP is corrected");
    assert_eq!(&*kept.account, "Expenses:Investment:Loss");
    assert_eq!(kept.narration, "虧損");
    assert!(chart.override_for("DROP").is_none());
    assert!(chart.override_for("ABSENT").is_none());
    Ok(())
}

/// A config that names no statement accounts cannot produce a ledger, so it
/// fails at load with the field named rather than deep inside the build.
#[test]
fn a_config_missing_required_accounts_is_rejected() -> anyhow::Result<()> {
    let mapping = temp("[expenses]\n\"飲食\" = \"Expenses:Food\"\n")?;
    let error = Chart::load(mapping.path().to_str().unwrap())
        .expect_err("a config with no institution should not load");
    assert!(
        format!("{error:#}").contains("institution.app_account"),
        "error should name the missing field, got: {error:#}"
    );
    Ok(())
}

/// An account field now carries a validated `Account`, not a bare string, so a
/// value that is not a Beancount account fails at load with the offending name
/// rather than sailing through to bean-check as an unopened account far away.
#[test]
fn a_non_beancount_account_is_rejected_at_load() -> anyhow::Result<()> {
    let mapping = temp(&REQUIRED.replace(
        r#"expense = "Expenses:Uncategorized""#,
        r#"expense = "Spending:Uncategorized""#,
    ))?;
    let error = Chart::load(mapping.path().to_str().unwrap())
        .expect_err("a non-Beancount root should not load");
    let shown = format!("{error:#}");
    assert!(shown.contains("Spending:Uncategorized"), "error should name the value, got: {shown}");
    assert!(shown.contains("Assets"), "error should name the roots allowed, got: {shown}");
    Ok(())
}

/// The shipped example is the only documentation of the config format, and it
/// is what a new setup gets copied from. If the code grows a required field and
/// the example is not updated, this fails rather than the user's first run.
#[test]
fn the_example_config_is_valid() -> anyhow::Result<()> {
    let chart = Chart::load("ledger/mapping.example.toml")?;
    assert!(!chart.institution.accounts.is_empty(), "example names no statement account");
    assert!(!chart.expenses.is_empty(), "example maps no expense category");
    Ok(())
}
