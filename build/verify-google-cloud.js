// 需 jsdom。独立验证网页提取、正常入口、方案 A 批次展示与未知 IPC 拒绝。
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { spawnSync } = require("node:child_process");
const { JSDOM } = require("jsdom");
const root = path.resolve(__dirname, "..");
const { apiSource, replaceOnce } = require("./lib/app-sources");
const source = fs.readFileSync(path.join(root, "app/src/google-cloud.js"), "utf8");
let api = apiSource();
if (process.env.GOOGLE_CLOUD_SENTINEL === "1") {
  api = replaceOnce(api, 'invoke("google_cloud_refresh"', 'invoke("__missing_google_cloud__"');
} else {
  const bad = spawnSync(process.execPath, [__filename], {
    env: { ...process.env, GOOGLE_CLOUD_SENTINEL: "1" }, encoding: "utf8",
  });
  assert.notEqual(bad.status, 0, "未知命令注入必须变红");
  assert.match(bad.stdout + bad.stderr, /AssertionError/, "哨兵必须因断言失败而变红");
}

function captureFixture(options = {}) {
  const endHeader = options.endHeader !== undefined
    ? options.endHeader
    : { id: "endDate", text: "End date", cell: "Feb 12, 2027" };
  const headers = [
    { id: "creditName", text: "Credit name", cell: "Free Trial" },
    { id: "status", text: "Status", cell: "Available" },
    { id: "remainingValue", text: "Remaining value", cell: "$300.00" },
    { id: "creditId", text: "Credit ID", cell: "test-credit" },
    { id: "scope", text: "Scope", cell: "Eligible usage" },
  ];
  if (options.missing) headers.splice(2, 1);
  if (endHeader) headers.push(endHeader);
  const ths = headers.map(h => `<th${h.id ? ` data-column-id="${h.id}"` : ""}>${h.text}</th>`).join("");
  const tds = headers.map(h => `<td>${h.cell}</td>`).join("");
  const markup = `<table role="treegrid" aria-busy="${!!options.loading}"><thead><tr>${ths}</tr></thead>
    <tbody><tr role="row">${tds}</tr></tbody></table>${options.filtered ? "<cfc-chip>test</cfc-chip>" : ""}${options.paging ? `<button aria-label="${options.paging} page"></button>` : ""}`;
  const dom = new JSDOM(markup);
  for (const element of dom.window.document.querySelectorAll("th, td")) {
    Object.defineProperty(element, "innerText", { get() { return this.textContent; } });
  }
  let tick;
  let time = 0;
  const location = { hostname: "console.cloud.google.com", pathname: "/billing/AAAAAA-BBBBBB-CCCCCC/credits/all", href: "https://console.cloud.google.com/billing/AAAAAA-BBBBBB-CCCCCC/credits/all?hl=en" };
  const window = {}; window.top = window;
  class Clock extends Date { static now() { return time; } }
  vm.runInNewContext(fs.readFileSync(path.join(root, "app/src-tauri/src/google_cloud_capture.js"), "utf8"), {
    window, location, document: dom.window.document, URL, Date: Clock, TextEncoder,
    btoa: value => Buffer.from(value, "binary").toString("base64"),
    setInterval: callback => { tick = callback; return 1; }, clearInterval() {},
  });
  tick(); time = 3000; tick();
  return location.href;
}

function parseCaptured(href) {
  assert.match(href, /grandettoken.invalid\/google-cloud/);
  return JSON.parse(Buffer.from(href.split("#")[1], "base64").toString("utf8"));
}

const data = parseCaptured(captureFixture());
assert.equal(data.rows[0].amount, "$300.00");
assert.equal(data.rows[0].endDate, "Feb 12, 2027");
assert.deepEqual(Object.keys(data.rows[0]).sort(), ["amount", "endDate", "id", "name", "scope", "status"]);

