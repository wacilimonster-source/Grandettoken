/* 本地网关进阶功能面板：虚拟路由组、用量账本、双协议接入与 8 客户端配置、可选 Prompt 脱敏。 */
(() => {
  const DEFAULT_MASKING = Object.freeze({
    enabled: false,
    sticky: true,
    maskApiKeys: true,
    maskEmails: true,
    maskPrivateKeys: true,
    maskPasswords: true,
    customPatterns: [],
  });

  const STRATEGY_LABELS = {
    order: "顺序优先（故障自动切备）",
    rotate: "会话轮询（Rotate）",
    expiring: "7日临期 / DeepSeek 谷时优先",
    pace: "余量充裕优先（Pace）",
  };

  const state = {
    groupDraft: null,
    usageDays: 7,
    usageSummary: null,
    usageError: null,
    usageLoading: false,
    usageTicket: 0,
    clientsData: null,
    drift: [],
    clientsError: null,
    clientsLoading: false,
    clientsTicket: 0,
    preview: null,
    reports: [],
    latestGw: null,
  };

  let lastHelpers = null;

  function escHtml(v) {
    if (lastHelpers?.esc) return lastHelpers.esc(v);
    return String(v ?? "").replace(/[&<>"']/g, (c) => ({
      "&": "&amp;",
      "<": "&lt;",
      ">": "&gt;",
      '"': "&quot;",
      "'": "&#39;",
    }[c]));
  }

  function notify(msg, isErr) {
    if (lastHelpers?.notify) lastHelpers.notify(msg, isErr);
    else if (typeof showToast === "function") showToast(msg, !!isErr);
  }

  function fmtTok(n) {
    if (n === null || n === undefined) return "—";
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + "M";
    if (n >= 1_000) return (n / 1_000).toFixed(1) + "K";
    return String(n);
  }

  function fmtTs(ts) {
    if (!ts) return "—";
    const d = new Date(ts * 1000);
    const pad = (x) => String(x).padStart(2, "0");
    return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }

  function collectCandidateModels(gw) {
    const out = [];
    const seen = new Set();
    for (const ch of gw?.channels || []) {
      const eff = ch.availableModels || [];
      const list = ch.allowedModels?.length ? eff.filter((m) => ch.allowedModels.includes(m)) : eff;
      const models = list.length ? list : eff;
      for (const m of models) {
        const key = `${ch.channelId}::${m}`;
        if (!seen.has(key)) {
          seen.add(key);
          out.push({
            key,
            channelId: ch.channelId,
            alias: ch.alias,
            model: m,
            fullId: `${ch.alias}/${m}`,
          });
        }
      }
    }
    return out;
  }

  function defaultGroupDraft(gw) {
    const cands = collectCandidateModels(gw);
    return {
      editingId: null,
      id: "",
      name: "",
      strategy: "order",
      sticky: true,
      stickyTtlSecs: 1800,
      cooldownSecs: 60,
      targets: cands.length ? [{ channelId: cands[0].channelId, model: cands[0].model }] : [],
    };
  }

  function ensureDraft(gw) {
    if (!state.groupDraft) state.groupDraft = defaultGroupDraft(gw);
    return state.groupDraft;
  }

  function normalizeMasking(raw) {
    return { ...DEFAULT_MASKING, ...(raw && typeof raw === "object" ? raw : {}) };
  }

  function syncDraftFromDom(box) {
    if (!state.groupDraft) return;
    const idEl = box.querySelector("#gwGrpId");
    const nameEl = box.querySelector("#gwGrpName");
    const strEl = box.querySelector("#gwGrpStrategy");
    const stickyEl = box.querySelector("#gwGrpSticky");
    const ttlEl = box.querySelector("#gwGrpStickyTtl");
    const cdEl = box.querySelector("#gwGrpCooldown");
    if (idEl && !idEl.readOnly) state.groupDraft.id = idEl.value;
    if (nameEl) state.groupDraft.name = nameEl.value;
    if (strEl) state.groupDraft.strategy = strEl.value;
    if (stickyEl) state.groupDraft.sticky = stickyEl.checked;
    if (ttlEl) state.groupDraft.stickyTtlSecs = Number(ttlEl.value) || 1800;
    if (cdEl) state.groupDraft.cooldownSecs = Number(cdEl.value) || 60;
  }

  function renderGroupsPane(box, gw) {
    const pane = box.querySelector('[data-gw-pane="groups"]');
    if (!pane) return;
    const groups = gw.groups || [];
    const cooldowns = gw.cooldowns || [];
    const cands = collectCandidateModels(gw);
    const channelsById = new Map((gw.channels || []).map((c) => [c.channelId, c]));
    const draft = ensureDraft(gw);

    const cdHtml = cooldowns.length
      ? `<section class="gw-card-box">
          <div class="gw-section-head"><b>冷却中候选（自动跳过直到恢复）</b><span>${cooldowns.length} 个冷却条目</span></div>
          <div class="gw-adv-chips">${cooldowns.map((cd) => {
            const ch = channelsById.get(cd.channelId);
            const name = ch ? `${ch.alias}/${cd.model}` : `${cd.channelId}/${cd.model}`;
            return `<span class="gw-badge is-err">${escHtml(name)} · 剩余 ${escHtml(cd.remainingSecs)}s · ${escHtml(cd.reason)}</span>`;
          }).join("")}</div>
        </section>`
      : "";

    const listHtml = groups.length
      ? groups.map((gr) => {
          const targets = (gr.targets || []).map((t, i) => {
            const ch = channelsById.get(t.channelId);
            const label = ch ? `${ch.alias}/${t.model}` : `${t.channelId}/${t.model}`;
            const isCooling = cooldowns.some((cd) => cd.channelId === t.channelId && cd.model === t.model);
            return `<span class="gw-target-pill ${isCooling ? "is-cooling" : ""}">${i + 1}. ${escHtml(label)}${isCooling ? " (冷却中)" : ""}</span>`;
          }).join("");
          return `<article class="gw-card-box gw-adv-group-card">
            <div class="gw-adv-card-head">
              <div>
                <b>group/${escHtml(gr.id)}</b>
                ${gr.name && gr.name !== gr.id ? `<span class="gw-badge">${escHtml(gr.name)}</span>` : ""}
                <span class="gw-badge is-agent">${escHtml(STRATEGY_LABELS[gr.strategy] || gr.strategy)}</span>
                ${gr.sticky ? `<span class="gw-badge">Sticky ${escHtml(gr.stickyTtlSecs)}s</span>` : `<span class="gw-badge">无 Sticky</span>`}
                <span class="gw-badge">冷却 ${escHtml(gr.cooldownSecs)}s</span>
              </div>
              <div class="gw-channel-actions">
                <button type="button" class="btn" data-gw-adv="copy-group" data-group-id="${escHtml(gr.id)}">复制 group/${escHtml(gr.id)}</button>
                <button type="button" class="btn" data-gw-adv="edit-group" data-group-id="${escHtml(gr.id)}">编辑</button>
                <button type="button" class="btn bad" data-gw-adv="delete-group" data-group-id="${escHtml(gr.id)}">删除</button>
              </div>
            </div>
            <div class="gw-adv-chips">${targets || `<span class="gw-empty-note">暂无成员</span>`}</div>
          </article>`;
        }).join("")
      : `<div class="gw-empty"><span>尚无虚拟路由组</span><small>在下方新建一个路由组后，外部客户端只要填 <code>group/&lt;id&gt;</code> 即可自动容灾切换</small></div>`;

    const targetRows = (draft.targets || []).map((t, idx) => {
      const ch = channelsById.get(t.channelId);
      const label = ch ? `${ch.alias}/${t.model}` : `${t.channelId}/${t.model}`;
      return `<div class="gw-adv-target-row">
        <span class="gw-adv-idx">#${idx + 1}</span>
        <code>${escHtml(label)}</code>
        <div class="gw-channel-actions">
          <button type="button" class="btn" data-gw-adv="move-target-up" data-idx="${idx}" ${idx === 0 ? "disabled" : ""}>上移</button>
          <button type="button" class="btn" data-gw-adv="move-target-down" data-idx="${idx}" ${idx + 1 >= draft.targets.length ? "disabled" : ""}>下移</button>
          <button type="button" class="btn bad" data-gw-adv="remove-target" data-idx="${idx}">移除</button>
        </div>
      </div>`;
    }).join("");

    const candOptions = cands.length
      ? cands.map((c) => `<option value="${escHtml(c.key)}">${escHtml(c.fullId)}</option>`).join("")
      : `<option value="">请先在“渠道与接入”中添加上游渠道</option>`;

    pane.innerHTML = `
      ${cdHtml}
      <section class="gw-section">
        <div class="gw-section-head"><b>虚拟路由组（${groups.length} 个）</b><span>统一暴露为 <code>group/&lt;id&gt;</code>，首字节前遇 429/5xx/超时自动切换并冷却</span></div>
        ${listHtml}
      </section>
      <section class="gw-card-box">
        <div class="gw-section-head">
          <b>${draft.editingId ? `编辑路由组：group/${escHtml(draft.editingId)}` : "新建虚拟路由组"}</b>
          ${draft.editingId ? `<button type="button" class="btn" data-gw-adv="reset-group">切回新建</button>` : ""}
        </div>
        <div class="gw-adv-grid">
          <label><span>路由组 ID（英文，用于 group/&lt;id&gt;）</span><input class="inp" id="gwGrpId" value="${escHtml(draft.id)}" ${draft.editingId ? "readonly" : ""} placeholder="例如 coding / claude"></label>
          <label><span>显示名称</span><input class="inp" id="gwGrpName" value="${escHtml(draft.name)}" placeholder="例如 主力编码池"></label>
          <label><span>选路策略</span>
            <select class="sel" id="gwGrpStrategy">
              <option value="order" ${draft.strategy === "order" ? "selected" : ""}>order · 顺序优先</option>
              <option value="rotate" ${draft.strategy === "rotate" ? "selected" : ""}>rotate · 轮询负载均衡</option>
              <option value="expiring" ${draft.strategy === "expiring" ? "selected" : ""}>expiring · 7日临期/DeepSeek谷时优先</option>
              <option value="pace" ${draft.strategy === "pace" ? "selected" : ""}>pace · 剩余额度比例最高优先</option>
            </select>
          </label>
          <label><span>粘性 TTL（秒，60~86400）</span><input class="inp" id="gwGrpStickyTtl" type="number" min="60" max="86400" value="${escHtml(draft.stickyTtlSecs)}"></label>
          <label><span>默认冷却时间（秒，5~600）</span><input class="inp" id="gwGrpCooldown" type="number" min="5" max="600" value="${escHtml(draft.cooldownSecs)}"></label>
          <label class="gw-adv-check"><input type="checkbox" id="gwGrpSticky" ${draft.sticky ? "checked" : ""}><span>同会话 Sticky 粘性命中同一上游</span></label>
        </div>
        <div class="gw-adv-targets">
          <div class="gw-section-head"><b>候选成员列表（按优先级排列）</b></div>
          ${targetRows || `<div class="gw-empty-note">尚未选择候选成员</div>`}
          <div class="gw-adv-add-row">
            <select class="sel" id="gwGrpAddPick" ${cands.length ? "" : "disabled"}>${candOptions}</select>
            <button type="button" class="btn" data-gw-adv="add-target" ${cands.length ? "" : "disabled"}>+ 添加候选</button>
          </div>
        </div>
        <div class="gw-adv-actions">
          <button type="button" class="btn p" data-gw-adv="save-group" ${cands.length ? "" : "disabled"}>保存路由组</button>
        </div>
      </section>`;
  }

  function renderUsagePane(box) {
    const pane = box.querySelector('[data-gw-pane="usage"]');
    if (!pane) return;
    const s = state.usageSummary;
    const hitPct = s?.cacheHitRate != null ? `${(s.cacheHitRate * 100).toFixed(1)}%` : "—";
    const costTxt = s?.estimatedCostCny != null ? `¥${Number(s.estimatedCostCny).toFixed(4)}` : "—";

    const byChannelRows = (s?.byChannel || []).map((c) => `<tr>
      <td>${escHtml(c.label || c.key)}</td>
      <td>${escHtml(c.requests)} (${escHtml(c.errors)}错)</td>
      <td>${escHtml(fmtTok(c.promptTokens))}</td>
      <td>${escHtml(fmtTok(c.cachedPromptTokens))}</td>
      <td>${escHtml(fmtTok(c.completionTokens))}</td>
      <td>${c.estimatedCostCny != null ? `¥${Number(c.estimatedCostCny).toFixed(4)}` : "—"}</td>
    </tr>`).join("");

    const byModelRows = (s?.byModel || []).map((m) => `<tr>
      <td><code>${escHtml(m.key)}</code></td>
      <td>${escHtml(m.requests)}</td>
      <td>${escHtml(fmtTok(m.cachedPromptTokens))} / ${escHtml(fmtTok(m.promptTokens))}</td>
      <td>${escHtml(fmtTok(m.reasoningTokens))}</td>
      <td>${m.estimatedCostCny != null ? `¥${Number(m.estimatedCostCny).toFixed(4)}` : "—"}</td>
    </tr>`).join("");

    const recentRows = (s?.recent || []).slice(0, 15).map((r) => `<tr>
      <td>${escHtml(fmtTs(r.ts))}</td>
      <td><code>${escHtml(r.model)}</code>${r.routeGroup ? ` <small>(${escHtml(r.routeGroup)})</small>` : ""}</td>
      <td>${escHtml(r.channelAlias)}</td>
      <td>${r.agent ? escHtml(r.agent) : "—"}</td>
      <td><span class="gw-badge ${r.status >= 400 ? "is-err" : ""}">${escHtml(r.status)}</span></td>
      <td>${escHtml(r.latencyMs)}ms${r.firstByteMs != null ? ` / ${escHtml(r.firstByteMs)}ms` : ""}</td>
      <td>${escHtml(fmtTok(r.cachedPromptTokens))}/${escHtml(fmtTok(r.promptTokens))}/${escHtml(fmtTok(r.completionTokens))}</td>
      <td>${r.failoverCount > 0 ? `切备${r.failoverCount} ` : ""}${r.maskedCount > 0 ? `脱敏${r.maskedCount}` : ""}</td>
    </tr>`).join("");

    pane.innerHTML = `
      <section class="gw-card-box">
        <div class="gw-section-head">
          <b>网关用量与缓存账本（仅元数据，保留 90 天）</b>
          <div class="gw-channel-actions">
            ${[1, 7, 30].map((d) => `<button type="button" class="btn ${state.usageDays === d ? "p" : ""}" data-gw-adv="usage-days" data-days="${d}">${d}天</button>`).join("")}
            <button type="button" class="btn" data-gw-adv="refresh-usage">刷新</button>
            <button type="button" class="btn bad" data-gw-adv="clear-usage">清空</button>
          </div>
        </div>
        ${state.usageLoading && !s ? `<p class="gw-empty-note">正在读取 SQLite 台账…</p>` : ""}
        ${state.usageError ? `<p class="gw-error">读取账本失败：${escHtml(state.usageError)}</p>` : ""}
        ${s ? `
          <div class="gw-adv-kpi-grid">
            <div class="gw-adv-kpi"><span>请求总数</span><b>${escHtml(s.totalRequests)}</b><small>成功 ${escHtml(s.okRequests)} · 失败 ${escHtml(s.errorRequests)}</small></div>
            <div class="gw-adv-kpi"><span>缓存命中率</span><b>${escHtml(hitPct)}</b><small>命中 ${escHtml(fmtTok(s.cachedPromptTokens))} / 写 ${escHtml(fmtTok(s.cacheCreationTokens))}</small></div>
            <div class="gw-adv-kpi"><span>首字 TTFT (P50/P95)</span><b>${s.ttftP50Ms ?? "—"} / ${s.ttftP95Ms ?? "—"} ms</b><small>总输出 ${escHtml(fmtTok(s.completionTokens))} · 思考 ${escHtml(fmtTok(s.reasoningTokens))}</small></div>
            <div class="gw-adv-kpi"><span>容灾切备 / 估算费用</span><b>${escHtml(s.failoverRequests)} 次 / ${escHtml(costTxt)}</b><small>${s.pricedRequests > 0 ? `已按 4SAPI 公开目录价估算 ${s.pricedRequests} 条` : "自定义渠道暂无目录单价"}</small></div>
          </div>
        ` : ""}
      </section>
      ${s ? `
        <section class="gw-card-box">
          <div class="gw-section-head"><b>按渠道统计</b></div>
          <div class="gw-adv-table-wrap"><table class="gw-adv-table"><thead><tr><th>渠道</th><th>请求</th><th>输入</th><th>缓存命中</th><th>输出</th><th>估算</th></tr></thead><tbody>${byChannelRows || `<tr><td colspan="6" class="gw-empty-note">暂无记录</td></tr>`}</tbody></table></div>
        </section>
        <section class="gw-card-box">
          <div class="gw-section-head"><b>按模型统计</b></div>
          <div class="gw-adv-table-wrap"><table class="gw-adv-table"><thead><tr><th>模型</th><th>请求</th><th>缓存 / 输入</th><th>思考 Token</th><th>估算</th></tr></thead><tbody>${byModelRows || `<tr><td colspan="5" class="gw-empty-note">暂无记录</td></tr>`}</tbody></table></div>
        </section>
        <section class="gw-card-box">
          <div class="gw-section-head"><b>最近请求流水（不含 Prompt/密钥正文）</b></div>
          <div class="gw-adv-table-wrap"><table class="gw-adv-table"><thead><tr><th>时间</th><th>模型</th><th>上游</th><th>Agent</th><th>状态</th><th>耗时/TTFT</th><th>缓存/输入/输出</th><th>标记</th></tr></thead><tbody>${recentRows || `<tr><td colspan="8" class="gw-empty-note">暂无记录</td></tr>`}</tbody></table></div>
        </section>
      ` : ""}`;
  }

  function collectInjectTargets(gw) {
    const out = [];
    for (const gr of gw?.groups || []) {
      out.push({ value: `group::${gr.id}`, modelId: `group/${gr.id}`, isGroup: true, label: `路由组 · group/${gr.id}（${gr.name || gr.id}）` });
    }
    for (const ch of gw?.channels || []) {
      if (ch.stale) continue;
      for (const m of ch.models || []) {
        out.push({ value: `model::${m}`, modelId: m, isGroup: false, label: `模型 · ${m}` });
      }
    }
    return out;
  }

  function renderConnectPane(box, gw) {
    const pane = box.querySelector('[data-gw-pane="connect"]');
    if (!pane) return;
    const oaiUrl = gw.endpoints?.openaiBaseUrl || `http://127.0.0.1:${gw.port}/v1`;
    const anthUrl = gw.endpoints?.anthropicBaseUrl || `http://127.0.0.1:${gw.port}`;
    const targets = collectInjectTargets(gw);
    const masking = normalizeMasking(gw.masking);
    const clients = state.clientsData?.clients || [];
    const logs = (state.clientsData?.log || []).slice(0, 6);

    const targetOpts = targets.length
      ? targets.map((t) => `<option value="${escHtml(t.value)}" ${state.preview?.targetValue === t.value ? "selected" : ""}>${escHtml(t.label)}</option>`).join("")
      : `<option value="">请先发布渠道或新建路由组</option>`;

    const clientsHtml = clients.length
      ? clients.map((c) => {
          const gwModels = (c.models || []).filter((m) => m.source === "gateway");
          return `<div class="gw-adv-client-row">
            <div class="gw-adv-client-info">
              <b>${escHtml(c.label)}</b>
              <span class="gw-badge ${c.installed ? "is-gemini" : ""}">${c.installed ? "已检测到" : "未检测到"}</span>
              ${c.running ? `<span class="gw-badge is-err">运行中（${escHtml(c.process)}）</span>` : ""}
              ${(state.drift || []).filter((d) => d.client === c.client).map((d) => `<span class="gw-badge is-err">配置被外部改写</span><small class="gw-adv-client-note">${escHtml(d.message)}（点「预览接入」后确认写入可修复）</small>`).join("")}
              ${gwModels.length ? `<span class="gw-badge is-agent">已接入：${escHtml(gwModels.map((m) => m.id).join(", "))}</span>` : ""}
              ${c.note ? `<small class="gw-adv-client-note">${escHtml(c.note)}</small>` : ""}
            </div>
            <button type="button" class="btn" data-gw-adv="preview-inject" data-client="${escHtml(c.client)}" ${targets.length && c.installed ? "" : "disabled"}>预览接入</button>
          </div>`;
        }).join("")
      : `<div class="gw-empty-note">${state.clientsLoading ? "正在检测本地客户端与 CLI（Claude Code / Codex / OpenCode / DSH 等）…" : state.clientsError ? `检测失败：${escHtml(state.clientsError)}` : "点击“刷新检测”扫描 8 个客户端"}</div>`;

    const prev = state.preview;
    const prevHtml = prev
      ? `<div class="gw-adv-preview-box">
          <div class="gw-section-head">
            <b>接入写入预告：${escHtml(prev.client)} ← ${escHtml(prev.modelId)}</b>
            <button type="button" class="btn" data-gw-adv="close-preview">收起</button>
          </div>
          ${(prev.items || []).map((item) => `
            <div class="gw-adv-preview-item">
              <div><b>${escHtml(item.label)}</b> · ${escHtml(item.action)}</div>
              ${item.source ? `<small>它现在跑在：${escHtml(item.source)}</small>` : ""}
              ${item.files?.length ? `<small>目标文件：<code>${escHtml(item.files.join(", "))}</code></small>` : ""}
              ${item.runningWarning ? `<small class="gw-adv-client-note">${escHtml(item.runningWarning)} 仍可写入，点确认时会再问一次。</small>` : ""}
              ${item.blocked ? `<p class="gw-error">阻止写入：${escHtml(item.blocked)}</p>` : ""}
              ${(item.notes || []).map((n) => `<small class="gw-adv-client-note">${escHtml(n)}</small>`).join("")}
            </div>
          `).join("")}
          <div class="gw-adv-actions">
            <button type="button" class="btn p" data-gw-adv="apply-inject" ${prev.items.every((i) => i.blocked || !i.changed) ? "disabled" : ""}>确认写入</button>
            ${prev.items.some((i) => i.configured) ? `<button type="button" class="btn bad" data-gw-adv="remove-inject">取消写入（移除网关配置）</button>` : ""}
          </div>
        </div>`
      : "";

    const reportHtml = state.reports.length
      ? `<div class="gw-adv-report-box">${state.reports.map((r) => `
          <div class="${r.error ? "gw-error" : "gw-diag-row"}">
            <b>${escHtml(r.label)}</b>：${r.error ? escHtml(r.error) : `新增 ${r.added} / 更新 ${r.updated} / 移除 ${r.removed} · ${escHtml(r.effect)}`}
            ${r.backup ? `<small>（备份：${escHtml(r.backup)}）</small>` : ""}
            ${(r.notes || []).map((n) => `<small class="gw-adv-client-note">${escHtml(n)}</small>`).join("")}
          </div>`).join("")}</div>`
      : "";

    const logHtml = logs.length
      ? `<div class="gw-adv-log-list">${logs.map((r) => `<div class="gw-adv-log-row">
          <span>${escHtml(fmtTs(r.at))} · ${escHtml(r.label)} · <code>${escHtml(r.model)}</code></span>
          <button type="button" class="btn" data-gw-adv="rollback-inject" data-backup="${escHtml(r.backupPath)}" ${r.backupOk ? "" : "disabled"}>${r.backupOk ? "回滚备份" : "备份已清理"}</button>
        </div>`).join("")}</div>`
      : "";

    pane.innerHTML = `
      <section class="gw-endpoints">
        <div class="gw-section-head"><b>双协议本机入口</b><span>仅暴露本地回环地址，真实上游密钥留在 Windows 凭据管理器</span></div>
        <div class="gw-endpoint-row">
          <div class="gw-endpoint-label"><span class="gw-endpoint-dot"></span><span>OpenAI Chat /v1</span></div>
          <code>${escHtml(oaiUrl)}</code>
          <button type="button" class="btn" data-gw-adv="copy-val" data-val="${escHtml(oaiUrl)}">复制</button>
        </div>
        <div class="gw-endpoint-row">
          <div class="gw-endpoint-label"><span class="gw-endpoint-dot is-lan"></span><span>Anthropic /v1/messages</span></div>
          <code>${escHtml(anthUrl)}</code>
          <button type="button" class="btn" data-gw-adv="copy-val" data-val="${escHtml(anthUrl)}">复制</button>
        </div>
        <div class="gw-snippet-card">
          <div class="gw-snippet-head"><b>Claude Code 快速接入环境变量</b><button type="button" class="btn" data-gw-adv="copy-claude-env">复制 Claude Code 配置</button></div>
          <pre>ANTHROPIC_BASE_URL=${escHtml(anthUrl)}
ANTHROPIC_AUTH_TOKEN=&lt;网关访问密钥&gt;
ANTHROPIC_MODEL=${escHtml((gw.groupModels || [])[0] || "alias/model")}</pre>
        </div>
      </section>

      <section class="gw-card-box">
        <div class="gw-section-head">
          <b>客户端「通过本地网关接入」（8 个支持目标）</b>
          <button type="button" class="btn" data-gw-adv="refresh-clients">刷新检测</button>
        </div>
        <p class="gw-footnote">只写本地网关地址与网关令牌，绝不向外部配置文件写入上游密钥；每次写入前先做 DPAPI 加密备份并支持一键回滚。</p>
        <div class="gw-adv-add-row">
          <select class="sel" id="gwInjectTarget" ${targets.length ? "" : "disabled"}>${targetOpts}</select>
        </div>
        <div class="gw-adv-client-list">${clientsHtml}</div>
        ${prevHtml}
        ${reportHtml}
        ${logHtml}
      </section>

      <section class="gw-card-box">
        <div class="gw-section-head">
          <b>可选 Sticky Prompt 脱敏（默认关闭）</b>
          <span class="gw-badge ${masking.enabled ? "is-agent" : ""}">${masking.enabled ? "已开启" : "已关闭（默认）"}</span>
        </div>
        <p class="gw-footnote">开启后在发往上游前把 API Key、邮箱、私钥等替换成同会话固定占位符（如 [MASKED_KEY_01]），不破坏 Prompt Cache，响应流自动还原；映射仅存内存，不写库不进日志。</p>
        <div class="gw-adv-checks-col">
          <label class="gw-adv-check"><input type="checkbox" id="gwMaskEnabled" ${masking.enabled ? "checked" : ""}><span>启用请求前 Prompt 脱敏</span></label>
          <label class="gw-adv-check"><input type="checkbox" id="gwMaskSticky" ${masking.sticky ? "checked" : ""}><span>同会话 Sticky 占位符映射</span></label>
          <label class="gw-adv-check"><input type="checkbox" id="gwMaskKeys" ${masking.maskApiKeys ? "checked" : ""}><span>脱敏 API Key / Bearer Token</span></label>
          <label class="gw-adv-check"><input type="checkbox" id="gwMaskEmails" ${masking.maskEmails ? "checked" : ""}><span>脱敏邮箱地址</span></label>
          <label class="gw-adv-check"><input type="checkbox" id="gwMaskPriv" ${masking.maskPrivateKeys ? "checked" : ""}><span>脱敏 PEM / OpenSSH 私钥</span></label>
          <label class="gw-adv-check"><input type="checkbox" id="gwMaskPwd" ${masking.maskPasswords ? "checked" : ""}><span>脱敏 password / secret 字段</span></label>
          <label><span>自定义敏感模式（每行一条正则或文本，至少 3 字符）</span>
            <textarea class="inp" id="gwMaskPatterns" rows="2" placeholder="例如 internal-secret-pattern">${escHtml((masking.customPatterns || []).join("\n"))}</textarea>
          </label>
        </div>
        <div class="gw-adv-actions">
          <button type="button" class="btn p" data-gw-adv="save-masking">保存脱敏设置</button>
        </div>
      </section>`;
  }

  function restoreMaskingDom(box, masking) {
    const m = normalizeMasking(masking);
    const set = (sel, val) => { const el = box.querySelector(sel); if (el) el.checked = !!val; };
    set("#gwMaskEnabled", m.enabled);
    set("#gwMaskSticky", m.sticky);
    set("#gwMaskKeys", m.maskApiKeys);
    set("#gwMaskEmails", m.maskEmails);
    set("#gwMaskPriv", m.maskPrivateKeys);
    set("#gwMaskPwd", m.maskPasswords);
    const pat = box.querySelector("#gwMaskPatterns");
    if (pat) pat.value = (m.customPatterns || []).join("\n");
  }

  async function loadUsage(box) {
    const ticket = ++state.usageTicket;
    state.usageLoading = true;
    state.usageError = null;
    renderUsagePane(box);
    try {
      const summary = await invoke("gateway_usage_summary", { days: state.usageDays, recentLimit: 40 });
      if (ticket !== state.usageTicket) return;
      state.usageSummary = summary;
    } catch (err) {
      if (ticket !== state.usageTicket) return;
      state.usageError = String(err);
      notify(`读取网关用量账本失败：${state.usageError}`, true);
    } finally {
      if (ticket === state.usageTicket) {
        state.usageLoading = false;
        renderUsagePane(box);
      }
    }
  }

  async function loadClients(box) {
    const ticket = ++state.clientsTicket;
    state.clientsLoading = true;
    state.clientsError = null;
    if (state.latestGw) renderConnectPane(box, state.latestGw);
    try {
      const [data, drift] = await Promise.all([
        invoke("gateway_inject_clients"),
        invoke("gateway_inject_drift").catch(() => []),
      ]);
      if (ticket !== state.clientsTicket) return;
      state.clientsData = data;
      state.drift = drift || [];
    } catch (err) {
      if (ticket !== state.clientsTicket) return;
      state.clientsError = String(err);
      notify(`检测客户端失败：${state.clientsError}`, true);
    } finally {
      if (ticket === state.clientsTicket && state.latestGw) {
        state.clientsLoading = false;
        renderConnectPane(box, state.latestGw);
      }
    }
  }

  function wireDelegation(box) {
    if (!box || box.dataset.gwAdvWired === "1") return;
    box.dataset.gwAdvWired = "1";

    box.addEventListener("click", async (e) => {
      const btn = e.target.closest("[data-gw-adv]");
      if (!btn || !state.latestGw) return;
      const act = btn.dataset.gwAdv;
      const gw = state.latestGw;

      try {
        if (act === "copy-group") {
          const gid = btn.dataset.groupId || "";
          await invoke("copy_text", { text: `group/${gid}` });
          notify(`已复制：group/${gid}`);
          return;
        }
        if (act === "copy-val") {
          await invoke("copy_text", { text: btn.dataset.val || "" });
          notify("已复制到剪贴板");
          return;
        }
        if (act === "copy-claude-env") {
          const key = await invoke("gateway_secret", { rotate: false });
          const anth = gw.endpoints?.anthropicBaseUrl || `http://127.0.0.1:${gw.port}`;
          const model = (gw.groupModels || [])[0] || (gw.channels || []).find((c) => !c.stale && c.models?.length)?.models?.[0] || "alias/model";
          await invoke("copy_text", {
            text: `ANTHROPIC_BASE_URL=${anth}\nANTHROPIC_AUTH_TOKEN=${key}\nANTHROPIC_MODEL=${model}`,
          });
          notify("Claude Code 环境变量已复制");
          return;
        }
        if (act === "edit-group") {
          const gr = (gw.groups || []).find((x) => x.id === btn.dataset.groupId);
          if (!gr) return;
          state.groupDraft = {
            editingId: gr.id,
            id: gr.id,
            name: gr.name || "",
            strategy: gr.strategy || "order",
            sticky: gr.sticky !== false,
            stickyTtlSecs: gr.stickyTtlSecs || 1800,
            cooldownSecs: gr.cooldownSecs || 60,
            targets: (gr.targets || []).map((t) => ({ channelId: t.channelId, model: t.model })),
          };
          renderGroupsPane(box, gw);
          return;
        }
        if (act === "reset-group") {
          state.groupDraft = defaultGroupDraft(gw);
          renderGroupsPane(box, gw);
          return;
        }
        if (act === "add-target") {
          syncDraftFromDom(box);
          const selVal = box.querySelector("#gwGrpAddPick")?.value || "";
          const sep = selVal.indexOf("::");
          if (sep <= 0) throw new Error("请先选择一个渠道模型");
          ensureDraft(gw).targets.push({ channelId: selVal.slice(0, sep), model: selVal.slice(sep + 2) });
          renderGroupsPane(box, gw);
          return;
        }
        if (act === "move-target-up" || act === "move-target-down" || act === "remove-target") {
          syncDraftFromDom(box);
          const idx = Number(btn.dataset.idx);
          const arr = ensureDraft(gw).targets;
          if (!Number.isInteger(idx) || idx < 0 || idx >= arr.length) return;
          if (act === "remove-target") arr.splice(idx, 1);
          else if (act === "move-target-up" && idx > 0) [arr[idx - 1], arr[idx]] = [arr[idx], arr[idx - 1]];
          else if (act === "move-target-down" && idx + 1 < arr.length) [arr[idx], arr[idx + 1]] = [arr[idx + 1], arr[idx]];
          renderGroupsPane(box, gw);
          return;
        }
        if (act === "save-group") {
          syncDraftFromDom(box);
          const d = ensureDraft(gw);
          if (!d.id.trim()) throw new Error("请填写路由组 ID（例如 coding）");
          if (!d.targets.length) throw new Error("请至少添加一个候选成员");
          const input = {
            id: d.id.trim(),
            name: d.name.trim(),
            strategy: d.strategy,
            sticky: !!d.sticky,
            stickyTtlSecs: Number(d.stickyTtlSecs) || 1800,
            cooldownSecs: Number(d.cooldownSecs) || 60,
            targets: d.targets,
          };
          const next = await lastHelpers.writeGateway(() => invoke("gateway_save_group", { input }));
          state.groupDraft = defaultGroupDraft(next);
          lastHelpers.applyStatus(next);
          notify(`路由组 group/${input.id} 已保存`);
          return;
        }
        if (act === "delete-group") {
          const gid = btn.dataset.groupId || "";
          if (!confirm(`确定删除虚拟路由组 group/${gid} 吗？`)) return;
          const next = await lastHelpers.writeGateway(() => invoke("gateway_delete_group", { id: gid }));
          if (state.groupDraft?.editingId === gid) state.groupDraft = null;
          lastHelpers.applyStatus(next);
          notify(`路由组 group/${gid} 已删除`);
          return;
        }
        if (act === "usage-days") {
          state.usageDays = Number(btn.dataset.days) || 7;
          await loadUsage(box);
          return;
        }
        if (act === "refresh-usage") {
          await loadUsage(box);
          return;
        }
        if (act === "clear-usage") {
          if (!confirm("确定清空所有本地网关用量账本记录吗？此操作不可撤销。")) return;
          await invoke("gateway_clear_usage");
          state.usageSummary = null;
          await loadUsage(box);
          notify("网关用量账本已清空");
          return;
        }
        if (act === "refresh-clients") {
          await loadClients(box);
          return;
        }
        if (act === "preview-inject") {
          const client = btn.dataset.client || "";
          const targetVal = box.querySelector("#gwInjectTarget")?.value || "";
          const sep = targetVal.indexOf("::");
          if (sep <= 0) throw new Error("请先选择要接入的模型或路由组");
          const kind = targetVal.slice(0, sep);
          const rawModel = targetVal.slice(sep + 2);
          const isGroup = kind === "group";
          const modelId = isGroup ? `group/${rawModel}` : rawModel;
          const items = await invoke("gateway_inject_preview", { client, modelId, isGroup });
          state.preview = { client, modelId, isGroup, targetValue: targetVal, items };
          state.reports = [];
          renderConnectPane(box, gw);
          return;
        }
        if (act === "close-preview") {
          state.preview = null;
          renderConnectPane(box, gw);
          return;
        }
        if (act === "apply-inject" || act === "remove-inject") {
          if (!state.preview) throw new Error("请先点击预览接入查看改动详情");
          const { client, modelId, isGroup } = state.preview;
          // 3C：在跑不拒写，但写入前二次确认（magpie 口径 + 我们的确认门）
          const warn = (state.preview.items || []).map((i) => i.runningWarning).find(Boolean);
          if (act === "apply-inject" && warn && !confirm(warn + "\n仍要写入吗？写入后需重启该工具或开新会话生效。")) return;
          const reports = await lastHelpers.writeGateway(() =>
            act === "apply-inject"
              ? invoke("gateway_inject_apply", { client, modelId, isGroup })
              : invoke("gateway_inject_remove", { client, modelId, isGroup }),
          );
          state.reports = reports || [];
          state.preview = null;
          notify(act === "apply-inject" ? "客户端网关接入配置已写入" : "已从客户端移除网关接入配置");
          await loadClients(box);
          return;
        }
        if (act === "rollback-inject") {
          const backup = btn.dataset.backup || "";
          if (!backup) return;
          const msg = await invoke("inject_rollback", { backup });
          notify(msg || "已从备份还原");
          await loadClients(box);
          return;
        }
        if (act === "save-masking") {
          const prevMask = normalizeMasking(gw.masking);
          const nextMask = {
            enabled: !!box.querySelector("#gwMaskEnabled")?.checked,
            sticky: !!box.querySelector("#gwMaskSticky")?.checked,
            maskApiKeys: !!box.querySelector("#gwMaskKeys")?.checked,
            maskEmails: !!box.querySelector("#gwMaskEmails")?.checked,
            maskPrivateKeys: !!box.querySelector("#gwMaskPriv")?.checked,
            maskPasswords: !!box.querySelector("#gwMaskPwd")?.checked,
            customPatterns: (box.querySelector("#gwMaskPatterns")?.value || "")
              .split(/\r?\n/)
              .map((s) => s.trim())
              .filter((s) => s.length >= 3),
          };
          try {
            const next = await lastHelpers.writeGateway(() => invoke("gateway_set_masking", { input: nextMask }));
            lastHelpers.applyStatus(next);
            notify(nextMask.enabled ? "已开启请求前 Sticky Prompt 脱敏" : "Prompt 脱敏已关闭");
          } catch (err) {
            restoreMaskingDom(box, prevMask);
            notify(`保存脱敏设置失败，已还原原选项：${String(err)}`, true);
          }
          return;
        }
      } catch (err) {
        notify(String(err), true);
      }
    });
  }

  window.GatewayAdvanced = {
    render(box, gw, helpers) {
      if (!box || !gw) return;
      lastHelpers = helpers || lastHelpers;
      state.latestGw = gw;
      wireDelegation(box);
      renderGroupsPane(box, gw);
      renderUsagePane(box);
      renderConnectPane(box, gw);
      const active = box.querySelector(".gw-nav-tab.is-active")?.dataset.gwTab;
      if (active) this.onTabShown(active, box, gw, helpers);
    },
    onTabShown(tab, box, gw, helpers) {
      lastHelpers = helpers || lastHelpers;
      state.latestGw = gw || state.latestGw;
      if (tab === "usage" && !state.usageLoading && !state.usageSummary) loadUsage(box);
      if (tab === "connect" && !state.clientsLoading && !state.clientsData) loadClients(box);
    },
  };
})();
