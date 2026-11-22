// ───────────── 渠道详情弹层(单例) ─────────────
// 同一份 renderDetail 既供弹层使用,也保留给测试与旧入口;DOM 里只有这一个弹层,
// 轮询更新只换内容,不新开第二个 dialog。
let DETAIL_ID = null;
let DETAIL_FOCUS = null;
let DETAIL_BUSY = false;

const KIND_CAPTION = {
  points: "积分 · 逐笔到期",
  percent: "配额 · 按窗口恢复",
  amount: "余额 · 本地快照推算消耗",
};

/** 弹层顶部的大数字:与卡片主数字同一口径,只放大。 */
function detailHeroHtml(c) {
  if (isAntigravity(c) && c.hasKey && (c.windows || []).length && window.AntigravityUI) return "";
  if (c.id === "google-cloud") return `<div class="detail-hero">${esc(money(c.remaining, c.unit))}</div>`;
  if (!c.hasKey) return `<div class="detail-hero" style="color:var(--tx3)">——</div>`;
  if (c.remaining === null || c.remaining === undefined) {
    return `<div class="detail-hero" style="color:var(--tx3)">——</div>`;
  }
  if (c.kind === "percent") {
    const mw = mainWindow(c);
    const v = dispPct(Math.max(0, Math.min(100, mw ? mw.remainPercent : c.remaining)));
    return `<div class="detail-hero">${v.toFixed(1)}<span class="unit">%</span></div>`;
  }
  if (c.kind === "points") {
    return `<div class="detail-hero">${points(c.remaining)}<span class="unit">积分</span></div>`;
  }
  const [int, dec] = money(c.remaining, c.unit).split(".");
  return `<div class="detail-hero"><span class="unit">${esc(curSign(c.unit).trim())}</span>${int}${
    dec ? `<span class="unit">.${dec}</span>` : ""
  }</div>`;
}

function detailHeaderHtml(c) {
  const failed = c.hasKey && !c.valid && !c.stale;
  const sub = c.authLabel || KIND_CAPTION[c.kind] || "";
  return `${iconHtml(c, "ico", !c.hasKey || failed)}
    <div class="detail-title"><b id="detailName">${esc(c.name)}</b><p>${esc(sub)}</p></div>
    <button type="button" class="detail-close" data-act="detail-close" aria-label="关闭详情">
      <svg aria-hidden="true"><use href="#i-close"/></svg>
    </button>`;
}

function renderDetailSheet() {
  if (!DETAIL_ID) return;
  const c = CHANNELS.find((x) => x.id === DETAIL_ID);
  if (!c) {
    closeDetail(false);
    showToast("渠道已隐藏或移除,详情已关闭", true);
    return;
  }
  const body = $("detailBody");
  const keepTop = body ? body.scrollTop : 0;
  // 轮询重画时保留弹层内的键盘焦点:按 data-act / data-sec 找回同一个控件
  const sheet = $("detailSheet");
  const active = document.activeElement;
  const inSheet = !!(active && sheet && sheet.contains(active) && active !== sheet);
  const focusSel = inSheet
    ? [
        active.dataset.act ? `[data-act="${active.dataset.act}"]` : "",
        active.dataset.sec ? `[data-sec="${active.dataset.sec}"]` : "",
      ].join("")
    : "";
  $("detailHead").innerHTML = detailHeaderHtml(c);
  const state = hvRow(c.id) || {};
  const stale = c.stale || state.state === "stale" || !c.valid;
  const caption = [
    c.id === "google-cloud" ? "Google Cloud 剩余赠金" : KIND_CAPTION[c.kind] || "",
    stale ? (c.recovery && c.recovery.phase === "retrying" ? "更新中 · 保留上次快照" : c.stale ? "保留上次快照" : "暂时无法更新") : "",
    c.updatedAt ? `更新于 ${relTime(c.updatedAt)}` : "",
    c.recovery && c.recovery.attempts ? `已尝试 ${c.recovery.attempts} 次` : "",
    c.error && !c.valid ? c.error : "",
  ].filter(Boolean).join(" · ");
  const resetOpen = c.id === "codex" && window.CodexResetUI && window.CodexResetUI.isOpen();
  if (resetOpen && window.CodexResetUI.isEditing()) return;
  if (window.QuotaPlanUI?.isEditing(c)) return;
  body.innerHTML = resetOpen
    ? window.CodexResetUI.renderDetail(c)
    : detailHeroHtml(c) +
      `<div class="detail-caption">${esc(caption)}</div>` +
      renderDetail(c, true) +
      (isAntigravity(c) && window.AntigravityUI ? window.AntigravityUI.snapshotNotice(c) : "") +
      `<div class="detail-actions"><button type="button" class="btn" data-act="detail-refresh"${
        DETAIL_BUSY ? " disabled" : ""
      }>${DETAIL_BUSY ? "刷新中…" : isAntigravity(c) ? "刷新当前登录账号" : c.valid ? "刷新数据" : "重试连接"}</button>
      <button type="button" class="btn p" data-act="detail-manage">设置与管理</button></div>`;
  body.scrollTop = keepTop;
  if (focusSel) {
    const el = sheet.querySelector(focusSel);
    if (el) el.focus({ preventScroll: true });
    else sheet.focus({ preventScroll: true });
  }
  // 弹层里没有折叠行,失败全文直接展开(否则它永远停在 hidden)
  document.querySelectorAll("#detailBody .fn-fail").forEach((el) => {
    el.hidden = false;
  });
  // 详情里新生的「?」也要补说明块(设置页那边是 renderChCards 自己做的)
  hydrateWhy(body);
}

/** 详情打开时把背景区域对读屏器隐藏(视觉遮罩已盖住,这里补语义层)。 */
function setBackdropHidden(on) {
  document.querySelectorAll("body > *").forEach((el) => {
    if (el.classList.contains("shade") || el.id === "toast") return;
    if (on) {
      if (el.getAttribute("aria-hidden") !== "true") {
        el.setAttribute("aria-hidden", "true");
        el.dataset.ah = "1";
      }
    } else if (el.dataset && el.dataset.ah === "1") {
      el.removeAttribute("aria-hidden");
      delete el.dataset.ah;
    }
  });
}

