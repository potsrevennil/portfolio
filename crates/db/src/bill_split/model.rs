//! A split and what follows from its shares alone.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use strum_macros::{Display, EnumString};

/// The `share.role` vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Display, EnumString, Serialize, Deserialize)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// What a person owes for this split, or is owed where they fronted it.
    Share,
    Repayment,
    /// 冇數, on `Expenses:Bad-Debt`.
    WriteOff,
    /// Your own share, on a line with no split leg.
    Own,
}

/// Netted or per-person, chosen per split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    /// `Split:A`, `Split:Gear:A`: each person's part is followed to settlement.
    PerPerson,
    /// `Split:X`: only your net with the group matters.
    Netted { account: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub id: i64,
    pub name: String,
    pub accounts: Vec<String>,
    /// What they owe you over all their accounts; negative, you owe them.
    pub balance: BTreeMap<Currency, Decimal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    pub id: i64,
    pub posting_id: i64,
    pub transaction_id: i64,
    pub date: NaiveDate,
    pub narration: Option<String>,
    pub role: Role,
    pub person: Option<i64>,
    pub account: String,
    /// This split's part of the posting, signed like it.
    pub amount: Decimal,
    pub currency: Currency,
    /// The whole posting: more than `amount` where the rest went elsewhere.
    pub posting_amount: Decimal,
    pub rewrote: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    pub id: i64,
    pub title: String,
    pub shape: Shape,
    /// Oldest first.
    pub shares: Vec<Share>,
    /// A netted split's account balance; empty for a per-person one.
    pub balance: BTreeMap<Currency, Decimal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Open,
    Partial,
    Settled,
    /// Netted: only `Split::balance` says where it stands.
    BalanceOnly,
}

/// How a split came to zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closing {
    /// Repaid in full.
    Repaid,
    /// Written off with nothing repaid.
    WrittenOff,
    /// Part repaid, the rest written off.
    PartlyWrittenOff,
    /// Its shares cancel: nothing was repaid or written off.
    Offset,
}

/// A split that has closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Closed {
    pub split: Split,
    pub how: Closing,
    pub on: NaiveDate,
}

impl Split {
    /// What each person still owes here, by currency; negative, you owe them.
    pub fn owed(&self) -> BTreeMap<(i64, Currency), Decimal> {
        let mut owed = BTreeMap::new();
        for share in &self.shares {
            if let Some(person) = share.person {
                *owed.entry((person, share.currency)).or_default() += share.amount;
            }
        }
        owed
    }

    pub fn people(&self) -> BTreeSet<i64> { self.shares.iter().filter_map(|s| s.person).collect() }

    fn settling(&self) -> bool {
        self.shares.iter().any(|s| matches!(s.role, Role::Repayment | Role::WriteOff))
    }

    pub fn status(&self) -> Status {
        let owed = self.owed();
        match &self.shape {
            Shape::Netted { .. } => Status::BalanceOnly,
            Shape::PerPerson if !owed.is_empty() && owed.values().all(Decimal::is_zero) => {
                Status::Settled
            }
            Shape::PerPerson if self.settling() => Status::Partial,
            Shape::PerPerson => Status::Open,
        }
    }

    /// The newest share's day.
    pub fn last_date(&self) -> Option<NaiveDate> { self.shares.iter().map(|s| s.date).max() }

    /// How and when it closed, or `None` while it is open. A netted split
    /// closes when its account's balance is zero, so it can open again.
    pub fn closing(&self) -> Option<(Closing, NaiveDate)> {
        let closed = match &self.shape {
            Shape::PerPerson => self.status() == Status::Settled,
            Shape::Netted { .. } => {
                self.shares.iter().any(|s| s.role != Role::Own)
                    && self.balance.values().all(Decimal::is_zero)
            }
        };
        let of = |role: Role| self.shares.iter().filter(move |s| s.role == role).map(|s| s.date);
        let how = match (of(Role::Repayment).next(), of(Role::WriteOff).next()) {
            (Some(_), None) => Closing::Repaid,
            (None, Some(_)) => Closing::WrittenOff,
            (Some(_), Some(_)) => Closing::PartlyWrittenOff,
            (None, None) => Closing::Offset,
        };
        let on = of(Role::Repayment).chain(of(Role::WriteOff)).max().or(self.last_date());
        match (closed, on) {
            (true, Some(on)) => Some((how, on)),
            _ => None,
        }
    }
}

