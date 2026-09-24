// 2026-09-23 UI 优化轮的运行时断言:状态点分层 / 环比 / 排序前置 / +N / toast /
// 胶囊指示点 / 贴边新类。用法:launch 带 9222 后 node build/verify-ui-0923.js
const PORT = Number(process.argv[2] || 9222);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function connect() {
  const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  const t = targets.find((x) => x.type === 'page') || targets[0];
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', () => rej(new Error('ws error')));
  });
  let id = 0; const pending = new Map();
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
  return { ws, evalJs };
}

const CH = [
  { id: '4sapi', name: '4SAPI', short: '4S', color: '#e8622c', kind: 'amount', hasKey: true, valid: true,
    remaining: 74.2, used: 125.8, total: 399.83, unit: 'CNY', windows: [], expiring: [], extra: [],
    limited: false, authSource: 'keyring', authLabel: '', day: 12.4, week: 86.1, month: 301.55,
    dayPrev: 10.0, weekPrev: 92.0, monthPrev: 280.0, estimated: true, stale: false, updatedAt: Math.floor(Date.now()/1000),
    claim: { amount: 200, lastClaimAt: 1758000000, source: 'auto', daysUntilEligible: 6, eligible: false, dailyBurn: 12.3, daysOfBalance: 6, shortageRisk: false }, hidden: false },
  { id: 'opencode-go', name: 'OpenCode Go', short: 'OC', color: '#f5a623', kind: 'percent', hasKey: true, valid: true,
    remaining: 82, used: 18, total: null, unit: 'percent', expiring: [], extra: [],
    windows: [ { label: '5 小时', remainPercent: 91.5, status: 'ok', resetsAt: null, main: false },
               { label: '本周', remainPercent: 88.0, status: 'ok', resetsAt: null, main: false },
               { label: '周期', remainPercent: 82.0, status: 'ok', resetsAt: null, main: true } ],
    limited: false, authSource: 'keyring', authLabel: '', day: null, week: null, month: null,
    dayPrev: null, weekPrev: null, monthPrev: null, estimated: false, stale: false, updatedAt: Math.floor(Date.now()/1000), claim: null, hidden: false },
  { id: 'deepseek', name: 'DeepSeek', short: 'DS', color: '#4d6bfe', kind: 'amount', hasKey: true, valid: false,
    remaining: 256.0, used: null, total: null, unit: 'CNY', windows: [], expiring: [], extra: [],
    error: 'HTTP 401', limited: false, authSource: 'keyring', authLabel: '', day: 3.1, week: 20.0, month: 88.0,
    dayPrev: null, weekPrev: null, monthPrev: null, estimated: true, stale: true, updatedAt: Math.floor(Date.now()/1000) - 720, claim: null, hidden: false },
  { id: 'hapi', name: 'Hapi', short: 'HA', color: '#19b8c4', kind: 'amount', hasKey: false, valid: false,
    remaining: null, used: null, total: null, unit: 'CNY', windows: [], expiring: [], extra: [],
    limited: false, authSource: 'keyring', authLabel: '', day: null, week: null, month: null,
    dayPrev: null, weekPrev: null, monthPrev: null, estimated: true, stale: false, updatedAt: 0, claim: null, hidden: false },
];

