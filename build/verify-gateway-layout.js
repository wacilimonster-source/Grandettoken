// 验证网关三个进阶面板在真实窗口宽度下不横向越界。CDP 连真机。
//
// 用法: launch-debug.ps1(或带 CDP 启动)后 node build/verify-gateway-layout.js [port]
//       node build/verify-gateway-layout.js [port] --self-test
// 背景: `.gw-tab-pane` 是 grid,隐式 auto 轨道会被用量账本表格的 nowrap 最小内容
//       撑宽整个 pane(v0.3.73 实机走查抓出:513px 容器里出现 713px 卡片)。
//       修复为 grid-template-columns:minmax(0,1fr),表格改在卡片内横向滚动。
// 说明: 布局切换用内存预览(CFG.panelLayout + applyForm),不写用户配置;结束还原。
//       --self-test 先注入一张超宽卡片坏样本,证明本脚本确实会变红,再移除复测。
const PORT = Number(process.argv[2] || process.env.CDP_PORT || 9222);
const SELF = process.argv.includes('--self-test');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const TABS = ['groups', 'usage', 'connect'];

(async () => {
  const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  const t = targets.filter((x) => x.type === 'page')
    .find((x) => /tauri|grandettoken|tokenscope/i.test(`${x.title || ''} ${x.url || ''}`));
  if (!t) { console.error('no app page on port ' + PORT); process.exit(1); }
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.addEventListener('open', res); ws.addEventListener('error', () => rej(new Error('ws'))); });
  let id = 0; const pending = new Map();
  ws.addEventListener('message', (ev) => { const m = JSON.parse(ev.data); const cb = pending.get(m.id); if (cb) { pending.delete(m.id); cb(m); } });
  const evalJs = (expression, awaitPromise = false) => new Promise((res, rej) => {
    const i = ++id;
    pending.set(i, (m) => {
      const ex = m.result && m.result.exceptionDetails;
      if (ex) rej(new Error(String((ex.exception && ex.exception.description) || ex.text).split('\n')[0]));
      else res(m.result.result.value);
    });
    ws.send(JSON.stringify({ id: i, method: 'Runtime.evaluate', params: { expression, awaitPromise, returnByValue: true } }));
  });

  let fails = 0;
  const check = (name, ok, detail) => { console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${ok ? '' : '  · ' + detail}`); if (!ok) fails++; };

  const openGateway = async () => {
    await evalJs(`(() => { openManage(); showTab('gateway'); return true; })()`);
    await sleep(1300);
  };
  const measure = async (tab) => {
    await evalJs(`(() => { const b = document.querySelector('#gatewayBox [data-gw-tab="${tab}"]'); if (b) b.click(); return true; })()`);
    await sleep(1000);
    return JSON.parse(await evalJs(`(() => {
      const p = document.querySelector('#gatewayBox [data-gw-pane="${tab}"]');
      if (!p || p.hidden) return JSON.stringify({ missing: true });
      const cards = [...p.querySelectorAll('.gw-card-box')].map((e) => Math.round(e.getBoundingClientRect().width));
      return JSON.stringify({ sw: p.scrollWidth, cw: p.clientWidth, maxCard: Math.max(0, ...cards),
        docSW: document.documentElement.scrollWidth, win: innerWidth });
    })()`));
  };
  const setLayout = async (layout) => {
    await evalJs(`(async () => { CFG.panelLayout = '${layout}'; updateLayoutUI(); render(); await applyForm('panel', false, true); })()`, true);
    await sleep(900);
    await openGateway();
  };

  try {
    console.log('=== 网关进阶面板横向越界 ===');
    await evalJs(`(async()=>{
      if(DOCK.on) throw new Error('请先解除贴边');
      window.__gwLayoutState={cfg:JSON.stringify(CFG),push:applyChannelsPush,refresh};
      applyChannelsPush=()=>{};refresh=async()=>{};
      CFG.fontScale='md';applyFontScale();
    })()`, true);

    const runSuite = async (tag) => {
      let bad = 0;
      for (const layout of ['single', 'double', 'thumbnail']) {
        await setLayout(layout);
        for (const tab of TABS) {
          const m = await measure(tab);
          const ok = !m.missing && m.sw <= m.cw + 2 && m.maxCard <= m.cw + 2 && m.docSW <= m.win + 1;
          if (!ok) bad++;
          check(`${tag} ${layout}/${tab} 不横向越界`, ok, JSON.stringify(m));
        }
      }
      return bad;
    };

    if (SELF) {
      console.log('--- self-test: 注入超宽卡片坏样本,期望变红 ---');
      await setLayout('single');
      await evalJs(`(() => { const s = document.createElement('style'); s.id = 'gwLayoutSentinel';
        s.textContent = '#gatewayBox [data-gw-pane="usage"] .gw-card-box{min-width:900px}';
        document.head.appendChild(s); return true; })()`);
      await sleep(400);
      const red = await measure('usage');
      const redOk = !red.missing && (red.sw > red.cw + 2 || red.maxCard > red.cw + 2);
      check('self-test 坏样本被判红(证明闸门有效)', redOk, JSON.stringify(red));
      await evalJs(`(() => { const s = document.getElementById('gwLayoutSentinel'); if (s) s.remove(); return true; })()`);
      await sleep(400);
      const green = await measure('usage');
      check('self-test 移除坏样本后复绿', !green.missing && green.sw <= green.cw + 2 && green.maxCard <= green.cw + 2, JSON.stringify(green));
    }

    const bad = await runSuite('正式');
    console.log(bad ? `\n${bad} 处越界` : '\nALL PASS');
    await evalJs(`(async()=>{
      const s=window.__gwLayoutState; if(!s) return;
      CFG=JSON.parse(s.cfg); applyChannelsPush=s.push; refresh=s.refresh;
      updateLayoutUI(); applyTheme(); applyFontScale(); render(); await applyForm(currentForm(),false,true);
      closeManage(false);
    })()`, true).catch(() => {});
    ws.close();
    process.exit(fails ? 1 : 0);
  } catch (e) {
    console.error('verify failed:', e.message || e);
    try { ws.close(); } catch (_) {}
    process.exit(1);
  }
})().catch((e) => { console.error('verify failed:', e.message || e); process.exit(1); });