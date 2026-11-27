// ───────────── 同步到手机 ─────────────
// 状态与子页共用 SYNC_STATUS；app.js 末尾的 bindSyncUI 绑定按钮。
// SYNC_* 错误码对用户隐藏:界面只显示中文消息,码本身保留在诊断卡与日志里(报障链路不受影响)
function syncUserMsg(e) {
  return String(e ?? "").replace(/^SYNC_[A-Z_]+\s*:\s*/, "");
}

function canUseSyncGithub(status) {
  return !!status?.hasGithub && !status.githubNeedsReauth;
}

// 行内确认条(替代原生 confirm):与删密钥的 .cfrm 同一组件 —— 样式受控、按钮文案可控、
// 不阻塞渲染进程。anchor 是触发按钮;条插在它所在 .acts 行(或所在行容器)之后。
// alt = 可选的推荐动作(如「找回旧组」),排在危险主钮之前。
function armConfirm(anchor, html, onOk, okLabel, alt) {
  document.querySelectorAll(".cfrm.arm").forEach((x) => x.remove());
  const strip = document.createElement("div");
  strip.className = "cfrm arm";
  strip.innerHTML = html +
    (alt ? `<button class="btn p" data-cfrm-alt>${esc(alt.label)}</button>` : "") +
    `<button class="btn danger" data-cfrm-ok>${esc(okLabel)}</button>` +
    `<button class="btn" data-cfrm-no>取消</button>`;
  (anchor.closest(".acts") || anchor.parentElement).after(strip);
  strip.addEventListener("click", (e) => {
    if (e.target.closest("[data-cfrm-ok]")) { strip.remove(); onOk(); }
    else if (e.target.closest("[data-cfrm-alt]")) { strip.remove(); alt.fn(); }
    else if (e.target.closest("[data-cfrm-no]")) strip.remove();
  });
  strip.scrollIntoView({ block: "nearest" });
  return strip;
}
function hideSyncCode() {
  SYNC_CODE_REVEALED = false;
  if ($("syncCompareInput")) $("syncCompareInput").value = "";
  if ($("syncRecoveryInput")) $("syncRecoveryInput").value = "";
  if ($("syncCompareResult")) $("syncCompareResult").textContent = "";
  renderSync();
}

async function loadSyncStatus() {
  const revision = ++SYNC_STATUS_REV;
  try {
    const status = await window.GT_API.sync.status();
    if (revision !== SYNC_STATUS_REV) return true;
    SYNC_STATUS = status;
    SYNC_STATUS_ERROR = null;
  } catch (e) {
    if (revision !== SYNC_STATUS_REV) return false;
    SYNC_STATUS = null;
    SYNC_STATUS_ERROR = String(e);
  }
  renderSync();
  if (manageOpen() && document.body.classList.contains("manage-index")) renderManageIndex();
  return !SYNC_STATUS_ERROR;
}

