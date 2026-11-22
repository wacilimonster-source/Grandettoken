/* TokenScope 前端。经 window.__TAURI__ 调 Rust 命令(withGlobalTauri)。 */
const T = window.__TAURI__;
const invoke = T.core.invoke;
const listen = T.event.listen;
const appWindow = T.window.getCurrentWindow();

const $ = (id) => document.getElementById(id);

/** 时间驱动文案只在真的变了才写 DOM:轮询/分钟 ticker 每跑一次都给 span 重写
 *  一遍 textContent,会让「相同数据刷新」凭空产生 DOM 变更(也打断节点复用)。 */
function setTextIfChanged(el, text) {
  const s = text === null || text === undefined ? "" : String(text);
  if (el && el.textContent !== s) el.textContent = s;
}

let CHANNELS = [];
let WINDOW_VISIBLE = true;
let CFG = null;
let VER = "";          // app_version 命令回来的版本号,关于页在首检之前也有的显示
let SYNC_STATUS = null; // 同步状态缓存(sync_status 命令),设置索引与同步子页共用
let SYNC_STATUS_ERROR = null;
let SYNC_STATUS_REV = 0;
let SYNC_RECOVERY_BUSY = false;
let SYNC_CODE_REVEALED = false;
// 二维码和同步码默认直接显示（用户确认）；「隐藏同步码」只做临时遮挡。
// 缓存按同步码文本区分；隐藏时仍立刻从 DOM 移除，避免截图或辅助工具读到旧码。
let SYNC_QR_CODE = "";
let SYNC_QR_SVG = "";
let SYNC_QR_BUSY = "";
let SYNC_QR_FAILED = "";
let SYNC_AUTH = null;   // 桌面 Device Flow 在途会话(只存 user_code 与计时,不存 device_code)
let changesCache = {}; // { [id]: 最近变动台账(展开时按需取,不进轮询) }
let changesState = {}; // { [id]: "loading" | "done" | "error" } —— 台账先画占位,取到再替换
// 展开区里点了「全部 N 条」的段:{ [id]: { ledger?: true, expiry?: true } }。
// 跟 rowOpen 一样是**状态**不是 DOM:轮询重画后还在,收起整行时清掉。
const rowMore = {};
// 面板里展开的行 id(DOM 上的 .open 只是它的投影):行头的 aria-expanded 与轮询重建后的
// 恢复都读它。以前 renderRow 里写的是未声明的 `open`,实际解析成 window.open(恒真),
// 于是每行都发 aria-expanded="true",开合也从不更新(F2)。
const rowOpen = new Set();
// WorkBuddy 网页登录窗口已打开、正在等令牌:按钮期间置真,防止连点开出第二个窗口
// (Rust 侧也会把已开的那个拉到前台,但界面要给出"进行中"的样子)
let WB_LOGGING = false;
// 豆包网页登录窗口:同一位纪律(各渠道各自一个标位,互不干扰)
let DB_LOGGING = false;

// 字号档位:--u 倍率写到 :root,styles.css 全部尺寸都是 calc(Npx*var(--u));
// 窗口尺寸走同一个倍率(见 formSize),否则大字会被固定高度的窗口裁切
const FS_U = { md: 1, lg: 1.15, xl: 1.3 };
const fsU = () => (CFG && FS_U[CFG.fontScale]) || 1;
function applyFontScale() {
  document.documentElement.style.setProperty("--u", String(fsU()));
}

/** 主题:深石墨(默认)/ 暖雾白。变量与组件样式在 styles.css 末尾的 V2 段。 */
function applyTheme() {
  const light = !!(CFG && CFG.theme === "light");
  document.body.classList.toggle("light", light);
  const grp = $("cfgTheme");
  if (grp) {
    grp.querySelectorAll("button[data-theme]").forEach((b) => {
      const on = (b.dataset.theme === "light") === light;
      b.setAttribute("aria-pressed", String(on));
    });
  }
}

/** 紧凑条开关:body 类记录开关状态(供测试与后续样式使用),并同步设置页开关。
    顶栏固定四枚按钮,不再随这个开关换「紧凑/胶囊」(2026-11-03)。 */
