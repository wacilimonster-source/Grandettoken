// 需 jsdom（npm install jsdom --no-save）。只使用合成数据，未知命令必须失败。
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { JSDOM } = require("jsdom");

const root = path.resolve(__dirname, "..");
const html = fs.readFileSync(path.join(root, "app/src/index.html"), "utf8");
let source = fs.readFileSync(path.join(root, "app/src/vault-view.js"), "utf8");
assert.match(html, /href="vault-view\.css"/);
assert.ok(html.indexOf('src="vault-view.js"') >= 0 && html.indexOf('src="vault-view.js"') < html.indexOf('src="app.js"'));

if (process.env.VAULT_SENTINEL === "1") {
  const token = 'invoke("vault_reveal"';
  assert.equal(source.split(token).length, 2, "哨兵必须命中唯一的显示命令");
  source = source.replace(token, 'invoke("__unknown_vault_command__"');
} else {
  const probe = spawnSync(process.execPath, [__filename], {
    env: { ...process.env, VAULT_SENTINEL: "1" }, encoding: "utf8", timeout: 15000,
  });
  assert.equal(probe.status, 1, "注入未知命令必须使冒烟变红");
  assert.match(probe.stderr, /明文按需显示/, "哨兵必须命中显示结果断言");
}

const dom = new JSDOM('<div id="vaultForm"></div><div id="vaultMeta"></div><button id="vaultQClear"></button><div id="vaultList"></div>', {
  runScripts: "outside-only", pretendToBeVisual: true, url: "http://localhost/",
});
const w = dom.window;
let nextTimer = 0;
const timers = new Map();
w.setTimeout = (callback, ms) => { const id = ++nextTimer; timers.set(id, { callback, ms }); return id; };
w.clearTimeout = (id) => timers.delete(id);
w.eval(source);
const ui = w.VaultUI;
const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[ch]));
const items = [
  { id: "one", platform: "Example", label: "工作", hint: "••••1234", note: "<img src=x>", apiEndpoint: "https://example.com/v1", website: "https://example.com", createdAt: 10, updatedAt: 20 },
  { id: "two", platform: "Example", label: "渠道默认密钥", hint: "", defaultChannel: "channel", defaultAvailable: false, createdAt: 10, updatedAt: 10 },
];
const state = { items, rows: items, query: "", error: "", edit: null, draft: null, platformHints: [],
  refsOf: (id) => id === "one" ? ["channel"] : [], channelName: () => "示例渠道", esc };
const render = () => ui.render(state);
const find = (action, id) => [...w.document.querySelectorAll(`[data-act="${action}"]`)].find((node) => node.dataset.id === id);
const toasts = [];
const toast = (text) => toasts.push(text);
const invoke = async (command) => {
  if (command !== "vault_reveal") throw new Error(`未知命令 ${command}`);
  return "示例明文";
};

async function run() {
  render();
  assert.equal(w.document.querySelectorAll('[data-act="vfold"][aria-expanded="false"]').length, items.length);
  assert.equal(w.document.querySelector(".vrow-address"), null, "默认不展示详情");
  assert.equal(w.document.querySelector("#vaultList img"), null, "账号文字必须转义");
  assert.match(w.document.querySelector("#vaultList").textContent, /正被 示例渠道 使用/);
  assert.match(w.document.querySelector("#vaultList").textContent, /渠道默认 · 只读/);
  assert.match(w.document.querySelector("#vaultList").textContent, /渠道未配置密钥/);
  assert.equal(find("vcopy", "two").disabled, true);
  assert.equal(find("vcopy-endpoint", "two").disabled, true);

  ui.toggle("one", render);
  assert.equal(find("vfold", "one").getAttribute("aria-expanded"), "true");
  assert.equal(w.document.activeElement, find("vfold", "one"), "展开后保留键盘焦点");
  assert.match(w.document.querySelector(".vrow-address").textContent, /https:\/\/example.com\/v1/);
  await ui.reveal("one", invoke, render, toast);
  assert.equal(w.document.querySelector(".vrow-reveal")?.textContent, "示例明文", "明文按需显示");
  assert.equal(w.document.activeElement, find("vreveal", "one"), "显示后保留键盘焦点");
  assert.equal(timers.size, 1);
  const timer = [...timers.values()][0];
  assert.equal(timer.ms, 8000, "明文必须在八秒后隐藏");
  timer.callback();
  assert.equal(w.document.querySelector(".vrow-reveal"), null);

  let finish;
  const delayed = () => new Promise((resolve) => { finish = resolve; });
  const pending = ui.reveal("one", delayed, render, toast);
  ui.toggle("two", render);
  finish("迟到的内容");
  await pending;
  assert.equal(w.document.querySelector(".vrow-reveal"), null, "切换条目后迟到内容不可出现");
  assert.equal(w.document.querySelectorAll('[data-act="vfold"][aria-expanded="true"]').length, 1);
  assert.equal(find("vreveal", "two").disabled, true);
  assert.equal(find("vedit", "two"), undefined);
  assert.equal(find("vdel", "two"), undefined);

  ui.toggle("one", render);
  const leaving = ui.reveal("one", delayed, render, toast);
  ui.reset();
  render();
  ui.toggle("one", render);
  finish("离开后迟到的内容");
  await leaving;
  assert.equal(w.document.querySelector(".vrow-reveal"), null, "离开后即使重新展开也不能显示旧请求");

  const searching = ui.reveal("one", delayed, render, toast);
  ui.hideReveal();
  render();
  finish("搜索后迟到的内容");
  await searching;
  assert.equal(w.document.querySelector(".vrow-reveal"), null, "搜索时作废在途明文");

  ui.beginForm();
  state.edit = { ...items[0], key: "" };
  render();
  let extra = w.document.querySelector(".vform-extra");
  assert.equal(extra.open, false);
  assert.match(extra.querySelector("summary").textContent, /已填写/);
  assert.equal(w.document.getElementById("vEndpoint").value, items[0].apiEndpoint);
  extra.open = true;
  state.draft = { ...state.edit, note: "草稿账号" };
  render();
  extra = w.document.querySelector(".vform-extra");
  assert.equal(extra.open, true);
  assert.equal(w.document.getElementById("vNote").value, "草稿账号");
  assert.equal(w.document.getElementById("vWebsite").value, items[0].website);
  console.log("密钥箱交互 PASS（含未知命令变红自检、八秒隐藏与迟到请求）");
  dom.window.close();
}
run().catch((error) => { console.error(error); process.exitCode = 1; dom.window.close(); });
