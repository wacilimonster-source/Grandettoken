// ───────────── 快捷键录制 ─────────────
// 存的是 Tauri accelerator 原始格式(KeyG/Digit1 这类 code 名),展示时美化
const HK_KEY = /^(Key[A-Z]|Digit[0-9]|F([1-9]|1[0-2])|Space|Enter|Tab|Arrow(Up|Down|Left|Right)|Home|End|PageUp|PageDown|Minus|Equal|Comma|Period|Slash)$/;
const hkLabel = (v) =>
  (v || "").replace(/Key([A-Z])/g, "$1").replace(/Digit(\d)/g, "$1").replace("Arrow", "");
async function saveHotkey(acc) {
  const old = CFG.hotkey || "";
  CFG.hotkey = acc;
  $("cfgHotkey").value = hkLabel(acc) || "未设置";
  try {
    await invoke("set_config", { config: CFG });
  } catch (err) {
    // 注册失败(多半被别的程序占用):回滚界面,别让用户以为设上了
    CFG.hotkey = old;
    $("cfgHotkey").value = hkLabel(old) || "未设置";
    showToast("快捷键设置失败:" + err, true);
  }
}
$("cfgHotkey").addEventListener("keydown", (e) => {
  e.preventDefault();
  e.stopPropagation(); // 别撞上文档级的 Esc 处理
  if (e.key === "Escape") return saveHotkey("");
  const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super"].filter(Boolean);
  if (!mods.length) return; // 只按修饰键:继续等主键
  if (HK_KEY.test(e.code)) saveHotkey(mods.join("+") + "+" + e.code);
});

/** role="switch" 的类与 aria-checked 必须一起落(语义全靠它)。 */
function setSwitch(id, on) {
  const el = $(id);
  if (!el) return;
  el.classList.toggle("on", !!on);
  el.setAttribute("aria-checked", String(!!on));
}

/** 把 CFG 推回静态控件(下拉 + 全部 bindSwitch 开关)。启动时与**保存失败回滚时**
    共用同一份:以前只有启动时同步,保存失败只重画列表,控件停在新值上,
    重启才"变回去"(F6)。 */
function syncStaticControls() {
  if (!CFG) return;
  const set = (id, v) => {
    const el = $(id);
    if (el) el.value = String(v);
  };
  set("cfgActive", CFG.activeIntervalSec);
  set("cfgIdle", CFG.idleIntervalSec);
  set("cfgBackoff", CFG.backoffIntervalSec);
  set("cfgSort", CFG.sort);
  set("cfgWarn", CFG.warnPercent);
  set("cfgCrit", CFG.critPercent);
  set("cfgFontScale", CFG.fontScale || "md");
  applyTheme();
  updateLayoutUI();
  const hk = $("cfgHotkey");
  if (hk) hk.value = hkLabel(CFG.hotkey) || "未设置";
  setSwitch("cfgAutostart", CFG.autostart);
  setSwitch("cfgAutoUpdate", CFG.autoCheckUpdate !== false);
  setSwitch("cfgTrayNum", CFG.trayShowNumber !== false);
  setSwitch("cfgPulse", CFG.alertPulse);
  setSwitch("cfgPeek", CFG.peekOnClick !== false);
  setSwitch("cfgNotify", CFG.notifyOnCrit !== false);
  setSwitch("cfgCheckinNotify", CFG.checkinNotify !== false);
  setSwitch("cfgMinimaxWriteback", CFG.minimaxWriteback !== false);
  setSwitch("cfgSound", !!CFG.soundOn);
  setSwitch("cfgDock", CFG.dockEnabled);
  setSwitch("cfgCompact", !!CFG.compactEnabled);
  setSwitch("cfgPin", CFG.alwaysOnTop);
  setSwitch("cfgTextTone", !!CFG.textTone);
  setSwitch("cfgShowUsed", !!CFG.showUsed);
  const mf = $("cfgMoneyFloor");
  if (mf) mf.value = String(CFG.moneyFloor > 0 ? CFG.moneyFloor : 10);
  document.body.classList.toggle("pulse", !!CFG.alertPulse);
  document.body.classList.toggle("texttone", !!CFG.textTone);
}

function applyConfigToUI() {
  if (!CFG) return;
  applyCompactUI();
  syncStaticControls();
  renderAbout();
}

