//! What a build produced, for the caller to report.

use std::{collections::BTreeSet, fmt, path::PathBuf};

use chrono::NaiveDate;

/// What a run produced, for the caller to report.
pub struct Summary {
    /// The file `build` wrote; `None` when the model was only assembled.
    pub output: Option<PathBuf>,
    /// Date of the earliest record, when records were given.
    pub records_start: Option<NaiveDate>,
    /// Statement lines booked before `records_start`, folded into the
    /// statement's opening balance.
    pub folded: usize,
    /// Currency conversions paired across two statements (a subset of the
    /// internal transfers).
    pub converted: usize,
    /// Declared `[opening_balances]` (as "account currency") left out because
    /// a statement opens that account and currency itself.
    pub superseded_openings: BTreeSet<String>,
    /// First date covered by a statement; everything earlier is backfill.
    pub anchor: NaiveDate,
    pub backfilled: usize,
    pub categorised: usize,
    pub internal: usize,
    pub in_transit: usize,
    /// Debits the bank itself reversed (錯誤更正), each netted with its
    /// reversal.
    pub reversed: usize,
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
        if let Some(output) = &self.output {
            writeln!(f, "wrote {}", output.display())?;
        }
        if let (Some(start), true) = (self.records_start, self.folded > 0) {
            writeln!(
                f,
                "  {} statement lines before the records begin ({start}) folded into opening \
                 balances",
                self.folded
            )?;
        }
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
        if self.converted > 0 {
            writeln!(f, "  {} of the internal transfers are currency conversions", self.converted)?;
        }
        if self.reversed > 0 {
            writeln!(f, "  {} bank reversals netted against the debit they undid", self.reversed)?;
        }
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
        if !self.superseded_openings.is_empty() {
            writeln!(
                f,
                "  declared opening balances superseded by a statement: {:?}",
                self.superseded_openings
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
