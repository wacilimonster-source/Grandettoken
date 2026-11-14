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
} else if (process.env.CODEX_RESET_DATE_SENTINEL === "1") {
  const marker = 'return state.open && !state.saving && document.activeElement?.id === "codexResetSubEnd";';
  if (moduleJs.split(marker).length !== 2) throw new Error("日期保护哨兵必须命中唯一入口");
  moduleJs = moduleJs.replace(marker, "return false;");
} else {
  const { spawnSync } = require("child_process");
  const probe = spawnSync(process.execPath, [__filename], {
    env: { ...process.env, CODEX_RESET_SENTINEL: "1" }, encoding: "utf8", timeout: 15000,
  });
  if (probe.status !== 1 || !probe.stderr.includes("FAIL: 初始结论 WAIT")) {
    throw new Error("重置顾问冒烟自检失败，未知命令未触发预期断言");
  }
  console.log("  ok   注入未知建议命令时冒烟变红");
  const dateProbe = spawnSync(process.execPath, [__filename], {
    env: { ...process.env, CODEX_RESET_DATE_SENTINEL: "1" }, encoding: "utf8", timeout: 15000,
  });
  if (dateProbe.status !== 1 || !dateProbe.stderr.includes("FAIL: 日期编辑期间时钟刷新")) {
    throw new Error("日期保护自检失败，移除保护后必须被回归断言拦截");
  }
  console.log("  ok   移除日期编辑保护时冒烟变红");
}

const dom = new JSDOM(html, {
  runScripts: "outside-only",
  pretendToBeVisual: true,
  url: "http://localhost/",
});
const w = dom.window;
const doc = w.document;
doc.getElementById("detailShade").hidden = false;
const calls = [];
let profileNeed = "patient";
let profileAmount = null;
// 夹具时间一律相对本机时钟:需求新鲜度(12h)/过期标记/重置点都按真实 now 推,
// 钉死绝对时间戳会在跨天/12h 后把 NOW 误降级成 WAIT(2026-11-14 实测)
const FIXTURE_NOW = Math.floor(Date.now() / 1000);
let profileNeedAt = FIXTURE_NOW;
let uiNow = FIXTURE_NOW;
w.Date.now = () => uiNow * 1000;
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
  extra: [["套餐", "plus"]],
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

let fixtureGapH = 5.26; // 时长口径夹具:改这里观察 fmtGap 的分钟/小时/天折算

function adviceFor(kind) {
  const now = kind === "urgent";
  const inventory = w.CHANNELS[0].resetCredits;
  const validExpiries = inventory.credits.map((c) => c.expiresAt).filter((t) => t > uiNow)
    .sort((a, b) => a - b).slice(0, inventory.availableCount);
  const cardEnd = inventory.error ? null : validExpiries.pop() || null;
  const planEnd = profileSubEnd || cardEnd;
  return {
    verdict: now ? "now" : "wait",
    reasonCode: now ? "deadline_before_reset" : "gap_too_short",
    headline: now ? "这次要用，就用一张重置卡" : "先等约 5.3 小时的自然重置",
    detail: now ? "周额度已用完，下次自然重置要等约 5.3 小时。" : "自然重置很快就到，现在用卡只能换回约 3.3%。",
    metrics: {
      weeklyRemainPercent: 0,
      weeklyUsedPercent: 100,
      nextResetAt: FIXTURE_NOW + 18936,
      gapHours: fixtureGapH,
      cardsAvailable: 3,
      cardsUnknown: false,
      cardsSource: "live",
      canWait: profileSubEnd == null ? null : true,
      fiveHourRemainPercent: 100,
      earliestCardExpiry: FIXTURE_NOW + 13 * 86400,
      cardsExpiringBeforeReset: 0,
      allCardsExpireBeforeReset: false,
      burnPercentPerDay: 14.8,
      burnEstimated: true,
      cardGainPercent: 3.3,
      subEndAt: profileSubEnd,
      daysToSubEnd: profileSubEnd == null ? null : 16.47,
      planEndAt: planEnd,
      planEndSource: profileSubEnd ? "subscription" : cardEnd ? "cards" : null,
      dataUpdatedAt: FIXTURE_NOW - 60,
      planAvailable: planEnd != null,
      plannedCards: 0,
      coverCards: 3,
      shortfallPercent: planEnd == null ? null : 10.9,
    },
    planLines: planEnd == null ? ["缺少有效重置卡到期时间"] : [
      profileSubEnd ? "自然重置 3 次 · 到 11-30 还剩 16 天" : "计划至最后一张有效卡到期",
      "按 14.8%/天：省卡 0 张；到到期差 10.9% 个周窗",
      "想全程不断：补满要 3 张，但每张只换回 4%–4%，补不补都行",
    ],
    warnings: [...(now ? ["low_value_now"] : []), ...(!profileSubEnd && cardEnd ? ["subscription_unknown"] : [])],
    computedAt: FIXTURE_NOW - 60,
  };
}

