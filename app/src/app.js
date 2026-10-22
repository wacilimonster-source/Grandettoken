/* TokenScope 前端。经 window.__TAURI__ 调 Rust 命令(withGlobalTauri)。 */
const T = window.__TAURI__;
const invoke = T.core.invoke;
const listen = T.event.listen;
const appWindow = T.window.getCurrentWindow();

const $ = (id) => document.getElementById(id);

let CHANNELS = [];
let CFG = null;
let VER = "";          // app_version 命令回来的版本号,关于页在首检之前也有的显示
let sparkCache = {};   // { [id]: { values:[], gaps:[], source:"detail"|"daily" } }
let sparkHours = {};   // { [id]: 当前选中的区间 },渲染详情时据此标记选中项
let changesCache = {}; // { [id]: 最近变动台账(展开时按需取,不进轮询) }
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

function relTime(ts) {
  if (!ts) return "从未";
  const s = Math.floor(Date.now() / 1000) - ts;
  if (s < 60) return "刚刚";
  if (s < 3600) return Math.floor(s / 60) + " 分钟前";
  if (s < 86400) return Math.floor(s / 3600) + " 小时前";
  return Math.floor(s / 86400) + " 天前";
}

/**
 * 配额型渠道的主窗口:后端用 `main` 标出来(OpenCode = 「周期」窗,Codex = primary),
 * 大数字与状态色都按它走。老数据没有这个标志时退回「周期」标签,再退到数组首位。
 */
function mainWindow(c) {
  const wins = c.windows || [];
  if (!wins.length) return null;
  return wins.find((w) => w.main) || wins.find((w) => w.label === "周期") || wins[0];
}

/** ISO 8601(UTC)的 resetsAt → 毫秒;缺失/异常一律 null,绝不猜。 */
function resetMs(w) {
  if (!w || !w.resetsAt) return null;
  const t = Date.parse(w.resetsAt);
  return Number.isNaN(t) ? null : t;
}

/** 台钟 HH:MM(缺口文案用:「今日数据自 10:23 起」)。 */
function fmtClock(ts) {
  const d = new Date(ts);
  return String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
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

// 1 秒一跳只改文案,不重绘整行 —— 一条消息管一天,不为倒计时加压轮询频率。
setInterval(() => {
  const nowMs = Date.now();
  document.querySelectorAll("[data-peakcd]").forEach((el) => {
    let p = null;
    try {
      p = JSON.parse(el.dataset.peakcd);
    } catch (e) {
      return;
    }
    el.textContent = peakCountdown(p, nowMs);
  });
}, 1000);

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
  return short ? d + " 天 " + rh + " 小时" : d + " 天 " + rh + " 小时后重置";
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
// 与 styles.css 的高对比色板保持一致(进度条/托盘角标是内联色,吃不到 CSS 变量)
const TONE_HEX = { ok: "#4ade9d", warn: "#f7c355", bad: "#ff6b63", off: "#3a4356" };

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
};

/**
 * 渠道图标。统一入口:列表行 / 密钥行 / 额度申请行都用它。
 * off = 未配置或取数失败 —— 字母方块变灰,官方标压暗去色,语义保持一致。
 * 官方标同时内嵌一个字母方块副本(.ltr):紧凑条 16px 档位位图会糊,
 * 由 CSS 切换到字母显示(U6),面板/管理页仍显示官方标。
 */
function iconHtml(c, cls = "ico", off = false) {
  const src = LOGOS[c.id];
  if (!src) {
    return `<div class="${cls}" style="background:${off ? "#3c445c" : c.color}">${esc(c.short)}</div>`;
  }
  return `<div class="${cls} logo${off ? " off" : ""}" style="--cc:${off ? "#3c445c" : c.color}"><img src="${src}" alt="${esc(c.short)}"><span class="ltr">${esc(c.short)}</span></div>`;
}

/** 自定义排序下的完整顺序:配置里列出的 + 未列入的(附在末尾,新增渠道不会丢)。 */
function currentOrderIds() {
  const order = (CFG && CFG.channelOrder) || [];
  return [
    ...order.filter((id) => CHANNELS.some((c) => c.id === id)),
    ...CHANNELS.filter((c) => !order.includes(c.id)).map((c) => c.id),
  ];
}

/** 主排序:剩余百分比升序(默认)/ 余额 / 今日消耗 / 自定义。 */
function sortChannels(list) {
  const mode = CFG.sort;
  if (mode === "custom") {
    // 自定义顺序就是用户排的顺序,不做任何干预(失败/未配置也按排的位置显示)
    const order = currentOrderIds();
    const rank = (c) => {
      const i = order.indexOf(c.id);
      return i < 0 ? 9999 : i;
    };
    return [...list].sort((a, b) => rank(a) - rank(b));
  }
  const score = (c) => {
    if (!c.hasKey) return 9999;
    if (!c.valid) return 5000;
    // 积分和金额/百分比不是一回事,不能混进同一条数轴比大小:
    // 统一排在"可比渠道之后、取数失败之前"
    if (c.kind === "points") return 4000;
    if (mode === "balance") return -(c.remaining ?? 0);
    if (mode === "dayUsage") return -(c.day ?? 0);
    const r = remainRatio(c);
    return r === null ? 4000 : r * 100;
  };
  return [...list].sort((a, b) => score(a) - score(b));
}

// ───────────── 统一视图(core::view::HomeView)─────────────
//
// 四种形态以前各自解释一次 `valid / limited / updatedAt`,同一个渠道能给出三种结论
// (设计稿 D01/D02/D03)。判定现在收在后端 `view.rs` 里,前端只保留一份结果:
// 排序、色调、主数字、陈旧标记都从它读。取不到(旧版后端 / IPC 失败)时,
// 下面的每个读取口都会退回原来的本地判断,行为不回退。
let HV = { byId: {}, order: [], risk: null, checkin: null, actionCount: 0, staleCount: 0, errorCount: 0 };

/** 收下一份后端视图。失败时保留上一份 —— 界面退回旧判断,而不是变空白。 */
function hvApply(v) {
  const rows = (v && v.rows) || [];
  const byId = {};
  for (const r of rows) byId[r.id] = r;
  HV = {
    byId,
    order: (v && v.order) || [],
    risk: (v && v.risk) || null,
    checkin: (v && v.checkin) || null,
    // 风险带第一/二格要的合计(总余额 / 今日消耗)。没有它 renderRiskBand 会退回老汇总。
    totals: (v && v.totals) || null,
    actionCount: (v && v.actionCount) || 0,
    staleCount: (v && v.staleCount) || 0,
    errorCount: (v && v.errorCount) || 0,
  };
}
const hvRow = (id) => HV.byId[id] || null;

/** 渠道过滤(P3):渠道一多就只能靠滚,十二个以上才把输入框放出来,平时不占地方。 */
let CH_Q = "";
function chFilter(c) {
  if (!CH_Q) return true;
  const q = CH_Q.toLowerCase();
  return (
    (c.name || "").toLowerCase().includes(q) ||
    (c.short || "").toLowerCase().includes(q) ||
    (c.id || "").toLowerCase().includes(q)
  );
}
const HV_TONE = { ok: "ok", warn: "warn", bad: "bad", off: "off" };

/** 色调:后端给了就照它画 —— 面板 / 紧凑条 / 胶囊 / 托盘同一个结论。 */
function hvTone(c) {
  const r = hvRow(c.id);
  return r ? HV_TONE[r.tone] || toneOf(c) : toneOf(c);
}

/** 排序:后端排好的顺序优先(五桶紧迫度 / 自定义 / 余额 / 消耗)。
 *  没拿到视图时退回 `sortChannels`,行为与以前一致。 */
