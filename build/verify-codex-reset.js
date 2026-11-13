// Codex 重置顾问前端冒烟：不依赖真实 Tauri，只验证页面入口、按需翻结论、
// 二次确认不存在、页签与速度试算、返回入口。运行需 NODE_PATH 指向仓库 node_modules。
const fs = require("fs");
const path = require("path");
const { JSDOM } = require("jsdom");

const root = path.resolve(__dirname, "..");
const html = fs.readFileSync(path.join(root, "app/src/index.html"), "utf8");
const appJs = fs.readFileSync(path.join(root, "app/src/app.js"), "utf8");
let moduleJs = fs.readFileSync(path.join(root, "app/src/codex-reset.js"), "utf8");
if (process.env.CODEX_RESET_SENTINEL === "1") {
  moduleJs = moduleJs.replace('invoke("codex_reset_advice"', 'invoke("__missing_reset_advice__"');
} else {
  const { spawnSync } = require("child_process");
  const probe = spawnSync(process.execPath, [__filename], {
    env: { ...process.env, CODEX_RESET_SENTINEL: "1" }, encoding: "utf8", timeout: 15000,
  });
  if (probe.status !== 1 || !probe.stderr.includes("FAIL: 初始结论 WAIT")) {
    throw new Error("重置顾问冒烟自检失败，未知命令未触发预期断言");
  }
  console.log("  ok   注入未知建议命令时冒烟变红");
}

const dom = new JSDOM(html, {
  runScripts: "outside-only",
  pretendToBeVisual: true,
  url: "http://localhost/",
});
const w = dom.window;
const doc = w.document;
const calls = [];
let profileNeed = "patient";
// 夹具时间一律相对本机时钟:需求新鲜度(12h)/过期标记/重置点都按真实 now 推,
// 钉死绝对时间戳会在跨天/12h 后把 NOW 误降级成 WAIT(2026-11-14 实测)
const FIXTURE_NOW = Math.floor(Date.now() / 1000);
let profileSubEnd = FIXTURE_NOW + 16 * 86400;
let toasts = [];
let failSave = false;
let failRead = false;
let tick;
w.setInterval = (callback) => { tick = callback; return 1; };

w.showToast = (msg, bad) => toasts.push({ msg, bad });
w.CHANNELS = [{
  id: "codex",
  kind: "codex",
  windows: [
    { key: "weekly", label: "周额度", percent: 100, remainPercent: 0, status: "rate-limited",
      resetsAt: "2026-11-13T09:56:52Z", main: true },
    { key: "5h", label: "5 小时窗", percent: 0, remainPercent: 100, status: "ok",
      resetsAt: "2026-11-13T09:53:53Z", main: false },
  ],
  resetCredits: {
    availableCount: 3,
    credits: [
      { title: "Full reset (Weekly + 5 hr)", expiresAt: 1796133030 },
      { title: "Full reset (Weekly + 5 hr)", expiresAt: 1796244909 },
      { title: "Full reset (Weekly + 5 hr)", expiresAt: FIXTURE_NOW + 25 * 86400 },
    ],
    lastRedeemedAt: null,
    updatedAt: FIXTURE_NOW - 60,
    error: null,
  },
  updatedAt: FIXTURE_NOW - 60,
  stale: false,
  valid: true,
}];
w.DETAIL_ID = "codex";