// ───────────── 检查更新 ─────────────
// 判断全在 Rust 侧(开关、24 小时节流、跳过此版本、版本比较),前端只负责渲染。
// 前端是无打包器的静态页,拿不到 @tauri-apps/plugin-updater,所以走命令 + 事件,
// 和 get_channels / channels-updated 同一套路子。
let UPD = { info: null, last: null };

const fmtMB = (n) => (n / 1048576).toFixed(1) + " MB";

// ───────────── 更新提示:标题旁的标签 + 覆盖卡片 ─────────────
// 有新版才出现;自动检查失败一律静默。折叠形态不显示(宽度敏感)。
// 版本信息只存 UPD.info 一处(曾有两份副本,「跳过此版本」只清了一份,
// 角标残留、覆盖层按钮失灵 —— 见 bug 报告 #3);UPDC 只放瞬时进度状态。
const UPDC = { phase: null, pct: 0, rec: 0, total: 0 };

const updBusy = () => UPDC.phase === "downloading" || UPDC.phase === "installing";
const updProgressText = () =>
  UPDC.total
    ? UPDC.pct + "% · " + fmtMB(UPDC.rec) + " / " + fmtMB(UPDC.total)
    : fmtMB(UPDC.rec);

function renderUpdChip() {
  const chip = $("updChip");
  if (!chip) return;
  let text = "";
  // 角标挤在标题栏里,它的宽度直接决定右侧按钮能不能放下:实测「新版本 0.1.15」
  // 85px 会把「隐藏」整个推出窗口外。文案压到最短,版本号与进度细节进 tooltip。
  if (UPDC.phase === "ready" && UPD.info) text = "新版本";
  else if (UPDC.phase === "downloading") text = "↓" + UPDC.pct + "%";
  else if (UPDC.phase === "installing") text = "安装中";
  else if (UPDC.phase === "failed") text = "重试";
  chip.textContent = text;
  chip.style.display = text ? "" : "none";
  const detail = [UPD.info && UPD.info.version ? "版本 " + UPD.info.version : "", updProgressText()].filter(Boolean).join(" · ");
  chip.title = text ? "点开查看更新详情" + (detail ? "(" + detail + ")" : "") : "";
}

/** 进度条有两个载体(管理页卡片 + 覆盖层),一起写,谁可见谁生效。 */
function paintUpdBars() {
  const show = updBusy();
  const txt = UPDC.phase === "installing" ? "安装中…" : updProgressText();
  for (const [bar, fill, label] of [
    ["updBar", "updBarI", "updPct"],
    ["ovBar", "ovBarI", "ovPct"],
  ]) {
    const b = $(bar);
    if (!b) continue;
    // 不忙时必须收起:只写 block 不写 none,失败/完成后条子会永远挂在 87%
    b.style.display = show ? "block" : "none";
    $(fill).style.width = UPDC.pct + "%";
    $(label).textContent = txt;
  }
}

function overlayUpdState() {
  // 覆盖层打开时按当前 phase 重画:进度条、按钮可用性与文案
  const busy = updBusy();
  $("ovGo").disabled = busy;
  $("ovSkip").disabled = busy;
  $("ovGo").textContent = UPDC.phase === "failed" ? "重试更新" : "立即更新";
  const msg = $("ovMsg");
  if (busy) {
    $("ovBar").style.display = "block";
    msg.style.display = "none";
    paintUpdBars();
  } else {
    $("ovBar").style.display = "none";
    if (UPDC.phase === "failed") {
      msg.textContent = "上次下载未完成,点「重试更新」继续";
      msg.style.display = "block";
    } else {
      msg.style.display = "none";
    }
  }
}

const ovOpen = () => $("updOv").style.display !== "none";

function openUpdOverlay() {
  const st = UPD.info;
  $("ovVer").textContent = st ? "发现新版本 " + st.version : "更新";
  $("ovDate").textContent = (st && st.date) || "";
  $("ovNotes").textContent = ((st && st.notes) || "").trim() || "(无更新说明)";
  $("updOv").style.display = "";
  overlayUpdState();
}
function closeUpdOverlay() { $("updOv").style.display = "none"; }

$("updChip").addEventListener("click", openUpdOverlay);
$("ovLater").addEventListener("click", closeUpdOverlay);
// 「立即更新 / 跳过」与管理页卡片共用同一个处理(避免两份实现分叉)
$("ovGo").addEventListener("click", startInstall);
$("ovSkip").addEventListener("click", () => $("btnUpdSkip").click());

function setMsg(text, bad) {
  const el = $("updMsg");
  el.textContent = text || "";
  el.style.display = text ? "block" : "none";
  el.classList.toggle("bad", !!bad);
}

