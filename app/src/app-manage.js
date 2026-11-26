// ───────────── 设置与管理(四页签;渠道卡渲染见 renderChCards)─────────────
const manageOpen = () => document.body.classList.contains("view-manage");
const isTabVisible = (tab) => manageOpen() && currentTab === tab && !document.body.classList.contains("manage-index");

function openManage() {
  // 详情弹层在最上层:任何入口进设置页都先把弹层收掉,不能让它盖着设置页
  if (DETAIL_ID) closeDetail(false);
  // 凭据读取依赖 view-manage，先让设置页进入打开态。
  document.body.classList.add("view-manage");
  // 用户可能刚在别处改了凭据，打开设置时刷新缓存。
  invalidateKeyMeta();
  renderChCards();
  fillKeyHints(true);
  renderPillPick();
  syncStaticControls(); // 关闭设置页时同步可能变化的状态
  renderAbout(UPD.last); // 版本号与上次检查时间
  loadCheckin();         // 签到状态(渠道卡与设置页都用)
  showTab("index");      // 默认落在索引页,31 号起不再把 7 个页签挤进 380px
  loadSyncStatus();      // 索引里的「同步到手机」需要状态摘要
  // 焦点交给页头的「← 返回」,入口与出口方向一致(鼠标点进来时 :focus-visible 不出环,
  // 看不出差别)。入口本身常驻在底栏(2026-11-08),这里只是把焦点送进新页面。
  const back = $("btnBack");
  if (back) back.focus();
}
function closeManage(restoreFocus = true) {
  hideSyncCode();
  window.VaultUI.reset();
  // 关掉设置页时把过期的弹层与编辑态一起作废:否则重开设置页会直接停在
  // 几分钟前那个「写入 · 确认」弹层上,而且还能点(报告 P2-31)
  MDL.sheet = null;
  renderModelSheet();
  VAULT_EDIT = null;
  VAULT_DRAFT = null;
  renderVault();
  // 焦点还在管理页里(含页头返回按钮)时,关掉之后把焦点还给底栏入口;
  // 若焦点在别处(比如点了标题栏「紧凑」触发切形态),不去抢它。
  const ae = document.activeElement;
  const mgr = $("manage");
  const fromManage = !ae || ae === document.body || (mgr && mgr.contains(ae));
  document.body.classList.remove("view-manage", "manage-index");
  // restoreFocus=false 给"切形态顺手关掉管理页"那条路:底栏只在面板形态存在,
  // 切紧凑条/胶囊时还给一个 display:none 的按钮,浏览器会立刻丢给 body,等于白聚焦。
  if (restoreFocus && fromManage && document.body.classList.contains("form-panel")) {
    const k = $("lkKeys");
    if (k && k.offsetParent !== null) k.focus();
  }
}

// 模型弹层:点在遮罩空白处也能关(它既没有背景关闭、也没有焦点收拢,
// Tab 还会跑到被遮住的底层按钮上 —— 报告 P2-31)。
if ($("mdlOv")) {
  $("mdlOv").addEventListener("click", (e) => {
    if (e.target === $("mdlOv") && MDL.sheet) {
      MDL.sheet = null;
      renderModelSheet();
    }
  });
}

// ───────────── 事件绑定 ─────────────
// 键盘兜底:行头/徽标/链接都是 div 或 span,光加 tabindex 不够,
// 还要让 Enter/Space 走同一条 data-act 委托(报告 P3-19:只用键盘的人进不去这个挂件)
function keyActivate(e) {
  if (e.key !== "Enter" && e.key !== " ") return;
  const t = e.target;
  if (!t || !t.closest) return;
  // 原生控件自己会响应 Enter/Space,再点一次就是双发(开关被连点两下)
  if (t.tagName === "BUTTON" || t.tagName === "INPUT" || t.tagName === "SELECT" || t.tagName === "A") return;
  const hit = t.closest("[data-act],.rhead");
  if (!hit || hit === document.body) return;
  e.preventDefault();
  hit.click();
}

$("list").addEventListener("keydown", keyActivate);
$("manage").addEventListener("keydown", keyActivate);