function adviceFor(kind) {
  const now = kind === "urgent";
  return {
    verdict: now ? "now" : "wait",
    reasonCode: now ? "deadline_before_reset" : "gap_too_short",
    headline: now ? "这次要用，就用一张重置卡" : "先等约 5.3 小时的自然重置",
    detail: now ? "周额度已用完，下次自然重置要等约 5.3 小时。" : "自然重置很快就到，现在用卡只能换回约 3.3%。",
    metrics: {
      weeklyRemainPercent: 0,
      weeklyUsedPercent: 100,
      nextResetAt: FIXTURE_NOW + 18936,
      gapHours: 5.26,
      cardsAvailable: 3,
      cardsUnknown: false,
      cardsSource: "live",
      canWait: true,
      fiveHourRemainPercent: 100,
      earliestCardExpiry: FIXTURE_NOW + 13 * 86400,
      cardsExpiringBeforeReset: 0,
      allCardsExpireBeforeReset: false,
      burnPercentPerDay: 14.8,
      burnEstimated: true,
      cardGainPercent: 3.3,
      subEndAt: profileSubEnd,
      daysToSubEnd: 16.47,
      dataUpdatedAt: FIXTURE_NOW - 60,
      planAvailable: true,
      plannedCards: 0,
      coverCards: 3,
      shortfallPercent: 10.9,
    },
    planLines: [
      "自然重置 3 次 · 到 11-30 还剩 16 天",
      "按 14.8%/天：省卡 0 张；到到期差 10.9% 个周窗",
      "想全程不断：补满要 3 张，但每张只换回 4%–4%，补不补都行",
    ],
    warnings: now ? ["low_value_now"] : [],
    computedAt: FIXTURE_NOW - 60,
  };
}

w.__TAURI__ = {
  core: {
    invoke: async (cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "codex_reset_profile_get") {
        if (failRead) throw new Error("read fixture");
        return { subEndAt: profileSubEnd, need: { kind: profileNeed, amountPercent: null, at: FIXTURE_NOW - 60 }, updatedAt: FIXTURE_NOW - 60 };
      }
      if (cmd === "codex_reset_profile_set") {
        if (failSave) throw new Error("write fixture");
        const p = (args && args.profile) || {};
        if (p.need && p.need.kind) profileNeed = p.need.kind;
        if (p.subEndAt) profileSubEnd = p.subEndAt;
        if (p.clearSubEnd) profileSubEnd = null;
        return { subEndAt: profileSubEnd, need: { kind: profileNeed, amountPercent: null, at: FIXTURE_NOW - 60 }, updatedAt: FIXTURE_NOW - 60 };
      }
      if (cmd === "codex_reset_advice") {
        const kind = (args && args.input && args.input.need && args.input.need.kind) || "patient";
        return adviceFor(kind === "urgent" ? "urgent" : "patient");
      }
      throw new Error("unexpected invoke: " + cmd);
    },
  },
};

w.renderDetailSheet = () => {
  const host = doc.getElementById("detailBody");
  if (w.CodexResetUI.isOpen()) host.innerHTML = w.CodexResetUI.renderDetail(w.CHANNELS[0]);
  else host.innerHTML = w.CodexResetUI.render(w.CHANNELS[0]);
};

w.eval(moduleJs);

const wait = (ms = 20) => new Promise((r) => setTimeout(r, ms));
const q = (sel) => doc.querySelector(sel);
const qa = (sel) => [...doc.querySelectorAll(sel)];
const assert = (ok, msg) => {
  if (!ok) throw new Error("FAIL: " + msg);
  console.log("  ok   " + msg);
};