function renderAbout(st) {
  // #uver 是「Grandettoken」字面量后面的小后缀,只放版本号:再带一遍产品名会
  // 渲染成 "GrandettokenGrandettoken 0.1.13"。后端首检之前 st 是空的,用 init
  // 时问到的 VER 兜底,关于页不会一直空着(报告 P2-16)
  const v = (st && st.current) || VER;
  if (v) $("uver").textContent = " " + v;
  const last = CFG && CFG.lastCheckAt;
  $("usub").textContent =
    "上次检查:" + (last ? new Date(last * 1000).toLocaleString("zh-CN", { hour12: false }) : "从未");
}

function showCard(st) {
  $("updCard").style.display = "block";
  $("updVer").textContent = "发现新版本 " + st.version;
  $("updDate").textContent = st.date || "";
  $("updNotes").textContent = (st.notes || "").trim() || "(无更新说明)";
  $("updBar").style.display = "none";
  setMsg("");
  // 常驻挂件不弹窗:只在底栏挂一个入口,点进去才展开卡片
  const lk = $("lkUpd");
  lk.textContent = "有新版本 " + st.version;
  lk.style.display = "";
}

function hideCard() {
  $("updCard").style.display = "none";
  $("lkUpd").style.display = "none";
}

async function checkUpdate(force) {
  try {
    if (force) setMsg("正在检查…");
    const st = await invoke("check_update", { force });
    UPD.last = st;
    if (st.checkedNow) CFG.lastCheckAt = Math.floor(Date.now() / 1000);
    renderAbout(st);
    if (st.available) {
      UPD.info = st;
      // 下载/安装进行中,不被一次成功的检查把状态改写回 ready —— 那会让覆盖层的
      // 「立即更新」重新可点,再点就是第二次并发下载(报告 B5)。信息本体照常更新。
      if (!updBusy()) {
        UPDC.phase = "ready";
        renderUpdChip();
        showCard(st);
      }
    } else if (st.error) {
      // 这次检查**没成功**(没网 / 被限流 / 清单拉不下来):不要把已经知道的新版本撤掉。
      // 踩过:用户刚看到「有新版本」,一次自动检查失败就把提示清空,再点就没反应了,
      // 看起来像应用坏了 —— 检查失败只是"这次没问到",不代表"没有新版本"。
      if (force) setMsg("检查失败:" + st.error, true);
    } else if (st.checkedNow) {
      // 真的问到了、确实没有新版本(或被「跳过此版本」):这时才清掉提示。
      // 节流/关掉自动检查返回的 idle(checkedNow:false)什么都不代表 ——
      // 现在每 10 分钟就会问一次,若这里不设闸,提示会被"没去问"反复清掉。
      UPD.info = null;
      UPDC.phase = null;
      renderUpdChip();
      hideCard();
      // 自动检查一律静默:没更新、没网、被限流都不打扰。只有手动点才给反馈。
      if (force) setMsg("已是最新版本");
    }
  } catch (e) {
    if (force) setMsg("检查失败:" + e, true);
  }
}

function onUpdProgress(p) {
  // 标题旁的标签同步反映下载状态:卡片关掉也能看到进度
  const phase = (p && p.phase) || "";
  if (phase === "downloading" || phase === "started") {
    UPDC.phase = "downloading";
    UPDC.total = (p && p.total) || 0;
    UPDC.rec = (p && p.received) || 0;
    UPDC.pct = UPDC.total ? Math.min(100, Math.round((UPDC.rec / UPDC.total) * 100)) : 0;
    paintUpdBars();
  } else if (phase === "failed") {
    UPDC.phase = "failed";
    // 失败也要重画进度条:不然条子停在"下载 87%",而角标已经变成「重试」,
    // 两处互相矛盾(报告 P2-16)
    paintUpdBars();
    if (ovOpen()) overlayUpdState();
  } else if (phase === "installing") {
    // Windows 上 install 那一步会直接退出进程,不会再有后续事件
    UPDC.phase = "installing";
    setMsg("正在安装,完成后会自动重启");
    if (ovOpen()) overlayUpdState();
  } else if (phase === "finished") {
    UPDC.phase = null;
  }
  renderUpdChip();
}

$("btnCheck").addEventListener("click", () => checkUpdate(true));