function renderSync() {
  const s = SYNC_STATUS;
  const setup = $("syncSetup");
  const panel = $("syncPanel");
  if (!setup || !panel) return;
  const failed = !!SYNC_STATUS_ERROR;
  $("syncStatusLoading").hidden = !!s || failed;
  $("syncStatusError").hidden = !failed;
  $("syncStatusErrorText").textContent = failed
    ? "同步设置读取失败(未创建新组): " + syncUserMsg(SYNC_STATUS_ERROR) : "";
  $("syncGithubCard").hidden = !s;
  $("syncCodeCard").hidden = !(s && s.relayId);
  if (!s) {
    setup.hidden = true;
    panel.hidden = true;
    $("syncRecovery").hidden = true;
    renderSyncQr(null);
    return;
  }
  const needsReauth = !!s.githubNeedsReauth;
  const hasGithub = canUseSyncGithub(s);
  const showReauth = !!s.hasGithub || needsReauth;
  const configured = !!s.relayId;

  // GitHub 连接卡在“未建组 / 已建组”两种状态下都要可见。
  const githubReady = s.oauthClientConfigured;
  $("syncGithubState").textContent = needsReauth ? "授权已失效" : hasGithub
    ? `已连接 @${s.githubLogin || "GitHub"}`
    : githubReady
      ? "未连接"
      : "OAuth App 未配置";
  $("syncGithubDot").className =
    "dot " + (hasGithub ? "ok" : needsReauth || githubReady ? "warn" : "mut");
  $("syncGithubMeta").textContent = needsReauth ? "需要重新授权" : hasGithub
    ? "仅 gist 权限"
    : githubReady
      ? "未授权"
      : "缺少 client_id";
  $("syncGithubHint").textContent = needsReauth
    ? "请用创建此同步组的 GitHub 账号重新授权。手机可继续使用现有同步组。"
    : hasGithub
    ? "每台电脑独立授权同一个 GitHub 账号;OAuth 令牌只存 Windows 凭据管理器,不进配置、日志或同步码。"
    : githubReady
      ? "通过 GitHub 官方浏览器页面授权,无需填写 PAT。授权前不会创建 Gist、不会上传数据。"
      : "在 GitHub → Settings → Developer settings 建 OAuth App(勾选 Device Flow),把公开 Client ID 填在下面,只需一次。";
  $("syncClientIdBox").hidden = !!githubReady;
  $("syncClientIdSaveRow").hidden = !!githubReady;
  $("syncGithubConnect").hidden = showReauth;
  $("syncGithubReauth").hidden = !showReauth;
  $("syncGithubReauth").classList.toggle("p", needsReauth);
  $("syncGithubDisconnect").hidden = !s.hasGithub;
  $("syncCreateBtn").disabled = !hasGithub;
  $("syncJoinBtn").disabled = !hasGithub;
  $("syncRotateBtn").disabled = !hasGithub;
  $("syncRecoveryScan").disabled = !hasGithub || SYNC_RECOVERY_BUSY;
  $("syncRecoveryImport").disabled = !hasGithub || SYNC_RECOVERY_BUSY;

  renderSyncQr(configured ? s : null);
  setup.hidden = configured;
  panel.hidden = !configured;
  if (!configured) return;

  $("syncCodeText").textContent = !s.codeText
    ? "无法读取同步码,请重试或找回旧同步组"
    : SYNC_CODE_REVEALED ? s.codeText : "GT1.••••••••••••••••(已隐藏)";
  $("syncCopyBtn").disabled = !s.codeText;
  $("syncRevealBtn").disabled = !s.codeText;
  $("syncRevealBtn").textContent = SYNC_CODE_REVEALED ? "隐藏同步码" : "显示同步码";
  $("syncRevealBtn").setAttribute("aria-pressed", String(SYNC_CODE_REVEALED));
  $("syncErr").textContent = s.keyError
    ? "本机同步密钥读取失败(" + s.keyError + ") · 可点「找回旧同步组」恢复"
    : !s.source
      ? "本机发布来源无法确认,自动发布不可用;同步码仍可用于读取,请从原电脑备份恢复发布状态。"
      : needsReauth ? "请在 GitHub 连接中重新授权后再发布。" : syncUserMsg(s.lastError);
  $("syncDot").className =
    "dot " + (!s.enabled ? "o" : s.lastError ? "b" : hasGithub && s.hasKey ? "" : "w");
  $("syncState").textContent = !s.hasKey ? "同步码不可用" : !s.source ? "发布来源缺失"
    : !s.enabled ? "已暂停" : needsReauth ? "等待重新授权" : s.lastError ? "发布失败" : "已开启";
  const parts = [];
  if (s.lastUploadAt) parts.push("上次上传 " + relTime(s.lastUploadAt));
  parts.push(s.hasKey ? "主密钥在本机" : "主密钥缺失");
  parts.push(needsReauth ? "GitHub 授权已失效" : hasGithub ? "GitHub 已授权" : "GitHub 未连接");
  if (s.pendingSeq) parts.push(`待重试(第 ${s.pendingSeq} 批)`);
  if (s.nextAllowedAt && s.nextAllowedAt > Date.now() / 1000) {
    parts.push("限流至 " + untilTime(s.nextAllowedAt));
  }
  $("syncMeta").textContent = parts.join(" · ");
  $("syncRelayHint").textContent = "中继 " + s.relayId.slice(0, 10) + "… · 可与手机设置中的中继辅助核对";
  $("syncPublishBtn").disabled = !s.hasKey || !s.source || !hasGithub;
  $("syncGroupBtn").disabled = !hasGithub;

  const interval = $("syncInterval");
  if (interval) interval.value = String(s.intervalSec || 60);
  const enabled = $("syncEnabled");
  if (enabled) {
    enabled.checked = !!s.enabled;
    enabled.disabled = !s.enabled && (!s.hasKey || !s.source || !hasGithub);
  }

  const box = $("syncChannels");
  if (box) {
    const selected = new Set(
      (s.channels || []).length ? s.channels : (CHANNELS || []).map((c) => c.id)
    );
    box.innerHTML = (CHANNELS || [])
      .filter((c) => !c.hidden && !c.autoHidden)
      .map(
        (c) =>
          `<label class="row"><input type="checkbox" data-ch="${esc(c.id)}" ${
            selected.has(c.id) || isAntigravity(c) && selected.has("antigravity") ? "checked" : ""
          }><span>${esc(c.name)}</span></label>`
      )
      .join("");
  }
  renderSyncLabels();
}

