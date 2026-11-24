/* Google Cloud 赠金渠道。按网页读取的结束日期与对应批次余额展示，无手填状态。 */
(() => {
  const ID = "google-cloud";
  const dataOf = c => c.googleCloud || { account: null, credits: [] };
  function todayOrdinal() {
    const now = new Date();
    return Date.UTC(now.getFullYear(), now.getMonth(), now.getDate()) / 86400000;
  }
  function parseYmd(ymd) {
    if (typeof ymd !== "string") return null;
    const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(ymd.trim());
    if (!m) return null;
    const year = Number(m[1]), month = Number(m[2]), day = Number(m[3]);
    const utc = Date.UTC(year, month - 1, day);
    const check = new Date(utc);
    if (check.getUTCFullYear() !== year || check.getUTCMonth() !== month - 1 || check.getUTCDate() !== day) {
      return null;
    }
    return { ymd: m[0], ordinal: utc / 86400000 };
  }
  function daysUntil(ymd) {
    const parsed = parseYmd(ymd);
    return parsed ? parsed.ordinal - todayOrdinal() : null;
  }
  function summarize(c) {
    const credits = dataOf(c).credits.filter(x => Number(x.remaining) > 0);
    const dated = credits
      .map(x => ({ credit: x, parsed: parseYmd(x.expiresDate) }))
      .filter(x => x.parsed !== null)
      .sort((a, b) => a.parsed.ordinal - b.parsed.ordinal);
    const undated = credits.filter(x => parseYmd(x.expiresDate) === null);
    if (!dated.length) {
      return { credits, dated: [], undated, earliestYmd: null, batch: [], batchAmount: 0, unit: c.unit || "USD", days: null };
    }
    const earliestYmd = dated[0].parsed.ymd;
    const batch = dated.filter(x => x.parsed.ymd === earliestYmd).map(x => x.credit);
    const batchAmount = batch.reduce((sum, x) => sum + Number(x.remaining), 0);
    const unit = batch[0].unit || c.unit || "USD";
    const days = dated[0].parsed.ordinal - todayOrdinal();
    return { credits, dated, undated, earliestYmd, batch, batchAmount, unit, days };
  }
  function expiry(ymd) {
    const days = daysUntil(ymd);
    if (days === null) return "到期日期未提供";
    if (days < 0) return `${ymd} · 赠金已到期（待刷新核对）`;
    if (days === 0) return `${ymd} · 今日到期`;
    return `${ymd} 到期 · 剩余 ${days} 天`;
  }
  function rail(c, thumbnail = false) {
    const { credits, dated, undated, earliestYmd, batch, batchAmount, unit, days } = summarize(c);
    if (!credits.length) return { left: "当前没有可用赠金", right: "", warn: false };
    if (!dated.length) {
      if (thumbnail) return { left: "到期日期未提供", right: "", warn: false };
      return { left: "赠金到期日期暂未提供", right: credits.length > 1 ? `共 ${credits.length} 笔` : "", warn: false };
    }
    const amt = esc(money(batchAmount, unit));
    const warn = days <= 7;
    if (thumbnail) {
      return { left: days < 0 ? `${amt} 已到期` : `${amt} 将到期`, right: earliestYmd, warn };
    }
    const left = days < 0
      ? `${earliestYmd} 已到期 · ${amt}`
      : days === 0
        ? `今日到期 · ${amt}`
        : `最近 ${earliestYmd} 到期 · ${amt}`;
    const rel = days < 0 ? "待刷新核对" : days === 0 ? "今日" : `剩余 ${days} 天`;
    const right = undated.length
      ? `${rel} · ${undated.length} 笔日期未知`
      : credits.length > batch.length
        ? `${rel} · 共 ${credits.length} 笔`
        : rel;
    return { left, right, warn };
  }
  function controls(c) {
    const connected = !!dataOf(c).account;
    return `<div class="ccbtns">
      <button class="btn p" data-gcp-action="login">${connected ? "连接 / 切换结算账号" : "连接 Google Cloud"}</button>
      ${connected ? '<button class="btn" data-gcp-action="refresh">刷新余额</button><button class="btn danger" data-gcp-action="disconnect">停止连接</button>' : ""}
    </div><p class="khint">主动登录后读取 Google Cloud 赠金表。隐藏渠道或停止连接会停止自动刷新。登录保存在本机独立浏览器配置中。</p>`;
  }
  function records(c) {
    const data = dataOf(c);
    if (!data.account) return "";
    const sum = summarize(c);
    const credits = [...data.credits].sort(
      (a, b) => (parseYmd(a.expiresDate)?.ordinal ?? Infinity) - (parseYmd(b.expiresDate)?.ordinal ?? Infinity)
    );
    const parts = [`结算账号 ${esc(data.account)}`, `共 ${credits.length} 笔可用赠金`];
    if (sum.earliestYmd) parts.push(`最早 ${sum.earliestYmd} 到期 ${esc(money(sum.batchAmount, sum.unit))}`);
    if (sum.undated.length) parts.push(`${sum.undated.length} 笔日期未提供`);
    return `<div class="sec"><div class="sec-h"><span>赠金明细与到期</span><span>网页自动读取</span></div>
      <p class="khint">${parts.join(" · ")}。</p>
      ${credits.map(credit => `<div class="gc-credit" data-gcp-account="${esc(data.account)}" data-gcp-credit="${esc(credit.id)}">
          <div class="sec-h"><b>${esc(credit.name)}</b><b>${esc(money(credit.remaining, credit.unit || c.unit))}</b></div>
          <p class="khint">${esc(credit.scope)}</p>
          <p>${esc(expiry(credit.expiresDate))}</p>
        </div>`).join("") || '<p class="khint">页面记录中没有状态为可用的赠金。</p>'}
      <p class="khint">到期日期自动读取自 Google Cloud 赠金表「结束日期」列。到期后保留 Google 最近上报的余额，刷新后核对。</p></div>`;
  }
  function detail(c) {
    return `<div class="rbody-in">${controls(c)}${records(c)}
      ${c.updatedAt ? `<p class="khint">余额查询于 ${esc(new Date(c.updatedAt * 1000).toLocaleString("zh-CN"))}${c.stale ? " · 上次快照" : ""}</p>` : ""}
      ${c.error ? `<p class="khint">${esc(c.error)}</p>` : ""}</div>`;
  }
  document.addEventListener("click", async e => {
    const button = e.target.closest("[data-gcp-action]");
    if (!button || button.disabled) return;
    e.preventDefault();
    const action = button.dataset.gcpAction;
    const channel = CHANNELS.find(c => c.id === ID);
    if (!channel) return;
    button.disabled = true;
    try {
      if (action === "login") {
        await invoke("google_cloud_login");
        showToast("请打开 Google 赠金表，显示全部记录和「结束日期」列，并清除过滤条件");
      } else if (action === "refresh") {
        await invoke("google_cloud_refresh");
        showToast("正在读取 Google 赠金，完成后自动更新");
      } else if (action === "disconnect") {
        await invoke("google_cloud_disconnect");
        await refresh();
        renderChCards();
      }
    } catch (error) { showToast("Google 赠金操作失败，请重试。" + error, true); }
    finally { button.disabled = false; }
  });
  listen("google-cloud-result", async event => {
    const result = event.payload || {};
    if (result.error) showToast(result.error, true);
    if (result.ok) {
      await refresh();
      if (typeof renderChCards === "function") renderChCards();
      if (result.interactive) showToast("Google 赠金已连接，赠金记录已更新");
    }
  }).catch(error => showToast("Google 赠金状态监听失败，请重启。" + error, true));
  window.GoogleCloudUI = { controls, records, detail, rail };
})();
