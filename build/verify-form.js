// End-to-end check for the window form switch: the body class AND the real
// window size must change together. The original bug left the window at panel
// size (380x560) while the DOM switched to compact, because setSize threw on a
// wrong Tauri v1 namespace and the catch swallowed it.
//
// Usage: launch-debug.ps1 first, then  node build/verify-form.js [port]
const PORT = Number(process.argv[2] || process.env.CDP_PORT || 9222);
const EXPECT = { compact: [380, 46], pill: [200, 46], panel: [380, 560] };

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function connect() {
  const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  const t = targets.find((x) => x.type === 'page') || targets[0];
  if (!t) throw new Error('no debuggable target');
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', () => rej(new Error('websocket error')));
  });

  let id = 0;
  const pending = new Map();
  ws.addEventListener('message', (ev) => {
    const m = JSON.parse(ev.data);
    const cb = pending.get(m.id);
    if (cb) { pending.delete(m.id); cb(m); }
  });

  const evalJs = (expression, awaitPromise = false) =>
    new Promise((res, rej) => {
      const i = ++id;
      pending.set(i, (m) => {
        const ex = m.result && m.result.exceptionDetails;
        if (ex) {
          const desc = (ex.exception && ex.exception.description) || ex.text || 'unknown';
          rej(new Error(String(desc).split('\n')[0]));
        } else {
          res(m.result.result.value);
        }
      });
      ws.send(JSON.stringify({ id: i, method: 'Runtime.evaluate', params: { expression, awaitPromise, returnByValue: true } }));
    });

  return { ws, evalJs };
}

(async () => {
  const { ws, evalJs } = await connect();
  const rows = [];
  let ok = true;

  for (const form of ['compact', 'pill', 'panel']) {
    // remember=false: exercise the real switch without touching the persisted config
    await evalJs(`applyForm(${JSON.stringify(form)}, false)`, true);
    await sleep(900);
    const raw = await evalJs(`JSON.stringify({
      bodyClass: document.body.className,
      w: window.innerWidth,
      h: window.innerHeight,
      tbar: getComputedStyle(document.querySelector('.tbar')).display
    })`);
    const s = JSON.parse(raw);
    const [ew, eh] = EXPECT[form];
    const pass =
      s.bodyClass === 'form-' + form &&
      Math.abs(s.w - ew) <= 2 &&
      Math.abs(s.h - eh) <= 2;
    if (!pass) ok = false;
    rows.push({ form, ...s, ew, eh, pass });
  }

  ws.close();
  console.log('=== form switch verification ===');
  for (const r of rows) {
    console.log(`  ${r.form.padEnd(8)} body=${r.bodyClass.padEnd(12)} tbar=${r.tbar.padEnd(6)} window=${r.w}x${r.h} expected=${r.ew}x${r.eh}  ${r.pass ? 'PASS' : 'FAIL'}`);
  }
  process.exit(ok ? 0 : 1);
})().catch((e) => {
  console.error('verify failed:', e.message || e);
  process.exit(1);
});
