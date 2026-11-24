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
    const defs = [...table.matchAll(/id:\s*"([^"]+)",\s*name:\s*"([^"]+)"[\s\S]*?default_kind:\s*Kind::(\w+)/g)];
    assert(defs.length > 0, "必须从源码读到渠道表");
    const base = data.CHANNELS.find(c => c.kind === "amount");
    const now = Math.floor(w.Date.now() / 1000);
    const channels = defs.map(([, id, name, kind]) => ({ ...base, id, name, kind: kind.toLowerCase(), short: id.slice(0, 2).toUpperCase(),
      remaining: kind === "Percent" ? 97 : 7742.08, unit: kind === "Points" ? "credits" : "CNY", valid: true, stale: false, hidden: false,
      expiring: kind === "Points" ? [{ at: now + 3 * 86400, amount: 174.96 }] : [],
      windows: kind === "Percent" ? [{ label: "周期", main: true, remainPercent: 97, status: "ok", resetsAt: new Date((now + 28 * 86400) * 1000).toISOString() }] : [], claim: null }));
    const futureDate = new Date((now + 89 * 86400) * 1000);
    const futureYmd = `${futureDate.getFullYear()}-${String(futureDate.getMonth() + 1).padStart(2, "0")}-${String(futureDate.getDate()).padStart(2, "0")}`;
    channels.push({ ...base, id: "google-cloud", name: "Gemini · Google Cloud", remaining: 299.72, unit: "USD", valid: true,
      googleCloud: { account: "layout-fixture", credits: [{ id: "fixture-credit", remaining: 299.72, unit: "USD", expiresDate: futureYmd }] } });
    const codex = channels.find(c => c.id === "codex");
    Object.assign(codex, { kind: "percent", remaining: 67, resetCredits: { availableCount: 3 }, windows: [
      { key: "weekly", label: "本周", main: true, remainPercent: 67, status: "ok", resetsAt: new Date(w.Date.now() + 5 * 86400000).toISOString() },
      { key: "5h", label: "5 小时", remainPercent: 59, status: "ok" },
    ] });
    const ag = channels.find(c => c.id === "antigravity");
    Object.assign(ag, { kind: "percent", remaining: 71.4, limited: true, windows: [
      { key: "gemini-weekly", label: "Gemini 本周", main: true, remainPercent: 71.4, status: "ok", resetsAt: new Date(w.Date.now() + 6 * 86400000).toISOString() },
      { key: "gemini-5h", label: "Gemini 5 小时", remainPercent: 77, status: "ok", resetsAt: new Date(w.Date.now() + 4 * 3600000).toISOString() },
      { key: "other-5h", label: "Claude/GPT 5 小时", remainPercent: 0, status: "rate-limited", resetsAt: new Date(w.Date.now() + 49 * 60000).toISOString() },
    ] });
    w.__TEST.patchCfg({ panelLayout: "thumbnail", form: "panel", showUsed: false });
    doc.body.className = "form-panel";
    w.__TEST.updateLayout();
    w.__TEST.setChannels(channels);
    w.hvApply({ ...data.HOME, rows: channels.map(c => ({ id: c.id, state: c.id === ag.id ? "limited" : "fresh", tone: c.id === ag.id ? "bad" : "ok", tier: 1,
      bindingWindow: c.id === ag.id ? "Claude/GPT 5 小时" : null,
      secondary: c.id === ag.id ? "Claude/GPT 限流 · 6天后恢复" : null })) });
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
      assert.equal(row(codex.id).querySelector(".thumb-caption").textContent, "本周剩余");
      assert.match(row(codex.id).querySelector(".v").textContent, /67/);
      assert.equal(row(codex.id).querySelector(".thumb-status-main").textContent, "重置卡 3 张");
      assert.equal(row(codex.id).querySelector(".thumb-status-end").textContent, "5h剩 59%");
    });
    check("Antigravity 缩略仅显示 Gemini 且不继承 Claude 限流", () => {
      const el = row(ag.id).querySelector(".thumb-status");
      assert.equal(el.querySelector(".thumb-status-main").textContent, "G5h剩 77%");
      assert.equal(el.querySelector(".thumb-status-end").textContent, "4小时后重置");
      assert(!/Claude|GPT|限流|6天/.test(row(ag.id).outerHTML));
      assert.equal(row(ag.id).dataset.dtone, "");
      assert.equal(row(ag.id).dataset.state, "fresh");
      assert.equal(ag.windows.length, 3);
      assert.equal(ag.limited, true);
    });
    check("Antigravity Gemini 耗尽保留恢复时间", () => {
      const windows = ag.windows.map(quota => quota.key === "gemini-5h" ? { ...quota, remainPercent: 0, status: "rate-limited", resetsAt: new Date(w.Date.now() + 49 * 60000).toISOString() } : quota);
      const box = doc.createElement("div"); box.innerHTML = w.renderRow({ ...ag, windows }, 0);
      assert.equal(box.querySelector(".thumb-status-main").textContent, "G5h剩 0%");
      assert.match(box.querySelector(".thumb-status-end").textContent, /限流 49分钟后恢复/);
      assert.equal(box.firstElementChild.dataset.dtone, "b");
    });
    check("Antigravity 缺 Gemini 时不借用其他池额度", () => {
      const box = doc.createElement("div"); box.innerHTML = w.renderRow({ ...ag, windows: ag.windows.filter(w => !w.key.startsWith("gemini-")) }, 0);
      assert.match(box.querySelector(".v").textContent, /——/);
      assert(!/Claude|GPT|限流/.test(box.querySelector(".thumb-status").outerHTML));
    });
    check("Google Cloud 短名保留完整名称", () => {
      const el = row("google-cloud").querySelector(".thumb-name");
      assert.equal(el.textContent, "Gemini · GCP");
      assert.equal(el.title, "Gemini · Google Cloud");
      assert.equal(row("google-cloud").querySelector(".thumb-status-main").textContent, "USD 299.72 将到期");
      assert.match(row("google-cloud").querySelector(".thumb-status-end").textContent, /^\d{4}-\d{2}-\d{2}$/);
    });
    check("Google Cloud 单渠道在三种布局跨年后自动更新同一批次", () => {
      const RealDate = w.Date;
      let time;
      w.Date = class extends RealDate {
        constructor(...args) { super(...(args.length ? args : [time])); }
        static now() { return time; }
      };
      const google = channels.find(c => c.id === "google-cloud");
      const c = { ...google, remaining: 285.62, googleCloud: { ...google.googleCloud, credits: [
        { id: "earlier", remaining: 275.62, unit: "USD", expiresDate: "2026-12-31" },
        { id: "later", remaining: 10, unit: "USD", expiresDate: "2027-01-08" },
      ] } };
      try {
        w.__TEST.setChannels([c]);
        for (const layout of ["single", "double", "thumbnail"]) {
          time = new RealDate(2026, 11, 31, 23, 59).getTime();
          w.__TEST.patchCfg({ panelLayout: layout }); w.__TEST.updateLayout(); w.render({});
          w.PanelLayoutUI.tick();
          const status = () => row(c.id).querySelector(layout === "thumbnail" ? ".thumb-status" : ".r2").textContent;
          assert.match(status(), layout === "thumbnail" ? /275\.62 将到期/ : /今日到期 · USD 275\.62/);
          time = new RealDate(2027, 0, 1, 0, 1).getTime();
          w.PanelLayoutUI.tick();
          assert.match(status(), layout === "thumbnail" ? /275\.62 已到期/ : /2026-12-31 已到期 · USD 275\.62/);
          assert.match(row(c.id).querySelector(".v").textContent, /285\.62/);
        }
      } finally {
        w.Date = RealDate;
        w.__TEST.patchCfg({ panelLayout: "thumbnail" }); w.__TEST.updateLayout();
        w.__TEST.setChannels(channels); w.render({});
      }
    });
    check("刷新保留卡片节点与焦点", () => {
      const el = row(codex.id).querySelector(".rhead"); el.focus(); w.render({});
      assert.equal(doc.activeElement, el);
      assert.equal(row(codex.id).querySelector(".rhead"), el);
    });
    check("已用口径同步更新标签与百分比", () => {
      w.__TEST.patchCfg({ showUsed: true }); w.render({});
      assert.equal(row(codex.id).querySelector(".thumb-caption").textContent, "本周已用");
      assert.match(row(codex.id).querySelector(".v").textContent, /33/);
      assert.equal(row(codex.id).querySelector(".thumb-status-end").textContent, "5h已用 41%");
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
      const c = { ...channels[0], kind: "points", remaining: 100, expiring: [], accountExpiry: { state: "never", at: null }, expiryComplete: false };
      const holder = doc.createElement("div"); holder.innerHTML = w.renderRow(c, 0);
      assert.match(holder.querySelector(".thumb-status").textContent, /账户额度/);
    });
    check("到期金额与日期分列", () => {
      const c = channels.find(c => c.kind === "points");
      assert.match(row(c.id).querySelector(".thumb-status-main").textContent, /174\.96 分将到期/);
      const date = row(c.id).querySelector(".thumb-status-end").textContent;
      const pattern = /^(?:\d{4}-)?\d{1,2}-\d{2} (?:(?:本|下)?周[日一二三四五六]|今天|明天|后天)$/;
      assert.match(date, pattern);
      assert.throws(() => assert.match(date.split(" ")[0], pattern), assert.AssertionError);
    });
    check("星期按自然周计算并覆盖跨月跨年闰日", () => {
      const at = (year, month, day) => new w.Date(year, month - 1, day, 12).getTime();
      const examples = [
        [[2026, 11, 15], [2026, 11, 15], "11-15 今天"],
        [[2026, 11, 15], [2026, 11, 16], "11-16 明天"],
        [[2026, 11, 15], [2026, 11, 17], "11-17 后天"],
        [[2026, 11, 15], [2026, 11, 18], "11-18 下周三"],
        [[2026, 11, 16], [2026, 11, 18], "11-18 后天"],
        [[2026, 11, 16], [2026, 11, 19], "11-19 本周四"],
        [[2026, 11, 15], [2026, 12, 9], "12-09 周三"],
        [[2026, 11, 30], [2026, 12, 1], "12-01 明天"],
        [[2026, 12, 27], [2027, 1, 2], "2027-1-02 下周六"],
        [[2028, 2, 28], [2028, 2, 29], "2-29 明天"],
        [[2026, 12, 31], [2027, 1, 2], "2027-1-02 后天"],
      ];
      for (const [today, target, expected] of examples) {
        assert.equal(w.PanelLayoutUI.expiryDate(at(...target) / 1000, at(...today)), expected);
      }
      assert.equal(w.PanelLayoutUI.expiryDate(null), "日期未知");
      assert.equal(w.PanelLayoutUI.expiryDate(NaN), "日期未知");
      assert.equal(w.PanelLayoutUI.expiryDate(Infinity), "日期未知");
    });
    check("WorkBuddy 当天到期与首页摘要一致并在午夜自动更新", () => {
      const RealDate = w.Date;
      let time = new RealDate(2026, 10, 17, 23, 59).getTime();
      const at = new RealDate(2026, 10, 18, 10, 26).getTime() / 1000;
      w.Date = class extends RealDate {
        constructor(...args) { super(...(args.length ? args : [time])); }
        static now() { return time; }
      };
      try {
        const workbuddy = channels.find(c => c.id === "workbuddy");
        w.__TEST.setChannels([{ ...workbuddy, expiring: [{ at, amount: 23.03 }], expiryComplete: true }]);
        w.render({});
        w.tickPeakCountdowns();
        const date = () => row(workbuddy.id).querySelector(".thumb-status-end").textContent;
        assert.equal(date(), "11-18 明天");
        time = new RealDate(2026, 10, 18, 0, 1).getTime();
        w.tickPeakCountdowns();
        assert.equal(date(), "11-18 今天");
        assert.match(doc.querySelector('.metric[data-id="workbuddy"] .due-date').textContent, /11-18今天/);
        assert.match(row(workbuddy.id).querySelector(".thumb-status").title, /11-18 今天/);
        assert.match(row(workbuddy.id).querySelector("[data-points-expiry]").title, /2026-11-18 周三 10:26/);
        for (const layout of ["single", "double"]) {
          w.__TEST.patchCfg({ panelLayout: layout }); w.__TEST.updateLayout(); w.render({});
          assert.match(row(workbuddy.id).querySelector(".r2").textContent, /11-18 今天 10:26/);
        }
      } finally {
        w.Date = RealDate;
        w.__TEST.patchCfg({ panelLayout: "thumbnail" }); w.__TEST.updateLayout();
        w.__TEST.setChannels(channels); w.render({});
      }
    });
    check("跨周分钟刷新更新日期及辅助提示并保留焦点", () => {
      const RealDate = w.Date;
      let time = new RealDate(2026, 10, 15, 23, 59).getTime();
      const expiry = new RealDate(2026, 10, 19, 10, 26).getTime() / 1000;
      w.Date = class extends RealDate {
        constructor(...args) { super(...(args.length ? args : [time])); }
        static now() { return time; }
      };
      try {
        const c = { ...channels.find(c => c.kind === "points"), expiring: [{ at: expiry, amount: 30 }] };
        w.__TEST.setChannels([c]); w.render({});
        const head = row(c.id).querySelector(".rhead"); head.focus();
        assert.match(row(c.id).querySelector(".thumb-status-end").textContent, /下周四/);
        time = new RealDate(2026, 10, 16, 0, 1).getTime();
        w.PanelLayoutUI.tick();
        assert.equal(row(c.id).querySelector(".thumb-status-end").textContent, "11-19 本周四");
        assert(!row(c.id).querySelector(".thumb-status").title.includes("下周四"));
        assert.match(row(c.id).querySelector(".thumb-status-end span").title, /2026-11-19 周四 10:26/);
        assert.equal(doc.activeElement, head);
        w.__TEST.patchCfg({ panelLayout: "single" }); w.__TEST.updateLayout(); w.render({});
        assert.match(row(c.id).querySelector(".r2").textContent, /11-19 本周四/);
        w.__TEST.patchCfg({ panelLayout: "thumbnail" }); w.__TEST.updateLayout(); w.render({});
        time = new RealDate(2026, 10, 17, 0, 1).getTime(); w.PanelLayoutUI.tick();
        assert.equal(row(c.id).querySelector(".thumb-status-end").textContent, "11-19 后天");
        time = new RealDate(2026, 10, 18, 0, 1).getTime(); w.PanelLayoutUI.tick();
        assert.equal(row(c.id).querySelector(".thumb-status-end").textContent, "11-19 明天");
      } finally {
        w.Date = RealDate;
        w.__TEST.patchCfg({ panelLayout: "thumbnail" }); w.__TEST.updateLayout();
        w.__TEST.setChannels(channels); w.render({});
      }
    });
    check("Codex 和 Gemini 周重置取各自周窗口", () => {
      for (const c of [codex, ag]) {
        const weekly = c.windows.find(w => w.key === (c.id === "codex" ? "weekly" : "gemini-weekly"));
        const el = row(c.id).querySelector(".thumb-window [data-weekly-reset]");
        assert.equal(Number(el.dataset.weeklyReset), Date.parse(weekly.resetsAt));
        assert.equal(el.textContent, w.PanelLayoutUI.expiryDate(Date.parse(weekly.resetsAt) / 1000) + "重置");
        assert.match(el.title, /周额度下次重置/);
        assert(!el.title.includes("Claude"));
      }
      try {
        w.__TEST.patchCfg({ panelLayout: "single" }); w.__TEST.updateLayout(); w.render({});
        assert.match(row(codex.id).querySelector(".r2").textContent, /周重置/);
        assert(row(ag.id).querySelector(".r2 [data-reset-at]"), "单列保留 Gemini 短窗倒计时");
        assert(row(ag.id).querySelector(".r2 [data-weekly-reset]"), "单列显示 Gemini 周重置日期");
      } finally {
        w.__TEST.patchCfg({ panelLayout: "thumbnail" }); w.__TEST.updateLayout(); w.render({});
      }
    });
    check("周重置时间未知及已到时间如实展示", () => {
      const box = doc.createElement("div");
      box.innerHTML = w.renderRow({ ...codex, windows: codex.windows.filter(w => w.key !== "weekly") }, 0);
      assert.equal(box.querySelector(".thumb-week-reset").textContent, "周重置未知");
      assert.match(w.PanelLayoutUI.weeklyReset({ ...ag, windows: [{ key: "claude-weekly", label: "Claude/GPT 本周", resetsAt: new Date(w.Date.now() + 6 * 86400000).toISOString() }] }), /周重置未知/);
      w.PanelLayoutUI.tick();
      const el = row(codex.id).querySelector(".thumb-window [data-weekly-reset]");
      const previous = el.dataset.weeklyReset;
      el.dataset.weeklyReset = String(w.Date.now() - 1); w.PanelLayoutUI.tick();
      assert.equal(el.textContent, "周重置待刷新");
      el.dataset.weeklyReset = previous; w.PanelLayoutUI.tick();
    });
    check("只有周额度渠道时跨日同步更新周重置辅助提示", () => {
      const RealDate = w.Date;
      let time = new RealDate(2026, 10, 15, 23, 59).getTime();
      const reset = new RealDate(2026, 10, 18, 10, 26).toISOString();
      w.Date = class extends RealDate {
        constructor(...args) { super(...(args.length ? args : [time])); }
        static now() { return time; }
      };
      try {
        const c = { ...codex, windows: codex.windows.map(quota => quota.key === "weekly" ? { ...quota, resetsAt: reset } : quota) };
        w.__TEST.setChannels([c]); w.render({});
        assert.equal(doc.querySelectorAll("[data-points-expiry]").length, 0);
        assert.match(row(c.id).querySelector(".thumb-window").textContent, /下周三重置/);
        time = new RealDate(2026, 10, 16, 0, 1).getTime(); w.PanelLayoutUI.tick();
        assert.match(row(c.id).querySelector(".thumb-window").textContent, /后天重置/);
        assert.match(row(c.id).querySelector(".thumb-status").title, /后天/);
        assert(!row(c.id).querySelector(".thumb-status").title.includes("下周三"));
        time = new RealDate(2026, 10, 18, 0, 1).getTime(); w.PanelLayoutUI.tick();
        assert.equal(row(c.id).querySelector("[data-weekly-reset]").textContent, "11-18 今天重置");
        assert.match(row(c.id).querySelector(".thumb-status").title, /周重置 11-18 今天/);
      } finally {
        w.Date = RealDate; w.__TEST.setChannels(channels); w.render({});
      }
    });
    check("跨重置点保留待刷新状态", () => {
      const el = row(ag.id).querySelector("[data-thumbnail-reset]");
      const previous = el.dataset.thumbnailReset;
      el.dataset.thumbnailReset = String(w.Date.now() - 1);
      w.PanelLayoutUI.tick();
      assert.equal(el.textContent, "待刷新");
      el.dataset.thumbnailReset = previous;
      w.PanelLayoutUI.tick();
      assert.match(el.textContent, /后重置$/);
    });
    check("所有模式的渠道摘要隐藏推算文字", () => {
      const amount = { ...base, estimated: true, day: 3 };
      const grant = { ...channels.find(c => c.kind === "points"), grantToday: { hasActivity: true, activityTotal: 30, total: 30, fromApi: false } };
      try {
        for (const [layout, form] of [["single", "panel"], ["double", "panel"], ["thumbnail", "panel"], ["single", "compact"], ["single", "pill"]]) {
          w.__TEST.patchCfg({ panelLayout: layout, form });
          doc.body.className = "form-" + form; w.__TEST.updateLayout();
          for (const c of [amount, grant]) {
            const box = doc.createElement("div"); box.innerHTML = w.renderRow(c, 0);
            assert(!/推算/.test(box.querySelector(".rhead").textContent), layout + "/" + form);
          }
        }
      } finally {
        w.__TEST.patchCfg({ panelLayout: "thumbnail", form: "panel" });
        doc.body.className = "form-panel"; w.__TEST.updateLayout(); w.render({});
      }
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
