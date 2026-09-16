/* TokenScope 前端。经 window.__TAURI__ 调 Rust 命令(withGlobalTauri)。 */
const T = window.__TAURI__;
const invoke = T.core.invoke;
const listen = T.event.listen;
const appWindow = T.window.getCurrentWindow();

const $ = (id) => document.getElementById(id);

let CHANNELS = [];
let CFG = null;
let sparkCache = {};   // { [id]: {24:[],168:[],720:[]} }

const emit = (ev, payload) => T.event.emit(ev, payload);

// ───────────── 格式化 ─────────────
const money = (v) => (v === null || v === undefined ? "——" : "¥" + v.toFixed(2));

function relTime(ts) {
  if (!ts) return "从未";
  const s = Math.floor(Date.now() / 1000) - ts;
  if (s < 60) return "刚刚";
  if (s < 3600) return Math.floor(s / 60) + " 分钟前";
  if (s < 86400) return Math.floor(s / 3600) + " 小时前";
  return Math.floor(s / 86400) + " 天前";
}

/** 剩余比例。充值型无 total 时返回 null —— 不画进度条,也不参与百分比排序。 */
function remainRatio(c) {
  if (c.remaining === null || c.total === null || !c.total) return null;
  return c.remaining / c.total;
}

function toneOf(c) {
  if (!c.hasKey || !c.valid) return "off";
  const r = remainRatio(c);
  if (r === null) return "ok";
  if (r < CFG.critPercent / 100) return "bad";
  if (r < CFG.warnPercent / 100) return "warn";
  return "ok";
}

const TONE_COLOR = { ok: "var(--ok)", warn: "var(--warn)", bad: "var(--bad)", off: "var(--tx3)" };
const TONE_HEX = { ok: "#3ecf8e", warn: "#f0b23c", bad: "#f0554d", off: "#333a4a" };

/** 主排序:剩余百分比升序,最紧张的置顶。 */
function sortChannels(list) {
  const mode = CFG.sort;
  const score = (c) => {
    if (!c.hasKey) return 9999;
    if (!c.valid) return 5000;
    if (mode === "balance") return -(c.remaining ?? 0);
    if (mode === "dayUsage") return -(c.day ?? 0);
    const r = remainRatio(c);
    return r === null ? 4000 : r * 100;
  };
  return [...list].sort((a, b) => score(a) - score(b));
}

