// 首页「今天优先用」真机校验(CDP 连真实 WebView2 应用)。
// 断言:
// ① 中格选「今天尚未到期的最早正数批次」,同渠道同截止时刻合计,限流渠道不参与;
// ② 点击中格打开该渠道的详情弹层;
// ③ 分钟刷新时刻按批次截止时刻预约,跨过截止时刻/本地午夜后重新选择;
// ④ 没有合格批次时回落到「最先到期 / 最早可确认」,账户截止继续进七日提醒。
const fs = require('node:fs');
const path = require('node:path');

if (process.argv.includes('--self-test')) {
  const read = (p) => fs.readFileSync(path.join(__dirname, '..', p), 'utf8');
  const expiry = read('app/src/expiry-ui.js');
  const view = read('app/src/app-view.js');
  const core = read('app/src/app-core.js');
  const rules = [
    [/find\(item => !item\.channel\.limited\)/, '限流渠道必须排除'],
    [/entry\.at\) < midnight/, '只接受今天截止的批次'],
    [/item\.channel\.id === earliest\.channel\.id && Number\(item\.entry\.at\) === at/, '同渠道同截止时刻合计'],
    [/nextPriorityCheck/, '必须预约下一次分钟刷新'],
    [/homeEvent\(\s*summary\s*\)/, '中格必须走 homeEvent 优先级口径'],
    [/"今天优先用"/, '中格标签必须可切换为今天优先用'],
    [/ExpiryUI\.needsTick/, '分钟定时器必须查询今天优先用的刷新时刻'],
  ];
  const missing = rules.filter(([re]) => !re.test(expiry + view + core)).map(([, why]) => why);
  if (missing.length) {
    console.error('self-test failed: ' + missing.join(' / '));
    process.exit(1);
  }
  console.log('PASS 今天优先用静态规则自检');
  process.exit(0);
}

const PORT = Number(process.argv[2] || process.env.CDP_PORT || 9222);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const EXPR = `(() => {
  const D = window.Date;
  const origNow = D.now;
  const origChannels = CHANNELS;
  const origVisible = WINDOW_VISIBLE;
  const out = { errors: [] };
  const flat = (s) => String(s || '').replace(/\\s+/g, ' ').trim();
  let cellOpen = false;
  try {
    WINDOW_VISIBLE = true;
    const base = new D();
    base.setHours(12, 0, 0, 0);
    const clock = base.getTime();
    const day = 86400000;
    D.now = () => clock;
    const tpl = (origChannels || []).find((c) => c.kind === 'points' && c.hasKey) || (origChannels || [])[0] || {};
    const mk = (o) => Object.assign({}, tpl, {
      kind: 'points', valid: true, hasKey: true, hidden: false, stale: false, expiryStale: false,
      expiryComplete: true, remaining: 100, limited: false, expiring: [], accountExpiry: null,
    }, o);
    const atA = clock / 1000 + 3600;
    const atB = clock / 1000 + day / 1000 + 3600;
    const A = mk({ id: 'qa-today', name: 'QA 今日', expiring: [{ at: atA, amount: 20 }, { at: atA, amount: 5 }] });
    const B = mk({ id: 'qa-tomorrow', name: 'QA 明日', expiring: [{ at: atB, amount: 80 }] });
    const C = mk({ id: 'qa-limited', name: 'QA 限流', limited: true, expiring: [{ at: clock / 1000 + 600, amount: 99 }] });
    CHANNELS = [C, A, B];
    render({});
    let cell = document.querySelector('#sum [data-act="jump"]');
    out.today = cell ? { id: cell.dataset.id, text: flat(cell.textContent), title: cell.title } : null;
    out.tickBefore = window.ExpiryUI.needsTick(atA * 1000 - 1);
    out.tickAt = window.ExpiryUI.needsTick(atA * 1000);
    if (cell) {
      try {
        cell.click();
        cellOpen = true;
        out.detailId = DETAIL_ID;
        out.detailOpen = document.body.classList.contains('detail-open');
        out.detailText = flat((document.getElementById('detailHead') || {}).textContent);
      } catch (e) {
        out.errors.push('detail:' + (e && e.message));
      }
      try { closeDetail(false); } catch (e) { out.errors.push('close:' + (e && e.message)); }
    }
    D.now = () => (atA + 2) * 1000;
    render({});
    cell = document.querySelector('#sum [data-act="jump"]');
    out.after = cell ? { id: cell.dataset.id, text: flat(cell.textContent) } : null;
    D.now = () => clock;
    CHANNELS = [B];
    render({});
    cell = document.querySelector('#sum [data-act="jump"]');
    out.tomorrow = cell ? { id: cell.dataset.id, text: flat(cell.textContent) } : null;
    const mid = new D(clock);
    mid.setHours(0, 0, 0, 0);
    mid.setDate(mid.getDate() + 1);
    out.midnight = mid.getTime();
    out.midnightBefore = window.ExpiryUI.needsTick(mid.getTime() - 1);
    out.midnightAt = window.ExpiryUI.needsTick(mid.getTime());
    CHANNELS = [mk({ id: 'qoder-cn', name: 'QA 账户', remaining: 500, expiryComplete: false,
      accountExpiry: { state: 'dated', at: clock / 1000 + 7200 } })];
    render({});
    cell = document.querySelector('#sum [data-act="jump"]');
    out.account = cell ? { id: cell.dataset.id, text: flat(cell.textContent) } : null;
  } catch (e) {
    out.errors.push(String((e && e.message) || e));
  } finally {
    D.now = origNow;
    CHANNELS = origChannels;
    WINDOW_VISIBLE = origVisible;
    if (cellOpen) { try { closeDetail(false); } catch (e) { out.errors.push('close2:' + (e && e.message)); } }
    try { render({}); } catch (e) { out.errors.push('restore:' + (e && e.message)); }
  }
  return JSON.stringify(out);
})()`;