function renderSyncQr(s) {
  const box = $("syncQr");
  if (!box) return;
  const code = SYNC_CODE_REVEALED && s && s.codeText ? s.codeText : "";
  if (!code) {
    box.hidden = true;
    box.replaceChildren();
    box.dataset.code = "";
    SYNC_QR_CODE = "";
    SYNC_QR_SVG = "";
    SYNC_QR_BUSY = "";
    SYNC_QR_FAILED = "";
    return;
  }
  box.hidden = false;
  if (SYNC_QR_FAILED === code) {
    box.textContent = "二维码生成失败,请手动输入同步码。";
    return;
  }
  if (SYNC_QR_CODE === code && SYNC_QR_SVG) {
    if (box.dataset.code !== code) {
      box.innerHTML = SYNC_QR_SVG;
      box.dataset.code = code;
    }
    return;
  }
  if (SYNC_QR_BUSY === code) return;
  SYNC_QR_BUSY = code;
  box.dataset.code = "";
  box.innerHTML = '<span class="khint">正在生成二维码…</span>';
  window.GT_API.sync.qrSvg()
    .then((svg) => {
      if (SYNC_QR_BUSY === code) SYNC_QR_BUSY = "";
      if (!SYNC_CODE_REVEALED || !SYNC_STATUS || SYNC_STATUS.codeText !== code) return;
      SYNC_QR_CODE = code;
      SYNC_QR_SVG = svg;
      box.innerHTML = svg;
      box.dataset.code = code;
    })
    .catch(() => {
      if (SYNC_QR_BUSY === code) SYNC_QR_BUSY = "";
      if (!SYNC_CODE_REVEALED || !SYNC_STATUS || SYNC_STATUS.codeText !== code) return;
      box.replaceChildren();
      box.textContent = "二维码生成失败,请手动输入同步码。";
      box.dataset.code = "";
      SYNC_QR_FAILED = code;
    });
}

function renderSyncLabels() {
  const box = $("syncLabels");
  if (!box) return;
  const custom = (CHANNELS || []).filter((c) => String(c.id).startsWith("custom-"));
  if (!custom.length) {
    box.innerHTML = "";
    return;
  }
  const labels = (SYNC_STATUS && SYNC_STATUS.platformLabels) || {};
  box.innerHTML = custom
    .map(
      (c) => `<label class="lbl"><span>${esc(c.name)} 的同步标签</span>
        <input class="inp" data-label="${esc(c.id)}" value="${esc(labels[c.id] || "")}"
          placeholder="如 my-relay(同标签跨机合并)" autocomplete="off" spellcheck="false"></label>`
    )
    .join("");
}

