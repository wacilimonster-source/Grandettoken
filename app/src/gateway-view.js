/* 本地模型网关视图：三类上游接入、密钥箱多上架、代理开关（默认开启）、模型目录、在线联调与诊断。 */
(() => {
  let GW = null;
  let GW_BUSY = false;
  let GW_KIND = "openai_compat";
  let GW_CATALOG_FILTER = "";
  let GW_VAULT_FILTER = "";
  let GW_MODEL_FILTERS = {};
  let GW_TEST_RESULT = null;
  let GW_TEST_BUSY = false;
  let GW_DIAGS = [];
  let GW_UI_WIRED = false;

  const byId = (id) => document.getElementById(id);
  const escHtml = (v) =>
    typeof esc === "function"
      ? esc(String(v ?? ""))
      : String(v ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

  function notify(msg, isErr) {
    if (typeof showToast === "function") showToast(msg, !!isErr);
  }

  function sourceBadge(kind) {
    if (kind === "agent_platform") return { cls: "is-agent", text: "Agent Platform" };
    if (kind === "gemini_aistudio") return { cls: "is-gemini", text: "Gemini AI Studio" };
    return { cls: "is-openai", text: "OpenAI 兼容" };
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

  async function refreshGatewayData(preserveAddDraft) {
    const box = byId("gatewayBox");
    if (!box) return;
    const snap = snapshotOpenState(box);
    const draft = preserveAddDraft ? captureAddDraft() : null;
    try {
      const [status, diags] = await Promise.all([
        invoke("gateway_status"),
        invoke("gateway_diagnostics").catch(() => []),
      ]);
      GW = status;
      GW_DIAGS = Array.isArray(diags) ? diags : [];
      renderGateway(snap, draft);
    } catch (e) {
      box.innerHTML = `<div class="empty">读取网关失败：${escHtml(String(e))}</div>`;
    }
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
    const avail = c.availableModels || [];
    const allowedSet = c.allowedModels ? new Set(c.allowedModels) : null;
    const enabledCount = avail.filter((m) => !allowedSet || allowedSet.has(m)).length;
    const filter = (GW_MODEL_FILTERS[id] || "").toLowerCase();
    const visibleModels = avail.filter((m) => !filter || m.toLowerCase().includes(filter));
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
        <button class="btn" data-gw="test-channel" data-id="${escHtml(id)}" ${GW_BUSY ? "disabled" : ""}>测连通</button>
        <button class="btn" data-gw="refresh" data-id="${escHtml(id)}" ${GW_BUSY ? "disabled" : ""}>刷新模型</button>
        <button class="btn" data-gw="remove" data-id="${escHtml(id)}" ${GW_BUSY ? "disabled" : ""}>下架</button>
      </div>
      <details class="gw-details" data-gw-detail="ch-${escHtml(id)}">
        <summary>渠道配置 · 模型与思维链 <span>别名 / 端点 / 手动模型 / 勾选(${enabledCount}/${avail.length})</span></summary>
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
            ${
              c.noUpstreamKey
                ? `<div class="gw-add-field"><span>鉴权模式</span><div class="gw-readonly-chip">免上游密钥（不附带 Authorization）</div></div>`
                : `<label class="gw-add-field"><span>关联密钥箱条目</span><select class="sel" data-gw-vault-edit="${escHtml(id)}">${vault
                    .map((v) => `<option value="${escHtml(v.id)}" ${v.id === c.vaultId ? "selected" : ""}>${escHtml(v.platform)} · ${escHtml(v.label)} (${escHtml(v.hint)})</option>`)
                    .join("")}</select></label>`
            }
            <label class="gw-add-field">
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
          </div>
          <div class="gw-edit-grid">
            <label class="gw-add-field">
              <span>手动模型列表（每行或逗号分隔，无 /models 接口或新模型可在此增补）</span>
              <textarea class="inp gw-textarea" rows="3" data-gw-manual="${escHtml(id)}" placeholder="例如：google/gemini-3.8-flash">${escHtml((c.manualModels || []).join("\n"))}</textarea>
            </label>
            <label class="gw-add-field">
              <span>自定义请求头（可选，每行 Header: Value）</span>
              <textarea class="inp gw-textarea" rows="3" data-gw-headers="${escHtml(id)}" placeholder="例如：HTTP-Referer: https://example.com">${escHtml(headersToText(c.customHeaders))}</textarea>
            </label>
          </div>
          <div class="gw-model-config">
            <div class="gw-model-config-head">
              <b>可见模型清单（点击右侧保存后生效）</b>
              <div class="gw-model-filter-row">
                <input class="inp gw-filter-inp" data-gw-model-filter="${escHtml(id)}" value="${escHtml(GW_MODEL_FILTERS[id] || "")}" placeholder="筛选模型...">
                <button type="button" class="btn" data-gw="select-all-models" data-id="${escHtml(id)}">全选</button>
                <button type="button" class="btn" data-gw="clear-all-models" data-id="${escHtml(id)}">全不选</button>
              </div>
            </div>
            <div class="gw-model-checklist">
              ${
                visibleModels.length
                  ? visibleModels
                      .map(
                        (m) => `<label class="gw-model-check">
                          <input type="checkbox" data-gw-model="${escHtml(id)}" value="${escHtml(m)}" ${!allowedSet || allowedSet.has(m) ? "checked" : ""}>
                          <span>${escHtml(m)}</span>
                        </label>`
                      )
                      .join("")
                  : `<div class="gw-empty-note">${avail.length ? "没有匹配的模型" : "暂无模型，请在上方手动模型中补充后保存"}</div>`
              }
            </div>
          </div>
          <div class="gw-card-footer">
            <label class="gw-proxy-inline"><input type="checkbox" data-gw-proxy="${escHtml(id)}" ${c.systemProxy ? "checked" : ""}> 使用系统代理（默认开启）</label>
            <button class="btn p" data-gw="save-channel" data-id="${escHtml(id)}" ${GW_BUSY ? "disabled" : ""}>保存渠道配置</button>
          </div>
        </div>
      </details>
    </article>`;
  }

  function renderAddWizard(vault, draft) {
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

    return `<section class="gw-section gw-add-section">
      <div class="gw-section-head">
        <b>添加上游渠道</b>
        <span>三类接入统一转为 OpenAI 格式 · 支持密钥箱多端点复用</span>
      </div>
      <div class="gw-kind-tabs" role="tablist">
        <button type="button" class="gw-kind-tab ${kind === "openai_compat" ? "is-active" : ""}" data-gw-kind="openai_compat">
          <b>OpenAI 兼容端点</b><small>密钥箱 / 中转站 / DeepSeek / Ollama</small>
        </button>
        <button type="button" class="gw-kind-tab ${kind === "agent_platform" ? "is-active" : ""}" data-gw-kind="agent_platform">
          <b>Agent Platform</b><small>Vertex OpenAI · x-goog-api-key</small>
        </button>
        <button type="button" class="gw-kind-tab ${kind === "gemini_aistudio" ? "is-active" : ""}" data-gw-kind="gemini_aistudio">
          <b>Gemini AI Studio</b><small>Google 官方接口 · 分页与思考摘要</small>
        </button>
      </div>

      <div class="gw-add">
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
                <div class="gw-readonly-chip">${kind === "agent_platform" ? "固定发送 x-goog-api-key（绝不发 Bearer）" : "OpenAI 兼容 Bearer + 自动 400 修复"}</div>
              </div>`
        }

        <label class="gw-add-field">
          <span>搜索并选择密钥箱条目</span>
          <div class="gw-vault-picker">
            <input class="inp gw-filter-inp" id="gwVaultSearch" value="${escHtml(GW_VAULT_FILTER)}" placeholder="按平台 / 名称 / 端点筛选密钥..." ${defaultNoKey ? "disabled" : ""}>
            <select class="sel" id="gwVault" ${defaultNoKey ? "disabled" : ""}>
              <option value="">选择密钥箱条目（支持同一密钥多次上架）</option>
              ${filteredVault
                .map(
                  (v) =>
                    `<option value="${escHtml(v.id)}" ${draft?.vaultId === v.id ? "selected" : ""}>${escHtml(v.platform)} · ${escHtml(v.label)} (${escHtml(v.hint)})${v.apiEndpoint ? ` — ${escHtml(v.apiEndpoint)}` : ""}</option>`
                )
                .join("")}
            </select>
          </div>
        </label>

        <label class="gw-add-field gw-span-2">
          <span>API Base URL ${kind === "openai_compat" ? "（支持自动补全 /v1，选中密钥箱条目会自动带入）" : "（已预置官方地址，可按需修改）"}</span>
          <input class="inp" id="gwBaseUrl" value="${escHtml(draft?.baseUrl ?? (kind === "agent_platform" ? GW.agentPlatformDefaults?.baseUrl || "" : kind === "gemini_aistudio" ? "https://generativelanguage.googleapis.com/v1beta/openai" : ""))}" placeholder="例如：https://api.example.com/v1">
        </label>

        <label class="gw-add-field">
          <span>英文调用别名（前缀：别名/模型名）</span>
          <input class="inp" id="gwAlias" value="${escHtml(draft?.alias ?? (kind === "agent_platform" ? "agent" : kind === "gemini_aistudio" ? "gemini" : ""))}" placeholder="例如 main / deepseek / agent" maxlength="32">
        </label>

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
                  <option value="follow_client" ${(draft?.thoughtPolicy || "follow_client") === "follow_client" ? "selected" : ""}>跟随客户端 include_reasoning / reasoning_effort（推荐给 SillyTavern）</option>
                  <option value="force_include" ${draft?.thoughtPolicy === "force_include" ? "selected" : ""}>强制开启思考摘要（除非 reasoning_effort=none，推荐给通用 Agent）</option>
                </select>
              </label>`
            : ""
        }

        <label class="gw-add-field gw-span-2">
          <span>手动补充模型（可选，每行或逗号分隔；无 /models 的私有端点必填）</span>
          <textarea class="inp gw-textarea" id="gwManualModels" rows="2" placeholder="${kind === "agent_platform" ? "已内置 PocketTavern 5 个 Agent Platform 预置模型，可在此追加更多模型 ID" : "例如：gpt-4o-mini, deepseek-reasoner"}">${escHtml(draft?.manualModels ?? "")}</textarea>
        </label>

        <div class="gw-add-flags gw-span-2">
          <label class="gw-proxy-new"><input type="checkbox" id="gwNewProxy" ${defaultProxyChecked ? "checked" : ""}> 使用系统代理（默认开启）</label>
          ${
            kind === "openai_compat"
              ? `<label class="gw-proxy-new"><input type="checkbox" id="gwNoUpstreamKey" ${defaultNoKey ? "checked" : ""}> 此端点无需上游密钥（如本地 Ollama）</label>
                 <label class="gw-proxy-new"><input type="checkbox" id="gwSaveEndpointToVault" ${draft?.saveEndpoint ? "checked" : ""} ${defaultNoKey ? "disabled" : ""}> 将端点地址写回所选密钥箱条目</label>`
              : ""
          }
          <button class="btn p" data-gw="publish" ${GW_BUSY ? "disabled" : ""}>上架渠道</button>
        </div>
      </div>
      <p class="gw-footnote">密钥箱与渠道管理页修改密钥后，已关联该密钥的网关渠道会自动同步刷新；下架不会删除密钥箱中的密钥。</p>
    </section>`;
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

    return `<details class="gw-section gw-model-catalog" data-gw-detail="test-playground" ${GW_TEST_RESULT ? "open" : ""}>
      <summary><span>在线流式与思维链联调（验证 SillyTavern / Agent 输出）</span><small>${callableModels.length} 个可测试模型</small></summary>
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
                <button class="btn p" data-gw="run-test-chat" ${GW_TEST_BUSY ? "disabled" : ""}>${GW_TEST_BUSY ? "请求中..." : "发送测试"}</button>
              </div>
              <input class="inp gw-play-prompt" id="gwTestPrompt" value="${escHtml(prevPrompt)}" placeholder="输入测试提示词...">`
            : `<div class="gw-empty-note">请先上架至少一个包含可用模型的渠道</div>`
        }
        ${
          GW_TEST_RESULT
            ? `<div class="gw-test-output ${GW_TEST_RESULT.ok ? "is-ok" : "is-err"}">
                <div class="gw-test-meta">
                  <span>状态 <b>HTTP ${escHtml(GW_TEST_RESULT.status)}</b></span>
                  <span>耗时 <b>${escHtml(GW_TEST_RESULT.latencyMs)} ms</b></span>
                  ${GW_TEST_RESULT.finishReason ? `<span>finish_reason: <code>${escHtml(GW_TEST_RESULT.finishReason)}</code></span>` : ""}
                  ${(GW_TEST_RESULT.transforms || []).length ? `<span>参数转换: <code>${escHtml(GW_TEST_RESULT.transforms.join(", "))}</code></span>` : ""}
                </div>
                ${GW_TEST_RESULT.error ? `<p class="gw-error">${escHtml(GW_TEST_RESULT.error)}</p>` : ""}
                ${
                  GW_TEST_RESULT.thinking
                    ? `<details class="gw-test-think" open><summary>思维链摘要（reasoning_content · ${GW_TEST_RESULT.thinking.length} 字，SillyTavern 将自动折叠展示）</summary><pre>${escHtml(GW_TEST_RESULT.thinking)}</pre></details>`
                    : `<div class="gw-empty-note">本次响应未返回思维链（reasoning_content 为空）</div>`
                }
                ${GW_TEST_RESULT.content ? `<div class="gw-test-content"><small>正文输出（content）</small><pre>${escHtml(GW_TEST_RESULT.content)}</pre></div>` : ""}
              </div>`
            : ""
        }
      </div>
    </details>`;
  }

  function renderGateway(snap, draft) {
    const box = byId("gatewayBox");
    if (!box || !GW) return;
    const channels = GW.channels || [];
    const vault = GW.vault || [];
    const baseLocalUrl = `http://127.0.0.1:${GW.port}/v1`;
    const allLiveModels = channels.filter((c) => !c.stale).flatMap((c) => c.models || []);
    const catFilter = GW_CATALOG_FILTER.trim().toLowerCase();
    const visibleCatalog = allLiveModels.filter((id) => !catFilter || id.toLowerCase().includes(catFilter));
    const sampleModel = allLiveModels[0] || "alias/model";

    const rows = channels.map((c) => renderChannelCard(c, vault)).join("");

    box.innerHTML = `<div class="gw-shell">
      <section class="gw-overview">
        <div class="gw-overview-head">
          <div>
            <b>网关状态</b>
            <span class="gw-running ${GW.running ? "is-running" : "is-stopped"}"><i></i>${GW.running ? "运行中" : "已停止"}</span>
          </div>
          <div class="gw-head-btns">
            <button class="btn p" data-gw="${GW.running ? "stop" : "start"}" ${GW_BUSY ? "disabled" : ""}>${GW.running ? "停止网关" : "启动网关"}</button>
          </div>
        </div>
        ${GW.startupError ? `<p class="gw-error">启动失败：${escHtml(GW.startupError)}</p>` : ""}
        <div class="gw-endpoints">
          <div class="gw-section-head"><b>统一接入地址（SillyTavern / PocketTavern 自定义源填此处，末尾无斜杠）</b></div>
          <div class="gw-endpoint-row">
            <div class="gw-endpoint-label"><span class="gw-endpoint-dot"></span><span>本机 OpenAI Base URL</span></div>
            <code>${escHtml(baseLocalUrl)}</code>
            <button class="btn" data-gw="copy-local">复制地址</button>
          </div>
          ${
            GW.lan
              ? `<div class="gw-lan">
                  <label for="gwLanIp">局域网网卡</label><span class="gw-lan-hint">同局域网设备（如 PocketTavern 手机端）连接用</span>
                  ${
                    (GW.lanIps || []).length
                      ? `<select class="sel" id="gwLanIp">${GW.lanIps.map((i) => `<option value="${escHtml(i.address)}" data-iface="${escHtml(i.name)}">${escHtml(i.name)} · ${escHtml(i.address)}</option>`).join("")}</select>
                         <div class="gw-endpoint-row"><div class="gw-endpoint-label"><span class="gw-endpoint-dot is-lan"></span><span>局域网 Base URL</span></div><code id="gwLanEndpoint"></code><button class="btn" data-gw="copy-lan">复制地址</button></div>`
                      : `<p class="gw-no-ip">未检测到可用的私有局域网 IPv4 地址</p>`
                  }
                </div>`
              : ""
          }
        </div>
        <div class="gw-overview-actions">
          <span>网关访问密钥（支持 Bearer 与 x-api-key）</span>
          <div>
            <button class="btn" data-gw="copy-secret">复制密钥</button>
            <button class="btn" data-gw="rotate-secret">重新生成</button>
          </div>
        </div>
        <details class="gw-snippets" data-gw-detail="snippets">
          <summary>SillyTavern / Agent 配置示例 <span>查看环境变量与自定义源填写方式</span></summary>
          <div class="gw-snippets-body">
            <div class="gw-snippet-card">
              <div class="gw-snippet-head"><b>SillyTavern / PocketTavern 自定义源（Custom OpenAI）</b></div>
              <pre>Chat Completion Source: Custom (OpenAI-compatible)
Custom Endpoint (Base URL): ${escHtml(baseLocalUrl)}
API Key: [点击上方「复制密钥」获取]
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
        </details>
      </section>

      <section class="gw-section">
        <div class="gw-section-head">
          <b>访问与网络设置</b>
          <button class="btn" data-gw="apply-restart" ${GW_BUSY ? "disabled" : ""}>${GW.running ? "应用设置并重启网关" : "保存并启动网关"}</button>
        </div>
        <div class="gw-settings-grid">
          <label class="gw-setting"><span><b>监听端口</b><small>默认 18771</small></span><input class="sel" id="gwPort" type="number" min="1" max="65535" value="${GW.port}"></label>
          <label class="gw-setting"><span><b>允许局域网访问</b><small>允许同局域网私有 IP 访问，开启时自动要求密钥</small></span><input id="gwLan" type="checkbox" ${GW.lan ? "checked" : ""}></label>
          <label class="gw-setting"><span><b>需要网关密钥</b><small>仅限本地回环不勾选时可免密；始终校验 Host 防重绑定</small></span><input id="gwAuth" type="checkbox" ${GW.auth ? "checked" : ""} ${GW.lan ? "disabled" : ""}></label>
        </div>
        ${GW.lan ? `<p class="gw-footnote">局域网访问仅限于可信网络；跨设备连不上时请放行 Windows 防火墙端口 ${GW.port}。</p>` : ""}
      </section>

      <section class="gw-section">
        <div class="gw-section-head"><b>已上架渠道</b><span>${channels.length} 个渠道 · 共 ${allLiveModels.length} 个可调用模型</span></div>
        ${rows || `<div class="gw-empty"><span>尚无已上架渠道</span><small>在下方选择 OpenAI 兼容端点、Agent Platform 或 Gemini AI Studio 即可统一暴露</small></div>`}
      </section>

      ${renderAddWizard(vault, draft)}

      <details class="gw-section gw-model-catalog" data-gw-detail="catalog" open>
        <summary><span>网关统一模型目录（点击模型 ID 即可复制）</span><small>${allLiveModels.length} 个可调用模型</small></summary>
        <div class="gw-catalog-bar">
          <input class="inp gw-filter-inp" id="gwCatalogFilter" value="${escHtml(GW_CATALOG_FILTER)}" placeholder="搜索模型 ID（如 flash / deepseek / agent）...">
        </div>
        <div class="gw-models">
          ${visibleCatalog.length ? visibleCatalog.map((id) => `<button type="button" class="gw-model-pill" data-gw="copy-model" data-model="${escHtml(id)}" title="点击复制完整模型 ID">${escHtml(id)}</button>`).join("") : `<span class="gw-empty-note">${allLiveModels.length ? "无匹配模型" : "暂无可调用模型"}</span>`}
        </div>
      </details>

      ${renderTestPlayground(channels)}

      <details class="gw-section gw-model-catalog" data-gw-detail="diagnostics">
        <summary><span>最近请求诊断（脱敏，最多 50 条）</span><small>${GW_DIAGS.length} 条记录</small></summary>
        <div class="gw-diag-bar">
          <button type="button" class="btn" data-gw="refresh-diag">刷新日志</button>
          <button type="button" class="btn" data-gw="clear-diag">清空</button>
        </div>
        <div class="gw-diag-list">
          ${
            GW_DIAGS.length
              ? GW_DIAGS.map((d) => `<div class="gw-diag-row ${d.status >= 400 ? "is-err" : ""}">
                  <div class="gw-diag-top"><b>${escHtml(d.model)}</b><span>HTTP ${escHtml(d.status)} · ${escHtml(d.latencyMs)}ms · ${d.stream ? "SSE" : "JSON"} · 思考 ${escHtml(d.thinkingChars)}字 / 正文 ${escHtml(d.contentChars)}字</span></div>
                  ${d.transforms?.length ? `<div class="gw-diag-sub">转换：${escHtml(d.transforms.join(", "))}</div>` : ""}
                  ${d.error ? `<div class="gw-diag-err">${escHtml(d.error)}</div>` : ""}
                </div>`).join("")
              : `<div class="gw-empty-note">暂无请求记录</div>`
          }
        </div>
      </details>
    </div>`;

    wireRenderedEvents(box, channels, vault);
    restoreOpenState(box, snap);
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
      };
    });

    box.querySelectorAll("[data-gw-quick-proxy]").forEach((cb) => {
      cb.onchange = async () => {
        const id = cb.dataset.gwQuickProxy;
        const nextVal = cb.checked;
        cb.disabled = true;
        try {
          await invoke("gateway_update_channel", { input: { channelId: id, systemProxy: nextVal } });
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

    box.addEventListener("click", async (e) => {
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
          const sample = (GW.channels || []).filter((c) => !c.stale).flatMap((c) => c.models || [])[0] || "alias/model";
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
          GW_DIAGS = await invoke("gateway_diagnostics");
          renderGateway(snapshotOpenState(box), captureAddDraft());
          return;
        }
        if (action === "clear-diag") {
          await invoke("gateway_clear_diagnostics");
          GW_DIAGS = [];
          renderGateway(snapshotOpenState(box), captureAddDraft());
          notify("诊断记录已清空");
          return;
        }
        if (action === "run-test-chat") {
          if (GW_TEST_BUSY) return;
          const val = byId("gwTestModel")?.value || "";
          const sep = val.indexOf("::");
          if (sep <= 0) throw new Error("请选择要测试的模型");
          const channelId = val.slice(0, sep);
          const model = val.slice(sep + 2);
          GW_TEST_BUSY = true;
          b.disabled = true;
          b.textContent = "请求中...";
          try {
            GW_TEST_RESULT = await invoke("gateway_test_chat", {
              input: {
                channelId,
                model,
                prompt: byId("gwTestPrompt")?.value || "",
                stream: byId("gwTestStream")?.checked ?? true,
                includeReasoning: byId("gwTestReasoning")?.checked ?? true,
                reasoningEffort: byId("gwTestEffort")?.value || null,
              },
            });
            GW_DIAGS = await invoke("gateway_diagnostics").catch(() => GW_DIAGS);
          } finally {
            GW_TEST_BUSY = false;
          }
          renderGateway(snapshotOpenState(box), captureAddDraft());
          return;
        }

        if (GW_BUSY) return;
        GW_BUSY = true;
        b.disabled = true;

        if (action === "start" || action === "apply-restart") {
          const port = Number(byId("gwPort")?.value);
          if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("请输入 1 到 65535 的端口");
          const lan = !!byId("gwLan")?.checked;
          const auth = lan || !!byId("gwAuth")?.checked;
          if (GW.running) {
            await invoke("gateway_restart", { port, lan, auth });
            notify("已应用设置并重启网关");
          } else {
            await invoke("gateway_start", { port, lan, auth });
            notify("网关已启动");
          }
          await refreshGatewayData(true);
          return;
        }
        if (action === "stop") {
          await invoke("gateway_stop");
          notify("网关已停止");
          await refreshGatewayData(true);
          return;
        }
        if (action === "remove") {
          const ch = (GW.channels || []).find((x) => (x.channelId || x.vaultId) === b.dataset.id);
          if (!confirm(`确定下架渠道「${ch?.alias || b.dataset.id}」？不会删除密钥箱中的密钥。`)) return;
          await invoke("gateway_unpublish", { channelId: b.dataset.id });
          notify("渠道已下架");
          await refreshGatewayData(true);
          return;
        }
        if (action === "refresh") {
          await invoke("gateway_refresh_channel", { channelId: b.dataset.id });
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
          const allAvail = new Set([...(ch?.fetchedModels || []), ...manualModels]);
          const checkedModels = [...card.querySelectorAll("[data-gw-model]")].filter((x) => x.checked && allAvail.has(x.value)).map((x) => x.value);
          // 新增的手动模型默认纳入可见清单
          for (const mm of manualModels) {
            if (!(ch?.manualModels || []).includes(mm) && !checkedModels.includes(mm)) checkedModels.push(mm);
          }
          const payload = {
            channelId: id,
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
          await invoke("gateway_update_channel", { input: payload });
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
            template: GW_KIND === "openai_compat" ? byId("gwTemplate")?.value || "custom" : "",
            baseUrl,
            saveEndpointToVault: !!byId("gwSaveEndpointToVault")?.checked,
            alias,
            systemProxy: !!byId("gwNewProxy")?.checked,
            manualModels: parseManualModels(byId("gwManualModels")?.value || ""),
            thinkingMode: byId("gwNewThinkingMode")?.value || "auto",
            thoughtPolicy: byId("gwNewThoughtPolicy")?.value || "follow_client",
          };
          GW = await invoke("gateway_publish", { input });
          notify("渠道已上架");
          renderGateway(snapshotOpenState(box), null);
        }
      } catch (err) {
        notify(String(err), true);
      } finally {
        GW_BUSY = false;
        b.disabled = false;
      }
    });
  }

  window.loadGateway = async function loadGateway() {
    wireBoxDelegation();
    await refreshGatewayData(false);
  };
})();
