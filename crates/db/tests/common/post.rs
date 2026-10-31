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

/// Says `account`'s leg of `txn` posted on `date`, as a card statement does.
pub async fn posted_on(conn: &mut SqliteConnection, txn: i64, account: &str, date: &str) {
    sqlx::query(
        "UPDATE postings SET posted_date = ? WHERE transaction_id = ? AND account_id =
         (SELECT id FROM accounts WHERE path = ?)",
    )
    .bind(date)
    .bind(txn)
    .bind(account)
    .execute(conn)
    .await
    .unwrap();
}
