/* AI 优惠官网的结构化列表与卡片摘要。 */
const PROMO_PAGES = 3;
function promoPageUrl(page) {
  return "https://www.aipromonow.com/deals" + (page > 1 ? "?page=" + page : "");
}
function parsePromoPage(html, page = 1) {
  const origin = "https://www.aipromonow.com";
  // 只在惰性模板里读文本，远程脚本和资源不会进入应用页面。
  const template = document.createElement("template");
  template.innerHTML = html;
  const lists = [...template.content.querySelectorAll('script[type="application/ld+json"]')]
    .flatMap(script => JSON.parse(script.textContent))
    .filter(data => data?.["@type"] === "ItemList" && data.url === promoPageUrl(page));
  const list = lists[0];
  if (lists.length !== 1 || !Array.isArray(list.itemListElement) ||
      !list.itemListElement.length || list.itemListElement.length !== list.numberOfItems) {
    throw Error("invalid promo list");
  }
  const ids = new Set();
  return list.itemListElement.map(item => {
    const title = item?.name?.trim();
    const link = new URL(item?.url);
    // 官网中文标题的 slug 是百分号编码的，卡片 id 却用解码后的字面；
    // 校验与查摘要都按解码后的路径，id/链接仍保留编码原样以免破坏已读迁移。
    let pathname;
    try { pathname = decodeURIComponent(link.pathname); } catch (_) { throw Error("invalid promo item"); }
    const slug = /^\/promos\/([\p{L}\p{N}-]+)$/u.exec(pathname)?.[1];
    if (!title || !slug || link.origin !== origin || link.username || link.password ||
        link.search || link.hash || ids.has(link.href)) {
      throw Error("invalid promo item");
    }
    ids.add(link.href);
    const card = template.content.getElementById("promo-" + slug);
    const description = card?.querySelector("p")?.textContent.trim() || "";
    return { id: link.href, link: link.href, title, description };
  });
}
// 官网末页之后返回 404，所以按首页自己给出的分页链接决定取到第几页。
function promoPageCount(html) {
  const template = document.createElement("template");
  template.innerHTML = html;
  const pages = [...template.content.querySelectorAll('a[href^="/deals?page="]')]
    .map(a => Number(/^\/deals\?page=(\d+)$/.exec(a.getAttribute("href"))?.[1]))
    .filter(n => Number.isInteger(n) && n > 1);
  return Math.max(1, ...pages);
}
// 任一页失败就整体抛出，调用方保留上次完整快照。翻页期间上游新发布造成的重复条目按首次出现保留。
async function loadPromoList() {
  const first = await window.GT_API.promos.fetchPromoPage({ page: 1 });
  const pages = Math.min(PROMO_PAGES, promoPageCount(first));
  const rest = await Promise.all(Array.from({ length: pages - 1 }, (_, i) => window.GT_API.promos.fetchPromoPage({ page: i + 2 })));
  const ids = new Set();
  return [first, ...rest].flatMap((html, i) => parsePromoPage(html, i + 1)).filter(item => {
    if (ids.has(item.id)) return false;
    ids.add(item.id); return true;
  });
}
/* AI 优惠：列表摘要与本机已读状态。 */
function initPromoFeed() {
  const key = "promo-feed-v1", PAGE_SIZE = 24;
  let state = { items: [], seen: [], updated: 0, initialized: false };
  try { state = { ...state, ...JSON.parse(localStorage.getItem(key) || "{}") }; } catch (_) { /* silent: 本地已读状态损坏就当作全新会话 */ }
  let busy = false, failed = "", query = "", scroll = 0, shown = PAGE_SIZE;
  const expanded = new Set();
  const save = () => { try { localStorage.setItem(key, JSON.stringify(state)); } catch (_) { /* silent: 已读标记存不下只影响下次的"新条目"计数 */ } };
  const unread = item => !state.seen.includes(item.id);
  // 看过列表就算读过:打开列表时把当前条目全部记入 seen,底栏的「N 条新」随之清零。
  const markAllSeen = () => {
    if (state.items.some(unread)) { state.seen = state.items.map(x => x.id); save(); }
  };
  const date = value => { const d = new Date(value); return Number.isNaN(d.getTime()) ? "" : d.toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" }); };
  function entry() {
    const count = state.items.filter(unread).length;
    $("promoEntry").textContent = "AI 优惠" + (count ? " · " + (count > 99 ? "99+" : count) + " 条新" : failed ? " · 更新失败" : "");
    $("promoEntry").classList.toggle("has-new", count > 0);
    $("promoEntry").title = "查看 AI 工具优惠" + (state.updated ? " · 上次更新 " + date(state.updated) : "");
  }
  function render() {
    if (!$("promoOverlay").hidden) markAllSeen();
    entry();
    if ($("promoOverlay").hidden) return;
    const content = $("promoContent");
    $("promoHeading").textContent = "AI 优惠";
    $("promoRefresh").disabled = busy;
    $("promoRefresh").textContent = busy ? "更新中…" : "刷新";
    const list = state.items.filter(x => (x.title + " " + x.description).toLowerCase().includes(query.toLowerCase())), visible = list.slice(0, shown);
    content.innerHTML = `<div class="promo-tools"><input id="promoSearch" type="search" placeholder="搜索优惠或工具名称" value="${esc(query)}" aria-label="搜索优惠"></div>${failed ? `<p class="promo-meta">更新失败 · ${esc(failed)}${state.updated ? " · 上次更新 " + esc(date(state.updated)) : ""}</p>` : ""}<div class="promo-list">${visible.map(x => `<article class="promo-item" data-id="${esc(x.id)}"><button class="promo-title" data-open="${esc(x.id)}">${esc(x.title)} ↗</button><p class="promo-text${expanded.has(x.id) ? " expanded" : ""}">${esc(x.description)}</p><button class="promo-toggle" data-toggle="${esc(x.id)}" aria-expanded="${expanded.has(x.id)}" hidden>${expanded.has(x.id) ? "收起" : "展开"}</button>${x.date ? `<small class="promo-date">发布于 ${esc(date(x.date))}</small>` : ""}</article>`).join("") || `<p class="promo-empty">${busy && !state.items.length ? "正在加载优惠…" : "没有匹配的优惠"}</p>`}</div>${list.length > visible.length ? `<button class="promo-more" id="promoMore">加载更多（还有 ${list.length - visible.length} 条）</button>` : ""}`;
    content.querySelectorAll(".promo-item").forEach(row => {
      const text = row.querySelector(".promo-text"), toggle = row.querySelector(".promo-toggle");
      const isExpanded = text.classList.contains("expanded");
      if (isExpanded) text.classList.remove("expanded");
      toggle.hidden = text.scrollHeight <= text.clientHeight + 1;
      if (isExpanded) text.classList.add("expanded");
    });
    $("promoSearch").oninput = e => { query = e.target.value; const start = e.target.selectionStart; render(); $("promoSearch").focus(); $("promoSearch").setSelectionRange(start, start); };
    content.querySelectorAll("[data-open]").forEach(b => b.onclick = async () => {
      const item = state.items.find(x => x.id === b.dataset.open);
      if (!item) return;
      try {
        await window.GT_API.promos.openPromoUrl({ url: item.link });
        scroll = content.scrollTop;
        render();
      } catch (_) { showToast("无法打开详情，请重试", true); }
    });
    content.querySelectorAll("[data-toggle]").forEach(b => b.onclick = () => {
      scroll = content.scrollTop;
      if (expanded.has(b.dataset.toggle)) expanded.delete(b.dataset.toggle); else expanded.add(b.dataset.toggle);
      render();
    });
    const more = $("promoMore"); if (more) more.onclick = () => { scroll = content.scrollTop; shown += PAGE_SIZE; render(); }; content.scrollTop = scroll;
  }
  async function refresh() {
    if (busy) return;
    busy = true; render();
    try {
      const items = await loadPromoList();
      const previous = new Map(state.items.map(item => [item.link, item])), lastKnown = items.map(item => previous.has(item.link)).lastIndexOf(true); // 官网倒序:排在已知最后一条之后的是扩大取数范围补进的历史优惠,不算新条目
      state.seen = items.filter((item, i) => !state.initialized || i > lastKnown || state.seen.includes(previous.get(item.link)?.id)).map(item => item.id);
      state.items = items.map(item => ({ ...item, date: previous.get(item.link)?.date }));
      state.initialized = true; state.updated = Date.now(); failed = ""; save();
    } catch (error) { failed = typeof error === "string" ? error : "优惠来源返回的列表格式异常"; }
    busy = false; render();
  }
  $("promoEntry").onclick = () => { $("promoOverlay").hidden = false; render(); if (!state.updated || Date.now() - state.updated > 1800000) refresh(); };
  $("promoBack").onclick = () => { $("promoOverlay").hidden = true; $("promoEntry").focus(); };
  $("promoRefresh").onclick = refresh;
  document.addEventListener("keydown", e => { if (e.key === "Escape" && !$("promoOverlay").hidden) $("promoBack").click(); });
  entry();
  if (!state.updated || Date.now() - state.updated > 1800000) setTimeout(refresh, 1000);
  setInterval(refresh, 1800000);
}