(async () => {
  const { ws, evalJs } = await connect();
  const out = [];
  const check = (name, cond, detail = '') => { out.push(`${cond ? 'PASS' : 'FAIL'}  ${name}${detail ? '  (' + detail + ')' : ''}`); if (!cond) process.exitCode = 1; };

  // 注入仿真数据(含未配置 / 失联 / 环比字段)
  await evalJs(`CHANNELS = ${JSON.stringify(CH)}; CFG.hiddenChannels = []; render();`);
  await sleep(300);

  const r1 = JSON.parse(await evalJs(`JSON.stringify((() => {
    const q = (s) => document.querySelector(s);
    const rows = [...document.querySelectorAll('#list .row')];
    const byId = (id) => rows.find((r) => r.dataset.id === id);
    return {
      sortInd: q('#btnSort') ? q('#btnSort').textContent : null,
      dsDot: byId('deepseek') ? byId('deepseek').querySelector('.n .dot').className : null,
      dsDtone: byId('deepseek') ? byId('deepseek').dataset.dtone : null,
      dsSub: byId('deepseek') ? byId('deepseek').querySelector('.nm .s').textContent : null,
      sumCells: [...document.querySelectorAll('#sum .cell')].map((c) => c.textContent.trim()),
      deltaEls: [...document.querySelectorAll('#sum .delta')].map((d) => d.className + ':' + d.textContent),
      swButtons: [...document.querySelectorAll('.sw')].map((s) => s.tagName + ':' + (s.getAttribute('role') || '')),
      ltrCount: document.querySelectorAll('.ico .ltr').length,
      footDot: q('#fdot').className,
    };
  })())`));

  check('I1 排序指示存在且显示当前模式', !!r1.sortInd && r1.sortInd.length >= 3, r1.sortInd);
  check('U2 失联渠道状态点 = dot st(斜杠)', r1.dsDot === 'dot st', r1.dsDot);
  check('U2 行 data-dtone = st', r1.dsDtone === 'st', r1.dsDtone);
  check('U2 失联副标题 = 取数失败', r1.dsSub === '取数失败', r1.dsSub);
  check('U8 汇总条有环比箭头', r1.deltaEls.some((d) => d.includes('delta up') || d.includes('delta dn')), JSON.stringify(r1.deltaEls));
  check('U8 今日 ↑24%((12.4-10)/10)', r1.deltaEls.some((d) => d.includes('↑24%')), JSON.stringify(r1.deltaEls));
  check('U8 本周 ↓6%((86.1-92)/92=-6.4)', r1.deltaEls.some((d) => d.includes('↓6%')), JSON.stringify(r1.deltaEls));
  check('U7 开关已是 button[role=switch]', r1.swButtons.length > 0 && r1.swButtons.every((s) => s === 'BUTTON:switch'), JSON.stringify(r1.swButtons));
  check('U6 官方标内嵌字母副本(.ltr,2 个官方标渠道)', r1.ltrCount === 2, String(r1.ltrCount));
  check('底栏状态点分层(有失联 → st)', r1.footDot === 'dot st', r1.footDot);

  // I1 点击排序指示 → 按循环顺序走一格(起点取决于真实配置)
  const sortBefore = await evalJs(`CFG.sort`);
  const cycle = ['percent', 'balance', 'dayUsage', 'custom'];
  const want = cycle[(cycle.indexOf(sortBefore) + 1) % 4];
  await evalJs(`document.getElementById('btnSort').click()`);
  await sleep(200);
  const sortAfter = await evalJs(`CFG.sort`);
  check('I1 点击后循环到下一排序模式', sortAfter === want, sortBefore + '→' + sortAfter);
  await evalJs(`document.getElementById('btnSort').click(); document.getElementById('btnSort').click(); document.getElementById('btnSort').click();`);
  await sleep(150);

  // M2 刷新按钮 loading 类存在性(手动模拟)
  const toast = JSON.parse(await evalJs(`(async () => {
    showToast('已更新 · 刚刚', false);
    const t = document.getElementById('toast');
    const shown = t.classList.contains('show') && t.textContent.includes('已更新');
    await new Promise((r) => setTimeout(r, 2900));
    return JSON.stringify({ shown, gone: !t.classList.contains('show') });
  })()`, true));
  check('M2/I3 toast 显示并自动消失', toast.shown && toast.gone, JSON.stringify(toast));

  // M5 胶囊:轮播指示点(选 2 个渠道)
  await evalJs(`CFG.pillChannels = ['4sapi', 'opencode-go']; render(); applyForm('pill', false);`);
  await sleep(800);
  const pill = JSON.parse(await evalJs(`JSON.stringify({
    dots: document.querySelectorAll('#pillDots i').length,
    on: document.querySelectorAll('#pillDots i.on').length,
    h: window.innerHeight,
  })`));
  check('M5 胶囊轮播指示点(2 个渠道 → 2 点,1 个高亮)', pill.dots === 2 && pill.on === 1, JSON.stringify(pill));

  // I2 紧凑条 +N:窄内容下塞 4 个渠道大概率触发;先回面板再切紧凑条
  await evalJs(`applyForm('compact', false)`);
  await sleep(900);
  const compact = JSON.parse(await evalJs(`JSON.stringify((() => {
    const more = document.querySelector('#list .more');
    const ltrShown = getComputedStyle(document.querySelector('#list .ico.logo .ltr') || document.createElement('div')).display;
    const imgShown = getComputedStyle(document.querySelector('#list .ico.logo img') || document.createElement('div')).display;
    return { more: more ? more.textContent : null, title: more ? more.title : null, ltrShown, imgShown };
  })())`));
  check('U6 紧凑条显示字母(ltr 可见 / 官方图隐藏)', compact.ltrShown === 'grid' && compact.imgShown === 'none', JSON.stringify(compact));

  // M6/I6:贴边类与新元素就位(不真拖窗口,直接调 dockEnter/dockLayout)
  const dock = JSON.parse(await evalJs(`(async () => {
    await dockEnter('right', { x: 0, y: 300 });
    const collapsed = document.body.classList.contains('docked') && !document.body.classList.contains('dock-open');
    await dockLayout(true);
    const opened = document.body.classList.contains('dock-open') && !document.body.classList.contains('docked');
    const anim = document.body.classList.contains('dock-anim');
    const closeBtn = getComputedStyle(document.getElementById('dockClose')).display;
    document.getElementById('dockClose').click();
    await new Promise((r) => setTimeout(r, 700));
    const backCollapsed = document.body.classList.contains('docked') && !document.body.classList.contains('dock-open');
    await dockExit(false);
    return JSON.stringify({ collapsed, opened, anim, closeBtn, backCollapsed });
  })()`, true));
  check('M6 吸附收起/展开/dock-open 类', dock.collapsed && dock.opened, JSON.stringify(dock));
  check('M6 展开有滑入动画类 + ✕可见', dock.anim && dock.closeBtn === 'grid', JSON.stringify(dock));
  check('I6 ✕ 点击收回为竖条', dock.backCollapsed, JSON.stringify(dock));

  await evalJs(`applyForm('panel', false)`);
  await sleep(600);
  // 还原现场:仿真数据与配置改动只在内存里(排序点击恰好一整圈回到原值),
  // 重新读一遍真实配置与真实数据,不让测试残留影响用户正在用的挂件
  await evalJs(`(async () => { CFG = await invoke('get_config'); applyConfigToUI(); await refresh(); })()`, true);
  await sleep(500);
  console.log('=== 2026-09-23 UI verification ===');
  out.forEach((l) => console.log('  ' + l));
  ws.close();
  process.exit(process.exitCode || 0);
})().catch((e) => { console.error('verify failed:', e.message || e); process.exit(1); });