// 表头文本回退：英文与中文换行文本；缺失结束日期列时仍正常返回余额且 endDate 为空。
const byEnHeader = parseCaptured(captureFixture({ endHeader: { id: null, text: "End date", cell: "Nov 24, 2027" } }));
assert.equal(byEnHeader.rows[0].endDate, "Nov 24, 2027");
const byZhHeader = parseCaptured(captureFixture({ endHeader: { id: null, text: "结束日期", cell: "2027年2月12\n日" } }));
assert.equal(byZhHeader.rows[0].endDate, "2027年2月12\n日");
const noEndCol = parseCaptured(captureFixture({ endHeader: null }));
assert.equal(noEndCol.rows[0].amount, "$300.00");
assert.equal(noEndCol.rows[0].endDate, "");

for (const options of [{ missing: true }, { loading: true }, { filtered: true }, { paging: "Next" }, { paging: "Previous" }]) {
  assert.doesNotMatch(captureFixture(options), /grandettoken.invalid/);
}

const html = fs.readFileSync(path.join(root, "app/src/index.html"), "utf8");
assert.match(html, /src="google-cloud.js"/);
const { appSource } = require("./lib/app-sources");
const appJs = appSource();
assert.match(appJs, /GoogleCloudUI.detail\(c\)/);
assert.match(appJs, /GoogleCloudUI.records\(c\)/);

const dom = new JSDOM('<div id="root"></div>', { runScripts: "outside-only" });
const w = dom.window;
const RealDate = w.Date;
let fixedNow = new RealDate(2026, 10, 24, 18, 0, 0).getTime();
w.Date = class extends RealDate {
  constructor(...args) { super(...(args.length ? args : [fixedNow])); }
  static now() { return fixedNow; }
};

const toasts = [];
const calls = [];
let listener;
const channel = {
  id: "google-cloud",
  hasKey: true,
  unit: "USD",
  googleCloud: {
    account: "AAAAAA-BBBBBB-CCCCCC",
    credits: [
      { id: "c-welcome", name: "Welcome credit", remaining: 275.62, unit: "USD", scope: "Free trial", expiresDate: "2027-02-12" },
      { id: "c-monthly", name: "Monthly credit", remaining: 10, unit: "USD", scope: "All services", expiresDate: "2027-11-24" },
    ],
  },
};
w.CHANNELS = [channel];
w.esc = value => String(value).replaceAll("&", "&amp;").replaceAll('"', "&quot;").replaceAll("<", "&lt;");
w.money = (value, unit) => `${unit} ${Number(value).toFixed(2)}`;
w.showToast = message => toasts.push(message);
w.refresh = async () => {};
w.renderChCards = () => render();
w.listen = async (event, callback) => { assert.equal(event, "google-cloud-result"); listener = callback; };
w.invoke = async (command, args) => {
  if (!["google_cloud_login", "google_cloud_refresh", "google_cloud_disconnect"].includes(command)) {
    throw new Error(`Unknown IPC ${command}`);
  }
  calls.push({ command, args });
};
w.__TAURI__ = { ...w.__TAURI__, core: { invoke: w.invoke } };
w.eval(api);
w.eval(source);
const box = w.document.getElementById("root");
const render = () => { box.innerHTML = w.GoogleCloudUI.detail(channel); };
const settle = () => new Promise(resolve => setImmediate(resolve));
async function click(action) {
  box.querySelector(`[data-gcp-action="${action}"]`).click();
  await settle();
}

const plain = v => ({ ...v });

