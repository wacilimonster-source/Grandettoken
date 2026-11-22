// ───────────── 本地密钥箱 ─────────────
// 设计见 docs/design/design-vault-and-hidden-restore.html 第三节(1A–7A 已批)。
// 三条要点:密文在 Windows 凭据管理器、SQLite 只存索引(列表不需要解密)、
// 明文只在「显示」那一次回界面且 8 秒自动隐;「复制」根本不回前端。
let VAULT = [];            // 索引列表(vault_list 的结果)
let VAULT_Q = "";          // 搜索词
let VAULT_SORT = "updated";
try { const saved = localStorage.getItem("vault-sort"); if (["updated", "created", "platform", "group"].includes(saved)) VAULT_SORT = saved; } catch (_) { /* silent: 排序偏好读不出就用默认顺序 */ }
const vaultSortEl = $("vaultSort");
if (vaultSortEl) { vaultSortEl.value = VAULT_SORT; vaultSortEl.addEventListener("change", () => { VAULT_SORT = vaultSortEl.value; try { localStorage.setItem("vault-sort", VAULT_SORT); } catch (_) { /* silent: 排序偏好存不下只影响下次默认值 */ } renderVault(); }); }
let VAULT_EDIT = null;     // 正在编辑/新建的表单:{id?, platform, label, key, note}
let VAULT_DRAFT = null;    // 表单里已敲未存的内容:{platform,label,key,note}(见 renderVault)
// vault_list 的失败原因:读失败 ≠ 箱子是空的 —— 前者说"读不出来 + 重试",
// 后者才是"去新增一把"。把故障画成空态会让用户以为密钥全丢了(F3,
// 与渠道卡 1417-1419 的「凭据库读取失败」同一条纪律)。
let VAULT_ERR = "";
let VAULT_SAVING = false;

const VAULT_PLATFORM_HINT = ["4SAPI", "Hapi", "OpenCode Go", "DeepSeek", "OpenAI", "Anthropic", "Gemini", "Moonshot", "智谱", "阿里云百炼", "腾讯", "火山引擎", "书生端砚"];

async function loadVault() {
  try {
    VAULT = await invoke("vault_list");
    VAULT_ERR = "";
  } catch (e) {
    VAULT = [];
    VAULT_ERR = String(e);
    showToast("读密钥箱失败:" + e, true);
  }
  // 反查关系要在渲染前就位:列表里要标"正被谁使用",而那是渲染时的同步判断
  await loadVaultRefs();
  renderVault();
}

/** 表单在列表上方:从底部分组点「新增 / 改」时,滚过去并聚焦第一个输入框。 */
function focusVaultForm() {
  setTimeout(() => {
    const form = $("vaultForm");
    if (form && form.scrollIntoView) form.scrollIntoView({ block: "nearest" });
    const el = $("vPlat");
    if (el) el.focus({ preventScroll: true });
  }, 30);
}

/** 保存/更新成功后就地亮一下那一行(表单已收起,列表可能已经滚到别处)。 */
function flashVaultRow(id) {
  const row = document.querySelector(`#vaultList .vrow[data-id="${id}"]`);
  if (!row) return false;
  const item = row.closest(".vitem");
  if (!item) return false;
  item.classList.add("flash");
  if (row.scrollIntoView) row.scrollIntoView({ block: "nearest" });
  setTimeout(() => item.classList.remove("flash"), 1150);
  return true;
}

function vaultSorted() {
  const query = VAULT_Q.trim().toLowerCase();
  const rows = VAULT.filter(it => !query || `${it.platform} ${it.label} ${it.note} ${it.hint} ${it.apiEndpoint || ""} ${it.website || ""}`.toLowerCase().includes(query));
  const cmp = (a, b) => String(a || "").localeCompare(String(b || ""), "zh-CN", { sensitivity: "base" });
  rows.sort((a, b) => {
    let n = 0;
    if (VAULT_SORT === "created") n = (b.createdAt || 0) - (a.createdAt || 0);
    else if (VAULT_SORT === "platform") n = cmp(a.platform, b.platform) || cmp(a.label, b.label);
    else if (VAULT_SORT === "group") n = cmp(a.label, b.label) || cmp(a.platform, b.platform);
    else n = (b.updatedAt || 0) - (a.updatedAt || 0);
    return n || (b.updatedAt || 0) - (a.updatedAt || 0) || cmp(a.id, b.id);
  });
  return rows;
}

function renderVault() {
  window.VaultUI.render({
    items: VAULT, rows: vaultSorted(), query: VAULT_Q, error: VAULT_ERR,
    edit: VAULT_EDIT, draft: VAULT_DRAFT, saving: VAULT_SAVING, platformHints: VAULT_PLATFORM_HINT,
    refsOf: refChannelsOf, channelName, esc,
  });
}

/** 把表单里的内容提交(新建或改名/改备注/换密钥)。 */
async function vaultSave() {
  const e = VAULT_EDIT;
  if (!e || VAULT_SAVING) return;
  const payload = {
    id: e.id || null,
    platform: ($("vPlat").value || "").trim(),
    label: ($("vLabel").value || "").trim(),
    key: ($("vKey").value || "").trim(),
    note: ($("vNote").value || "").trim(),
    apiEndpoint: ($("vEndpoint").value || "").trim(),
    website: ($("vWebsite").value || "").trim(),
  };
  VAULT_SAVING = true;
  const saveButton = document.querySelector('#vaultForm [data-act="vsave"]');
  if (saveButton) saveButton.disabled = true;
  try {
    const saved = await invoke("vault_save", { item: payload });
    const sameForm = VAULT_EDIT === e;
    if (sameForm) {
      VAULT_EDIT = null;
      VAULT_DRAFT = null;
    }
    // 条目名/尾号变了 → 引用它的渠道卡"密钥来源"要重读
    invalidateKeyMeta(null, "source");
    showToast(e.id ? "已更新" : "已存进密钥箱");
    await loadVault();
    if (saved && saved.id && (sameForm || !VAULT_EDIT)) {
      // 搜索词可能把刚存的那条挡住:清掉筛选再亮一次,否则"已存进密钥箱"之后
      // 眼前什么都没变,用户会以为没存上。
      if (!flashVaultRow(saved.id) && VAULT_Q) {
        VAULT_Q = "";
        if ($("vaultQ")) $("vaultQ").value = "";
        renderVault();
        flashVaultRow(saved.id);
      }
    }
    if (manageOpen()) await fillKeySources(true);
  } catch (err) {
    showToast(String(err), true);
  } finally {
    VAULT_SAVING = false;
    const button = document.querySelector('#vaultForm [data-act="vsave"]');
    if (button) button.disabled = false;
  }
}

/** 删除要内联确认(与渠道密钥删除同一套做法,不用原生 confirm)。 */
function vaultDel(id) {
  const row = document.querySelector(`.vrow [data-act="vdel"][data-id="${id}"]`);
  if (!row) return;
  // N07(2026-10-21):这里**不再**用索引判断"有没有人在用" —— 拦截已经搬到后端,
  // 而且后端扫的是真实指针(书架)。索引读坏了、过期了都不影响判断。
  // 所以这里只问一句"删吗",真正的后果提示由后端把清单交回来后再说。
  const strip = document.createElement("span");
  strip.className = "cfrm";
  strip.innerHTML = `删除这一把?<button class="btn s danger" data-act="vdel-yes" data-id="${esc(id)}">删除</button>
    <button class="btn s" data-act="vdel-no">取消</button>`;
  row.parentElement.appendChild(strip);
  row.remove();
}

