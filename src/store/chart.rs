//! Labels for use outside the account tree, built on
//! [`LedgerData::labels`](super::query::LedgerData::labels) so every label
//! comes from one read of the accounts table.

use std::collections::BTreeMap;

/// Labels for use outside a tree, where no parent row gives context: a label
/// several accounts share takes its parent's in front (甲公司分帳 vs
/// 乙公司分帳), unless the parent is a root. Fava's `uniqueName`
/// (`ledger/MandarinOverview.js`). Keyed by path.
pub fn standalone_labels(labels: &BTreeMap<&str, &str>) -> BTreeMap<String, String> {
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, label) in labels.iter().filter(|(path, label)| path.rsplit(':').next() != Some(**label))
    {
        *count.entry(label).or_default() += 1;
    }
    labels
        .iter()
        .map(|(path, label)| {
            let shared = count.get(label).is_some_and(|n| *n > 1);
            let standalone = match path.rsplit_once(':') {
                Some((parent, _)) if shared && parent.contains(':') => {
                    format!("{}{label}", labels.get(parent).copied().unwrap_or(parent))
                }
                _ => label.to_string(),
            };
            (path.to_string(), standalone)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shared_label_takes_its_parents_in_front() {
        let labels = BTreeMap::from([
            ("Assets", "資產"),
            ("Assets:Split", "分帳"),
            ("Assets:Split:Alpha", "甲公司"),
            ("Assets:Split:Alpha:Tab", "分帳"),
            ("Assets:Split:Beta", "乙公司"),
            ("Assets:Split:Beta:Tab", "分帳"),
            ("Assets:Cash", "現金"),
            // Leaves shown as-is are never "shared".
            ("Assets:Tab", "Tab"),
            ("Assets:Cash:Tab", "Tab"),
        ]);
        let labels = standalone_labels(&labels);
        assert_eq!(labels["Assets:Split:Alpha:Tab"], "甲公司分帳");
        assert_eq!(labels["Assets:Split:Beta:Tab"], "乙公司分帳");
        // Under a root there is no parent to add.
        assert_eq!(labels["Assets:Split"], "分帳");
        assert_eq!(labels["Assets:Cash"], "現金");
        assert_eq!(labels["Assets:Cash:Tab"], "Tab");
    }
}
