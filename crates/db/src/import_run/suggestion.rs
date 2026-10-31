//! What an import asks a person to look at on one transaction.

use std::fmt;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Suggestion {
    /// Booked to a fallback: nothing names its category.
    Uncategorised,
    /// Several fit, a line or a record of the same amount; it took the one
    /// nearest in date.
    PickedNearest,
    /// Waits for a statement to show it, or for a transfer's other half.
    Waiting,
    /// A statement passed its day without showing it. `siblings` are the
    /// accounts sharing its app account that have a line it fits.
    NotOnStatement { through: NaiveDate, siblings: Vec<String> },
    /// Booked on a pooled account, but a sibling's statement shows it: its leg
    /// moved to `to`.
    Moved { from: String, to: String },
    /// One of several openings on its account: the earliest counts for the
    /// balance chain.
    ExtraOpening,
}

impl fmt::Display for Suggestion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Suggestion::Uncategorised => f.write_str("uncategorised"),
            Suggestion::PickedNearest => {
                f.write_str("several of this amount fit; paired with the nearest in date")
            }
            Suggestion::Waiting => f.write_str("waits for its statement line"),
            Suggestion::NotOnStatement { through, siblings } if siblings.is_empty() => {
                write!(f, "not on the statement through {through}")
            }
            Suggestion::NotOnStatement { through, siblings } => write!(
                f,
                "not on the statement through {through}; a line on {} fits",
                siblings.join(", ")
            ),
            Suggestion::Moved { from, to } => write!(f, "moved from {from} to {to}"),
            Suggestion::ExtraOpening => f.write_str("a further opening on its account"),
        }
    }
}
