/**
 * 前端烟测:在 jsdom 里真跑一遍 app.js 的渲染路径,验证四种形态**不抛异常**、
 * 且画出了设计要求里的结构(风险带 / 两级行 / 分组 / 骨架 / 失败横幅 / 签到 chip /
 * 胶囊限定语 / 吸附分段 / 非颜色模式 / 视图缺失时的兜底)。
 *
 * 为什么需要它:本机 GNU 工具链缺 windres,编不出 Tauri 外壳、起不了真机,
 * 界面改动就没有回归保护。它把真实的 index.html + app.js 装进 jsdom,塞合成数据跑 render()。
 *
 * 用法:  NODE_PATH=<装了 jsdom 的 node_modules> node build/jsdom-smoke.js
 * 说明:  它验证"代码能跑、结构在",不验证"像素好看";像素与真机几何仍需真机复测。
 */
const fs = require("fs");
const path = require("path");
// jsdom 是**开发期**依赖,不进产品依赖树。没装时给一条明确提示就退出:
//   cd <node 工作区> && npm install jsdom
let JSDOM = null;
try {
    ({ JSDOM } = require("jsdom"));
} catch {
    console.error("这个脚本需要 jsdom: npm install jsdom 后重跑");
    process.exit(2);
}

const SRC = "G:/game/nw/Grandettoken/app/src";
const html = fs.readFileSync(path.join(SRC, "index.html"), "utf8");
const js = fs.readFileSync(path.join(SRC, "app.js"), "utf8");

const dom = new JSDOM(html, { runScripts: "outside-only", pretendToBeVisual: true, url: "http://localhost/" });
const w = dom.window;

w.__TAURI__ = {
  core: { invoke: async () => null },
  event: { listen: async () => () => {} },
  window: {
    // 窗口对象方法很多(onMoved / 主题 / 拖拽…),用 Proxy 兜住未知的:
    // 监听类方法返回"可解绑的空函数",查询类返回真值。
    getCurrentWindow: () =>
      new Proxy(
        {
          outerPosition: async () => ({ x: 0, y: 0 }),
          outerSize: async () => ({ width: 400, height: 580 }),
          innerSize: async () => ({ width: 380, height: 560 }),
          isVisible: async () => true,
          scaleFactor: async () => 1.25,
          theme: async () => "dark",
        },
        { get: (t, k) => (k in t ? t[k] : async () => () => {}) }
      ),
  },
};
w.Notification = class { static requestPermission() {} };
w.matchMedia = w.matchMedia || (() => ({ matches: false, addEventListener() {} }));
// jsdom 没有 canvas 实现,托盘图标 / 曲线的绘制会拿到 null 上下文 —— 桩一个空 2D 上下文
const ctx2d = new Proxy(
  { canvas: { width: 64, height: 64 }, measureText: () => ({ width: 10 }),
    createLinearGradient: () => ({ addColorStop() {} }),
    createRadialGradient: () => ({ addColorStop() {} }),
    getImageData: () => ({ data: new Uint8ClampedArray(4) }) },
  { get: (t, k) => (k in t ? t[k] : () => {}) }
);
w.HTMLCanvasElement.prototype.getContext = () => ctx2d;
w.HTMLCanvasElement.prototype.toDataURL = () => "data:image/png;base64,";

// ── 合成数据:9 个渠道,覆盖百分比 / 金额 / 积分、限流、无分母、陈旧、未配置 ──
const NOW = Math.floor(Date.now() / 1000);
const DATA = buildData(NOW);

