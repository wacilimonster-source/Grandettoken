// ───────────── 启动 ─────────────
async function refresh(force = false) {
  // render() 挪出 try:渲染抛错以前会被当成"取数失败"再 toast 一次,信息是错的(P2-33)
  let list;
  try {
    list = await window.GT_API.channels.getChannels({ force });
  } catch (e) {
    // 以前这里直接覆写 #list 就完了:紧凑条形态下条里一片空白、窗口留着旧宽度,
    // 胶囊与托盘也停在旧值(报告 P2-14)。走同一条 render 收尾,只是把错误提示出来。
    showToast("取数失败:" + e, true);
    render({ updated: false });
    return;
  }
  CHANNELS = list;
  await loadHomeView();
  // 签到状态必须**在 render 之前**取好(2026-10-21 修):
  // 以前 loadCheckin() 只在「打开设置页 / 点签到按钮 / 后端推事件」时调用 ——
  // 于是首页从来就没有 CK,徽标一律拿到 undefined 直接不画,用户当然看不到签到。
  // 它是**一个** IPC(一次返回全部渠道),比每个渠道各查一次便宜,放在这里不心疼。
  await loadCheckin();
  render({ updated: true });
}

/** 收下一轮轮询推送:`{ views, home }` —— 数据与统一视图**一起**到。
 *
 * 只收 views 的话,面板数字会更新(行里的大数字是前端自己从 `remaining` 算的),
 * 而胶囊主数字 / 行色调 / 分组 / 风险带读的是 `home_view` 的结论 —— 那一份以前
 * 只在启动时取过一次,于是永远停在启动那一刻(2026-10-26 真机:4S 余额一直在动,
 * 胶囊一直不动,托盘也跟着胶囊一起僵住)。
 *
 * 老后端只推一个 views 数组:退回补取一次 `home_view`(与这一轮同一份取数结果,
 * 5 秒内复用缓存,不会多打账务接口),行为不回退。
 */
function applyChannelsPush(payload) {
  const legacy = Array.isArray(payload);
  CHANNELS = legacy ? payload : (payload && payload.views) || [];
  if (!legacy && payload && payload.home) {
    hvApply(payload.home);
    render({ updated: true });
    return;
  }
  loadHomeView().then(() => render({ updated: true }));
}

