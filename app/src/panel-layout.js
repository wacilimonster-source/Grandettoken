/* 面板布局与缩略摘要。余额与告警口径由现有行渲染提供。 */
(() => {
  const layouts = ["single", "double", "thumbnail"];
  let saving = false;
  let frame = 0;

  function normalize(layout) {
    return layouts.includes(layout) ? layout : "single";
  }

  function update(layout) {
    layout = normalize(layout);
    for (const name of layouts) document.body.classList.toggle(`layout-${name}`, layout === name);
    document.querySelectorAll("#lkLayout [data-layout], #cfgLayout [data-layout]").forEach(button => {
      button.setAttribute("aria-pressed", String(button.dataset.layout === layout));
      button.disabled = saving;
    });
    schedule();
  }

  async function select(layout) {
    const next = normalize(layout);
    const previous = panelLayout();
    if (saving || next === previous) return;
    saving = true;
    CFG.panelLayout = next;
    update(next);
    try {
      if (!await saveCfg("窗口布局")) CFG.panelLayout = previous;
      if (currentForm() === "panel") await applyForm("panel", false, true);
    } catch (error) {
      showToast("切换窗口失败:" + error, true);
    } finally {
      saving = false;
      update(panelLayout());
      render({});
    }
  }

  function windowValue(w) {
    return w && Number.isFinite(w.remainPercent) ? `${dispPct(w.remainPercent).toFixed(0)}%` : "未知";
  }

  function windowTime(w) {
    const at = resetMs(w);
    if (at === null) return "重置未知";
    return `<span data-thumbnail-reset="${at}">${esc(fmtResetTightShort(at))}</span>`;
  }

  function summary(c, row) {
    const { noKey, failed, isOld, mw, sub, subVal, railLeft, railRight, winWarn, claimWarn } = row;
    let label = "";
    let status = railLeft || sub || subVal;
    let detail = [sub, subVal, railLeft, railRight, winWarn, claimWarn].map(plainText).filter(Boolean).join(" · ");
    const health = hvRow(c.id);
    const auth = health && health.state === "auth" || c.recovery && c.recovery.phase === "auth";
    if (mw) {
      label = mw.label === "本周" || mw.key === "gemini-weekly" ? "周" : mw.label;
      if (c.id === "antigravity") label = mw.key === "gemini-weekly" ? "G周" : mw.key === "gemini-5h" ? "G5h" : mw.label;
      label += CFG.showUsed ? "已用" : "剩";
    }
    if (noKey || failed || isOld || auth) {
      const state = noKey ? "待登录/配置" : auth ? "登录失效" : failed && c.remaining == null ? "读取失败" : "数据已旧";
      status = esc(state) + (sub ? " · " + sub : "");
    } else if (c.id === "codex" && mw) {
      const short = (c.windows || []).find(w => w.label === "5 小时");
      const cards = c.resetCredits && c.resetCredits.availableCount;
      status = `5h${CFG.showUsed ? "用" : "剩"} ${windowValue(short)} · 卡 ${cards == null ? "未知" : esc(cards)}`;
      if (short && (short.status === "rate-limited" || short.remainPercent === 0)) status += ` · 耗尽 ${windowTime(short)}`;
    } else if (c.id === "antigravity") {
      const wins = c.windows || [];
      const short = wins.find(w => w.key === "gemini-5h");
      const limitedWins = wins.filter(w => w.status === "rate-limited" || w.remainPercent === 0);
      const limited = limitedWins.find(w => health && (w.key === health.bindingWindow || w.label === health.bindingWindow)) || limitedWins[0];
      status = `G5h${CFG.showUsed ? "用" : "剩"} ${windowValue(short)}`;
      if (limited) status += ` · 限流 ${windowTime(limited)}`;
      else if (c.limited) status += " · 限流";
      else if (short) status += ` · ${windowTime(short)}`;
      detail = [detail, limited ? `${limited.label}限流` : "", plainText(status)].filter(Boolean).join(" · ");
    } else if (c.limited) {
      status = `限流 · ${railLeft || sub}`;
    } else if (winWarn || claimWarn) {
      status = esc(plainText(winWarn || claimWarn));
    } else if (c.kind === "points") {
      const expiry = (c.expiring || []).filter(e => e.amount > 0 && e.at * 1000 > Date.now()).sort((a, b) => a.at - b.at)[0];
      if (expiry) status = `${esc(fmtDay(expiry.at))} 到期 ${esc(points(expiry.amount))} ${esc(c.unit && c.unit !== "credits" ? c.unit : "分")}`;
      else status = railLeft || railRight || sub;
    } else if (mw) {
      status = `${esc(mw.label)} · ${windowTime(mw)}`;
    }
    const text = plainText(status);
    const full = [...new Set([text, detail].filter(Boolean))].join(" · ");
    const shortName = c.id === "google-cloud" ? "Gemini · GCP" : c.name;
    return {
      label: `<span class="thumb-window">${esc(label)}</span>`,
      name: `<span class="thumb-name" title="${esc(c.name)}">${esc(shortName)}</span>`,
      html: `<div class="thumb-status" role="note" title="${esc(full)}" aria-label="${esc(full)}">${status || "&nbsp;"}</div>`,
    };
  }

  function fit() {
    frame = 0;
    const list = document.getElementById("list");
    if (!list || !document.body.classList.contains("layout-thumbnail") || currentForm() !== "panel") return;
    const rows = [...list.querySelectorAll(":scope > .row")];
    const label = document.getElementById("thumbnailCount");
    const total = CHANNELS.filter(c => c.hasKey && !c.hidden).length;
    setTextIfChanged(document.getElementById("thumbnailMetric"), CFG.showUsed ? "余额 / 已用额度" : "余额 / 剩余额度");
    const filtered = rows.length !== total;
    setTextIfChanged(label, filtered ? `显示 ${rows.length} / ${total} 个渠道` : `全部 ${total} 个渠道`);
    const style = getComputedStyle(list);
    const gap = parseFloat(style.rowGap) || 0;
    const lines = Math.ceil(rows.length / 2);
    const extras = [...list.children].filter(el => !el.classList.contains("row"));
    const extraHeight = extras.reduce((sum, el) => sum + el.getBoundingClientRect().height + gap, 0);
    const room = list.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom) - extraHeight;
    const u = fsU();
    const height = lines ? Math.max(68 * u, Math.min(80 * u, (room - gap * (lines - 1)) / lines)) : 80 * u;
    list.style.setProperty("--thumb-card-h", `${Math.floor(height)}px`);
    rows.forEach(row => {
      const value = row.querySelector(".val .v");
      value.style.setProperty("--thumb-value-font", `${22 * u}px`);
      value.title = plainText(value.innerHTML);
      if (value.scrollWidth > value.clientWidth && value.clientWidth > 0) {
        value.style.setProperty("--thumb-value-font", `${Math.max(14 * u, 22 * u * value.clientWidth / value.scrollWidth)}px`);
      }
    });
  }

  function schedule() {
    if (!frame) frame = requestAnimationFrame(fit);
  }

  function tick() {
    document.querySelectorAll("[data-thumbnail-reset]").forEach(el => {
      setTextIfChanged(el, fmtResetTightShort(Number(el.dataset.thumbnailReset)));
    });
  }

  document.getElementById("lkLayout").addEventListener("click", event => {
    const button = event.target.closest("[data-layout]");
    if (button) setPanelLayout(button.dataset.layout);
  });
  document.addEventListener("keydown", event => {
    if (event.key === "Escape") document.getElementById("footerMore").open = false;
  });
  window.addEventListener("resize", schedule);
  if (typeof ResizeObserver === "function") new ResizeObserver(schedule).observe(document.getElementById("list"));
  window.PanelLayoutUI = { normalize, update, select, summary, schedule, tick };
})();
