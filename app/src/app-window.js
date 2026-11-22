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
let pillW = 200; // 胶囊实测内容宽(已是当前档位的最终逻辑像素),fitPill 维护

function panelLayout() {
  return window.PanelLayoutUI.normalize(CFG && CFG.panelLayout);
}
/** 当前字号档位下的面板内容尺寸(逻辑 px)。 */
function panelContentSize(layout = panelLayout()) {
  const u = fsU();
  const spec = layout === "double" ? PANEL_DOUBLE : PANEL_SINGLE;
  return [Math.round(spec[0] * u), Math.round(spec[1] * u)];
}
function compactMaxPx() {
  const u = fsU();
  // 原可由配置 compactMaxW 调节;唯一写入口 save_window_size 已随固定尺寸改版废弃(v0.3.36)。
  return clampNum(Math.round(380 * u), Math.round(COMPACT_MIN_W * u), Math.round(COMPACT_MAX_W * u));
}

/** 当前字号档位下某形态的**窗口**尺寸(内容 + 两侧透明边距)。 */
function formSize(form, monitor) {
  const u = fsU();
  if (form === "panel") {
    const [cw, ch] = panelContentSize();
    const w = cw + CARD_MARGIN * 2;
    const h = ch + CARD_MARGIN * 2;
    if (!monitor) return [w, h];
    return [Math.min(w, Math.floor((monitor.right - monitor.left) / monitor.sf)),
      Math.min(h, Math.floor((monitor.bottom - monitor.top) / monitor.sf))];
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

/** 底栏与设置使用同一组布局状态。 */
function updateLayoutUI() {
  window.PanelLayoutUI.update(panelLayout());
}

/** 切到指定的固定布局。先落盘再改窗口,失败时 saveCfg 会回滚并重画按钮。 */
async function setPanelLayout(layout) {
  return window.PanelLayoutUI.select(layout);
}

// DPI / 显示器缩放变化后按固定尺寸重新落一次;不再有用户拖拽 resize。
if (typeof appWindow.onScaleChanged === "function") {
  appWindow.onScaleChanged(async () => {
    refreshTraySize();
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
  const [w, h] = formSize(form, form === "panel" ? await monitorInfo() : null);
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
// 窗口位置落盘失败只提示一次:拖拽会反复触发,逐次弹 toast 反而盖住真正的问题。
let POS_SAVE_WARNED = false;

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
  if (!mon) return null;
  const sf = (await appWindow.scaleFactor()) || mon.scaleFactor || 1;
  // Tauri 的工作区和窗口位置都是物理坐标，只有设尺寸时才转逻辑像素。
  const { position, size } = mon.workArea || mon;
  return { mon, sf, left: position.x, right: position.x + size.width,
    top: position.y, bottom: position.y + size.height };
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
  const [w, h] = open ? formSize(currentForm(), info) : DOCK_SIZE;
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
    setTimeout(updateSizeClass, 30);
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
  const info = await monitorInfo();
  const [w, h] = formSize(currentForm(), info);
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
      const y = clampNum(keepPos ? pos.y : Math.max(info.top + 60, pos.y), info.top, maxY);
      await appWindow.setPosition(new T.dpi.PhysicalPosition(x, y));
    }
    if (currentForm() !== "panel" && currentForm() !== "compact") {
      await appWindow.setMinSize(new T.dpi.LogicalSize(w, h));
      await appWindow.setMaxSize(new T.dpi.LogicalSize(w, h));
    }
    await readSizeFromWindow();
    updateSizeClass();
  } catch (e) {
    // GUI 里 console 没人看得见(AGENTS.md 2.1.6)。解吸附失败会让窗口停在
    // 半吸附状态(尺寸解锁了、形态没换回来),用户以为操作成功,必须说出来。
    showToast("解除吸附失败:" + e, true);
  } finally {
    SIZE_APPLYING = false;
  }
  try {
    await invoke("set_pin", { enabled: !!(CFG && CFG.alwaysOnTop) });
  } catch (e) {
    // 置顶是窗口层状态:写失败时按钮仍显示"已置顶",实际没置顶,重启才对上。
    showToast("恢复置顶失败:" + e, true);
  }
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
    let pos = await appWindow.outerPosition();
    const size = await appWindow.outerSize();
    const snap = Math.round(DOCK_SNAP_LOGICAL * info.sf);
    // "拖到边"的实际情况是窗口探出屏幕外(拖动时光标能到屏幕边缘,窗口会超出去),
    // 所以贴齐和探出都算 —— 只判"恰好贴齐"会漏掉绝大多数真实拖动。
    const nearRight = pos.x + size.width >= info.right - snap;
    const nearLeft = pos.x <= info.left + snap;
    // 自由态下把落点记进配置(报告 O-6):吸附态不记,那坐标是布局算出来的。
    // 停稳 250ms 才写,一次拖拽只落一次盘,不会拖动途中反复写库。
    if (!DOCK.on) {
      const snapSide = CFG?.dockEnabled && (nearRight ? "right" : nearLeft ? "left" : null);
      if (!snapSide && currentForm() === "panel") {
        const [w, h] = formSize("panel", info);
        if (Math.abs(size.width - w * info.sf) > 2 || Math.abs(size.height - h * info.sf) > 2) {
          await applyForm("panel", false);
          pos = await appWindow.outerPosition();
        } else if (pos.x < info.left || pos.y < info.top || pos.x + size.width > info.right || pos.y + size.height > info.bottom) {
          await enqueueGeom(clampToMonitor);
          pos = await appWindow.outerPosition();
        }
      }
      if (CFG.winX !== pos.x || CFG.winY !== pos.y) {
        const prevX = CFG.winX, prevY = CFG.winY;
        CFG.winX = pos.x;
        CFG.winY = pos.y;
        // 落盘失败必须回滚内存里的坐标:否则"这一秒记住了、磁盘上没有",
        // 下次启动跳回旧位置,用户只会觉得"软件记不住窗口位置",查无可查。
        // 一次会话只提示一次,免得每拖一次弹一次。
        invoke("save_window_pos", { x: pos.x, y: pos.y }).catch((e) => {
          CFG.winX = prevX;
          CFG.winY = prevY;
          if (!POS_SAVE_WARNED) {
            POS_SAVE_WARNED = true;
            showToast("保存窗口位置失败:" + e, true);
          }
        });
      }
      // 开关关着就不吸附:挂件随手一拖就变竖条太意外,所以默认关闭
      if (snapSide) await dockEnter(snapSide, pos);
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

