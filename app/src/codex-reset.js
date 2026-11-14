/* Codex 重置顾问:在现有 Codex 详情弹层内展开，不新开弹窗、不执行兑换。 */
(function () {
  const invoke = window.__TAURI__ && window.__TAURI__.core ? window.__TAURI__.core.invoke : null;
  const SPEEDS = [10, 14.8, 16, 18, 20, 25, 30];
  const VERDICT = {
    wait: { en: "WAIT", zh: "等自然重置" },
    now: { en: "NOW", zh: "现在要用" },
    hold: { en: "HOLD", zh: "先留着" },
    unknown: { en: "UNKNOWN", zh: "不确定" },
  };
  const WARN = {
    verify_eligibility: "请在 Codex 核对兑换资格。官方要求重置涵盖的至少一项额度剩余不足 80%。",
    burn_unknown: "本周期样本太短，消耗速度只是估算",
    low_value_now: "同一计划范围内的净增供给较少，可能为负；本次明确的使用期限仍优先。",
    cards_derived: "卡数是从明细推导的",
    count_mismatch: "接口数量与明细条数不一致，以数量为准",
    card_expiry_unknown: "有卡的到期日未知，计划没有假设它永久有效",
    card_expiry_loss: "有卡会在下次重置前到期；这次不用可能浪费",
    data_time_unknown: "数据更新时间未知",
    sub_ends_soon: "订阅不到 1 天到期",
    sub_short: "订阅不到 3 天到期",
    plan_unavailable: "缺少有效卡到期时间、订阅截止或足够用量样本，计划暂不可用",
    subscription_unknown: "订阅到期日未填，暂按订阅持续有效规划至最后一张有效卡到期；续订情况变化后请更新日期。",
    reset_imminent: "自然恢复已不足一小时；本次按必须立即使用给出建议，若能等可保留卡。",
    five_hour_demand_unknown: "本次用量按周额度填写，五小时实际需求需在 Codex 核对；其百分比与周额度分别计算。",
    weekly_plan_only: "计划仅试算周额度供给，五小时窗口可能另有限流。",
    scope_mismatch: "账号范围不一致，设置没有串用",
    amount_defaulted: "没有写用量，按 100% 算",
    need_defaulted: "需求已过期或无效，按「能等」算",
  };
  const NEEDS = [
    ["urgent", "现在就要", "这次必须马上用"],
    ["soon", "今天要用", "能等，但今天要"],
    ["patient", "能等", "等自然重置再说"],
    ["idle", "暂时不用", "只是看看，不急"],
  ];
  // 设计 §5.3 的用量档位：标了小用量，剩余够时就不会被劝去烧卡。
  const AMOUNTS = [
    [25, "少量 25%"],
    [50, "一半 50%"],
    [100, "整窗 100%"],
  ];

  const state = {
    open: false,
    tab: "advice",
    advice: null,
    loading: false,
    error: "",
    key: "",
    seq: 0,
    profileLoaded: false,
    profileError: "",
    subEndAt: null,
    need: "patient",
    needAt: null,
    amount: null,
    speed: null,
    scope: null,
    saving: false,
  };

  const esc = (v) =>
    String(v == null ? "" : v).replace(/[&<>"']/g, (c) => ({
      "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
    }[c]));

  const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
  const pct = (v) => (v == null || !Number.isFinite(Number(v)) ? "—" : `${Number(v).toFixed(1)}%`);
  const num = (v) => (v == null || !Number.isFinite(Number(v)) ? "—" : String(Math.round(Number(v))));
  const ts = (v) => Number(v) || 0;
  const nowSec = () => Math.floor(Date.now() / 1000);

  function fmtTime(t) {
    if (!t) return "—";
    try {
      return new Date(t * 1000).toLocaleString("zh-CN", {
        month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false,
      });
    } catch (_) {
      return "—";
    }
  }

  function fmtDay(t) {
    if (!t) return "—";
    try {
      return new Date(t * 1000).toLocaleDateString("zh-CN", { month: "2-digit", day: "2-digit" });
    } catch (_) {
      return "—";
    }
  }

  function fmtGap(h) {
    if (h == null || !Number.isFinite(Number(h))) return "—";
    const v = Number(h);
    if (v < 1) return `约 ${Math.max(1, Math.round(v * 60))} 分钟`;
    return `约 ${v.toFixed(1)} 小时`;
  }

  function dateValue(t) {
    if (!t) return "";
    const d = new Date(t * 1000);
    if (Number.isNaN(d.getTime())) return "";
    const y = d.getFullYear();
    const m = String(d.getMonth() + 1).padStart(2, "0");
    const day = String(d.getDate()).padStart(2, "0");
    return `${y}-${m}-${day}`;
  }

  function verdict(a) {
    return (a && VERDICT[a.verdict]) || VERDICT.unknown;
  }

  function chip(a) {
    const v = verdict(a);
    const cls = a && VERDICT[a.verdict] ? a.verdict : "unknown";
    return `<span class="cr-chip cr-chip-${cls}">${v.en}</span>`;
  }

  function cardCountText(c) {
    const r = c && c.resetCredits;
    if (!r) return "卡数未知";
    if (r.availableCount == null) return r.credits ? `${r.credits.length} 张（推导）` : "卡数未知";
    return `${r.availableCount} 张可用`;
  }

  function cardsRows(c, limit) {
    const r = c && c.resetCredits;
    const list = (r && r.credits) || [];
    if (!list.length) {
      return `<div class="cr-empty">${r && r.error ? esc(r.error) : "接口未返回逐张到期明细"}</div>`;
    }
    const shown = limit ? list.slice(0, limit) : list;
    return shown.map((card, i) => {
      const at = card.expiresAt;
      const expired = at && at * 1000 <= Date.now();
      const afterSub = at && state.subEndAt && at > state.subEndAt;
      const label = at == null ? "到期日未知" : `${fmtTime(at)} 到期`;
      return `<div class="cr-card-row">
        <span class="cr-card-title">${esc(card.title || "额度重置卡")}</span>
        <span class="cr-card-date">${esc(label)}${expired ? " · 已过期" : ""}${afterSub ? " · 比订阅晚" : ""}</span>
        ${i === 0 && at ? '<span class="cr-card-tag">最早到期</span>' : ""}
      </div>`;
    }).join("") + (limit && list.length > limit
      ? `<div class="cr-empty">还有 ${list.length - limit} 张，详情里看全部</div>`
      : "");
  }

  function compactInner(c) {
    const a = state.advice;
    const v = verdict(a);
    const headline = state.loading && !a
      ? "正在计算…"
      : state.error && !a
        ? "建议暂时不可用"
        : a ? a.headline : "正在读取周额度…";
    const meta = a
      ? `周额度 ${pct(a.metrics.weeklyRemainPercent)} · 下次重置 ${fmtTime(a.metrics.nextResetAt)} · ${cardCountText(c)}`
      : `周额度读取中 · ${cardCountText(c)}`;
    const notes = [];
    if (c.resetCredits && c.resetCredits.error) notes.push(c.resetCredits.error);
    if (c.resetCredits && c.resetCredits.lastRedeemedAt) {
      notes.push(`最近用卡重置 ${fmtTime(c.resetCredits.lastRedeemedAt)}`);
    }
    const updated = c.resetCredits && c.resetCredits.updatedAt
      ? `<div class="cr-note">查询于 ${esc(fmtTime(c.resetCredits.updatedAt))}${c.stale || !c.valid ? " · 上次快照" : ""}</div>`
      : "";
    return `
      <div class="cr-entry">
        <div class="cr-entry-top"><span class="cr-kicker">重置顾问</span>${chip(a)}</div>
        <div class="cr-entry-title">${esc(headline)}</div>
        <div class="cr-entry-meta">${esc(meta)}</div>
        <div class="cr-entry-cta">查看重置建议 <span aria-hidden="true">›</span></div>
      </div>
      <div class="cr-compact-cards">${cardsRows(c, 3)}</div>
      ${notes.map((s) => `<div class="cr-note">${esc(s)}</div>`).join("")}
      ${updated}
      <div class="cr-note">只看周额度 · 不自动兑换</div>`;
  }

  function needButtons() {
    return NEEDS.map(([key, label, desc]) => `
      <button type="button" class="cr-need${state.need === key ? " on" : ""}"
        data-codex-need="${key}" aria-pressed="${state.need === key}"${state.saving ? " disabled" : ""}>
        <b>${label}</b><span>${desc}</span>
      </button>`).join("");
  }

  function amountRow() {
    if (state.need !== "urgent" && state.need !== "soon") return "";
    return `<div class="cr-amts">
      <div class="cr-label">这次大概用多少周额度？</div>
      <div class="cr-amt-row">${AMOUNTS.map(([v, label]) => `
        <button type="button" class="cr-amt${state.amount === v ? " on" : ""}"
          data-codex-amt="${v}" aria-pressed="${state.amount === v}"${state.saving ? " disabled" : ""}>${label}</button>`).join("")}
      </div>
      <div class="cr-hint">不选按整窗 100% 保守估；标了小用量，剩余够时就不会建议用卡。</div>
    </div>`;
  }

  function needAmountText() {
    if (state.need !== "urgent" && state.need !== "soon") return "—";
    return state.amount == null ? "100%（默认）" : `${state.amount}%`;
  }

  function metricGrid(a) {
    if (!a) return "";
    const m = a.metrics || {};
    return `<div class="cr-metrics">
      <div><span>周额度剩余</span><b>${pct(m.weeklyRemainPercent)}</b></div>
      <div><span>下次自然重置</span><b>${esc(fmtTime(m.nextResetAt))}</b><small>${m.gapHours == null ? "" : esc(fmtGap(m.gapHours))}</small></div>
      <div><span>五小时额度剩余</span><b>${pct(m.fiveHourRemainPercent)}</b><small>恢复 ${esc(fmtTime(m.fiveHourResetAt))}</small></div>
      <div><span>现在用卡净收益</span><b>${m.cardGainPercent == null ? "—" : `${Number(m.cardGainPercent).toFixed(1)}%`}</b><small>同一计划范围内对比等待，按周额度估算</small></div>
      <div><span>可用重置卡</span><b>${m.cardsAvailable == null ? "未知" : `${m.cardsAvailable} 张`}</b><small>${m.cardsUnknown ? "不能劝用" : "来自当前详情"}</small></div>
      <div><span>订阅到期</span><b>${esc(fmtTime(m.subEndAt))}</b><small>${m.daysToSubEnd == null ? "未填" : `还剩 ${m.daysToSubEnd.toFixed(1)} 天`}</small></div>
      <div><span>本周期平均速度</span><b>${m.burnPercentPerDay == null ? "—" : `${Number(m.burnPercentPerDay).toFixed(1)}%/天`}</b><small>${m.burnEstimated ? "按本周期估算" : "样本不足"}</small></div>
    </div>`;
  }

  function warningsHtml(a) {
    const list = (a && a.warnings) || [];
    if (!list.length) return `<div class="cr-ok">没有额外口径提示</div>`;
    return `<ul class="cr-warns">${list.map((w) => `<li>${esc(WARN[w] || w)}</li>`).join("")}</ul>`;
  }

  function actionNote(a) {
    if (!a) return "";
    if (a.verdict === "now") {
      return `<div class="cr-action-note now">
        <b>如果决定用：去 Codex 用量页面核对涵盖范围后手动兑换。</b>
        <span>本工具只给建议，不代点、不自动兑换。</span>
      </div>`;
    }
    if (a.verdict === "wait") {
      return `<div class="cr-action-note wait">
        <b>按建议等：查看受限窗口的自然恢复时间。</b>
        <span>周额度与五小时窗口可能分别恢复。</span>
      </div>`;
    }
    return `<div class="cr-action-note hold">
      <b>先留着卡，不用为了「快过期」强行兑换。</b>
      <span>需要时再去 Codex 手动操作。</span>
    </div>`;
  }

  function advicePanel(c) {
    const a = state.advice;
    const error = state.error
      ? `<div class="cr-error"><b>建议没算出来</b><span>${esc(state.error)}</span>
          <button type="button" class="btn s" data-codex-retry>重试</button></div>`
      : "";
    if (!a) {
      return `<div class="cr-panel">${error || `<div class="cr-loading">正在计算建议…</div>`}</div>`;
    }
    return `<div class="cr-panel">
      <div class="cr-block">
        <div class="cr-label">你这次要用吗？</div>
        <div class="cr-needs">${needButtons()}</div>
        <div class="cr-hint">默认按「能等」计算；选择用量后，会结合自然重置时间与卡的有效期给出建议。</div>
        ${amountRow()}
      </div>
      <div class="cr-block">
        <label class="cr-label" for="codexResetSubEnd">订阅到期日（可选）</label>
        <div class="cr-date-row">
          <input type="date" id="codexResetSubEnd" value="${dateValue(state.subEndAt)}"
            aria-label="订阅到期日"${state.saving || state.loading ? " disabled" : ""}>
          <button type="button" class="btn s" data-codex-clear-date
            aria-label="清空订阅到期日"${!state.subEndAt || state.saving || state.loading ? " disabled" : ""}>清空</button>
          <span>可留空，按重置卡有效期规划；填写后按当日 23:59 截止，日期只存本地</span>
        </div>
        ${state.profileError ? `<div class="cr-note bad">${esc(state.profileError)}</div>` : ""}
      </div>
      ${metricGrid(a)}
      ${warningsHtml(a)}
      ${actionNote(a)}
      ${error}
    </div>`;
  }

  function timelineHtml(a) {
    if (!a) return "";
    const sub = a.metrics.planEndAt || a.metrics.subEndAt || state.subEndAt;
    if (!sub) return "";
    const endLabel = a.metrics.planEndSource === "cards" ? "最后一张卡到期" : "订阅到期";
    const now = nowSec();
    const reset = a.metrics.nextResetAt;
    const start = Math.min(now, sub - 1);
    const span = Math.max(1, sub - start);
    const rp = reset ? clamp((reset - start) / span * 100, 0, 100) : null;
    const dot = (left, cls) =>
      `<span class="cr-tl-dot ${cls}" style="left:${left}%"><i></i></span>`;
    return `<div class="cr-timeline" aria-label="从现在到${endLabel}的时间轴">
      <div class="cr-tl-line"></div>
      ${dot(0, "now")}
      ${rp == null ? "" : dot(rp, "reset")}
      ${dot(100, "end")}
    </div>
    <div class="cr-tl-legend">
      <span><b>今天</b>${esc(fmtDay(now))}</span>
      <span><b>下次自然重置</b>${esc(fmtDay(reset))}</span>
      <span><b>${endLabel}</b>${esc(fmtDay(sub))}</span>
    </div>`;
  }

  function planPanel(c) {
    const a = state.advice;
    if (!a) return `<div class="cr-panel"><div class="cr-loading">正在计算计划…</div></div>`;
    const lines = (a.planLines || []).map((s) => `<li>${esc(s)}</li>`).join("");
    const idx = state.speed == null ? 1 : Math.max(0, SPEEDS.indexOf(state.speed));
    const current = state.speed == null
      ? (a.metrics.burnPercentPerDay == null ? "本周期平均" : `本周期平均 ${Number(a.metrics.burnPercentPerDay).toFixed(1)}%/天`)
      : `${state.speed}%/天`;
    return `<div class="cr-panel">
      <ol class="cr-plan">${lines}</ol>
      ${timelineHtml(a)}
      <div class="cr-block">
        <div class="cr-label-row"><span class="cr-label">按不同速度试算</span><b>${esc(current)}</b></div>
        <input type="range" id="codexResetSpeed" min="0" max="6" step="1" value="${idx}"
          aria-label="按不同消耗速度试算">
        <div class="cr-speed-marks">${SPEEDS.map((s) => `<span>${s}</span>`).join("")}</div>
        <div class="cr-hint">速度只改计划试算，不改上面的 NOW/WAIT 结论。「补满」按严格不断档算，不等于建议用卡；省卡看计划第二行。</div>
      </div>
      <div class="cr-block">
        <div class="cr-label">重置卡（按到期日）</div>
        <div class="cr-card-list">${cardsRows(c, 0)}</div>
      </div>
    </div>`;
  }

  function pathRows(a) {
    if (!a) return "";
    const m = a.metrics || {};
    const cards = m.cardsAvailable == null ? "未知" : `${m.cardsAvailable} 张`;
    const canWait = m.canWait === false ? "订阅结束前等不到" : m.canWait === true ? "能等到"
      : m.planEndSource === "cards" ? "订阅未填，按卡有效期规划" : "未填订阅到期日";
    const need = NEEDS.find(([k]) => k === state.need);
    const needVal = need
      ? need[1] + ((state.need === "urgent" || state.need === "soon")
        ? ` · ${state.amount == null ? "按整窗估" : `${state.amount}%`}` : "")
      : "能等";
    const steps = [
      ["0", "硬门", a.reasonCode === "no_weekly" || a.reasonCode === "data_inconsistent" || a.reasonCode === "full_window" || a.reasonCode === "full_window_both" || a.reasonCode === "full_window_five_hour" || a.reasonCode === "sub_expired" ? "卡住/不判断" : "通过"],
      ["1", "等得到吗", canWait],
      ["2", "需求", needVal],
      ["3", "卡数", cards],
      ["4", "质量", (a.warnings || []).length ? `${a.warnings.length} 条提示` : "无"],
    ];
    return `<ol class="cr-path">${steps.map(([n, k, v]) => `
      <li><span class="cr-path-n">${n}</span><b>${esc(k)}</b><em>${esc(v)}</em></li>`).join("")}</ol>`;
  }

  function evidencePanel() {
    const a = state.advice;
    if (!a) return `<div class="cr-panel"><div class="cr-loading">正在读取依据…</div></div>`;
    const m = a.metrics || {};
    return `<div class="cr-panel">
      <div class="cr-block">
        <div class="cr-label-row"><span class="cr-label">这次结论的路径</span><code>${esc(a.reasonCode)}</code></div>
        ${pathRows(a)}
      </div>
      <div class="cr-block">
        <div class="cr-label">关键数字</div>
        <div class="cr-evidence-grid">
          <div><span>这次标的用量</span><b>${esc(needAmountText())}</b></div>
          <div><span>周额度已用</span><b>${pct(m.weeklyUsedPercent)}</b></div>
          <div><span>周额度剩余</span><b>${pct(m.weeklyRemainPercent)}</b></div>
          <div><span>下次重置</span><b>${esc(fmtTime(m.nextResetAt))}</b></div>
          <div><span>可用卡</span><b>${m.cardsAvailable == null ? "未知" : `${m.cardsAvailable} 张`}</b></div>
          <div><span>省卡策略用几张</span><b>${m.plannedCards == null ? "—" : `${m.plannedCards} 张`}</b></div>
          <div><span>补满要几张（严格不断档）</span><b>${m.coverCards == null ? "现有卡不够" : `${m.coverCards} 张`}</b></div>
          <div><span>到计划截止仍差</span><b>${m.shortfallPercent == null ? "—" : `${Number(m.shortfallPercent).toFixed(1)}%`}</b></div>
          <div><span>计划截止${m.planEndSource === "cards" ? "（卡到期）" : ""}</span><b>${esc(fmtTime(m.planEndAt))}</b></div>
          <div><span>最早卡到期</span><b>${esc(fmtTime(m.earliestCardExpiry))}</b></div>
        </div>
      </div>
      <div class="cr-block">
        <div class="cr-label">口径</div>
        <ul class="cr-warns">
          <li>只看周额度；5 小时窗只影响文案，不参与结论。</li>
          <li>用卡会改写下一次周窗锚点，计划不是「自然重置 + 卡数」的简单加法。</li>
          <li>卡到期日未知时不假设永久有效；现有卡也不够时，不推荐买卡。</li>
        </ul>
      </div>
      ${warningsHtml(a)}
    </div>`;
  }

  function detailInner(c) {
    const a = state.advice;
    const v = verdict(a);
    const vclass = a && VERDICT[a.verdict] ? a.verdict : "unknown";
    const tab = state.tab;
    const tabBtn = (key, label) =>
      `<button type="button" class="cr-tab${tab === key ? " on" : ""}" data-codex-tab="${key}"
        role="tab" aria-selected="${tab === key}">${label}</button>`;
    return `
      <div class="cr-page-head">
        <button type="button" class="btn s cr-back" data-codex-reset-back>返回 Codex</button>
        <div class="cr-page-title"><span>重置顾问</span><small>只看周额度 · 不自动兑换</small></div>
      </div>
      <div class="cr-verdict-card cr-verdict-${vclass}">
        <div class="cr-verdict-badge">${v.en}</div>
        <div class="cr-verdict-copy">
          <b>${esc(a ? a.headline : state.loading ? "正在计算…" : state.error ? "建议暂时不可用" : "正在读取周额度…")}</b>
          <p>${esc(a ? a.detail : state.error || "先刷新 Codex 周额度。")}</p>
        </div>
      </div>
      <div class="cr-tabs" role="tablist" aria-label="重置顾问页签">
        ${tabBtn("advice", "建议")}${tabBtn("plan", "计划")}${tabBtn("evidence", "依据")}
      </div>
      <div class="cr-tab-panel" role="tabpanel">
        ${tab === "advice" ? advicePanel(c) : tab === "plan" ? planPanel(c) : evidencePanel()}
      </div>
      <div class="cr-footer">
        本工具只给建议。兑换前请在 Codex 用量页面核对卡涵盖的额度和有效期。
      </div>`;
  }

  function currentChannel() {
    try {
      if (typeof CHANNELS === "undefined") return null;
      const id = typeof DETAIL_ID === "undefined" ? "codex" : DETAIL_ID;
      return CHANNELS.find((x) => x.id === (id || "codex")) || null;
    } catch (_) {
      return null;
    }
  }

  function buildInput(c) {
    const r = c.resetCredits || null;
    const weekly = (c.windows || []).find((w) => w.key === "weekly") || null;
    const five = (c.windows || []).find((w) => w.key === "5h") || null;
    return {
      scope: state.scope,
      planType: ((c.extra || []).find(([key]) => key === "套餐") || [])[1] || null,
      dataStale: !!c.stale || c.valid === false,
      weekly,
      fiveHour: five,
      resetCredits: r,
      dataUpdatedAt: ts(c.updatedAt) || null,
      subEndAt: state.subEndAt || null,
      need: {
        kind: state.need,
        amountPercent: state.amount,
        at: state.needAt || null,
      },
      planBurnPercentPerDay: state.speed,
    };
  }

  function inputKey(c) {
    return [
      c.id || "",
      Math.floor(nowSec() / 60),
      JSON.stringify([c.windows, c.resetCredits, c.stale, c.valid, c.extra]),
      ts(c.updatedAt),
      c.resetCredits && ts(c.resetCredits.updatedAt),
      c.resetCredits ? (c.resetCredits.availableCount == null ? "u" : String(c.resetCredits.availableCount)) : "u",
      state.need,
      state.amount == null ? "d" : String(state.amount),
      state.subEndAt || "0",
      state.speed == null ? "actual" : String(state.speed),
    ].join("|");
  }

  function applyProfile(p) {
    const profile = p || {};
    state.scope = profile.scope || null;
    state.subEndAt = profile.subEndAt || null;
    const need = profile.need || {};
    state.needAt = need.at || null;
    const amt = Number(need.amountPercent);
    state.amount = Number.isFinite(amt) && amt > 0 ? Math.min(100, amt) : null;
    const sameDay = need.kind !== "soon" || new Date(need.at * 1000).toDateString()
      === new Date(nowSec() * 1000).toDateString();
    if (need.kind && need.at && nowSec() - need.at <= 12 * 3600 && sameDay) {
      state.need = need.kind;
    } else {
      state.need = "patient";
      state.needAt = null;
      state.amount = null;
    }
  }

  function isEditingDate() {
    return state.open && !state.saving && document.activeElement?.id === "codexResetSubEnd";
  }

  function paint() {
    if (isEditingDate()) return;
    const c = currentChannel();
    if (!c) return;
    if (state.open) {
      const page = document.querySelector("#detailBody [data-codex-reset-page]");
      if (page) page.innerHTML = detailInner(c);
    } else {
      const root = document.querySelector("#detailBody [data-codex-reset-root]");
      if (root) {
        root.innerHTML = compactInner(c);
        root.setAttribute("data-codex-reset-open", "");
        root.setAttribute("role", "button");
        root.setAttribute("tabindex", "0");
        root.setAttribute("aria-expanded", "false");
      }
    }
  }

  async function loadAdvice(c, force) {
    if (!invoke) {
      state.error = "当前环境没有 Tauri invoke";
      paint();
      return;
    }
    const key = inputKey(c);
    if (!force && state.key === key && (state.advice || state.loading)) return;
    state.key = key;
    state.loading = true;
    state.advice = null;
    state.error = "";
    const seq = ++state.seq;
    paint();
    try {
      {
        try {
          const profile = await invoke("codex_reset_profile_get", { id: "codex" });
          if (seq !== state.seq) return;
          applyProfile(profile);
          state.profileLoaded = true;
          state.profileError = "";
          state.key = inputKey(c);
        } catch (e) {
          if (seq !== state.seq) return;
          applyProfile(null);
          // 读设置失败不阻断即时建议：用默认「能等」先算，计划区会标不可用。
          state.profileLoaded = true;
          state.profileError = "读取本地订阅设置失败:" + e;
        }
      }
      const advice = await invoke("codex_reset_advice", {
        id: "codex",
        input: buildInput(c),
      });
      if (seq !== state.seq) return;
      state.advice = advice;
      state.loading = false;
    } catch (e) {
      if (seq !== state.seq) return;
      state.loading = false;
      state.error = String(e || "读取建议失败");
    }
    paint();
  }

  function ensureAdvice(c, force) {
    const key = inputKey(c);
    if (!force && state.key === key && (state.advice || state.loading)) return;
    loadAdvice(c, force);
  }

  function paintPageAndFocus(sel) {
    paint();
    requestAnimationFrame(() => {
      const el = document.querySelector(sel);
      if (el) el.focus({ preventScroll: true });
    });
  }

  function open() {
    if (!currentChannel()) return;
    state.open = true;
    state.tab = "advice";
    if (typeof renderDetailSheet === "function") renderDetailSheet();
    const body = document.getElementById("detailBody");
    if (body) body.scrollTop = 0;
    paintPageAndFocus("[data-codex-reset-back]");
  }

  function back() {
    state.open = false;
    state.tab = "advice";
    if (typeof renderDetailSheet === "function") renderDetailSheet();
    const body = document.getElementById("detailBody");
    if (body) body.scrollTop = 0;
    paintPageAndFocus("[data-codex-reset-root]");
  }

  async function saveNeed(kind, amountPercent) {
    if (state.saving || state.loading) return;
    const c = currentChannel();
    if (!c) return;
    const prev = state.need;
    const prevAt = state.needAt;
    const prevAmount = state.amount;
    state.saving = true;
    ++state.seq;
    state.need = kind;
    state.needAt = nowSec();
    // patient/idle 不带用量；urgent/soon 保留旧用量或用本次点选的档位。
    state.amount = kind === "urgent" || kind === "soon"
      ? (amountPercent == null ? state.amount : amountPercent)
      : null;
    paint();
    try {
      await invoke("codex_reset_profile_set", {
        id: "codex",
        profile: { scope: state.scope, need: { kind, amountPercent: state.amount, at: state.needAt } },
      });
      state.profileError = "";
      state.profileLoaded = true;
      await loadAdvice(c, true);
      requestAnimationFrame(() => {
        const el = document.querySelector(`[data-codex-need="${kind}"]`);
        if (el) el.focus({ preventScroll: true });
      });
    } catch (e) {
      state.need = prev;
      state.needAt = prevAt;
      state.amount = prevAmount;
      if (typeof showToast === "function") showToast("保存需求失败:" + e, true);
      paint();
    } finally {
      state.saving = false;
      paint();
    }
  }

  async function saveDate(value) {
    if (state.saving || state.loading) { paint(); return; }
    const c = currentChannel();
    if (!c) return;
    const prev = state.subEndAt;
    if (!value) {
      state.subEndAt = null;
    } else {
      const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
      if (!m) {
        if (typeof showToast === "function") showToast("订阅到期日格式不对", true);
        paint();
        return;
      }
      const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]), 23, 59, 59, 0);
      if (Number.isNaN(d.getTime())) {
        if (typeof showToast === "function") showToast("订阅到期日无效", true);
        paint();
        return;
      }
      state.subEndAt = Math.floor(d.getTime() / 1000);
    }
    state.saving = true;
    ++state.seq;
    paint();
    try {
      const patch = value ? { subEndAt: state.subEndAt } : { clearSubEnd: true };
      patch.scope = state.scope;
      await invoke("codex_reset_profile_set", { id: "codex", profile: patch });
      state.profileError = "";
      state.profileLoaded = true;
      await loadAdvice(c, true);
    } catch (e) {
      state.subEndAt = prev;
      if (typeof showToast === "function") showToast("保存订阅到期日失败:" + e, true);
      paint();
    } finally {
      state.saving = false;
      paint();
    }
  }

  function clickHandler(ev) {
    const t = ev.target && ev.target.closest
      ? ev.target.closest("[data-codex-reset-open],[data-codex-reset-back],[data-codex-tab],[data-codex-need],[data-codex-amt],[data-codex-clear-date],[data-codex-retry]")
      : null;
    if (!t) return;
    if (t.hasAttribute("data-codex-clear-date")) {
      if (!t.disabled) saveDate("");
      return;
    }
    if (t.hasAttribute("data-codex-reset-open")) {
      ev.preventDefault();
      open();
      return;
    }
    if (t.hasAttribute("data-codex-reset-back")) {
      ev.preventDefault();
      back();
      return;
    }
    if (t.hasAttribute("data-codex-tab")) {
      state.tab = t.getAttribute("data-codex-tab") || "advice";
      paintPageAndFocus(`[data-codex-tab="${state.tab}"]`);
      return;
    }
    if (t.hasAttribute("data-codex-need")) {
      const kind = t.getAttribute("data-codex-need");
      if (kind && kind !== state.need) saveNeed(kind);
      return;
    }
    if (t.hasAttribute("data-codex-amt")) {
      const amt = Number(t.getAttribute("data-codex-amt"));
      if (Number.isFinite(amt) && amt > 0 && amt !== state.amount) saveNeed(state.need, amt);
      return;
    }
    if (t.hasAttribute("data-codex-retry")) {
      const c = currentChannel();
      if (c) loadAdvice(c, true);
    }
  }

  function changeHandler(ev) {
    const t = ev.target;
    if (!t) return;
    if (t.id === "codexResetSpeed") {
      const idx = Number(t.value);
      state.speed = SPEEDS[idx] == null ? null : SPEEDS[idx];
      const c = currentChannel();
      if (c) loadAdvice(c, true);
      return;
    }
    if (t.id === "codexResetSubEnd") {
      saveDate(t.value);
    }
  }

  function keyHandler(ev) {
    if (ev.key !== "Enter" && ev.key !== " ") return;
    const t = ev.target;
    if (t && t.hasAttribute && t.hasAttribute("data-codex-reset-open")) {
      ev.preventDefault();
      open();
    }
  }

  window.CodexResetUI = {
    render(c) {
      if (!c || c.id !== "codex") return "";
      if (state.open) return `<div class="cr-page" data-codex-reset-page>${detailInner(c)}</div>`;
      ensureAdvice(c);
      return `<div class="sec codex-resets" data-codex-reset-root data-codex-reset-open
        role="button" tabindex="0" aria-expanded="false">${compactInner(c)}</div>`;
    },
    renderDetail(c) {
      if (!c || c.id !== "codex") return "";
      ensureAdvice(c);
      return `<div class="cr-page" data-codex-reset-page>${detailInner(c)}</div>`;
    },
    isOpen() {
      return state.open;
    },
    isEditing: isEditingDate,
    onDetailClosed() {
      state.open = false;
      state.tab = "advice";
    },
  };

  const body = document.getElementById("detailBody");
  setInterval(() => {
    if (!body || !body.querySelector("[data-codex-reset-root],[data-codex-reset-page]") || state.saving || isEditingDate()) return;
    const c = currentChannel();
    if (c && c.id === "codex") ensureAdvice(c);
  }, 60_000);
  if (body) {
    body.addEventListener("focusout", ev => {
      if (ev.target.id !== "codexResetSubEnd") return;
      setTimeout(() => {
        if (!state.open || state.saving || isEditingDate()) return;
        paint();
        const c = currentChannel();
        if (c) ensureAdvice(c);
      }, 0);
    });
    body.addEventListener("click", clickHandler);
    body.addEventListener("change", changeHandler);
    body.addEventListener("keydown", keyHandler);
  }
})();