for (const [id, platform, label] of [
  ["btnCopyWindowsDownload", "windows", "Windows 安装包"],
  ["btnCopyAndroidDownload", "android", "安卓 APK"],
]) {
  $(id).addEventListener("click", async () => {
    const btn = $(id);
    btn.disabled = true;
    btn.textContent = "正在获取…";
    try {
      const text = await invoke("download_link", { platform });
      await invoke("copy_text", { text });
      showToast(label + "下载直链已复制");
    } catch (e) {
      showToast("复制下载直链失败: " + e, true);
    } finally {
      btn.disabled = false;
      btn.textContent = "复制下载直链";
    }
  });
}

// ───────────── 排查日志(设置页 · 关于页签)─────────────
// 用户报"某渠道取不到数"时,这一段是唯一能把现场带回来的东西:
// 路径 + 大小 + 最近若干行,一键复制 / 打开目录 / 清空。
// 日志里不含密钥(写入侧 diaglog 已做摘要与脱敏),所以复制不需要二次确认。

let LOG_INFO = null;

/** 把字节数说成人话(界面上"342 KB"比 349913 好读)。 */
function fmtBytes(n) {
  if (!n) return "空";
  if (n < 1024) return n + " B";
  if (n < 1024 * 1024) return (n / 1024).toFixed(0) + " KB";
  return (n / 1024 / 1024).toFixed(1) + " MB";
}

/** 刷新日志卡(路径 / 大小 / 是否被环境变量关掉 / 详细日志开关 / 会话 id)。 */
async function refreshLogCard() {
  try {
    const info = await invoke("diag_log_info");
    LOG_INFO = info;
    const el = $("logPath");
    if (el) el.textContent = info.path || info.dir || "—";
    const sz = $("logSize");
    if (sz) sz.textContent = info.enabled ? fmtBytes(info.size) : "已关闭";
    const off = $("logOff");
    if (off) off.style.display = info.enabled ? "none" : "block";
    // 详细日志开关:以后端说的为准(它是唯一能改这个开关的地方)
    const vb = $("logVerbose");
    if (vb) vb.checked = !!info.verbose;
    // 会话 id:用户报问题时说出它,就能定位到那一次运行
    const sid = $("logSession");
    if (sid && info.session) {
      sid.textContent = "会话 " + info.session;
      sid.title = "报问题时把这个号一起说上,能精确定位到那一次运行";
    }
  } catch (e) {
    const el = $("logPath");
    if (el) el.textContent = "拿不到日志路径:" + e;
  }
}

if ($("btnLogCopy")) {
  $("btnLogCopy").addEventListener("click", async () => {
    const btn = $("btnLogCopy");
    btn.disabled = true;
    try {
      const text = await invoke("diag_log_tail", { lines: 600 });
      await invoke("copy_text", { text });
      showToast("最近日志已复制 · 可直接粘贴给 AI 排查");
    } catch (e) {
      showToast("复制日志失败:" + e, true);
    } finally {
      btn.disabled = false;
    }
  });
  $("btnLogOpen").addEventListener("click", async () => {
    try {
      await invoke("open_log_dir");
    } catch (e) {
      showToast("打开日志目录失败:" + e, true);
    }
  });
  $("btnLogClear").addEventListener("click", () => {
    // 清空不可逆,但代价很小(只是排查线索);行内确认说清「清掉就没了」(替代原生 confirm)
    armConfirm($("btnLogClear"), "清掉之后,下次出问题就没有这段记录了。", async () => {
      try {
        await invoke("diag_log_clear");
        await refreshLogCard();
        showToast("日志已清空");
      } catch (e) {
        showToast("清空失败:" + e, true);
      }
    }, "清空");
  });
}

/**
 * 导出诊断包:环境 + 体检 + 最近的事件,合成**一份**文本。
 *
 * 为什么不是让用户贴两样(体检报告 + 日志):合成一份之后用户只贴一样,
 * 也不用 support 再人工对一遍 —— 这是整个排查链路里最省事的一步。
 *
 * 体检要真打一轮请求(最坏十几秒),所以按钮期间禁用,避免连点叠请求。
 */
if ($("btnDiagExport")) {
  $("btnDiagExport").addEventListener("click", async () => {
    const btn = $("btnDiagExport");
    btn.disabled = true;
    const old = btn.textContent;
    btn.textContent = "导出中…";
    try {
      const text = await invoke("diag_export");
      await invoke("copy_text", { text });
      showToast("诊断包已复制 · 可直接粘贴给 AI 排查");
    } catch (e) {
      showToast("导出诊断包失败:" + e, true);
    } finally {
      btn.disabled = false;
      btn.textContent = old;
    }
  });
}

