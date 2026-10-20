// 真实的"鼠标点进去 / 鼠标点返回"路径下,焦点环是否会出现(:focus-visible)。
// 键盘路径(Tab + Enter)必须出环,鼠标路径不该出环 —— 两条都验。
const PORT = Number(process.env.CDP_PORT || 9222);
const sleep = ms => new Promise(r => setTimeout(r, ms));
(async () => {
  const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  const t = targets.filter(x => x.type === 'page').find(x => /tauri|grandettoken/i.test(`${x.title} ${x.url}`));
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.addEventListener('open', res); ws.addEventListener('error', rej); });
  let id = 0; const pending = new Map();
  ws.addEventListener('message', e => { const m = JSON.parse(e.data); const cb = pending.get(m.id); if (cb) { pending.delete(m.id); cb(m); } });
  const send = (method, params = {}) => new Promise((res, rej) => { const i = ++id; pending.set(i, m => m.error ? rej(new Error(m.error.message)) : res(m.result)); ws.send(JSON.stringify({ id: i, method, params })); });
  const ev = (expr, ap = false) => send('Runtime.evaluate', { expression: expr, awaitPromise: ap, returnByValue: true }).then(r => r.result.value);
  const center = async (sel) => JSON.parse(await ev(`JSON.stringify((() => { const r = document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect(); return { x: r.x + r.width/2, y: r.y + r.height/2 }; })())`));
  const mouseClick = async (sel) => {
    const p = await center(sel);
    await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: p.x, y: p.y, button: 'none', clickCount: 0 });
    await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: p.x, y: p.y, button: 'left', clickCount: 1 });
    await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: p.x, y: p.y, button: 'left', clickCount: 1 });
    await sleep(300);
  };

  // 鼠标路径
  await ev(`closeManage(); document.activeElement.blur && document.activeElement.blur();`);
  await sleep(150);
  await mouseClick('#lkKeys');
  const inMouse = JSON.parse(await ev(`JSON.stringify({ manage: document.body.classList.contains('view-manage'), active: document.activeElement.id, ring: document.activeElement.matches(':focus-visible') })`));
  await mouseClick('#btnBack');
  const outMouse = JSON.parse(await ev(`JSON.stringify({ manage: document.body.classList.contains('view-manage'), active: document.activeElement.id, ring: document.activeElement.matches(':focus-visible') })`));

  // 键盘路径:Tab 到 lkKeys(先聚焦它前一个可聚焦点再 Tab),再按 Enter
  await ev(`document.getElementById('lkQuit').focus()`);
  await sleep(60);
  await send('Input.dispatchKeyEvent', { type: 'rawKeyDown', windowsVirtualKeyCode: 9, key: 'Tab', code: 'Tab' }); // Shift+Tab? no: plain Tab
  await send('Input.dispatchKeyEvent', { type: 'keyUp', windowsVirtualKeyCode: 9, key: 'Tab', code: 'Tab' });
  await sleep(200);
  const tabbed = JSON.parse(await ev(`JSON.stringify({ active: document.activeElement.id, ring: document.activeElement.matches(':focus-visible') })`));
  await send('Input.dispatchKeyEvent', { type: 'rawKeyDown', windowsVirtualKeyCode: 13, key: 'Enter', code: 'Enter' });
  await send('Input.dispatchKeyEvent', { type: 'keyUp', windowsVirtualKeyCode: 13, key: 'Enter', code: 'Enter' });
  await sleep(300);
  const inKey = JSON.parse(await ev(`JSON.stringify({ manage: document.body.classList.contains('view-manage'), active: document.activeElement.id, ring: document.activeElement.matches(':focus-visible') })`));
  await ev(`closeManage()`);
  await sleep(200);

  console.log(JSON.stringify({ inMouse, outMouse, tabbed, inKey }, null, 0));
  ws.close(); process.exit(0);
})().catch(e => { console.error(e.message); process.exit(1); });
