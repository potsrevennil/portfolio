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
/** Card liability account → its credit limit, from mapping.toml [credit_limits]. */
let creditLimits = {};
/** Last segment → Chinese, for the few places that show only a leaf and give no
 *  other clue. Segments two accounts share are dropped rather than guessed at;
 *  links do not need this, since their href carries the full account. */
let byLeaf = {};
/** Labels two or more accounts share — the same 分帳 under two different groups,
 *  say. Outside a tree, such a label needs its parent's. */
let sharedLabels = new Set();

function index(labels) {
  byAccount = labels;
  const count = {};
  for (const label of Object.values(labels)) count[label] = (count[label] ?? 0) + 1;
  sharedLabels = new Set(Object.keys(count).filter((label) => count[label] > 1));
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

/** The journal's entry-type filter buttons, which Fava labels in English. */
const ENTRY_TYPES = { Open: "開戶", Close: "銷戶", Transaction: "交易", Balance: "餘額", Note: "註記",
  Document: "文件", Pad: "補差", Query: "查詢", Custom: "自訂" };
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTH_NAMES = ["January", "February", "March", "April", "May", "June", "July", "August",
  "September", "October", "November", "December"];
/** English Fava leaves untranslated in zh_Hant_TW: report column names, the tree
 *  tables' "Other" currency column, the journal's entry-type filters, and the month names d3
 *  prints on chart axes — "October", or "Sep 2022" on the bar charts. */
/** Column names of Fava's query-backed reports (資產 / holdings), which it
 *  prints as the raw BQL column names. */
const COLUMNS = { account: "帳戶", units: "數量", cost: "成本", price: "價格", book_value: "帳面價值",
  market_value: "市值", unrealized_profit_pct: "未實現損益%", acquisition_date: "取得日期",
  currency: "幣別", cost_currency: "成本幣別", balance: "餘額", date: "日期" };
function translateChrome() {
  for (const th of document.querySelectorAll("th")) {
    const zh = COLUMNS[th.textContent.trim()];
    if (zh) th.textContent = zh;
  }
  for (const el of document.querySelectorAll("li.head span.num.other")) {
    if (el.textContent.trim() === "Other") el.textContent = "其他";
  }
  for (const button of document.querySelectorAll("form button")) {
    const zh = ENTRY_TYPES[button.textContent.trim()];
    if (zh) button.textContent = zh;
  }
  for (const text of document.querySelectorAll("svg text")) {
    const t = text.textContent.trim();
    // Only exact month names — a chart cell could be an English leaf like "Max".
    const m = t.match(/^([A-Z][a-z]+)(?: (\d{4}))?$/);
    if (!m) continue;
    const month = MONTHS.findIndex((abbr, i) => m[1] === abbr || m[1] === MONTH_NAMES[i]) + 1;
    if (month) text.textContent = m[2] ? `${m[2]}/${month}` : `${month}月`;
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
/** What to show for an account: its label, or — for an account with none, like
 *  Assets:Broker — its own name, never the full ASCII path. Charts and the
 *  journal print full names (the sunburst's centre shows Assets:Split:Friend);
 *  only the account itself is wanted there, as in the tree. */
function displayName(account) {
  return byAccount[account] ?? account.split(":").pop();
}

/** displayName, made unambiguous where no tree shows the parent: a shared label
 *  takes its parent's in front — 甲公司分帳 beside 乙公司分帳. */
function uniqueName(account) {
  const name = displayName(account);
  const parent = account.split(":").slice(0, -1).join(":");
  return sharedLabels.has(name) && parent.includes(":") ? displayName(parent) + name : name;
}

/** A table that lists one row per position — the 資產 (holdings) report — shows
 *  an account once per currency it holds, so a two-currency 外幣 account appears twice. Tell
 *  such rows apart by their currency, named as the cash accounts name it:
 *  外幣（日票）, 外幣（美金）. Built into the label itself, so relabelling stays
 *  idempotent. */
function currencySuffix(link, account) {
  const table = link.closest("table");
  if (!table) return "";
  const rows = [...table.querySelectorAll('a[href*="/account/"]')].filter(
    (a) => decodeURIComponent(a.getAttribute("href").match(/\/account\/([^/?#]+)/)?.[1] ?? "") === account,
  );
  if (rows.length < 2) return "";
  const cell = [...(link.closest("tr")?.querySelectorAll("td") ?? [])]
    .map((td) => td.textContent.trim().match(/\s([A-Z][A-Z0-9]{2,})$/))
    .find(Boolean);
  if (!cell) return "";
  return `（${byAccount[`Assets:Cash:${cell[1]}`] ?? cell[1]}）`;
}

/** The text node a link shows its name in: its own text, or — for the treemap
 *  and sunburst, whose links wrap an SVG <text> — the first text inside it. */
function linkTextNode(link) {
  for (const node of link.childNodes) {
    if (node.nodeType === Node.TEXT_NODE && node.nodeValue.trim()) return node;
  }
  const walker = document.createTreeWalker(link, NodeFilter.SHOW_TEXT);
  while (walker.nextNode()) {
    if (walker.currentNode.nodeValue.trim()) return walker.currentNode;
  }
  return null;
}

/** Tree rows, chart cells and account references are links whose href ends in
 *  /account/<full account>/. Reading the account from there is exact, so two
 *  accounts sharing a leaf — Expenses:Food and Expenses:Travel:Food — are both
 *  relabelled correctly, which guessing from the visible text cannot do. Whether
 *  the link shows a leaf or the full name, it gets the account's display name.
 */
function relabelLinks() {
  const named = new Set();
  for (const link of document.querySelectorAll('a[href*="/account/"]')) {
    const match = link.getAttribute("href").match(/\/account\/([^/?#]+)\/?/);
    if (!match) continue;
    const account = decodeURIComponent(match[1]);
    const node = linkTextNode(link);
    if (!node) continue;
    named.add(node);
    if (link.closest("#mo-holdings")) continue; // named when it was built
    // In a tree the parent row gives the context; elsewhere the name must carry it.
    const name = link.closest("ol.flex-table") ? displayName(account) : uniqueName(account);
    const label = name + currencySuffix(link, account);
    if (label && node.nodeValue.trim() !== label) node.nodeValue = label;
  }
  return named;
}

/** The 資產 (holdings) report is a flat list, one row per position. Show it as a
 *  tree of fold-out groups instead — 現金, 銀行, 分帳 › 公司 … — each summing
 *  its positions per currency, with an account's several currencies on one row
 *  (外幣 1,000 JPY · 5 USD). Fava's own table stays in the page, hidden, as the
 *  data source: rebuilding it in place would fight Svelte, which owns it. */
function groupHoldings() {
  if (!/\/holdings\/$/.test(window.location.pathname)) return;
  const table = document.querySelector("article table, main table, table");
  if (!table) return;
  const rows = [];
  for (const tr of table.querySelectorAll("tbody tr")) {
    const link = tr.querySelector('a[href*="/account/"]');
    // Fava prints negatives with U+2212, not a hyphen.
    const units = tr.querySelectorAll("td")[1]?.textContent.trim().replace("\u2212", "-").match(/^(-?[\d,.]+)\s+(\S+)$/);
    if (!link || !units) continue;
    const account = decodeURIComponent(link.getAttribute("href").match(/\/account\/([^/?#]+)/)[1]);
    if (account.startsWith(INSTALMENTS)) continue; // shown in their own section
    rows.push([account, Number(units[1].replace(/,/g, "")), units[2]]);
  }
  if (!rows.length) return;
  const signature = JSON.stringify(rows) + JSON.stringify(byAccount);
  let view = document.getElementById("mo-holdings");
  if (view?.dataset.signature === signature) return;

  // account tree: node = { account, children: Map, sums: Map(currency → amount) }
  const root = { account: "", children: new Map(), sums: new Map() };
  for (const [account, amount, currency] of rows) {
    let node = root;
    node.sums.set(currency, (node.sums.get(currency) ?? 0) + amount);
    const parts = account.split(":");
    for (let i = 1; i <= parts.length; i++) {
      const name = parts.slice(0, i).join(":");
      if (!node.children.has(name)) node.children.set(name, { account: name, children: new Map(), sums: new Map() });
      node = node.children.get(name);
      node.sums.set(currency, (node.sums.get(currency) ?? 0) + amount);
    }
  }
  const format = (sums) => [...sums].filter(([, v]) => Math.abs(v) > 1e-9)
    .map(([c, v]) => `${v.toLocaleString("en-US", { maximumFractionDigits: 2 })} ${c}`).join(" · ") || "0";
  let open = new Set();
  try { open = new Set(JSON.parse(localStorage.getItem("mo-holdings-open") ?? "[]")); } catch {}
  const base = window.location.pathname.replace(/holdings\/$/, "");
  const render = (node, depth) => {
    const name = document.createElement("a");
    name.href = `${base}account/${node.account}/${window.location.search}`;
    name.textContent = displayName(node.account);
    const amount = document.createElement("span");
    amount.className = "mo-amount";
    amount.textContent = format(node.sums);
    const row = document.createElement("div");
    row.className = "mo-row";
    row.style.paddingLeft = `${depth * 1.3 + 0.6}em`;
    const marker = document.createElement("span");
    marker.className = "mo-marker";
    row.append(marker, name, amount);
    // An account with no sub-accounts is a plain row; one with some folds out.
    if (!node.children.size) return row;
    const group = document.createElement("div");
    const children = document.createElement("div");
    children.append(...[...node.children.values()].map((child) => render(child, depth + 1)));
    const show = (on) => {
      group.classList.toggle("mo-open", on);
      children.hidden = !on;
      marker.textContent = on ? "▾" : "▸";
    };
    show(open.has(node.account));
    row.classList.add("mo-head");
    row.addEventListener("click", (event) => {
      if (event.target.closest("a")) return; // the name still opens the account
      show(!group.classList.contains("mo-open"));
      group.classList.contains("mo-open") ? open.add(node.account) : open.delete(node.account);
      try { localStorage.setItem("mo-holdings-open", JSON.stringify([...open])); } catch {}
    });
    group.append(row, children);
    return group;
  };
  // 資產 and 負債 are headings, always open; the groups under them fold.
  const sections = [...root.children.values()].map((node) => {
    const heading = document.createElement("div");
    heading.className = "mo-row mo-section";
    const name = document.createElement("span");
    name.textContent = displayName(node.account);
    const amount = document.createElement("span");
    amount.className = "mo-amount";
    amount.textContent = format(node.sums);
    heading.append(name, amount);
    return [heading, ...[...node.children.values()].map((child) => render(child, 0))];
  }).flat();
  if (!view) {
    view = document.createElement("div");
    view.id = "mo-holdings";
    const style = document.createElement("style");
    style.textContent = `
      #mo-holdings { max-width: 46em; margin: 0.5em 0 1.5em; border-top: 1px solid var(--table-border, #444); }
      #mo-holdings .mo-row { display: flex; justify-content: space-between; gap: 2em; padding: 0.35em 0.6em;
        border-bottom: 1px solid var(--table-border, #444); cursor: default; }
      #mo-holdings .mo-row > a { margin-right: auto; }
      #mo-holdings .mo-marker { display: inline-block; width: 1.2em; flex: none; }
      #mo-holdings .mo-head { cursor: pointer; font-weight: 600; }
      #mo-holdings .mo-section { font-weight: 700; font-size: 1.05em; margin-top: 0.6em; }
      #mo-holdings .mo-row > :not(.mo-amount) { flex: none; }
      #mo-holdings .mo-amount { text-align: right; }
      #mo-holdings .mo-row:hover { background: var(--table-header-background, rgba(127,127,127,0.12)); }
      #mo-holdings .mo-amount { font-family: var(--font-family-monospaced, monospace); white-space: nowrap; }
      #mo-holdings .mo-tools { display: flex; gap: 1em; justify-content: flex-end; margin-bottom: 0.4em; }
      #mo-holdings .mo-tools button { background: none; border: none; color: var(--link-color, #6af); cursor: pointer; padding: 0; }`;
    document.head.append(style);
    table.before(view);
  }
  const tools = document.createElement("div");
  tools.className = "mo-tools";
  for (const [label, state] of [["全部展開", true], ["全部收合", false]]) {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = label;
    button.addEventListener("click", () => {
      for (const head of view.querySelectorAll(".mo-head")) {
        if (head.parentElement.classList.contains("mo-open") !== state) head.click();
      }
    });
    tools.append(button);
  }
  view.replaceChildren(tools, ...sections);
  view.dataset.signature = signature;
  table.style.setProperty("display", "none", "important");
}

/** The subtree holding instalment plans: one account per plan, the purchase
 *  booked on it in full and each billed instalment a "第n/m期" payment. */
const INSTALMENTS = "Liabilities:Instalments";

/** Runs a BQL query through Fava's own API — the JS half can use it without
 *  the Python half changing, so a running Fava needs no restart. */
async function query(sql) {
  const base = window.location.pathname.replace(/[^/]+\/$/, "");
  const response = await fetch(`${base}api/query?query_string=${encodeURIComponent(sql)}`);
  if (!response.ok) throw new Error(`query failed: ${response.status}`);
  return (await response.json()).data.rows;
}

/** The 資產 page's own 分期 section: each plan's progress rather than a bare
 *  balance in the liability tree — how many instalments are paid, what is
 *  left, when the next one falls due. Paid-off plans fold away below. */
let instalmentsBusy = false;
async function showInstalments() {
  if (!/\/holdings\/$/.test(window.location.pathname) || instalmentsBusy) return;
  const anchor = document.getElementById("mo-holdings");
  if (!anchor) return;
  instalmentsBusy = true;
  try {
    const postings = await query(
      `SELECT date, narration, account, number, currency WHERE account ~ '^${INSTALMENTS}:' ORDER BY date`,
    );
    const payers = await query(
      `SELECT date, narration, account WHERE narration ~ '第[0-9]+/[0-9]+期' AND NOT account ~ '^${INSTALMENTS}'`,
    );
    const cards = Object.keys(creditLimits);
    const balances = cards.length
      ? await query(`SELECT account, sum(number) WHERE account ~ '^(${cards.join("|")})$' GROUP BY account`)
      : [];
    const signature = JSON.stringify(postings) + JSON.stringify(balances) + JSON.stringify(byAccount);
    let view = document.getElementById("mo-instalments");
    if (view?.dataset.signature === signature) return;

    const plans = new Map();
    for (const [date, narration, account, number, currency] of postings) {
      const plan = plans.get(account) ?? { account, currency, total: 0, bought: date, paid: [], of: 0 };
      plans.set(account, plan);
      if (number < 0) {
        plan.total -= number;
        if (date < plan.bought) plan.bought = date;
      } else {
        const m = narration.match(/第(\d+)\/(\d+)期/);
        const payer = payers.find(([d, n]) => d === date && n === narration)?.[2];
        plan.paid.push({ date, amount: number, n: m ? Number(m[1]) : 0, payer });
        if (m) plan.of = Math.max(plan.of, Number(m[2]));
      }
    }
    const money = (v) => v.toLocaleString("en-US", { maximumFractionDigits: 2 });
    // Same day next month, on the ISO string itself — Date would shift it by
    // the local time zone. Clamped to the month's last day (01-31 → 02-28).
    const nextMonth = (iso) => {
      const [y, m, d] = iso.split("-").map(Number);
      const [ny, nm] = m === 12 ? [y + 1, 1] : [y, m + 1];
      const last = new Date(Date.UTC(ny, nm, 0)).getUTCDate();
      return `${ny}-${String(nm).padStart(2, "0")}-${String(Math.min(d, last)).padStart(2, "0")}`;
    };
    const all = [...plans.values()].map((p) => {
      const paid = p.paid.reduce((s, x) => s + x.amount, 0);
      const last = p.paid.at(-1);
      return { ...p, paidSum: paid, left: Math.round((p.total - paid) * 100) / 100, count: p.paid.length,
        payer: last?.payer, next: last && p.total - paid > 0.005 ? nextMonth(last.date) : "" };
    });
    const active = all.filter((p) => p.left > 0.005).sort((a, b) => b.left - a.left);
    const done = all.filter((p) => p.left <= 0.005).sort((a, b) => b.bought.localeCompare(a.bought));
    const base = window.location.pathname.replace(/holdings\/$/, "");
    const cell = (tag, text, cls) => {
      const el = document.createElement(tag);
      if (cls) el.className = cls;
      el.textContent = text;
      return el;
    };
    const row = (p) => {
      const tr = document.createElement("tr");
      const name = document.createElement("a");
      name.href = `${base}account/${p.account}/${window.location.search}`;
      name.textContent = displayName(p.account);
      const nameCell = document.createElement("td");
      nameCell.append(name);
      const bar = document.createElement("td");
      bar.className = "mo-progress";
      const fill = document.createElement("span");
      fill.style.width = `${Math.min(100, (p.paidSum / p.total) * 100)}%`;
      const track = document.createElement("span");
      track.className = "mo-track";
      track.append(fill);
      bar.append(track, ` ${p.count}/${p.of || "?"}`);
      tr.append(nameCell, cell("td", p.payer ? displayName(p.payer) : ""), cell("td", p.bought),
        cell("td", money(p.total), "mo-num"), cell("td", p.paid.length ? money(p.paid.at(-1).amount) : "", "mo-num"),
        bar, cell("td", money(p.left), "mo-num"), cell("td", p.next));
      return tr;
    };
    const table = (list) => {
      const t = document.createElement("table");
      const head = document.createElement("tr");
      for (const h of ["項目", "扣款", "購入", "總額", "每期", "期數", "尚欠", "下期"]) head.append(cell("th", h));
      const thead = document.createElement("thead");
      thead.append(head);
      const tbody = document.createElement("tbody");
      tbody.append(...list.map(row));
      t.append(thead, tbody);
      // Scrolls sideways on a narrow window rather than cutting columns off.
      const wrap = document.createElement("div");
      wrap.className = "mo-scroll";
      wrap.append(t);
      return wrap;
    };
    if (!view) {
      view = document.createElement("section");
      view.id = "mo-instalments";
      const style = document.createElement("style");
      style.textContent = `
        #mo-instalments { max-width: 60em; margin: 1.5em 0; }
        #mo-instalments h3 { display: flex; justify-content: space-between; margin: 0 0 0.5em; }
        #mo-instalments table { width: 100%; border-collapse: collapse; }
        #mo-instalments th, #mo-instalments td { padding: 0.35em 0.6em; border-bottom: 1px solid var(--table-border, #444); text-align: left; white-space: nowrap; }
        #mo-instalments .mo-num { text-align: right; font-family: var(--font-family-monospaced, monospace); }
        #mo-instalments .mo-track { display: inline-block; width: 6em; height: 0.55em; border-radius: 0.3em;
          background: rgba(127,127,127,0.25); vertical-align: middle; overflow: hidden; }
        #mo-instalments .mo-track > span { display: block; height: 100%; background: var(--link-color, #6af); }
        #mo-instalments .mo-done { margin-top: 0.8em; cursor: pointer; color: var(--link-color, #6af); }
        #mo-instalments .mo-scroll { overflow-x: auto; }
        #mo-instalments .mo-note { margin: 0.4em 0 1.2em; opacity: 0.75; font-size: 0.9em; }
        #mo-instalments .mo-credit { margin-bottom: 1.5em; }`;
      document.head.append(style);
      anchor.after(view);
    }
    const title = document.createElement("h3");
    title.append(cell("span", "分期"), cell("span", `尚欠 ${money(active.reduce((s, p) => s + p.left, 0))} ${active[0]?.currency ?? ""}`, "mo-num"));
    const parts = [...creditView(all, balances, cell, money), title, table(active)];
    if (done.length) {
      const toggle = cell("div", `▸ 已繳清（${done.length}）`, "mo-done");
      const doneTable = table(done);
      doneTable.hidden = true;
      toggle.addEventListener("click", () => {
        doneTable.hidden = !doneTable.hidden;
        toggle.textContent = `${doneTable.hidden ? "▸" : "▾"} 已繳清（${done.length}）`;
      });
      parts.push(toggle, doneTable);
    }
    view.replaceChildren(...parts);
    view.dataset.signature = signature;
  } catch (error) {
    console.error("MandarinOverview: instalments", error);
  } finally {
    instalmentsBusy = false;
  }
}

/** Credit used and left on each card, to check against the card's own app.
 *
 * The card account holds only what has been billed or charged outright; an
 * instalment purchase sits in its plan until each part is billed. The bank's
 * used credit counts the whole purchase from day one, so used = what the card
 * owes + every unbilled plan it pays. A plan belongs to the card that pays its
 * instalments; one with nothing billed yet has no card, and is listed apart.
 */
function creditView(plans, balances, cell, money) {
  const cards = Object.entries(creditLimits);
  if (!cards.length) return [];
  const owed = Object.fromEntries(balances.map(([account, sum]) => [account, -Number(sum)]));
  const active = plans.filter((p) => p.left > 0.005);
  const rows = cards.map(([card, limit]) => {
    const unbilled = active.filter((p) => p.payer === card).reduce((s, p) => s + p.left, 0);
    const used = (owed[card] ?? 0) + unbilled;
    return { card, limit: Number(limit), bill: owed[card] ?? 0, unbilled, used, left: Number(limit) - used };
  });
  const t = document.createElement("table");
  const head = document.createElement("tr");
  for (const h of ["卡", "額度", "卡費未繳", "分期未入帳", "已用", "", "可用"]) head.append(cell("th", h));
  const thead = document.createElement("thead");
  thead.append(head);
  const tbody = document.createElement("tbody");
  for (const r of rows) {
    const tr = document.createElement("tr");
    const bar = document.createElement("td");
    bar.className = "mo-progress";
    const track = document.createElement("span");
    track.className = "mo-track";
    const fill = document.createElement("span");
    fill.style.width = `${Math.max(0, Math.min(100, (r.used / r.limit) * 100))}%`;
    track.append(fill);
    bar.append(track, ` ${Math.round((r.used / r.limit) * 100)}%`);
    tr.append(cell("td", displayName(r.card)), cell("td", money(r.limit), "mo-num"), cell("td", money(r.bill), "mo-num"),
      cell("td", money(r.unbilled), "mo-num"), cell("td", money(r.used), "mo-num"), bar, cell("td", money(r.left), "mo-num"));
    tbody.append(tr);
  }
  t.append(thead, tbody);
  const wrap = document.createElement("div");
  wrap.className = "mo-scroll mo-credit";
  wrap.append(t);
  const title = document.createElement("h3");
  const total = rows.reduce((s, r) => s + r.limit, 0);
  title.append(cell("span", "信用額度"), cell("span", `可用 ${money(rows.reduce((s, r) => s + r.left, 0))} / ${money(total)}`, "mo-num"));
  const parts = [title, wrap];
  const unassigned = active.filter((p) => !p.payer);
  if (unassigned.length) {
    parts.push(cell("p", `未算入：${unassigned.map((p) => displayName(p.account)).join("、")}（尚未入帳，不知扣哪張卡）`, "mo-note"));
  }
  return parts;
}

/** Fava titles an account page with its ASCII name. */
function relabelTitle() {
  const title = document.title.replace(/\b(?:Assets|Liabilities|Equity|Income|Expenses)(?::[A-Za-z0-9-]+)*/g, displayName);
  if (title !== document.title) document.title = title;
}

function relabel() {
  if (!Object.keys(byAccount).length) return;
  // A link names its account exactly; leave its text out of the leaf guessing
  // below, which would give Expenses:Gifts the label of Income:Gifts.
  const named = relabelLinks();
  relabelTitle();

  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      // Skip the inputs Fava's filter bar accepts account names in, where the
      // ASCII name is what the user is typing and must survive.
      // The journal's entry-type filters (buttons in a form) name entry types,
      // not accounts — "Transaction" would otherwise take the label of
      // Expenses:Fees:Transaction. translateChrome gives them their own names.
      if (named.has(node) || node.parentElement?.closest("input, textarea, form button")) {
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
    // A full name (a chart tooltip) is shown as its account; a bare leaf gets its label.
    const label = /^(?:Assets|Liabilities|Equity|Income|Expenses):/.test(text) ? displayName(text) : byAccount[text] ?? byLeaf[text];
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
    translateChrome();
    groupHoldings();
    showInstalments();
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
      creditLimits = config.credit_limits ?? {};
      defaultConversion = config.conversion ?? "";
      applyDefaultConversion();
    } catch (error) {
      console.error("MandarinOverview: could not load config", error);
      return;
    }
    // Fava is a single-page app and re-renders after onPageLoad fires, so a
    // one-shot pass would miss most of the content.
    // characterData too: the sunburst's centre text and chart tooltips update a
    // text node in place, which adds no child. Our own edits trigger it as well,
    // but relabelling is idempotent, so the second pass changes nothing and stops.
    new MutationObserver(schedule).observe(document.body, {
      childList: true,
      characterData: true,
      subtree: true,
    });
    schedule();
  },
  onPageLoad() {
    applyDefaultConversion();
    schedule();
  },
};