/// How much of a posting with `left` unallocated repays `owed`, signed like the
/// posting: `asked`, or all it can where `None`. Anything over `owed` stays on
/// the account, unallocated, since the money really did arrive.
pub(super) fn allocate(left: Decimal, owed: Decimal, asked: Option<Decimal>) -> Result<Decimal> {
    if left.is_zero() || owed.is_zero() || left.is_sign_negative() == owed.is_sign_negative() {
        bail!("這筆不是還款：和欠的方向相同，或者已經沒有欠");
    }
    let most = left.abs().min(owed.abs());
    match asked {
        None if left.is_sign_negative() => Ok(-most),
        None => Ok(most),
        Some(a) if a.is_zero() || a.is_sign_negative() != left.is_sign_negative() => {
            bail!("還款金額的方向和這筆不同")
        }
        Some(a) if a.abs() > most => bail!("還款金額超過這筆或還欠的 {most}"),
        Some(a) => Ok(a),
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn share(person: i64, role: Role, amount: Decimal, day: u32) -> Share {
        Share {
            id: 0,
            posting_id: 0,
            transaction_id: 0,
            date: NaiveDate::from_ymd_opt(2026, 1, day).unwrap(),
            narration: None,
            role,
            person: Some(person),
            account: "Assets:Split:A".into(),
            amount,
            currency: Currency::TWD,
            posting_amount: amount,
            rewrote: false,
        }
    }

    fn per_person(shares: Vec<Share>) -> Split {
        Split {
            id: 1,
            title: "t".into(),
            shape: Shape::PerPerson,
            shares,
            balance: BTreeMap::new(),
        }
    }

    #[test]
    fn status_follows_the_shares() {
        assert_eq!(per_person(vec![]).status(), Status::Open);
        let open = vec![share(1, Role::Share, dec!(500), 1), share(2, Role::Share, dec!(300), 1)];
        assert_eq!(per_person(open.clone()).status(), Status::Open);

        let mut partial = open;
        partial.push(share(1, Role::Repayment, dec!(-500), 2));
        assert_eq!(per_person(partial.clone()).status(), Status::Partial);

        partial.push(share(2, Role::WriteOff, dec!(-300), 3));
        let settled = per_person(partial);
        assert_eq!(settled.status(), Status::Settled);
        let (how, on) = settled.closing().unwrap();
        assert_eq!(how, Closing::PartlyWrittenOff);
        assert_eq!(on, NaiveDate::from_ymd_opt(2026, 1, 3).unwrap());
    }

    #[test]
    fn shares_that_cancel_close_without_a_repayment() {
        let split = per_person(vec![
            share(1, Role::Share, dec!(500), 1),
            share(1, Role::Share, dec!(-500), 4),
        ]);
        assert_eq!(split.status(), Status::Settled);
        assert_eq!(split.closing().unwrap().0, Closing::Offset);
    }

    #[test]
    fn a_netted_split_is_balance_only_and_closes_on_its_balance() {
        let mut split = per_person(vec![share(1, Role::Share, dec!(500), 1)]);
        split.shape = Shape::Netted { account: "Assets:Split:Gear".into() };
        split.balance = BTreeMap::from([(Currency::TWD, dec!(500))]);
        assert_eq!(split.status(), Status::BalanceOnly);
        assert_eq!(split.closing(), None);

        split.balance = BTreeMap::from([(Currency::TWD, dec!(0))]);
        assert_eq!(split.closing().unwrap().0, Closing::Offset);

        // Your own share alone owes nobody: nothing has closed.
        split.shares = vec![Share { person: None, ..share(1, Role::Own, dec!(500), 1) }];
        assert_eq!(split.closing(), None);
    }

    #[test]
    fn allocating_stops_at_the_smaller_side() {
        assert_eq!(allocate(dec!(-1000), dec!(600), None).unwrap(), dec!(-600));
        assert_eq!(allocate(dec!(-300), dec!(600), None).unwrap(), dec!(-300));
        // Where they fronted it, you repay them: the signs turn over.
        assert_eq!(allocate(dec!(500), dec!(-800), None).unwrap(), dec!(500));
        assert_eq!(allocate(dec!(-1000), dec!(600), Some(dec!(-200))).unwrap(), dec!(-200));
    }

    #[test]
    fn allocating_refuses_what_is_no_repayment() {
        assert!(allocate(dec!(500), dec!(600), None).is_err());
        assert!(allocate(dec!(-500), dec!(0), None).is_err());
        assert!(allocate(dec!(-500), dec!(600), Some(dec!(-501))).is_err());
        assert!(allocate(dec!(-500), dec!(600), Some(dec!(100))).is_err());
    }
}