// 顶部摘要:筛选临期渠道 / 跳到最先到期的行 / 打开签到设置。
$("sum").addEventListener("click", async (e) => {
  const act = e.target.closest("[data-act]");
  if (!act) return;
  const a = act.dataset.act;
  if (a === "expiry-filter") {
    if (!expirySummary().hasConfirmed) return;
    EXPIRY_FILTER = !EXPIRY_FILTER;
    render({});
    return;
  }
  if (a === "jump") {
    const id = act.dataset.id;
    // 从"最先到期"点进去,要看的就是它的逐笔到期 —— 直接打开同一个详情弹层
    if (id) openDetail(id, act);
    return;
  }
  if (a === "ckstripopen") {
    // 签到格 = "看签到":落签到中心页签,不再滚到「刷新与启动」里的设置区
    openCheckinCenter();
    return;
  }
  if (a === "open-diag") {
    // 诊断卡在「刷新与启动」页签里,直接滚到体检那一段
    openManage();
    showTab("run");
    focusEl($("btnDiag"));
  }
});

/** 打开设置页后把某个元素滚到视野中央并闪一下(点"最紧/看诊断"时用)。 */
function focusEl(el) {
  if (!el) return;
  requestAnimationFrame(() => {
    el.scrollIntoView({ block: "center" });
    el.classList.add("flash");
    setTimeout(() => el.classList.remove("flash"), 1150);
  });
}

// 详情弹层:点遮罩空白关闭;弹层内动作走 detailAction。
$("detailShade").addEventListener("click", async (e) => {
  if (e.target === $("detailShade")) {
    closeDetail();
    return;
  }
  const act = e.target.closest("[data-act]");
  if (!act) return;
  e.stopPropagation();
  await detailAction(act);
});

// 焦点收拢在弹层内:Tab / Shift+Tab 不逃到背后的卡片和底栏
$("detailSheet").addEventListener("keydown", (e) => {
  if (e.key !== "Tab") return;
  const sheet = $("detailSheet");
  const f = [
    ...sheet.querySelectorAll(
      'button:not([disabled]),[href],input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])'
    ),
  ].filter((el) => el.offsetParent !== null || el === document.activeElement);
  if (!f.length) return;
  const first = f[0];
  const last = f[f.length - 1];
  if (e.shiftKey && (document.activeElement === first || document.activeElement === sheet)) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && document.activeElement === last) {
    e.preventDefault();
    first.focus();
  }
});

