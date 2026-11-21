// Fixed clocks and synthetic quotas. All runs use the production planner and adapters.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { spawnSync } = require("node:child_process");

if (!process.argv.includes("--run")) {
  for (const tz of ["Asia/Shanghai", "America/New_York"]) {
    const run = spawnSync(process.execPath, [__filename, "--run"], {
      env: { ...process.env, TZ: tz }, encoding: "utf8",
    });
    assert.equal(run.status, 0, `${tz}\n${run.stdout}\n${run.stderr}`);
    console.log(`${tz}: ${run.stdout.trim()}`);
  }
  for (const [flag, expected] of [["--skip-today", "Codex 必须在今天提醒"], ["--ignore-short", "5 小时窗满额是独立上限"]]) {
    const bad = spawnSync(process.execPath, [__filename, "--run", flag], { encoding: "utf8" });
    assert.equal(bad.status, 1);
    assert.ok(bad.stderr.includes(expected), bad.stderr);
  }
  console.log("PASS 错过今天与跳过短窗两种坏样本被拦截");
  process.exit(0);
}

const window = {};
const source = name => fs.readFileSync(path.join(__dirname, "../app/src", name + ".js"), "utf8");
let solver = source("quota-plan");
for (const [flag, from, to] of [
  ["--skip-today", "urgent: firstDay === 0 && missingIfSkipped(1)", "urgent: false"],
  ["--ignore-short", "Math.min(s.main, s.week, s.short)", "Math.min(s.main, s.week)"],
]) {
  if (!process.argv.includes(flag)) continue;
  assert.equal(solver.split(from).length, 2);
  solver = solver.replace(from, to);
}
vm.runInNewContext(solver + source("quota-plan-data") + source("expiry-ui"), { window, Date });

const { schedule, HOUR, WEEK } = window.QuotaPlan;
const usage = window.QuotaPlanData;
const date = (day, hour, minute = 0) => new Date(2026, 10, day, hour, minute).getTime();
const now = date(21, 9);
const until = date(22, 19);
const reset = date(23, 8);
const iso = t => t && new Date(t).toISOString();
const win = (key, remainPercent, resetsAt) => ({ key, remainPercent, resetsAt: iso(resetsAt) });
const scope = "a".repeat(24);
const base = { hasKey: true, valid: true, updatedAt: now / 1000 };
const codex = { ...base, id: "codex", quotaPlanScope: scope, extra: [["套餐", "plus"]],
  windows: [win("weekly", 100, reset), win("5h", 100, null)] };
const profile = { scope, subEndAt: until / 1000 };
const make = (c, settings, account) => usage.prepare(c, settings, [], account, now, 12 * HOUR);
const amount = day => day.slots.reduce((n, s) => n + s.amount, 0);
const trial = { rate: 8, shortCapacity: 30 };

const codexModel = make(codex, trial, profile);
const plan = schedule(codexModel, now);
assert.equal(plan.urgent, true, "Codex 必须在今天提醒");
assert.equal(amount(plan.schedule[0]), 60, "5 小时窗满额是独立上限");
assert.ok(plan.latestStart >= date(21, 12, 40) && plan.latestStart <= date(21, 12, 46), "最晚开工时刻给下一短窗留下可用时间");
assert.equal(amount(plan.schedule[1]), 40);
assert.equal(plan.incomplete, false);
assert.equal(plan.schedule[0].slots.length, 2);
assert.equal(plan.schedule[0].slots[1].start, date(21, 14));
assert.equal(schedule(codexModel, date(22, 9)).incomplete, true);
assert.equal(schedule({ ...codexModel, mainAt: until }, now).incomplete, false, "到期同秒的重置属于下一期");

const go = { ...base, id: "opencode-go", quotaPlanScope: scope, windows: [
  win("monthly", 65, date(24, 19)), win("weekly", 30, reset), win("rolling", 100, null),
] };
const goModel = make(go, { rate: 10 });
const goPlan = schedule(goModel, now);
assert.equal(amount(goPlan.schedule[0]), 15, "重置前使用本周可用的月额度，百分比按不同基数换算");
assert.equal(goPlan.schedule[1].day, date(23, 0));
assert.equal(goPlan.urgent, false, "今天的 15 个点可在明天补齐");
assert.equal(schedule(goModel, date(22, 9)).urgent, true, "周重置前的最后可用日必须提醒");
assert.equal(goPlan.incomplete, false);

