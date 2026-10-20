// 给 README 截界面图。三步一条链:注入仿真数据 → 截图 → 还原现场。
//
// 用法:
//   powershell -File build/launch-debug.ps1        # 起应用并开 CDP
//   node build/shot-panel.js [输出目录]
//
// 默认输出到 docs/assets/。产物:panel.png / compact.png / pill.png。
//
// **为什么必须用仿真数据**:截图既是公开门面,也是隐私边界。真实渠道里有余额、
// 有渠道名、有「今日花了多少」,直接截等于把账务数据连同仓库一起公开。仿真数据的
// 形状按 verify-ui-0923.js 的 CH 口径来(那个夹具已经和真实 ChannelView 对齐过),
// 数字是编的,但渲染路径是真的 —— 状态点、进度条、百分比、汇总条环比全都走真代码。
//
// **还原现场是硬要求**:verify-ui-0923.js 踩过 —— 仿真数据会原地改 CFG 并
// 通过 invoke('set_config') 落盘,异常时若没还原,用户下次启动看到的就是假数据。
// 这里把还原放 finally,且验证还原后的 CFG 与初始快照逐字段相同。
const fs = require('fs');
const path = require('path');

const PORT = Number(process.argv[3] || process.env.CDP_PORT || 9222);
const OUT = process.argv[2] || path.join(__dirname, '..', 'docs', 'assets');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// 与 verify-ui-0923.js 同一套夹具(它本来就照着真实 ChannelView 取的形状),
// 只把数值换成"看起来体面"的演示值。window 字段与 expiring 都留着 ——
// OpenCode Go 的限流窗口、Trae 的逐笔到期都是界面上真实存在的区块。
const CH = [
  { id: '4sapi', name: '4SAPI', short: '4S', color: '#e8622c', kind: 'amount', hasKey: true, valid: true,
    remaining: 74.2, used: 125.8, total: 399.83, unit: 'CNY', windows: [], expiring: [], extra: [],
    limited: false, authSource: 'keyring', authLabel: '', day: 12.4, week: 86.1, month: 301.55,
    dayPrev: 10.0, weekPrev: 92.0, monthPrev: 280.0, estimated: true, stale: false, updatedAt: Math.floor(Date.now() / 1000),
    claim: { amount: 200, lastClaimAt: 1758000000, source: 'auto', daysUntilEligible: 6, eligible: false, dailyBurn: 12.3, daysOfBalance: 6, shortageRisk: false },
    unstable: false, hidden: false },
  { id: 'opencode-go', name: 'OpenCode Go', short: 'OC', color: '#f5a623', kind: 'percent', hasKey: true, valid: true,
    remaining: 82, used: 18, total: null, unit: 'percent', expiring: [], extra: [],
    windows: [ { label: '5 小时', remainPercent: 91.5, status: 'ok', resetsAt: null, main: false },
               { label: '本周', remainPercent: 88.0, status: 'ok', resetsAt: null, main: false },
               { label: '周期', remainPercent: 82.0, status: 'ok', resetsAt: null, main: true } ],
    limited: false, authSource: 'keyring', authLabel: '', day: null, week: null, month: null,
    dayPrev: null, weekPrev: null, monthPrev: null, estimated: false, stale: false, updatedAt: Math.floor(Date.now() / 1000),
    claim: null, unstable: true, hidden: false },
  { id: 'deepseek', name: 'DeepSeek', short: 'DS', color: '#4d6bfe', kind: 'amount', hasKey: true, valid: false,
    remaining: 256.0, used: null, total: null, unit: 'CNY', windows: [], expiring: [], extra: [],
    error: 'HTTP 401', limited: false, authSource: 'keyring', authLabel: '', day: 3.1, week: 20.0, month: 88.0,
    dayPrev: null, weekPrev: null, monthPrev: null, estimated: true, stale: true, updatedAt: Math.floor(Date.now() / 1000) - 720,
    claim: null, unstable: false, hidden: false },
  { id: 'hapi', name: 'Hapi', short: 'HA', color: '#19b8c4', kind: 'amount', hasKey: false, valid: false,
    remaining: null, used: null, total: null, unit: 'CNY', windows: [], expiring: [], extra: [],
    limited: false, authSource: 'keyring', authLabel: '', day: null, week: null, month: null,
    dayPrev: null, weekPrev: null, monthPrev: null, estimated: true, stale: false, updatedAt: 0,
    claim: null, unstable: false, hidden: false },
];