$("list").addEventListener("click", async (e) => {
  // I2:紧凑条的 +N 徽标 —— 展开面板,滚动到第一个被藏渠道并高亮 1 秒
  const more = e.target.closest(".more");
  if (more) {
    e.stopPropagation();
    const first = more.dataset.first;
    applyForm("panel").then(() => {
      const row = first && document.querySelector(`.row[data-id="${first}"]`);
      if (row) {
        row.scrollIntoView({ block: "center" });
        row.classList.add("flash");
        setTimeout(() => row.classList.remove("flash"), 1150);
      }
    });
    return;
  }
  const act = e.target.closest("[data-act]");
  if (act) {
    e.stopPropagation();
    const id = act.dataset.id;
    const a = act.dataset.act;
    if (a === "ckstripopen") {
      if (id) {
        // 单渠道待签:落到它的渠道卡并展开 —— 「去登录 / 立即重试」那一行在卡片里。
        // 以前一律跳签到设置区,而那里只有两个开关,用户看完不知道要做什么(2026-10-23)。
        openManage();
        showTab("ch");
        requestAnimationFrame(() => {
          const card = document.querySelector(`#chList .ccard[data-id="${id}"]`);
          if (!card) return;
          cardOpen.add(id);
          card.classList.add("open");
          card.scrollIntoView({ block: "center" });
          const row = card.querySelector(".ckwrap") || card;
          row.classList.add("flash");
          setTimeout(() => row.classList.remove("flash"), 1150);
        });
        return;
      }
      // 多个渠道:落到签到中心(今日行就在最上面,能直接操作)
      openCheckinCenter();
    } else if (a === "open-diag") {
      // 大面积失败横幅上的「看诊断」:直接落到体检那一段,不用自己翻
      openManage();
      showTab("run");
      focusEl($("btnDiag"));
    } else if (a === "ckrun") {
      // N08(2026-10-21):徽标上的「可领」按钮带着 data-act="ckrun",但这个委托
      // 以前只认 ckstripopen / open-manage / open-models / range —— ckrun 落进来
      // 就 return 了,而且前面已经 stopPropagation,连展开卡片都没有,点了完全没反应。
      // 现在与设置页走同一个 ckRun(),首页就能一键领。
      if (CK_LOG) return;
      if (CK[id] && CK[id].canTry === false) {
        showToast("还没到点: " + ckHold(CK[id]), true);
        return;
      }
      await ckRun(id);
    } else if (a === "open-manage") {
      if (DETAIL_ID) closeDetail(false);
      openManage();
      const first = act.dataset.first;
      if (first) {
        showTab("ch");
        requestAnimationFrame(() => {
          const card = document.querySelector(`#chList .ccard[data-id="${first}"]`);
          if (card) {
            card.scrollIntoView({ block: "center" });
            card.classList.add("flash");
            setTimeout(() => card.classList.remove("flash"), 1150);
          }
        });
      }
    } else if (a === "open-models") {
      openManage();
      showTab("mdl");
      // 渠道 id 就是来源 id(4sapi / hapi / opencode-go)
      ensureModels(id);
    } else if (a === "sec-more") {
      // 段标题旁的「全部 N 条」:原地展开 / 收起。状态挂在 rowMore 上,
      // 轮询重画后还在。弹层开着时优先刷新弹层。
      const sec = act.dataset.sec;
      rowMore[id] = rowMore[id] || {};
      rowMore[id][sec] = !rowMore[id][sec];
      if (DETAIL_ID === id) renderDetailSheet();
      else {
        const c = CHANNELS.find((x) => x.id === id);
        const body = document.querySelector(`.rbody[data-body="${id}"]`);
        if (c && body) body.innerHTML = renderDetail(c);
        const cur = document.querySelector(`.row[data-id="${id}"]`);
        revealFailFull(cur);
      }
    }
    return;
  }

  const head = e.target.closest(".rhead");
  if (!head) return;
  // 面板里的卡片点击 = 打开单例详情弹层(紧凑条里点 chip 仍是展开面板的旧路径)
  if (!document.body.classList.contains("form-panel")) return;
  const row = head.closest(".row");
  const id = row.dataset.id;
  const c = CHANNELS.find((x) => x.id === id);
  if (!c) return;
  openDetail(id, head);

  // 金额型的「最近变动」仍按需取:先画占位,取到后只刷新当前弹层。
  if (c.kind === "amount") {
    // 已有缓存就先画缓存,后台重取;从没取到过才写「读取中…」
    const hadCache = !!(changesCache[id] && changesCache[id].length);
    if (!hadCache) changesState[id] = "loading";
    renderDetailSheet();
    try {
      changesCache[id] = await window.GT_API.channels.channelChanges({ id, days: 7 });
      changesState[id] = "done";
    } catch {
      // 重取失败不清掉已有缓存(报告 L1):失败路径推翻"先画缓存"的设计意图,
      // 正展示着的真实台账会被换成错误态,下次打开也拿不到旧数据了。
      if (!hadCache) {
        changesCache[id] = [];
        changesState[id] = "error";
      } else {
        showToast("最近变动刷新失败,先展示上次记录");
      }
    }
    // 期间可能已切换/关闭弹层:只刷新仍是同一个 ID 的那一个
    if (DETAIL_ID === id) renderDetailSheet();
  }
});

/** 轻量提示条(替代原生 alert/底栏回执):2.6 秒自动淡出不阻塞。bad=true 走红色边框。 */
let toastTimer = null;
function showToast(msg, bad) {
  const t = $("toast");
  if (!t) return;
  t.textContent = String(msg ?? "");
  t.classList.toggle("bad", !!bad);
  t.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("show"), 2600);
}

// 顶栏固定四枚入口(刷新 / 置顶 / 隐藏 / 胶囊),不再有弹出菜单。

/** 标题栏右侧的"上次更新":取所有可见渠道里最新的 updatedAt,分钟级刷新。 */
function updateHeaderLive() {
  const el = $("tbLive");
  const ts = Math.max(0, ...CHANNELS.filter((c) => c.hasKey && !c.hidden).map((c) => c.updatedAt || 0));
  setTextIfChanged(el, ts ? relTime(ts) : "");
}

