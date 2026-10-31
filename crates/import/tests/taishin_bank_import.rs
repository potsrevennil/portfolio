//! 台新 bank history over records the freeze booked from 天天記帳, on invented
//! figures. What a statement vouches for is `cathay_card_import.rs`'s to test;
//! this holds what a bank's queries add: no stated opening, and a gap.

mod common;

use anyhow::Result;
use common::card::{records_on, Fixture, BANK, BANK_FOLDER};
use db::coverage;
use ledger::statements::bank::Bank;
use rust_decimal_macros::dec;

fn pdf_text(from: &str, to: &str, rows: &str) -> String {
    format!(
        "查詢結果 (幣別 / 新臺幣)資料日期：2026/09/17 \
         17:38:52\n查詢帳號：2222-22-222222\n查詢期間：{from}~{to}\n交易日 帳務日 摘要 支出金額 \
         存入金額 餘額 備註\n{rows}"
    )
}

/// Two queries with days between them, across which a salary arrived. It's
/// dated on the first's last day but on neither, so it waits unverified just
/// past it; the gate holds, the days stay a gap, and a second import adds
/// nothing.
#[tokio::test]
async fn a_gap_between_queries_stays_a_gap_with_the_gate_holding() -> Result<()> {
    let f = Fixture::frozen(&records_on("二銀", &[
        ("2026-01-05", "income", 100, "回饋"),
        ("2026-02-01", "income", 500, "回饋"),
        ("2026-02-20", "expense", 40, "飲食"),
    ]))
    .await?;
    let january = f.pdf(
        BANK_FOLDER,
        "2026a",
        &pdf_text(
            "2026/01/01",
            "2026/02/01",
            "2026/01/05\n10:00:00\n2026/01/05 CD轉入 0 100 100\nATM 000\n",
        ),
    )?;
    let february = f.pdf(
        BANK_FOLDER,
        "2026b",
        &pdf_text(
            "2026/02/18",
            "2026/03/01",
            "2026/02/20\n10:00:00\n2026/02/20\n跨轉手續\n費\n40 0 560\n",
        ),
    )?;
    let paths = [january, february];

    let report = f.import(Bank::Taishin, &paths).await?;

    assert_eq!((report.inserted, report.counts.verified), (0, 2));
    assert!(f.gate_passes().await?);
    let figures = f.figures_of(BANK).await?;
    let closings: Vec<_> = figures.iter().map(|a| (a.period_end.to_string(), a.closing)).collect();
    assert_eq!(closings, [
        ("2026-02-01".to_string(), dec!(100)),
        ("2026-03-01".to_string(), dec!(560))
    ]);
    let gaps = coverage::gaps(&figures);
    let [gap] = gaps.as_slice() else { panic!("{gaps:?}") };
    assert_eq!(
        (gap.from.to_string(), gap.to.to_string()),
        ("2026-02-02".to_string(), "2026-02-17".to_string())
    );

    let again = f.import(Bank::Taishin, &paths).await?;
    assert_eq!((again.inserted, again.counts.known), (0, 2));
    Ok(())
}
