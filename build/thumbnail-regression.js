// 在真实初始化后的 jsdom 冒烟中串行调用，使用主程序的渲染与保存入口。
const fs = require("fs");
const path = require("path");
const assert = require("assert/strict");

module.exports = async function ({ w, R, data, src }) {
  const doc = w.document;
  const before = JSON.parse(JSON.stringify(w.__TEST.cfg()));
  const check = (name, action) => {
    try { action(); R.push({ name: "缩略模式 " + name, ok: true }); }
    catch (error) { R.push({ name: "缩略模式 " + name, ok: false, extra: error.message }); }
  };
  try {
    const source = fs.readFileSync(path.join(src, "../core/src/providers.rs"), "utf8");
    const table = source.slice(source.indexOf("pub const PROVIDERS:"));
    const defs = [...table.matchAll(/id:\s*"([^"]+)",\s*name:\s*"([^"]+)"/g)];
    assert(defs.length > 0, "必须从源码读到渠道表");
    const base = data.CHANNELS.find(c => c.kind === "amount");
    const channels = defs.map(([, id, name]) => ({ ...base, id, name, short: id.slice(0, 2).toUpperCase(),
      remaining: 7742.08, valid: true, stale: false, hidden: false, expiring: [], windows: [], claim: null }));
    channels.push({ ...base, id: "google-cloud", name: "Gemini · Google Cloud", remaining: 299.72, unit: "USD", valid: true });
    const codex = channels.find(c => c.id === "codex");
    Object.assign(codex, { kind: "percent", remaining: 67, resetCredits: { availableCount: 3 }, windows: [
      { key: "weekly", label: "本周", main: true, remainPercent: 67, status: "ok" },
      { key: "5h", label: "5 小时", remainPercent: 59, status: "ok" },
    ] });
    const ag = channels.find(c => c.id === "antigravity");
    Object.assign(ag, { kind: "percent", remaining: 71.4, windows: [
      { key: "gemini-weekly", label: "Gemini 本周", main: true, remainPercent: 71.4, status: "ok" },
      { key: "gemini-5h", label: "Gemini 5 小时", remainPercent: 1, status: "ok" },
      { key: "other-5h", label: "Claude/GPT 5 小时", remainPercent: 0, status: "rate-limited", resetsAt: new Date(w.Date.now() + 49 * 60000).toISOString() },
    ] });
    w.__TEST.patchCfg({ panelLayout: "thumbnail", form: "panel", showUsed: false });
    doc.body.className = "form-panel";
    w.__TEST.updateLayout();
    w.__TEST.setChannels(channels);
    w.hvApply({ ...data.HOME, rows: channels.map(c => ({ id: c.id, state: c.id === ag.id ? "limited" : "fresh", tone: "ok", tier: 1 })) });
    w.render({});
    const row = id => doc.querySelector(`#list .row[data-id="${id}"]`);
    check("动态渠道无遗漏或重复", () => {
      const ids = [...doc.querySelectorAll("#list > .row")].map(el => el.dataset.id);
      assert.deepEqual([...ids].sort(), channels.map(c => c.id).sort());
      assert.equal(new Set(ids).size, channels.length);
    });
    check("漏行坏样本触发断言", () => {
      const list = doc.getElementById("list");
      const last = list.querySelector(".row:last-child");
      const validate = () => assert.equal(list.querySelectorAll(":scope > .row").length, channels.length);
      validate();
      last.remove();
      try { assert.throws(validate, assert.AssertionError); }
      finally { list.append(last); }
      validate();
    });
    check("窄窗尺寸与单列相同", () => assert.deepEqual(w.__TEST.panelContentSize("thumbnail"), w.__TEST.panelContentSize("single")));
    check("Codex 周与短窗以及卡数量同时存在", () => {
      assert.match(row(codex.id).querySelector(".v").textContent, /周剩67/);
      assert.match(row(codex.id).querySelector(".thumb-status").textContent, /5h剩 59% · 卡 3/);
    });
    check("Antigravity 短窗与限流时间同时存在", () => {
      const el = row(ag.id).querySelector(".thumb-status");
      assert.match(el.textContent, /G5h剩 1% · 限流 49分钟/);
      assert.match(el.title, /Claude\/GPT/);
    });
    check("Google Cloud 短名保留完整名称", () => {
      const el = row("google-cloud").querySelector(".thumb-name");
      assert.equal(el.textContent, "Gemini · GCP");
      assert.equal(el.title, "Gemini · Google Cloud");
    });
    check("刷新保留卡片节点与焦点", () => {
      const el = row(codex.id).querySelector(".rhead"); el.focus(); w.render({});
      assert.equal(doc.activeElement, el);
      assert.equal(row(codex.id).querySelector(".rhead"), el);
    });
    check("已用口径同步更新标签与百分比", () => {
      w.__TEST.patchCfg({ showUsed: true }); w.render({});
      assert.match(row(codex.id).querySelector(".v").textContent, /周已用33/);
      assert.match(row(codex.id).querySelector(".thumb-status").textContent, /5h用 41%/);
      w.__TEST.patchCfg({ showUsed: false });
    });
    check("错误与旧数据及零余额区分", () => {
      const test = { ...channels[0], valid: false, remaining: null, error: "fixture failure" };
      let html = w.renderRow(test, 0);
      assert.match(html, /读取失败/);
      html = w.renderRow({ ...test, remaining: 10, stale: true }, 0);
      assert.match(html, /数据已旧/); assert.match(html, /class="tilde"/);
      html = w.renderRow({ ...test, valid: true, remaining: 0 }, 0);
      assert(!html.includes("读取失败"));
    });
    check("登录失效优先于配额摘要", () => {
      const html = w.renderRow({ ...codex, recovery: { phase: "auth" }, error: "请重新登录" }, 0);
      assert.match(html, /thumb-status[^>]*[\s\S]*登录失效/);
    });
    check("未知逐笔到期保留账户限定", () => {
      const c = { ...channels[0], kind: "points", remaining: 100, accountExpiry: { state: "never", at: null }, expiryComplete: false };
      const holder = doc.createElement("div"); holder.innerHTML = w.renderRow(c, 0);
      assert.match(holder.querySelector(".thumb-status").textContent, /账户额度/);
    });
    const saved = [];
    w.__SET_CONFIG_INVOKE = async ({ config }) => saved.push(config.panelLayout);
    const waitSaved = async layout => {
      for (let attempt = 0; attempt < 200; attempt++) {
        if (w.__TEST.cfg().panelLayout === layout && !doc.querySelector('#lkLayout button').disabled) return;
        await new Promise(resolve => setTimeout(resolve, 10));
      }
      throw new Error("布局点击未完成 " + layout);
    };
    doc.querySelector('#lkLayout [data-layout="single"]').click();
    await waitSaved("single");
    await w.__TEST.setLayout("thumbnail");
    check("模式保存并同步底栏与设置", () => {
      assert.deepEqual(saved, ["single", "thumbnail"]);
      assert.equal(doc.querySelector('#lkLayout [data-layout="thumbnail"]').getAttribute("aria-pressed"), "true");
      assert.equal(doc.querySelector('#cfgLayout [data-layout="thumbnail"]').getAttribute("aria-pressed"), "true");
    });
    w.__SET_CONFIG_INVOKE = async () => { throw new Error("layout fixture write failure"); };
    await w.__TEST.setLayout("double");
    check("保存失败回滚选择并提示", () => {
      assert.equal(w.__TEST.cfg().panelLayout, "thumbnail");
      assert(doc.body.classList.contains("layout-thumbnail"));
      assert.match(doc.getElementById("toast").textContent, /保存窗口布局失败/);
    });
    w.__SET_CONFIG_INVOKE = null;
    w.__TEST.patchCfg({ panelLayout: "thumbnail" }); w.__TEST.updateLayout(); w.render({});
    if (process.env.THUMBNAIL_LAYOUT_FIXTURES) fs.writeFileSync(process.env.THUMBNAIL_LAYOUT_FIXTURES,
      JSON.stringify({ body: doc.body.innerHTML, channels: channels.map(c => c.id) }));
  } finally {
    w.__SET_CONFIG_INVOKE = null;
    w.__TEST.patchCfg(before); w.__TEST.setChannels(data.CHANNELS);
    w.hvApply(data.HOME); w.__TEST.updateLayout(); w.render({});
  }
};