(async () => {
  let targets;
  try {
    targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  } catch (e) {
    console.error('CDP unreachable on port', PORT, '->', e.message);
    process.exit(1);
  }
  const t = targets.filter((x) => x.type === 'page')
    .find((x) => /tauri|grandettoken|tokenscope/i.test(`${x.title || ''} ${x.url || ''}`));
  if (!t) { console.error('no app page'); process.exit(1); }
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.addEventListener('open', res); ws.addEventListener('error', () => rej(new Error('ws'))); });
  let id = 0; const pending = new Map();
  ws.addEventListener('message', (ev) => { const m = JSON.parse(ev.data); const cb = pending.get(m.id); if (cb) { pending.delete(m.id); cb(m); } });
  const evalJs = (expression) => new Promise((res, rej) => {
    const i = ++id;
    pending.set(i, (m) => {
      const ex = m.result && m.result.exceptionDetails;
      if (ex) rej(new Error(String((ex.exception && ex.exception.description) || ex.text).split('\n')[0]));
      else res(m.result.result.value);
    });
    ws.send(JSON.stringify({ id: i, method: 'Runtime.evaluate', params: { expression, returnByValue: true } }));
  });

  let fails = 0;
  const check = (name, ok, detail) => { console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${ok ? '' : '  · ' + (detail || '')}`); if (!ok) fails++; };

  try {
    console.log('=== 首页今天优先用 ===');
    await evalJs('(async () => { await loadCheckin(); })()');
    await sleep(300);
    const r = JSON.parse(await evalJs(EXPR));
    check('中格选今天最早批次并合计同刻积分',
      !!r.today && r.today.id === 'qa-today' && /今天优先用/.test(r.today.text) &&
      /QA 今日/.test(r.today.text) && /25 分/.test(r.today.text), JSON.stringify(r.today));
    check('限流渠道的更早批次不参与优先', !!r.today && r.today.id !== 'qa-limited', JSON.stringify(r.today));
    check('分钟刷新在批次截止时刻才触发',
      r.tickBefore === false && r.tickAt === true, `before=${r.tickBefore} at=${r.tickAt}`);
    check('点击中格打开对应渠道详情',
      r.detailId === 'qa-today' && r.detailOpen === true && /QA 今日/.test(r.detailText || ''),
      `id=${r.detailId} open=${r.detailOpen} text=${r.detailText}`);
    check('跨过截止时刻后重选明日批次并改回最先到期',
      !!r.after && r.after.id === 'qa-tomorrow' && /最先到期/.test(r.after.text) && !/今天优先用/.test(r.after.text),
      JSON.stringify(r.after));
    check('只剩明日批次时中格不是今天优先用',
      !!r.tomorrow && !/今天优先用/.test(r.tomorrow.text), JSON.stringify(r.tomorrow));
    check('本地午夜预约下一次分钟刷新',
      r.midnightBefore === false && r.midnightAt === true, `before=${r.midnightBefore} at=${r.midnightAt}`);
    check('账户截止继续进七日提醒但不冒充今日积分',
      !!r.account && /账户额度到期/.test(r.account.text) && !/今天优先用/.test(r.account.text), JSON.stringify(r.account));
    check('探针未产生脚本异常', r.errors.length === 0, r.errors.join('; '));
  } catch (e) {
    console.log('  FAIL  脚本异常 · ' + e.message);
    fails++;
  }
  ws.close();
  console.log(fails ? `✗ ${fails} 项失败` : '✓ 首页今天优先用真机校验通过');
  process.exit(fails ? 1 : 0);
})().catch((e) => { console.error(e.message); process.exit(1); });
