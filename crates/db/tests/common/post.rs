//! Transactions entered straight into the tables, for tests that need
//! postings without an importer.

use rust_decimal::Decimal;
use sqlx::SqliteConnection;

/// A two-leg transaction between `account` and Income:Salary, `tags` on the
/// `account` leg; its id.
pub async fn post(
    conn: &mut SqliteConnection,
    date: &str,
    account: &str,
    amount: &str,
    currency: &str,
    tags: Option<&str>,
) -> i64 {
    let txn = sqlx::query("INSERT INTO transactions (date, source) VALUES (?, 'manual')")
        .bind(date)
        .execute(&mut *conn)
        .await
        .unwrap()
        .last_insert_rowid();
    let negated = (-amount.parse::<Decimal>().unwrap()).to_string();
    for (path, amount, tags) in [(account, amount, tags), ("Income:Salary", negated.as_str(), None)]
    {
        sqlx::query(
            "INSERT INTO postings (transaction_id, account_id, amount, currency, tags)
             SELECT ?, id, ?, ?, ? FROM accounts WHERE path = ?",
        )
        .bind(txn)
        .bind(amount)
        .bind(currency)
        .bind(tags)
        .bind(path)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    txn
}
