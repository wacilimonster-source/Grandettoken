/**
 * 三形态视觉回归:在**真机窗口**里逐个场景渲染,截图 + 量越界。
 *
 * 为什么不能只靠 jsdom:jsdom 不参与布局,CSS 写错(比如 v0.1.56 把吸附竖条
 * 漏进面板)它一声不响。这个脚本连 CDP,让 WebView2 真的排版,再按形态量:
 *   panel   —— 横向越界(纵向滚动是正常的)
 *   compact —— chip 流被裁(scrollWidth > clientWidth)、纵向越界
 *   pill    —— 主数字/限定语被裁
 *
 * 用法:
 *   1) 起应用并开调试端口(launch-debug.ps1 或手动设 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS)
 *   2) node build/visual-forms.js [--port 9333] [--out docs/assets/qa] [--only normal,empty]
 *
 * 产物:<out>/<场景>-<形态>.png + 一份 <out>/report.json(每张图的尺寸与越界项)。
 * 收尾会 Page.reload 回真实数据 —— 注入的合成数据不落盘(全程不调 saveCfg)。
 */
const fs = require("fs");
const path = require("path");

const argv = process.argv.slice(2);
const argOf = (k, d) => {
  const i = argv.indexOf(k);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : d;
};
const PORT = Number(argOf("--port", process.env.CDP_PORT || 9333));
const OUT = path.resolve(argOf("--out", path.join(__dirname, "..", "docs", "assets", "qa-forms")));
const ONLY = argOf("--only", "").split(",").filter(Boolean);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ── 合成数据(与 jsdom-smoke.js 同一套口径)──
const NOW = Math.floor(Date.now() / 1000);
const ch = (o) =>
  Object.assign(
    {
      id: "x", name: "X", short: "X", color: "#888", kind: "amount", unstable: false,
      hasKey: true, valid: true, remaining: null, used: null, total: null, unit: "CNY",
      windows: [], expiring: [], expiringSoon: null, extra: [], error: null, limited: false,
      authSource: "keyring", authLabel: "", day: null, week: null, month: null,
      dayPrev: null, weekPrev: null, monthPrev: null, plan: null, estimated: false,
      stale: false, updatedAt: NOW, claim: null, hidden: false, peak: null,
      gapDayFrom: null, gapWeekFrom: null, gapMonthFrom: null, gapDays: [], accounts: 0,
    },
    o
  );
const pct = (id, name, remain, limited) =>
  ch({
    id, name, kind: "percent", remaining: remain, unit: "percent",
    windows: [
      { label: "周期", percent: 38, remainPercent: 62, status: "ok", resetsAt: null, main: true },
      { label: "5 小时", percent: 100 - remain, remainPercent: remain,
        status: limited ? "rate-limited" : "ok", resetsAt: null, main: false },
    ],
    limited: !!limited,
  });
const row = (o) => Object.assign(
  { id: "x", state: "fresh", tone: "ok", tier: 1, urgency: 0.5, bindingWindow: null,
    headline: {}, qualifier: null, secondary: null, etaSecs: null, ageSecs: 10,
    checkin: { enabled: false, done: false } }, o);
const home = (rows, extra) => Object.assign(
  { rows, order: rows.map((r) => r.id), risk: null, actionCount: 0, staleCount: 0,
    errorCount: 0, checkin: { total: 0, done: 0, pending: [] },
    totals: { cny: 0, others: [], today: 0, todayPrev: null, hidden: 0 }, now: NOW }, extra);

