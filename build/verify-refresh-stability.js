/**
 * 首页刷新稳定性快照(真实排版,不是 jsdom 假几何)。
 *
 * 做法:把仓库里真实的 index.html + styles.css + app.js 装进无头 Edge,注入
 * 一份可控的合成数据,逐个场景在刷新前后量:
 *   · 渠道顺序 / 每张卡片的矩形 / 摘要高度 / #list.scrollTop / 当前焦点
 *   · 节点身份(直接持有 DOM 引用,刷新后看是否仍是同一个对象)
 *   · 逐帧的 transform / ghost / sweep / roll-in(Layout Shift 只作辅助证据)
 * 结果写进 <script id="__QA_RESULT">,由 --dump-dom 带回。
 *
 * 用法:
 *   node build/verify-refresh-stability.js --out build/qa/refresh-baseline.json
 *   node build/verify-refresh-stability.js --out build/qa/refresh-after.json --reduced
 *   node build/verify-refresh-stability.js --compare build/qa/refresh-baseline.json build/qa/refresh-after.json
 */
const fs = require("fs");
const path = require("path");
const os = require("os");
const { execFileSync } = require("child_process");

const ROOT = path.join(__dirname, "..");
const argv = process.argv.slice(2);
const argOf = (k, d) => {
  const i = argv.indexOf(k);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : d;
};
// --src 可以指向一份改动前的源码副本,用来跑同一套场景做前后对比
const SRC = path.resolve(argOf("--src", path.join(ROOT, "app", "src")));
const EDGE_CANDIDATES = [
  "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
  "C:/Program Files/Microsoft/Edge/Application/msedge.exe",
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
];

if (argv.includes("--compare")) {
  const a = JSON.parse(fs.readFileSync(argv[argv.indexOf("--compare") + 1], "utf8"));
  const b = JSON.parse(fs.readFileSync(argv[argv.indexOf("--compare") + 2], "utf8"));
  console.log("场景".padEnd(26), "顺序变化", "卡片最大位移", "摘要高差", "滚动差", "焦点丢失", "ghost峰值", "sweep峰值", "节点身份");
  for (const s of b.scenarios) {
    const old = a.scenarios.find((x) => x.name === s.name);
    const d = s.diff;
    console.log(
      s.name.padEnd(26),
      String(d.orderChanged).padEnd(9),
      String(d.maxRectDelta).padEnd(13),
      String(d.sumHeightDelta).padEnd(9),
      String(d.scrollDelta).padEnd(7),
      String(d.focusKept === null ? "—" : !d.focusKept).padEnd(9),
      String(d.ghostPeak + (old ? " (前 " + old.diff.ghostPeak + ")" : "")).padEnd(11),
      String(d.sweepPeak + (old ? " (前 " + old.diff.sweepPeak + ")" : "")).padEnd(11),
      d.allNodesKept ? "全部保留" : "有重建"
    );
  }
  // 这个分支只打印前后对照表,**没有阈值、没有断言**,退出码 0 不代表回归通过。
  // 以前没有任何说明,容易被当成「跑过了 = 全绿」。判定请跑不带 --compare 的主流程。
  console.log("\n[说明] --compare 仅输出对照数据,不做通过/失败判定,退出码 0 不代表门禁通过。");
  process.exit(0);
}

// ───────────────────────── 合成数据 ─────────────────────────
const NOW = Math.floor(Date.now() / 1000);
const iso = (s) => new Date((NOW + s) * 1000).toISOString();
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
      expiryComplete: true, resetCredits: null, grantToday: null, recovery: null,
    },
    o
  );

const DATA = (() => {
  const channels = [
    ch({ id: "amtA", name: "Alpha 余额", short: "AL", remaining: 1000, total: 2000, day: 12, week: 80, month: 200 }),
    ch({ id: "amtB", name: "Beta 余额", short: "BE", remaining: 500, total: 1000, day: 8, week: 40, month: 100 }),
    ch({
      id: "pctC", name: "Gamma 配额", short: "GA", kind: "percent", remaining: 62, unit: "percent",
      windows: [{ label: "周期", percent: 38, remainPercent: 62, status: "ok", resetsAt: iso(10 * 86400), main: true }],
    }),
    ch({
      id: "codex", name: "Codex 双配额", short: "CX", kind: "percent", unit: "percent", remaining: 42,
      windows: [
        { label: "周期", percent: 58, remainPercent: 42, status: "ok", resetsAt: iso(9 * 86400), main: true },
        { label: "5 小时", percent: 12, remainPercent: 88, status: "ok", resetsAt: iso(2 * 3600) },
        { label: "本周", percent: 58, remainPercent: 42, status: "ok", resetsAt: iso(4 * 86400) },
      ],
      extra: [["套餐", "Plus"]],
      resetCredits: { availableCount: 2, credits: [] },
    }),
    ch({
      id: "pts", name: "Delta 积分", short: "DE", kind: "points", unit: "points", remaining: 12345,
      expiring: [{ at: NOW + 3 * 86400, amount: 3000 }],
      grantToday: { total: 30, activityTotal: 30, hasActivity: true, lastAt: NOW - 3600, fromApi: true },
    }),
    ch({ id: "trae", name: "Trae 签到", short: "TR", remaining: 200, total: 400, day: 4 }),
    ch({
      id: "sapi", name: "4SAPI 申请制", short: "4S", remaining: 120, total: 500, day: 3,
      claim: { amount: 500, lastClaimAt: NOW - 20 * 86400, source: "auto", daysUntilEligible: 0,
        eligible: true, dailyBurn: 4, daysOfBalance: 30, shortageRisk: false },
    }),
  ];
  const home = {
    rows: channels.map((c) => ({
      id: c.id, state: "fresh", tone: "ok", tier: 1, urgency: 0.5, bindingWindow: null,
      headline: c.kind === "points" ? { points: c.remaining } : c.kind === "percent" ? { percent: c.remaining } : { amount: c.remaining, unit: c.unit },
      qualifier: null, secondary: null, etaSecs: null, ageSecs: 5, checkin: { enabled: false, done: false },
    })),
    order: channels.map((c) => c.id), checkin: { total: 1, done: 0, pending: ["trae"] },
    actionCount: 0, staleCount: 0, errorCount: 0, now: NOW,
  };
  const cfg = {
    sort: "balance", warnPercent: 40, critPercent: 15, moneyFloor: 10, textTone: false,
    fontScale: "md", panelLayout: "single", form: "panel", theme: "dark",
    hiddenChannels: [], channelOrder: [], pillChannels: [], alertPulse: false,
    peekOnClick: true, alwaysOnTop: false, dockEnabled: false, compactEnabled: true,
    activeIntervalSec: 60, idleIntervalSec: 300, backoffIntervalSec: 900,
    trayShowNumber: true, autostart: false, autoCheckUpdate: false, notifyOnCrit: true,
    soundOn: false, resizeHintSeen: true, modelShowAll: false, modelGroup: null,
    modelBaseUrl: null, claimChannels: { sapi: { enabled: true, amount: 500, minIntervalDays: 14 } },
  };
  const ck = { trae: { enabled: true, doneToday: false, canTry: true, name: "Trae 签到" } };
  return { channels, home, cfg, ck };
})();