function openDetail(id, trigger) {
  const c = CHANNELS.find((x) => x.id === id);
  if (!c) return;
  if (!DETAIL_ID) DETAIL_FOCUS = trigger || document.activeElement;
  DETAIL_ID = id;
  const shade = $("detailShade");
  shade.hidden = false;
  setBackdropHidden(true);
  document.body.classList.add("detail-open");
  renderDetailSheet();
  if ($("detailBody")) $("detailBody").scrollTop = 0;
  // 焦点进入弹层本身(而不是直接落在关闭按钮上):避免一打开就出现一圈按钮焦点框,
  // Tab 的第一站仍是关闭按钮。
  const sheet = $("detailSheet");
  if (sheet) sheet.focus();
}

function closeDetail(restoreFocus = true) {
  if (window.CodexResetUI) window.CodexResetUI.onDetailClosed();
  const id = DETAIL_ID;
  DETAIL_ID = null;
  const shade = $("detailShade");
  if (shade) shade.hidden = true;
  setBackdropHidden(false);
  document.body.classList.remove("detail-open");
  const head = $("detailHead");
  const body = $("detailBody");
  if (head) head.innerHTML = "";
  if (body) body.innerHTML = "";
  if (!restoreFocus) return;
  // 焦点回到触发元素(卡片或"最先到期"格);元素消失时退回当前筛选芯片。
  const trigger = DETAIL_FOCUS;
  DETAIL_FOCUS = null;
  const card = id && document.querySelector(`.row[data-id="${id}"] .rhead`);
  const fallback = document.querySelector("#sum button:not(:disabled)") || $("lkKeys");
  if (trigger && trigger.isConnected && trigger.offsetParent !== null) trigger.focus();
  else if (card && card.offsetParent !== null) card.focus();
  else if (fallback) fallback.focus();
}

/** 弹层里的动作:展开明细段 / 刷新 / 去设置。 */
async function detailAction(act) {
  const id = DETAIL_ID;
  const a = act.dataset.act;
  if (a === "detail-close") {
    closeDetail();
    return;
  }
  if (a === "sec-more") {
    const sec = act.dataset.sec;
    const cid = act.dataset.id || id;
    rowMore[cid] = rowMore[cid] || {};
    rowMore[cid][sec] = !rowMore[cid][sec];
    renderDetailSheet();
    // 重画后把焦点交回同一个「全部 / 收起」按钮,键盘用户不会掉到 body
    const again = document.querySelector(`#detailBody .sec-more[data-sec="${sec}"]`);
    if (again) again.focus();
    return;
  }
  if (a === "detail-refresh") {
    if (DETAIL_BUSY) return;
    DETAIL_BUSY = true;
    renderDetailSheet();
    try {
      await refresh(true);
      // 刷新回执不再弹 toast:caption 同步变「更新于 刚刚」+ 刷新按钮已有 ✓ 角标,同屏反馈足够
    } finally {
      DETAIL_BUSY = false;
      renderDetailSheet();
      const again = document.querySelector('#detailBody [data-act="detail-refresh"]');
      if (again) again.focus();
    }
    return;
  }
  if (a === "detail-manage" || a === "open-manage") {
    closeDetail(false);
    openManage();
    showTab("ch");
    const target = act.dataset.id || id;
    const cid = isAntigravity({ id: target }) ? "antigravity" : target;
    requestAnimationFrame(() => {
      const card = cid && document.querySelector(`#chList .ccard[data-id="${cid}"]`);
      if (card) {
        card.classList.add("open");
        card.scrollIntoView({ block: "center" });
        card.classList.add("flash");
        setTimeout(() => card.classList.remove("flash"), 1150);
      }
    });
    return;
  }
  if (a === "open-models") {
    const cid = act.dataset.id || id;
    closeDetail(false);
    openManage();
    showTab("mdl");
    ensureModels(cid);
    return;
  }
  // 详情里的就地登录入口:不跳设置页,直接开窗(与设置页卡片同一个后端命令)
  if (a === "qoderweblogin") {
    await qoderWebLogin(act.dataset.id || id);
  }
}

/**
 * 数值翻页 —— 现在**只服务胶囊轮播**(动效 E)。首页数据刷新已改为直接写数值:
 * renderInner 不再调用它,所以主数字不会有上下位移、旧数字重影或整卡扫光。
 * el 的内容必须已是新值;host 提供 position:relative 与字体上下文。
 * oldHtml 给幽灵层保留 <i class="dec/unit/pct/pq"> 这类数字排版结构。
 */
const ROLL_GEN = new WeakMap(); // el → 最近一次翻页的代次
let ROLL_SEQ = 0;
function rollValue(el, oldText, host, oldHtml) {
  if (!el || oldText == null || oldText === el.textContent) return;
  const gen = ++ROLL_SEQ;
  ROLL_GEN.set(el, gen); // 旧回调认不出新代次,不会误清新一轮的 class / ghost
  // 同一容器里上一轮的幽灵先收掉:轮播快速切换不许叠出两层旧数字
  if (host) {
    Array.from(host.children).forEach((g) => {
      if (g.classList && g.classList.contains("roll-ghost")) g.remove();
    });
  }
  el.classList.remove("roll-in");
  void el.offsetWidth; // 胶囊轮播重播同名动画仍需一次回流;首页路径已不走这里
  el.classList.add("roll-in");
  setTimeout(() => { if (ROLL_GEN.get(el) === gen) el.classList.remove("roll-in"); }, 360);
  if (!oldText || !host) return;
  const g = document.createElement("span");
  // ghost 只吃稳定的排版 class:带上临时的 roll-in 会让旧值也跟着上下位移,
  // 看起来就是"两层数字一起动"的重影。
  g.className =
    el.className.split(/\s+/).filter((c) => c && c !== "roll-in" && c !== "roll-ghost").join(" ") +
    " roll-ghost";
  if (oldHtml) g.innerHTML = oldHtml;
  else g.textContent = oldText;
  g.style.left = el.offsetLeft + "px";
  g.style.top = el.offsetTop + "px";
  host.appendChild(g);
  setTimeout(() => { if (ROLL_GEN.get(el) === gen) g.remove(); }, 380);
}

