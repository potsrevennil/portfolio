//! Cathay TWD exports as the bank writes them, from invented lines. Not every
//! test binary uses every item.
#![allow(dead_code)]

/// A line as (date, 說明, 提出, 存入, 餘額, 交易資訊, 備註), oldest first.
pub type Line<'a> = (&'a str, &'a str, &'a str, &'a str, &'a str, &'a str, &'a str);

/// A TWD export: BOM, CRLF, the lone-quote spacer record, newest line first.
pub fn twd(account: &str, lines: &[Line]) -> String { period(account, None, lines) }

/// The same, with the `(自 … 至 …)` header the bank puts on a dated download:
/// it vouches through the day before `period_end`.
pub fn period(account: &str, period_end: Option<&str>, lines: &[Line]) -> String {
    let header = period_end
        .map(|end| format!("\"筆數\",\"(自 2020/01/01 至 {end})\"\r\n"))
        .unwrap_or_default();
    // The lone-quote spacer record, then the column header.
    let columns = ["交易日期", "帳務日期", "說明", "提出", "存入", "餘額", "交易資訊", "備註"]
        .map(|c| format!("\"{c}\""))
        .join(",");
    let mut out =
        format!("\u{feff}\"{account} 活存\"\r\n{header}\"幣別：TWD\"\r\n\"\r\n\"\r\n{columns}\r\n");
    for (date, desc, out_, in_, bal, info, memo) in lines.iter().rev() {
        out.push_str(&format!(
            "\"{date}\",\"{date}\",\"{desc}\",\"{out_}\",\"{in_}\",\"{bal}\",\"{info}\",\"{memo}\"\
             \r\n"
        ));
    }
    out
}
