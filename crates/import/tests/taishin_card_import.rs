//! 台新 card statements over records the freeze booked from 天天記帳, on
//! invented figures. What a statement vouches for is `cathay_card_import.rs`'s
//! to test; this holds that a PDF's rows reach the same place.

mod common;

use std::path::PathBuf;

use anyhow::Result;
use common::{
    card::{records, Fixture},
    pdf::fake_pdftotext,
};
use ledger::statements::bank::Bank;
use ledger_types::assertion::AssertionSource;
use rust_decimal_macros::dec;

fn statement(closes: &str, opening: u32, closing: u32, rows: &str) -> String {
    format!(
        "帳單結帳日 {closes}\n上期應繳總額 {opening}\n-已繳退款總額 0\n=前期餘額 \
         0\n=本期累計應繳金額 {closing}\n消費日 入帳起息日消費明細 新臺幣金額 外幣折算日 消費地 \
         幣別 外幣金額\n{rows}台端依契約得選擇\n"
    )
}

/// Writes the statement's text, and gives the PDF path it stands in for.
fn pdf(f: &Fixture, name: &str, text: &str) -> Result<PathBuf> {
    fake_pdftotext();
    f.write(&format!("{name}.txt"), text)?;
    Ok(f.dir.path().join("testcard").join(format!("{name}.pdf")))
}

#[tokio::test]
async fn a_statement_vouches_for_records_and_its_closing_is_checked() -> Result<()> {
    let f = Fixture::frozen(&records(&[
        ("2026-01-05", "expense", 100, "飲食"),
        ("2026-01-08", "expense", 53, "飲食"),
    ]))
    .await?;
    let january = pdf(
        &f,
        "2026-01",
        &statement(
            "115/01/20",
            0,
            183,
            "115/01/05 115/01/08 咖啡店TAIPEI 100 TW\n115/01/08 115/01/10\n範例\n便當?\n53 \
             TW\n115/01/09 115/01/12 無記錄 30 TW\n",
        ),
    )?;

    let report = f.import(Bank::TaishinCard, std::slice::from_ref(&january)).await?;

    assert_eq!((report.counts.verified, report.inserted), (2, 1));
    let legs = f.legs().await?;
    assert_eq!(legs[0].1.as_deref(), Some("2026-01-08"));
    assert_eq!(legs[1].1.as_deref(), Some("2026-01-10"));
    let reference: String =
        sqlx::query_scalar("SELECT external_ref FROM transactions WHERE date = '2026-01-09'")
            .fetch_one(&f.pool)
            .await?;
    assert!(reference.starts_with("taishin-card:testcard:2026-01-12:"), "{reference}");
    let figures = f.figures().await?;
    let [figure] = figures.as_slice() else { panic!("{figures:?}") };
    assert_eq!((figure.source, figure.closing), (AssertionSource::Statement, dec!(-183)));
    assert!(f.gate_passes().await?);

    let again = f.import(Bank::TaishinCard, &[january]).await?;
    assert_eq!((again.inserted, again.counts.known), (0, 3));
    Ok(())
}