/**
 * 「详细日志」开关:连成功的请求也记下来。
 *
 * 默认关 —— 排查"余额数字不对"必须看成功响应,那是唯一能看到"接口到底返了什么"
 * 的地方;但平时用不到,而且开着日志量会大一截。
 */
if ($("logVerbose")) {
  $("logVerbose").addEventListener("change", async (e) => {
    const on = e.target.checked;
    try {
      await invoke("diag_log_verbose", { on });
      showToast(on ? "详细日志已打开 · 成功的请求也会记录" : "详细日志已关闭");
    } catch (err) {
      e.target.checked = !on; // 没改成就弹回去,别让界面显示着一个没生效的状态
      showToast("设置失败:" + err, true);
    }
  });
}

// 一键体检(裁决 2A):逐渠道真打一次 + 本机检查,产出可复制文本。
// 跑一次要打 7 个真请求(最坏十几秒),按钮期间禁用,避免连点叠请求。
$("btnDiag").addEventListener("click", async () => {
  const btn = $("btnDiag");
  btn.disabled = true;
  btn.textContent = "体检中…";
  $("diagOut").style.display = "block";
  $("diagOut").textContent = "正在逐渠道测试连接…";
  try {
    const report = await invoke("run_diagnostic");
    $("diagOut").textContent = report;
    $("btnDiagCopy").style.display = "inline-block";
    // 体检把失败链也写进了日志,顺手刷新一下大小/路径。
    // 结论(「结论:N 项需要处理 / 全部正常」)已完整展示在结果区,不再重复弹 toast
    refreshLogCard();
  } catch (e) {
    $("diagOut").textContent = "体检失败:" + e;
    showToast("体检失败:" + e, true);
  } finally {
    btn.disabled = false;
    btn.textContent = "运行体检";
  }
});
$("btnDiagCopy").addEventListener("click", async () => {
  try {
    await invoke("copy_text", { text: $("diagOut").textContent });
    showToast("报告已复制 · 可直接粘贴给 AI 排查");
  } catch (e) {
    showToast("复制失败:" + e, true);
  }
});
$("lkUpd").addEventListener("click", () => {
  openManage();
  // 更新卡片在「关于」页签里。之前只 openManage 不切页签:默认停在「渠道」页,
  // scrollIntoView 找不到隐藏的卡片,点角标看起来就是"没反应"(报告 P1-5)
  showTab("about");
  if ($("updCard")) $("updCard").scrollIntoView({ block: "center" });
});

/** 下载安装。覆盖层与管理页卡片共用这一个入口。 */
async function startInstall() {
  if (!UPD.info) return;
  // 防重入:下载/安装进行中再点会并发第二次 download_and_install,
  // 两路流写同一个临时文件,轻则进度乱、重则签名校验失败(bug 报告 #4)
  if (updBusy()) return;
  setMsg("");
  UPDC.phase = "downloading";
  UPDC.pct = 0;
  UPDC.rec = 0;
  UPDC.total = 0;
  renderUpdChip();
  if (ovOpen()) overlayUpdState(); // 按钮置灰、进度条出现
  try {
    await invoke("install_update");
  } catch (e) {
    setMsg("更新失败:" + e, true);
    // 后端只在 promise 里报错、没有 failed 事件:这里自己落 phase,
    // 角标才有机会从"下载 x%"变成"重试"(bug 报告 #5)
    UPDC.phase = "failed";
    renderUpdChip();
    if (ovOpen()) overlayUpdState();
  }
}

$("btnUpdGo").addEventListener("click", startInstall);
$("btnUpdSkip").addEventListener("click", async () => {
  if (!UPD.info) return;
  // 下载/安装中不跳过:状态清了后台却还在装,下一个进度事件又把角标"复活",
  // 跳过与安装互相打架(报告 B5;与覆盖层按钮 busy 置灰的行为对齐)
  if (updBusy()) return;
  const v = UPD.info.version;
  // 先等后端落盘再清界面。以前是"发出去就清":写失败时界面说"已跳过",
  // 重启后又弹同一个版本,而且没有任何提示 —— 用户只会认为"跳过按钮没用"。
  try {
    await invoke("skip_update_version", { version: v });
  } catch (e) {
    showToast("跳过该版本失败:" + e, true);
    return;
  }
  // 版本信息只有 UPD.info 一处,清它 + 清 phase,角标/卡片/覆盖层一起消失
  UPD.info = null;
  UPDC.phase = null;
  renderUpdChip();
  closeUpdOverlay();
  hideCard();
  setMsg("已跳过 " + v + ",下次发布新版本再提醒");
});
$("btnUpdLater").addEventListener("click", () => hideCard());
listen("update-progress", (ev) => onUpdProgress(ev.payload));