$("btnR").addEventListener("click", async () => {
  // M2 真实进度:取数期间按钮持续脉冲(禁用),完成后才停 —— 动画结束 = 数据已更新,
  // 不再固定 720ms 空转导致用户重复点(报告 B6 的后续修正)
  const btn = $("btnR");
  if (btn.classList.contains("loading")) return;
  btn.classList.add("loading");
  btn.setAttribute("aria-busy", "true");
  try {
    await refresh(true);
    btn.classList.add("done");
    setTimeout(() => btn.classList.remove("done"), 800);
  } finally {
    btn.classList.remove("loading");
    btn.removeAttribute("aria-busy");
  }
  const bad = CHANNELS.filter((c) => c.hasKey && !c.hidden && !c.valid && !(c.recovery && c.recovery.phase === "retrying")).length;
  const retrying = CHANNELS.some((c) => !c.hidden && c.recovery && c.recovery.phase === "retrying");
  if (!retrying || bad) showToast(bad ? `${bad} 个渠道暂时无法更新` : "已更新 · 刚刚", bad > 0);
});
// 模型页:搜索框与「重新取价目表」按钮
// 搜索去抖:一敲就重建 160 行列表太贵(每个字符一次 innerHTML + 排序)(F15);
// MDL.q 仍然立刻更新 —— 语义不变,只是渲染晚 140ms,连打时只画最后一次
let mdlQTimer = null;
$("mdlQ").addEventListener("input", () => {
  MDL.q = $("mdlQ").value || "";
  clearTimeout(mdlQTimer);
  mdlQTimer = setTimeout(renderModels, 140);
});
$("mdlR").addEventListener("click", () => loadModels(true));

// ───────────── 标题栏按钮 ─────────────
// 标题栏没有「设置」入口(2026-10-20 收敛):它与底栏「设置与管理」调的是同一个
// openManage(),却用另一个名字,而且 5 个文字按钮在 380px 里本就挤不下 ——
// 特大字号 + 更新角标时内容宽 405px,最右的「隐藏」被挤出窗口点不到。
// 排序也不放标题栏:加了排序键会重演同一个溢出(排序在 设置 → 显示 与 渠道页 ↑↓)。

// 注意别直接写 addEventListener("click", closeManage):那样 click 的 Event 会
// 落到 closeManage(restoreFocus) 这个形参上(Event 恒为真,行为碰巧对,但语义是错的)。
$("btnBack").addEventListener("click", () => {
  if (manageOpen() && !document.body.classList.contains("manage-index")) {
    showTab("index");
    const back = $("btnBack");
    if (back) back.focus();
    return;
  }
  closeManage();
});
// 设置页:索引 → 子页。旧页签仍在 DOM 里(CSS 隐藏),外部跳转与测试继续可用。
const TAB_TITLES = {
  index: "设置与管理", ch: "渠道管理", ck: "每日签到", mdl: "模型与客户端",
  disp: "外观与显示", run: "刷新与启动", vault: "密钥箱", about: "关于与诊断",
  sync: "同步到手机", gateway: "本地模型网关",
};
const FONT_NAME = { md: "标准", lg: "大", xl: "特大" };
const SORT_NAME = {
  urgency: "紧迫度", percent: "剩余百分比", balance: "余额",
  dayUsage: "今日消耗", custom: "自定义",
};
const idxMin = (sec) => `${Math.max(1, Math.round(Number(sec || 0) / 60))} 分钟`;

/** 设置索引:每行一个分区 + 一行当前状态。只在索引页可见时重建。 */
function renderManageIndex() {
  const box = $("mIndexList");
  if (!box) return;
  const withKey = CHANNELS.filter((c) => c.hasKey && !c.hidden);
  const ck = HV && HV.checkin;
  const ckOn = !!(ck && ck.total > 0);
  const font = FONT_NAME[(CFG && CFG.fontScale) || "md"] || "标准";
  const theme = CFG && CFG.theme === "light" ? "浅色" : "深色";
  const compact = CFG && CFG.compactEnabled ? "紧凑条开" : "紧凑条关";
  const sort = SORT_NAME[(CFG && CFG.sort) || "urgency"] || "紧迫度";
  const run = CFG
    ? `面板 ${idxMin(CFG.activeIntervalSec)} · 折叠 ${idxMin(CFG.idleIntervalSec)}${
        CFG.autostart ? " · 开机自启" : ""
      }`
    : "—";
  const last = CFG && CFG.lastCheckAt;
  const about = `${VER || "—"} · 上次检查 ${
    last ? new Date(last * 1000).toLocaleDateString("zh-CN") : "从未"
  }`;
  const row = (p, icon, title, sub, cls = "") => ({ key: p, html: `<button type="button" class="midx-row" data-p="${p}">
    <span class="midx-ico ${cls}"><svg aria-hidden="true"><use href="#${icon}"/></svg></span>
    <span class="midx-tx"><span class="midx-tt">${esc(title)}</span><span class="midx-ss">${esc(sub)}</span></span>
    <svg class="midx-chev" aria-hidden="true"><use href="#i-chev"/></svg></button>` });
  qReconcile(box, [
    row("ch", "i-layers", "渠道管理",
      withKey.length ? `${withKey.length} 个渠道` : "尚未配置渠道"),
    row("mdl", "i-spark", "模型与客户端", "4SAPI · Hapi · OpenCode Go", "mdl"),
    row("ck", "i-cal", "每日签到",
      ckOn ? `今日 ${ck.done}/${ck.total}${ck.done >= ck.total ? " · 已完成" : ""}` : "未开启签到", "ck"),
    row("disp", "i-display", "外观与显示", `${theme}主题 · ${font}字号 · ${compact} · 排序: ${sort}`, "disp"),
    row("run", "i-refresh", "刷新与启动", run, "run"),
    row("vault", "i-key", "密钥箱", "查看、新增与管理本机密钥", "vault"),
    row("gateway", "i-layers", "本地模型网关", "汇总模型 · 本机或局域网调用"),
    row("sync", "i-sync", "同步到手机", syncSummary(), "sync"),
    row("about", "i-info", "关于与诊断", about),
  ]);
}

