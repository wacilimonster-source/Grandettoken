// 通过 WebView2 的 CDP 远程调试端口直接查询运行中应用的 DOM。
// 比改代码重编译快得多,而且能拿到真实的 computed style。
const PORT = process.env.CDP_PORT || 9222;

const EXPR = `JSON.stringify({
  bodyClass: document.body.className,
  innerW: window.innerWidth,
  innerH: window.innerHeight,
  dpr: window.devicePixelRatio,
  sheets: Array.from(document.styleSheets).map(s => {
    try { return (s.href || 'inline') + ':' + s.cssRules.length + 'rules'; }
    catch (e) { return (s.href || 'inline') + ':BLOCKED'; }
  }),
  tbar: (() => { const e = document.querySelector('.tbar'); return e ? getComputedStyle(e).display : 'MISSING'; })(),
  listDisplay: (() => { const e = document.querySelector('.list'); return e ? getComputedStyle(e).display : 'MISSING'; })(),
  listDir: (() => { const e = document.querySelector('.list'); return e ? getComputedStyle(e).flexDirection : 'MISSING'; })(),
  sumDisplay: (() => { const e = document.querySelector('.sum'); return e ? getComputedStyle(e).display : 'MISSING'; })(),
  footDisplay: (() => { const e = document.querySelector('.foot'); return e ? getComputedStyle(e).display : 'MISSING'; })(),
  pillDisplay: (() => { const e = document.querySelector('.pill-body'); return e ? getComputedStyle(e).display : 'MISSING'; })(),
  emptyText: (() => { const e = document.querySelector('.empty'); return e ? e.textContent.trim().slice(0, 40) : null; })(),
  rowCount: document.querySelectorAll('.row').length,
  bodyRect: (() => { const r = document.body.getBoundingClientRect(); return Math.round(r.width) + 'x' + Math.round(r.height); })(),
  listRect: (() => { const e = document.querySelector('.list'); if (!e) return null; const r = e.getBoundingClientRect(); return Math.round(r.width) + 'x' + Math.round(r.height); })(),
  emptyRect: (() => { const e = document.querySelector('.empty'); if (!e) return null; const r = e.getBoundingClientRect(); return Math.round(r.width) + 'x' + Math.round(r.height); })(),
  caretDisplay: (() => { const e = document.querySelector('.caret'); return e ? getComputedStyle(e).display : 'MISSING'; })(),
  rootBg: getComputedStyle(document.documentElement).getPropertyValue('--bg').trim() || 'UNSET',
  bodyBg: getComputedStyle(document.body).backgroundColor,
  bodyFlexDir: getComputedStyle(document.body).flexDirection
})`;

(async () => {
  let targets;
  try {
    const r = await fetch(`http://127.0.0.1:${PORT}/json`);
    targets = await r.json();
  } catch (e) {
    console.log('CDP unreachable on port', PORT, '->', e.message);
    process.exit(1);
  }

  // Only real pages, and prefer the app's own page: with several debug targets
  // attached, pages[0] can be an iframe/SW and the DOM reads are all garbage.
  const pages = targets.filter((t) => t.type === 'page');
  console.log('targets:', targets.map((t) => `${t.type}:${t.title || ''}`).join(' | '));

  const target =
    pages.find((t) => /tauri|grandettoken|tokenscope/i.test(`${t.title || ''} ${t.url || ''}`)) ||
    pages[0];
  if (!target) { console.log('no debuggable target'); process.exit(1); }

  const ws = new WebSocket(target.webSocketDebuggerUrl);
  const send = (id, method, params) => ws.send(JSON.stringify({ id, method, params }));

  ws.addEventListener('open', () => send(1, 'Runtime.evaluate', { expression: EXPR, returnByValue: true }));

  ws.addEventListener('message', (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id !== 1) return;
    if (msg.result && msg.result.result) {
      const val = msg.result.result.value;
      try {
        const o = JSON.parse(val);
        console.log('\n=== live DOM state ===');
        for (const [k, v] of Object.entries(o)) console.log(`  ${k.padEnd(14)} ${JSON.stringify(v)}`);
      } catch {
        console.log('raw:', val, JSON.stringify(msg.result));
      }
    } else {
      console.log('CDP error:', JSON.stringify(msg));
    }
    ws.close();
  });

  ws.addEventListener('error', (e) => { console.log('ws error', e.message || e); process.exit(1); });
  setTimeout(() => { console.log('timeout'); process.exit(1); }, 15000);
})();