// ───────────────────────── 注入桩 ─────────────────────────
function qaStubs() {
  const D = window.__QA_DATA;
  const clone = (o) => JSON.parse(JSON.stringify(o));
  window.__QA_ERR = [];
  window.addEventListener("error", (e) => window.__QA_ERR.push(e.message + " @" + e.filename + ":" + e.lineno));
  window.addEventListener("unhandledrejection", (e) => window.__QA_ERR.push("reject:" + String((e.reason && e.reason.message) || e.reason)));
  window.__TAURI__ = {
    core: {
      invoke: async (cmd) => {
        if (cmd === "get_config") return clone(D.cfg);
        if (cmd === "get_channels") return clone(D.channels);
        if (cmd === "home_view") return clone(D.home);
        if (cmd === "checkin_state") return [];
        if (cmd === "checkin_history") return null;
        if (cmd === "app_version") return "0.0.0-qa";
        if (cmd === "sync_qr_svg") return '<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h1v1H0z"/></svg>';
        if (cmd === "vault_list") return [];
        if (cmd === "vault_ref_map") return [];
        return null;
      },
    },
    event: { listen: async () => () => {} },
    dpi: {
      LogicalSize: class { constructor(w, h) { this.width = w; this.height = h; } },
      PhysicalPosition: class { constructor(x, y) { this.x = x; this.y = y; } },
    },
    window: {
      currentMonitor: async () => null,
      getCurrentWindow: () =>
        new Proxy(
          {
            outerPosition: async () => ({ x: 0, y: 0 }),
            outerSize: async () => ({ width: 420, height: 700 }),
            innerSize: async () => ({ width: 420, height: 700 }),
            isVisible: async () => true,
            isAlwaysOnTop: async () => false,
            scaleFactor: async () => 1,
            theme: async () => "dark",
          },
          { get: (t, k) => (k in t ? t[k] : async () => () => {}) }
        ),
    },
  };
  window.Notification = class { static requestPermission() {} };
  const ctx = new Proxy(
    { canvas: { width: 64, height: 64 }, measureText: () => ({ width: 10 }),
      createLinearGradient: () => ({ addColorStop() {} }),
      createRadialGradient: () => ({ addColorStop() {} }),
      getImageData: () => ({ data: new Uint8ClampedArray(4) }) },
    { get: (t, k) => (k in t ? t[k] : () => {}) }
  );
  window.HTMLCanvasElement.prototype.getContext = () => ctx;
  window.HTMLCanvasElement.prototype.toDataURL = () => "data:image/png;base64,";
}