const gemini = { ...base, id: "antigravity:" + scope, windows: [
  win("gemini-weekly", 100, reset), win("gemini-5h", 100, null), win("claude-gpt-weekly", 0, reset),
] };
const geminiModel = make(gemini, { ...trial, end: until });
const geminiPlan = schedule(geminiModel, now);
assert.equal(geminiPlan.incomplete, false);
for (const channel of [plan, goPlan, geminiPlan]) {
  assert.equal(channel.schedule[0].slots[0].start, now, "三个渠道可同时安排在同一段在线时间");
}
assert.match(make({ ...gemini, stale: true }, { ...trial, end: until }).reason, /待刷新/);
assert.match(make({ ...gemini, windows: [gemini.windows[0]] }, { ...trial, end: until }).reason, /尚未确认/);
assert.equal(make({ ...gemini, windows: [gemini.windows[0]] }, { rate: 12, end: until, weeklyOnly: true }).shortCapacity, null);
assert.match(make(codex, trial, { ...profile, scope: "b".repeat(24) }).reason, /确认订阅到期/);
assert.match(make(codex, { rate: 8 }, profile).reason, /短窗与主窗/);
assert.equal(make({ ...codex, windows: [win("weekly", 0, reset), codex.windows[1]] }, {}, profile).exhausted, true);

const periodPlan = schedule({ ...codexModel, mainAt: date(21, 12), mainPeriod: WEEK, main: { remaining: 15 } }, now);
assert.equal(periodPlan.schedule[0].slots[0].due, date(21, 12));
assert.equal(periodPlan.schedule[0].slots[0].amount, 15, "当前周余额会在重置前安排");
assert.ok(periodPlan.schedule[0].slots.some(slot => slot.due === until), "换周后单独安排下一周额度");
const tail = schedule({ ...codexModel, short: { remaining: 100, reset: date(21, 9, 10) }, end: date(21, 19) }, now);
assert.equal(tail.schedule[0].slots[0].start, date(21, 9, 10), "可跳过只剩十分钟的旧短窗");
assert.equal(tail.schedule[0].slots.length, 2);
assert.equal(schedule({ ...codexModel, end: date(21, 9, 30) }, now).schedule[0].slots[0].end, date(21, 9, 30));
assert.equal(window.QuotaPlan.validHours({ start: "09:00", end: "19:01" }), false);

const fall = new Date(2026, 10, 1, 0).getTime();
const fallPlan = schedule({ main: { remaining: 100 }, mainAt: Infinity, mainPeriod: 0,
  end: new Date(2026, 10, 1, 12).getTime(), rate: 1 }, fall, { start: "00:00", end: "10:00" });
assert.equal(amount(fallPlan.schedule[0]), 10, "夏令时日也不得使用超过十个实际小时");

const first = usage.sample(null, usage.limits(codex, trial), now, now, window.QuotaPlan.defaultHours);
const next = { ...codex, windows: [win("weekly", 95, reset), win("5h", 80, date(21, 14))] };
const second = usage.sample(first, usage.limits(next, trial), now + HOUR / 4, now + HOUR / 4, window.QuotaPlan.defaultHours);
assert.equal(second.history.length, 1);
assert.equal(second.history[0].short, null, "不能用跨短窗的样本学习两窗比例");
const third = usage.sample(second, usage.limits({ ...next, windows: [win("weekly", 90, reset), win("5h", 60, date(21, 14))] }, trial),
  now + HOUR / 2, now + HOUR / 2, window.QuotaPlan.defaultHours);
assert.equal(third.history[1].short, 20);
assert.equal(usage.prepare(codex, {}, third.history, profile, now + HOUR / 2, 12 * HOUR).shortCapacity, 25);

const pkg = { channel: codex, entry: { at: until / 1000 }, type: "package" };
const summary = window.ExpiryUI.summary([codex], 7, now, [pkg]);
assert.equal(summary.total, 0);
assert.equal(summary.first, pkg);
assert.equal(summary.hasConfirmed, true);
console.log("PASS 三渠道并行、提前提醒、限额窗口、账号及时间边界");