const NORMAL_CH = [
  pct("claude", "Claude Code", 0, true),
  ch({ id: "ds", name: "DeepSeek", kind: "amount", remaining: 0.03, unit: "CNY", day: 3, dayPrev: 2 }),
  ch({ id: "4sapi", name: "4SAPI", kind: "amount", remaining: 120, unit: "CNY", day: 5, dayPrev: 4,
    claim: { amount: 500, lastClaimAt: null, source: "auto", daysUntilEligible: 12, eligible: false,
      dailyBurn: 1, daysOfBalance: 120, shortageRisk: false } }),
  pct("oc", "OpenCode Go", 22, false),
  ch({ id: "wb", name: "WorkBuddy", kind: "points", remaining: 1000, unit: "credits",
    expiring: [{ at: NOW + 3 * 86400, amount: 600, label: "签到" }] }),
  pct("codex", "Codex", 42, false),
  pct("kimi", "Kimi", 88, false),
  ch({ id: "old", name: "陈旧渠道", kind: "amount", remaining: 50, stale: true, updatedAt: NOW - 3600 }),
  ch({ id: "none", name: "未配置", kind: "amount", hasKey: false, valid: false }),
];
const NORMAL_HOME = home(
  [
    row({ id: "claude", state: "limited", tone: "bad", tier: 0, urgency: 0, bindingWindow: "5 小时",
      headline: { percent: 0 }, qualifier: "限流", secondary: "5 小时窗 · 2.0 小时 后恢复 · 周期 62%",
      etaSecs: 7200, checkin: { enabled: false, done: false } }),
    row({ id: "ds", state: "fresh", tone: "bad", tier: 0, urgency: 2,
      headline: { amount: 0.03, unit: "CNY" }, qualifier: "低于底线", checkin: { enabled: true, done: false } }),
    row({ id: "4sapi", tone: "warn", urgency: 0.24, headline: { amount: 120, unit: "CNY" },
      secondary: "按当前速度约可用 120 天", checkin: { enabled: false, done: false } }),
    row({ id: "oc", urgency: 0.22, bindingWindow: "5 小时", headline: { percent: 22 },
      checkin: { enabled: true, done: true } }),
    row({ id: "wb", tone: "bad", tier: 3, urgency: 0.4, headline: { points: 1000 },
      checkin: { enabled: true, done: true } }),
    row({ id: "codex", urgency: 0.42, bindingWindow: "周期", headline: { percent: 42 } }),
    row({ id: "kimi", urgency: 0.88, bindingWindow: "周期", headline: { percent: 88 } }),
    row({ id: "old", state: "stale", urgency: 0.5, headline: { amount: 50, unit: "CNY" },
      qualifier: "~", secondary: "1 小时 前的快照", ageSecs: 3600 }),
    row({ id: "none", state: "noKey", tone: "off", tier: 4, urgency: null,
      headline: {}, qualifier: "未配置" }),
  ],
  { risk: "claude", actionCount: 2, staleCount: 1, errorCount: 0,
    checkin: { total: 3, done: 2, pending: ["ds"] },
    totals: { cny: 170.03, others: [], today: 8, todayPrev: 6, hidden: 0 } }
);
const NORMAL_CK = {
  ds: { id: "ds", name: "DeepSeek", enabled: true, auto: true, outcome: "retry:ready", credits: 100,
    canTry: true, doneToday: false, lastMessage: null, streakDays: null, monthCredits: 0, nextTryAt: null },
  oc: { id: "oc", name: "OpenCode Go", enabled: true, auto: true, outcome: "claimed", credits: 100,
    canTry: false, doneToday: true, lastMessage: null, streakDays: 5, monthCredits: 900, nextTryAt: null },
  wb: { id: "wb", name: "WorkBuddy", enabled: true, auto: true, outcome: "already", credits: null,
    canTry: false, doneToday: true, lastMessage: null, streakDays: null, monthCredits: 0, nextTryAt: null },
};