document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  // AI 优惠弹层由自己的监听管(报告 L6):它开着时主链不动 —— 否则一次 Esc 会
  // 同时关弹层 + 把面板收成胶囊/退出设置页,一次按键两个动作。
  if ($("promoOverlay") && !$("promoOverlay").hidden) return;
  // 详情弹层盖在最上层:Esc 只关它,同一次按键不能再把窗口收成胶囊。
  if (DETAIL_ID) {
    e.preventDefault();
    closeDetail();
    return;
  }
  // 覆盖层最优先:它盖在所有内容上,Esc 却先去关设置页/收形态,
  // 用户按半天 Esc 都关不掉眼前那个下载框(报告 P2-16)
  if (MDL.sheet) {
    // 模型弹层(「写入 · 确认」)盖在设置页之上,Esc 第一优先级该关的是它。
    // 以前这条链只认 #updOv,于是 Esc 关掉的是整个设置页,而那个确认弹层留在原地
    // 等下次打开设置页时凭空出现、按钮还能点(报告 P2-31)。
    MDL.sheet = null;
    renderModelSheet();
  } else if (ovOpen()) {
    closeUpdOverlay();
  } else if (manageOpen()) {
    if (!document.body.classList.contains("manage-index")) {
      showTab("index");
      const back = $("btnBack");
      if (back) back.focus();
    } else closeManage();
  } else if (
    document.body.className.includes("form-panel") &&
    document.querySelector("#list .row.open")
  ) {
    // 逐层退:先收起展开的渠道行,再考虑收形态(10-21 设计稿 §6.6「Esc 逐层退」)。
    // 只认**看得见**的展开行:临期筛选把它藏起来时,rowOpen 里还记着它,
    // 以前这一下 Esc 就被悄悄吃掉、界面毫无变化(2026-10-23 边界复核)。
    const focusInRow = document.activeElement && document.activeElement.closest(".row.open");
    document.querySelectorAll(".row.open").forEach((r) => {
      r.classList.remove("open");
      const h = r.querySelector(".rhead");
      if (h) h.setAttribute("aria-expanded", "false");
      delete rowMore[r.dataset.id];
    });
    rowOpen.clear();
    // 焦点在被收起的行里(比如停在「渠道设置」上):还给行头,别留在看不见的按钮上
    if (focusInRow) {
      const h = focusInRow.querySelector(".rhead");
      if (h) h.focus();
    }
  } else if (DOCK.on && DOCK.open) {
    dockLayout(false); // I6:吸附展开态,Esc 收回为竖条(不退出吸附)
  } else if (document.body.className.includes("form-panel")) {
    applyForm("pill");
  } else if (document.body.className.includes("form-compact")) {
    applyForm("panel");
  } else {
    // 胶囊 → 紧凑条;关了紧凑条就直接回面板(2026-10-23 裁决 D2)
    applyForm(CFG && CFG.compactEnabled ? "compact" : "panel");
  }
});

// 形态快捷键与行间移动(D11):同一个手势在四种形态里做四件不同的事,是
// "没有肌肉记忆"的根因。这里给出三条统一的:Ctrl+1/2/3 直接切形态,↑↓ 在面板里走行。
document.addEventListener("keydown", (e) => {
  if (manageOpen()) return;
  const t = e.target;
  const typing = t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable);
  if (e.ctrlKey && !e.altKey && !e.metaKey) {
    if (e.key === "1") { e.preventDefault(); applyForm("panel"); return; }
    if (e.key === "2") {
      e.preventDefault();
      // 紧凑条没开启时静默忽略(2026-10-23 裁决 D3):不为没开启的形态弹提示
      if (CFG && CFG.compactEnabled) applyForm("compact");
      return;
    }
    if (e.key === "3") { e.preventDefault(); applyForm("pill"); return; }
  }
  if (typing) return;
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
  if (!document.body.className.includes("form-panel")) return;
  const heads = [...document.querySelectorAll("#list .rhead")];
  if (!heads.length) return;
  e.preventDefault();
  const i = heads.indexOf(document.activeElement);
  if (i < 0) { heads[0].focus(); return; }
  heads[Math.max(0, Math.min(heads.length - 1, i + (e.key === "ArrowDown" ? 1 : -1)))].focus();
});