// ───────────────────────── 浏览器内驱动 ─────────────────────────
function qaDriver() {
  const D = window.__QA_DATA;
  const out = { scenarios: [], meta: {}, extra: {}, errors: [] };
  let STAGE = "boot";
  const stage = (s) => { STAGE = s; };
  const writeResult = (o) => {
    if (document.getElementById("__QA_RESULT")) return;
    const n = document.createElement("script");
    n.id = "__QA_RESULT";
    n.type = "application/json";
    n.textContent = JSON.stringify(Object.assign({ stage: STAGE, jsErrors: window.__QA_ERR || [] }, o));
    document.body.appendChild(n);
  };
  setTimeout(() => writeResult({ fatal: "watchdog: 驱动未完成", scenarios: out.scenarios, extra: out.extra, meta: out.meta }), 16000);
  const clone = (o) => JSON.parse(JSON.stringify(o));
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  // 无头 + 虚拟时间下 rAF 不保证持续产帧;getBoundingClientRect 会强制同步排版,
  // 用计时器让样式/动画生效即可,不依赖产帧。
  const frame = () => sleep(16);
  const round = (n) => Math.round(n * 10) / 10;
  const rect = (el) => {
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return { x: round(r.x), y: round(r.y), w: round(r.width), h: round(r.height) };
  };
  let FOCUS_REF = null;
  let lsTotal = 0;
  let mutTotal = 0;
  let mutLog = [];
  try {
    new MutationObserver((recs) => {
      mutTotal += recs.length;
      for (const r of recs) mutLog.push(r.type + ":" +
        (r.target.nodeType === 1 ? r.target.tagName.toLowerCase() + "." + (r.target.className || "") : "#text") +
        (r.attributeName ? "[" + r.attributeName + "]" : ""));
      if (mutLog.length > 60) mutLog = mutLog.slice(-60);
    }).observe(document.getElementById("list"), {
      subtree: true, childList: true, attributes: true, characterData: true,
    });
  } catch (e) { out.errors.push("mutation observer: " + e.message); }
  try {
    const po = new PerformanceObserver((l) => {
      for (const e of l.getEntries()) if (!e.hadRecentInput) lsTotal += e.value;
    });
    po.observe({ type: "layout-shift", buffered: true });
  } catch (e) { out.errors.push("layout-shift observer: " + e.message); }

  function apply(d, updated) {
    CHANNELS = clone(d.channels);
    CFG = clone(d.cfg);
    hvApply(clone(d.home));
    CK = clone(d.ck || {});
    EXPIRY_FILTER = !!d.expiryFilter;
    render({ updated: !!updated });
  }
  function base() { return clone(D); }

  function rowsOf(list) {
    return [...list.querySelectorAll(".row")];
  }
  function snap(label) {
    const list = document.getElementById("list");
    const sum = document.getElementById("sum");
    const ae = document.activeElement;
    const rows = rowsOf(list).map((r) => {
      const v = r.querySelector(".val .v");
      const cs = v ? getComputedStyle(v) : null;
      const s = r.querySelector(".nm .s");
      const p = r.querySelector(".val .p");
      const r2 = r.querySelector(".r2");
      const bar = r.querySelector(".bar");
      const q = (sel) => r.querySelector(sel);
      return {
        id: r.dataset.id, cls: r.className,
        state: r.dataset.state, dtone: r.dataset.dtone, railwarn: r.dataset.railwarn,
        rect: rect(r), headRect: rect(q(".rhead")), vRect: rect(v),
        nRect: rect(q(".nm .n")), sRect: rect(s), pRect: rect(p), r2Rect: rect(r2), barRect: rect(bar),
        badgeRects: [...r.querySelectorAll(".nm .n .dot,.qtag,.wmark,.ckb")].map(rect),
        vText: v ? v.textContent : null, vColor: cs ? cs.color : null,
        vTransform: cs ? cs.transform : null, vFont: cs ? cs.fontVariantNumeric : null,
        sText: s ? s.textContent : null, pText: p ? p.textContent : null,
        r2Text: r2 ? r2.textContent : null,
        barWidth: bar && bar.firstElementChild ? bar.firstElementChild.style.width : null,
        warn: [...r.querySelectorAll(".claim-warn,.win-warn")].map((e) => e.textContent),
        badges: [...r.querySelectorAll(".qtag,.wmark,.ckb")].map((e) => e.className + ":" + e.textContent.trim()),
        decCount: v ? v.querySelectorAll("i.dec,i.unit,i.pct,i.tilde").length : 0,
      };
    });
    return {
      label,
      order: rows.map((r) => r.id), rows,
      sumRect: rect(sum),
      sumHeight: sum ? round(sum.getBoundingClientRect().height) : null,
      listRect: rect(list),
      listScrollTop: list ? list.scrollTop : null,
      focus: {
        tag: ae && ae.tagName, cls: ae && typeof ae.className === "string" ? ae.className : null,
        id: ae && ae.id, rowId: ae && ae.closest && ae.closest(".row") ? ae.closest(".row").dataset.id : null,
        kept: FOCUS_REF ? ae === FOCUS_REF : null,
      },
      ghosts: document.querySelectorAll(".roll-ghost").length,
      rollIn: document.querySelectorAll(".roll-in").length,
      sweepsGo: document.querySelectorAll(".sweep.go").length,
      sweepTransforms: [...document.querySelectorAll(".sweep")].slice(0, 4).map((e) => getComputedStyle(e).transform),
      strips: {
        fail: !!list.querySelector(".fstrip"), ck: !!list.querySelector(".ckstrip"),
        empty: !!list.querySelector(".empty"),
      },
      bodyClass: document.body.className,
      lsTotal: round(lsTotal * 1000) / 1000,
      mutTotal, mutLog: mutLog.slice(-20),
    };
  }
  function refsNow() {
    const list = document.getElementById("list");
    return rowsOf(list).map((r) => ({
      id: r.dataset.id, row: r,
      head: r.querySelector(".rhead"), v: r.querySelector(".val .v"),
      bar: r.querySelector(".bar"), barI: r.querySelector(".bar > i"),
      r2: r.querySelector(".r2"),
    }));
  }
  function identity(refs) {
    const list = document.getElementById("list");
    return refs.map((r) => ({
      id: r.id,
      rowKept: r.row.isConnected && r.row.parentElement === list && r.row.dataset.id === r.id,
      headKept: !!r.head && r.head.isConnected && r.head.parentElement === r.row,
      vKept: !!r.v && r.v.isConnected && r.v.parentElement === r.row.querySelector(".val"),
      r2Kept: !!r.r2 && r.r2.isConnected && r.r2.parentElement === r.row.querySelector(".rhead"),
      barKept: r.bar ? r.bar.isConnected && r.bar.parentElement === r.row.querySelector(".rhead") : null,
      barIKept: r.barI ? r.barI.isConnected && r.barI.parentElement === r.bar : null,
    }));
  }
  function summarize(b, a, ident, frames) {
    const byId = new Map(b.rows.map((r) => [r.id, r]));
    let maxRectDelta = 0;
    let maxXDelta = 0;
    const rectDeltas = [];
    for (const r of a.rows) {
      const o = byId.get(r.id);
      if (!o) { rectDeltas.push({ id: r.id, delta: null }); continue; }
      const d = Math.max(
        Math.abs(r.rect.x - o.rect.x), Math.abs(r.rect.y - o.rect.y),
        Math.abs(r.rect.w - o.rect.w), Math.abs(r.rect.h - o.rect.h)
      );
      maxRectDelta = Math.max(maxRectDelta, round(d));
      const xs = [["nRect", r.nRect, o.nRect], ["sRect", r.sRect, o.sRect], ["r2Rect", r.r2Rect, o.r2Rect]];
      let dx = 0;
      for (const [, x, y] of xs) if (x && y) dx = Math.max(dx, Math.abs(x.x - y.x));
      for (let i = 0; i < Math.min(r.badgeRects.length, o.badgeRects.length); i++) {
        dx = Math.max(dx, Math.abs(r.badgeRects[i].x - o.badgeRects[i].x));
      }
      maxXDelta = Math.max(maxXDelta, round(dx));
      rectDeltas.push({ id: r.id, delta: round(d), dh: round(r.rect.h - o.rect.h), dx: round(dx) });
    }
    const allFrames = [a, ...frames];
    return {
      orderChanged: b.order.join(",") !== a.order.join(","),
      beforeOrder: b.order, afterOrder: a.order,
      maxRectDelta, maxXDelta, rectDeltas,
      sumHeightDelta: round(a.sumHeight - b.sumHeight),
      scrollDelta: round(a.listScrollTop - b.listScrollTop),
      focusKept: a.focus.kept,
      focusBefore: b.focus, focusAfter: a.focus,
      ghostPeak: Math.max(...allFrames.map((f) => f.ghosts)),
      rollInPeak: Math.max(...allFrames.map((f) => f.rollIn)),
      sweepPeak: Math.max(...allFrames.map((f) => f.sweepsGo)),
      vTransforms: allFrames.map((f) => f.rows.map((r) => r.vTransform)),
      decCounts: a.rows.map((r) => ({ id: r.id, n: r.decCount })),
      // 被删掉 / 被筛选掉的渠道不参与"节点身份保留"判定
      allNodesKept: ident.filter((i) => byId.has(i.id) && a.rows.some((r) => r.id === i.id))
        .every((i) => i.rowKept && i.headKept && i.vKept && i.r2Kept && i.barKept !== false && i.barIKept !== false),
      removedIds: ident.filter((i) => !a.rows.some((r) => r.id === i.id)).map((i) => i.id),
      identity: ident,
      lsDelta: round((a.lsTotal - b.lsTotal) * 1000) / 1000,
      mutDelta: a.mutTotal - b.mutTotal,
    };
  }

  async function scenario(name, mutate, opts) {
    const o = opts || {};
    stage("scenario:" + name);
    const d = base();
    if (o.pre) o.pre(d);
    stage("scenario:" + name + ":apply");
    apply(d, false);
    stage("scenario:" + name + ":frame");
    await frame();
    stage("scenario:" + name + ":armed");
    const list = document.getElementById("list");
    list.scrollTop = 40;
    const head = rowsOf(list)[0] && rowsOf(list)[0].querySelector(".rhead");
    if (head) { head.focus({ preventScroll: true }); FOCUS_REF = head; }
    else FOCUS_REF = null;
    const b = snap("before");
    const refs = refsNow();
    if (mutate) mutate(d);
    stage("scenario:" + name + ":refresh");
    apply(d, o.updated !== false);
    await Promise.resolve(); // 让 MutationObserver 的微任务回调先结算
    stage("scenario:" + name + ":after");
    const a = snap("after");
    stage("scenario:" + name + ":f1");
    const f1 = await sleep(70).then(snap.bind(null, "f70"));
    stage("scenario:" + name + ":f2");
    const f2 = await sleep(150).then(snap.bind(null, "f220"));
    stage("scenario:" + name + ":f3");
    const f3 = await sleep(200).then(snap.bind(null, "f420"));
    const ident = identity(refs);
    const frames = [f1, f2, f3];
    out.scenarios.push({
      name, before: b, after: a, frames, identity: ident,
      diff: summarize(b, a, ident, frames),
    });
    FOCUS_REF = null;
  }

  const pick = (d, id) => d.channels.find((c) => c.id === id);
  const hv = (d, id) => d.home.rows.find((r) => r.id === id);

  async function main() {
    stage("wait-init");
    // 等 init() 的首轮渲染落地
    for (let i = 0; i < 120; i++) {
      if (document.querySelectorAll("#list .row").length) break;
      await sleep(25);
    }
    out.meta.initialRows = document.querySelectorAll("#list .row").length;
    out.meta.reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
    out.meta.dpr = window.devicePixelRatio;
    out.meta.viewport = [window.innerWidth, window.innerHeight];

    await scenario("01 相同数据连续刷新", null);
    await scenario("01b 时间戳变化但文案不变", (d) => {
      const t = Math.floor(Date.now() / 1000);
      d.channels.forEach((c) => { c.updatedAt = t - 7200; });
      d.home.now = t;
    });
    await scenario("02 单渠道余额变化(同序)", (d) => { pick(d, "amtA").remaining = 980; hv(d, "amtA").headline.amount = 980; });
    await scenario("03 数字跨位数 1000→999", (d) => { pick(d, "amtA").remaining = 999; hv(d, "amtA").headline.amount = 999; });
    const setFailed = (d) => {
      const c = pick(d, "amtA"); c.valid = false; c.stale = false; c.error = "连接超时";
      const h = hv(d, "amtA"); h.state = "error"; h.tone = "bad"; h.qualifier = "取数失败";
    };
    const setStale = (d) => {
      const c = pick(d, "amtA"); c.stale = true; c.valid = false; c.error = null;
      const h = hv(d, "amtA"); h.state = "stale"; h.tone = "warn"; h.qualifier = "~";
    };
    const setWarn = (d) => {
      const c = pick(d, "amtA"); c.valid = true; c.stale = false; c.limited = true;
      const h = hv(d, "amtA"); h.state = "fresh"; h.tone = "warn"; h.qualifier = "低于底线"; h.tier = 0;
    };
    // 状态切换用自定义序钉住顺序,单独量"卡片高度/内容"是否推动列表;
    // 真实排序变化由 06 组单独验收。
    const fixedOrder = (d) => { d.cfg.sort = "custom"; d.cfg.channelOrder = d.channels.map((c) => c.id); };
    await scenario("04 正常→失败(固定序)", setFailed, { pre: fixedOrder });
    await scenario("04b 失败→陈旧(固定序)", (d) => setStale(d), { pre: (d) => { fixedOrder(d); setFailed(d); } });
    await scenario("04c 陈旧→正常(固定序)", (d) => {
      const c = pick(d, "amtA"); c.stale = false; c.valid = true;
      const h = hv(d, "amtA"); h.state = "fresh"; h.tone = "ok"; h.qualifier = null;
    }, { pre: (d) => { fixedOrder(d); setStale(d); } });
    await scenario("04d 正常→告警(固定序)", setWarn, { pre: fixedOrder });
    await scenario("05 状态轨/进度条出现", (d) => {
      const c = pick(d, "amtB"); c.total = 1000; c.day = 8;
      hv(d, "amtB").secondary = "窗口 · 2 天后重置";
    }, {});
    await scenario("05b 申请额警告出现", (d) => { pick(d, "sapi").claim.shortageRisk = true; pick(d, "sapi").claim.daysOfBalance = 2; });
    await scenario("05c 周期警告出现", (d) => {
      const w = pick(d, "pctC").windows[0]; w.remainPercent = 8; w.percent = 92;
      pick(d, "pctC").remaining = 8;
    });
    await scenario("05d 失败横幅+签到条出现", (d) => { d.home.errorCount = 3; d.ck.trae.doneToday = false; });
    const warnOn = (d) => {
      pick(d, "sapi").claim.shortageRisk = true; pick(d, "sapi").claim.daysOfBalance = 2;
      const w = pick(d, "pctC").windows[0]; w.remainPercent = 8; w.percent = 92;
      pick(d, "pctC").remaining = 8;
    };
    await scenario("05e 两类警告同时消失", (d) => {
      pick(d, "sapi").claim.shortageRisk = false;
      const w = pick(d, "pctC").windows[0]; w.remainPercent = 62; w.percent = 38;
      pick(d, "pctC").remaining = 62;
    }, { pre: (d) => { d.cfg.sort = "custom"; d.cfg.channelOrder = d.channels.map((c) => c.id); warnOn(d); } });
    await scenario("06 排序键交叉(真实重排)", (d) => { pick(d, "amtA").remaining = 100; hv(d, "amtA").headline.amount = 100; });
    await scenario("06b 排序键相等(平局稳定)", (d) => { pick(d, "amtA").remaining = 500; hv(d, "amtA").headline.amount = 500; });
    await scenario("08 新增渠道", (d) => {
      d.channels.push({ id: "newbie", name: "新渠道", short: "NW", color: "#888", kind: "amount",
        hasKey: true, valid: true, remaining: 42, total: 100, day: 1, unit: "CNY",
        windows: [], expiring: [], extra: [], error: null, limited: false, stale: false,
        updatedAt: Math.floor(Date.now() / 1000), claim: null, hidden: false, peak: null,
        expiryComplete: true, grantToday: null });
      d.home.rows.push({ id: "newbie", state: "fresh", tone: "ok", tier: 1, urgency: 0.5,
        bindingWindow: null, headline: { amount: 42, unit: "CNY" }, qualifier: null,
        secondary: null, etaSecs: null, ageSecs: 5, checkin: { enabled: false, done: false } });
    }, { pre: (d) => { d.cfg.sort = "custom"; d.cfg.channelOrder = d.channels.map((c) => c.id).concat("newbie"); } });
    await scenario("08b 移除渠道", (d) => {
      d.channels = d.channels.filter((c) => c.id !== "sapi");
      d.home.rows = d.home.rows.filter((r) => r.id !== "sapi");
    }, { pre: (d) => { d.cfg.sort = "custom"; d.cfg.channelOrder = d.channels.map((c) => c.id); } });
    await scenario("08c 临期筛选", (d) => { d.expiryFilter = true; },
      { pre: (d) => { d.cfg.sort = "custom"; d.cfg.channelOrder = d.channels.map((c) => c.id); } });

    // 连续快速刷新:三次渲染互不等待,检查旧动画回调是否会留下幽灵
    stage("rapid");
    {
      const d = base();
      apply(d, false);
      await frame();
      const b = snap("before");
      const refs = refsNow();
      for (const v of [990, 980, 970]) {
        d.channels.find((c) => c.id === "amtA").remaining = v;
        d.home.rows.find((r) => r.id === "amtA").headline.amount = v;
        apply(d, true);
        await Promise.resolve();
      }
      const a = snap("after");
      const f1 = await sleep(70).then(snap.bind(null, "f70"));
      const f2 = await sleep(200).then(snap.bind(null, "f270"));
      const f3 = await sleep(250).then(snap.bind(null, "f520"));
      const ident = identity(refs);
      out.scenarios.push({ name: "07 连续快速刷新", before: b, after: a, frames: [f1, f2, f3],
        identity: ident, diff: summarize(b, a, ident, [f1, f2, f3]) });
    }

    // 组合更新保真:Codex 双配额 / 签到徽标 / 告警标签 / 状态轨 / 条件进度条 /
    // 两类警告一起变,逐行比对 DOM 与 renderRow() 的输出是否逐字节一致。
    stage("fidelity");
    {
      const d = base();
      d.cfg.textTone = true;
      apply(d, false);
      await frame();
      const refs = refsNow();
      const c1 = pick(d, "sapi");
      c1.remaining = 360; c1.claim.shortageRisk = true; c1.claim.daysOfBalance = 2;
      const c2 = pick(d, "pctC");
      c2.remaining = 8; c2.windows[0].remainPercent = 8; c2.windows[0].percent = 92;
      const c3 = pick(d, "codex");
      c3.windows[1].remainPercent = 31;
      d.channels.find((c) => c.id === "pts").expiring[0].amount = 9000;
      d.ck.trae.activityState = { status: "claimed" }; d.ck.trae.doneToday = true;
      hv(d, "amtA").qualifier = "低于底线";
      hv(d, "amtA").tone = "bad";
      apply(d, true);
      const list = document.getElementById("list");
      const checks = rowsOf(list).map((r) => {
        const c = d.channels.find((x) => x.id === r.dataset.id);
        if (!c) return { id: r.dataset.id, ok: false, why: "channel missing" };
        const tpl = document.createElement("template");
        tpl.innerHTML = renderRow(c, 0).trim();
        const exp = tpl.content.firstElementChild;
        // 一次性状态红晕 / 数值翻页是临时动画 class,不算展示内容
        const clean = (s) => s
          .replace(/\s(?:roll-in|breath1|ico-in)(?=[\s"])/g, "")
          .replace(/class="sweep go"/g, 'class="sweep"');
        const got = clean(r.outerHTML), want = clean(exp ? exp.outerHTML : "");
        let k = 0;
        while (k < Math.min(got.length, want.length) && got[k] === want[k]) k++;
        return { id: r.dataset.id, ok: got === want, at: k,
          got: got === want ? "" : got.slice(Math.max(0, k - 70), k + 110),
          want: got === want ? "" : want.slice(Math.max(0, k - 70), k + 110) };
      });
      out.extra.fidelity = {
        checks, allOk: checks.every((x) => x.ok),
        nodesKept: identity(refs).every((i) => i.rowKept && i.headKept && i.vKept &&
          i.r2Kept && i.barKept !== false && i.barIKept !== false),
        barWidthBefore: "24%",
        barWidthAfter: (list.querySelector('.row[data-id="sapi"] .bar > i') || { style: {} }).style.width,
        badges: [...list.querySelectorAll(".qtag,.wmark,.ckb")].map((e) => e.className + ":" + e.textContent.trim()),
        warns: [...list.querySelectorAll(".claim-warn,.win-warn")].map((e) => e.textContent.trim()),
      };
    }

    // 胶囊轮播:rollValue 的 ghost 必须排除临时动画 class,并保留数字排版结构
    stage("pill");
    {
      const d = base();
      d.home.rows.find((r) => r.id === "amtA").qualifier = "告急";
      apply(d, false);
      document.body.className = "form-pill";
      PILL.list = [CHANNELS.find((c) => c.id === "amtA")];
      PILL.idx = 0;
      renderPillFace();
      const beforeText = document.getElementById("pillV").textContent;
      const beforeHtml = document.getElementById("pillV").innerHTML;
      d.home.rows.find((r) => r.id === "amtA").headline.amount = 888;
      d.home.rows.find((r) => r.id === "amtA").qualifier = "正常";
      hvApply(d.home);
      renderPillFace();
      const gs = [...document.querySelectorAll(".roll-ghost")];
      // 连续 3 次轮播换值:旧回调不许把新动画清掉,也不许叠出多个幽灵
      for (const v of [777, 666, 555]) {
        d.home.rows.find((r) => r.id === "amtA").headline.amount = v;
        hvApply(d.home);
        renderPillFace();
      }
      const rapid = [...document.querySelectorAll(".roll-ghost")];
      out.extra.pill = {
        beforeText, beforeHtml,
        afterText: document.getElementById("pillV").textContent,
        ghostCount: gs.length,
        ghostClass: gs.map((g) => g.className),
        ghostHtml: gs.map((g) => g.innerHTML),
        ghostHasRollIn: gs.some((g) => g.classList.contains("roll-in")),
        rollInCount: document.querySelectorAll(".roll-in").length,
        rapidGhostCount: rapid.length,
        rapidHasRollIn: rapid.some((g) => g.classList.contains("roll-in")),
        rapidLastHtml: rapid.length ? rapid[rapid.length - 1].innerHTML : null,
      };
      document.body.className = "form-panel";
      apply(d, false);
    }

    // 设置页:.mbody 滚动 / 未保存输入 / 展开态 / 焦点
    stage("settings");
    {
      const d = base();
      apply(d, false);
      openManage();
      showTab("ch");
      const box = document.getElementById("chList");
      const scroller = box && box.closest(".mbody");
      const beforeScroller = scroller ? scroller.scrollTop : null;
      if (scroller) scroller.scrollTop = 160;
      const card = box && box.querySelector('.ccard[data-id="amtA"]');
      const fold = card && card.querySelector('[data-act="chfold"]');
      if (fold) fold.click(); // 走真实交互,cardOpen 集合才会记下展开态
      const inp = box && box.querySelector('[id^="key-"]');
      let typed = null;
      if (inp) { inp.value = "sk-not-saved-123"; inp.focus({ preventScroll: true }); }
      // 把焦点挪到同卡片的按钮上:refreshChCards 对正在输入的 INPUT 会跳过重绘,
      // 这里要测的正是"真的重建"那条路径(草稿 / 展开态 / 滚动 / 按钮焦点)。
      // 假装刚点过「显示」:重绘必须把明文收回去
      const rev = card && card.querySelector('.reveal-box');
      if (rev) { rev.dataset.on = "1"; rev.innerHTML = '<div class="reveal">sk-visible-123</div>'; }
      const focusBtn = card && card.querySelector('button[data-act="savekey"]');
      if (focusBtn) focusBtn.focus({ preventScroll: true });
      const before = {
        scrollTop: scroller ? scroller.scrollTop : null,
        scrollHeight: scroller ? scroller.scrollHeight : null,
        scrollerFound: !!scroller, cardOpen: !!card && card.classList.contains("open"),
        inputValue: inp ? inp.value : null, focusIsInput: inp ? document.activeElement === inp : null,
        inputId: inp ? inp.id : null,
        focusAct: document.activeElement && document.activeElement.getAttribute
          ? document.activeElement.getAttribute("data-act") : null,
      };
      refreshChCards();
      const afterBox = document.getElementById("chList");
      const afterScroller = afterBox && afterBox.closest(".mbody");
      const afterCard = afterBox && afterBox.querySelector('.ccard[data-id="amtA"]');
      const afterInp = afterBox && afterBox.querySelector('[id^="key-"]');
      if (afterInp) typed = afterInp.value;
      out.extra.settings = {
        before, after: {
          scrollTop: afterScroller ? afterScroller.scrollTop : null,
          scrollHeight: afterScroller ? afterScroller.scrollHeight : null,
          scrollerSame: !!afterScroller && afterScroller === scroller,
          cardOpen: !!afterCard && afterCard.classList.contains("open"),
          inputValue: typed,
          focusIsInput: afterInp ? document.activeElement === afterInp : null,
          inputId: afterInp ? afterInp.id : null,
          activeId: document.activeElement ? document.activeElement.id : null,
          focusAct: document.activeElement && document.activeElement.getAttribute
            ? document.activeElement.getAttribute("data-act") : null,
          revealLeaked: /sk-visible-123/.test(afterBox ? afterBox.innerHTML : ""),
          revealOn: afterBox ? afterBox.querySelectorAll('.reveal-box[data-on="1"]').length : null,
        },
      };
      closeManage();
    }

    // 三格摘要:高度稳定 + 按钮节点/焦点保留
    stage("summary");
    {
      const d = base();
      apply(d, false);
      const sum = document.getElementById("sum");
      const cell = sum.querySelector("button.cell");
      if (cell) cell.focus({ preventScroll: true });
      const ref = cell;
      const h0 = round(sum.getBoundingClientRect().height);
      apply(d, true);
      const h1 = round(sum.getBoundingClientRect().height);
      out.extra.summary = { heightBefore: h0, heightAfter: h1,
        cellKept: ref ? ref.isConnected && ref.parentElement === sum : null,
        focusKept: ref ? document.activeElement === ref : null };
    }

    // 事件委托 / 详情入口 / 键盘 / 空态:节点复用后这些交互必须原样可用
    stage("interaction");
    {
      const d = base();
      apply(d, false);
      const head = document.querySelector('#list .row[data-id="amtA"] .rhead');
      head.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
      const kbOpened = !document.getElementById("detailShade").hidden;
      closeDetail(false);
      const head2 = document.querySelector('#list .row[data-id="amtA"] .rhead');
      head2.click();
      const clickOpened = !document.getElementById("detailShade").hidden;
      closeDetail(false);
      const emptyD = base();
      emptyD.channels.forEach((c) => { c.hasKey = false; });
      apply(emptyD, false);
      const emptyText = (document.querySelector("#list .empty") || {}).textContent || "";
      const skD = base();
      skD.channels = [];
      apply(skD, false);
      const skeleton = !!document.querySelector("#list .sk-wrap");
      apply(d, false);
      out.extra.interaction = { keyboardOpensDetail: kbOpened, clickOpensDetail: clickOpened,
        emptyState: /配置第一个渠道/.test(emptyText), skeleton,
        restoredRows: document.querySelectorAll("#list .row").length };
    }

    // 详情弹层:焦点与滚动
    stage("detail");
    {
      const d = base();
      apply(d, false);
      const trigger = document.querySelector("#list .row .rhead");
      openDetail("amtA", trigger);
      const body = document.getElementById("detailBody");
      body.scrollTop = 80;
      const btn = body.querySelector('[data-act="detail-refresh"]');
      if (btn) btn.focus({ preventScroll: true });
      const before = { scrollTop: body.scrollTop, scrollHeight: body.scrollHeight,
        focus: btn ? document.activeElement === btn : null,
        focusAct: document.activeElement && document.activeElement.dataset ? document.activeElement.dataset.act : null };
      render({ updated: true });
      const after = { scrollTop: body.scrollTop, scrollHeight: body.scrollHeight,
        focus: btn ? document.activeElement === btn : null,
        focusAct: document.activeElement && document.activeElement.dataset ? document.activeElement.dataset.act : null,
        stillOpen: !document.getElementById("detailShade").hidden };
      out.extra.detail = { before, after };
      closeDetail(false);
    }

    // 布局矩阵:单列/双列 × 字号 × 主题,检查数值区是否被裁
    stage("matrix");
    const matrix = [];
    for (const layout of ["single", "double"]) {
      for (const scale of ["md", "xl"]) {
        for (const theme of ["dark", "light"]) {
          const d = base();
          d.cfg.fontScale = scale;
          d.cfg.theme = theme;
          d.channels.find((c) => c.id === "amtA").remaining = 1234567.89;
          d.channels.find((c) => c.id === "pts").remaining = 9876543;
          apply(d, false);
          CFG.fontScale = scale; applyFontScale();
          CFG.theme = theme; applyTheme();
          await sleep(40); // 让此前形态切换留下的异步 class 落地
          document.body.className = "form-panel" + (theme === "light" ? " light" : "");
          document.body.dataset.w = layout === "double" ? "lg" : "md";
          document.body.classList.toggle("layout-single", layout === "single");
          render({ updated: false });
          await frame();
          document.body.classList.remove("view-manage", "manage-index", "detail-open", "form-pill", "form-compact");
          const r = document.querySelector('#list .row[data-id="amtA"]');
          const v = r && r.querySelector(".val .v");
          const val = r && r.querySelector(".val");
          const nm = r && r.querySelector(".nm");
          const q = (e) => (e ? { sw: e.scrollWidth, cw: e.clientWidth, rect: rect(e) } : null);
          const cardRect = r ? r.getBoundingClientRect() : null;
          const vRect = v ? v.getBoundingClientRect() : null;
          matrix.push({ layout, scale, theme, bodyClass: document.body.className,
            vFont: v ? getComputedStyle(v).fontSize : null,
            vNumeric: v ? getComputedStyle(v).fontVariantNumeric : null,
            v: q(v), val: q(val), nm: q(nm),
            vText: v ? v.textContent : null,
            overflow: v ? round(v.scrollWidth - v.clientWidth) : null,
            valOverflow: val ? round(val.scrollWidth - val.clientWidth) : null,
            cardOverflow: cardRect && vRect ? round(vRect.right - cardRect.right) : null,
            cardH: r ? round(r.getBoundingClientRect().height) : null,
          });
        }
      }
    }
    out.extra.matrix = matrix;

    // 紧凑条 / 胶囊 / 减少动态效果
    stage("compact");
    {
      const d = base();
      apply(d, false);
      document.body.className = "form-compact";
      CFG.form = "compact";
      render({ updated: true });
      await frame();
      const beforeRefs = refsNow();
      d.channels.find((c) => c.id === "amtA").remaining = 777;
      apply(d, true);
      out.extra.compact = {
        rows: document.querySelectorAll("#list .row").length,
        nodesKept: identity(beforeRefs).every((i) => i.rowKept),
        ghosts: document.querySelectorAll(".roll-ghost").length,
        sweepsGo: document.querySelectorAll(".sweep.go").length,
      };
      document.body.className = "form-panel";
      CFG.form = "panel";
      apply(d, false);
    }

    out.meta.reducedAnimationDuration = (() => {
      const v = document.querySelector(".val .v");
      return v ? getComputedStyle(v).animationDuration : null;
    })();
    stage("done");
    writeResult({ scenarios: out.scenarios, extra: out.extra, meta: out.meta, errors: out.errors });
  }

  main().catch((e) => {
    writeResult({ fatal: String(e && e.stack || e), scenarios: out.scenarios, extra: out.extra, meta: out.meta });
  });
}

// ───────────────────────── 组装 + 运行 ─────────────────────────
function buildHarness() {
  const html = fs.readFileSync(path.join(SRC, "index.html"), "utf8");
  const base = path.resolve(SRC).replace(/\\/g, "/");
  const withBase = html.replace("<head>", `<head>\n<base href="file:///${base}/">`);
  const inject =
    `<script id="__QA_DATA" type="application/json">${JSON.stringify(DATA).replace(/</g, "\\u003c")}</script>\n` +
    `<script>window.__QA_DATA = JSON.parse(document.getElementById('__QA_DATA').textContent);</script>\n` +
    `<script>(${qaStubs.toString()})();</script>\n`;
  const driver = `<script>(${qaDriver.toString()})();</script>\n`;
  return withBase
    .replace('<script src="app.js"></script>', inject + '<script src="app.js"></script>\n' + driver);
}

const edge = argOf("--edge", EDGE_CANDIDATES.find((p) => fs.existsSync(p)) || EDGE_CANDIDATES[0]);
const harnessPath = path.join(os.tmpdir(), "gt-refresh-qa.html");
fs.writeFileSync(harnessPath, buildHarness(), "utf8");
const url = "file:///" + harnessPath.replace(/\\/g, "/");
const args = [
  "--headless=new", "--disable-gpu", "--no-sandbox", "--disable-extensions",
  "--no-first-run", "--disable-background-networking",
  "--user-data-dir=" + path.join(os.tmpdir(), "gt-refresh-qa-profile-" + (argv.includes("--reduced") ? "rm" : "std")),
  "--window-size=" + argOf("--window-size", "420,700"),
  "--force-device-scale-factor=1",
  "--virtual-time-budget=30000",
];
if (argv.includes("--reduced")) args.push("--force-prefers-reduced-motion=reduce");
args.push("--dump-dom", url);

let stdout;
try {
  stdout = execFileSync(edge, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, stdio: ["ignore", "pipe", "ignore"] });
} catch (e) {
  console.error("无头浏览器执行失败:", e.message);
  process.exit(1);
}
const m = /<script id="__QA_RESULT"[^>]*>([\s\S]*?)<\/script>/.exec(stdout);
if (!m) {
  console.error("没拿到结果节点。DOM 尾部:", stdout.slice(-1200));
  process.exit(1);
}
const result = JSON.parse(m[1].replace(/&quot;/g, '"').replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">"));
if (result.fatal) {
  console.error("浏览器内驱动异常:\n" + result.fatal);
  console.error("卡在阶段:", result.stage, "\nJS 错误:", JSON.stringify(result.jsErrors || []));
  console.error("已完成场景:", (result.scenarios || []).map((s) => s.name));
  process.exit(1);
}
const outPath = argOf("--out", path.join(ROOT, "build", "qa", "refresh-snapshot.json"));
fs.mkdirSync(path.dirname(outPath), { recursive: true });
fs.writeFileSync(outPath, JSON.stringify(result, null, 2), "utf8");
fs.unlinkSync(harnessPath);

console.log(`快照已写入 ${outPath}`);
console.log(`初始行数 ${result.meta.initialRows}, 视口 ${result.meta.viewport}, 减少动效=${result.meta.reduced}`);
console.log("场景".padEnd(28), "顺序变化", "最大位移", "横向抖动", "摘要高差", "滚动差", "焦点保留", "ghost", "sweep", "节点身份");
for (const s of result.scenarios) {
  const d = s.diff;
  console.log(
    s.name.padEnd(28),
    String(d.orderChanged).padEnd(9),
    String(d.maxRectDelta).padEnd(9),
    String(d.maxXDelta).padEnd(9),
    String(d.sumHeightDelta).padEnd(9),
    String(d.scrollDelta).padEnd(7),
    String(d.focusKept).padEnd(9),
    String(d.ghostPeak).padEnd(6),
    String(d.sweepPeak).padEnd(6),
    d.allNodesKept ? "全部保留" : "有重建"
  );
}
