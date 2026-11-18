// 零依赖：从真实前端模块验证账户到期、逐笔积分与本地自然日边界。
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { spawnSync } = require("node:child_process");

if (!process.argv.includes("--run")) {
  for (const tz of ["Asia/Shanghai", "America/New_York"]) {
    const run = spawnSync(process.execPath, [__filename, "--run"], { env: { ...process.env, TZ: tz }, encoding: "utf8" });
    if (run.status !== 0) throw new Error(`${tz} 临期验证失败\n${run.stdout}\n${run.stderr}`);
    console.log(`${tz}: ${run.stdout.trim()}`);
  }
  const bad = spawnSync(process.execPath, [__filename, "--run", "--sentinel"],
    { env: { ...process.env, TZ: "Asia/Shanghai" }, encoding: "utf8" });
  if (bad.status === 0 || !bad.stderr.includes("账户临期应进入筛选")) {
    throw new Error("哨兵自检失败：漏掉账户到期时必须由对应断言报错");
  }
  console.log("PASS 账户临期漏筛坏样本被拦截");
  const stale = spawnSync(process.execPath, [__filename, "--run", "--stale-sentinel"],
    { env: { ...process.env, TZ: "Asia/Shanghai" }, encoding: "utf8" });
  if (stale.status === 0 || !stale.stderr.includes("旧渠道快照不能作为当前已确认积分")) {
    throw new Error("哨兵自检失败：把旧明细计入确认金额必须被断言拦截");
  }
  console.log("PASS 旧明细冒充当前数据的坏样本被拦截");
  const dst = spawnSync(process.execPath, [__filename, "--run", "--dst-sentinel"],
    { env: { ...process.env, TZ: "America/New_York" }, encoding: "utf8" });
  if (dst.status === 0 || !dst.stderr.includes("自然日边界不可按固定小时换算")) {
    throw new Error("哨兵自检失败：把自然日换成固定小时必须被夏令时断言拦截");
  }
  console.log("PASS 固定小时替代自然日的坏样本被拦截");
  process.exit(0);
}

let source = fs.readFileSync(path.join(__dirname, "../app/src/expiry-ui.js"), "utf8");
if (process.argv.includes("--sentinel")) {
  const marker = "if (account !== null) dates.push(account);";
  assert.equal(source.split(marker).length, 2, "哨兵必须命中唯一账户筛选入口");
  source = source.replace(marker, "");
}
if (process.argv.includes("--stale-sentinel")) {
  const marker = "if (channel.stale || channel.expiryStale) return [];";
  assert.equal(source.split(marker).length, 2, "哨兵必须命中唯一旧明细判断");
  source = source.replace(marker, "");
}
if (process.argv.includes("--dst-sentinel")) {
  const marker = "end.setDate(end.getDate() + days);";
  assert.equal(source.split(marker).length, 2, "哨兵必须命中唯一本地自然日边界");
  source = source.replace(marker, "end.setTime(end.getTime() + days * 86400000);");
}
const window = {};
vm.runInNewContext(source, { window, Date, Number, Math, Set });
const ui = window.ExpiryUI;
const date = (y, m, d, h = 0, min = 0) => new Date(y, m - 1, d, h, min).getTime();
const now = date(2026, 11, 18, 12);
const qoder = { id: "qoder-cn", hasKey: true, valid: true, hidden: false, kind: "points",
  remaining: 100, expiryComplete: false, expiring: [],
  accountExpiry: { state: "dated", at: date(2026, 11, 22, 0, 31) / 1000 } };
assert.equal(ui.nearest(qoder, 7, now), qoder.accountExpiry.at, "账户临期应进入筛选");
let summary = ui.summary([qoder], 7, now);
assert.equal(summary.hasConfirmed, true);
assert.equal(summary.total, 0, "账户到期不能折算为积分金额");
assert.equal(summary.accounts.length, 1);
assert.equal(summary.unknown.length, 1, "账户有时间不等于逐笔明细完整");
assert.equal(summary.first.type, "account");