const HARNESS = `
;(function () {
  const R = (window.__R = []);
  const ck = (name, cond, extra) => R.push({ name, ok: !!cond, extra: cond ? "" : String(extra || "") });
  const L = () => document.getElementById("list").innerHTML;
  const S = () => document.getElementById("sum").innerHTML;
  const data = window.__DATA;

  try {
    CHANNELS = data.CHANNELS;
    CFG = data.CFG;
    CK = {};
    hvApply(data.HOME);
    render({ updated: false });
    ck("面板 render() 全流程不抛异常", true);
  } catch (e) {
    ck("面板 render() 全流程不抛异常", false, e.message + " @ " + (e.stack || "").split("\\n")[1]);
  }

  // ── 顶部待签提示条:说清"哪个渠道 + 下一步"(2026-10-23) ──
  // 现场:条上只写「去查看」,点进去是签到设置(只有开关),用户不知道要做什么。
  try {
    document.body.className = "form-panel";
    CHECKIN_ON.add("wb");
    CFG.checkin = { wb: { enabled: true } };
    CK = {
      wb: { id: "wb", name: "WorkBuddy", enabled: true, auto: true,
            doneToday: false, canTry: true, outcome: "retry:ready" },
    };
    render({});
    const strip = document.querySelector("#list .ckstrip");
    ck("单渠道待签:条上写渠道名,并直接给「点此领取」", !!strip &&
      strip.dataset.act === "ckrun" && strip.dataset.id === "wb" &&
      /WorkBuddy/.test(strip.textContent) && /点此领取/.test(strip.textContent),
      strip && strip.textContent);
    CK.wb = Object.assign({}, CK.wb, { canTry: false, outcome: "auth", nextTryAt: null });
    render({});
    const strip2 = document.querySelector("#list .ckstrip");
    ck("领不了的渠道:不画成可领取,写清原因并落到渠道卡", !!strip2 &&
      strip2.dataset.act === "ckstripopen" && strip2.dataset.id === "wb" &&
      /凭据失效/.test(strip2.textContent), strip2 && strip2.textContent);
    CK.wb = Object.assign({}, CK.wb, {
      canTry: true, outcome: "no_cred:没检测到 WorkBuddy 登录", nextTryAt: null,
    });
    render({});
    const strip3 = document.querySelector("#list .ckstrip");
    ck("no_cred 即使 canTry 也不给空按钮", !!strip3 &&
      strip3.dataset.act === "ckstripopen" && /本机没读到凭据/.test(strip3.textContent),
      strip3 && strip3.textContent);
  } catch (e) {
    ck("顶部待签提示条可读且可操作", false, e.message);
  } finally {
    CK = {};
    CFG.checkin = undefined;
    CHECKIN_ON.delete("wb");
  }

  // ── 风险带(v0.1.58 起:临期积分 / 最先到期 / 签到,不再放总余额)──
  ck("风险带三格都在(临期 / 到期 / 签到)",
    /临期/.test(S()) && /最早可确认|最先到期/.test(S()) && /签到/.test(S()), S().slice(0, 160));
  ck("临期积分 600 分", /600 分/.test(S()), S().slice(0, 200));
  ck("临期副行说明渠道数并可点筛选", /个渠道/.test(S()) && /点击筛选/.test(S()), S().slice(0, 200));
  ck("签到常驻 2/3(未签满不出现 ✓)", /2\\/3/.test(S()) && !/\\u2713/.test(S()), S().slice(0, 300));
  ck("最紧那格可点(跳行)", /data-act="jump"/.test(S()));

  // ── 两级行与分组 ──
  ck("分组:「需要处理」与「其余」", /需要处理/.test(L()) && /其余/.test(L()));
  // 紧急 = T0(需要处理)∪ 已跌破红线:claude(限流)、ds(低于底线)、wb(7 天内到期 60%)
  ck("两级行:三条紧急行带 urg 类", (L().match(/class="row urg"/g) || []).length === 3,
    (L().match(/class="row urg"/g) || []).length);
  ck("分组只按 T0 切:标题写 2,下面正好 2 行",
    new RegExp("需要处理</span><em>2").test(L()),
    (L().match(/<em>(\\d+)<\\/em>/g) || []).join(" "));
  ck("紧急行画出副行(绑定窗 + 恢复时间)", /5 小时窗/.test(L()));
  ck("状态标签 qtag(颜色之外的第二个通道)", /class="qtag"/.test(L()));
  ck("「更新于」只在陈旧行出现", (L().match(/class="rhint/g) || []).length === 1,
    (L().match(/class="rhint/g) || []).length);
  ck("第三段包进 seg3(折叠时不显示)", /class="seg3"/.test(L()));
  ck("排序照后端的 order(限流的排第一)",
    L().indexOf('data-id="claude"') < L().indexOf('data-id="kimi"'), L().indexOf('data-id="claude"') + " vs " + L().indexOf('data-id="kimi"'));

  // ── 冷启动骨架 ──
  try {
    CHANNELS = [];
    render({});
    ck("冷启动走骨架,不是「加载中…」", /sk-row/.test(L()) && !/加载中/.test(L()));
  } catch (e) { ck("冷启动走骨架,不是「加载中…」", false, e.message); }

  // ── 大面积失败横幅 ──
  try {
    CHANNELS = data.CHANNELS;
    HV.errorCount = 4;
    render({});
    ck("一轮 ≥3 个失败时出现横幅", /个渠道取数失败/.test(L()), L().slice(0, 160));
    HV.errorCount = 0;
  } catch (e) { ck("一轮 ≥3 个失败时出现横幅", false, e.message); }

  // ── 紧凑条 ──
  try {
    document.body.className = "form-compact";
    render({});
    fitCompact();
    const l = document.getElementById("list");
    const chip = l.querySelector(".ckchip");
    ck("紧凑条里有常驻的签到 chip", !!chip, l.innerHTML.slice(0, 200));
    ck("签到 chip 显示 2/3 且可点", !!chip && /2\\/3/.test(chip.textContent) && chip.dataset.act === "ckstripopen",
      chip && chip.textContent);
    ck("降级顺序:第一个 chip(最紧)保持完整",
      !l.querySelector(".row.lv1") && !l.querySelector(".row.lv2"),
      "lv1=" + !!l.querySelector(".row.lv1") + " lv2=" + !!l.querySelector(".row.lv2"));
  } catch (e) { ck("紧凑条渲染不抛异常", false, e.message); }

  // ── 胶囊 ──
  try {
    PILL.sorted = CHANNELS.slice();
    PILL.list = [];
    PILL.idx = 0;
    renderPill(CHANNELS.slice());
    renderPillFace();
    const v = document.getElementById("pillV").innerHTML;
    ck("胶囊自动挑最紧的(限流的那个)", PILL.list.length === 1 && PILL.list[0].id === "claude",
      PILL.list.map((c) => c.id).join(","));
    ck("胶囊主数字用后端绑定窗(限流 → 0.0%)", /0\\.0%/.test(v), v);
    ck("胶囊带限定语「限流」", /class="pq">限流/.test(v), v);
  } catch (e) { ck("胶囊渲染不抛异常", false, e.message); }

  // ── 吸附竖条 ──
  try {
    DOCK.on = true;
    renderDock();
    const d = document.getElementById("dockBody");
    ck("吸附条:一段一渠道(9 段)", d.querySelectorAll("i").length === 9, d.querySelectorAll("i").length);
    ck("陈旧的那一段带斜纹类", !!d.querySelector("i.stale"));
  } catch (e) { ck("吸附条渲染不抛异常", false, e.message); }

  // ── 非颜色模式 ──
  try {
    document.body.className = "form-panel";
    CFG.textTone = true;
    render({});
    ck("非颜色模式:每行都写文字态", document.getElementById("list").querySelectorAll(".qtag").length >= 7,
      document.getElementById("list").querySelectorAll(".qtag").length);
    CFG.textTone = false;
  } catch (e) { ck("非颜色模式渲染不抛异常", false, e.message); }

  // ── 拿不到统一视图时的兜底 ──
  try {
    hvApply(null);
    HV = { byId: {}, order: [], risk: null, checkin: null, actionCount: 0, staleCount: 0, errorCount: 0, totals: null };
    render({});
    ck("视图缺失时退回本地判断(不空白、不抛错)",
      document.getElementById("list").querySelectorAll(".row").length > 0);
  } catch (e) { ck("视图缺失时退回本地判断(不空白、不抛错)", false, e.message); }
})();
`;