// 14 个渠道:过滤条出现 + 紧凑条要收 +N
const MANY_CH = [
  ...NORMAL_CH,
  pct("m1", "通义灵码", 66, false), pct("m2", "Qwen Coder", 74, false),
  pct("m3", "GLM Coding", 31, false), pct("m4", "Moonshot 企业版", 55, false),
  ch({ id: "m5", name: "中转站 A(自定义)", kind: "amount", remaining: 888.88, unit: "CNY" }),
];
const MANY_HOME = (() => {
  const h = JSON.parse(JSON.stringify(NORMAL_HOME));
  for (const c of MANY_CH.slice(NORMAL_CH.length)) {
    h.rows.push(row({ id: c.id, headline: c.kind === "percent" ? { percent: c.remaining } : { amount: c.remaining, unit: c.unit } }));
    h.order.push(c.id);
  }
  return h;
})();

// 极端文案:超长名字 + 六位数余额 + 千分位
const BIG_CH = [
  ch({ id: "big1", name: "某个名字特别特别长的中转站渠道", kind: "amount", remaining: 1234567.89, unit: "CNY", day: 12345.67 }),
  ch({ id: "big2", name: "Hapi Enterprise 企业版", kind: "amount", remaining: 98765.43, unit: "USD", day: 1234 }),
  pct("big3", "Claude Code Max 20x 订阅", 99.9, false),
  ch({ id: "big4", name: "积分渠道", kind: "points", remaining: 98765432, unit: "credits" }),
];
const BIG_HOME = home(
  BIG_CH.map((c, i) =>
    row({ id: c.id, tier: i === 0 ? 0 : 1, urgency: i === 0 ? 0 : 0.3 + i / 10,
      tone: i === 0 ? "bad" : "ok",
      headline: c.kind === "percent" ? { percent: c.remaining }
        : c.kind === "points" ? { points: c.remaining } : { amount: c.remaining, unit: c.unit },
      secondary: i === 0 ? "今日 ¥12,345.67 ↑1312%" : null, qualifier: i === 0 ? "低于底线" : null })
  ),
  { risk: "big1", actionCount: 1, totals: { cny: 1333333.32, others: [{ unit: "USD", amount: 98765.43 }], today: 13579.67, todayPrev: 1000, hidden: 0 } }
);

