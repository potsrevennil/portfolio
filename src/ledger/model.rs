//! The assembled ledger as an in-memory model.
//!
//! `build` computes this; the Beancount text is *serialised from it* (see
//! `render`), and the freeze tool *consumes it directly* — no code re-parses
//! the importer's own output. Keeping one authoritative structure is the point:
//! the text is a view, not a second source of truth.
//!
//! A [`Directive`] is deliberately close to Beancount's own grammar (comments,
//! opens, transactions, balance assertions, blank lines) so `render` is a
//! straight serialisation and the model still carries everything a reader
//! needs. Transactions additionally carry [`Source`] and `external_ref`, which
//! the text has no place for but the schema does.

use std::fmt::Write as _;

use chrono::NaiveDate;
use rust_decimal::Decimal;

use super::writer::{self, Posting};
use crate::currency::Currency;

/// Which pipeline produced a transaction — the `transactions.source` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Bank-statement import, and the derived history that makes it reconcile.
    Import,
    /// Frozen 天天記帳 history.
    Tiantian,
    /// Hand-entered cash from `manual.csv`.
    Manual,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Import => "import",
            Source::Tiantian => "tiantian",
            Source::Manual => "manual",
        }
    }
}

/// A transaction: a header, its legs, and the schema-only provenance fields.
#[derive(Debug)]
pub struct Transaction {
    pub date: NaiveDate,
    pub payee: String,
    pub narration: String,
    pub tags: Vec<String>,
    pub postings: Vec<Posting>,
    pub source: Source,
    /// A stable id for the source row (bank line identity, 天天記帳 UUID), or
    /// `None` where the source has no per-row id. Dedup key for later imports.
    pub external_ref: Option<String>,
}

/// A `balance` assertion — a reconciliation target.
#[derive(Debug)]
pub struct Balance {
    pub date: NaiveDate,
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
}

/// One line of a generated file, in emission order.
#[derive(Debug)]
pub enum Directive {
    /// A blank separator line.
    Blank,
    /// A `;;` comment line (the leading `;;` is part of the text).
    Comment(String),
    /// `YYYY-MM-DD open <account>` — the fixed epoch date the importer uses.
    Open(String),
    Transaction(Transaction),
    Balance(Balance),
}

/// The whole assembled ledger, grouped as the three generated files expect. The
/// grouping is a rendering convenience; the freeze tool reads across all of it.
#[derive(Default)]
pub struct Model {
    /// `cathay.beancount` body: the backfill, the statement blocks, and the
    /// 國泰 records no statement matched.
    pub cathay: Vec<Directive>,
    /// `daily.beancount` transactions: accounts with no statement.
    pub daily: Vec<Directive>,
    /// `daily.beancount` balance assertions computed from 天天記帳.
    pub asserts: Vec<Directive>,
    /// `accounts.beancount` opening-balance transactions (declared openings).
    pub openings: Vec<Directive>,
    /// Every account to emit an `open` for.
    pub opens: std::collections::BTreeSet<String>,
}

impl Model {
    /// Every transaction across all groups, in a stable order.
    pub fn transactions(&self) -> impl Iterator<Item = &Transaction> {
        self.cathay.iter().chain(&self.daily).chain(&self.openings).filter_map(|d| match d {
            Directive::Transaction(t) => Some(t),
            _ => None,
        })
    }

    /// Every balance assertion across all groups.
    pub fn balances(&self) -> impl Iterator<Item = &Balance> {
        self.cathay.iter().chain(&self.daily).chain(&self.asserts).filter_map(|d| match d {
            Directive::Balance(b) => Some(b),
            _ => None,
        })
    }
}

/// Serialises a directive stream to Beancount text — used by `build` to write
/// the generated files, and the only place the model becomes text.
pub fn render(directives: &[Directive]) -> String {
    let mut out = String::new();
    for directive in directives {
        match directive {
            Directive::Blank => out.push('\n'),
            Directive::Comment(text) => {
                out.push_str(text);
                out.push('\n');
            }
            Directive::Open(account) => {
                let _ = writeln!(out, "2000-01-01 open {account}");
            }
            Directive::Transaction(t) => {
                out.push_str(&writer::transaction(
                    t.date,
                    &t.payee,
                    &t.narration,
                    &t.tags,
                    &t.postings,
                ));
            }
            Directive::Balance(b) => {
                out.push_str(&writer::balance(b.date, &b.account, b.amount, b.currency));
            }
        }
    }
    out
}