function applyCompactUI() {
  const on = !!(CFG && CFG.compactEnabled);
  document.body.classList.toggle("compact-on", on);
  setSwitch("cfgCompact", on);
}

// ───────────── 格式化 ─────────────
/** HTML 转义:接口返回的错误文案、渠道名等会直接进 innerHTML。
    CSP 已经挡掉脚本执行,但把数据当 HTML 拼本身就该避免。 */
const esc = (v) =>
  String(v ?? "").replace(/[&<>"']/g, (ch) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[ch])
  );
/** 币种前缀:CNY 是 ¥,其余(DeepSeek 的余额会带自己的币种代码)直接用代码本身 ——
    只做**标注**,不做任何换算。 */
const curSign = (unit) => (!unit || unit === "CNY" ? "¥" : unit + " ");
/** 金额的唯一口径:千分位 + 两位小数,币种按渠道自己的 unit(F11/F12)。
    null/undefined 一律「——」(0 与"没取到数"不能混);Number() 兜住字符串,
    输出永远是纯数字文本,进 innerHTML 也安全。 */
const money = (v, unit) =>
  v === null || v === undefined
    ? "——"
    : curSign(unit) +
      Number(v).toLocaleString("zh-CN", { minimumFractionDigits: 2, maximumFractionDigits: 2 });

/** 积分:带千分位;小数按需显示(3855 → 3,855;6075.5 → 6,075.5)。 */
const points = (v) =>
  v === null || v === undefined
    ? "——"
    : v.toLocaleString("zh-CN", { maximumFractionDigits: 2 });

/** 到期相对时间。今天/明天按**本地自然日**判断,不用"不足 24 小时"顶替今天;
 *  跨过到期时刻一律写「已到期,待刷新」,不出现负数倒计时。 */
function expiryRelative(ts) {
  if (!Number.isFinite(ts) || ts <= 0) return "时间未知";
  const diff = ts * 1000 - Date.now();
  if (diff <= 0) return "已到期,待刷新";
  const mins = Math.floor(diff / 60000);
  if (mins < 60) return `${Math.max(1, mins)} 分钟后到期`;
  const d = new Date(ts * 1000);
  const now = new Date();
  const day0 = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const target = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const days = Math.round((target - day0) / 86400000);
  if (days === 0) return "今天到期";
  if (days === 1) return "明天到期";
  return `${days} 天后到期`;
}

/** 可被 60 秒 ticker 原地更新的相对时间(不重建整页)。 */
function expiryRelativeSpan(ts) {
  return `<span data-exp-at="${Number(ts)}">${expiryRelative(Number(ts))}</span>`;
}

/** 到期日期/时间拆分(绝对日期 + 本地时钟)。 */
function expiryParts(ts) {
  const d = new Date(ts * 1000);
  const p = (n) => String(n).padStart(2, "0");
  return {
    date: `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`,
    time: `${p(d.getHours())}:${p(d.getMinutes())}`,
  };
}

function relTime(ts) {
  if (!ts) return "从未";
  const s = Math.floor(Date.now() / 1000) - ts;
  if (s < 60) return "刚刚";
  if (s < 3600) return Math.floor(s / 60) + " 分钟前";
  if (s < 86400) return Math.floor(s / 3600) + " 小时前";
  return Math.floor(s / 86400) + " 天前";
}

/**
 * 配额型渠道的主窗口:后端用 `main` 标出来(OpenCode = 「周期」窗,Codex = 周窗优先),
 * 大数字与状态色都按它走。老数据没有这个标志时退回「周期」标签,再退到数组首位。
 */
function isAntigravity(c) {
  return c.id === "antigravity" || /^antigravity:[a-f0-9]{24}$/.test(c.id);
}

function mainWindow(c) {
  const wins = c.windows || [];
  if (!wins.length) return null;
  if (c.id === "codex") {
    const weekly = wins.find((w) => w.label === "本周");
    if (weekly) return weekly;
  }
  if (isAntigravity(c)) {
    return wins.find((w) => w.main) || wins.find((w) => w.key === "gemini-weekly") || wins.find((w) => w.key === "gemini-5h") || wins[0];
  }
  return wins.find((w) => w.main) || wins.find((w) => w.label === "周期") || wins[0];
}

/** ISO 8601(UTC)的 resetsAt → 毫秒;缺失/异常一律 null,绝不猜。 */
function resetMs(w) {
  if (!w || !w.resetsAt) return null;
  const t = Date.parse(w.resetsAt);
  return Number.isNaN(t) ? null : t;
}

/** 台钟 HH:MM(缺口文案、最近变动用)。入参是**秒** —— 与后端 now_ts() / ChangeEntry 同口径。
    以前按毫秒解释(ts * 1000 漏了),7 天的台账被压成 01:58,见
    docs/design/design-row-detail-2026-10-23.html §6.1。 */
function fmtClock(ts) {
  const d = new Date(ts * 1000);
  return String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
}

/** 折叠行右侧的快照年龄:「12 分钟前快照」;不到一分钟写「刚刚的快照」(「刚刚快照」不通) */
function snapAge(ts) {
  const r = relTime(ts);
  return r === "刚刚" ? "刚刚的快照" : r + "快照";
}

/** 同一本地日?(跨零点的聚合区间要分开写日期) */
function sameLocalDay(a, b) {
  const x = new Date(a * 1000);
  const y = new Date(b * 1000);
  return (
    x.getFullYear() === y.getFullYear() &&
    x.getMonth() === y.getMonth() &&
    x.getDate() === y.getDate()
  );
}

/** 台账时间戳:今天只写 HH:MM,更早补 MM-DD。 */
function fmtStamp(ts) {
  if (sameLocalDay(ts, Date.now() / 1000)) return fmtClock(ts);
  const d = new Date(ts * 1000);
  return `${d.getMonth() + 1}-${String(d.getDate()).padStart(2, "0")} ${fmtClock(ts)}`;
}

/**
 * 峰谷状态(只有 DeepSeek 这类按峰谷计价的渠道带 c.peak;见 core/peak.rs)。
 * 界面只报"现在贵不贵 + 还有多久变",**不显示任何单价数字** —— 规则两个月被
 * 官方改过三次,数字会过期,时段判据不会。
 */
function peakParts(c) {
  const p = c && c.peak;
  if (!p) return null;
  if (p.holiday) return { label: "法定节假日 · 全天谷价" };
  if (p.weekend) return { label: "周末 · 全天谷价" };
  return { label: p.peak ? "高峰价" : "谷价中" };
}

/** 距下一次切换的倒计时文案(按**本地时钟**每秒算,不依赖轮询频率)。 */
function peakCountdown(p, nowMs) {
  if (!p || !p.nextChangeAt) return "";
  const ms = p.nextChangeAt * 1000 - nowMs;
  if (ms <= 0) return "";
  const h = Math.floor(ms / 3600e3);
  const m = Math.floor((ms % 3600e3) / 60000);
  const left = h ? h + " 小时 " + m + " 分" : m + " 分";
  if (p.peak) return left + "后转谷价";
  if (p.weekend || p.holiday) return "距下次高峰 " + left;
  return left + "后转高峰";
}

/**
 * 峰谷倒计时:只改文案,不重绘整行。
 *
 * 显示精度是**分钟**,所以对齐到分钟边界跑一次即可 —— 以前 1 秒一次,每小时
 * 3600 次回调、每次都扫全文档。定时器误差可能落在边界前几毫秒,统一 +20ms 起跳。
 */
function tickPeakCountdowns() {
  if (!WINDOW_VISIBLE) return;
  if (window.PanelLayoutUI) window.PanelLayoutUI.tick();
  if (window.AntigravityUI) window.AntigravityUI.tick();
  const nowMs = Date.now();
  document.querySelectorAll("[data-peakcd]").forEach((el) => {
    let p = null;
    try {
      p = JSON.parse(el.dataset.peakcd);
    } catch (e) {
      return;
    }
    setTextIfChanged(el, peakCountdown(p, nowMs));
  });
  document.querySelectorAll("[data-reset-at]").forEach((el) => {
    setTextIfChanged(el, quotaResetCountdown(Number(el.dataset.resetAt)));
  });
  // Codex 数值标签里的极简倒计时:按同一份降级档位重算,别把 fit 收窄的那一档冲掉
  document.querySelectorAll("[data-reset-tight]").forEach((el) => {
    const ms = Number(el.dataset.resetTight);
    const mode = el.dataset.tightMode || "full";
    setTextIfChanged(el,
      mode === "off" ? "" : "·" + (mode === "short" ? fmtResetTightShort(ms, nowMs) : fmtResetTight(ms, nowMs)));
  });
  // 到期相对时间:每分钟原地更新,跨过到期时刻立刻变成「已到期,待刷新」
  let expCrossed = false;
  document.querySelectorAll("[data-exp-at]").forEach((el) => {
    const at = Number(el.dataset.expAt);
    if (!Number.isFinite(at)) return;
    const before = el.textContent;
    const after = expiryRelative(at);
    if (before && before !== after && before !== "已到期,待刷新" && after === "已到期,待刷新") {
      expCrossed = true;
    }
    setTextIfChanged(el, after);
  });
  if (expCrossed) render({});
  updateHeaderLive();
}

let peakTickTimer = null;
function schedulePeakTicks() {
  clearTimeout(peakTickTimer);
  const delay = 60_000 - (Date.now() % 60_000) + 20;
  peakTickTimer = setTimeout(() => {
    if (WINDOW_VISIBLE && window.QuotaPlanUI?.needsTick(Date.now())) render({});
    else tickPeakCountdowns();
    schedulePeakTicks();
  }, delay);
}
schedulePeakTicks();

/**
 * 提示音两声(裁决 8A:签到结果 + 跌破红线,默认关)。
 * 用 WebAudio 现场合成,不新增任何音频资源文件(单 exe 约束下少一个二进制就少一份事);
 * 用户没开 soundOn 时一个采样都不产生。
 */
let AUDIO_CTX = null;
function beep(kind) {
  if (!CFG || !CFG.soundOn) return;
  try {
    AUDIO_CTX = AUDIO_CTX || new (window.AudioContext || window.webkitAudioContext)();
    const g = AUDIO_CTX.createGain();
    g.connect(AUDIO_CTX.destination);
    g.gain.value = 0.06;
    const seq = kind === "warn" ? [[880, 0], [587, 0.18]] : [[880, 0], [1319, 0.1]];
    const t0 = AUDIO_CTX.currentTime;
    seq.forEach(([f, off]) => {
      const o = AUDIO_CTX.createOscillator();
      o.type = "sine";
      o.frequency.value = f;
      o.connect(g);
      o.start(t0 + off);
      o.stop(t0 + off + 0.14);
    });
  } catch (e) {
    // 声音是锦上添花:任何环境问题都不该影响账本展示
    console.warn("提示音播放失败", e);
  }
}

/** 倒计时档位化:<1 小时用分钟,<24 小时用小时,更长按「天+小时」;short 用于窄列。 */
function fmtReset(ms, short) {
  if (ms === null) return short ? "—" : "重置时间未知";
  const left = ms - Date.now();
  if (left <= 0) return "正在重置";
  // 最少报 1 分:不到 30 秒的余量取整成 0,界面就成了"0 分后重置"却还在跳(报告 P2-15)
  const m = Math.max(1, Math.round(left / 60000));
  if (m < 60) return short ? m + " 分" : m + " 分钟后重置";
  const h = left / 3600e3;
  if (h < 24) return short ? h.toFixed(1) + " 小时" : h.toFixed(1) + " 小时后重置";
  const d = Math.floor(h / 24), rh = Math.round(h % 24);
  if (rh === 24) return short ? d + 1 + " 天" : d + 1 + " 天后重置";
  // 正好整天不写「4 天 0 小时」
  if (rh === 0) return short ? d + " 天" : d + " 天后重置";
  return short ? d + " 天 " + rh + " 小时" : d + " 天 " + rh + " 小时后重置";
}

function quotaResetCountdown(ms) {
  return ms <= Date.now() ? "已到重置时间,待刷新" : fmtReset(ms, false);
}

/**
 * 极简倒计时(数值行标签用):「4天7小时」「5.2小时」「37分钟」「待刷新」。
 *
 * 为什么不直接用 `fmtReset(ms, true)`:那个带空格(「4 天 7 小时」),在数值行里
 * 实测多占 51px,面板拉到最窄(340)时会把左侧副标题挤成省略号。
 * 口径与 `fmtReset` 一致,只是去掉空格和「后重置」这类可以省的字。
 */
function fmtResetTight(ms, nowMs) {
  const left = ms - (nowMs || Date.now());
  if (left <= 0) return "待刷新";
  const m = Math.max(1, Math.round(left / 60000));
  // 「1 分钟」不用「不足」—— 那个词留给"确实不到一分钟"的情况
  if (m < 60) return m === 1 && left < 60e3 ? "不足1分钟" : m + "分钟";
  const h = left / 3600e3;
  if (h < 24) return h.toFixed(1) + "小时";
  const d = Math.floor(h / 24);
  const rh = Math.round(h % 24);
  if (rh === 24) return d + 1 + "天";
  if (rh === 0) return d + "天";
  return d + "天" + rh + "小时";
}

/** 再短一档(只有天数/小时):窄面板放不下「4天7小时」时先退到这里。 */
function fmtResetTightShort(ms, nowMs) {
  const left = ms - (nowMs || Date.now());
  if (left <= 0) return "待刷新";
  const h = left / 3600e3;
  if (h >= 24) return Math.floor(h / 24) + "天";
  if (h >= 1) return h.toFixed(0) + "小时";
  return Math.max(1, Math.round(left / 60000)) + "分钟";
}

/**
 * 数值标签里那一段重置倒计时(Codex 专用):`·4天7小时`。
 *
 * 没有 `resetsAt` → 空串(不写「—」占位,那一行已经够挤)。
 * 挂 `data-reset-tight` 给每分钟的对齐 ticker 更新;`data-tight-mode` 由
 * [`fitCodexResetLabels`] 在放不下时降级(full → short → off)。
 */
/**
 * 面板被拉窄时,先缩短**倒计时**,别让它把左侧副标题(套餐 · 重置卡)挤成省略号。
 *
 * 为什么渲染后再量:数值列的宽度是内容决定的(`flex:none`),标签一长,左侧就窄;
 * 只有真实布局才知道差多少。降级顺序 full → short → off,退到底就是改动前的样子,
 * 三种形态都不会溢出,也不需要给每一行写死宽度。
 */
function fitCodexResetLabels() {
  document.querySelectorAll(".row.codex-quota [data-reset-tight]").forEach((el) => {
    const ms = Number(el.dataset.resetTight);
    // 紧凑条里这段是 `display:none`,量出来全是 0 —— 不能据此降级:否则切回面板时
    // 倒计时会带着"已经收掉"的状态凭空消失(2026-11-03 真机截图抓到一次)。
    if (!el.getClientRects().length) {
      if (el.dataset.tightMode !== "full") el.dataset.tightMode = "full";
      setTextIfChanged(el, "·" + fmtResetTight(ms, Date.now()));
      return;
    }
    const row = el.closest(".row");
    const s = row && row.querySelector(".nm .s");
    if (!s) return;
    const fits = () => s.scrollWidth <= s.clientWidth + 1;
    if (fits()) return;
    if (el.dataset.tightMode !== "short") el.dataset.tightMode = "short";
    setTextIfChanged(el, "·" + fmtResetTightShort(ms, Date.now()));
    if (fits()) return;
    if (el.dataset.tightMode !== "off") el.dataset.tightMode = "off";
    setTextIfChanged(el, "");
  });
}

/** 完整本地时间与时区,避免把卡片到期日和配额重置日混在一起。 */
function exactLocalTime(ts) {
  const d = new Date(ts * 1000);
  if (!Number.isFinite(d.getTime())) return "时间未知";
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function localZoneLabel() {
  const mins = -new Date().getTimezoneOffset();
  return `本地时间 UTC${mins >= 0 ? "+" : "-"}${String(Math.floor(Math.abs(mins) / 60)).padStart(2, "0")}:${String(Math.abs(mins) % 60).padStart(2, "0")}`;
}

/**
 * 主展示比例 —— 状态色、排序、胶囊都按它算。
 * 配额型:周期窗剩余;金额型:剩余/分母(申请制用单次额度,其他用接口给的
 * 总额 —— 累计发放只增不减,拿它当分母会越算越低,没有决策价值)。
 * 充值型无 total 时返回 null,不画进度条也不参与百分比排序。
 */
function remainRatio(c) {
  if (c.kind === "percent") {
    const w = mainWindow(c);
    if (!w) return null;
    return Math.max(0, Math.min(100, w.remainPercent)) / 100;
  }
  // 分母只认「本轮额度」(claim.amount)。后端的 total 是**累计发放**,只增不减 ——
  // 拿它当分母会越算越低,ClaimConfig 的注释明令禁止(报告 P1-11:关掉申请制后
  // 4SAPI 行会显示「额度 ¥4800 · 剩 21%」并因此染红、被胶囊挑成最紧渠道)。
  const denom = c.claim && c.claim.amount > 0 ? c.claim.amount : null;
  if (c.remaining === null || denom === null || denom === undefined || !denom) return null;
  // 余额可能超出本轮额度(改小过 amount、平台多充),钳到 [0,1] 再给进度条用
  return Math.max(0, Math.min(1, c.remaining / denom));
}

/** 申请周期状态:可申请 / 还要等几天 / 未记录。 */
function claimTail(cl) {
  if (cl.daysUntilEligible === null || cl.daysUntilEligible === undefined) {
    return ' · <span class="ct">未记录申请时间</span>';
  }
  if (cl.eligible) return ' · <span class="cy">现在可申请</span>';
  return ` · 再等 ${cl.daysUntilEligible} 天可申请`;
}

/** 本地日期(yyyy-mm-dd),给 date input 用。 */
function toDateInput(ts) {
  if (!ts) return "";
  const d = new Date(ts * 1000);
  const p = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

function fmtDay(ts) {
  if (!ts) return "——";
  const d = new Date(ts * 1000);
  return `${d.getMonth() + 1}-${String(d.getDate()).padStart(2, "0")}`;
}

/** 从今天起 days 个本地自然日内的逐笔积分合计。 */
function expiringWithin(c, days) {
  return window.ExpiryUI.entriesWithin(c, days).reduce((sum, e) => sum + Number(e.amount), 0);
}

/** 顶部摘要的临期窗口(天)。只影响首页摘要 / 临期筛选;
 *  详情仍列全部记录,风控 / 通知 / 30 日历史查询不跟着改窗口。 */
const SUMMARY_EXPIRY_DAYS = 7;

function expirySummary() {
  const now = Date.now();
  return window.ExpiryUI.summary(CHANNELS, SUMMARY_EXPIRY_DAYS, now, window.QuotaPlanUI?.events(CHANNELS, SUMMARY_EXPIRY_DAYS, now));
}

let EXPIRY_FILTER = false;
function hasConfirmedExpiry(c) {
  return nearestExpiryAt(c) !== null;
}
function nearestExpiryAt(c) {
  return window.ExpiryUI.nearest(c, SUMMARY_EXPIRY_DAYS) ??
    window.QuotaPlanUI?.events(CHANNELS, SUMMARY_EXPIRY_DAYS, Date.now()).find(item => item.channel.id === c.id)?.entry.at ?? null;
}

function toneOf(c) {
  if (!c.hasKey || !c.valid) return "off";
  // 积分型的风险是"过期",不是"余额低"。看**近 7 天**到期的量占剩余的比例:
  // 用 30 天会误报 —— Trae 的签到积分本来就是 31 天有效,几乎所有积分都落在
  // "30 天内到期",整行会常红;7 天窗口才是"该动手花掉"的信号。
  // 同时用占比而不是绝对值:过期 5 分不该把整行染红。
  if (c.kind === "points") {
    const r = c.remaining ?? 0;
    if (r <= 0) return "ok";
    const share = expiringWithin(c, 7) / r;
    if (share >= 0.3) return "bad";
    if (share >= 0.1) return "warn";
    return "ok";
  }
  const r = remainRatio(c);
  if (r === null) return "ok";
  if (r < CFG.critPercent / 100) return "bad";
  if (r < CFG.warnPercent / 100) return "warn";
  return "ok";
}

const TONE_COLOR = { ok: "var(--ok)", warn: "var(--warn)", bad: "var(--bad)", off: "var(--tx3)" };
/** 大数字默认回到主文字色:颜色只留给 warn / bad / off。
 *  healthy 行的小绿点、进度条、状态标签已经足够表达"正常"。 */
const toneFg = (t) => (t === "ok" ? "var(--tx)" : TONE_COLOR[t] || "var(--tx)");
const toneFgSoft = (t) => (t === "ok" ? "var(--tx2)" : TONE_COLOR[t] || "var(--tx2)");
// 与 styles.css 的高对比色板保持一致(进度条/托盘角标是内联色,吃不到 CSS 变量)
const TONE_HEX = { ok: "#8bc9a2", warn: "#edc185", bad: "#ef9d94", off: "#3a4356" };

/** 单个配额的数字只按自己的余量着色;行状态仍可由另一窗口触发限流。 */
function quotaTone(c, w) {
  if (!c.hasKey || !c.valid || !w || !Number.isFinite(w.remainPercent)) return "off";
  if (w.status === "rate-limited" || w.remainPercent < CFG.critPercent) return "bad";
  return w.remainPercent < CFG.warnPercent ? "warn" : "ok";
}

/**
 * 状态点分层(U2):未配置 = off(空心灰环,没启用不是故障);
 * 取数失败/失联 = st(灰点加斜杠);其余按阈值红黄绿。
 * 列表行 / 胶囊 / 渠道卡共用,保证同一个渠道到处一个样。
 */
function dotCls(c) {
  if (!c.hasKey) return "off";
  if (isAntigravity(c) && c.stale) return "st";
  if (!c.valid) return "st";
  // 阈值判断与行 / 胶囊 / 托盘同源:以后端那一份为准(没有视图时退回本地 toneOf)
  if (c.limited || hvTone(c) === "bad") return "b";
  if (hvTone(c) === "warn") return "w";
  return "";
}

// 官方图标(素材出处见 app/src/logos/README.md)。没有官方标的渠道继续用字母方块。
const LOGOS = {
  "opencode-go": "logos/opencode.svg",
  deepseek: "logos/deepseek.png",
  trae: "logos/trae.svg",
  workbuddy: "logos/workbuddy.png",
  codex: "logos/chatgpt.svg",
  qoder: "logos/qoder.png",
  // 国内版与国际版是**两套账号**,但视觉上是同一个产品 —— 共用同一份官方标,
  // 靠渠道名("Qoder 国内版")与主色区分,不再另画一个图标。
  "qoder-cn": "logos/qoder.png",
  // WorkBuddy AI(国际版)同理:同一个产品、另一套账号,共用 WorkBuddy 的标。
  "workbuddy-ai": "logos/workbuddy.png",
  minimax: "logos/minimax.png",
  doubao: "logos/doubao.png",
  antigravity: "logos/antigravity.png",
};

/**
 * 渠道图标。统一入口:列表行 / 密钥行 / 额度申请行都用它。
 * off = 未配置或取数失败 —— 字母方块变灰,官方标压暗去色,语义保持一致。
 * 官方标同时内嵌一个字母方块副本(.ltr):紧凑条 16px 档位位图会糊,
 * 由 CSS 切换到字母显示(U6),面板/管理页仍显示官方标。
 */
/** 兜底字母:2 位大写;短名不足两位时取名称前两个字。
 *  绝不能落成小写 x / × —— 那和"关闭"同形,用户会以为能点。 */
function shortLabel(c) {
  const raw = String((c && c.short) || "").trim();
  const name = String((c && c.name) || "").trim();
  let out = raw.length >= 2 ? raw.slice(0, 2) : (name || raw || "?").replace(/\s+/g, "").slice(0, 2);
  out = out.toUpperCase();
  if (!out || out === "X" || out === "×") out = "··";
  return out;
}

function iconHtml(c, cls = "ico", off = false) {
  const src = LOGOS[isAntigravity(c) ? "antigravity" : c.id];
  const label = shortLabel(c);
  if (!src) {
    return `<div class="${cls}" title="${esc(c.name)}" style="background:${off ? "#343d4f" : c.color}">${esc(label)}</div>`;
  }
  return `<div class="${cls} logo${off ? " off" : ""}" style="--cc:${off ? "#343d4f" : c.color}"><img src="${src}" alt="${esc(label)}"><span class="ltr">${esc(label)}</span></div>`;
}

/** 自定义排序下的完整顺序:配置里列出的 + 未列入的(附在末尾,新增渠道不会丢)。 */
function currentOrderIds() {
  const order = (CFG && CFG.channelOrder) || [];
  return [
    ...order.filter((id) => CHANNELS.some((c) => c.id === id)),
    ...CHANNELS.filter((c) => !order.includes(c.id)).map((c) => c.id),
  ];
}

/** 上一轮真实画出来的渠道顺序 —— 排序键相同(平局)时用它兜底,避免同一份数据
 *  每轮刷新都在等分的渠道之间来回换位。真实交叉时排序键不同,不受它影响。 */
let LAST_RENDER_ORDER = [];
/** 平局比较器:上一轮在前的仍在前;新出现的渠道排在平局组末尾。 */
function stableTie(prevOrder) {
  const rank = new Map((prevOrder || []).map((id, i) => [id, i]));
  return (a, b) =>
    (rank.has(a.id) ? rank.get(a.id) : Infinity) - (rank.has(b.id) ? rank.get(b.id) : Infinity);
}

/** 主排序:剩余百分比升序(默认)/ 余额 / 今日消耗 / 自定义。 */
function sortChannels(list, prevOrder) {
  const mode = CFG.sort;
  const tie = stableTie(prevOrder || LAST_RENDER_ORDER);
  if (mode === "custom") {
    // 自定义顺序就是用户排的顺序,不做任何干预(失败/未配置也按排的位置显示)
    const order = currentOrderIds();
    const rank = (c) => {
      const i = order.indexOf(c.id);
      return i < 0 ? 9999 : i;
    };
    return [...list].sort((a, b) => rank(a) - rank(b) || tie(a, b));
  }
  const score = (c) => {
    if (!c.hasKey) return 9999;
    if (!c.valid && !c.stale) return 5000;
    // 积分和金额/百分比不是一回事,不能混进同一条数轴比大小:
    // 统一排在"可比渠道之后、取数失败之前"
    if (c.kind === "points") return 4000;
    if (mode === "balance") return -(c.remaining ?? 0);
    if (mode === "dayUsage") return -(c.day ?? 0);
    const r = remainRatio(c);
    return r === null ? 4000 : r * 100;
  };
  return [...list].sort((a, b) => score(a) - score(b) || tie(a, b));
}

// ───────────── 统一视图(core::view::HomeView)─────────────
//
// 四种形态以前各自解释一次 `valid / limited / updatedAt`,同一个渠道能给出三种结论
// (设计稿 D01/D02/D03)。判定现在收在后端 `view.rs` 里,前端只保留一份结果:
// 排序、色调、主数字、陈旧标记都从它读。取不到(旧版后端 / IPC 失败)时,
// 下面的每个读取口都会退回原来的本地判断,行为不回退。
let HV = { byId: {}, order: [], checkin: null, actionCount: 0, staleCount: 0, errorCount: 0 };

/** 收下一份后端视图。失败时保留上一份 —— 界面退回旧判断,而不是变空白。 */
function hvApply(v) {
  const rows = (v && v.rows) || [];
  const byId = {};
  for (const r of rows) byId[r.id] = r;
  HV = {
    byId,
    order: (v && v.order) || [],
    checkin: (v && v.checkin) || null,
    actionCount: (v && v.actionCount) || 0,
    staleCount: (v && v.staleCount) || 0,
    errorCount: (v && v.errorCount) || 0,
  };
}
const hvRow = (id) => HV.byId[id] || null;

const HV_TONE = { ok: "ok", warn: "warn", bad: "bad", off: "off" };

/** 色调:后端给了就照它画 —— 面板 / 紧凑条 / 胶囊 / 托盘同一个结论。 */
function hvTone(c) {
  const r = hvRow(c.id);
  return r ? HV_TONE[r.tone] || toneOf(c) : toneOf(c);
}

