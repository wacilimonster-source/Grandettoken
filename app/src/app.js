/* TokenScope 前端。经 window.__TAURI__ 调 Rust 命令(withGlobalTauri)。 */
const T = window.__TAURI__;
const invoke = T.core.invoke;
const listen = T.event.listen;
const appWindow = T.window.getCurrentWindow();

const $ = (id) => document.getElementById(id);

let CHANNELS = [];
let CFG = null;
let sparkCache = {};   // { [id]: {24:[],168:[],720:[]} }
let sparkHours = {};   // { [id]: 当前选中的区间 },渲染详情时据此标记选中项

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
const money = (v) => (v === null || v === undefined ? "——" : "¥" + v.toFixed(2));

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
 * 配额型渠道的主窗口:固定看「周期」窗(实测锚定开通日,旧称「本月」),
 * 窗口缺失时退到最后一个。大数字与状态色都按它走 —— 5 小时 / 本周的波动不该左右整行的观感。
 */
function mainWindow(c) {
  const wins = c.windows || [];
  if (!wins.length) return null;
  return wins.find((w) => w.label === "周期") || wins[wins.length - 1];
}

/** ISO 8601(UTC)的 resetsAt → 毫秒;缺失/异常一律 null,绝不猜。 */
function resetMs(w) {
  if (!w || !w.resetsAt) return null;
  const t = Date.parse(w.resetsAt);
  return Number.isNaN(t) ? null : t;
}