/** 动效 I:面板入场(行自上而下 30ms 节奏淡入)。只在「进入面板形态/从托盘唤出/启动首帧」放。 */
function playEntrance() {
  const l = $("list");
  if (!l) return;
  l.classList.remove("st-in");
  void l.offsetWidth; // 同名 class 重挂必须先回流才能重播动画
  l.classList.add("st-in");
  setTimeout(() => l.classList.remove("st-in"), 700);
}

/** 渲染异常不许把界面留在「正在取…」或整块空白上,也不许被误报成"取数失败"。
 *  后端成功 + 前端抛异常时,用户只看到空白与一句错的提示(报告 P2-33;
 *  renderModels 早就为同一个坑包过一层,主路径漏了)。 */
function render(opts) {
  // 轮询会把 #list 与展开行的详情整块重建,新建元素的 scrollTop 天生是 0。
  // 逐笔到期不再自带内滚(2026-10-23 五段骨架),只剩列表自己的滚动位置要补。
  const listEl = $("list");
  const listTop = listEl ? listEl.scrollTop : 0;
  try {
    renderInner(opts);
  } catch (err) {
    const box = $("list");
    if (box) {
      box.innerHTML =
        '<div class="empty">界面渲染出错:' + esc(String((err && err.message) || err)) + "</div>";
    }
    showToast("渲染出错:" + ((err && err.message) || err), true);
    return;
  }
  if (listEl) listEl.scrollTop = listTop;
}

/** 主数字的文字写法。四种形态共用它 —— 不再各自拼 percent / money / points。 */
/** 百分比口径:看剩余还是看用掉(设计 P3)。开了「用掉还是剩余」时,这里一处翻转,
 *  四个形态都跟着变 —— 口径只能有一处,不然同一份数据会自相矛盾。 */
function dispPct(remain) {
  const v = Number(remain);
  if (!Number.isFinite(v)) return v;
  return CFG && CFG.showUsed ? 100 - v : v;
}

function hvText(h) {
  if (!h) return "——";
  if (h.percent !== null && h.percent !== undefined) return `${dispPct(h.percent).toFixed(1)}%`;
  if (h.amount !== null && h.amount !== undefined) return money(h.amount, h.unit || "CNY");
  if (h.points !== null && h.points !== undefined) return `${points(h.points)} 分`;
  return "——";
}

/** 一轮里 ≥3 个渠道取不到数:多半是网络或系统代理,不是渠道坏了 —— 明说(D14)。 */
function failBannerHtml() {
  const n = (HV && HV.errorCount) || 0;
  if (n < 3) return "";
  return `<div class="fstrip"><b>${n}</b> 个渠道取数失败 · 可能是网络或系统代理 ·
    <span class="lk" data-act="open-diag">看诊断</span></div>`;
}

/* ═════════════ 按 key 复用的最小 DOM 更新 ═════════════
 * 首页以前每轮 `innerHTML = ...` 整块重建:节点身份、键盘焦点、进度条宽度过渡
 * 全部丢失,旧值幽灵与扫光也只能在"新节点"上重播。这里改为按稳定 key 复用:
 *   · 展示计算仍只走 renderRow()/各自的 html 函数(不维护第二套文案逻辑);
 *   · 签名就是这一行最终要输出的 HTML 字符串 —— 覆盖所有真正影响显示的输入
 *     (状态、徽标、配额、警告、时间驱动文案),又不会被"时间戳变了但文案没变"
 *     骗去做整行重建;
 *   · 签名不同才做一次逐节点 diff,只写变化的文本/属性/class/样式。
 */
const QKEY = new WeakMap();   // 元素 → 稳定 key
const QSIG = new WeakMap();   // 元素 → 上次输出的 HTML

function qParse(html) {
  const t = document.createElement("template");
  t.innerHTML = String(html).trim();
  return t.content.firstElementChild;
}

/** 子元素匹配键:取第一个 class 作为结构角色,状态类(dot b / ckb ok)不改变键,
 *  所以状态切换是"改 class"而不是"换节点"。 */
function qChildKey(n) {
  if (!n) return "#none";
  if (n.nodeType === 3 || n.nodeType === 8) return "#text";
  if (n.nodeType !== 1) return "#" + n.nodeType;
  const first = (n.getAttribute("class") || "").trim().split(/\s+/)[0] || "";
  const id = n.getAttribute("data-act") || n.getAttribute("data-body") ||
    n.getAttribute("data-id") || n.getAttribute("data-sec") || "";
  return n.tagName.toLowerCase() + "|" + first + "|" + id;
}

function qPatchAttrs(dst, src) {
  for (const a of Array.from(dst.attributes)) {
    if (!src.hasAttribute(a.name)) dst.removeAttribute(a.name);
  }
  for (const a of Array.from(src.attributes)) {
    if (dst.getAttribute(a.name) !== a.value) dst.setAttribute(a.name, a.value);
  }
}

function qPatchChildren(dst, src) {
  let di = 0;
  for (const s of Array.from(src.childNodes)) {
    const key = qChildKey(s);
    let d = dst.childNodes[di] || null;
    if (!d || qChildKey(d) !== key) {
      let found = null;
      for (let j = di + 1; j < dst.childNodes.length; j++) {
        if (qChildKey(dst.childNodes[j]) === key) { found = dst.childNodes[j]; break; }
      }
      if (found) { dst.insertBefore(found, d); d = found; }
      else { d = s.cloneNode(true); dst.insertBefore(d, dst.childNodes[di] || null); }
    }
    // .rbody 的内容由详情/展开逻辑拥有,不参与刷新 diff
    if (!(d.nodeType === 1 && d.classList && d.classList.contains("rbody"))) {
      if (s.nodeType === 3 || s.nodeType === 8) {
        if (d.nodeValue !== s.nodeValue) d.nodeValue = s.nodeValue;
      } else if (s.nodeType === 1 && d.nodeType === 1) {
        qPatchAttrs(d, s);
        qPatchChildren(d, s);
      }
    }
    di++;
  }
  while (dst.childNodes[di]) dst.removeChild(dst.childNodes[di]);
}

