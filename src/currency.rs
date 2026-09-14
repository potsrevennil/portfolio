//! The currencies the system handles.
//!
//! One shared type across the crate: the portfolio calculator treats it as the
//! closed set it can report in and convert between, and the ledger uses it as
//! the commodity label on a posting. Every currency either side can see must be
//! a variant here — a new one is a single line, and an unknown code fails to
//! parse rather than reaching the ledger unchecked.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use strum_macros::{Display, EnumIter, EnumString};

#[derive(
    ValueEnum,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Deserialize,
    Serialize,
    Default,
    Display,
    EnumIter,
    EnumString,
)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    #[default]
    USD,
    TWD,
    JPY,
    KRW,
    THB,
    VND,
    CNY,
    EUR,
    GBP,
}
