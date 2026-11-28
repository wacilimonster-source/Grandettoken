(() => {
  "use strict";
  const box = document.getElementById("commanderBox");
  const status = document.getElementById("commanderState");
  const toggle = document.getElementById("commanderToggle");
  const verify = document.getElementById("commanderVerify");
  const api = window.GT_API.commander;
  let active = false;
  let timer;
  let generation = 0;
  let visible = false;
  let busy = false;
  async function refresh() {
    const id = ++generation;
    clearTimeout(timer);
    try {
      const info = await api.status();
      if (id !== generation) return;
      active = info.active;
      status.textContent = (info.status || "未启动") +
        (info.status === "正在准备代理依赖" ? " · " + info.elapsed + " 秒" : "");
      toggle.textContent = active ? "停止" : "启动";
      toggle.disabled = busy;
      document.getElementById("commanderLog").textContent = info.logs.join("\n") || "暂无运行记录";
      document.getElementById("commanderPair").hidden = !info.code && !info.verify;
      document.getElementById("commanderCode").textContent = info.code ? "配对码 " + info.code : "";
      verify.hidden = !info.verify;
    } catch (e) {
      if (id === generation) { status.textContent = String(e); toggle.disabled = true; }
    } finally {
      if (id === generation && visible && !busy) timer = setTimeout(refresh, 1500);
    }
  }
  toggle.onclick = async () => {
    busy = true;
    toggle.disabled = true;
    ++generation;
    clearTimeout(timer);
    try {
      if (active) await api.stop(); else await api.start();
    } catch (e) {
      window.showToast(String(e));
    } finally {
      busy = false;
      await refresh();
    }
  };
  verify.onclick = () => api.verify().catch(e => window.showToast(String(e)));
  new IntersectionObserver(([entry]) => {
    visible = entry.isIntersecting;
    if (visible && !busy) refresh(); else clearTimeout(timer);
  }).observe(box);
})();
