/* Antigravity CLI 配额渠道：只调用本机 agy /usage --output-format json，不触碰桌面端凭据。 */
(() => {
  const ID = "antigravity";
  let pathDraft = null;
  let saving = false;

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

  window.AntigravityUI = { controls };
})();
