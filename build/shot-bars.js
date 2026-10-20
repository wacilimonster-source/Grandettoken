const fs = require('fs');
const PORT = Number(process.env.CDP_PORT || 9222);
(async () => {
  const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  const t = targets.filter(x => x.type === 'page').find(x => /tauri|grandettoken/i.test(`${x.title} ${x.url}`));
  if (!t) throw new Error('no page');
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.addEventListener('open', res); ws.addEventListener('error', rej); });
  let id = 0; const pending = new Map();
  ws.addEventListener('message', e => { const m = JSON.parse(e.data); const cb = pending.get(m.id); if (cb) { pending.delete(m.id); cb(m); } });
  const send = (method, params = {}) => new Promise((res, rej) => { const i = ++id; pending.set(i, m => m.error ? rej(new Error(m.error.message)) : res(m.result)); ws.send(JSON.stringify({ id: i, method, params })); });
  const ev = (expr, awaitPromise = false) => send('Runtime.evaluate', { expression: expr, awaitPromise, returnByValue: true }).then(r => r.result.value);
  // 只截标题栏与底栏两条:不含任何余额/渠道数据
  const rects = JSON.parse(await ev(`JSON.stringify((() => {
    const g = s => { const e = document.querySelector(s); const r = e.getBoundingClientRect(); return {x:r.x,y:r.y,width:r.width,height:r.height}; };
    return { tbar: g('.tbar'), foot: g('.foot'), dpr: devicePixelRatio };
  })())`));
  await send('Emulation.setDefaultBackgroundColorOverride', { color: { r: 0, g: 0, b: 0, a: 0 } });
  for (const [name, r] of Object.entries({ tbar: rects.tbar, foot: rects.foot })) {
    const shot = await send('Page.captureScreenshot', { format: 'png',
      clip: { x: r.x, y: r.y, width: r.width, height: r.height, scale: 2 } });
    fs.writeFileSync(`C:/Users/wacil/AppData/Local/Temp/gt-measure/live-${name}.png`, Buffer.from(shot.data, 'base64'));
    console.log(name, JSON.stringify(r));
  }
  await send('Emulation.setDefaultBackgroundColorOverride', {});
  ws.close(); process.exit(0);
})().catch(e => { console.error(e.message); process.exit(1); });