/** 倒计时档位化:<1 小时用分钟,<24 小时用小时,更长按「天+小时」;short 用于窄列。 */
function fmtReset(ms, short) {
  if (ms === null) return short ? "—" : "重置时间未知";
  const left = ms - Date.now();
  if (left <= 0) return "正在重置";
  const m = Math.round(left / 60000);
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
  const denom = c.claim && c.claim.amount > 0 ? c.claim.amount : c.total;
  if (c.remaining === null || denom === null || denom === undefined || !denom) return null;
  return c.remaining / denom;
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

// 官方图标(素材出处见 app/src/logos/README.md)。没有官方标的渠道继续用字母方块。
const LOGOS = {
  "opencode-go": "logos/opencode.svg",
  deepseek: "logos/deepseek.png",
  trae: "logos/trae.svg",
  workbuddy: "logos/workbuddy.png",
};

/**
 * 渠道图标。统一入口:列表行 / 密钥行 / 额度申请行都用它。
 * off = 未配置或取数失败 —— 字母方块变灰,官方标压暗去色,语义保持一致。
 */
function iconHtml(c, cls = "ico", off = false) {
  const src = LOGOS[c.id];
  if (!src) {
    return `<div class="${cls}" style="background:${off ? "#3c445c" : c.color}">${esc(c.short)}</div>`;
  }
  return `<div class="${cls} logo${off ? " off" : ""}"><img src="${src}" alt="${esc(c.short)}"></div>`;
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

// ───────────── 渲染 ─────────────
function renderSummary() {
  // 汇总只累加金额型渠道 —— 百分比和金额不能相加;隐藏的渠道不参与
  const am = CHANNELS.filter((c) => c.kind === "amount" && c.hasKey && c.remaining !== null && !c.hidden);
  const sum = (f) => {
    const vals = am.map((c) => c[f]).filter((v) => v !== null && v !== undefined);
    if (!vals.length) return null;
    return vals.reduce((a, b) => a + b, 0);
  };
  // 每格显式带上自己的字段名与注释文案 —— 不要拿"是否推算"去推字段名,
  // 那种写法今天恰好对,给今日/本周也加计数时就会读到错的字段
  const cells = [
    { label: "今日消耗", field: "day", note: "推算" },
    { label: "本周消耗", field: "week", note: "推算" },
    { label: "本月消耗", field: "month", note: null },
  ];
  if (!am.length) {
    $("sum").innerHTML = "";
    return;
  }
  $("sum").innerHTML = cells
    .map(({ label, field, note: cellNote }) => {
      const v = sum(field);
      const txt = v === null ? "——" : money(v);
      // 计数要按"真正参与了求和"的渠道数 —— 有的渠道这个窗口还没数据,
      // 用 am.length 会把没算进去的也算上,看起来像少加了钱
      const contributors = am.filter((c) => c[field] != null).length;
      const note = v === null ? "数据不足" : cellNote || `${contributors} 个渠道`;
      return `<div class="cell"><div class="lb">${label}</div>
        <div class="vv">${txt}<span class="dl">${note}</span></div></div>`;
    })
    .join("");
}

function renderRow(c, i) {
  const tone = toneOf(c);
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
    const v = Math.max(0, Math.min(100, mw ? mw.remainPercent : c.remaining));
    main = v.toFixed(1) + '<i class="pct">%</i>';
  } else if (isPts) {
    // 积分不带货币符号;整数部分千分位,小数部分照旧
    const [ip, dp] = points(c.remaining).split(".");
    main = ip + (dp ? '<i class="dec">.' + dp + "</i>" : "") + '<i class="unit">分</i>';
  } else {
    const [int, dec] = c.remaining.toFixed(2).split(".");
    main = "¥" + int + '<i class="dec">.' + dec + "</i>";
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
  if (noKey) sub = c.authSource === "app" ? `未检测到 ${c.name} 登录信息` : "未配置密钥";
  else if (failed) sub = "取数失败";
  else if (wins.length) sub = "";
  else if (c.limited) sub = "已限流";
  else if (isPts) {
    sub = soon
      ? `近 30 天将过期 ${points(soon.amount)} 分`
      : c.expiring && c.expiring.length
        ? "30 天内无到期"
        : "近期无到期";
  } else if (claim) sub = `本轮额度 ¥${claim.amount}${claimTail(claim)}`;
  else if (pct !== null) sub = `额度 ¥${c.total}`;
  else sub = ""; // 金额型拿到多少就是可用多少,不加说明

  let subVal;
  if (noKey) subVal = c.authSource === "app" ? "需打开应用" : "待配置";
  else if (failed) subVal = c.stale ? "上次快照" : "失联";
  else if (mw) subVal = `${mw.label}窗 · ${fmtReset(resetMs(mw), true)}`;
  else if (isPts) subVal = soon ? `最近 ${fmtDay(soon.at)} 到期` : "无近期到期";
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

  // 状态点
  let dot = "";
  if (noKey) dot = '<span class="dot o"></span>';
  else if (failed) dot = '<span class="dot o"></span>';
  else if (c.limited || tone === "bad") dot = '<span class="dot b"></span>';
  else if (tone === "warn") dot = '<span class="dot w"></span>';
  else dot = '<span class="dot"></span>';

  // 第三段:金额型显示日/周/月;配额型显示三个限流窗口;积分型显示最近的几笔到期
  let useHtml;
  if (isPts) {
    const list = (c.expiring || []).filter((e) => daysUntil(e.at) >= 0);
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
          <span class="wl">${w.label}</span>
          <i class="wb"><i style="width:${wr}%;background:${TONE_HEX[wt]}"></i></i>
          <b class="wv" style="color:${lim ? "var(--bad)" : TONE_COLOR[wt]}">${wr.toFixed(1)}%</b>
          ${cd}
        </div>`;
      })
      .join("")}</div>`;
  } else {
    const f = (v) => (v === null || v === undefined ? "——" : "¥" + v.toFixed(2));
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

  const src = c.stale
    ? `<div class="hint" style="color:var(--warn)">显示的是 ${relTime(c.updatedAt)} 的成功快照${
        c.error ? " · " + esc(c.error) : ""
      }</div>`
    : `<div class="hint">更新于 ${relTime(c.updatedAt)}</div>`;

  return `<div class="row" data-id="${c.id}" data-i="${i}">
    <div class="rhead">
      <div class="r1">
        ${iconHtml(c, "ico", noKey || failed)}
        <div class="nm">
          <div class="n"><span class="nn">${esc(c.name)}</span>${dot}${wmark}</div>
          <div class="s">${sub}</div>
        </div>
        <div class="val">
          <div class="v" style="color:${TONE_COLOR[tone]}">${main}</div>
          <div class="p">${subVal}</div>
        </div>
        <div class="caret">&#9654;</div>
      </div>
      ${bar}
      ${useHtml}
      ${claimWarn}
      ${winWarn}
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
    claimKv.push(["本轮额度", `¥${cl.amount}`]);
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
      claimKv.push(["日均消耗(推算)", `¥${cl.dailyBurn.toFixed(2)}`]);
    }
    if (cl.daysOfBalance !== null && cl.daysOfBalance !== undefined) {
      claimKv.push(["预计可用", `约 ${Math.round(cl.daysOfBalance)} 天`]);
    }
    if (c.total !== null && c.total !== undefined) {
      claimKv.push(["累计发放", `¥${c.total}`]);
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
      : "";

  const spark = sparkCache[c.id] || [];
  const sparkHtml = spark.length
    ? `<div class="spark">${(() => {
        const max = Math.max(...spark, 0.000001);
        return spark
          .map((v) => `<i style="height:${Math.max(2, (v / max) * 100)}%"></i>`)
          .join("");
      })()}</div>`
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
       ${[[24, "近 24 小时"], [168, "近 7 天"], [720, "近 30 天"]]
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
  if (c.estimated && c.kind === "amount") {
    body.push(
      `<div class="hint" style="margin:0 0 8px">消耗为本地快照推算值 —— 该接口只返回当前余额,不含累计消耗;程序未运行的时段不计入。</div>`
    );
  }
  if (authHint) body.push(authHint);
  body.push(manageLink);

  return `<div class="rbody-in">${body.join("")}</div>`;
}

function render() {
  const sorted = sortChannels(CHANNELS);
  // 标题栏挤了 6 个按钮,计数用短写法,完整说法放 tooltip
  // 标题栏改文字按钮后不再放计数(放不下),底栏已有「N 正常 · M 预警」;
  // 元素可能不存在,这里做守卫
  const cnt = $("cnt");
  if (cnt) {
    const withKeyN = CHANNELS.filter((c) => c.hasKey).length;
    cnt.textContent = withKeyN + "/" + CHANNELS.length;
    cnt.title = `已配置 ${withKeyN} / 共 ${CHANNELS.length} 个渠道`;
  }

  renderSummary();

  if (!CHANNELS.length) {
    $("list").innerHTML = `<div class="empty"><b>正在取数…</b>首次启动需要几秒</div>`;
    return;
  }

  const openId = document.querySelector(".row.open")?.dataset.id;

  // 展示区只显示已配置密钥、且未被隐藏的渠道(隐藏的只在管理页出现,用于恢复);
  // 一个都没配时给一张引导卡,全被隐藏时给「去设置恢复」卡
  const hiddenN = CHANNELS.filter((c) => c.hidden).length;
  const shown = sorted.filter((c) => c.hasKey && !c.hidden);
  const configured = shown.length > 0;
  $("list").innerHTML = configured
    ? shown.map((c, i) => renderRow(c, i)).join("")
    : hiddenN && CHANNELS.some((c) => c.hasKey && c.hidden)
      ? `<div class="empty">
           <div class="ek">&#128065;</div>
           <b>已配置的渠道都被隐藏了</b>
           到设置 → 渠道页点眼睛恢复显示;隐藏期间不再请求接口,历史快照保留
           <div><button class="btn p" data-act="open-manage">去设置</button></div>
         </div>`
      : `<div class="empty">
         <div class="ek">&#128273;</div>
         <b>还没有可显示的渠道</b>
         填入至少一个 API Key,或在别的应用里登录一次(Trae / WorkBuddy 会直接读取)<br>密钥只写入 Windows 凭据管理器,界面保存后不回显
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
  const warn = withKey.filter((c) => c.valid && (c.limited || toneOf(c) === "bad")).length;
  // 与行内同一套约定:失联=灰,预警=黄(之前 warn 用了红点,颜色梯度倒挂)
  $("fdot").className =
    "dot" + (bad ? " o" : warn ? " w" : withKey.length ? "" : " o");
  $("fstat").textContent = !withKey.length
    ? (hiddenN ? "渠道已全部隐藏" : "未配置渠道")
    : `${withKey.length - bad} 正常${warn ? ` · ${warn} 预警` : ""}${bad ? ` · ${bad} 失联` : ""}${hiddenN ? ` · ${hiddenN} 隐藏` : ""}`;

  renderPill(sorted);
  renderDock();
  renderPillPick();
  fitCompact();
  refreshChCards();
}

/** 紧凑条:按重要度排在前面,尾部放不下的收进 "+N" 徽标(设计稿的截断规则)。 */
function fitCompact() {
  const list = $("list");
  const rows = [...list.querySelectorAll(".row")];
  const badge = list.querySelector(".more");
  // 先整体还原:切回面板时必须把紧凑条里藏掉的行放出来
  rows.forEach((r) => (r.style.display = ""));
  if (badge) badge.remove();
  if (!document.body.classList.contains("form-compact") || !rows.length) return;

  const more = document.createElement("div");
  more.className = "more";
  list.appendChild(more);
  let hidden = 0;
  for (let i = rows.length - 1; i >= 0 && list.scrollWidth > list.clientWidth; i--) {
    rows[i].style.display = "none";
    hidden += 1;
    more.textContent = "+" + hidden;
  }
  if (hidden) {
    more.title = hidden + " 个渠道放不下,展开面板查看";
  } else {
    more.remove();
  }
}

// 胶囊显示哪些渠道:设置里选中的按顺序轮播;一个都没选就自动取最紧张的那个
// (总额不能告诉你哪个 Key 要挂了,所以自动模式只挑最紧的)
const PILL = { list: [], sorted: [], idx: 0, auto: true };
const PILL_ROTATE_MS = 5000;

function renderPill(sorted) {
  PILL.sorted = sorted;
  const picked = (CFG && CFG.pillChannels) || [];
  PILL.auto = !picked.length;
  PILL.list = picked.length
    ? picked.map((id) => CHANNELS.find((c) => c.id === id)).filter(Boolean).filter((c) => !c.hidden)
    : (() => {
        const active = sorted.filter((c) => c.hasKey && !c.hidden);
        const tight = active
          .filter((c) => c.valid)
          .sort((a, b) => (remainRatio(a) ?? 2) - (remainRatio(b) ?? 2))[0];
        return tight ? [tight] : [];
      })();
  if (PILL.idx >= PILL.list.length) PILL.idx = 0;
  renderPillFace();
}

function renderPillFace() {
  const dot = $("pillDot");
  const c = PILL.list[PILL.idx];

  if (!c) {
    const withKey = PILL.sorted.filter((x) => x.hasKey);
    dot.className = "dot o";
    $("pillV").textContent = withKey.length ? "取数失败" : "未配置";
    $("pill").title = withKey.length ? "渠道全部取数失败,点开面板看原因" : "尚未配置密钥";
    return;
  }

  const tone = toneOf(c);
  dot.className =
    "dot" + (tone === "bad" ? " b" : tone === "warn" ? " w" : tone === "off" ? " o" : "");
  $("pillV").innerHTML =
    c.remaining === null
      ? `${esc(c.short)} ——`
      : c.kind === "percent"
        ? `${esc(c.short)} ${((remainRatio(c) ?? 0) * 100).toFixed(1)}<i>%</i>`
        : c.kind === "points"
          ? `${esc(c.short)} ${points(c.remaining)}`
          : `${esc(c.short)} ${money(c.remaining)}`;

  const bits = [c.name];  // tooltip 是纯文本,不需要转义
  const r = remainRatio(c);
  if (!c.hasKey) bits.push(c.authSource === "app" ? "未检测到登录" : "未配置密钥");
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
  if (PILL.auto) bits.push("自动:最紧张的一个");
  else if (PILL.list.length > 1) bits.push(`${PILL.idx + 1}/${PILL.list.length} 轮播`);
  $("pill").title = bits.join(" · ");

  fitPill();
  drawTrayIcon();
}

/**
 * 胶囊宽度随内容自适应:去掉图标后不再固定 200,能装下就行。
 * 用离屏 span 量文本实际宽度(带同款字体),再加圆点/按钮/内边距的固定开销。
 * 宽度变化小于 6px 就不动窗口,避免数值抖动时窗口一直跳。
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
  probe.textContent = el.textContent || "";
  document.body.appendChild(probe);
  const textW = probe.getBoundingClientRect().width;
  probe.remove();
  // 圆点 6 + 间距 8×2 + 展开按钮 22 + 内边距 16 ≈ 76 —— 这些固定件都随字号长
  const u = fsU();
  const w = Math.max(Math.round(132 * u), Math.min(Math.round(240 * u), Math.ceil(textW) + Math.round(76 * u)));
  if (Math.abs(w - pillW) >= 6) {
    pillW = w;
    if (currentForm() === "pill") applyForm("pill", false);
  }
}

/**
 * 托盘角标:和胶囊同一套逻辑(同一个渠道、同一个轮播位、同一个状态色),
 * 托盘只有 16px,写数字看不清,所以角标是纯色点,数字放 tooltip。
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
  const key = tone + "|" + text;
  if (key === trayKey) return; // 内容没变就不重画、不跨进程传数据
  trayKey = key;

  const S = 32;
  const cv = document.createElement("canvas");
  cv.width = S;
  cv.height = S;
  const g = cv.getContext("2d");
  const r = 8;
  const grad = g.createLinearGradient(0, 0, S, S);
  grad.addColorStop(0, "#5b8cff");
  grad.addColorStop(1, "#8b5bff");
  g.beginPath();
  g.moveTo(r, 0);
  g.arcTo(S, 0, S, S, r);
  g.arcTo(S, S, 0, S, r);
  g.arcTo(0, S, 0, 0, r);
  g.arcTo(0, 0, S, 0, r);
  g.closePath();
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

  const rgba = Array.from(g.getImageData(0, 0, S, S).data);
  invoke("set_tray_icon", { rgba, size: S, tooltip: text }).catch(() => {});
}

/** 设置页「渠道」页签:一卡一渠道 —— 显隐、排序、密钥、额度申请全在卡里。
    (旧版把这三件事拆在「密钥 / 渠道顺序 / 额度申请」三个分区,是"乱"的根源) */
const cardOpen = new Set(); // 展开的卡片 id;轮询重建时保持,不让用户白收起

function chDot(c) {
  if (c.hidden || !c.hasKey || !c.valid) return "o";
  if (c.limited || toneOf(c) === "bad") return "b";
  if (toneOf(c) === "warn") return "w";
  return "";
}

function renderChCards() {
  const ids = currentOrderIds();
  const list = ids.map((id) => CHANNELS.find((c) => c.id === id)).filter(Boolean);
  const map = (CFG && CFG.claimChannels) || {};
  $("chList").innerHTML = list
    .map((c, i) => {
      const isApp = c.authSource === "app";
      const cc = map[c.id];
      const on = !!(cc && cc.enabled);
      let st;
      if (c.hidden) st = "已隐藏 · 不再取数(历史快照保留)";
      else if (!c.hasKey) st = isApp ? "未检测到登录" : "未配置";
      else if (c.valid) st = isApp ? `已读取本机登录 · ${esc(c.authLabel)}` : `已配置<span class="kh" data-hint="${c.id}"></span>`;
      else st = (isApp ? "已读取本机登录" : "已配置") + " · 取数失败";

      const keyBlock = isApp
        ? `<div class="mini">凭据来自本机已登录的 ${esc(c.name)} 客户端,只读复用、不写回不刷新;` +
          `失效时打开一次 ${esc(c.name)} 即可;隐藏本渠道后连读取也会停止。</div>`
        : `<div class="ccbtns">
            <input type="password" id="key-${c.id}" autocomplete="off" spellcheck="false"
              placeholder="${c.hasKey ? "已保存 · 留空则不修改" : "粘贴 API Key"}">
            <button class="btn p" data-act="savekey" data-id="${c.id}">保存</button>
            ${c.hasKey ? `<button class="btn danger" data-act="delkey" data-id="${c.id}">删除</button>` : ""}
          </div>`;

      const claimBlock =
        c.kind === "amount"
          ? `<div class="claim-in">
              <div class="field"><span>额度申请制<span class="desc">定期申请把余额补到固定上限;上次申请时间由快照跳升自动检测</span></span>
                <div class="sw${on ? " on" : ""}" data-act="claimtoggle" data-id="${c.id}"></div></div>
              ${on
                ? `<div class="claim-grid">
                    <label>单次额度<input type="number" min="1" step="10" value="${cc.amount}" data-act="claimamount" data-id="${c.id}">元</label>
                    <label>最短间隔<input type="number" min="1" step="1" value="${cc.minIntervalDays}" data-act="claimdays" data-id="${c.id}">天</label>
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
            title="${c.hidden ? "显示(恢复取数)" : "隐藏(停止取数)"}">${c.hidden ? "–" : "👁"}</button>
          <span class="ccaret">&#9654;</span>
        </div>
        <div class="ccbody">
          ${keyBlock}
          ${claimBlock}
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

/** 轮询刷新时更新渠道卡;正在输入就不重建,免得把输入内容冲掉。 */
function refreshChCards() {
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
    } catch {
      /* 拿不到就不显示,不打扰 */
    }
  });
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
// 这里是「标准档」基准;实际尺寸走 formSize(),按字号档位 × 倍率。
const SIZES = {
  panel: [380, 560],
  compact: [380, 46],
  pill: [200, 46],
};
let pillW = 200; // 胶囊实测宽度(已是当前档位的最终逻辑像素),fitPill 维护

/** 当前字号档位下某形态的窗口尺寸。 */
function formSize(form) {
  const u = fsU();
  const base = SIZES[form] || SIZES.panel;
  const w = form === "pill" ? pillW : Math.round(base[0] * u);
  return [w, Math.round(base[1] * u)];
}

async function applyFormInner(form, remember = true) {
  if (!SIZES[form]) form = "panel"; // 白名单:老配置里的 form 可能是个已废弃的名字
  // 切换到折叠形态时管理页没有意义(它的入口都在面板上),顺手关掉
  if (form !== "panel" && manageOpen()) closeManage();
  // 只换 form-* 与 view-manage 类,**保留 docked / dock-left** ——
  // 整体覆写 className 会在吸附期间抹掉竖条形态,还把 8px 窗口强拉回形态尺寸
  document.body.classList.remove("form-panel", "form-compact", "form-pill");
  document.body.classList.add("form-" + form);
  document.body.classList.toggle("view-manage", manageOpen());
  // 面板里行要能点开,所以禁用拖拽;紧凑条整条可拖(里面没有需要点击的东西)
  const list = $("list");
  if (list) list.setAttribute("data-tauri-drag-region", form === "compact" ? "deep" : "false");
  if (remember && CFG) {
    CFG.form = form;
    invoke("set_config", { config: CFG }).catch(() => {});
  }
  // 吸附期间窗口几何归吸附逻辑管:展开态换完形态重新量一次,收起态压根不碰尺寸
  if (DOCK.on) {
    if (DOCK.open) dockLayout(true);
    return;
  }
  const [w, h] = formSize(form);
  try {
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
    setTimeout(fitCompact, 150);
  } catch (e) {
    console.error("切换形态失败", e);
  }
  clampToMonitor();
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
// 数字)。鼠标移入 150ms 后展开成吸附前的形态,移出 0.7 秒收回;把窗口从边缘
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

/** 自己的 setPosition / setSize 也会触发 moved 事件,这段时间内不判定吸附。 */
function markSelfMove() {
  DOCK.selfMove = true;
  clearTimeout(DOCK.selfMoveTimer);
  DOCK.selfMoveTimer = setTimeout(() => (DOCK.selfMove = false), 600);
}

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

/** 展开(true)/ 收起(false)。展开时贴边那一侧保持对齐,窗口不会跑到屏幕外。 */
async function dockLayoutNow(open) {
  const info = await monitorInfo();
  if (!info) return;
  const [w, h] = open ? formSize(currentForm()) : DOCK_SIZE;
  const barW = Math.round(DOCK_SIZE[0] * info.sf);
  const x = DOCK.side === "right"
    ? info.right - (open ? Math.round(w * info.sf) : barW)
    : info.left + (open ? 0 : 0);
  try {
    markSelfMove();
    await appWindow.setMinSize(null);
    await appWindow.setMaxSize(null);
    await appWindow.setSize(new T.dpi.LogicalSize(w, h));
    await appWindow.setPosition(new T.dpi.PhysicalPosition(x, DOCK.y));
    await appWindow.setMinSize(new T.dpi.LogicalSize(w, h));
    await appWindow.setMaxSize(new T.dpi.LogicalSize(w, h));
  } catch (e) {
    console.error("吸附尺寸切换失败", e);
  }
  DOCK.open = open;
  document.body.classList.toggle("docked", !open);
  document.body.classList.toggle("dock-left", DOCK.side === "left");
  if (open) setTimeout(fitCompact, 150);
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
  document.body.classList.remove("docked");
  const [w, h] = formSize(currentForm());
  const info = await monitorInfo();
  try {
    markSelfMove();
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
    const info = await monitorInfo();
    if (!info) return;
    const pos = await appWindow.outerPosition();
    const size = await appWindow.outerSize();
    const snap = Math.round(DOCK_SNAP_LOGICAL * info.sf);
    // "拖到边"的实际情况是窗口探出屏幕外(拖动时光标能到屏幕边缘,窗口会超出去),
    // 所以贴齐和探出都算 —— 只判"恰好贴齐"会漏掉绝大多数真实拖动。
    const nearRight = pos.x + size.width >= info.right - snap;
    const nearLeft = pos.x <= info.left + snap;
    if (!DOCK.on) {
      // 开关关着就不吸附:挂件随手一拖就变竖条太意外,所以默认关闭
      if (!(CFG && CFG.dockEnabled)) return;
      if (nearRight || nearLeft) await dockEnter(nearRight ? "right" : "left", pos);
    } else if (!nearRight && !nearLeft) {
      await dockExit(true); // 拖离边缘 = 解除吸附,留在松手的位置
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

/** 竖条只染色,不显示任何数字 —— 状态色是唯一信息。 */
function renderDock() {
  const body = $("dockBody");
  const c = PILL.list[0] || null; // 与胶囊同一套选择逻辑
  if (!DOCK.on) return;
  const color = c ? TONE_HEX[toneOf(c)] : "#3a4152";
  body.style.background = `linear-gradient(180deg, ${color}, ${color}cc)`;
  body.title = c ? `${c.name} · 移入展开` : "移入展开";
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
  document.body.classList.add("view-manage");
  $("btnS").classList.add("on");
}
function closeManage() {
  document.body.classList.remove("view-manage");
  $("btnS").classList.remove("on");
}

// ───────────── 事件绑定 ─────────────
$("list").addEventListener("click", async (e) => {
  const act = e.target.closest("[data-act]");
  if (act) {
    e.stopPropagation();
    const id = act.dataset.id;
    const a = act.dataset.act;
    if (a === "open-manage") {
      openManage();
    } else if (a === "range") {
      const h = Number(act.dataset.h);
      sparkHours[id] = h; // 记住选择,轮询重建详情时不会再跳回 24 小时
      sparkCache[id] = await invoke("get_series", { id, hours: h });
      document
        .querySelectorAll(`.seg button[data-id="${id}"]`)
        .forEach((b) => b.classList.toggle("on", b === act));
      const c = CHANNELS.find((x) => x.id === id);
      const body = document.querySelector(`.rbody[data-body="${id}"]`);
      if (c && body) body.innerHTML = renderDetail(c);
    }
    return;
  }

  const head = e.target.closest(".rhead");
  if (!head) return;
  const row = head.closest(".row");
  const id = row.dataset.id;
  const was = row.classList.contains("open");
  document.querySelectorAll(".row").forEach((r) => r.classList.remove("open"));
  if (!was) {
    row.classList.add("open");
    const c = CHANNELS.find((x) => x.id === id);
    const body = row.querySelector(".rbody");
    if (c && body) {
      if (!sparkCache[id]) {
        try {
          sparkCache[id] = await invoke("get_series", { id, hours: 24 });
        } catch {
          sparkCache[id] = [];
        }
      }
      body.innerHTML = renderDetail(c);
    }
  }
});

$("btnR").addEventListener("click", async (e) => {
  e.currentTarget.classList.add("spin");
  setTimeout(() => e.currentTarget.classList.remove("spin"), 720);
  await refresh();
});
$("btnS").addEventListener("click", () => (manageOpen() ? closeManage() : openManage()));
$("btnBack").addEventListener("click", closeManage);
// 设置页页签:渠道 / 显示 / 刷新与启动 / 关于
$("mTabs").addEventListener("click", (e) => {
  const b = e.target.closest("button[data-p]");
  if (!b) return;
  for (const x of $("mTabs").children) x.classList.toggle("on", x === b);
  document
    .querySelectorAll("#manage .panel")
    .forEach((p) => p.classList.toggle("on", p.dataset.p === b.dataset.p));
});
// 置顶:窗口状态与配置一起改(见 Rust 的 set_pin),按钮与设置项共用同一个值
async function setPin(enabled) {
  try {
    await invoke("set_pin", { enabled });
    CFG.alwaysOnTop = enabled;
    applyPinUI();
  } catch (e) {
    alert("置顶设置失败:" + e);
  }
}

function applyPinUI() {
  const on = !!(CFG && CFG.alwaysOnTop);
  const btn = $("btnP");
  btn.classList.toggle("on", on);
  btn.title = on ? "已置顶(点击取消)" : "窗口置顶";
  const dock = $("cfgDock");
  if (dock) dock.classList.toggle("on", !!CFG.dockEnabled);
  const sw = $("cfgPin");
  if (sw) sw.classList.toggle("on", on);
}

$("btnP").addEventListener("click", () => setPin(!CFG.alwaysOnTop));
$("btnC").addEventListener("click", () => {
  applyForm(document.body.className.includes("form-compact") ? "panel" : "compact");
});
$("btnH").addEventListener("click", () => invoke("window_cmd", { action: "hide" }));
$("lkQuit").addEventListener("click", () => invoke("window_cmd", { action: "quit" }));
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
// 拖动判定:窗口一移动就重新计时,停稳 250ms 后看是否贴边
appWindow.onMoved(() => scheduleSettleCheck());
// 胶囊轮播。不做"悬停暂停":挂件上的鼠标经常就停在胶囊附近,
// 一暂停看起来就像轮播坏了(实测踩过);点击动作只是展开面板,内容变换无副作用。
setInterval(() => {
  if (PILL.list.length < 2) return;
  if (!document.body.className.includes("form-pill")) return;
  PILL.idx = (PILL.idx + 1) % PILL.list.length;
  renderPillFace();
}, PILL_ROTATE_MS);
// 胶囊整块是拖动区(见 index.html),展开只走 ▲ 按钮,避免和拖动抢手势

/** 存配置 + 立刻重取一次:申请状态是后端按配置算的,不重取看不到变化。 */
async function saveConfigAndRefresh() {
  await invoke("set_config", { config: CFG }).catch(() => {});
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
$("manage").addEventListener("click", async (e) => {
  const act = e.target.closest("[data-act]");
  if (!act) return;
  const id = act.dataset.id;
  const a = act.dataset.act;
  if (a === "chfold") {
    // 就地开合,不整页重建 —— 展开区里可能正填着密钥
    const card = act.closest(".ccard");
    card.classList.toggle("open");
    card.classList.contains("open") ? cardOpen.add(id) : cardOpen.delete(id);
  } else if (a === "chvis") {
    const hid = CFG.hiddenChannels || (CFG.hiddenChannels = []);
    const at = hid.indexOf(id);
    at >= 0 ? hid.splice(at, 1) : hid.push(id);
    await saveConfigAndRefresh(); // 隐藏即停止取数:重取一轮,汇总/胶囊/底栏同步
  } else if (a === "savekey") {
    const input = $("key-" + id);
    const val = input ? input.value.trim() : "";
    if (!val) return;
    try {
      await invoke("set_key", { id, key: val });
      input.value = "";
      await refresh();
      renderChCards();
    } catch (err) {
      alert("保存失败:" + err);
    }
  } else if (a === "delkey") {
    if (!confirm("删除该渠道的密钥?余额数据会保留在本地。")) return;
    try {
      await invoke("delete_key", { id });
      await refresh();
      renderChCards();
    } catch (err) {
      alert("删除失败:" + err);
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
    await invoke("set_config", { config: CFG }).catch(() => {});
    render(); // 排序是前端算的,不用重新取数
  } else if (a === "pillpick") {
    const sel = CFG.pillChannels || (CFG.pillChannels = []);
    const at = sel.indexOf(id);
    if (at >= 0) sel.splice(at, 1);
    else sel.push(id);
    act.classList.toggle("on", at < 0);
    PILL.idx = 0;
    invoke("set_config", { config: CFG }).catch(() => {});
    render(); // 胶囊内容立即跟着变
  }
});

// 设置项
function bindSelect(id, key, cast = Number) {
  const el = $(id);
  el.addEventListener("change", () => {
    CFG[key] = cast(el.value);
    invoke("set_config", { config: CFG }).catch(() => {});
    if (key === "sort") render();
  });
}
function bindSwitch(id, key, onChange) {
  const el = $(id);
  el.addEventListener("click", async () => {
    CFG[key] = !CFG[key];
    el.classList.toggle("on", CFG[key]);
    invoke("set_config", { config: CFG }).catch(() => {});
    if (onChange) {
      try {
        await onChange(CFG[key]);
      } catch (err) {
        // 系统操作失败则回滚开关与配置,不让界面骗人
        CFG[key] = !CFG[key];
        el.classList.toggle("on", CFG[key]);
        invoke("set_config", { config: CFG }).catch(() => {});
        alert("操作失败:" + err);
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
  invoke("set_config", { config: CFG }).catch(() => {});
  applyFontScale();
  applyForm(currentForm());
  render();
});
bindSwitch("cfgAutostart", "autostart", (on) => invoke("set_autostart", { enabled: on }));
bindSwitch("cfgAutoUpdate", "autoCheckUpdate");
// 置顶走 set_pin:窗口与配置一起改,失败时 bindSwitch 会回滚开关
// 吸附开关:关掉时如果正吸附着,立刻解除
bindSwitch("cfgDock", "dockEnabled", async (on) => {
  if (!on && DOCK.on) await dockExit(true);
});
bindSwitch("cfgPin", "alwaysOnTop", async (on) => {
  await invoke("set_pin", { enabled: on });
  applyPinUI();
});

function applyConfigToUI() {
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
  $("cfgAutostart").classList.toggle("on", !!CFG.autostart);
  $("cfgAutoUpdate").classList.toggle("on", CFG.autoCheckUpdate !== false);
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
  if (UPDC.phase === "ready" && UPD.info) text = "新版本 " + UPD.info.version;
  else if (UPDC.phase === "downloading") text = "下载 " + UPDC.pct + "%";
  else if (UPDC.phase === "installing") text = "安装中";
  else if (UPDC.phase === "failed") text = "重试";
  chip.textContent = text;
  chip.style.display = text ? "" : "none";
  chip.title = text ? "点开查看更新详情" : "";
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
    if (show) b.style.display = "block";
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
  if (st && st.current) $("uver").textContent = "Grandettoken " + st.current;
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
      UPDC.phase = "ready";
      renderUpdChip();
      showCard(st);
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
$("lkUpd").addEventListener("click", () => {
  openManage();
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
  if (manageOpen()) {
    closeManage();
  } else if (document.body.className.includes("form-panel")) {
    applyForm("pill");
  } else {
    applyForm("panel");
  }
});

// ───────────── 启动 ─────────────
async function refresh() {
  try {
    const list = await invoke("get_channels");
    CHANNELS = list;
    render();
  } catch (e) {
    $("list").innerHTML = `<div class="empty"><b>取数失败</b>${esc(e)}</div>`;
  }
}

(async function init() {
  try {
    CFG = await invoke("get_config");
  } catch {
    CFG = {
      activeIntervalSec: 60, idleIntervalSec: 300, backoffIntervalSec: 900,
      warnPercent: 40, critPercent: 15, notify: true, autostart: false,
      collapseOnBlur: false, form: "panel", sort: "percent", pillChannels: [],
      fontScale: "md", hiddenChannels: [],
      claimChannels: {
        "4sapi": { enabled: true, amount: 200, minIntervalDays: 14, manualLastAt: null, manualSetAt: null },
      },
    };
  }
  applyConfigToUI();
  applyFontScale();
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

  listen("channels-updated", (ev) => {
    CHANNELS = ev.payload;
    render();
  });

  // 启动 30 秒后先自动检查一次;之后每 10 分钟再问一次 —— 是否真发请求由
  // Rust 侧 24 小时节流把关(最多每天一次)。之前只有启动那一次,
  // 软件连开几天就再也不会重检(实测:上次检查时间永远停在开机那一刻)。
  setTimeout(() => checkUpdate(false), 30_000);
  setInterval(() => checkUpdate(false), 10 * 60 * 1000);
})();
