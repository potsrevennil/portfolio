//! Broker statement lines as an importer stores them.

use db::broker::NewRecord;
use ledger_types::currency::Currency;
use portfolio::broker::{BrokerRecord, RecordKind};
use rust_decimal::Decimal;

/// A USD line on `account` that settles on `settle` and moves nothing, keyed
/// `test:<key>`; callers set what it moves.
pub fn line(account: &str, kind: RecordKind, settle: &str, key: &str) -> NewRecord {
    NewRecord {
        account: account.into(),
        external_ref: format!("test:{key}"),
        import_batch_id: None,
        record: BrokerRecord {
            kind,
            trade_date: None,
            settle_date: settle.parse().unwrap(),
            executed_at: None,
            symbol: None,
            quantity: Decimal::ZERO,
            price: Decimal::ZERO,
            amount: Decimal::ZERO,
            commission: Decimal::ZERO,
            currency: Currency::USD,
            description: "invented".into(),
            key: key.into(),
        },
    }
}
