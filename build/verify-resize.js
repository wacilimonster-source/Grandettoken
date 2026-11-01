// 面板/紧凑条缩放与自适应回归(CDP 连真机)。
//
// 用法: launch-debug.ps1 后 node build/verify-resize.js [port]
// 覆盖:
//   · 四档尺寸的 data-w / data-h 与关键区域不越界
//   · 面板 lg 两列、xs 两行摘要;紧凑条 E/W 可缩放、胶囊/贴边锁死
//   · 尺寸写回配置;面板尺寸跨形态恢复;恢复默认
//   · 右下角手柄可拖;字号变大时只长到新最小尺寸
const http = require('http');

const PORT = Number(process.argv[2] || process.env.CDP_PORT || 9222);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const get = (p) => new Promise((res, rej) => {
  http.get({ host: '127.0.0.1', port: PORT, path: p }, (r) => {
    let d = ''; r.on('data', (c) => d += c); r.on('end', () => res(JSON.parse(d)));
  }).on('error', rej);
});

const fails = [];
const check = (name, ok, detail) => {
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  · ' + detail : ''}`);
  if (!ok) fails.push(name);
};

(async () => {
  const targets = await get('/json');
  const t = targets.filter((x) => x.type === 'page')
    .find((x) => /tauri|grandettoken|tokenscope/i.test(`${x.title || ''} ${x.url || ''}`));
  if (!t) throw new Error('no app page on port ' + PORT);
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.addEventListener('open', res); ws.addEventListener('error', rej); });
  let id = 0; const pending = new Map();
  ws.addEventListener('message', (ev) => {
    const m = JSON.parse(ev.data); const cb = pending.get(m.id);
    if (cb) { pending.delete(m.id); cb(m); }
  });
  const send = (method, params = {}) => new Promise((res, rej) => {
    const i = ++id;
    pending.set(i, (m) => m.error ? rej(new Error(JSON.stringify(m.error))) : res(m.result));
    ws.send(JSON.stringify({ id: i, method, params }));
  });
  const ev = async (expr, awaitPromise = false) => {
    const r = await send('Runtime.evaluate', { expression: expr, awaitPromise, returnByValue: true });
    if (r.exceptionDetails) throw new Error(String(r.exceptionDetails.text));
    return r.result.value;
  };

  const measure = () => ev(`(() => {
    const rect = (s) => { const e = document.querySelector(s); if (!e) return null;
      const r = e.getBoundingClientRect(); return { l: r.left, r: r.right, t: r.top, b: r.bottom, w: r.width, h: r.height }; };
    const rows = [...document.querySelectorAll('#list .row')];
    return JSON.stringify({
      w: innerWidth, h: innerHeight, dw: document.body.dataset.w, dh: document.body.dataset.h,
      form: (document.body.className.match(/form-(\\w+)/) || [])[1],
      tbar: rect('.tbar'), sum: rect('#sum'), foot: rect('.foot'), list: rect('#list'),
      listDisplay: getComputedStyle(document.getElementById('list')).display,
      sumAreas: getComputedStyle(document.getElementById('sum')).gridTemplateAreas,
      grip: getComputedStyle(document.getElementById('rzGrip')).display,
      rows: rows.length,
      rowOverflow: rows.filter((r) => { const x = r.getBoundingClientRect(); return x.right > innerWidth + 1; }).length,
      horizontal: document.getElementById('list').scrollWidth > document.getElementById('list').clientWidth + 2,
    });
  })()`);

  const cfg = () => ev(`(async () => {
    const c = await window.__TAURI__.core.invoke('get_config');
    return JSON.stringify({ winW: c.winW, winH: c.winH, compactMaxW: c.compactMaxW, fontScale: c.fontScale });
  })()`, true);

  const setSize = (w, h) => ev(`(async () => {
    const win = window.__TAURI__.window.getCurrentWindow();
    await win.setSize(new window.__TAURI__.dpi.LogicalSize(${w}, ${h}));
  })()`, true);

  const state = async () => JSON.parse(await measure());
  const setForm = (form) => ev(`(async () => { await applyForm(${JSON.stringify(form)}, false); })()`, true);

  let pos = null;
  try {
    pos = await ev(`(async () => {
      const p = await window.__TAURI__.window.getCurrentWindow().outerPosition();
      return JSON.stringify({ x: p.x, y: p.y });
    })()`, true).then(JSON.parse);
    await ev(`(async () => {
      const win = window.__TAURI__.window.getCurrentWindow();
      await win.setPosition(new window.__TAURI__.dpi.PhysicalPosition(0, 0));
    })()`, true);
    await ev(`(async () => { resetPanelSize(); await new Promise(r => setTimeout(r, 700)); })()`, true);

    // 1. 四档尺寸:类、布局、无横向越界
    const sizes = [
      [360, 480, 'xs'], [400, 580, 'sm'], [560, 700, 'md'], [760, 940, 'lg'],
    ];
    for (const [w, h, want] of sizes) {
      await setSize(w, h);
      await sleep(700);
      const s = await state();
      const c = JSON.parse(await cfg());
      check(`尺寸 ${w}×${h} 有效宽度类=${want}`, s.dw === want, `${s.dw} (sum=${Math.round(s.sum.h)})`);
      check(`尺寸 ${w}×${h} 无横向越界`, !s.horizontal && s.rowOverflow === 0, `h=${s.horizontal} rows=${s.rowOverflow}`);
      check(`尺寸 ${w}×${h} 关键区域在窗内`, s.tbar && s.sum && s.foot && s.list
        && s.tbar.r <= w + 1 && s.sum.r <= w + 1 && s.foot.r <= w + 1 && s.list.r <= w + 1,
        `${s.tbar && Math.round(s.tbar.r)}/${s.sum && Math.round(s.sum.r)}/${s.list && Math.round(s.list.r)}`);
      check(`尺寸 ${w}×${h} 写回配置`, c.winW === w - 20 && c.winH === h - 20, JSON.stringify(c));
      if (want === 'lg') check('lg 列表为两列网格', s.listDisplay === 'grid', s.listDisplay);
      if (want === 'xs') check('xs 摘要为两行(risk 跨列)', /risk risk/.test(s.sumAreas.replace(/\n/g, ' ')), s.sumAreas);
    }

    // 2. 面板尺寸跨形态恢复
    await setSize(560, 700); await sleep(700);
    await setForm('compact'); await sleep(700);
    const compact = await state();
    check('紧凑条可缩放', compact.form === 'compact' && compact.w >= 150 && compact.w <= 620, `${compact.w}×${compact.h}`);
    check('紧凑条无可见手柄', compact.grip === 'none', compact.grip);
    await setForm('panel'); await sleep(700);
    const back = await state();
    check('回到面板恢复用户尺寸', back.form === 'panel' && back.w === 560 && back.h === 700, `${back.w}×${back.h}`);

    // 3. 胶囊锁死
    await setForm('pill'); await sleep(700);
    const pill = await state();
    const pillResizable = await ev(`window.__TAURI__.window.getCurrentWindow().isResizable()`, true);
    check('胶囊不可缩放且无手柄', pill.form === 'pill' && pillResizable === false && pill.grip === 'none',
      `${pill.w}×${pill.h} resizable=${pillResizable} grip=${pill.grip}`);
    await setForm('panel'); await sleep(700);

    // 4. 右下角手柄
    await ev(`(async () => { resetPanelSize(); await new Promise(r => setTimeout(r, 700)); })()`, true);
    const before = await state();
    const gr = JSON.parse(await ev(`(() => { const r = document.getElementById('rzGrip').getBoundingClientRect();
      return JSON.stringify({ x: r.x + r.width / 2, y: r.y + r.height / 2 }); })()`));
    const gx = Math.round(gr.x), gy = Math.round(gr.y);
    await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: gx, y: gy, button: 'left', buttons: 1, clickCount: 1 });
    await sleep(100);
    for (let i = 1; i <= 6; i++) {
      await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: gx + i * 10, y: gy + i * 8, button: 'none', buttons: 1 });
      await sleep(60);
    }
    await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: gx + 60, y: gy + 48, button: 'left', buttons: 0, clickCount: 1 });
    await sleep(900);
    const after = await state();
    check('右下角手柄能拉大', after.w > before.w && after.h > before.h, `${before.w}×${before.h} → ${after.w}×${after.h}`);
    const resetVisible = await ev(`getComputedStyle(document.getElementById('rzReset')).display`, true);
    check('自定义尺寸显示“默认大小”按钮', resetVisible !== 'none', resetVisible);
    await ev(`document.getElementById('rzReset').click()`);
    await sleep(700);
    const reset = await state();
    check('点“默认大小”按钮回到 400×580', reset.w === 400 && reset.h === 580, `${reset.w}×${reset.h}`);
    const resetHidden = await ev(`getComputedStyle(document.getElementById('rzReset')).display`, true);
    check('默认尺寸隐藏“默认大小”按钮', resetHidden === 'none', resetHidden);

    // 5. 字号变大:只长到新的最小尺寸,不回到默认
    await ev(`(() => { const s = document.getElementById('cfgFontScale'); s.value = 'xl'; s.dispatchEvent(new Event('change', { bubbles: true })); })()`);
    await sleep(900);
    const xl = await state();
    check('xl 字号窗口长到最小可用尺寸', xl.w >= Math.round(340 * 1.3) + 20 && xl.h >= Math.round(460 * 1.3) + 20,
      `${xl.w}×${xl.h}`);
    check('xl 字号布局无越界', !xl.horizontal && xl.rowOverflow === 0, `h=${xl.horizontal} rows=${xl.rowOverflow}`);
    await ev(`(() => { const s = document.getElementById('cfgFontScale'); s.value = 'md'; s.dispatchEvent(new Event('change', { bubbles: true })); })()`);
    await sleep(700);
    await ev(`(async () => { resetPanelSize(); await new Promise(r => setTimeout(r, 700)); })()`, true);

    // 6. 恢复默认(尺寸 + 紧凑 max)
    const end = await state();
    const endCfg = JSON.parse(await cfg());
    check('恢复默认 400×580', end.w === 400 && end.h === 580, `${end.w}×${end.h}`);
    check('恢复默认写回 380×560', endCfg.winW === 380 && endCfg.winH === 560, JSON.stringify(endCfg));
    await ev(`(async () => { await window.__TAURI__.core.invoke('save_window_size', { w: null, h: null, compactMaxW: 380 }); })()`, true);
  } finally {
    if (pos) {
      await ev(`(async () => {
        const win = window.__TAURI__.window.getCurrentWindow();
        await win.setPosition(new window.__TAURI__.dpi.PhysicalPosition(${pos.x}, ${pos.y}));
      })()`, true).catch(() => {});
    }
    try { ws.close(); } catch {}
  }

  console.log(fails.length ? `\n${fails.length} FAILED` : '\nALL PASS');
  process.exit(fails.length ? 1 : 0);
})().catch((e) => { console.error('verify failed:', e.message || e); process.exit(1); });