/** box 的直接子元素按 key 复用;顺序相同不移动节点,顺序变化才移动既有节点。 */
function qReconcile(box, items) {
  if (!box) return;
  const old = new Map();
  for (const el of Array.from(box.children)) {
    const k = QKEY.get(el);
    if (k) old.set(k, el);
    else el.remove(); // 形态脚本临时刻的节点 / 旧骨架
  }
  const wanted = new Set(items.map((it) => it.key));
  for (const [k, el] of old) if (!wanted.has(k)) el.remove();
  let i = 0;
  for (const it of items) {
    let el = old.get(it.key);
    if (!el) {
      el = qParse(it.html);
      if (!el) continue; // 空模板不该出现;真出现了也不能把 key 挂到 null 上
      QKEY.set(el, it.key);
      QSIG.set(el, it.html);
    } else if (QSIG.get(el) !== it.html) {
      const src = qParse(it.html);
      if (src && el.tagName !== src.tagName) {
        // 摘要格会在 button / div 之间切换(有可点入口 vs 纯展示):
        // 标签变了只能换节点,qPatchAttrs 改不了 tagName。
        QKEY.set(src, it.key);
        QSIG.set(src, it.html);
        box.replaceChild(src, el);
        el = src;
      } else if (src) {
        qPatchAttrs(el, src);
        qPatchChildren(el, src);
        QSIG.set(el, it.html);
      }
    }
    const cur = box.children[i];
    if (cur !== el) box.insertBefore(el, cur || null);
    i++;
  }
  while (box.children[i]) box.removeChild(box.children[i]);
}

/** 未来 7 天的临期分布:按本地日切桶,用迷你柱表示"哪几天集中到期"。
 *  没有可确认的到期记录时只画一条虚线基线 —— 不虚构柱子。 */
function expiry7Bars(summary) {
  const buckets = Array(7).fill(0);
  for (const x of (summary && summary.entries) || []) {
    const idx = window.ExpiryUI.dayIndex(Number(x.entry.at));
    if (idx >= 0 && idx < 7) buckets[idx] += Number(x.entry.amount) || 0;
  }
  const max = Math.max(...buckets);
  if (!(max > 0)) return '<span class="expbars is-empty" aria-hidden="true"></span>';
  return `<span class="expbars" aria-hidden="true">${buckets
    .map((v) => {
      const h = Math.max(2, Math.round((v / max) * 14));
      const op = (0.3 + 0.6 * (v / max)).toFixed(2);
      return `<i style="height:calc(${h}px*var(--u));opacity:${op}"></i>`;
    })
    .join("")}</span>`;
}

function renderRiskBand() {
  const box = $("sum");
  if (!box) return;
  const summary = expirySummary();
  const hasConfirmed = summary.hasConfirmed;
  const hasPoints = summary.entries.length > 0;
  // 如果一轮新数据已没有可确认的临期事项,退出旧筛选避免渠道列表空白。
  if (EXPIRY_FILTER && !hasConfirmed) EXPIRY_FILTER = false;

  const { label: riskLabel, value: riskValue, sub: riskSub, title: riskTitle } =
    window.ExpiryUI.summaryText(summary, EXPIRY_FILTER, CHANNELS.length, SUMMARY_EXPIRY_DAYS);
  const riskCount = hasPoints ? `${points(summary.total)} 分` : "";
  const cell1 = `<button type="button" class="cell metric risk-cell${EXPIRY_FILTER ? " on" : ""}${hasConfirmed ? " has-risk" : ""}"
      data-act="expiry-filter" aria-pressed="${EXPIRY_FILTER}" title="${esc(riskCount ? riskCount + " · " + riskTitle : riskTitle)}"${hasConfirmed ? "" : " disabled"}>
      <div class="metric-label lb"><svg aria-hidden="true"><use href="#i-clock"/></svg>${riskLabel}</div>
      <div class="metric-number vv">${hasPoints
        ? `<span class="hero-v">${points(summary.total)}</span><span class="hero-unit">分</span>`
        : `<span class="hero-v" style="color:var(--tx3)">${riskValue}</span>`}</div>
      <div class="metric-sub sb sb-row"><span class="sb-tx">${riskSub}</span>${expiry7Bars(summary)}</div></button>`;

  // 有明细缺口时标成“最早可确认”,不把未知日期的渠道误排在它之后。
  let cell2 = "";
  const first = summary.first;
  if (first) {
    // 自然日差,与 expiryRelative 同一口径(报告 L4):ceil 的"剩 N 天"会把
    // 今天 23:00 到期的积分写成"剩 1 天",同一界面另一处却是"今天到期"。
    const days = window.ExpiryUI.dayIndex(Number(first.entry.at));
    const when = days <= 0 ? "今天" : days === 1 ? "明天" : `剩 ${days} 天`;
    const dueColor = days <= 7 ? "var(--warn)" : "var(--tx)";
    const description = first.type === "package" ? "套餐截止" : first.type === "account" ? "账户额度到期" : `${points(first.entry.amount)} 分`;
    const title = `打开 ${first.channel.name} 的到期明细 · ${fmtDay(first.entry.at)} · ${description}`;
    cell2 = `<button type="button" class="cell metric" data-act="jump" data-id="${esc(first.channel.id)}" title="${esc(title)}">
      <div class="metric-label lb">${summary.unknown.length ? "最早可确认" : "最先到期"}</div>
      <div class="metric-number due-date" style="color:${dueColor}">${fmtDay(first.entry.at)}<small style="font-size:11px;color:var(--tx3);margin-left:5px;letter-spacing:0">${when}</small></div>
      <div class="metric-sub"><span class="due-name">${esc(first.channel.name)} · ${description}</span></div></button>`;
  } else if (summary.unknown.length) {
    const unknown = summary.unknown[0];
    cell2 = `<div class="cell metric"><div class="metric-label lb">到期日期未知</div>
      <div class="metric-number due-date" style="color:var(--tx3)">——</div>
      <div class="metric-sub"><span class="due-name">${esc(unknown.name)} · 逐笔未知</span></div></div>`;
  } else {
    cell2 = `<div class="cell metric"><div class="metric-label lb">最先到期</div>
      <div class="metric-number due-date" style="color:var(--tx3)">暂无</div>
      <div class="metric-sub"><span class="due-name">暂无已知到期记录</span></div></div>`;
  }

  // ③ 签到:常驻。全部签完也要留着「✓ 3/3」—— "完成"和"坏了"不能长得一样(D09)
  let cell3 = "";
  const ck = HV && HV.checkin;
  // total = 0 时写 —— :一个「✓ 0/0」既不是"签完了"也不是"还有事",
  // 只是把"没有签到渠道"这件事说了一遍(2026-10-22 体检:全隐藏时它还在)
  if (ck && ck.total > 0) {
    const segs = Array.from({ length: ck.total }, (_, i) => `<i class="${i < ck.done ? "on" : ""}"></i>`).join("");
    const done = ck.done >= ck.total;
    cell3 = `<button type="button" class="cell metric ck-cell check-cell" data-act="ckstripopen" title="${ck.done}/${ck.total} · 每日签到">
      <div class="metric-label lb">每日签到</div>
      <div class="metric-number vv">${done ? "\u2713 " : ""}<span class="hero-v">${ck.done}</span><span class="hero-unit">/${ck.total}</span></div>
      <div class="metric-sub"><span class="ckseg" aria-hidden="true">${segs}</span></div></button>`;
  } else {
    cell3 = `<div class="cell metric ck-cell"><div class="metric-label lb">每日签到</div>
      <div class="metric-number vv" style="color:var(--tx3)">——</div><div class="metric-sub">未启用</div></div>`;
  }

  // 三格摘要也按 key 复用:轮询时不再整块重建(焦点/按钮身份/高度都更稳)。
  qReconcile(box, [
    { key: "risk", html: cell1 },
    { key: "first", html: cell2 },
    { key: "checkin", html: cell3 },
  ]);
  return summary;
}

