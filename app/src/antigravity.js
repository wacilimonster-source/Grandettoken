/* Antigravity 官方状态栏回调的账号卡、配额详情与管理入口。 */
(() => {
  const ID = "antigravity";
  let pathDraft = null;
  let saving = false;
  let accountSaving = false;
  const accountDrafts = new Map();
  let timeSignature = "";

  function isAccount(c) {
    return /^antigravity:[a-f0-9]{24}$/.test(c.id);
  }

  function expandSelection(ids) {
    const accounts = CHANNELS.filter(isAccount).map(c => c.id);
    return [...new Set(ids.flatMap(id => id === ID && accounts.length ? accounts : [id]))];
  }

  function tick() {
    const now = Date.now();
    const accounts = CHANNELS.filter(isAccount);
    const signature = new Date(now).toLocaleDateString("en-CA") + accounts.map(c =>
      c.id + (c.windows || []).map(w => `${w.key}:${resetMs(w) !== null && resetMs(w) <= now}`).join(",")).join("|");
    if (signature === timeSignature) return;
    timeSignature = signature;
    if (!accounts.length) return;
    render({});
    if (document.querySelector("[data-ag-name]")) renderChCards();
  }

  function accountStatus(c, compact = false) {
    const at = c.updatedAt ? `${sameLocalDay(c.updatedAt, Date.now() / 1000) ? "今天" : new Date(c.updatedAt * 1000).toLocaleDateString("zh-CN", { month: "numeric", day: "numeric" })} ${fmtClock(c.updatedAt)}` : "时间未知";
    const status = c.stale ? "上次记录" : "当前账号";
    if (compact) {
      const time = !c.updatedAt ? "时间未知" : sameLocalDay(c.updatedAt, Date.now() / 1000)
        ? fmtClock(c.updatedAt) : new Date(c.updatedAt * 1000).toLocaleDateString("zh-CN", { month: "numeric", day: "numeric" });
      return esc(`${status} ${time}`);
    }
    return esc(`${status} · ${at}${!compact && c.error ? " · " + c.error : ""}`);
  }

  function snapshotNotice(c) {
    if (!isAccount(c)) return "";
    const overdue = c.stale && (c.windows || []).some(w => resetMs(w) !== null && resetMs(w) <= Date.now());
    return `<p class="khint ag-snapshot">${accountStatus(c)}${overdue ? " · 重置已到，待刷新" : ""}${c.stale ? " · 切回此账号后刷新可更新记录" : ""}</p>`;
  }

  function managementList(list) {
    let inserted = false;
    return list.flatMap(c => {
      if (!isAccount(c)) return [c];
      if (inserted) return [];
      inserted = true;
      const current = list.find(x => isAccount(x) && !x.stale) || c;
      return [{ ...current, id: ID, name: "Antigravity", hidden: !!(CFG.hiddenChannels || []).includes(ID) }];
    });
  }

  function accountControls() {
    const order = currentOrderIds();
    const accounts = order.map(id => CHANNELS.find(c => c.id === id)).filter(c => c && isAccount(c));
    if (!accounts.length) return "";
    return `<div class="ag-accounts"><p class="khint" data-ag-visibility-note>隐藏账号卡会冻结该账号的记录，当前会话识别按采集节奏继续。隐藏整个 Antigravity 分组后停止 CLI 采集。</p>${accounts.map(c => {
      const position = order.indexOf(c.id);
      const name = accountDrafts.has(c.id) ? accountDrafts.get(c.id) : (CFG.antigravityAccountNames || {})[c.id] || "";
      return `<div class="ag-account" data-ag-account="${esc(c.id)}">
        <div class="ag-account-heading"><b>${esc(c.name)}</b><span>${accountStatus(c)}</span></div>
        <div class="ccbtns"><input id="ag-name-${esc(c.id)}" data-ag-name="${esc(c.id)}" value="${esc(name)}" maxlength="24" placeholder="账号备注"${accountSaving ? " disabled" : ""}>
        <button class="btn s" data-ag-account-action="name"${accountSaving ? " disabled" : ""}>保存备注</button>
        <button class="btn s" data-ag-account-action="visibility"${accountSaving ? " disabled" : ""}>${(CFG.hiddenChannels || []).includes(c.id) ? "恢复显示" : "隐藏账号卡"}</button>
        <button class="btn s" data-ag-account-action="up" aria-label="上移 ${esc(c.name)}"${accountSaving || position === 0 ? " disabled" : ""}>上移</button>
        <button class="btn s" data-ag-account-action="down" aria-label="下移 ${esc(c.name)}"${accountSaving || position === order.length - 1 ? " disabled" : ""}>下移</button></div>
      </div>`;
    }).join("")}</div>`;
  }

  document.addEventListener("input", e => {
    if (e.target && e.target.matches("[data-ag-name]")) accountDrafts.set(e.target.dataset.agName, e.target.value);
  });

  document.addEventListener("click", async e => {
    const button = e.target && e.target.closest ? e.target.closest("[data-ag-account-action]") : null;
    if (!button || button.disabled || accountSaving || saving) return;
    e.preventDefault();
    const id = button.closest("[data-ag-account]").dataset.agAccount;
    const before = JSON.parse(JSON.stringify(CFG));
    const action = button.dataset.agAccountAction;
    if (action === "name") {
      const name = accountDrafts.has(id) ? accountDrafts.get(id) : (CFG.antigravityAccountNames || {})[id] || "";
      CFG.antigravityAccountNames = { ...(CFG.antigravityAccountNames || {}), [id]: name.trim() };
    } else if (action === "visibility") {
      const hidden = new Set(CFG.hiddenChannels || []);
      if (hidden.has(id)) hidden.delete(id); else hidden.add(id);
      CFG.hiddenChannels = [...hidden];
    } else {
      const order = currentOrderIds();
      const peer = order[order.indexOf(id) + (action === "up" ? -1 : 1)];
      if (!peer) return;
      const from = order.indexOf(id), to = order.indexOf(peer);
      [order[from], order[to]] = [order[to], order[from]];
      CFG.channelOrder = order;
      CFG.sort = "custom";
    }
    accountSaving = true;
    renderChCards();
    const ok = await saveCfg("Antigravity 账号设置");
    if (!ok) CFG = before;
    if (action === "name") accountDrafts.delete(id);
    accountSaving = false;
    if (ok) {
      showToast("Antigravity 账号设置已保存");
      await refresh();
    }
    renderChCards();
    render();
  });

  function groups(c) {
    const result = new Map();
    for (const w of c.windows || []) {
      const name = w.key === "gemini-weekly" || w.key === "gemini-5h"
        ? "Gemini" : w.label.replace(/\s+(本周|5 小时)$/, "");
      if (!result.has(name)) result.set(name, []);
      result.get(name).push(w);
    }
    return [...result].sort(([a], [b]) => Number(b === "Gemini") - Number(a === "Gemini"));
  }

  function percent(w) {
    return Number.isFinite(w.remainPercent) ? Math.max(0, Math.min(100, w.remainPercent)) : null;
  }

  function value(w) {
    const n = percent(w);
    return n === null ? "未知" : `${dispPct(n).toFixed(1)}%`;
  }

  function exhausted(w) {
    return percent(w) === 0 || w.status === "rate-limited";
  }

  function resetText(w) {
    const ms = resetMs(w);
    if (ms === null) return "重置时间未知";
    const d = new Date(ms);
    const date = sameLocalDay(ms / 1000, Date.now() / 1000)
      ? "今日" : `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
    return `${date} ${fmtClock(ms / 1000)} 重置${ms <= Date.now() ? " · 重置已到，待刷新" : ""}`;
  }

  function countdown(w) {
    const ms = resetMs(w);
    return ms === null ? "" : `<span data-reset-at="${ms}">${esc(quotaResetCountdown(ms))}</span>`;
  }

  function rail(c) {
    const w = (c.windows || []).find((w) => w.key === "gemini-5h");
    if (!w) return { left: "Gemini 5 小时窗口未提供", right: "" };
    const state = exhausted(w) ? " · 已耗尽" : "";
    return {
      left: `Gemini 5 小时${CFG.showUsed ? "已用" : "剩余"} ${value(w)}${state}`,
      right: countdown(w) || "重置时间未知",
    };
  }

  function windowHtml(c, w, primary) {
    const n = percent(w);
    const tone = n === null ? "off" : quotaTone(c, w);
    const label = w.label.replace(/^Gemini\s+/, "");
    return `<div class="ag-window${primary ? " ag-primary" : ""}" data-ag-window="${esc(w.key)}">
      <div class="ag-window-head"><span>${esc(label)}${CFG.showUsed ? "已用" : "剩余"}</span><b style="color:${toneFg(tone)}">${esc(value(w))}</b></div>
      <div class="ag-bar" aria-hidden="true"><i style="width:${n ?? 0}%;background:${TONE_HEX[tone]}"></i></div>
      <div class="ag-reset">${exhausted(w) ? '<span class="ag-exhausted">已耗尽</span> · ' : ""}${esc(resetText(w))}${resetMs(w) === null ? "" : ` · ${countdown(w)}`}</div>
    </div>`;
  }

  function description(c, name) {
    return (c.extra || []).filter(([k]) => k === `模型 · ${name}`)
      .map(([, v]) => `<p class="ag-description">${esc(v)}</p>`).join("");
  }

  function windows(c) {
    const all = groups(c);
    const more = rowMore[c.id] || {};
    const content = all.map(([name, wins]) => {
      const sorted = [...wins].sort((a, b) => Number(b.key === "gemini-weekly") - Number(a.key === "gemini-weekly"));
      const rows = sorted.map((w) => windowHtml(c, w, w.key === "gemini-weekly")).join("");
      const desc = description(c, name);
      if (name === "Gemini") {
        const modelsOpen = !!more["ag-models"];
        const models = desc ? `<button class="sec-more" data-act="sec-more" data-id="${esc(c.id)}"
          data-sec="ag-models" aria-expanded="${modelsOpen}">适用模型 ${modelsOpen ? "▴" : "▾"}</button>` : "";
        return `<section class="ag-gemini" aria-label="Gemini 配额"><h3>Gemini</h3>${rows}
          ${models}${modelsOpen ? desc : ""}</section>`;
      }
      const sec = `ag-${encodeURIComponent(name)}`;
      const open = !!more[sec];
      const summary = wins.map((w) => `${w.label.replace(name, "").trim()} ${value(w)}${exhausted(w) ? " · 已耗尽" : ""}`).join(" / ");
      return `<section class="ag-group"><button class="sec-more ag-group-toggle" data-act="sec-more" data-id="${esc(c.id)}" data-sec="${esc(sec)}" aria-expanded="${open}">
        <span class="ag-group-name">${open ? "▾" : "▸"} ${esc(name)}</span><span class="ag-summary">${esc(summary)}</span></button>
        ${open ? rows + desc : ""}</section>`;
    });
    const names = new Set(all.map(([name]) => `模型 · ${name}`));
    const unmatched = (c.extra || []).filter(([k]) => ownsExtra(k) && !names.has(k));
    return `<div class="sec ag-quota"><div class="sec-h"><span>配额窗口</span><span>${esc(localZoneLabel())}</span></div>${content.join("")}
      ${unmatched.map(([k, v]) => `<p class="ag-description">${esc(k)} · ${esc(v)}</p>`).join("")}</div>`;
  }

  function ownsExtra(k) {
    return k.startsWith("模型 · ");
  }

  function currentVal() {
    if (pathDraft !== null) return pathDraft;
    return (CFG && CFG.antigravityCliPath) || "";
  }

  function controls(c) {
    const val = currentVal();
    const hasCustom = !!(val.trim() || (CFG && CFG.antigravityCliPath));
    const statusHint = c.hasKey
      ? "通过官方 agy 状态栏回调识别当前账号与配额，采集间隔至少 120 秒。历史账号保留最后记录，切号后在下一轮采集识别。"
      : "未检测到 agy CLI 或尚未登录。安装后在官方客户端完成登录，或在终端启动 agy 完成 Google 授权。下方可指定 CLI 路径。";
    return `<div class="mini">跟随官方当前登录会话采集，凭据由官方工具管理。<button class="why" data-why="agy" aria-expanded="false" aria-label="Antigravity 取数说明"></button></div>
      <div class="ccbtns" style="margin-top:6px">
        <input type="text" id="key-${ID}" data-ag-path autocomplete="off" spellcheck="false"
          value="${esc(val)}"${saving ? " disabled" : ""}
          placeholder="可选：agy CLI 路径（留空自动探测 %LOCALAPPDATA%\\agy\\bin\\agy.exe）">
        <button class="btn p" data-ag-action="save"${saving ? " disabled" : ""}>${saving ? "保存中…" : "保存"}</button>
        ${hasCustom ? `<button class="btn" data-ag-action="clear"${saving ? " disabled" : ""}>自动探测</button>` : ""}
      </div>
      <p class="khint">${esc(statusHint)}</p>${accountControls()}`;
  }

  document.addEventListener("input", (e) => {
    if (!e.target || !e.target.matches("[data-ag-path]")) return;
    pathDraft = e.target.value;
  });

  document.addEventListener("click", async (e) => {
    const btn = e.target && e.target.closest ? e.target.closest("[data-ag-action]") : null;
    if (!btn || btn.disabled || accountSaving) return;
    e.preventDefault();
    if (!CFG) return;
    const action = btn.dataset.agAction;
    const raw = action === "clear" ? "" : currentVal().trim();
    saving = true;
    renderChCards();
    const prevPath = CFG.antigravityCliPath ?? null;
    const nextPath = raw || null;
    CFG.antigravityCliPath = nextPath;
    const ok = await saveCfg("Antigravity CLI 路径");
    saving = false;
    if (!ok) {
      CFG.antigravityCliPath = prevPath;
      pathDraft = prevPath || "";
      renderChCards();
      return;
    }
    pathDraft = null;
    showToast(nextPath ? "已保存 agy CLI 路径，正在刷新" : "已恢复自动探测 agy CLI，正在刷新");
    await refresh();
    renderChCards();
  });

  window.AntigravityUI = { controls, rail, windows, ownsExtra, accountStatus, snapshotNotice, managementList, expandSelection, tick };
})();
