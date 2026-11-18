module.exports = async ({ w, R }) => {
  const cfg = w.__TEST.cfg();
  const { fontScale, panelLayout, dockEnabled, winX, winY } = cfg;
  const flags = { fontScale, panelLayout, dockEnabled, winX, winY };
  const monitor = {
    position: { x: 0, y: 0 }, size: { width: 1920, height: 1080 }, scaleFactor: 1.25,
    workArea: { position: { x: 0, y: 0 }, size: { width: 1920, height: 1020 } },
  };
  const geometry = { monitor, position: { x: 850, y: 350 }, resizable: true };
  const heightFits = () => {
    const { position, size } = geometry;
    const area = monitor.workArea;
    return size.height >= area.size.height - 1 &&
      position.y >= area.position.y && position.y + size.height <= area.position.y + area.size.height;
  };
  const record = (name, ok) => R.push({ name, ok, extra: JSON.stringify(geometry) });
  w.__GEOMETRY = geometry;
  try {
    Object.assign(cfg, { fontScale: "lg", dockEnabled: false });
    let allLayoutsFit = true;
    for (const layout of ["single", "double", "thumbnail"]) {
      cfg.panelLayout = layout;
      geometry.position = { x: 850, y: 350 };
      await w.applyForm("panel", false);
      allLayoutsFit = allLayoutsFit && heightFits() &&
        geometry.position.x >= 0 && geometry.position.x + geometry.size.width <= monitor.size.width;
    }
    record("125% 大字号普通面板完整落在工作区", allLayoutsFit && geometry.resizable === false);

    await w.dockEnter("right", { x: 1910, y: 900 });
    await w.dockLayout(true);
    const openFits = heightFits() && w.document.body.classList.contains("dock-open");
    await w.dockExit(false);
    record("125% 贴边展开与解除贴边使用相同的工作区高度", openFits && heightFits());

    // 另一台同 DPI 的显示器可有负坐标及不同位置的任务栏。
    monitor.position = { x: -1280, y: 0 };
    monitor.size = { width: 1280, height: 800 };
    monitor.workArea = { position: { x: -1230, y: 50 }, size: { width: 1230, height: 750 } };
    geometry.position = { x: -1275, y: 75 };
    w.markSelfMove(0);
    await new Promise(resolve => setTimeout(resolve, 10));
    w.scheduleSettleCheck();
    const deadline = Date.now() + 1000;
    while ((cfg.winX !== -1230 || cfg.winY !== 50) && Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    record("同 DPI 跨屏停稳后按新工作区缩放并记住可见落点",
      heightFits() && geometry.position.x === -1230 && cfg.winX === -1230 && cfg.winY === 50);
  } finally {
    delete w.__GEOMETRY;
    Object.assign(cfg, flags);
  }
};
