(() => {
  const calc = window.QuotaPlan;
  const data = window.QuotaPlanData;
  const storageKey = "quota_usage_plan_v1";
  const empty = { hours: calc.defaultHours, accounts: {} };
  let saved;
  let storageNotice = "";
  let profile = null;
  let request = null;
  let revision = 0;
  let cache = null;

  function notify(message) {
    storageNotice = message;
    showToast(message, true);
  }

  function store() {
    if (!saved) {
      saved = empty;
      try {
        const value = JSON.parse(localStorage.getItem(storageKey));
        if (value && calc.validHours(value.hours || {}) && value.accounts && typeof value.accounts === "object") saved = value;
        else if (value !== null) notify("套餐用量计划设置格式异常");
      } catch {
        notify("套餐用量计划设置读取失败");
      }
    }
    return saved;
  }

  function persist(next) {
    localStorage.setItem(storageKey, JSON.stringify(next));
    saved = next;
    storageNotice = "";
    cache = null;
  }

  function setCodexProfile(value) {
    if (!value?.scope || !CHANNELS.some(c => c.id === "codex" && c.quotaPlanScope === value.scope)) return;
    revision++;
    profile = value;
    cache = null;
    render({});
  }

  function loadCodex(c) {
    const scope = c.quotaPlanScope;
    if (!scope || profile?.scope === scope || request?.scope === scope && request.at === c.updatedAt) return;
    const current = { scope, at: c.updatedAt, revision: ++revision };
    request = current;
    invoke("codex_reset_profile_get", { id: "codex" }).then(value => {
      if (current.revision !== revision) return;
      if (value?.scope === scope && CHANNELS.some(c => c.id === "codex" && c.quotaPlanScope === scope)) setCodexProfile(value);
      else if (!CHANNELS.some(c => c.id === "codex" && c.quotaPlanScope === scope)) request = null;
    }).catch(() => {
      if (current.revision === revision) showToast("套餐计划未读到 Codex 订阅日期", true);
    });
  }

  function snapshot(channels, now) {
    const minute = Math.floor(now / 60000);
    if (cache?.channels === channels && cache.minute === minute) return cache.items;
    const selected = channels.filter(c => c.hasKey && !c.hidden && data.scope(c));
    if (!selected.length) {
      cache = { channels, minute, items: [] };
      return cache.items;
    }
    const maxAge = (CFG?.staleAfterSec > 0 ? CFG.staleAfterSec : 900) * 1000;
    const initial = store();
    let accounts = initial.accounts;
    for (const c of selected) {
      if (c.id === "codex") loadCodex(c);
      if (!data.fresh(c, now, maxAge)) continue;
      const key = data.scope(c);
      const account = accounts[key] || {};
      const observation = data.sample(account.observation, data.limits(c, account), c.updatedAt * 1000, now, initial.hours);
      if (observation !== account.observation) accounts = { ...accounts, [key]: { ...account, observation } };
    }
    if (accounts !== initial.accounts) {
      try {
        persist({ ...initial, accounts });
      } catch {
        if (!storageNotice) notify("套餐用量样本保存失败");
      }
    }
    const items = selected.map(channel => {
      const account = accounts[data.scope(channel)] || {};
      const model = data.prepare(channel, account, account.observation?.history, profile, now, maxAge);
      const result = model.rate ? calc.schedule(model, now, initial.hours) : null;
      return { channel, model, result };
    });
    cache = { channels, minute, items };
    return items;
  }

  function events(channels, days, now) {
    const stop = new Date(now);
    stop.setHours(0, 0, 0, 0);
    stop.setDate(stop.getDate() + days);
    return snapshot(channels, now).filter(({ model, result }) => {
      const early = result?.urgent && result.schedule[0].slots.some(slot => slot.due === model.end);
      return model.end > now && !model.exhausted && (model.end < stop.getTime() || early);
    }).map(item => ({ ...item, entry: { at: item.model.end / 1000 }, type: "package" }));
  }

  function percent(amount) {
    return Number(amount.toFixed(1)) + "%";
  }

  function dailyTarget(day, unit) {
    const periods = new Map();
    for (const slot of day.slots) periods.set(slot.due, (periods.get(slot.due) || 0) + slot.amount);
    return [...periods.values()].map(amount => percent(amount)).join(" + ") + unit;
  }

  function banner(packages) {
    const urgent = packages.filter(item => item.result?.urgent).sort((a, b) => a.model.end - b.model.end);
    if (!urgent.length) return "";
    return `<div class="qp-nudges" aria-label="今天该用的套餐">${urgent.map(({ channel, model, result }) =>
      `<button type="button" class="btn qp-nudge" data-quota-open="${esc(channel.id)}">
        <b>今天该用 · ${esc(channel.name)}</b><span>目标 ${esc(dailyTarget(result.schedule[0], model.unit))} · ${fmtClock(result.schedule[0].slots[0].start / 1000)} 开始</span>
      </button>`).join("")}</div>`;
  }

  function dateInput(value) {
    if (!value) return "";
    const d = new Date(value);
    const part = n => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${part(d.getMonth() + 1)}-${part(d.getDate())}T${part(d.getHours())}:${part(d.getMinutes())}`;
  }

  function rows(result, unit) {
    const visible = result.schedule.slice(0, 7);
    return visible.map(day =>
      `<li><b>${esc(window.PanelLayoutUI.expiryDate(day.day / 1000))} · ${esc(dailyTarget(day, unit))}</b>
        <span>${day.slots.map(slot => `${fmtClock(slot.start / 1000)}–${fmtClock(slot.end / 1000)} 用 ${percent(slot.amount)}`).join(" · ")}</span></li>`
    ).join("") + (result.schedule.length > visible.length ? `<li>先显示最近 ${visible.length} 个安排日，计划随用量刷新</li>` : "");
  }

  function detail(c) {
    if (!data.kind(c) || !c.hasKey) return "";
    const key = data.scope(c);
    if (!key) return '<div class="sec"><h3>套餐用量计划</h3><p class="hint">刷新配额，确认当前账号后可安排用量</p></div>';
    const item = snapshot(CHANNELS, Date.now()).find(x => x.channel.id === c.id && data.scope(x.channel) === key);
    if (!item) return "";
    const { model, result } = item;
    const { hours, accounts } = store();
    const settings = accounts[key] || {};
    const isGemini = data.kind(c) === "gemini";
    const status = result ? [
      result.urgent ? "今天该用" : result.schedule.length ? "按计划使用" : "截止前没有可安排时段",
      result.incomplete ? "按当前速度和时段来不及全部用完" : "",
      result.latestStart !== null ? `最晚约 ${window.PanelLayoutUI.expiryDate(result.latestStart / 1000)} ${fmtClock(result.latestStart / 1000)} 开始` : "",
    ].filter(Boolean).join(" · ") : model.reason;
    const duration = `每天 ${esc(hours.start)}–${esc(hours.end)} · 三个渠道可以同时使用`;
    return `<section class="sec qp-detail" aria-label="套餐用量计划">
      <h3>套餐用量计划</h3>
      <div>${esc(status)}</div>
      <div class="hint">${model.end ? `<span title="${esc(exactLocalTime(model.end / 1000))}">截止 ${esc(window.PanelLayoutUI.expiryDate(model.end / 1000))} ${fmtClock(model.end / 1000)}</span> · ` : ""}${duration}</div>
      ${result?.schedule.length ? `<ul class="qp-days">${rows(result, model.unit)}</ul>` : ""}
      <form data-quota-settings="${esc(key)}" data-quota-channel="${esc(c.id)}">
        ${isGemini ? `<label>订阅截止 <input name="end" type="datetime-local" value="${dateInput(settings.end)}"></label>
        <label><input name="weeklyOnly" type="checkbox"${settings.weeklyOnly ? " checked" : ""}> 已确认此账号只有周窗</label>` : ""}
        ${c.id === "codex" ? '<span class="hint">在重置顾问中填写或清除订阅到期日</span>' : ""}
        <label>每在线小时用量（主窗百分点）<input name="rate" type="number" min="0.1" max="1000" step="any" placeholder="自动估算" value="${esc(settings.rate ?? "")}"></label>
        ${c.id !== "opencode-go" && data.limits(c, settings).short ? `<label>满 5 小时窗相当于主窗多少百分点<input name="shortCapacity" type="number" min="0.1" max="1000" step="any" placeholder="自动估算" value="${esc(settings.shortCapacity ?? "")}"></label>` : ""}
        <div class="qp-hours"><label>每天开始 <input name="start" type="time" value="${esc(hours.start)}" required></label>
        <label>每天结束 <input name="stop" type="time" value="${esc(hours.end)}" required></label></div>
        <button class="btn s" type="submit">保存计划设置</button>
        <span class="hint qp-error" role="status">${esc(storageNotice)}</span>
      </form>
    </section>`;
  }

  document.addEventListener("click", event => {
    const button = event.target.closest("[data-quota-open]");
    if (button) openDetail(button.dataset.quotaOpen, button);
  });

  document.addEventListener("input", event => {
    const form = event.target.closest("[data-quota-settings]");
    if (form) form.dataset.dirty = "";
  });

  document.addEventListener("submit", event => {
    const form = event.target.closest("[data-quota-settings]");
    if (!form) return;
    event.preventDefault();
    const c = CHANNELS.find(item => item.id === form.dataset.quotaChannel && item.hasKey && !item.hidden);
    const key = c && data.scope(c);
    const values = new FormData(form);
    const hours = { start: values.get("start"), end: values.get("stop") };
    const error = form.querySelector(".qp-error");
    if (!key || key !== form.dataset.quotaSettings) {
      error.textContent = "账号已切换，请重新打开详情";
      return;
    }
    if (!calc.validHours(hours)) {
      error.textContent = "结束时间须晚于开始时间，最多 10 小时";
      return;
    }
    const end = values.get("end");
    const date = end && Date.parse(end);
    if (end && (!Number.isFinite(date) || dateInput(date) !== end)) {
      error.textContent = "订阅截止时间无效";
      return;
    }
    const current = store();
    const account = { ...current.accounts[key], rate: Number(values.get("rate")) || null };
    if (values.has("shortCapacity")) account.shortCapacity = Number(values.get("shortCapacity")) || null;
    if (data.kind(c) === "gemini") {
      account.end = date || null;
      account.weeklyOnly = values.has("weeklyOnly");
    }
    try {
      persist({ hours, accounts: { ...current.accounts, [key]: account } });
    } catch {
      error.textContent = "计划设置保存失败，可保留输入后重试";
      return;
    }
    delete form.dataset.dirty;
    if (form.contains(document.activeElement)) document.activeElement.blur();
    render({});
    document.querySelector("#detailBody [data-quota-settings] button")?.focus();
    showToast("套餐计划已更新");
  });

  function isEditing(c) {
    const form = document.querySelector(`#detailBody [data-quota-settings="${data.scope(c)}"]`);
    return !!form && (form.hasAttribute("data-dirty") || document.activeElement?.matches("input") && form.contains(document.activeElement));
  }

  function needsTick(now) {
    return !!cache && cache.minute !== Math.floor(now / 60000)
      && cache.items.some(item => item.model.end > cache.minute * 60000
        && (item.result || item.model.end <= now));
  }

  window.QuotaPlanUI = { events, detail, banner, isEditing, setCodexProfile, needsTick };
})();
