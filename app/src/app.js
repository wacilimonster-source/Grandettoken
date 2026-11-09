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

/** 距今天还有几天(向上取整:不足一天也算 1 天,不说"0 天后过期")。 */
function daysUntil(ts) {
  if (!ts) return null;
  return Math.ceil((ts * 1000 - Date.now()) / 86400000);
}

/** 到期紧迫度:7 天内红、30 天内黄,其余不强调。 */
function expClass(days, warnDays = 30, critDays = 7) {
  if (days === null) return "";
  if (days <= critDays) return "b";
  if (days <= warnDays) return "w";
  return "";
}

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
function mainWindow(c) {
  const wins = c.windows || [];
  if (!wins.length) return null;
  if (c.id === "codex") {
    const weekly = wins.find((w) => w.label === "本周");
    if (weekly) return weekly;
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
    tickPeakCountdowns();
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

/** 未来 days 天内会过期的积分合计。 */
function expiringWithin(c, days) {
  const now = Date.now() / 1000;
  const limit = now + days * 86400;
  return (c.expiring || [])
    .filter((e) => e.at >= now && e.at <= limit)
    .reduce((a, e) => a + e.amount, 0);
}

/** 积分到期摘要中的有效记录。未知的逐笔到期信息不伪装成“没有临期”。 */
function expiringEntriesWithin(c, days) {
  const now = Date.now() / 1000;
  const limit = now + days * 86400;
  return (c && c.expiring ? c.expiring : []).filter(
    (e) =>
      e &&
      Number.isFinite(Number(e.at)) &&
      Number.isFinite(Number(e.amount)) &&
      Number(e.amount) > 0 &&
      e.at >= now &&
      e.at <= limit
  );
}

/** 顶部摘要的临期窗口(天)。只影响首页摘要 / 临期筛选;
 *  详情仍列全部记录,风控 / 通知 / 30 日历史查询不跟着改窗口。 */
const SUMMARY_EXPIRY_DAYS = 7;

/** 顶部摘要只统计当前可见、取数有效的积分渠道；逐笔明细未知的渠道单独计数。 */
function expirySummary() {
  const channels = CHANNELS.filter(
    (c) => c.hasKey && !c.hidden && c.valid && c.kind === "points"
  );
  const unknown = channels.filter(
    (c) => c.expiryComplete !== true && Number(c.remaining || 0) > 0
  );
  const entries = channels.flatMap((c) =>
    expiringEntriesWithin(c, SUMMARY_EXPIRY_DAYS).map((entry) => ({ channel: c, entry }))
  );
  entries.sort((a, b) => a.entry.at - b.entry.at);
  const total = entries.reduce((sum, x) => sum + Number(x.entry.amount), 0);
  return {
    total: Math.round(total * 100) / 100,
    channels: new Set(entries.map((x) => x.channel.id)).size,
    entries,
    first: entries[0] || null,
    unknown,
  };
}

let EXPIRY_FILTER = false;
function hasConfirmedExpiry(c) {
  return (
    c && c.hasKey && !c.hidden && c.valid && c.kind === "points" &&
    expiringWithin(c, SUMMARY_EXPIRY_DAYS) > 0
  );
}
function nearestExpiryAt(c) {
  return expiringEntriesWithin(c, SUMMARY_EXPIRY_DAYS).reduce(
    (at, e) => (at === null || e.at < at ? e.at : at),
    null
  );
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
  const src = LOGOS[c.id];
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

// ───────────── 渲染 ─────────────
/** 今日积分到账一行(只有记到账的渠道有 `grantToday`)。
 *
 *  口径见 docs/design/design-workbuddy-ai-channel-2026-10-23.html §4.1:
 *    · 有活跃奖励 → "今日活跃 +30 · 08:12"(不是自己领的就不写"已领取")
 *    · 只有充值   → "今日新增 +500 · 非活跃"(充值到账 ≠ 活跃获取)
 *    · 什么都没到 → "今日未获取积分"(确定的结论,不是"读不到")
 *  `fromApi` 为 false 时补一个"推算" —— 余额正增量反推的,不能当平台确认说。
 */
function grantLine(c) {
  const g = c.grantToday;
  if (!g) return "";
  if (g.hasActivity) {
    const t = g.lastAt ? " · " + fmtClock(g.lastAt) : "";
    return `今日活跃 +${points(g.activityTotal)}${t}${g.fromApi ? "" : " · 推算"}`;
  }
  if (g.total > 0) return `今日新增 +${points(g.total)} · 非活跃`;
  return "今日未获取积分";
}

/** 30 天记录明细。懒加载:点「记录」才查库,不在每轮轮询里读。
 *  倒序 —— 最近的在上;来源如实标(现在全是"推算",接上平台接口后会变"平台确认")。 */
function grantListHtml(id) {
  const rows = GRANTS[id];
  if (!rows) return '<div class="glist"><div class="grow"><span>读取中…</span></div></div>';
  if (!rows.length)
    return '<div class="glist"><div class="grow"><span>还没有记录 —— 第一次到账会在下一轮取数后出现</span></div></div>';
  return `<div class="glist">${rows
    .slice()
    // 最近的在上。后端给的是升序(按天分组要顺着看),这里再排一次 ——
    // 别把"显示顺序"押在调用方的返回顺序上。
    .sort((a, b) => b.ts - a.ts)
    .map(
      (r) =>
        `<div class="grow"><span>${esc(r.day)} ${fmtClock(r.ts)}</span><b>+${points(
          r.amount
        )}</b><i>${r.source === "api" ? "平台确认" : "推算"}</i></div>`
    )
    .join("")}</div>`;
}

/** 状态轨是单行省略:把完整文案挂到 title / aria-label 上,键盘与读屏都能读到全文,
 *  又不会为了换行把行高改来改去。 */
function plainText(html) {
  const d = document.createElement("div");
  d.innerHTML = String(html || "");
  return (d.textContent || "").replace(/\s+/g, " ").trim();
}

function renderRow(c, i) {
  const tone = hvTone(c); // 与其余三种形态同源(设计 P3 · 状态同源)
  const noKey = !c.hasKey;
  const failed = c.hasKey && !c.valid && !c.stale;
  const r = remainRatio(c);
  const pct = r === null ? null : Math.round(r * 100);
  const isPct = c.kind === "percent";
  const isPts = c.kind === "points";
  // 配额型(OpenCode Go):大数字固定取「周期」窗的剩余(实测锚定开通日,旧称「本月」),
  // 下面三行各带一条迷你条把 5 小时 / 本周 / 周期都摊开,右侧附重置倒计时
  const wins = isPct && c.windows && c.windows.length ? c.windows : [];
  const mw = wins.length ? mainWindow(c) : null;
  const codex = c.id === "codex" && !!mw;
  const fiveHour = codex ? wins.find((w) => w.label === "5 小时") : null;
  const codexDual = codex && mw.label === "本周" && !!fiveHour && !noKey && c.remaining != null;

  let main;
  if (noKey) main = "——";
  else if (c.remaining === null) main = "——";
  else if (isPct) {
    // 主数字取绑定窗的余量;「用掉还是剩余」只在这里翻一次,四处同源
    const v = dispPct(Math.max(0, Math.min(100, mw ? mw.remainPercent : c.remaining)));
    main = v.toFixed(1) + '<i class="pct">%</i>';
  } else if (isPts) {
    // 积分不带货币符号;整数部分千分位,小数部分照旧
    const [ip, dp] = points(c.remaining).split(".");
    main = ip + (dp ? '<i class="dec">.' + dp + "</i>" : "") + '<i class="unit">分</i>';
  } else {
    // 金额型:统一走 money(千分位 + 币种,非 CNY 渠道照它自己的币种标,不换算);
    // 小数部分单独包一层 <i class="dec"> 缩一号
    const [int, dec] = money(c.remaining, c.unit).split(".");
    main = int + (dec ? '<i class="dec">.' + dec + "</i>" : "");
  }

  // 申请制额度(4SAPI):分母是本轮额度,副标题给周期状态
  const claim = c.claim || null;

  // 第二行给状态语义,大数字下方给数值口径,两处不重复。
  // 2026-10-23(设计 docs/design/design-row-detail-2026-10-23.html §5):失败 / 需登录 / 陈旧
  // 这三种"旧快照"行,同一句话只说一次 —— 状态标签说"坏了",副标题只说原因,
  // 右侧只说"多久以前的快照"。以前这三样各写两遍,折叠行 94px、五行字。
  const hv = hvRow(c.id);
  const isStale = !!c.stale || !!(hv && hv.state === "stale");
  const isOld = isStale || failed;
  const hasTag = !!(hv && hv.qualifier && hv.qualifier !== "~") || !!(CFG && CFG.textTone);

  let sub;
  // web = 挂件内网页登录型(WorkBuddy):没登录时说"未登录 X",不是"未检测到登录"
  // —— 后者会让人以为打开客户端就能修好,而桌面端 5.6.2 起把令牌加密存了(读不到)。
  if (noKey)
    sub =
      c.authSource === "app"
        ? `未检测到 ${esc(c.name)} 登录信息`
        : c.authSource === "web"
        ? `未登录 ${esc(c.name)}`
        : "未配置密钥";
  else if ((hv && hv.state === "auth") || (c.recovery && c.recovery.phase === "auth")) sub = esc(c.error || "登录凭据已失效");
  else if (failed && c.recovery && c.recovery.phase === "retrying") sub = "正在获取 · 自动重试中";
  else if (failed)
    sub = hasTag
      ? c.error
        ? esc(c.error)
        : ""
      : `取数失败${c.error ? " · " + esc(c.error) : ""}`;
  else if (c.stale) sub = esc(hv && hv.secondary ? hv.secondary : (c.recovery && c.recovery.phase === "retrying" ? "更新中 · 保留上次额度" : "暂时无法更新 · 保留上次额度"));
  else if (wins.length) {
    // Codex 的套餐档位(free/plus/pro)画在副标题里。OpenCode Go 的 extra 里也可能有
    // 一条「套餐」($10/期 · 未折进消耗),那不是 ChatGPT 订阅 —— 只认 Codex(§6.2)
    const plan = c.id === "codex" ? (c.extra || []).find((e) => e[0] === "套餐") : null;
    sub = plan ? `<span class="plan" title="ChatGPT 订阅">${esc(plan[1])}</span>` : "";
    if (c.id === "codex" && c.resetCredits) {
      const n = c.resetCredits.availableCount;
      sub += `${sub ? " · " : ""}重置卡 ${n == null ? "未知" : esc(n) + " 张"}`;
    }
  }
  else if (c.limited) sub = "已限流";
  else if (isPts) {
    // 记到账的渠道(WorkBuddy AI):这一行给"今天有没有活跃获取" —— 那是它最该被看见的
    // 信息,比"近期会过期多少"更天天变。到期信息仍在右侧 `.p` 与展开区里。
    const gl = grantLine(c);
    const soon7 = expiringWithin(c, SUMMARY_EXPIRY_DAYS);
    sub = gl
      ? esc(gl)
      : soon7 > 0
      ? `近 ${SUMMARY_EXPIRY_DAYS} 天将过期 ${points(soon7)} 分`
      : !c.expiryComplete
        ? c.expiring && c.expiring.length
          ? "到期信息不完整"
          : "未提供到期明细"
        : c.expiring && c.expiring.length
          ? `近 ${SUMMARY_EXPIRY_DAYS} 天无到期`
          : "没有待到期积分";
  } else if (claim) sub = `本轮额度 ${esc(money(claim.amount))}${claimTail(claim)}`;
  // 以前没开申请制时这里显示「额度 ¥{c.total}」:c.total 是**累计发放**(只增不减),
  // 不是额度上限 → "剩 21%" 会随每次充值一路下滑,与还能用多少钱毫无关系(报告 P1-11)。
  // 峰谷渠道(DeepSeek):第二行如实说"现在贵不贵",只报状态不报单价(规则会变)
  else if (c.peak) sub = esc(peakParts(c).label);
  else sub = ""; // 金额型拿到多少就是可用多少,不加说明

  let subVal;
  if (noKey)
    subVal = c.authSource === "app" ? "需打开应用" : c.authSource === "web" ? "待登录" : "待配置";
  else if (failed) subVal = c.updatedAt ? snapAge(c.updatedAt) : "未连上";
  // 陈旧(取数成功但过期):年龄比"剩多少 / 哪个窗"更重要 —— 数字再准也是旧的
  else if (isStale) subVal = c.updatedAt ? snapAge(c.updatedAt) : "上次快照";
  else if (mw) {
    // 重置时间未知时不再写一个悬空的「—」:写「周期窗 · —」比只写「周期窗」更难读
    const ms = resetMs(mw);
    subVal = `${esc(mw.label)}窗${ms !== null ? " · " + fmtReset(ms, true) : ""}`;
  }
  else if (isPts) {
    // 最近到期由卡片底部的状态轨承担(日期 + 数量 + 相对时间);
    // 右侧限定语不再重复一次,把空间留给大数字。
    subVal = "";
  }
  // 峰谷倒计时交给 1 秒一跳的 ticker 填,不用等下一轮取数
  else if (c.peak)
    subVal = `<span data-peakcd='${JSON.stringify(c.peak)}'>${esc(
      peakCountdown(c.peak, Date.now())
    )}</span>`;
  else if (pct !== null) subVal = `剩 ${pct}%`;
  else subVal = "";

  if (codexDual) {
    const shortValue = dispPct(Math.max(0, Math.min(100, fiveHour.remainPercent)));
    subVal = `<span class="quota-secondary"><span>5 小时${CFG.showUsed ? "已用" : "剩余"}</span><b style="color:${toneFgSoft(quotaTone(c, fiveHour))}">${shortValue.toFixed(1)}%</b></span>`;
    // 两项额度占右侧两行,旧数据的年龄移到左侧,仍然显式可见。
    if (isOld) sub = `${c.updatedAt ? snapAge(c.updatedAt) : "上次快照"}${sub ? " · " + sub : ""}`;
  }
  // 失联 / 旧快照:数值降为 text-3 并加 ~,不让"上次成功值"看起来像当前值。
  // 紧凑条里由 CSS 的 ::before 画 ~(省一个节点),面板里用真实节点。
  const compactNow = document.body.className.includes("form-compact");
  // Codex 的窗口名与重置时间由状态轨承担(「本周窗 · N 天后重置」);
  // 主数字只留百分比,避免「本周剩余」与「本周窗」在同一张卡上重复。
  const mainValue = isOld && !compactNow ? `<i class="tilde">~</i>${main}` : main;
  const mainTone = isOld ? "off" : codex ? quotaTone(c, mw) : tone;

  // 金额型第二行两边都空时给今日消耗兜底:否则整行会比别的行矮一截(Hapi 39px vs 51px)
  if (!noKey && !failed && c.kind === "amount" && !sub && !subVal && c.day !== null) {
    sub = `今日 ${money(c.day, c.unit)}${c.estimated ? "(推算)" : ""}`;
    subVal = "&nbsp;"; // 右侧留一行空位,和别的行一样高(不留就矮 2px)
  }

  // 组合预警(裁决②):周期余量低于标红线 **且** 距重置 >3 天才提示 ——
  // 重置就在眼前的低余量不值得喊,避免天天狼来了
  let winWarn = "";
  if (isPct && mw && !failed) {
    const ms = resetMs(mw);
    const rem = Math.max(0, Math.min(100, mw.remainPercent));
    if (ms !== null && rem < CFG.critPercent && ms - Date.now() > 3 * 86400e3) {
      winWarn = `<div class="win-warn">周期余量仅 ${rem.toFixed(1)}%,距重置还有 ${Math.round(
        (ms - Date.now()) / 86400e3
      )} 天 —— 省着用或等重置</div>`;
    }
  }

  // 状态点(U2 分层):未配置=空心环,失联=斜杠灰点 —— 两种灰色不再共用
  const dcls = dotCls(c);
  const dot = `<span class="dot${dcls ? " " + dcls : ""}"></span>`;

  // 配额型的三条迷你条已经表达了余量,积分型的到期列表也没有"余量比例"可画
  const bar =
    pct !== null && !failed && !noKey && !wins.length && !isPts
      ? `<div class="bar"><i style="width:${pct}%;background:${TONE_HEX[tone]}"></i></div>`
      : "";

  // 撑不到下次可申请:按当前速度余额不够撑到冷静期结束,行内直接提示
  const claimWarn =
    claim && claim.shortageRisk && claim.daysOfBalance !== null && claim.daysOfBalance !== undefined
      ? `<div class="claim-warn">按当前速度余额约可用 ${Math.max(1, Math.round(
          claim.daysOfBalance
        ))} 天,可能撑不到下次可申请</div>`
      : "";

  // 紧凑条放不下整句警示(第二行会把 46px 的单行条撑爆)——折叠形态只在名称行
  // 挂一枚 ⚠:黄=撑不到下次申请,红=周期余量告急;整句仍只在面板形态显示(CSS 控制)
  const wmark = winWarn
    ? '<span class="wmark bad" title="周期余量告急">⚠</span>'
    : claimWarn
      ? '<span class="wmark warn" title="余额可能撑不到下次可申请">⚠</span>'
      : "";

  // 签到徽标:只在该渠道开了签到时出现,折叠成紧凑条也保留(与 .wmark 同一套机制)
  const ckb = ckBadgeHtml(c);

  // 「更新于」不占行头:CSS 会把陈旧行的数字降透明度,旧快照的年龄由右侧 `.p` 写一次
  // (2026-10-23 起;以前这里还有一条黄色 `.rhint`,与右侧「上次快照 / 12 分钟前快照」重复)

  // 两级行:需要动作的(T0)与已跌破红线的行给两行高与一条进度条,其余只留一行。
  // 状态标签是"颜色之外"的第二个通道(P4 · 颜色不单独承重)。
  const urgent = !!(hv && (hv.tier === 0 || hv.tone === "bad"));
  // 状态标签:颜色之外的第二个通道(P4)。默认只给"需要动作"的挂一枚;
  // 开了「非颜色模式」后每一行都写文字态 —— 灰度截图下也读得出来(D12)。
  const TONE_WORD = { ok: "正常", warn: "预警", bad: "告急", off: "无数据" };
  const qtag =
    hv && hv.qualifier && hv.qualifier !== "~"
      ? `<span class="qtag">${esc(hv.qualifier)}</span>`
      : CFG && CFG.textTone
        ? `<span class="qtag">${TONE_WORD[tone] || ""}</span>`
        : "";

  // 卡片状态轨:第三行说清"接下来会发生什么"(到期 / 重置 / 快照),
  // 不重复大数字。积分型的最近一笔到期在这里常驻 —— 不用先触发告警。
  let railLeft = sub || "";
  let railRight = subVal || "";
  let railWarn = false;
  if (noKey || failed || isStale) {
    // 原因与快照年龄已经分别写在 .s / .p 里,状态轨不再重复(HUD 同一句话只说一次)
    railLeft = "";
    railRight = "";
  } else if (isPts) {
    const upcoming = (c.expiring || [])
      .filter((e) => e && Number(e.amount) > 0 && e.at * 1000 > Date.now())
      .sort((a, b) => a.at - b.at);
    if (upcoming.length) {
      const e = upcoming[0];
      railLeft = `最近 ${fmtDay(e.at)} ${fmtClock(e.at)} 到期 · ${points(e.amount)} 分`;
      railRight = expiryRelativeSpan(e.at);
      railWarn = e.at * 1000 - Date.now() <= 7 * 86400e3;
    } else {
      // 没有逐笔明细时,Qoder 的账户级有效期是唯一能确认的时间信息;
      // 它仍然只代表账户,不能写成"积分全部到期"。
      const acc = accountExpiryOf(c);
      if (acc && acc.state === "dated" && acc.at) {
        railLeft = `账户额度 ${expiryParts(acc.at).date} ${expiryParts(acc.at).time} 到期`;
        railRight = expiryRelativeSpan(acc.at);
        railWarn = acc.at * 1000 - Date.now() <= 7 * 86400e3;
      } else if (acc && acc.state === "never") {
        railLeft = "账户额度不过期";
        railRight = "逐笔到期未知";
      } else if (acc && acc.state === "dateOnly") {
        railLeft = `账户额度 ${acc.at} 到期`;
        railRight = "逐笔到期未知";
      } else if (Number(c.remaining || 0) <= 0) {
        railLeft = "当前没有剩余积分";
        railRight = "";
      } else if (!c.expiryComplete) {
        railLeft = "逐笔到期时间暂未提供";
        railRight = "到期未知";
      } else {
        railLeft = "当前剩余积分没有已知到期日";
        railRight = "";
      }
    }
  } else if (mw) {
    // 后端视图的 secondary 信息量更大(绑定窗 + 恢复时间),优先用它;
    // 拿不到时给窗口重置时间,不重复右侧 `.p` 的窗口限定语。
    const sec = hv && hv.secondary && !claimWarn && !winWarn ? esc(hv.secondary) : "";
    if (sec) railLeft = sec;
    else {
      const ms = resetMs(mw);
      railLeft = ms !== null
        ? `${mw.label}窗 · ${fmtReset(ms, true)}后重置`
        : "配额窗口 · 按重置恢复,不折算金额";
    }
    railRight = "";
  } else if (c.kind === "amount") {
    // 副标题已经说了申请制 / 峰谷;状态轨给"今天花了多少"和剩余比例,
    // 两条信息不重复。没有今日数据时如实写"暂无",不补 0。
    railLeft = c.day !== null
      ? `今日 ${money(c.day, c.unit)}${c.estimated ? " · 推算" : ""}`
      : c.peak
        ? esc(peakParts(c).label)
        : "今日消耗暂无";
    railRight = pct !== null ? `剩 ${pct}%` : hv && hv.qualifier ? esc(hv.qualifier) : "";
  }
  const railIcon = isPts || mw ? "#i-clock" : failed || isStale ? "#i-info" : "#i-info";

  // data-state:后端判的七态,给 CSS 用 —— 紧凑条与胶囊里没有 .rhint 那行字,
  // 只有靠它才能把"这是旧快照"画出来(设计 P0 验收①)
  return `<div class="row${urgent ? " urg" : ""}${codex ? " codex-quota" : ""}${codexDual ? " codex-dual" : ""}" data-id="${c.id}" data-kind="${c.kind}" data-railwarn="${railWarn ? "1" : "0"}" data-dtone="${dcls}" data-state="${(hv || {}).state || ""}">
    <div class="rhead" tabindex="0" role="button" aria-haspopup="dialog" aria-label="查看 ${esc(c.name)} 详情">
      <div class="r1">
        ${iconHtml(c, "ico", noKey || failed)}
        <div class="nm">
          <div class="n"><span class="nn">${esc(c.name)}</span><span class="nsh">${esc(c.short)}</span>${dot}${wmark}${ckb}${qtag}</div>
          <div class="s">${sub}</div>
        </div>
        <div class="val">
          <div class="v" style="color:${toneFg(mainTone)}">${mainValue}</div>
          <div class="p">${subVal}</div>
        </div>
        <div class="caret">&#9654;</div>
      </div>
      <div class="r2"${railLeft || railRight ? ` role="note" aria-label="${esc(plainText(railLeft) + (railRight ? " · " + plainText(railRight) : ""))}"` : ""}>${
        railLeft || railRight
          ? `<svg aria-hidden="true"><use href="${railIcon}"/></svg><span class="rt" title="${esc(plainText(railLeft))}">${railLeft}</span>${
              railRight ? `<span class="end" title="${esc(plainText(railRight))}">${railRight}</span>` : ""
            }`
          : // 状态轨空态常驻一行不可见占位:既避开 .r2:empty{display:none},
            // 又有和"有文案"完全相同的行高,状态切换不再推动列表
            `<span class="rt rail-hold" aria-hidden="true">&nbsp;</span>`
      }</div>
      ${bar}
      ${claimWarn}
      ${winWarn}
      <span class="sweep"></span>
    </div>
    <div class="rbody" data-body="${c.id}" hidden></div>
  </div>`;
}

// ───────────── 行展开详情:五段骨架 ─────────────
// 2026-10-23(docs/design/design-row-detail-2026-10-23.html §4.2):核心 → 明细 → 最近变动 → 签到 → 页脚,
// 固定顺序,没内容的段不画。同一份列表只画一次:折叠行给结论,展开区给完整列表与动作。

/** ① 核心 · 金额:今日 / 本周 / 本月。 */
function coreAmount(c) {
  const f = (v) => money(v, c.unit);
  return `<div class="sec"><div class="core-use">
    <div>今日<b>${f(c.day)}</b></div>
    <div>本周<b>${f(c.week)}</b></div>
    <div>本月<b>${f(c.month)}</b></div>
    ${
      c.estimated
        ? '<span class="est" title="消耗由本机余额快照变化估算;挂件未运行时段不计入">推算</span>'
        : ""
    }
  </div></div>`;
}

/** ① 核心 · 配额:一窗一行。重置 < 24 小时写相对时间,更久写「N 天 · MM-DD HH:MM」。 */
function coreWindows(c) {
  const codex = c.id === "codex";
  const rows = (c.windows || []).map((w) => {
    const wr = Math.max(0, Math.min(100, w.remainPercent));
    const lim = w.status === "rate-limited";
    const wt = lim ? "bad" : wr < CFG.critPercent ? "bad" : wr < CFG.warnPercent ? "warn" : "ok";
    const ms = resetMs(w);
    let when = "";
    let whenOk = false;
    if (ms !== null) {
      const left = ms - Date.now();
      if (left <= 0) when = "正在重置";
      else if (left < 86400e3) when = fmtReset(ms, true) + "后重置";
      else {
        // 天数走同一个 fmtReset,别再自己 floor 一次 —— 行头写「19 天」、核心段写「18 天」
        // 是同一天的两个说法,谁看都会以为出错(2026-10-23 真机复核抓到)
        const d = new Date(ms);
        when = `${fmtReset(ms, true)} · ${d.getMonth() + 1}-${String(d.getDate()).padStart(
          2,
          "0"
        )} ${fmtClock(Math.floor(ms / 1000))}`;
      }
      // 限流中且 1 小时内重置:写「马上恢复」并染绿 —— 中性倒计时里唯一的好消息
      if (lim && left > 0 && left < 3600e3) {
        when = "马上恢复";
        whenOk = true;
      }
    }
    if (codex) {
      when = ms === null ? "重置时间未知" : `下次重置 ${exactLocalTime(ms / 1000)} · <span data-reset-at="${ms}">${esc(quotaResetCountdown(ms))}</span>`;
      whenOk = false;
    }
    return `<div class="core-w${codex ? " codex-w" : ""}">
      <span class="wl">${esc(w.label)}</span>
      <i class="wb"><i style="width:${wr}%;background:${TONE_HEX[wt]}"></i></i>
      <b class="wv" style="color:${lim ? "var(--bad)" : toneFg(wt)}">${wr.toFixed(1)}%</b>
      <span class="wt${whenOk ? " ok" : ""}">${when}</span>
    </div>`;
  });
  if (!rows.length) {
    const v =
      c.remaining === null
        ? "——"
        : Math.max(0, Math.min(100, Number(c.remaining))).toFixed(1) + "%";
    return `<div class="sec"><div class="core-use"><div>当前剩余<b>${v}</b></div></div></div>`;
  }
  return `<div class="sec">${codex ? `<div class="sec-h"><span>配额窗口</span><span>${esc(localZoneLabel())}</span></div>` : ""}${rows.join("")}${
    // 口径说明只放一次:周窗到点清零,5 小时窗到点恢复 —— 两件事措辞不能混
    codex ? `<div class="core-note">周窗到期即清零,不滚存;5 小时窗按滚动恢复</div>` : ""
  }</div>`;
}

function coreCodexResets(c) {
  if (c.id !== "codex") return "";
  const r = c.resetCredits;
  const n = r && r.availableCount;
  const count = n == null ? "未获取" : `${esc(n)} 张可用`;
  const cards = (r && r.credits) || [];
  const more = !!(rowMore[c.id] || {}).resets;
  const shown = more ? cards : cards.slice(0, 3);
  const rows = shown.map((card) => {
    const at = card.expiresAt;
    const expires = at == null ? "接口未提供到期日" : `${exactLocalTime(at)} 到期${at * 1000 <= Date.now() ? " · 已到期,待刷新" : ""}`;
    return `<div class="reset-credit"><span>${esc(card.title)}</span><b>${esc(expires)}</b></div>`;
  }).join("");
  const notes = [];
  if (!r || r.error) notes.push(r && r.error ? r.error : "重置卡信息未获取,刷新后重试");
  else if (n > 0 && r.credits == null) notes.push("接口仅返回数量,到期明细未提供");
  else if (n > cards.length) notes.push(`接口返回 ${cards.length} 张到期明细,可用数量以 ${n} 张为准`);
  if (r && r.lastRedeemedAt) notes.push(`最近用卡重置 ${exactLocalTime(r.lastRedeemedAt)}`);
  const updated = r && r.updatedAt ? `<div class="fn-note">查询于 ${esc(exactLocalTime(r.updatedAt))}${c.stale || !c.valid ? " · 上次快照" : ""}</div>` : "";
  const toggle = cards.length > 3 ? `<button class="sec-more" data-act="sec-more" data-id="codex" data-sec="resets" aria-expanded="${more}">${more ? "收起" : `全部 ${cards.length} 张`}</button>` : "";
  return `<div class="sec codex-resets"><div class="sec-h"><span>重置卡</span><b>${count}</b></div>${rows}${toggle}${notes.map((s) => `<div class="fn-note">${esc(s)}</div>`).join("")}${updated}</div>`;
}

/** 账户级有效期:新字段优先;老快照只对 Qoder 回退 `extra` 的日期文案。
 *  回退严格只认「YYYY-MM-DD」/「不过期」,不把任意 extra 解释成日期。 */
function accountExpiryOf(c) {
  if (c && c.accountExpiry && c.accountExpiry.state) return c.accountExpiry;
  if (!c || (c.id !== "qoder" && c.id !== "qoder-cn")) return null;
  const line = (c.extra || []).find(([k]) => k === "账户额度到期");
  if (!line) return null;
  const v = String(line[1] || "").trim();
  if (v === "不过期") return { state: "never", at: null };
  if (/^\d{4}-\d{2}-\d{2}$/.test(v)) return { state: "dateOnly", at: v };
  return { state: "unknown", at: null };
}

/** 账户额度到期卡(Qoder):账户级时间与逐笔积分到期严格分开展示。 */
function accountExpiryHtml(c) {
  const acc = accountExpiryOf(c);
  if (!acc) {
    return `<div class="expiry-summary">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>账户额度到期</div>
      <div class="expiry-date" style="color:var(--tx3)">暂未提供</div>
      <div class="expiry-meta"><span>尚无法确认账户有效期</span><strong>账户级有效期</strong></div>
    </div>`;
  }
  if (acc.state === "dated" && acc.at) {
    const p = expiryParts(acc.at);
    const urgent = acc.at * 1000 - Date.now() <= 7 * 86400e3;
    return `<div class="expiry-summary${urgent ? " urgent" : ""}">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>账户额度到期</div>
      <div class="expiry-date">${p.date}<small>${p.time}</small></div>
      <div class="expiry-meta"><span>${expiryRelativeSpan(acc.at)}</span><strong>账户级有效期</strong></div>
    </div>`;
  }
  if (acc.state === "never") {
    return `<div class="expiry-summary">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>账户额度到期</div>
      <div class="expiry-date">长期有效</div>
      <div class="expiry-meta"><span>账户额度不过期</span><strong>账户级有效期</strong></div>
    </div>`;
  }
  if (acc.state === "dateOnly") {
    // 旧快照只有日期:只显示这一天,不补 23:59、不画小时倒计时
    return `<div class="expiry-summary">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>账户额度到期</div>
      <div class="expiry-date">${esc(acc.at)}</div>
      <div class="expiry-meta"><span>旧快照只记录了日期</span><strong>账户级有效期</strong></div>
    </div>`;
  }
  return `<div class="expiry-summary">
    <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>账户额度到期</div>
    <div class="expiry-date">暂未提供</div>
    <div class="expiry-meta"><span>尚无法确认账户有效期</span><strong>账户级有效期</strong></div>
  </div>`;
}

/**
 * 详情里的「登录网页版」就地入口(2026-11-08 用户反馈"页面上找不到入口")。
 *
 * 只在**逐笔明细还不完整**时出现:数据已经在的时候别再劝登录。位置就在
 * 「逐笔积分到期 · 未知」那一段下面 —— 用户是在这里发现"未知"的,入口必须也在
 * 这里,而不是让他去设置页里翻两层折叠。
 */
function qoderLoginBar(c) {
  if (c.id !== "qoder" && c.id !== "qoder-cn") return "";
  if (c.expiryComplete === true) return "";
  return `<div class="qoder-login-bar">
    <span>逐笔积分到期明细需登录网页账号</span>
    <button class="btn s" data-act="qoderweblogin" data-id="${c.id}"${
    QODER_LOGGING ? " disabled" : ""
  }>${QODER_LOGGING ? "登录中…" : "登录网页版"}</button>
    <button class="why" data-why="qoderweb" aria-expanded="false" aria-label="网页会话的说明"></button>
  </div>`;
}

/** 积分到期:WorkBuddy / Trae / WorkBuddy AI / Qoder 共用的唯一组件。
 *  `expiring` 与 `expiryComplete` 是唯一数据源;不从渠道名或文案猜日期。 */
function pointsExpiryHtml(c, showAll) {
  const nowS = Date.now() / 1000;
  const isQoder = c.id === "qoder" || c.id === "qoder-cn";
  const all = (c.expiring || []).filter(
    (e) => e && Number(e.at) > 0 && Number.isFinite(Number(e.at)) && Number.isFinite(Number(e.amount))
  );
  const upcoming = all.filter((e) => e.at > nowS).sort((a, b) => a.at - b.at);
  const past = all.filter((e) => e.at <= nowS).sort((a, b) => b.at - a.at);
  let html = isQoder ? accountExpiryHtml(c) : "";

  if (!all.length) {
    const unknownItemized = c.expiryComplete !== true;
    if (isQoder) {
      // 账户卡已经说明了账户级有效期;这里只补"逐笔未知"这一行,
      // 不再叠一张「暂未提供」的大卡(两句话说的是同一件事)。
      html += `<div class="expiry-table"><div class="expiry-entry">
        <div><b>逐笔积分到期</b><p>${
          unknownItemized ? "当前未提供逐笔明细" : "当前剩余积分没有已知到期日"
        }</p></div>
        <span class="setting-end">${unknownItemized ? "未知" : "无记录"}</span></div></div>`;
      html += `<p class="expiry-footnote">${
        unknownItemized
          ? "接口未提供完整的逐笔到期信息;账户有效期与逐笔积分到期不同,不能据此判断所有积分的到期时间。"
          : "账户有效期与逐笔积分到期不同,不能据此判断所有积分在同一时间到期。"
      }</p>`;
      return html;
    }
    const note = unknownItemized
      ? "接口未提供完整的逐笔到期信息,无法判断积分包何时到期"
      : Number(c.remaining || 0) > 0
        ? "当前剩余积分没有已知到期日"
        : "当前没有剩余积分";
    html += `<div class="expiry-summary">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>${
        unknownItemized ? "逐笔到期时间暂未提供" : "最近一笔到期"
      }</div>
      <div class="expiry-date">暂未提供</div>
      <div class="expiry-meta"><span>${note}</span><strong>${unknownItemized ? "到期信息不完整" : "到期信息完整"}</strong></div>
    </div>`;
    html += `<p class="expiry-footnote">${unknownItemized
      ? "渠道尚未返回完整的逐笔到期信息;已知记录会照常显示,不把未知说成「无到期」。"
      : "接口已返回完整明细,但没有可用于展示的到期记录。"}</p>`;
    return html;
  }

  const first = upcoming[0];
  if (first) {
    const p = expiryParts(first.at);
    const urgent = first.at * 1000 - Date.now() <= 7 * 86400e3;
    html += `<div class="expiry-summary${urgent ? " urgent" : ""}">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>${c.expiryComplete === false ? "最近已知到期" : "最近一笔到期"}</div>
      <div class="expiry-date">${p.date}<small>${p.time}</small></div>
      <div class="expiry-meta"><span>${expiryRelativeSpan(first.at)}</span><strong>${points(first.amount)} 积分将到期</strong></div>
    </div>`;
  } else {
    html += `<div class="expiry-summary">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>最近一笔到期</div>
      <div class="expiry-date" style="color:var(--tx3)">没有未到期</div>
      <div class="expiry-meta"><span>已知积分包均已过期,等待下次刷新</span><strong>${past.length} 笔已到期</strong></div>
    </div>`;
  }

  const head = showAll ? [...upcoming, ...past] : upcoming.length ? upcoming.slice(0, 3) : past.slice(0, 3);
  const total = upcoming.length + past.length;
  const rows = head
    .map((e) => {
      const isPast = e.at <= nowS;
      const p = expiryParts(e.at);
      const urgent = !isPast && e.at * 1000 - Date.now() <= 7 * 86400e3;
      return `<div class="expiry-entry core-exp${isPast ? " past" : urgent ? " urgent" : ""}" title="${points(e.amount)} 分 · ${esc(e.label || "积分")}">
        <div><b>${p.date} ${p.time}</b><p>${esc(e.label || "积分")}</p></div>
        <div class="expiry-amount"><strong>${points(e.amount)} <small>分</small></strong>
          <p>${isPast ? "已到期,待刷新" : expiryRelativeSpan(e.at)}</p></div>
      </div>`;
    })
    .join("");
  const more = total > head.length || showAll
    ? `<button class="sec-more" data-act="sec-more" data-id="${c.id}" data-sec="expiry" aria-expanded="${showAll ? "true" : "false"}">${
        showAll ? "收起" : `全部 ${total} 笔`
      }</button>`
    : "";
  html += `<div class="expiry-section-label">逐笔到期明细 · 最近到期优先${
    more ? `<span style="float:right">${more}</span>` : ""
  }</div><div class="expiry-table">${rows}</div>`;

  const notes = [];
  if (!upcoming.length) notes.push("没有未到期的已知积分包;已到期记录保留在列表里。");
  if (c.expiryComplete !== true) {
    notes.push("到期信息不完整:以上只是已获取的记录,不能当作全部积分的最近到期。");
  }
  const sumExp = all.reduce((n, e) => n + Number(e.amount || 0), 0);
  const remain = Number(c.remaining || 0);
  if (c.expiryComplete === true && remain - sumExp > 0.01) {
    notes.push(`已知到期记录合计 ${points(sumExp)} 分,少于当前余额 ${points(remain)} 分;其余积分没有已知到期日。`);
  } else if (c.expiryComplete === true && sumExp - remain > 0.01) {
    notes.push(`已知到期记录合计 ${points(sumExp)} 分,多于当前余额 ${points(remain)} 分;总额仍以后端余额为准。`);
  }
  notes.push("每笔显示尚未使用的积分;不同来源的积分分别到期。");
  html += `<p class="expiry-footnote">${notes.map(esc).join("<br>")}</p>`;
  if (isQoder) {
    html += `<p class="expiry-footnote">账户有效期与逐笔积分到期不同,不能据此判断所有积分在同一时间到期。</p>`;
  }
  return html;
}

/** 近 7 天消耗柱:直接聚合 channel_changes 的现成台账,不新增取数。
 *  零消耗的日子画一根很淡的基线;标题写"无记录",不把缺失数据说成 0。 */
function spendBars(c) {
  if (c.kind !== "amount" || changesState[c.id] === "loading") return "";
  const list = changesCache[c.id] || [];
  if (!list.length) return "";
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const days = Array(7).fill(0);
  for (const g of list) {
    if (!(g.delta < 0)) continue;
    const d = new Date(g.endTs * 1000);
    d.setHours(0, 0, 0, 0);
    const idx = Math.round((today.getTime() - d.getTime()) / 86400000);
    if (idx >= 0 && idx < 7) days[6 - idx] += -g.delta;
  }
  const max = Math.max(...days);
  if (!(max > 0)) return "";
  const bars = days
    .map((v, i) => {
      const date = new Date(today.getTime() - (6 - i) * 86400000);
      const ds = `${date.getMonth() + 1}-${String(date.getDate()).padStart(2, "0")}`;
      const tip = v > 0 ? `${ds} · ${money(v, c.unit || "CNY")}` : `${ds} · 无记录`;
      const h = Math.max(2, Math.round((v / max) * 34));
      return `<i class="${v > 0 ? "" : "zero"}" style="height:calc(${h}px*var(--u))" title="${esc(tip)}"></i>`;
    })
    .join("");
  return `<div class="sec"><div class="sec-h"><span>近 7 天消耗</span><span>推算</span></div><div class="sparkbars">${bars}</div></div>`;
}

/** ③ 最近变动:默认 3 条;先画占位,取到再替换。 */
function ledgerHtml(c, showAll) {
  const st = changesState[c.id];
  const list = changesCache[c.id] || [];
  if (st === "loading") {
    return `<div class="sec"><div class="sec-h"><span>最近变动</span></div><div class="lg-empty">读取中…</div></div>`;
  }
  if (st === "error") {
    return `<div class="sec"><div class="sec-h"><span>最近变动</span></div><div class="lg-empty">读不到变动记录</div></div>`;
  }
  if (!list.length) {
    // 确实取到了台账但没有变动:如实说"无记录";没取过则不装作知道
    if (st !== "done") return "";
    return `<div class="sec"><div class="sec-h"><span>最近变动</span></div><div class="lg-empty">7 天无变动记录</div></div>`;
  }
  const head = showAll ? list : list.slice(0, 3);
  const more =
    list.length > 3
      ? `<button class="sec-more" data-act="sec-more" data-id="${c.id}" data-sec="ledger" aria-expanded="${
          showAll ? "true" : "false"
        }">${showAll ? "收起" : `全部 ${list.length} 条`}</button>`
      : "";
  const rows = head
    .map((g) => {
      const amt = Math.abs(g.delta) < 0.005 ? "±0" : money(Math.abs(g.delta), c.unit || "CNY");
      const cls = g.delta < 0 ? "bad" : "ok";
      // 时间列只有 84px:今天的区间写 HH:MM–HH:MM(跨零点也只写钟点,紧挨着
      // 今天的条目看得懂);更早的只写结束时刻 MM-DD HH:MM。以前两头都带日期,
      // 「10-22 21:00–10-22 21:20」在三档字号下都被省略号切掉(2026-10-23 边界复核)。
      const when =
        g.startTs !== g.endTs && sameLocalDay(g.endTs, Date.now() / 1000)
          ? `${fmtClock(g.startTs)}–${fmtClock(g.endTs)}`
          : fmtStamp(g.endTs);
      const tail =
        g.samples > 1
          ? `推算消费(${g.samples} 次轮询)`
          : g.delta < 0
            ? "推算消费"
            : "入账(充值/额度)";
      const gapH =
        g.gapSecs > 7200 ? `<i class="gap">含 ${Math.round(g.gapSecs / 3600)} 小时未观测</i>` : "";
      return `<div class="lrow"><span class="lt">${when}</span><b class="${cls}">${
        g.delta < 0 ? "−" : "+"
      }${amt}</b><span class="lx">${tail}${gapH}</span></div>`;
    })
    .join("");
  return `<div class="sec"><div class="sec-h"><span>最近变动</span>${more}</div><div class="ledger">${rows}</div></div>`;
}

/** 失败原因在折叠行被省略号截断时,才把全文补到展开区页脚(设计 §4.3)。 */
function revealFailFull(rowEl) {
  if (!rowEl) return;
  const sub = rowEl.querySelector(".nm .s");
  const full = rowEl.querySelector(".fn-fail");
  if (sub && full && sub.scrollWidth > sub.clientWidth + 1) full.hidden = false;
}

/**
 * 展开后的滚动定位:动画走完(320ms)再量,否则量到的是展开前的高度。
 * 卡片比列表还高时让行头贴住列表顶部,否则让卡片下沿进入可视区。
 */
const rowRevealTimers = {};
function scheduleRowReveal(id) {
  clearTimeout(rowRevealTimers[id]);
  rowRevealTimers[id] = setTimeout(() => {
    const row = document.querySelector(`.row[data-id="${id}"]`);
    const list = $("list");
    if (!row || !list || !row.classList.contains("open")) return;
    const lr = list.getBoundingClientRect();
    const rr = row.getBoundingClientRect();
    const top = rr.top - lr.top + list.scrollTop;
    if (rr.height > lr.height || top < list.scrollTop) {
      list.scrollTop = Math.max(0, top);
    } else if (top + rr.height > list.scrollTop + lr.height) {
      list.scrollTop = top + rr.height - lr.height;
    }
  }, 340);
}

function renderDetail(c, inDialog = false) {
  const noKey = !c.hasKey;
  const failed = c.hasKey && !c.valid && !c.stale;

  // 行详情只留一个短操作入口;具体设置集中在管理页。
  const manageLink = `<button class="btn s" data-act="open-manage" data-id="${c.id}">渠道设置</button>`;

  if (noKey) {
    // 网页登录型渠道(WorkBuddy):说清为什么不能自动读 —— 桌面客户端把令牌加密了,
    // 只能在本挂件里登录一次网页账号。别让人去开客户端,那没用。
    if (c.authSource === "web") {
      return `<div class="rbody-in">
        <div class="hint" style="margin:0 0 8px">读不到桌面端登录态;请在渠道设置里登录 ${esc(
          c.name
        )} 网页账号。</div>
        <div class="detail-actions">${manageLink}</div>
      </div>`;
    }
    // 复用本机登录态的渠道:没有"密钥"可填,只能引导用户去客户端登录一次
    if (c.authSource === "app") {
      return `<div class="rbody-in">
        <div class="hint" style="margin:0 0 8px">未检测到 ${esc(
          c.name
        )} 登录态。打开客户端登录后刷新;挂件只读凭据,不会改写或续期。</div>
        <div class="detail-actions">${manageLink}</div>
      </div>`;
    }
    return `<div class="rbody-in">
      <div class="hint" style="margin:0 0 8px">未配置密钥,当前没有数据。密钥保存在 Windows 凭据管理器。</div>
      <div class="detail-actions">${manageLink}</div>
    </div>`;
  }

  const state = hvRow(c.id) || {};
  const dim = failed || state.state === "stale" || !!c.stale;
  const more = rowMore[c.id] || {};

  // ① 核心(口径三选一);积分型统一走「最近一笔 + 逐笔明细」组件,
  // 不再按 id 分支 —— WorkBuddy / Trae / WorkBuddy AI / Qoder 共用同一套。
  let core;
  if (c.kind === "points") core = pointsExpiryHtml(c, !!more.expiry);
  else if (c.kind === "percent" && (c.windows || []).length) core = coreWindows(c);
  else core = coreAmount(c);

  // ② 明细:申请制四项 + extra。已经出现在行头或核心段的不再列(设计 §4.2):
  // 本轮额度 / 可再申请 / 套餐档位 / 额度包笔数;逐笔到期明细由核心段兜底文案承担。
  const claimKv = [];
  if (c.claim) {
    const cl = c.claim;
    claimKv.push([
      "上次申请",
      cl.lastClaimAt
        ? `${fmtDay(cl.lastClaimAt)}(${cl.source === "auto" ? "自动检测" : "手动"})`
        : "未记录",
    ]);
    if (cl.dailyBurn !== null && cl.dailyBurn !== undefined) {
      claimKv.push(["日均消耗(推算)", money(cl.dailyBurn, c.unit)]);
    }
    if (cl.daysOfBalance !== null && cl.daysOfBalance !== undefined) {
      claimKv.push(["预计可用", `约 ${Math.round(cl.daysOfBalance)} 天`]);
    }
    if (c.total !== null && c.total !== undefined) {
      claimKv.push(["累计发放", money(c.total, c.unit)]);
    }
  }
  // 只去掉**确实重复**的几条,不再按条数截断:截断会把「统计范围(只取了前 200 笔)」
  // 「套餐 · 未折进消耗」这类警示悄悄吞掉(2026-10-23 边界复核)。
  //   套餐         —— 只有 Codex 把它画进了副标题;Qoder / OpenCode Go 的套餐只在这里
  //   额度包/积分包 —— 笔数,核心段已经逐笔列出(有逐笔数据时才算重复)
  //   逐笔到期明细  —— 「接口未提供」,核心段的兜底文案已经说了
  const hasExpList = c.kind === "points" && (c.expiring || []).length > 0;
  const dupExtra = (k) =>
    (k === "套餐" && c.id === "codex") ||
    ((k === "额度包" || k === "积分包") && hasExpList) ||
    (k === "逐笔到期明细" && c.kind === "points" && !hasExpList) ||
    // 账户额度到期已经由积分组件顶部的账户卡说了,不再在明细里重复
    (k === "账户额度到期" && c.kind === "points" && !!accountExpiryOf(c));
  const extras = (c.extra || []).filter(([k]) => !dupExtra(k));
  // 显示宽度:中日韩字符按 2 计。半栏大约放得下 26 个单位(键 + 值),
  // 再长就独占一行 —— 按字符数判会把「只取了前 200 笔,实际 230 笔」放进半栏切掉
  const dispW = (s) => [...String(s)].reduce((n, ch) => n + (/[\u2e80-\uffef]/.test(ch) ? 2 : 1), 0);
  const kv = [...claimKv, ...extras].map(([k, v]) => [k, String(v), dispW(k) + dispW(v) > 26]);
  // 窄的在前、宽的在后:宽条目夹在中间会把两栏网格切出半行空洞
  kv.sort((a, b) => Number(a[2]) - Number(b[2]));
  const kvHtml = kv.length
    ? `<div class="sec"><div class="kv">${kv
        .map(
          ([k, v, wide]) =>
            `<div${wide ? ' class="wide"' : ""}><span>${esc(k)}</span><b>${esc(v)}</b></div>`
        )
        .join("")}</div></div>`
    : "";

  // ⑤ 页脚说明:只留跟当前数据状态有关的提示(统计缺口 / 换过密钥 / 峰谷规则陈旧)。
  // 接口是否稳定、凭据从哪来属于固定套话,不再展示(2026-11-03)。
  const notes = [];
  if (c.gapDayFrom) {
    notes.push([`今日从 ${fmtClock(c.gapDayFrom)} 起才有数据,今日消耗可能偏低`, "warn"]);
  }
  if (c.accounts > 1) {
    notes.push(["已更换过密钥;这里只统计当前密钥,旧账户历史不混算", ""]);
  }
  if (c.peak && c.peak.tableStale) {
    notes.push(["节假日安排尚未更新,峰谷时段暂按周末规则估算", ""]);
  }
  const failFull =
    failed && c.error ? `<div class="fn-note bad fn-fail" hidden>取数失败:${esc(c.error)}</div>` : "";

  // ③ 最近变动(只金额型) ④ 签到一行(徽标已经说了"签没签",这里只说徽标没说的)
  const ledger = c.kind === "amount" ? ledgerHtml(c, !!more.ledger) : "";
  const spend = c.kind === "amount" ? spendBars(c) : "";
  let ckHtml = "";
  const cks = CK[c.id];
  if (CHECKIN_ON.has(c.id) && cks && cks.enabled) {
    const bits = [];
    if (cks.streakDays) bits.push(`连续 ${cks.streakDays} 天`);
    if (cks.monthCredits) bits.push(`本月 ${cks.monthCredits} 分`);
    if (cks.lastTryAt) bits.push(`上次尝试 ${relTime(cks.lastTryAt)}`);
    if (cks.nextTryAt && cks.nextTryAt * 1000 > Date.now()) bits.push(`下次 ${ckHold(cks)}`);
    if (bits.length) {
      ckHtml = `<div class="sec"><div class="ckline">签到 · ${bits.join(" · ")}</div></div>`;
    }
  }

  // ⑤ 页脚:说明在上、动作在下(右对齐)
  const notesHtml = notes
    .map(([t, cls]) => `<div class="fn-note${cls ? " " + cls : ""}">${esc(t)}</div>`)
    .join("");
  const actions = [];
  if (MODEL_SOURCES.has(c.id) && !noKey) {
    const v = MDL.views[c.id];
    const n = v ? v.usableCount || v.total : 0;
    actions.push(`<button class="btn s" data-act="open-models" data-id="${c.id}" title="浏览并配置 ${esc(c.name)} 的模型">模型配置${
      n ? ` · ${n} 个可用` : ""
    }</button>`);
  }
  // 弹层底部已有「刷新 / 设置与管理」动作条,详情正文不再重复同一入口
  if (!inDialog) actions.push(manageLink);

  // 类名用 rb-dim 不用 dim:全局已有 .dim{color:var(--tx3)},撞名会把整张卡的数值再染一层灰
  return `<div class="rbody-in${dim ? " rb-dim" : ""}">${core}${qoderLoginBar(c)}${spend}${coreCodexResets(c)}${kvHtml}${ledger}${ckHtml}<div class="foot-det">${notesHtml}${failFull}<div class="fn-act">${actions.join(
    ""
  )}</div></div></div>`;
}

// ───────────── 渠道详情弹层(单例) ─────────────
// 同一份 renderDetail 既供弹层使用,也保留给测试与旧入口;DOM 里只有这一个弹层,
// 轮询更新只换内容,不新开第二个 dialog。
let DETAIL_ID = null;
let DETAIL_FOCUS = null;
let DETAIL_BUSY = false;

const KIND_CAPTION = {
  points: "积分 · 逐笔到期",
  percent: "配额 · 按窗口恢复",
  amount: "余额 · 本地快照推算消耗",
};

/** 弹层顶部的大数字:与卡片主数字同一口径,只放大。 */
function detailHeroHtml(c) {
  if (!c.hasKey) return `<div class="detail-hero" style="color:var(--tx3)">——</div>`;
  if (c.remaining === null || c.remaining === undefined) {
    return `<div class="detail-hero" style="color:var(--tx3)">——</div>`;
  }
  if (c.kind === "percent") {
    const mw = mainWindow(c);
    const v = dispPct(Math.max(0, Math.min(100, mw ? mw.remainPercent : c.remaining)));
    return `<div class="detail-hero">${v.toFixed(1)}<span class="unit">%</span></div>`;
  }
  if (c.kind === "points") {
    return `<div class="detail-hero">${points(c.remaining)}<span class="unit">积分</span></div>`;
  }
  const [int, dec] = money(c.remaining, c.unit).split(".");
  return `<div class="detail-hero"><span class="unit">${esc(curSign(c.unit).trim())}</span>${int}${
    dec ? `<span class="unit">.${dec}</span>` : ""
  }</div>`;
}

function detailHeaderHtml(c) {
  const failed = c.hasKey && !c.valid && !c.stale;
  const sub = c.authLabel || KIND_CAPTION[c.kind] || "";
  return `${iconHtml(c, "ico", !c.hasKey || failed)}
    <div class="detail-title"><b id="detailName">${esc(c.name)}</b><p>${esc(sub)}</p></div>
    <button type="button" class="detail-close" data-act="detail-close" aria-label="关闭详情">
      <svg aria-hidden="true"><use href="#i-close"/></svg>
    </button>`;
}

function renderDetailSheet() {
  if (!DETAIL_ID) return;
  const c = CHANNELS.find((x) => x.id === DETAIL_ID);
  if (!c) {
    closeDetail(false);
    showToast("渠道已隐藏或移除,详情已关闭", true);
    return;
  }
  const body = $("detailBody");
  const keepTop = body ? body.scrollTop : 0;
  // 轮询重画时保留弹层内的键盘焦点:按 data-act / data-sec 找回同一个控件
  const sheet = $("detailSheet");
  const active = document.activeElement;
  const inSheet = !!(active && sheet && sheet.contains(active) && active !== sheet);
  const focusSel = inSheet
    ? [
        active.dataset.act ? `[data-act="${active.dataset.act}"]` : "",
        active.dataset.sec ? `[data-sec="${active.dataset.sec}"]` : "",
      ].join("")
    : "";
  $("detailHead").innerHTML = detailHeaderHtml(c);
  const state = hvRow(c.id) || {};
  const stale = c.stale || state.state === "stale" || !c.valid;
  const caption = [
    KIND_CAPTION[c.kind] || "",
    stale ? (c.recovery && c.recovery.phase === "retrying" ? "更新中 · 保留上次快照" : c.stale ? "保留上次快照" : "暂时无法更新") : "",
    c.updatedAt ? `更新于 ${relTime(c.updatedAt)}` : "",
    c.recovery && c.recovery.attempts ? `已尝试 ${c.recovery.attempts} 次` : "",
    c.error && !c.valid ? c.error : "",
  ].filter(Boolean).join(" · ");
  body.innerHTML =
    detailHeroHtml(c) +
    `<div class="detail-caption">${esc(caption)}</div>` +
    renderDetail(c, true) +
    `<div class="detail-actions"><button type="button" class="btn" data-act="detail-refresh"${
      DETAIL_BUSY ? " disabled" : ""
    }>${DETAIL_BUSY ? "刷新中…" : c.valid ? "刷新数据" : "重试连接"}</button>
      <button type="button" class="btn p" data-act="detail-manage">设置与管理</button></div>`;
  body.scrollTop = keepTop;
  if (focusSel) {
    const el = sheet.querySelector(focusSel);
    if (el) el.focus({ preventScroll: true });
    else sheet.focus({ preventScroll: true });
  }
  // 弹层里没有折叠行,失败全文直接展开(否则它永远停在 hidden)
  document.querySelectorAll("#detailBody .fn-fail").forEach((el) => {
    el.hidden = false;
  });
  // 详情里新生的「?」也要补说明块(设置页那边是 renderChCards 自己做的)
  hydrateWhy(body);
}

/** 详情打开时把背景区域对读屏器隐藏(视觉遮罩已盖住,这里补语义层)。 */
function setBackdropHidden(on) {
  document.querySelectorAll("body > *").forEach((el) => {
    if (el.classList.contains("shade") || el.id === "toast") return;
    if (on) {
      if (el.getAttribute("aria-hidden") !== "true") {
        el.setAttribute("aria-hidden", "true");
        el.dataset.ah = "1";
      }
    } else if (el.dataset && el.dataset.ah === "1") {
      el.removeAttribute("aria-hidden");
      delete el.dataset.ah;
    }
  });
}

function openDetail(id, trigger) {
  const c = CHANNELS.find((x) => x.id === id);
  if (!c) return;
  if (!DETAIL_ID) DETAIL_FOCUS = trigger || document.activeElement;
  DETAIL_ID = id;
  const shade = $("detailShade");
  shade.hidden = false;
  setBackdropHidden(true);
  document.body.classList.add("detail-open");
  renderDetailSheet();
  if ($("detailBody")) $("detailBody").scrollTop = 0;
  // 焦点进入弹层本身(而不是直接落在关闭按钮上):避免一打开就出现一圈按钮焦点框,
  // Tab 的第一站仍是关闭按钮。
  const sheet = $("detailSheet");
  if (sheet) sheet.focus();
}

function closeDetail(restoreFocus = true) {
  const id = DETAIL_ID;
  DETAIL_ID = null;
  const shade = $("detailShade");
  if (shade) shade.hidden = true;
  setBackdropHidden(false);
  document.body.classList.remove("detail-open");
  const head = $("detailHead");
  const body = $("detailBody");
  if (head) head.innerHTML = "";
  if (body) body.innerHTML = "";
  if (!restoreFocus) return;
  // 焦点回到触发元素(卡片或"最先到期"格);元素消失时退回当前筛选芯片。
  const trigger = DETAIL_FOCUS;
  DETAIL_FOCUS = null;
  const card = id && document.querySelector(`.row[data-id="${id}"] .rhead`);
  const fallback = document.querySelector("#sum button:not(:disabled)") || $("lkKeys");
  if (trigger && trigger.isConnected && trigger.offsetParent !== null) trigger.focus();
  else if (card && card.offsetParent !== null) card.focus();
  else if (fallback) fallback.focus();
}

/** 弹层里的动作:展开明细段 / 刷新 / 去设置。 */
async function detailAction(act) {
  const id = DETAIL_ID;
  const a = act.dataset.act;
  if (a === "detail-close") {
    closeDetail();
    return;
  }
  if (a === "sec-more") {
    const sec = act.dataset.sec;
    const cid = act.dataset.id || id;
    rowMore[cid] = rowMore[cid] || {};
    rowMore[cid][sec] = !rowMore[cid][sec];
    renderDetailSheet();
    // 重画后把焦点交回同一个「全部 / 收起」按钮,键盘用户不会掉到 body
    const again = document.querySelector(`#detailBody .sec-more[data-sec="${sec}"]`);
    if (again) again.focus();
    return;
  }
  if (a === "detail-refresh") {
    if (DETAIL_BUSY) return;
    DETAIL_BUSY = true;
    renderDetailSheet();
    try {
      await refresh(true);
      // 刷新回执不再弹 toast:caption 同步变「更新于 刚刚」+ 刷新按钮已有 ✓ 角标,同屏反馈足够
    } finally {
      DETAIL_BUSY = false;
      renderDetailSheet();
      const again = document.querySelector('#detailBody [data-act="detail-refresh"]');
      if (again) again.focus();
    }
    return;
  }
  if (a === "detail-manage" || a === "open-manage") {
    closeDetail(false);
    openManage();
    showTab("ch");
    const cid = act.dataset.id || id;
    requestAnimationFrame(() => {
      const card = cid && document.querySelector(`#chList .ccard[data-id="${cid}"]`);
      if (card) {
        card.classList.add("open");
        card.scrollIntoView({ block: "center" });
        card.classList.add("flash");
        setTimeout(() => card.classList.remove("flash"), 1150);
      }
    });
    return;
  }
  if (a === "open-models") {
    const cid = act.dataset.id || id;
    closeDetail(false);
    openManage();
    showTab("mdl");
    ensureModels(cid);
    return;
  }
  // 详情里的就地登录入口:不跳设置页,直接开窗(与设置页卡片同一个后端命令)
  if (a === "qoderweblogin") {
    await qoderWebLogin(act.dataset.id || id);
  }
}

/**
 * 数值翻页 —— 现在**只服务胶囊轮播**(动效 E)。首页数据刷新已改为直接写数值:
 * renderInner 不再调用它,所以主数字不会有上下位移、旧数字重影或整卡扫光。
 * el 的内容必须已是新值;host 提供 position:relative 与字体上下文。
 * oldHtml 给幽灵层保留 <i class="dec/unit/pct/pq"> 这类数字排版结构。
 */
const ROLL_GEN = new WeakMap(); // el → 最近一次翻页的代次
let ROLL_SEQ = 0;
function rollValue(el, oldText, host, oldHtml) {
  if (!el || oldText == null || oldText === el.textContent) return;
  const gen = ++ROLL_SEQ;
  ROLL_GEN.set(el, gen); // 旧回调认不出新代次,不会误清新一轮的 class / ghost
  // 同一容器里上一轮的幽灵先收掉:轮播快速切换不许叠出两层旧数字
  if (host) {
    Array.from(host.children).forEach((g) => {
      if (g.classList && g.classList.contains("roll-ghost")) g.remove();
    });
  }
  el.classList.remove("roll-in");
  void el.offsetWidth; // 胶囊轮播重播同名动画仍需一次回流;首页路径已不走这里
  el.classList.add("roll-in");
  setTimeout(() => { if (ROLL_GEN.get(el) === gen) el.classList.remove("roll-in"); }, 360);
  if (!oldText || !host) return;
  const g = document.createElement("span");
  // ghost 只吃稳定的排版 class:带上临时的 roll-in 会让旧值也跟着上下位移,
  // 看起来就是"两层数字一起动"的重影。
  g.className =
    el.className.split(/\s+/).filter((c) => c && c !== "roll-in" && c !== "roll-ghost").join(" ") +
    " roll-ghost";
  if (oldHtml) g.innerHTML = oldHtml;
  else g.textContent = oldText;
  g.style.left = el.offsetLeft + "px";
  g.style.top = el.offsetTop + "px";
  host.appendChild(g);
  setTimeout(() => { if (ROLL_GEN.get(el) === gen) g.remove(); }, 380);
}

/** 动效 I:面板入场(行自上而下 30ms 节奏淡入)。只在「进入面板形态/从托盘唤出/启动首帧」放。 */
function playEntrance() {
  const l = $("list");
  if (!l) return;
  l.classList.remove("st-in");
  void l.offsetWidth; // 同名 class 重挂必须先回流才能重播动画
  l.classList.add("st-in");
  setTimeout(() => l.classList.remove("st-in"), 700);
}

/** 渲染异常不许把界面留在「正在取…」或整块空白上,也不许被误报成"取数失败"。
 *  后端成功 + 前端抛异常时,用户只看到空白与一句错的提示(报告 P2-33;
 *  renderModels 早就为同一个坑包过一层,主路径漏了)。 */
function render(opts) {
  // 轮询会把 #list 与展开行的详情整块重建,新建元素的 scrollTop 天生是 0。
  // 逐笔到期不再自带内滚(2026-10-23 五段骨架),只剩列表自己的滚动位置要补。
  const listEl = $("list");
  const listTop = listEl ? listEl.scrollTop : 0;
  try {
    renderInner(opts);
  } catch (err) {
    const box = $("list");
    if (box) {
      box.innerHTML =
        '<div class="empty">界面渲染出错:' + esc(String((err && err.message) || err)) + "</div>";
    }
    showToast("渲染出错:" + ((err && err.message) || err), true);
    return;
  }
  if (listEl) listEl.scrollTop = listTop;
}

/** 主数字的文字写法。四种形态共用它 —— 不再各自拼 percent / money / points。 */
/** 百分比口径:看剩余还是看用掉(设计 P3)。开了「用掉还是剩余」时,这里一处翻转,
 *  四个形态都跟着变 —— 口径只能有一处,不然同一份数据会自相矛盾。 */
function dispPct(remain) {
  const v = Number(remain);
  if (!Number.isFinite(v)) return v;
  return CFG && CFG.showUsed ? 100 - v : v;
}

function hvText(h) {
  if (!h) return "——";
  if (h.percent !== null && h.percent !== undefined) return `${dispPct(h.percent).toFixed(1)}%`;
  if (h.amount !== null && h.amount !== undefined) return money(h.amount, h.unit || "CNY");
  if (h.points !== null && h.points !== undefined) return `${points(h.points)} 分`;
  return "——";
}

/** 非人民币并列时的写法:只标币种,不折算(Q1 的裁断:静默换算等于改了用户的钱数)。 */
function otherMoneyTxt(o) {
  const sym = o.unit === "USD" ? "$" : o.unit === "CNY" ? "¥" : o.unit + " ";
  return sym + (Math.abs(o.amount) < 100 ? o.amount.toFixed(1) : o.amount.toFixed(0));
}

/** 一轮里 ≥3 个渠道取不到数:多半是网络或系统代理,不是渠道坏了 —— 明说(D14)。 */
function failBannerHtml() {
  const n = (HV && HV.errorCount) || 0;
  if (n < 3) return "";
  return `<div class="fstrip"><b>${n}</b> 个渠道取数失败 · 可能是网络或系统代理 ·
    <span class="lk" data-act="open-diag">看诊断</span></div>`;
}

/* ═════════════ 按 key 复用的最小 DOM 更新 ═════════════
 * 首页以前每轮 `innerHTML = ...` 整块重建:节点身份、键盘焦点、进度条宽度过渡
 * 全部丢失,旧值幽灵与扫光也只能在"新节点"上重播。这里改为按稳定 key 复用:
 *   · 展示计算仍只走 renderRow()/各自的 html 函数(不维护第二套文案逻辑);
 *   · 签名就是这一行最终要输出的 HTML 字符串 —— 覆盖所有真正影响显示的输入
 *     (状态、徽标、配额、警告、时间驱动文案),又不会被"时间戳变了但文案没变"
 *     骗去做整行重建;
 *   · 签名不同才做一次逐节点 diff,只写变化的文本/属性/class/样式。
 */
const QKEY = new WeakMap();   // 元素 → 稳定 key
const QSIG = new WeakMap();   // 元素 → 上次输出的 HTML

function qParse(html) {
  const t = document.createElement("template");
  t.innerHTML = String(html).trim();
  return t.content.firstElementChild;
}

/** 子元素匹配键:取第一个 class 作为结构角色,状态类(dot b / ckb ok)不改变键,
 *  所以状态切换是"改 class"而不是"换节点"。 */
function qChildKey(n) {
  if (!n) return "#none";
  if (n.nodeType === 3 || n.nodeType === 8) return "#text";
  if (n.nodeType !== 1) return "#" + n.nodeType;
  const first = (n.getAttribute("class") || "").trim().split(/\s+/)[0] || "";
  const id = n.getAttribute("data-act") || n.getAttribute("data-body") ||
    n.getAttribute("data-id") || n.getAttribute("data-sec") || "";
  return n.tagName.toLowerCase() + "|" + first + "|" + id;
}

function qPatchAttrs(dst, src) {
  for (const a of Array.from(dst.attributes)) {
    if (!src.hasAttribute(a.name)) dst.removeAttribute(a.name);
  }
  for (const a of Array.from(src.attributes)) {
    if (dst.getAttribute(a.name) !== a.value) dst.setAttribute(a.name, a.value);
  }
}

function qPatchChildren(dst, src) {
  let di = 0;
  for (const s of Array.from(src.childNodes)) {
    const key = qChildKey(s);
    let d = dst.childNodes[di] || null;
    if (!d || qChildKey(d) !== key) {
      let found = null;
      for (let j = di + 1; j < dst.childNodes.length; j++) {
        if (qChildKey(dst.childNodes[j]) === key) { found = dst.childNodes[j]; break; }
      }
      if (found) { dst.insertBefore(found, d); d = found; }
      else { d = s.cloneNode(true); dst.insertBefore(d, dst.childNodes[di] || null); }
    }
    // .rbody 的内容由详情/展开逻辑拥有,不参与刷新 diff
    if (!(d.nodeType === 1 && d.classList && d.classList.contains("rbody"))) {
      if (s.nodeType === 3 || s.nodeType === 8) {
        if (d.nodeValue !== s.nodeValue) d.nodeValue = s.nodeValue;
      } else if (s.nodeType === 1 && d.nodeType === 1) {
        qPatchAttrs(d, s);
        qPatchChildren(d, s);
      }
    }
    di++;
  }
  while (dst.childNodes[di]) dst.removeChild(dst.childNodes[di]);
}

/** box 的直接子元素按 key 复用;顺序相同不移动节点,顺序变化才移动既有节点。 */
function qReconcile(box, items) {
  if (!box) return;
  const old = new Map();
  for (const el of Array.from(box.children)) {
    const k = QKEY.get(el);
    if (k) old.set(k, el);
    else el.remove(); // 形态脚本临时刻的节点 / 旧骨架
  }
  const wanted = new Set(items.map((it) => it.key));
  for (const [k, el] of old) if (!wanted.has(k)) el.remove();
  let i = 0;
  for (const it of items) {
    let el = old.get(it.key);
    if (!el) {
      el = qParse(it.html);
      if (!el) continue; // 空模板不该出现;真出现了也不能把 key 挂到 null 上
      QKEY.set(el, it.key);
      QSIG.set(el, it.html);
    } else if (QSIG.get(el) !== it.html) {
      const src = qParse(it.html);
      if (src && el.tagName !== src.tagName) {
        // 摘要格会在 button / div 之间切换(有可点入口 vs 纯展示):
        // 标签变了只能换节点,qPatchAttrs 改不了 tagName。
        QKEY.set(src, it.key);
        QSIG.set(src, it.html);
        box.replaceChild(src, el);
        el = src;
      } else if (src) {
        qPatchAttrs(el, src);
        qPatchChildren(el, src);
        QSIG.set(el, it.html);
      }
    }
    const cur = box.children[i];
    if (cur !== el) box.insertBefore(el, cur || null);
    i++;
  }
  while (box.children[i]) box.removeChild(box.children[i]);
}

/** 顶部临期摘要:30 天内确认到期积分 / 最先到期渠道 / 签到进度。 */
/** 未来 7 天的临期分布:按本地日切桶,用迷你柱表示"哪几天集中到期"。
 *  没有可确认的到期记录时只画一条虚线基线 —— 不虚构柱子。 */
function expiry7Bars(summary) {
  const base = new Date();
  base.setHours(0, 0, 0, 0);
  const baseSec = base.getTime() / 1000;
  const buckets = Array(7).fill(0);
  for (const x of (summary && summary.entries) || []) {
    const idx = Math.floor((x.entry.at - baseSec) / 86400);
    if (idx >= 0 && idx < 7) buckets[idx] += Number(x.entry.amount) || 0;
  }
  const max = Math.max(...buckets);
  if (!(max > 0)) return '<span class="expbars is-empty" aria-hidden="true"></span>';
  return `<span class="expbars" aria-hidden="true">${buckets
    .map((v) => {
      const h = Math.max(2, Math.round((v / max) * 14));
      const op = (0.3 + 0.6 * (v / max)).toFixed(2);
      return `<i style="height:calc(${h}px*var(--u));opacity:${op}"></i>`;
    })
    .join("")}</span>`;
}

function renderRiskBand() {
  const box = $("sum");
  if (!box) return;
  const summary = expirySummary();
  const hasConfirmed = summary.total >= 0.01;
  // 如果一轮新数据已没有可确认的临期积分,退出旧筛选避免渠道列表空白。
  if (EXPIRY_FILTER && !hasConfirmed) EXPIRY_FILTER = false;

  const riskLabel = hasConfirmed
    ? summary.unknown.length ? "已确认临期" : "临期积分"
    : "临期积分";
  const riskValue = hasConfirmed
    ? points(summary.total)
    : summary.unknown.length
      ? "已知 0"
      : CHANNELS.length ? "暂无" : "——";
  const riskSub = hasConfirmed
    ? EXPIRY_FILTER
      ? "点击恢复全部渠道"
      : summary.unknown.length
        ? `${summary.channels} 渠道 · ${summary.unknown.length} 未知`
        : `${summary.channels} 个渠道 · 筛选`
    : summary.unknown.length
      ? "部分渠道到期信息未知"
      : CHANNELS.length ? `${SUMMARY_EXPIRY_DAYS} 天内暂无到期` : "等待数据";
  const riskTitle = EXPIRY_FILTER
    ? "当前只显示临期积分渠道;点击恢复全部渠道"
    : hasConfirmed
      ? `未来 ${SUMMARY_EXPIRY_DAYS} 天可确认的积分到期合计 · 点击筛选临期渠道${
          summary.unknown.length ? `;另有 ${summary.unknown.length} 个渠道到期明细未知` : ""
        }`
      : summary.unknown.length
        ? "有积分渠道未提供逐笔到期信息,无法确认是否临期"
        : `未来 ${SUMMARY_EXPIRY_DAYS} 天没有可确认的积分到期记录`;
  const riskCount = hasConfirmed ? `${points(summary.total)} 分` : "";
  const cell1 = `<button type="button" class="cell metric risk-cell${EXPIRY_FILTER ? " on" : ""}${hasConfirmed ? " has-risk" : ""}"
      data-act="expiry-filter" aria-pressed="${EXPIRY_FILTER}" title="${esc(riskCount ? riskCount + " · " + riskTitle : riskTitle)}"${hasConfirmed ? "" : " disabled"}>
      <div class="metric-label lb"><svg aria-hidden="true"><use href="#i-clock"/></svg>${riskLabel}</div>
      <div class="metric-number vv">${hasConfirmed
        ? `<span class="hero-v">${points(summary.total)}</span><span class="hero-unit">分</span>`
        : `<span class="hero-v" style="color:var(--tx3)">${riskValue}</span>`}</div>
      <div class="metric-sub sb sb-row"><span class="sb-tx">${riskSub}</span>${expiry7Bars(summary)}</div></button>`;

  // 有明细缺口时标成“最早可确认”,不把未知日期的渠道误排在它之后。
  let cell2 = "";
  const first = summary.first;
  if (first) {
    const days = daysUntil(first.entry.at);
    const when = days === 0 ? "今天" : `剩 ${days} 天`;
    const dueColor = days <= 7 ? "var(--warn)" : "var(--tx)";
    const title = `打开 ${first.channel.name} 的到期明细 · ${fmtDay(first.entry.at)} · ${points(first.entry.amount)} 分`;
    cell2 = `<button type="button" class="cell metric" data-act="jump" data-id="${esc(first.channel.id)}" title="${esc(title)}">
      <div class="metric-label lb">${summary.unknown.length ? "最早可确认" : "最先到期"}</div>
      <div class="metric-number due-date" style="color:${dueColor}">${fmtDay(first.entry.at)}<small style="font-size:11px;color:var(--tx3);margin-left:5px;letter-spacing:0">${when}</small></div>
      <div class="metric-sub"><span class="due-name">${esc(first.channel.name)} · ${points(first.entry.amount)} 分</span></div></button>`;
  } else if (summary.unknown.length) {
    const unknown = summary.unknown[0];
    cell2 = `<div class="cell metric"><div class="metric-label lb">到期日期未知</div>
      <div class="metric-number due-date" style="color:var(--tx3)">——</div>
      <div class="metric-sub"><span class="due-name">${esc(unknown.name)} · 逐笔未知</span></div></div>`;
  } else {
    cell2 = `<div class="cell metric"><div class="metric-label lb">最先到期</div>
      <div class="metric-number due-date" style="color:var(--tx3)">暂无</div>
      <div class="metric-sub"><span class="due-name">暂无已知到期记录</span></div></div>`;
  }

  // ③ 签到:常驻。全部签完也要留着「✓ 3/3」—— "完成"和"坏了"不能长得一样(D09)
  let cell3 = "";
  const ck = HV && HV.checkin;
  // total = 0 时写 —— :一个「✓ 0/0」既不是"签完了"也不是"还有事",
  // 只是把"没有签到渠道"这件事说了一遍(2026-10-22 体检:全隐藏时它还在)
  if (ck && ck.total > 0) {
    const segs = Array.from({ length: ck.total }, (_, i) => `<i class="${i < ck.done ? "on" : ""}"></i>`).join("");
    const done = ck.done >= ck.total;
    cell3 = `<button type="button" class="cell metric ck-cell check-cell" data-act="ckstripopen" title="${ck.done}/${ck.total} · 每日签到">
      <div class="metric-label lb">每日签到</div>
      <div class="metric-number vv">${done ? "\u2713 " : ""}<span class="hero-v">${ck.done}</span><span class="hero-unit">/${ck.total}</span></div>
      <div class="metric-sub"><span class="ckseg" aria-hidden="true">${segs}</span></div></button>`;
  } else {
    cell3 = `<div class="cell metric ck-cell"><div class="metric-label lb">每日签到</div>
      <div class="metric-number vv" style="color:var(--tx3)">——</div><div class="metric-sub">未启用</div></div>`;
  }

  // 三格摘要也按 key 复用:轮询时不再整块重建(焦点/按钮身份/高度都更稳)。
  qReconcile(box, [
    { key: "risk", html: cell1 },
    { key: "first", html: cell2 },
    { key: "checkin", html: cell3 },
  ]);
}

/** 状态红晕的代次:快速连续刷新时,旧定时器不许清掉新一轮刚挂上的动画。 */
let STATUS_FLASH_GEN = 0;
const DOT_FLASH_GEN = new WeakMap();

/** 过滤条:默认收起,放大镜展开;渠道多起来(>=12)时仍自动放出输入框。 */
function renderInner(opts) {
  // updated=true 只由「数据刷新」调用方传。首页数据刷新已改为直接写数值,
  // 不再做主数字翻页位移 / 幽灵 / 整卡扫光(见 rollValue 与 CSS 动效层说明)。
  const updated = !!(opts && opts.updated);
  // M7:只记上一轮的状态点,用来识别"刚刚跌破红线"的行 —— 那是唯一保留的
  // 刷新反馈(一枚小红晕),不碰数值、不碰整卡。
  const prevDt = {};
  document.querySelectorAll("#list .row").forEach((r) => {
    if (r.dataset.dtone !== undefined) prevDt[r.dataset.id] = r.dataset.dtone;
  });
  // 标题栏挤了 6 个按钮,计数用短写法,完整说法放 tooltip
  // 标题栏改文字按钮后不再放计数(放不下),底栏已有「N 正常 · M 预警」

  renderRiskBand(); // 顶部摘要常驻:临期积分 / 最先到期 / 签到
  const expiryOnly = EXPIRY_FILTER && currentForm() === "panel";
  // 排序只听设置里的「渠道排序」(2026-11-03 用户裁决):
  // 限流 / 失效不再被强制排到前排,排序口径只有一个来源。
  const tie = stableTie(LAST_RENDER_ORDER);
  const sorted = expiryOnly
    ? CHANNELS.filter(hasConfirmedExpiry)
        .sort((a, b) =>
          (nearestExpiryAt(a) || Infinity) - (nearestExpiryAt(b) || Infinity) || tie(a, b))
    : sortChannels(CHANNELS, LAST_RENDER_ORDER);
  LAST_RENDER_ORDER = sorted.map((c) => c.id);

  if (!CHANNELS.length) {
    // 冷启动画骨架(D14):一行"加载中…"区分不了"正在加载 / 卡住了 / 没配好",
    // 骨架至少说清"这里有 N 个渠道的位置,正在一个个点亮"。
    qReconcile($("list"), [
      { key: "sk-wrap", html: `<div class="sk-wrap">${Array.from({ length: 6 }, () =>
        `<div class="sk-row"><i class="sk-ico"></i><i class="sk-nm"></i><i class="sk-v"></i></div>`
      ).join("")}</div>` },
      { key: "sk-foot", html: `<div class="sk-foot">正在取数 · 首次启动需要几秒</div>` },
    ]);
    LAST_RENDER_ORDER = [];
    // 底栏也要跟着回到"还没数"的状态:以前这里直接 return,底栏留着**上一轮**
    // 的「8 正常 · 3 预警」—— 刚启动就写着 8 个渠道,而列表里一个都还没有。
    $("fdot").className = "dot off";
    $("fstat").innerHTML = '<span class="fb-dot off"></span>正在取数…';
    return;
  }

  // 展示区只显示已配置密钥、且未被隐藏的渠道(隐藏的只在管理页出现,用于恢复);
  // 一个都没配时给一张引导卡,全被隐藏时给「去设置恢复」卡
  const hiddenN = CHANNELS.filter((c) => c.hidden).length;
  const shown = sorted.filter((c) => c.hasKey && !c.hidden);
  const configured = shown.length > 0;

  // 列表按稳定 key 复用:.row / .rhead / .val .v / 进度条都保留同一个节点,
  // 只有输出 HTML 真的变了的行才做一次最小 diff。失败横幅与签到提示条同样按 key 更新。
  const items = [];
  const banner = failBannerHtml();
  const ckStrip = ckPendingStrip();
  if (banner) items.push({ key: "fstrip", html: banner });
  if (ckStrip) items.push({ key: "ckstrip", html: ckStrip });
  if (configured) {
    shown.forEach((c, i) => items.push({ key: "row:" + c.id, html: renderRow(c, i) }));
  } else {
    const emptyHtml = expiryOnly
      ? `<div class="empty">
          <div class="ek">⌕</div>
          <b>没有匹配的临期渠道</b>
          点击顶部临期摘要显示全部渠道
        </div>`
      : hiddenN && CHANNELS.some((c) => c.hasKey && c.hidden)
      ? `<div class="empty">
           <div class="ek">&#128065;</div>
           <b>已配置的渠道都被隐藏了</b>
           隐藏期间不再请求接口,历史快照保留
           <div><button class="btn p" data-act="open-manage" data-first="${
             (CHANNELS.find((c) => c.hidden && c.hasKey) || {}).id || ""
           }">去恢复</button></div>
         </div>`
      : `<div class="empty">
         <div class="ek">&#128273;</div>
         <b>配置第一个渠道</b>
         支持 9 个内置渠道 + 自定义(自己的取数地址与字段路径)<br>密钥与登录令牌只写入 Windows 凭据管理器,界面保存后不回显
         <div><button class="btn p" data-act="open-manage">去配置密钥</button></div>
       </div>`;
    items.push({ key: "empty", html: emptyHtml });
  }
  qReconcile($("list"), items);

  // 列表已经落到 DOM 里,现在才有真实宽度可量:把 Codex 的倒计时按当前面板宽度收一档
  if (configured) fitCodexResetLabels();

  // 底栏状态(隐藏渠道不计入,只报个数)
  const withKey = CHANNELS.filter((c) => c.hasKey && !c.hidden);
  const bad = withKey.filter((c) => !c.valid && !(c.recovery && c.recovery.phase === "retrying")).length;
  const warn = withKey.filter((c) => c.valid && (c.limited || hvTone(c) === "bad")).length;
  // 与行内同一套约定:未配置=空心环,失联=斜杠灰点,预警=黄(之前 warn 用了红点,颜色梯度倒挂)
  $("fdot").className =
    "dot" + (bad ? " st" : warn ? " w" : withKey.length ? "" : " off");
  // 三段必须**互斥**:正常 = 总数 - 告急 - 预警;每个数字前面带自己的状态点。
  const normalN = withKey.length - bad - warn;
  $("fstat").innerHTML = !withKey.length
    ? `<span class="fb-dot off"></span>${hiddenN ? "渠道已全部隐藏" : "未配置渠道"}`
    : [
        normalN ? `<span class="fb-dot"></span>${normalN} 正常` : "",
        warn ? `<span class="fb-dot warn"></span>${warn} 预警` : "",
        bad ? `<span class="fb-dot bad"></span>${bad} 告急` : "",
        hiddenN ? `<span class="fb-dot off"></span>${hiddenN} 隐藏` : "",
      ].filter(Boolean).join('<span class="fb-sep">·</span>');

  // 首页刷新回执只剩「状态颜色变化 + 刚跌破红线的一次性红晕」:
  //   · 主数字由 qReconcile 原地改文本,不再翻页位移、不生成 ghost;
  //   · 整卡 sweep 不再触发(.sweep 节点只是保留下来的空壳,不挂 .go);
  //   · 没有错峰定时器,也没有为重启同名动画而强制读布局。
  // 颜色本身写在 .val .v 的 inline style 里,随 diff 原地过渡(CSS transition)。
  if (updated && currentForm() === "panel") {
    const gen = ++STATUS_FLASH_GEN;
    document.querySelectorAll("#list .row").forEach((r) => {
      const id = r.dataset.id;
      // M7:只播一次,连续快速刷新时旧回调认不出最新 gen 就不会误清新的红晕
      if (prevDt[id] !== undefined && prevDt[id] !== "b" && r.dataset.dtone === "b") {
        const d = r.querySelector(".n .dot");
        if (d) {
          DOT_FLASH_GEN.set(d, gen);
          d.classList.add("breath1");
          setTimeout(() => {
            if (DOT_FLASH_GEN.get(d) === gen) {
              d.classList.remove("breath1");
              DOT_FLASH_GEN.delete(d);
            }
          }, 1300);
        }
      }
    });
  }

  renderPill(sorted);
  renderDock();
  renderPillPick();
  fitCompact();
  refreshChCards();
  // 弹层开着时只换内容:保持滚动位置与展开状态,不新开第二个 dialog
  if (DETAIL_ID) renderDetailSheet();
  // 新建的 [data-peakcd] 先填一次文案,别等到下一个整分才出现
  tickPeakCountdowns();
  updateHeaderLive();
  if (manageOpen() && document.body.classList.contains("manage-index")) renderManageIndex();
}

// 紧凑条实测宽度(已是当前字号档的最终逻辑像素),fitCompact 维护、formSize 取用
let compactW = 380;
// 换形态/吸附展开后都要重量一次,但只有"最后一次"有意义:两处裸 setTimeout
// 交叠时前一次会拿着半更新 DOM 算出个错宽度再盖上来(报告 O-10)
let fitCompactTimer = null;
function scheduleFitCompact() {
  clearTimeout(fitCompactTimer);
  fitCompactTimer = setTimeout(fitCompact, 150);
}

/**
 * 紧凑条:宽度贴合内容(上限 380×字号倍率)+ 三级信息降级(裁决 A/B/A)。
 * L0 缩写名+精确数 → 装不下就从最宽的 chip 起去名字(L1)→ 仍超就舍小数位(L2)
 * → 还超才把尾部渠道收进 "+N"。chip 都是 flex:none,offsetWidth 即自然宽,
 * 窗口当前多宽都不影响测量;量完交给 setCompactW 重钳。
 */
/**
 * 紧凑条里的签到 chip:**常驻**。
 *
 * 以前它只在"今天还有没签的"时出现 —— 全部签完就什么都不剩,于是"签完了"
 * 与"这功能坏了 / 没开"在界面上完全同形(D09)。现在它一直在,只是内容不同。
 * 没开签到(分母为 0)时,按"不该提醒一件用户没要求的事"的老规矩不出现。
 */
function compactCheckinChip(list) {
  const ck = HV && HV.checkin;
  // total = 0 时不给 chip:一个「✓ 0/0」既不是"签完了"也不是"还有事",
  // 只是把"没有签到渠道"这件事说了一遍(2026-10-22 体检:全隐藏时它还在)
  if (!ck || !ck.total) return null;
  const el = document.createElement("span");
  el.className = "ckchip";
  el.dataset.act = "ckstripopen";
  el.tabIndex = 0;
  // 整块是拖动区,chip 要能点、能 Tab 到(与 +N 同一套处理)
  el.setAttribute("data-tauri-drag-region", "false");
  const all = ck.done >= ck.total;
  el.textContent = (all ? "\u2713 " : "") + ck.done + "/" + ck.total;
  el.title = all ? "今天已全部签到" : `还有 ${ck.total - ck.done} 个渠道没签到`;
  list.appendChild(el);
  return el;
}

function fitCompact() {
  const list = $("list");
  const rows = [...list.querySelectorAll(".row")];
  // 先整体还原:切回面板时必须把紧凑条里藏掉的行和降级类放出来。
  // 面板形态下这些都是空操作 —— 只在真的带着降级态时才写,免得每轮刷新都
  // 给每张卡片的 class/style 各来一次无意义的 DOM 写入(节点复用的前提)。
  rows.forEach((r) => {
    if (r.style.display) r.style.display = "";
    if (r.classList.contains("lv1")) r.classList.remove("lv1");
    if (r.classList.contains("lv2")) r.classList.remove("lv2");
  });
  list.querySelectorAll(".more,.cempty,.ckchip").forEach((el) => el.remove());
  if (!document.body.classList.contains("form-compact")) return;
  // 收起的 8px 竖条没法量;**展开态**(无论是否吸附)都要量 —— 之前整段跳过,
  // 吸附展开后宽度冻结,数值变长被裁、变短则窗口偏宽(报告 B9)。
  // 展开态量完的落点:setCompactW → applyForm → dockLayout(true),几何归吸附管。
  if (DOCK.on && !DOCK.open) return;

  const u = fsU();
  const maxW = compactMaxPx();
  const gap = 10 * u;
  // 签到 chip 常驻:先插进去、把它的宽度算进 chrome —— 不预留宽度的话,
  // 装到最后一步才发现放不下,末尾的渠道会被它挤掉。
  const ckEl = compactCheckinChip(list);
  const ckW = ckEl && list.contains(ckEl) ? ckEl.offsetWidth + gap : 0;
  // 列表左右留白(11+4) + 右侧按钮组 + 窗口两条竖边框 + 签到 chip
  const chrome = Math.round(15 * u) + $("cctl").offsetWidth + 2 + ckW;
  // `+N` 的宽度**真量一次**(以前写死 28u):"+6" 与 "+12" 差好几像素,
  // 写死的结果是末尾那个 chip 被裁掉一条边(实测 many 场景 283 > 279)。
  const moreW = (() => {
    const probe = document.createElement("div");
    probe.className = "more";
    probe.textContent = "+99";
    probe.style.visibility = "hidden";
    list.appendChild(probe);
    const w = probe.offsetWidth;
    probe.remove();
    return w || Math.round(28 * u);
  })();

  if (!rows.length) {
    // 空态(没配置 / 全隐藏):一条最小 chip 贴到内容宽,不再空荡 380
    const ce = document.createElement("div");
    ce.className = "cempty";
    ce.textContent = $("fstat").textContent || "暂无渠道";
    // 紧凑条整块是 data-tauri-drag-region="deep",新加的孩子默认也在里面:
    // 显式退出拖动区,不然按住它就成了拖窗口(F13)
    ce.setAttribute("data-tauri-drag-region", "false");
    list.appendChild(ce);
    setCompactW(Math.min(maxW, Math.ceil(ce.offsetWidth + gap + chrome)));
    return;
  }

  // 三档各量一遍(读 offsetWidth 会强制回流,6 行 × 3 趟完全够快)
  const wAt = (lv) => {
    rows.forEach((r) => {
      r.classList.remove("lv1", "lv2");
      if (lv) r.classList.add("lv" + lv);
    });
    return rows.map((r) => r.offsetWidth);
  };
  const W = [wAt(0), wAt(1), wAt(2)];
  rows.forEach((r) => r.classList.remove("lv1", "lv2"));

  const lv = rows.map(() => 0);
  let hidden = 0;
  const total = () => {
    const n = rows.length - hidden;
    // 间隙数 = chip 之间 + 「最后一个 chip 与 +N 之间」。以前只算 n-1,
    // 有 +N 时少算一个 gap(10px)—— 末尾 chip 因此被裁掉 4~10px。
    const gaps = Math.max(0, n - 1 + (hidden ? 1 : 0));
    let s = chrome + gap * gaps + (hidden ? moreW : 0);
    for (let i = 0; i < n; i++) s += W[lv[i]][i];
    return s;
  };
  // 降级顺序 = 重要性的倒序(P5 · 降级按重要性,不按宽度):
  // 第一个 chip 就是"最紧的那一个",永远保持完整(图标 + 名称 + 数值);
  // 从最后一个开始依次去名字、舍小数,实在装不下才收进 +N。
  // 以前是"挑当前最宽的降级"—— 最紧的渠道通常名字最长、数值最长,
  // 于是它第一个被削掉名字,而降级顺序与"哪个最该被看见"毫无关系(D07)。
  const strip = (from, to) => {
    while (total() > maxW) {
      let bi = -1;
      for (let i = rows.length - hidden - 1; i >= 1; i--)
        if (lv[i] === from) { bi = i; break; }
      if (bi < 0) break;
      lv[bi] = to;
    }
  };
  strip(0, 1); // ① 去名字,从最后一个起
  strip(1, 2); // ② 舍小数,从最后一个起
  while (rows.length - hidden > 1 && total() > maxW) hidden++; // ③ 尾部收 +N

  const n = rows.length - hidden;
  rows.forEach((r, i) => {
    r.classList.toggle("lv1", lv[i] === 1);
    r.classList.toggle("lv2", lv[i] === 2);
    r.style.display = i < n ? "" : "none";
  });
  if (hidden) {
    const more = document.createElement("div");
    more.className = "more";
    more.textContent = "+" + hidden;
    // I2:截断不再是信息黑洞 —— tooltip 列出被藏渠道,点击展开面板并定位过去
    const names = rows
      .slice(n)
      .map((r) => (CHANNELS.find((c) => c.id === r.dataset.id) || {}).name)
      .filter(Boolean);
    more.title = names.join("、") + " —— 点击展开面板查看";
    more.dataset.first = rows[n] ? rows[n].dataset.id : "";
    // +N 是可点、也要能 Tab 到的(F19):显式退出紧凑条的拖动区(F13),
    // 再给一个 tabindex 与 data-act —— keyActivate 认的是 [data-act]/[data-act] 那套,
    // 没有它键盘按 Enter/Space 是空的(点击本身仍由 #list 的委托分支处理)
    more.setAttribute("data-tauri-drag-region", "false");
    more.tabIndex = 0;
    more.dataset.act = "more";
    list.appendChild(more);
  }
  setCompactW(Math.min(maxW, Math.ceil(total())));
}

/** 重钳紧凑条窗口宽:变宽立刻动(哪怕 1px,窄了 chip 会被裁),变窄 <6px 不动(数值抖动不牵窗)。 */
function setCompactW(w) {
  if (!w || !(w > compactW || compactW - w >= 6)) return;
  compactW = w;
  if (currentForm() === "compact") applyForm("compact", false); // 已排在几何队列里
}

// 胶囊显示哪些渠道:设置里选中的按顺序轮播;一个都没选就自动取最紧张的那个
// (总额不能告诉你哪个 Key 要挂了,所以自动模式只挑最紧的)
const PILL = { list: [], sorted: [], idx: 0, auto: true, degraded: false };
// 8 秒一换(原 5 秒偏赶);悬停不暂停但节奏 ×3 —— 指示点让"会变"可见,
// 所以不需要暂停(暂停曾被误认为轮播坏了),只需要"别在用户盯着看时刚好切走"
const PILL_ROTATE_MS = 8000;

function renderPill(sorted) {
  PILL.sorted = sorted;
  const picked = (CFG && CFG.pillChannels) || [];
  const autoPick = () => {
    // 自动轮播是「最紧张的一个」,口径独立于列表排序(列表顺序只按设置):
    // 绑定窗比例升序 → 无分母金额型按「余额 / 底线」贴近度 → 积分按 7 日到期占比。
    const active = CHANNELS.filter((c) => c.hasKey && !c.hidden && (c.valid || c.stale || (c.recovery && c.recovery.phase === "retrying")));
    const tightness = (c) => {
      const r = remainRatio(c);
      if (r !== null) return r;
      if (c.kind === "amount" && c.remaining !== null) {
        const floor = (CFG && CFG.moneyFloor) || 10;
        return Math.max(0, Math.min(1, c.remaining / floor));
      }
      if (c.kind === "points") {
        return 1 - Math.min(1, expiringWithin(c, SUMMARY_EXPIRY_DAYS) / Math.max(1, c.remaining || 1));
      }
      return 2;
    };
    const first = [...active].sort((a, b) => tightness(a) - tightness(b))[0];
    return first ? [first] : [];
  };
  // 固定列表里的渠道可能全被「隐藏」了(显隐功能):降级为自动挑选。
  // 不能落进 renderPillFace 的「取数失败」兜底 —— 那是与事实相反的故障告警,
  // 其余渠道明明取数正常(报告 B2);degraded 供 tooltip 说明实情。
  const fixed = picked.length
    ? picked.map((id) => CHANNELS.find((c) => c.id === id)).filter(Boolean).filter((c) => !c.hidden)
    : [];
  PILL.degraded = !!picked.length && !fixed.length;
  PILL.auto = !fixed.length;
  PILL.list = PILL.auto ? autoPick() : fixed;
  if (PILL.idx >= PILL.list.length) PILL.idx = 0;
  renderPillFace();
}

let LAST_PILL = { id: null, text: "", html: "" };
/** 胶囊翻页(动效 E):轮播换渠道/换数值时 200ms 纵向翻页 + 图标淡入;只在胶囊形态放 */
function pillAnim(newId, lastText, lastId, lastHtml) {
  const pv = $("pillV");
  const text = pv.textContent;
  if (currentForm() === "pill") {
    if (lastText && text !== lastText) rollValue(pv, lastText, $("pill"), lastHtml);
    if (lastId !== newId) {
      const p = $("pillIco").querySelector(".pico");
      if (p) {
        p.classList.remove("ico-in");
        void p.offsetWidth;
        p.classList.add("ico-in");
      }
    }
  }
  LAST_PILL = { id: newId, text, html: pv.innerHTML };
}

// ─────────── 胶囊单击速览(方案 C:抬升卡片)───────────
// 点一下:窗口向下长出一条,圆角从 999 收到 16,胶囊脸留在顶部,下方三行信息
// (今日 / 最紧 / 签到)。再点一次或 4.5 秒收回。
//
// 为什么是"向下 + 小圆角":999 的圆角在 120px 高时会被钳到 60 —— 视觉上从药丸
// 变成"被吹起来的豆子",这是 v0.1.35/36 看着怪的根因(真机截图量出来的)。
// 圆角收起后它是"药丸展开成一张小卡片",形状身份是连续的。
// 几何操作全走 enqueueGeom(与形态切换同一条队列),尺寸用 LogicalSize。
let PEEK = { open: false, timer: null, h: 0, w: 0 };

/** 今天签到的进度(只数"真领到"的,与签到那三条规矩一致)。 */
/** 今天签到的进度(胶囊速览第 3 行)。
 *
 * **不再自己算日期**(2026-10-21 修两个 bug):
 *   ① 以前只数 `claimed`,`already` 不算 —— 而"平台说今天已签"同样是签完了,
 *      于是真实已签却显示成没签(当天 WorkBuddy 就是这个);
 *   ② 以前用 `new Date()` 算本地日去比对 log 里的 day,而 **Qoder 的周期日从 10:00
 *      起算** —— 10:00 之前必然对不上,明明签了也显示没签。
 * 现在直接用后端算好的 `doneToday`,前端**零日期运算**。 */
/** 参与签到统计的渠道:开了签到 + **没被隐藏** + 确实在渠道列表里。
 *
 * N11(2026-10-21):隐藏只改 hiddenChannels,**不会**关掉签到开关;而节拍器会跳过隐藏渠道、
 * 徽标也不给它画。三处口径不一致的后果是:隐藏 Trae 之后顶部一直挂着
 * "今天还有 1 个渠道没签到" —— 一个永远不会发生的任务被钉在最显眼的位置。
 * 现在三处都用这一份名单。 */
function ckActiveIds() {
  const out = [];
  for (const id of CHECKIN_ON) {
    const s = CK[id];
    if (!s || !s.enabled) continue;
    const c = CHANNELS.find((x) => x.id === id);
    if (!c || c.hidden) continue;
    out.push(id);
  }
  return out;
}

function peekCheckinLine() {
  const ids = ckActiveIds();
  const on = ids.length;
  const done = ids.filter((id) => CK[id] && CK[id].doneToday).length;
  return on ? `签到 ${done}/${on}` : "";
}

function pillPeekHtml() {
  const amt = CHANNELS.filter((c) => c.hasKey && !c.hidden && (c.valid || c.stale) && c.kind === "amount");
  const today = amt.reduce((s, c) => s + (c.day || 0), 0);
  // 胶囊常态只有 132~280px 宽:每行一句话,长渠道名退简称 ——
  // 截断成 "OpenCod…" 比不给还难看(真机截图确认过)
  const lines = [`<div class="pk-row"><span class="pk-k">今日</span><b>${esc(money(today, "CNY"))}</b></div>`];
  const tight = (PILL.sorted || []).find((c) => c.hasKey && !c.hidden) || null;
  if (tight) {
    const r = remainRatio(tight);
    const pct = r === null ? "—" : Math.round(r * 100) + "%";
    // 放得下就用全名(4SAPI / DeepSeek),放不下才退简称(OC)
    const nm = (tight.name || "").trim();
    const name = nm && nm.length <= 7 ? nm : (tight.short || nm).trim();
    lines.push(
      `<div class="pk-row"><span class="pk-k">最紧</span><span class="pk-n">${esc(
        name
      )}</span><b style="color:${TONE_COLOR[toneOf(tight)]}">${pct}</b></div>`
    );
  } else {
    lines.push(`<div class="pk-row"><span class="pk-k">还没有可用渠道</span></div>`);
  }
  const ck = peekCheckinLine();
  if (ck) lines.push(`<div class="pk-row"><span class="pk-k">${esc(ck)}</span></div>`);
  return lines.join("");
}

async function pillPeekToggle() {
  if (currentForm() !== "pill" || DOCK.on) return;
  if (CFG && CFG.peekOnClick === false) return; // 显示页可以关掉
  await enqueueGeom(async () => {
    const box = $("pillPeek");
    if (!box) return;
    if (PEEK.open) return pillPeekHide();
    const u = fsU();
    box.innerHTML = pillPeekHtml();
    box.classList.add("on");
    // 高度:贴着实测的三行(字体档/内容长短都可能变),夹在 56~96 之间
    const ih = Math.ceil(box.getBoundingClientRect().height);
    PEEK.h = Math.min(Math.round(96 * u), Math.max(Math.round(56 * u), ih + Math.round(12 * u)));
    // 宽度:锁在「当前胶囊宽 vs 三行所需宽」的较大值(上限 280 内容宽),
    // 否则长渠道名会被截、或窄胶囊里挤成一团
    const need = Math.ceil(box.scrollWidth) + Math.round(24 * u);
    const [w0, h0] = formSize("pill");
    PEEK.w = Math.min(Math.round(300 * u), Math.max(w0, need));
    document.body.style.setProperty("--peekh", PEEK.h + "px");
    document.body.classList.add("peek-open");
    markSelfMove(FORM_SELF_MOVE_MS);
    await appWindow.setMinSize(null);
    await appWindow.setMaxSize(null);
    // 向下长高:顶边不动,卡片在胶囊下方展开(位置由 clampToMonitor 兜住屏幕边)
    await appWindow.setSize(new T.dpi.LogicalSize(PEEK.w, h0 + PEEK.h));
    await appWindow.setMinSize(new T.dpi.LogicalSize(PEEK.w, h0 + PEEK.h));
    await appWindow.setMaxSize(new T.dpi.LogicalSize(PEEK.w, h0 + PEEK.h));
    await clampToMonitor();
    PEEK.open = true;
    if (PEEK.timer) clearTimeout(PEEK.timer);
    PEEK.timer = setTimeout(() => {
      enqueueGeom(pillPeekHide);
    }, 4500);
  });
}

async function pillPeekHide() {
  if (PEEK.timer) {
    clearTimeout(PEEK.timer);
    PEEK.timer = null;
  }
  const box = $("pillPeek");
  if (box) {
    box.classList.remove("on");
    box.innerHTML = "";
  }
  document.body.classList.remove("peek-open");
  document.body.style.removeProperty("--peekh");
  if (!PEEK.open) return;
  PEEK.open = false;
  // 形态已经换了(比如用户按了 ▲):尺寸归那个形态管,这里不再动窗口
  if (currentForm() !== "pill" || DOCK.on) return;
  const [w, h] = formSize("pill");
  markSelfMove(FORM_SELF_MOVE_MS);
  await appWindow.setSize(new T.dpi.LogicalSize(w, h));
  await appWindow.setMinSize(new T.dpi.LogicalSize(w, h));
  await appWindow.setMaxSize(new T.dpi.LogicalSize(w, h));
}

// 单击 vs 拖动:拖动是系统级拖窗(元素照样收到 mousedown/up),
// 按"位移 < 4px 且 < 400ms"判单击 —— 与吸附的 250ms 判定互不干扰。
(function () {
  const pill = $("pill");
  if (!pill) return;
  let down = null;
  pill.addEventListener("mousedown", (e) => {
    down = { x: e.screenX, y: e.screenY, t: Date.now() };
  });
  pill.addEventListener("mouseup", (e) => {
    if (!down) return;
    const dx = Math.abs(e.screenX - down.x);
    const dy = Math.abs(e.screenY - down.y);
    const dt = Date.now() - down.t;
    down = null;
    if (dx < 4 && dy < 4 && dt < 400) pillPeekToggle();
  });
})();

/** B 光环(2026-09-25,docs/design/design-pill-restyle-5.html 方案 B):状态染到整颗胶囊
 *  (body 的 st-* 类,只在 form-pill 下有样式)+ 发丝条常驻显示剩余百分比。
 *  状态与状态点(dotCls)同源,单一事实来源;灰态 = 无数据,细分语义仍由点承担
 *  (off 空心环 / st 斜杠)。比率拿不到时发丝条清 0 只留轨道。M5 轮播点已退役。 */
const AURA_OF = { "": "ok", w: "warn", b: "bad", off: "fail", st: "fail" };
function applyPillAura(state, ratio) {
  document.body.classList.remove("st-ok", "st-warn", "st-bad", "st-fail");
  document.body.classList.add("st-" + state);
  const fill = $("pillHair") && $("pillHair").firstElementChild;
  if (fill) {
    fill.style.width =
      ratio === null || ratio === undefined
        ? "0%"
        : Math.min(100, Math.max(0, ratio * 100)).toFixed(1) + "%";
  }
}

function renderPillFace() {
  const dot = $("pillDot");
  const icoBox = $("pillIco");
  const c = PILL.list[PILL.idx];
  // 此刻 #pillV 里还是上一次的文本 —— 先存旧值(含排版结构)才能做翻页对比
  const lastText = $("pillV").textContent;
  const lastHtml = $("pillV").innerHTML;
  const lastId = LAST_PILL.id;

  if (!c) {
    const withKey = PILL.sorted.filter((x) => x.hasKey && !x.hidden);
    dot.className = "dot " + (withKey.length ? "st" : "off");
    icoBox.style.display = "none";
    icoBox.innerHTML = "";
    $("pillV").textContent = withKey.length ? "取数失败" : "未配置";
    $("pill").title = withKey.length ? "渠道全部取数失败,点开面板看原因" : "尚未配置密钥";
    applyPillAura("fail", null);
    // 这条分支以前直接 return:中文占位比原来的数字宽,不 fitPill 就被截成
    // "取数失…";托盘也停在上一轮的数字上,该退回品牌图标时退不回去(报告 P1-4)
    fitPill();
    drawTrayIcon();
    pillAnim(null, lastText, lastId, lastHtml);
    return;
  }

  // 图标与列表行同源:官方标 → 字母块 → 失败/未配置灰化(docs/design/design-pill-icon.html 方案 A)
  icoBox.style.display = "contents";
  icoBox.innerHTML = iconHtml(c, "pico", !c.hasKey || !c.valid);

  const pd = dotCls(c);
  dot.className = "dot" + (pd ? " " + pd : "");
  applyPillAura(AURA_OF[pd] || "fail", remainRatio(c));
  // 陈旧:胶囊上没有 .rhint 那行字,只在数值前加一个 ~ —— 四种形态里最省地方的一种写法
  const hvP = hvRow(c.id);
  const staleTilde = c.stale || (hvP && hvP.state === "stale") ? '<i class="tilde">~</i>' : "";
  // 主数字优先用后端的(它已经切到绑定窗 —— 短窗限流时大数字跟着变 0%),
  // 拿不到视图时退回本地算法,行为不回退(D03 在胶囊上的那一半)。
  $("pillV").innerHTML =
    staleTilde +
    (hvP
      ? hvText(hvP.headline) +
        (hvP.qualifier && hvP.qualifier !== "~"
          ? `<i class="pq">${esc(hvP.qualifier)}</i>`
          : "")
      : c.remaining === null
      ? "——"
      : c.kind === "percent"
        ? // 比率拿不到时照面板那样显示「——」:以前 `?? 0` 把"没数"画成 0.0%,
          // 同一份数据在胶囊上说"用完了"、在面板上说"不知道"(F21)
          remainRatio(c) === null
          ? "——"
          : `${(remainRatio(c) * 100).toFixed(1)}<i>%</i>`
        : c.kind === "points"
          ? points(c.remaining)
          : money(c.remaining, c.unit));

  const bits = [c.name];  // tooltip 是纯文本,不需要转义
  const r = remainRatio(c);
  if (!c.hasKey)
    bits.push(c.authSource === "app" ? "未检测到登录" : c.authSource === "web" ? "未登录" : "未配置密钥");
  else if (c.remaining === null) bits.push("取数失败");
  else {
    if (r !== null) bits.push(`剩 ${Math.round(r * 100)}%`);
    if (c.kind === "percent") {
      const mw2 = mainWindow(c);
      const ms2 = resetMs(mw2);
      if (ms2 !== null) bits.push(`${mw2.label}窗 ${fmtReset(ms2, false)}`);
    }
    if (c.kind === "points" && c.expiringSoon) {
      bits.push(`近 30 天过期 ${points(c.expiringSoon.amount)} 分`);
    }
    if (c.limited) bits.push("已限流");
  }
  if (PILL.degraded) bits.push("固定显示的渠道已被隐藏,临时自动挑选");
  else if (PILL.auto) bits.push("自动:最紧张的一个");
  else if (PILL.list.length > 1) bits.push(`${PILL.idx + 1}/${PILL.list.length} 轮播`);
  $("pill").title = bits.join(" · ");

  fitPill();
  drawTrayIcon();
  pillAnim(c.id, lastText, lastId, lastHtml);
}

/**
 * 胶囊宽度随内容自适应:能装下就行,不留多余空白。
 * 用离屏 span 量文本实际宽度(带同款字体,面板形态下也量得到),再加
 * 圆点/图标/按钮/内边距/边框的固定开销。
 * 宽度缩小的变化 <6px 不动窗口(数值抖动不牵窗);增大只要 ≥1px 就立刻动 ——
 * 宁可宽 1px,不能窄 1px(窄了就是截图里 "91…" 那种半截数字)。
 */
function fitPill() {
  const el = $("pillV");
  if (!el) return;
  const probe = document.createElement("span");
  probe.style.cssText =
    "position:absolute;left:-9999px;top:-9999px;white-space:nowrap;visibility:hidden";
  // 逐项拷字体,不要用 `font` 简写 —— 在 Chromium 里 getComputedStyle().font
  // 经常是空串,量出来的就是默认 13px/400 的宽度,窗口会比文字窄几个像素,
  // 胶囊里出现 "WB 6,075…" 这种半截数字(实测踩过)。
  const cs = getComputedStyle(el);
  probe.style.fontFamily = cs.fontFamily;
  probe.style.fontSize = cs.fontSize;
  probe.style.fontWeight = cs.fontWeight;
  probe.style.fontStyle = cs.fontStyle;
  probe.style.letterSpacing = cs.letterSpacing;
  // .t 开着 tabular-nums(等宽数字比比例数字宽 ~5%)。漏拷这项探针会少测,
  // 窗口天生比文字窄一截,"91.5%" 被省略号吃成 "91…"(实测踩过,勿再删)
  probe.style.fontVariantNumeric = cs.fontVariantNumeric;
  probe.textContent = el.textContent || "";
  document.body.appendChild(probe);
  const textW = probe.getBoundingClientRect().width;
  probe.remove();
  // 圆点 6 + 图标 26 + 间距 8×4 + 展开按钮 22 + 内边距 16 + 边框/缓冲 ≈ 104
  const u = fsU();
  const cap = Math.round(280 * u);
  let w = Math.max(Math.round(104 * u), Math.min(cap, Math.ceil(textW) + Math.round(104 * u)));
  // 兜底:探针估算若被真实字体/DPI 打脸(当前窗口正把文字裁掉),按实测差额补够。
  // 一次性补,不驻留余量 —— 数值变小后靠下面的收缩防抖自然窄回去。
  if (currentForm() === "pill" && el.scrollWidth > el.clientWidth) {
    w = Math.min(cap, Math.max(w, pillW + (el.scrollWidth - el.clientWidth) + 4));
  }
  if (w > pillW || pillW - w >= 6) {
    pillW = w;
    if (currentForm() === "pill") applyForm("pill", false);
  }
}

/** 托盘缩写:10 以内留一位小数,两位优先(<1000 直接取整),千「k」万「w」各留一位小数。 */
function trayAbbr(v) {
  const a = Math.abs(v);
  // 小于 10 时取整会把"只剩 5.28"报成 5,把"只剩 0.4"报成 0 —— 越紧张越需要精度
  if (a < 10) return v.toFixed(1);
  if (a < 1000) return String(Math.round(v));
  if (a < 10000) return (v / 1000).toFixed(1) + "k";
  return (v / 10000).toFixed(1) + "w";
}

/** 当前托盘渠道的概览数;拿不到可信数字(未配置/失败/关闭)返回 "" → 画回品牌样式。 */
function trayNumber(c) {
  if (!c || !c.hasKey || (!c.valid && !c.stale)) return "";
  if (c.kind === "percent") {
    const r = remainRatio(c);
    return r === null ? "" : String(Math.round(r * 100));
  }
  if (c.remaining === null || c.remaining === undefined) return "";
  return trayAbbr(c.remaining);
}

/**
 * 托盘图标。两种画法(设置→显示→托盘数字切换):
 *  - 数字式:状态色整底 + 白色粗体概览数(描深边保证任意底色可读)——扫一眼托盘要的就是这个;
 *  - 品牌式:应用图标 + 右下状态角点(无可信数字时的回退,也是开关关闭的样式)。
 * 32×32 画布,系统按 DPI 缩到 16~24px;tooltip 永远带完整信息。
 */
let trayKey = "";
const trayBrand = new Image();
trayBrand.onload = () => {
  trayKey = "";
  drawTrayIcon();
};
trayBrand.src = "brand/logo-tray.png";
function drawTrayIcon() {
  const c = PILL.list[PILL.idx] || null;
  const tone = c ? toneOf(c) : "off";
  const text = c
    ? `${c.name} ${($("pillV").textContent || "").trim()}`
    : PILL.sorted.some((x) => x.hasKey && !x.hidden)
      ? "渠道全部取数失败"
      : "尚未配置密钥";
  const num = CFG && CFG.trayShowNumber !== false ? trayNumber(c) : "";
  const key = tone + "|" + text + "|" + num;
  if (key === trayKey) return; // 内容没变就不重画、不跨进程传数据
  trayKey = key;

  const S = 32;
  const cv = document.createElement("canvas");
  cv.width = S;
  cv.height = S;
  const g = cv.getContext("2d");
  const r = S * 0.22;
  const roundRect = () => {
    g.beginPath();
    g.moveTo(r, 0);
    g.arcTo(S, 0, S, S, r);
    g.arcTo(S, S, 0, S, r);
    g.arcTo(0, S, 0, 0, r);
    g.arcTo(0, 0, S, 0, r);
    g.closePath();
  };
  if (num) {
    roundRect();
    g.fillStyle = TONE_HEX[tone === "off" ? "off" : tone];
    g.fill();
    const fs = num.length >= 4 ? S * 0.34 : num.length === 3 ? S * 0.42 : S * 0.56;
    g.font = `700 ${fs}px "Segoe UI",sans-serif`;
    g.textAlign = "center";
    g.textBaseline = "middle";
    g.lineWidth = S * 0.07;
    g.strokeStyle = "rgba(18,20,26,.8)";
    g.strokeText(num, S / 2, S * 0.54);
    g.fillStyle = "#fff";
    g.fillText(num, S / 2, S * 0.54);
  } else {
    if (trayBrand.complete && trayBrand.naturalWidth) {
      g.drawImage(trayBrand, 0, 0, S, S);
    } else {
      // 图片加载前仍有可见的托盘图标;加载完成后自动重画。
      roundRect();
      g.fillStyle = "#293243";
      g.fill();
    }
    // 右下角标:logo-tray.png 里的 G 已缩到 78%,这里只画底色环 + 状态点。
    g.beginPath();
    g.arc(S * 0.74, S * 0.74, S * 0.185, 0, Math.PI * 2);
    g.fillStyle = "#12141a";
    g.fill();
    g.beginPath();
    g.arc(S * 0.74, S * 0.74, S * 0.13, 0, Math.PI * 2);
    g.fillStyle = TONE_HEX[tone];
    g.fill();
  }

  // 像素走 base64:4096 字节的 RGBA 若按数组过 IPC 会变成两万多字符的 JSON,
  // 胶囊每几秒轮播一次就重发一遍(报告 O-9)。分块转 binary string 是为了
  // 避开 fromCharCode 一次传几千年参数的栈溢出上限。
  const px = g.getImageData(0, 0, S, S).data;
  let bin = "";
  for (let i = 0; i < px.length; i += 0x8000) {
    bin += String.fromCharCode.apply(null, px.subarray(i, i + 0x8000));
  }
  const rgbaB64 = btoa(bin);
  invoke("set_tray_icon", { rgbaB64, size: S, tooltip: text }).catch((e) => {
    // 失败要清掉去重键:否则 trayKey 已记成这一帧,后面永远不再重试,
    // 托盘就停在旧数字上(报告 O-9)
    console.error("更新托盘图标失败", e);
    trayKey = "";
  });
}

/** 设置页「渠道」页签:一卡一渠道 —— 显隐、排序、密钥、额度申请全在卡里。
    (旧版把这三件事拆在「密钥 / 渠道顺序 / 额度申请」三个分区,是"乱"的根源) */
const cardOpen = new Set(); // 展开的卡片 id;轮询重建时保持,不让用户白收起

/** 积分获取记录(只有 WorkBuddy AI 这类"只在余额上留痕"的渠道有)。
 *  `grantOpen` 是展开状态,`GRANTS` 是明细缓存 —— 打开时才查,不在每轮轮询里读库。 */
const grantOpen = new Set();
const GRANTS = {};

function chDot(c) {
  if (c.hidden || !c.hasKey) return "off";
  if (!c.valid) return "st";
  if (c.limited || hvTone(c) === "bad") return "b";
  if (hvTone(c) === "warn") return "w";
  return "";
}

/** 未保存的密钥输入草稿:重绘 #chList 前存、重建后回填(报告 P2-28)。 */
const keyDraft = {};

/** 明文「显示」的 8 秒自动隐。计时器挂在盒子自己身上 —— 渠道卡与模型弹层各有一个
    .reveal-box,两者共用一个变量时,后显示的那个会把前一个的倒计时清掉,
    于是另一处明文永远留在屏幕上(F5)。 */
const REVEAL_MS = 8000;
function revealBoxHide(box) {
  if (!box) return;
  clearTimeout(box._revealTimer);
  box._revealTimer = null;
  box.dataset.on = "0";
  box.innerHTML = "";
}
function revealBoxShow(box, key) {
  if (!box) return;
  clearTimeout(box._revealTimer);
  box.dataset.on = "1";
  box.innerHTML = `<div class="reveal" style="margin-top:4px">${esc(key)}</div>
    <div class="note">8 秒后自动隐;切页签/刷新也会收回</div>`;
  box._revealTimer = setTimeout(() => revealBoxHide(box), REVEAL_MS);
}

/** 设置页的「按需说明」(2026-10-23)。
 *
 *  背景:设置页的说明文字曾堆到 936 个汉字(静态),折合约 570px —— 超过一整个面板的高度;
 *  「密钥只进 Windows 凭据管理器」这一句在界面里出现了 10 次。问题不是"解释太多",而是
 *  **必读和可选平铺在一起**。所以分三层:
 *    L1 常驻 —— 一句后果,写在控件旁边(.desc),只在"说错会让人做错事"时出现;
 *    L2 按需 —— 本表,点「?」原地展开,同一句话全应用只写一份;
 *    L3 归档 —— 关于页的 <details>,读一次就够的长段落。
 *  判据一句话:这句话如果说错了,用户会不会做错事?会 → L1;不会 → L2/L3。
 *
 *  静态模板用 `<button class="why" data-why="key">` 引用,hydrateWhy() 负责补出说明块;
 *  动态渲染(渠道卡)也走同一套,重绘后直接再调一次 hydrateWhy 即可(幂等)。
 */
const WHY = {
  hide: "不再请求接口,也不再读取本机登录凭据;已记录的历史快照不删,点「恢复」立即回到取数。",
  custom: "地址与密钥只发往你填的那个域名;字段路径决定从返回 JSON 的哪一层取余额,换算倍数把它折成元。探活模式只验证可达,不解析金额。",
  wblogin: "点「登录」开一个独立窗口走网页登录(微信扫码/手机号),成功后窗口自动关掉,挂件用这份会话取数 —— 不复制任何东西,也不动桌面客户端的登录。会话约 7 天过期;国内版 5.6.2 起令牌加密存储,已无法自动读取。",
  qoderweb: "逐笔到期明细只有网页会话能拿到,桌面端凭据只有汇总(平台两套接口的差别)。点「登录网页版」开独立窗口,登录后自动关窗并刷新;这套会话只读明细,签到与余额照旧走桌面端。",
  appauth: "凭据来自本机已登录的客户端;失效时打开一次客户端即可。隐藏本渠道后连读取也会停止。",
  compact: "开启后可切成 380×46 的横向条;顶栏固定四枚入口不再显示「紧凑」,用 Ctrl+2 切换紧凑条,Esc 在面板与胶囊之间切换。",
  trayNum: "托盘图标画概览缩写(74 / 1.2k / 82);关闭后回到品牌样式。",
  pulse: "只有红色状态点会 2 秒一次红晕。低位持续期间不重复播报,免得常驻桌面一直闪。",
  notify: "通知本身不支持点击回跳(插件限制),所以窗口藏着时改为闪一下任务栏。",
  sound: "WebAudio 合成,不新增资源文件;默认关。",
  pill: "选中的渠道每 8 秒轮播一次,鼠标停在胶囊上时放慢到 24 秒;渠道隐藏后自动从轮播里剔除。",
  checkin: "走平台内部接口查活动并签到;规则一变就自动暂停,余额显示不受影响。签到走直连不用系统代理 —— 平台按出口 IP 限流,代理 IP 更容易被拒。",
  freq: "间隔只受对方限流礼貌性约束;连续失败会自动退避,面板展开与折叠可以分别设。",
  vault: "加密存放在 Windows 凭据管理器(绑当前 Windows 账户);复制走系统剪贴板;不导出、不自动填到任何地方。",
  diag: "本机检查含快照库可写、开机自启、系统代理;报告里只有域名、状态与数值,没有任何密钥。",
  log: "令牌只记「长度 + 指纹前 8 位」。出问题时点「导出诊断包」,里面已经有环境、体检与最近事件,不用再截图。",
  used: "大数字看「还剩多少」还是「已用多少」;金额型永远显示余额。",
  floor: "算不出比例的渠道(充值型)也据此排序。",
  hotkey: "如 Alt+G;Esc 清除即不启用。",
};

/** 把 [data-why] 按钮补成「按钮 + 可展开说明」。幂等,重绘后可直接再调。
    文案缺失时把按钮一起摘掉 —— 点开一片空白比没有入口更糟。 */
let whySeq = 0;
function hydrateWhy(root) {
  (root || document).querySelectorAll("button.why[data-why]").forEach((btn) => {
    if (btn.dataset.for) return;
    const text = WHY[btn.dataset.why];
    if (!text) { btn.remove(); return; }
    const box = document.createElement("span");
    box.className = "whybox";
    box.dataset.for = btn.dataset.for = "w" + (++whySeq);
    box.innerHTML = `<span class="whybox-in">${text}</span>`;
    // 说明块落在**这一行的最后**:副标题(desc)是 L1,必须紧贴标签;
    // 按钮在横向按钮行(.ccbtns)里时插到整行之后 —— 塞进 flex 行会白占一段宽度。
    const next = btn.nextElementSibling;
    const p = btn.parentElement;
    // 蜷在按钮行里的说明块必须落在整行之后,否则会成为 flex 行的窄列(登录入口两处都这样)
    if (
      p &&
      (p.classList.contains("ccbtns") ||
        p.classList.contains("ccquick") ||
        p.classList.contains("qoder-login-bar"))
    )
      p.after(box);
    else if (next && next.classList.contains("desc")) p.appendChild(box);
    else btn.after(box);
  });
}

function toggleWhy(btn) {
  const on = btn.getAttribute("aria-expanded") === "true";
  btn.setAttribute("aria-expanded", String(!on));
  // 说明块不一定紧跟在按钮后面(见上),按 data-for 找
  const box = document.querySelector(`.whybox[data-for="${btn.dataset.for}"]`);
  if (box) box.classList.toggle("on", !on);
}

function renderChCards() {
  const chBox = $("chList");
  // 设置页真正的滚动容器是 #chList 所在的 .mbody(不是 #chList 自己)。
  // 重绘会整块换掉 #chList 的子节点:滚动值、阅读锚点、输入焦点都要先记下来。
  const scroller = chBox ? chBox.closest(".mbody") : null;
  const keepTop = scroller ? scroller.scrollTop : 0;
  let anchor = null;
  if (scroller && chBox) {
    const sr = scroller.getBoundingClientRect();
    for (const card of chBox.querySelectorAll(".ccard[data-id]")) {
      const r = card.getBoundingClientRect();
      if (r.bottom > sr.top + 4) { anchor = { id: card.dataset.id, delta: r.top - sr.top }; break; }
    }
  }
  const ae = document.activeElement;
  const focusIn = !!(ae && chBox && chBox.contains(ae));
  const focusId = focusIn && ae.id ? ae.id : null;
  const focusAct = focusIn ? ae.getAttribute("data-act") : null;
  const focusDataId = focusIn ? ae.getAttribute("data-id") || "" : "";
  const focusSel = focusIn && ae.selectionStart !== undefined && ae.selectionStart !== null
    ? { start: ae.selectionStart, end: ae.selectionEnd } : null;
  // 重绘前把已显示的密钥收回:明文不许留在 DOM 里过夜(盒子自己的计时器一起清)
  document.querySelectorAll(".reveal-box").forEach((b) => revealBoxHide(b));
  // 点别的卡上的「恢复」眼睛也会整块重建 #chList,新建输入框的 value 是空的 ——
  // 用户刚粘进去、还没点保存的那串密钥就这么没了(报告 P2-28)
  document.querySelectorAll('#chList [id^="key-"]').forEach((el) => {
    keyDraft[el.id.slice(4)] = el.value;
  });
  const ids = currentOrderIds();
  const list = ids.map((id) => CHANNELS.find((c) => c.id === id)).filter(Boolean);
  const map = (CFG && CFG.claimChannels) || {};
  // 顶部恢复条:从**配置**里算,不依赖渠道视图 —— 万一某条卡片被渲染分支吞掉,
  // 这里仍然给得出恢复入口(兜底);点一下全部恢复。
  const hidIds = ((CFG && CFG.hiddenChannels) || []).filter((x) => CHANNELS.some((c) => c.id === x));
  const hidNames = hidIds.map((x) => (CHANNELS.find((c) => c.id === x) || {}).name || x);
  const restoreBar = hidIds.length
    ? `<div class="restorebar" data-id="__all"><span><b>已隐藏 ${hidIds.length} 个</b>(${esc(
        hidNames.join("、")
      )})—— 隐藏期间不取数,历史快照保留</span><span class="btn s" data-act="chshowall">全部恢复</span></div>`
    : "";
  $("chList").innerHTML = restoreBar + list
    .map((c, i) => {
      const isApp = c.authSource === "app";
      // web = 挂件内网页登录(WorkBuddy):桌面客户端把令牌加密了,只能登录网页账号拿
      const isWeb = c.authSource === "web";
      const cc = map[c.id];
      const on = !!(cc && cc.enabled);
      let st;
      if (c.hidden)
        st = c.autoHidden
          ? // 系统藏的,不是用户藏的 —— 必须说清区别,否则用户会去设置里找"我什么时候隐藏的"
            "未检测到登录 · 已自动隐藏(登录后自动出现)"
          : "已隐藏 · 不再取数(历史快照保留)";
      else if (!c.hasKey) st = isApp ? "未检测到登录" : isWeb ? "未登录" : "未配置";
      else if (c.valid)
        st = isApp
          ? `已读取本机登录 · ${esc(c.authLabel)}`
          : isWeb
          ? `已登录 · ${esc(c.authLabel)}<span class="kh" data-hint="${c.id}"></span>`
          : `已配置<span class="kh" data-hint="${c.id}"></span>`;
      else st = (isApp ? "已读取本机登录" : isWeb ? "已登录" : "已配置") + " · 取数失败" + (isWeb ? "(令牌可能已过期)" : "");

      const keyBlock = isWeb
        ? `<div class="ccbtns">
            <button class="btn p" data-act="wblogin" data-id="${c.id}"${WB_LOGGING ? " disabled" : ""}>${
          c.hasKey ? "重新登录 WorkBuddy 账号" : "登录 WorkBuddy 账号"
        }</button>
            <button class="why" data-why="wblogin" aria-expanded="false" aria-label="登录方式的说明"></button>
          </div>
          <div class="ccbtns">
            <input type="password" id="key-${c.id}" autocomplete="off" spellcheck="false"
              value="${esc(keyDraft[c.id] || '')}"
              placeholder="${c.hasKey ? "也可粘贴访问密钥 · 留空则不修改" : "或粘贴访问密钥(登录窗口不通时自救)"}">
            <button class="btn" data-act="savekey" data-id="${c.id}">保存</button>
            ${c.hasKey ? `<button class="btn danger" data-act="delkey" data-id="${c.id}">清除登录</button>` : ""}
          </div>`
        : isApp
        ? `<div class="mini">${
            // 国际版这条链路**会续期**(refreshToken 换 accessToken,成果存在我们自己的库里、
            // 不写回客户端文件),照抄别的 app 型渠道写"不刷新"就是错的(设计稿 §6 记过这一条)
            c.id === "workbuddy-ai" ? "只读复用本机登录,自动续期" : "只读复用本机登录,不写回、不刷新"
          }<button class="why" data-why="appauth" aria-expanded="false" aria-label="凭据来源的说明"></button></div>`
        : `<div class="ksrc" data-src="${c.id}">${srcRowHtml(c.id)}</div>
          <div class="ccbtns">
            <input type="password" id="key-${c.id}" autocomplete="off" spellcheck="false"
              value="${esc(keyDraft[c.id] || '')}"
              placeholder="${c.hasKey ? "已保存 · 留空则不修改" : "粘贴 API Key"}">
            <button class="btn p" data-act="savekey" data-id="${c.id}">保存</button>
            ${c.hasKey ? `<button class="btn danger" data-act="delkey" data-id="${c.id}">删除</button>` : ""}
          </div>`;

      const claimBlock =
        c.kind === "amount"
          ? `<div class="claim-in">
              <div class="field"><span>额度申请制<span class="desc">余额补到固定上限;申请时间自动检测</span></span>
                <button class="sw${on ? " on" : ""}" role="switch" aria-checked="${on}" aria-label="额度申请制" data-act="claimtoggle" data-id="${c.id}"></button></div>
              ${on
                ? `<div class="claim-grid">
                    <label>单次额度<input type="number" min="1" step="10" value="${esc(cc.amount)}" data-act="claimamount" data-id="${c.id}">元</label>
                    <label>最短间隔<input type="number" min="1" step="1" value="${esc(cc.minIntervalDays)}" data-act="claimdays" data-id="${c.id}">天</label>
                    <label>上次申请<input type="date" value="${toDateInput(c.claim && c.claim.lastClaimAt)}" data-act="claimdate" data-id="${c.id}"></label>
                    <div class="ccbtns">
                      <button class="btn" data-act="claimnow" data-id="${c.id}">记一次申请=今天</button>
                      <button class="btn" data-act="claimclear" data-id="${c.id}">清除手动值</button>
                    </div>
                  </div>`
                : ""}
            </div>`
          : "";

      // 积分获取(只有记到账的渠道):今日一句 + 近 7 天汇总,「记录」展开 30 天明细。
      // 记录是**懒加载**的 —— 每轮轮询都查一次 30 天明细纯属浪费。
      const grantBlock = c.grantToday
        ? `<div class="krow"><span class="k1" style="flex:1">积分获取
             <span class="desc">${esc(grantLine(c))}${
            c.grantWeek && c.grantWeek.days ? ` · 近 7 天 ${c.grantWeek.days} 天有获取` : ""
          }</span></span>
             <button class="btn s" data-act="grantlog" data-id="${c.id}">${
            grantOpen.has(c.id) ? "收起" : "记录"
          }</button></div>
           ${grantOpen.has(c.id) ? grantListHtml(c.id) : ""}`
        : "";

      return `<div class="ccard${c.hidden ? " off" : ""}${cardOpen.has(c.id) ? " open" : ""}" data-id="${c.id}">
        <div class="cchead" data-act="chfold" data-id="${c.id}">
          ${iconHtml(c, "ico", !c.hasKey || c.hidden)}
          <div class="ccmeta">
            <div class="ccname">${esc(c.name)}<span class="dot ${chDot(c)}"></span>${c.hidden ? '<span class="tagn">已隐藏</span>' : ""}</div>
            <div class="ccst">${st}</div>
          </div>
          <button class="eye${c.hidden ? " off" : ""}" data-act="chvis" data-id="${c.id}"
            title="${
              c.hidden
                ? c.autoHidden
                  ? "仍然显示(当前没检测到登录,登录后会自动恢复取数)"
                  : "恢复显示(重新开始取数)"
                : "隐藏(停止取数,历史快照保留)"
            }">${c.hidden ? (c.autoHidden ? "显示" : "恢复") : "👁"}</button>
          <span class="ccaret">&#9654;</span>
        </div>
        ${
          // 逐包到期明细的登录入口**必须放在折叠体外面**(2026-11-08 用户反馈):
          // 原来它生在 .ccbody 里,而 .ccbody 默认 display:none、要点卡片头才展开,
          // 等于把一个"去找它"的入口藏进了第二层折叠。现在常驻卡片头下方。
          // 国际版/国内版共用同一入口,Cookie 与会话键在 Rust 侧按渠道隔离;
          // 这套会话只做账务只读,签到与余额的桌面链路一概不碰。
          c.id === "qoder" || c.id === "qoder-cn"
            ? `<div class="ccquick"><span>积分包到期明细走网页会话</span>
                <button class="btn s" data-act="qoderweblogin" data-id="${c.id}"${QODER_LOGGING ? " disabled" : ""}>${QODER_LOGGING ? "登录中…" : "登录网页版"}</button>
                <button class="why" data-why="qoderweb" aria-expanded="false" aria-label="网页会话的说明"></button></div>`
            : ""
        }
        <div class="ccbody">
          ${keyBlock}
          ${
        c.hasKey && c.authSource === "keyring"
          ? `<div class="krow"><span class="k1">密钥 · <span class="khint" data-hint="${c.id}">••••</span></span>
               <button class="btn s" data-act="keycopy" data-id="${c.id}" title="复制整把密钥(明文不回界面)">复制</button>
               <button class="btn s" data-act="keyreveal" data-id="${c.id}" title="显示明文,8 秒后自动隐">显示</button></div>
             <div class="reveal-box" data-rev="${c.id}"></div>`
          : ""
      }
      ${claimBlock}
      ${grantBlock}
      ${
        c.hasKey && !c.hidden
          ? `<div class="krow"><span class="k1" style="flex:1">连接自检<span class="desc">真打一次接口,不落快照</span></span>
               <button class="btn s" data-act="probe" data-id="${c.id}">测试连接</button></div>
             <div class="prb" data-prb="${c.id}"></div>`
          : ""
      }
      ${
        String(c.id).startsWith("custom-")
          ? `<div class="krow"><span class="k1" style="flex:1">自定义渠道<span class="desc">删除只清定义与密钥,历史快照保留</span></span>
               <button class="btn s danger" data-act="cdel" data-id="${c.id}">删除</button></div>`
          : ""
      }
      ${CHECKIN_ON.has(c.id) ? '<div class="ckwrap" data-id="' + esc(c.id) + '">' + ckRowHtml(c.id) + "</div>" : ""}
          <div class="ccord"><span class="mini">在列表中的位置</span>
            <span class="ord">
              <button class="btn ord" data-act="ordermove" data-id="${c.id}" data-dir="-1"${i === 0 ? " disabled" : ""} title="上移">&#9650;</button>
              <button class="btn ord" data-act="ordermove" data-id="${c.id}" data-dir="1"${i === list.length - 1 ? " disabled" : ""} title="下移">&#9660;</button>
            </span>
          </div>
        </div>
      </div>`;
    })
    .join("");
  // 卡片里的「?」是动态生成的,补出说明块(幂等;轮询重绘后也不会丢)
  hydrateWhy($("chList"));
  // 重绘把 .kh / .ksrc 清空了:有缓存就同步贴回去(不产生 RPC)
  paintCachedKeyMeta();
  // 滚动位置 + 可见渠道锚点:卡片展开/收起导致高度变化时,仍停在用户正在读的那张卡
  if (scroller) {
    scroller.scrollTop = keepTop;
    if (anchor) {
      const sr2 = scroller.getBoundingClientRect();
      for (const card of chBox.querySelectorAll(".ccard[data-id]")) {
        if (card.dataset.id !== anchor.id) continue;
        scroller.scrollTop += card.getBoundingClientRect().top - sr2.top - anchor.delta;
        break;
      }
    }
  }
  // 焦点还给同一个控件(输入值由 keyDraft 回填),光标位置一并带上。
  // 没有 id 的按钮用 data-act + data-id 找回来 —— 键盘用户按完按钮后轮询重绘不该丢焦点。
  if (focusIn) {
    let el = focusId ? document.getElementById(focusId) : null;
    if ((!el || !chBox.contains(el)) && focusAct) {
      el = Array.from(chBox.querySelectorAll("[data-act]")).find(
        (x) => x.getAttribute("data-act") === focusAct &&
          (x.getAttribute("data-id") || "") === focusDataId
      ) || null;
    }
    if (el && chBox.contains(el)) {
      el.focus({ preventScroll: true });
      if (focusSel && el.setSelectionRange) {
        try { el.setSelectionRange(focusSel.start, focusSel.end); } catch (_) {}
      }
    }
  }
}

/** 轮询刷新时更新渠道卡;正在输入就不重建,免得把输入内容冲掉。 */function refreshChCards() {
  if (!manageOpen()) return;
  const ae = document.activeElement;
  if (ae && ae.closest && ae.closest("#chList") && (ae.tagName === "INPUT" || ae.tagName === "SELECT")) return;
  renderChCards();
  fillKeyHints();
}

/**
 * 补每把 key 的尾号:重绘后先用缓存**同步**回填,缺失/过期才异步读一次。
 * 失败仍把"凭据库读取失败"写出来 —— 凭据管理器坏了 ≠ 没配密钥(报告 P2-15)。
 */
function fillKeyHints(force = false) {
  paintCachedKeyMeta();
  refreshKeyMeta(force);
}

// ───────────── 渠道密钥的来源(直接填 / 引用密钥箱)─────────────
// 设计见 docs/product/product-vault-channel-bridge-2026-10-20.md(裁决 1–7 已批)。
// 主线是**引用**:同一把钥匙不存两份,改箱子渠道立刻跟着变。
const KEYSRC = {};        // 渠道 id → key_source 的结果
let VAULTREF = [];        // [[渠道 id, 箱子条目 id], ...] 反查用

// 密钥元数据缓存(P2)。打开设置页时,以前每一轮轮询都会读:
//   key_hint × N + key_source × N + 一次引用表 —— 余额刷新顺带读了一堆
// 与余额无关的秘密元数据。现在重绘时**同步**用缓存回填,只有缺失或超过 TTL
// 才异步读一次;保存/删除/引用变化/openManage 时显式失效。
const KEY_META = {};          // 渠道 id → { hint, hintErr, hintAt, source, sourceAt }
const KEY_META_TTL = 5 * 60 * 1000;
let KEY_META_BUSY = null;
let KEY_META_PENDING = false;

function metaFresh(at) {
  return !!at && Date.now() - at < KEY_META_TTL;
}

/** 同步把缓存贴回 DOM:重绘后立刻恢复尾号/来源,不产生任何 RPC。 */
function paintCachedKeyMeta() {
  document.querySelectorAll("[data-hint]").forEach((el) => {
    const m = KEY_META[el.dataset.hint];
    if (!m || !m.hintAt) return;
    if (m.hintErr) {
      el.textContent = " · 凭据库读取失败:" + m.hintErr;
      el.classList.add("err");
    } else {
      el.classList.remove("err");
      el.textContent = m.hint ? " · " + m.hint : "";
    }
  });
  document.querySelectorAll(".ksrc[data-src]").forEach((box) => {
    const id = box.dataset.src;
    const m = KEY_META[id];
    if (!m || !m.sourceAt || !m.source) return;
    KEYSRC[id] = m.source;
    box.innerHTML = srcRowHtml(id);
  });
}

/** 显式失效。`kind` = both | hint | source;`id = null` 表示全部渠道。 */
function invalidateKeyMeta(id = null, kind = "both") {
  const drop = (cid) => {
    const m = KEY_META[cid];
    if (!m) return;
    if (kind !== "source") {
      m.hint = null;
      m.hintErr = null;
      m.hintAt = 0;
    }
    if (kind !== "hint") {
      m.source = null;
      m.sourceAt = 0;
      delete KEYSRC[cid]; // 重绘时先回到占位,别再读旧来源
    }
  };
  if (id) drop(id);
  else Object.keys(KEY_META).forEach(drop);
}

async function doRefreshKeyMeta(force) {
  const wantedHints = CHANNELS.filter(
    (c) => c.hasKey && (force || !metaFresh((KEY_META[c.id] || {}).hintAt))
  );
  const wantedSources = CHANNELS.filter(
    (c) => c.authSource === "keyring" && (force || !metaFresh((KEY_META[c.id] || {}).sourceAt))
  );
  if (!wantedHints.length && !wantedSources.length) return;

  // 「或从密钥箱选一把」要用到箱子列表;用户可能没打开过密钥箱页签,这里按需补一次。
  if (wantedSources.length) {
    if (!VAULT.length) {
      try { VAULT = await invoke("vault_list"); } catch { /* 读不出来就不显示下拉 */ }
    }
    await loadVaultRefs();
  }

  const now = Date.now();
  await Promise.all([
    ...wantedHints.map(async (c) => {
      const m = KEY_META[c.id] || (KEY_META[c.id] = {});
      try {
        m.hint = await invoke("key_hint", { id: c.id });
        m.hintErr = null;
      } catch (e) {
        // 凭据管理器坏了 ≠ 没配密钥:把故障如实写出来,否则用户会反复重填
        m.hint = null;
        m.hintErr = String(e);
      }
      m.hintAt = now;
    }),
    ...wantedSources.map(async (c) => {
      const m = KEY_META[c.id] || (KEY_META[c.id] = {});
      try {
        m.source = await invoke("key_source", { id: c.id });
      } catch (e) {
        // 读不到来源时退回"直接填"的样子 —— 不显示未经验证的引用状态
        m.source = { mode: "inline" };
      }
      m.sourceAt = now;
    }),
  ]);
  paintCachedKeyMeta();
}

/** 读一轮元数据(同一时刻只有一个在飞;期间收到 force 会在结束后补跑)。 */
function refreshKeyMeta(force = false) {
  if (!manageOpen()) return Promise.resolve();
  if (KEY_META_BUSY) {
    KEY_META_PENDING = KEY_META_PENDING || force;
    return KEY_META_BUSY;
  }
  KEY_META_BUSY = doRefreshKeyMeta(force).finally(() => {
    KEY_META_BUSY = null;
    if (KEY_META_PENDING) {
      KEY_META_PENDING = false;
      refreshKeyMeta(true);
    }
  });
  return KEY_META_BUSY;
}

/** 一次问出全部引用关系(箱子列表要标"正被谁用")。 */
async function loadVaultRefs() {
  try { VAULTREF = await invoke("vault_ref_map"); } catch { VAULTREF = []; }
}

/**
 * 哪些渠道正引用这条箱子密钥 —— **只用于列表显示**。
 *
 * 读的是索引(VAULTREF),不是凭据管理器里的真实指针。这个分工是刻意的:
 *   · **显示**用索引:一行一次 SQL,列表几十条也不卡;而且索引在启动时与每次删除后
 *     都会按真实指针重抄一遍(product-vault-ref-reconcile),所以它现在是可信的。
 *   · **删除决策**用真实指针:那是破坏性操作,一律由后端 vault_delete 扫指针判定,
 *     不信索引 —— 索引读坏了也不该让人误删。
 * 换句话说:索引可以拿来**看**,不能拿来**决定删不删**。
 */
function refChannelsOf(vaultId) {
  return VAULTREF.filter(([, v]) => v === vaultId).map(([c]) => c);
}

/** 渠道 id → 中文名(反查结果显示用)。 */
function channelName(id) {
  const c = CHANNELS.find((x) => x.id === id);
  return c ? c.name : id;
}

/**
 * 渠道卡上的「密钥来源」一行。
 *
 * 三种情形必须说清区别(裁决 4):
 *   · 引用正常 → 显示引用的条目 + 尾号,给「改回手填」
 *   · 断链     → 说"引用的条目已删除",**不能**含糊成"未配置"(该去箱子看,不是去重填)
 *   · 直接填   → 什么都不用额外说,输入框本身就是来源
 */
function srcRowHtml(cid) {
  const s = KEYSRC[cid];
  if (!s || s.mode === "inline" || s.mode === "none") return vaultPickHtml(cid);
  if (s.mode === "dangling") {
    return `<div class="krow warn"><span class="k1" style="flex:1">引用的密钥箱条目已删除
      <span class="desc">去密钥箱看看,或改回在这里直接填</span></span>
      <button class="btn s" data-act="unvault" data-id="${esc(cid)}">改回手填</button></div>`;
  }
  // vault:把"用的是哪一条"直接写出来,别让用户猜
  const which = s.vaultPlatform || s.vaultLabel
    ? `${esc(s.vaultPlatform || "")}${s.vaultLabel ? " / " + esc(s.vaultLabel) : ""}`
    : "箱子条目";
  return `<div class="krow"><span class="k1" style="flex:1">密钥来源
    <span class="desc">引用密钥箱 · ${which}${s.hint ? " · " + esc(s.hint) : ""}</span></span>
    <button class="btn s" data-act="unvault" data-id="${esc(cid)}">改回手填</button></div>`;
}

/** "或从密钥箱选一把"——把箱子里的条目平铺成下拉(按平台分组)。 */
function vaultPickHtml(cid) {
  if (!VAULT.length) return "";
  const opts = VAULT.map(
    (it) => `<option value="${esc(it.id)}">${esc(it.platform)} / ${esc(it.label)} · ${esc(it.hint)}</option>`
  ).join("");
  return `<div class="ccbtns">
      <select id="vref-${cid}" class="kref">${opts}</select>
      <button class="btn" data-act="usevault" data-id="${cid}" title="让这个渠道改用箱子里那把(会清掉这里手填的值)">改用这把</button>
    </div>`;
}

/** 补每张卡上的来源一行:缓存同步回填,缺失/过期才异步读(失败仍按原提示回退)。 */
async function fillKeySources(force = false) {
  paintCachedKeyMeta();
  await refreshKeyMeta(force);
}


/** 管理页里的胶囊渠道选择:点一下加入/移出轮播列表。 */
function renderPillPick() {
  const sel = (CFG && CFG.pillChannels) || [];
  $("cfgPill").innerHTML = CHANNELS.filter((c) => !c.hidden).map(
    (c) =>
      `<button class="pick${sel.includes(c.id) ? " on" : ""}" data-act="pillpick" data-id="${c.id}">${esc(c.name)}</button>`
  ).join("");
}

// ───────────── 形态切换 ─────────────
// 每种形态都是固定尺寸:拖标题栏只能移动窗口,拉不动大小。
// resizable(false) 去掉缩放宽边,min/max 双钳位兜底(即便有残留的缩放边框也拉不动)。
// 这里是「内容区」基准;窗口 = 内容 + 四周 CARD_MARGIN 透明边距(卡片阴影画在边距里,
// 见 styles.css body 的注释);实际尺寸走 formSize(),内容按字号档位 × 倍率。
const CARD_MARGIN = 10; // 逻辑 px,固定不随字号缩放
const SIZES = {
  panel: [480, 820],   // 仅作 resolveForm 白名单;真实尺寸由 panelContentSize() 决定
  compact: [380, 46],
  pill: [200, 46],
};
// 面板内容尺寸范围(u=1 时);窗口 = 内容 + 20
const clampNum = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
// 面板只有两个固定尺寸(内容逻辑 px,随字号倍率缩放)。不再支持用户拖动缩放。
// 单列:一列卡片,宽高按 4 个渠道一屏可见设计;双列:两列卡片,与单列同高。
const PANEL_SINGLE = [480, 820];
const PANEL_DOUBLE = [860, 820];
const COMPACT_MIN_W = 220;
const COMPACT_MAX_W = 580;
const COMPACT_H = 46;
let SIZE_APPLYING = false;   // 程序化 setSize 期间忽略缩放回调
let RESIZING = false;        // 保留给吸附松手判定;固定尺寸下恒为 false
let pillW = 200; // 胶囊实测内容宽(已是当前档位的最终逻辑像素),fitPill 维护

function panelLayout() {
  return CFG && CFG.panelLayout === "double" ? "double" : "single";
}
/** 当前字号档位下的面板内容尺寸(逻辑 px)。 */
function panelContentSize(layout = panelLayout()) {
  const u = fsU();
  const spec = layout === "double" ? PANEL_DOUBLE : PANEL_SINGLE;
  return [Math.round(spec[0] * u), Math.round(spec[1] * u)];
}
function compactMaxPx() {
  const u = fsU();
  const raw = Number(CFG && CFG.compactMaxW) || Math.round(380 * u);
  return clampNum(Math.round(raw), Math.round(COMPACT_MIN_W * u), Math.round(COMPACT_MAX_W * u));
}

/** 当前字号档位下某形态的**窗口**尺寸(内容 + 两侧透明边距)。 */
function formSize(form) {
  const u = fsU();
  if (form === "panel") {
    const [cw, ch] = panelContentSize();
    return [cw + CARD_MARGIN * 2, ch + CARD_MARGIN * 2];
  }
  if (form === "compact") {
    const cw = clampNum(compactW, Math.round(COMPACT_MIN_W * u), compactMaxPx());
    return [cw + CARD_MARGIN * 2, Math.round(COMPACT_H * u) + CARD_MARGIN * 2];
  }
  const base = SIZES[form] || SIZES.pill;
  const cw = form === "pill" ? pillW : Math.round(base[0] * u);
  const ch = Math.round(base[1] * u);
  return [cw + CARD_MARGIN * 2, ch + CARD_MARGIN * 2];
}

/** 从真实窗口读回内容尺寸。面板是固定尺寸,不读回;紧凑条按内容宽记账。 */
function readSizeFromWindow() {
  const cw = Math.max(1, Math.round(window.innerWidth - CARD_MARGIN * 2));
  const ch = Math.max(1, Math.round(window.innerHeight - CARD_MARGIN * 2));
  if (currentForm() === "compact") {
    const u = fsU();
    compactW = clampNum(cw, Math.round(COMPACT_MIN_W * u), Math.round(COMPACT_MAX_W * u));
  }
  return [cw, ch];
}

/** 有效宽度/高度断点:用内容 / 字号倍率,保证 lg/xl 下信息密度一致。 */
function updateSizeClass() {
  const u = fsU();
  const cw = Math.max(1, window.innerWidth - CARD_MARGIN * 2);
  const chh = Math.max(1, window.innerHeight - CARD_MARGIN * 2);
  const ew = cw / u;
  const eh = chh / u;
  const wc = ew < 380 ? "xs" : ew < 520 ? "sm" : ew < 720 ? "md" : "lg";
  const hc = eh < 560 ? "xs" : eh < 760 ? "sm" : "lg";
  if (document.body.dataset.w !== wc) document.body.dataset.w = wc;
  if (document.body.dataset.h !== hc) document.body.dataset.h = hc;
  updateLayoutUI();
}

/** 底栏与设置里的「单列 / 双列」状态同源。
 *  底栏那枚是**动作标签**(2026-11-08 用户裁定):显示"点下去会切到哪一档",
 *  单列时显示「双列」、双列时显示「单列」;当前布局由布局本身与 title 说明。
 *  设置页里的两个按钮维持"当前选中"语义(aria-pressed),不是动作。 */
function updateLayoutUI() {
  const layout = panelLayout();
  const cur = layout === "double" ? "双列" : "单列";
  const next = layout === "double" ? "单列" : "双列";
  const btn = $("lkLayout");
  if (btn) {
    btn.textContent = next;
    btn.title = `当前${cur} · 点击切换为${next}`;
  }
  const grp = $("cfgLayout");
  if (grp) {
    grp.querySelectorAll("button[data-layout]").forEach((b) => {
      b.setAttribute("aria-pressed", String(b.dataset.layout === layout));
    });
  }
  document.body.classList.toggle("layout-single", layout === "single");
  document.body.classList.toggle("layout-double", layout === "double");
}

/** 切到指定的固定布局。先落盘再改窗口,失败时 saveCfg 会回滚并重画按钮。 */
async function setPanelLayout(layout) {
  const next = layout === "double" ? "double" : "single";
  if (panelLayout() === next) {
    updateLayoutUI();
    return;
  }
  CFG.panelLayout = next;
  updateLayoutUI();
  const saved = await saveCfg("窗口布局");
  if (!saved) updateLayoutUI();
  if (currentForm() === "panel") await applyForm("panel", false, true);
  render({});
}
function togglePanelLayout() {
  return setPanelLayout(panelLayout() === "double" ? "single" : "double");
}

async function savePosSoon() {
  if (DOCK.on) return;
  try {
    const pos = await appWindow.outerPosition();
    const x = pos.x, y = pos.y;
    if (CFG.winX !== x || CFG.winY !== y) {
      CFG.winX = x;
      CFG.winY = y;
      invoke("save_window_pos", { x, y }).catch(() => {});
    }
  } catch {}
}

// DPI / 显示器缩放变化后按固定尺寸重新落一次;不再有用户拖拽 resize。
if (typeof appWindow.onScaleChanged === "function") {
  appWindow.onScaleChanged(async () => {
    if (SIZE_APPLYING) return;
    await applyForm(currentForm(), false, true);
  });
}

/** 形态白名单 + 可选形态闸门:紧凑条关闭时,任何来源的 compact 都落成面板。
 *  按钮 / Esc / Ctrl+2 / 旧配置 / 以后新增的入口都经过这里,只判一次。 */
function resolveForm(form) {
  if (!SIZES[form]) return "panel"; // 老配置里的 form 可能是个已废弃的名字
  if (form === "compact" && !(CFG && CFG.compactEnabled)) return "panel";
  return form;
}

async function applyFormInner(form, remember = true, forceSize = false) {
  form = resolveForm(form);
  // 速览卡片开着时切形态/重排:先收回,否则会出现"class=pill 但窗口还是卡片高"的半截状态
  if (PEEK.open) await pillPeekHide();
  const prevForm = currentForm();
  const changed = prevForm !== form;
  // 切换到折叠形态时管理页没有意义(它的入口都在面板上),顺手关掉 ——
  // 这条路上不要把焦点还给底栏(那一刻底栏正要被 display:none 掉)。
  if (form !== "panel" && manageOpen()) closeManage(false);
  // 只换 form-* 与 view-manage 类,**保留 docked / dock-left** ——
  // 整体覆写 className 会在吸附期间抹掉竖条形态,还把 8px 窗口强拉回形态尺寸
  document.body.classList.remove("form-panel", "form-compact", "form-pill");
  document.body.classList.add("form-" + form);
  document.body.classList.toggle("view-manage", manageOpen());
  // 动效 D1:形态真的换了才给新内容一次淡入上浮(轮询重钳宽度的 applyForm 不重播)
  // 动效 I:切到面板时,行自上而下错峰入场
  if (changed) {
    document.body.classList.remove("xf-in");
    void document.body.offsetWidth;
    document.body.classList.add("xf-in");
    setTimeout(() => document.body.classList.remove("xf-in"), 260);
    if (form === "panel") playEntrance();
  }
  // 面板里行要能点开,所以禁用拖拽;紧凑条整条可拖(里面没有需要点击的东西)
  const list = $("list");
  if (list) list.setAttribute("data-tauri-drag-region", form === "compact" ? "deep" : "false");
  if (remember && CFG) {
    CFG.form = form;
    saveCfg();
  }
  // 吸附期间窗口几何归吸附逻辑管:展开态换完形态重新量一次,收起态压根不碰尺寸
  if (DOCK.on) {
    if (DOCK.open) dockLayout(true);
    return;
  }
  let [w, h] = formSize(form);
  if (form === "panel") {
    // 固定尺寸也要尊重物理屏幕:大字号档在 1080p 上会超出工作区。
    // 窗口按显示器尺寸钳制(列表自身滚动,标题栏 / 页脚始终可见)。
    const mon = await T.window.currentMonitor();
    const sf = (await appWindow.scaleFactor()) || 1;
    if (mon) {
      const maxW = Math.floor(mon.size.width / sf) - 2;
      const maxH = Math.floor(mon.size.height / sf) - 48; // 任务栏余量
      w = Math.min(w, maxW);
      h = Math.min(h, maxH);
    }
  }
  SIZE_APPLYING = true;
  try {
    // 改尺寸在 Windows 上会连带挪动窗口(被工作区/边缘约束顶开),那一发 moved
    // 不是用户拖的:不标 selfMove 的话松手判定会把它当成"拖到边缘",切个形态就自己吸附上了(报告 P1-8)
    markSelfMove(FORM_SELF_MOVE_MS);
    // 先解除上一形态的钳位,再按固定尺寸落一次。
    await appWindow.setMinSize(null);
    await appWindow.setMaxSize(null);
    // 取消自由拖动缩放:所有形态都关掉原生 resize 边框。
    await appWindow.setResizable(false);
    if (form === "panel" || form === "compact") {
      await appWindow.setSize(new T.dpi.LogicalSize(w, h));
    } else {
      await appWindow.setSize(new T.dpi.LogicalSize(w, h));
      await appWindow.setMinSize(new T.dpi.LogicalSize(w, h));
      await appWindow.setMaxSize(new T.dpi.LogicalSize(w, h));
    }
    // 收尾校验:类与尺寸必须成对落地。IPC 偶发丢失/交叠时这里兜底
    const sf = (await appWindow.scaleFactor()) || 1;
    const sz = await appWindow.outerSize();
    if (Math.abs(sz.width - w * sf) > 2 || Math.abs(sz.height - h * sf) > 2) {
      console.warn("窗口尺寸与形态不符,重设", { want: [w, h], got: [sz.width, sz.height], sf });
      await appWindow.setMinSize(null);
      await appWindow.setMaxSize(null);
      await appWindow.setSize(new T.dpi.LogicalSize(w, h));
      if (form !== "panel" && form !== "compact") {
        await appWindow.setMinSize(new T.dpi.LogicalSize(w, h));
        await appWindow.setMaxSize(new T.dpi.LogicalSize(w, h));
      }
    }
    // 紧凑条的宽度仍按内容记账;面板尺寸由布局固定,不记账。
    if (form === "compact") compactW = Math.round(w - CARD_MARGIN * 2);
    // 等一帧让 WebView 的 innerWidth/Height 跟上,再切断点/按钮。
    setTimeout(updateSizeClass, 30);
    // 视口变化后重新量一次紧凑条,决定尾巴要收几个进 "+N"
    scheduleFitCompact();
    // 形态换了,DOM 宽度也变了:Codex 倒计时的降级档要按新宽度重算
    // (紧凑条里它被 display:none 掉,那次量不到,切回面板必须重新量)
    setTimeout(fitCodexResetLabels, 30);
  } catch (e) {
    console.error("切换形态失败", e);
  } finally {
    SIZE_APPLYING = false;
  }
  // 必须 await:applyFormInner 本身在几何队列里,不加 await 就等于让队列提前认为这一环
  // 完成,而 clampToMonitor 还在拿"上一环未落地时"的尺寸算坐标、并排到队尾之后才落地
  // —— 违反下面那条"一切改窗口几何的入口都排进同一条链"的纪律(报告 P2-30)。
  await clampToMonitor();
  // 掩护窗口要从**最后一次**几何 IPC 之后重新起算:不然尾部那次 setSize
  // 触发的 moved 会落在窗口之外,被当成"用户自己拖到屏幕边"→ 挂件自己吸成竖条(P3-29)。
  // 时长用形态切换那一档:上面整条链(~12 个 await)在慢机器上会超过 600ms(F14)。
  markSelfMove(FORM_SELF_MOVE_MS);
}

// 形态/几何操作**全局串行**:applyForm 的类切换是同步的,但尺寸是 6 连 await 的 IPC——
// 并发的两次(用户点击 × 轮询引发的 fitPill 自动重排、吸附的 dockLayout)一旦交叠,
// 最终「类」来自后调用的、「尺寸」来自后落地的,就会错位成
// class=panel + 窗口 46px 高(用户看到的"折叠后变成大窗口的顶栏")。
// 一切改窗口几何的入口都排进同一条 promise 链,类与尺寸成对落地。
let geomQueue = Promise.resolve();
function enqueueGeom(fn) {
  geomQueue = geomQueue.then(fn, fn); // 上一环失败也继续排,不让队列断死
  return geomQueue;
}

function applyForm(form, remember = true, forceSize = false) {
  // 切形态前先关详情:胶囊/紧凑条没有底部弹层的落点
  if (DETAIL_ID && form !== "panel") closeDetail(false);
  return enqueueGeom(() => applyFormInner(form, remember, forceSize));
}

/** 把窗口夹进当前显示器:胶囊在屏幕角落时展开成面板会"长出"屏幕,看不全。 */
async function clampToMonitor() {
  try {
    const info = await monitorInfo();
    if (!info) return;
    if (DOCK.on && !DOCK.open) return; // 吸附收起态由吸附逻辑自己管位置
    const pos = await appWindow.outerPosition();
    const size = await appWindow.outerSize();
    const x = Math.max(info.left, Math.min(info.right - size.width, pos.x));
    const y = Math.max(info.top, Math.min(info.bottom - size.height, pos.y));
    if (x !== pos.x || y !== pos.y) {
      markSelfMove(); // 别让这次移动触发"拖到边缘"判定
      await appWindow.setPosition(new T.dpi.PhysicalPosition(x, y));
    }
  } catch (e) {
    console.error("窗口夹回屏幕失败", e);
  }
}

// ───────────── 贴边(拖到屏幕边缘自动吸附) ─────────────
// 没有按钮:把窗口拖到屏幕左/右边缘松手就吸附,收成 8×64 的纯色条(不显示任何
// 数字)。鼠标移入 150ms 后展开成吸附前的形态,移出 1 秒收回;把窗口从边缘
// 拖走即解除吸附。吸附期间强制置顶 —— 否则鼠标移过去也看不见它。
const DOCK_SIZE = [8, 64];
const DOCK_SNAP_LOGICAL = 16;   // 松手时距边缘多少逻辑像素内算"贴边"
const DOCK_SETTLE_MS = 250;     // 停止移动多久算松手(拖拽过程中不判)
const DOCK_HOVER_DELAY = 150;   // 移入意图延迟:8px 太窄,不加延迟路过就会弹开
const DOCK = {
  on: false, open: false, side: "right", y: 0,
  collapseTimer: null, hoverTimer: null, settleTimer: null,
  selfMove: false, selfMoveTimer: null,
};

const currentForm = () => (document.body.className.match(/form-(\w+)/) || [])[1] || "panel";

/** 自己的 setPosition / setSize 也会触发 moved 事件,这段时间内不判定吸附。
    ms 可调:形态切换的几何链有十来个 await,慢机器上会超过 600ms,尾部那发 moved
    就会被当成"用户拖到屏幕边"而自己吸成竖条(F14)。 */
function markSelfMove(ms = 600) {
  DOCK.selfMove = true;
  clearTimeout(DOCK.selfMoveTimer);
  DOCK.selfMoveTimer = setTimeout(() => (DOCK.selfMove = false), ms);
}

/** 长档掩护时长:形态切换与吸附布局的几何链都是多连 await,都要盖住(见 markSelfMove)。 */
const FORM_SELF_MOVE_MS = 1500;

async function monitorInfo() {
  const mon = await T.window.currentMonitor();
  const sf = await appWindow.scaleFactor();
  if (!mon) return null;
  return { mon, sf, left: mon.position.x, right: mon.position.x + mon.size.width,
    top: mon.position.y, bottom: mon.position.y + mon.size.height };
}

/** 吸附。side = right | left,y 保持当前竖直位置(夹进显示器范围)。 */
async function dockEnter(side, pos) {
  if (DOCK.on) return;
  const info = await monitorInfo();
  if (!info) return;
  DOCK.side = side;
  DOCK.y = Math.max(info.top + 8,
    Math.min(info.bottom - Math.round(DOCK_SIZE[1] * info.sf) - 8, pos ? pos.y : info.top + 120));
  DOCK.on = true;
  try {
    await appWindow.setAlwaysOnTop(true);
    await dockLayout(false);
    renderDock(); // 立刻染色,不然要等下一次轮询才有颜色
  } catch (e) {
    DOCK.on = false;
    console.error("吸附失败", e);
  }
}

/** 展开(true)/ 收起(false)。展开时贴边那一侧保持对齐,窗口不会跑到屏幕外。
    窗口含 10px 透明边距(卡片画在内部),展开时把窗口多探出一边距,
    让卡片(而不是透明边)与屏幕边缘贴齐。 */
async function dockLayoutNow(open) {
  const info = await monitorInfo();
  if (!info) return;
  const [w, h] = open ? formSize(currentForm()) : DOCK_SIZE;
  const barW = Math.round(DOCK_SIZE[0] * info.sf);
  const mgn = Math.round(CARD_MARGIN * info.sf);
  const x = DOCK.side === "right"
    ? info.right - (open ? Math.round(w * info.sf) - mgn : barW)
    : info.left - (open ? mgn : 0);
  // DOCK.y 是按 64px 竖条夹过的;展开成面板/紧凑条后同一个 y 会让窗口
  // 垂到屏幕外(报告 P1-7),按当前高度再夹一次
  const y = Math.max(info.top, Math.min(DOCK.y, Math.max(info.top, info.bottom - Math.round(h * info.sf))));
  try {
    // 吸附的几何链一样是 6 连 await:用形态切换那一档掩护,不然尾部的 moved
    // 会被当成用户拖拽 —— 而吸附态误判的后果正是"自己吸成 8px 竖条"(G2)
    markSelfMove(FORM_SELF_MOVE_MS);
    SIZE_APPLYING = true;
    await appWindow.setMinSize(null);
    await appWindow.setMaxSize(null);
    await appWindow.setResizable(false);
    await appWindow.setSize(new T.dpi.LogicalSize(w, h));
    await appWindow.setPosition(new T.dpi.PhysicalPosition(x, y));
    await appWindow.setMinSize(new T.dpi.LogicalSize(w, h));
    await appWindow.setMaxSize(new T.dpi.LogicalSize(w, h));
  } catch (e) {
    console.error("吸附尺寸切换失败", e);
  } finally {
    SIZE_APPLYING = false;
  }
  DOCK.open = open;
  document.body.classList.toggle("docked", !open);
  document.body.classList.toggle("dock-left", DOCK.side === "left");
  document.body.classList.toggle("dock-open", open);
  if (open) {
    // M6:展开内容从贴边侧滑入 + 淡入 180ms,不再瞬变"砸"出来
    document.body.classList.remove("dock-anim");
    void document.body.offsetWidth;
    document.body.classList.add("dock-anim");
    setTimeout(() => document.body.classList.remove("dock-anim"), 240);
    scheduleFitCompact();
  }
}
// 吸附的展开/收起同样走几何队列(见 enqueueGeom)
function dockLayout(open) {
  return enqueueGeom(() => dockLayoutNow(open));
}

/** 解除吸附。keepPos = 留在当前位置(拖着离开边缘时用),否则回到屏幕内可见处。 */
async function dockExitNow(keepPos) {
  if (!DOCK.on) return;
  DOCK.on = false;
  DOCK.open = false;
  document.body.classList.remove("docked", "dock-open", "dock-anim");
  const [w, h] = formSize(currentForm());
  const info = await monitorInfo();
  try {
    // 与 dockLayoutNow 同理:解除吸附也是多连 await 的几何链,用长档掩护(G2)
    markSelfMove(FORM_SELF_MOVE_MS);
    SIZE_APPLYING = true;
    await appWindow.setMinSize(null);
    await appWindow.setMaxSize(null);
    await appWindow.setResizable(false);
    const pos = await appWindow.outerPosition();
    await appWindow.setSize(new T.dpi.LogicalSize(w, h));
    if (info) {
      // 夹进屏幕:拖到边缘松开时展开不能跑到屏幕外
      const maxX = info.right - Math.round(w * info.sf);
      const maxY = info.bottom - Math.round(h * info.sf);
      const x = keepPos ? Math.max(info.left, Math.min(maxX, pos.x)) : Math.max(info.left, maxX - 24);
      const y = keepPos ? Math.max(info.top, Math.min(maxY, pos.y)) : Math.max(info.top + 60, Math.min(maxY, pos.y));
      await appWindow.setPosition(new T.dpi.PhysicalPosition(x, y));
    }
    if (currentForm() !== "panel" && currentForm() !== "compact") {
      await appWindow.setMinSize(new T.dpi.LogicalSize(w, h));
      await appWindow.setMaxSize(new T.dpi.LogicalSize(w, h));
    }
    await readSizeFromWindow();
    updateSizeClass();
  } catch (e) {
    console.error("解除吸附失败", e);
  } finally {
    SIZE_APPLYING = false;
  }
  await invoke("set_pin", { enabled: !!(CFG && CFG.alwaysOnTop) }).catch(() => {});
  render();
}
// 解除吸附也走几何队列(见 enqueueGeom)
function dockExit(keepPos) {
  return enqueueGeom(() => dockExitNow(keepPos));
}

/** 松手判定:停止移动 250ms 后看窗口是不是贴着屏幕左/右边缘。 */
function scheduleSettleCheck() {
  if (RESIZING || DOCK.selfMove) return;
  clearTimeout(DOCK.settleTimer);
  DOCK.settleTimer = setTimeout(async () => {
    if (DOCK.selfMove) return;
    setDockPreview(null); // 拖动已停:预览线使命结束(吸上与否都收掉)
    const info = await monitorInfo();
    if (!info) return;
    const pos = await appWindow.outerPosition();
    const size = await appWindow.outerSize();
    const snap = Math.round(DOCK_SNAP_LOGICAL * info.sf);
    // "拖到边"的实际情况是窗口探出屏幕外(拖动时光标能到屏幕边缘,窗口会超出去),
    // 所以贴齐和探出都算 —— 只判"恰好贴齐"会漏掉绝大多数真实拖动。
    const nearRight = pos.x + size.width >= info.right - snap;
    const nearLeft = pos.x <= info.left + snap;
    // 自由态下把落点记进配置(报告 O-6):吸附态不记,那坐标是布局算出来的。
    // 停稳 250ms 才写,一次拖拽只落一次盘,不会拖动途中反复写库。
    if (!DOCK.on) {
      if (CFG.winX !== pos.x || CFG.winY !== pos.y) {
        CFG.winX = pos.x;
        CFG.winY = pos.y;
        invoke("save_window_pos", { x: pos.x, y: pos.y }).catch(() => {});
      }
      // 开关关着就不吸附:挂件随手一拖就变竖条太意外,所以默认关闭
      if (!(CFG && CFG.dockEnabled)) return;
      if (nearRight || nearLeft) await dockEnter(nearRight ? "right" : "left", pos);
    } else {
      // 吸附中:把用户拖出来的竖直位置记下来。以前 DOCK.y 只在吸附那一刻算一次,
      // 之后上下拖动竖条,松手后一展开又弹回原处(报告 P1-7)
      const barH = Math.round(DOCK_SIZE[1] * info.sf);
      DOCK.y = Math.max(info.top, Math.min(info.bottom - barH, pos.y));
      if (!nearRight && !nearLeft) await dockExit(true); // 拖离边缘 = 解除吸附,留在松手的位置
    }
  }, DOCK_SETTLE_MS);
}

/** 移出后 0.7 秒收回,避免手一抖就收走;正在设置页里操作时不收。 */
function dockScheduleCollapse() {
  // 已经在倒计时就别重置 —— 兜底的轮询每 400ms 一次,重置会让 700ms 永远等不到
  if (DOCK.collapseTimer) return;
  DOCK.collapseTimer = setTimeout(() => {
    DOCK.collapseTimer = null;
    if (!DOCK.on || !DOCK.open) return;
    if (document.body.classList.contains("view-manage")) return;
    dockLayout(false);
  }, 1000);
}

function dockCancelCollapse() {
  if (DOCK.collapseTimer) {
    clearTimeout(DOCK.collapseTimer);
    DOCK.collapseTimer = null;
  }
}

function dockCancelHover() {
  if (DOCK.hoverTimer) {
    clearTimeout(DOCK.hoverTimer);
    DOCK.hoverTimer = null;
  }
}

/**
 * 竖条:**一段一个渠道**,顺序与面板一致(D10)。
 *
 * 以前它是一整块颜色,而且颜色跟着"看不见的胶囊轮播"每 8 秒变一次 ——
 * 用户既不知道它代表谁,也不知道它为什么在变。现在段数 = 渠道数,
 * 上下顺序 = 紧迫度顺序,陈旧的那一段加斜纹(P4 · 形状优先于颜色)。
 */
function renderDock() {
  const body = $("dockBody");
  if (!body) return;
  if (!DOCK.on) return;
  const ids = sortChannels(CHANNELS).map((c) => c.id);
  const segs = ids
    .map((id) => {
      const c = CHANNELS.find((x) => x.id === id);
      if (!c) return "";
      const r = hvRow(id);
      const stale = !!c.stale || !!(r && r.state === "stale");
      return `<i data-id="${esc(id)}" style="background:${TONE_HEX[hvTone(c)]}" class="${
        stale ? "stale" : ""
      }" title="${esc(c.name)}"></i>`;
    })
    .join("");
  // 没有渠道时退回一块中性色,而不是留一条空白
  body.innerHTML = segs || '<i style="background:#3a4152" title="移入展开"></i>';
  body.title = ids.length ? `${ids.length} 个渠道 · 移入展开` : "移入展开";
}

// ───────────── 设置与管理(四页签;渠道卡渲染见 renderChCards)─────────────
const manageOpen = () => document.body.classList.contains("view-manage");

function openManage() {
  // 详情弹层在最上层:任何入口进设置页都先把弹层收掉,不能让它盖着设置页
  if (DETAIL_ID) closeDetail(false);
  // 先挂上 view-manage:下面的 fillKeyHints 会检查 manageOpen(),
  // 类晚加一步就会让这一次读取被当成"页面没打开"而跳过。
  document.body.classList.add("view-manage");
  // 这些块只在管理页里出现,轮询 render 会因为"页面没打开"跳过它们,
  // 所以打开时主动建一次,不然第一次进来会是空的
  // 打开设置页 = 缓存失效点:用户可能刚从密钥箱/别的程序改了凭据,重读一次
  invalidateKeyMeta();
  renderChCards();
  fillKeyHints(true);
  renderPillPick();
  syncStaticControls(); // 关闭设置页时同步可能变化的状态
  renderAbout(UPD.last); // 版本号与上次检查时间
  loadCheckin();         // 签到状态(渠道卡与设置页都用)
  showTab("index");      // 默认落在索引页,31 号起不再把 7 个页签挤进 380px
  loadSyncStatus();      // 索引里的「同步到手机」需要状态摘要
  // 焦点交给页头的「← 返回」,入口与出口方向一致(鼠标点进来时 :focus-visible 不出环,
  // 看不出差别)。入口本身常驻在底栏(2026-11-08),这里只是把焦点送进新页面。
  const back = $("btnBack");
  if (back) back.focus();
}
function closeManage(restoreFocus = true) {
  hideSyncCode();
  // 关掉设置页时把过期的弹层与编辑态一起作废:否则重开设置页会直接停在
  // 几分钟前那个「写入 · 确认」弹层上,而且还能点(报告 P2-31)
  MDL.sheet = null;
  renderModelSheet();
  VAULT_EDIT = null;
  // 焦点还在管理页里(含页头返回按钮)时,关掉之后把焦点还给底栏入口;
  // 若焦点在别处(比如点了标题栏「紧凑」触发切形态),不去抢它。
  const ae = document.activeElement;
  const mgr = $("manage");
  const fromManage = !ae || ae === document.body || (mgr && mgr.contains(ae));
  document.body.classList.remove("view-manage", "manage-index");
  // restoreFocus=false 给"切形态顺手关掉管理页"那条路:底栏只在面板形态存在,
  // 切紧凑条/胶囊时还给一个 display:none 的按钮,浏览器会立刻丢给 body,等于白聚焦。
  if (restoreFocus && fromManage && document.body.classList.contains("form-panel")) {
    const k = $("lkKeys");
    if (k && k.offsetParent !== null) k.focus();
  }
}

// 模型弹层:点在遮罩空白处也能关(它既没有背景关闭、也没有焦点收拢,
// Tab 还会跑到被遮住的底层按钮上 —— 报告 P2-31)。
if ($("mdlOv")) {
  $("mdlOv").addEventListener("click", (e) => {
    if (e.target === $("mdlOv") && MDL.sheet) {
      MDL.sheet = null;
      renderModelSheet();
    }
  });
}

// ───────────── 事件绑定 ─────────────
// 键盘兜底:行头/徽标/链接都是 div 或 span,光加 tabindex 不够,
// 还要让 Enter/Space 走同一条 data-act 委托(报告 P3-19:只用键盘的人进不去这个挂件)
function keyActivate(e) {
  if (e.key !== "Enter" && e.key !== " ") return;
  const t = e.target;
  if (!t || !t.closest) return;
  // 原生控件自己会响应 Enter/Space,再点一次就是双发(开关被连点两下)
  if (t.tagName === "BUTTON" || t.tagName === "INPUT" || t.tagName === "SELECT" || t.tagName === "A") return;
  const hit = t.closest("[data-act],.rhead");
  if (!hit || hit === document.body) return;
  e.preventDefault();
  hit.click();
}

$("list").addEventListener("keydown", keyActivate);
$("manage").addEventListener("keydown", keyActivate);

// 顶部摘要:筛选临期渠道 / 跳到最先到期的行 / 打开签到设置。
$("sum").addEventListener("click", async (e) => {
  const act = e.target.closest("[data-act]");
  if (!act) return;
  const a = act.dataset.act;
  if (a === "expiry-filter") {
    if (expirySummary().total < 0.01) return;
    EXPIRY_FILTER = !EXPIRY_FILTER;
    render({});
    return;
  }
  if (a === "jump") {
    const id = act.dataset.id;
    // 从"最先到期"点进去,要看的就是它的逐笔到期 —— 直接打开同一个详情弹层
    if (id) openDetail(id, act);
    return;
  }
  if (a === "ckstripopen") {
    // 签到格 = "看签到":落签到中心页签,不再滚到「刷新与启动」里的设置区
    openCheckinCenter();
    return;
  }
  if (a === "open-diag") {
    // 诊断卡在「刷新与启动」页签里,直接滚到体检那一段
    openManage();
    showTab("run");
    focusEl($("btnDiag"));
  }
});

/** 打开设置页后把某个元素滚到视野中央并闪一下(点"最紧/看诊断"时用)。 */
function focusEl(el) {
  if (!el) return;
  requestAnimationFrame(() => {
    el.scrollIntoView({ block: "center" });
    el.classList.add("flash");
    setTimeout(() => el.classList.remove("flash"), 1150);
  });
}

// 详情弹层:点遮罩空白关闭;弹层内动作走 detailAction。
$("detailShade").addEventListener("click", async (e) => {
  if (e.target === $("detailShade")) {
    closeDetail();
    return;
  }
  const act = e.target.closest("[data-act]");
  if (!act) return;
  e.stopPropagation();
  await detailAction(act);
});

// 焦点收拢在弹层内:Tab / Shift+Tab 不逃到背后的卡片和底栏
$("detailSheet").addEventListener("keydown", (e) => {
  if (e.key !== "Tab") return;
  const sheet = $("detailSheet");
  const f = [
    ...sheet.querySelectorAll(
      'button:not([disabled]),[href],input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])'
    ),
  ].filter((el) => el.offsetParent !== null || el === document.activeElement);
  if (!f.length) return;
  const first = f[0];
  const last = f[f.length - 1];
  if (e.shiftKey && (document.activeElement === first || document.activeElement === sheet)) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && document.activeElement === last) {
    e.preventDefault();
    first.focus();
  }
});

$("list").addEventListener("click", async (e) => {
  // I2:紧凑条的 +N 徽标 —— 展开面板,滚动到第一个被藏渠道并高亮 1 秒
  const more = e.target.closest(".more");
  if (more) {
    e.stopPropagation();
    const first = more.dataset.first;
    applyForm("panel").then(() => {
      const row = first && document.querySelector(`.row[data-id="${first}"]`);
      if (row) {
        row.scrollIntoView({ block: "center" });
        row.classList.add("flash");
        setTimeout(() => row.classList.remove("flash"), 1150);
      }
    });
    return;
  }
  const act = e.target.closest("[data-act]");
  if (act) {
    e.stopPropagation();
    const id = act.dataset.id;
    const a = act.dataset.act;
    if (a === "ckstripopen") {
      if (id) {
        // 单渠道待签:落到它的渠道卡并展开 —— 「去登录 / 立即重试」那一行在卡片里。
        // 以前一律跳签到设置区,而那里只有两个开关,用户看完不知道要做什么(2026-10-23)。
        openManage();
        showTab("ch");
        requestAnimationFrame(() => {
          const card = document.querySelector(`#chList .ccard[data-id="${id}"]`);
          if (!card) return;
          cardOpen.add(id);
          card.classList.add("open");
          card.scrollIntoView({ block: "center" });
          const row = card.querySelector(".ckwrap") || card;
          row.classList.add("flash");
          setTimeout(() => row.classList.remove("flash"), 1150);
        });
        return;
      }
      // 多个渠道:落到签到中心(今日行就在最上面,能直接操作)
      openCheckinCenter();
    } else if (a === "open-diag") {
      // 大面积失败横幅上的「看诊断」:直接落到体检那一段,不用自己翻
      openManage();
      showTab("run");
      focusEl($("btnDiag"));
    } else if (a === "ckrun") {
      // N08(2026-10-21):徽标上的「可领」按钮带着 data-act="ckrun",但这个委托
      // 以前只认 ckstripopen / open-manage / open-models / range —— ckrun 落进来
      // 就 return 了,而且前面已经 stopPropagation,连展开卡片都没有,点了完全没反应。
      // 现在与设置页走同一个 ckRun(),首页就能一键领。
      if (CK_LOG) return;
      if (CK[id] && CK[id].canTry === false) {
        showToast("还没到点: " + ckHold(CK[id]), true);
        return;
      }
      await ckRun(id);
    } else if (a === "open-manage") {
      if (DETAIL_ID) closeDetail(false);
      openManage();
      const first = act.dataset.first;
      if (first) {
        showTab("ch");
        requestAnimationFrame(() => {
          const card = document.querySelector(`#chList .ccard[data-id="${first}"]`);
          if (card) {
            card.scrollIntoView({ block: "center" });
            card.classList.add("flash");
            setTimeout(() => card.classList.remove("flash"), 1150);
          }
        });
      }
    } else if (a === "open-models") {
      openManage();
      showTab("mdl");
      // 渠道 id 就是来源 id(4sapi / hapi / opencode-go)
      ensureModels(id);
    } else if (a === "sec-more") {
      // 段标题旁的「全部 N 条」:原地展开 / 收起。状态挂在 rowMore 上,
      // 轮询重画后还在。弹层开着时优先刷新弹层。
      const sec = act.dataset.sec;
      rowMore[id] = rowMore[id] || {};
      rowMore[id][sec] = !rowMore[id][sec];
      if (DETAIL_ID === id) renderDetailSheet();
      else {
        const c = CHANNELS.find((x) => x.id === id);
        const body = document.querySelector(`.rbody[data-body="${id}"]`);
        if (c && body) body.innerHTML = renderDetail(c);
        const cur = document.querySelector(`.row[data-id="${id}"]`);
        revealFailFull(cur);
      }
    }
    return;
  }

  const head = e.target.closest(".rhead");
  if (!head) return;
  // 面板里的卡片点击 = 打开单例详情弹层(紧凑条里点 chip 仍是展开面板的旧路径)
  if (!document.body.classList.contains("form-panel")) return;
  const row = head.closest(".row");
  const id = row.dataset.id;
  const c = CHANNELS.find((x) => x.id === id);
  if (!c) return;
  openDetail(id, head);

  // 金额型的「最近变动」仍按需取:先画占位,取到后只刷新当前弹层。
  if (c.kind === "amount") {
    // 已有缓存就先画缓存,后台重取;从没取到过才写「读取中…」
    if (!(changesCache[id] && changesCache[id].length)) changesState[id] = "loading";
    renderDetailSheet();
    try {
      changesCache[id] = await invoke("channel_changes", { id, days: 7 });
      changesState[id] = "done";
    } catch {
      changesCache[id] = [];
      changesState[id] = "error";
    }
    // 期间可能已切换/关闭弹层:只刷新仍是同一个 ID 的那一个
    if (DETAIL_ID === id) renderDetailSheet();
  }
});

/** 轻量提示条(替代原生 alert/底栏回执):2.6 秒自动淡出不阻塞。bad=true 走红色边框。 */
let toastTimer = null;
function showToast(msg, bad) {
  const t = $("toast");
  if (!t) return;
  t.textContent = String(msg ?? "");
  t.classList.toggle("bad", !!bad);
  t.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("show"), 2600);
}

// 顶栏固定四枚入口(刷新 / 置顶 / 隐藏 / 胶囊),不再有弹出菜单。

/** 标题栏右侧的"上次更新":取所有可见渠道里最新的 updatedAt,分钟级刷新。 */
function updateHeaderLive() {
  const el = $("tbLive");
  const ts = Math.max(0, ...CHANNELS.filter((c) => c.hasKey && !c.hidden).map((c) => c.updatedAt || 0));
  if (el) el.textContent = ts ? relTime(ts) : "";
}

$("btnR").addEventListener("click", async () => {
  // M2 真实进度:取数期间按钮持续脉冲(禁用),完成后才停 —— 动画结束 = 数据已更新,
  // 不再固定 720ms 空转导致用户重复点(报告 B6 的后续修正)
  const btn = $("btnR");
  if (btn.classList.contains("loading")) return;
  btn.classList.add("loading");
  btn.setAttribute("aria-busy", "true");
  try {
    await refresh(true);
    btn.classList.add("done");
    setTimeout(() => btn.classList.remove("done"), 800);
  } finally {
    btn.classList.remove("loading");
    btn.removeAttribute("aria-busy");
  }
  const bad = CHANNELS.filter((c) => c.hasKey && !c.hidden && !c.valid && !(c.recovery && c.recovery.phase === "retrying")).length;
  const retrying = CHANNELS.some((c) => !c.hidden && c.recovery && c.recovery.phase === "retrying");
  if (!retrying || bad) showToast(bad ? `${bad} 个渠道暂时无法更新` : "已更新 · 刚刚", bad > 0);
});
// 模型页:搜索框与「重新取价目表」按钮
// 搜索去抖:一敲就重建 160 行列表太贵(每个字符一次 innerHTML + 排序)(F15);
// MDL.q 仍然立刻更新 —— 语义不变,只是渲染晚 140ms,连打时只画最后一次
let mdlQTimer = null;
$("mdlQ").addEventListener("input", () => {
  MDL.q = $("mdlQ").value || "";
  clearTimeout(mdlQTimer);
  mdlQTimer = setTimeout(renderModels, 140);
});
$("mdlR").addEventListener("click", () => loadModels(true));

// ───────────── 标题栏按钮 ─────────────
// 标题栏没有「设置」入口(2026-10-20 收敛):它与底栏「设置与管理」调的是同一个
// openManage(),却用另一个名字,而且 5 个文字按钮在 380px 里本就挤不下 ——
// 特大字号 + 更新角标时内容宽 405px,最右的「隐藏」被挤出窗口点不到。
// 排序也不放标题栏:加了排序键会重演同一个溢出(排序在 设置 → 显示 与 渠道页 ↑↓)。

// 注意别直接写 addEventListener("click", closeManage):那样 click 的 Event 会
// 落到 closeManage(restoreFocus) 这个形参上(Event 恒为真,行为碰巧对,但语义是错的)。
$("btnBack").addEventListener("click", () => {
  if (manageOpen() && !document.body.classList.contains("manage-index")) {
    showTab("index");
    const back = $("btnBack");
    if (back) back.focus();
    return;
  }
  closeManage();
});
// 设置页:索引 → 子页。旧页签仍在 DOM 里(CSS 隐藏),外部跳转与测试继续可用。
const TAB_TITLES = {
  index: "设置与管理", ch: "渠道管理", ck: "每日签到", mdl: "模型与客户端",
  disp: "外观与显示", run: "刷新与启动", vault: "密钥箱", about: "关于与诊断",
  sync: "同步到手机", gateway: "本地模型网关",
};
const FONT_NAME = { md: "标准", lg: "大", xl: "特大" };
const SORT_NAME = {
  urgency: "紧迫度", percent: "剩余百分比", balance: "余额",
  dayUsage: "今日消耗", custom: "自定义",
};
const idxMin = (sec) => `${Math.max(1, Math.round(Number(sec || 0) / 60))} 分钟`;

/** 设置索引:每行一个分区 + 一行当前状态。只在索引页可见时重建。 */
function renderManageIndex() {
  const box = $("mIndexList");
  if (!box) return;
  const withKey = CHANNELS.filter((c) => c.hasKey && !c.hidden);
  const ck = HV && HV.checkin;
  const ckOn = !!(ck && ck.total > 0);
  const font = FONT_NAME[(CFG && CFG.fontScale) || "md"] || "标准";
  const theme = CFG && CFG.theme === "light" ? "浅色" : "深色";
  const compact = CFG && CFG.compactEnabled ? "紧凑条开" : "紧凑条关";
  const sort = SORT_NAME[(CFG && CFG.sort) || "urgency"] || "紧迫度";
  const run = CFG
    ? `面板 ${idxMin(CFG.activeIntervalSec)} · 折叠 ${idxMin(CFG.idleIntervalSec)}${
        CFG.autostart ? " · 开机自启" : ""
      }`
    : "—";
  const last = CFG && CFG.lastCheckAt;
  const about = `${VER || "—"} · 上次检查 ${
    last ? new Date(last * 1000).toLocaleDateString("zh-CN") : "从未"
  }`;
  const row = (p, icon, title, sub, cls = "") => `<button type="button" class="midx-row" data-p="${p}">
    <span class="midx-ico ${cls}"><svg aria-hidden="true"><use href="#${icon}"/></svg></span>
    <span class="midx-tx"><span class="midx-tt">${esc(title)}</span><span class="midx-ss">${esc(sub)}</span></span>
    <svg class="midx-chev" aria-hidden="true"><use href="#i-chev"/></svg></button>`;
  box.innerHTML = [
    row("ch", "i-layers", "渠道管理",
      withKey.length ? `${withKey.length} 个渠道` : "尚未配置渠道"),
    row("mdl", "i-spark", "模型与客户端", "4SAPI · Hapi · OpenCode Go", "mdl"),
    row("ck", "i-cal", "每日签到",
      ckOn ? `今日 ${ck.done}/${ck.total}${ck.done >= ck.total ? " · 已完成" : ""}` : "未开启签到", "ck"),
    row("disp", "i-display", "外观与显示", `${theme}主题 · ${font}字号 · ${compact} · 排序: ${sort}`, "disp"),
    row("run", "i-refresh", "刷新与启动", run, "run"),
    row("vault", "i-key", "密钥箱", "查看、新增与管理本机密钥", "vault"),
    row("gateway", "i-layers", "本地模型网关", "汇总模型 · 本机或局域网调用"),
    row("sync", "i-sync", "同步到手机", syncSummary(), "sync"),
    row("about", "i-info", "关于与诊断", about),
  ].join("");
}

function syncSummary() {
  const s = SYNC_STATUS;
  if (SYNC_STATUS_ERROR) return "同步设置读取失败 · 点击重试";
  if (!s) return "打开查看 · 手机只读";
  if (s.nextAllowedAt && s.nextAllowedAt > Date.now() / 1000) {
    return "限流中 · " + untilTime(s.nextAllowedAt) + "重试";
  }
  if (!s.relayId) return s.hasGithub ? "未建组 · 已连接 GitHub" : "未启用 · 手机只读";
  if (!s.hasKey) return "同步码读取失败 · 请找回旧组";
  if (!s.source) return "本机发布来源缺失 · 已暂停";
  if (!s.hasGithub) return "已暂停 · GitHub 未连接";
  if (!s.enabled) return "已暂停 · 手机只读";
  if (s.lastError) return "发布失败 · " + String(s.lastError).slice(0, 42);
  return s.lastUploadAt ? `已开启 · 上次 ${relTime(s.lastUploadAt)}` : "已开启 · 等待首次发布";
}

function untilTime(ts) {
  const delta = Math.max(0, Math.ceil(Number(ts) - Date.now() / 1000));
  if (delta < 60) return `${delta} 秒后`;
  if (delta < 3600) return `${Math.ceil(delta / 60)} 分钟后`;
  return `${Math.ceil(delta / 3600)} 小时后`;
}

function showTab(name) {
  if (name !== "sync") hideSyncCode();
  // 用户确认：进入同步页直接显示同步码和二维码；「隐藏同步码」只作为临时遮挡。
  else {
    SYNC_CODE_REVEALED = true;
    renderSync();
  }
  const btns = [...$("mTabs").children];
  const b = btns.find((x) => x.dataset.p === name);
  if (name !== "index" && !b) return;
  for (const x of btns) x.classList.toggle("on", x === b);
  document
    .querySelectorAll("#manage .panel")
    .forEach((p) => p.classList.toggle("on", p.dataset.p === name));
  document.body.classList.toggle("manage-index", name === "index");
  const mt = document.querySelector(".mhead .mt");
  if (mt) mt.textContent = TAB_TITLES[name] || "设置与管理";
  const back = $("btnBack");
  if (back) back.title = name === "index" ? "关闭设置" : "返回设置索引";
  if (name === "index") {
    renderManageIndex();
    return;
  }
  // 页签的「进入时读一次」集中在这里:索引页(mIndexList)与旧页签栏都走 showTab。
  // 以前这些只挂在 mTabs 的 click 上,而旧页签栏被 CSS 隐藏 —— 从索引点进密钥箱
  // 时列表根本没读过,画面上没有条目也没有「新增」,像坏了。
  // 签到中心:每次切进来都重读一次流水(本地 SQLite,便宜;后台可能刚签完)
  if (name === "ck") loadCheckinHistory();
  // 模型页签才去取清单:没打开就不打扰接口(价目表一天一次的手动/自动刷新)
  if (name === "mdl") ensureModels();
  // 关于页签里的「排查日志」卡:每次切进来刷新路径与大小(便宜,一次本地文件 stat)
  if (name === "about") refreshLogCard();
  // 密钥箱:每次切进来都重新读索引(列表不需要解密,读起来很便宜);
  // 重新渲染也会把已显示的明文收回
  if (name === "vault") {
    VAULT_REVEAL = {};
    clearTimeout(vaultRevealTimer);
    loadVault();
  }
  if (name === "sync") loadSyncStatus();
  if (name === "gateway") loadGateway();
}
$("mIndexList").addEventListener("click", (e) => {
  const b = e.target.closest("button[data-p]");
  if (b) showTab(b.dataset.p);
});
$("mTabs").addEventListener("click", (e) => {
  const b = e.target.closest("button[data-p]");
  if (!b) return;
  showTab(b.dataset.p);
});

// ───────────── 同步到手机 ─────────────
// 状态与子页共用 SYNC_STATUS；app.js 末尾的 bindSyncUI 绑定按钮。
// SYNC_* 错误码对用户隐藏:界面只显示中文消息,码本身保留在诊断卡与日志里(报障链路不受影响)
function syncUserMsg(e) {
  return String(e ?? "").replace(/^SYNC_[A-Z_]+\s*:\s*/, "");
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
    const status = await invoke("sync_status");
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
  const hasGithub = !!(s && s.hasGithub);
  const configured = !!(s && s.relayId);

  // GitHub 连接卡在“未建组 / 已建组”两种状态下都要可见。
  const githubReady = s && s.oauthClientConfigured;
  $("syncGithubState").textContent = hasGithub
    ? `已连接 @${s.githubLogin || "GitHub"}`
    : githubReady
      ? "未连接"
      : "OAuth App 未配置";
  $("syncGithubDot").className =
    "dot " + (hasGithub ? "ok" : githubReady ? "warn" : "mut");
  $("syncGithubMeta").textContent = hasGithub
    ? "仅 gist 权限"
    : githubReady
      ? "未授权"
      : "缺少 client_id";
  $("syncGithubHint").textContent = hasGithub
    ? "每台电脑独立授权同一个 GitHub 账号;OAuth 令牌只存 Windows 凭据管理器,不进配置、日志或同步码。"
    : githubReady
      ? "通过 GitHub 官方浏览器页面授权,无需填写 PAT。授权前不会创建 Gist、不会上传数据。"
      : "在 GitHub → Settings → Developer settings 建 OAuth App(勾选 Device Flow),把公开 Client ID 填在下面,只需一次。";
  $("syncClientIdBox").hidden = !!githubReady;
  $("syncClientIdSaveRow").hidden = !!githubReady;
  $("syncGithubConnect").hidden = hasGithub;
  $("syncGithubReauth").hidden = !hasGithub;
  $("syncGithubDisconnect").hidden = !hasGithub;
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
      : s.lastError ? syncUserMsg(s.lastError) : "";
  $("syncDot").className =
    "dot " + (!s.enabled ? "o" : s.lastError ? "b" : s.hasGithub && s.hasKey ? "" : "w");
  $("syncState").textContent = !s.hasKey ? "同步码不可用" : !s.source ? "发布来源缺失" : !s.enabled ? "已暂停" : s.lastError ? "发布失败" : "已开启";
  const parts = [];
  if (s.lastUploadAt) parts.push("上次上传 " + relTime(s.lastUploadAt));
  parts.push(s.hasKey ? "主密钥在本机" : "主密钥缺失");
  parts.push(s.hasGithub ? "GitHub 已授权" : "GitHub 未连接");
  if (s.pendingSeq) parts.push(`待重试(第 ${s.pendingSeq} 批)`);
  if (s.nextAllowedAt && s.nextAllowedAt > Date.now() / 1000) {
    parts.push("限流至 " + untilTime(s.nextAllowedAt));
  }
  $("syncMeta").textContent = parts.join(" · ");
  $("syncRelayHint").textContent = "中继 " + s.relayId.slice(0, 10) + "… · 可与手机设置中的中继辅助核对";
  $("syncPublishBtn").disabled = !s.hasKey || !s.source || !s.hasGithub;

  const interval = $("syncInterval");
  if (interval) interval.value = String(s.intervalSec || 60);
  const enabled = $("syncEnabled");
  if (enabled) {
    enabled.checked = !!s.enabled;
    enabled.disabled = !s.enabled && (!s.hasKey || !s.source || !s.hasGithub);
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
            selected.has(c.id) ? "checked" : ""
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
  invoke("sync_qr_svg")
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
      await invoke("sync_create", { allowExisting: force === true });
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
    await invoke("sync_join", { code });
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
    await invoke("sync_publish_now");
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
    const snap = await invoke("sync_preview");
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
    const r = await invoke("sync_read_group");
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
    const r = await invoke("sync_rotate", { expectedRelayId });
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
  if (!SYNC_STATUS || !SYNC_STATUS.hasGithub) {
    list.textContent = "请先连接创建旧组时使用的 GitHub 账号。";
    return;
  }
  SYNC_RECOVERY_BUSY = true;
  renderSync();
  list.textContent = "正在查找并验证本机旧同步组…";
  try {
    const result = await invoke("sync_recovery_candidates");
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
  $("syncRecoveryList").textContent = SYNC_STATUS && SYNC_STATUS.hasGithub
    ? "" : "请先连接创建旧组时使用的 GitHub 账号。";
  $("syncRecovery").scrollIntoView({ block: "nearest" });
  if (SYNC_STATUS && SYNC_STATUS.hasGithub) syncScanRecovery();
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
    const result = await invoke("sync_restore_group", {
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
    await invoke("sync_forget", { expectedRelayId });
    await loadSyncStatus();
    showToast("已退出同步组");
  } catch (e) {
    showToast("退出失败: " + syncUserMsg(e), true);
  }
}

async function syncDisconnect() {
  if (!confirm("断开 GitHub 授权后,本机将不能再发布;手机仍可用同步码匿名读取。继续？")) return;
  try {
    await invoke("sync_disconnect_github");
    await loadSyncStatus();
    showToast("已断开 GitHub 授权");
  } catch (e) {
    showToast("断开失败: " + syncUserMsg(e), true);
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
    await invoke("sync_set_channels", { channels: ids });
    await loadSyncStatus();
  } catch (e) {
    showToast("保存发布渠道失败: " + syncUserMsg(e), true);
  }
}

async function syncSetEnabledFromUI(on) {
  try {
    await invoke("sync_set_enabled", { enabled: on });
    await loadSyncStatus();
  } catch (e) {
    showToast("切换失败: " + syncUserMsg(e), true);
    renderSync();
  }
}

async function syncSetIntervalFromUI(sec) {
  try {
    await invoke("sync_set_interval", { seconds: Number(sec) });
    await loadSyncStatus();
  } catch (e) {
    showToast("保存间隔失败: " + syncUserMsg(e), true);
  }
}

async function syncSetLabel(id, label) {
  try {
    await invoke("sync_set_platform_label", { channelId: id, label });
    await loadSyncStatus();
  } catch (e) {
    showToast("保存同步标签失败: " + syncUserMsg(e), true);
  }
}

// ── 桌面 GitHub Device Flow(不手填 PAT) ───────────────────────────────
async function syncGithubConnect() {
  if (SYNC_AUTH) return;
  const btn = $("syncGithubConnect");
  btn.disabled = true;
  try {
    const r = await invoke("sync_oauth_start");
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
    const r = await invoke("sync_oauth_poll");
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
  try { await invoke("sync_oauth_cancel"); } catch { /* 已经取消 */ }
  if (!silent) showToast("已取消 GitHub 授权");
}

async function syncAuthCopyOpen() {
  if (!SYNC_AUTH) return;
  try { await invoke("copy_text", { text: SYNC_AUTH.userCode }); } catch { /* 手动抄写 */ }
  try {
    await invoke("open_url", { url: SYNC_AUTH.verificationUri });
    $("syncAuthStatus").textContent = "已复制授权码。请在 GitHub 页面输入它,并确认只授予 gist 权限。";
  } catch (e) {
    showToast("打开浏览器失败:" + e, true);
  }
}

async function syncAuthOpenPage() {
  if (!SYNC_AUTH) return;
  try {
    await invoke("open_url", { url: SYNC_AUTH.verificationUri });
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
    await invoke("sync_disconnect_github");
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
    await invoke("sync_set_oauth_client_id", { clientId: id });
    $("syncClientId").value = "";
    await loadSyncStatus(); // 保存后 GitHub 卡从「OAuth App 未配置」切到「未连接」,同屏反馈足够
  } catch (e) {
    showToast("保存 Client ID 失败: " + syncUserMsg(e), true);
  }
}

async function syncOpenDevSettings() {
  try {
    await invoke("open_url", { url: "https://github.com/settings/developers" });
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
      await invoke("copy_text", { text });
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

// 置顶:窗口状态与配置一起改(见 Rust 的 set_pin),按钮与设置项共用同一个值
async function setPin(enabled) {
  try {
    await invoke("set_pin", { enabled });
    CFG.alwaysOnTop = enabled;
    applyPinUI();
  } catch (e) {
    showToast("置顶设置失败:" + e, true);
  }
}

function applyPinUI() {
  const on = !!(CFG && CFG.alwaysOnTop);
  const btn = $("btnP");
  btn.classList.toggle("on", on);
  btn.title = on ? "已置顶(点击取消)" : "窗口置顶";
  btn.setAttribute("aria-pressed", String(on));
  const dock = $("cfgDock");
  if (dock) {
    dock.classList.toggle("on", !!CFG.dockEnabled);
    dock.setAttribute("aria-checked", String(!!CFG.dockEnabled));
  }
  const sw = $("cfgPin");
  if (sw) {
    sw.classList.toggle("on", on);
    sw.setAttribute("aria-checked", String(on));
  }
}

$("btnP").addEventListener("click", () => setPin(!CFG.alwaysOnTop));
// 顶栏第四枚固定「胶囊」:紧凑条开关只留在设置里,不再在顶栏出现「紧凑」。
// 胶囊形态里顶栏是隐藏的,这里只服务"面板 → 胶囊"这一跳;▲ 仍是回面板的入口。
$("btnPill").addEventListener("click", () => {
  applyForm(document.body.className.includes("form-pill") ? "panel" : "pill");
});
// 失败必须说出来:invoke 被拒时以前没有 catch,"点了没反应"用户只会以为按钮坏了(F16)
$("btnH").addEventListener("click", () =>
  invoke("window_cmd", { action: "hide" }).catch((e) => showToast("隐藏失败:" + e, true))
);
$("lkQuit").addEventListener("click", () =>
  invoke("window_cmd", { action: "quit" }).catch((e) => showToast("退出失败:" + e, true))
);
$("lkLayout").addEventListener("click", (e) => {
  e.preventDefault();
  e.stopPropagation();
  togglePanelLayout();
});
$("lkKeys").addEventListener("click", () => {
  // 入口常驻(2026-11-08):管理页里也固定显示。已在管理页时再点**不重置**,
  // 不把用户正在读的子页抖回索引页
  if (manageOpen()) return;
  openManage();
});
// 紧凑条上的两个方向:展开一级 / 再收一级
$("btnExpand").addEventListener("click", () => applyForm("panel"));
$("btnToPill").addEventListener("click", () => applyForm("pill"));
$("btnPillUp").addEventListener("click", (e) => {
  e.stopPropagation(); // 别让胶囊整体的"点哪都能展开"再触发一次
  applyForm("panel");
});
// 移入竖条 → 延迟 150ms 展开(8px 太窄,路过就弹开很打扰);移出 → 收回
$("dockBody").addEventListener("mouseenter", () => {
  dockCancelCollapse();
  if (!DOCK.on || DOCK.open) return;
  dockCancelHover();
  DOCK.hoverTimer = setTimeout(() => {
    DOCK.hoverTimer = null;
    if (DOCK.on && !DOCK.open) dockLayout(true);
  }, DOCK_HOVER_DELAY);
});
// 注意:这里**不**监听竖条自身的 mouseleave —— 展开时它被隐藏,浏览器会补发
// 一次假的 mouseleave,光标其实还在窗口里,会导致刚展开就折叠。
document.addEventListener("mouseleave", () => {
  dockCancelHover();
  if (DOCK.on && DOCK.open) dockScheduleCollapse();
});
document.addEventListener("mouseenter", dockCancelCollapse);
// 折叠只认「文档级 mouseleave」这一个信号 —— 它是真的离开了窗口才会触发。
// 不要用 body.matches(":hover") 判断:窗口会周期性重绘(轮询 render 替换 DOM),
// 之后若没有任何鼠标事件,Chromium 不会重算 hover,那份"陈旧 false"会让光标
// 明明还在窗口里也把窗口收走(实测踩过)。
// 拖动判定:窗口一移动就重新计时,停稳 250ms 后看是否贴边;
// 拖动途中(节流 100ms)维持「吸附预览线」—— 吸附阈值原本不可见,
// 有了这条线,"离多近会吸上"从玄学变成看得见(仅吸附开关开启时)
appWindow.onMoved(() => {
  scheduleSettleCheck();
  previewTick();
});
let previewAt = 0;
function setDockPreview(side) {
  document.body.classList.toggle("dock-prev-r", side === "right");
  document.body.classList.toggle("dock-prev-l", side === "left");
}
async function previewTick() {
  const now = Date.now();
  if (now - previewAt < 180) return;
  previewAt = now;
  if (!CFG || !CFG.dockEnabled || DOCK.on || DOCK.selfMove || RESIZING) return setDockPreview(null);
  try {
    const info = await monitorInfo();
    if (!info) return;
    const pos = await appWindow.outerPosition();
    const size = await appWindow.outerSize();
    // 预览线比吸附阈值(16)宽:40 逻辑像素内就亮,给用户留出反应距离
    const near = Math.round(40 * info.sf);
    const nearR = pos.x + size.width >= info.right - near;
    const nearL = pos.x <= info.left + near;
    setDockPreview(nearR ? "right" : nearL ? "left" : null);
  } catch {}
}
// I6:吸附展开态的显式收回按钮(移出鼠标 / Esc 之外的第三条出路)
$("dockClose").addEventListener("click", () => {
  if (DOCK.on && DOCK.open) dockLayout(false);
});
// 胶囊轮播。不做"悬停暂停":挂件上的鼠标经常就停在胶囊附近,
// 一暂停看起来就像轮播坏了(实测踩过);点击动作只是展开面板,内容变换无副作用。
// 折中:悬停时节奏延长 3 倍(每 3 拍才进一步),既不"像坏了"也不打断阅读。
// 也**不**在 document.hidden 时短路:窗口收进托盘时,这个轮播就是托盘数字的轮播
// 驱动(报告 O-10 建议加可见性判断,加了会让托盘停在第一个渠道上不再轮换)。
let pillHover = false, pillTick = 0;
$("pill").addEventListener("mouseenter", () => (pillHover = true));
$("pill").addEventListener("mouseleave", () => (pillHover = false));
// 轮播节奏用 setTimeout 链而不是 setInterval:停留时长要能读配置
// (`pillDwellSec`,设计附录 B:默认 12 秒,原写死 8 秒),setInterval 的间隔改不了。
function pillDwellMs() {
  const v = (CFG && CFG.pillDwellSec) || 12;
  return Math.max(4, v) * 1000;
}
let pillTimer = null;
function pillSchedule() {
  clearTimeout(pillTimer);
  pillTimer = setTimeout(function pillTickOnce() {
    if (PILL.list.length >= 2 && document.body.className.includes("form-pill")) {
      pillTick++;
      // 悬停时节奏延长 3 倍:暂停曾被误认为轮播坏了(实测踩过),所以只放慢、不暂停
      const slowed = pillHover && pillTick % 3 !== 0;
      // 告急时锁定**自动**轮播(Q3):最该被看见的那一页自动走掉,是最容易错过的一次。
      // 手动翻页不受影响 —— 锁的是"自动",不是"人"。
      const cur = PILL.list[PILL.idx];
      const cr = cur && hvRow(cur.id);
      if (!slowed && !(cr && cr.tier === 0)) {
        PILL.idx = (PILL.idx + 1) % PILL.list.length;
        renderPillFace();
      }
    }
    pillSchedule();
  }, pillDwellMs());
}
pillSchedule();
// 胶囊整块是拖动区(见 index.html),展开只走 ▲ 按钮,避免和拖动抢手势

/** 存配置 + 立刻重取一次:申请状态是后端按配置算的,不重取看不到变化。 */
async function saveCfg(what) {
  // 统一的配置提交:失败一定要说清楚,并把界面退回提交前的值(报告 P2-32)。
  // saveConfigAndRefresh 的注释早就立过这条纪律,但另外几处 set_config 用的还是
  // .catch(() => {}) —— 失败静默,重启后设置悄悄回滚。
  // 快照必须是**拷贝**:调用方都是原地改 CFG 的字段(CFG.sort = …、数组 splice),
  // 只存引用的话失败时"回滚"回去的还是改过的对象,等于没回滚。
  const backup = JSON.parse(JSON.stringify(CFG));
  try {
    await invoke("set_config", { config: CFG });
    return true;
  } catch (e) {
    showToast("保存" + (what || "设置") + "失败:" + e, true);
    CFG = backup;
    // 静态控件(下拉 / 开关)也必须退回:它们不在 render() 的重绘范围里,
    // 以前只重画列表,开关停在新状态上,重启才"变回去" —— 界面等于在骗人(F6)
    syncStaticControls();
    render();
    return false;
  }
}

async function saveConfigAndRefresh() {
  // **不许吞错**:以前这里 catch(() => {}) 一吞,保存失败时界面只会"回到原状",
  // 看起来就是"点了没反应"(用户实测:隐藏渠道点「–」恢复不动)。
  // 而且调用方都是**先改状态再保存**(隐藏渠道、申请制…),不做快照回滚就等于
  // 乐观地把失败当成功:界面上那条渠道"恢复了",重启后又冒出来(F7)。
  const cfgBackup = JSON.parse(JSON.stringify(CFG));
  // CHANNELS 上的 hidden 是被就地改的(chvis / chshowall);其余字段后端会给新视图。
  // 整份深拷贝 CHANNELS 太重,这里只存真正会被调用方改的那一个字段。
  const hidBackup = CHANNELS.map((c) => ({ c, hidden: c.hidden }));
  try {
    await invoke("set_config", { config: CFG });
  } catch (e) {
    showToast("保存设置失败:" + e, true);
    CFG = cfgBackup;
    for (const b of hidBackup) b.c.hidden = b.hidden;
    syncStaticControls();
    render();
  }
  await refresh();
}

// 数值 / 日期字段用 change(失焦或回车提交),避免每敲一个字符就写盘取数
$("manage").addEventListener("change", async (e) => {
  const el = e.target.closest("[data-act]");
  if (!el) return;
  const id = el.dataset.id;
  const a = el.dataset.act;
  const cur = CFG.claimChannels && CFG.claimChannels[id];
  if (!cur) return;
  if (a === "claimamount") {
    const v = Number(el.value);
    if (!(v > 0)) return;
    cur.amount = v;
  } else if (a === "claimdays") {
    const v = Math.round(Number(el.value));
    if (!(v > 0)) return;
    cur.minIntervalDays = v;
  } else if (a === "claimdate") {
    if (!el.value) {
      cur.manualLastAt = null;
      cur.manualSetAt = null;
    } else {
      // 按本地日期零点算:用户填的是"哪天申请的"
      const ts = Math.floor(new Date(el.value + "T00:00:00").getTime() / 1000);
      if (!Number.isFinite(ts)) return;
      cur.manualLastAt = ts;
      cur.manualSetAt = Math.floor(Date.now() / 1000);
    }
  } else {
    return;
  }
  await saveConfigAndRefresh();
});

// 渠道卡上的操作:折叠/显隐/密钥保存删除/额度申请/排序/胶囊选择,全走 data-act
$("manage").addEventListener("input", (e) => {
  if (e.target && e.target.id === "vaultQ") {
    VAULT_Q = e.target.value || "";
    renderVault();
  } else if (e.target && e.target.closest && e.target.closest("#vaultForm .vform")) {
    // 密钥箱表单:输入即记草稿。提交前的任何重绘(显示明文 / 改搜索词)都靠它回填(F4)。
    // 监听挂在常驻的 #manage 上,表单 innerHTML 被重建也不会掉。
    const val = (id) => {
      const el = $(id);
      return el ? el.value : "";
    };
    VAULT_DRAFT = {
      platform: val("vPlat"),
      label: val("vLabel"),
      key: val("vKey"),
      note: val("vNote"),
      apiEndpoint: val("vEndpoint"),
      website: val("vWebsite"),
    };
  }
});

// 密钥箱表单的键盘约定:Esc 取消,回车保存(焦点在按钮上时交给按钮自己的 click)。
// e.isComposing 必须放行 —— 中文输入法用回车确认候选词,不能顺手把表单提交了。
$("manage").addEventListener("keydown", (e) => {
  if (!e.target || !e.target.closest || !e.target.closest("#vaultForm .vform")) return;
  if (e.isComposing || e.keyCode === 229) return;
  if (e.key === "Escape") {
    e.preventDefault();
    VAULT_EDIT = null;
    VAULT_DRAFT = null;
    renderVault();
  } else if (e.key === "Enter" && !e.target.closest("button")) {
    e.preventDefault();
    vaultSave();
  }
});

$("manage").addEventListener("click", async (e) => {
  // 「?」按需说明:它不带 data-act,必须先于下面的委托处理,否则会被当成卡片折叠/行点击。
  // 展开状态在 DOM 上(aria-expanded + .on),轮询重绘渠道卡后自然复位 —— 说明是"看一次"的东西,
  // 不跨重绘保持是对的(用户已经读过了)。
  const why = e.target.closest("button.why");
  if (why) {
    e.stopPropagation();
    toggleWhy(why);
    return;
  }
  const act = e.target.closest("[data-act]");
  if (!act) return;
  const id = act.dataset.id;
  const a = act.dataset.act;
  // ── 模型页:过滤芯片 / 写入预告 / 写入 / 复制三件套 / 回滚 / 开目录 ──
  // ── 签到:开启 / 切自动 / 领一次 / 去登录 ──
  if (a === "ckfilter") {
    CK_ACTIVITY_FILTER = act.dataset.filter || "all";
    renderCkActs();
    return;
  }
  if (a === "ckhfilter") {
    // 明细表的渠道筛选:只重画流水段,不惊动别的段
    CKH_FILTER = act.dataset.filter || "all";
    renderCkHist();
    return;
  }
  if (a === "ckjump") {
    // 总览条上的分段:落到该渠道的今日行并闪一下
    const row = document.querySelector('.ckch[data-ch="' + (act.dataset.id || "") + '"]');
    if (row) {
      row.scrollIntoView({ block: "center" });
      row.classList.add("flash");
      setTimeout(() => row.classList.remove("flash"), 1150);
    }
    return;
  }
  if (a === "ckenable") {
    const on = !(CK[id] && CK[id].enabled);
    await ckSetEnabled(id, on, null);
    return;
  }
  if (a === "ckauto") {
    const on = !(CK[id] && CK[id].auto);
    await ckSetEnabled(id, null, on);
    return;
  }
  if (a === "ckrun") {
    if (CK_LOG) return;
    // 后端闸门(报告 P0-6):退避未到 / 今天已处理完时,点下去不该再发请求
    if (CK[id] && CK[id].canTry === false) {
      showToast("还没到点: " + ckHold(CK[id]), true);
      return;
    }
    await ckRun(id);
    return;
  }
  if (a === "cklogin") {
    // 凭据失效的两条指路:Trae 去开客户端;WorkBuddy 回渠道卡登录
    if (id === "workbuddy") {
      await invoke("workbuddy_login").catch((e) => showToast("打不开登录窗口:" + e, true));
    } else {
      showToast("打开一次 Trae 即可(它自己会续期)");
    }
    return;
  }
  if (a === "mdlcli") {
    await openModelSheet(act.dataset.c, id, MDL.src);
    return;
  }
  if (a === "mdlinfo") {
    await openModelInfo(id, MDL.src);
    return;
  }
  if (a === "infocopy") {
    const sheet = MDL.sheet;
    const v = infoFields(sheet)[id];
    if (!v) {
      showToast("这项没有值可复制", true);
      return;
    }
    try {
      await invoke("copy_text", { text: v });
      showToast("已复制 " + id);
    } catch (e) {
      showToast("复制失败:" + e, true);
    }
    return;
  }
  if (a === "infocopyall") {
    const f = infoFields(MDL.sheet);
    const text = Object.keys(f)
      .filter((k) => f[k] !== "" && !(k === "图片" && f[k] === "不支持"))
      .map((k) => k + ":" + f[k])
      .join("\n");
    try {
      await invoke("copy_text", { text });
      showToast("已复制(不含密钥)");
    } catch (e) {
      showToast("复制失败:" + e, true);
    }
    return;
  }
  if (a === "infokeycopy") {
    const src = (MDL.sheet && MDL.sheet.source) || MDL.src;
    try {
      const msg = await invoke("key_copy", { id: src });
      showToast(msg + " · 剪贴板对其他程序可见");
    } catch (e) {
      showToast("复制失败:" + e, true);
    }
    return;
  }
  if (a === "infokeyreveal") {
    const box = document.querySelector('.reveal-box[data-rev="info"]');
    if (!box) return;
    if (box.dataset.on === "1") {
      revealBoxHide(box); // 再点一次立即收回
      return;
    }
    const src = (MDL.sheet && MDL.sheet.source) || MDL.src;
    try {
      const key = await invoke("key_reveal", { id: src });
      // 计时器归这个盒子自己:渠道卡那边的明文不受影响(F5)
      revealBoxShow(box, key);
    } catch (e) {
      showToast("取密钥失败:" + e, true);
    }
    return;
  }
  if (a === "mdlcli2") {
    // 客户端详情里的条目:来源由它的 data-src 决定(认不出的来源归"其它")
    const src = act.dataset.src || "";
    if (!src || src === "其它" || !MDL.sources.some((x) => x.id === src)) {
      showToast("这条不是四个已知来源写的,挂件不认识它的地址", true);
      return;
    }
    await pickSource(src);
    await openModelSheet(act.dataset.c, id, src);
    return;
  }
  if (a === "mdlsrc") {
    await pickSource(id);
    return;
  }
  if (a === "mdlclistate") {
    MDL.sheet = { kind: "client", client: id };
    renderModelSheet();
    return;
  }
  if (a === "injectgo") {
    await injectGo();
    return;
  }
  if (a === "injectrm") {
    await injectRemove();
    return;
  }
  if (a === "mdlclose") {
    MDL.sheet = null;
    renderModelSheet();
    return;
  }
  if (a === "mdlcopy") {
    try {
      const msg = await invoke("copy_model_bundle", {
        model: id,
        source: MDL.sheet && MDL.sheet.source,
      });
      showToast(msg + " · 剪贴板对其他程序可见");
    } catch (err) {
      showToast("复制失败:" + err, true);
    }
    return;
  }
  if (a === "mdlrollback") {
    try {
      const msg = await invoke("inject_rollback", { backup: id });
      showToast(msg);
      MDL.sheet = null;
      loadInjects();
    } catch (err) {
      showToast("回滚失败:" + err, true);
    }
    return;
  }
  if (a === "openpath") {
    try {
      await invoke("open_path", { path: id });
    } catch (err) {
      showToast("打开失败:" + err, true);
    }
    return;
  }
  if (a === "chfold") {
    // 就地开合,不整页重建 —— 展开区里可能正填着密钥
    const card = act.closest(".ccard");
    card.classList.toggle("open");
    card.classList.contains("open") ? cardOpen.add(id) : cardOpen.delete(id);
  } else if (a === "vnew") {
    VAULT_EDIT = { platform: act.dataset.id || "", label: "", key: "", note: "", apiEndpoint: "", website: "" };
    VAULT_DRAFT = null; // 换目标就丢草稿,不然会拿上一张表单里敲的内容回填(F4)
    renderVault();
    focusVaultForm();
  } else if (a === "vqclear") {
    VAULT_Q = "";
    const q = $("vaultQ");
    if (q) q.value = "";
    renderVault();
    if (q) q.focus();
  } else if (a === "vedit") {
    const it = VAULT.find((x) => x.id === id);
    if (it && !it.defaultChannel) {
      VAULT_EDIT = { id: it.id, platform: it.platform, label: it.label, key: "", note: it.note, apiEndpoint: it.apiEndpoint || "", website: it.website || "" };
      VAULT_DRAFT = null; // 同上:改另一条时以它自己的值为准
      renderVault();
      focusVaultForm();
    }
  } else if (a === "vcancel") {
    VAULT_EDIT = null;
    VAULT_DRAFT = null;
    renderVault();
  } else if (a === "vreload") {
    await loadVault(); // 读失败后的重试入口(F3)
  } else if (a === "vsave") {
    await vaultSave();
  } else if (a === "vdel") {
    vaultDel(id);
  } else if (a === "vdel-no") {
    loadVault();
  } else if (a === "vdel-yes" || a === "vdel-force") {
    // N07(2026-10-21):删前拦截现在在**后端**,而且扫的是真实指针(不是索引)——
    // 有渠道在用时后端把清单交回来而不是直接删。这里据此弹二次确认,再带 force 重来。
    // 以前拦截在浏览器里靠索引:索引读失败就静默放行,索引不准就拦了个寂寞。
    const force = a === "vdel-force";
    try {
      const r = await invoke("vault_delete", force ? { id, force: true } : { id });
      if (r && r.deleted === false) {
        const list = r.referrers || [];
        const names = list.map(channelName).join("、");
        const strip = act.closest(".cfrm");
        if (strip) {
          strip.innerHTML =
            `<span class="err">这把正被 ${esc(names)} 使用。删掉后` +
            (list.length > 1 ? "这些渠道" : "它") +
            `就没密钥可用了(会显示「引用的条目已删除」,不会自动改用别的)。</span><br>` +
            `仍然删除?<button class="btn s danger" data-act="vdel-force" data-id="${esc(id)}">仍然删除</button>` +
            `<button class="btn s" data-act="vdel-no">取消</button>`;
        }
        return;
      }
      delete VAULT_REVEAL[id];
      showToast("已删除");
    } catch (e) {
      showToast("删除失败:" + e, true);
    }
    invalidateKeyMeta(null, "source");
    await loadVault();
    // N16:渠道卡上的「密钥来源」也要立刻变 —— 否则它还显示"正在使用这把",
    // 用户以为没删掉(以前要等下一轮轮询,最长 60 秒)。
    if (document.body.classList.contains("view-manage")) {
      renderChCards();
      fillKeySources(true);
    }
  } else if (a === "vcopy") {
    try {
      const msg = await invoke("vault_copy", { id });
      showToast(msg + " · 剪贴板对其他程序可见");
    } catch (e) {
      showToast("复制失败:" + e, true);
    }
  } else if (a === "vcopy-endpoint") {
    const item = VAULT.find((x) => x.id === id);
    const endpoint = item && (item.apiEndpoint || "").trim();
    if (!endpoint) { showToast("该密钥尚未填写调用端点", true); return; }
    try {
      await invoke("copy_text", { text: endpoint });
      showToast("调用端点已复制");
    } catch (e) {
      showToast("复制端点失败:" + e, true);
    }
  } else if (a === "vreveal") {
    if (VAULT_REVEAL[id]) {
      delete VAULT_REVEAL[id];
      clearTimeout(vaultRevealTimer);
      renderVault();
    } else {
      try {
        VAULT_REVEAL[id] = await invoke("vault_reveal", { id });
        renderVault();
        clearTimeout(vaultRevealTimer);
        // 8 秒后自动收回:明文不许留在界面上(切页签/搜索也会重绘收回)
        vaultRevealTimer = setTimeout(() => {
          VAULT_REVEAL = {};
          renderVault();
        }, 8000);
      } catch (e) {
        showToast("取密钥失败:" + e, true);
      }
    }
  } else if (a === "keycopy") {
    try {
      const msg = await invoke("key_copy", { id });
      showToast(msg + " · 剪贴板对其他程序可见");
    } catch (e) {
      showToast("复制失败:" + e, true);
    }
  } else if (a === "probe") {
    // 「测试连接」:真打一次接口,结果如实贴在按钮下面(不落快照,不改任何数据)
    const box = document.querySelector(`.prb[data-prb="${id}"]`);
    if (box) box.innerHTML = '<span class="mini">正在测试…</span>';
    try {
      const r = await invoke("probe_channel", { id });
      if (!box) return;
      if (r.ok) {
        const val =
          r.unit === "%"
            ? (r.remaining === null ? "—" : r.remaining.toFixed(0) + "%")
            : r.unit === "CNY"
            ? (r.remaining === null ? "—" : "¥" + r.remaining.toFixed(2))
            : r.remaining === null
            ? "—"
            : String(Math.round(r.remaining));
        const host = (r.urlUsed || "").split("/")[2] || "(未记录)";
        box.innerHTML = `<span class="okc">✓ 通了</span> · ${r.elapsedMs}ms · 域名 <b>${esc(
          host
        )}</b> · 读到 <b>${val}</b> · 凭据来自 ${esc(r.authLabel)}${
          r.accounts > 1 ? ` · 这个渠道用过 ${r.accounts} 把密钥` : ""
        }`;
      } else {
        box.innerHTML = `<span class="badc">✗ ${esc(r.error || "没通")}</span> · 凭据来源:${esc(
          r.authLabel
        )} —— 界面上的数字仍是本地快照`;
      }
    } catch (e) {
      if (box) box.innerHTML = `<span class="badc">✗ ${esc(String(e))}</span>`;
    }
  } else if (a === "cdel") {
    // 删除自定义渠道:走行内确认(不用原生 confirm —— 单 exe + 无边框窗口下它很丑)
    const card = act.closest(".ccard");
    const c = CHANNELS.find((x) => x.id === id);
    if (!card) return;
    if (card.dataset.confirm !== "1") {
      card.dataset.confirm = "1";
      act.textContent = "点一次确认删除";
      setTimeout(() => {
        if (card.dataset.confirm === "1") {
          card.dataset.confirm = "";
          act.textContent = "删除";
        }
      }, 4000);
      return;
    }
    try {
      const msg = await invoke("custom_remove", { id });
      showToast(msg);
      await refresh();
    } catch (e) {
      showToast("删除失败:" + e, true);
    }
  } else if (a === "keyreveal") {
    const box = document.querySelector(`.reveal-box[data-rev="${id}"]`);
    if (!box) return;
    if (box.dataset.on === "1") {
      // 再点一次立即收回(明文不留在界面上)
      revealBoxHide(box);
      return;
    }
    try {
      const key = await invoke("key_reveal", { id });
      // 计时器归这个盒子自己:模型弹层那边的明文不受影响(F5)
      revealBoxShow(box, key);
    } catch (e) {
      showToast("取密钥失败:" + e, true);
    }
  } else if (a === "chshowall") {
    CFG.hiddenChannels = [];
    for (const c of CHANNELS) c.hidden = false;
    render();
    refreshChCards();
    await saveConfigAndRefresh();
  } else if (a === "chvis") {
    const hid = CFG.hiddenChannels || (CFG.hiddenChannels = []);
    const at = hid.indexOf(id);
    const cc = CHANNELS.find((x) => x.id === id);
    // 点过眼睛 = 用户表过态。记一笔,自动隐藏从此对这个渠道失效 ——
    // 否则"点了显示、下一轮又被系统藏回去",比没有自动隐藏更让人火大。
    const ut = CFG.userTouchedChannels || (CFG.userTouchedChannels = []);
    if (!ut.includes(id)) ut.push(id);
    // 自动隐藏的渠道:眼睛是「仍然显示」。它本来就不在 hiddenChannels 里,
    // 走下面那条会把 id 塞进去 —— 那就成了"用户主动隐藏",再也不会自动恢复。
    if (cc && cc.autoHidden && cc.hidden) {
      cc.hidden = false;
      cc.autoHidden = false;
      render();
      refreshChCards();
      await saveConfigAndRefresh();
      return;
    }
    at >= 0 ? hid.splice(at, 1) : hid.push(id);
    // **先本地翻一次**:取数有 5 秒去重窗口(fetch_shared),保存后立刻重取可能拿到旧视图,
    // 卡片会原地不动 —— 那就是用户看到的"没反应"。这里先按意图画出结果,下一轮取数再对齐。
    if (cc) cc.hidden = at < 0;
    render();
    refreshChCards();
    await saveConfigAndRefresh(); // 隐藏即停止取数:重取一轮,汇总/胶囊/底栏同步
  } else if (a === "qoderweblogin") {
    await qoderWebLogin(id);
  } else if (a === "wblogin") {
    // 打开 WorkBuddy 网页登录窗。令牌由 Rust 侧从窗口导航里接住,前端只负责状态与提示,
    // 结果走 workbuddy-login 事件(成功/取消/失败都从那里回)。
    try {
      await invoke("workbuddy_login");
      WB_LOGGING = true;
      renderChCards();
      showToast("登录窗口已打开 · 完成登录后会自动关窗");
    } catch (err) {
      showToast("打不开登录窗口:" + err, true);
    }
  } else if (a === "savekey") {
    const input = $("key-" + id);
    const val = input ? input.value.trim() : "";
    if (!val) return;
    try {
      await invoke("set_key", { id, key: val });
      input.value = "";
      invalidateKeyMeta(id);
      await refresh();
      renderChCards();
      fillKeyHints(true); // 立刻回显尾号:确认"存的是哪把 key"
      // I5 成功反馈:卡片绿色左边框亮 1.4s —— 之前只有失败才提示,存没存上靠猜
      const card = document.querySelector(`.ccard[data-id="${id}"]`);
      if (card) {
        card.classList.add("saved");
        setTimeout(() => card.classList.remove("saved"), 1400);
      }
    } catch (err) {
      showToast("保存失败:" + err, true);
    }
  } else if (a === "usevault") {
    // 让渠道改用密钥箱里的某一把。后端会先写指针、再**清掉手填的值**(裁决 7)——
    // 留着就是两份真相,而且箱子条目被删后还可能被回退使用。
    const sel = $("vref-" + id);
    const vid = sel ? sel.value : "";
    if (!vid) return;
    const it = VAULT.find((x) => x.id === vid);
    try {
      await invoke("set_key_ref", { id, vaultId: vid });
      invalidateKeyMeta(id);
      await refresh();
      renderChCards();
      fillKeyHints(true);
      showToast("已改用密钥箱那把" + (it ? `(${it.platform} / ${it.label})` : "") + ",手填值已清除");
      const card = document.querySelector(`.ccard[data-id="${id}"]`);
      if (card) {
        card.classList.add("saved");
        setTimeout(() => card.classList.remove("saved"), 1400);
      }
    } catch (err) {
      showToast("改用失败:" + err, true);
    }
  } else if (a === "unvault") {
    // 取消引用,改回"直接填"。
    try {
      await invoke("clear_key_ref", { id });
      invalidateKeyMeta(id);
      await refresh();
      renderChCards();
      fillKeyHints(true);
      showToast("已改回直接填 · 现在可以在这个渠道上贴密钥了");
    } catch (err) {
      showToast("取消失败:" + err, true);
    }
  } else if (a === "delkey") {
    // I3 行内确认:原生 confirm 在无边框透明置顶窗上风格脱节又阻塞
    const card = act.closest(".ccard");
    const ch = CHANNELS.find((x) => x.id === id);
    const what = ch && ch.authSource === "web" ? "登录会话" : "密钥";
    card.querySelectorAll(".cfrm").forEach((x) => x.remove());
    const strip = document.createElement("div");
    strip.className = "cfrm";
    strip.innerHTML =
      `确认删除该渠道的${what}?余额快照会保留。` +
      (ch && ch.authSource === "web" ? "删除后需要重新登录一次才能取数(会一并清掉浏览器里的登录会话)。" : "") +
      `<button class="btn danger" data-act="delkey-yes" data-id="${esc(id)}">删除</button>` +
      `<button class="btn" data-act="delkey-no">取消</button>`;
    card.querySelector(".ccbody").appendChild(strip);
  } else if (a === "delkey-no") {
    act.closest(".cfrm").remove();
  } else if (a === "delkey-yes") {
    try {
      // delete_key 现在回一句人话(删了什么 / 浏览器会话清没清干净):照它说的显示,
      // 后端更清楚这次到底动了哪些东西(F17)。老后端返回空时退回原来的文案。
      const msg = await invoke("delete_key", { id });
      invalidateKeyMeta(id);
      await refresh();
      renderChCards();
      fillKeyHints(true);
      showToast(msg || "已删除密钥");
    } catch (err) {
      showToast("删除失败:" + err, true);
    }
  } else if (a === "claimtoggle") {
    const map = CFG.claimChannels || (CFG.claimChannels = {});
    const cur = map[id] || { enabled: false, amount: 200, minIntervalDays: 14, manualLastAt: null, manualSetAt: null };
    cur.enabled = !cur.enabled;
    map[id] = cur;
    await saveConfigAndRefresh();
    renderChCards();
  } else if (a === "claimnow") {
    const cur = CFG.claimChannels[id];
    cur.manualLastAt = Math.floor(Date.now() / 1000);
    cur.manualSetAt = cur.manualLastAt;
    await saveConfigAndRefresh();
  } else if (a === "grantlog") {
    // 展开/收起 30 天记录。第一次展开才查库(见 grantListHtml 的注释)。
    if (grantOpen.has(id)) {
      grantOpen.delete(id);
      renderChCards();
      return;
    }
    grantOpen.add(id);
    renderChCards();
    try {
      GRANTS[id] = await invoke("credit_grants", { id, days: 30 });
    } catch (e) {
      GRANTS[id] = [];
      showToast("读取积分记录失败:" + e, true);
    }
    renderChCards();
    return;
  } else if (a === "claimclear") {
    const cur = CFG.claimChannels[id];
    cur.manualLastAt = null;
    cur.manualSetAt = null;
    await saveConfigAndRefresh();
  } else if (a === "ordermove") {
    const dir = Number(act.dataset.dir);
    const ids = currentOrderIds();
    const i = ids.indexOf(id);
    const j = i + dir;
    if (i < 0 || j < 0 || j >= ids.length) return;
    [ids[i], ids[j]] = [ids[j], ids[i]];
    CFG.channelOrder = ids; // 未列入的渠道先落到数组里,顺序不会因为新增渠道而乱
    await saveCfg();
    render(); // 排序是前端算的,不用重新取数
  } else if (a === "pillpick") {
    const sel = CFG.pillChannels || (CFG.pillChannels = []);
    const at = sel.indexOf(id);
    if (at >= 0) sel.splice(at, 1);
    else sel.push(id);
    act.classList.toggle("on", at < 0);
    PILL.idx = 0;
    saveCfg();
    render(); // 胶囊内容立即跟着变
  }
});

// 设置项。第三个参数:改完立刻要重画界面的键(阈值改的是颜色,不重绘就等于没生效)
const RERENDER_KEYS = new Set(["sort", "warnPercent", "critPercent"]);
function bindSelect(id, key, cast = Number) {
  const el = $(id);
  el.addEventListener("change", () => {
    // 下拉里找不到对应选项时 value 是空串,Number('') === 0 —— 阈值变 0 会
    // 让所有渠道瞬间全红(老配置/手改 HTML 时踩过),这种变更直接忽略
    if (el.value === "") return;
    CFG[key] = cast(el.value);
    saveCfg();
    if (RERENDER_KEYS.has(key)) render();
  });
}
function bindSwitch(id, key, onChange) {
  const el = $(id);
  el.addEventListener("click", async () => {
    CFG[key] = !CFG[key];
    el.classList.toggle("on", CFG[key]);
    el.setAttribute("aria-checked", String(!!CFG[key]));
    saveCfg();
    if (onChange) {
      try {
        await onChange(CFG[key]);
      } catch (err) {
        // 系统操作失败则回滚开关与配置,不让界面骗人
        CFG[key] = !CFG[key];
        el.classList.toggle("on", CFG[key]);
        el.setAttribute("aria-checked", String(!!CFG[key]));
        saveCfg();
        showToast("操作失败:" + err, true);
      }
    }
  });
}
bindSelect("cfgActive", "activeIntervalSec");
bindSelect("cfgIdle", "idleIntervalSec");
bindSelect("cfgBackoff", "backoffIntervalSec");
bindSelect("cfgSort", "sort", String);
bindSelect("cfgWarn", "warnPercent");
bindSelect("cfgCrit", "critPercent");
// 字号:换档写 --u 并按新倍率重落固定窗口尺寸;紧凑条重新截断
$("cfgFontScale").addEventListener("change", () => {
  CFG.fontScale = $("cfgFontScale").value;
  saveCfg();
  applyFontScale();
  applyForm(currentForm(), false, true);
  updateSizeClass();
  render();
});

// 主题:即时预览 → 真实落盘;失败由 saveCfg() 回滚 CFG 并调用 syncStaticControls(含 applyTheme)
$("cfgTheme").addEventListener("click", async (e) => {
  const btn = e.target.closest("button[data-theme]");
  if (!btn) return;
  const next = btn.dataset.theme === "light" ? "light" : "dark";
  if ((CFG.theme || "dark") === next) return;
  CFG.theme = next;
  applyTheme();
  await saveCfg("主题");
  applyTheme();
});

$("cfgLayout").addEventListener("click", (e) => {
  const btn = e.target.closest("button[data-layout]");
  if (btn) setPanelLayout(btn.dataset.layout);
});
bindSwitch("cfgAutostart", "autostart", (on) => invoke("set_autostart", { enabled: on }));
bindSwitch("cfgAutoUpdate", "autoCheckUpdate");
// 托盘余额数字:切换后强制重画(绕过 trayKey 缓存)
bindSwitch("cfgTrayNum", "trayShowNumber", () => { trayKey = ""; drawTrayIcon(); });
// 告急呼吸(动效 G):全应用唯一循环动画,默认关;开关只拨 body.pulse 这个总闸
bindSwitch("cfgPulse", "alertPulse", (on) => document.body.classList.toggle("pulse", !!on));
// 单击胶囊速览(方案 C):关掉后单击无动作,▲ 仍展开面板
bindSwitch("cfgPeek", "peekOnClick");
// 跌破红线的系统通知(裁决 3A):判定全在 Rust 侧(与 UI 同一口径),这里只是开关
bindSwitch("cfgNotify", "notifyOnCrit");
// 提示音两声(裁决 8A):开的时候试听一声,让用户知道会是什么声音
bindSwitch("cfgSound", "soundOn", (on) => {
  if (on) beep("ding");
});

// ─────────── 自定义渠道表单(裁决 6A):测试通过才允许保存 ───────────
// 为什么卡在"测试"这一步:取数地址与字段路径都是用户手填的,坏配置进来会变成一个
// 天天报错的渠道 —— 测试按钮是唯一能在保存前验一次的地方(照小鲸鱼的做法)。
(function () {
  const F = {
    name: $("cName"),
    short: $("cShort"),
    color: $("cColor"),
    url: $("cUrl"),
    method: $("cMethod"),
    unit: $("cUnit"),
    path: $("cPath"),
    scale: $("cScale"),
    key: $("cKey"),
    probeOnly: $("cProbeOnly"),
    out: $("cOut"),
    test: $("cTest"),
    save: $("cSave"),
  };
  if (!F.test) return;
  let passed = false; // 这份表单测试通过了没有;任何字段一改动立刻作废
  function draft() {
    const sc = parseFloat(F.scale.value);
    return {
      id: "",
      name: F.name.value.trim(),
      short: F.short.value.trim(),
      color: F.color.value.trim(),
      url: F.url.value.trim(),
      method: F.method.value,
      body: null,
      path: F.path.value.trim(),
      scale: Number.isFinite(sc) ? sc : 0,
      unit: F.unit.value,
      probeOnly: F.probeOnly.classList.contains("on"),
      probeUrl: null,
    };
  }
  function invalidate() {
    if (!passed) return;
    passed = false;
    F.save.disabled = true;
    F.save.textContent = "测试通过后保存";
  }
  ["input", "change"].forEach((ev) =>
    [F.name, F.short, F.color, F.url, F.method, F.unit, F.path, F.scale, F.key].forEach((el) =>
      el.addEventListener(ev, invalidate)
    )
  );
  F.probeOnly.addEventListener("click", () => {
    const on = !F.probeOnly.classList.contains("on");
    F.probeOnly.classList.toggle("on", on);
    F.probeOnly.setAttribute("aria-checked", String(on));
    invalidate();
  });
  F.test.addEventListener("click", async () => {
    F.out.innerHTML = '<span class="mini">正在测试…</span>';
    F.test.disabled = true;
    try {
      // `|| {}`:命令返回 null(异常实现)时也不该把整段逻辑炸掉 —— 报"没通"就够了
      const r = (await invoke("custom_test", { ch: draft(), key: F.key.value })) || {};
      passed = !!r.ok;
      F.save.disabled = !passed;
      F.save.textContent = passed ? "保存这个渠道" : "测试通过后保存";
      if (r.ok) {
        const val =
          r.remaining === null || r.remaining === undefined ? "—" : String(r.remaining);
        F.out.innerHTML = `<span class="okc">✓ 通了</span> · ${r.elapsedMs}ms · 读到 <b>${esc(
          val
        )}</b>${esc(r.unit || "")}`;
      } else {
        F.out.innerHTML = `<span class="badc">✗ ${esc(r.error || "没通")}</span>`;
      }
    } catch (e) {
      passed = false;
      F.save.disabled = true;
      F.save.textContent = "测试通过后保存";
      F.out.innerHTML = `<span class="badc">✗ ${esc(String(e))}</span>`;
    } finally {
      F.test.disabled = false;
    }
  });
  F.save.addEventListener("click", async () => {
    if (!passed) return;
    F.save.disabled = true;
    try {
      const id = await invoke("custom_add", { ch: draft(), key: F.key.value });
      showToast("已添加自定义渠道 " + id + ",下一次取数就会带上它");
      [F.name, F.short, F.color, F.url, F.path, F.scale, F.key].forEach((el) => (el.value = ""));
      F.probeOnly.classList.remove("on");
      F.probeOnly.setAttribute("aria-checked", "false");
      F.out.innerHTML = "";
      passed = false;
      F.save.textContent = "测试通过后保存";
      await refresh(); // 立刻带出新渠道(Rust 侧已作废 5 秒缓存)
    } catch (e) {
      F.out.innerHTML = `<span class="badc">✗ ${esc(String(e))}</span>`;
      F.save.disabled = false;
    }
  });
})();
// 置顶走 set_pin:窗口与配置一起改,失败时 bindSwitch 会回滚开关
// 吸附开关:关掉时如果正吸附着,立刻解除
bindSwitch("cfgTextTone", "textTone", (on) => document.body.classList.toggle("texttone", !!on));
// 「用掉还是剩余」:百分比型看剩余还是看已用(设计 P3),四处同源
bindSwitch("cfgShowUsed", "showUsed", () => render({}));
// 余额底线:充值型渠道低于它就算"需要处理",不能因为算不出分母而缺席(D02)
$("cfgMoneyFloor").addEventListener("change", async () => {
  const v = Math.max(1, Math.round(Number($("cfgMoneyFloor").value) || 10));
  CFG.moneyFloor = v;
  $("cfgMoneyFloor").value = String(v);
  await saveConfigAndRefresh();
});
bindSwitch("cfgDock", "dockEnabled", async (on) => {
  if (!on && DOCK.on) await dockExit(true);
});
bindSwitch("cfgPin", "alwaysOnTop", async (on) => {
  await invoke("set_pin", { enabled: on });
  applyPinUI();
});
// 紧凑条开关:关掉要立刻从紧凑条退回面板。不用 bindSwitch —— 保存失败时
// saveCfg 会回滚 CFG 并重画静态控件,但 body 上的 compact-on 不归它管,
// 这里在 await 之后按**回滚后的实际值**再刷一次。
$("cfgCompact").addEventListener("click", async () => {
  if (!CFG) return;
  CFG.compactEnabled = !CFG.compactEnabled;
  applyCompactUI();
  const ok = await saveCfg("紧凑条设置");
  if (!ok) {
    applyCompactUI();
    return;
  }
  if (!CFG.compactEnabled && currentForm() === "compact") await applyForm("panel");
});

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
$("btnUpdSkip").addEventListener("click", () => {
  if (!UPD.info) return;
  // 下载/安装中不跳过:状态清了后台却还在装,下一个进度事件又把角标"复活",
  // 跳过与安装互相打架(报告 B5;与覆盖层按钮 busy 置灰的行为对齐)
  if (updBusy()) return;
  const v = UPD.info.version;
  invoke("skip_update_version", { version: v }).catch(() => {});
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


// ───────────── 本地密钥箱 ─────────────
// 设计见 docs/design/design-vault-and-hidden-restore.html 第三节(1A–7A 已批)。
// 三条要点:密文在 Windows 凭据管理器、SQLite 只存索引(列表不需要解密)、
// 明文只在「显示」那一次回界面且 8 秒自动隐;「复制」根本不回前端。
let VAULT = [];            // 索引列表(vault_list 的结果)
let VAULT_Q = "";          // 搜索词
let VAULT_SORT = "updated";
try { const saved = localStorage.getItem("vault-sort"); if (["updated", "created", "platform", "group"].includes(saved)) VAULT_SORT = saved; } catch (_) {}
const vaultSortEl = $("vaultSort");
if (vaultSortEl) { vaultSortEl.value = VAULT_SORT; vaultSortEl.addEventListener("change", () => { VAULT_SORT = vaultSortEl.value; try { localStorage.setItem("vault-sort", VAULT_SORT); } catch (_) {} renderVault(); }); }
let VAULT_EDIT = null;     // 正在编辑/新建的表单:{id?, platform, label, key, note}
let VAULT_DRAFT = null;    // 表单里已敲未存的内容:{platform,label,key,note}(见 renderVault)
let VAULT_REVEAL = {};     // id → 明文(8 秒后删)
let vaultRevealTimer = null;
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
  const list = $("vaultList");
  const form = $("vaultForm");
  if (!list || !form) return;

  // 表单(新建/编辑)。草稿优先:它比 VAULT_EDIT 新 —— 用户刚敲进去、还没点保存的
  // 平台/标签/密钥/备注,不能因为「显示明文」或改搜索词触发一次重绘就被吞掉
  // (与 #chList 的 keyDraft 同一套做法,F4)。草稿只在 vnew/vedit/vcancel/保存成功时清。
  const e = VAULT_EDIT;
  const d = VAULT_DRAFT;
  form.innerHTML = e
    ? `<div class="vform">
        <div class="vform-head"><b>${e.id ? "编辑密钥" : "新增密钥"}</b><span>${
        e.id ? "密钥留空 = 不改这把" : "保存后列表只留尾 4 位"
      }</span></div>
        <div class="vf"><span>选择平台</span><select class="sel" id="vPlatSelect"><option value="">选择平台或自行输入</option>${[...new Set([...VAULT.map(x => x.platform), ...VAULT_PLATFORM_HINT])].map(p => `<option value="${esc(p)}">${esc(p)}</option>`).join("")}</select></div>
        <div class="vf"><span>平台名称</span><input class="inp" id="vPlat" list="vPlatList" value="${esc(d ? d.platform : e.platform)}" placeholder="选择或输入新平台">
          <datalist id="vPlatList">${[...new Set([...VAULT.map(x => x.platform), ...VAULT_PLATFORM_HINT])].map((p) => `<option value="${esc(p)}">`).join("")}</datalist></div>
        <div class="vf"><span>分组</span><input class="inp" id="vLabel" value="${esc(d ? d.label : e.label)}" placeholder="如 主账号 / 工作"></div>
        <div class="vf"><span>密钥</span><input class="inp" id="vKey" type="password" autocomplete="off" value="${esc(
          d ? d.key : ""
        )}" placeholder="${e.id ? "留空 = 不改这把" : "粘贴密钥"}"></div>
        <div class="vf"><span>账号</span><input class="inp" id="vNote" value="${esc(d ? d.note : e.note)}" placeholder="选填，如邮箱或账号名"></div>
        <div class="vf"><span>调用端点</span><input class="inp" id="vEndpoint" type="url" value="${esc(d ? d.apiEndpoint : e.apiEndpoint || "")}" placeholder="选填，如 https://example.com/v1"></div>
        <div class="vf"><span>官网地址</span><input class="inp" id="vWebsite" type="url" value="${esc(d ? d.website : e.website || "")}" placeholder="选填，如 https://example.com"></div>
        <div class="vform-acts">
          <button class="btn p" data-act="vsave">保存</button>
          <button class="btn" data-act="vcancel">取消</button>
          <span class="vform-note">密钥写进 Windows 凭据管理器;按 Esc 取消,回车保存。</span>
        </div>
      </div>`
    : "";

  const platformSelect = $("vPlatSelect");
  if (platformSelect) platformSelect.onchange = () => { if (!platformSelect.value) return; const input = $("vPlat"); input.value = platformSelect.value; input.dispatchEvent(new Event("input", { bubbles: true })); };
  const qclear = $("vaultQClear");
  if (qclear) qclear.hidden = !VAULT_Q;

  const rows = vaultSorted();
  const shown = rows.length;
  const platformCount = new Set(VAULT.map(it => it.platform.toLocaleLowerCase())).size;
  const meta = $("vaultMeta");
  if (meta) {
    meta.textContent = VAULT_ERR
      ? "读取失败"
      : VAULT_Q
      ? `匹配 ${shown} 把 / 共 ${VAULT.length} 把`
      : `${VAULT.length} 把 · ${platformCount} 个平台`;
  }

  // 读失败:说清是"读不出来"并给重试,绝不画成"密钥箱是空的 + 新增一把"(F3)
  if (VAULT_ERR) {
    list.innerHTML = `<div class="empty"><div class="ek"><svg aria-hidden="true"><use href="#i-key"/></svg></div>
      <b class="badc">读不出密钥箱</b>
      <span>${esc(VAULT_ERR)}</span>
      <div><button class="btn p" data-act="vreload">重试</button></div>
      <div class="note">列表(以及里面的密钥)并没有丢,只是这一次没读出来</div></div>`;
    return;
  }

  if (!VAULT.length) {
    list.innerHTML = `<div class="empty"><div class="ek"><svg aria-hidden="true"><use href="#i-key"/></svg></div>
      <b>密钥箱还是空的</b>
      <span>把常用平台的密钥存进来,换机或重填时不用再翻聊天记录。</span>
      <div><button class="btn p" data-act="vnew">新增一把密钥</button></div></div>`;
    return;
  }
  if (!rows.length) {
    list.innerHTML = `<div class="empty"><div class="ek"><svg aria-hidden="true"><use href="#i-search"/></svg></div>
      <b>没有匹配的条目</b>
      <span>「${esc(VAULT_Q)}」没有命中平台、分组、账号、地址或尾号。</span>
      <div><button class="btn" data-act="vqclear">清除搜索</button></div></div>`;
    return;
  }
  list.innerHTML = '<div class="vgroup">' + rows.map(it => {
    const plain = VAULT_REVEAL[it.id];
    const refs = refChannelsOf(it.id);
    const created = new Date(it.createdAt * 1000).toLocaleDateString("zh-CN");
    const updated = it.updatedAt !== it.createdAt ? new Date(it.updatedAt * 1000).toLocaleDateString("zh-CN") + " 更新" : "";
    const unavailable = !!it.defaultChannel && it.defaultAvailable === false;
    return `<div class="vitem"><div class="vrow" data-id="${esc(it.id)}"><div class="vrow-main">
      <div class="vrow-t"><b>${esc(it.platform)} · ${esc(it.label)}</b><span class="vrow-hint">${unavailable ? "渠道未配置密钥" : esc(it.hint)}</span></div>
      ${it.defaultChannel ? '<div class="vrow-sub">渠道默认密钥 · 只读，请在渠道管理中修改</div>' : ""}
      ${it.note ? `<div class="vrow-sub">账号：${esc(it.note)}</div>` : ""}
      ${it.apiEndpoint ? `<div class="vrow-sub vrow-address" title="${esc(it.apiEndpoint)}"><span>调用端点</span><span class="vrow-address-value">${esc(it.apiEndpoint)}</span></div>` : ""}
      ${it.website ? `<div class="vrow-sub vrow-address" title="${esc(it.website)}"><span>官网</span><span class="vrow-address-value">${esc(it.website)}</span></div>` : ""}
      <div class="vrow-sub vrow-meta"><span>${created} 创建${updated ? " · " + updated : ""}</span>${refs.length ? `<span class="vrow-ref on">正被 ${esc(refs.map(c => channelName(c)).join("、"))} 使用</span>` : ""}</div>
      </div><div class="vrow-acts">
      <button class="btn s" data-act="vcopy" data-id="${esc(it.id)}" ${unavailable ? "disabled" : ""} title="复制密钥">复制密钥</button>
      <button class="btn s" data-act="vcopy-endpoint" data-id="${esc(it.id)}" ${it.apiEndpoint ? "" : "disabled"} title="${it.apiEndpoint ? "复制调用端点" : "尚未填写调用端点"}">复制端点</button>
      <button class="btn s" data-act="vreveal" data-id="${esc(it.id)}" ${unavailable ? "disabled" : ""} title="显示明文，8 秒后隐藏">${plain ? "收起" : "显示"}</button>
      ${it.defaultChannel ? "" : `<button class="btn s" data-act="vedit" data-id="${esc(it.id)}">改</button><button class="btn s danger" data-act="vdel" data-id="${esc(it.id)}">删</button>`}
      </div></div>${plain ? `<div class="reveal vrow-reveal">${esc(plain)}</div>` : ""}</div>`;
  }).join("") + '</div>';

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
    VAULT_EDIT = null;
    VAULT_DRAFT = null; // 存进去了就不再是草稿,免得下次开表单又被回填(F4)
    // 条目名/尾号变了 → 引用它的渠道卡"密钥来源"要重读
    invalidateKeyMeta(null, "source");
    showToast(e.id ? "已更新" : "已存进密钥箱");
    await loadVault();
    if (saved && saved.id) {
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

// ───────────── 每日签到(Trae / WorkBuddy)─────────────
// 设计见 docs/design/design-auto-checkin-2026-10-13.html。三条不许破的规矩:
//   ① 活动监测与自动领取分开控制;关闭监测就一个请求都不发;
//   ② **不把"点了按钮"说成"领到了"**:数额只显示平台自己给的字段,拿不到就不写数字;
//   ③ 签到状态**不参与余额渲染** —— 签到失败永远不可能让余额显示成 0。
let QODER_LOGGING = false; // 「登录网页版」正在跑(逐包到期会话,qoderweb-login 事件收尾)

/**
 * 打开 Qoder 网页登录窗(逐包到期明细用)。设置页卡片与详情弹层共用这一个入口。
 * 结果走 qoderweb-login 事件:登录窗自动关,前端只管按钮与提示。
 * **必须带 id**:国际版与国内版是两套账号,后端据此选站点/Cookie 域/会话键。
 */
async function qoderWebLogin(id) {
  try {
    await invoke("qoderweb_login", { id });
    QODER_LOGGING = true;
    renderChCards();
    if (DETAIL_ID) renderDetailSheet();
    showToast("登录窗口已打开 · 完成登录后会自动关窗");
  } catch (err) {
    showToast("打不开登录窗口:" + err, true);
  }
}
let CK = {};            // 渠道 id → checkin_state 的那一条
let CK_LOG = false;     // 「领取」正在跑
let CK_ACTIVITY_FILTER = "all";
const CHECKIN_ON = new Set(["trae", "workbuddy", "qoder", "qoder-cn"]);
const CHECKIN_QODER = new Set(["qoder", "qoder-cn"]);

/** 退避没到 / 今天已处理完:按钮留着但点不动,并写明还要等多久(报告 P0-6)。 */
function ckHold(s) {
  const left = s && s.nextTryAt ? Math.ceil((s.nextTryAt * 1000 - Date.now()) / 1000) : 0;
  if (left > 0) return (left >= 60 ? Math.round(left / 60) + " 分钟" : left + " 秒") + "后再试";
  return "今天已处理";
}

/** 各渠道的跨日时刻(与 core::checkin::CheckinDef::day_roll_hour 对应)。
 *  Qoder 是 10:00 —— 官方活动描述原文「每日 10:00（UTC+8）刷新」。 */
const CK_ROLL = { trae: 0, workbuddy: 0, qoder: 10, "qoder-cn": 10 };

/**
 * claim 之后的余额到账核对(**纯证据**,设计见 docs/design/design-arrival-confirm-2026-10-24.html)。
 *
 * 判定永远以平台回查为准(签到红线②),所以这里只有「确认得更实」的说法,
 * **没有「确认失败」**—— 取数间隔里的正常消耗会把 +100 压成负增量
 * (grants.rs 已论证过),见不到增量只能说"未见",明说"不代表没领到"。
 * 增量按纪律带「按余额推算」标注,不当平台事实说。
 *
 * 返回一句话(不带分隔符);后端还没给核对单(老版本 / 没取到基线)返回 null,
 * 调用方落回旧文案。
 */
function ckArrivalNote(s) {
  const a = s && s.arrival;
  if (!a) return null;
  if (a.status === "confirmed" && a.delta !== null && a.delta !== undefined) {
    const d = "+" + points(a.delta);
    const match =
      s.credits !== null &&
      s.credits !== undefined &&
      Math.abs(a.delta - s.credits) <= 5;
    return match
      ? "余额已确认到账(增量 " + d + ",与平台数额吻合)"
      : "余额已见增量 " + d + "(按余额推算)";
  }
  if (a.status === "pending") return "正在核对到账(随下一轮取数更新)";
  if (a.status === "unseen") return "余额未见增量(可能被消耗抵消,不代表没领到)";
  return null;
}

/**
 * 状态 → 一行字 + 一个按钮。八种状态照设计稿 7.2 的表。
 *
 * ⚠ **数额可能没有**(2026-10-21 真机):Trae / WorkBuddy 的签到接口常常**不返回**
 * 领到多少分(`credits` 为 null,本机日志 10-20 与 10-21 都是)。以前文案写死成
 * `"今日已领" + n + " 分"`,n 为空时就成了「今日已领 分」—— 悬空一个"分",
 * 用户看到的正是"显示不出领了多少分"。
 *
 * 规矩:有数额就报数额;**没有就明说"平台未返回数额"**,绝不留一个洞。
 * 这与签到红线一致 —— 数额只信平台给的字段,拿不到就不编。
 */
function ckText(s) {
  if (!s) return null;
  const hasN = s.credits !== null && s.credits !== undefined;
  const n = hasN ? " +" + s.credits : "";
  const amt = hasN ? " +" + s.credits + " 分" : "";
  const o = s.outcome || "";
  const activityTitle =
    (s.latestActivity && s.latestActivity.title) ||
    (o.startsWith("manual:") ? o.slice("manual:".length) : "");
  // 后端把"现在到底能不能点"算好了(can_try = 开关开 && 今天没完结 && 不在退避中)。
  // 以前前端不读它,退避中连点 N 次 = 对平台发 N×3 个请求,越限流撞得越凶。
  const canRun = s.canTry !== false;
  const run = (label, cls) => (canRun ? { btn: label, act: "ckrun", cls } : { btn: ckHold(s), disabled: true });
  if (!s.enabled) {
    // 关着也要把最近一次已知状态带上:上一轮手动验过/刚关掉的人,不该看不到"其实今天已领"
    // N18(2026-10-21):这里以前写死 " · 今天已领" + n + " 分",n 为空就成「今天已领 分」——
    // 与 0.1.52 修的是同一个洞,但当时只改了 claimed / already 两条,漏了这一条。
    const tail =
      o.startsWith("claimed") || o.startsWith("already")
        ? hasN
          ? " · 已领到" + amt
          : " · 已签到(数额未知)"
        : "";
    return { text: "活动监测未开启(关闭时不会发请求)" + tail, btn: "开启监测", act: "ckenable" };
  }
  if (s.note) return { text: s.note, btn: "去登录", act: "cklogin", warn: true };
  const activityState = s.activityState || {};
  if (activityState.status === "device_used") {
    return { text: "本设备名额已用" };
  }
  if (activityState.status === "claimable") {
    const scope = activityState.scope === "device" ? "本设备" : "当前账号";
    return Object.assign({ text: "发现可领取活动 · " + scope }, run("领取", "p"));
  }
  if (o.startsWith("confirming")) {
    return Object.assign(
      { text: s.lastMessage || "平台已收到领取请求,正在确认状态" },
      run("重新确认", "")
    );
  }
  // 连续天数只有 Qoder 的接口给(streak_days),有就带上 —— 比现有两个渠道多一个可展示项
  const streak = s.streakDays ? " · 连续 " + s.streakDays + " 天" : "";
  if (o.startsWith("claimed")) {
    // 到账核对:余额见着增量就说一声;没见着也不谎报失败(消耗可能把发放抵消掉)
    const an = ckArrivalNote(s);
    return {
      text:
        (hasN ? "已领到" + amt : "已签到(数额未知)") +
        streak +
        (an ? " · " + an : " · 已触发到账核对"),
    };
  }
  if (o.startsWith("already"))
    return {
      text: (hasN ? "已领到" + amt : "已签到(数额未知)") +
        streak + "(平台侧显示已签到)",
    };
  if (activityState.status === "manual" && (!o || o.startsWith("inactive"))) {
    return { text: "有活动:" + (activityState.title || activityTitle || "客户端活动") + " · 需在客户端参与" };
  }
  if (activityState.status === "inactive" && !o) {
    return { text: "本期没有可签的活动,等下一期" };
  }
  if (activityState.status === "claimed" && !o) {
    return { text: "平台已签" };
  }
  if (activityState.status === "unknown" && !o) {
    return { text: "活动状态暂时无法确认" };
  }
  if (o === "retry:ready") return Object.assign({ text: "今日可领" + n }, run("领取", "p"));
  // Qoder 的可领短码形如 retry:ready:{活动id} —— 同样落进"可领"
  if (o.startsWith("retry:ready:")) return Object.assign({ text: "今日可领" + n }, run("领取", "p"));
  if (o === "retry:device")
    return Object.assign(
      { text: "平台太挤 · 今天还会自动再试(第 " + (s.attempts || 1) + "/5 次)" },
      run("重试", "")
    );
  if (o.startsWith("retry"))
    return Object.assign(
      { text: "上次没成功,今天还会再试(第 " + (s.attempts || 1) + "/5 次)" },
      run("重试", "")
    );
  if (o.startsWith("auth")) return { text: "凭据失效 · 打开一次客户端,或在渠道卡登录", btn: "去登录", act: "cklogin", warn: true };
  // `recheck` = 上次那条 `auth` 是**旧版本**判的,而凭据链路可能已经修好了。
  // 后端在节拍器重新判定之前会这么报,界面**不该给「去登录」** —— 那会把用户引去网页登录,
  // 而更常见的情况是桌面端凭据本来就好用(2026-10-20 真机就是这么被误导的)。
  // N12(2026-10-21):以前这里只给 text、不给 btn,于是 ckRowHtml 连按钮都不画;
  // 而且"马上会再试一次"在**自动签到关着**时是句假话 —— 什么都不会发生,
  // 这个状态会一直挂到第二天。后端在这个状态下 can_try 是 true(陈旧的 auth 不挡当天),
  // 手动点完全跑得动,界面把入口露出来就行。
  if (o === "recheck")
    return Object.assign(
      {
        text: s.auto
          ? "正在重新确认凭据…(上次的失效判定已过期,马上会再试一次)"
          : "上次的凭据失效判定已过期,但自动签到是关着的 —— 点这里手动重试一次",
      },
      run("立即重试", "")
    );
  // `no_cred` = **本机没读到凭据**(没装桌面端 / 没登录 / DPAPI 解不开),
  // 与 `auth`(平台拒绝了这份凭据)是两件事,提示的方向也不同。
  if (o.startsWith("no_cred"))
    return { text: s.lastMessage || "本机没读到签到凭据 · 打开一次客户端即可", warn: true };
  // 无活动与"已结束"分开说:前者是本期没有打卡活动(活动换季),不是失败也不是已签
  if (o.startsWith("inactive")) return { text: s.lastMessage || "本期没有可签的活动,等下一期" };
  // manual = **有活动,但只能去客户端里领**(Qoder 的 VIEW_DETAILS 类活动)。
  // 不能说成"本期无活动" —— 用户在客户端里明明看得到那一条(2026-10-22 实证)。
  if (o.startsWith("manual"))
    return {
      text: "有活动:" + (activityTitle || "客户端里可查看") + " · 领取入口在客户端",
      warn: false,
    };
  if (o.startsWith("shape")) return { text: "签到接口有变动,已暂停自动签到 · 余额显示不受影响", warn: true };
  if (o.startsWith("failed")) return Object.assign({ text: "上次没成功:" + (s.lastMessage || o.slice(7)) }, run("重试", ""));
  return Object.assign({ text: "今天还没签" }, run("领取", "p"));
}

/**
 * 首页顶部「今天还有 N 个渠道没签到」提示条。
 *
 * **只在真有未签时出现**:全部签完就消失 —— "全部已签"是噪音不是信息;
 * 签到全关时也不出现(没开的人不该被提醒一件他没要求的事)。
 *
 * 2026-10-23 真机:只有一个渠道待签时,条上写「去查看」,点进去是签到设置区
 * (两个开关 + 流水),看完不知道要做什么 —— 真正的动作在渠道卡上。所以:
 *   · 只剩一个渠道时,把**渠道名和下一步**写进条上;能自己领就直接给「领取」;
 *   · 领不了(凭据失效 / 退避中 / 接口变动)就写清原因,落点改成**渠道卡的那一行**;
 *   · 多个渠道时保留汇总 + 去签到区(那里现在也画出每渠道状态与按钮)。
 */
function ckPendingStrip() {
  const ids = ckActiveIds();
  if (!ids.length) return "";
  const pending = ids.filter((id) => !(CK[id] && CK[id].doneToday));
  if (!pending.length) return "";

  if (pending.length === 1) {
    const id = pending[0];
    const s = CK[id] || {};
    const c = CHANNELS.find((x) => x.id === id) || {};
    const name = s.name || c.name || id;
    const detail = (ckText(s) || {}).text || "今天还没签";
    // `no_cred`(本机没读到凭据)虽然 can_try 是 true,但点一次只会再失败一次;
    // 该做的是去渠道卡打开客户端 / 重新登录,所以不给「领取」。
    const noCred = String(s.outcome || "").startsWith("no_cred");
    if (s.canTry !== false && !noCred) {
      // 后端把"现在到底能不能点"算好了:能点就直接把动作放在首页,少一次跳转
      return `<button type="button" class="ckstrip" data-act="ckrun" data-id="${esc(id)}"
        title="${esc(detail)}">${esc(name)} 还没签 · <b>领取</b></button>`;
    }
    // 不能自己领:别画成"可领取",说清是哪种,并把落点定到渠道卡上那一行
    return `<button type="button" class="ckstrip" data-act="ckstripopen" data-id="${esc(id)}"
      title="${esc(detail)}">${esc(name)} 签到待处理:${esc(ckShortReason(s))} · <b>去处理</b></button>`;
  }

  return `<button type="button" class="ckstrip" data-act="ckstripopen"
      title="打开签到中心">今天还有 <b>${pending.length}</b> 个渠道没签到 · 去查看</button>`;
}

/** 提示条用的短原因 —— 完整说法留在 tooltip 与渠道卡里,这里只够写一行。 */
function ckShortReason(s) {
  const o = String((s && s.outcome) || "");
  if (s && s.nextTryAt && s.nextTryAt * 1000 > Date.now()) return ckHold(s);
  if (o.startsWith("auth")) return "凭据失效";
  if (o.startsWith("no_cred")) return "本机没读到凭据";
  if (o.startsWith("shape")) return "接口有变动";
  if (o === "recheck") return "正在重新确认";
  if (o.startsWith("retry")) return "等自动重试";
  return "点开看详情";
}

/**
 * 首页卡片上的签到徽标。设计见 docs/product/product-home-checkin-badge-2026-10-21.md。
 *
 * 内部有十几种 short code,但对用户只有四态:**已签 / 可领 / 待签 / 需处理**。
 * 其余细节(reason、连续天数、退避剩余)全进 tooltip。
 *
 * 三条硬规矩:
 *   ① 签到**没开启**就一个像素都不画 —— 默认关,没开的人不该看到满屏"待签";
 *   ② `CK[id]` 还没读到就**不画** —— 宁可晚 200ms,也不先给假状态再跳变
 *      (这产品被"界面显示陈旧结论"坑过一次,见 0.1.50);
 *   ③ 徽标**不参与余额渲染** —— 颜色只表达签到态,绝不影响金额与状态点。
 */
function ckBadgeHtml(c) {
  if (!CHECKIN_ON.has(c.id)) return "";
  if (c.hidden) return "";
  const s = CK[c.id];
  if (!s || !s.enabled) return "";
  const o = String(s.outcome || "");
  const n = s.credits === null || s.credits === undefined ? "" : " +" + s.credits;
  const tip = esc(ckBadgeTip(s));
  if (s.activityState && s.activityState.status === "claimable") {
    return s.canTry === false
      ? `<span class="ckb" title="${tip}">可领</span>`
      : `<button class="ckb hot" data-act="ckrun" data-id="${esc(c.id)}" title="${tip}">可领</button>`;
  }
  if (s.activityState && s.activityState.status === "device_used")
    return `<span class="ckb ok" title="${tip}">本设备已签</span>`;
  if (s.activityState && s.activityState.status === "manual")
    return `<span class="ckb info" title="${tip}">有活动</span>`;
  if (s.activityState && s.activityState.status === "inactive" && (!o || o.startsWith("inactive")))
    return `<span class="ckb ok" title="${tip}">本期无活动</span>`;
  if (s.activityState && s.activityState.status === "claimed" && !o)
    return `<span class="ckb ok" title="${tip}">平台已签</span>`;
  if (s.activityState && s.activityState.status === "unknown" && !o)
    return `<span class="ckb" title="${tip}">状态未知</span>`;
  if (o.startsWith("confirming"))
    return `<span class="ckb" title="${tip}">确认中</span>`;

  // claimed = **我们刚替他领到了** → 与 already 一样只说「已签」,数额进 tooltip。
  // 2026-11-03:以前 claimed 有数额就画「✓ +100」,而 Qoder 的领取接口每次都回数额,
  // 于是同一枚徽标在 Qoder 上是 +100、在其它渠道是「已签」—— 统一成「已签」。
  if (o.startsWith("claimed")) return `<span class="ckb ok" title="${tip}">✓ 已签</span>`;
  // already = **平台侧已签**(可能是他自己在客户端点的)→ 只说"已签",数额进 tooltip。
  // 写成 "+100" 会让人以为是我们领的 —— 签到红线第 2 条(不把点了按钮说成领到了)。
  if (o.startsWith("already")) return `<span class="ckb ok" title="${tip}">✓ 已签</span>`;
  // inactive = 本期无活动:不是"已签",但今天确实没有待办,聚合里算它一份
  if (o.startsWith("inactive")) return `<span class="ckb ok" title="${tip}">本期无活动</span>`;
  // manual = 有活动,但只能在客户端领:要让人看见"有事可做",所以不用灰色的"本期无活动"
  if (o.startsWith("manual")) return `<span class="ckb info" title="${tip}">有活动</span>`;
  if (o.startsWith("retry:ready")) {
    // 后端把"能不能点"算好了(canTry):退避中就别给一个点了没反应的按钮
    return s.canTry === false
      ? `<span class="ckb" title="${tip}">待签</span>`
      : `<button class="ckb hot" data-act="ckrun" data-id="${esc(c.id)}" title="${tip}">可领${esc(n)}</button>`;
  }
  if (o.startsWith("auth")) {
    return `<button class="ckb warn" data-act="open-manage" data-id="${esc(c.id)}" title="${tip}">需登录</button>`;
  }
  if (o.startsWith("shape")) {
    return `<button class="ckb warn" data-act="open-manage" data-id="${esc(c.id)}" title="${tip}">已暂停</button>`;
  }
  if (o.startsWith("no_cred")) {
    return `<button class="ckb" data-act="open-manage" data-id="${esc(c.id)}" title="${tip}">未就绪</button>`;
  }
  // 空 / retry:* / recheck / failed → 一律"待签"(细节在 tooltip)
  return `<span class="ckb" title="${tip}">待签</span>`;
}

/** 徽标的 tooltip:把 short code 翻成一句人话,再补上关键时间。 */
function ckBadgeTip(s) {
  const o = String(s.outcome || "");
  const bits = [];
  if (o.startsWith("claimed")) {
    bits.push("已领到" + (s.credits ? " +" + s.credits + " 分" : ""));
    const an = ckArrivalNote(s);
    if (an) bits.push(an);
  }
  else if (o.startsWith("already"))
    bits.push("平台显示今天已签到" + (s.credits ? "(数额 " + s.credits + " 分)" : ""));
  else if (o.startsWith("inactive")) bits.push(s.lastMessage || "本期没有可签的活动,等下一期");
  else if (o.startsWith("manual"))
    bits.push("有活动:" + (s.latestActivity && s.latestActivity.title ? s.latestActivity.title : o.slice("manual:".length)) + " —— 领取入口在客户端里,挂件领不了");
  else if (o.startsWith("retry:ready"))
    bits.push("今天可领" + (s.credits ? " +" + s.credits + " 分" : ""));
  else if (o.startsWith("auth")) bits.push("凭据失效 —— 打开一次客户端");
  else if (o.startsWith("no_cred")) bits.push(s.lastMessage || "本机没读到签到凭据");
  else if (o.startsWith("shape")) bits.push("签到接口有变动,已暂停自动签到(余额不受影响)");
  else if (o === "recheck") bits.push("正在重新确认凭据");
  else if (o.startsWith("retry")) bits.push("上次没成功,今天还会自动再试");
  else bits.push("今天还没签");
  if (s.streakDays) bits.push("连续 " + s.streakDays + " 天");
  if (s.latestActivity && s.latestActivity.title)
    bits.push("最近活动: " + s.latestActivity.title);
  if (s.activityState) {
    bits.push((ACTIVITY_LABELS[s.activityState.status] || ACTIVITY_LABELS.unknown) + " · " + (s.activityState.scope === "device" ? "设备级" : "账号级"));
  }
  if (s.monthCredits) bits.push("本月共 " + s.monthCredits + " 分");
  if (s.nextTryAt) {
    const left = Math.ceil((s.nextTryAt * 1000 - Date.now()) / 1000);
    if (left > 0) {
      bits.push((left >= 60 ? Math.round(left / 60) + " 分钟" : left + " 秒") + "后再试");
    }
  } else if (!s.doneToday) {
    // 还没到本渠道的跨日时刻:必须说清是几点,不能说"明天"
    // (Qoder 的"今天"从 10:00 起算 —— 说"明天"会让人以为今天没戏了)
    const roll = CK_ROLL[s.id] || 0;
    if (roll > 0 && new Date().getHours() < roll) bits.push(roll + ":00 后可签");
  }
  return bits.join(" · ");
}

// 活动状态统一标签:渠道卡行 / 签到中心 / 徽标 tooltip 三处共用,防止叫法漂移
const ACTIVITY_LABELS = {
  claimable: "可领取",
  manual: "需客户端参与",
  confirming: "领取确认中",
  claimed: "已领取",
  device_used: "本设备名额已用",
  inactive: "本期无活动",
  unknown: "暂时无法确认",
};

function ckRowHtml(id) {
  const s = CK[id];
  if (!s) return "";
  const t = ckText(s);
  if (!t) return "";
  const activity = s.activityState;
  const scopeLabel = activity && activity.scope === "device" ? "设备级" : "账号级";
  const regionLabel = activity && activity.region === "CN" ? "国内版" : activity && activity.region === "Global" ? "国际版" : "";
  const activityLine = activity && s.enabled
    ? '<div class="ckactivity" title="' + esc(activity.detail || activity.title || "") + '">' +
      "活动监测 · " + esc(activity.title || "签到状态") + " · " +
      esc(ACTIVITY_LABELS[activity.status] || ACTIVITY_LABELS.unknown) + " · " +
      esc([regionLabel, scopeLabel].filter(Boolean).join(" / ")) +
      "</div>"
    : !activity && CHECKIN_QODER.has(id) && s.latestActivity && s.latestActivity.title
      ? '<div class="ckactivity">活动监测 · ' + esc(s.latestActivity.title) + "</div>"
      : "";
  return (
    '<div class="ckrow' + (t.warn ? " warn" : "") + '" data-id="' + esc(id) + '">' +
    "<span>" + esc(t.text) + "</span>" +
    (t.btn
      ? '<button class="btn s ' + (t.cls || "") + '" data-act="' + (t.act || "") + '" data-id="' + esc(id) + '"' +
        (t.disabled ? " disabled" : "") +
        ">" + esc(t.btn) + "</button>"
      : "") +
    "</div>" + activityLine
  );
}

/** 首页的统一视图(七态 / 五桶 / 绑定窗)。
 *
 * 取数走 `fetch_shared` —— 与 `get_channels` 同一个闸门、同一份 5 秒内的近期结果,
 * 所以紧贴着调一次不会多打一次账务接口。失败时保留上一份视图,四个渲染器退回本地判断。
 */
async function loadHomeView() {
  try {
    hvApply(await invoke("home_view"));
  } catch (e) {
    // 视图拿不到不该让首页变空白:渲染器各自退回 toneOf / sortChannels
  }
}

/**
 * 读一次签到状态。**带序号**:两次并发读可能乱序返回 —— 启动时首轮 refresh 与
 * 后台签到的 `checkin-updated` 就会撞在一起,晚到的旧快照会把新结论盖回去。
 * 只让最后一次发出的请求落地(与 fetch_shared 的合流同一个道理)。
 */
let CK_SEQ = 0;
async function loadCheckin() {
  const seq = ++CK_SEQ;
  let list = null;
  try {
    list = await invoke("checkin_state");
  } catch (e) {}
  if (seq !== CK_SEQ) return; // 更晚的一次已经发出,这份结果作废
  CK = {};
  if (list) for (const x of list) CK[x.id] = x;
  ckTabUpdate();
  // 渠道卡与签到中心都跟着更新(哪个在场更新哪个)
  if (document.body.classList.contains("view-manage")) {
    renderCheckinCenter();
    document.querySelectorAll("#chList .ckwrap").forEach((el) => {
      el.innerHTML = ckRowHtml(el.dataset.id);
    });
  }
}

function activityStatusView(status) {
  return { label: ACTIVITY_LABELS[status] || ACTIVITY_LABELS.unknown, kind: status || "unknown" };
}

function renderActivityOverview() {
  const entries = [];
  const now = Math.floor(Date.now() / 1000);
  for (const id of CHECKIN_ON) {
    const state = CK[id];
    if (!state || !state.enabled) continue;
    const channel = CHANNELS.find((item) => item.id === id);
    if (channel && channel.hidden) continue;
    const region = id === "qoder" ? "国际版" : "国内版";
    const history = CHECKIN_QODER.has(id) && Array.isArray(state.activityHistory)
      ? state.activityHistory.filter((item) => state.lastActivityCheckAt
          ? item.observedAt === state.lastActivityCheckAt
          : !item.observedAt || now - item.observedAt < 24 * 3600)
      : [];
    if (history.length) {
      for (const item of history) {
        const canClaim =
          (item.actionType === "CLAIM_BENEFIT" || item.actionType === "CLAIM_BEFIT") &&
          item.claimStatus === "CLAIMABLE";
        const status = canClaim
          ? "claimable"
          : item.claimStatus === "CLAIMABLE"
            ? "manual"
            : item.claimStatus === "CLAIMED"
              ? "claimed"
              : ["EXPIRED", "ENDED"].includes(item.claimStatus)
                ? "inactive"
                : "unknown";
        entries.push({
          channel: state.name || id,
          id,
          key: item.id,
          title: item.title || "Qoder 活动",
          status,
          scope: "账号级",
          region,
          observedAt: item.observedAt,
        });
      }
      continue;
    }
    const activity = state.activityState;
    entries.push({
      channel: state.name || id,
      id,
      key: activity && activity.key ? activity.key : id,
      title: activity && activity.title ? activity.title : "等待首次状态检查",
      status: activity && activity.status ? activity.status : "unknown",
      scope: activity && activity.scope === "device" ? "设备级" : "账号级",
      region: activity && activity.region === "Global" ? "国际版" : activity && activity.region === "CN" ? "国内版" : region,
      observedAt: activity && activity.observedAt,
    });
  }
  const actionable = entries.filter((item) => ["claimable", "manual", "confirming", "unknown"].includes(item.status));
  const completed = entries.filter((item) => ["claimed", "device_used", "inactive"].includes(item.status));
  const visible = entries.filter((item) =>
    CK_ACTIVITY_FILTER === "action" ? ["claimable", "manual", "confirming", "unknown"].includes(item.status) :
    CK_ACTIVITY_FILTER === "done" ? ["claimed", "device_used", "inactive"].includes(item.status) : true
  );
  const filters = [
    ["all", "全部 " + entries.length],
    ["action", "需处理 " + actionable.length],
    ["done", "已完成 " + completed.length],
  ];
  const rows = visible.map((item) => {
    const badge = activityStatusView(item.status);
    const time = item.observedAt
      ? new Date(item.observedAt * 1000).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })
      : "尚未检查";
    const action = item.status === "claimable"
      ? '<button class="btn s p" data-act="ckrun" data-id="' + esc(item.id) + '"' +
        (CK[item.id] && CK[item.id].canTry === false ? " disabled" : "") + '>领取</button>'
      : "";
    return '<article class="ck-activity-item">' +
      '<div class="ck-activity-head"><span>' + esc(item.channel) + " · " + esc(item.region) + " / " + esc(item.scope) + '</span>' +
      '<span class="ck-activity-status ' + esc(badge.kind) + '">' + esc(badge.label) + '</span></div>' +
      '<div class="ck-activity-title" title="' + esc(item.key) + '">' + esc(item.title) + '</div>' +
      '<div class="ck-activity-foot"><span>' + esc(time) + '</span>' + action + '</div>' +
      '</article>';
  }).join("");
  return '<div class="ck-overview"><div class="ck-overview-head"><b>活动状态</b>' +
    '<span>可领取 ' + entries.filter((x) => x.status === "claimable").length +
    ' · 客户端参与 ' + entries.filter((x) => x.status === "manual").length +
    ' · 确认中 ' + entries.filter((x) => x.status === "confirming").length +
    ' · 已确认 ' + entries.filter((x) => x.status === "claimed" || x.status === "device_used").length + '</span></div>' +
    '<div class="ck-overview-filters" role="group" aria-label="筛选活动">' + filters.map(([key, label]) =>
      '<button type="button" class="ckfilter' + (CK_ACTIVITY_FILTER === key ? " on" : "") +
      '" aria-pressed="' + (CK_ACTIVITY_FILTER === key) + '" data-act="ckfilter" data-filter="' + key + '">' + esc(label) + '</button>'
    ).join("") + '</div>' +
    '<div class="ck-activity-list">' + (rows || '<div class="ck-overview-empty">当前筛选下没有活动 · 点「全部」看所有</div>') + '</div>' +
    '</div>';
}

// ───────────── 签到中心(独立页签)─────────────
// 设计见 docs/product/product-checkin-center-2026-10-29.md。五段:今日总览 + 逐渠道行 /
// 活动状态 / 近 30 天日历带 / 明细流水(90 天)/ 管理开关。首页签到格与多渠道待签
// 提示条都落到这里(格子 = 看,提示条 = 办,职责分开)。
const CK_CENTER_OPEN = new Set(); // 刷新状态时保留用户展开的分区
let CKH = null;          // checkin_history 的返回(日历带 + 明细表)
let CKH_SEQ = 0;         // 与 loadCheckin 同一个道理:晚到的旧快照不许盖新的
let CKH_FILTER = "all";  // 明细表的渠道筛选
const CKH_DAYS = 90;     // 明细表窗口(后端 clamp 到 7~365)
const CKH_BAND = 30;     // 日历带窗口(天)
const CKH_TABLE_CAP = 120; // 明细表最多画多少行 —— 90 天 × 每小时重试能把表撑到上千行

/** 流水短码 → 结果字。与 ckText 同一口径:already 绝不说成"领到"。 */
function ckOutcomeLabel(o) {
  o = String(o || "");
  if (o.startsWith("claimed")) return "领到";
  if (o.startsWith("already")) return "平台已签";
  if (o.startsWith("inactive")) return "无活动";
  if (o.startsWith("manual")) return "需客户端";
  if (o.startsWith("confirming")) return "确认中";
  if (o.startsWith("auth")) return "凭据失效";
  if (o.startsWith("shape")) return "接口变动";
  if (o.startsWith("no_cred")) return "没读到凭据";
  if (o.startsWith("retry")) return "未成功";
  if (o.startsWith("failed")) return "失败";
  return o || "—";
}

/** 后端逐天结论 → 日历格的状态类与说明字。status 由后端给(周期日口径),
 *  前端只翻译,不算日期。 */
function ckDayCell(s) {
  return {
    claimed: ["领到", "ok"],
    already: ["平台已签", "ok"],
    inactive: ["本期无活动", "ok"],
    manual: ["有活动 · 在客户端", "info"],
    failed: ["没签成", "warn"],
    missed: ["没签", ""],
    pending: ["今天待签", "today"],
    no_data: ["无记录", "dim"],
  }[s] || [s, ""];
}

/** 读一次签到历史(签到中心页签打开时取,后台签完也要重取)。 */
async function loadCheckinHistory() {
  const seq = ++CKH_SEQ;
  let h = null;
  try {
    h = await invoke("checkin_history", { days: CKH_DAYS });
  } catch (e) {}
  if (seq !== CKH_SEQ) return;
  CKH = h;
  renderCkHist();
}

/** 打开签到中心:首页签到格 / 多渠道待签提示条的落点。 */
function openCheckinCenter() {
  openManage();
  showTab("ck"); // 流水由 showTab 统一读取,这里只补页面本身的渲染
  renderCheckinCenter();
}

/** 签到页签的可见性:有渠道开着监测才出现。全部关闭时整页签隐藏(开启入口在
 *  渠道卡),页签栏少一项比一个空页签干净 —— 与"不该提醒一件用户没要求的事"同一原则。 */
function ckTabUpdate() {
  const btn = document.querySelector('#mTabs [data-p="ck"]');
  if (!btn) return;
  let any = false;
  for (const id of CHECKIN_ON) if (CK[id] && CK[id].enabled) { any = true; break; }
  btn.hidden = !any;
  if (!any && btn.classList.contains("on")) showTab("ch");
}

/** 签到中心页签。CK 还没读到时先给一句"正在读取"(宁可晚 200ms,不先给假状态)。 */
function renderCheckinCenter() {
  const box = $("ckCenter");
  if (!box) return;
  if (!Object.keys(CK).length) {
    box.innerHTML = '<div class="cksec"><div class="ckempty">正在读取签到状态…</div></div>';
    return;
  }
  let anyOn = false;
  for (const id of CHECKIN_ON) if (CK[id] && CK[id].enabled) { anyOn = true; break; }
  if (!anyOn) {
    box.innerHTML =
      '<div class="cksec"><div class="ckempty">没有开启活动监测的渠道 —— 在「渠道」页签点开渠道卡,打开「活动监测」即可。</div></div>';
    return;
  }
  box.innerHTML =
    '<div id="ckTodayBox"></div>' +
    [['activity', '活动详情', '查看活动与领取状态', 'ckActsBox'],
     ['history', '签到记录', '近 30 天概览与 90 天明细', 'ckHistBox'],
     ['settings', '签到设置', '管理监测与自动领取', 'ckManageBox']]
      .map(([key, title, hint, id]) => '<details class="ck-fold" data-ck-section="' + key + '"' +
        (CK_CENTER_OPEN.has(key) ? ' open' : '') + '><summary><span><b>' + title +
        '</b><small>' + hint + '</small></span></summary><div class="ck-fold-body" id="' + id + '"></div></details>')
      .join('');
  box.querySelectorAll('[data-ck-section]').forEach((section) => {
    section.addEventListener('toggle', () => {
      if (section.open) CK_CENTER_OPEN.add(section.dataset.ckSection);
      else CK_CENTER_OPEN.delete(section.dataset.ckSection);
    });
  });
  renderCkToday();
  renderCkActs();
  renderCkHist();
  renderCkManage();
}

/** ① 今日总览条 + ② 今日逐渠道行(没签的排前面,签完的靠后)。 */
function renderCkToday() {
  const box = $("ckTodayBox");
  if (!box) return;
  const ids = ckActiveIds();
  if (!ids.length) {
    box.innerHTML = "";
    return;
  }
  const done = ids.filter((id) => CK[id] && CK[id].doneToday).length;
  const all = done >= ids.length;
  const month = ids.reduce((n, id) => n + ((CK[id] && CK[id].monthCredits) || 0), 0);
  const tone = all ? " ok" : ids.some((id) => CK[id] && CK[id].needsUser) ? " warn" : "";
  // 没签的排前面 —— 从别处跳进来的人,要看的正是没签的那几行
  const order = ids.slice().sort((a, b) => (CK[a].doneToday ? 1 : 0) - (CK[b].doneToday ? 1 : 0));
  box.innerHTML =
    '<div class="cksec cktoday-section"><h4>今日签到</h4>' +
    '<div class="cktoday' + tone + '"><b>' + (all ? "✓ " : "今日进度 ") + done + "/" + ids.length +
    '</b><span>本月已领 ' + points(month) + ' 分</span></div>' +
    order
      .map(
        (id) =>
          '<div class="ckch" data-ch="' + esc(id) + '"><b>' + esc((CK[id] && CK[id].name) || id) + "</b>" +
          '<div class="ckwrap" data-id="' + esc(id) + '">' + ckRowHtml(id) + "</div></div>"
      )
      .join("") +
    "</div>";
}

/** ③ 活动状态(原设置页那块原样搬来;筛选只重画这一段,不惊动别的段)。 */
function renderCkActs() {
  const box = $("ckActsBox");
  if (box) box.innerHTML = renderActivityOverview();
}

/** 日历格的 tooltip:日期 + 结果 + 数额 + 尝试次数,一格说清一天。 */
function ckCellTitle(cell, name) {
  const bits = [cell.day];
  if (cell.ts) {
    bits.push(new Date(cell.ts * 1000).toLocaleString("zh-CN", { hour12: false }));
  }
  const [label] = ckDayCell(cell.status);
  bits.push(label + (cell.status === "claimed" && cell.credits ? " +" + cell.credits + " 分" : ""));
  if (cell.attempts > 1) bits.push("尝试 " + cell.attempts + " 次");
  return (name ? name + " · " : "") + bits.join(" · ");
}

/** ④ 近 30 天日历带(每渠道一条)+ 明细流水表(90 天,渠道筛选)。 */
function renderCkHist() {
  const box = $("ckHistBox");
  if (!box) return;
  if (!CKH) {
    box.innerHTML = '<div class="cksec"><h4>近 30 天</h4><div class="ckempty">正在读取签到记录…</div></div>';
    return;
  }
  const active = new Set(ckActiveIds());
  // 日历带只画现役渠道(开着监测且没被隐藏):"没签"只对"该签的渠道"有意义;
  // 关掉/隐藏的渠道,流水仍全量留在下面的明细表里。
  const bands = CKH.channels
    .filter((ch) => active.has(ch.id))
    .map((ch) => {
      const days = (ch.days || []).slice(-CKH_BAND);
      if (!days.length) return "";
      const done30 = days.filter((d) => d.day !== ch.today && ["claimed", "already", "inactive", "manual"].includes(d.status)).length;
      const miss30 = days.filter((d) => d.status === "missed" || d.status === "failed").length;
      const cells = days
        .map((d) => '<i class="ckc ' + d.status + '" title="' + esc(ckCellTitle(d, ch.name)) + '"></i>')
        .join("");
      return (
        '<div class="ckband"><span class="ckband-name">' + esc(ch.name) + "</span>" +
        '<span class="ckband-cells">' + cells + "</span>" +
        '<span class="ckband-sum">完成 ' + done30 + (miss30 ? " · 漏 " + miss30 : "") + "</span></div>"
      );
    })
    .join("");
  const legend =
    '<div class="cklegend">' +
    [["claimed", "领到"], ["already", "平台已签"], ["missed", "没签"], ["failed", "没签成"], ["no_data", "无记录"]]
      .map(([s, label]) => '<i class="ckc ' + s + '"></i>' + esc(label))
      .join(" ") +
    "</div>";
  const names = {};
  for (const ch of CKH.channels) names[ch.id] = ch.name;
  const all = CKH.rows || [];
  const shown = all.filter((r) => CKH_FILTER === "all" || r.id === CKH_FILTER).slice(0, CKH_TABLE_CAP);
  const chips = [["all", "全部 " + all.length]].concat(
    CKH.channels.map((ch) => [ch.id, names[ch.id] || ch.id])
  );
  const table = shown
    .map((r) => {
      const bad = /^(auth|shape|no_cred|failed|retry|confirming)/.test(r.outcome || "");
      return (
        '<div class="cklr' + (r.outcome && r.outcome.startsWith("claimed") ? " ok" : bad ? " warn" : "") + '">' +
        '<span class="cklt">' + esc(new Date(r.ts * 1000).toLocaleString("zh-CN", { hour12: false })) + "</span>" +
        '<span class="ckln">' + esc(r.name || r.id) + "</span>" +
        '<span class="cklo">' + esc(ckOutcomeLabel(r.outcome)) + "</span>" +
        '<span class="cklv">' + (r.credits !== null && r.credits !== undefined ? "+" + points(r.credits) : "") + "</span>" +
        '<span class="ckld" title="' + esc(r.detail || "") + '">' + esc(r.detail || "") + "</span></div>"
      );
    })
    .join("");
  box.innerHTML =
    '<div class="cksec"><h4>近 30 天</h4>' +
    (bands || '<div class="ckempty">窗口内还没有签到记录</div>') +
    legend +
    "</div>" +
    '<div class="cksec"><h4>明细(近 90 天)</h4>' +
    '<div class="ck-overview-filters" role="group" aria-label="按渠道筛选">' +
    chips
      .map(
        ([k, label]) =>
          '<button type="button" class="ckfilter' + (CKH_FILTER === k ? " on" : "") +
          '" aria-pressed="' + (CKH_FILTER === k) + '" data-act="ckhfilter" data-filter="' + esc(k) + '">' +
          esc(label) + "</button>"
      )
      .join("") +
    "</div>" +
    '<div class="cktable">' +
    (table || '<div class="ckempty">当前筛选下没有流水</div>') +
    "</div>" +
    (all.length > shown.length ? '<div class="ckmore">只显示最近 ' + shown.length + " 条(共 " + all.length + " 条)</div>" : "") +
    "</div>";
}

/** ⑤ 管理区:每渠道两个开关 + 免责(从「刷新与启动」搬来;今日状态在上面第②段)。 */
function renderCkManage() {
  const box = $("ckManageBox");
  if (!box) return;
  const rows = [];
  for (const id of CHECKIN_ON) {
    const s = CK[id];
    const on = s ? s.enabled : false;
    const auto = s ? s.auto : true;
    rows.push(
      '<div class="field"><span>' + esc(s ? s.name : id) +
        '<span class="desc">' + (on ? "每小时检查一次" : "已关闭 · 不再请求") + "</span></span>" +
        '<button class="sw' + (on ? " on" : "") + '" role="switch" aria-checked="' + on +
        '" data-act="ckenable" data-id="' + esc(id) + '" aria-label="' + esc((s ? s.name : id) + " 活动监测") + '"></button></div>' +
      (on
        ? '<div class="field"><span>自动领取<span class="desc">' +
          (auto ? "可直接领的自动领" : "只提示,不代领") +
          '</span></span><button class="sw' + (auto ? " on" : "") + '" role="switch" aria-checked="' + auto +
          '" data-act="ckauto" data-id="' + esc(id) + '" aria-label="自动领取"></button></div>'
        : "")
    );
  }
  box.innerHTML =
    '<div class="cksec"><h4>管理</h4>' + rows.join("") +
    '<div class="khint">只读平台状态;自动领取单独控制。<button class="why" data-why="checkin" aria-expanded="false" aria-label="活动监测的说明"></button></div></div>';
  // 段是动态重建的,「?」的水合要跟着跑一遍(与渠道卡同一条规矩)
  hydrateWhy(box);
}

async function ckRun(id) {
  CK_LOG = true;
  try {
    const st = await invoke("checkin_now", { id });
    CK[st.id] = st;
    const t = ckText(st);
    if (st.outcome && st.outcome.startsWith("claimed")) showToast("已领到" + (st.credits ? " +" + st.credits + " 分" : ""));
    else if (st.outcome && st.outcome.startsWith("already")) showToast("平台已签");
    else showToast(t && t.text ? t.text : "没签成", true);
  } catch (e) {
    showToast(String(e), true);
  }
  CK_LOG = false;
  await loadCheckin();
  // 首页那个「领取」按完,条上要立刻变成 3/3 或"待处理",不能等下一轮轮询;
  // 签到中心开着时,流水表与日历带也要立刻带上这一次的记录
  if (manageOpen()) {
    renderChCards();
    loadCheckinHistory();
  } else if (CHANNELS.length) render({ updated: false });
}

async function ckSetEnabled(id, on, auto) {
  const cur = CFG.checkin && CFG.checkin[id];
  CFG.checkin = CFG.checkin || {};
  CFG.checkin[id] = Object.assign(
    { enabled: false, auto: true, attempts: 0 },
    cur || {},
    on === null ? {} : { enabled: on },
    auto === null || auto === undefined ? {} : { auto }
  );
  if (!(await saveCfg("签到设置"))) return;
  if (on) {
    showToast("已开启 " + (CK[id] ? CK[id].name : id) + " 活动监测");
  }
  loadCheckin();
}

// ───────────── 模型页:多来源 + 弹层确认 ─────────────
// 设计见 docs/design/design-models-multi-source.html。三条不变的口径:
//   ① 列表只列「这个来源能用的 ∪ 已配置到这个来源的」,别的永不出现;
//   ② 有价格的来源按单价从高到低排,没价格的排在末尾并写明原因;
//   ③ 写入/取消写入的确认走**弹层**(以前长在列表底部,得滚到底才看得见)。
// 密钥仍由后端现读现写、不回前端;「复制三件套」也是后端拼好字符串。
const MDL = {
  views: {},                 // 来源 id → get_model_catalog 的返回
  src: "",                   // 当前来源
  sources: [],               // 后端给的来源表 [{id,name,hasPrice,listPublic}]
  clients: [],               // inject_state 的客户端状态
  stateError: "",            // inject_state 失败原因(与"没装客户端"分开显示)
  log: [],                   // 写入台账(不含密钥)
  q: "",                     // 搜索词
  sheet: null,               // 当前弹层内容(预告 / 结果 / 客户端详情)
  loading: false,
};
/// 既能当余额渠道、又能当模型来源的三个渠道(id 与来源 id 一致)。
const MODEL_SOURCES = new Set(["4sapi", "hapi", "opencode-go"]);
const CLIENT_NAME = {
  trae: "Trae",
  workbuddy: "WorkBuddy",
  "workbuddy-ai": "WorkBuddy AI",
  zcode: "ZCode",
  dsh: "DeepSeek Harness",
};
/// 唯一「边跑边写」的客户端:DSH 的 profile 按请求解析,它自己的设置层与凭据层都会
/// 收敛外部编辑 —— 所以运行中不是「先退出」,而是「下一个请求生效」。
const HOT_CLIENTS = new Set(["dsh"]);

/** 单价显示。价目表口径是元/百万 token;按次计费的是元/次。
    金额走统一的 money(千分位 + 两位);只有 <¥1 的档位保留三位小数 ——
    0.005/百万 token 用两位口径会被抹成 0.01,单价信息就没了。 */
function mdlMoney(v) {
  if (v === null || v === undefined) return "—";
  return v < 1 ? "¥" + Number(v).toFixed(3) : money(v);
}
function mdlPriceText(r) {
  const p = r.price || {};
  if (p.perCall !== null && p.perCall !== undefined) return mdlMoney(p.perCall) + " / 次";
  if (p.input === null || p.input === undefined) return "";
  const out = p.output === null || p.output === undefined ? "—" : mdlMoney(p.output);
  return "入 " + mdlMoney(p.input) + " · 出 " + out + " / 百万 token" + (p.tiered ? " *" : "");
}
/** 「贵」用状态色标出来 —— 选哪个模型直接决定余额能撑多久,和余额是同一类判断。 */
function mdlPriceTone(r) {
  const p = r.price || {};
  const v = p.input === null || p.input === undefined ? null : p.input;
  if (v === null) return "";
  if (v >= 100) return "bad";
  if (v >= 20) return "warn";
  return "";
}

function mdlView() {
  return MDL.views[MDL.src] || null;
}
function mdlHasPrice() {
  const v = mdlView();
  return !!(v && v.hasPrice);
}
/** 这个来源里,某个模型配到了哪些客户端。 */
function mdlDoneFor(clientId, modelId) {
  const c = MDL.clients.find((x) => x.client === clientId);
  if (!c) return false;
  return (c.models || []).some((m) => m.id === modelId && m.source === MDL.src);
}
function mdlConfiguredIds() {
  const set = new Set();
  for (const c of MDL.clients) {
    for (const m of c.models || []) if (m.source === MDL.src) set.add(m.id);
  }
  return set;
}

function mdlVisibleRows() {
  const v = mdlView();
  if (!v) return [];
  const q = MDL.q.trim().toLowerCase();
  const done = mdlConfiguredIds();
  const out = [];
  // 固定规则:列表 = 「这个来源能用的」∪「已配置到这个来源的」。
  // 取不到可用集合时只列已配置的 —— 不把"不知道能不能用"当成"能用"。
  const knowUsable = (v.usableAt || 0) > 0;
  for (const r of v.rows || []) {
    const useful = knowUsable ? r.usable === true || done.has(r.id) : done.has(r.id);
    if (!useful) continue;
    if (q) {
      const hay = (
        r.id + " " + (r.vendor || "") + " " + (r.tags || []).join(" ") + " " + (r.description || "")
      ).toLowerCase();
      if (!hay.includes(q)) continue;
    }
    out.push(r);
  }
  // 排序:有价格的来源按单价从高到低(最烧钱的在最上面),价格相同按名称;
  // 没价格的来源(订阅制 / 站点不给价)整列排在末尾,内部「已配置优先 + 名称」。
  if (v.hasPrice) {
    out.sort((a, b) => {
      const pa = (a.price || {}).input;
      const pb = (b.price || {}).input;
      const ha = pa === null || pa === undefined ? -1 : pa;
      const hb = pb === null || pb === undefined ? -1 : pb;
      if (ha !== hb) return hb - ha;
      return a.id.localeCompare(b.id);
    });
  } else {
    out.sort((a, b) => {
      const da = done.has(a.id) ? 0 : 1;
      const db = done.has(b.id) ? 0 : 1;
      return da - db || a.id.localeCompare(b.id);
    });
  }
  return out;
}

function mdlClientChip(c) {
  const n = (c.models || []).length;
  const st = !c.installed ? "未装" : c.running ? "运行中" : n ? n + " 条" : "未配";
  return (
    '<div class="mcli-c' + (c.installed ? "" : " off") + '" data-act="mdlclistate" data-id="' +
    esc(c.client) + '" title="' + esc(c.label + " · " + (c.files || []).join(" / ")) + '">' +
    "<b>" + esc(c.short) + "</b><span>" + esc(st) + "</span></div>"
  );
}

function mdlRowHtml(r) {
  const done = mdlDoneFor;
  const bits = [];
  if (r.context) bits.push(r.context);
  for (const t of r.tags || []) if (t !== r.context) bits.push(t);
  if (r.vendor) bits.push(r.vendor);
  // 页面里不出现「分组」字样(用户口径):不列、不标、不解释
  const unusable = r.usable === false;
  const configured = mdlConfiguredIds().has(r.id);
  const badges = [];
  if (configured) badges.push('<span class="tagd ok">已配置</span>');
  if (r.tiered) badges.push('<span class="tagd">阶梯</span>');
  if (unusable) badges.push('<span class="tagd bad">不可用</span>');
  if (MDL.src === "opencode-go") {
    const apiLabel = {
      "open-ai-chat-completions": "Chat API",
      "open-ai-responses": "Responses API",
      "anthropic-messages": "Anthropic API",
      "google-generative": "Google API",
    }[r.apiProtocol];
    badges.push(apiLabel
      ? '<span class="tagd" title="官网公布的模型接口协议">' + esc(apiLabel) + "</span>"
      : '<span class="tagd bad" title="官网模型接口列表未标注该模型协议">协议待确认</span>');
  }
  const btns = ["trae", "workbuddy", "workbuddy-ai", "zcode", "dsh"]
    .map((id) => {
      const st = MDL.clients.find((x) => x.client === id);
      const installed = st ? st.installed : false;
      const running = st ? st.running : false;
      const on = done(id, r.id);
      // DSH 是唯一「边跑边写」的:它的 profile 按请求解析,运行中不必先退出,
      // 所以既不给它「运行中」的黄色警示,也不在提示里让人去退它。
      const hot = HOT_CLIENTS.has(id);
      const cls = on ? " on" : running && !hot ? " run" : installed ? "" : " no";
      const tip = installed
        ? (on ? "已配置 · 点开可更新或取消写入" : "写入 " + st.label) +
          (hot
            ? (running ? "(运行中也能写 · 下一个请求生效)" : "(不用退出 · 下一个请求生效)")
            : running ? "(正在运行 · 先退出它再写,否则会被它覆盖)" : "")
        : "没检测到配置文件";
      return (
        '<button class="cdot' + cls + '" data-act="mdlcli" data-id="' + esc(r.id) + '" data-c="' + id +
        '" title="' + esc(tip) + '"' + (installed ? "" : " disabled") + ">" +
        esc(st ? st.short : "?") + "</button>"
      );
    })
    .join("");
  // 不可用的行不给单价:它的价不属于你这把 key;没有价格的来源干脆不显示价格行
  const price = mdlHasPrice() && !unusable ? mdlPriceText(r) : "";
  return (
    '<div class="mrow' + (configured ? " done" : "") + '">' +
    '<div class="mtop"><span class="mname">' + esc(r.id) +
    '<button class="minfo" data-act="mdlinfo" data-id="' + esc(r.id) +
    '" title="基本信息 · 逐项可复制(客户端里手填时用这个)">ⓘ</button>' +
    '</span><span class="cdots">' + btns + "</span></div>" +
    '<div class="mtags">' + esc(bits.join(" · ")) + badges.join("") + "</div>" +
    (price ? '<div class="mprice ' + mdlPriceTone(r) + '">' + esc(price) + "</div>" : "") +
    "</div>"
  );
}

/** 渲染异常不许把界面留在「正在取…」上(真机踩过:后端成功 + 前端抛异常,看着像在加载)。 */
function renderModels() {
  try {
    renderModelsInner();
  } catch (err) {
    const meta = $("mdlMeta");
    if (meta) meta.innerHTML = '<div class="mdlwarn">模型页渲染出错:' + esc(err && err.message) + "</div>";
    console.error("renderModels", err);
  }
}

function renderModelsInner() {
  const meta = $("mdlMeta");
  const list = $("mdlList");
  if (!meta || !list) return;
  // 来源芯片:始终三个,没取到的来源标「—」
  const chips = MDL.sources
    .map((s) => {
      const v = MDL.views[s.id];
      const sub = !v ? "—" : v.usableAt ? v.usableCount + " 个可用" : "可用数未知";
      return (
        '<div class="srcbar-i' + (s.id === MDL.src ? " on" : "") + '" data-act="mdlsrc" data-id="' +
        esc(s.id) + '" title="' + esc(s.name) + '">' + esc(s.name) + "<i>" + esc(sub) + "</i></div>"
      );
    })
    .join("");
  const srcEl = $("mdlSrc");
  if (srcEl) srcEl.innerHTML = chips;
  // 空列表有两种意思:"探测环节坏了"与"这台机器真没装客户端",必须分开说(P2-35)
  $("mdlCli").innerHTML = MDL.clients.length
    ? MDL.clients.map(mdlClientChip).join("")
    : MDL.stateError
      ? '<span class="dim">检测客户端失败:' + esc(MDL.stateError) + "</span>"
      : '<span class="dim">没检测到可写入的客户端(Trae / WorkBuddy / ZCode / DeepSeek Harness)</span>';

  const v = mdlView();
  if (!v) {
    meta.textContent = MDL.loading ? "正在取价目表…" : "还没取到模型清单";
    list.innerHTML = "";
    return;
  }
  const rows = mdlVisibleRows();
  const done = mdlConfiguredIds();
  const isGo = v.source === "opencode-go";
  meta.innerHTML =
    '<div class="mrow1"><span class="dim">' +
    (isGo
      ? "OpenCode Go 按模型选择协议端点(仅兼容的模型可自动写入)"
      : '写进客户端 <b>' + esc(v.baseUrl) + "</b>") +
    "</span>" +
    '<span class="dim">' +
    (v.hasPrice ? "单价按站点自动分组折算" : isGo ? "订阅制:不按 token 计价" : "该来源不提供单价") +
    "</span></div>" +
    '<div class="mrow1"><span class="dim">显示 ' + rows.length +
    (v.usableAt ? " · 可用 " + v.usableCount : " · 可用数未知") +
    " · 已配置 " + done.size + "</span>" +
    '<span class="dim">' +
    (v.fetchedAt ? "清单 " + relTime(v.fetchedAt) + " · " : "") +
    (v.hasPrice ? "价格 ¥高→低" : "按 已配置 → 名称") + "</span></div>" +
    (v.usableAt
      ? ""
      : '<div class="mdlwarn">' +
        (v.hasKey || v.listPublic
          ? "可用集合没取到(刷新试试)"
          : "未配 " + esc(v.sourceName) + " 密钥") +
        ":按「不列不可用模型」的规则,这里只显示你已经配到客户端的那几条</div>") +
    (v.error ? '<div class="mdlwarn">' + esc(v.error) + "</div>" : "");
  list.innerHTML = rows.length
    ? rows.map(mdlRowHtml).join("")
    : '<div class="empty">' +
      ((v.usableAt || 0) === 0
        ? "还没取到" + esc(v.sourceName) + "的模型清单" +
          (v.hasKey || v.listPublic ? "(刷新试试)" : ":先在「渠道」页签填密钥") +
          " —— 按规则这里只显示已配置的那几条"
        : "没有符合条件的模型" +
          " —— 只列这个来源能用的、以及已经配到客户端的") +
      "</div>";
}

/** 弹层内容:写入预告 / 取消写入 / 写入结果 / 客户端详情 —— 四合一。 */
function renderModelSheet() {
  const ov = $("mdlOv");
  const box = $("mdlOvIn");
  if (!ov || !box) return;
  const s = MDL.sheet;
  if (!s) {
    ov.style.display = "none";
    box.innerHTML = "";
    return;
  }
  ov.style.display = "flex";

  if (s.kind === "info") {
    box.innerHTML = infoSheetHtml(s);
    return;
  }

  if (s.kind === "client") {
    const c = MDL.clients.find((x) => x.client === s.client);
    if (!c) {
      MDL.sheet = null;
      renderModelSheet();
      return;
    }
    const recent = MDL.log.filter((r) => r.client === s.client).slice(0, 3);
    const bySource = {};
    for (const m of c.models || []) (bySource[m.source || "其它"] ||= []).push(m.id);
    const chipsHtml = Object.keys(bySource).length
      ? Object.entries(bySource)
          .map(
            ([src, ids]) =>
              '<div class="srow"><span class="sk">' +
              esc(src === "其它" ? "其它" : (MDL.sources.find((x) => x.id === src) || {}).name || src) +
              '</span><span class="sv">' +
              ids
                .map(
                  (m) =>
                    '<button class="btn s" data-act="mdlcli2" data-c="' + esc(c.client) + '" data-src="' +
                    esc(src) + '" data-id="' + esc(m) + '" title="点开看这条的状态 / 取消写入">' + esc(m) + "</button>"
                )
                .join(" ") +
              "</span></div>"
          )
          .join("")
      : '<div class="srow"><span class="sk">已写入</span><span class="sv">还没有 —— 去模型页挑一个</span></div>';
    box.innerHTML =
      '<div class="ov-h"><b>' + esc(c.label) + "</b>" +
      '<span class="dim" style="font-size:11px" data-act="mdlclose">✕</span></div>' +
      '<div class="srow"><span class="sk">状态</span><span class="sv">' +
      (c.installed
        ? c.running
          ? HOT_CLIENTS.has(c.client)
            ? "正在运行(不用退出 · 下一个请求生效)"
            : "正在运行(写入前需先退出)"
          : "未运行"
        : "没检测到配置文件") + "</span></div>" +
      '<div class="srow"><span class="sk">配置</span><span class="sv">' + esc((c.files || []).join("  ·  ")) + "</span></div>" +
      chipsHtml +
      '<div class="srow"><span class="sk">选中</span><span class="sv">' +
      (c.selected ? esc(c.selected) : "读不到(它自己的选中项)") + "</span></div>" +
      (c.note ? '<div class="note">' + esc(c.note) + "</div>" : "") +
      (recent.length
        ? '<div class="note" style="font-weight:600;color:#cdd5e4;margin-top:8px">最近写入</div>' +
          recent
            .map(
              (r) =>
                '<div class="srow"><span class="sk">' + esc(r.stamp.slice(9)) + '</span><span class="sv">' +
                esc(r.model) + (r.added ? " · 新增 " + r.added : "") +
                (r.updated ? " · 更新 " + r.updated : "") +
                (r.removed ? " · 移除 " + r.removed : "") +
                (r.backupOk
                  ? '<button class="btn s" data-act="openpath" data-id="' + esc(r.backupPath) + '">备份</button>' +
                    '<button class="btn s" data-act="mdlrollback" data-id="' + esc(r.backupPath) + '">回滚</button>'
                  : '<span class="dim"> · 备份已清理(只保留最近几份),这条回滚不了</span>') +
                "</span></div>"
            )
            .join("")
        : "") +
      '<div class="acts">' +
      '<button class="btn" data-act="openpath" data-id="' + esc((c.roots || [])[0] || "") + '">打开配置目录</button>' +
      '<button class="btn p" data-act="mdlclose">关闭</button></div>';
    return;
  }

  // 写入预告 / 结果
  const head =
    '<div class="ov-h"><b>' +
    (s.reports ? (s.mode === "remove" ? "取消写入结果" : "写入结果") : "写入 · ") +
    esc(s.model) + "</b>" +
    '<span class="dim" style="font-size:11px">' + esc(CLIENT_NAME[s.client] || s.client) +
    ' <span data-act="mdlclose" style="cursor:pointer">✕</span></span></div>';
  if (s.error) {
    box.innerHTML =
      head + '<div class="mdlwarn">' + esc(s.error) + "</div>" +
      '<div class="acts"><button class="btn" data-act="mdlcopy" data-id="' + esc(s.model) + '">复制三件套</button>' +
      '<button class="btn p" data-act="mdlclose">关闭</button></div>';
    return;
  }
  if (!s.previews) {
    box.innerHTML = head + '<div class="dim">正在生成写入预告…</div>';
    return;
  }
  if (s.reports) {
    box.innerHTML =
      head +
      (s.reports.some((r) => r.error)
        ? '<div class="mdlwarn">有几家没写成,原因见下。已写成的那些照常可以回滚。</div>'
        : "") +
      s.reports
        .map((r) => {
          // 没写成的那一家:给原因,不给"打开备份/回滚"两个必然报错的按钮(报告 P1-6 / P3-6)
          if (r.error) {
            return (
              '<div class="okbox"><b>✕ ' + esc(r.label) + "</b> · 这一家没写成" +
              '<div class="mdlwarn">' + esc(r.error) + "</div>" +
              (r.notes || []).map((n) => '<div class="note">' + esc(n) + "</div>").join("") +
              "</div>"
            );
          }
          const hasBk = !!r.backupPath;
          return (
            '<div class="okbox"><b>✓ ' + esc(r.label) + "</b> · " +
            (r.added ? "新增 " + r.added + " 条" : "") + (r.updated ? "更新 " + r.updated + " 条" : "") +
            (r.removed ? "移除 " + r.removed + " 条" : "") +
            (r.added + r.updated + r.removed === 0 ? "没有改动(本来就没有,或内容一致)" : "") +
            '<div class="srow"><span class="sk">生效</span><span class="sv">' + esc(r.effect) + "</span></div>" +
            (r.selected ? '<div class="srow"><span class="sk">选中</span><span class="sv">已写入选中项</span></div>' : "") +
            (hasBk
              ? '<div class="srow"><span class="sk">备份</span><span class="sv">' + esc(r.backup) +
                '<button class="btn s" data-act="openpath" data-id="' + esc(r.backupPath) + '">打开</button>' +
                '<button class="btn s" data-act="mdlrollback" data-id="' + esc(r.backupPath) + '">回滚这次写入</button>' +
                "</span></div>"
              : '<div class="srow"><span class="sk">备份</span><span class="sv">本次没动文件,无需备份</span></div>') +
            "</div>" +
            (r.notes || []).map((n) => '<div class="note">' + esc(n) + "</div>").join("")
          );
        })
        .join("") +
      '<div class="acts"><button class="btn" data-act="mdlcopy" data-id="' + esc(s.model) + '">复制三件套</button>' +
      '<button class="btn p" data-act="mdlclose">完成</button></div>';
    return;
  }
  const blocked = s.previews.find((p) => p.blocked);
  const blockedIsRuntime = blocked && /正在运行/.test(blocked.blocked);
  const changed = s.previews.some((p) => p.changed);
  const writesSecret = s.previews.some((p) => p.writesSecret);
  const writesSel = s.previews.some((p) => p.writesSelection);
  const removals = s.previews.filter((p) => p.removal);
  box.innerHTML =
    head +
    s.previews
      .map(
        (p) =>
          '<div class="srow"><span class="sk">目标</span><span class="sv">' + esc(p.label) + " · " +
          esc(p.files.join("  ·  ")) + "</span></div>" +
          '<div class="srow"><span class="sk">动作</span><span class="sv">' + esc(p.action) + "</span></div>" +
          '<div class="srow"><span class="sk">选中</span><span class="sv">' +
          (p.writesSelection ? "顺带写入客户端自己的选中项" : "不改选中项") + "</span></div>"
      )
      .join("") +
    (writesSecret
      ? '<div class="mdlwarn">会把这把 ' + esc(s.sourceName || "") +
        ' 密钥<b>明文</b>写进上面那个客户端自己的配置文件' +
        (s.client === "dsh" ? "(DSH 这边是 <code>.credentials.yaml</code> 的 <code>refs:</code>,它自己也是这么存的)" : "") +
        "。挂件自己不存副本:密钥不进数据库、不进日志、不进写入台账。" +
        "<b>但回滚要靠备份</b> —— 备份是照原样拷那个客户端那几个文件,而里面的密钥本来就是明文," +
        "所以 <code>backup-inject/&lt;时间戳&gt;/</code> 下会留下含明文密钥的历史快照" +
        "(每个客户端最多 5 份;换密钥后旧快照不自动失效)。想清掉就去模型页的「打开配置目录」。"+
        "</div>"
      : "") +
    (!writesSel && !blocked && !removals.length && s.client !== "dsh"
      ? '<div class="note">选中项这次不写:写入后在客户端里选一次,挂件会记住口径,以后就能直接帮你选。</div>'
      : "") +
    (removals.length
      ? '<div class="note" style="font-weight:600;color:#cdd5e4;margin-top:8px">当前状态:已配置</div>' +
        removals
          .map(
            (p) =>
              '<div class="srow"><span class="sk">会移除</span><span class="sv">' + esc(p.label) + " · " +
              esc(p.removal.what) + "</span></div>" +
              (p.removal.clearsSelection
                ? '<div class="note">它的「选中项」正指向这条模型,会一并清掉(回到"没选"的状态)。</div>'
                : "") +
              '<div class="note">其它条目与你的手改都不动;移除前照样先备份。</div>'
          )
          .join("")
      : "") +
    (blocked
      ? '<div class="mdlwarn">' + esc(blocked.blocked) +
        '<div class="note">' +
        (blockedIsRuntime
          ? "退出它之后直接再点一次下面的按钮就行 —— 按钮会自己重查一遍,这个窗口不用关。" +
            (s.recheckAt ? "(" + esc(s.recheckAt) + " 刚查过,它还在运行)" : "")
          : "本次不会修改客户端配置。") +
        "</div></div>"
      : "") +
    '<div class="acts">' +
    (blocked
      ? blockedIsRuntime
        // 别在这里禁掉按钮:用户常常是**弹层开着的时候**才去退客户端,禁掉的按钮
        // 点了毫无反应,看着就像"写入没成功"(实机反馈)。所以按钮留着,点它 = 重查。
        ? '<button class="btn p"' + (s.busy ? " disabled" : ' data-act="injectgo"') + ">" +
          esc(s.busy ? "正在重查…" : "写入(" + blocked.label + " 在运行 · 点这里重查)") + "</button>"
        : '<button class="btn p" disabled>写入(当前不可写)</button>'
      : changed
      ? '<button class="btn p" data-act="injectgo"' + (s.busy ? " disabled" : "") + ">" +
        (s.busy ? "写入中…" : "写入") + "</button>"
      : '<button class="btn" disabled>内容一致 · 无需写入</button>') +
    (removals.length
      ? '<button class="btn danger" data-act="injectrm"' + (s.busy ? " disabled" : "") + ">" +
        "取消写入(移除这条)</button>"
      : "") +
    '<button class="btn" data-act="mdlcopy" data-id="' + esc(s.model) + '">复制三件套</button>' +
    '<button class="btn" data-act="mdlclose">取消</button></div>';
}

async function loadModels(force, source) {
  const src = source || MDL.src || "4sapi";
  MDL.loading = true;
  if (!MDL.views[src]) renderModels();
  try {
    const view = await invoke("get_model_catalog", { source: src, force: !!force });
    MDL.views[src] = view;
    if (view.sources && view.sources.length) MDL.sources = view.sources;
    if (!MDL.src) MDL.src = view.source || src;
  } catch (e) {
    showToast("取模型清单失败:" + e, true);
  } finally {
    MDL.loading = false;
    renderModels();
  }
}
async function loadInjects() {
  try {
    const ov = await invoke("inject_state");
    MDL.clients = ov.clients || [];
    MDL.log = ov.log || [];
  } catch (e) {
    MDL.clients = [];
    // 后端现在会把"探测环节坏了"与"你没装客户端"分开说,这里别再把它吞成空列表
    MDL.stateError = String(e);
  }
  if (MDL.clients.length) MDL.stateError = "";
  renderModels();
  renderModelSheet();
}
/** 模型基本信息里那几行(标签 → 要复制的值)。逐项复制与「复制全部」共用一份口径。 */
function infoFields(s) {
  const i = (s && s.info) || {};
  return {
    模型名: i.id || "",
    显示名: i.displayName || "",
    接口协议: i.apiType || "",
    接口地址: i.baseUrl || "",
    完整端点: i.endpoint || "",
    上下文: i.contextTokens ? String(i.contextTokens) : "",
    图片: i.multimodal ? "支持" : "不支持",
  };
}

/** 模型基本信息弹层:手工在客户端里新建自定义模型时照着填的那几个值。
 * 密钥在这里只给后四位,「显示」才出明文(key_reveal,8 秒自动收回)。 */
function infoSheetHtml(s) {
  const head =
    '<div class="ov-h"><b>' + esc(s.model) + '</b><span class="dim" style="font-size:11px">' +
    esc(s.sourceName || s.source || "") +
    ' 基本信息 <span data-act="mdlclose" style="cursor:pointer">✕</span></span></div>';
  if (s.error) {
    return (
      head + '<div class="mdlwarn">' + esc(s.error) + "</div>" +
      '<div class="acts"><button class="btn p" data-act="mdlclose">关闭</button></div>'
    );
  }
  if (!s.info) return head + '<div class="dim">正在读取…</div>';
  const f = infoFields(s);
  const tips = {
    模型名: "OpenAI 兼容接口的 model 参数;Trae 里也是它",
    显示名: "WorkBuddy / ZCode 里的名字口径(Trae 用上面的模型名)",
    接口协议: "OpenCode Go 模型的请求协议;不同模型可能不同",
    接口地址: "客户端配置中的 base_url",
    完整端点: "模型实际调用的完整 API 端点",
    上下文: "上下文窗口(token);取不到就不显示",
    图片: "能不能收图",
  };
  const rows = Object.keys(f)
    .filter((k) => f[k] !== "" && !(k === "图片" && f[k] === "不支持"))
    .map(
      (k) =>
        '<div class="irow"><span class="ik" title="' + esc(tips[k] || "") + '">' + esc(k) + "</span>" +
        '<span class="iv">' + esc(f[k]) + "</span>" +
        '<button class="btn s" data-act="infocopy" data-id="' + esc(k) + '">复制</button></div>'
    )
    .join("");
  const keyRow = s.info.hasKey
    ? '<div class="krow"><span class="k1">API Key · <span class="khint">' +
      esc(s.info.keyHint || "••••") + "</span></span>" +
      '<button class="btn s" data-act="infokeycopy" title="复制整把密钥(明文不回界面)">复制</button>' +
      '<button class="btn s" data-act="infokeyreveal" title="显示明文,8 秒后自动隐">显示</button></div>' +
      '<div class="reveal-box" data-rev="info"></div>'
    : '<div class="mdlwarn">还没配 ' + esc(s.sourceName || s.source) +
      " 密钥 —— 先在「渠道」页签填一次,这里才有可复制的密钥(地址与模型名照样能复制)。</div>";
  return (
    head +
    '<div class="note">这里显示官网公布的接口协议与端点。自动写入会检查目标客户端是否支持该协议;' +
    "不兼容时会阻止写入。需要手动配置时,可复制对应字段。</div>" +
    rows + keyRow +
    '<div class="acts">' +
    '<button class="btn" data-act="infocopyall">复制全部(不含密钥)</button>' +
    '<button class="btn p" data-act="mdlclose">关闭</button></div>'
  );
}

/** 打开某个模型的「基本信息」(纯读:不写盘、不碰任何客户端)。 */
async function openModelInfo(model, source) {
  const src = source || MDL.src;
  const sheet = { kind: "info", mode: "info", model, source: src };
  MDL.sheet = sheet;
  renderModelSheet();
  let info, failure = "";
  try {
    info = await invoke("model_info", { source: src, model });
  } catch (e) {
    failure = String(e);
  }
  if (MDL.sheet !== sheet) return;
  sheet.info = info;
  sheet.sourceName = (MDL.sources.find((x) => x.id === src) || {}).name || src;
  if (failure) sheet.error = failure;
  renderModelSheet();
}

/** 打开某个模型 × 某个客户端的弹层(纯读,还没有任何写盘动作)。 */
async function openModelSheet(client, model, source) {
  const src = source || MDL.src;
  const sheet = { kind: "write", mode: "write", client, model, source: src };
  MDL.sheet = sheet;
  renderModelSheet();
  // 预告要读客户端配置,可能跑一两秒。这期间用户按 Esc 或点遮罩会把 MDL.sheet 置空,
  // 于是"await 之后再取 MDL.sheet"会撞上 null 直接抛 TypeError(实测:关掉弹层后
  // inject_preview 才回来,报 "Cannot set properties of null (setting 'error')")。
  // 先把这个对象存进局部变量,回来时只对"还是当前弹层"的结果落账。
  let previews, failure = "";
  try {
    previews = await invoke("inject_preview", { client, source: src, model });
  } catch (e) {
    failure = String(e);
  }
  if (MDL.sheet !== sheet) return;
  sheet.previews = previews;
  sheet.sourceName = (MDL.sources.find((s) => s.id === src) || {}).name || "";
  if (failure) sheet.error = failure;
  renderModelSheet();
}
/** 确认写入:预告看过之后才走到这里。 */
/** 点「写入 / 取消写入」前先重查一次"客户端还在不在运行"。
 *
 * 为什么要有这一步:用户常常是**弹层已经开着**的时候才去退客户端。以前这里是个禁用的
 * 按钮 —— 退完客户端回来一看按钮还是灰的、点了毫无反应,看着就像"写入没成功"
 * (实机反馈)。现在按钮留着可点,点它 = 重新生成一次预告:还挡着就把原因跟时间点
 * 就地写出来,放开了就继续做原先那件事。
 * 返回 true = 可以继续;false = 还被挡着(界面已更新,调用方直接收手)。 */
async function mdlRecheck(s) {
  if (!(s.previews || []).some((p) => p.blocked)) return true;
  s.busy = true;
  renderModelSheet();
  let fresh;
  try {
    fresh = await invoke("inject_preview", { client: s.client, source: s.source, model: s.model });
  } catch (e) {
    if (MDL.sheet === s) {
      s.busy = false;
      s.error = String(e);
      renderModelSheet();
    }
    return false;
  }
  if (MDL.sheet !== s) return false;
  s.busy = false;
  s.previews = fresh;
  const still = (fresh || []).find((p) => p.blocked);
  s.recheckAt = new Date().toTimeString().slice(0, 8);
  renderModelSheet();
  if (still) {
    showToast(s.recheckAt + " 查过:" + (still.label || "") + " 还在运行,先退出它再点一次", true);
    return false;
  }
  return true;
}

async function injectGo() {
  const s = MDL.sheet;
  if (!s || !s.model || s.busy) return;
  if (!(await mdlRecheck(s))) return;
  s.busy = true;
  renderModelSheet();
  try {
    s.reports = await invoke("inject_apply", { client: s.client, source: s.source, model: s.model });
    const ok = (s.reports || []).filter((r) => !r.error);
    // 只有一家、且写成了的时候才把"怎么生效"顺带说一句 —— 多家时详情在结果里
    showToast(
      "已写入 " + (CLIENT_NAME[s.client] || s.client) +
        (s.reports.length === 1 && ok.length === 1 ? " · " + ok[0].effect : "")
    );
  } catch (e) {
    s.error = String(e);
  }
  s.busy = false;
  renderModelSheet();
  loadInjects();
}
/** 取消写入:把这条模型从客户端里删掉。 */
async function injectRemove() {
  const s = MDL.sheet;
  if (!s || !s.model || s.busy) return;
  if (!(await mdlRecheck(s))) return;
  s.mode = "remove";
  s.busy = true;
  renderModelSheet();
  try {
    s.reports = await invoke("inject_remove", { client: s.client, source: s.source, model: s.model });
    const n = s.reports.reduce((a, r) => a + (r.removed || 0), 0);
    showToast(n ? "已从 " + (CLIENT_NAME[s.client] || s.client) + " 移除 " + n + " 条" : "本来就没有这条");
  } catch (e) {
    s.error = String(e);
  }
  s.busy = false;
  renderModelSheet();
  loadInjects();
}
/** 切来源(不重新联网,除非那一份还没取过)。 */
async function pickSource(id) {
  if (!id || id === MDL.src) return;
  MDL.src = id;
  MDL.sheet = null;
  renderModelSheet();
  renderModels();
  if (!MDL.views[id]) await loadModels(false, id);
}
/** 打开模型页签;带来源时直接切过去(渠道行的「去挑模型」用的就是这个)。 */
async function ensureModels(source) {
  if (!MDL.clients.length) await loadInjects();
  const want = source || MDL.src || "4sapi";
  if (!MDL.src) MDL.src = want;
  if (!MDL.views[want]) await loadModels(false, want);
  renderModels();
}

// ───────────── 启动 ─────────────
async function refresh(force = false) {
  // render() 挪出 try:渲染抛错以前会被当成"取数失败"再 toast 一次,信息是错的(P2-33)
  let list;
  try {
    list = await invoke("get_channels", { force });
  } catch (e) {
    // 以前这里直接覆写 #list 就完了:紧凑条形态下条里一片空白、窗口留着旧宽度,
    // 胶囊与托盘也停在旧值(报告 P2-14)。走同一条 render 收尾,只是把错误提示出来。
    showToast("取数失败:" + e, true);
    render({ updated: false });
    return;
  }
  CHANNELS = list;
  await loadHomeView();
  // 签到状态必须**在 render 之前**取好(2026-10-21 修):
  // 以前 loadCheckin() 只在「打开设置页 / 点签到按钮 / 后端推事件」时调用 ——
  // 于是首页从来就没有 CK,徽标一律拿到 undefined 直接不画,用户当然看不到签到。
  // 它是**一个** IPC(一次返回全部渠道),比每个渠道各查一次便宜,放在这里不心疼。
  await loadCheckin();
  render({ updated: true });
}

/** 收下一轮轮询推送:`{ views, home }` —— 数据与统一视图**一起**到。
 *
 * 只收 views 的话,面板数字会更新(行里的大数字是前端自己从 `remaining` 算的),
 * 而胶囊主数字 / 行色调 / 分组 / 风险带读的是 `home_view` 的结论 —— 那一份以前
 * 只在启动时取过一次,于是永远停在启动那一刻(2026-10-26 真机:4S 余额一直在动,
 * 胶囊一直不动,托盘也跟着胶囊一起僵住)。
 *
 * 老后端只推一个 views 数组:退回补取一次 `home_view`(与这一轮同一份取数结果,
 * 5 秒内复用缓存,不会多打账务接口),行为不回退。
 */
function applyChannelsPush(payload) {
  const legacy = Array.isArray(payload);
  CHANNELS = legacy ? payload : (payload && payload.views) || [];
  if (!legacy && payload && payload.home) {
    hvApply(payload.home);
    render({ updated: true });
    return;
  }
  loadHomeView().then(() => render({ updated: true }));
}

(async function init() {
  try {
    CFG = await invoke("get_config");
  } catch {
    CFG = {
      activeIntervalSec: 60, idleIntervalSec: 300, backoffIntervalSec: 900,
      warnPercent: 40, critPercent: 15, autostart: false,
      form: "panel", compactEnabled: false, sort: "percent", pillChannels: [],
      fontScale: "md", theme: "dark", panelLayout: "single", hiddenChannels: [], hotkey: "", trayShowNumber: true,
      claimChannels: {
        "4sapi": { enabled: true, amount: 200, minIntervalDays: 14, manualLastAt: null, manualSetAt: null },
      },
      modelShowAll: false, modelGroup: null, modelBaseUrl: null,
      selectionPrefix: {}, traeAutoSelect: false,
    };
  }
  applyConfigToUI();
  applyFontScale();
  // 静态模板里的「?」补出说明块(渠道卡里的是动态的,renderChCards 自己会补)
  hydrateWhy();
  // 版本号:等首检(最坏 30 秒)才显示,期间关于页是空的。直接问后端要(报告 P2-16)
  invoke("app_version")
    .then((v) => {
      VER = v;
      const el = $("uver");
      if (el && !el.textContent) el.textContent = " " + v;
    })
    .catch(() => {});
  // 启动期的非致命错误(配置坏了回落默认、快捷键被占用):后端只往 stderr 写过,
  // 而绿色版从托盘启动时没人看得到控制台 —— 取回来显示在关于页的状态行上
  invoke("take_startup_error")
    .then((msg) => {
      if (msg) setMsg(msg, true);
    })
    .catch(() => {});
  // 置顶以真实窗口状态为准(Rust 启动时按配置应用),避免按钮与实际不一致
  try {
    const real = await appWindow.isAlwaysOnTop();
    if (real !== CFG.alwaysOnTop) CFG.alwaysOnTop = real;
  } catch {}
  applyPinUI();
  // ── 签到事件:必须在首轮 refresh **之前**挂上 ──
  // 后台签到节拍器在 Rust setup 里立刻跑一轮(`spawn_checkin_ticker`),常常正好赶上
  // 前端首帧 refresh。以前这些监听写在 refresh 之后:这一轮的事件没人接,CK 就停在
  // 启动前的旧结论上 —— 2026-10-23 现场,Qoder 10:11 已判 inactive,顶部却一直挂
  // "还有 1 个渠道没签到",点进签到区也看不到任何能做的事,直到 10:55 手动领一次才消失。
  await listen("checkin-updated", () => {
    loadCheckin().then(() => {
      if (manageOpen()) {
        renderChCards();
        fillKeyHints();
        // 签到中心页签开着:流水与日历跟着后台签到当场更新
        const ckBtn = document.querySelector('#mTabs [data-p="ck"]');
        if (ckBtn && ckBtn.classList.contains("on")) loadCheckinHistory();
      } else if (CHANNELS.length) {
        // 首页的提示条 / 风险带也是 CK 的使用者:后台签完要当场收掉那条提示
        render({ updated: false });
      }
    });
  });
  // ── 跨日重取:签到短码是**周期日作用域**的,北京 0 点(trae/workbuddy)与
  // 10 点(Qoder)一翻页,昨天的「已签」必须当场翻成「待签」,不能等下一个事件来撞
  // (2026-10-25 真机:0 点后界面挂着昨天的已签,直到 00:09 的监测事件才被动刷掉)。
  // 后端已按周期日门控短码;前端只负责在边界时刻重取一次(loadCheckin 自带重渲)。
  // 北京 0 点 = UTC 16:00,Qoder 10:00 = UTC 02:00(北京无夏令时,UTC 算术稳定)。
  {
    const DAY = 86400000;
    const msToBoundary = () => {
      const t = Date.now();
      const dayStart = Math.floor(t / DAY) * DAY;
      let at = dayStart + 2 * 3600000; // UTC 02:00(Qoder 跨日)
      if (at <= t) at = dayStart + 16 * 3600000; // UTC 16:00(北京 0 点)
      if (at <= t) at = dayStart + DAY + 2 * 3600000;
      return at - t;
    };
    const scheduleRollover = () => {
      setTimeout(() => {
        loadCheckin();
        scheduleRollover();
      }, msToBoundary());
    };
    scheduleRollover();
  }
  await listen("checkin-alert", (ev) => {
    // 后端只在"需要用户动手"(凭据失效 / 接口结构变了)时发这条;绿色版从托盘启动时
    // stderr 永远没人看得到(报告 P2-14 / P3-36)。
    const d = (ev && ev.payload) || {};
    showToast(
      (d.name || "签到渠道") + "需要处理:" + (d.message || "去渠道卡重新登录一次"),
      true
    );
  });
  await listen("checkin-activity", (ev) => {
    const d = (ev && ev.payload) || {};
    const manual = d.status === "manual" || (!d.canClaim && d.claimStatus === "CLAIMABLE");
    const where = d.canClaim ? " · 渠道卡里领" : manual ? " · 需在客户端参与" : "";
    // 活动标题在渠道卡与签到中心都看得到,toast 只报「谁 + 什么事 + 去哪办」
    showToast((d.name || "渠道") + (d.canClaim ? " 有活动可领" : " 有新活动") + where);
  });
  // 自动签到领到了(后台静默跑,这一声就是收据);手动点的由按钮回执负责,不重响
  await listen("checkin-claimed", (ev) => {
    const d = (ev && ev.payload) || {};
    const cr = d.credits === null || d.credits === undefined ? "" : " +" + d.credits + " 分";
    beep("ding");
    showToast((d.name || "签到") + " 已领到" + cr);
  });

  // 紧凑条改成可选(默认关,2026-10-23 裁决 D1):老配置若停在紧凑条,
  // 回到首页并提示一次。把 CFG.form 清成 panel 就是"只提示一次"的标记,
  // 不用再加一个"提示已展示"的配置字段。
  if (CFG && !CFG.compactEnabled && CFG.form === "compact") {
    CFG.form = "panel";
    saveCfg("紧凑条设置");
    showToast("紧凑条已改为可选:设置 → 显示 → 启用紧凑条");
  }

  // 启动一律面板形态:安装后(以及每天开机)先看到完整信息,而不是上次留下的
  // 紧凑条 / 胶囊。CFG.form 仍会记录用户的选择,只是不再用于启动恢复。
  await applyForm("panel", false, true);
  updateSizeClass();
  await refresh();
  playEntrance(); // 启动首帧也来一次入场(上面 applyForm 没换形态,不会自动放)

  listen("channels-updated", (ev) => applyChannelsPush(ev.payload));

  // 跌破红线(Rust 判定、一天一次):播一声预警音(开了才有)+ 应用内提示。
  // 判定不放前端 —— 界面与通知必须用同一个口径,否则会出现"红了但没响"。
  listen("crit-alert", (ev) => {
    const d = (ev && ev.payload) || {};
    beep("warn");
    showToast(d.body || d.name + " 余额告急", true);
  });

  // WorkBuddy 网页登录的三种结果都走这个事件:抓到令牌 / 用户自己关了窗 / 失败。
  // 成功时 Rust 已经立刻刷过一轮(channels-updated 会带出新数值),这里只管按钮与提示。
  listen("qoderweb-login", (ev) => {
    QODER_LOGGING = false;
    const p = ev.payload || {};
    const name = p.id === "qoder-cn" ? "Qoder 国内版" : "Qoder";
    // ok + detail = 会话确实被平台接受了,但逐包结构还没认出来:不许说成"失败",
    // 也不许说成"明细已刷新" —— 两句都是假话(2026-11-03 真机,见 core::qoderweb)。
    if (p.ok && p.detail) showToast(p.detail);
    else if (p.ok) showToast(`已登录 ${name} 网页 · 积分明细已刷新`);
    else if (p.cancelled) showToast("已取消登录");
    else showToast(p.reason || "登录没成功,请再试一次", true);
    renderChCards();
    // 详情开着时把按钮状态一起换掉:成功且拿到明细后 qoderLoginBar 会自己消失
    if (DETAIL_ID) renderDetailSheet();
  });
  listen("workbuddy-login", (ev) => {
    WB_LOGGING = false;
    const p = ev.payload || {};
    if (p.ok) showToast("已登录 WorkBuddy · 读数已刷新");
    else if (p.cancelled) showToast("已取消登录");
    else showToast(p.reason || "登录没成功,请再试一次", true);
    invalidateKeyMeta();
    renderChCards();
    fillKeyHints(true);
  });

  // 从托盘/热键唤回窗口:Rust 侧在「隐藏 → 显示」那一刻发 window-shown,这里重放入场。
  // 不能监听 focus / visibilitychange —— 它们分不清"从托盘唤回"和普通 alt-tab,
  // 每次切走再切回都会重播一遍(报告 B8)。
  listen("window-shown", () => {
    // 隐藏期间分钟定时器仍会跑,但窗口可能错过最后一次重绘;唤回时先补一次文案
    tickPeakCountdowns();
    if (currentForm() === "panel") playEntrance();
  });

  // 启动 30 秒后先自动检查一次;之后每 10 分钟再问一次 —— 是否真发请求由
  // Rust 侧 24 小时节流把关(最多每天一次)。之前只有启动那一次,
  // 软件连开几天就再也不会重检(实测:上次检查时间永远停在开机那一刻)。
  setTimeout(() => checkUpdate(false), 30_000);
  setInterval(() => checkUpdate(false), 10 * 60 * 1000);
})();

/* AI 优惠：RSS 摘要与本机已读状态。 */
(() => {
  const key = "promo-feed-v1";
  let state = { items: [], seen: [], updated: 0, initialized: false };
  try { state = { ...state, ...JSON.parse(localStorage.getItem(key) || "{}") }; } catch (_) {}
  let busy = false, failed = false, filter = "all", query = "", scroll = 0;
  const expanded = new Set();
  const save = () => { try { localStorage.setItem(key, JSON.stringify(state)); } catch (_) {} };
  const unread = item => !state.seen.includes(item.id);
  const date = value => { const d = new Date(value); return Number.isNaN(d.getTime()) ? "" : d.toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" }); };
  function entry() {
    const count = state.items.filter(unread).length;
    $("promoEntry").textContent = "AI 优惠" + (count ? " · " + (count > 99 ? "99+" : count) + " 条新" : failed ? " · 更新失败" : "");
    $("promoEntry").classList.toggle("has-new", count > 0);
    $("promoEntry").title = "查看 AI 工具优惠" + (state.updated ? " · 上次更新 " + date(state.updated) : "");
  }
  function render() {
    entry();
    if ($("promoOverlay").hidden) return;
    const content = $("promoContent");
    $("promoHeading").textContent = "AI 优惠";
    $("promoRefresh").disabled = busy;
    $("promoRefresh").textContent = busy ? "更新中…" : "刷新";
    const list = state.items.filter(x => (filter === "all" || unread(x)) && (x.title + " " + x.description).toLowerCase().includes(query.toLowerCase()));
    content.innerHTML = `<div class="promo-tools"><input id="promoSearch" type="search" placeholder="搜索优惠或工具名称" value="${esc(query)}" aria-label="搜索优惠"><div><button class="btn ${filter === "all" ? "p" : ""}" data-filter="all">全部</button> <button class="btn ${filter === "unread" ? "p" : ""}" data-filter="unread">未读</button></div></div>${failed ? `<p class="promo-meta">更新失败${state.updated ? " · 上次更新 " + esc(date(state.updated)) : "，请检查网络后重试"}</p>` : ""}<div class="promo-list">${list.map(x => `<article class="promo-item" data-id="${esc(x.id)}"><button class="promo-title" data-open="${esc(x.id)}">${unread(x) ? '<span class="promo-dot"></span>' : ""}${esc(x.title)} ↗</button><p class="promo-text${expanded.has(x.id) ? " expanded" : ""}">${esc(x.description)}</p><button class="promo-toggle" data-toggle="${esc(x.id)}" aria-expanded="${expanded.has(x.id)}" hidden>${expanded.has(x.id) ? "收起" : "展开"}</button><small class="promo-date">发布于 ${esc(date(x.date)) || "时间未知"}</small></article>`).join("") || `<p class="promo-empty">${busy && !state.items.length ? "正在加载优惠…" : filter === "unread" && !query ? "暂无未读优惠" : "没有匹配的优惠"}</p>`}</div>`;
    content.querySelectorAll(".promo-item").forEach(row => {
      const text = row.querySelector(".promo-text"), toggle = row.querySelector(".promo-toggle");
      const isExpanded = text.classList.contains("expanded");
      if (isExpanded) text.classList.remove("expanded");
      toggle.hidden = text.scrollHeight <= text.clientHeight + 1;
      if (isExpanded) text.classList.add("expanded");
    });
    $("promoSearch").oninput = e => { query = e.target.value; const start = e.target.selectionStart; render(); $("promoSearch").focus(); $("promoSearch").setSelectionRange(start, start); };
    content.querySelectorAll("[data-filter]").forEach(b => b.onclick = () => { filter = b.dataset.filter; scroll = 0; render(); });
    content.querySelectorAll("[data-open]").forEach(b => b.onclick = async () => {
      const item = state.items.find(x => x.id === b.dataset.open);
      if (!item) return;
      try {
        await invoke("open_promo_url", { url: item.link });
        scroll = content.scrollTop;
        if (unread(item)) state.seen.push(item.id);
        save(); render();
      } catch (_) { showToast("无法打开详情，请重试", true); }
    });
    content.querySelectorAll("[data-toggle]").forEach(b => b.onclick = () => {
      scroll = content.scrollTop;
      if (expanded.has(b.dataset.toggle)) expanded.delete(b.dataset.toggle); else expanded.add(b.dataset.toggle);
      render();
    });
    content.scrollTop = scroll;
  }
  async function refresh() {
    if (busy) return;
    busy = true; render();
    try {
      const xml = new DOMParser().parseFromString(await invoke("fetch_promo_feed"), "application/xml");
      if (xml.querySelector("parsererror") || !xml.querySelector("channel")) throw Error("invalid feed");
      const field = (node, name) => node.querySelector(name)?.textContent?.trim() || "";
      const items = Array.from(xml.querySelectorAll("channel > item")).map(n => ({ title: field(n, "title"), description: field(n, "description"), link: field(n, "link"), id: field(n, "guid") || field(n, "link"), date: field(n, "pubDate") })).filter(x => x.id && x.title && x.link);
      if (!items.length) throw Error("empty feed");
      if (!state.initialized) state.seen = items.map(x => x.id);
      state.items = items.sort((a, b) => (Date.parse(b.date) || 0) - (Date.parse(a.date) || 0));
      state.seen = state.seen.filter(id => items.some(x => x.id === id));
      state.initialized = true; state.updated = Date.now(); failed = false; save();
    } catch (_) { failed = true; }
    busy = false; render();
  }
  $("promoEntry").onclick = () => { $("promoOverlay").hidden = false; render(); if (!state.updated || Date.now() - state.updated > 1800000) refresh(); };
  $("promoBack").onclick = () => { $("promoOverlay").hidden = true; $("promoEntry").focus(); };
  $("promoRefresh").onclick = refresh;
  document.addEventListener("keydown", e => { if (e.key === "Escape" && !$("promoOverlay").hidden) $("promoBack").click(); });
  entry();
  if (!state.updated || Date.now() - state.updated > 1800000) setTimeout(refresh, 1000);
  setInterval(refresh, 1800000);
})();


// 本地模型网关：不把上游密钥传到页面。
let GW = null;
let GW_BUSY = false;
async function loadGateway() {
  try { GW = await invoke("gateway_status"); renderGateway(); }
  catch (e) { $("gatewayBox").innerHTML = '<div class="empty">读取网关失败：' + esc(String(e)) + '</div>'; }
}
function renderGateway() {
  const box = $("gatewayBox"); if (!box || !GW) return;
  const items = (GW.channels || []).map(c => { const enabled = (c.availableModels || []).filter(m => !c.allowedModels || c.allowedModels.includes(m)); return {...c, count: enabled.length, models: enabled.map(m => `${c.alias}/${m}`)}; }), vault = GW.vault || [];
  const rows = items.map(c => `<article class="gw-channel"><div class="gw-channel-main"><div class="gw-channel-heading"><div><b>${esc(c.label || c.alias)}</b><span class="gw-channel-meta">${esc(c.platform)} · ${esc(c.source)} / ${esc(c.alias)}</span></div><span class="gw-count ${c.stale ? "is-stale" : ""}">${c.stale ? "待刷新" : `${c.count} 个模型`}</span></div></div><div class="gw-channel-actions"><button class="btn" data-gw="refresh" data-id="${esc(c.vaultId)}">刷新列表</button><button class="btn" data-gw="remove" data-id="${esc(c.vaultId)}">下架</button></div><details class="gw-details"><summary>分组设置 <span>代理与可用模型</span></summary><div class="gw-details-body"><div class="gw-proxy-setting"><label><input type="checkbox" data-gw-proxy="${esc(c.vaultId)}" ${c.systemProxy ? "checked" : ""}> 使用系统代理</label><button class="btn" data-gw="save-proxy" data-id="${esc(c.vaultId)}">保存代理设置</button><p class="khint">跟随电脑的系统代理及绕过规则；未配置或命中绕过规则时会直连。暂不支持 PAC 自动配置。</p></div><div class="gw-model-config"><div class="gw-model-config-head"><b>可用模型</b><span>勾选后可通过网关调用</span></div><div class="gw-model-checklist"><label class="gw-model-check gw-model-all"><input type="checkbox" data-gw-all="${esc(c.vaultId)}" ${(c.availableModels || []).length && (c.availableModels || []).every(m => !c.allowedModels || c.allowedModels.includes(m)) ? "checked" : ""}> 全选</label>${(c.availableModels || []).map(m => `<label class="gw-model-check"><input type="checkbox" data-gw-model="${esc(c.vaultId)}" value="${esc(m)}" ${!c.allowedModels || c.allowedModels.includes(m) ? "checked" : ""}> <span>${esc(m)}</span></label>`).join("")}</div><button class="btn p gw-save-models" data-gw="save-models" data-id="${esc(c.vaultId)}">保存可用模型</button></div></div></details></article>`).join("");
  const modelRows = items.filter(c => !c.stale).flatMap(c => c.models).map(id => `<div class="gw-model">${esc(id)}</div>`).join("");
  box.innerHTML = `<div class="gw-shell">
    <section class="gw-overview">
      <div class="gw-overview-head"><div><b>网关状态</b><span class="gw-running ${GW.running ? "is-running" : "is-stopped"}"><i></i>${GW.running ? "运行中" : "已停止"}</span></div><button class="btn p" data-gw="${GW.running ? "stop" : "start"}" ${GW_BUSY ? "disabled" : ""}>${GW.running ? "停止网关" : "启动网关"}</button></div>
      ${GW.startupError ? `<p class="gw-error">启动失败：${esc(GW.startupError)}</p>` : ""}
      <div class="gw-endpoints"><div class="gw-section-head"><b>连接地址</b></div>
        <div class="gw-endpoint-row"><div class="gw-endpoint-label"><span class="gw-endpoint-dot"></span><span>本机</span></div><code>http://127.0.0.1:${GW.port}/v1</code><button class="btn" data-gw="copy-local">复制</button></div>
        ${GW.lan ? `<div class="gw-lan"><label for="gwLanIp">局域网网卡</label><span class="gw-lan-hint">选择与手机同一局域网的网卡</span>${(GW.lanIps || []).length ? `<select class="sel" id="gwLanIp">${GW.lanIps.map(i => `<option value="${esc(i.address)}" data-iface="${esc(i.name)}">${esc(i.name)} · ${esc(i.address)}</option>`).join("")}</select><div class="gw-endpoint-row"><div class="gw-endpoint-label"><span class="gw-endpoint-dot is-lan"></span><span>局域网</span></div><code id="gwLanEndpoint"></code><button class="btn" data-gw="copy-lan">复制</button></div>` : `<p class="gw-no-ip">未检测到可用的局域网地址</p>`}</div>` : ""}
      </div>
      <div class="gw-overview-actions"><span>网关密钥用于客户端身份验证</span><div><button class="btn" data-gw="copy-secret">复制密钥</button><button class="btn" data-gw="rotate-secret">重新生成</button></div></div>
    </section>
    <section class="gw-section"><div class="gw-section-head"><b>访问设置</b><span>重启网关后应用端口与网络设置</span></div>
      <div class="gw-settings-grid"><label class="gw-setting"><span><b>监听端口</b><small>默认 18771</small></span><input class="sel" id="gwPort" type="number" min="1" max="65535" value="${GW.port}" ${GW.running ? "disabled" : ""}></label>
        <label class="gw-setting"><span><b>允许局域网访问</b><small>开启后自动要求网关密钥</small></span><input id="gwLan" type="checkbox" ${GW.lan ? "checked" : ""} ${GW.running ? "disabled" : ""}></label>
        <label class="gw-setting"><span><b>需要网关密钥</b><small>保护本机端点的调用</small></span><input id="gwAuth" type="checkbox" ${GW.auth ? "checked" : ""} ${GW.running || GW.lan ? "disabled" : ""}></label>
      </div>
      ${GW.lan ? `<p class="gw-footnote">仅在可信局域网使用。其他设备无法连接时请检查 Windows 防火墙；HTTP 传输未加密。</p>` : ""}
    </section>
    <section class="gw-section"><div class="gw-section-head"><b>已上架分组</b><span>${items.length} 个密钥分组</span></div>${rows || `<div class="gw-empty"><span>尚无已上架分组</span><small>从密钥箱选择一个分组并发布模型</small></div>`}</section>
    <section class="gw-section gw-add-section"><div class="gw-section-head"><b>添加分组</b><span>选择密钥并设置调用别名</span></div><div class="gw-add"><label class="gw-add-field"><span>平台</span><select class="sel" id="gwPlatform"><option value="">选择平台</option>${[...new Set(vault.filter(v => !items.some(c => c.vaultId === v.id)).map(v => v.platform))].map(p => `<option value="${esc(p)}">${esc(p)}</option>`).join("")}</select></label><label class="gw-add-field"><span>密钥</span><select class="sel" id="gwVault"><option value="">选择密钥箱条目</option></select></label><label class="gw-add-field"><span>模型来源</span><select class="sel" id="gwSource"><option value="hapi">Hapi</option><option value="gemini">Gemini</option><option value="inkstone">书生端砚</option></select></label><label class="gw-add-field"><span>英文别名</span><input class="inp" id="gwAlias" placeholder="例如 main" maxlength="32"></label><label class="gw-proxy-new"><input type="checkbox" id="gwNewProxy"> 使用系统代理</label><button class="btn p" data-gw="publish" ${GW_BUSY ? "disabled" : ""}>上架分组</button></div>
      <p class="gw-footnote">上架会读取该密钥的模型清单；下架不会删除密钥。模型实际权限以服务商调用结果为准。</p>
    </section>
    <details class="gw-model-catalog"><summary><span>网关模型目录</span><small>${items.filter(c=>!c.stale).reduce((n,c)=>n+c.count,0)} 个可调用模型</small></summary><div class="gw-models">${modelRows || `<span class="gw-empty-note">暂无模型</span>`}</div></details>
  </div>`;
  if ($("gwLanIp")) {
    const select = $("gwLanIp");
    const saved = localStorage.getItem("gatewayLanInterface");
    const selected = [...select.options].find(o => o.dataset.iface === saved);
    if (selected) select.selectedIndex = selected.index;
    const updateAddress = () => { $("gwLanEndpoint").textContent = `http://${select.value}:${GW.port}/v1`; };
    select.onchange = () => { localStorage.setItem("gatewayLanInterface", select.selectedOptions[0].dataset.iface); updateAddress(); };
    updateAddress();
  }
  box.querySelectorAll("[data-gw-all]").forEach(all => { all.onchange = () => boxModels(all.dataset.gwAll).forEach(input => { input.checked = all.checked; }); });
  box.querySelectorAll("[data-gw-model]").forEach(input => { input.onchange = () => { const all = [...box.querySelectorAll("[data-gw-all]")].find(x => x.dataset.gwAll === input.dataset.gwModel); const models = boxModels(input.dataset.gwModel); all.checked = models.every(x => x.checked); all.indeterminate = !all.checked && models.some(x => x.checked); }; });
  const lan = $("gwLan"), auth = $("gwAuth"); lan.onchange = () => { if (lan.checked) auth.checked = true; auth.disabled = lan.checked || GW.running; };
  $("gwPlatform").onchange = e => { const matches = vault.filter(v => v.platform === e.target.value && !items.some(c => c.vaultId === v.id)); $("gwVault").innerHTML = `<option value="">选择密钥箱条目</option>` + matches.map(v => `<option value="${esc(v.id)}">${esc(v.label)} ${esc(v.hint)}</option>`).join(""); if (matches.length === 1) { $("gwVault").value = matches[0].id; $("gwVault").dispatchEvent(new Event("change")); } };
  $("gwVault").onchange = e => { const item = vault.find(v => v.id === e.target.value); if (!item) return; const p = item.platform.toLowerCase(); if (p.includes("gemini")) $("gwSource").value = "gemini"; else if (p.includes("书生") || p.includes("端砚")) $("gwSource").value = "inkstone"; else if (p.includes("hapi")) $("gwSource").value = "hapi"; };
}
function boxModels(id) { return [...document.querySelectorAll("[data-gw-model]")].filter(x => x.dataset.gwModel === id); }
$("gatewayBox").addEventListener("click", async e => {
  const b = e.target.closest("[data-gw]"); if (!b || GW_BUSY) return; const action = b.dataset.gw;
  try {
    if (action === "copy-local" || action === "copy-lan") { const ip = action === "copy-local" ? "127.0.0.1" : $("gwLanIp").value; await invoke("copy_text", {text:`http://${ip}:${GW.port}/v1`}); showToast("地址已复制"); return; }
    if (action === "copy-secret" || action === "rotate-secret") { if (action === "rotate-secret" && !confirm("重新生成后旧网关密钥立即失效，继续吗？")) return; const text = await invoke("gateway_secret", {rotate:action === "rotate-secret"}); await invoke("copy_text", {text}); showToast("网关密钥已复制"); return; }
    GW_BUSY = true; b.disabled = true;
    if (action === "start") { const port=Number($("gwPort").value); if (!Number.isInteger(port) || port < 1 || port > 65535) throw Error("请输入 1 到 65535 的端口"); await invoke("gateway_start", {port, lan:$("gwLan").checked, auth:$("gwAuth").checked}); }
    if (action === "save-proxy") { const input = [...document.querySelectorAll("[data-gw-proxy]")].find(x => x.dataset.gwProxy === b.dataset.id); await invoke("gateway_set_proxy", {vaultId:b.dataset.id, systemProxy:input.checked}); }
    if (action === "save-models") await invoke("gateway_set_models", {vaultId:b.dataset.id, models:[...boxModels(b.dataset.id)].filter(x=>x.checked).map(x=>x.value)});
    if (action === "stop") await invoke("gateway_stop");
    if (action === "remove") await invoke("gateway_unpublish", {vaultId:b.dataset.id});
    if (action === "publish" || action === "refresh") { const c = action === "refresh" ? GW.channels.find(x=>x.vaultId===b.dataset.id) : null; const vaultId=c ? c.vaultId : $("gwVault").value, source=c ? c.source : $("gwSource").value, alias=c ? c.alias : $("gwAlias").value; if (!vaultId || !alias.trim()) throw Error("请选择密钥并填写英文别名"); await invoke("gateway_publish", {vaultId,source,alias,systemProxy:c ? null : $("gwNewProxy").checked}); }
    GW_BUSY = false; await loadGateway();
  } catch (err) { showToast(String(err), true); b.disabled = false; }
  finally { GW_BUSY = false; }
});
