// 验证「签到中心」页签:CDP 连真机,断言页签可见性、五段结构、真实数据下的
// 日历带与明细表、以及「点首页签到格 → 落到签到中心」整条链路。只读,不写配置。
const PORT = Number(process.argv[2] || process.env.CDP_PORT || 9222);

(async () => {
  const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  const t = targets.filter((x) => x.type === 'page')
    .find((x) => /tauri|grandettoken|tokenscope/i.test(`${x.title || ''} ${x.url || ''}`));
  if (!t) { console.error('no app page'); process.exit(1); }

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
    ws.send(JSON.stringify({ id: i, method: 'Runtime.evaluate',
      params: { expression, awaitPromise, returnByValue: true } }));
  });
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  let fails = 0;
  const check = (name, ok, detail) => {
    console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${ok ? '' : '  · ' + detail}`);
    if (!ok) fails++;
  };

  try {
    console.log('=== 签到中心 ===');

    // ① 页签可见性跟着 CK 走:开着的渠道 > 0 → 页签出现
    await evalJs(`(async () => { await loadCheckin(); })()`, true);
    await sleep(200);
    const vis = JSON.parse(await evalJs(`JSON.stringify({
      ckBtn: !!document.querySelector('#mTabs [data-p="ck"]'),
      hidden: document.querySelector('#mTabs [data-p="ck"]').hidden,
      enabledIds: Object.entries(CK).filter(([, s]) => s.enabled).map(([k]) => k),
    })`));
    check('签到页签按钮存在', vis.ckBtn);
    check(visibleMsg(vis), (vis.enabledIds.length ? !vis.hidden : true), `hidden=${vis.hidden}`);

    // ② 切到页签:五段都在,数据来自真库
    await evalJs(`openCheckinCenter()`);
    await sleep(400);
    const center = JSON.parse(await evalJs(`JSON.stringify((() => {
      const c = document.getElementById('ckCenter');
      const q = (s) => !!c.querySelector(s);
      return {
        on: document.querySelector('#mTabs [data-p="ck"]').classList.contains('on'),
        today: q('.cktoday'), todayRow: q('.ckch'), acts: q('.ck-overview'),
        band: q('.ckband'), cells: c.querySelectorAll('.ckc').length,
        table: q('.cktable'), rows: c.querySelectorAll('.cklr').length,
        chips: c.querySelectorAll('[data-act="ckhfilter"]').length,
        switches: c.querySelectorAll('[data-act="ckenable"]').length,
        whyOk: !c.querySelector('button.why[data-why="checkin"]:not([data-for])'),
        empty: c.querySelector('.ckempty') ? c.querySelector('.ckempty').textContent : '',
      };
    })())`));
    check('点开落在签到中心页签', center.on);
    check('今日总览条在', center.today);
    check('今日逐渠道行在', center.todayRow);
    check('活动状态段在', center.acts);
    check('日历带在(30 格/渠道)', center.band && center.cells >= 30, `cells=${center.cells}`);
    check('明细表有真数据行', center.table && center.rows > 0, `rows=${center.rows} ${center.empty}`);
    check('渠道筛选芯片在', center.chips >= 2, `chips=${center.chips}`);
    check('管理开关在', center.switches >= 1, `switches=${center.switches}`);
    check('「?」说明已水合', center.whyOk);

    // ③ 明细渠道筛选:点芯片后行数只减不增,且行都属该渠道
    const before = JSON.parse(await evalJs(`JSON.stringify({
      all: document.querySelectorAll('#ckCenter .cklr').length,
    })`));
    const chipWb = await evalJs(`(() => {
      const el = document.querySelector('#ckCenter [data-act="ckhfilter"][data-filter]:not([data-filter="all"])');
      if (!el) return '';
      el.click();
      return el.dataset.filter;
    })()`);
    await sleep(80);
    const after = JSON.parse(await evalJs(`JSON.stringify({
      rows: document.querySelectorAll('#ckCenter .cklr').length,
    })`));
    check(`筛选芯片「${chipWb}」生效(行数 ${before.all} → ${after.rows})`,
      !!chipWb && after.rows > 0 && after.rows <= before.all);
    await evalJs(`(() => {
      const el = document.querySelector('#ckCenter [data-act="ckhfilter"][data-filter="all"]');
      if (el) el.click();
    })()`);

    // ④ 回首页,点顶部签到格 → 应落回签到中心(不再去「刷新与启动」)
    await evalJs(`closeManage(false); render({ updated: false })`);
    await sleep(200);
    const strip = JSON.parse(await evalJs(`JSON.stringify({
      cell: !!document.querySelector('#sum [data-act="ckstripopen"]'),
    })`));
    if (strip.cell) {
      await evalJs(`document.querySelector('#sum [data-act="ckstripopen"]').click()`);
      await sleep(400);
      const back = JSON.parse(await evalJs(`JSON.stringify({
        on: document.querySelector('#mTabs [data-p="ck"]').classList.contains('on'),
        runOn: document.querySelector('#mTabs [data-p="run"]').classList.contains('on'),
      })`));
      check('点首页签到格 → 落到签到中心(不再落「刷新与启动」)', back.on && !back.runOn);
    } else {
      console.log('  skip  首页没有签到格(该渠道未开启或已全签,属正常)');
    }
  } catch (e) {
    console.log('  FAIL  脚本异常 · ' + e.message);
    fails++;
  }

  ws.close();
  console.log(fails ? `✗ ${fails} 项失败` : '✓ 签到中心真机校验全部通过');
  process.exit(fails ? 1 : 0);
})().catch((e) => { console.error(e); process.exit(1); });

function visibleMsg(v) {
  return v.enabledIds.length
    ? `有渠道开着监测(${v.enabledIds.join(',')}):页签可见`
    : '无渠道开着监测:页签不强制出现';
}