/** 状态红晕的代次:快速连续刷新时,旧定时器不许清掉新一轮刚挂上的动画。 */
let STATUS_FLASH_GEN = 0;
const DOT_FLASH_GEN = new WeakMap();

/** 过滤条:默认收起,放大镜展开;渠道多起来(>=12)时仍自动放出输入框。 */
function renderInner(opts) {
  if (!WINDOW_VISIBLE) {
    renderPill(sortChannels(CHANNELS, LAST_RENDER_ORDER));
    return;
  }
  // updated=true 只由「数据刷新」调用方传。首页数据刷新已改为直接写数值,
  // 不再做主数字翻页位移 / 幽灵 / 整卡扫光(见 rollValue 与 CSS 动效层说明)。
  const updated = !!(opts && opts.updated);
  // M7:只记上一轮的状态点,用来识别"刚刚跌破红线"的行 —— 那是唯一保留的
  // 刷新反馈(一枚小红晕),不碰数值、不碰整卡。
  const prevDt = {};
  document.querySelectorAll("#list .row").forEach((r) => {
    if (r.dataset.dtone !== undefined) prevDt[r.dataset.id] = r.dataset.dtone;
  });
  // 标题栏挤了 6 个按钮,计数用短写法,完整说法放 tooltip
  // 标题栏改文字按钮后不再放计数(放不下),底栏已有「N 正常 · M 预警」

  const expiry = renderRiskBand(); // 顶部摘要常驻:临期积分 / 最先到期 / 签到
  const expiryOnly = EXPIRY_FILTER && currentForm() === "panel";
  // 排序只听设置里的「渠道排序」(2026-11-03 用户裁决):
  // 限流 / 失效不再被强制排到前排,排序口径只有一个来源。
  const tie = stableTie(LAST_RENDER_ORDER);
  const sorted = expiryOnly
    ? CHANNELS.filter(hasConfirmedExpiry)
        .sort((a, b) =>
          (nearestExpiryAt(a) || Infinity) - (nearestExpiryAt(b) || Infinity) || tie(a, b))
    : sortChannels(CHANNELS, LAST_RENDER_ORDER);
  LAST_RENDER_ORDER = sorted.map((c) => c.id);

  if (!CHANNELS.length) {
    // 冷启动画骨架(D14):一行"加载中…"区分不了"正在加载 / 卡住了 / 没配好",
    // 骨架至少说清"这里有 N 个渠道的位置,正在一个个点亮"。
    qReconcile($("list"), [
      { key: "sk-wrap", html: `<div class="sk-wrap">${Array.from({ length: 6 }, () =>
        `<div class="sk-row"><i class="sk-ico"></i><i class="sk-nm"></i><i class="sk-v"></i></div>`
      ).join("")}</div>` },
      { key: "sk-foot", html: `<div class="sk-foot">正在取数 · 首次启动需要几秒</div>` },
    ]);
    LAST_RENDER_ORDER = [];
    // 底栏也要跟着回到"还没数"的状态:以前这里直接 return,底栏留着**上一轮**
    // 的「8 正常 · 3 预警」—— 刚启动就写着 8 个渠道,而列表里一个都还没有。
    $("fdot").className = "dot off";
    $("fstat").innerHTML = '<span class="fb-dot off"></span>正在取数…';
    return;
  }

  // 展示区只显示已配置密钥、且未被隐藏的渠道(隐藏的只在管理页出现,用于恢复);
  // 一个都没配时给一张引导卡,全被隐藏时给「去设置恢复」卡
  const hiddenN = CHANNELS.filter((c) => c.hidden).length;
  const shown = sorted.filter((c) => c.hasKey && !c.hidden);
  const configured = shown.length > 0;

  // 列表按稳定 key 复用:.row / .rhead / .val .v / 进度条都保留同一个节点,
  // 只有输出 HTML 真的变了的行才做一次最小 diff。失败横幅与签到提示条同样按 key 更新。
  const items = [];
  const banner = failBannerHtml();
  const ckStrip = ckPendingStrip();
  if (banner) items.push({ key: "fstrip", html: banner });
  const quotaBanner = window.QuotaPlanUI?.banner(expiry?.packages || []);
  if (quotaBanner) items.push({ key: "quota-plan", html: quotaBanner });
  if (ckStrip) items.push({ key: "ckstrip", html: ckStrip });
  if (configured) {
    shown.forEach((c, i) => items.push({ key: "row:" + c.id, html: renderRow(c, i) }));
  } else {
    const emptyHtml = expiryOnly
      ? `<div class="empty">
          <div class="ek">⌕</div>
          <b>没有匹配的临期渠道</b>
          点击顶部临期摘要显示全部渠道
        </div>`
      : hiddenN && CHANNELS.some((c) => c.hasKey && c.hidden)
      ? `<div class="empty">
           <div class="ek">&#128065;</div>
           <b>已配置的渠道都被隐藏了</b>
           隐藏期间不再请求接口,历史快照保留
           <div><button class="btn p" data-act="open-manage" data-first="${
             (CHANNELS.find((c) => c.hidden && c.hasKey) || {}).id || ""
           }">去恢复</button></div>
         </div>`
      : `<div class="empty">
         <div class="ek">&#128273;</div>
         <b>配置第一个渠道</b>
         支持 9 个内置渠道 + 自定义(自己的取数地址与字段路径)<br>密钥与登录令牌只写入 Windows 凭据管理器,界面保存后不回显
         <div><button class="btn p" data-act="open-manage">去配置密钥</button></div>
       </div>`;
    items.push({ key: "empty", html: emptyHtml });
  }
  qReconcile($("list"), items);
  if (window.PanelLayoutUI) window.PanelLayoutUI.schedule();

  // 列表已经落到 DOM 里,现在才有真实宽度可量:把 Codex 的倒计时按当前面板宽度收一档
  if (configured) fitCodexResetLabels();

  // 底栏状态(隐藏渠道不计入,只报个数)。口径与行内 dotCls 同源(报告 B5):
  // 红线/限流=告急(红),阈值预警=预警(黄),失联沿用"告急"段(与旧行为一致)。
  // 以前 warn 过滤写成 hvTone==="bad":黄色预警渠道不落在任何段里被算成"正常",
  // 跌破红线的反而标成黄"预警" —— 颜色梯度倒挂,只看底栏会漏掉预警。
  const withKey = CHANNELS.filter((c) => c.hasKey && !c.hidden);
  const bad = withKey.filter(
    (c) =>
      (!c.valid && !(c.recovery && c.recovery.phase === "retrying")) ||
      (c.valid && (c.limited || hvTone(c) === "bad"))
  ).length;
  const warn = withKey.filter((c) => c.valid && !c.limited && hvTone(c) === "warn").length;
  $("fdot").className =
    "dot" + (bad ? " st" : warn ? " w" : withKey.length ? "" : " off");
  // 三段必须**互斥**:正常 = 总数 - 告急 - 预警;每个数字前面带自己的状态点。
  const normalN = withKey.length - bad - warn;
  $("fstat").innerHTML = !withKey.length
    ? `<span class="fb-dot off"></span>${hiddenN ? "渠道已全部隐藏" : "未配置渠道"}`
    : [
        normalN ? `<span class="fb-dot"></span>${normalN} 正常` : "",
        warn ? `<span class="fb-dot warn"></span>${warn} 预警` : "",
        bad ? `<span class="fb-dot bad"></span>${bad} 告急` : "",
        hiddenN ? `<span class="fb-dot off"></span>${hiddenN} 隐藏` : "",
      ].filter(Boolean).join('<span class="fb-sep">·</span>');

  // 首页刷新回执只剩「状态颜色变化 + 刚跌破红线的一次性红晕」:
  //   · 主数字由 qReconcile 原地改文本,不再翻页位移、不生成 ghost;
  //   · 整卡 sweep 不再触发(.sweep 节点只是保留下来的空壳,不挂 .go);
  //   · 没有错峰定时器,也没有为重启同名动画而强制读布局。
  // 颜色本身写在 .val .v 的 inline style 里,随 diff 原地过渡(CSS transition)。
  if (updated && currentForm() === "panel") {
    const gen = ++STATUS_FLASH_GEN;
    document.querySelectorAll("#list .row").forEach((r) => {
      const id = r.dataset.id;
      // M7:只播一次,连续快速刷新时旧回调认不出最新 gen 就不会误清新的红晕
      if (prevDt[id] !== undefined && prevDt[id] !== "b" && r.dataset.dtone === "b") {
        const d = r.querySelector(".n .dot");
        if (d) {
          DOT_FLASH_GEN.set(d, gen);
          d.classList.add("breath1");
          setTimeout(() => {
            if (DOT_FLASH_GEN.get(d) === gen) {
              d.classList.remove("breath1");
              DOT_FLASH_GEN.delete(d);
            }
          }, 1300);
        }
      }
    });
  }

  renderPill(sorted);
  renderDock();
  renderPillPick();
  fitCompact();
  refreshChCards();
  // 弹层开着时只换内容:保持滚动位置与展开状态,不新开第二个 dialog
  if (DETAIL_ID) renderDetailSheet();
  // 新建的 [data-peakcd] 先填一次文案,别等到下一个整分才出现
  tickPeakCountdowns();
  updateHeaderLive();
  if (manageOpen() && document.body.classList.contains("manage-index")) renderManageIndex();
}