const entries = [
  { at: date(2026, 11, 18, 11) / 1000, amount: 90 },
  { at: date(2026, 11, 18, 13) / 1000, amount: 10 },
  { at: date(2026, 11, 24, 23, 59) / 1000, amount: 20 },
  { at: date(2026, 11, 25) / 1000, amount: 30 },
  { at: date(2026, 11, 20) / 1000, amount: -100 },
  { at: date(2026, 11, 20) / 1000, amount: "bad" },
];
const points = { ...qoder, id: "workbuddy", expiryComplete: true, expiring: entries, accountExpiry: null };
summary = ui.summary([qoder, points, { ...qoder, id: "qoder", hidden: true },
  { ...qoder, id: "qoder", valid: false }, { ...qoder, id: "qoder", hasKey: false }], 7, now);
assert.equal(summary.total, 30, "只计未来七个自然日内的正数逐笔金额");
assert.equal(summary.entries.length, 2);
assert.equal(summary.accounts.length, 1);
assert.equal(summary.first.type, "points");
assert.equal(ui.nearest(points, 7, now), entries[1].at);
const stalePoints = ui.summary([{ ...points, stale: true }], 7, now);
assert.equal(stalePoints.total, 0, "旧渠道快照不能作为当前已确认积分");
assert.equal(stalePoints.unknown.length, 1);
assert.equal(ui.summary([{ ...points, expiryStale: true }], 7, now).total, 0);
assert.equal(ui.nearest({ ...qoder, accountExpiry: { state: "never" } }, 7, now), null);
assert.equal(ui.nearest({ ...qoder, accountExpiry: { state: "dated", at: now / 1000 } }, 7, now), null);
assert.equal(ui.nearest({ ...qoder, accountExpiry: { state: "dated", at: date(2026, 11, 25) / 1000 } }, 7, now), null);
assert.equal(ui.nextEvent({ ...qoder, expiring: [{ at: date(2026, 11, 23) / 1000, amount: 1 }] }, now).type, "account");
assert.equal(ui.nextEvent({ ...qoder, expiring: [{ at: date(2026, 11, 19) / 1000, amount: 1 }] }, now).type, "points");
const oldPacks = { ...qoder, expiryComplete: false, expiryStale: true,
  expiring: [{ at: date(2026, 11, 19) / 1000, amount: 500 }], extra: [] };
const oldSummary = ui.summary([oldPacks], 7, now);
assert.equal(oldSummary.total, 0, "旧明细不能作为当前已确认积分");
assert.equal(oldSummary.unknown.length, 1);
assert.equal(ui.nearest(oldPacks, 7, now), qoder.accountExpiry.at, "账户到期独立于网页明细");
assert.equal(ui.nextEvent(oldPacks, now).type, "account");
assert.equal(ui.nearest({ ...qoder, stale: true, expiring: oldPacks.expiring }, 7, now), null);

for (const [y, m, d] of [[2026, 3, 8], [2026, 11, 1]]) {
  const start = date(y, m, d, 1, 30);
  const end = new Date(start); end.setHours(0, 0, 0, 0); end.setDate(end.getDate() + 7);
  const last = end.getTime() / 1000 - 60;
  assert.equal(ui.dayIndex(last, start), 6, "夏令时切换的最后一天仍是第七个自然日");
  const ch = { ...points, expiring: [{ at: last, amount: 1 }, { at: end.getTime() / 1000, amount: 5 }] };
  assert.equal(ui.summary([ch], 7, start).total, 1, "自然日边界不可按固定小时换算");
}
for (const message of ["网页会话失效或未登录 · 请登录网页版", "网页已登录 · 积分明细接口返回结构未识别", "逐笔明细暂时获取失败 · 稍后自动重试"]) {
  assert.equal(ui.qoderNotice({ extra: [["逐笔到期明细", message]] }).text, message);
}
const cached = ui.qoderNotice({ id: "qoder-cn", expiryComplete: false, expiryStale: true, extra: [["逐笔到期明细", "网页会话失效 · 沿用 11-18 12:00 抓到的列表"]] });
assert.equal(cached.fresh, false, "缓存不能显示为本轮登录成功");
assert.match(cached.text, /沿用/);
assert.equal(ui.qoderNotice({ expiryComplete: true, extra: [] }).fresh, true);
assert.equal(ui.qoderNotice({ expiryComplete: true, stale: true, extra: [] }).fresh, false);
console.log("PASS 临期筛选、金额、排序、失败状态与日期边界");