w.__DATA = DATA;
try {
  w.eval(js + "\n;\n" + HARNESS);
} catch (e) {
  console.log("✗ 脚本整体执行抛异常:" + e.message);
  console.log((e.stack || "").split("\n").slice(0, 4).join("\n"));
  process.exit(1);
}

const R = w.__R || [];
let ok = 0, bad = 0;
for (const r of R) {
  if (r.ok) { ok++; console.log("  ok   " + r.name); }
  else { bad++; console.log("  FAIL " + r.name + (r.extra ? " :: " + r.extra : "")); }
}
console.log("\n" + (bad ? `✗ ${bad} 项失败` : "✓ 全部通过") + ` (${ok}/${R.length})`);
process.exit(bad ? 1 : 0);

// ── 合成数据 ──
function buildData(NOW) {
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
  const pct = (id, remain, limited) =>
    ch({
      id, name: id.toUpperCase() + " 配额", kind: "percent", remaining: remain, unit: "percent",
      windows: [
        { label: "周期", percent: 100 - 62, remainPercent: 62, status: "ok", resetsAt: null, main: true },
        { label: "5 小时", percent: 100 - remain, remainPercent: remain,
          status: limited ? "rate-limited" : "ok", resetsAt: null, main: false },
      ],
    });

  const CHANNELS = [
    pct("claude", 0, true),                                   // 限流 → T0
    ch({ id: "ds", name: "DeepSeek", kind: "amount", remaining: 0.03, unit: "CNY", day: 3, dayPrev: 2 }),
    ch({ id: "4sapi", name: "4SAPI", kind: "amount", remaining: 120, unit: "CNY", day: 5, dayPrev: 4,
         claim: { amount: 500, lastClaimAt: null, source: "auto", daysUntilEligible: 12, eligible: false,
                  dailyBurn: 1, daysOfBalance: 120, shortageRisk: false } }),
    pct("oc", 22, false),
    ch({ id: "wb", name: "WorkBuddy", kind: "points", remaining: 1000, unit: "credits",
         expiring: [{ at: NOW + 3 * 86400, amount: 600, label: "签到" }] }),
    pct("codex", 42, false),
    pct("kimi", 88, false),
    ch({ id: "old", name: "陈旧渠道", kind: "amount", remaining: 50, updatedAt: NOW - 3600,
         claim: { amount: 100, lastClaimAt: null, source: "auto", daysUntilEligible: 0, eligible: true,
                  dailyBurn: 1, daysOfBalance: 50, shortageRisk: false } }),
    ch({ id: "none", name: "未配置", kind: "amount", hasKey: false, valid: false }),
  ];

  const HOME = {
    rows: [
      { id: "claude", state: "limited", tone: "bad", tier: 0, urgency: 0, bindingWindow: "5 小时",
        headline: { percent: 0 }, qualifier: "限流", secondary: "5 小时窗 · 2.0 小时 后恢复 · 周期 62%",
        etaSecs: 7200, ageSecs: 10, checkin: { enabled: false, done: false } },
      { id: "ds", state: "fresh", tone: "bad", tier: 0, urgency: 2, bindingWindow: null,
        headline: { amount: 0.03, unit: "CNY" }, qualifier: "低于底线", secondary: null,
        etaSecs: null, ageSecs: 10, checkin: { enabled: true, done: false } },
      { id: "4sapi", state: "fresh", tone: "warn", tier: 1, urgency: 0.24, bindingWindow: null,
        headline: { amount: 120, unit: "CNY" }, qualifier: null, secondary: "按当前速度约可用 120 天",
        etaSecs: 120 * 86400, ageSecs: 12, checkin: { enabled: false, done: false } },
      { id: "oc", state: "fresh", tone: "ok", tier: 1, urgency: 0.22, bindingWindow: "5 小时",
        headline: { percent: 22 }, qualifier: null, secondary: null, etaSecs: null, ageSecs: 15,
        checkin: { enabled: true, done: true } },
      { id: "wb", state: "fresh", tone: "bad", tier: 3, urgency: 0.4, bindingWindow: null,
        headline: { points: 1000 }, qualifier: null, secondary: null, etaSecs: null, ageSecs: 20,
        checkin: { enabled: true, done: true } },
      { id: "codex", state: "fresh", tone: "ok", tier: 1, urgency: 0.42, bindingWindow: "周期",
        headline: { percent: 42 }, qualifier: null, secondary: null, etaSecs: null, ageSecs: 11,
        checkin: { enabled: false, done: false } },
      { id: "kimi", state: "fresh", tone: "ok", tier: 1, urgency: 0.88, bindingWindow: "周期",
        headline: { percent: 88 }, qualifier: null, secondary: null, etaSecs: null, ageSecs: 9,
        checkin: { enabled: false, done: false } },
      { id: "old", state: "stale", tone: "ok", tier: 1, urgency: 0.5, bindingWindow: null,
        headline: { amount: 50, unit: "CNY" }, qualifier: "~", secondary: "1 小时 前的快照",
        etaSecs: null, ageSecs: 3600, checkin: { enabled: false, done: false } },
      { id: "none", state: "noKey", tone: "off", tier: 4, urgency: null, bindingWindow: null,
        headline: {}, qualifier: "未配置", secondary: null, etaSecs: null, ageSecs: 0,
        checkin: { enabled: false, done: false } },
    ],
    order: ["claude", "ds", "4sapi", "oc", "wb", "codex", "kimi", "old", "none"],
    risk: "claude",
    actionCount: 2,
    staleCount: 1,
    errorCount: 0,
    checkin: { total: 3, done: 2, pending: ["ds"] },
    totals: { cny: 170.03, others: [], today: 8, todayPrev: 6, hidden: 0 },
    now: NOW,
  };

  const CFG = {
    sort: "urgency", warnPercent: 40, critPercent: 15, moneyFloor: 10, textTone: false,
    fontScale: "md", form: "panel", hiddenChannels: [], channelOrder: [], pillChannels: [],
    alertPulse: false, peekOnClick: true, alwaysOnTop: false, dockEnabled: false,
    activeIntervalSec: 60, idleIntervalSec: 300, backoffIntervalSec: 900, trayShowNumber: true,
    autostart: false, autoCheckUpdate: true, notifyOnCrit: true, soundOn: false,
    modelShowAll: false, modelGroup: null, modelBaseUrl: null,
    claimChannels: { "4sapi": { enabled: true, amount: 500, minIntervalDays: 14 } },
  };

  return { CHANNELS, HOME, CFG };
}