async function syncCreate(force) {
  const btn = $("syncCreateBtn");
  btn.disabled = true;
  try {
    try {
      await window.GT_API.sync.create({ allowExisting: force === true });
    } catch (e) {
      if (force === true || !String(e).includes("SYNC_OLD_GROUP_FOUND")) throw e;
      // 行内确认:本机留有旧组,给「找回旧组」与「仍要新建」两条路(替代原生 confirm)
      btn.disabled = false;
      armConfirm(btn,
        "本机留有旧同步组 —— 建议先找回,继续用以前的同步码。仍要新建?",
        () => syncCreate(true), "仍要新建",
        { label: "找回旧组", fn: syncOpenRecovery });
      return;
    }
    const loaded = await loadSyncStatus();
    showToast(loaded ? "同步组已创建 · 同步码已就绪" : "同步组已创建 · 状态读取失败,点「查看组状态」重试", !loaded);
  } catch (e) {
    showToast("创建同步组失败: " + syncUserMsg(e), true);
  } finally {
    renderSync();
  }
}

async function syncJoin() {
  const code = ($("syncJoinCode").value || "").trim();
  if (!code) return showToast("请填写同步码", true);
  const btn = $("syncJoinBtn");
  btn.disabled = true;
  try {
    await window.GT_API.sync.join({ code });
    $("syncJoinCode").value = "";
    await loadSyncStatus();
    showToast("已加入同步组");
  } catch (e) {
    showToast("加入失败: " + syncUserMsg(e), true);
  } finally {
    renderSync();
  }
}

async function syncPublish() {
  const btn = $("syncPublishBtn");
  btn.disabled = true;
  try {
    await window.GT_API.sync.publishNow();
    await loadSyncStatus();
    showToast("已发布");
  } catch (e) {
    showToast("发布失败: " + syncUserMsg(e), true);
  } finally {
    renderSync();
  }
}

async function syncPreview() {
  const out = $("syncPreviewOut");
  try {
    const snap = await window.GT_API.sync.preview();
    out.textContent = JSON.stringify(snap, null, 2);
    showToast("这是本机将上传的明文快照");
  } catch (e) {
    out.textContent = String(e);
    showToast("预览失败: " + syncUserMsg(e), true);
  }
}

async function syncGroup() {
  const out = $("syncPreviewOut");
  try {
    const r = await window.GT_API.sync.readGroup();
    out.textContent = JSON.stringify(r, null, 2);
    if (r.kind === "loaded") showToast(`组内 ${r.merged.sources} 个来源`);
    else showToast("组未变化");
  } catch (e) {
    out.textContent = String(e);
    showToast("读取组状态失败: " + syncUserMsg(e), true);
  }
}

async function syncRotate() {
  if (!SYNC_STATUS || !SYNC_STATUS.relayId) return;
  // 行内确认(替代原生 confirm):说清「换新组」影响哪些设备再动手
  armConfirm($("syncRotateBtn"),
    "重新生成会换新组:已配对的手机和电脑要用新码才能收到新数据。旧组不删,旧码仍能读旧数据。",
    syncRotateGo, "重新生成");
}

async function syncRotateGo() {
  if (!SYNC_STATUS || !SYNC_STATUS.relayId) return;
  const expectedRelayId = SYNC_STATUS.relayId;
  try {
    $("syncRotateBtn").disabled = true;
    const r = await window.GT_API.sync.rotate({ expectedRelayId });
    SYNC_CODE_REVEALED = true;
    $("syncCompareInput").value = "";
    $("syncCompareResult").textContent = "";
    await loadSyncStatus();
    showToast("已生成新同步码" + (r.oldRelayId ? "; 旧中继未删除" : ""));
  } catch (e) {
    showToast("重新生成失败: " + syncUserMsg(e), true);
  } finally {
    renderSync();
  }
}