w.__TAURI__ = {
  core: {
    invoke: async (cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "codex_reset_profile_get") {
        if (failRead) throw new Error("read fixture");
        return { subEndAt: profileSubEnd, need: { kind: profileNeed, amountPercent: profileAmount, at: profileNeedAt }, updatedAt: FIXTURE_NOW - 60 };
      }
      if (cmd === "codex_reset_profile_set") {
        if (failSave) throw new Error("write fixture");
        const p = (args && args.profile) || {};
        if (p.need && p.need.kind) {
          profileNeed = p.need.kind;
          profileNeedAt = p.need.at;
          profileAmount = p.need.amountPercent == null ? null : p.need.amountPercent;
        }
        if (p.subEndAt) profileSubEnd = p.subEndAt;
        if (p.clearSubEnd) profileSubEnd = null;
        return { subEndAt: profileSubEnd, need: { kind: profileNeed, amountPercent: profileAmount, at: profileNeedAt }, updatedAt: FIXTURE_NOW - 60 };
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
  assert(appJs.includes("if (resetOpen && window.CodexResetUI.isEditing()) return;"),
    "渠道刷新接线保留正在编辑的日期控件");
  assert(appJs.includes("window.CodexResetUI.onDetailClosed()"), "关闭详情会清掉子视图状态");
  const host = doc.getElementById("detailBody");
  host.innerHTML = w.CodexResetUI.render(w.CHANNELS[0]);
  await wait(30);
  assert(!!q("[data-codex-reset-root]"), "重置卡区有整块点击入口");
  assert(q(".cr-chip").textContent === "WAIT", "初始结论 WAIT");
  assert(calls.some((c) => c.cmd === "codex_reset_profile_get"), "进入时读一次本地 profile");
  assert(calls.some((c) => c.cmd === "codex_reset_advice"), "进入时调用一次纯函数建议");
  assert(calls.filter((c) => c.cmd === "codex_reset_advice").pop().args.input.planType === "plus",
    "当前套餐传入判定以核对重置涵盖范围");

  q("[data-codex-reset-root]").dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(30);
  assert(!!q(".cr-page"), "点击后进入详情子视图");
  assert(q(".cr-verdict-badge").textContent === "WAIT", "详情结论优先显示 WAIT");
  assert(!!q("[data-codex-reset-back]"), "详情保留回到 Codex 的入口");
  assert(qa("[data-codex-tab]").length === 3, "建议/计划/依据三个页签都在");

  // 时长口径(与后端 format_gap_hours 同步):不足一天 0.1 小时精度,≥24h 折「N 天 M 小时」
  assert(q(".cr-metrics").textContent.includes("约 5.3 小时"),
    "指标行时长:不足一天保留 0.1 小时精度");
  fixtureGapH = 146.4;
  uiNow += 61; // 建议缓存含时钟,只改夹具不推时钟不会重算
  tick();
  await wait(40);
  assert(q(".cr-metrics").textContent.includes("约 6 天 2 小时")
    && !q(".cr-metrics").textContent.includes("146.4"),
    "指标行时长:≥24 小时折成「N 天 M 小时」");
  fixtureGapH = 5.26;
  uiNow += 61;
  tick();
  await wait(40);

  q('[data-codex-need="urgent"]').dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(40);
  assert(q(".cr-verdict-badge").textContent === "NOW", "选「现在就要」翻成 NOW");
  assert(q(".cr-action-note").textContent.includes("手动兑换"), "NOW 只提示去 Codex 手动兑换");
  assert(qa("[data-codex-use]").length === 0, "没有任何自动兑换或「用一张」按钮");
  assert(!!q(".cr-amts"), "选 urgent 后展开用量档位行");
  assert(qa("[data-codex-amt]").length === 3, "三个用量档位都在");
  q('[data-codex-amt="50"]').dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(40);
  assert(calls.some((c) => c.cmd === "codex_reset_profile_set" && c.args.profile.need
    && c.args.profile.need.amountPercent === 50), "用量档位写进 profile");
  const amtCall = calls.filter((c) => c.cmd === "codex_reset_advice").pop();
  assert(amtCall && amtCall.args.input.need.amountPercent === 50, "用量随建议请求传给纯函数");
  assert(!!q('.cr-amt.on[data-codex-amt="50"]'), "选中的用量档位高亮");

  q('[data-codex-need="soon"]').dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(40);
  const soonCall = calls.filter((c) => c.cmd === "codex_reset_advice").pop();
  assert(soonCall.args.input.need.kind === "soon" && soonCall.args.input.need.amountPercent === 50,
    "今天要用携带所选用量请求建议");
  failSave = true;
  q('[data-codex-amt="25"]').dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(40);
  assert(!!q('.cr-amt.on[data-codex-amt="50"]'), "用量保存失败回滚原档位");
  assert(toasts.some((t) => t.bad && t.msg.includes("保存需求失败")), "用量保存失败有用户可见反馈");
  failSave = false;
  toasts = [];

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
  await wait(40);
  const date = q("#codexResetSubEnd");
  assert(!!date, "建议页有订阅到期日输入");
  date.focus();
  assert(doc.activeElement === date, "日期输入可获得编辑焦点");
  date.value = "2026-11-30";
  uiNow += 61;
  tick();
  await wait(40);
  assert(q("#codexResetSubEnd") === date && doc.activeElement === date
    && date.value === "2026-11-30", "日期编辑期间时钟刷新保留输入节点、焦点和草稿");
  date.value = "2026-11-30";
  date.dispatchEvent(new w.Event("change", { bubbles: true }));
  await wait(40);
  assert(calls.some((c) => c.cmd === "codex_reset_profile_set" && c.args.profile.subEndAt), "日期变更写 profile");
  assert(toasts.length === 0, "保存成功不弹错误 toast");
  assert(!!q("[data-codex-clear-date]"), "已填写日期提供明确清空入口");

  const savedDate = q("#codexResetSubEnd").value;
  failSave = true;
  q("#codexResetSubEnd").value = "2026-12-15";
  q("#codexResetSubEnd").dispatchEvent(new w.Event("change", { bubbles: true }));
  await wait(40);
  assert(q("#codexResetSubEnd").value === savedDate, "写失败恢复已保存日期");
  assert(toasts.some((t) => t.bad && t.msg.includes("保存订阅到期日失败")), "写失败有用户可见反馈");

  q("[data-codex-clear-date]").dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(40);
  assert(q("#codexResetSubEnd").value === savedDate && profileSubEnd != null,
    "清空保存失败恢复日期并保留已保存值");
  failSave = false;
  q("[data-codex-clear-date]").dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(40);
  assert(calls.some((c) => c.cmd === "codex_reset_profile_set" && c.args.profile.clearSubEnd === true),
    "清空通过明确标志写入 profile");
  assert(q("#codexResetSubEnd").value === "" && profileSubEnd === null, "清空成功保存空日期");
  assert(q("[data-codex-clear-date]").disabled, "空日期禁用重复清空");
  const clearedAdvice = calls.filter((c) => c.cmd === "codex_reset_advice").pop();
  assert(clearedAdvice.args.input.subEndAt === null && clearedAdvice.args.input.need.kind === "soon",
    "清空后沿用需求并以空日期重算即时建议");
  assert(q(".cr-verdict-badge").textContent === "WAIT", "空日期仍展示即时建议");
  q('[data-codex-tab="plan"]').dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  assert(q(".cr-plan").textContent.includes("最后一张有效卡到期") && !!q(".cr-timeline"),
    "空日期按卡到期时间展示计划及时间轴");
  assert(q(".cr-tl-legend").textContent.includes("最后一张卡到期"), "时间轴标明卡有效期截止");

  const readsBeforeReopen = calls.filter((c) => c.cmd === "codex_reset_profile_get").length;
  w.CodexResetUI.onDetailClosed();
  w.CHANNELS[0].updatedAt += 1;
  host.innerHTML = w.CodexResetUI.render(w.CHANNELS[0]);
  await wait(40);
  q("[data-codex-reset-root]").dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  await wait(20);
  assert(calls.filter((c) => c.cmd === "codex_reset_profile_get").length > readsBeforeReopen
    && q("#codexResetSubEnd").value === "", "重新读取设置后日期仍为空");
  q("#codexResetSubEnd").value = savedDate;
  q("#codexResetSubEnd").dispatchEvent(new w.Event("change", { bubbles: true }));
  await wait(40);
  assert(q("#codexResetSubEnd").value === savedDate && profileSubEnd != null, "空日期可重新填写并保存");
  q('[data-codex-tab="plan"]').dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  assert(!!q(".cr-timeline"), "重新填写后恢复到期时间轴");
  q('[data-codex-tab="advice"]').dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  q("#codexResetSubEnd").value = "";
  q("#codexResetSubEnd").dispatchEvent(new w.Event("change", { bubbles: true }));
  await wait(40);
  assert(profileSubEnd === null && q("#codexResetSubEnd").value === "", "日期控件直接清空也能保存");
  const savedCredits = w.CHANNELS[0].resetCredits.credits;
  w.CHANNELS[0].resetCredits.credits = [{ title: "Expiring", expiresAt: uiNow + 30 }];
  tick();
  await wait(40);
  q('[data-codex-tab="plan"]').dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  assert(!!q(".cr-timeline"), "有效卡在空日期下提供计划截止");
  uiNow += 120;
  tick();
  await wait(40);
  assert(!q(".cr-timeline") && q(".cr-plan").textContent.includes("缺少有效"),
    "跨过卡到期时刻自动撤下失效计划");
  w.CHANNELS[0].resetCredits.credits = [{ title: "Unknown", expiresAt: null }];
  tick();
  await wait(40);
  assert(!q(".cr-timeline"), "未知卡到期时间不编造计划截止");
  w.CHANNELS[0].resetCredits.credits = savedCredits;
  w.CHANNELS[0].resetCredits.error = "fixture refresh failure";
  tick();
  await wait(40);
  assert(!q(".cr-timeline"), "卡读取失败不沿用旧有效期计划");
  w.CHANNELS[0].resetCredits.error = null;
  failRead = true;
  const retry = doc.querySelector('[data-codex-tab="plan"]');
  retry.dispatchEvent(new w.MouseEvent("click", { bubbles: true }));
  q("#codexResetSpeed").dispatchEvent(new w.Event("change", { bubbles: true }));
  await wait(40);
  const fallback = calls.filter((c) => c.cmd === "codex_reset_advice").pop();
  assert(fallback.args.input.subEndAt === null && fallback.args.input.need.kind === "patient"
    && fallback.args.input.need.amountPercent === null, "读取失败清掉旧设置并降级为能等");
  assert(typeof tick === "function", "注册每分钟重新计算的时钟");

  assert(!calls.some((c) => /redeem|reset_credit/i.test(c.cmd)), "全程没有兑换或读卡命令");
  console.log("Codex 重置顾问前端 PASS");
  dom.window.close();
})().catch((e) => {
  console.error(e.stack || e);
  process.exit(1);
});