function syncSummary() {
  const s = SYNC_STATUS;
  if (SYNC_STATUS_ERROR) return "同步设置读取失败 · 点击重试";
  if (!s) return "打开查看 · 手机只读";
  if (s.nextAllowedAt && s.nextAllowedAt > Date.now() / 1000) {
    return "限流中 · " + untilTime(s.nextAllowedAt) + "重试";
  }
  if (!s.relayId) return s.hasGithub ? "未建组 · 已连接 GitHub" : "未启用 · 手机只读";
  if (!s.hasKey) return "同步码读取失败 · 请找回旧组";
  if (!s.source) return "本机发布来源缺失 · 已暂停";
  if (!s.hasGithub) return "已暂停 · GitHub 未连接";
  if (!s.enabled) return "已暂停 · 手机只读";
  if (s.lastError) return "发布失败 · " + String(s.lastError).slice(0, 42);
  return s.lastUploadAt ? `已开启 · 上次 ${relTime(s.lastUploadAt)}` : "已开启 · 等待首次发布";
}

function untilTime(ts) {
  const delta = Math.max(0, Math.ceil(Number(ts) - Date.now() / 1000));
  if (delta < 60) return `${delta} 秒后`;
  if (delta < 3600) return `${Math.ceil(delta / 60)} 分钟后`;
  return `${Math.ceil(delta / 3600)} 小时后`;
}

function showTab(name) {
  window.VaultUI.reset();
  if (name !== "sync") hideSyncCode();
  // 用户确认：进入同步页直接显示同步码和二维码；「隐藏同步码」只作为临时遮挡。
  else {
    SYNC_CODE_REVEALED = true;
    renderSync();
  }
  const btns = [...$("mTabs").children];
  const b = btns.find((x) => x.dataset.p === name);
  if (name !== "index" && !b) return;
  currentTab = name;
  for (const x of btns) x.classList.toggle("on", x === b);
  document
    .querySelectorAll("#manage .panel")
    .forEach((p) => p.classList.toggle("on", p.dataset.p === name));
  document.body.classList.toggle("manage-index", name === "index");
  const mt = document.querySelector(".mhead .mt");
  if (mt) mt.textContent = TAB_TITLES[name] || "设置与管理";
  const back = $("btnBack");
  if (back) back.title = name === "index" ? "关闭设置" : "返回设置索引";
  const dirty = window.GT_STORE.setTabDirty(name, false);
  if (name === "index") {
    renderManageIndex();
    return;
  }
  if (dirty && name === "ch") refreshChCards();
  if (dirty && name === "disp") renderPillPick();
  // 索引与页签栏共用进入时的数据读取。
  // 签到中心:每次切进来都重读一次流水(本地 SQLite,便宜;后台可能刚签完)
  if (name === "ck") loadCheckinHistory();
  // 模型页签才去取清单:没打开就不打扰接口(价目表一天一次的手动/自动刷新)
  if (name === "mdl") ensureModels();
  // 关于页签里的「排查日志」卡:每次切进来刷新路径与大小(便宜,一次本地文件 stat)
  if (name === "about") refreshLogCard();
  // 密钥箱:每次切进来都重新读索引(列表不需要解密,读起来很便宜);
  // 重新渲染也会把已显示的明文收回
  if (name === "vault") {
    loadVault();
  }
  if (name === "sync") loadSyncStatus();
  if (name === "gateway") loadGateway();
}
$("mIndexList").addEventListener("click", (e) => {
  const b = e.target.closest("button[data-p]");
  if (b) showTab(b.dataset.p);
});
$("mTabs").addEventListener("click", (e) => {
  const b = e.target.closest("button[data-p]");
  if (!b) return;
  showTab(b.dataset.p);
});