// 紧凑条实测宽度(已是当前字号档的最终逻辑像素),fitCompact 维护、formSize 取用
let compactW = 380;
// 换形态/吸附展开后都要重量一次,但只有"最后一次"有意义:两处裸 setTimeout
// 交叠时前一次会拿着半更新 DOM 算出个错宽度再盖上来(报告 O-10)
let fitCompactTimer = null;
function scheduleFitCompact() {
  clearTimeout(fitCompactTimer);
  fitCompactTimer = setTimeout(fitCompact, 150);
}

/**
 * 紧凑条:宽度贴合内容(上限 380×字号倍率)+ 三级信息降级(裁决 A/B/A)。
 * L0 缩写名+精确数 → 装不下就从最宽的 chip 起去名字(L1)→ 仍超就舍小数位(L2)
 * → 还超才把尾部渠道收进 "+N"。chip 都是 flex:none,offsetWidth 即自然宽,
 * 窗口当前多宽都不影响测量;量完交给 setCompactW 重钳。
 */
/**
 * 紧凑条里的签到 chip:**常驻**。
 *
 * 以前它只在"今天还有没签的"时出现 —— 全部签完就什么都不剩,于是"签完了"
 * 与"这功能坏了 / 没开"在界面上完全同形(D09)。现在它一直在,只是内容不同。
 * 没开签到(分母为 0)时,按"不该提醒一件用户没要求的事"的老规矩不出现。
 */
