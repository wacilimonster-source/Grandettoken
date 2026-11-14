/* 密钥箱展示状态。索引与密钥写入仍由原有命令处理。 */
window.VaultUI = (() => {
  let openId = null;
  let revealed = null;
  let revealGeneration = 0;
  let revealTimer = null;
  let extrasOpen = false;
  let resetForm = false;

  function hideReveal() {
    revealGeneration++;
    revealed = null;
    clearTimeout(revealTimer);
    revealTimer = null;
    document.querySelectorAll("#vaultList .vrow-reveal").forEach((el) => el.remove());
  }

  function reset() {
    openId = null;
    hideReveal();
  }

  function beginForm() {
    extrasOpen = false;
    resetForm = true;
  }

  function focusAction(id, action) {
    const button = [...document.querySelectorAll(`#vaultList [data-act="${action}"]`)]
      .find((el) => el.dataset.id === id);
    if (button) button.focus({ preventScroll: true });
  }

  function toggle(id, render) {
    hideReveal();
    openId = openId === id ? null : id;
    render();
    focusAction(id, "vfold");
  }

  async function reveal(id, invoke, render, showToast) {
    if (openId !== id) return;
    if (revealed && revealed.id === id) {
      hideReveal();
      render();
      focusAction(id, "vreveal");
      return;
    }
    const request = ++revealGeneration;
    try {
      const value = await invoke("vault_reveal", { id });
      if (request !== revealGeneration || openId !== id) return;
      revealed = { id, value };
      render();
      focusAction(id, "vreveal");
      clearTimeout(revealTimer);
      revealTimer = setTimeout(() => {
        hideReveal();
        render();
      }, 8000);
    } catch (err) {
      if (request === revealGeneration && openId === id) showToast("取密钥失败:" + err, true);
    }
  }

  function render(state) {
    const { items, rows, query, error, edit, draft, saving, platformHints, refsOf, channelName, esc } = state;
    const list = document.getElementById("vaultList");
    const form = document.getElementById("vaultForm");
    if (!list || !form) return;

    const identity = edit ? edit.id || "new" : "";
    const previous = form.querySelector(".vform-extra");
    if (!resetForm && previous && form.dataset.editIdentity === identity) extrasOpen = previous.open;
    resetForm = false;
    form.dataset.editIdentity = identity;
    const d = draft || edit || {};
    const filledExtra = !!(d.note || d.apiEndpoint || d.website);
    const platforms = [...new Set([...items.map((item) => item.platform), ...platformHints])];
    form.innerHTML = edit
      ? `<div class="vform">
          <div class="vform-head"><b>${edit.id ? "编辑密钥" : "新增密钥"}</b><span>${edit.id ? "密钥留空 = 不改这把" : "保存后列表只留尾 4 位"}</span></div>
          <div class="vf"><span>选择平台</span><select class="sel" id="vPlatSelect"><option value="">选择平台或自行输入</option>${platforms.map((p) => `<option value="${esc(p)}">${esc(p)}</option>`).join("")}</select></div>
          <div class="vf"><span>平台名称</span><input class="inp" id="vPlat" list="vPlatList" value="${esc(d.platform)}" placeholder="选择或输入新平台">
            <datalist id="vPlatList">${platforms.map((p) => `<option value="${esc(p)}">`).join("")}</datalist></div>
          <div class="vf"><span>分组</span><input class="inp" id="vLabel" value="${esc(d.label)}" placeholder="如 主账号 / 工作"></div>
          <div class="vf"><span>密钥</span><input class="inp" id="vKey" type="password" autocomplete="off" value="${esc(d.key || "")}" placeholder="${edit.id ? "留空 = 不改这把" : "粘贴密钥"}"></div>
          <details class="vform-extra" ${extrasOpen ? "open" : ""}><summary>补充信息（选填）${filledExtra ? '<span>已填写</span>' : ""}</summary>
            <div class="vf"><span>账号</span><input class="inp" id="vNote" value="${esc(d.note)}" placeholder="选填，如邮箱或账号名"></div>
            <div class="vf"><span>调用端点</span><input class="inp" id="vEndpoint" type="url" value="${esc(d.apiEndpoint || "")}" placeholder="选填，如 https://example.com/v1"></div>
            <div class="vf"><span>官网地址</span><input class="inp" id="vWebsite" type="url" value="${esc(d.website || "")}" placeholder="选填，如 https://example.com"></div>
          </details>
          <div class="vform-acts"><button class="btn p" data-act="vsave" ${saving ? "disabled" : ""}>保存</button><button class="btn" data-act="vcancel">取消</button>
            <span class="vform-note">密钥写进 Windows 凭据管理器;按 Esc 取消,回车保存。</span></div>
        </div>`
      : "";

    const select = document.getElementById("vPlatSelect");
    if (select) select.onchange = () => {
      if (!select.value) return;
      const input = document.getElementById("vPlat");
      input.value = select.value;
      input.dispatchEvent(new Event("input", { bubbles: true }));
    };
    const qclear = document.getElementById("vaultQClear");
    if (qclear) qclear.hidden = !query;
    const meta = document.getElementById("vaultMeta");
    const platformCount = new Set(items.map((item) => item.platform.toLocaleLowerCase())).size;
    if (meta) meta.textContent = error ? "读取失败" : query
      ? `匹配 ${rows.length} 把 / 共 ${items.length} 把`
      : `${items.length} 把 · ${platformCount} 个平台`;

    if (openId && (error || !rows.some((item) => item.id === openId))) reset();
    if (error) {
      list.innerHTML = `<div class="empty"><div class="ek"><svg aria-hidden="true"><use href="#i-key"/></svg></div>
        <b class="badc">读不出密钥箱</b><span>${esc(error)}</span>
        <div><button class="btn p" data-act="vreload">重试</button></div>
        <div class="note">列表(以及里面的密钥)并没有丢,只是这一次没读出来</div></div>`;
      return;
    }
    if (!items.length) {
      list.innerHTML = `<div class="empty"><div class="ek"><svg aria-hidden="true"><use href="#i-key"/></svg></div>
        <b>密钥箱还是空的</b><span>把常用平台的密钥存进来,换机或重填时不用再翻聊天记录。</span>
        <div><button class="btn p" data-act="vnew">新增一把密钥</button></div></div>`;
      return;
    }
    if (!rows.length) {
      list.innerHTML = `<div class="empty"><div class="ek"><svg aria-hidden="true"><use href="#i-search"/></svg></div>
        <b>没有匹配的条目</b><span>「${esc(query)}」没有命中平台、分组、账号、地址或尾号。</span>
        <div><button class="btn" data-act="vqclear">清除搜索</button></div></div>`;
      return;
    }
    list.innerHTML = '<div class="vgroup">' + rows.map((item) => row(item, refsOf(item.id), channelName, esc)).join("") + "</div>";
  }

  function row(item, refs, channelName, esc) {
    const id = esc(item.id);
    const isOpen = openId === item.id;
    const unavailable = !!item.defaultChannel && item.defaultAvailable === false;
    const detailId = "vault-detail-" + encodeURIComponent(item.id);
    const title = `${item.platform} · ${item.label}`;
    const created = new Date(item.createdAt * 1000).toLocaleDateString("zh-CN");
    const updated = item.updatedAt !== item.createdAt ? ` · ${new Date(item.updatedAt * 1000).toLocaleDateString("zh-CN")} 更新` : "";
    const flags = [
      item.defaultChannel ? '<span>渠道默认 · 只读</span>' : "",
      unavailable ? '<span class="vrow-warning">渠道未配置密钥</span>' : "",
      refs.length ? `<span class="vrow-ref">正被 ${esc(refs.map(channelName).join("、"))} 使用</span>` : "",
    ].join("");
    const detail = isOpen ? `
      ${item.defaultChannel ? '<div class="vrow-sub">请在渠道管理中修改默认密钥</div>' : ""}
      ${item.note ? `<div class="vrow-sub">账号：${esc(item.note)}</div>` : ""}
      ${item.apiEndpoint ? `<div class="vrow-sub vrow-address"><span>调用端点</span><span>${esc(item.apiEndpoint)}</span></div>` : ""}
      ${item.website ? `<div class="vrow-sub vrow-address"><span>官网</span><span>${esc(item.website)}</span></div>` : ""}
      <div class="vrow-sub">${created} 创建${updated}</div>
      <div class="vrow-detail-acts">
        <button class="btn s" data-act="vreveal" data-id="${id}" ${unavailable ? "disabled" : ""} title="显示明文，8 秒后隐藏">${revealed && revealed.id === item.id ? "隐藏" : "显示"}</button>
        ${item.defaultChannel ? "" : `<button class="btn s" data-act="vedit" data-id="${id}">编辑</button><button class="btn s danger" data-act="vdel" data-id="${id}">删除</button>`}
      </div>
      ${revealed && revealed.id === item.id ? `<div class="reveal vrow-reveal">${esc(revealed.value)}</div>` : ""}` : "";
    return `<div class="vitem"><div class="vrow" data-id="${id}">
      <div class="vrow-head"><button type="button" class="vrow-toggle" data-act="vfold" data-id="${id}" aria-expanded="${isOpen}" aria-controls="${esc(detailId)}" title="${esc(title)}">
        <span class="vrow-caret" aria-hidden="true">${isOpen ? "▾" : "▸"}</span><b>${esc(title)}</b></button>
        ${unavailable ? "" : `<span class="vrow-hint">${esc(item.hint)}</span>`}</div>
      <div class="vrow-summary"><span class="vrow-account" title="${esc(item.note || "")}">${item.note ? `账号：${esc(item.note)}` : ""}</span>
        <div class="vrow-acts"><button class="btn s" data-act="vcopy" data-id="${id}" ${unavailable ? "disabled" : ""} title="复制密钥">复制密钥</button>
        <button class="btn s" data-act="vcopy-endpoint" data-id="${id}" ${item.apiEndpoint ? "" : "disabled"} title="${item.apiEndpoint ? "复制调用端点" : "尚未填写调用端点"}">复制端点</button></div></div>
      ${flags ? `<div class="vrow-flags">${flags}</div>` : ""}
      <div class="vrow-details" id="${esc(detailId)}" ${isOpen ? "" : "hidden"}>${detail}</div>
    </div></div>`;
  }

  return { beginForm, hideReveal, render, reset, reveal, toggle };
})();