async function syncScanRecovery() {
  if (SYNC_RECOVERY_BUSY) return;
  const list = $("syncRecoveryList");
  if (!canUseSyncGithub(SYNC_STATUS)) {
    list.textContent = "请先连接创建旧组时使用的 GitHub 账号。";
    return;
  }
  SYNC_RECOVERY_BUSY = true;
  renderSync();
  list.textContent = "正在查找并验证本机旧同步组…";
  try {
    const result = await window.GT_API.sync.recoveryCandidates();
    const rows = (result.candidates || []).map((c) =>
      `<div class="sync-recovery-row"><span>中继 ${esc(String(c.gistId).slice(0, 10))}…<br><span class="khint">${c.hasLocalSource ? "已验证 · 本机发布来源可恢复" : "已验证 · 本机发布来源未确认"}</span></span>
       <button class="btn" data-recover-id="${esc(c.gistId)}" data-epoch="${Number(c.keyEpoch) || 0}">恢复</button></div>`
    );
    if (!rows.length) rows.push('<div class="khint">没有找到可验证的旧组;如有旧同步码,可在下方输入。</div>');
    if (result.unverified) rows.push(`<div class="khint">另有 ${Number(result.unverified)} 个本机记录未能验证,请检查网络和账号,或输入旧码。</div>`);
    if (result.truncated) rows.push('<div class="khint">本次只检查了前 24 个记录;也可直接输入旧码。</div>');
    list.innerHTML = rows.join("");
  } catch (e) {
    list.textContent = "查找失败: " + syncUserMsg(e) + "。请重试或输入旧同步码。";
  } finally {
    SYNC_RECOVERY_BUSY = false;
    renderSync();
  }
}

function syncOpenRecovery() {
  $("syncRecovery").hidden = false;
  const hasGithub = canUseSyncGithub(SYNC_STATUS);
  $("syncRecoveryList").textContent = hasGithub ? "" : "请先连接创建旧组时使用的 GitHub 账号。";
  $("syncRecovery").scrollIntoView({ block: "nearest" });
  if (hasGithub) syncScanRecovery();
}

async function syncRestoreGroup(gistId, code, keyEpoch, anchor) {
  if (SYNC_RECOVERY_BUSY || !SYNC_STATUS) return;
  // 行内确认(替代原生 confirm)
  armConfirm(anchor || $("syncRecoveryImport"),
    "恢复后切换到所选旧组,当前组保留;自动发布先暂停,确认发布状态后再开。",
    () => syncRestoreGroupGo(gistId, code, keyEpoch), "恢复旧组");
}

async function syncRestoreGroupGo(gistId, code, keyEpoch) {
  if (SYNC_RECOVERY_BUSY || !SYNC_STATUS) return;
  const expectedCurrent = SYNC_STATUS.relayId || null;
  SYNC_RECOVERY_BUSY = true;
  renderSync();
  try {
    const result = await window.GT_API.sync.restoreGroup({
      gistId: gistId || null,
      code: code || null,
      keyEpoch: keyEpoch == null ? null : keyEpoch,
      expectedCurrent,
    });
    $("syncRecoveryInput").value = "";
    $("syncRecovery").hidden = true;
    SYNC_CODE_REVEALED = true;
    await loadSyncStatus();
    showToast(result.hasLocalSource
      ? "已找回旧同步组 · 自动发布已暂停"
      : "旧同步码已找回,但本机发布来源未确认,请先恢复原发布状态", !result.hasLocalSource);
  } catch (e) {
    showToast("找回失败: " + syncUserMsg(e), true);
  } finally {
    SYNC_RECOVERY_BUSY = false;
    renderSync();
  }
}

async function syncForget() {
  if (!SYNC_STATUS || !SYNC_STATUS.relayId) return;
  // 行内确认(替代原生 confirm):退出会删本机密钥,写清后果
  armConfirm($("syncForgetBtn"),
    "退出会删除本机主密钥和发布状态;GitHub 上的密文不会自动删除。",
    syncForgetGo, "退出并删除本机密钥");
}