function compactCheckinChip(list) {
  const ck = HV && HV.checkin;
  // total = 0 时不给 chip:一个「✓ 0/0」既不是"签完了"也不是"还有事",
  // 只是把"没有签到渠道"这件事说了一遍(2026-10-22 体检:全隐藏时它还在)
  if (!ck || !ck.total) return null;
  const el = document.createElement("span");
  el.className = "ckchip";
  el.dataset.act = "ckstripopen";
  el.tabIndex = 0;
  // 整块是拖动区,chip 要能点、能 Tab 到(与 +N 同一套处理)
  el.setAttribute("data-tauri-drag-region", "false");
  const all = ck.done >= ck.total;
  el.textContent = (all ? "\u2713 " : "") + ck.done + "/" + ck.total;
  el.title = all ? "今天已全部签到" : `还有 ${ck.total - ck.done} 个渠道没签到`;
  list.appendChild(el);
  return el;
}

function fitCompact() {
  const list = $("list");
  const rows = [...list.querySelectorAll(".row")];
  // 先整体还原:切回面板时必须把紧凑条里藏掉的行和降级类放出来。
  // 面板形态下这些都是空操作 —— 只在真的带着降级态时才写,免得每轮刷新都
  // 给每张卡片的 class/style 各来一次无意义的 DOM 写入(节点复用的前提)。
  rows.forEach((r) => {
    if (r.style.display) r.style.display = "";
    if (r.classList.contains("lv1")) r.classList.remove("lv1");
    if (r.classList.contains("lv2")) r.classList.remove("lv2");
  });
  list.querySelectorAll(".more,.cempty,.ckchip").forEach((el) => el.remove());
  if (!document.body.classList.contains("form-compact")) return;
  // 收起的 8px 竖条没法量;**展开态**(无论是否吸附)都要量 —— 之前整段跳过,
  // 吸附展开后宽度冻结,数值变长被裁、变短则窗口偏宽(报告 B9)。
  // 展开态量完的落点:setCompactW → applyForm → dockLayout(true),几何归吸附管。
  if (DOCK.on && !DOCK.open) return;

  const u = fsU();
  const maxW = compactMaxPx();
  const gap = 10 * u;
  // 签到 chip 常驻:先插进去、把它的宽度算进 chrome —— 不预留宽度的话,
  // 装到最后一步才发现放不下,末尾的渠道会被它挤掉。
  const ckEl = compactCheckinChip(list);
  const ckW = ckEl && list.contains(ckEl) ? ckEl.offsetWidth + gap : 0;
  // 列表左右留白(11+4) + 右侧按钮组 + 窗口两条竖边框 + 签到 chip
  const chrome = Math.round(15 * u) + $("cctl").offsetWidth + 2 + ckW;
  // `+N` 的宽度**真量一次**(以前写死 28u):"+6" 与 "+12" 差好几像素,
  // 写死的结果是末尾那个 chip 被裁掉一条边(实测 many 场景 283 > 279)。
  const moreW = (() => {
    const probe = document.createElement("div");
    probe.className = "more";
    probe.textContent = "+99";
    probe.style.visibility = "hidden";
    list.appendChild(probe);
    const w = probe.offsetWidth;
    probe.remove();
    return w || Math.round(28 * u);
  })();

  if (!rows.length) {
    // 空态(没配置 / 全隐藏):一条最小 chip 贴到内容宽,不再空荡 380
    const ce = document.createElement("div");
    ce.className = "cempty";
    ce.textContent = $("fstat").textContent || "暂无渠道";
    // 紧凑条整块是 data-tauri-drag-region="deep",新加的孩子默认也在里面:
    // 显式退出拖动区,不然按住它就成了拖窗口(F13)
    ce.setAttribute("data-tauri-drag-region", "false");
    list.appendChild(ce);
    setCompactW(Math.min(maxW, Math.ceil(ce.offsetWidth + gap + chrome)));
    return;
  }

  // 三档各量一遍(读 offsetWidth 会强制回流,6 行 × 3 趟完全够快)
  const wAt = (lv) => {
    rows.forEach((r) => {
      r.classList.remove("lv1", "lv2");
      if (lv) r.classList.add("lv" + lv);
    });
    return rows.map((r) => r.offsetWidth);
  };
  const W = [wAt(0), wAt(1), wAt(2)];
  rows.forEach((r) => r.classList.remove("lv1", "lv2"));

  const lv = rows.map(() => 0);
  let hidden = 0;
  const total = () => {
    const n = rows.length - hidden;
    // 间隙数 = chip 之间 + 「最后一个 chip 与 +N 之间」。以前只算 n-1,
    // 有 +N 时少算一个 gap(10px)—— 末尾 chip 因此被裁掉 4~10px。
    const gaps = Math.max(0, n - 1 + (hidden ? 1 : 0));
    let s = chrome + gap * gaps + (hidden ? moreW : 0);
    for (let i = 0; i < n; i++) s += W[lv[i]][i];
    return s;
  };
  // 降级顺序 = 重要性的倒序(P5 · 降级按重要性,不按宽度):
  // 第一个 chip 就是"最紧的那一个",永远保持完整(图标 + 名称 + 数值);
  // 从最后一个开始依次去名字、舍小数,实在装不下才收进 +N。
  // 以前是"挑当前最宽的降级"—— 最紧的渠道通常名字最长、数值最长,
  // 于是它第一个被削掉名字,而降级顺序与"哪个最该被看见"毫无关系(D07)。
  const strip = (from, to) => {
    while (total() > maxW) {
      let bi = -1;
      for (let i = rows.length - hidden - 1; i >= 1; i--)
        if (lv[i] === from) { bi = i; break; }
      if (bi < 0) break;
      lv[bi] = to;
    }
  };
  strip(0, 1); // ① 去名字,从最后一个起
  strip(1, 2); // ② 舍小数,从最后一个起
  while (rows.length - hidden > 1 && total() > maxW) hidden++; // ③ 尾部收 +N

  const n = rows.length - hidden;
  rows.forEach((r, i) => {
    r.classList.toggle("lv1", lv[i] === 1);
    r.classList.toggle("lv2", lv[i] === 2);
    r.style.display = i < n ? "" : "none";
  });
  if (hidden) {
    const more = document.createElement("div");
    more.className = "more";
    more.textContent = "+" + hidden;
    // I2:截断不再是信息黑洞 —— tooltip 列出被藏渠道,点击展开面板并定位过去
    const names = rows
      .slice(n)
      .map((r) => (CHANNELS.find((c) => c.id === r.dataset.id) || {}).name)
      .filter(Boolean);
    more.title = names.join("、") + " —— 点击展开面板查看";
    more.dataset.first = rows[n] ? rows[n].dataset.id : "";
    // +N 是可点、也要能 Tab 到的(F19):显式退出紧凑条的拖动区(F13),
    // 再给一个 tabindex 与 data-act —— keyActivate 认的是 [data-act]/[data-act] 那套,
    // 没有它键盘按 Enter/Space 是空的(点击本身仍由 #list 的委托分支处理)
    more.setAttribute("data-tauri-drag-region", "false");
    more.tabIndex = 0;
    more.dataset.act = "more";
    list.appendChild(more);
  }
  setCompactW(Math.min(maxW, Math.ceil(total())));
}

