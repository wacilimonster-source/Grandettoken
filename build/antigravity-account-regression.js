// 在已初始化的真实前端中验证账号卡与设置，复用 jsdom-smoke 的严格 IPC 桩。
const assert = require("node:assert/strict");
const fs = require("node:fs");

module.exports = async function ({ w, R, data }) {
  const doc = w.document;
  const before = JSON.parse(JSON.stringify(w.__TEST.cfg()));
  const original = data.CHANNELS;
  const now = Math.floor(w.Date.now() / 1000);
  const base = original.find(c => c.kind === "percent");
  const accounts = ["a", "b"].map((key, i) => ({ ...base,
    id: "antigravity:" + key.repeat(24), name: "Antigravity · " + (i ? "备用账号" : "主账号"), short: "AG",
    valid: true, hasKey: true, hidden: false, stale: i === 1, limited: true,
    remaining: i ? 35 : 68, total: 100, unit: "%", authSource: "app", authLabel: i ? "上次记录" : "当前账号",
    error: null, recovery: null, updatedAt: now - (i ? 86400 : 0), extra: [],
    windows: [
      { key: "gemini-weekly", label: "Gemini 本周", main: true, remainPercent: i ? 35 : 68, status: "ok", resetsAt: new Date((now + 86400) * 1000).toISOString() },
      { key: "gemini-5h", label: "Gemini 5 小时", remainPercent: i ? 80 : 92, status: "ok", resetsAt: new Date((now + 3600) * 1000).toISOString() },
      { key: "3p-5h", label: "Claude/GPT 5 小时", remainPercent: 0, status: "rate-limited", resetsAt: new Date((now + 3600) * 1000).toISOString() },
    ],
  }));
  const check = (name, fn) => {
    try { fn(); R.push({ name: "Antigravity 多账号 " + name, ok: true }); }
    catch (error) { R.push({ name: "Antigravity 多账号 " + name, ok: false, extra: error.message }); }
  };
  const settle = () => new Promise(resolve => setTimeout(resolve, 70));
  const row = id => doc.querySelector(`#list .row[data-id="${id}"]`);
  const card = id => doc.querySelector(`[data-ag-account="${id}"]`);
  const fixtures = [];
  const click = async (id, action) => { card(id).querySelector(`[data-ag-account-action="${action}"]`).click(); await settle(); };
  const render = () => { w.__TEST.setChannels(data.CHANNELS); w.render({}); w.renderChCards(); };
  try {
    data.CHANNELS = accounts;
    w.__TEST.patchCfg({ hiddenChannels: [], antigravityAccountNames: {}, channelOrder: accounts.map(c => c.id), sort: "custom", form: "panel", showUsed: false });
    w.hvApply({ ...data.HOME, rows: [], order: [] });
    for (const layout of ["single", "double", "thumbnail"]) {
      w.__TEST.patchCfg({ panelLayout: layout });
      w.__TEST.updateLayout();
      render();
      check(layout + " 两卡独立且主数值准确", () => {
        assert.equal(doc.querySelectorAll("#list > .row").length, accounts.length);
        assert.match(row(accounts[0].id).querySelector(".v").textContent, /68\.0/);
        assert.match(row(accounts[1].id).querySelector(".v").textContent, /35\.0/);
        assert.match(row(accounts[0].id).textContent, /当前账号/);
        assert.match(row(accounts[1].id).textContent, /上次记录/);
        assert(row(accounts[1].id).querySelector(".dot.st"));
        assert.match(row(accounts[1].id).textContent, /80\.0%/);
        assert(row(accounts[0].id).querySelector('img[src="logos/antigravity.png"]'));
        if (layout === "thumbnail") {
          assert.match(row(accounts[0].id).querySelector(".thumb-status-main").textContent, /G5h剩 92%/);
          assert.match(row(accounts[1].id).querySelector(".thumb-status-main").textContent, /G5h剩 80%/);
        }
      });
      fixtures.push({ layout, body: doc.body.innerHTML, bodyClass: doc.body.className,
        bodyStyle: doc.body.getAttribute("style"), htmlStyle: doc.documentElement.getAttribute("style") });
    }
    check("管理页只有一个分组且列出全部账号", () => {
      assert.equal(doc.querySelectorAll('#chList .ccard[data-id="antigravity"]').length, 1);
      assert.equal(doc.querySelectorAll("#chList .ag-account").length, accounts.length);
      assert.equal(doc.querySelectorAll("#chList [data-ag-path]").length, 1);
      assert.match(doc.querySelector("[data-ag-visibility-note]").textContent, /冻结该账号的记录/);
      assert.match(doc.querySelector("[data-ag-visibility-note]").textContent, /隐藏整个 Antigravity 分组后停止 CLI 采集/);
    });
    // 分组坏样本已有失败断言，后续交互依赖这些控件，保留结果交给主冒烟报告。
    if (!doc.querySelector("#chList .ag-account")) return;
    check("旧版胶囊渠道选择扩展到账号并去重", () => {
      assert.deepEqual([...w.AntigravityUI.expandSelection(["antigravity", accounts[0].id])], accounts.map(c => c.id));
      assert.deepEqual([...w.AntigravityUI.expandSelection(["codex"])], ["codex"]);
    });
    check("漏卡、重复和错账号数值坏样本均能判红", () => {
      const verify = rows => {
        assert.equal(rows.length, accounts.length);
        assert.equal(new Set(rows.map(r => r.id)).size, accounts.length);
        for (const expected of accounts) assert.equal(rows.find(r => r.id === expected.id).remaining, expected.remaining);
      };
      verify(accounts);
      assert.throws(() => verify(accounts.slice(1)));
      assert.throws(() => verify([accounts[0], accounts[0]]));
      assert.throws(() => verify(accounts.map((c, i) => ({ ...c, remaining: accounts[1 - i].remaining }))));
    });
    w.openDetail(accounts[0].id);
    doc.querySelector('#detailBody [data-sec="ag-Claude%2FGPT"]').click();
    check("当前账号详情可以展开其他池", () => assert(doc.querySelector('#detailBody [data-ag-window="3p-5h"]')));
    w.openDetail(accounts[1].id);
    check("两个账号的详情展开状态独立", () => assert(!doc.querySelector('#detailBody [data-ag-window="3p-5h"]')));
    doc.querySelector('#detailBody [data-sec="ag-Claude%2FGPT"]').click();
    check("历史账号详情展示对应额度", () => {
      assert.match(doc.getElementById("detailBody").textContent, /35\.0%/);
      assert.match(doc.getElementById("detailBody").textContent, /上次记录/);
      assert(doc.querySelector('#detailBody [data-ag-window="3p-5h"]'));
    });
    accounts[1].windows[0].resetsAt = new Date((now - 60) * 1000).toISOString();
    w.AntigravityUI.tick();
    check("历史记录跨重置后保持余额并提示待刷新", () => {
      assert.match(doc.getElementById("detailBody").textContent, /重置已到，待刷新/);
      assert.match(doc.querySelector('#detailBody [data-ag-window="gemini-weekly"]').textContent, /35\.0%/);
    });
    w.closeDetail(false);
    w.__SET_CONFIG_INVOKE = async ({ config }) => {
      for (const account of accounts) {
        account.hidden = config.hiddenChannels.includes(account.id);
        const alias = config.antigravityAccountNames[account.id];
        if (alias) account.name = "Antigravity · " + alias;
      }
    };
    const input = card(accounts[0].id).querySelector("[data-ag-name]");
    input.value = "工作账号";
    input.dispatchEvent(new w.Event("input", { bubbles: true }));
    await click(accounts[0].id, "name");
    check("备注从正常入口保存并更新卡片", () => {
      assert.equal(w.__TEST.cfg().antigravityAccountNames[accounts[0].id], "工作账号");
      assert.match(row(accounts[0].id).textContent, /工作账号/);
    });
    await click(accounts[1].id, "visibility");
    check("单账号隐藏保留另一卡与管理恢复入口", () => {
      assert(row(accounts[0].id)); assert(!row(accounts[1].id));
      assert.match(card(accounts[1].id).textContent, /恢复显示/);
      assert.match(doc.querySelector(".restorebar").textContent, /账号卡冻结记录并保留会话识别/);
    });
    await click(accounts[1].id, "visibility");
    await click(accounts[1].id, "up");
    check("恢复和排序作用于具体账号", () => {
      assert(row(accounts[1].id));
      assert.equal(doc.querySelector("#list > .row").dataset.id, accounts[1].id);
    });
    w.__SET_CONFIG_INVOKE = async () => { throw new Error("account fixture disk failure"); };
    const saved = JSON.stringify(w.__TEST.cfg());
    const failedInput = card(accounts[0].id).querySelector("[data-ag-name]");
    failedInput.value = "写入失败";
    failedInput.dispatchEvent(new w.Event("input", { bubbles: true }));
    await click(accounts[0].id, "name");
    check("备注写盘失败回滚并提示", () => {
      assert.equal(JSON.stringify(w.__TEST.cfg()), saved);
      assert.equal(card(accounts[0].id).querySelector("[data-ag-name]").value, "工作账号");
      assert.match(doc.getElementById("toast").textContent, /保存Antigravity 账号设置失败/);
    });
    await click(accounts[0].id, "visibility");
    check("隐藏失败回滚并保留卡片", () => {
      assert.equal(JSON.stringify(w.__TEST.cfg()), saved);
      assert(row(accounts[0].id));
    });
    if (process.env.ANTIGRAVITY_LAYOUT_FIXTURES) fs.writeFileSync(process.env.ANTIGRAVITY_LAYOUT_FIXTURES,
      JSON.stringify({ fixtures, channels: accounts.map(c => c.id) }));
  } finally {
    w.closeDetail(false); w.closeManage();
    w.__SET_CONFIG_INVOKE = null;
    data.CHANNELS = original;
    w.__TEST.patchCfg(before); w.__TEST.setChannels(original);
    w.hvApply(data.HOME); w.__TEST.updateLayout(); w.render({}); w.renderChCards();
  }
};
