// 需 jsdom。独立验证网页提取、正常入口、账号隔离与保存失败，未知 IPC 一律抛错。
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { spawnSync } = require("node:child_process");
const { JSDOM } = require("jsdom");
const root = path.resolve(__dirname, "..");
let source = fs.readFileSync(path.join(root, "app/src/google-cloud.js"), "utf8");
if (process.env.GOOGLE_CLOUD_SENTINEL === "1") {
  source = source.replace('invoke("google_cloud_expiry_set"', 'invoke("__missing_google_cloud__"');
} else {
  const bad = spawnSync(process.execPath, [__filename], {
    env: { ...process.env, GOOGLE_CLOUD_SENTINEL: "1" }, encoding: "utf8",
  });
  assert.notEqual(bad.status, 0, "未知命令注入必须变红");
  assert.match(bad.stdout + bad.stderr, /AssertionError/, "哨兵必须因断言失败而变红");
}
function captureFixture(options = {}) {
  const headers = ["creditName", "status", "remainingValue", "creditId", "scope"];
  if (options.missing) headers.splice(2, 1);
  const markup = `<table role="treegrid" aria-busy="${!!options.loading}"><thead><tr>${headers.map(h => `<th data-column-id="${h}">${h}</th>`).join("")}</tr></thead>
    <tbody><tr role="row"><td>Free Trial</td><td>Available</td><td>$300.00</td><td>test-credit</td><td>Eligible usage</td></tr></tbody></table>${options.filtered ? "<cfc-chip>test</cfc-chip>" : ""}${options.paging ? `<button aria-label="${options.paging} page"></button>` : ""}`;
  const dom = new JSDOM(markup);
  for (const element of dom.window.document.querySelectorAll("td")) {
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
const captured = captureFixture();
assert.match(captured, /grandettoken.invalid\/google-cloud/);
const data = JSON.parse(Buffer.from(captured.split("#")[1], "base64").toString("utf8"));
assert.equal(data.rows[0].amount, "$300.00");
assert.deepEqual(Object.keys(data.rows[0]).sort(), ["amount", "id", "name", "scope", "status"]);
for (const options of [{ missing: true }, { loading: true }, { filtered: true }, { paging: "Next" }, { paging: "Previous" }]) {
  assert.doesNotMatch(captureFixture(options), /grandettoken.invalid/);
}
const html = fs.readFileSync(path.join(root, "app/src/index.html"), "utf8");
assert.match(html, /src="google-cloud.js"/);
const appSource = fs.readFileSync(path.join(root, "app/src/app.js"), "utf8");
assert.match(appSource, /GoogleCloudUI.detail\(c\)/);
assert.match(appSource, /GoogleCloudUI.records\(c\)/);
const dom = new JSDOM('<div id="root"></div>', { runScripts: "outside-only" });
const w = dom.window;
const toasts = [];
const calls = [];
let failure = false;
let listener;
const channel = { id: "google-cloud", hasKey: true, googleCloud: {
  account: "AAAAAA-BBBBBB-CCCCCC", credits: [{ id: "test-credit", name: "Free Trial", remaining: 300,
    unit: "USD", scope: "Eligible usage", expiresAt: null }],
} };
w.CHANNELS = [channel];
w.esc = value => String(value).replaceAll("&", "&amp;").replaceAll('"', "&quot;").replaceAll("<", "&lt;");
w.money = (value, unit) => `${unit} ${value.toFixed(2)}`;
w.showToast = message => toasts.push(message);
w.refresh = async () => {};
w.renderChCards = () => render();
w.listen = async (event, callback) => { assert.equal(event, "google-cloud-result"); listener = callback; };
w.invoke = async (command, args) => {
  if (!["google_cloud_login", "google_cloud_refresh", "google_cloud_disconnect", "google_cloud_expiry_set"].includes(command)) throw new Error(`Unknown IPC ${command}`);
  calls.push({ command, args });
  if (command === "google_cloud_expiry_set") render();
  if (failure) throw new Error("disk failure");
  if (command === "google_cloud_expiry_set") channel.googleCloud.credits[0].expiresAt = args.at;
};
w.eval(source);
const box = w.document.getElementById("root");
const render = () => { box.innerHTML = w.GoogleCloudUI.detail(channel); };
const settle = () => new Promise(resolve => setImmediate(resolve));
async function click(action) {
  box.querySelector(`[data-gcp-action="${action}"]`).click();
  await settle();
}
(async () => {
  render();
  assert.match(box.textContent, /到期时间未填/);
  await click("login"); assert.equal(calls.at(-1).command, "google_cloud_login");
  await click("refresh"); assert.equal(calls.at(-1).command, "google_cloud_refresh");
  const input = box.querySelector("input");
  input.value = "2027-02-12T23:59";
  input.dispatchEvent(new w.Event("input", { bubbles: true }));
  render();
  assert.equal(box.querySelector("input").value, "2027-02-12T23:59", "轮询重绘必须保留草稿");
  failure = true;
  await click("expiry");
  assert.equal(channel.googleCloud.credits[0].expiresAt, null, "写失败不得伪装保存成功");
  assert.match(toasts.at(-1), /失败/);
  render(); assert.equal(box.querySelector("input").value, "2027-02-12T23:59");
  failure = false;
  await click("expiry");
  assert.ok(channel.googleCloud.credits[0].expiresAt > 0, "正常保存必须落到数据");
  assert.equal(box.querySelector("input").disabled, false, "保存完成后的新控件必须可编辑");
  channel.googleCloud.account = "DDDDDD-EEEEEE-FFFFFF";
  channel.googleCloud.credits = [{ ...channel.googleCloud.credits[0], expiresAt: null }];
  const savedCalls = calls.length;
  await click("expiry");
  assert.equal(calls.length, savedCalls, "换号后旧 DOM 的保存请求必须被拒绝");
  render();
  assert.notEqual(box.querySelector("input").value, "2027-02-12T23:59");
  channel.googleCloud.credits[0].expiresAt = 1;
  assert.match(w.GoogleCloudUI.rail(channel).left, /已到期/);
  render();
  box.querySelector("input").value = "";
  await click("expiry");
  assert.equal(channel.googleCloud.credits[0].expiresAt, null, "允许清除日期");
  await listener({ payload: { error: "test refresh failure" } });
  assert.equal(toasts.at(-1), "test refresh failure");
  await click("disconnect"); assert.equal(calls.at(-1).command, "google_cloud_disconnect");
  console.log("Google Cloud 冒烟 PASS（含未知命令变红自检）");
  dom.window.close();
})().catch(error => { console.error(error); process.exit(1); });
