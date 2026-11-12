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

// 必须从脚本位置推导仓库根:以前写死 "G:/game/nw/Grandettoken/app/src",
// 换一台机器 / 换个 clone 目录就静默读不到文件,而门禁脚本可能仍然报绿。
const SRC = path.resolve(__dirname, "..", "app", "src");
const html = fs.readFileSync(path.join(SRC, "index.html"), "utf8");
const js = fs.readFileSync(path.join(SRC, "app.js"), "utf8");

const dom = new JSDOM(html, { runScripts: "outside-only", pretendToBeVisual: true, url: "http://localhost/" });
const w = dom.window;

const CALLS = Object.create(null);
w.__CALLS = CALLS;
w.__TAURI__ = {
  // 真实启动链路会读配置/渠道/视图;全返回 null 的话 init 走到 applyPinUI 会崩
  // (以前同步断言在 init 恢复之前就退出了,没有暴露)。异步用例要等 init,所以喂真数据。
  core: {
    invoke: async (cmd, args) => {
      CALLS[cmd] = (CALLS[cmd] || 0) + 1;
      if (w.__VAULT_COPY_INVOKE && ["vault_copy", "copy_text"].includes(cmd)) {
        return w.__VAULT_COPY_INVOKE(cmd, args);
      }
      if (w.__DOWNLOAD_INVOKE && ["download_link", "copy_text"].includes(cmd)) {
        return w.__DOWNLOAD_INVOKE(cmd, args);
      }
      if (cmd === "get_config") return JSON.parse(JSON.stringify(DATA.CFG));
      if (cmd === "get_channels") return JSON.parse(JSON.stringify(DATA.CHANNELS));
      if (cmd === "home_view") return JSON.parse(JSON.stringify(DATA.HOME));
      if (cmd === "checkin_state") return w.__CK_ST || [];
      if (cmd === "checkin_history") return w.__CK_H || null;
      if (cmd === "app_version") return "0.0.0-test";
      if (cmd === "sync_qr_svg") return '<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h1v1H0z"/></svg>';
      if (cmd === "vault_list") return w.__VAULT || [];
      if (cmd === "vault_ref_map") return w.__VAULTREF || [];
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
    ck("单渠道待签:条上写渠道名,并直接给「领取」", !!strip &&
      strip.dataset.act === "ckrun" && strip.dataset.id === "wb" &&
      /WorkBuddy/.test(strip.textContent) && /领取/.test(strip.textContent),
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
  ck("临期副行说明渠道数并可筛选", /个/.test(S()) && /(筛选|未知)/.test(S()), S().slice(0, 200));
  ck("签到常驻 2/3(未签满不出现 ✓)", /2\\/3/.test(S()) && !/\\u2713/.test(S()), S().slice(0, 300));
  ck("最紧那格可点(跳行)", /data-act="jump"/.test(S()));
  ck("摘要英雄数字与单位分开", !!document.querySelector("#sum .hero-v") && !!document.querySelector("#sum .hero-unit"), S().slice(0, 300));
  ck("摘要画出 7 日临期分布", !!document.querySelector("#sum .expbars"), S().slice(0, 400));
  ck("最先到期分两行:渠道名 + 日期", !!document.querySelector("#sum .due-name") && !!document.querySelector("#sum .due-date"), S().slice(0, 400));
  ck("底栏状态各自带状态点", document.querySelectorAll("#fstat .fb-dot").length >= 2,
    document.getElementById("fstat").innerHTML);

  // ── 颜色纪律:健康行大数字不再染色 ──
  const ocRow = document.querySelector('.row[data-id="oc"]');
  const fourRow = document.querySelector('.row[data-id="4sapi"]');
  const dsRow = document.querySelector('.row[data-id="ds"]');
  ck("健康行大数字回到主文字色",
    !!ocRow && ocRow.querySelector(".val .v").getAttribute("style").includes("var(--tx)"),
    ocRow && ocRow.querySelector(".val .v").getAttribute("style"));
  ck("预警行仍保留 warn 色",
    !!fourRow && fourRow.querySelector(".val .v").getAttribute("style").includes("var(--warn)"),
    fourRow && fourRow.querySelector(".val .v").getAttribute("style"));
  ck("兜底字母两位大写,绝不落成小写 x",
    window.shortLabel({ short: "x", name: "Claude Code" }) === "CL" &&
    window.shortLabel({ short: "x", name: "x" }) === "··",
    window.shortLabel({ short: "x", name: "Claude Code" }));

  // ── 平铺列表(2026-11-03 用户裁决:取消「需要处理」分组)──
  ck("列表平铺:不再有「需要处理 / 其余」分组标题",
    !/需要处理/.test(L()) && !/class="grp"/.test(L()), L().slice(0, 200));
  ck("平铺不丢行:行数 = 可见渠道数",
    document.querySelectorAll("#list .row").length ===
      data.CHANNELS.filter((c) => c.hasKey && !c.hidden).length,
    document.querySelectorAll("#list .row").length);
  // urg 类保留:面板里已无视觉差异,紧凑条靠它给告急 chip 染红/黄底
  // 紧急 = T0 ∪ tone bad:claude(限流)、ds(低于底线)、wb(7 天内到期 60%)
  ck("urg 类保留(紧凑条染色依赖)", (L().match(/class="row urg"/g) || []).length === 3,
    (L().match(/class="row urg"/g) || []).length);
  ck("紧急行画出副行(绑定窗 + 恢复时间)", /5 小时窗/.test(L()));
  ck("状态标签 qtag(颜色之外的第二个通道)", /class="qtag"/.test(L()));
  ck("陈旧行的年龄只写在右侧(.rhint 已退役)", !/class="rhint/.test(L()) && /小时前快照/.test(L()),
    L().slice(0, 300));
  ck("折叠行不再画第三段(核心段挪进展开区)", !/class="seg3"/.test(L()) && !/class="wins"/.test(L()));
  ck("重置时间未知时不写悬空的「窗 · —」", !/窗 · —/.test(L()), L().slice(0, 200));

  // ── 展开区五段骨架(2026-10-23 设计)──
  try {
    const four = CHANNELS.find((c) => c.id === "4sapi");
    const nowS = Math.floor(Date.now() / 1000);
    changesCache["4sapi"] = Array.from({ length: 12 }, (_, i) => ({
      startTs: nowS - 1200 - i * 5400, endTs: nowS - 600 - i * 5400, delta: -1.2,
      remaining: 100, samples: 2, gapSecs: 0,
    }));
    changesState["4sapi"] = "done";
    const d = renderDetail(four);
    ck("详情五段:核心 / 明细 / 最近变动 / 页脚",
      /core-use/.test(d) && /class="kv"/.test(d) && /最近变动/.test(d) && /foot-det/.test(d), d.slice(0, 300));
    ck("详情里有近 7 天消耗柱图", /sparkbars/.test(d) && /近 7 天消耗/.test(d), d.slice(0, 300));
    ck("台账默认 3 条 +「全部 12 条」",
      (d.match(/class="lrow"/g) || []).length === 3 && /全部 12 条/.test(d),
      (d.match(/class="lrow"/g) || []).length);
    const ltimes = d.match(/\\d{2}:\\d{2}/g) || [];
    ck("台账时间是本地时钟(不是同一分钟)", new Set(ltimes).size >= 3, ltimes.join(","));
    changesState["4sapi"] = "done"; // 缓存留着,不影响别的断言
  } catch (e) { ck("展开区五段骨架", false, e.message); }

  // ── 失败行去重(§5):标签说一次,副标题只写原因,右侧只写快照年龄 ──
  try {
    const home = JSON.parse(JSON.stringify(data.HOME));
    const r = home.rows.find((x) => x.id === "ds");
    Object.assign(r, { state: "error", tone: "off", qualifier: "取数失败", headline: {} });
    hvApply(home);
    const ds = CHANNELS.find((c) => c.id === "ds");
    ds.valid = false;
    ds.error = "HTTP 401";
    render({});
    const html = document.querySelector('.row[data-id="ds"]').outerHTML;
    ck("失败行「取数失败」只出现一次", (html.match(/取数失败/g) || []).length === 1, html);
    ck("失败行只说一次原因,右侧写快照年龄", (html.match(/HTTP 401/g) || []).length === 1 && /快照/.test(html), html);
    ck("失败行不再画黄字 rhint", !/rhint/.test(html), html);
    ck("失败行大数字降为 text-3",
      document.querySelector('.row[data-id="ds"] .val .v').getAttribute("style").includes("var(--tx3)"),
      document.querySelector('.row[data-id="ds"] .val .v').getAttribute("style"));
    ds.valid = true;
    ds.error = null;
    hvApply(data.HOME);
    render({});
  } catch (e) { ck("失败行去重", false, e.message); }

  // ── Codex 专属副标题:OpenCode Go 的「套餐」不能写成 ChatGPT 订阅(§6.2)──
  try {
    const cx = CHANNELS.find((c) => c.id === "codex");
    const oc = CHANNELS.find((c) => c.id === "oc");
    cx.extra = [["套餐", "plus"]];
    oc.extra = [["套餐", "$10/期 · 未折进消耗(汇率还没取到)"]];
    render({});
    const l = L();
    const cxRow = document.querySelector('.row[data-id="codex"]').outerHTML;
    const ocRow = document.querySelector('.row[data-id="oc"]').outerHTML;
    ck("Codex 副标题写 ChatGPT 订阅", /ChatGPT 订阅/.test(cxRow), cxRow.slice(0, 200));
    ck("OpenCode Go 不写 ChatGPT 订阅", !/ChatGPT 订阅/.test(ocRow), ocRow.slice(0, 200));
    ck("视图里没有多余的旧样式类", !/class="(seg|spark|extab|ckdet)"/.test(l));
    // 套餐不能一律去重:OpenCode Go「未折进消耗」只写在明细里(2026-10-23 边界复核)
    ck("OpenCode Go 明细保留「套餐 · 未折进消耗」", /未折进消耗/.test(renderDetail(oc)));
    ck("Codex 明细不重复副标题里的套餐", !/<span>套餐<\\/span>/.test(renderDetail(cx)));
    cx.extra = [];
    oc.extra = [];
    render({});
  } catch (e) { ck("Codex 专属副标题", false, e.message); }

  // ── Codex 收起双额度:周主、5 小时次,风险独立 ──
  try {
    const cx = CHANNELS.find((c) => c.id === "codex");
    const saved = { ...cx };
    const used = CFG.showUsed;
    const home = JSON.parse(JSON.stringify(data.HOME));
    const vr = home.rows.find((r) => r.id === "codex");
    const readRow = () => {
      const el = document.createElement("div");
      el.innerHTML = renderRow(cx);
      return el;
    };
    CFG.showUsed = false;
    cx.remaining = 98;
    cx.extra = [["套餐", "plus"]];
    cx.windows = [
      { label: "5 小时", percent: 15, remainPercent: 85, status: "ok", main: true },
      { label: "本周", percent: 2, remainPercent: 98, status: "ok", main: false }
    ]; // 兼容旧快照仍把 primary 标成 main
    let el = readRow();
    ck("Codex 收起大数字固定显示周额度", el.querySelector(".val .v").textContent === "98.0%");
    ck("Codex 收起同时显示5小时额度且无需展开", /5 小时剩余85.0%/.test(el.querySelector(".val .p").textContent) && !el.querySelector(".rbody").textContent);
    ck("Codex 左侧保留套餐与重置卡", /plus/.test(el.querySelector(".nm .s").textContent) && /ChatGPT 订阅/.test(el.innerHTML));
    CFG.showUsed = true;
    el = readRow();
    ck("Codex 两项同步切换已用口径", el.querySelector(".val .v").textContent === "2.0%" && /5 小时已用15.0%/.test(el.querySelector(".val .p").textContent));
    CFG.showUsed = false;
    cx.windows[0].remainPercent = 0;
    cx.windows[0].status = "rate-limited";
    Object.assign(vr, { state: "limited", tone: "bad", qualifier: "限流" });
    hvApply(home);
    el = readRow();
    ck("Codex 短期耗尽不替换周主数字", /98.0%/.test(el.querySelector(".val .v").textContent) && /0.0%/.test(el.querySelector(".val .p").textContent));
    ck("Codex 周主数字保持中性、短期告警着色并保留限流提示", el.querySelector(".val .v").getAttribute("style").includes("var(--tx)") && el.querySelector(".quota-secondary b").getAttribute("style").includes("var(--bad)") && /限流/.test(el.querySelector(".nm .n").textContent));
    cx.windows[0].remainPercent = 85;
    cx.windows[0].status = "ok";
    cx.stale = true;
    cx.updatedAt = Math.floor(Date.now() / 1000) - 600;
    Object.assign(vr, { state: "stale", tone: "ok", qualifier: "~" });
    hvApply(home);
    el = readRow();
    ck("Codex 旧快照仍显示双额度并在左侧标明年龄", /快照/.test(el.querySelector(".nm .s").textContent) && /85.0%/.test(el.querySelector(".val .p").textContent));
    cx.valid = false;
    cx.error = "登录状态已失效";
    Object.assign(vr, { state: "auth", tone: "off", qualifier: "需登录" });
    hvApply(home);
    el = readRow();
    ck("Codex 取数失败保留旧双额度且明确失败", /快照/.test(el.querySelector(".nm .s").textContent) && /登录状态已失效/.test(el.querySelector(".nm .s").textContent) && el.querySelector(".quota-secondary b").getAttribute("style").includes("var(--tx3)"));
    cx.valid = true;
    cx.stale = false;
    cx.windows = [cx.windows[1]];
    hvApply(data.HOME);
    el = readRow();
    ck("Codex 仅周窗不编造5小时额度", el.querySelector(".val .v").textContent === "98.0%" && !el.querySelector(".quota-secondary"));
    Object.assign(cx, saved);
    CFG.showUsed = used;
    hvApply(data.HOME);
    render({});
  } catch (e) { ck("Codex 双额度与周优先", false, e.message); }

  // ── Codex 套餐升级 / 重置卡 / 精确重置时间 ──
  try {
    const cx = CHANNELS.find((c) => c.id === "codex");
    const saved = { windows: cx.windows, resetCredits: cx.resetCredits, extra: cx.extra, stale: cx.stale };
    const ts = Math.floor(Date.now() / 1000) + 3600;
    cx.windows = [{ label: "5 小时", percent: 2, remainPercent: 98, status: "ok", main: true,
      resetsAt: new Date(ts * 1000).toISOString() }];
    cx.extra = [["套餐", "plus"]];
    cx.resetCredits = { availableCount: 0, credits: [], updatedAt: ts - 3600 };
    let detail = renderDetail(cx);
    ck("Codex 零张重置卡是确定的数量", /0 张可用/.test(detail) && !/未获取/.test(coreCodexResets(cx)));
    ck("Codex 每个窗口显示完整本地重置时间和时区", detail.includes(exactLocalTime(ts)) && detail.includes(localZoneLabel()) && /下次重置/.test(detail));
    cx.resetCredits = { availableCount: null, credits: null, error: "重置卡查询 HTTP 403", updatedAt: 0 };
    detail = coreCodexResets(cx);
    ck("Codex 重置卡失败不能渲染成零张", /未获取/.test(detail) && /HTTP 403/.test(detail) && !/0 张/.test(detail));
    ck("Codex 重置卡失败不影响配额窗口", /98.0%/.test(coreWindows(cx)));
    cx.resetCredits = { availableCount: 4, credits: null, updatedAt: ts - 3600 };
    ck("Codex 仅有数量时不编造卡片到期日", /4 张可用/.test(coreCodexResets(cx)) && /明细未提供/.test(coreCodexResets(cx)));
    cx.resetCredits.credits = [
      { title: "<img src=x onerror=alert(1)>", expiresAt: ts + 86400 },
      { title: "额度重置卡", expiresAt: ts + 172800 },
      { title: "额度重置卡", expiresAt: ts + 259200 },
      { title: "额度重置卡", expiresAt: null }
    ];
    cx.resetCredits.lastRedeemedAt = ts - 7200;
    detail = coreCodexResets(cx);
    ck("Codex 卡片标题转义且精确显示到期日", !/<img/.test(detail) && /&lt;img/.test(detail) && detail.includes(exactLocalTime(ts + 86400)));
    ck("Codex 多张重置卡可以展开且不猜测无日期卡永久有效", /全部 4 张/.test(detail) && !/永久/.test(detail));
    rowMore.codex = { resets: true };
    ck("Codex 展开后包含所有卡片及未知到期日", /接口未提供到期日/.test(coreCodexResets(cx)) && /最近用卡重置/.test(coreCodexResets(cx)));
    delete rowMore.codex;
    ck("Codex 过时重置时间提示等待刷新", /待刷新/.test(quotaResetCountdown(Date.now() - 1)));
    cx.resetCredits.credits[0].title = "额度重置卡";
    cx.stale = true;
    ck("Codex 陈旧重置卡保留查询时间并标明快照", /查询于/.test(coreCodexResets(cx)) && /上次快照/.test(coreCodexResets(cx)));
    Object.assign(cx, saved);
    render({});
  } catch (e) { ck("Codex 重置卡与时间", false, e.message); }

  // ── 2026-10-23 边界复核修掉的几条 ──
  try {
    const nowS = Math.floor(Date.now() / 1000);
    const wb = CHANNELS.find((c) => c.id === "wb");
    const bak = wb.expiring;
    wb.expiring = [
      { at: nowS - 5 * 86400, amount: 300, label: "过期包A" },
      { at: nowS - 2 * 86400, amount: 200, label: "过期包B" },
      { at: nowS + 3 * 86400, amount: 600, label: "签到" },
      { at: nowS + 9 * 86400, amount: 1500, label: "活动" },
    ];
    const d = renderDetail(wb);
    const firstRow = (d.match(/class="core-exp[^"]*"[\\s\\S]*?<\\/div>/) || [""])[0];
    ck("积分核心段:已过期的包不占「最近 3 笔」", !/已过期/.test(firstRow) && /全部 4 笔/.test(d), firstRow);
    wb.expiring = bak;

    const four = CHANNELS.find((c) => c.id === "4sapi");
    changesCache["4sapi"] = [{ startTs: nowS - 30 * 3600, endTs: nowS - 30 * 3600 + 1200, delta: -1,
      remaining: 1, samples: 2, gapSecs: 0 }];
    changesState["4sapi"] = "done";
    const lt = (renderDetail(four).match(/class="lt">([^<]*)</) || [])[1] || "";
    ck("更早的台账只写结束时刻(不带两头日期,84px 放得下)", /^\\d{1,2}-\\d{2} \\d{2}:\\d{2}$/.test(lt), lt);

    const ds = CHANNELS.find((c) => c.id === "ds");
    ds.valid = false;
    ck("失败卡用 rb-dim,不撞全局 .dim", /rbody-in rb-dim/.test(renderDetail(ds)) && !/rbody-in dim/.test(renderDetail(ds)));
    ds.valid = true;

    // 紧凑条里点 chip:不把行记成展开,也不取台账
    document.body.className = "form-compact";
    rowOpen.clear();
    render({});
    const chip = document.querySelector('#list .row[data-id="kimi"] .rhead');
    if (chip) chip.click();
    ck("紧凑条点 chip 不偷偷展开行", rowOpen.size === 0, rowOpen.size);
    document.body.className = "form-panel";
    render({});
  } catch (e) { ck("边界复核修掉的几条", false, e.message); }
  // 排序只认设置里的「渠道排序」:fixture 是 urgency 档(本地按绑定窗剩余比例升序),
  // 4sapi(24%)排在 claude(周期 62%)前面 —— 后端 T0 order 里 claude 恒第一,正好验证切换生效。
  ck("排序按设置的「渠道排序」,不再照后端 T0 order",
    L().indexOf('data-id="4sapi"') < L().indexOf('data-id="claude"') &&
      L().indexOf('data-id="claude"') < L().indexOf('data-id="kimi"'),
    "4sapi=" + L().indexOf('data-id="4sapi"') + " claude=" + L().indexOf('data-id="claude"'))

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
    // 自动轮播独立口径:绑定窗比例 → 无分母金额按「余额/底线」;夹具里 ds(¥0.03 < 底线 ¥10)最紧
    ck("胶囊自动挑最紧的(低于底线的那个)", PILL.list.length === 1 && PILL.list[0].id === "ds",
      PILL.list.map((c) => c.id).join(","));
    ck("胶囊主数字显示最紧渠道余额", v.includes("0.03"), v);
    ck("胶囊带限定语「低于底线」", v.includes("低于底线"), v);
  } catch (e) { ck("胶囊渲染不抛异常", false, e.message); }

  // ── 轮询推送:胶囊必须跟着**这一轮**的数字走(2026-10-26 真机)──
  // 现场:4S 余额一直在动、面板数字在变,胶囊一直停在启动时的那个数。
  // 根因:推送只带 views,而胶囊主数字读的是 home_view 的结论 —— 那一份只在
  // 启动时取过一次。推送现在带 { views, home },两边必须一起更新。
  try {
    CFG.pillChannels = ["4sapi"]; // 固定显示 4S,不靠"最紧"碰运气
    PILL.idx = 0;
    renderPill(CHANNELS.slice());
    const before = document.getElementById("pillV").textContent;
    const views = JSON.parse(JSON.stringify(data.CHANNELS));
    views.find((c) => c.id === "4sapi").remaining = 118; // 这一轮花掉 2 块
    const home = JSON.parse(JSON.stringify(data.HOME));
    home.rows.find((r) => r.id === "4sapi").headline = { amount: 118, unit: "CNY" };
    applyChannelsPush({ views, home });
    const after = document.getElementById("pillV").textContent;
    ck("推送后胶囊跟着这一轮的数字变(¥120 → ¥118)",
      /120/.test(before) && /118/.test(after), "before=" + before + " after=" + after);
  } catch (e) {
    ck("推送后胶囊跟着这一轮的数字变", false, e.message);
  } finally {
    CFG.pillChannels = [];
    CHANNELS = data.CHANNELS;
    hvApply(data.HOME);
    render({});
  }

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

  // ── 短暂断联:四种形态继续显示历史额度,恢复后解除标记 ──
  try {
    const offline = Object.assign({}, data.CHANNELS.find((c) => c.id === "ds"), {
      valid: false, stale: true, remaining: 321.09,
      recovery: { phase: "retrying", attempts: 1, nextRetryAt: Math.floor(Date.now() / 1000) + 2 }
    });
    CHANNELS = [offline];
    hvApply({ rows: [{ id: offline.id, state: "stale", tone: "off", tier: 2,
      headline: { amount: 321.09, unit: "CNY" }, qualifier: "更新中", secondary: "更新中 · 5 分钟前的快照" }] });
    document.body.className = "form-panel";
    let row = renderRow(offline);
    ck("断联重试:面板保留历史额度并说明更新中", /321/.test(row) && /更新中/.test(row) && !/需登录/.test(row));
    document.body.className = "form-compact";
    ck("断联重试:紧凑条保留历史额度", /321/.test(renderRow(offline)));
    CFG.pillChannels = [];
    renderPill(CHANNELS);
    ck("断联重试:自动胶囊继续选择历史额度", /321/.test(document.getElementById("pillV").textContent));
    ck("断联重试:托盘保留历史额度", trayNumber(offline) === "321");
    renderDock();
    ck("断联重试:吸附条标记旧数据", !!document.querySelector("#dockBody i.stale"));
    offline.recovery.phase = "unavailable";
    hvApply({ rows: [{ id: offline.id, state: "stale", tone: "off", headline: { amount: 321.09, unit: "CNY" }, qualifier: "~", secondary: "暂时无法更新 · 5 分钟前的快照" }] });
    ck("重试耗尽:仍保留历史额度并提示", /321/.test(renderRow(offline)) && /暂时无法更新/.test(renderRow(offline)));
    offline.valid = true; offline.stale = false; offline.recovery = null;
    hvApply({ rows: [{ id: offline.id, state: "fresh", tone: "ok", headline: { amount: 321.09, unit: "CNY" } }] });
    ck("恢复成功:自动移除重试提示", !/更新中|暂时无法更新/.test(renderRow(offline)));
  } catch (e) { ck("断联历史额度回归", false, e.message); }
  finally {
    document.body.className = "form-panel";
    CFG.pillChannels = [];
    CHANNELS = data.CHANNELS;
    hvApply(data.HOME);
    render({});
  }

  // ── 拿不到统一视图时的兜底 ──
  try {
    hvApply(null);
    HV = { byId: {}, order: [], risk: null, checkin: null, actionCount: 0, staleCount: 0, errorCount: 0, totals: null };
    render({});
    ck("视图缺失时退回本地判断(不空白、不抛错)",
      document.getElementById("list").querySelectorAll(".row").length > 0);
  } catch (e) { ck("视图缺失时退回本地判断(不空白、不抛错)", false, e.message); }

  // ── 签到中心页签(2026-10-29)──
  // 入口从「刷新与启动」的设置区改成独立页签(docs/product/product-checkin-center-2026-10-29.md)。
  // 这里验证:页签可见性、五段结构、日历口径(领到/没签/今天待签)、明细表与渠道筛选。
  try {
    document.body.className = "form-panel";
    CHANNELS = data.CHANNELS;
    CHECKIN_ON.add("wb");
    CFG.checkin = { wb: { enabled: true, auto: true } };
    CK = {};
    ckTabUpdate();
    ck("CK 未读到:签到页签先藏着(不先给假状态)", document.querySelector('#mTabs [data-p="ck"]').hidden === true);
    CK = {
      wb: { id: "wb", name: "WorkBuddy", enabled: true, auto: true, doneToday: true,
            canTry: false, outcome: "claimed:wb", credits: 100, monthCredits: 3100 },
    };
    ckTabUpdate();
    ck("有渠道开着监测:签到页签出现", document.querySelector('#mTabs [data-p="ck"]').hidden === false);
    const mkDay = (i, status, credits) => ({
      day: "2026-10-" + String(i).padStart(2, "0"), ts: 1790000000 + i * 3600,
      status, credits: credits === undefined ? null : credits, attempts: 1,
    });
    CKH = {
      days: 90,
      channels: [{ id: "wb", name: "WorkBuddy", enabled: true, auto: true, enabledSince: 1790000000,
        monthCredits: 3100, today: "2026-10-30",
        days: Array.from({ length: 30 }, (_, i) => mkDay(i + 1, i === 3 ? "missed" : i === 29 ? "pending" : "claimed", 100)) }],
      rows: [
        { id: "wb", name: "WorkBuddy", ts: 1794300000, day: "2026-10-30", outcome: "claimed:wb", credits: 100, attempts: 1, detail: "" },
        { id: "wb", name: "WorkBuddy", ts: 1793400000, day: "2026-10-28", outcome: "retry:device", credits: null, attempts: 3, detail: "太挤了" },
      ],
    };
    showTab("ck");
    renderCheckinCenter();
    const ckc = () => document.getElementById("ckCenter").innerHTML;
    ck("签到中心五段:今日/活动状态/近 30 天/明细/管理",
      /今日/.test(ckc()) && /活动状态/.test(ckc()) && /近 30 天/.test(ckc()) &&
      /明细\\(近 90 天\\)/.test(ckc()) && /管理/.test(ckc()), ckc().slice(0, 160));
    ck("今日总览:✓ 1/1 + 本月已领 3,100 分", /✓ 1\\/1/.test(ckc()) && /3,100 分/.test(ckc()), ckc().slice(0, 220));
    ck("今日行:签完的渠道给完整文案(已领 +100 分)", /已领到 \\+100 分/.test(ckc()), ckc().slice(0, 300));
    ck("日历带:领到/没签/今天待签三种格都在",
      !!document.querySelector("#ckCenter .ckc.claimed") &&
      !!document.querySelector("#ckCenter .ckc.missed") &&
      !!document.querySelector("#ckCenter .ckc.pending"));
    ck("日历带汇总:完成 28(今天与漏签不算完成)", /完成 28/.test(ckc()), (ckc().match(/完成 \\d+/g) || []).join(","));
    ck("明细表:领到 +100 与 未成功 都在", /\\+100/.test(ckc()) && /未成功/.test(ckc()));
    const chip = document.querySelector('#ckCenter [data-act="ckhfilter"][data-filter="wb"]');
    if (chip) chip.click();
    ck("明细渠道筛选可用", !!document.querySelector("#ckCenter .cklr"));
    CK.wb.enabled = false;
    ckTabUpdate();
    ck("全部关闭:签到页签隐藏且自动退回渠道页",
      document.querySelector('#mTabs [data-p="ck"]').hidden === true &&
      document.querySelector('#mTabs [data-p="ch"]').classList.contains("on"));
  } catch (e) {
    ck("签到中心页签", false, e.message);
  } finally {
    CK = {};
    CKH = null;
    CKH_FILTER = "all";
    CFG.checkin = undefined;
    CHECKIN_ON.delete("wb");
  }

  // ── 紧凑条可选开关(2026-10-23)──
  window.__ASYNC = (async () => {
    try {
      // 先让真实 init(读配置 → applyForm(panel) → refresh)跑完,别和用例抢形态
      await new Promise((r) => setTimeout(r, 400));
      CHANNELS = data.CHANNELS;
      CFG = JSON.parse(JSON.stringify(data.CFG));
      hvApply(data.HOME);
      DOCK.on = false;
      rowOpen.clear();
      document.body.className = "form-panel";

      CFG.compactEnabled = false;
      applyCompactUI();
      ck("紧凑条默认关:body 不带 compact-on", !document.body.classList.contains("compact-on"));
      ck("收口函数:关闭时 compact → panel", resolveForm("compact") === "panel", resolveForm("compact"));

      await applyForm("compact");
      ck("关闭时切紧凑条会落成面板",
        document.body.classList.contains("form-panel") && !document.body.classList.contains("form-compact"),
        document.body.className);

      await applyForm("pill");
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      await new Promise((r) => { const t0 = Date.now(); (function p(){ if (document.body.classList.contains("form-panel") || Date.now() - t0 > 1500) return r(); setTimeout(p, 15); })(); });
      ck("关闭时胶囊 Esc 回面板", document.body.classList.contains("form-panel"), document.body.className);

      // 顶栏折叠入口:关紧凑条时是「胶囊」,不能只剩 Esc/Ctrl+3 两条隐藏路径
      document.body.className = "form-panel";
      document.getElementById("btnPill").click();
      await new Promise((r) => { const t0 = Date.now(); (function p(){ if (document.body.classList.contains("form-panel") || Date.now() - t0 > 1500) return r(); setTimeout(p, 15); })(); });
      ck("关闭紧凑条时顶栏「胶囊」能进胶囊", document.body.classList.contains("form-pill"), document.body.className);
      await applyForm("panel");

      document.dispatchEvent(new KeyboardEvent("keydown", { key: "2", ctrlKey: true, bubbles: true }));
      await new Promise((r) => { const t0 = Date.now(); (function p(){ if (document.body.classList.contains("form-pill") || Date.now() - t0 > 1500) return r(); setTimeout(p, 15); })(); });
      ck("关闭时 Ctrl+2 不切形态", document.body.classList.contains("form-panel"), document.body.className);

      CFG.compactEnabled = true;
      applyCompactUI();
      ck("打开开关后 body 带 compact-on", document.body.classList.contains("compact-on"));
      ck("收口函数:开启时 compact 放行", resolveForm("compact") === "compact");
      await applyForm("compact");
      ck("开启后能进紧凑条", document.body.classList.contains("form-compact"), document.body.className);

      CFG.compactEnabled = false;
      applyCompactUI();
      await applyForm("panel");
      ck("关掉开关后按钮状态同步(无 compact-on)", !document.body.classList.contains("compact-on"));
    } catch (e) {
      ck("紧凑条可选开关", false, e.message);
    }
  })();

  // 签到中心入口(异步):点首页签到格 → 落到签到中心页签,数据来自
  // checkin_state / checkin_history 桩 —— 验证 openCheckinCenter 整条链路。
  window.__ASYNC2 = (async () => {
    try {
      await new Promise((r) => setTimeout(r, 420)); // 等上一个异步块收尾
      document.body.className = "form-panel";
      CHANNELS = data.CHANNELS;
      CFG = JSON.parse(JSON.stringify(data.CFG));
      hvApply(data.HOME);
      CHECKIN_ON.add("wb");
      CFG.checkin = { wb: { enabled: true, auto: true } };
      window.__CK_ST = [{ id: "wb", name: "WorkBuddy", enabled: true, auto: true, doneToday: true,
        canTry: false, outcome: "claimed:wb", credits: 100, monthCredits: 3100 }];
      window.__CK_H = {
        days: 90,
        channels: [{ id: "wb", name: "WorkBuddy", enabled: true, auto: true, enabledSince: 1790000000,
          monthCredits: 3100, today: "2026-10-30",
          days: Array.from({ length: 30 }, (_, i) => ({
            day: "2026-10-" + String(i + 1).padStart(2, "0"), ts: 1790000000 + i * 3600,
            status: i === 29 ? "pending" : "claimed", credits: i === 29 ? null : 100, attempts: 1 })) }],
        rows: [{ id: "wb", name: "WorkBuddy", ts: 1794300000, day: "2026-10-30", outcome: "claimed:wb", credits: 100, attempts: 1, detail: "" }],
      };
      render({ updated: false });
      const cell = document.querySelector('#sum [data-act="ckstripopen"]');
      if (!cell) { ck("签到格在场", false, document.getElementById("sum").innerHTML.slice(0, 120)); return; }
      cell.click();
      await new Promise((r) => setTimeout(r, 60));
      const btn = document.querySelector('#mTabs [data-p="ck"]');
      ck("点首页签到格:落到签到中心页签且页签可见",
        btn.classList.contains("on") && btn.hidden === false, "hidden=" + btn.hidden);
      ck("签到中心渲染出今日行与日历带",
        !!document.querySelector('#ckCenter .ckch[data-ch="wb"]') &&
        !!document.querySelector("#ckCenter .ckband") &&
        /已领到 \\+100 分/.test(document.getElementById("ckCenter").innerHTML),
        document.getElementById("ckCenter").innerHTML.slice(0, 160));
    } catch (e) {
      ck("签到中心入口链路", false, e.message);
    } finally {
      window.__CK_ST = null;
      window.__CK_H = null;
      CK = {};
      CKH = null;
      CFG.checkin = undefined;
      CHECKIN_ON.delete("wb");
      CHANNELS = data.CHANNELS;
      hvApply(data.HOME);
      document.body.className = "form-panel";
      render({ updated: false });
    }
  })();

  // 给外层断言留个后门:CHANNELS / CFG 是 eval 里的 let,外层 window 上看不到
  // (只有函数声明会挂到 globalThis)。自动隐藏那条用例要直接摆一张卡片出来。
  window.__TEST = {
    setChannels: (list) => { CHANNELS = list; },
    patchCfg: (patch) => Object.assign(CFG, patch),
    cfg: () => CFG,
    openManage: () => openManage(),
    refreshChCards: () => refreshChCards(),
    fillKeyHints: (force) => fillKeyHints(force),
    invalidateKeyMeta: (id, kind) => invalidateKeyMeta(id, kind),
    layoutLabel: () => ($("lkLayout") ? $("lkLayout").textContent : ""),
    panelContentSize: (layout) => panelContentSize(layout),
    updateLayout: () => updateLayoutUI(),
    theme: (t) => {
      const was = CFG.theme;
      CFG.theme = t;
      applyTheme();
      const on = document.body.classList.contains("light") === (t === "light");
      const btn = document.querySelector('#cfgTheme button[data-theme="' + t + '"]');
      const state = on && btn && btn.getAttribute("aria-pressed") === "true";
      CFG.theme = was;
      applyTheme();
      return state;
    },
  };

  // ── Qoder 逐包到期(2026-10-30)──
  // 三层:明细卡逐包列表(契约含"已过期但还有余量")、无数据的诚实空态、
  // 渠道卡「登录网页版」入口(只给 qoder,别的渠道不塞)。
  try {
    const nowS = Math.floor(Date.now() / 1000);
    const qd = { id: "qoder", name: "Qoder", short: "QD", color: "#00b3a4", kind: "points",
      unit: "credits", remaining: 986, valid: true, hasKey: true, authSource: "app",
      expiring: [
        { at: nowS - 86400 * 2, amount: 300, label: "Purchased Credits" },
        { at: nowS + 86400 * 3, amount: 500, label: "Bonus" },
        { at: nowS + 86400 * 20, amount: 186, label: "积分包" },
      ], expiryComplete: true, updatedAt: nowS };
    window.__TEST.setChannels([...data.CHANNELS, qd]);
    window.__TEST.refreshChCards();
    const dq = renderDetail(qd);
    ck("逐包到期:未到期的包带日期与剩余,默认收起,过期的进「全部 3 笔」",
      /500 分/.test(dq) && /186 分/.test(dq) && /全部 3 笔/.test(dq) && !/300 分/.test(dq),
      dq.slice(0, 300));
    ck("有真实包时不再写「接口未提供」", !/接口未提供/.test(dq), (dq.match(/接口未提供/g) || []).join(","));

    // 来源代号翻译(2026-11-10 真机截图):活动编号、带前缀的 source 枚举、
    // 认不出的代号、本来就是中文的 label —— 四种都要读得懂。
    const qdZh = Object.assign({}, qd, {
      expiring: [
        { at: nowS + 86400 * 3, amount: 2000, label: "act-20261110-699" },
        { at: nowS + 86400 * 29, amount: 98, label: "RESOURCE_PACKAGE_SOURCE_BONUS" },
        { at: nowS + 86400 * 30, amount: 100, label: "MYSTERY_CODE" },
      ],
      expiryComplete: true,
    });
    const dZh = renderDetail(qdZh);
    ck("逐包来源代号译成中文:活动编号 / BONUS / 未识别代号保留原文",
      /活动积分包 · 2026-11-10/.test(dZh) && /赠送积分包/.test(dZh) &&
      /积分包 · MYSTERY_CODE/.test(dZh) && !/<p>RESOURCE_PACKAGE_SOURCE_BONUS<[/]p>/.test(dZh),
      dZh.slice(0, 400));
    ck("原始代号留在悬停提示里,便于去官网对账",
      /平台原始代号:RESOURCE_PACKAGE_SOURCE_BONUS/.test(dZh) &&
      /平台原始代号:act-20261110-699/.test(dZh),
      (dZh.match(/title="[^"]*"/g) || []).slice(0, 3).join(" | "));
    ck("译过的明细补一句说明", /来源代号已译为中文/.test(dZh));
    const dCn = renderDetail(Object.assign({}, qd, {
      expiring: [{ at: nowS + 86400, amount: 10, label: "签到积分" }], expiryComplete: true,
    }));
    ck("本来就是中文的来源标签不被改写,也不加多余说明",
      /<p>签到积分<[/]p>/.test(dCn) && !/来源代号已译为中文/.test(dCn),
      dCn.slice(0, 300));

    window.__TEST.openManage();
    const qcard = document.querySelector('#chList .ccard[data-id="qoder"]');
    const qquick = qcard && qcard.querySelector(".ccquick");
    ck("已登录:Qoder 渠道卡显示「已登录」与「重新登录」,不再劝登录",
      !!qquick && /已登录网页账号 · 逐笔明细可用/.test(qquick.textContent) &&
      /重新登录/.test(qquick.textContent) && !/登录网页版/.test(qquick.textContent),
      qquick && qquick.innerHTML.slice(0, 320));
    // 未登录态:逐笔明细不完整时仍要给「登录网页版」入口
    window.__TEST.setChannels([...data.CHANNELS,
      Object.assign({}, qd, { expiryComplete: false, expiring: [] })]);
    window.__TEST.openManage();
    const qcardOff = document.querySelector('#chList .ccard[data-id="qoder"]');
    const qquickOff = qcardOff && qcardOff.querySelector(".ccquick");
    ck("未登录:Qoder 渠道卡仍写「需登录」并给「登录网页版」",
      !!qquickOff && /逐笔积分到期明细需登录网页账号/.test(qquickOff.textContent) &&
      /登录网页版/.test(qquickOff.textContent) && !/重新登录/.test(qquickOff.textContent),
      qquickOff && qquickOff.innerHTML.slice(0, 320));
    const ocard = document.querySelector('#chList .ccard[data-id="ds"]');
    ck("别的渠道不塞登录入口", !!ocard && !/登录网页版/.test(ocard.innerHTML));

    const qd2 = Object.assign({}, qd, { expiring: [], expiryComplete: false });
    const dq2 = renderDetail(qd2);
    ck("没有包数据:空态仍说「接口未提供完整的逐笔到期信息」",
      /接口未提供完整的逐笔到期信息/.test(dq2), dq2.slice(0, 200));
    const qd3 = Object.assign({}, qd, { expiring: [], expiryComplete: true });
    const dq3 = renderDetail(qd3);
    ck("平台确认无包:说「没有已知到期日」而不是装失败",
      /没有已知到期日/.test(dq3), dq3.slice(0, 200));

    // 账户级有效期:结构化字段优先;旧快照回退只认日期,不补时间/倒计时
    const at = Math.floor(Date.now() / 1000) + 20 * 86400;
    const qdAcc = Object.assign({}, qd, {
      expiring: [], expiryComplete: false, extra: [], accountExpiry: { state: "dated", at },
    });
    ck("Qoder 账户到期用结构化时间", /账户额度到期/.test(renderDetail(qdAcc)) &&
      /\\d{2}:\\d{2}/.test(renderDetail(qdAcc)) && /账户级有效期/.test(renderDetail(qdAcc)),
      renderDetail(qdAcc).slice(0, 240));
    const qdNever = Object.assign({}, qd, {
      expiring: [], expiryComplete: false, extra: [], accountExpiry: { state: "never", at: null },
    });
    ck("Qoder 明确不过期:写长期有效", /长期有效/.test(renderDetail(qdNever)) &&
      /账户额度不过期/.test(renderDetail(qdNever)), renderDetail(qdNever).slice(0, 200));
    const qdOld = Object.assign({}, qd, {
      expiring: [], expiryComplete: false, accountExpiry: null,
      extra: [["账户额度到期", "2026-11-30"]],
    });
    const dOld = renderDetail(qdOld);
    ck("Qoder 旧快照只显示日期,不补 23:59 或小时倒计时",
      /2026-11-30/.test(dOld) && !/\\d{2}:\\d{2}/.test(dOld) && /旧快照只记录了日期/.test(dOld),
      dOld.slice(0, 240));
    const qdNoAcc = Object.assign({}, qd, {
      expiring: [], expiryComplete: false, accountExpiry: null, extra: [],
    });
    ck("Qoder 无账户字段:账户级仍写暂未提供,不冒充长期有效",
      /账户额度到期/.test(renderDetail(qdNoAcc)) && /暂未提供/.test(renderDetail(qdNoAcc)) &&
      !/长期有效/.test(renderDetail(qdNoAcc)), renderDetail(qdNoAcc).slice(0, 240));
  } catch (e) {
    ck("Qoder 逐包到期 UI", false, e.message);
  } finally {
    window.__TEST.setChannels(data.CHANNELS);
    window.__TEST.refreshChCards();
  }

  // ── 详情弹层交互(2026-11-03 真机探针固化)──
  try {
    const nowS2 = Math.floor(Date.now() / 1000);
    const wbd = { id: "wbd", name: "WorkBuddy", short: "WB", color: "#3b82f6", kind: "points",
      unit: "credits", remaining: 860, valid: true, hasKey: true, authSource: "web",
      expiryComplete: true,
      expiring: [1, 2, 3, 4, 5].map((n) => ({ at: nowS2 + n * 86400, amount: 100 * n, label: "来源" + n })),
      updatedAt: nowS2 };
    window.__TEST.setChannels([...data.CHANNELS, wbd]);
    openDetail("wbd");
    ck("详情:焦点进入弹层", document.getElementById("detailSheet").contains(document.activeElement));
    ck("详情:最近一笔卡 + 默认 3 笔明细",
      /最近一笔到期/.test(document.getElementById("detailBody").textContent) &&
      document.querySelectorAll("#detailBody .expiry-entry").length === 3);
    document.activeElement.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    ck("详情:Tab 焦点不逃逸", document.getElementById("detailSheet").contains(document.activeElement));
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    // 焦点回卡依赖 offsetParent(jsdom 恒为 null),真机探针单独验证;这里断言关闭本身
    ck("详情:Esc 关闭弹层", document.getElementById("detailShade").hidden === true);
    const four = CHANNELS.find((c) => c.id === "4sapi");
    changesCache["4sapi"] = []; changesState["4sapi"] = "done";
    ck("详情:空台账给「7 天无变动记录」", /7 天无变动记录/.test(renderDetail(four)));
    changesState["4sapi"] = "loading";
    ck("详情:台账未取到不装作无记录", !/7 天无变动记录/.test(renderDetail(four)));
    delete changesState["4sapi"]; delete changesCache["4sapi"];
  } catch (e) { ck("详情弹层交互", false, e.message); }

  // ── 设置索引 + 标题栏 ⋯ 菜单(2026-11-01 精修)──
  try {
    openManage();
    const idxOn = document.body.classList.contains("manage-index");
    const idxRows = document.querySelectorAll("#mIndexList .midx-row").length;
    const idxTitle = document.querySelector(".mhead .mt").textContent;
    ck("设置默认落在索引页(含网关，共 9 个分区)", idxOn && idxRows === 9 && !!document.querySelector('#mIndexList [data-p="gateway"]') && !!document.querySelector('#mIndexList [data-p="sync"]') && idxTitle === "设置与管理", idxOn + "/" + idxRows + "/" + idxTitle);
    const savedSyncStatus = SYNC_STATUS;
    const testSync = { relayId: "test", hasGithub: true, hasKey: true, enabled: true, oauthClientConfigured: true, codeText: "GT1.test-secret", channels: [] };
    SYNC_STATUS = testSync;
    SYNC_CODE_REVEALED = false;
    renderSync();
    ck("点隐藏后不显示明文", !document.getElementById("syncCodeText").textContent.includes("test-secret"));
    SYNC_CODE_REVEALED = true;
    renderSync();
    ck("进入同步页直接显示同步码", document.getElementById("syncCodeText").textContent === "GT1.test-secret");
    document.getElementById("syncRevealBtn").click();
    ck("点隐藏后不显示明文", !document.getElementById("syncCodeText").textContent.includes("test-secret"));
    document.getElementById("syncRevealBtn").click();
    ck("再次显示同步码", document.getElementById("syncCodeText").textContent === "GT1.test-secret");
    showTab("index");
    ck("离开同步页时隐藏同步码", !document.getElementById("syncCodeText").textContent.includes("test-secret"));
    SYNC_STATUS = savedSyncStatus;
    renderSync();
    window.__ASYNC3 = (async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
      SYNC_STATUS = testSync;
      SYNC_CODE_REVEALED = true;
      renderSync();
      await new Promise((resolve) => setTimeout(resolve, 0));
      ck("二维码与同步码同时生成", !!document.querySelector("#syncQr svg"),
        (document.getElementById("syncQr").innerHTML || "").slice(0, 120) + " busy=" + SYNC_QR_BUSY + " failed=" + SYNC_QR_FAILED + " code=" + SYNC_QR_CODE + " revealed=" + SYNC_CODE_REVEALED + " statusCode=" + (SYNC_STATUS && SYNC_STATUS.codeText));
      SYNC_CODE_REVEALED = false;
      renderSync();
      SYNC_STATUS = savedSyncStatus;
      renderSync();
    })();
    document.querySelector('#mIndexList [data-p="ch"]').click();
    ck("点索引行进入子页并更新页头", !document.body.classList.contains("manage-index")
      && document.querySelector('#mTabs [data-p="ch"]').classList.contains("on")
      && document.querySelector(".mhead .mt").textContent === "渠道管理",
      document.querySelector(".mhead .mt").textContent);
    document.getElementById("btnBack").click();
    ck("返回键回到索引页", document.body.classList.contains("manage-index")
      && document.querySelector(".mhead .mt").textContent === "设置与管理",
      document.querySelector(".mhead .mt").textContent);
    const topIds = ["btnR", "btnP", "btnPill", "btnH"];
    const topBtns = topIds.map((id) => document.getElementById(id));
    // 顺序本身是用户裁决(2026-11-03 定四枚,2026-11-11 胶囊与隐藏对调),
    // 用 compareDocumentPosition 断言 DOM 先后,防止以后被无意挪动。
    const topOrderOk = topBtns.every((el, i) =>
      i === 0 || !!(el.compareDocumentPosition(topBtns[i - 1]) & Node.DOCUMENT_POSITION_PRECEDING));
    ck("顶栏固定四枚:刷新/置顶/胶囊/隐藏",
      topBtns.every(Boolean) && topBtns.every((el) => el.hidden === false) && topOrderOk,
      topIds.map((id, i) => id + "=" + (topBtns[i] ? !topBtns[i].hidden : "missing")).join(" "));
    ck("顶栏不再有「更多操作」菜单",
      !document.getElementById("btnMenu") && !document.getElementById("hmenu"));
    const layoutBtn = document.getElementById("lkLayout");
    ck("底栏常驻「单列 / 双列」布局切换",
      !!layoutBtn && layoutBtn.style.display !== "none" && /单列|双列/.test(layoutBtn.textContent),
      layoutBtn ? layoutBtn.textContent.trim() + " / " + (layoutBtn.style.display || "inline") : "missing");
    // 从索引进子页必须触发该页签的「进入时读一次」:密钥箱以前只在隐藏的旧页签栏
    // 的 click 里 loadVault,索引行点进去是个空面板,连「新增一把密钥」都没有。
    document.querySelector('#mIndexList [data-p="vault"]').click();
    window.__ASYNC3 = (window.__ASYNC3 || Promise.resolve()).then(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
      const vaultHtml = document.getElementById("vaultList").innerHTML;
      ck("从索引进密钥箱:读出列表并给出新增入口", /新增一把密钥/.test(vaultHtml), vaultHtml.slice(0, 100));

      // 非空列表:平台分组卡 / 条目行 / 引用说明 / 顶部计数
      const nowTs = Math.floor(Date.now() / 1000);
      window.__VAULT = [
        { id: "v1", platform: "4SAPI", label: "主号", hint: "••••abcd", note: "充值用的那个", defaultChannel: "4sapi", defaultAvailable: true, apiEndpoint: "https://4sapi.com/v1",
          createdAt: nowTs, updatedAt: nowTs },
        { id: "v2", platform: "4SAPI", label: "备用", hint: "••••wxyz", note: "",
          createdAt: nowTs - 2, updatedAt: nowTs - 1 },
        { id: "v3", platform: "Hapi", label: "工作", hint: "••••1234", note: "",
          createdAt: nowTs - 3, updatedAt: nowTs + 1 },
      ];
      window.__VAULTREF = [["4sapi", "v1"]];
      await loadVault();
      ck("密钥箱非空:三条密钥", /class="vgroup"/.test(document.getElementById("vaultList").innerHTML)
        && document.querySelectorAll("#vaultList .vitem").length === 3,
        document.querySelectorAll("#vaultList .vitem").length);
      ck("密钥箱计数读作 3 把 · 2 个平台",
        document.getElementById("vaultMeta").textContent === "3 把 · 2 个平台",
        document.getElementById("vaultMeta").textContent);
      ck("被引用的条目写明正被谁使用", /正被 4SAPI 使用/.test(document.getElementById("vaultList").innerHTML));
      ck("未引用条目不显示多余提示", !/未被任何渠道使用/.test(document.getElementById("vaultList").textContent));
      ck("默认按更新时间全局排序", [...document.querySelectorAll("#vaultList .vrow")].map(x=>x.dataset.id).join(",") === "v3,v1,v2");
      ck("渠道默认密钥只读", !document.querySelector('[data-act="vedit"][data-id="v1"]') && !document.querySelector('[data-act="vdel"][data-id="v1"]') && !!document.querySelector('[data-act="vedit"][data-id="v2"]'));
      const copied = [];
      window.__VAULT_COPY_INVOKE = async (cmd, args) => { copied.push({cmd, args}); return "已复制"; };
      document.querySelector('[data-act="vcopy"][data-id="v1"]').click();
      document.querySelector('[data-act="vcopy-endpoint"][data-id="v1"]').click();
      await new Promise(resolve => setTimeout(resolve, 0));
      ck("密钥与端点分别复制", copied.length === 2 && copied[0].cmd === "vault_copy" && copied[0].args.id === "v1"
        && copied[1].cmd === "copy_text" && copied[1].args.text === "https://4sapi.com/v1");
      ck("未填写端点时禁用复制端点", document.querySelector('[data-act="vcopy-endpoint"][data-id="v2"]').disabled);
      delete window.__VAULT_COPY_INVOKE;
      const sort = document.getElementById("vaultSort"); sort.value = "platform"; sort.dispatchEvent(new Event("change", {bubbles:true}));
      ck("可按平台排序", [...document.querySelectorAll("#vaultList .vrow")].map(x=>x.dataset.id).join(",") === "v2,v1,v3");
      sort.value = "updated"; sort.dispatchEvent(new Event("change", {bubbles:true}));
      // 分组「新增」:带入平台,表单可 Esc 取消
      document.querySelector('.vault-top [data-act="vnew"]').click();
      ck("新增密钥打开表单", !!document.querySelector("#vaultForm .vform"));
      ck("密钥箱表单提供平台选择、分组、账号和网址", !!document.getElementById("vPlatSelect")
        && !!document.getElementById("vEndpoint") && !!document.getElementById("vWebsite")
        && /分组/.test(document.getElementById("vaultForm").textContent) && /账号/.test(document.getElementById("vaultForm").textContent));
      const select = document.getElementById("vPlatSelect"); select.value = "Hapi";
      select.dispatchEvent(new Event("change", { bubbles: true }));
      ck("选择平台后填入平台名称", document.getElementById("vPlat").value === "Hapi");
      document.querySelector("#vaultForm .vform").dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true })
      );
      ck("表单 Esc 取消后收起", !document.querySelector("#vaultForm .vform"));
      // 搜索无结果:给清除入口;点一下恢复整份列表
      const vq = document.getElementById("vaultQ");
      vq.value = "不存在的关键词";
      vq.dispatchEvent(new Event("input", { bubbles: true }));
      ck("搜索无结果给出清除入口", /清除搜索/.test(document.getElementById("vaultList").innerHTML)
        && document.getElementById("vaultQClear").hidden === false);
      document.querySelector('[data-act="vqclear"]').click();
      ck("清除搜索恢复列表", document.querySelectorAll("#vaultList .vitem").length === 3
        && document.getElementById("vaultQClear").hidden === true);

      delete window.__VAULT;
      delete window.__VAULTREF;
      showTab("index");
    });
    closeManage();
  } catch (e) {
    ck("设置索引 / 顶栏入口", false, e.message);
  }

})();
`;

w.__DATA = DATA;
(async () => {
  try {
    w.eval(js + "\n;\n" + HARNESS);
  } catch (e) {
    console.log("✗ 脚本整体执行抛异常:" + e.message);
    console.log((e.stack || "").split("\n").slice(0, 4).join("\n"));
    process.exit(1);
  }
  if (w.__ASYNC) await w.__ASYNC;
  if (w.__ASYNC2) await w.__ASYNC2;
  if (w.__ASYNC3) await w.__ASYNC3;

  const R = w.__R || [];
  // 真正点击分享按钮，覆盖获取/复制两阶段失败与重试。
  for (const [id, platform] of [
    ["btnCopyWindowsDownload", "windows"], ["btnCopyAndroidDownload", "android"],
  ]) {
    const btn = w.document.getElementById(id);
    for (const failure of ["download_link", "copy_text", null]) {
      const calls = [];
      let finish;
      w.__DOWNLOAD_INVOKE = async (cmd, args) => {
        calls.push({ cmd, args });
        if (cmd === "download_link") await new Promise((resolve) => { finish = resolve; });
        if (cmd === failure) throw new Error("模拟失败");
        return "https://example.com/" + platform;
      };
      btn.click();
      btn.click();
      R.push({ name: platform + " 获取时禁用按钮，阻止重复点击 " + failure,
        ok: btn.disabled && btn.textContent === "正在获取…" && calls.length === 1 });
      finish();
      await new Promise((resolve) => setTimeout(resolve, 0));
      const copied = calls.find((c) => c.cmd === "copy_text");
      R.push({ name: platform + " 分享下载 " + (failure || "成功重试"),
        ok: calls[0].args.platform === platform && !btn.disabled && btn.textContent === "复制下载直链" &&
          (failure === "download_link" ? !copied : copied.args.text === "https://example.com/" + platform) &&
          w.document.getElementById("toast").textContent.includes(failure ? "复制下载直链失败" : "下载直链已复制") });
    }
  }
  delete w.__DOWNLOAD_INVOKE;
  const css = fs.readFileSync(path.join(SRC, "styles.css"), "utf8");
  R.push({
    name: "顶栏固定四枚,不再有「紧凑」按钮与弹出菜单",
    ok: /id="btnR"/.test(html) && /id="btnP"/.test(html) && /id="btnH"/.test(html) &&
      /id="btnPill"/.test(html) && !/id="btnC"/.test(html) && !/id="btnMenu"/.test(html) &&
      !/id="hmenu"/.test(html) && !/\.hmenu\{/.test(css),
    extra: "index.html/styles.css 仍残留旧顶栏入口",
  });
  R.push({
    name: "设置页有紧凑条开关、顶栏四枚按钮齐全",
    ok: /id="cfgCompact"/.test(html) &&
      ["btnR", "btnP", "btnH", "btnPill"].every((id) => html.includes(`id="${id}"`)),
    extra: "index.html 缺 cfgCompact 或顶栏按钮",
  });
  R.push({
    name: "底栏布局切换常驻,旧的拖动缩放入口已移除",
    ok: /id="lkLayout"/.test(html) && !/id="lkReset"/.test(html) &&
      !/id="rzFrame"/.test(html) && !/id="rzGrip"/.test(html) && /id="cfgLayout"/.test(html),
    extra: "index.html 仍有 lkReset / rzFrame / rzGrip,或缺 lkLayout / cfgLayout",
  });

  // ── 设置页文案预算(2026-10-23 三层分流)────────────────────────────────
  // 常驻说明曾经堆到 936 汉字(折合约 570px,超过一整个面板高)。现在的口径:
  //   L1 常驻 ≤ 每页 120 字、单条 desc ≤ 30 字;L2 全部收进 WHY,点「?」才展开。
  // 这两条是**回归闸门**:以后再加说明,超预算就在这里红。
  const cn = (s) => (s.match(/[\u4e00-\u9fa5]/g) || []).length;
  const doc = w.document;
  const over = [];
  doc.querySelectorAll(".mbody").forEach((body) => {
    const page = body.dataset.p;
    let sum = 0;
    body.querySelectorAll(".khint").forEach((el) => {
      // 互斥的恢复/设置状态不会同时显示，不应把隐藏面板里的文案累加。
      if (el.closest("[hidden]")) return;
      // 「?」展开块是 L2(按需),不算常驻 —— 它只是挂在 .khint 里面
      const c = el.cloneNode(true);
      c.querySelectorAll(".whybox").forEach((x) => x.remove());
      sum += cn(c.textContent);
    });
    if (sum > 120) over.push(`${page} khint ${sum} 字`);
    body.querySelectorAll(".desc").forEach((el) => {
      const n = cn(el.textContent);
      if (n > 30) over.push(`${page} desc「${el.textContent.trim().slice(0, 12)}…」${n} 字`);
    });
  });
  R.push({
    name: "设置页常驻文案在预算内(khint ≤120 字/页,desc ≤30 字/条)",
    ok: over.length === 0,
    extra: over.join(" · "),
  });

  const whys = [...doc.querySelectorAll("#manage button.why[data-why]")];
  const boxOf = (b) => doc.querySelector(`.whybox[data-for="${b.dataset.for}"]`);
  R.push({
    name: "每个「?」都有说明文案(缺文案时按钮应被摘掉)",
    ok: whys.length > 0 && whys.every((b) => {
      const box = boxOf(b);
      return box && cn(box.textContent) > 0;
    }),
    extra: `${whys.length} 个按钮,hydrate 未补齐`,
  });

  const first = whys[0];
  let toggled = false;
  if (first) {
    first.dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
    const box = boxOf(first);
    toggled = first.getAttribute("aria-expanded") === "true" && !!box && box.classList.contains("on");
    // 再点一次要收回:展开是"看一次"的东西,不能点开就关不上
    first.dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
    toggled = toggled && first.getAttribute("aria-expanded") === "false" &&
      !!box && !box.classList.contains("on");
  }
  R.push({ name: "点「?」原地展开、再点收回", ok: toggled, extra: "aria-expanded / .on 未同步" });

  const fold = doc.querySelector(".mbody[data-p='about'] details.fold");
  R.push({
    name: "关于页长文默认收起(<details> 无 open)",
    ok: !!fold && !fold.hasAttribute("open") && !!fold.querySelector("summary"),
    extra: "缺 details.fold 或默认展开",
  });

  // ── WorkBuddy AI(国际版)的积分获取文案 ─────────────────────────────
  // 四态必须分得开:活跃 / 只有充值 / 今天没有 / 这个渠道不记到账。
  // 用户问的是"今天有没有活跃获取",把充值说成活跃就是误报。
  try {
    const line = (g) => w.grantLine({ grantToday: g });
    const active = line({ hasActivity: true, activityTotal: 30, total: 30, lastAt: 1_700_000_000, fromApi: false });
    const api = line({ hasActivity: true, activityTotal: 30, total: 30, lastAt: 1_700_000_000, fromApi: true });
    const topup = line({ hasActivity: false, total: 500, lastAt: 1_700_000_000 });
    const none = line({ hasActivity: false, total: 0 });
    R.push({
      name: "积分获取文案:活跃 / 充值 / 未获取 / 不记",
      ok:
        /今日活跃 \+30/.test(active) && /推算/.test(active) &&
        /今日活跃 \+30/.test(api) && !/推算/.test(api) &&
        /非活跃/.test(topup) && none === "今日未获取积分" && line(null) === "",
      extra: [active, api, topup, none].join(" | "),
    });
  } catch (e) {
    R.push({ name: "积分获取文案:活跃 / 充值 / 未获取 / 不记", ok: false, extra: e.message });
  }
  R.push({
    name: "渠道卡有「记录」入口(懒加载 30 天明细)",
    ok: /data-act="grantlog"/.test(js) && /invoke\("credit_grants"/.test(js),
    extra: "app.js 缺 grantlog 分支或 credit_grants 调用",
  });

  // ── 没凭据自动隐藏(2026-10-23)────────────────────────────────────
  // 系统藏的 ≠ 用户藏的:文案必须说清,眼睛的动作也必须变成「仍然显示」。
  try {
    w.__TEST.setChannels([
      {
        id: "workbuddy-ai", name: "WorkBuddy AI", short: "WBA", color: "#4f7cff",
        kind: "points", hasKey: false, valid: false, hidden: true, autoHidden: true,
        authSource: "app", authLabel: "未检测到 WorkBuddy AI 登录信息", remaining: null,
        unit: "credits", expiring: [], expiryComplete: false, extra: [],
        error: "没检测到登录", stale: false, updatedAt: 0,
      },
    ]);
    w.__TEST.patchCfg({ hiddenChannels: [] });
    w.__TEST.openManage();
    const card = doc.querySelector("#chList .ccard");
    const st = card ? card.querySelector(".ccst").textContent : "";
    const eye = card ? card.querySelector(".eye").textContent : "";
    R.push({
      name: "自动隐藏的卡片说清是系统藏的,眼睛变成「显示」",
      ok: !!card && /已自动隐藏/.test(st) && eye === "显示",
      extra: JSON.stringify({ has: !!card, st, eye }),
    });
    // 点「显示」:记"用户表过态",**不**写进 hiddenChannels ——
    // 写进去就变成用户主动隐藏,以后再也不会自动恢复了。
    if (card) {
      card.querySelector(".eye").dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
    }
    const cfg = w.__TEST.cfg();
    R.push({
      name: "点眼睛记「表过态」且不写进用户隐藏清单",
      ok: !!cfg.userTouchedChannels && cfg.userTouchedChannels.includes("workbuddy-ai") &&
        !(cfg.hiddenChannels || []).includes("workbuddy-ai"),
      extra: JSON.stringify({
        touched: cfg.userTouchedChannels,
        hidden: cfg.hiddenChannels,
      }),
    });
  } catch (e) {
    R.push({ name: "自动隐藏的卡片说清是系统藏的,眼睛变成「显示」", ok: false, extra: e.message });
  }
  R.push({
    name: "点眼睛会记「用户表过态」,自动隐藏从此让位",
    ok: /userTouchedChannels/.test(js) && /cc\.autoHidden/.test(js),
    extra: "app.js 的 chvis 分支没处理 autoHidden / userTouched",
  });

  // ── 设置页密钥元数据缓存(2026-10-27 P2)──────────────────────────────
  // 打开设置页时每轮轮询都读 key_hint × N + key_source × N;现在只有首次/TTL
  // 过期/显式失效才读。这里连跑 10 次重绘,断言 RPC 次数不再增长。
  try {
    w.__TEST.setChannels([
      {
        id: "keychan", name: "KeyChan", short: "KC", color: "#4f7cff",
        kind: "amount", hasKey: true, valid: true, hidden: false, autoHidden: false,
        authSource: "keyring", authLabel: "Windows 凭据管理器", remaining: 42,
        unit: "CNY", expiring: [], expiryComplete: false, extra: [], error: null,
        stale: false, updatedAt: Math.floor(Date.now() / 1000),
      },
    ]);
    w.__TEST.patchCfg({ hiddenChannels: [], claimChannels: {} });
    w.__TEST.openManage();
    await new Promise((r) => setTimeout(r, 60));
    const hint0 = w.__CALLS.key_hint || 0;
    const src0 = w.__CALLS.key_source || 0;
    for (let i = 0; i < 10; i++) {
      w.__TEST.refreshChCards();
      await new Promise((r) => setTimeout(r, 5));
    }
    await new Promise((r) => setTimeout(r, 40));
    const hint1 = w.__CALLS.key_hint || 0;
    const src1 = w.__CALLS.key_source || 0;
    R.push({
      name: "设置页连续 10 次重绘不再重复读密钥元数据",
      ok: hint0 > 0 && src0 > 0 && hint1 === hint0 && src1 === src0,
      extra: JSON.stringify({ hint0, hint1, src0, src1 }),
    });
    // 保存密钥 = 显式失效点:下一次重绘必须重读
    w.__TEST.invalidateKeyMeta("keychan");
    w.__TEST.fillKeyHints(true);
    await new Promise((r) => setTimeout(r, 60));
    R.push({
      name: "密钥变更后元数据缓存失效并重读",
      ok: (w.__CALLS.key_hint || 0) > hint1 && (w.__CALLS.key_source || 0) > src1,
      extra: JSON.stringify({ hint: w.__CALLS.key_hint, src: w.__CALLS.key_source }),
    });
  } catch (e) {
    R.push({
      name: "设置页密钥元数据缓存",
      ok: false,
      extra: e.message + " @ " + (e.stack || "").split("\n")[1],
    });
  }

  // ── 固定窗口布局:单行 / 双行,两个尺寸都不可拖动 ──
  try {
    const single = w.__TEST.panelContentSize("single");
    const double = w.__TEST.panelContentSize("double");
    R.push({
      name: "固定布局尺寸:单列 480×820 / 双列 860×820",
      ok: single[0] === 480 && single[1] === 820 && double[0] === 860 && double[1] === 820,
      extra: JSON.stringify({ single, double }),
    });
    const before = w.__TEST.cfg().panelLayout;
    w.__TEST.patchCfg({ panelLayout: "double" });
    w.__TEST.updateLayout();
    const labelDouble = w.__TEST.layoutLabel();
    w.__TEST.patchCfg({ panelLayout: "single" });
    w.__TEST.updateLayout();
    const labelSingle = w.__TEST.layoutLabel();
    R.push({
      name: "底栏布局按钮显示切换目标(单列时显双列,双列时显单列)",
      ok: labelDouble === "单列" && labelSingle === "双列" &&
        w.document.querySelector('#cfgLayout [data-layout="double"]').getAttribute("aria-pressed") === "false" &&
        w.document.querySelector('#cfgLayout [data-layout="single"]').getAttribute("aria-pressed") === "true",
      extra: `double=${labelDouble} single=${labelSingle}`,
    });
    w.__TEST.patchCfg({ panelLayout: before || "single" });
    w.__TEST.updateLayout();
  } catch (e) {
    R.push({ name: "固定窗口布局", ok: false, extra: e.message });
  }

  // ── 主题令牌:切换只动 body 类与按钮状态,不写配置 ──
  try {
    R.push({ name: "暖白主题:body.light 与按钮状态同步", ok: w.__TEST.theme("light") });
    R.push({ name: "深色主题恢复:body 不再带 light", ok: w.__TEST.theme("dark") });
  } catch (e) {
    R.push({ name: "主题切换", ok: false, extra: e.message });
  }

  // 可选:把真实渲染函数的合成行交给浏览器排版检查,不读取用户配置或凭据。
  if (process.env.CODEX_LAYOUT_FIXTURES) {
    const base = { ...buildData(NOW).CHANNELS.find((c) => c.id === "codex"),
      name: "Codex", short: "CX", remaining: 98, valid: true, stale: false,
      extra: [["套餐", "plus"]], resetCredits: { availableCount: 0 },
      windows: [
        { label: "本周", remainPercent: 98, status: "ok", main: true },
        { label: "5 小时", remainPercent: 85, status: "ok", main: false }
      ]
    };
    const fixtures = [];
    for (const name of ["normal", "short-limited", "week-low", "stale", "error", "used", "week-only", "long-plan"]) {
      const c = JSON.parse(JSON.stringify(base));
      const state = name === "stale" ? "stale" : name === "error" ? "auth" : name === "short-limited" ? "limited" : "fresh";
      if (name === "short-limited") Object.assign(c.windows[1], { remainPercent: 0, status: "rate-limited" });
      if (name === "week-low") { c.windows[0].remainPercent = 4; c.remaining = 4; }
      if (name === "stale" || name === "error") { c.stale = true; c.updatedAt = NOW - 600; }
      if (name === "error") { c.valid = false; c.error = "登录状态已失效"; }
      if (name === "week-only") c.windows.pop();
      if (name === "long-plan") c.extra[0][1] = "enterprise-workspace-subscription";
      w.__TEST.patchCfg({ showUsed: name === "used" });
      w.hvApply({ ...DATA.HOME, rows: [{ id: "codex", state,
        tone: name === "short-limited" || name === "week-low" ? "bad" : "ok",
        tier: name === "short-limited" || name === "error" ? 0 : 1,
        secondary: name === "short-limited" ? "5 小时窗 · 2 小时后恢复" : null,
        qualifier: name === "short-limited" ? "限流" : null, headline: { percent: c.remaining } }] });
      fixtures.push({ name, markup: w.renderRow(c, 0), dual: name !== "week-only" });
    }
    fs.writeFileSync(process.env.CODEX_LAYOUT_FIXTURES, JSON.stringify(fixtures));
  }

  let ok = 0, bad = 0;
  for (const r of R) {
    if (r.ok) { ok++; console.log("  ok   " + r.name); }
    else { bad++; console.log("  FAIL " + r.name + (r.extra ? " :: " + r.extra : "")); }
  }
  console.log("\n" + (bad ? `✗ ${bad} 项失败` : "✓ 全部通过") + ` (${ok}/${R.length})`);
  process.exit(bad ? 1 : 0);
})();

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
    fontScale: "md", panelLayout: "single", form: "panel", hiddenChannels: [], channelOrder: [], pillChannels: [],
    alertPulse: false, peekOnClick: true, alwaysOnTop: false, dockEnabled: false,
    activeIntervalSec: 60, idleIntervalSec: 300, backoffIntervalSec: 900, trayShowNumber: true,
    autostart: false, autoCheckUpdate: true, notifyOnCrit: true, soundOn: false,
    resizeHintSeen: true,
    modelShowAll: false, modelGroup: null, modelBaseUrl: null,
    claimChannels: { "4sapi": { enabled: true, amount: 500, minIntervalDays: 14 } },
  };

  return { CHANNELS, HOME, CFG };
}
