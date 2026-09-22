//! Broker statements parsed into typed records, one per statement line, with
//! the balances each statement states.

pub mod cathay;
pub mod firstrade;
pub mod ib;
pub mod record;

pub use record::{replay, BrokerRecord, BrokerStatement, Commodity, Holdings, RecordKind};