const SCENARIOS = {
  normal: { CH: NORMAL_CH, HOME: NORMAL_HOME, CK: NORMAL_CK },
  loading: { CH: [], HOME: null, CK: {} },
  // 全部隐藏 = 真实的"空面板"(CHANNELS 为空只会在冷启动那一瞬间出现,见 loading)
  empty: {
    CH: [
      ch({ id: "a", name: "4SAPI", hidden: true, hasKey: true }),
      ch({ id: "b", name: "DeepSeek", hidden: true, hasKey: true }),
    ],
    HOME: home([], { totals: { cny: null, others: [], today: null, todayPrev: null, hidden: 2 } }),
    CK: {},
    CFG: { hiddenChannels: ["a", "b"] },
  },
  many: { CH: MANY_CH, HOME: MANY_HOME, CK: NORMAL_CK },
  big: { CH: BIG_CH, HOME: BIG_HOME, CK: {} },
  fails: { CH: NORMAL_CH, HOME: Object.assign({}, NORMAL_HOME, { errorCount: 4 }), CK: NORMAL_CK },
  checkin: (() => {
    // 只放**真的在 CHECKIN_ON 里**的渠道:trae / workbuddy / qoder,否则徽标不渲染,
    // 这一组就白测了(踩过:用 ds/oc 当 id,截图里一个徽标都没有)
    const CH = [
      ch({ id: "trae", name: "Trae", kind: "points", remaining: 3200, unit: "credits", valid: true }),
      ch({ id: "workbuddy", name: "WorkBuddy", kind: "points", remaining: 1000, unit: "credits" }),
      ch({ id: "qoder", name: "Qoder", kind: "points", remaining: 186, unit: "credits" }),
      pct("oc", "OpenCode Go", 22, false),
    ];
    const rows = [
      row({ id: "trae", tone: "ok", headline: { points: 3200 }, checkin: { enabled: true, done: true } }),
      row({ id: "workbuddy", tone: "bad", tier: 3, urgency: 0.4, headline: { points: 1000 },
        checkin: { enabled: true, done: true } }),
      row({ id: "qoder", tone: "ok", headline: { points: 186 }, checkin: { enabled: true, done: true } }),
      row({ id: "oc", urgency: 0.22, bindingWindow: "5 小时", headline: { percent: 22 },
        checkin: { enabled: true, done: false } }),
    ];
    const HOME = home(rows, {
      risk: "workbuddy", actionCount: 1,
      checkin: { total: 4, done: 3, pending: ["oc"] },
      totals: { cny: null, others: [], today: null, todayPrev: null, hidden: 0 },
    });
    return {
      CH, HOME,
      CK: {
        trae: { id: "trae", name: "Trae", enabled: true, auto: true, outcome: "claimed", credits: 100,
          canTry: false, doneToday: true, lastMessage: null, streakDays: null, monthCredits: 300, nextTryAt: null },
        workbuddy: { id: "workbuddy", name: "WorkBuddy", enabled: true, auto: true, outcome: "already", credits: null,
          canTry: false, doneToday: true, lastMessage: null, streakDays: null, monthCredits: 0, nextTryAt: null },
        qoder: { id: "qoder", name: "Qoder", enabled: true, auto: true, outcome: "manual:1024 | 创造力集结",
          credits: null, canTry: false, doneToday: true, lastMessage: "1024 | 创造力集结", streakDays: null,
          monthCredits: 0, nextTryAt: null },
        oc: { id: "oc", name: "OpenCode Go", enabled: true, auto: true, outcome: "retry:ready", credits: 100,
          canTry: true, doneToday: false, lastMessage: null, streakDays: null, monthCredits: 0, nextTryAt: null },
      },
    };
  })(),
  auth: {
    CH: NORMAL_CH.map((c) => (c.id === "claude" ? Object.assign({}, c, { valid: false, error: "HTTP 401", limited: false }) : c)),
    HOME: home(
      NORMAL_HOME.rows.map((r) => (r.id === "claude" ? Object.assign({}, r, { state: "auth", tone: "bad", qualifier: "需登录", headline: {} }) : r)),
      { risk: "claude", actionCount: 1, totals: NORMAL_HOME.totals, checkin: NORMAL_HOME.checkin }
    ),
    CK: { claude: { id: "claude", name: "Claude Code", enabled: true, auto: true, outcome: "auth",
      credits: null, canTry: false, doneToday: true, lastMessage: null, nextTryAt: null } },
  },
  // 特大字号:窗口尺寸应当跟着长,内容不许被裁
  xl: { CH: NORMAL_CH, HOME: NORMAL_HOME, CK: NORMAL_CK, CFG: { fontScale: "xl" } },
  // 非颜色模式:状态靠文字,不靠颜色
  texttone: { CH: NORMAL_CH, HOME: NORMAL_HOME, CK: NORMAL_CK, CFG: { textTone: true } },
};

// ── CDP ──
async function connect() {
  const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  const pages = targets.filter((x) => x.type === "page");
  const t = pages.find((x) => /grandettoken|tauri/i.test(`${x.title} ${x.url}`));
  if (!t) {
    console.error(`端口 ${PORT} 上没有应用页面。候选:`);
    for (const x of pages) console.error(`  ${x.type} ${x.title} ${x.url}`);
    process.exit(1);
  }
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.addEventListener("open", res); ws.addEventListener("error", rej); });
  let id = 0;
  const pending = new Map();
  ws.addEventListener("message", (ev) => {
    const m = JSON.parse(ev.data);
    const cb = pending.get(m.id);
    if (cb) { pending.delete(m.id); cb(m); }
  });
  const send = (method, params = {}) => new Promise((res, rej) => {
    const i = ++id;
    pending.set(i, (m) => (m.error ? rej(new Error(`${method}: ${m.error.message}`)) : res(m.result)));
    ws.send(JSON.stringify({ id: i, method, params }));
  });
  const evalJs = (expression, awaitPromise = false) => new Promise((res, rej) => {
    const i = ++id;
    pending.set(i, (m) => {
      const ex = m.result && m.result.exceptionDetails;
      if (ex) rej(new Error(String((ex.exception && ex.exception.description) || ex.text).split("\n")[0]));
      else res(m.result.result.value);
    });
    ws.send(JSON.stringify({ id: i, method: "Runtime.evaluate", params: { expression, awaitPromise, returnByValue: true } }));
  });
  return { send, evalJs, close: () => ws.close() };
}