(async () => {
  render();
  assert.equal(box.querySelectorAll("[data-gcp-date]").length, 0, "不再渲染手填日期输入框");
  assert.equal(box.querySelectorAll('[data-gcp-action="expiry"]').length, 0, "不再渲染保存到期按钮");
  assert.match(box.textContent, /网页自动读取/);
  assert.match(box.textContent, /2027-02-12 到期 · 剩余 80 天/);

  // 方案 A：两笔不同到期日，只显示最早批次金额与日期，标准卡附带总笔数。
  assert.deepEqual(plain(w.GoogleCloudUI.rail(channel, true)), {
    left: "USD 275.62 将到期", right: "2027-02-12", warn: false,
  });
  assert.deepEqual(plain(w.GoogleCloudUI.rail(channel, false)), {
    left: "最近 2027-02-12 到期 · USD 275.62", right: "剩余 80 天 · 共 2 笔", warn: false,
  });

  // 同日多笔自动合并该日批次金额。
  channel.googleCloud.credits = [
    { id: "c1", name: "A", remaining: 100, unit: "USD", scope: "s", expiresDate: "2027-02-12" },
    { id: "c2", name: "B", remaining: 50, unit: "USD", scope: "s", expiresDate: "2027-02-12" },
    { id: "c3", name: "C", remaining: 10, unit: "USD", scope: "s", expiresDate: "2027-11-24" },
  ];
  assert.equal(w.GoogleCloudUI.rail(channel, true).left, "USD 150.00 将到期");
  assert.equal(w.GoogleCloudUI.rail(channel, false).right, "剩余 80 天 · 共 3 笔");

  // 部分有日期 + 部分无日期：缩略卡仍给出确定金额与日期，标准卡注明未知笔数。
  channel.googleCloud.credits = [
    { id: "c1", name: "A", remaining: 275.62, unit: "USD", scope: "s", expiresDate: "2027-02-12" },
    { id: "c2", name: "B", remaining: 10, unit: "USD", scope: "s", expiresDate: null },
  ];
  assert.deepEqual(plain(w.GoogleCloudUI.rail(channel, true)), {
    left: "USD 275.62 将到期", right: "2027-02-12", warn: false,
  });
  assert.deepEqual(plain(w.GoogleCloudUI.rail(channel, false)), {
    left: "最近 2027-02-12 到期 · USD 275.62", right: "剩余 80 天 · 1 笔日期未知", warn: false,
  });

  // 7 天 / 8 天阈值、今日到期、已到期与闰日。
  channel.googleCloud.credits = [{ id: "c1", name: "A", remaining: 25, unit: "USD", scope: "s", expiresDate: "2026-12-01" }];
  assert.equal(w.GoogleCloudUI.rail(channel, true).warn, true);
  assert.equal(w.GoogleCloudUI.rail(channel, false).right, "剩余 7 天");
  channel.googleCloud.credits[0].expiresDate = "2026-12-02";
  assert.equal(w.GoogleCloudUI.rail(channel, true).warn, false);
  assert.equal(w.GoogleCloudUI.rail(channel, false).right, "剩余 8 天");
  channel.googleCloud.credits[0].expiresDate = "2026-11-24";
  assert.deepEqual(plain(w.GoogleCloudUI.rail(channel, false)), {
    left: "今日到期 · USD 25.00", right: "今日", warn: true,
  });
  channel.googleCloud.credits[0].expiresDate = "2026-11-23";
  assert.deepEqual(plain(w.GoogleCloudUI.rail(channel, true)), {
    left: "USD 25.00 已到期", right: "2026-11-23", warn: true,
  });
  assert.deepEqual(plain(w.GoogleCloudUI.rail(channel, false)), {
    left: "2026-11-23 已到期 · USD 25.00", right: "待刷新核对", warn: true,
  });
  fixedNow = new RealDate(2028, 1, 28, 12, 0, 0).getTime();
  channel.googleCloud.credits[0].expiresDate = "2028-02-29";
  assert.equal(w.GoogleCloudUI.rail(channel, false).right, "剩余 1 天");

  // 全部无日期。
  channel.googleCloud.credits[0].expiresDate = null;
  assert.deepEqual(plain(w.GoogleCloudUI.rail(channel, true)), { left: "到期日期未提供", right: "", warn: false });
  assert.deepEqual(plain(w.GoogleCloudUI.rail(channel, false)), { left: "赠金到期日期暂未提供", right: "", warn: false });

  await click("login"); assert.equal(calls.at(-1).command, "google_cloud_login");
  assert.match(toasts.at(-1), /显示全部记录和「结束日期」列/);
  await click("refresh"); assert.equal(calls.at(-1).command, "google_cloud_refresh");
  await listener({ payload: { ok: true, interactive: true } });
  assert.equal(toasts.at(-1), "Google 赠金已连接，赠金记录已更新");
  await listener({ payload: { error: "test refresh failure" } });
  assert.equal(toasts.at(-1), "test refresh failure");
  await click("disconnect"); assert.equal(calls.at(-1).command, "google_cloud_disconnect");
  console.log("Google Cloud 冒烟 PASS（含未知命令变红自检）");
  dom.window.close();
})().catch(error => { console.error(error); process.exit(1); });
