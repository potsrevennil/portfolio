//! Bank and broker statement parsers, one module per institution, each
//! yielding the shared [`bank::BankStatement`].

pub mod bank;
pub mod cathay;
pub mod line_bank;