const MEASURE = `(() => {
  const q = (s) => document.querySelector(s);
  const rect = (e) => { if (!e) return null; const r = e.getBoundingClientRect();
    return { l: Math.round(r.left), t: Math.round(r.top), w: Math.round(r.width), h: Math.round(r.height) }; };
  const cs = (e) => e ? getComputedStyle(e) : null;
  const list = q("#list");
  // 被祖先的 overflow:hidden 裁掉的元素不算"越界":那是有意的裁切
  // (行内的 .sweep 微光从 -360 扫进来,就是靠这个藏在外面)
  const clipped = (el) => {
    let p = el.parentElement;
    while (p && p !== document.body) {
      const c = cs(p);
      if (c && (c.overflow !== "visible" || c.overflowX !== "visible")) return true;
      p = p.parentElement;
    }
    return false;
  };
  const overflow = [];
  document.querySelectorAll("body *").forEach((el) => {
    const c = cs(el);
    if (!c || c.display === "none" || c.visibility === "hidden" || Number(c.opacity) === 0) return;
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) return;
    // 只报横向越界:面板纵向滚动是设计
    if (r.right > innerWidth + 1 || r.left < -1) {
      if (clipped(el)) return;
      overflow.push((el.id ? "#" + el.id : el.tagName.toLowerCase()) +
        (typeof el.className === "string" && el.className ? "." + el.className.trim().split(/\\s+/).join(".") : "") +
        " [" + Math.round(r.left) + ".." + Math.round(r.right) + "]");
    }
  });
  const cut = [];
  document.querySelectorAll("body *").forEach((el) => {
    const c = cs(el);
    if (!c || c.display === "none") return;
    if (c.textOverflow === "ellipsis" || c.overflow === "hidden") {
      if (el.scrollWidth > el.clientWidth + 1 && el.textContent.trim() && el.children.length === 0) {
        cut.push((el.id ? "#" + el.id : el.tagName.toLowerCase()) +
          (typeof el.className === "string" && el.className ? "." + el.className.trim().split(/\\s+/).join(".") : "") +
          " '" + el.textContent.trim().slice(0, 18) + "' " + el.scrollWidth + ">" + el.clientWidth);
      }
    }
  });
  // 纵向越界:卡片(含内边距)以外的内容。面板的列表自己滚(clipped 会跳过它的孩子),
  // 折叠形态没有滚动条 —— 撑出去就是被裁。
  const card = document.body.getBoundingClientRect();
  const vover = [];
  document.querySelectorAll("body *").forEach((el) => {
    const c = cs(el);
    if (!c || c.display === "none" || c.position === "fixed" || c.position === "absolute") return;
    const r = el.getBoundingClientRect();
    if (r.height <= 0) return;
    if (r.bottom > card.bottom + 1 && !clipped(el)) {
      vover.push((el.id ? "#" + el.id : el.tagName.toLowerCase()) +
        (typeof el.className === "string" && el.className ? "." + el.className.trim().split(/\\s+/).join(".") : "") +
        " bottom=" + Math.round(r.bottom) + " > card=" + Math.round(card.bottom));
    }
  });
  return JSON.stringify({
    form: document.body.className, win: [innerWidth, innerHeight],
    list: list ? { h: list.clientHeight, sh: list.scrollHeight, w: list.clientWidth, sw: list.scrollWidth,
      rows: list.querySelectorAll(".row").length } : null,
    tbar: rect(q(".tbar")), sum: rect(q("#sum")), foot: rect(q(".foot")),
    cctl: rect(q("#cctl")), pill: rect(q("#pill")), dock: rect(q("#dockBody")),
    overflow: overflow.slice(0, 12), vover: vover.slice(0, 8), cut: cut.slice(0, 12),
  }, null, 1);
})()`;

