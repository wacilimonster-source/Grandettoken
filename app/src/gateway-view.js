/* 本地模型网关视图：三类上游接入、密钥箱多上架、代理开关（默认开启）、模型目录、在线联调与诊断。 */
(() => {
  let GW = null;
  let GW_BUSY = false;
  let GW_TAB = "channels";
  let GW_KIND = "openai_compat";
  let GW_CATALOG_FILTER = "";
  let GW_VAULT_FILTER = "";
  let GW_MODEL_FILTERS = {};
  let GW_TEST_RESULT = null;
  let GW_TEST_BUSY = false;
  let GW_TEST_STARTED_AT = 0;
  let GW_TEST_TIMER = null;
  let GW_TEST_RUN_ID = 0;
  let GW_DIAGS = [];
  let GW_UI_WIRED = false;
  const UI = createGatewayUiState();
  let GW_SYNC_LISTENER = null;

  const byId = (id) => document.getElementById(id);
  const escHtml = (v) =>
    typeof esc === "function"
      ? esc(String(v ?? ""))
      : String(v ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

  function notify(msg, isErr) {
    if (typeof showToast === "function") showToast(msg, !!isErr);
  }

  function syncGatewayButtons(box) {
    box.querySelectorAll("[data-gw-exclusive]").forEach((button) => {
      button.disabled = GW_BUSY;
    });
  }

  async function writeGateway(work) {
    UI.beginWrite();
    try { return await work(); }
    finally { UI.endWrite(); }
  }

  function testButtonLabel() {
    if (!GW_TEST_BUSY) return "发送测试";
    const sec = Math.max(0, Math.floor((Date.now() - GW_TEST_STARTED_AT) / 1000));
    return sec > 0 ? `请求中 ${sec}s...` : "请求中...";
  }

  function syncTestBusyUi(box) {
    const runBtn = box.querySelector('[data-gw="run-test-chat"]');
    if (runBtn) {
      runBtn.disabled = GW_TEST_BUSY;
      runBtn.textContent = testButtonLabel();
    }
    const cancelBtn = box.querySelector('[data-gw="cancel-test-chat"]');
    if (cancelBtn) {
      cancelBtn.hidden = !GW_TEST_BUSY;
    }
  }

  function stopTestTimer(box) {
    if (GW_TEST_TIMER) {
      clearInterval(GW_TEST_TIMER);
      GW_TEST_TIMER = null;
    }
    if (box) syncTestBusyUi(box);
  }

  function startTestTimer(box) {
    stopTestTimer(null);
    GW_TEST_STARTED_AT = Date.now();
    syncTestBusyUi(box);
    GW_TEST_TIMER = setInterval(() => {
      if (!GW_TEST_BUSY) {
        stopTestTimer(box);
        return;
      }
      syncTestBusyUi(box);
    }, 1000);
  }

  function formatDiagMeta(d) {
    const parts = [
      `HTTP ${d.status}`,
      d.firstByteMs != null ? `${d.latencyMs}ms (首字 ${d.firstByteMs}ms)` : `${d.latencyMs}ms`,
      d.stream ? (d.frameCount != null ? `SSE ${d.frameCount}帧` : "SSE") : "JSON",
      `思考 ${d.thinkingChars}字 / 正文 ${d.contentChars}字`,
    ];
    if ((d.toolCallCount || 0) > 0) {
      parts.push(`工具 ${d.toolCallCount}次(${d.toolCallChars || 0}字)`);
    }
    if (d.finishReason) {
      parts.push(`结束 ${d.finishReason}`);
    }
    if (d.promptTokens != null || d.completionTokens != null) {
      parts.push(`tok ${d.promptTokens ?? "?"}/${d.completionTokens ?? "?"}`);
    }
    return parts.join(" · ");
  }

  function sourceBadge(kind) {
    if (kind === "agent_platform") return { cls: "is-agent", text: "Agent Platform" };
    if (kind === "gemini_aistudio") return { cls: "is-gemini", text: "Gemini AI Studio" };
    return { cls: "is-openai", text: "OpenAI 兼容" };
  }

  function wireOptions(wire) {
    return `<option value="chat_completions">Chat Completions</option>
      <option value="responses" ${wire === "responses" ? "selected" : ""}>Responses</option>`;
  }

  function exampleModel() {
    const channel = (GW.channels || []).find((c) => !c.stale && c.models?.length);
    return channel?.models[0] || "alias/model";
  }

  function headersToText(map) {
    if (!map || typeof map !== "object") return "";
    return Object.entries(map)
      .map(([k, v]) => `${k}: ${v}`)
      .join("\n");
  }

  function parseHeadersText(raw) {
    const out = {};
    for (const line of String(raw || "").split(/\r?\n/)) {
      const t = line.trim();
      if (!t) continue;
      const idx = t.indexOf(":");
      if (idx <= 0) throw new Error(`自定义请求头格式无效（应为 Name: Value）：${t}`);
      const k = t.slice(0, idx).trim();
      const v = t.slice(idx + 1).trim();
      if (!k) continue;
      out[k] = v;
    }
    return out;
  }

  function parseManualModels(raw) {
    const seen = new Set();
    const out = [];
    for (const part of String(raw || "").split(/[\r\n,]+/)) {
      const t = part.trim();
      if (t && !seen.has(t)) {
        seen.add(t);
        out.push(t);
      }
    }
    return out;
  }

  function snapshotOpenState(box) {
    const openSet = new Set();
    box.querySelectorAll("details[data-gw-detail]").forEach((el) => {
      if (el.open) openSet.add(el.dataset.gwDetail);
    });
    return {
      openSet,
      scrollTop: box.scrollTop || 0,
      modelFilters: { ...GW_MODEL_FILTERS },
    };
  }

  function restoreOpenState(box, snap) {
    if (!snap) return;
    box.querySelectorAll("details[data-gw-detail]").forEach((el) => {
      if (snap.openSet.has(el.dataset.gwDetail)) el.open = true;
    });
    if (snap.scrollTop > 0) box.scrollTop = snap.scrollTop;
  }

  async function refreshDiagnostics() {
    const ticket = UI.read("diag");
    try {
      const diags = await invoke("gateway_diagnostics");
      if (!UI.current("diag", ticket)) return;
      GW_DIAGS = Array.isArray(diags) ? diags : [];
      renderDiagnostics(); // diagnostic-only
    } catch (err) {
      if (UI.current("diag", ticket)) notify(`读取诊断失败：${String(err)}`, true);
    }
  }

  async function clearDiagnostics() {
    UI.beginDiagWrite();
    try {
      await invoke("gateway_clear_diagnostics");
      GW_DIAGS = [];
      renderDiagnostics();
    } finally {
      UI.endDiagWrite();
    }
  }

  async function refreshGatewayData(options = {}) {
    const box = byId("gatewayBox");
    if (!box) return;
    const ticket = UI.read("state");
    const diagnostics = refreshDiagnostics();
    try {
      const status = await invoke("gateway_status");
      if (!UI.current("state", ticket)) return;
      GW = status;
      UI.sync(status.syncState);
      const snap = snapshotOpenState(box);
      const uiSnap = UI.snapshot(box);
      const clearAdd = options.clearAdd && !UI.hasDirty(box.querySelector('[data-gw-detail="add-channel"]'));
      if (clearAdd) {
        snap.openSet.delete("add-channel");
        snap.openSet.delete("add-adv");
      }
      renderGateway(snap, clearAdd ? null : captureAddDraft(), uiSnap);
    } catch (e) {
      if (!UI.current("state", ticket)) return;
      if (GW) notify(`读取网关失败：${String(e)}`, true);
      else box.innerHTML = `<div class="empty">读取网关失败：${escHtml(String(e))}</div>`;
    } finally {
      await diagnostics;
    }
  }

  function renderSyncStatus() {
    const el = byId("gatewayBox")?.querySelector("[data-gw-sync-status]");
    if (!el) return;
    const sync = UI.syncState();
    el.hidden = !sync;
    el.className = sync?.error ? "gw-error" : "gw-footnote";
    el.textContent = sync?.error ? `后台同步失败：${sync.error}` : sync?.pending ? "后台同步中…" : "后台同步已完成";
  }

  async function listenGatewaySync() {
    if (!GW_SYNC_LISTENER) GW_SYNC_LISTENER = listen("gateway-sync-status", (event) => {
      if (UI.sync(event.payload)) renderSyncStatus();
    }).catch((err) => {
      GW_SYNC_LISTENER = null;
      notify(`监听网关同步状态失败：${String(err)}`, true);
    });
    await GW_SYNC_LISTENER;
  }

  function captureAddDraft() {
    const vaultEl = byId("gwVault");
    const urlEl = byId("gwBaseUrl");
    const aliasEl = byId("gwAlias");
    const tplEl = byId("gwTemplate");
    const manualEl = byId("gwManualModels");
    const proxyEl = byId("gwNewProxy");
    const noKeyEl = byId("gwNoUpstreamKey");
    const saveEpEl = byId("gwSaveEndpointToVault");
    const thinkEl = byId("gwNewThinkingMode");
    const policyEl = byId("gwNewThoughtPolicy");
    if (!aliasEl) return null;
    return {
      kind: GW_KIND,
      wire: byId("gwNewWire")?.value || "chat_completions",
      template: tplEl ? tplEl.value : "",
      vaultId: vaultEl ? vaultEl.value : "",
      baseUrl: urlEl ? urlEl.value : "",
      alias: aliasEl.value || "",
      manualModels: manualEl ? manualEl.value : "",
      proxy: proxyEl ? proxyEl.checked : true,
      noKey: noKeyEl ? noKeyEl.checked : false,
      saveEndpoint: saveEpEl ? saveEpEl.checked : false,
      thinkingMode: thinkEl ? thinkEl.value : "auto",
      thoughtPolicy: policyEl ? policyEl.value : "follow_client",
    };
  }

  function renderChannelCard(c, vault) {
    const id = c.channelId || c.vaultId;
    const badge = sourceBadge(c.source);
    const isResponses = c.wire === "responses";
    const avail = c.availableModels || [];
    const allowedSet = c.allowedModels ? new Set(c.allowedModels) : null;
    const enabledCount = avail.filter((m) => !allowedSet || allowedSet.has(m)).length;
    const filter = (GW_MODEL_FILTERS[id] || "").trim().toLowerCase();
    const hasMatches = avail.some((m) => !filter || m.toLowerCase().includes(filter));
    const statusText = c.stale
      ? "待同步"
      : c.lastStatus === "error"
      ? `异常 (${enabledCount})`
      : `${enabledCount} 个模型`;
    const statusCls = c.stale ? "is-stale" : c.lastStatus === "error" ? "is-err" : "";

    return `<article class="gw-channel" data-gw-card="${escHtml(id)}">
      <div class="gw-channel-main">
        <div class="gw-channel-heading">
          <div class="gw-title-wrap">
            <b>${escHtml(c.label || c.alias)}</b>
            <span class="gw-badge ${badge.cls}">${escHtml(badge.text)}</span>
            ${isResponses ? '<span class="gw-badge is-openai">Responses</span>' : ""}
            <span class="gw-count ${statusCls}">${escHtml(statusText)}</span>
          </div>
        </div>
        <div class="gw-channel-meta">
          <span>别名 <code>${escHtml(c.alias)}</code></span>
          <span>·</span>
          <span>${escHtml(c.host || c.baseUrl)}</span>
          ${c.noUpstreamKey ? `<span>· 免上游密钥</span>` : c.platform ? `<span>· ${escHtml(c.platform)}</span>` : ""}
        </div>
        ${c.lastError ? `<p class="gw-inline-note ${c.lastStatus === "error" ? "is-error" : ""}">${escHtml(c.lastError)}</p>` : ""}
      </div>
      <div class="gw-channel-actions">
        <label class="gw-quick-proxy" title="默认开启；取消勾选则直连上游不走系统代理">
          <input type="checkbox" data-gw-quick-proxy="${escHtml(id)}" ${c.systemProxy ? "checked" : ""}>
          <span>代理</span>
        </label>
        <button class="btn" data-gw="test-channel" data-id="${escHtml(id)}" data-gw-exclusive ${isResponses ? 'title="发送一条实际模型请求，可能消耗上游额度"' : ""}>测连通</button>
        <button class="btn" data-gw="refresh" data-id="${escHtml(id)}" data-gw-exclusive>刷新模型</button>
        <button class="btn" data-gw="remove" data-id="${escHtml(id)}" data-gw-exclusive>下架</button>
      </div>
      <details class="gw-details" data-gw-detail="ch-${escHtml(id)}">
        <summary>配置与模型 <span>(${enabledCount}/${avail.length} 可调用)</span></summary>
        <div class="gw-details-body">
          <div class="gw-edit-grid">
            <label class="gw-add-field">
              <span>英文别名（唯一）</span>
              <input class="inp" data-gw-alias="${escHtml(id)}" value="${escHtml(c.alias)}" maxlength="32">
            </label>
            <label class="gw-add-field">
              <span>上游 Base URL</span>
              <input class="inp" data-gw-baseurl="${escHtml(id)}" value="${escHtml(c.baseUrl)}">
            </label>
            ${c.source === "openai_compat" ? `<label class="gw-add-field gw-span-2">
              <span>上游 API 协议</span>
              <select class="sel" data-gw-wire>${wireOptions(c.wire)}</select>
            </label>` : ""}
            ${
              c.noUpstreamKey
                ? `<div class="gw-add-field gw-span-2"><span>鉴权模式</span><div class="gw-readonly-chip">免上游密钥（不附带 Authorization）</div></div>`
                : `<label class="gw-add-field gw-span-2"><span>关联密钥箱条目</span><select class="sel" data-gw-vault-edit="${escHtml(id)}">${vault
                    .map((v) => `<option value="${escHtml(v.id)}" ${v.id === c.vaultId ? "selected" : ""}>${escHtml(v.platform)} · ${escHtml(v.label)} (${escHtml(v.hint)})</option>`)
                    .join("")}</select></label>`
            }
          </div>
          <div class="gw-model-config">
            <div class="gw-model-config-head">
              <b>可见模型清单（保存后生效）</b>
              <div class="gw-model-filter-row">
                <input class="inp gw-filter-inp" data-gw-model-filter="${escHtml(id)}" value="${escHtml(GW_MODEL_FILTERS[id] || "")}" placeholder="筛选模型...">
                <button type="button" class="btn" data-gw="select-all-models" data-id="${escHtml(id)}">全选</button>
                <button type="button" class="btn" data-gw="clear-all-models" data-id="${escHtml(id)}">全不选</button>
              </div>
            </div>
            <div class="gw-model-checklist">
              ${avail.map((m) => `<label class="gw-model-check" style="display:${!filter || m.toLowerCase().includes(filter) ? "" : "none"}">
                <input type="checkbox" data-gw-model="${escHtml(id)}" value="${escHtml(m)}" ${!allowedSet || allowedSet.has(m) ? "checked" : ""}>
                <span>${escHtml(m)}</span>
              </label>`).join("")}
              <div class="gw-empty-note" data-gw-model-empty ${hasMatches ? "hidden" : ""}>${avail.length ? "没有匹配的模型" : "暂无模型，可在下方「高级选项」补充手动模型"}</div>
            </div>
          </div>
          <details class="gw-subdetails" data-gw-detail="ch-adv-${escHtml(id)}">
            <summary>高级选项 <span>思维链策略 · 手动模型 · 自定义请求头</span></summary>
            <div class="gw-subdetails-body">
              <div class="gw-edit-grid">
                <label class="gw-add-field ${c.source === "agent_platform" || c.source === "gemini_aistudio" ? "" : "gw-span-2"}">
                  <span>思维链归一（Thinking Mode）</span>
                  <select class="sel" data-gw-thinking="${escHtml(id)}">
                    <option value="auto" ${c.thinkingMode === "auto" ? "selected" : ""}>自动归并 + 切分 &lt;think&gt;（推荐）</option>
                    <option value="fields_only" ${c.thinkingMode === "fields_only" ? "selected" : ""}>仅转结构化字段（reasoning/thought）</option>
                    <option value="passthrough" ${c.thinkingMode === "passthrough" ? "selected" : ""}>原样透传不切分</option>
                  </select>
                </label>
                ${
                  c.source === "agent_platform" || c.source === "gemini_aistudio"
                    ? `<label class="gw-add-field"><span>Google 思考摘要请求策略</span><select class="sel" data-gw-policy="${escHtml(id)}">
                        <option value="follow_client" ${c.thoughtPolicy === "follow_client" ? "selected" : ""}>跟随客户端传入参数（默认）</option>
                        <option value="force_include" ${c.thoughtPolicy === "force_include" ? "selected" : ""}>对通用 Agent 默认请求思考摘要</option>
                      </select></label>`
                    : ""
                }
                <label class="gw-add-field">
                  <span>手动模型列表（每行或逗号分隔）</span>
                  <textarea class="inp gw-textarea" rows="2" data-gw-manual="${escHtml(id)}" placeholder="例如：google/gemini-3.8-flash">${escHtml((c.manualModels || []).join("\n"))}</textarea>
                </label>
                <label class="gw-add-field">
                  <span>自定义请求头（每行 Header: Value）</span>
                  <textarea class="inp gw-textarea" rows="2" data-gw-headers="${escHtml(id)}" placeholder="例如：HTTP-Referer: https://example.com">${escHtml(headersToText(c.customHeaders))}</textarea>
                </label>
              </div>
            </div>
          </details>
          <div class="gw-card-footer">
            <label class="gw-proxy-inline"><input type="checkbox" data-gw-proxy="${escHtml(id)}" ${c.systemProxy ? "checked" : ""}> 使用系统代理（默认开启）</label>
            <button class="btn p" data-gw="save-channel" data-id="${escHtml(id)}" data-gw-exclusive>保存渠道配置</button>
          </div>
        </div>
      </details>
    </article>`;
  }

  function renderAddWizard(vault, draft, hasChannels) {
    const presets = GW.presets || [];
    const kind = draft?.kind || GW_KIND;
    GW_KIND = kind;
    const vaultFilter = GW_VAULT_FILTER.trim().toLowerCase();
    const filteredVault = vault.filter(
      (v) =>
        !vaultFilter ||
        `${v.platform} ${v.label} ${v.apiEndpoint || ""} ${v.hint}`.toLowerCase().includes(vaultFilter)
    );
    const defaultProxyChecked = draft ? draft.proxy : true;
    const defaultNoKey = draft ? draft.noKey : false;

    return `<details class="gw-section gw-add-drawer" data-gw-detail="add-channel" ${!hasChannels || draft ? "open" : ""}>
      <summary class="gw-add-summary">
        <b>+ 添加上游渠道</b>
        <span>OpenAI 兼容 · Agent Platform · Gemini AI Studio</span>
      </summary>
      <div class="gw-add-drawer-body">
        <div class="gw-kind-tabs" role="tablist">
          <button type="button" class="gw-kind-tab ${kind === "openai_compat" ? "is-active" : ""}" data-gw-kind="openai_compat">
            <b>OpenAI 兼容</b><small>密钥箱 / 中转站 / Ollama</small>
          </button>
          <button type="button" class="gw-kind-tab ${kind === "agent_platform" ? "is-active" : ""}" data-gw-kind="agent_platform">
            <b>Agent Platform</b><small>Vertex · x-goog-api-key</small>
          </button>
          <button type="button" class="gw-kind-tab ${kind === "gemini_aistudio" ? "is-active" : ""}" data-gw-kind="gemini_aistudio">
            <b>Gemini AI Studio</b><small>Google 官方 · 分页模型</small>
          </button>
        </div>

        <div class="gw-add">
          <div class="gw-add-field gw-span-2">
            <span>选择密钥箱条目</span>
            <div class="gw-vault-picker gw-vault-picker-row">
              <input class="inp gw-filter-inp" id="gwVaultSearch" value="${escHtml(GW_VAULT_FILTER)}" placeholder="筛选密钥..." ${defaultNoKey ? "disabled" : ""}>
              <select class="sel" id="gwVault" ${defaultNoKey ? "disabled" : ""}>
                <option value="">选择密钥（支持同密钥多次上架）</option>
                ${filteredVault
                  .map(
                    (v) =>
                      `<option value="${escHtml(v.id)}" ${draft?.vaultId === v.id ? "selected" : ""}>${escHtml(v.platform)} · ${escHtml(v.label)} (${escHtml(v.hint)})${v.apiEndpoint ? ` — ${escHtml(v.apiEndpoint)}` : ""}</option>`
                  )
                  .join("")}
              </select>
            </div>
          </div>

          <label class="gw-add-field">
            <span>API Base URL ${kind === "openai_compat" ? "（可选中密钥自动带入）" : "（已预置）"}</span>
            <input class="inp" id="gwBaseUrl" value="${escHtml(draft?.baseUrl ?? (kind === "agent_platform" ? GW.agentPlatformDefaults?.baseUrl || "" : kind === "gemini_aistudio" ? "https://generativelanguage.googleapis.com/v1beta/openai" : ""))}" placeholder="https://api.example.com/v1">
          </label>

          <label class="gw-add-field">
            <span>英文调用别名（模型前缀）</span>
            <input class="inp" id="gwAlias" value="${escHtml(draft?.alias ?? (kind === "agent_platform" ? "agent" : kind === "gemini_aistudio" ? "gemini" : ""))}" placeholder="例如 main / agent" maxlength="32">
          </label>

          ${kind === "openai_compat" ? `<label class="gw-add-field gw-span-2">
            <span>上游 API 协议</span>
            <select class="sel" id="gwNewWire">${wireOptions(draft?.wire)}</select>
          </label>
          <p class="gw-inline-note gw-span-2" id="gwResponsesHint" ${draft?.wire === "responses" ? "" : "hidden"}>客户端继续使用 Chat Completions。上游没有 /models 时，请在高级选项中填写模型 ID。</p>` : ""}

          <details class="gw-subdetails gw-span-2" data-gw-detail="add-adv">
            <summary>高级选项 <span>${kind === "openai_compat" ? "快捷模板 · 免密钥 · " : ""}思维链策略 · 手动补充模型</span></summary>
            <div class="gw-subdetails-body">
              <div class="gw-edit-grid">
                ${
                  kind === "openai_compat"
                    ? `<label class="gw-add-field">
                        <span>快捷模板</span>
                        <select class="sel" id="gwTemplate">
                          ${presets.map((p) => `<option value="${escHtml(p.id)}" ${draft?.template === p.id ? "selected" : ""}>${escHtml(p.name)}</option>`).join("")}
                        </select>
                      </label>`
                    : `<div class="gw-add-field">
                        <span>鉴权策略</span>
                        <div class="gw-readonly-chip">${kind === "agent_platform" ? "固定发送 x-goog-api-key（不发 Bearer）" : "OpenAI 兼容 Bearer + 自动 400 修复"}</div>
                      </div>`
                }

                <label class="gw-add-field">
                  <span>思维链输出策略</span>
                  <select class="sel" id="gwNewThinkingMode">
                    <option value="auto" ${(draft?.thinkingMode || "auto") === "auto" ? "selected" : ""}>自动归并 + 切分 &lt;think&gt;（推荐）</option>
                    <option value="fields_only" ${draft?.thinkingMode === "fields_only" ? "selected" : ""}>仅转结构化字段（reasoning/thought）</option>
                    <option value="passthrough" ${draft?.thinkingMode === "passthrough" ? "selected" : ""}>原样透传</option>
                  </select>
                </label>

                ${
                  kind === "agent_platform" || kind === "gemini_aistudio"
                    ? `<label class="gw-add-field gw-span-2">
                        <span>Google 思考摘要请求策略</span>
                        <select class="sel" id="gwNewThoughtPolicy">
                          <option value="follow_client" ${(draft?.thoughtPolicy || "follow_client") === "follow_client" ? "selected" : ""}>跟随客户端（支持仅传 reasoning_effort；推荐 SillyTavern）</option>
                          <option value="force_include" ${draft?.thoughtPolicy === "force_include" ? "selected" : ""}>强制开启思考摘要（除非 reasoning_effort=none）</option>
                        </select>
                      </label>`
                    : ""
                }

                <label class="gw-add-field gw-span-2">
                  <span>手动补充模型（可选，每行或逗号分隔；无 /models 的私有端点必填）</span>
                  <textarea class="inp gw-textarea" id="gwManualModels" rows="2" placeholder="${kind === "agent_platform" ? "已内置 PocketTavern 5 个 Agent Platform 预置模型，可在此追加" : "例如：gpt-4o-mini, deepseek-reasoner"}">${escHtml(draft?.manualModels ?? "")}</textarea>
                </label>

                ${
                  kind === "openai_compat"
                    ? `<div class="gw-adv-checks gw-span-2">
                        <label class="gw-proxy-new"><input type="checkbox" id="gwNoUpstreamKey" ${defaultNoKey ? "checked" : ""}> 此端点无需上游密钥（如本地 Ollama）</label>
                        <label class="gw-proxy-new"><input type="checkbox" id="gwSaveEndpointToVault" ${draft?.saveEndpoint ? "checked" : ""} ${defaultNoKey ? "disabled" : ""}> 将端点地址写回所选密钥箱条目</label>
                      </div>`
                    : ""
                }
              </div>
            </div>
          </details>

          <div class="gw-add-flags gw-span-2">
            <label class="gw-proxy-new"><input type="checkbox" id="gwNewProxy" ${defaultProxyChecked ? "checked" : ""}> 使用系统代理（默认开启）</label>
            <button class="btn p" data-gw="publish" data-gw-exclusive>上架渠道</button>
          </div>
        </div>
        <p class="gw-footnote">密钥箱与渠道管理修改密钥后自动同步刷新；下架不会删除密钥箱中的密钥。</p>
      </div>
    </details>`;
  }

  function renderTestPlayground(channels) {
    const callableModels = [];
    for (const c of channels) {
      if (c.stale) continue;
      for (const m of c.models || []) {
        callableModels.push({ channelId: c.channelId, fullModel: m });
      }
    }
    const prevSel = byId("gwTestModel")?.value || "";
    const selectedFull =
      callableModels.find((x) => `${x.channelId}::${x.fullModel}` === prevSel) ||
      callableModels[0] ||
      null;
    const prevPrompt = byId("gwTestPrompt")?.value ?? "请简短思考，然后只回答：12 + 23 = ?";
    const prevStream = byId("gwTestStream") ? byId("gwTestStream").checked : true;
    const prevReason = byId("gwTestReasoning") ? byId("gwTestReasoning").checked : true;
    const prevEffort = byId("gwTestEffort")?.value || "";

    return `<section class="gw-section gw-card-box">
      <div class="gw-section-head"><b>在线流式与思维链联调</b><span>${callableModels.length} 个可测试模型</span></div>
      <div class="gw-playground">
        ${
          callableModels.length
            ? `<div class="gw-play-controls">
                <select class="sel" id="gwTestModel">
                  ${callableModels
                    .map((x) => {
                      const val = `${x.channelId}::${x.fullModel}`;
                      const isSel = selectedFull && `${selectedFull.channelId}::${selectedFull.fullModel}` === val;
                      return `<option value="${escHtml(val)}" ${isSel ? "selected" : ""}>${escHtml(x.fullModel)}</option>`;
                    })
                    .join("")}
                </select>
                <select class="sel" id="gwTestEffort">
                  <option value="" ${prevEffort === "" ? "selected" : ""}>思考深度：默认</option>
                  <option value="low" ${prevEffort === "low" ? "selected" : ""}>reasoning_effort = low</option>
                  <option value="medium" ${prevEffort === "medium" ? "selected" : ""}>reasoning_effort = medium</option>
                  <option value="high" ${prevEffort === "high" ? "selected" : ""}>reasoning_effort = high</option>
                  <option value="none" ${prevEffort === "none" ? "selected" : ""}>reasoning_effort = none</option>
                </select>
                <label class="gw-proxy-new"><input type="checkbox" id="gwTestStream" ${prevStream ? "checked" : ""}> 流式 SSE</label>
                <label class="gw-proxy-new"><input type="checkbox" id="gwTestReasoning" ${prevReason ? "checked" : ""}> 请求思考摘要</label>
                <button class="btn p" data-gw="run-test-chat" ${GW_TEST_BUSY ? "disabled" : ""}>${escHtml(testButtonLabel())}</button>
                <button type="button" class="btn" data-gw="cancel-test-chat" ${GW_TEST_BUSY ? "" : "hidden"}>取消</button>
              </div>
              <input class="inp gw-play-prompt" id="gwTestPrompt" value="${escHtml(prevPrompt)}" placeholder="输入测试提示词...">`
            : `<div class="gw-empty-note">请先到「渠道与接入」上架至少一个可用渠道</div>`
        }
        ${
          GW_TEST_RESULT
            ? `<div class="gw-test-output ${GW_TEST_RESULT.ok ? "is-ok" : "is-err"}">
                <div class="gw-test-meta">
                  <span>状态 <b>HTTP ${escHtml(GW_TEST_RESULT.status)}</b></span>
                  <span>耗时 <b>${escHtml(GW_TEST_RESULT.latencyMs)} ms</b>${GW_TEST_RESULT.firstByteMs != null ? `（首字 ${escHtml(GW_TEST_RESULT.firstByteMs)} ms）` : ""}</span>
                  ${GW_TEST_RESULT.finishReason ? `<span>finish_reason: <code>${escHtml(GW_TEST_RESULT.finishReason)}</code></span>` : ""}
                  ${(GW_TEST_RESULT.toolCallCount || 0) > 0 ? `<span>工具调用: <code>${escHtml(GW_TEST_RESULT.toolCallCount)} 次 (${escHtml(GW_TEST_RESULT.toolCallChars || 0)} 字)</code></span>` : ""}
                  ${GW_TEST_RESULT.promptTokens != null || GW_TEST_RESULT.completionTokens != null ? `<span>tokens: <code>${escHtml(GW_TEST_RESULT.promptTokens ?? "?")} / ${escHtml(GW_TEST_RESULT.completionTokens ?? "?")}</code></span>` : ""}
                  ${(GW_TEST_RESULT.transforms || []).length ? `<span>参数转换: <code>${escHtml(GW_TEST_RESULT.transforms.join(", "))}</code></span>` : ""}
                </div>
                ${GW_TEST_RESULT.error ? `<p class="gw-error">${escHtml(GW_TEST_RESULT.error)}</p>` : ""}
                ${GW_TEST_RESULT.diagNote ? `<div class="gw-diag-note">${escHtml(GW_TEST_RESULT.diagNote)}</div>` : ""}
                ${
                  GW_TEST_RESULT.thinking
                    ? `<details class="gw-test-think" open><summary>思维链摘要（reasoning_content · ${GW_TEST_RESULT.thinking.length} 字）</summary><pre>${escHtml(GW_TEST_RESULT.thinking)}</pre></details>`
                    : `<div class="gw-empty-note">本次响应未返回思维链（reasoning_content 为空）</div>`
                }
                ${GW_TEST_RESULT.content ? `<div class="gw-test-content"><small>正文输出（content）</small><pre>${escHtml(GW_TEST_RESULT.content)}</pre></div>` : ""}
              </div>`
            : ""
        }
      </div>
    </section>`;
  }

  function diagnosticsHtml() {
    return GW_DIAGS.length ? GW_DIAGS.map((d) => `<div class="gw-diag-row ${d.status >= 400 ? "is-err" : d.diagNote ? "is-warn" : ""}">
      <div class="gw-diag-top"><b>${escHtml(d.model)}</b><span>${escHtml(formatDiagMeta(d))}</span></div>
      ${d.transforms?.length ? `<div class="gw-diag-sub">转换：${escHtml(d.transforms.join(", "))}</div>` : ""}
      ${d.diagNote ? `<div class="gw-diag-note">${escHtml(d.diagNote)}</div>` : ""}
      ${d.error ? `<div class="gw-diag-err">${escHtml(d.error)}</div>` : ""}
    </div>`).join("") : `<div class="gw-empty-note">暂无请求记录</div>`;
  }

  function renderDiagnostics() {
    const list = byId("gatewayBox")?.querySelector(".gw-diag-list");
    if (list) list.innerHTML = diagnosticsHtml();
  }

  function renderGateway(snap, draft, uiSnap) {
    const box = byId("gatewayBox");
    if (!box || !GW) return;
    uiSnap = uiSnap || UI.snapshot(box);
    const channels = GW.channels || [];
    const vault = GW.vault || [];
    const baseLocalUrl = `http://127.0.0.1:${GW.port}/v1`;
    const allLiveModels = channels.filter((c) => !c.stale).flatMap((c) => c.models || []);
    const catFilter = GW_CATALOG_FILTER.trim().toLowerCase();
    const visibleCatalog = allLiveModels.filter((id) => !catFilter || id.toLowerCase().includes(catFilter));
    const sampleModel = exampleModel();

    const rows = channels.map((c) => renderChannelCard(c, vault)).join("");

    box.innerHTML = `<div class="gw-shell">
      <div class="gw-topbar">
        <div class="gw-topbar-info">
          <span class="gw-running ${GW.running ? "is-running" : "is-stopped"}"><i></i>${GW.running ? "运行中" : "已停止"}</span>
          <span class="gw-topbar-meta">端口 ${escHtml(GW.port)} · ${channels.length} 渠道 · ${allLiveModels.length} 模型${GW.lan ? " · 局域网已开" : ""}</span>
        </div>
        <button class="btn p" data-gw="${GW.running ? "stop" : "start"}" data-gw-exclusive>${GW.running ? "停止网关" : "启动网关"}</button>
      </div>
      ${GW.startupError ? `<p class="gw-error">启动失败：${escHtml(GW.startupError)}</p>` : ""}
      <p data-gw-sync-status hidden></p>

      <div class="gw-nav-tabs" role="tablist">
        <button type="button" class="gw-nav-tab ${GW_TAB === "channels" ? "is-active" : ""}" data-gw-tab="channels">渠道与接入 <small>${channels.length}</small></button>
        <button type="button" class="gw-nav-tab ${GW_TAB === "tools" ? "is-active" : ""}" data-gw-tab="tools">模型与联调 <small>${allLiveModels.length}</small></button>
        <button type="button" class="gw-nav-tab ${GW_TAB === "network" ? "is-active" : ""}" data-gw-tab="network">网络与示例</button>
      </div>

      <div class="gw-tab-pane" data-gw-pane="channels" ${GW_TAB === "channels" ? "" : "hidden"}>
        <section class="gw-endpoints">
          <div class="gw-endpoint-row">
            <div class="gw-endpoint-label"><span class="gw-endpoint-dot"></span><span>本机接入</span></div>
            <code>${escHtml(baseLocalUrl)}</code>
            <button class="btn" data-gw="copy-local">复制地址</button>
          </div>
          ${
            GW.lan
              ? `<div class="gw-lan">
                  <div class="gw-lan-pick">
                    <label for="gwLanIp">局域网</label>
                    ${
                      (GW.lanIps || []).length
                        ? `<select class="sel" id="gwLanIp">${GW.lanIps.map((i) => `<option value="${escHtml(i.address)}" data-iface="${escHtml(i.name)}">${escHtml(i.name)} · ${escHtml(i.address)}</option>`).join("")}</select>`
                        : `<span class="gw-no-ip">未检测到私有局域网 IPv4</span>`
                    }
                  </div>
                  ${
                    (GW.lanIps || []).length
                      ? `<div class="gw-endpoint-row"><div class="gw-endpoint-label"><span class="gw-endpoint-dot is-lan"></span><span>局域网接入</span></div><code id="gwLanEndpoint"></code><button class="btn" data-gw="copy-lan">复制地址</button></div>`
                      : ""
                  }
                </div>`
              : ""
          }
          <div class="gw-overview-actions">
            <span>访问密钥（Bearer / x-api-key）</span>
            <div>
              <button class="btn" data-gw="copy-secret">复制密钥</button>
              <button class="btn" data-gw="rotate-secret">重新生成</button>
            </div>
          </div>
        </section>

        <section class="gw-section">
          <div class="gw-section-head"><b>已上架渠道</b><span>${channels.length} 个渠道 · ${allLiveModels.length} 个模型</span></div>
          ${rows || `<div class="gw-empty"><span>尚无已上架渠道</span><small>在下方添加 OpenAI 兼容端点、Agent Platform 或 Gemini AI Studio</small></div>`}
        </section>

        ${renderAddWizard(vault, draft, channels.length > 0)}
      </div>

      <div class="gw-tab-pane" data-gw-pane="tools" ${GW_TAB === "tools" ? "" : "hidden"}>
        ${renderTestPlayground(channels)}

        <section class="gw-section gw-card-box">
          <div class="gw-section-head"><b>统一模型目录（点击复制 ID）</b><span>${allLiveModels.length} 个可调用模型</span></div>
          <div class="gw-catalog-bar">
            <input class="inp gw-filter-inp" id="gwCatalogFilter" value="${escHtml(GW_CATALOG_FILTER)}" placeholder="搜索模型 ID（如 flash / deepseek / agent）...">
          </div>
          <div class="gw-models">
            ${visibleCatalog.length ? visibleCatalog.map((id) => `<button type="button" class="gw-model-pill" data-gw="copy-model" data-model="${escHtml(id)}" title="点击复制完整模型 ID">${escHtml(id)}</button>`).join("") : `<span class="gw-empty-note">${allLiveModels.length ? "无匹配模型" : "暂无可调用模型"}</span>`}
          </div>
        </section>

        <section class="gw-section gw-card-box">
          <div class="gw-section-head">
            <b>最近请求诊断（脱敏，最多 50 条）</b>
            <div class="gw-diag-bar">
              <button type="button" class="btn" data-gw="refresh-diag">刷新</button>
              <button type="button" class="btn" data-gw="clear-diag">清空</button>
            </div>
          </div>
          <div class="gw-diag-list">${diagnosticsHtml()}</div>
        </section>
      </div>

      <div class="gw-tab-pane" data-gw-pane="network" ${GW_TAB === "network" ? "" : "hidden"}>
        <section class="gw-section">
          <div class="gw-section-head">
            <b>访问与网络设置</b>
            <button class="btn p" data-gw="apply-restart" data-gw-exclusive>${GW.running ? "应用设置并重启网关" : "保存并启动网关"}</button>
          </div>
          <div class="gw-settings-grid">
            <label class="gw-setting"><span><b>监听端口</b><small>默认 18771</small></span><input class="sel" id="gwPort" type="number" min="1" max="65535" value="${GW.port}"></label>
            <label class="gw-setting"><span><b>允许局域网访问</b><small>允许同局域网私有 IP 访问，开启时自动要求密钥</small></span><input id="gwLan" type="checkbox" ${GW.lan ? "checked" : ""}></label>
            <label class="gw-setting"><span><b>需要网关密钥</b><small>仅限本地回环不勾选时可免密；始终校验 Host 防重绑定</small></span><input id="gwAuth" type="checkbox" ${GW.auth ? "checked" : ""} ${GW.lan ? "disabled" : ""}></label>
          </div>
          ${GW.lan ? `<p class="gw-footnote">局域网访问仅限于可信网络；跨设备连不上时请放行 Windows 防火墙端口 ${GW.port}。</p>` : ""}
        </section>

        <section class="gw-section">
          <div class="gw-section-head"><b>客户端接入示例</b><span>Base URL 末尾不带斜杠</span></div>
          <div class="gw-snippets-body">
            <div class="gw-snippet-card">
              <div class="gw-snippet-head"><b>SillyTavern / PocketTavern 自定义源（Custom OpenAI）</b></div>
              <pre>Chat Completion Source: Custom (OpenAI-compatible)
Custom Endpoint (Base URL): ${escHtml(baseLocalUrl)}
API Key: [点击「渠道与接入 → 复制密钥」获取]
Model ID: ${escHtml(sampleModel)}
提示：网关已自动统一 reasoning_content 与 &lt;think&gt; 标签，可直接开启思维链折叠。</pre>
            </div>
            <div class="gw-snippet-card">
              <div class="gw-snippet-head"><b>主流 Agent / CLI（OpenCode · Cline · Cursor · OpenAI SDK）</b><button class="btn" data-gw="copy-env-snippet">复制环境变量</button></div>
              <pre id="gwEnvSnippet">OPENAI_BASE_URL=${escHtml(baseLocalUrl)}
OPENAI_API_KEY=&lt;点击复制网关密钥&gt;
# 示例模型：${escHtml(sampleModel)}</pre>
            </div>
          </div>
        </section>
      </div>
    </div>`;

    UI.bind(box);
    restoreOpenState(box, snap);
    UI.restore(box, uiSnap);
    syncGatewayButtons(box);
    wireRenderedEvents(box, channels, vault);
    renderSyncStatus();
  }

  function wireRenderedEvents(box, channels, vault) {
    const lanIp = byId("gwLanIp");
    if (lanIp) {
      const saved = localStorage.getItem("gatewayLanInterface");
      const selected = [...lanIp.options].find((o) => o.dataset.iface === saved);
      if (selected) lanIp.selectedIndex = selected.index;
      const updateAddress = () => {
        const ep = byId("gwLanEndpoint");
        if (ep) ep.textContent = `http://${lanIp.value}:${GW.port}/v1`;
      };
      lanIp.onchange = () => {
        localStorage.setItem("gatewayLanInterface", lanIp.selectedOptions[0]?.dataset.iface || "");
        updateAddress();
      };
      updateAddress();
    }

    const lan = byId("gwLan"),
      auth = byId("gwAuth");
    if (lan && auth) {
      lan.onchange = () => {
        if (lan.checked) auth.checked = true;
        auth.disabled = lan.checked;
      };
    }

    const tpl = byId("gwTemplate");
    if (tpl) {
      tpl.onchange = () => {
        const p = (GW.presets || []).find((x) => x.id === tpl.value);
        if (!p) return;
        const urlEl = byId("gwBaseUrl");
        const aliasEl = byId("gwAlias");
        if (urlEl && p.baseUrl) urlEl.value = p.baseUrl;
        if (aliasEl && (!aliasEl.value.trim() || (GW.presets || []).some((x) => x.defaultAlias === aliasEl.value.trim()))) {
          aliasEl.value = p.defaultAlias;
        }
      };
    }
    const wire = byId("gwNewWire");
    if (wire) {
      wire.onchange = () => {
        const isResponses = wire.value === "responses";
        byId("gwResponsesHint").hidden = !isResponses;
        if (isResponses) box.querySelector('details[data-gw-detail="add-adv"]').open = true;
      };
    }

    const vaultSel = byId("gwVault");
    if (vaultSel) {
      vaultSel.onchange = () => {
        const item = vault.find((v) => v.id === vaultSel.value);
        if (!item) return;
        const urlEl = byId("gwBaseUrl");
        const aliasEl = byId("gwAlias");
        if (GW_KIND === "openai_compat" && urlEl && item.apiEndpoint) {
          urlEl.value = item.apiEndpoint;
        }
        if (aliasEl && !aliasEl.value.trim()) {
          const slug = `${item.platform || ""}`
            .toLowerCase()
            .replace(/[^a-z0-9]+/g, "-")
            .replace(/^-|-$/g, "")
            .slice(0, 16);
          if (slug) aliasEl.value = slug;
        }
      };
    }

    const noKey = byId("gwNoUpstreamKey");
    if (noKey) {
      noKey.onchange = () => {
        const vEl = byId("gwVault"),
          sEl = byId("gwVaultSearch"),
          saveEl = byId("gwSaveEndpointToVault");
        if (vEl) vEl.disabled = noKey.checked;
        if (sEl) sEl.disabled = noKey.checked;
        if (saveEl) {
          if (noKey.checked) saveEl.checked = false;
          saveEl.disabled = noKey.checked;
        }
      };
    }

    const vs = byId("gwVaultSearch");
    if (vs) {
      vs.oninput = () => {
        GW_VAULT_FILTER = vs.value;
        const sel = byId("gwVault");
        if (!sel) return;
        const cur = sel.value;
        const q = GW_VAULT_FILTER.trim().toLowerCase();
        const filtered = vault.filter(
          (v) => !q || `${v.platform} ${v.label} ${v.apiEndpoint || ""} ${v.hint}`.toLowerCase().includes(q)
        );
        sel.innerHTML =
          `<option value="">选择密钥箱条目（支持同一密钥多次上架）</option>` +
          filtered
            .map(
              (v) =>
                `<option value="${escHtml(v.id)}" ${v.id === cur ? "selected" : ""}>${escHtml(v.platform)} · ${escHtml(v.label)} (${escHtml(v.hint)})${v.apiEndpoint ? ` — ${escHtml(v.apiEndpoint)}` : ""}</option>`
            )
            .join("");
      };
    }

    const catIn = byId("gwCatalogFilter");
    if (catIn) {
      catIn.oninput = () => {
        GW_CATALOG_FILTER = catIn.value;
        const allModels = channels.filter((c) => !c.stale).flatMap((c) => c.models || []);
        const q = GW_CATALOG_FILTER.trim().toLowerCase();
        const filtered = allModels.filter((m) => !q || m.toLowerCase().includes(q));
        const wrap = box.querySelector(".gw-models");
        if (wrap) {
          wrap.innerHTML = filtered.length
            ? filtered.map((id) => `<button type="button" class="gw-model-pill" data-gw="copy-model" data-model="${escHtml(id)}" title="点击复制完整模型 ID">${escHtml(id)}</button>`).join("")
            : `<span class="gw-empty-note">${allModels.length ? "无匹配模型" : "暂无可调用模型"}</span>`;
        }
      };
    }

    box.querySelectorAll("[data-gw-model-filter]").forEach((inp) => {
      inp.oninput = () => {
        const cid = inp.dataset.gwModelFilter;
        GW_MODEL_FILTERS[cid] = inp.value;
        const q = inp.value.trim().toLowerCase();
        box.querySelectorAll(`[data-gw-model="${CSS.escape ? CSS.escape(cid) : cid}"]`).forEach((cb) => {
          const lbl = cb.closest(".gw-model-check");
          if (lbl) lbl.style.display = !q || cb.value.toLowerCase().includes(q) ? "" : "none";
        });
        const card = inp.closest("[data-gw-card]");
        const empty = card.querySelector("[data-gw-model-empty]");
        empty.hidden = [...card.querySelectorAll("[data-gw-model]")].some((cb) => !q || cb.value.toLowerCase().includes(q));
      };
    });

    box.querySelectorAll("[data-gw-quick-proxy]").forEach((cb) => {
      cb.onchange = async () => {
        const id = cb.dataset.gwQuickProxy;
        const nextVal = cb.checked;
        cb.disabled = true;
        UI.observe(box);
        const submitted = UI.submitted(box, cb.parentElement);
        try {
          await writeGateway(() => invoke("gateway_update_channel", { input: { channelId: id, systemProxy: nextVal } }));
          UI.commit(box, submitted);
          notify(nextVal ? "已开启系统代理访问该渠道" : "已改为直连该渠道");
          await refreshGatewayData(true);
        } catch (err) {
          cb.checked = !nextVal;
          notify(`更新代理设置失败：${String(err)}`, true);
        } finally {
          cb.disabled = false;
        }
      };
    });
  }

  function wireBoxDelegation() {
    const box = byId("gatewayBox");
    if (!box || GW_UI_WIRED) return;
    GW_UI_WIRED = true;
    box.addEventListener("input", () => UI.observe(box));
    box.addEventListener("change", () => UI.observe(box));

    box.addEventListener("click", async (e) => {
      const navTab = e.target.closest("[data-gw-tab]");
      if (navTab) {
        GW_TAB = navTab.dataset.gwTab;
        box.querySelectorAll("[data-gw-tab]").forEach((el) => {
          el.classList.toggle("is-active", el.dataset.gwTab === GW_TAB);
        });
        box.querySelectorAll("[data-gw-pane]").forEach((pane) => {
          pane.hidden = pane.dataset.gwPane !== GW_TAB;
        });
        return;
      }

      const tab = e.target.closest("[data-gw-kind]");
      if (tab && !GW_BUSY) {
        GW_KIND = tab.dataset.gwKind;
        const snap = snapshotOpenState(box);
        const draft = captureAddDraft();
        if (draft) {
          draft.kind = GW_KIND;
          if (GW_KIND === "agent_platform") {
            draft.baseUrl = GW?.agentPlatformDefaults?.baseUrl || "";
            if (!draft.alias || draft.alias === "gemini") draft.alias = "agent";
          } else if (GW_KIND === "gemini_aistudio") {
            draft.baseUrl = "https://generativelanguage.googleapis.com/v1beta/openai";
            if (!draft.alias || draft.alias === "agent") draft.alias = "gemini";
          } else {
            const p = (GW?.presets || [])[0];
            draft.baseUrl = p?.baseUrl || "";
            if (draft.alias === "agent" || draft.alias === "gemini") draft.alias = "";
          }
        }
        renderGateway(snap, draft);
        return;
      }

      const b = e.target.closest("[data-gw]");
      if (!b) return;
      const action = b.dataset.gw;
      const exclusive = b.hasAttribute("data-gw-exclusive");
      if (exclusive) {
        if (GW_BUSY) return;
        GW_BUSY = true;
        syncGatewayButtons(box);
      }

      const runtimeWrite = action === "start" || action === "apply-restart" || action === "stop";
      try {
        if (action === "copy-local" || action === "copy-lan") {
          const ip = action === "copy-local" ? "127.0.0.1" : byId("gwLanIp")?.value;
          if (!ip) throw new Error("未选择局域网网卡");
          await invoke("copy_text", { text: `http://${ip}:${GW.port}/v1` });
          notify("接入地址已复制（末尾无斜杠）");
          return;
        }
        if (action === "copy-model") {
          await invoke("copy_text", { text: b.dataset.model || "" });
          notify(`已复制模型：${b.dataset.model}`);
          return;
        }
        if (action === "copy-env-snippet") {
          const key = await invoke("gateway_secret", { rotate: false });
          const sample = exampleModel();
          const txt = `OPENAI_BASE_URL=http://127.0.0.1:${GW.port}/v1\nOPENAI_API_KEY=${key}\n# 示例模型：${sample}`;
          await invoke("copy_text", { text: txt });
          notify("Agent 环境变量已复制");
          return;
        }
        if (action === "copy-secret" || action === "rotate-secret") {
          if (action === "rotate-secret" && !confirm("重新生成后旧网关密钥立即失效，继续吗？")) return;
          const text = await invoke("gateway_secret", { rotate: action === "rotate-secret" });
          await invoke("copy_text", { text });
          notify(action === "rotate-secret" ? "新网关密钥已生成并复制" : "网关密钥已复制");
          return;
        }
        if (action === "select-all-models" || action === "clear-all-models") {
          const check = action === "select-all-models";
          const card = box.querySelector(`[data-gw-card="${CSS.escape ? CSS.escape(b.dataset.id) : b.dataset.id}"]`);
          card?.querySelectorAll("[data-gw-model]").forEach((cb) => {
            cb.checked = check;
          });
          return;
        }
        if (action === "refresh-diag") {
          await refreshDiagnostics();
          return;
        }
        if (action === "clear-diag") {
          await clearDiagnostics();
          notify("诊断记录已清空");
          return;
        }
        if (action === "cancel-test-chat") {
          if (!GW_TEST_BUSY) return;
          GW_TEST_RUN_ID += 1;
          GW_TEST_BUSY = false;
          stopTestTimer(box);
          await invoke("gateway_cancel_test_chat");
          notify("已取消本次联调测试");
          return;
        }
        if (action === "run-test-chat") {
          if (GW_TEST_BUSY) return;
          const val = byId("gwTestModel")?.value || "";
          const sep = val.indexOf("::");
          if (sep <= 0) throw new Error("请选择要测试的模型");
          const channelId = val.slice(0, sep);
          const model = val.slice(sep + 2);
          const runId = ++GW_TEST_RUN_ID;
          GW_TEST_BUSY = true;
          startTestTimer(box);
          try {
            const res = await invoke("gateway_test_chat", {
              input: {
                channelId,
                model,
                prompt: byId("gwTestPrompt")?.value || "",
                stream: byId("gwTestStream")?.checked ?? true,
                includeReasoning: byId("gwTestReasoning")?.checked ?? true,
                reasoningEffort: byId("gwTestEffort")?.value || null,
              },
            });
            if (GW_TEST_RUN_ID !== runId) return;
            GW_TEST_RESULT = res;
            await refreshDiagnostics();
            if (GW_TEST_RUN_ID !== runId) return;
          } catch (err) {
            if (GW_TEST_RUN_ID !== runId) return;
            throw err;
          } finally {
            if (GW_TEST_RUN_ID === runId) {
              GW_TEST_BUSY = false;
              stopTestTimer(box);
            }
          }
          renderGateway(snapshotOpenState(box), captureAddDraft());
          return;
        }

        if (action === "start" || action === "apply-restart") {
          const port = Number(byId("gwPort")?.value);
          if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("请输入 1 到 65535 的端口");
          const lan = !!byId("gwLan")?.checked;
          const auth = lan || !!byId("gwAuth")?.checked;
          const submitted = UI.submitted(box, box.querySelector(".gw-settings-grid"));
          if (GW.running) {
            await writeGateway(() => invoke("gateway_restart", { port, lan, auth }));
            notify("已应用设置并重启网关");
          } else {
            await writeGateway(() => invoke("gateway_start", { port, lan, auth }));
            notify("网关已启动");
          }
          UI.commit(box, submitted);
          await refreshGatewayData(true);
          return;
        }
        if (action === "stop") {
          await writeGateway(() => invoke("gateway_stop"));
          notify("网关已停止");
          await refreshGatewayData(true);
          return;
        }
        if (action === "remove") {
          const ch = (GW.channels || []).find((x) => (x.channelId || x.vaultId) === b.dataset.id);
          if (!confirm(`确定下架渠道「${ch?.alias || b.dataset.id}」？不会删除密钥箱中的密钥。`)) return;
          await writeGateway(() => invoke("gateway_unpublish", { channelId: b.dataset.id }));
          notify("渠道已下架");
          await refreshGatewayData(true);
          return;
        }
        if (action === "refresh") {
          await writeGateway(() => invoke("gateway_refresh_channel", { channelId: b.dataset.id }));
          notify("模型列表已刷新");
          await refreshGatewayData(true);
          return;
        }
        if (action === "test-channel") {
          const res = await invoke("gateway_test_channel", { channelId: b.dataset.id });
          if (res.ok) notify(`连通正常 · ${res.latencyMs}ms${res.fetchedCount != null ? ` · ${res.fetchedCount} 个模型` : ""}`);
          else notify(`连通异常：${res.error || "未知错误"}`, true);
          return;
        }
        if (action === "save-channel") {
          const id = b.dataset.id;
          const card = box.querySelector(`[data-gw-card="${CSS.escape ? CSS.escape(id) : id}"]`);
          if (!card) return;
          const manualModels = parseManualModels(card.querySelector("[data-gw-manual]")?.value || "");
          const customHeaders = parseHeadersText(card.querySelector("[data-gw-headers]")?.value || "");
          const ch = (GW.channels || []).find((x) => (x.channelId || x.vaultId) === id);
          const wire = card.querySelector("[data-gw-wire]")?.value;
          const fetched = wire === "responses" && manualModels.length ? [] : ch?.fetchedModels || [];
          const allAvail = new Set([...fetched, ...manualModels]);
          const checkedModels = [...card.querySelectorAll("[data-gw-model]")].filter((x) => x.checked && allAvail.has(x.value)).map((x) => x.value);
          // 新增的手动模型默认纳入可见清单
          for (const mm of manualModels) {
            if (!(ch?.manualModels || []).includes(mm) && !checkedModels.includes(mm)) checkedModels.push(mm);
          }
          const payload = {
            channelId: id,
            wire,
            alias: card.querySelector("[data-gw-alias]")?.value || "",
            baseUrl: card.querySelector("[data-gw-baseurl]")?.value || "",
            systemProxy: !!card.querySelector("[data-gw-proxy]")?.checked,
            thinkingMode: card.querySelector("[data-gw-thinking]")?.value || "auto",
            thoughtPolicy: card.querySelector("[data-gw-policy]")?.value || undefined,
            manualModels,
            customHeaders,
            allowedModels: checkedModels,
          };
          const vSel = card.querySelector("[data-gw-vault-edit]");
          if (vSel) payload.vaultId = vSel.value;
          const submitted = UI.submitted(box, card);
          await writeGateway(() => invoke("gateway_update_channel", { input: payload }));
          UI.commit(box, submitted);
          notify("渠道配置已保存");
          await refreshGatewayData(true);
          return;
        }
        if (action === "publish") {
          const noKey = !!byId("gwNoUpstreamKey")?.checked && GW_KIND === "openai_compat";
          const vaultId = noKey ? "" : byId("gwVault")?.value || "";
          const alias = byId("gwAlias")?.value || "";
          const baseUrl = byId("gwBaseUrl")?.value || "";
          if (!noKey && !vaultId) throw new Error("请先在密钥箱中选择一条密钥（或勾选免上游密钥）");
          if (!alias.trim()) throw new Error("请填写英文调用别名");
          const input = {
            vaultId,
            noUpstreamKey: noKey,
            source: GW_KIND,
            wire: byId("gwNewWire")?.value,
            template: GW_KIND === "openai_compat" ? byId("gwTemplate")?.value || "custom" : "",
            baseUrl,
            saveEndpointToVault: !!byId("gwSaveEndpointToVault")?.checked,
            alias,
            systemProxy: !!byId("gwNewProxy")?.checked,
            manualModels: parseManualModels(byId("gwManualModels")?.value || ""),
            thinkingMode: byId("gwNewThinkingMode")?.value || "auto",
            thoughtPolicy: byId("gwNewThoughtPolicy")?.value || "follow_client",
          };
          const submitted = UI.submitted(box, box.querySelector('[data-gw-detail="add-channel"]'));
          const published = await writeGateway(() => invoke("gateway_publish", { input }));
          UI.commit(box, submitted);
          notify("渠道已上架");
          await refreshGatewayData({ clearAdd: true }); // canonical-publish
        }
      } catch (err) {
        notify(String(err), true);
        if (runtimeWrite) await refreshGatewayData(true);
      } finally {
        if (exclusive) {
          GW_BUSY = false;
          // 操作期间可能已重绘，按忙碌状态更新当前页面上的按钮。
          syncGatewayButtons(box);
        }
      }
    });
  }

  window.loadGateway = async function loadGateway() {
    wireBoxDelegation();
    const subscription = listenGatewaySync();
    await refreshGatewayData();
    await subscription;
  };
})();
