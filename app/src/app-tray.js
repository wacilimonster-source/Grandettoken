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
    if (c.kind === "points" && c.expiringSoon && !c.expiryStale && !c.stale) {
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
let PILL_MEASURE_KEY = "", PILL_MEASURE_W = 0;

function fitPill() {
  const el = $("pillV");
  if (!el) return;
  const text = el.textContent || "";
  const u = fsU();
  const key = text + "|" + u + "|" + document.body.className;
  let textW = PILL_MEASURE_W;
  if (key !== PILL_MEASURE_KEY) {
    const probe = document.createElement("span");
    probe.style.cssText = "position:absolute;left:-9999px;top:-9999px;white-space:nowrap;visibility:hidden";
    // 逐项拷字体,不要用 `font` 简写 —— 在 Chromium 里 getComputedStyle().font 经常是空串;
    // fontVariantNumeric 决定 tabular-nums 等宽数字宽度(实测漏拷会导致 "91.5%" 被裁成 "91…")。
    const cs = getComputedStyle(el);
    for (const k of ["fontFamily", "fontSize", "fontWeight", "fontStyle", "letterSpacing", "fontVariantNumeric"]) probe.style[k] = cs[k];
    probe.textContent = text;
    document.body.appendChild(probe);
    textW = probe.getBoundingClientRect().width;
    probe.remove();
    PILL_MEASURE_KEY = key;
    PILL_MEASURE_W = textW;
  }
  // 圆点 6 + 图标 26 + 间距 8×4 + 展开按钮 22 + 内边距 16 + 边框/缓冲 ≈ 104
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

/** 托盘缩写:低余额保留一位小数;去掉无意义的 .0,较大的万位数取整。完整数值仍在 tooltip。 */
function trayAbbr(v) {
  const a = Math.abs(v);
  const compact = (n) => {
    const s = n.toFixed(1);
    // 非零数被舍入到 0.0 时仍保留小数,避免把接近零误写成精确的零。
    return s.endsWith(".0") && (n === 0 || Math.abs(n) >= 0.05) ? s.slice(0, -2) : s;
  };
  // 小于 10 时取整会把"只剩 5.28"报成 5,把"只剩 0.4"报成 0 —— 越紧张越需要精度
  if (a < 10) return compact(v);
  if (a < 1000) return String(Math.round(v));
  if (a < 10000) return compact(v / 1000) + "k";
  if (a < 100000) return compact(v / 10000) + "w";
  return String(Math.round(v / 10000)) + "w";
}

/** 当前托盘渠道的概览数;拿不到可信数字(未配置/失败/关闭)返回 "" → 画回品牌样式。 */
function trayNumber(c) {
  if (!c || !c.hasKey || (!c.valid && !c.stale)) return "";
  if (c.kind === "percent") {
    const r = remainRatio(c);
    return Number.isFinite(r) ? String(Math.round(r * 100)) : "";
  }
  if (!Number.isFinite(c.remaining)) return "";
  return trayAbbr(c.remaining);
}

// 窄体系统字体让多位数字在 16px 下仍有足够高度,同时保留自然的字形。
// 按实际墨迹边界量字号和居中,避免按字符个数估算导致小数或单位被挤小、裁切。
function drawTrayNumber(g, size, num, tone) {
  const padX = Math.max(1, Math.round(size * 0.06));
  const padY = Math.max(1, Math.round(size * 0.1));
  const availableWidth = size - padX * 2;
  const availableHeight = size - padY * 2;
  g.fillStyle = tone === "off" ? "#fff" : "#141921";
  g.textAlign = "left";
  g.textBaseline = "alphabetic";
  let bounds, fits = false;
  for (let fontSize = size * 1.75; fontSize >= 4; fontSize -= 0.25) {
    g.font = `700 ${fontSize}px "Bahnschrift SemiCondensed", "Segoe UI", sans-serif`;
    const m = g.measureText(num);
    // 老版 WebView 或测试桩可能没有实际墨迹边界,退回字体近似尺寸。
    const left = Number.isFinite(m.actualBoundingBoxLeft) ? m.actualBoundingBoxLeft : 0;
    const right = Number.isFinite(m.actualBoundingBoxRight) ? m.actualBoundingBoxRight : m.width;
    const ascent = Number.isFinite(m.actualBoundingBoxAscent) ? m.actualBoundingBoxAscent : fontSize * 0.8;
    const descent = Number.isFinite(m.actualBoundingBoxDescent) ? m.actualBoundingBoxDescent : fontSize * 0.2;
    bounds = { left, right, ascent, descent };
    if (left + right <= availableWidth && ascent + descent <= availableHeight) {
      fits = true;
      break;
    }
  }
  if (!fits) {
    // 极长缩写按墨迹边界等比缩小,避免字体的左右突出部分被裁掉。
    const width = bounds.left + bounds.right, height = bounds.ascent + bounds.descent;
    const scale = Math.min(1, (availableWidth - 1) / width, (availableHeight - 1) / height);
    g.save();
    g.translate(size / 2, size / 2);
    g.scale(scale, scale);
    g.fillText(num, (bounds.left - bounds.right) / 2, (bounds.ascent - bounds.descent) / 2);
    g.restore();
    return;
  }
  const x = (size - bounds.left - bounds.right) / 2 + bounds.left;
  const y = (size - bounds.ascent - bounds.descent) / 2 + bounds.ascent;
  // 半像素定位兼顾居中与清晰,不使用额外描边或非等比压缩。
  g.fillText(num, Math.round(x * 2) / 2, Math.round(y * 2) / 2);
}

/**
 * 托盘图标。两种画法(设置→显示→托盘数字切换):
 *  - 数字式:状态色整底 + 高对比数字,按托盘实际物理像素绘制;
 *  - 品牌式:应用图标 + 右下状态角点(无可信数字时的回退,也是开关关闭的样式)。
 * tooltip 永远带完整信息。
 */
let trayKey = "";
let traySize = 16;
let traySizeBusy = false;
let trayFrame = null, traySending = false;
let trayLastSentAt = 0;
let trayMinGapMs = 1500; // 真需要换图时的最小写入间隔,连续变化只留最后一帧
let trayTip = "";
let trayTipChain = Promise.resolve();
let trayBrandReady = false;

async function refreshTraySize() {
  if (traySizeBusy) return;
  traySizeBusy = true;
  try {
    const size = await invoke("tray_icon_size");
    if (Number.isInteger(size) && size >= 8 && size <= 64 && size !== traySize) {
      traySize = size;
      trayKey = "";
      drawTrayIcon();
    }
  } catch (e) {
    console.warn("获取托盘图标尺寸失败", e);
  } finally {
    traySizeBusy = false;
  }
}

// 串行发送并合并待更新帧,避免 DPI 变化时旧尺寸的图标后到并覆盖新图标。
// 真需要换图时也留最小间隔:连续变化只留最后一帧,不会把每帧都送进
// Shell_NotifyIcon 排队闪一串。首次发送(时间戳为 0)不受影响。
function sendTrayFrame(frame) {
  trayFrame = frame;
  if (traySending) return;
  traySending = true;
  (async () => {
    while (trayFrame) {
      const wait = trayLastSentAt + trayMinGapMs - Date.now();
      if (wait > 0) await new Promise((r) => setTimeout(r, wait));
      const current = trayFrame;
      trayFrame = null;
      try {
        await invoke("set_tray_icon", current.args);
      } catch (e) {
        if (trayKey === current.key) trayKey = "";
        console.error("更新托盘图标失败", e);
      } finally {
        trayLastSentAt = Date.now();
      }
    }
    traySending = false;
  })();
}

/** tooltip 单独走 NIF_TIP:文字每轮都可能变,但不该连带重画图标。
    串行发送,避免慢的那次后到把新文字盖回旧文字。 */
function sendTrayTip(text) {
  if (text === trayTip) return;
  trayTip = text;
  trayTipChain = trayTipChain
    .then(() => invoke("set_tray_tooltip", { tooltip: text }))
    .catch((e) => {
      if (trayTip === text) trayTip = "";
      console.error("更新托盘提示失败", e);
    });
}

const trayBrand = new Image();
// 品牌图没加载完不发帧:否则会先画一块灰底、加载完再换真图,开机多闪一次。
trayBrand.onload = () => {
  trayBrandReady = true;
  trayKey = "";
  drawTrayIcon();
};
// 图片缺失也不能永远停在启动时的默认图标上:退回灰底占位。
trayBrand.onerror = () => {
  trayBrandReady = true;
  trayKey = "";
  drawTrayIcon();
};
trayBrand.src = "brand/logo-tray.png";
refreshTraySize();
// 任务栏可以在主窗口隐藏时移动到另一台显示器,不能只监听窗口的 DPI 变化。
setInterval(refreshTraySize, 30_000);
function drawTrayIcon() {
  const c = PILL.list[PILL.idx] || null;
  const tone = c ? toneOf(c) : "off";
  const text = c
    ? `${c.name} ${($("pillV").textContent || "").trim()}`
    : PILL.sorted.some((x) => x.hasKey && !x.hidden)
      ? "渠道全部取数失败"
      : "尚未配置密钥";
  const num = CFG && CFG.trayShowNumber !== false ? trayNumber(c) : "";
  // tooltip 与图标走两条通道:胶囊的小数每轮都在变,但它不该参与图标去重 ——
  // 以前 text 进 key,取数刷新会把逐像素相同的图重发一遍,Explorer 重绘即"闪一下"。
  sendTrayTip(text);
  // 品牌图就绪前保持启动时的默认图标,避免"灰块 → 真图"多换一次。
  if (!num && !trayBrandReady) return;
  // 键只含真正画进画面的内容:尺寸 + 状态色 + 数字(或品牌模式)。
  const key = traySize + "|" + tone + "|" + (num || "brand");
  if (key === trayKey) return; // 内容没变就不重画、不跨进程传数据
  trayKey = key;

  const S = traySize;
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
    drawTrayNumber(g, S, num, tone);
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

  // 像素走 base64:RGBA 若按数组过 IPC 会膨胀成很长的 JSON,
  // 胶囊每几秒轮播一次就重发一遍(报告 O-9)。分块转 binary string 是为了
  // 避开 fromCharCode 一次传几千年参数的栈溢出上限。
  const px = g.getImageData(0, 0, S, S).data;
  let bin = "";
  for (let i = 0; i < px.length; i += 0x8000) {
    bin += String.fromCharCode.apply(null, px.subarray(i, i + 0x8000));
  }
  const rgbaB64 = btoa(bin);
  sendTrayFrame({ key, args: { rgbaB64, size: S } });
}