/** 重钳紧凑条窗口宽:变宽立刻动(哪怕 1px,窄了 chip 会被裁),变窄 <6px 不动(数值抖动不牵窗)。 */
function setCompactW(w) {
  if (!w || !(w > compactW || compactW - w >= 6)) return;
  compactW = w;
  if (currentForm() === "compact") applyForm("compact", false); // 已排在几何队列里
}

// 胶囊显示哪些渠道:设置里选中的按顺序轮播;一个都没选就自动取最紧张的那个
// (总额不能告诉你哪个 Key 要挂了,所以自动模式只挑最紧的)
const PILL = { list: [], sorted: [], idx: 0, auto: true, degraded: false };

function renderPill(sorted) {
  PILL.sorted = sorted;
  const selection = (CFG && CFG.pillChannels) || [];
  const picked = window.AntigravityUI ? window.AntigravityUI.expandSelection(selection) : selection;
  const autoPick = () => {
    // 自动轮播是「最紧张的一个」,口径独立于列表排序(列表顺序只按设置):
    // 绑定窗比例升序 → 无分母金额型按「余额 / 底线」贴近度 → 积分按 7 日到期占比。
    const active = CHANNELS.filter((c) => c.hasKey && !c.hidden && (c.valid || c.stale || (c.recovery && c.recovery.phase === "retrying")));
    const tightness = (c) => {
      const r = remainRatio(c);
      if (r !== null) return r;
      if (c.kind === "amount" && c.remaining !== null) {
        const floor = (CFG && CFG.moneyFloor) || 10;
        return Math.max(0, Math.min(1, c.remaining / floor));
      }
      if (c.kind === "points") {
        return 1 - Math.min(1, expiringWithin(c, SUMMARY_EXPIRY_DAYS) / Math.max(1, c.remaining || 1));
      }
      return 2;
    };
    const first = [...active].sort((a, b) => tightness(a) - tightness(b))[0];
    return first ? [first] : [];
  };
  // 固定列表里的渠道可能全被「隐藏」了(显隐功能):降级为自动挑选。
  // 不能落进 renderPillFace 的「取数失败」兜底 —— 那是与事实相反的故障告警,
  // 其余渠道明明取数正常(报告 B2);degraded 供 tooltip 说明实情。
  const fixed = picked.length
    ? picked.map((id) => CHANNELS.find((c) => c.id === id)).filter(Boolean).filter((c) => !c.hidden)
    : [];
  PILL.degraded = !!picked.length && !fixed.length;
  PILL.auto = !fixed.length;
  PILL.list = PILL.auto ? autoPick() : fixed;
  if (PILL.idx >= PILL.list.length) PILL.idx = 0;
  renderPillFace();
}

let LAST_PILL = { id: null, text: "", html: "" };
/** 胶囊翻页(动效 E):轮播换渠道/换数值时 200ms 纵向翻页 + 图标淡入;只在胶囊形态放 */
function pillAnim(newId, lastText, lastId, lastHtml) {
  const pv = $("pillV");
  const text = pv.textContent;
  if (currentForm() === "pill") {
    if (lastText && text !== lastText) rollValue(pv, lastText, $("pill"), lastHtml);
    if (lastId !== newId) {
      const p = $("pillIco").querySelector(".pico");
      if (p) {
        p.classList.remove("ico-in");
        void p.offsetWidth;
        p.classList.add("ico-in");
      }
    }
  }
  LAST_PILL = { id: newId, text, html: pv.innerHTML };
}