async function syncForgetGo() {
  if (!SYNC_STATUS || !SYNC_STATUS.relayId) return;
  const expectedRelayId = SYNC_STATUS.relayId;
  try {
    await window.GT_API.sync.forget({ expectedRelayId });
    await loadSyncStatus();
    showToast("已退出同步组");
  } catch (e) {
    showToast("退出失败: " + syncUserMsg(e), true);
  }
}

async function syncSetChannelsFromUI() {
  const ids = [...document.querySelectorAll("#syncChannels input[data-ch]")]
    .filter((x) => x.checked)
    .map((x) => x.dataset.ch);
  if (!ids.length) {
    showToast("至少选择一个渠道", true);
    renderSync();
    return;
  }
  try {
    await window.GT_API.sync.setChannels({ channels: ids });
    await loadSyncStatus();
  } catch (e) {
    showToast("保存发布渠道失败: " + syncUserMsg(e), true);
  }
}

async function syncSetEnabledFromUI(on) {
  try {
    await window.GT_API.sync.setEnabled({ enabled: on });
    await loadSyncStatus();
  } catch (e) {
    showToast("切换失败: " + syncUserMsg(e), true);
    renderSync();
  }
}

async function syncSetIntervalFromUI(sec) {
  try {
    await window.GT_API.sync.setInterval({ seconds: Number(sec) });
    await loadSyncStatus();
  } catch (e) {
    showToast("保存间隔失败: " + syncUserMsg(e), true);
  }
}

async function syncSetLabel(id, label) {
  try {
    await window.GT_API.sync.setPlatformLabel({ channelId: id, label });
    await loadSyncStatus();
  } catch (e) {
    showToast("保存同步标签失败: " + syncUserMsg(e), true);
  }
}

// ── 桌面 GitHub Device Flow(不手填 PAT) ───────────────────────────────
async function syncGithubConnect({ currentTarget: btn }) {
  if (SYNC_AUTH || btn.disabled) return;
  btn.disabled = true;
  try {
    const r = await window.GT_API.sync.oauthStart();
    SYNC_AUTH = {
      userCode: r.userCode,
      verificationUri: r.verificationUri,
      expiresAt: Date.now() / 1000 + Number(r.expiresIn || 900),
      interval: Math.max(5, Number(r.interval || 5)),
      timer: null,
    };
    $("syncUserCode").textContent = r.userCode;
    $("syncAuthStatus").textContent = `等待授权 · ${Math.max(1, Math.round(Number(r.expiresIn || 900) / 60))} 分钟内有效`;
    $("syncAuth").hidden = false;
    syncAuthSchedule(SYNC_AUTH.interval);
  } catch (e) {
    showToast("连接 GitHub 失败: " + syncUserMsg(e), true);
  } finally {
    btn.disabled = false;
  }
}

function syncAuthSchedule(sec) {
  if (!SYNC_AUTH) return;
  clearTimeout(SYNC_AUTH.timer);
  SYNC_AUTH.timer = setTimeout(syncAuthPoll, Math.max(5, Number(sec) || 5) * 1000);
}

async function syncAuthPoll() {
  if (!SYNC_AUTH) return;
  if (Date.now() / 1000 >= SYNC_AUTH.expiresAt) {
    await syncAuthCancel(true);
    showToast("授权码已过期,点「连接 GitHub」重新开始", true);
    return;
  }
  try {
    const r = await window.GT_API.sync.oauthPoll();
    if (!SYNC_AUTH) return; // 等待期间用户已取消/页面已关闭
    if (r.kind === "pending") {
      $("syncAuthStatus").textContent = "等待你在 GitHub 授权…";
      syncAuthSchedule(r.interval);
    } else if (r.kind === "authorized") {
      clearTimeout(SYNC_AUTH.timer);
      SYNC_AUTH = null;
      $("syncAuth").hidden = true;
      await loadSyncStatus();
      showToast("已连接 @" + r.login);
    } else if (r.kind === "error") {
      clearTimeout(SYNC_AUTH.timer);
      SYNC_AUTH = null;
      $("syncAuthStatus").textContent = r.message;
      showToast("GitHub 授权失败:" + r.message, true);
    } else {
      clearTimeout(SYNC_AUTH.timer);
      SYNC_AUTH = null;
      $("syncAuth").hidden = true;
    }
  } catch (e) {
    $("syncAuthStatus").textContent = "轮询失败,稍后重试";
    syncAuthSchedule(5);
  }
}

