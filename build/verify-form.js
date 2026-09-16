// End-to-end check for the window forms. For every form it asserts:
//   1. body class and the real window size change together (the original bug
//      left the window at panel size while the DOM had already switched)
//   2. the window reports resizable=false
// Then it reads the Win32 style bits: a resizable window carries WS_THICKFRAME
// (the drag border) and WS_MAXIMIZEBOX; fixed-size means both are absent.
// Note: min/max size setters do NOT clamp programmatic SetWindowPos on Windows
// (they only bound user-initiated tracking), so the style bits are the real
// evidence that a user cannot drag the window bigger.
//
// Usage: launch-debug.ps1 first, then  node build/verify-form.js [port]
const { execSync } = require('child_process');

const PORT = Number(process.argv[2] || process.env.CDP_PORT || 9222);
const EXPECT = { compact: [380, 46], pill: [200, 46], panel: [380, 560] };
const WS_THICKFRAME = 0x00040000;
const WS_MAXIMIZEBOX = 0x00010000;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const near = (a, b, tol = 2) => Math.abs(a - b) <= tol;

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

  // 通用调用:截图 / 模拟透明默认底色这类非 Runtime 域的命令要走这里
  const send = (method, params = {}) =>
    new Promise((res, rej) => {
      const i = ++id;
      pending.set(i, (m) => (m.error ? rej(new Error(`${method}: ${m.error.message}`)) : res(m.result)));
      ws.send(JSON.stringify({ id: i, method, params }));
    });

  return { ws, evalJs, send };
}

// 圆角外必须"真透明"。根元素 html 没有背景时,body 的 background 会被
// **背景传播**提升到根画布绘制,而画布恒为整窗矩形、不吃 border-radius ——
// 圆角描边(body 的 border,不参与传播)和直角底色就会同框。
// 透明窗口下正确的结果是:四角 alpha=0,胶囊本体仍然不透明。
// 抓图后把 PNG 塞回页面里用 canvas 读像素,省掉一个 PNG 解码依赖。
async function cornerAlpha(send, evalJs) {
  // 真实窗口是 transparent:true,截图也必须以透明默认底色合成,
  // 否则拿到的是 CDP 的白色底,测不出角上透不透。
  await send('Emulation.setDefaultBackgroundColorOverride', { color: { r: 0, g: 0, b: 0, a: 0 } });
  await sleep(400);
  const shot = await send('Page.captureScreenshot', { format: 'png' });
  const raw = await evalJs(`(async () => {
    const img = new Image();
    img.src = 'data:image/png;base64,${shot.data}';
    await img.decode();
    const c = document.createElement('canvas');
    c.width = img.width; c.height = img.height;
    const g = c.getContext('2d');
    g.drawImage(img, 0, 0);
    const at = (x, y) => [...g.getImageData(x, y, 1, 1).data];
    return JSON.stringify({
      w: img.width, h: img.height,
      tl: at(1, 1), tr: at(img.width - 2, 1),
      bl: at(1, img.height - 2), br: at(img.width - 2, img.height - 2),
      ct: at(img.width >> 1, img.height >> 1),
    });
  })()`, true);
  await send('Emulation.setDefaultBackgroundColorOverride', {});
  const p = JSON.parse(raw);
  const corners = [p.tl, p.tr, p.bl, p.br];
  return { ...p, corners, cornerAlpha: Math.max(...corners.map((c) => c[3])), centerAlpha: p.ct[3] };
}