(async () => {
  assert(html.includes("codex-reset.css") && html.includes("codex-reset.js"), "index.html 已加载新 CSS/JS");
  assert(appJs.includes("if (window.CodexResetUI) return window.CodexResetUI.render(c)"), "coreCodexResets 已委托新模块");
  assert(appJs.includes("window.CodexResetUI.renderDetail(c)"), "详情弹层已切到重置顾问子视图");
  assert(appJs.includes("window.CodexResetUI.onDetailClosed()"), "关闭详情会清掉子视图状态");
  const host = doc.getElementById("detailBody");
  host.innerHTML = w.CodexResetUI.render(w.CHANNELS[0]);
  await wait(30);
  assert(!!q("[data-codex-reset-root]"), "重置卡区有整块点击入口");
  assert(q(".cr-chip").textContent === "WAIT", "初始结论 WAIT");
  assert(calls.some((c) => c.cmd === "codex_reset_profile_get"), "进入时读一次本地 profile");
  assert(calls.some((c) => c.cmd === "codex_reset_advice"), "进入时调用一次纯函数建议");

  q("[data-codex-reset-root]").dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(30);
  assert(!!q(".cr-page"), "点击后进入详情子视图");
  assert(q(".cr-verdict-badge").textContent === "WAIT", "详情结论优先显示 WAIT");
  assert(!!q("[data-codex-reset-back]"), "详情保留回到 Codex 的入口");
  assert(qa("[data-codex-tab]").length === 3, "建议/计划/依据三个页签都在");

  q('[data-codex-need="urgent"]').dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(40);
  // probe removed
  // console.log("BADGE_AFTER_URGENT:", JSON.stringify(q(".cr-verdict-badge").textContent), "note:", JSON.stringify(q(".cr-action-note") && q(".cr-action-note").textContent));
  assert(q(".cr-verdict-badge").textContent === "NOW", "选「现在就要」翻成 NOW");
  assert(q(".cr-action-note").textContent.includes("手动兑换"), "NOW 只提示去 Codex 手动兑换");
  assert(qa("[data-codex-use]").length === 0, "没有任何自动兑换或「用一张」按钮");

  q('[data-codex-tab="plan"]').dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(20);
  assert(!!q(".cr-plan") && qa(".cr-plan li").length >= 3, "计划页有三行自然语言计划");
  assert(!!q("#codexResetSpeed"), "计划页有速度滑块");
  assert(q(".cr-card-list").textContent.includes("Full reset"), "计划页列出每张卡");

  const slider = q("#codexResetSpeed");
  slider.value = "6";
  slider.dispatchEvent(new w.Event("change", { bubbles: true }));
  await wait(35);
  const lastAdvice = calls.filter((c) => c.cmd === "codex_reset_advice").pop();
  assert(lastAdvice && lastAdvice.args.input.planBurnPercentPerDay === 30, "滑块把 30%/天传给纯函数");

  q('[data-codex-tab="evidence"]').dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(20);
  assert(!!q(".cr-path") && qa(".cr-path li").length === 5, "依据页有 0→4 的判定路径");
  assert(q(".cr-path").textContent.includes("等得到"), "依据页显示能不能等到自然重置");

  q("[data-codex-reset-back]").dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(20);
  assert(!!q("[data-codex-reset-root]"), "返回后回到重置卡区");
  assert(!q(".cr-page"), "返回后详情子视图收起");

  q("[data-codex-reset-root]").dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(20);
  const date = q("#codexResetSubEnd");
  assert(!!date, "建议页有订阅到期日输入");
  date.value = "2026-11-30";
  date.dispatchEvent(new w.Event("change", { bubbles: true }));
  await wait(40);
  assert(calls.some((c) => c.cmd === "codex_reset_profile_set" && c.args.profile.subEndAt), "日期变更写 profile");
  assert(toasts.length === 0, "保存成功不弹错误 toast");

  const savedDate = q("#codexResetSubEnd").value;
  failSave = true;
  q("#codexResetSubEnd").value = "2026-12-15";
  q("#codexResetSubEnd").dispatchEvent(new w.Event("change", { bubbles: true }));
  await wait(40);
  assert(q("#codexResetSubEnd").value === savedDate, "写失败恢复已保存日期");
  assert(toasts.some((t) => t.bad && t.msg.includes("保存订阅到期日失败")), "写失败有用户可见反馈");
  failSave = false;
  failRead = true;
  const retry = doc.querySelector('[data-codex-tab="plan"]');
  retry.dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  q("#codexResetSpeed").dispatchEvent(new w.Event("change", { bubbles: true }));
  await wait(40);
  const fallback = calls.filter((c) => c.cmd === "codex_reset_advice").pop();
  assert(fallback.args.input.subEndAt === null && fallback.args.input.need.kind === "patient", "读取失败清掉旧设置并降级为能等");
  assert(typeof tick === "function", "注册每分钟重新计算的时钟");

  assert(!calls.some((c) => /redeem|reset_credit/i.test(c.cmd)), "全程没有兑换或读卡命令");
  console.log("Codex 重置顾问前端 PASS");
  dom.window.close();
})().catch((e) => {
  console.error(e.stack || e);
  process.exit(1);
});
