/* Google Cloud 赠金渠道。到期时间按结算账号和赠金 ID 隔离，编辑草稿跨轮询保留。 */
(() => {
  const drafts = new Map();
  const busy = new Set();
  const ID = "google-cloud";
  const dataOf = c => c.googleCloud || { account: null, credits: [] };
  const keyOf = (account, credit) => JSON.stringify([account, credit]);
  function dateValue(at) {
    if (!at) return "";
    const d = new Date(at * 1000);
    const p = n => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
  }
  function expiry(at) {
    if (!at) return "到期时间未填";
    const when = new Date(at * 1000).toLocaleString("zh-CN", { hour12: false });
    const left = at * 1000 - Date.now();
    return left <= 0 ? `${when} · 赠金已到期` : `${when} 到期 · 剩余 ${Math.ceil(left / 86400e3)} 天`;
  }
  function rail(c) {
    const credits = dataOf(c).credits.filter(x => x.remaining > 0);
    const dated = credits.filter(x => x.expiresAt).sort((a, b) => a.expiresAt - b.expiresAt);
    if (!credits.length) return { left: "当前没有可用赠金", right: "", warn: false };
    if (!dated.length) return { left: "赠金到期时间未填", right: "手动填写", warn: false };
    const at = dated[0].expiresAt;
    return { left: expiry(at), right: dated.length < credits.length ? "部分日期未填" : "手填日期",
      warn: at * 1000 - Date.now() <= 7 * 86400e3 };
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
    const credits = [...data.credits].sort((a, b) => (a.expiresAt || Infinity) - (b.expiresAt || Infinity));
    return `<div class="sec"><div class="sec-h"><span>赠金到期时间</span><span>${esc(Intl.DateTimeFormat().resolvedOptions().timeZone)}</span></div>
      <p class="khint">结算账号 ${esc(data.account)}。金额为账户共享赠金，适用范围以每笔说明为准。</p>
      ${credits.map(credit => {
        const key = keyOf(data.account, credit.id);
        const value = drafts.has(key) ? drafts.get(key) : dateValue(credit.expiresAt);
        return `<div class="gc-credit" data-gcp-account="${esc(data.account)}" data-gcp-credit="${esc(credit.id)}">
          <div class="sec-h"><b>${esc(credit.name)}</b><b>${esc(money(credit.remaining, credit.unit))}</b></div>
          <p class="khint">${esc(credit.scope)}</p>
          <p>${esc(expiry(credit.expiresAt))}${credit.expiresAt ? " · 手动填写" : ""}</p>
          <label>赠金到期时间<input class="inp" type="datetime-local" data-gcp-date value="${esc(value)}"${busy.has(key) ? " disabled" : ""}></label>
          <button class="btn" data-gcp-action="expiry"${busy.has(key) ? " disabled" : ""}>${busy.has(key) ? "保存中…" : "保存到期时间"}</button>
        </div>`;
      }).join("") || '<p class="khint">页面记录中没有状态为可用的赠金。</p>'}
      <p class="khint">日期留空后保存可清除手填值。到期后保留 Google 最近上报的余额，刷新后核对。</p></div>`;
  }
  function detail(c) {
    return `<div class="rbody-in">${controls(c)}${records(c)}
      ${c.updatedAt ? `<p class="khint">余额查询于 ${esc(new Date(c.updatedAt * 1000).toLocaleString("zh-CN"))}${c.stale ? " · 上次快照" : ""}</p>` : ""}
      ${c.error ? `<p class="khint">${esc(c.error)}</p>` : ""}</div>`;
  }
  document.addEventListener("input", e => {
    if (!e.target.matches("[data-gcp-date]")) return;
    const row = e.target.closest("[data-gcp-credit]");
    drafts.set(keyOf(row.dataset.gcpAccount, row.dataset.gcpCredit), e.target.value);
  });
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
        showToast("请在 Google 窗口登录并打开赠金页面，显示全部记录并清除过滤条件");
      } else if (action === "refresh") {
        await invoke("google_cloud_refresh");
        showToast("正在读取 Google 赠金，完成后自动更新");
      } else if (action === "disconnect") {
        await invoke("google_cloud_disconnect");
        await refresh();
        renderChCards();
      } else if (action === "expiry") {
        const row = button.closest("[data-gcp-credit]");
        const account = row.dataset.gcpAccount;
        const credit = row.dataset.gcpCredit;
        const key = keyOf(account, credit);
        const value = row.querySelector("[data-gcp-date]").value;
        if (account !== dataOf(channel).account) throw new Error("结算账号已变化，请刷新后重试");
        let at = null;
        if (value) {
          at = new Date(value).getTime() / 1000;
          if (!Number.isFinite(at) || at <= 0 || dateValue(at) !== value) throw new Error("到期时间无效");
        }
        if (busy.has(key)) return;
        busy.add(key);
        try {
          await invoke("google_cloud_expiry_set", { account, credit, at });
          drafts.delete(key);
          showToast("赠金到期时间已保存");
        } finally {
          busy.delete(key);
          await refresh();
          renderChCards();
        }
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
      if (result.interactive) showToast("Google 赠金余额已连接，可手动填写到期时间");
    }
  }).catch(error => showToast("Google 赠金状态监听失败，请重启。" + error, true));
  window.GoogleCloudUI = { controls, records, detail, rail };
})();