(async function init() {
  try {
    CFG = await window.GT_API.config.getConfig();
  } catch {
    CFG = {
      activeIntervalSec: 60, idleIntervalSec: 300, backoffIntervalSec: 900,
      warnPercent: 40, critPercent: 15, autostart: false,
      form: "panel", compactEnabled: false, sort: "percent", pillChannels: [],
      fontScale: "md", theme: "dark", panelLayout: "single", hiddenChannels: [], hotkey: "", trayShowNumber: true,
      claimChannels: {
        "4sapi": { enabled: true, amount: 200, minIntervalDays: 14, manualLastAt: null, manualSetAt: null },
      },
      modelShowAll: false, modelGroup: null, modelBaseUrl: null,
      selectionPrefix: {}, traeAutoSelect: false,
    };
  }
  applyConfigToUI();
  applyFontScale();
  // 静态模板里的「?」补出说明块(渠道卡里的是动态的,renderChCards 自己会补)
  hydrateWhy();
  // 版本号:等首检(最坏 30 秒)才显示,期间关于页是空的。直接问后端要(报告 P2-16)
  window.GT_API.desktop.appVersion()
    .then((v) => {
      VER = v;
      const el = $("uver");
      if (el && !el.textContent) el.textContent = " " + v;
    })
    // silent: 版本号拿不到只是关于页少个后缀,首检成功后 renderAbout 会补。
    .catch(() => {});
  // 启动期的非致命错误(配置坏了回落默认、快捷键被占用):后端只往 stderr 写过,
  // 而绿色版从托盘启动时没人看得到控制台 —— 取回来显示在关于页的状态行上
  window.GT_API.desktop.takeStartupError()
    .then((msg) => {
      if (msg) setMsg(msg, true);
    })
    // silent: 启动期错误本身是非致命的;取不到就没有可显示的内容,不额外提示"提示读取失败"。
    .catch(() => {});
  // 置顶以真实窗口状态为准(Rust 启动时按配置应用),避免按钮与实际不一致
  try {
    const real = await appWindow.isAlwaysOnTop();
    if (real !== CFG.alwaysOnTop) CFG.alwaysOnTop = real;
  } catch { /* silent: 读不到真实置顶态就沿用配置值,只影响按钮初始高亮 */ }
  applyPinUI();
  // ── 签到事件:必须在首轮 refresh **之前**挂上 ──
  // 后台签到节拍器在 Rust setup 里立刻跑一轮(`spawn_checkin_ticker`),常常正好赶上
  // 前端首帧 refresh。以前这些监听写在 refresh 之后:这一轮的事件没人接,CK 就停在
  // 启动前的旧结论上 —— 2026-10-23 现场,Qoder 10:11 已判 inactive,顶部却一直挂
  // "还有 1 个渠道没签到",点进签到区也看不到任何能做的事,直到 10:55 手动领一次才消失。
  await listen("checkin-updated", () => {
    loadCheckin().then(() => {
      if (manageOpen()) {
        renderChCards();
        fillKeyHints();
        // 签到中心页签开着:流水与日历跟着后台签到当场更新
        const ckBtn = document.querySelector('#mTabs [data-p="ck"]');
        if (ckBtn && ckBtn.classList.contains("on")) loadCheckinHistory();
      } else if (CHANNELS.length) {
        // 首页的提示条 / 风险带也是 CK 的使用者:后台签完要当场收掉那条提示
        render({ updated: false });
      }
    });
  });
  // ── 跨日重取:签到短码是**周期日作用域**的,北京 0 点(trae/workbuddy)与
  // 10 点(Qoder)一翻页,昨天的「已签」必须当场翻成「待签」,不能等下一个事件来撞
  // (2026-10-25 真机:0 点后界面挂着昨天的已签,直到 00:09 的监测事件才被动刷掉)。
  // 后端已按周期日门控短码;前端只负责在边界时刻重取一次(loadCheckin 自带重渲)。
  // 北京 0 点 = UTC 16:00,Qoder 10:00 = UTC 02:00(北京无夏令时,UTC 算术稳定)。
  {
    const DAY = 86400000;
    const msToBoundary = () => {
      const t = Date.now();
      const dayStart = Math.floor(t / DAY) * DAY;
      let at = dayStart + 2 * 3600000; // UTC 02:00(Qoder 跨日)
      if (at <= t) at = dayStart + 16 * 3600000; // UTC 16:00(北京 0 点)
      if (at <= t) at = dayStart + DAY + 2 * 3600000;
      return at - t;
    };
    const scheduleRollover = () => {
      setTimeout(() => {
        loadCheckin();
        scheduleRollover();
      }, msToBoundary());
    };
    scheduleRollover();
  }
  await listen("checkin-alert", (ev) => {
    // 后端只在"需要用户动手"(凭据失效 / 接口结构变了)时发这条;绿色版从托盘启动时
    // stderr 永远没人看得到(报告 P2-14 / P3-36)。
    const d = (ev && ev.payload) || {};
    showToast(
      (d.name || "签到渠道") + "需要处理:" + (d.message || "去渠道卡重新登录一次"),
      true
    );
  });
  await listen("checkin-activity", (ev) => {
    const d = (ev && ev.payload) || {};
    const manual = d.status === "manual" || (!d.canClaim && d.claimStatus === "CLAIMABLE");
    const where = d.canClaim ? " · 渠道卡里领" : manual ? " · 需在客户端参与" : "";
    // 活动标题在渠道卡与签到中心都看得到,toast 只报「谁 + 什么事 + 去哪办」
    showToast((d.name || "渠道") + (d.canClaim ? " 有活动可领" : " 有新活动") + where);
  });
  // 自动签到领到了(后台静默跑,这一声就是收据);手动点的由按钮回执负责,不重响
  await listen("checkin-claimed", (ev) => {
    const d = (ev && ev.payload) || {};
    const cr = d.credits === null || d.credits === undefined ? "" : " +" + d.credits + " 分";
    beep("ding");
    showToast((d.name || "签到") + " 已领到" + cr);
  });

  // 紧凑条改成可选(默认关,2026-10-23 裁决 D1):老配置若停在紧凑条,
  // 回到首页并提示一次。把 CFG.form 清成 panel 就是"只提示一次"的标记,
  // 不用再加一个"提示已展示"的配置字段。
  if (CFG && !CFG.compactEnabled && CFG.form === "compact") {
    CFG.form = "panel";
    saveCfg("紧凑条设置");
    showToast("紧凑条已改为可选:设置 → 显示 → 启用紧凑条");
  }

  const resumed = window.__GT_RESUME_MAIN;
  if (resumed) restoreWindowPresentation(resumed);
  else await applyForm("panel", false, true);
  updateSizeClass();

  // ── 数据/结果事件:必须在首轮 refresh **之前**挂上(报告 L8,与上方 checkin-*
  // 同一条纪律)── 后端 setup 里就跑首轮取数与红线判定,慢网络下能持续数秒;以前
  // 这批监听挂在 refresh 之后,首检那一刻发出的 channels-updated / crit-alert 没人接,
  // 预警音与应用内提示会丢。全部 await,确保注册完成才放行首轮取数。
  let receivedPush = false;
  await listen("channels-updated", (ev) => {
    receivedPush = true;
    applyChannelsPush(ev.payload);
  });

  // 跌破红线(Rust 判定、一天一次):播一声预警音(开了才有)+ 应用内提示。
  // 判定不放前端 —— 界面与通知必须用同一个口径,否则会出现"红了但没响"。
  await listen("crit-alert", (ev) => {
    const d = (ev && ev.payload) || {};
    beep("warn");
    showToast(d.body || d.name + " 余额告急", true);
  });

  // WorkBuddy 网页登录的三种结果都走这个事件:抓到令牌 / 用户自己关了窗 / 失败。
  // 成功时 Rust 已经立刻刷过一轮(channels-updated 会带出新数值),这里只管按钮与提示。
  await listen("qoderweb-login", (ev) => {
    QODER_LOGGING = false;
    const p = ev.payload || {};
    const name = p.id === "qoder-cn" ? "Qoder 国内版" : "Qoder";
    // ok + detail = 会话确实被平台接受了,但逐包结构还没认出来:不许说成"失败",
    // 也不许说成"明细已刷新" —— 两句都是假话(2026-11-03 真机,见 core::qoderweb)。
    if (p.ok && p.detail) showToast(p.detail);
    else if (p.ok) showToast(`已登录 ${name} 网页 · 积分明细已刷新`);
    else if (p.cancelled) showToast("已取消登录");
    else showToast(p.reason || "登录没成功,请再试一次", true);
    renderChCards();
    // 详情开着时把按钮状态一起换掉:成功且拿到明细后 qoderLoginBar 会自己消失
    if (DETAIL_ID) renderDetailSheet();
  });
  await listen("workbuddy-login", (ev) => {
    WB_LOGGING = false;
    const p = ev.payload || {};
    if (p.ok) showToast("已登录 WorkBuddy · 读数已刷新");
    else if (p.cancelled) showToast("已取消登录");
    else showToast(p.reason || "登录没成功,请再试一次", true);
    invalidateKeyMeta();
    renderChCards();
    fillKeyHints(true);
  });
  await listen("doubao-login", (ev) => {
    DB_LOGGING = false;
    const p = ev.payload || {};
    if (p.ok) showToast("已登录豆包 · 读数已刷新");
    else if (p.cancelled) showToast("已取消登录");
    else showToast(p.reason || "登录没成功,请再试一次", true);
    invalidateKeyMeta();
    renderChCards();
    fillKeyHints(true);
  });

  // 从托盘/热键唤回窗口:Rust 侧在「隐藏 → 显示」那一刻发 window-shown,这里重放入场。
  // 不能监听 focus / visibilitychange —— 它们分不清"从托盘唤回"和普通 alt-tab,
  // 每次切走再切回都会重播一遍(报告 B8)。
  await listen("window-hidden", () => { WINDOW_VISIBLE = false; });
  await listen("window-shown", () => {
    WINDOW_VISIBLE = true;
    render({ updated: false });
    // 隐藏期间分钟定时器仍会跑,但窗口可能错过最后一次重绘;唤回时先补一次文案
    tickPeakCountdowns();
    if (currentForm() === "panel") playEntrance();
  });

  // 监听都挂好了才做首轮取数(与上方 L8 注释同一理由);首帧入场随后放
  WINDOW_VISIBLE = await appWindow.isVisible();
  if (resumed) await loadResumedChannels(() => receivedPush);
  else await refresh();
  playEntrance(); // 启动首帧也来一次入场(上面 applyForm 没换形态,不会自动放)

  // 启动 30 秒后先自动检查一次;之后每 10 分钟再问一次 —— 是否真发请求由
  // Rust 侧 24 小时节流把关(最多每天一次)。之前只有启动那一次,
  // 软件连开几天就再也不会重检(实测:上次检查时间永远停在开机那一刻)。
  setTimeout(() => checkUpdate(false), 30_000);
  setInterval(() => checkUpdate(false), 10 * 60 * 1000);
})();

/* AI 优惠：列表摘要与本机已读状态。 */
(() => {
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
})();

// 本地模型网关已拆至 gateway-view.js。
