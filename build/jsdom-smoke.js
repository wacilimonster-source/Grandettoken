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

const CALLS = Object.create(null);
w.__CALLS = CALLS;
w.__TAURI__ = {
  // 真实启动链路会读配置/渠道/视图;全返回 null 的话 init 走到 applyPinUI 会崩
  // (以前同步断言在 init 恢复之前就退出了,没有暴露)。异步用例要等 init,所以喂真数据。
  core: {
    invoke: async (cmd) => {
      CALLS[cmd] = (CALLS[cmd] || 0) + 1;
      if (cmd === "get_config") return JSON.parse(JSON.stringify(DATA.CFG));
      if (cmd === "get_channels") return JSON.parse(JSON.stringify(DATA.CHANNELS));
      if (cmd === "home_view") return JSON.parse(JSON.stringify(DATA.HOME));
      if (cmd === "checkin_state") return [];
      if (cmd === "app_version") return "0.0.0-test";
      if (cmd === "vault_list" || cmd === "vault_ref_map") return [];
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
    ck("详情里没有区间按钮与柱图", !/data-act="range"/.test(d) && !/spark/.test(d), d.slice(0, 300));
    ck("台账默认 3 条 +「全部 12 条」",
      (d.match(/class="lrow"/g) || []).length === 3 && /全部 12 条/.test(d),
      (d.match(/class="lrow"/g) || []).length);
    ck("台账时间是本地时钟(不再挤在 01:5x)", !/01:5\\d/.test(d) && /\\d{2}:\\d{2}/.test(d), d.match(/\\d{2}:\\d{2}/g));
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

  // ── 拿不到统一视图时的兜底 ──
  try {
    hvApply(null);
    HV = { byId: {}, order: [], risk: null, checkin: null, actionCount: 0, staleCount: 0, errorCount: 0, totals: null };
    render({});
    ck("视图缺失时退回本地判断(不空白、不抛错)",
      document.getElementById("list").querySelectorAll(".row").length > 0);
  } catch (e) { ck("视图缺失时退回本地判断(不空白、不抛错)", false, e.message); }

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
      await new Promise((r) => setTimeout(r, 30));
      ck("关闭时胶囊 Esc 回面板", document.body.classList.contains("form-panel"), document.body.className);

      // 顶栏折叠入口:关紧凑条时是「胶囊」,不能只剩 Esc/Ctrl+3 两条隐藏路径
      document.body.className = "form-panel";
      document.getElementById("btnPill").click();
      await new Promise((r) => setTimeout(r, 30));
      ck("关闭紧凑条时顶栏「胶囊」能进胶囊", document.body.classList.contains("form-pill"), document.body.className);
      await applyForm("panel");

      document.dispatchEvent(new KeyboardEvent("keydown", { key: "2", ctrlKey: true, bubbles: true }));
      await new Promise((r) => setTimeout(r, 30));
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
  };
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

  const R = w.__R || [];
  const css = fs.readFileSync(path.join(SRC, "styles.css"), "utf8");
  R.push({
    name: "关闭紧凑条时顶栏隐藏「紧凑」、显示「胶囊」",
    ok: /body:not\(\.compact-on\) #btnC\{display:none\}/.test(css) &&
      /body\.compact-on #btnPill\{display:none\}/.test(css),
    extra: "styles.css 缺互斥规则",
  });
  R.push({
    name: "设置页有紧凑条开关、顶栏有胶囊按钮",
    ok: /id="cfgCompact"/.test(html) && /id="btnPill"/.test(html),
    extra: "index.html 缺 cfgCompact / btnPill",
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
    fontScale: "md", form: "panel", hiddenChannels: [], channelOrder: [], pillChannels: [],
    alertPulse: false, peekOnClick: true, alwaysOnTop: false, dockEnabled: false,
    activeIntervalSec: 60, idleIntervalSec: 300, backoffIntervalSec: 900, trayShowNumber: true,
    autostart: false, autoCheckUpdate: true, notifyOnCrit: true, soundOn: false,
    modelShowAll: false, modelGroup: null, modelBaseUrl: null,
    claimChannels: { "4sapi": { enabled: true, amount: 500, minIntervalDays: 14 } },
  };

  return { CHANNELS, HOME, CFG };
}