(async () => {
  const { ws, evalJs, send } = await connect();
  const rows = [];
  let ok = true;

  for (const form of ['compact', 'pill', 'panel']) {
    // remember=false: exercise the real switch without touching the persisted config
    await evalJs(`applyForm(${JSON.stringify(form)}, false)`, true);
    await sleep(900);

    const state = JSON.parse(await evalJs(`(async () => {
      const rows = [...document.querySelectorAll('#list .row')];
      const visible = rows.filter((r) => r.style.display !== 'none').length;
      const badge = document.querySelector('#list .more');
      return JSON.stringify({
        bodyClass: document.body.className,
        w: window.innerWidth,
        h: window.innerHeight,
        tbar: getComputedStyle(document.querySelector('.tbar')).display,
        resizable: await appWindow.isResizable(),
        rows: rows.length,
        visible,
        badge: badge ? badge.textContent : null
      });
    })()`, true));

    const [ew, eh] = EXPECT[form];
    // 胶囊宽度随内容自适应(去掉图标后不再固定 200),这里只卡高度与合理区间
    const sizeOk =
      form === 'pill'
        ? near(state.h, eh) && state.w >= 130 && state.w <= 242
        : near(state.w, ew) && near(state.h, eh);
    const classOk = state.bodyClass.includes('form-' + form);
    // 行可见性:面板必须显示全部行(紧凑条藏起来的行要还原);
    // 紧凑条若有隐藏行,数目必须和 "+N" 徽标对得上
    let rowsOk = true;
    if (state.rows > 0) {
      const hidden = state.rows - state.visible;
      if (form === 'panel') rowsOk = hidden === 0;
      else rowsOk = hidden === 0 ? state.badge === null : state.badge === '+' + hidden;
    }
    // 胶囊形态额外验:圆角外真透明(见 cornerAlpha 的注释)
    let corner = null;
    let cornerOk = true;
    if (form === 'pill') {
      corner = await cornerAlpha(send, evalJs);
      cornerOk = corner.cornerAlpha === 0 && corner.centerAlpha > 200;
    }
    const pass = sizeOk && classOk && state.resizable === false && rowsOk && cornerOk;
    if (!pass) ok = false;
    rows.push({ form, ...state, ew, eh, sizeOk, classOk, rowsOk, corner, cornerOk, pass });
  }

  // Win32 style bits: fixed-size window must have no resize border / maximize box
  let style = null;
  let styleErr = null;
  try {
    const helper = require('path').join(__dirname, 'win-style.ps1');
    const out = execSync(`powershell -NoProfile -ExecutionPolicy Bypass -File "${helper}"`, { encoding: 'utf8', timeout: 20000 }).trim();
    if (!/^[0-9A-F]{8}$/i.test(out)) styleErr = `unexpected output: ${out}`;
    else style = parseInt(out, 16);
  } catch (e) {
    styleErr = String(e.message).split('\n')[0];
  }

  ws.close();
  console.log('=== form verification ===');
  for (const r of rows) {
    console.log(
      `  ${r.form.padEnd(8)} body=${r.bodyClass.padEnd(18)} tbar=${r.tbar.padEnd(6)} window=${r.w}x${r.h} expected=${r.form === 'pill' ? '132~242' : r.ew}x${r.eh}  ` +
      `switch=${r.sizeOk && r.classOk ? 'PASS' : 'FAIL'}  resizable=${r.resizable ? 'YES(BAD)' : 'no'}  ` +
      `rows=${r.visible}/${r.rows}${r.badge ? ' (' + r.badge + ')' : ''} ${r.rowsOk ? 'PASS' : 'FAIL(rows)'}`
    );
    if (r.corner) {
      console.log(
        `           corners alpha=${r.corner.cornerAlpha} center alpha=${r.corner.centerAlpha} ` +
        `(${r.corner.w}x${r.corner.h})  ${r.cornerOk ? 'PASS 角上真透明' : 'FAIL 圆角被直角底色盖住'}`
      );
    }
  }

  if (styleErr) {
    ok = false;
    console.log(`  win32 style          = UNKNOWN (${styleErr})`);
  } else {
    const thick = (style & WS_THICKFRAME) !== 0;
    const maxbox = (style & WS_MAXIMIZEBOX) !== 0;
    const fixed = !thick && !maxbox;
    if (!fixed) ok = false;
    console.log(
      `  win32 style          0x${style.toString(16).padStart(8, '0')}  ` +
      `WS_THICKFRAME(drag border)=${thick ? 'YES(BAD)' : 'no'}  WS_MAXIMIZEBOX=${maxbox ? 'YES(BAD)' : 'no'}  ` +
      `fixed-size=${fixed ? 'PASS' : 'FAIL'}`
    );
  }

  process.exit(ok ? 0 : 1);
})().catch((e) => {
  console.error('verify failed:', e.message || e);
  process.exit(1);
});