async function syncAuthCancel(silent = false) {
  if (SYNC_AUTH) clearTimeout(SYNC_AUTH.timer);
  SYNC_AUTH = null;
  $("syncAuth").hidden = true;
  try { await window.GT_API.sync.oauthCancel(); } catch { /* silent: 后台授权会话已结束或已取消 */ }
  if (!silent) showToast("已取消 GitHub 授权");
}

async function syncAuthCopyOpen() {
  if (!SYNC_AUTH) return;
  try { await window.GT_API.desktop.copyText({ text: SYNC_AUTH.userCode }); } catch { /* silent: 剪贴板写入失败时页面已展示授权码可手动抄写 */ }
  try {
    await window.GT_API.desktop.openUrl({ url: SYNC_AUTH.verificationUri });
    $("syncAuthStatus").textContent = "已复制授权码。请在 GitHub 页面输入它,并确认只授予 gist 权限。";
  } catch (e) {
    showToast("打开浏览器失败:" + e, true);
  }
}

async function syncAuthOpenPage() {
  if (!SYNC_AUTH) return;
  try {
    await window.GT_API.desktop.openUrl({ url: SYNC_AUTH.verificationUri });
  } catch (e) {
    showToast("打开浏览器失败:" + e, true);
  }
}

async function syncGithubDisconnect() {
  // 行内确认(替代原生 confirm)
  armConfirm($("syncGithubDisconnect"),
    "断开后本机停止发布;手机仍可用同步码匿名读取。重新授权即可恢复。",
    syncGithubDisconnectGo, "断开");
}

async function syncGithubDisconnectGo() {
  if (!SYNC_STATUS || !SYNC_STATUS.hasGithub) return;
  try {
    await window.GT_API.sync.disconnectGithub();
    await loadSyncStatus();
    showToast("已断开 GitHub 授权");
  } catch (e) {
    showToast("断开失败: " + syncUserMsg(e), true);
  }
}

async function syncSaveClientId() {
  const id = ($("syncClientId").value || "").trim();
  if (!id) return showToast("请填写 OAuth App Client ID", true);
  try {
    await window.GT_API.sync.setOauthClientId({ clientId: id });
    $("syncClientId").value = "";
    await loadSyncStatus(); // 保存后 GitHub 卡从「OAuth App 未配置」切到「未连接」,同屏反馈足够
  } catch (e) {
    showToast("保存 Client ID 失败: " + syncUserMsg(e), true);
  }
}

async function syncOpenDevSettings() {
  try {
    await window.GT_API.desktop.openUrl({ url: "https://github.com/settings/developers" });
  } catch (e) {
    showToast("打开 GitHub 失败:" + e, true);
  }
}

