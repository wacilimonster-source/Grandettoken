// 在 Google Cloud 独立窗口读取可见赠金表，不读取 Cookie、令牌或页面内部对象。
(() => {
  if (window.top !== window || location.hostname !== "console.cloud.google.com") return;
  const nonce = "__CAPTURE_NONCE__";
  let sent = false;
  let previous = "";
  let stableAt = 0;
  function collect() {
    const account = location.pathname.match(/^\/billing\/([A-Fa-f0-9-]{20})\/credits\/all\/?$/);
    if (!account || new URL(location.href).searchParams.get("hl") !== "en") return null;
    const tables = [...document.querySelectorAll('table[role="treegrid"]')];
    const table = tables.find(t => t.querySelector('[data-column-id="remainingValue"]'));
    if (!table || table.getAttribute("aria-busy") === "true") return null;
    // 用户设置过筛选或翻页时保留上次值，防止把部分行当作账户总额。
    if (document.querySelector('cfc-chip,cfc-filter-chip')) return null;
    const paging = [...document.querySelectorAll('button[aria-label]')].find(b =>
      /next page|previous page/i.test(b.getAttribute("aria-label")) && !b.disabled && b.getAttribute("aria-disabled") !== "true");
    if (paging) return null;
    const headerEls = [...table.querySelectorAll('thead th')];
    const headers = headerEls.map(c => c.getAttribute("data-column-id"));
    const required = ["creditId", "creditName", "status", "remainingValue", "scope"];
    if (required.some(k => !headers.includes(k))) return null;
    const endDateIds = ["endDate", "expirationDate", "expiryDate", "endTime", "validUntil"];
    let endDateIndex = headers.findIndex(id => endDateIds.includes(id));
    if (endDateIndex < 0) {
      endDateIndex = headers.findIndex(id => typeof id === "string" && /^(credit)?(end|expir\w*)(date|time)$/i.test(id));
    }
    if (endDateIndex < 0) {
      endDateIndex = headerEls.findIndex(th => {
        const text = (th.innerText || th.textContent || "").replace(/\s+/g, " ").trim();
        return /^(end\s*date|expiration\s*date|expires?(\s+on)?|结束日期|到期日期)(\b|$)/i.test(text);
      });
    }
    const rows = [...table.querySelectorAll('tbody tr[role="row"]')].map(row => {
      const cells = [...row.querySelectorAll('td')];
      const read = key => (cells[headers.indexOf(key)]?.innerText || "").trim();
      const endDate = endDateIndex >= 0 ? (cells[endDateIndex]?.innerText || "").trim() : "";
      return { id: read("creditId"), name: read("creditName"), status: read("status"),
        amount: read("remainingValue"), scope: read("scope"), endDate };
    });
    if (!rows.length || rows.some(r => !r.id || !r.status)) return null;
    const count = Number(table.getAttribute("aria-rowcount"));
    if (count > rows.length + 1) return null;
    return { account: account[1].toUpperCase(), rows };
  }
  const timer = setInterval(() => {
    if (sent) { clearInterval(timer); return; }
    const data = collect();
    if (!data) { previous = ""; return; }
    const encoded = JSON.stringify(data);
    if (encoded !== previous) { previous = encoded; stableAt = Date.now(); return; }
    if (Date.now() - stableAt < 2500) return;
    sent = true;
    const bytes = new TextEncoder().encode(encoded);
    const payload = btoa(Array.from(bytes, b => String.fromCharCode(b)).join(""));
    location.href = `https://grandettoken.invalid/google-cloud/${nonce}#${payload}`;
  }, 1000);
})();
