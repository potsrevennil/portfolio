//! 天天記帳 exports from invented records. Not every test binary uses every
//! item.
#![allow(dead_code)]

pub const EXPORT_HEADER: &str =
    "日期,類別,大類別,金額,幣別,成員,帳戶,標籤,備註,收支區分,上次更新,UUID\n";
pub const TRANSFER_HEADER: &str =
    "日期,從帳戶,轉出金額,幣別,到帳戶,轉入金額,幣別,標籤,備註,上次更新,UUID\n";

/// A 飲食 spend from `account`.
pub fn flow(date: &str, amount: u32, account: &str, id: &str) -> String {
    memoed(date, amount, account, id, "")
}

pub fn memoed(date: &str, amount: u32, account: &str, id: &str, memo: &str) -> String {
    format!("{date},飲食,食食,{amount},TWD,自己,{account},,{memo},支,{date},{id}\n")
}
