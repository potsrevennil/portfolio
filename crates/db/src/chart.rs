//! Labels for use outside the account tree, built on
//! [`LedgerData::labels`](super::query::LedgerData::labels) so every label
//! comes from one read of the accounts table.

use std::collections::BTreeMap;

/// The label a tree row shows, where the row above gives the context: the
/// chart's, or the path's last segment where it has none.
pub fn tree_label(labels: &BTreeMap<&str, &str>, path: &str) -> String {
    labels
        .get(path)
        .map(|l| l.to_string())
        .unwrap_or_else(|| path.rsplit(':').next().unwrap_or(path).to_string())
}

/// A root's label, or `fallback`, its Taiwanese name, where the chart leaves
/// the root unlabelled (the loader then stores the ASCII name): no heading
/// may fall back to English.
pub fn root_label(labels: &BTreeMap<&str, &str>, root: &str, fallback: &str) -> String {
    match tree_label(labels, root) {
        own if own == root => fallback.to_string(),
        own => own,
    }
}

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
    fn an_unlabelled_root_takes_its_taiwanese_name() {
        let labels =
            BTreeMap::from([("Assets", "Assets"), ("Income", "進項"), ("Assets:Cash", "現金")]);
        assert_eq!(root_label(&labels, "Assets", "資產"), "資產");
        assert_eq!(root_label(&labels, "Income", "收入"), "進項");
        assert_eq!(root_label(&labels, "Expenses", "支出"), "支出");
        assert_eq!(tree_label(&labels, "Assets:Bank"), "Bank");
    }

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
