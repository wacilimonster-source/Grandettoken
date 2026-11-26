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