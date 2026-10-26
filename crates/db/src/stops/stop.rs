//! The stops themselves: what an import met.

use std::fmt;

use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use strum_macros::IntoStaticStr;

use crate::pairing::Ambiguity;

/// A 天天記帳 record an import could not place, as the export has it: it is
/// not in the ledger yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// `tiantian:<uuid>`.
    pub external_ref: String,
    pub date: NaiveDate,
    /// Where its statement leg books.
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
    /// Where its other legs book: its category, or the account it moves to.
    pub counter: Vec<String>,
    /// Its note.
    pub narration: String,
}

/// Why an import stopped, over what the ledger holds rather than the files.
/// Its `import_stop.kind` word is its variant's name, by `Into<&str>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, IntoStaticStr)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Stop {
    /// A statement line fitting several unverified records, or whose one
    /// record fits another line too. Kept in `verification_choice`.
    LineAmbiguous(Ambiguity),
    /// A record fitting several uncategorised lines, or whose one line fits
    /// another record too.
    RecordAmbiguous { record: Record, lines: Vec<i64> },
    /// A record on a day the statements vouch for, with no uncategorised line
    /// for it. `lines` are the statement lines of its amount it might be all
    /// the same: categorised already, or on a sibling account.
    RecordUnexplained { record: Record, through: NaiveDate, lines: Vec<i64> },
    /// An unverified record the statement passes without a line for it.
    /// `siblings` are the accounts sharing its app account where a line of
    /// this import fits it.
    NotOnStatement {
        transaction_id: i64,
        account: String,
        currency: Currency,
        date: NaiveDate,
        amount: Decimal,
        through: NaiveDate,
        siblings: Vec<String>,
    },
    /// An unverified record from the statement's last days that the bank may
    /// book after it, with a leg on `other`, another statement account:
    /// moving it to the next statement would move that leg too.
    SharedDeferral {
        transaction_id: i64,
        account: String,
        currency: Currency,
        date: NaiveDate,
        amount: Decimal,
        other: String,
    },
    /// Two statements' lines verify one record, each on its own day, and on
    /// neither day do both statements balance.
    TwoDates { transaction_id: i64, dates: Vec<(String, NaiveDate)> },
    /// An account with more than one opening.
    SeveralOpenings { account: String, currency: Currency },
    /// The ledger plus the import would not end a statement day on its
    /// balance: a download starts mid-day, overlaps unevenly, or is missing.
    Unchained {
        account: String,
        currency: Currency,
        day: NaiveDate,
        ledger: Decimal,
        statement: Decimal,
    },
}

impl Stop {
    /// The transactions it names: the lines it may be, or its record.
    pub fn transactions(&self) -> Vec<i64> {
        match self {
            Stop::LineAmbiguous(a) => a.candidates.clone(),
            Stop::RecordAmbiguous { lines, .. } | Stop::RecordUnexplained { lines, .. } => {
                lines.clone()
            }
            Stop::NotOnStatement { transaction_id, .. }
            | Stop::SharedDeferral { transaction_id, .. }
            | Stop::TwoDates { transaction_id, .. } => vec![*transaction_id],
            Stop::SeveralOpenings { .. } | Stop::Unchained { .. } => Vec::new(),
        }
    }

    /// What it is about, so a run meeting it again updates its row.
    pub(super) fn subject(&self) -> String {
        match self {
            Stop::LineAmbiguous(a) => a.statement_ref.clone(),
            Stop::RecordAmbiguous { record, .. } | Stop::RecordUnexplained { record, .. } => {
                record.external_ref.clone()
            }
            Stop::NotOnStatement { transaction_id, .. }
            | Stop::SharedDeferral { transaction_id, .. }
            | Stop::TwoDates { transaction_id, .. } => transaction_id.to_string(),
            Stop::SeveralOpenings { account, currency }
            | Stop::Unchained { account, currency, .. } => {
                format!("{account} {currency}")
            }
        }
    }
}

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stop::LineAmbiguous(a) => write!(
                f,
                "{} {}: the line {} {} and the unverified records it fits ({}) pair ambiguously; \
                 pick one in 待配對",
                a.account,
                a.currency,
                a.date,
                a.amount,
                a.candidates.len()
            ),
            Stop::RecordAmbiguous { record: r, lines } => write!(
                f,
                "{} {} {} on {}: fits {} bank lines; tell them apart in 天天記帳",
                r.external_ref,
                r.date,
                r.amount,
                r.account,
                lines.len()
            ),
            Stop::RecordUnexplained { record: r, through, .. } => write!(
                f,
                "{} {} {} on {}: the statements through {through} show no line for it",
                r.external_ref, r.date, r.amount, r.account
            ),
            Stop::NotOnStatement {
                transaction_id,
                account,
                currency,
                date,
                amount,
                through,
                ..
            } => {
                write!(
                    f,
                    "{account} {currency}: the unverified record {transaction_id} of {amount} on \
                     {date} is not on the statement through {through}"
                )
            }
            Stop::SharedDeferral { account, currency, date, amount, other, .. } => write!(
                f,
                "{account} {currency}: the record of {amount} on {date} is also {other}'s, and \
                 this statement does not show it. Import it together with the next statement, \
                 which may"
            ),
            Stop::TwoDates { transaction_id, dates, .. } => {
                let days: Vec<String> = dates.iter().map(|(a, d)| format!("{d} on {a}")).collect();
                write!(
                    f,
                    "the record {transaction_id} was booked {}, and on neither day do both \
                     statements balance",
                    days.join(" and ")
                )
            }
            Stop::SeveralOpenings { account, currency } => {
                write!(f, "{account} {currency} has more than one opening")
            }
            Stop::Unchained { account, currency, day, ledger, statement } => write!(
                f,
                "{account} {currency}: the ledger would end {day} at {ledger} but the statement \
                 says {statement}. A download starts mid-day, overlaps one already imported \
                 unevenly, or one is missing"
            ),
        }
    }
}

/// An import that stopped: it wrote nothing, and these say why.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused(pub Vec<Stop>);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the import stopped and nothing was imported; fix these, then import again:")?;
        for stop in &self.0 {
            write!(f, "\n  {stop}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Refused {}
