/* AI 优惠官网的结构化列表与卡片摘要。 */
function parsePromoPage(html) {
  const origin = "https://www.aipromonow.com";
  // 只在惰性模板里读文本，远程脚本和资源不会进入应用页面。
  const template = document.createElement("template");
  template.innerHTML = html;
  const lists = [...template.content.querySelectorAll('script[type="application/ld+json"]')]
    .flatMap(script => JSON.parse(script.textContent))
    .filter(data => data?.["@type"] === "ItemList" && data.url === origin + "/deals");
  const list = lists[0];
  if (lists.length !== 1 || !Array.isArray(list.itemListElement) ||
      !list.itemListElement.length || list.itemListElement.length !== list.numberOfItems) {
    throw Error("invalid promo list");
  }
  const ids = new Set();
  return list.itemListElement.map(item => {
    const title = item?.name?.trim();
    const link = new URL(item?.url);
    const slug = /^\/promos\/([a-z0-9-]+)$/i.exec(link.pathname)?.[1];
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
