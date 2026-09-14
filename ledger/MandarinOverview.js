/**
 * Relabels Beancount account names in Chinese across Fava's built-in reports.
 *
 * Beancount account names must be ASCII and Fava renders them verbatim — it has
 * no alias, display-name metadata or stylesheet hook. But an extension's
 * onPageLoad runs on every page, so the names can be swapped in the DOM after
 * render. This is display only: the ledger, the URLs and every link still use
 * the real ASCII names, so nothing downstream is affected.
 */

/** Full account name → Chinese, e.g. "Assets:Cathay:Savings" → "活存". */
let byAccount = {};
/** Report slugs to remove from the sidebar. */
let hidden = [];
/** Last segment → Chinese, for the few places that show only a leaf and give no
 *  other clue. Segments two accounts share are dropped rather than guessed at;
 *  links do not need this, since their href carries the full account. */
let byLeaf = {};

function index(labels) {
  byAccount = labels;
  const seen = {};
  for (const [account, label] of Object.entries(labels)) {
    const leaf = account.split(":").pop();
    if (leaf in seen && seen[leaf] !== label) {
      seen[leaf] = null; // ambiguous — two accounts, different labels
    } else {
      seen[leaf] = label;
    }
  }
  byLeaf = Object.fromEntries(
    Object.entries(seen).filter(([, label]) => label !== null),
  );
}

/** A hidden report is still reachable by URL, and Fava will happily render it —
 *  which is how 通貨 kept appearing after its link was removed. Send it back to
 *  the default page so hiding actually means hidden.
 */
function leaveHiddenReport() {
  const path = window.location.pathname;
  if (!hidden.some((slug) => path.endsWith(`/${slug}/`))) return false;
  // Everything before the report name is the ledger's own root.
  window.location.replace(path.replace(/[^/]+\/$/, ""));
  return true;
}

/** Fava has no option for hiding reports, so the links go here.
 *
 * The `hidden` attribute alone is not enough: Fava's stylesheet sets an explicit
 * `display` on these elements, which beats the browser's default
 * `[hidden] { display: none }`. So set display directly, with !important, rather
 * than relying on a rule that loses the cascade.
 */
function hideReports() {
  for (const slug of hidden) {
    for (const link of document.querySelectorAll(`a[href$="/${slug}/"]`)) {
      const item = link.closest("li") ?? link;
      item.style.setProperty("display", "none", "important");
    }
  }
}

/** The user prefers the short forms 年/季/月. Fava shows 年度/季度/月 from its
 *  zh_Hant_TW catalogue; shorten them in the control. Fava binds each option's
 *  value in code, not from its text, so relabelling the text does not change
 *  which interval a click selects. */
const INTERVAL_SHORT = { 年度: "年", 季度: "季" };
function shortenIntervalLabels() {
  for (const el of document.querySelectorAll('button[role="combobox"], li[role="option"]')) {
    const short = INTERVAL_SHORT[el.textContent.trim()];
    if (short) el.textContent = short;
  }
}

/** The chart's conversion selector offers 按照成本／市價／單位 (at cost / market /
 *  units) beside 轉為 <currency>. For a ledger of cash held in many currencies,
 *  the first three plot the raw per-currency numbers — so VND, counted in
 *  millions, draws a huge bar even when its converted value is tiny, comparing
 *  amounts that are not comparable. Hide them and leave only the converted
 *  views, the only ones prefixed 轉為. (Revisit once securities exist: at-market
 *  value means something then.) */
function pruneConversions() {
  const list = [...document.querySelectorAll('li[role="option"]')]
    .find((li) => li.textContent.trim().startsWith("轉為"))?.parentElement;
  if (!list) return;
  for (const option of list.querySelectorAll('li[role="option"]')) {
    if (!option.textContent.trim().startsWith("轉為")) {
      option.style.setProperty("display", "none", "important");
    }
  }
}
/** Tree rows and account references are links whose href ends in
 *  /account/<full account>/. Reading the account from there is exact, so two
 *  accounts sharing a leaf — Assets:Store and Liabilities:Store — are both
 *  relabelled correctly, which guessing from the visible text cannot do.
 */
function relabelLinks() {
  for (const link of document.querySelectorAll('a[href*="/account/"]')) {
    const match = link.getAttribute("href").match(/\/account\/([^/?#]+)\/?/);
    if (!match) continue;
    const label = byAccount[decodeURIComponent(match[1])];
    if (!label) continue;
    for (const node of link.childNodes) {
      if (node.nodeType === Node.TEXT_NODE && node.nodeValue.trim()) {
        node.nodeValue = label;
        break;
      }
    }
  }
}

function relabel() {
  if (!Object.keys(byAccount).length) return;
  relabelLinks();

  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      // Skip the inputs Fava's filter bar accepts account names in, where the
      // ASCII name is what the user is typing and must survive.
      if (node.parentElement?.closest("input, textarea")) {
        return NodeFilter.FILTER_REJECT;
      }
      return NodeFilter.FILTER_ACCEPT;
    },
  });

  const edits = [];
  while (walker.nextNode()) {
    const node = walker.currentNode;
    const text = node.nodeValue.trim();
    if (!text || !/^[\x20-\x7E]+$/.test(text)) continue; // already Chinese, or not a name
    const label = byAccount[text] ?? byLeaf[text];
    if (label && label !== text) {
      edits.push([node, node.nodeValue.replace(text, label)]);
    }
  }
  // Applied after walking so the tree is not mutated mid-traversal. Replacing
  // ASCII with Chinese makes this idempotent: a second pass finds no matches.
  for (const [node, value] of edits) node.nodeValue = value;
}

let scheduled = false;
function schedule() {
  if (scheduled) return;
  scheduled = true;
  requestAnimationFrame(() => {
    scheduled = false;
    if (leaveHiddenReport()) return;
    relabel();
    hideReports();
    shortenIntervalLabels();
    pruneConversions();
  });
}

/** Default every report to a converted view, so none is shown at raw
 *  per-currency values.
 *
 * Fava defaults to "at cost", which for a ledger of cash in many currencies
 * stacks incomparable units — VND, counted in millions, dwarfs everything on a
 * chart. Fava has no option for a default conversion, so add one to the URL
 * whenever a report has none. An explicit choice (轉為 TWD / USD) is a parameter
 * in the URL and is left alone; only a report with no conversion is redirected.
 * Runs on every page, because Fava does not carry the parameter onto a report
 * opened directly rather than through a sidebar link.
 */
let defaultConversion = "";
function applyDefaultConversion() {
  if (!defaultConversion) return;
  const url = new URL(window.location.href);
  if (url.searchParams.has("conversion")) return;
  // Conversion is a report parameter; an extension route does not take it and
  // Fava bounces to a default report instead, losing the page asked for.
  if (url.pathname.includes("/extension/")) return;
  url.searchParams.set("conversion", defaultConversion);
  window.location.replace(url.toString());
}

export default {
  async init(context) {
    try {
      const config = await context.api.get("config");
      index(config.labels ?? {});
      hidden = config.hide ?? [];
      defaultConversion = config.conversion ?? "";
      applyDefaultConversion();
    } catch (error) {
      console.error("MandarinOverview: could not load config", error);
      return;
    }
    // Fava is a single-page app and re-renders after onPageLoad fires, so a
    // one-shot pass would miss most of the content.
    new MutationObserver(schedule).observe(document.body, {
      childList: true,
      subtree: true,
    });
    schedule();
  },
  onPageLoad() {
    applyDefaultConversion();
    schedule();
  },
};