(async () => {
  const { send, evalJs, close } = await connect();
  fs.mkdirSync(OUT, { recursive: true });
  await send("Emulation.setDefaultBackgroundColorOverride", { color: { r: 255, g: 255, b: 255, a: 255 } });
  // 轮询每 60 秒会把真实数据灌回来,把注入的合成数据盖掉(实测:跑到 empty 时
  // 冒出了 8 行真实渠道)。app.js 顶部是 `const invoke = T.core.invoke` —— **按值捕获**,
  // 改 T.core.invoke 没用;直接停掉轮询入口(refresh / loadCheckin 是函数声明,
  // 覆盖 window 上的同名绑定就能让所有调用点走空实现),注入的数据才留得住。
  await evalJs(`(() => {
    if (window.__pollOff) return "already";
    window.__pollOff = true;
    window.refresh = () => {};
    window.loadCheckin = () => {};
    window.__FIX = { CH: null, HOME: null, CK: null };
    return "patched";
  })()`);
  const report = {};
  const names = Object.keys(SCENARIOS).filter((n) => !ONLY.length || ONLY.includes(n));

  for (const name of names) {
    const s = SCENARIOS[name];
    // 注入数据(不落盘:全程不调 saveCfg)
    await evalJs(`(() => {
      const d = ${JSON.stringify({ CH: s.CH, HOME: s.HOME, CK: s.CK, CFG: s.CFG || {} })};
      window.__FIX = { CH: d.CH, HOME: d.HOME, CK: d.CK };
      CHANNELS = d.CH;
      if (d.HOME) hvApply(d.HOME);
      else { HV = { byId: {}, order: [], risk: null, checkin: null, totals: null,
        actionCount: 0, staleCount: 0, errorCount: 0 }; }
      CK = d.CK;
      CFG.textTone = false;
      CFG.fontScale = "md";
      Object.assign(CFG, d.CFG);
      return "ok";
    })()`);

    for (const form of ["panel", "compact", "pill"]) {
      await evalJs(`applyForm(${JSON.stringify(form)}, false)`, true);
      await sleep(1100);
      await evalJs(`render({ updated: false })`);
      await sleep(500);
      const m = JSON.parse(await evalJs(MEASURE));
      const file = `${name}-${form}.png`;
      const r = await send("Page.captureScreenshot", { format: "png" });
      fs.writeFileSync(path.join(OUT, file), Buffer.from(r.data, "base64"));
      report[`${name}/${form}`] = m;
      const flag = m.overflow.length ? `  ⚠ 横越界 ${m.overflow.length}` : "";
      const vflag = m.vover.length ? `  ⚠ 竖越界 ${m.vover.length}` : "";
      const cutf = m.cut.length ? `  ✂ 裁切 ${m.cut.length}` : "";
      console.log(`${name.padEnd(8)} ${form.padEnd(8)} ${m.win.join("x").padEnd(9)} rows=${m.list ? m.list.rows : "-"}${flag}${vflag}${cutf}`);
    }
  }

  fs.writeFileSync(path.join(OUT, "report.json"), JSON.stringify(report, null, 1));
  console.log(`\n截图与报告:${OUT}`);
  // 还原:整页重载,真实数据从磁盘/后端重新来一遍(注入的东西一样都不留)
  try { await send("Page.reload", { ignoreCache: true }); } catch {}
  await sleep(1500);
  close();
  process.exit(0);
})().catch((e) => { console.error("失败:", e.message); process.exit(1); });