(async () => {
  const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  const pages = targets.filter((x) => x.type === 'page');
  // 和 verify-form.js 同一条规矩:只挂应用自己的页面。9222 被别的 Chromium /
  // 遗留 Edge WebView 占用时,老写法会静默连到无关页面,把整套流程跑在它上面。
  const t = pages.find((x) => /tauri|grandettoken|tokenscope/i.test(`${x.title || ''} ${x.url || ''}`));
  if (!t) {
    console.error(`no app page on port ${PORT} - refusing to attach to an unrelated target`);
    console.error('candidates:');
    for (const x of pages) console.error(`  ${x.type}  ${x.title || '(no title)'}  ${x.url || '(no url)'}`);
    process.exit(1);
  }

  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', () => rej(new Error('ws error')));
  });

  let id = 0;
  const pending = new Map();
  ws.addEventListener('message', (ev) => {
    const m = JSON.parse(ev.data);
    const cb = pending.get(m.id);
    if (cb) { pending.delete(m.id); cb(m); }
  });
  const evalJs = (expression, awaitPromise = false) => new Promise((res, rej) => {
    const i = ++id;
    pending.set(i, (m) => {
      const ex = m.result && m.result.exceptionDetails;
      if (ex) rej(new Error(String((ex.exception && ex.exception.description) || ex.text).split('\n')[0]));
      else res(m.result.result.value);
    });
    ws.send(JSON.stringify({ id: i, method: 'Runtime.evaluate', params: { expression, awaitPromise, returnByValue: true } }));
  });
  const send = (method, params = {}) => new Promise((res, rej) => {
    const i = ++id;
    pending.set(i, (m) => (m.error ? rej(new Error(`${method}: ${m.error.message}`)) : res(m.result)));
    ws.send(JSON.stringify({ id: i, method, params }));
  });

  fs.mkdirSync(OUT, { recursive: true });
  let ok = true;

  // ① 快照 CFG。仿真会改 hiddenChannels / pillChannels / sort,并且 saveCfg 会
  //    把这些值落到磁盘配置上 —— 不还原的话用户下次启动还在看演示数据。
  const cfgSnap = await evalJs(`JSON.stringify(CFG)`);
  if (!cfgSnap || cfgSnap === 'null') {
    console.error('could not snapshot CFG (app still starting?) - refusing to inject fake data');
    ws.close();
    process.exit(1);
  }

  // 真实窗口 transparent:true。不加透明底色,截图拿到的是 CDP 的白色底 ——
  // README 里会出现一圈难看的直角白边(verify-form.js 的 cornerAlpha 也是同一条理由)。
  await send('Emulation.setDefaultBackgroundColorOverride', { color: { r: 0, g: 0, b: 0, a: 0 } });

  try {
    await evalJs(`CHANNELS = ${JSON.stringify(CH)}; CFG.hiddenChannels = []; CFG.sort = 'percent'; render();`);
    await sleep(400);

    const shot = async (form, file, expect) => {
      await evalJs(`applyForm(${JSON.stringify(form)}, false)`, true);
      await sleep(1000);
      const st = JSON.parse(await evalJs(`JSON.stringify({ w: window.innerWidth, h: window.innerHeight })`));
      const r = await send('Page.captureScreenshot', { format: 'png' });
      const out = path.join(OUT, file);
      fs.writeFileSync(out, Buffer.from(r.data, 'base64'));
      const match = expect ? `${st.w}x${st.h} (expect ${expect})` : `${st.w}x${st.h}`;
      console.log(`  ${file.padEnd(12)} ${match}  ${Buffer.from(r.data, 'base64').length} bytes -> ${out}`);
      return st;
    };

    console.log('=== screenshot run ===');
    await shot('panel', 'panel.png', '400x580');
    await shot('compact', 'compact.png', 'h=66');
    await shot('pill', 'pill.png', 'h=66');
    await evalJs(`applyForm('panel', false)`, true);
    await sleep(500);
  } catch (e) {
    ok = false;
    console.error('screenshot failed:', e.message || e);
  } finally {
    // ② 还原现场:连内存带磁盘一起写回,并核对与快照一致。
    try {
      const back = await evalJs(`(async () => {
        CFG = ${cfgSnap};
        await invoke('set_config', { config: CFG });
        applyConfigToUI();
        await applyForm(CFG.form || 'panel', false);
        return JSON.stringify(CFG);
      })()`, true);
      const same = back === cfgSnap;
      if (!same) { ok = false; console.error('  FAIL restore: CFG differs from the snapshot'); }
      else console.log('  restore  CFG written back and verified identical');
    } catch (e) {
      ok = false;
      console.error('  FAIL restore (fake data may still be visible):', e.message || e);
    }
    try { await send('Emulation.setDefaultBackgroundColorOverride', {}); } catch {}
    try { ws.close(); } catch {}
    process.exit(ok ? 0 : 1);
  }
})().catch((e) => { console.error('shot failed:', e.message || e); process.exit(1); });