// ───────────── 渲染 ─────────────
function renderSummary() {
  // 汇总只累加金额型渠道 —— 百分比和金额不能相加
  const am = CHANNELS.filter((c) => c.kind === "amount" && c.hasKey && c.remaining !== null);
  const sum = (f) => {
    const vals = am.map((c) => c[f]).filter((v) => v !== null && v !== undefined);
    if (!vals.length) return null;
    return vals.reduce((a, b) => a + b, 0);
  };
  const cells = [
    ["今日消耗", sum("day"), true],
    ["本周消耗", sum("week"), true],
    ["本月消耗", sum("month"), false],
  ];
  if (!am.length) {
    $("sum").innerHTML = "";
    return;
  }
  $("sum").innerHTML = cells
    .map(([label, v, est]) => {
      const txt = v === null ? "——" : money(v);
      const note = v === null ? "数据不足" : est ? "推算" : `${am.length} 个渠道`;
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
  // 配额型(OpenCode Go)的展示基准是三个限流窗口本身:
  // 大数字取最紧的那个窗口,下面三行各带一条迷你条
  const wins = isPct && c.windows && c.windows.length ? c.windows : [];
  const tightest = wins.length
    ? wins.reduce((a, b) => (b.remainPercent < a.remainPercent ? b : a))
    : null;
  const limitedWin = wins.find((w) => w.status === "rate-limited") || null;

  let main;
  if (noKey) main = "——";
  else if (c.remaining === null) main = "——";
  else if (isPct) main = c.remaining.toFixed(1) + '<i class="pct">%</i>';
  else {
    const [int, dec] = c.remaining.toFixed(2).split(".");
    main = "¥" + int + '<i class="dec">.' + dec + "</i>";
  }

  // 第二行给状态语义,大数字下方给数值口径,两处不重复
  let sub;
  if (noKey) sub = "未配置密钥";
  else if (failed) sub = "取数失败";
  else if (limitedWin) sub = `${limitedWin.label}窗触顶 · 已限流`;
  else if (tightest) sub = `${tightest.label}窗最紧`;
  else if (c.limited) sub = "已限流";
  else if (pct !== null) sub = `额度 ¥${c.total}`;
  else sub = ""; // 金额型拿到多少就是可用多少,不加说明

  let subVal;
  if (noKey) subVal = "待配置";
  else if (failed) subVal = c.stale ? "上次快照" : "失联";
  else if (tightest) subVal = `${tightest.label}窗`;
  else if (pct !== null) subVal = `剩 ${pct}%`;
  else subVal = "";

  // 状态点
  let dot = "";
  if (noKey) dot = '<span class="dot o"></span>';
  else if (failed) dot = '<span class="dot o"></span>';
  else if (c.limited || tone === "bad") dot = '<span class="dot b"></span>';
  else if (tone === "warn") dot = '<span class="dot w"></span>';
  else dot = '<span class="dot"></span>';

  // 第三段:金额型显示日/周/月;配额型显示三个限流窗口,每行一条迷你条
  let useHtml;
  if (wins.length) {
    useHtml = `<div class="wins">${wins
      .map((w) => {
        const wr = Math.max(0, Math.min(100, w.remainPercent));
        const lim = w.status === "rate-limited";
        const wt = lim ? "bad" : wr < CFG.critPercent ? "bad" : wr < CFG.warnPercent ? "warn" : "ok";
        return `<div class="win">
          <span class="wl">${w.label}</span>
          <i class="wb"><i style="width:${wr}%;background:${TONE_HEX[wt]}"></i></i>
          <b class="wv" style="color:${lim ? "var(--bad)" : TONE_COLOR[wt]}">${wr.toFixed(1)}%</b>
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

  // 配额型的三条迷你条已经表达了余量,不再重复画顶部大条
  const bar =
    pct !== null && !failed && !noKey && !wins.length
      ? `<div class="bar"><i style="width:${pct}%;background:${TONE_HEX[tone]}"></i></div>`
      : "";

  const src = c.stale
    ? `<div class="hint" style="color:var(--warn)">显示的是 ${relTime(c.updatedAt)} 的成功快照${
        c.error ? " · " + c.error : ""
      }</div>`
    : `<div class="hint">更新于 ${relTime(c.updatedAt)}</div>`;

  return `<div class="row" data-id="${c.id}" data-i="${i}">
    <div class="rhead">
      <div class="r1">
        <div class="ico" style="background:${noKey || failed ? "#333a4a" : c.color}">${c.short}</div>
        <div class="nm">
          <div class="n"><span class="nn">${c.name}</span>${dot}</div>
          <div class="s">${c.unstable ? "未公开接口 · " : ""}${sub}</div>
        </div>
        <div class="val">
          <div class="v" style="color:${TONE_COLOR[tone]}">${main}</div>
          <div class="p">${subVal}</div>
        </div>
        <div class="caret">&#9654;</div>
      </div>
      ${bar}
      ${useHtml}
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
    return `<div class="rbody-in">
      <div class="hint" style="margin:0 0 8px">未配置密钥,尚未开始取数。密钥只写入 Windows 凭据管理器,不会进配置文件、数据库或日志。</div>
      ${manageLink}
    </div>`;
  }

  const kv = c.extra && c.extra.length ? c.extra : [];
  const kvHtml = kv.length
    ? `<div class="kv">${kv
        .map(([k, v]) => `<div><span>${k}</span><b>${v}</b></div>`)
        .join("")}</div>`
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

  const body = [];
  body.push(
    `<div class="seg">
       <button class="on" data-act="range" data-id="${c.id}" data-h="24">近 24 小时</button>
       <button data-act="range" data-id="${c.id}" data-h="168">近 7 天</button>
       <button data-act="range" data-id="${c.id}" data-h="720">近 30 天</button>
     </div>`
  );
  if (c.error) {
    body.push(
      `<div class="hint" style="color:var(--bad);margin:0 0 8px">${c.error}${
        c.stale ? " · 下列数值为上次成功快照" : ""
      }</div>`
    );
  }
  body.push(kvHtml);
  body.push(sparkHtml);
  if (c.estimated && c.kind === "amount") {
    body.push(
      `<div class="hint" style="margin:0 0 8px">消耗为本地快照推算值 —— 该接口只返回当前余额,不含累计消耗;程序未运行的时段不计入。</div>`
    );
  }
  body.push(manageLink);

  return `<div class="rbody-in">${body.join("")}</div>`;
}

function render() {
  const sorted = sortChannels(CHANNELS);
  $("cnt").textContent = CHANNELS.filter((c) => c.hasKey).length + " / " + CHANNELS.length + " 个渠道";

  renderSummary();

  if (!CHANNELS.length) {
    $("list").innerHTML = `<div class="empty"><b>正在取数…</b>首次启动需要几秒</div>`;
    return;
  }

  const openId = document.querySelector(".row.open")?.dataset.id;

  // 一个密钥都没配时不铺灰行,直接给一张引导卡 —— 展示区只承载真实数据
  const configured = sorted.some((c) => c.hasKey);
  $("list").innerHTML = configured
    ? sorted.map((c, i) => renderRow(c, i)).join("")
    : `<div class="empty">
         <div class="ek">&#128273;</div>
         <b>还没有配置任何渠道</b>
         填入至少一个 API Key 后开始取数<br>密钥只写入 Windows 凭据管理器,界面保存后不回显
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

  // 底栏状态
  const withKey = CHANNELS.filter((c) => c.hasKey);
  const bad = withKey.filter((c) => !c.valid).length;
  const warn = withKey.filter((c) => c.valid && (c.limited || toneOf(c) === "bad")).length;
  $("fdot").className =
    "dot" + (bad ? " o" : warn ? " b" : withKey.length ? "" : " o");
  $("fstat").textContent = !withKey.length
    ? "未配置渠道"
    : `${withKey.length - bad} 正常${warn ? ` · ${warn} 预警` : ""}${bad ? ` · ${bad} 失联` : ""}`;

  renderPill(sorted);
  renderPillPick();
  fitCompact();
  refreshKeyStatuses();
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
    ? picked.map((id) => CHANNELS.find((c) => c.id === id)).filter(Boolean)
    : (() => {
        const active = sorted.filter((c) => c.hasKey);
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
      ? `${c.short} ——`
      : c.kind === "percent"
        ? `${c.short} ${c.remaining.toFixed(1)}<i>%</i>`
        : `${c.short} ${money(c.remaining)}`;

  const bits = [c.name];
  const r = remainRatio(c);
  if (!c.hasKey) bits.push("未配置密钥");
  else if (c.remaining === null) bits.push("取数失败");
  else {
    if (r !== null) bits.push(`剩 ${Math.round(r * 100)}%`);
    if (c.limited) bits.push("已限流");
  }
  if (PILL.auto) bits.push("自动:最紧张的一个");
  else if (PILL.list.length > 1) bits.push(`${PILL.idx + 1}/${PILL.list.length} 轮播`);
  $("pill").title = bits.join(" · ");
}

/** 管理页里的胶囊渠道选择:点一下加入/移出轮播列表。 */
function renderPillPick() {
  const sel = (CFG && CFG.pillChannels) || [];
  $("cfgPill").innerHTML = CHANNELS.map(
    (c) =>
      `<button class="pick${sel.includes(c.id) ? " on" : ""}" data-act="pillpick" data-id="${c.id}">${c.name}</button>`
  ).join("");
}

// ───────────── 形态切换 ─────────────
// 每种形态都是固定尺寸:拖标题栏只能移动窗口,拉不动大小。
// resizable(false) 去掉缩放宽边,min/max 双钳位兜底(即便有残留的缩放边框也拉不动)。
const SIZES = {
  panel: [380, 560],
  compact: [380, 46],
  pill: [200, 46],
};

async function applyForm(form, remember = true) {
  document.body.className =
    "form-" + form + (manageOpen() ? " view-manage" : "");
  const [w, h] = SIZES[form] || SIZES.panel;
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
    // 视口变化后重新量一次紧凑条,决定尾巴要收几个进 "+N"
    setTimeout(fitCompact, 150);
  } catch (e) {
    console.error("切换形态失败", e);
  }
  if (remember && CFG) {
    CFG.form = form;
    invoke("set_config", { config: CFG }).catch(() => {});
  }
}

// ───────────── 设置与管理(密钥与设置同页,与展示视图分开) ─────────────
const manageOpen = () => document.body.classList.contains("view-manage");

function renderKeyRows() {
  $("keyList").innerHTML = CHANNELS.map((c) => {
    const state = !c.hasKey
      ? '<span class="dot o"></span>未配置'
      : c.valid
        ? '<span class="dot"></span>已配置'
        : '<span class="dot o"></span>已配置 · 取数失败';
    return `<div class="krow" data-id="${c.id}">
      <div class="krow1">
        <div class="ico" style="background:${c.hasKey ? c.color : "#333a4a"}">${c.short}</div>
        <div class="kmeta">
          <div class="kn">${c.name}</div>
          <div class="ks">${state}</div>
        </div>
        ${
          c.hasKey
            ? `<button class="btn danger" data-act="delkey" data-id="${c.id}">删除</button>`
            : ""
        }
      </div>
      <div class="krow2">
        <input type="password" id="key-${c.id}" autocomplete="off" spellcheck="false"
          placeholder="${c.hasKey ? "已保存 · 留空则不修改" : "粘贴 API Key"}">
        <button class="btn p" data-act="savekey" data-id="${c.id}">保存</button>
      </div>
    </div>`;
  }).join("");
}

/** 轮询刷新时更新状态;正在输入就不重建,免得把输入内容冲掉。 */
function refreshKeyStatuses() {
  if (!manageOpen()) return;
  const typing = [...$("keyList").querySelectorAll("input")].some((i) => i.value);
  if (!typing) renderKeyRows();
}

function openManage() {
  renderKeyRows();
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
$("btnP").addEventListener("click", async (e) => {
  await invoke("window_cmd", { action: "pin" });
  e.currentTarget.classList.toggle("on");
});
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
// 胶囊轮播。不做"悬停暂停":挂件上的鼠标经常就停在胶囊附近,
// 一暂停看起来就像轮播坏了(实测踩过);点击动作只是展开面板,内容变换无副作用。
setInterval(() => {
  if (PILL.list.length < 2) return;
  if (!document.body.className.includes("form-pill")) return;
  PILL.idx = (PILL.idx + 1) % PILL.list.length;
  renderPillFace();
}, PILL_ROTATE_MS);
$("pill").addEventListener("click", () => applyForm("panel"));

// 密钥的保存 / 删除只在管理页里发生
$("manage").addEventListener("click", async (e) => {
  const act = e.target.closest("[data-act]");
  if (!act) return;
  const id = act.dataset.id;
  const a = act.dataset.act;
  if (a === "savekey") {
    const input = $("key-" + id);
    const val = input ? input.value.trim() : "";
    if (!val) return;
    try {
      await invoke("set_key", { id, key: val });
      input.value = "";
      await refresh();
      renderKeyRows();
    } catch (err) {
      alert("保存失败:" + err);
    }
  } else if (a === "delkey") {
    if (!confirm("删除该渠道的密钥?余额数据会保留在本地。")) return;
    try {
      await invoke("delete_key", { id });
      await refresh();
      renderKeyRows();
    } catch (err) {
      alert("删除失败:" + err);
    }
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
    emit("config-changed", CFG);
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
bindSwitch("cfgAutostart", "autostart", (on) => invoke("set_autostart", { enabled: on }));
bindSwitch("cfgNotify", "notify");
bindSwitch("cfgBlur", "collapseOnBlur");

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
  $("cfgAutostart").classList.toggle("on", !!CFG.autostart);
  $("cfgNotify").classList.toggle("on", !!CFG.notify);
  $("cfgBlur").classList.toggle("on", !!CFG.collapseOnBlur);
}

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
    $("list").innerHTML = `<div class="empty"><b>取数失败</b>${e}</div>`;
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
    };
  }
  applyConfigToUI();
  await applyForm(CFG.form || "panel", false);
  await refresh();

  listen("channels-updated", (ev) => {
    CHANNELS = ev.payload;
    render();
  });
})();
