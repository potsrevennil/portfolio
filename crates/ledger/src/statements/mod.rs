//! Bank and broker statement parsers, one module per institution, each
//! yielding the shared [`bank::BankStatement`].

pub mod bank;
pub mod card;
pub mod cathay;
pub mod cathay_card;
pub mod line_bank;
pub mod pdf;
pub mod taishin_card;
pub mod transfer;