function bindSyncUI() {
  if (!$("syncCreateBtn")) return;
  $("syncStatusRetry").addEventListener("click", () => {
    SYNC_STATUS_ERROR = null;
    renderSync();
    loadSyncStatus();
  });
  $("syncCreateBtn").addEventListener("click", () => syncCreate(false));
  $("syncJoinBtn").addEventListener("click", syncJoin);
  $("syncPublishBtn").addEventListener("click", syncPublish);
  $("syncPreviewBtn").addEventListener("click", syncPreview);
  $("syncGroupBtn").addEventListener("click", syncGroup);
  $("syncRotateBtn").addEventListener("click", syncRotate);
  $("syncForgetBtn").addEventListener("click", syncForget);
  $("syncGithubConnect").addEventListener("click", syncGithubConnect);
  $("syncGithubReauth").addEventListener("click", syncGithubConnect);
  $("syncGithubDisconnect").addEventListener("click", syncGithubDisconnect);
  $("syncSaveClientId").addEventListener("click", syncSaveClientId);
  $("syncOpenDevSettings").addEventListener("click", syncOpenDevSettings);
  $("syncAuthCopy").addEventListener("click", syncAuthCopyOpen);
  $("syncAuthOpen").addEventListener("click", syncAuthOpenPage);
  $("syncAuthCancel").addEventListener("click", () => syncAuthCancel(false));
  $("syncAuth").addEventListener("click", (e) => {
    if (e.target === $("syncAuth")) syncAuthCancel(false);
  });
  $("syncOptionsBtn").addEventListener("click", () => {
    const box = $("syncOptions");
    box.hidden = !box.hidden;
    $("syncOptionsBtn").setAttribute("aria-expanded", String(!box.hidden));
    if (box.hidden) {
      $("syncCompareInput").value = "";
      $("syncCompareResult").textContent = "";
      $("syncCompare").hidden = true;
    }
  });
  $("syncCompareOpen").addEventListener("click", () => {
    $("syncCompare").hidden = !$("syncCompare").hidden;
    $("syncCompareInput").value = "";
    $("syncCompareResult").textContent = "";
  });
  $("syncCompareBtn").addEventListener("click", () => {
    const old = $("syncCompareInput").value.trim();
    const current = (SYNC_STATUS && SYNC_STATUS.codeText) || "";
    $("syncCompareResult").textContent = !current ? "当前同步码无法读取,请先找回。"
      : !old ? "请先输入旧同步码。"
      : old === current ? "两次同步码完全相同。" : "同步码不同,属于不同的同步凭据。";
    $("syncCompareInput").value = "";
  });
  $("syncRecoverOpen").addEventListener("click", syncOpenRecovery);
  $("syncRecoverOpenSetup").addEventListener("click", syncOpenRecovery);
  $("syncRecoveryScan").addEventListener("click", syncScanRecovery);
  $("syncRecoveryClose").addEventListener("click", () => {
    $("syncRecovery").hidden = true;
    $("syncRecoveryInput").value = "";
  });
  $("syncRecoveryImport").addEventListener("click", () => {
    const code = $("syncRecoveryInput").value.trim();
    if (!code) return showToast("请先输入旧同步码", true);
    syncRestoreGroup(null, code, null);
  });
  $("syncRecoveryList").addEventListener("click", (e) => {
    const button = e.target.closest("button[data-recover-id]");
    if (button) syncRestoreGroup(button.dataset.recoverId, null, Number(button.dataset.epoch), button);
  });
  $("syncRevealBtn").addEventListener("click", () => {
    SYNC_CODE_REVEALED = !SYNC_CODE_REVEALED;
    renderSync();
  });
  $("syncCopyBtn").addEventListener("click", async () => {
    const text = (SYNC_STATUS && SYNC_STATUS.codeText) || "";
    if (!text) return showToast("没有可复制的同步码", true);
    try {
      await window.GT_API.desktop.copyText({ text });
      showToast("同步码已复制");
    } catch (e) {
      showToast("复制失败:" + e, true);
    }
  });
  $("syncInterval").addEventListener("change", (e) => syncSetIntervalFromUI(e.target.value));
  $("syncEnabled").addEventListener("change", (e) => syncSetEnabledFromUI(e.target.checked));
  $("syncChannels").addEventListener("change", (e) => {
    if (e.target.matches("input[data-ch]")) syncSetChannelsFromUI();
  });
  $("syncLabels").addEventListener("change", (e) => {
    const id = e.target.dataset.label;
    if (id) syncSetLabel(id, e.target.value);
  });
  loadSyncStatus();
  listen("sync-status", (e) => {
    SYNC_STATUS_REV++;
    SYNC_STATUS = e.payload;
    SYNC_STATUS_ERROR = null;
    renderSync();
    if (manageOpen() && document.body.classList.contains("manage-index")) renderManageIndex();
  });
  listen("sync-error", (e) => showToast("同步失败: " + syncUserMsg(e.payload), true));
}
bindSyncUI();

