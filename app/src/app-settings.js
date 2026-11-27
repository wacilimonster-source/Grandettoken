// 置顶:窗口状态与配置一起改(见 Rust 的 set_pin),按钮与设置项共用同一个值
async function setPin(enabled) {
  try {
    await window.GT_API.config.setPin({ enabled });
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
  window.GT_API.desktop.windowCmd({ action: "hide" }).catch((e) => showToast("隐藏失败:" + e, true))
);
$("lkQuit").addEventListener("click", () =>
  window.GT_API.desktop.windowCmd({ action: "quit" }).catch((e) => showToast("退出失败:" + e, true))
);
$("lkKeys").addEventListener("click", () => {
  // 入口常驻(2026-11-08):管理页里也固定显示,行为改成开关式 ——
  // 未在管理页时进入;已在管理页时再点一次则退出回首页,不必挪到页头找「← 返回」
  if (manageOpen()) {
    closeManage();
    return;
  }
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
  } catch {
    // silent: 纯视觉预览线,取不到显示器/窗口几何就不亮,无需打断拖拽去提示。
  }
}
// I6:吸附展开态的显式收回按钮(移出鼠标 / Esc 之外的第三条出路)
$("dockClose").addEventListener("click", () => {
  if (DOCK.on && DOCK.open) dockLayout(false);
});
// 胶囊轮播。不做"悬停暂停":挂件上的鼠标经常就停在胶囊附近,
// 一暂停看起来就像轮播坏了(实测踩过);点击动作只是展开面板,内容变换无副作用。
// 折中:悬停时节奏延长 3 倍(每 3 拍才进一步),既不"像坏了"也不打断阅读。
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
    if (WINDOW_VISIBLE && PILL.list.length >= 2 && document.body.className.includes("form-pill")) {
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
    await window.GT_API.config.setConfig({ config: CFG });
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
    await window.GT_API.config.setConfig({ config: CFG });
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
    window.VaultUI.hideReveal();
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
  } else if (e.key === "Enter" && !e.target.closest("button, summary")) {
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
  if (a === "ckhretry") {
    // 签到历史读失败后的「重试」(报告 L7)
    loadCheckinHistory();
    return;
  }
  if (a === "cklogin") {
    // 凭据失效的指路,各渠道各回各家(报告 B6):Qoder 的签到凭据来自 Qoder 桌面端,
    // 以前统一落到 Trae 文案 —— 打开 Trae 对 Qoder 毫无作用,4 个可签到渠道里 2 个被误导。
    if (id === "workbuddy") {
      await window.GT_API.channels.workbuddyLogin().catch((e) => showToast("打不开登录窗口:" + e, true));
    } else if (id === "doubao") {
      await window.GT_API.desktop.doubaoLogin().catch((e) => showToast("打不开登录窗口:" + e, true));
    } else if (CHECKIN_QODER.has(id)) {
      showToast("打开一次 Qoder 桌面端即可(它自己会续期)");
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
      await window.GT_API.desktop.copyText({ text: v });
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
      await window.GT_API.desktop.copyText({ text });
      showToast("已复制(不含密钥)");
    } catch (e) {
      showToast("复制失败:" + e, true);
    }
    return;
  }
  if (a === "infokeycopy") {
    const src = (MDL.sheet && MDL.sheet.source) || MDL.src;
    try {
      const msg = await window.GT_API.desktop.keyCopy({ id: src });
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
      const key = await window.GT_API.desktop.keyReveal({ id: src });
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
      const msg = await window.GT_API.models.copyModelBundle({
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
      const msg = await window.GT_API.inject.rollback({ backup: id });
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
      await window.GT_API.desktop.openPath({ path: id });
    } catch (err) {
      showToast("打开失败:" + err, true);
    }
    return;
  }
  if (a === "chfold") {
    // 就地开合,不整页重建 —— 展开区里可能正填着密钥
    const card = act.closest(".ccard");
    act.setAttribute("aria-expanded", String(card.classList.toggle("open")));
    card.classList.contains("open") ? cardOpen.add(id) : cardOpen.delete(id);
  } else if (a === "vfold") {
    window.VaultUI.toggle(id, renderVault);
  } else if (a === "vnew") {
    window.VaultUI.beginForm();
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
      window.VaultUI.beginForm();
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
      const r = await window.GT_API.vault.delete(force ? { id, force: true } : { id });
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
      window.VaultUI.reset();
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
      const msg = await window.GT_API.vault.copy({ id });
      showToast(msg + " · 剪贴板对其他程序可见");
    } catch (e) {
      showToast("复制失败:" + e, true);
    }
  } else if (a === "vcopy-endpoint") {
    const item = VAULT.find((x) => x.id === id);
    const endpoint = item && (item.apiEndpoint || "").trim();
    if (!endpoint) { showToast("该密钥尚未填写调用端点", true); return; }
    try {
      await window.GT_API.desktop.copyText({ text: endpoint });
      showToast("调用端点已复制");
    } catch (e) {
      showToast("复制端点失败:" + e, true);
    }
  } else if (a === "vreveal") {
    await window.VaultUI.reveal(id, window.GT_API.vault.reveal, renderVault, showToast);
  } else if (a === "keycopy") {
    try {
      const msg = await window.GT_API.desktop.keyCopy({ id });
      showToast(msg + " · 剪贴板对其他程序可见");
    } catch (e) {
      showToast("复制失败:" + e, true);
    }
  } else if (a === "probe") {
    // 「测试连接」:真打一次接口,结果如实贴在按钮下面(不落快照,不改任何数据)
    const box = document.querySelector(`.prb[data-prb="${id}"]`);
    if (box) box.innerHTML = '<span class="mini">正在测试…</span>';
    try {
      const r = await window.GT_API.channels.probeChannel({ id });
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
      const msg = await window.GT_API.channels.customRemove({ id });
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
      const key = await window.GT_API.desktop.keyReveal({ id });
      // 计时器归这个盒子自己:模型弹层那边的明文不受影响(F5)
      revealBoxShow(box, key);
    } catch (e) {
      showToast("取密钥失败:" + e, true);
    }
  } else if (a === "chshowall") {
    CFG.hiddenChannels = [];
    for (const c of CHANNELS) c.hidden = false;
    render();
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
      await saveConfigAndRefresh();
      return;
    }
    at >= 0 ? hid.splice(at, 1) : hid.push(id);
    // **先本地翻一次**:取数有 5 秒去重窗口(fetch_shared),保存后立刻重取可能拿到旧视图,
    // 卡片会原地不动 —— 那就是用户看到的"没反应"。这里先按意图画出结果,下一轮取数再对齐。
    if (cc) cc.hidden = at < 0;
    render();
    await saveConfigAndRefresh(); // 隐藏即停止取数:重取一轮,汇总/胶囊/底栏同步
  } else if (a === "qoderweblogin") {
    await qoderWebLogin(id, false);
  } else if (a === "qoderwebswitch") {
    // 换号:先退出该区域当前的网页登录,再开窗。否则旧会话仍然有效,
    // 登录窗会被判成"已登录"并自动关掉,用户根本没机会换账号。
    await qoderWebLogin(id, true);
  } else if (a === "wblogin") {
    // 打开 WorkBuddy 网页登录窗。令牌由 Rust 侧从窗口导航里接住,前端只负责状态与提示,
    // 结果走 workbuddy-login 事件(成功/取消/失败都从那里回)。
    try {
      await window.GT_API.channels.workbuddyLogin();
      WB_LOGGING = true;
      renderChCards();
      showToast("登录窗口已打开 · 完成登录后会自动关窗");
    } catch (err) {
      showToast("打不开登录窗口:" + err, true);
    }
  } else if (a === "dblogin") {
    // 豆包登录窗:同一条纪律,结果走 doubao-login 事件
    try {
      await window.GT_API.desktop.doubaoLogin();
      DB_LOGGING = true;
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
      await window.GT_API.config.setKey({ id, key: val });
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
      await window.GT_API.config.setKeyRef({ id, vaultId: vid });
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
      await window.GT_API.config.clearKeyRef({ id });
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
      const msg = await window.GT_API.desktop.deleteKey({ id });
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
      GRANTS[id] = await window.GT_API.channels.creditGrants({ id, days: 30 });
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
    if (window.AntigravityUI) CFG.pillChannels = window.AntigravityUI.expandSelection(CFG.pillChannels || []);
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
bindSwitch("cfgAutostart", "autostart", (on) => window.GT_API.desktop.setAutostart({ enabled: on }));
bindHibernateSwitch();
bindSwitch("cfgAutoUpdate", "autoCheckUpdate");
// 保存后由 Rust 更新托盘图标。
bindSwitch("cfgTrayNum", "trayShowNumber");
// 告急呼吸(动效 G):全应用唯一循环动画,默认关;开关只拨 body.pulse 这个总闸
bindSwitch("cfgPulse", "alertPulse", (on) => document.body.classList.toggle("pulse", !!on));
// 单击胶囊速览(方案 C):关掉后单击无动作,▲ 仍展开面板
bindSwitch("cfgPeek", "peekOnClick");
// 跌破红线的系统通知(裁决 3A):判定全在 Rust 侧(与 UI 同一口径),这里只是开关
bindSwitch("cfgNotify", "notifyOnCrit");
// 签到"需要处理"的提醒:同一天同一个问题只提示一次;关掉后不再弹
bindSwitch("cfgCheckinNotify", "checkinNotify");
// MiniMax 托管(AGENTS 2.4 披露项):临近过期自动续期并原子回写客户端凭据文件;
// 关掉 = 只读模式 —— 不刷新也不回写,渠道在令牌过期后要靠打开官方客户端续期
bindSwitch("cfgMinimaxWriteback", "minimaxWriteback");
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
      const r = (await window.GT_API.channels.customTest({ ch: draft(), key: F.key.value })) || {};
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
      const id = await window.GT_API.channels.customAdd({ ch: draft(), key: F.key.value });
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
  await window.GT_API.config.setPin({ enabled: on });
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

