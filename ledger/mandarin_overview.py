"""Fava extension: relabel the ledger's ASCII account names in Chinese.

Beancount account names must be ASCII and Fava renders them verbatim, with no
alias mechanism. This extension ships a JS module whose onPageLoad hook swaps
the names in the DOM on every page, and a `config` endpoint that hands it what
to swap (labels from mapping.toml), which reports to hide, and a default
conversion. It adds no report page of its own — Fava's own reports, relabelled,
are the view.
"""

from __future__ import annotations

import tomllib
from collections import defaultdict
from pathlib import Path

from flask import jsonify

from fava.ext import FavaExtensionBase, extension_endpoint

# Fava reports to drop from the sidebar; the JS removes the links, as Fava has
# no option for it. 通貨 lists every commodity's price history — today only
# exchange-rate charts, noise until securities exist. A slug is the last path
# segment of the report URL.
HIDDEN_REPORTS = ["commodities"]

# Applied on first load by the JS, since Fava has no default-conversion option.
# Without it, charts plot each currency as its own series and invite comparing
# units that cannot be compared.
DEFAULT_CONVERSION = "TWD"


class MandarinOverview(FavaExtensionBase):
    """Relabels Fava's built-in reports in Chinese; adds no page of its own."""

    # Ships MandarinOverview.js, whose onPageLoad hook runs on every Fava page —
    # the only way to reach the built-in reports, whose account names come
    # straight from the data with no alias mechanism.
    has_js_module = True

    @extension_endpoint
    def config(self):
        """What the JS module needs: labels to swap, reports to hide, card limits."""
        self._load_labels()
        labels = {}
        for account in set(self._derived) | set(self._explicit):
            label = self._label(account)
            if label != account.rsplit(":", 1)[-1]:
                labels[account] = label
        return jsonify(
            {
                "labels": labels,
                "hide": HIDDEN_REPORTS,
                "conversion": DEFAULT_CONVERSION,
                "credit_limits": self._credit_limits,
            }
        )

    def _load_labels(self) -> None:
        path = Path(self.ledger.beancount_file_path).parent / "mapping.toml"
        data = tomllib.loads(path.read_text(encoding="utf-8"))

        # Every category that maps to an account, and — separately — those mapped
        # without tags. A tagged category is a sub-classification (咖啡 within
        # Expenses:Food); an untagged one names the account in general (交通 →
        # Expenses:Transport). See `_label`.
        derived: dict[str, list[str]] = defaultdict(list)
        plain: dict[str, list[str]] = defaultdict(list)
        for section in ("expenses", "income", "accounts"):
            for zh, target in data.get(section, {}).items():
                if isinstance(target, str):
                    account, tagged = target, False
                else:
                    account, tagged = target.get("account", ""), bool(target.get("tags"))
                if not account:
                    continue
                if zh not in derived[account]:
                    derived[account].append(zh)
                if not tagged and zh not in plain[account]:
                    plain[account].append(zh)
        self._derived = derived
        self._plain = plain
        self._explicit = data.get("display", {})
        self._credit_limits = data.get("credit_limits", {})

    def _label(self, account: str) -> str:
        """The account's Chinese label.

        An explicit [display] name wins. Otherwise a single category names the
        account outright (薪資 → Income:Salary). When several share it, only an
        untagged category names it in general — 交通 → Expenses:Transport, versus
        the tagged 機車停車錢 within it — so the sole untagged one is the label.
        An account several tagged categories share with no general name (咖啡,
        食材, 飲食 → Expenses:Food) would be mislabelled by any of them — clicking
        咖啡 would open every Food record — so it falls to [display] or its leaf.
        """
        if account in self._explicit:
            return self._explicit[account]
        names = self._derived.get(account, [])
        if len(names) == 1:
            return names[0]
        plain = self._plain.get(account, [])
        if len(plain) == 1:
            return plain[0]
        return account.rsplit(":", 1)[-1]
