//! What a build produced, for the caller to report.

use std::{collections::BTreeSet, fmt, path::PathBuf};

use chrono::NaiveDate;

/// What a run produced, for the caller to report.
pub struct Summary {
    pub output: PathBuf,
    /// First date covered by a statement; everything earlier is backfill.
    pub anchor: NaiveDate,
    pub backfilled: usize,
    pub categorised: usize,
    pub internal: usize,
    pub in_transit: usize,
    pub uncategorised: usize,
    /// Records emitted for accounts that have no statement.
    pub other_accounts: usize,
    /// Per-account balance assertions checking the ledger against 天天記帳.
    pub balance_assertions: usize,
    /// 天天記帳 records touching 國泰 that no statement line matched. Their far
    /// side is still emitted, but the near side lands in an uncategorised
    /// bucket — a rising count means the matcher is losing ground.
    pub unmatched_records: usize,
    /// 天天記帳 names with no entry in mapping.toml.
    pub unmapped: BTreeSet<String>,
    /// [overrides] ids that matched no record in the exports. A correction is
    /// keyed by a 36-character UUID, so a mistyped one silently corrects
    /// nothing; this is the only thing that would say so.
    pub stale_overrides: BTreeSet<String>,
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "wrote {}", self.output.display())?;
        if self.backfilled > 0 {
            writeln!(
                f,
                "  {} pre-{} records from 天天記帳 (derived opening balance)",
                self.backfilled, self.anchor
            )?;
        }
        writeln!(
            f,
            "  {} categorised from 天天記帳, {} internal transfers paired, {} in transit via \
             clearing, {} uncategorised",
            self.categorised, self.internal, self.in_transit, self.uncategorised
        )?;
        if self.other_accounts > 0 {
            writeln!(
                f,
                "  {} records for accounts with no statement, from 天天記帳",
                self.other_accounts
            )?;
        }
        if self.balance_assertions > 0 {
            writeln!(
                f,
                "  {} balance assertions checking the ledger against 天天記帳",
                self.balance_assertions
            )?;
        }
        if self.unmatched_records > 0 {
            writeln!(
                f,
                "  {} 天天記帳 records matched no statement line (far side kept, near side \
                 uncategorised)",
                self.unmatched_records
            )?;
        }
        if !self.unmapped.is_empty() {
            writeln!(f, "  unmapped names in mapping.toml: {:?}", self.unmapped)?;
        }
        if !self.stale_overrides.is_empty() {
            writeln!(
                f,
                "  [overrides] ids matching no record, correcting nothing: {:?}",
                self.stale_overrides
            )?;
        }
        Ok(())
    }
}