function hvSort(list) {
  if (!HV.order.length) return sortChannels(list);
  const rank = new Map(HV.order.map((id, i) => [id, i]));
  return [...list].sort(
    (a, b) =>
      (rank.get(a.id) ?? 9999) - (rank.get(b.id) ?? 9999) ||
      (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
  );
}

// ───────────── 渲染 ─────────────
function renderSummary() {
  // 汇总只累加金额型渠道 —— 百分比和金额不能相加;隐藏的渠道不参与;
  // 币种不是 CNY 的也不参与(DeepSeek 可能返回非人民币余额,混进来就是把美元当人民币加)
  const am = CHANNELS.filter(
    (c) =>
      c.kind === "amount" &&
      c.hasKey &&
      c.remaining !== null &&
      !c.hidden &&
      (c.unit || "CNY") === "CNY"
  );
  // 订阅型套餐(OpenCode Go $10/期)是另一笔:它不是余额差值,但这个周期确实掏了这笔钱。
  // 后端按天摊好(周期长度从真实重置里学,汇率每月自动取),这里只管加进三格并说明口径。
  // **必须带 hasKey**:后端会把 plan 连没有密钥的占位视图一起挂上,只按 plan 过滤时
  // 全新安装(只配了 4SAPI)会凭空多出「OpenCode Go 套餐摊销」那笔钱(F1)。
  const plans = CHANNELS.filter((c) => !c.hidden && c.hasKey && c.plan);
  const planSum = (f) => plans.reduce((a, c) => a + (c.plan[f] || 0), 0);
  const p0 = plans.length ? plans[0].plan : null;
  const planTip = p0
    ? ` · 含套餐 $${p0.usd.toFixed(0)}/期 按天摊 ${money(p0.perDay)}/天` +
      `(周期 ${p0.days.toFixed(1)} 天${p0.learned ? "·实测" : "·按上一期推"},汇率 ${p0.rate}·${p0.rateMonth})`
    : "";
  // 被排除在 CNY 合计之外的非人民币渠道要点名:合计里少了谁,用户有权知道(F12)
  const offCur = CHANNELS.filter(
    (c) => c.kind === "amount" && c.hasKey && c.remaining !== null && !c.hidden && !am.includes(c)
  );
  const offTip = offCur.length
    ? " · " + offCur.map((c) => `${c.name}(${c.unit || "?"})不是人民币,未计入`).join("、")
    : "";
  const sum = (f) => {
    const vals = am.map((c) => c[f]).filter((v) => v !== null && v !== undefined);
    if (!vals.length) return null;
    return vals.reduce((a, b) => a + b, 0);
  };
  // 每格显式带上自己的字段名与比对口径 —— 不要拿"是否推算"去推字段名。
  // prev* = 上一周期同时段(U8 环比):今日 vs 昨日同时段,以此类推
  const cells = [
    { label: "今日消耗", field: "day", prev: "dayPrev", cmp: "对比昨日同时段" },
    { label: "本周消耗", field: "week", prev: "weekPrev", cmp: "对比上周同时段" },
    { label: "本月消耗", field: "month", prev: "monthPrev", cmp: "对比上月同时段" },
  ];
  // 一格数都没有时不留空条 —— 但**只有非人民币渠道**时要把条留着:整条消失
  // 看起来像挂件坏了,三格「——」才诚实(G1);谁没被计入放在 tooltip 里点名。
  // 无论如何不做跨币种求和。
  if (!am.length && !plans.length && !offCur.length) {
    $("sum").innerHTML = "";
    return;
  }
  // 面上只留「标签 + 金额」,干干净净;口径 / 环比 / 套餐摊销 / 非人民币点名
  // 全部收进 title —— 之前塞在金额后面,窄窗 + 大字号下被 ellipsis 截成
  // 「¥123…」,钱和说明两头都看不清(P3-31)。
  $("sum").innerHTML = cells
    .map(({ label, field, prev, cmp }) => {
      const v = sum(field);
      const add = plans.length ? planSum(field) : 0;
      // 金额型这一格没数时,套餐那一笔仍然要显示(否则"今日"会因为一家接口挂了而空掉)
      const total = v === null ? (add > 0 ? add : null) : v + add;
      const txt = total === null ? "——" : money(total);
      // 环比只在"本期与上期都有数"的渠道集合上算,两边口径才对称;
      // 套餐那一笔两边都加(按天摊,只会让比例更平);上期无基线时如实说
      const both = am.filter((c) => c[field] != null && c[prev] != null);
      let cur2 = both.reduce((a, c) => a + c[field], 0);
      let prev2 = both.reduce((a, c) => a + c[prev], 0);
      if (add > 0) {
        cur2 += add;
        prev2 += planSum(prev);
      }
      const hasBase = (both.length || add > 0) && prev2 > 0;
      const tip =
        `${cmp}:本期 ${txt}` +
        (hasBase
          ? ` · 上期 ${money(prev2)} · 环比 ${(((cur2 - prev2) / prev2) * 100).toFixed(1)}%`
          : " · 上期无同期基线") +
        planTip +
        offTip;
      return `<div class="cell" title="${esc(tip)}"><div class="lb">${label}</div>
        <div class="vv">${txt}</div></div>`;
    })
    .join("");
}

function renderRow(c, i) {
  const tone = hvTone(c); // 与其余三种形态同源(设计 P3 · 状态同源)
  const noKey = !c.hasKey;
  const failed = c.hasKey && !c.valid;
  const r = remainRatio(c);
  const pct = r === null ? null : Math.round(r * 100);
  const isPct = c.kind === "percent";
  const isPts = c.kind === "points";
  // 配额型(OpenCode Go):大数字固定取「周期」窗的剩余(实测锚定开通日,旧称「本月」),
  // 下面三行各带一条迷你条把 5 小时 / 本周 / 周期都摊开,右侧附重置倒计时
  const wins = isPct && c.windows && c.windows.length ? c.windows : [];
  const mw = wins.length ? mainWindow(c) : null;

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

  // 积分型:副标题说清"近期会过期多少",这是这个渠道最该被看见的信息
  const soon = c.expiringSoon || null;
  const soonDays = soon ? daysUntil(soon.at) : null;

  // 第二行给状态语义,大数字下方给数值口径,两处不重复
  // 副标题只留"状态语义":渠道是否可用。其余说明文字一律不写
  // (未公开接口、哪个窗口最紧 —— 明细里都有,不必占一行)
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
  else if (failed) sub = "取数失败";
  else if (wins.length) {
    // Codex 把套餐放在 extra 里(裁决②:副标题带 free/plus/pro 徽章);OpenCode 无此键,维持空白
    const plan = (c.extra || []).find((e) => e[0] === "套餐");
    sub = plan ? `ChatGPT 订阅 · <span class="plan">${esc(plan[1])}</span>` : "";
  }
  else if (c.limited) sub = "已限流";
  else if (isPts) {
    sub = soon
      ? `近 30 天将过期 ${points(soon.amount)} 分`
      : c.expiring && c.expiring.length
        ? "30 天内无到期"
        : "近期无到期";
  } else if (claim) sub = `本轮额度 ${esc(money(claim.amount))}${claimTail(claim)}`;
  // 以前没开申请制时这里显示「额度 ¥{c.total}」:c.total 是**累计发放**(只增不减),
  // 不是额度上限 → "剩 21%" 会随每次充值一路下滑,与还能用多少钱毫无关系(报告 P1-11)。
  // 峰谷渠道(DeepSeek):第二行如实说"现在贵不贵",只报状态不报单价(规则会变)
  else if (c.peak) sub = esc(peakParts(c).label);
  else sub = ""; // 金额型拿到多少就是可用多少,不加说明

  let subVal;
  if (noKey)
    subVal = c.authSource === "app" ? "需打开应用" : c.authSource === "web" ? "待登录" : "待配置";
  else if (failed) subVal = c.stale ? "上次快照" : "失联";
  else if (mw) subVal = `${esc(mw.label)}窗 · ${fmtReset(resetMs(mw), true)}`;
  else if (isPts) subVal = soon ? `最近 ${fmtDay(soon.at)} 到期` : "无近期到期";
  // 峰谷倒计时交给 1 秒一跳的 ticker 填,不用等下一轮取数
  else if (c.peak)
    subVal = `<span data-peakcd='${JSON.stringify(c.peak)}'>${esc(
      peakCountdown(c.peak, Date.now())
    )}</span>`;
  else if (pct !== null) subVal = `剩 ${pct}%`;
  else subVal = "";

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

  // 第三段:金额型显示日/周/月;配额型显示三个限流窗口;积分型显示最近的几笔到期
  let useHtml;
  if (isPts) {
    const list = (c.expiring || []).filter((e) => e.at > 0 && daysUntil(e.at) >= 0);
    const head = list.slice(0, 3);
    useHtml = head.length
      ? `<div class="exps">${head
          .map((e) => {
            const d = daysUntil(e.at);
            const cls = expClass(d);
            return `<div class="exp${cls ? " " + cls : ""}">
              <span class="ed">${fmtDay(e.at)}<i>${d === 0 ? "今天" : "剩 " + d + " 天"}</i></span>
              <b class="ea">${points(e.amount)} 分</b>
              <span class="el">${esc(e.label)}</span>
            </div>`;
          })
          .join("")}${
          list.length > head.length
            ? `<div class="expmore">另有 ${list.length - head.length} 笔更晚到期,展开看全部</div>`
            : ""
        }</div>`
      : `<div class="exps"><div class="expmore">没有待用积分:所有额度包都已用完或已过期</div></div>`;
  } else if (wins.length) {
    useHtml = `<div class="wins">${wins
      .map((w) => {
        const wr = Math.max(0, Math.min(100, w.remainPercent));
        const lim = w.status === "rate-limited";
        const wt = lim ? "bad" : wr < CFG.critPercent ? "bad" : wr < CFG.warnPercent ? "warn" : "ok";
        const ms = resetMs(w);
        // 倒计时是中性信息用次级色;唯一例外:限流中且 1 小时内重置 → 绿色「马上恢复」
        const soon = lim && ms !== null && ms - Date.now() < 3600e3;
        const cd = soon ? '<span class="wt ok">马上恢复</span>' : `<span class="wt">${fmtReset(ms, true)}</span>`;
        return `<div class="win">
          <span class="wl">${esc(w.label)}</span>
          <i class="wb"><i style="width:${wr}%;background:${TONE_HEX[wt]}"></i></i>
          <b class="wv" style="color:${lim ? "var(--bad)" : TONE_COLOR[wt]}">${wr.toFixed(1)}%</b>
          ${cd}
        </div>`;
      })
      .join("")}</div>`;
  } else {
    const f = (v) => money(v, c.unit);
    useHtml = `<div class="use">
      <div>今日<b>${f(c.day)}</b></div>
      <div>本周<b>${f(c.week)}</b></div>
      <div>本月<b>${f(c.month)}</b></div>
    </div>`;
  }

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

  // 「更新于」只在陈旧时说(设计附录 B:`.rhint` 只在 stale 时画)。
  // 新鲜是默认状态,不值得占一行 —— 那一行本来就是给"这是旧快照"用的。
  const hv = hvRow(c.id);
  const isStale = hv ? hv.state === "stale" : !!c.stale;
  const src = isStale
    ? `<div class="rhint stale">显示的是 ${relTime(c.updatedAt)} 的成功快照${
        c.error ? " · " + esc(c.error) : ""
      }</div>`
    : "";

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

  // data-state:后端判的七态,给 CSS 用 —— 紧凑条与胶囊里没有 .rhint 那行字,
  // 只有靠它才能把"这是旧快照"画出来(设计 P0 验收①)
  return `<div class="row${urgent ? " urg" : ""}" data-id="${c.id}" data-dtone="${dcls}" data-state="${(hv || {}).state || ""}">
    <div class="rhead" tabindex="0" role="button" aria-expanded="${rowOpen.has(c.id) ? "true" : "false"}">
      <div class="r1">
        ${iconHtml(c, "ico", noKey || failed)}
        <div class="nm">
          <div class="n"><span class="nn">${esc(c.name)}</span><span class="nsh">${esc(c.short)}</span>${dot}${wmark}${ckb}${qtag}</div>
          <div class="s">${sub}</div>
        </div>
        <div class="val">
          <div class="v" style="color:${TONE_COLOR[tone]}">${main}</div>
          <div class="p">${subVal}</div>
        </div>
        <div class="caret">&#9654;</div>
      </div>
      <div class="r2">${hv && hv.secondary ? esc(hv.secondary) : subVal}</div>
      ${bar}
      <div class="seg3">${useHtml}</div>
      ${claimWarn}
      ${winWarn}
      ${src}
      <span class="sweep"></span>
    </div>
    <div class="rbody" data-body="${c.id}"></div>
  </div>`;
}

function renderDetail(c) {
  const noKey = !c.hasKey;

  // 密钥收纳进「设置与管理」页,行详情只留一个入口 —— 展示与设置分开
  const manageLink = `<div class="hint" style="margin-top:2px">密钥、刷新与阈值在
    <span class="lk" data-act="open-manage" data-id="${c.id}">设置与管理</span> 里配置。</div>`;

  if (noKey) {
    // 网页登录型渠道(WorkBuddy):说清为什么不能自动读 —— 桌面客户端把令牌加密了,
    // 只能在本挂件里登录一次网页账号。别让人去开客户端,那没用。
    if (c.authSource === "web") {
      return `<div class="rbody-in">
        <div class="hint" style="margin:0 0 8px">还没登录 ${esc(
          c.name
        )} 账号。它的积分没有开放 API Key,而桌面客户端 5.6.2 起把本机存的令牌改成加密存储,` +
        `本挂件已读不到;在 <span class="lk" data-act="open-manage" data-id="${c.id}">设置与管理</span> 里点一次「登录 ${esc(
          c.name
        )} 账号」即可,登录后自动关窗,不用你复制任何东西。</div>
        ${manageLink}
      </div>`;
    }
    // 复用本机登录态的渠道:没有"密钥"可填,只能引导用户去客户端登录一次
    if (c.authSource === "app") {
      return `<div class="rbody-in">
        <div class="hint" style="margin:0 0 8px">未检测到 ${esc(
          c.name
        )} 的登录信息。它的积分只能用客户端自己的登录态查询,先打开一次 ${esc(
          c.name
        )} 并登录,再回来刷新即可。本应用只读那份凭据,不写回、也不刷新。</div>
        ${manageLink}
      </div>`;
    }
    return `<div class="rbody-in">
      <div class="hint" style="margin:0 0 8px">未配置密钥,尚未开始取数。密钥只写入 Windows 凭据管理器,不会进配置文件、数据库或日志。</div>
      ${manageLink}
    </div>`;
  }

  // 申请制额度:把周期口径摊开在详情里(列表行只放状态)
  const claimKv = [];
  if (c.claim) {
    const cl = c.claim;
    // 本轮额度是申请制自己的口径(设置页按「元」填),单独用 ¥;
    // 日均消耗与累计发放都来自该渠道的余额序列,跟着渠道的 unit 走
    claimKv.push(["本轮额度", money(cl.amount)]);
    claimKv.push([
      "上次申请",
      cl.lastClaimAt
        ? `${fmtDay(cl.lastClaimAt)}(${cl.source === "auto" ? "自动检测" : "手动"})`
        : "未记录",
    ]);
    claimKv.push([
      "可再申请",
      cl.daysUntilEligible === null || cl.daysUntilEligible === undefined
        ? "——"
        : cl.eligible
          ? "现在可申请"
          : `再等 ${cl.daysUntilEligible} 天`,
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

  const kv = [...claimKv, ...(c.extra && c.extra.length ? c.extra : [])];
  const kvHtml = kv.length
    ? `<div class="kv">${kv
        .map(([k, v]) => `<div><span>${esc(k)}</span><b>${esc(v)}</b></div>`)
        .join("")}</div>`
    : "";

  // 积分型:逐笔到期全量表(到期日 | 剩余 | 来源),近 7/30 天分别红/黄
  let expHtml = "";
  if (c.kind === "points" && c.expiring && c.expiring.length) {
    const rows = c.expiring
      .map((e) => {
        const d = daysUntil(e.at);
        const cls = d !== null && d < 0 ? "past" : expClass(d);
        const tail = d === null ? "" : d < 0 ? "已过期" : d === 0 ? "今天" : `剩 ${d} 天`;
        return `<div class="exrow${cls ? " " + cls : ""}">
          <span class="ed">${fmtDay(e.at)}<i>${tail}</i></span>
          <b class="ea">${points(e.amount)}</b>
          <span class="el">${esc(e.label)}</span>
        </div>`;
      })
      .join("");
    const soon = c.expiringSoon;
    expHtml = `<div class="extab">
      <div class="exhead"><span class="ed">到期日</span><span class="ea">剩余积分</span><span class="el">来源</span></div>
      ${rows}
      <div class="exfoot">共 ${c.expiring.length} 笔有剩余 · 合计 ${points(c.remaining)} 分${
        soon ? ` · 近 30 天将过期 ${points(soon.amount)} 分` : ""
      }</div>
    </div>`;
  }

  // 复用本机登录态的渠道:说清凭据从哪来、为什么不刷新
  const authHint =
    c.authSource === "app"
      ? `<div class="hint" style="margin:0 0 8px">凭据来自本机已登录的 ${esc(
          c.authLabel
        )} · 只读复用,不写回、不刷新;失效时打开一次 ${esc(
          c.name
        )} 即可(刷新会顶掉客户端手里的登录态,把你挤下线)。</div>`
      : c.authSource === "web"
      ? `<div class="hint" style="margin:0 0 8px">登录状态来自你在挂件里登录的 ${esc(
          c.name
        )} 网页账号(会话存在挂件的浏览器里,不写凭据库、也不碰桌面客户端);` +
        `失效时重新登录一次即可,<b>不影响你桌面客户端的登录状态</b>。</div>`
      : "";

  // 曲线:老形状是数组,现在是 { values, gaps, source }(带缺口标注与长期趋势)
  const sv = sparkCache[c.id] || null;
  const sparkVals = sv && sv.values && sv.values.length ? sv.values : [];
  const gapSet = new Set((sv && sv.gaps) || []);
  const sparkHtml = sparkVals.length
    ? `<div class="spark">${(() => {
        const max = Math.max(...sparkVals, 0.000001);
        return sparkVals
          .map(
            (v, i) =>
              `<i class="${gapSet.has(i) ? "gap" : ""}" style="height:${Math.max(
                2,
                (v / max) * 100
              )}%"></i>`
          )
          .join("");
      })()}</div>${
        gapSet.size
          ? `<div class="hint" style="margin:2px 0 0">斜纹的柱子:那段时间挂件没在运行,数字只算了一半 —— 不是"那几天没花钱"。</div>`
          : ""
      }${
        sv && sv.source === "daily"
          ? `<div class="hint" style="margin:2px 0 0">更早的部分由日级摘要拼成(90 天明细之外),一天一格。</div>`
          : ""
      }`
    : `<div class="spark">${Array.from({ length: 24 }, () => '<i style="height:2%"></i>').join(
        ""
      )}</div>`;

  // 配额型:逐窗明细(剩余% + 相对倒计时 + 本地绝对时间)
  const winKv =
    c.kind === "percent" && (c.windows || []).length
      ? `<div class="kv">${c.windows
          .map((w) => {
            const ms = resetMs(w);
            const rem = Math.max(0, Math.min(100, w.remainPercent));
            const abs = ms === null ? "重置时间未知" : `${fmtReset(ms, false)} · ${new Date(ms).toLocaleString("zh-CN", { hour12: false })}`;
            return `<div><span>${esc(w.label)}窗 · 剩 ${rem.toFixed(1)}%</span><b>${abs}</b></div>`;
          })
          .join("")}</div>`
      : "";

  const body = [];
  body.push(
    `<div class="seg">
       ${[[24, "近 24 小时"], [168, "近 7 天"], [720, "近 30 天"], [2160, "近 3 月"], [8760, "近 12 月"]]
         .map(
           ([h, label]) =>
             `<button class="${(sparkHours[c.id] || 24) === h ? "on" : ""}" data-act="range" data-id="${c.id}" data-h="${h}">${label}</button>`
         )
         .join("")}
     </div>`
  );
  if (c.error) {
    body.push(
      `<div class="hint" style="color:var(--bad);margin:0 0 8px">${esc(c.error)}${
        c.stale ? " · 下列数值为上次成功快照" : ""
      }</div>`
    );
  }
  body.push(kvHtml);
  body.push(winKv);
  body.push(expHtml);
  body.push(sparkHtml);
  // 观测缺口:今天的数据不是从零点开始的,如实说出来 ——
  // 否则"今日消耗偏小"永远没有解释,用户只会觉得数字不准
  if (c.gapDayFrom) {
    body.push(
      `<div class="hint" style="margin:8px 0 0">今日数据自 <b>${fmtClock(
        c.gapDayFrom
      )}</b> 起 —— 此前挂件没在运行,今日消耗只覆盖之后的时段(不是没花钱)。</div>`
    );
  }
  // 变动台账:同方向的连续轮询已聚合成一条(轮询是分钟级的,逐条列出来是噪音);
  // 空档秒数如实标注 —— 一条 +50 里如果揉了几小时的空白,得让用户知道
  const chags = changesCache[c.id];
  if (c.kind === "amount" && chags && chags.length) {
    body.push(`<div class="ledger"><div class="ldh">最近变动<span>共 ${chags.length} 条</span></div>${chags
      .map((g) => {
        const amt = Math.abs(g.delta) < 0.005 ? "±0" : money(Math.abs(g.delta), c.unit || "CNY");
        const cls = g.delta < 0 ? "bad" : "ok";
        const when =
          g.startTs === g.endTs
            ? fmtClock(g.startTs)
            : `${fmtClock(g.startTs)}–${fmtClock(g.endTs)}`;
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
      .join("")}</div>`);
  }
  // 换过密钥的渠道:说清"现在这笔账是谁的",旧账不混算但也不消失
  if (c.accounts > 1) {
    body.push(
      `<div class="hint" style="margin:8px 0 0">这个渠道换过密钥:上方消耗只按<b>当前这把</b>推算;更早那本账不参与合计(历史快照仍在库里,只是不混算)。</div>`
    );
  }
  // 节假日表过期:如实提示,不假装知道明年的安排
  if (c.peak && c.peak.tableStale) {
    body.push(
      `<div class="hint" style="margin:8px 0 0">明年的法定节假日安排还没发布,时段提示暂按"周末规则"估算,发布后会随版本更新。</div>`
    );
  }
  if (c.estimated && c.kind === "amount") {
    body.push(
      `<div class="hint" style="margin:0 0 8px">消耗为本地快照推算值 —— 该接口只返回当前余额,不含累计消耗;程序未运行的时段不计入。</div>`
    );
  }
  if (authHint) body.push(authHint);
  // 能提供模型的渠道都给入口:渠道 id 就是来源 id
  if (MODEL_SOURCES.has(c.id) && !noKey) {
    const v = MDL.views[c.id];
    const n = v ? v.usableCount || v.total : 0;
    body.push(`<div class="hint" style="margin:0 0 8px">${esc(c.name)} 可用模型${
      n ? " " + n + " 个" : ""
    } —— <span class="lk" data-act="open-models" data-id="${c.id}">去「模型」页挑一个,一键配到 Trae / WorkBuddy / ZCode</span>。</div>`);
  }
  // 签到详情:卡片上那枚徽标只给"一眼"的结论,这里给完整信息
  // (连续天数、本月合计、上次尝试、下次自动重试)。数据全在已有的 CheckinState 里。
  const cks = CK[c.id];
  if (CHECKIN_ON.has(c.id) && cks && cks.enabled) {
    body.push(
      `<div class="ckdet">` +
        `<div class="k1">签到 · ${esc(ckText(cks) ? ckText(cks).text : "今天还没签")}</div>` +
        (cks.streakDays || cks.monthCredits
          ? `<div class="k2">${[
              cks.streakDays ? "连续 " + cks.streakDays + " 天" : "",
              cks.monthCredits ? "本月共 " + cks.monthCredits + " 分" : "",
            ]
              .filter(Boolean)
              .join(" · ")}</div>`
          : "") +
        (cks.lastTryAt
          ? `<div class="k2">上次尝试 ${relTime(cks.lastTryAt)}${
              cks.nextTryAt && cks.nextTryAt * 1000 > Date.now() ? " · 下次 " + ckHold(cks) : ""
            }</div>`
          : "") +
        `</div>`
    );
  }
  body.push(manageLink);

  return `<div class="rbody-in">${body.join("")}</div>`;
}

/**
 * 数值翻页(动效 A/E 共用):el 的内容必须已是新值 ——
 * 新值从下方 9px 淡入进入,旧值以幽灵层上移淡出,320ms 一次。
 * host 提供 position:relative 与字体上下文(幽灵复制 el 的 class 吃同款样式)。
 */
function rollValue(el, oldText, host) {
  if (!el || oldText == null || oldText === el.textContent) return;
  el.classList.remove("roll-in");
  void el.offsetWidth; // 同名 class 重挂必须先回流才能重播动画
  el.classList.add("roll-in");
  setTimeout(() => el.classList.remove("roll-in"), 360);
  if (!oldText || !host) return;
  const g = document.createElement("span");
  g.className = el.className + " roll-ghost";
  g.textContent = oldText;
  g.style.left = el.offsetLeft + "px";
  g.style.top = el.offsetTop + "px";
  host.appendChild(g);
  setTimeout(() => g.remove(), 380);
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
  // 轮询会把 #list 与展开行的详情整块重建,新建元素的 scrollTop 天生是 0:
  // 用户翻到第 14 笔的「逐笔到期」小表会跳回第一笔(报告 P2-29)。
  // 数值照旧翻页,只把滚动位置补回来(选区随锚节点被移除,需要更细的差量渲染才能保住)。
  const listEl = $("list");
  const listTop = listEl ? listEl.scrollTop : 0;
  const exps = [];
  if (listEl) {
    listEl.querySelectorAll(".extab").forEach((t) => {
      const row = t.closest(".row");
      if (row && row.dataset.id) exps.push([row.dataset.id, t.scrollTop]);
    });
  }
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
  if (listEl) {
    listEl.scrollTop = listTop;
    exps.forEach(function (pair) {
      const t = listEl.querySelector('.row[data-id="' + pair[0] + '"] .extab');
      if (t) t.scrollTop = pair[1];
    });
  }
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

/**
 * 两级行 + 分组(D06):需要动作的、以及已经跌破红线的行单独成组并自动变大,
 * 其余压成一行。九行长得一模一样,等于没有层级。
 * 拿不到统一视图时退回"一整段列表",行为不回退。
 */
function listHtmlWithGroups(shown) {
  if (!HV.order.length) return shown.map((c, i) => renderRow(c, i)).join("");
  // 分组只按 后端 的 T0(需要处理)切 —— 与 HV.actionCount 同一个口径,
  // 免得标题写 2、下面站了 3 行。行本身的"变大"另算:它还包括已跌破红线的。
  const urgent = shown.filter((c) => {
    const r = hvRow(c.id);
    return !!(r && r.tier === 0);
  });
  const rest = shown.filter((c) => urgent.indexOf(c) < 0);
  const head = (n, t) => `<div class="grp"><span>${t}</span><em>${n}</em></div>`;
  return (
    (urgent.length ? head(urgent.length, "需要处理") + urgent.map((c, i) => renderRow(c, i)).join("") : "") +
    (rest.length
      ? head(rest.length, urgent.length ? "其余" : "全部渠道") +
        rest.map((c, i) => renderRow(c, i)).join("")
      : "")
  );
}

/**
 * 风险带(替换原来的「三格消耗汇总」):总余额 / 最紧 / 签到。
 *
 * 设计 D04:面板最上面这条 55px 的横带以前回答的是"花掉多少",而"还剩多少、
 * 哪家快没了"要往下扫九行才知道 —— 对一个余额挂件来说,那是次要问题。
 * 消耗没有消失,它降成第一格的副行,口径一个字没改。
 */
function renderRiskBand() {
  const box = $("sum");
  if (!box) return;
  const t = HV && HV.totals;
  const has = t && ((t.cny !== null && t.cny !== undefined) || (t.others || []).length ||
    (t.today !== null && t.today !== undefined));
  // 拿不到统一视图时退回原来的三格消耗,行为不回退
  if (!has) return renderSummary();

  const cnyTxt = t.cny === null || t.cny === undefined ? "——" : money(t.cny);
  const dayTxt = t.today === null || t.today === undefined ? "——" : money(t.today);
  // 环比只在"两边都有基线"时算,与底部汇总同一套口径;没有基线就不写涨跌
  let delta = "";
  if (t.today != null && t.todayPrev != null && t.todayPrev > 0) {
    const p = ((t.today - t.todayPrev) / t.todayPrev) * 100;
    delta = ` ${p >= 0 ? "\u2191" : "\u2193"}${Math.abs(p).toFixed(0)}%`;
  }
  const otherTxt = (t.others || []).length
    ? " · 另 " + t.others.map(otherMoneyTxt).join(" + ")
    : "";

  // ② 最紧:点一下跳到那一行(它是"现在该看谁"的答案,要能一键抵达)
  let cell2 = "";
  const rc = HV.risk && CHANNELS.find((c) => c.id === HV.risk);
  if (rc) {
    const r = hvRow(rc.id);
    const sub2 = esc([r && r.qualifier, r && r.secondary].filter(Boolean).join(" · "));
    cell2 = `<div class="cell" data-act="jump" data-id="${esc(rc.id)}" title="点一下跳到这一行">
      <div class="lb">最紧</div>
      <div class="vv" style="color:${TONE_COLOR[hvTone(rc)]}">${esc(rc.name)} <b>${hvText(r && r.headline)}</b></div>
      <div class="sb">${sub2 || "——"}</div></div>`;
  }

  // ③ 签到:常驻。全部签完也要留着「✓ 3/3」—— "完成"和"坏了"不能长得一样(D09)
  let cell3 = "";
  const ck = HV.checkin;
  if (ck) {
    const segs = Array.from({ length: ck.total }, (_, i) => `<i class="${i < ck.done ? "on" : ""}"></i>`).join("");
    cell3 = `<div class="cell" data-act="ckstripopen" title="每日签到">
      <div class="lb">签到</div>
      <div class="vv">${ck.done >= ck.total ? "\u2713 " : ""}${ck.done}/${ck.total}</div>
      <div class="sb"><span class="ckseg">${segs}</span></div></div>`;
  }

  box.innerHTML =
    `<div class="cell"><div class="lb">总余额</div><div class="vv">${cnyTxt}</div>` +
    `<div class="sb">今日 ${dayTxt}${delta}${otherTxt}</div></div>${cell2}${cell3}`;
}

/** 过滤条:十二个渠道以内靠排序就够,超过才需要搜索(P3)。 */
function syncQBar() {
  const bar = $("qbar");
  if (bar) bar.style.display = CHANNELS.length >= 12 ? "" : "none";
}

function renderInner(opts) {
  // updated=true 只由「数据刷新」调用方传(settings 变更等重渲染不放动效)
  const updated = !!(opts && opts.updated);
  // 动效 A/B:重建 DOM 前先把每行大数字的旧文本存下来,替换后对比才知道谁变了
  const prevV = {};
  // M7:顺便记下每行上一轮的状态点,等下识别"刚刚跌破红线"的行
  const prevDt = {};
  document.querySelectorAll("#list .row").forEach((r) => {
    const v = r.querySelector(".val .v");
    if (v) prevV[r.dataset.id] = v.textContent;
    if (r.dataset.dtone !== undefined) prevDt[r.dataset.id] = r.dataset.dtone;
  });
  const sorted = hvSort(CHANNELS); // 后端排好;拿不到视图时 hvSort 内部退回 sortChannels
  // 标题栏挤了 6 个按钮,计数用短写法,完整说法放 tooltip
  // 标题栏改文字按钮后不再放计数(放不下),底栏已有「N 正常 · M 预警」

  renderRiskBand(); // 顶部换成风险带;拿不到视图时它内部退回 renderSummary
  syncQBar(); // 渠道多起来(>=12)才把过滤框放出来,平时不占这一行

  if (!CHANNELS.length) {
    // 冷启动画骨架(D14):一行"加载中…"区分不了"正在加载 / 卡住了 / 没配好",
    // 骨架至少说清"这里有 N 个渠道的位置,正在一个个点亮"。
    $("list").innerHTML =
      `<div class="sk-wrap">${Array.from({ length: 6 }, () =>
        `<div class="sk-row"><i class="sk-ico"></i><i class="sk-nm"></i><i class="sk-v"></i></div>`
      ).join("")}</div>
       <div class="sk-foot">正在取数 · 首次启动需要几秒</div>`;
    return;
  }

  // 展示区只显示已配置密钥、且未被隐藏的渠道(隐藏的只在管理页出现,用于恢复);
  // 一个都没配时给一张引导卡,全被隐藏时给「去设置恢复」卡
  const hiddenN = CHANNELS.filter((c) => c.hidden).length;
  const shown = sorted.filter((c) => c.hasKey && !c.hidden).filter(chFilter);
  const configured = shown.length > 0;
  // 哪些行要展开由 rowOpen 决定(DOM 只是投影):轮询重建后用同一份状态恢复,
  // 行头的 aria-expanded 在 renderRow 里也读它,开合两边永远一致(F2)
  const openId = [...rowOpen].find((x) => shown.some((c) => c.id === x)) || null;
  $("list").innerHTML = configured
    ? failBannerHtml() + ckPendingStrip() + listHtmlWithGroups(shown)
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

  if (configured && openId) {
    const row = document.querySelector(`.row[data-id="${openId}"]`);
    if (row) {
      row.classList.add("open");
      const c = CHANNELS.find((x) => x.id === openId);
      const body = row.querySelector(".rbody");
      if (c && body) body.innerHTML = renderDetail(c);
    }
  }

  // 底栏状态(隐藏渠道不计入,只报个数)
  const withKey = CHANNELS.filter((c) => c.hasKey && !c.hidden);
  const bad = withKey.filter((c) => !c.valid).length;
  const warn = withKey.filter((c) => c.valid && (c.limited || hvTone(c) === "bad")).length;
  // 与行内同一套约定:未配置=空心环,失联=斜杠灰点,预警=黄(之前 warn 用了红点,颜色梯度倒挂)
  $("fdot").className =
    "dot" + (bad ? " st" : warn ? " w" : withKey.length ? "" : " off");
  $("fstat").textContent = !withKey.length
    ? (hiddenN ? "渠道已全部隐藏" : "未配置渠道")
    : `${withKey.length - bad} 正常${warn ? ` · ${warn} 预警` : ""}${bad ? ` · ${bad} 失联` : ""}${hiddenN ? ` · ${hiddenN} 隐藏` : ""}`;

  // 动效 A/B:数据刷新后,值真的变了的行 → 大数字翻页进入 + 一道微光;
  // 值没变的行一动不动 —— 这是「刷新完成」的视觉回执,不是装饰
  if (updated && currentForm() === "panel") {
    let n = 0;
    document.querySelectorAll("#list .row").forEach((r) => {
      const id = r.dataset.id;
      const v = r.querySelector(".val .v");
      if (v && id in prevV && prevV[id] !== v.textContent) {
        rollValue(v, prevV[id], v.closest(".val"));
        const sw = r.querySelector(".sweep");
        if (sw) {
          const d = n++ * 60;
          setTimeout(() => {
            sw.classList.remove("go");
            void sw.offsetWidth; // 重启动画要先强制一次回流
            sw.classList.add("go");
          }, d);
        }
      }
      // M7:这一轮刚从非红跌进红线的行,状态点播一次 1.2s 红晕(只播一次,
      // 持续低位不重复 —— 告警要闭环,但不打扰;阈值上调"回红"同理)
      if (prevDt[id] !== undefined && prevDt[id] !== "b" && r.dataset.dtone === "b") {
        const d = r.querySelector(".n .dot");
        if (d) {
          d.classList.add("breath1");
          setTimeout(() => d.classList.remove("breath1"), 1300);
        }
      }
    });
  }

  renderPill(sorted);
  renderDock();
  renderPillPick();
  fitCompact();
  refreshChCards();
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
  if (!ck) return null;
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
  // 先整体还原:切回面板时必须把紧凑条里藏掉的行和降级类放出来
  rows.forEach((r) => {
    r.style.display = "";
    r.classList.remove("lv1", "lv2");
  });
  list.querySelectorAll(".more,.cempty,.ckchip").forEach((el) => el.remove());
  if (!document.body.classList.contains("form-compact")) return;
  // 收起的 8px 竖条没法量;**展开态**(无论是否吸附)都要量 —— 之前整段跳过,
  // 吸附展开后宽度冻结,数值变长被裁、变短则窗口偏宽(报告 B9)。
  // 展开态量完的落点:setCompactW → applyForm → dockLayout(true),几何归吸附管。
  if (DOCK.on && !DOCK.open) return;

  const u = fsU();
  const maxW = Math.round(380 * u);
  const gap = 10 * u;
  // 签到 chip 常驻:先插进去、把它的宽度算进 chrome —— 不预留宽度的话,
  // 装到最后一步才发现放不下,末尾的渠道会被它挤掉。
  const ckEl = compactCheckinChip(list);
  const ckW = ckEl && list.contains(ckEl) ? ckEl.offsetWidth + gap : 0;
  // 列表左右留白(11+4) + 右侧按钮组 + 窗口两条竖边框 + 签到 chip
  const chrome = Math.round(15 * u) + $("cctl").offsetWidth + 2 + ckW;
  const moreW = Math.round(28 * u);

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
    let s = chrome + (n > 1 ? gap * (n - 1) : 0) + (hidden ? moreW : 0);
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
    const active = sorted.filter((c) => c.hasKey && !c.hidden);
    // 「最紧」= 后端排好的第一个:五桶紧迫度,先故障、再比例、再绝对额。
    // 以前这里是 `remainRatio(a) ?? 2` —— 2 比任何真实比例都大,于是没有分母的
    // 充值型渠道(只剩 ¥0.03 的那种)永远排最后、永远不被胶囊选中(设计 D02)。
    if (HV.order.length) {
      const first = HV.order
        .map((id) => active.find((c) => c.id === id && c.valid))
        .find(Boolean);
      if (first) return [first];
    }
    const tight = active
      .filter((c) => c.valid)
      .sort((a, b) => (remainRatio(a) ?? 2) - (remainRatio(b) ?? 2))[0];
    return tight ? [tight] : [];
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

let LAST_PILL = { id: null, text: "" };
/** 胶囊翻页(动效 E):轮播换渠道/换数值时 200ms 纵向翻页 + 图标淡入;只在胶囊形态放 */
function pillAnim(newId, lastText, lastId) {
  const pv = $("pillV");
  const text = pv.textContent;
  if (currentForm() === "pill") {
    if (lastText && text !== lastText) rollValue(pv, lastText, $("pill"));
    if (lastId !== newId) {
      const p = $("pillIco").querySelector(".pico");
      if (p) {
        p.classList.remove("ico-in");
        void p.offsetWidth;
        p.classList.add("ico-in");
      }
    }
  }
  LAST_PILL = { id: newId, text };
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
  const amt = CHANNELS.filter((c) => c.hasKey && !c.hidden && c.valid && c.kind === "amount");
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
  // 此刻 #pillV 里还是上一次的文本 —— 先存旧值才能做翻页对比
  const lastText = $("pillV").textContent;
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
    pillAnim(null, lastText, lastId);
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
  const staleTilde = hvP && hvP.state === "stale" ? '<i class="tilde">~</i>' : "";
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
  pillAnim(c.id, lastText, lastId);
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
  if (!c || !c.hasKey || !c.valid) return "";
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
 *  - 品牌式:渐变方块 + 右下状态角点(无可信数字时的回退,也是开关关闭的样式)。
 * 32×32 画布,系统按 DPI 缩到 16~24px;tooltip 永远带完整信息。
 */
let trayKey = "";
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
    roundRect();
    const grad = g.createLinearGradient(0, 0, S, S);
    grad.addColorStop(0, "#5b8cff");
    grad.addColorStop(1, "#8b5bff");
    g.fillStyle = grad;
    g.fill();
    g.globalCompositeOperation = "destination-out"; // 中间挖空,和标题栏图标同款
    g.beginPath();
    if (g.roundRect) g.roundRect(10, 10, 12, 12, 3);
    else g.rect(10, 10, 12, 12);
    g.fill();
    g.globalCompositeOperation = "source-over";
    g.beginPath();
    g.arc(22.5, 22.5, 7, 0, Math.PI * 2); // 右下角标
    g.fillStyle = TONE_HEX[tone];
    g.fill();
    g.lineWidth = 2;
    g.strokeStyle = "#12141a";
    g.stroke();
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

function renderChCards() {
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
      if (c.hidden) st = "已隐藏 · 不再取数(历史快照保留)";
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
          </div>
          <div class="ccbtns">
            <input type="password" id="key-${c.id}" autocomplete="off" spellcheck="false"
              value="${esc(keyDraft[c.id] || '')}"
              placeholder="${c.hasKey ? "也可粘贴访问密钥 · 留空则不修改" : "或粘贴访问密钥(登录窗口不通时自救)"}">
            <button class="btn" data-act="savekey" data-id="${c.id}">保存</button>
            ${c.hasKey ? `<button class="btn danger" data-act="delkey" data-id="${c.id}">清除登录</button>` : ""}
          </div>
          <div class="mini">点「登录」会开一个独立窗口加载 www.workbuddy.cn 登录页(微信扫码 / 手机号)。` +
          `登录成功后窗口自己关掉,挂件接着用这份浏览器会话取数 —— 不用你复制任何东西,` +
          `也不会动你桌面客户端的登录状态。会话约 7 天到期,到点了再登一次就行。` +
          `国内版桌面端 5.6.2 起把本机令牌改为加密存储,已无法自动读取。</div>`
        : isApp
        ? `<div class="mini">凭据来自本机已登录的 ${esc(c.name)} 客户端,只读复用、不写回不刷新;` +
          `失效时打开一次 ${esc(c.name)} 即可;隐藏本渠道后连读取也会停止。</div>`
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
              <div class="field"><span>额度申请制<span class="desc">定期申请把余额补到固定上限;上次申请时间由快照跳升自动检测</span></span>
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

      return `<div class="ccard${c.hidden ? " off" : ""}${cardOpen.has(c.id) ? " open" : ""}" data-id="${c.id}">
        <div class="cchead" data-act="chfold" data-id="${c.id}">
          ${iconHtml(c, "ico", !c.hasKey || c.hidden)}
          <div class="ccmeta">
            <div class="ccname">${esc(c.name)}<span class="dot ${chDot(c)}"></span>${c.hidden ? '<span class="tagn">已隐藏</span>' : ""}</div>
            <div class="ccst">${st}</div>
          </div>
          <button class="eye${c.hidden ? " off" : ""}" data-act="chvis" data-id="${c.id}"
            title="${c.hidden ? "恢复显示(重新开始取数)" : "隐藏(停止取数,历史快照保留)"}">${c.hidden ? "恢复" : "👁"}</button>
          <span class="ccaret">&#9654;</span>
        </div>
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
}

/** 轮询刷新时更新渠道卡;正在输入就不重建,免得把输入内容冲掉。 */function refreshChCards() {
  if (!manageOpen()) return;
  const ae = document.activeElement;
  if (ae && ae.closest && ae.closest("#chList") && (ae.tagName === "INPUT" || ae.tagName === "SELECT")) return;
  renderChCards();
  fillKeyHints();
}

/** 异步补每把 key 的尾号(IPC 一次一个,失败就留空,不影响其它信息)。 */
function fillKeyHints() {
  CHANNELS.filter((c) => c.hasKey).forEach(async (c) => {
    try {
      const hint = await invoke("key_hint", { id: c.id });
      const el = document.querySelector(`.kh[data-hint="${c.id}"]`);
      if (el && hint) el.textContent = " · " + hint;
    } catch (e) {
      // 凭据管理器坏了 ≠ 没配密钥:把故障如实写出来,否则用户会以为密钥丢了而反复重填
      // (报告 P2-15)。后端现在区分了这两种情况,这里只负责把它显示出来。
      const el = document.querySelector(`.kh[data-hint="${c.id}"]`);
      if (el) {
        el.textContent = " · 凭据库读取失败:" + String(e);
        el.classList.add("err");
      }
    }
  });
  fillKeySources();
}

// ───────────── 渠道密钥的来源(直接填 / 引用密钥箱)─────────────
// 设计见 docs/product/product-vault-channel-bridge-2026-10-20.md(裁决 1–7 已批)。
// 主线是**引用**:同一把钥匙不存两份,改箱子渠道立刻跟着变。
const KEYSRC = {};        // 渠道 id → key_source 的结果
let VAULTREF = [];        // [[渠道 id, 箱子条目 id], ...] 反查用

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

/** 补每张卡上的来源一行(读到就替换掉占位)。 */
async function fillKeySources() {
  // 「或从密钥箱选一把」要用到箱子列表;用户可能没打开过密钥箱页签,这里按需补一次。
  if (!VAULT.length) {
    try { VAULT = await invoke("vault_list"); } catch { /* 读不出来就不显示下拉 */ }
  }
  await loadVaultRefs();
  for (const c of CHANNELS) {
    const box = document.querySelector(`.ksrc[data-src="${c.id}"]`);
    if (!box) continue;
    if (c.authSource !== "keyring") { box.innerHTML = ""; continue; }
    try {
      KEYSRC[c.id] = await invoke("key_source", { id: c.id });
    } catch (e) {
      // 读不到来源时退回"直接填"的样子 —— 不显示未经验证的引用状态
      KEYSRC[c.id] = { mode: "inline" };
    }
    box.innerHTML = srcRowHtml(c.id);
  }
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
  panel: [380, 560],
  compact: [380, 46],
  pill: [200, 46],
};
let pillW = 200; // 胶囊实测内容宽(已是当前档位的最终逻辑像素),fitPill 维护

/** 当前字号档位下某形态的**窗口**尺寸(内容 × 倍率 + 两侧透明边距)。 */
function formSize(form) {
  const u = fsU();
  const base = SIZES[form] || SIZES.panel;
  const cw = form === "pill" ? pillW : form === "compact" ? compactW : Math.round(base[0] * u);
  const ch = Math.round(base[1] * u);
  return [cw + CARD_MARGIN * 2, ch + CARD_MARGIN * 2];
}

async function applyFormInner(form, remember = true) {
  if (!SIZES[form]) form = "panel"; // 白名单:老配置里的 form 可能是个已废弃的名字
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
  const [w, h] = formSize(form);
  try {
    // 改尺寸在 Windows 上会连带挪动窗口(被工作区/边缘约束顶开),那一发 moved
    // 不是用户拖的:不标 selfMove 的话松手判定会把它当成"拖到边缘",切个形态就自己吸附上了(报告 P1-8)
    markSelfMove(FORM_SELF_MOVE_MS);
    // 先解除上一形态的钳位,否则新尺寸会被旧 min/max 卡住
    await appWindow.setMinSize(null);
    await appWindow.setMaxSize(null);
    await appWindow.setResizable(false);
    // Tauri 2 的 LogicalSize 在 dpi 命名空间(v1 才在 window 下),
    // 写错命名空间会抛 TypeError 并被这里的 catch 吞掉,窗口尺寸就永远不变
    await appWindow.setSize(new T.dpi.LogicalSize(w, h));
    await appWindow.setMinSize(new T.dpi.LogicalSize(w, h));
    await appWindow.setMaxSize(new T.dpi.LogicalSize(w, h));
    // 收尾校验:类与尺寸必须成对落地。IPC 偶发丢失/交叠时这里兜底
    // (踩过:轮询的胶囊重排与用户点展开并发,类=panel 但尺寸=46 → 窗口缩成顶栏)
    const sf = (await appWindow.scaleFactor()) || 1;
    const sz = await appWindow.outerSize();
    if (Math.abs(sz.width - w * sf) > 2 || Math.abs(sz.height - h * sf) > 2) {
      console.warn("窗口尺寸与形态不符,重设", { want: [w, h], got: [sz.width, sz.height], sf });
      await appWindow.setMinSize(null);
      await appWindow.setMaxSize(null);
      await appWindow.setSize(new T.dpi.LogicalSize(w, h));
      await appWindow.setMinSize(new T.dpi.LogicalSize(w, h));
      await appWindow.setMaxSize(new T.dpi.LogicalSize(w, h));
    }
    // 视口变化后重新量一次紧凑条,决定尾巴要收几个进 "+N"
    scheduleFitCompact();
  } catch (e) {
    console.error("切换形态失败", e);
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

function applyForm(form, remember = true) {
  return enqueueGeom(() => applyFormInner(form, remember));
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
    await appWindow.setMinSize(null);
    await appWindow.setMaxSize(null);
    await appWindow.setSize(new T.dpi.LogicalSize(w, h));
    await appWindow.setPosition(new T.dpi.PhysicalPosition(x, y));
    await appWindow.setMinSize(new T.dpi.LogicalSize(w, h));
    await appWindow.setMaxSize(new T.dpi.LogicalSize(w, h));
  } catch (e) {
    console.error("吸附尺寸切换失败", e);
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
    await appWindow.setMinSize(null);
    await appWindow.setMaxSize(null);
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
    await appWindow.setMinSize(new T.dpi.LogicalSize(w, h));
    await appWindow.setMaxSize(new T.dpi.LogicalSize(w, h));
  } catch (e) {
    console.error("解除吸附失败", e);
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
  if (DOCK.selfMove) return;
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
  const ids = HV.order.length
    ? HV.order
    : CHANNELS.filter((c) => c.hasKey && !c.hidden).map((c) => c.id);
  const segs = ids
    .map((id) => {
      const c = CHANNELS.find((x) => x.id === id);
      if (!c) return "";
      const r = hvRow(id);
      const stale = !!(r && r.state === "stale");
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
  // 这些块只在管理页里出现,轮询 render 会因为"页面没打开"跳过它们,
  // 所以打开时主动建一次,不然第一次进来会是空的
  renderChCards();
  fillKeyHints();
  renderPillPick();
  renderAbout(UPD.last); // 版本号与上次检查时间
  loadCheckin();         // 签到状态(渠道卡与设置页都用)
  document.body.classList.add("view-manage");
  // 键盘用户是从底栏那枚按钮进来的,而它进管理页后就被 CSS 隐藏了 ——
  // 焦点会掉到 body,Tab 一下回到标题栏。把焦点交给页头的「← 返回」,
  // 入口与出口方向一致(鼠标点进来时 :focus-visible 不出环,看不出差别)。
  const back = $("btnBack");
  if (back) back.focus();
}
function closeManage(restoreFocus = true) {
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
  document.body.classList.remove("view-manage");
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

// 风险带上的两个动作:跳到最紧的那一行 / 打开签到设置(与 #list 里同名的动作同义)
$("sum").addEventListener("click", async (e) => {
  const act = e.target.closest("[data-act]");
  if (!act) return;
  const a = act.dataset.act;
  if (a === "jump") {
    const id = act.dataset.id;
    const row = id && document.querySelector(`.row[data-id="${id}"]`);
    if (!row) return;
    row.scrollIntoView({ block: "center" });
    row.classList.add("flash");
    setTimeout(() => row.classList.remove("flash"), 1150);
    // 顺手展开:从"最紧"跳过去的人,要看的就是它的原因
    if (!row.classList.contains("open")) row.querySelector(".rhead").click();
    return;
  }
  if (a === "ckstripopen") {
    openManage();
    showTab("run");
    focusEl($("ckSet"));
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

$("chQ").addEventListener("input", (e) => {
  CH_Q = e.target.value || "";
  render({}); // 过滤只影响"画哪些行",不重取数
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
      // 首页顶部「还有 N 个没签」→ 直接落到设置里的签到区,不用自己翻页签
      openManage();
      showTab("run");
      requestAnimationFrame(() => {
        const el = $("ckSet");
        if (el) {
          el.scrollIntoView({ block: "center" });
          el.classList.add("flash");
          setTimeout(() => el.classList.remove("flash"), 1150);
        }
      });
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
        showToast("现在还不能领:" + ckHold(CK[id]), true);
        return;
      }
      await ckRun(id);
    } else if (a === "open-manage") {
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
    } else if (a === "range") {
      const h = Number(act.dataset.h);
      sparkHours[id] = h; // 记住选择,轮询重建详情时不会再跳回 24 小时
      const series = await invoke("get_series", { id, hours: h }).catch(() => null);
      // await 期间用户可能已经切到别的区间:晚到的旧响应不许盖掉新查询的结果
      if (series !== null && sparkHours[id] === h) sparkCache[id] = series;
      const want = sparkHours[id];
      document
        .querySelectorAll(`.seg button[data-id="${id}"]`)
        .forEach((b) => b.classList.toggle("on", Number(b.dataset.h) === want));
      const c = CHANNELS.find((x) => x.id === id);
      const body = document.querySelector(`.rbody[data-body="${id}"]`);
      if (c && body) body.innerHTML = renderDetail(c);
    }    return;
  }

  const head = e.target.closest(".rhead");
  if (!head) return;
  const row = head.closest(".row");
  const id = row.dataset.id;
  const was = row.classList.contains("open");
  // 收起的行同时改 aria:开合是可感知状态,不能等下一次轮询重绘才更新(F2)
  document.querySelectorAll(".row").forEach((r) => {
    r.classList.remove("open");
    const h = r.querySelector(".rhead");
    if (h) h.setAttribute("aria-expanded", "false");
  });
  rowOpen.clear();
  if (!was) {
    rowOpen.add(id);
    row.classList.add("open");
    head.setAttribute("aria-expanded", "true");
    const c = CHANNELS.find((x) => x.id === id);
    if (c) {
      if (!sparkCache[id]) {
        try {
          sparkCache[id] = await invoke("get_series", { id, hours: 24 });
        } catch {
          sparkCache[id] = null;
        }
      }
      // 变动台账每次展开都重取(它扫 7 天明细,很便宜;缓存住反而会显示过期账目)
      if (c.kind === "amount") {
        try {
          changesCache[id] = await invoke("channel_changes", { id, days: 7 });
        } catch {
          changesCache[id] = [];
        }
      }
      // 等曲线这段时间里轮询可能把整行换成了新节点:手里那份 body 成了游离节点,
      // 写进去永远看不见(点开是空白,再点一次才有图)。按当前 DOM 重新查一次。
      const cur = document.querySelector(`.row[data-id="${id}"]`);
      const cbody = cur && cur.classList.contains("open") ? cur.querySelector(".rbody") : null;
      if (cbody) cbody.innerHTML = renderDetail(c);
    }
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

$("btnR").addEventListener("click", async () => {
  // M2 真实进度:取数期间按钮持续脉冲(禁用),完成后才停 —— 动画结束 = 数据已更新,
  // 不再固定 720ms 空转导致用户重复点(报告 B6 的后续修正)
  const btn = $("btnR");
  if (btn.classList.contains("loading")) return;
  btn.classList.add("loading");
  await refresh();
  btn.classList.remove("loading");
  const bad = CHANNELS.filter((c) => c.hasKey && !c.hidden && !c.valid).length;
  showToast(bad ? `${bad} 个渠道取数失败` : "已更新 · 刚刚", bad > 0);
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
$("btnBack").addEventListener("click", () => closeManage());
// 设置页页签:渠道 / 显示 / 刷新与启动 / 关于
function showTab(name) {
  const btns = [...$("mTabs").children];
  const b = btns.find((x) => x.dataset.p === name);
  if (!b) return;
  for (const x of btns) x.classList.toggle("on", x === b);
  document
    .querySelectorAll("#manage .panel")
    .forEach((p) => p.classList.toggle("on", p.dataset.p === name));
}
$("mTabs").addEventListener("click", (e) => {
  const b = e.target.closest("button[data-p]");
  if (!b) return;
  showTab(b.dataset.p);
  // 模型页签才去取清单:没打开就不打扰接口(价目表一天一次的手动/自动刷新)
  if (b.dataset.p === "mdl") ensureModels();
  // 关于页签里的「排查日志」卡:每次切进来刷新路径与大小(便宜,一次本地文件 stat)
  if (b.dataset.p === "about") refreshLogCard();
  // 密钥箱:每次切进来都重新读索引(列表不需要解密,读起来很便宜);
  // 重新渲染也会把已显示的明文收回
  if (b.dataset.p === "vault") {
    VAULT_REVEAL = {};
    clearTimeout(vaultRevealTimer);
    loadVault();
  }
});
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
$("btnC").addEventListener("click", () => {
  applyForm(document.body.className.includes("form-compact") ? "panel" : "compact");
});
// 失败必须说出来:invoke 被拒时以前没有 catch,"点了没反应"用户只会以为按钮坏了(F16)
$("btnH").addEventListener("click", () =>
  invoke("window_cmd", { action: "hide" }).catch((e) => showToast("隐藏失败:" + e, true))
);
$("lkQuit").addEventListener("click", () =>
  invoke("window_cmd", { action: "quit" }).catch((e) => showToast("退出失败:" + e, true))
);
$("lkKeys").addEventListener("click", openManage);
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
  if (now - previewAt < 100) return;
  previewAt = now;
  if (!CFG || !CFG.dockEnabled || DOCK.on || DOCK.selfMove) return setDockPreview(null);
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
  } else if (e.target && e.target.closest && e.target.closest("#vaultForm .kform")) {
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
    };
  }
});

$("manage").addEventListener("click", async (e) => {
  const act = e.target.closest("[data-act]");
  if (!act) return;
  const id = act.dataset.id;
  const a = act.dataset.act;
  // ── 模型页:过滤芯片 / 写入预告 / 写入 / 复制三件套 / 回滚 / 开目录 ──
  // ── 签到:开启 / 切自动 / 领一次 / 去登录 ──
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
      showToast("现在还不能领:" + ckHold(CK[id]), true);
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
  if (a === "mdlfilter") {
    const f = act.dataset.f;
    MDL.f[f] = !MDL.f[f];
    act.classList.toggle("on", MDL.f[f]);
    // 「只看编码/对话」= !modelShowAll,顺手持久化,下次打开还是这个口径
    if (f === "chat") {
      CFG.modelShowAll = !MDL.f.chat;
      saveCfg();
    }
    renderModels();
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
      showToast("已复制:" + id);
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
      showToast("已复制地址/名称等信息(密钥在下面单独复制)");
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
      showToast(msg + " · 已复制到剪贴板");
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
    VAULT_EDIT = { platform: act.dataset.id || "", label: "", key: "", note: "" };
    VAULT_DRAFT = null; // 换目标就丢草稿,不然会拿上一张表单里敲的内容回填(F4)
    renderVault();
    setTimeout(() => { const el = $("vPlat"); if (el) el.focus(); }, 30);
  } else if (a === "vedit") {
    const it = VAULT.find((x) => x.id === id);
    if (it) {
      VAULT_EDIT = { id: it.id, platform: it.platform, label: it.label, key: "", note: it.note };
      VAULT_DRAFT = null; // 同上:改另一条时以它自己的值为准
      renderVault();
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
    await loadVault();
    // N16:渠道卡上的「密钥来源」也要立刻变 —— 否则它还显示"正在使用这把",
    // 用户以为没删掉(以前要等下一轮轮询,最长 60 秒)。
    if (document.body.classList.contains("view-manage")) {
      renderChCards();
      fillKeySources();
    }
  } else if (a === "vcopy") {
    try {
      const msg = await invoke("vault_copy", { id });
      showToast(msg + " · 剪贴板对其他程序可见");
    } catch (e) {
      showToast("复制失败:" + e, true);
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
    at >= 0 ? hid.splice(at, 1) : hid.push(id);
    // **先本地翻一次**:取数有 5 秒去重窗口(fetch_shared),保存后立刻重取可能拿到旧视图,
    // 卡片会原地不动 —— 那就是用户看到的"没反应"。这里先按意图画出结果,下一轮取数再对齐。
    const cc = CHANNELS.find((x) => x.id === id);
    if (cc) cc.hidden = at < 0;
    render();
    refreshChCards();
    await saveConfigAndRefresh(); // 隐藏即停止取数:重取一轮,汇总/胶囊/底栏同步
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
      await refresh();
      renderChCards();
      fillKeyHints(); // 立刻回显尾号:确认"存的是哪把 key"
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
      await refresh();
      renderChCards();
      fillKeyHints();
      showToast("已改用密钥箱里的那把" + (it ? `(${it.platform} / ${it.label})` : "") + " · 手填的值已清除");
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
      await refresh();
      renderChCards();
      fillKeyHints();
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
      await refresh();
      renderChCards();
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
// 字号:换档要同时写 --u、按新倍率重新钳窗口尺寸、重量紧凑条截断
$("cfgFontScale").addEventListener("change", () => {
  CFG.fontScale = $("cfgFontScale").value;
  saveCfg();
  applyFontScale();
  applyForm(currentForm());
  render();
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
      showToast("最近日志已复制 · 可直接贴给我");
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
  $("btnLogClear").addEventListener("click", async () => {
    // 清空不可逆,但代价很小(只是排查线索);提示一句让用户知道"清掉就没了"
    if (!confirm("清空排查日志?清掉之后,下次出问题就没有这段记录了。")) return;
    try {
      await invoke("diag_log_clear");
      await refreshLogCard();
      showToast("日志已清空");
    } catch (e) {
      showToast("清空失败:" + e, true);
    }
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
      showToast("诊断包已复制到剪贴板 · 直接贴给我就行");
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
    // 体检把失败链也写进了日志,顺手刷新一下大小/路径
    refreshLogCard();
    // 报告里有需要处理的渠道就顺手提示一句(不替用户判断严重性)
    showToast(String(report).includes("✗") ? "体检完成:有项目需要处理" : "体检完成:全部正常");
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
    showToast("报告已复制 · 可直接贴给我");
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
    closeManage();
  } else if (DOCK.on && DOCK.open) {
    dockLayout(false); // I6:吸附展开态,Esc 收回为竖条(不退出吸附)
  } else if (document.body.className.includes("form-panel")) {
    applyForm("pill");
  } else if (document.body.className.includes("form-compact")) {
    applyForm("panel");
  } else {
    // 胶囊 → 紧凑条:Esc 现在能走完一圈,不再绕开用户待得最久的那一档
    applyForm("compact");
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
    if (e.key === "2") { e.preventDefault(); applyForm("compact"); return; }
    if (e.key === "3") { e.preventDefault(); applyForm("pill"); return; }
  }
  if (typing) return;
  if (e.key === "/") {
    // 只有过滤框可见时才抢这个键,免得面板里按不出别的斜杠输入
    const q = $("chQ");
    if (q && q.offsetParent) { e.preventDefault(); q.focus(); }
    return;
  }
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
let VAULT_EDIT = null;     // 正在编辑/新建的表单:{id?, platform, label, key, note}
let VAULT_DRAFT = null;    // 表单里已敲未存的内容:{platform,label,key,note}(见 renderVault)
let VAULT_REVEAL = {};     // id → 明文(8 秒后删)
let vaultRevealTimer = null;
// vault_list 的失败原因:读失败 ≠ 箱子是空的 —— 前者说"读不出来 + 重试",
// 后者才是"去新增一把"。把故障画成空态会让用户以为密钥全丢了(F3,
// 与渠道卡 1417-1419 的「凭据库读取失败」同一条纪律)。
let VAULT_ERR = "";

const VAULT_PLATFORM_HINT = ["4SAPI", "Hapi", "OpenCode Go", "DeepSeek", "OpenAI", "Anthropic", "Gemini", "Moonshot", "智谱", "阿里云百炼", "腾讯", "火山引擎"];

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

function vaultGrouped() {
  const out = new Map();
  for (const it of VAULT) {
    if (VAULT_Q && !`${it.platform} ${it.label} ${it.note}`.toLowerCase().includes(VAULT_Q.toLowerCase())) continue;
    if (!out.has(it.platform)) out.set(it.platform, []);
    out.get(it.platform).push(it);
  }
  return out;
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
    ? `<div class="kform">
        <div class="kf"><span>平台</span><input class="inp" id="vPlat" list="vPlatList" value="${esc(d ? d.platform : e.platform)}" placeholder="如 4SAPI">
          <datalist id="vPlatList">${VAULT_PLATFORM_HINT.map((p) => `<option value="${esc(p)}">`).join("")}</datalist></div>
        <div class="kf"><span>标签</span><input class="inp" id="vLabel" value="${esc(d ? d.label : e.label)}" placeholder="如 主号 / 备用"></div>
        <div class="kf"><span>密钥</span><input class="inp" id="vKey" type="password" autocomplete="off" value="${esc(
          d ? d.key : ""
        )}" placeholder="${
          e.id ? "留空 = 不改这把" : "粘贴密钥"
        }"></div>
        <div class="kf"><span>备注</span><input class="inp" id="vNote" value="${esc(d ? d.note : e.note)}" placeholder="选填,如 充值用的那个"></div>
        <div class="acts">
          <button class="btn p" data-act="vsave">保存</button>
          <button class="btn" data-act="vcancel">取消</button>
          <span class="note" style="margin-left:6px">密钥写进 Windows 凭据管理器,列表里只留尾 4 位</span>
        </div>
      </div>`
    : "";

  // 读失败:说清是"读不出来"并给重试,绝不画成"密钥箱是空的 + 新增一把"(F3)
  if (VAULT_ERR) {
    list.innerHTML = `<div class="empty">读不出密钥箱:${esc(VAULT_ERR)}
      <div style="margin-top:8px"><button class="btn p" data-act="vreload">重试</button></div>
      <div class="note" style="margin-top:6px">列表(以及里面的密钥)并没有丢,只是这一次没读出来</div></div>`;
    return;
  }

  if (!VAULT.length) {
    list.innerHTML = `<div class="empty">密钥箱是空的<div style="margin-top:8px"><button class="btn p" data-act="vnew">新增一把密钥</button></div></div>`;
    return;
  }
  const groups = vaultGrouped();
  if (!groups.size) {
    list.innerHTML = `<div class="empty">没有匹配的条目</div>`;
    return;
  }
  list.innerHTML = [...groups.entries()]
    .map(([plat, items]) => {
      const rows = items
        .map((it) => {
          const plain = VAULT_REVEAL[it.id];
          // 反查:这把钥匙正被哪些渠道引用(裁决 4 —— 删前要说清后果,平时要看得见)
          const refs = refChannelsOf(it.id);
          const refLine = refs.length
            ? `<br><span class="dim">🔗 正被 ${refs.map((c) => esc(channelName(c))).join("、")} 使用</span>`
            : `<br><span class="dim">未被任何渠道使用</span>`;
          return `<div class="krow"><span class="k1">${esc(it.label)} · <span class="khint">${esc(it.hint)}</span>
            ${it.note ? `<br><span class="dim">${esc(it.note)}</span>` : ""}
            <br><span class="dim">${new Date(it.createdAt * 1000).toLocaleDateString("zh-CN")} 建${
              it.updatedAt !== it.createdAt ? " · " + new Date(it.updatedAt * 1000).toLocaleDateString("zh-CN") + " 改" : ""
            }</span>${refLine}</span>
            <button class="btn s" data-act="vcopy" data-id="${esc(it.id)}" title="明文不回界面,直接进剪贴板">复制</button>
            <button class="btn s" data-act="vreveal" data-id="${esc(it.id)}" title="显示明文,8 秒后自动隐">${
              plain ? "收起" : "显示"
            }</button>
            <button class="btn s" data-act="vedit" data-id="${esc(it.id)}">改</button>
            <button class="btn s danger" data-act="vdel" data-id="${esc(it.id)}">删</button></div>
            ${plain ? `<div class="reveal" style="margin:-2px 0 6px">${esc(plain)}</div>` : ""}`;
        })
        .join("");
      return `<div class="pf"><b>${esc(plat)}</b><span>${items.length} 把</span>
        <span style="margin-left:auto"><button class="btn s" data-act="vnew" data-id="${esc(plat)}">+ 新增</button></span></div>${rows}`;
    })
    .join("");
}

/** 把表单里的内容提交(新建或改名/改备注/换密钥)。 */
async function vaultSave() {
  const e = VAULT_EDIT;
  if (!e) return;
  const payload = {
    id: e.id || null,
    platform: ($("vPlat").value || "").trim(),
    label: ($("vLabel").value || "").trim(),
    key: ($("vKey").value || "").trim(),
    note: ($("vNote").value || "").trim(),
  };
  try {
    await invoke("vault_save", { item: payload });
    VAULT_EDIT = null;
    VAULT_DRAFT = null; // 存进去了就不再是草稿,免得下次开表单又被回填(F4)
    showToast(e.id ? "已更新" : "已存进密钥箱");
    await loadVault();
  } catch (err) {
    showToast(String(err), true);
  }
}

/** 删除要内联确认(与渠道密钥删除同一套做法,不用原生 confirm)。 */
function vaultDel(id) {
  const row = document.querySelector(`.krow [data-act="vdel"][data-id="${id}"]`);
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
//   ① 独立开关、默认关 —— 关掉就一个请求都不发;
//   ② **不把"点了按钮"说成"领到了"**:数额只显示平台自己给的字段,拿不到就不写数字;
//   ③ 签到状态**不参与余额渲染** —— 签到失败永远不可能让余额显示成 0。
let CK = {};            // 渠道 id → checkin_state 的那一条
let CK_LOG = false;     // 「领积分」正在跑
const CHECKIN_ON = new Set(["trae", "workbuddy", "qoder", "qoder-cn"]);

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
          ? " · 今天已领" + amt
          : " · 今天已签到(平台未返回数额)"
        : "";
    return { text: "签到未开启(开了才会发请求)" + tail, btn: "开启签到", act: "ckenable" };
  }
  if (s.note) return { text: s.note, btn: "去登录", act: "cklogin", warn: true };
  // 连续天数只有 Qoder 的接口给(streak_days),有就带上 —— 比现有两个渠道多一个可展示项
  const streak = s.streakDays ? " · 连续 " + s.streakDays + " 天" : "";
  if (o.startsWith("claimed"))
    return {
      text:
        (hasN ? "今日已领" + amt : "今日已签到(平台未返回数额)") +
        streak +
        " · 已触发到账核对",
    };
  if (o.startsWith("already"))
    return {
      text: (hasN ? "今日已领" + amt : "今日已签到(平台未返回数额)") +
        streak + "(平台侧显示已签到)",
    };
  if (o === "retry:ready") return Object.assign({ text: "今日可领" + n }, run("领积分", "p"));
  // Qoder 的可领短码形如 retry:ready:{活动id} —— 同样落进"可领"
  if (o.startsWith("retry:ready:")) return Object.assign({ text: "今日可领" + n }, run("领积分", "p"));
  if (o === "retry:device")
    return Object.assign(
      { text: "太挤了,今天还会自动再试(第 " + (s.attempts || 1) + "/5 次)" },
      run("重试", "")
    );
  if (o.startsWith("retry"))
    return Object.assign(
      { text: "上次没成功,今天还会再试(第 " + (s.attempts || 1) + "/5 次)" },
      run("重试", "")
    );
  if (o.startsWith("auth")) return { text: "凭据失效 —— 打开一次客户端 / 在渠道卡上登录一次", btn: "去登录", act: "cklogin", warn: true };
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
  if (o.startsWith("inactive")) return { text: s.lastMessage || "本期无打卡活动(等下一期)" };
  if (o.startsWith("shape")) return { text: "签到接口有变动,已暂停自动签到 · 余额显示不受影响", warn: true };
  if (o.startsWith("failed")) return Object.assign({ text: "上次没成功:" + (s.lastMessage || o.slice(7)) }, run("重试", ""));
  return Object.assign({ text: "今天还没签" }, run("领积分", "p"));
}

/**
 * 首页顶部「今天还有 N 个渠道没签到」提示条。
 *
 * **只在真有未签时出现**:全部签完就消失 —— "全部已签"是噪音不是信息;
 * 签到全关时也不出现(没开的人不该被提醒一件他没要求的事)。
 */
function ckPendingStrip() {
  const ids = ckActiveIds();
  if (!ids.length) return "";
  const pending = ids.filter((id) => !(CK[id] && CK[id].doneToday)).length;
  if (!pending) return "";
  return `<button type="button" class="ckstrip" data-act="ckstripopen"
      title="打开「刷新与启动 → 每日签到」">今天还有 <b>${pending}</b> 个渠道没签到 · 去查看</button>`;
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

  // claimed = **我们刚替他领到了** → 可以说数额
  // N10(2026-10-21):Trae / WorkBuddy 的接口**经常不返回数额**(本机 10-20、10-21 都是
  // credits = null)—— 所以"没有数额"是生产常态,不是边缘情况。以前这里出 ✓${n},
  // n 为空就只剩一个孤零零的 ✓,挂件又只有 380px、tooltip 要悬停才看得见。
  if (o.startsWith("claimed"))
    return `<span class="ckb ok" title="${tip}">${n ? "✓" + esc(n) : "✓ 已签"}</span>`;
  // already = **平台侧已签**(可能是他自己在客户端点的)→ 只说"已签",数额进 tooltip。
  // 写成 "+100" 会让人以为是我们领的 —— 签到红线第 2 条(不把点了按钮说成领到了)。
  if (o.startsWith("already")) return `<span class="ckb ok" title="${tip}">✓ 已签</span>`;
  // inactive = 本期无活动:不是"已签",但今天确实没有待办,聚合里算它一份
  if (o.startsWith("inactive")) return `<span class="ckb ok" title="${tip}">本期无活动</span>`;
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
    return `<button class="ckb warn" data-act="open-manage" data-id="${esc(c.id)}" title="${tip}">异常</button>`;
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
  if (o.startsWith("claimed")) bits.push("今天已领" + (s.credits ? " +" + s.credits + " 分" : ""));
  else if (o.startsWith("already"))
    bits.push("平台显示今天已签到" + (s.credits ? "(数额 " + s.credits + " 分)" : ""));
  else if (o.startsWith("inactive")) bits.push(s.lastMessage || "本期无打卡活动,等下一期");
  else if (o.startsWith("retry:ready"))
    bits.push("今天可领" + (s.credits ? " +" + s.credits + " 分" : ""));
  else if (o.startsWith("auth")) bits.push("凭据失效 —— 打开一次客户端");
  else if (o.startsWith("no_cred")) bits.push(s.lastMessage || "本机没读到签到凭据");
  else if (o.startsWith("shape")) bits.push("签到接口有变动,已暂停自动签到(余额不受影响)");
  else if (o === "recheck") bits.push("正在重新确认凭据");
  else if (o.startsWith("retry")) bits.push("上次没成功,今天还会自动再试");
  else bits.push("今天还没签");
  if (s.streakDays) bits.push("连续 " + s.streakDays + " 天");
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

function ckRowHtml(id) {
  const s = CK[id];
  if (!s) return "";
  const t = ckText(s);
  if (!t) return "";
  return (
    '<div class="ckrow' + (t.warn ? " warn" : "") + '" data-id="' + esc(id) + '">' +
    "<span>" + esc(t.text) + "</span>" +
    (t.btn
      ? '<button class="btn s ' + (t.cls || "") + '" data-act="' + (t.act || "") + '" data-id="' + esc(id) + '"' +
        (t.disabled ? " disabled" : "") +
        ">" + esc(t.btn) + "</button>"
      : "") +
    "</div>"
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

async function loadCheckin() {
  try {
    const list = await invoke("checkin_state");
    CK = {};
    for (const x of list) CK[x.id] = x;
  } catch (e) {
    CK = {};
  }
  // 渠道卡与设置页都跟着更新(哪个在场更新哪个)
  if (document.body.classList.contains("view-manage")) {
    renderCheckinSet();
    document.querySelectorAll("#chList .ckwrap").forEach((el) => {
      el.innerHTML = ckRowHtml(el.dataset.id);
    });
  }
}

/** 设置页的签到区:每渠道两个开关 + 近 7 天流水 + 免责。 */
function renderCheckinSet() {
  const box = $("ckSet");
  if (!box) return;
  const rows = [];
  for (const id of CHECKIN_ON) {
    const s = CK[id];
    const on = s ? s.enabled : false;
    const auto = s ? s.auto : true;
    let logs = "";
    if (s && s.log && s.log.length) {
      logs =
        '<div class="cklog">' +
        s.log
          .slice(0, 7)
          .map(
            (r) =>
              '<span class="ckd">' + esc(r.day.slice(5)) + "</span>" +
              esc((r.outcome || "").split(":")[0]) +
              (r.credits ? " +" + r.credits : "")
          )
          .join(" · ") +
        "</div>";
    }
    rows.push(
      '<div class="field"><span>' + esc(s ? s.name : id) +
        '<span class="desc">' +
        (on
          ? (auto
              ? (CK_ROLL[id] ? "每天 " + CK_ROLL[id] + ":00 后自动领" : "每天 00:23 后自动领") +
                ",失败当天退避重试"
              : "只保留手动「领积分」按钮")
          : "关着 —— 一个请求都不发") +
        (s && s.monthCredits ? " · 本月已领 " + points(s.monthCredits) + " 分" : "") +
        "</span></span>" +
        '<button class="sw' + (on ? " on" : "") + '" role="switch" aria-checked="' + on +
        '" data-act="ckenable" data-id="' + esc(id) + '" aria-label="' + esc((s ? s.name : id) + " 签到") + '"></button></div>' +
      (on
        ? '<div class="field"><span>自动签到<span class="desc">关掉只留手动按钮</span></span>' +
          '<button class="sw' + (auto ? " on" : "") + '" role="switch" aria-checked="' + auto +
          '" data-act="ckauto" data-id="' + esc(id) + '" aria-label="自动签到"></button></div>'
        : "") +
      logs
    );
  }
  box.innerHTML =
    rows.join("") +
    '<div class="khint" style="margin:8px 0 0">签到走<b>直连</b>(不经过系统代理):平台按出口 IP 限流,代理 IP 更容易被拒。' +
    "签到状态与余额显示互不影响。</div>";
}

async function ckRun(id) {
  CK_LOG = true;
  try {
    const st = await invoke("checkin_now", { id });
    CK[st.id] = st;
    const t = ckText(st);
    if (st.outcome && st.outcome.startsWith("claimed")) showToast("已领到" + (st.credits ? " +" + st.credits + " 分" : ""));
    else if (st.outcome && st.outcome.startsWith("already")) showToast("今天已经签过了");
    else showToast(t && t.text ? t.text : "签到没成功", true);
  } catch (e) {
    showToast(String(e), true);
  }
  CK_LOG = false;
  loadCheckin();
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
    showToast("已开启 " + (CK[id] ? CK[id].name : id) + " 签到");
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
  f: { chat: true },         // 芯片只剩「只看编码/对话」;「只列能用上的」是固定规则
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
    if (MDL.f.chat && !r.chat) continue;
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
      const n = v ? v.total : null;
      const sub = v ? (n ? n + " 个" : "—") : "—";
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
    '<div class="mrow1"><span class="dim">写进客户端 <b>' + esc(v.baseUrl) + "</b></span>" +
    '<span class="dim">' +
    (v.hasPrice ? "单价按站点自动分组折算" : isGo ? "订阅制:不按 token 计价" : "该来源不提供单价") +
    "</span></div>" +
    '<div class="mrow1"><span class="dim">显示 ' + rows.length +
    (v.usableCount ? " · 可用 " + v.usableCount : "") +
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
          (MDL.f.chat ? "(「只看编码/对话」排掉了图像/视频/音频/向量模型)" : "") +
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
        '<div class="note">退出它之后直接再点一次下面的按钮就行 —— 按钮会自己重查一遍,这个窗口不用关。' +
        (s.recheckAt ? "(" + esc(s.recheckAt) + " 刚查过,它还在运行)" : "") +
        "</div></div>"
      : "") +
    '<div class="acts">' +
    (blocked
      ? /正在运行/.test(blocked.blocked)
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
    接口地址: "base_url,到 /v1 为止",
    完整端点: "有的客户端要完整地址(…/chat/completions)",
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
    '<div class="note">这些就是挂件往客户端里写的那几个值。自动写入客户端不认的时候,' +
    "可以对着这几行在客户端自己的「自定义模型」里手填一遍。</div>" +
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
  MDL.f.chat = !CFG.modelShowAll;
  const chip = document.querySelector('#mdlF .pick[data-f="chat"]');
  if (chip) chip.classList.toggle("on", MDL.f.chat);
  if (!MDL.clients.length) await loadInjects();
  const want = source || MDL.src || "4sapi";
  if (!MDL.src) MDL.src = want;
  if (!MDL.views[want]) await loadModels(false, want);
  renderModels();
}

// ───────────── 启动 ─────────────
async function refresh() {
  // render() 挪出 try:渲染抛错以前会被当成"取数失败"再 toast 一次,信息是错的(P2-33)
  let list;
  try {
    list = await invoke("get_channels");
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

(async function init() {
  try {
    CFG = await invoke("get_config");
  } catch {
    CFG = {
      activeIntervalSec: 60, idleIntervalSec: 300, backoffIntervalSec: 900,
      warnPercent: 40, critPercent: 15, autostart: false,
      form: "panel", sort: "percent", pillChannels: [],
      fontScale: "md", hiddenChannels: [], hotkey: "", trayShowNumber: true,
      claimChannels: {
        "4sapi": { enabled: true, amount: 200, minIntervalDays: 14, manualLastAt: null, manualSetAt: null },
      },
      modelShowAll: false, modelGroup: null, modelBaseUrl: null,
      selectionPrefix: {}, traeAutoSelect: false,
    };
  }
  applyConfigToUI();
  applyFontScale();
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
  // 启动一律面板形态:安装后(以及每天开机)先看到完整信息,而不是上次留下的
  // 紧凑条 / 胶囊。CFG.form 仍会记录用户的选择,只是不再用于启动恢复。
  await applyForm("panel", false);
  await refresh();
  playEntrance(); // 启动首帧也来一次入场(上面 applyForm 没换形态,不会自动放)

  listen("checkin-updated", () => {
    loadCheckin();
  });

  listen("checkin-alert", (ev) => {
    // 后端只在"需要用户动手"(凭据失效 / 接口结构变了)时发这条;绿色版从托盘启动时
    // stderr 永远没人看得到(报告 P2-14 / P3-36)。
    const d = (ev && ev.payload) || {};
    showToast(
      (d.name || "签到渠道") + "需要处理:" + (d.message || "去渠道卡重新登录一次"),
      true
    );
  });
  listen("channels-updated", (ev) => {
    CHANNELS = ev.payload;
    render({ updated: true });
  });

  // 跌破红线(Rust 判定、一天一次):播一声预警音(开了才有)+ 应用内提示。
  // 判定不放前端 —— 界面与通知必须用同一个口径,否则会出现"红了但没响"。
  listen("crit-alert", (ev) => {
    const d = (ev && ev.payload) || {};
    beep("warn");
    showToast(d.body || d.name + " 余额很低", true);
  });

  // 自动签到领到了(后台静默跑,这一声就是收据);手动点的由按钮回执负责,不重响
  listen("checkin-claimed", (ev) => {
    const d = (ev && ev.payload) || {};
    const cr = d.credits === null || d.credits === undefined ? "" : " +" + d.credits + " 分";
    beep("ding");
    showToast((d.name || "签到") + " 今天领到了" + cr);
  });

  // WorkBuddy 网页登录的三种结果都走这个事件:抓到令牌 / 用户自己关了窗 / 失败。
  // 成功时 Rust 已经立刻刷过一轮(channels-updated 会带出新数值),这里只管按钮与提示。
  listen("workbuddy-login", (ev) => {
    WB_LOGGING = false;
    const p = ev.payload || {};
    if (p.ok) showToast("已登录 WorkBuddy · 读数已刷新");
    else if (p.cancelled) showToast("已取消登录");
    else showToast(p.reason || "登录没成功,请再试一次", true);
    renderChCards();
    fillKeyHints();
  });

  // 从托盘/热键唤回窗口:Rust 侧在「隐藏 → 显示」那一刻发 window-shown,这里重放入场。
  // 不能监听 focus / visibilitychange —— 它们分不清"从托盘唤回"和普通 alt-tab,
  // 每次切走再切回都会重播一遍(报告 B8)。
  listen("window-shown", () => {
    if (currentForm() === "panel") playEntrance();
  });

  // 启动 30 秒后先自动检查一次;之后每 10 分钟再问一次 —— 是否真发请求由
  // Rust 侧 24 小时节流把关(最多每天一次)。之前只有启动那一次,
  // 软件连开几天就再也不会重检(实测:上次检查时间永远停在开机那一刻)。
  setTimeout(() => checkUpdate(false), 30_000);
  setInterval(() => checkUpdate(false), 10 * 60 * 1000);
})();
