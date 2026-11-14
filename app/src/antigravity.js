/* Antigravity CLI 配额渠道：只调用本机 agy /usage --output-format json，不触碰桌面端凭据。 */
(() => {
  const ID = "antigravity";
  let pathDraft = null;
  let saving = false;

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
    return `${date} ${fmtClock(ms / 1000)} 重置`;
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
        const models = desc ? `<button class="sec-more" data-act="sec-more" data-id="${ID}"
          data-sec="ag-models" aria-expanded="${modelsOpen}">适用模型 ${modelsOpen ? "▴" : "▾"}</button>` : "";
        return `<section class="ag-gemini" aria-label="Gemini 配额"><h3>Gemini</h3>${rows}
          ${models}${modelsOpen ? desc : ""}</section>`;
      }
      const sec = `ag-${encodeURIComponent(name)}`;
      const open = !!more[sec];
      const summary = wins.map((w) => `${w.label.replace(name, "").trim()} ${value(w)}${exhausted(w) ? " · 已耗尽" : ""}`).join(" / ");
      return `<section class="ag-group"><button class="sec-more ag-group-toggle" data-act="sec-more" data-id="${ID}" data-sec="${esc(sec)}" aria-expanded="${open}">
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
      ? "仅调用本机官方 agy CLI 读取配额（自动冷却 ≥120 秒），不读取 IDE 凭据库或桌面端令牌。"
      : "未检测到 agy CLI 或尚未登录。安装后在终端运行 agy auth login 完成 Google 授权，或下方指定路径。";
    return `<div class="mini">仅通过官方 agy CLI 的公开 /usage 接口查询配额，不写入、不刷新密钥。<button class="why" data-why="agy" aria-expanded="false" aria-label="Antigravity 取数说明"></button></div>
      <div class="ccbtns" style="margin-top:6px">
        <input type="text" id="key-${ID}" data-ag-path autocomplete="off" spellcheck="false"
          value="${esc(val)}"${saving ? " disabled" : ""}
          placeholder="可选：agy CLI 路径（留空自动探测 %LOCALAPPDATA%\\agy\\bin\\agy.exe）">
        <button class="btn p" data-ag-action="save"${saving ? " disabled" : ""}>${saving ? "保存中…" : "保存"}</button>
        ${hasCustom ? `<button class="btn" data-ag-action="clear"${saving ? " disabled" : ""}>自动探测</button>` : ""}
      </div>
      <p class="khint">${esc(statusHint)}</p>`;
  }

  document.addEventListener("input", (e) => {
    if (!e.target || !e.target.matches("[data-ag-path]")) return;
    pathDraft = e.target.value;
  });

  document.addEventListener("click", async (e) => {
    const btn = e.target && e.target.closest ? e.target.closest("[data-ag-action]") : null;
    if (!btn || btn.disabled) return;
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

  window.AntigravityUI = { controls, rail, windows, ownsExtra };
})();
