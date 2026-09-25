//! 天天記帳 records exported after the freeze, booked by the rules freeze
//! books them with, for the interim importer (T4c). Pure: whether a record's
//! statement leg pairs with a line already held, or waits for one, is the
//! importer's call against the database.

use std::{collections::BTreeSet, slice};

use anyhow::{bail, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use super::{
    accounts::Chart,
    daily::{self, Entry},
    emit::{daily_transaction, record_on},
    model::{self, OPENING_EQUITY},
};

/// Namespaces a record's UUID in the shared dedup scope.
pub const REF_PREFIX: &str = "tiantian:";

pub fn external_ref(id: &str) -> String { format!("{REF_PREFIX}{id}") }

/// Where a record lands on a statement account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementLeg {
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
}

#[derive(Debug)]
pub enum Booking {
    /// Touches no statement account: booked as written.
    Standalone(model::Transaction),
    /// Touches one statement account at `leg`. Untagged; the importer tags it
    /// unverified when no line explains it yet.
    OnStatement { leg: StatementLeg, transaction: model::Transaction },
    /// A transfer between two statement accounts: the statements book both
    /// halves themselves.
    LeftToStatements,
}

#[derive(Debug)]
pub struct Booked {
    pub id: String,
    pub date: NaiveDate,
    /// From the 轉帳 export rather than 收支.
    pub transfer: bool,
    pub booking: Booking,
}

#[derive(Debug, Default)]
pub struct Books {
    pub records: Vec<Booked>,
    /// App labels no mapping names; their records went to a fallback.
    pub unmapped: BTreeSet<String>,
}

/// Books every record that moves something, each with its
/// `tiantian:<uuid>` ref.
pub fn book(chart: &Chart, entries: &[Entry]) -> Result<Books> {
    let institution = &chart.institution;
    // The app labels a statement stands behind, as freeze pools them.
    let statement_labels: BTreeSet<&str> = institution
        .accounts
        .values()
        .map(|a| Ok(chart.app_account_for(a)?.unwrap_or(&institution.app_account)))
        .collect::<Result<_>>()?;
    let is_equity = |name: &str| chart.account(name).is_some_and(|m| &*m.account == OPENING_EQUITY);

    let mut books = Books::default();
    let (mut used_accounts, mut used_overrides) = (BTreeSet::new(), BTreeSet::new());
    for entry in entries {
        let (id, transfer) = match entry {
            Entry::Flow { id, .. } => (id, false),
            Entry::Transfer { id, .. } => (id, true),
        };
        if id.is_empty() {
            bail!("a record on {} has no UUID, so it could not be told apart later", entry.date());
        }
        if entry.accounts().into_iter().any(is_equity) {
            bail!("record {id} is an opening; openings belong to the frozen history");
        }
        let touched: Vec<&str> =
            entry.accounts().into_iter().filter(|a| statement_labels.contains(a)).collect();
        let booking = match touched.as_slice() {
            [] => daily_transaction(
                chart,
                entry,
                &mut used_accounts,
                &mut books.unmapped,
                &mut used_overrides,
            )
            .map(Booking::Standalone),
            [label] => match daily::view(slice::from_ref(entry), label).as_slice() {
                [event] if !event.delta.is_zero() => {
                    let settles = matches!(&event.contra,
                        daily::Contra::Account(n) if *n == institution.settlement_app_account);
                    let near: &str = match (*label == institution.app_account, settles) {
                        (true, true) => &institution.settlement,
                        (true, false) => &institution.primary,
                        (false, _) => &chart.account(label).expect("a statement label").account,
                    };
                    let transaction = record_on(
                        chart,
                        event,
                        near,
                        String::new(),
                        false,
                        &mut books.unmapped,
                        &mut used_overrides,
                    );
                    let leg = StatementLeg {
                        account: near.to_string(),
                        amount: event.delta,
                        currency: event.currency,
                    };
                    Some(Booking::OnStatement { leg, transaction })
                }
                [_] => None,
                // A transfer from the account to itself.
                _ => Some(Booking::LeftToStatements),
            },
            _ => Some(Booking::LeftToStatements),
        };
        if let Some(mut booking) = booking {
            if let Booking::Standalone(t) | Booking::OnStatement { transaction: t, .. } =
                &mut booking
            {
                t.external_ref = Some(external_ref(id));
            }
            books.records.push(Booked { id: id.clone(), date: entry.date(), transfer, booking });
        }
    }
    Ok(books)
}

#[cfg(test)]
mod tests;
