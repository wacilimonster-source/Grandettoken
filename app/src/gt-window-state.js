async function publishWindowPresentation(force = false) {
  if (!force && !CFG?.hibernateWebviewOnHide) return true;
  const dock = DOCK.on ? { side: DOCK.side, y: Math.round(DOCK.y), open: DOCK.open } : null;
  try {
    await window.GT_API.desktop.windowCmd({ action: "presentation", presentation: { form: currentForm(), dock } });
    return true;
  } catch (error) {
    showToast("记录窗口形态失败：" + window.GT_API.errorText(error), true);
    return false;
  }
}

function restoreWindowPresentation({ form, dock }) {
  markSelfMove(FORM_SELF_MOVE_MS);
  document.body.classList.remove("form-panel", "form-compact", "form-pill");
  document.body.classList.add("form-" + form);
  $("list").setAttribute("data-tauri-drag-region", form === "compact" ? "deep" : "false");
  if (dock) {
    Object.assign(DOCK, dock, { on: true });
    document.body.classList.toggle("docked", !dock.open);
    document.body.classList.toggle("dock-left", dock.side === "left");
    document.body.classList.toggle("dock-open", dock.open);
  }
}

function bindHibernateSwitch() {
  const toggle = $("cfgHibernateWebview");
  toggle.addEventListener("click", async () => {
    const previous = !!CFG.hibernateWebviewOnHide;
    toggle.disabled = true;
    try {
      if (!previous && !await publishWindowPresentation(true)) return;
      CFG.hibernateWebviewOnHide = !previous;
      setSwitch(toggle.id, !previous);
      if (!await saveCfg("托盘省内存模式")) {
        CFG.hibernateWebviewOnHide = previous;
        setSwitch(toggle.id, previous);
      }
    } finally {
      toggle.disabled = false;
    }
  });
}

async function loadResumedChannels(receivedPush) {
  let cached;
  try {
    cached = await window.GT_API.channels.getCachedChannels();
  } catch (error) {
    showToast("读取上次渠道数据失败：" + window.GT_API.errorText(error), true);
  }
  if (receivedPush() || cached) {
    if (!receivedPush()) applyChannelsPush(cached);
    await loadCheckin();
    render({ updated: false });
    return;
  }
  await refresh();
}
