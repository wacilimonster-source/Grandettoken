// 验证「固定窗口布局」(2026-11-03 取消自由拖动缩放后的替代闸门)。CDP 连真机。
//
// 用法: launch-debug.ps1(或带 CDP 启动)后 node build/verify-resize.js [port]
// 覆盖:
//   · 拖动缩放入口已移除(rzFrame / rzGrip 不存在)
//   · 单列 / 双列两档固定尺寸与 data-w 断点、关键区域不越界
//   · 形态往返(紧凑条 / 胶囊 → 面板)后固定尺寸保持
//   · xl 字号:窗口高度被显示器钳制,不超出屏幕
// 说明:布局切换用内存预览(CFG.panelLayout + applyForm),不写用户配置;
//       结束时还原原布局并 refresh()。
const PORT = Number(process.argv[2] || process.env.CDP_PORT || 9222);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  const t = targets.filter((x) => x.type === 'page')
    .find((x) => /tauri|grandettoken|tokenscope/i.test(`${x.title || ''} ${x.url || ''}`));
  if (!t) { console.error('no app page'); process.exit(1); }
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

  try {
    console.log('=== 固定窗口布局 ===');
    await evalJs(`(()=>{window.__vPush=applyChannelsPush;window.__vRefresh=refresh;applyChannelsPush=()=>{};refresh=async()=>{};window.__vCfg=JSON.stringify(CFG);})()`);
    await evalJs(`(()=>{CFG.panelLayout='single';CFG.theme='dark';applyTheme();applyForm('panel',false,true);})()`);
    await sleep(900);

    // 1. 拖动缩放入口已移除
    const gone = JSON.parse(await evalJs(`JSON.stringify({frame:!document.getElementById('rzFrame'),grip:!document.getElementById('rzGrip')})`));
    check('拖动缩放入口已移除(rzFrame/rzGrip)', gone.frame && gone.grip, JSON.stringify(gone));

    // 2. 两档固定尺寸
    for (const [layout, wantW, wantH, wantWcls] of [['single', 500, 840, 'sm'], ['double', 880, 840, 'lg']]) {
      await evalJs(`(()=>{CFG.panelLayout='${layout}';updateLayoutUI();applyForm('panel',false,true);})()`);
      await sleep(900);
      const s = JSON.parse(await evalJs(`(()=>{const btn=document.querySelector('.tbar').getBoundingClientRect();
        const foot=document.querySelector('.foot').getBoundingClientRect();
        return JSON.stringify({w:innerWidth,h:innerHeight,dw:document.body.dataset.w,
          hScroll:document.documentElement.scrollWidth>innerWidth,
          inWin:btn.right<=innerWidth+1&&foot.bottom<=innerHeight+1,
          label:document.getElementById('lkLayout').textContent})})()`));
      check(`${layout}:窗口 ${wantW}×${wantH}`, s.w === wantW && s.h === wantH, JSON.stringify(s));
      check(`${layout}:有效宽度断点 ${wantWcls}`, s.dw === wantWcls, s.dw);
      check(`${layout}:无横向越界`, !s.hScroll);
      check(`${layout}:标题栏 / 页脚在窗内`, s.inWin);
    }

    // 3. 形态往返后固定尺寸保持
    await evalJs(`(()=>{CFG.panelLayout='single';updateLayoutUI();applyForm('panel',false,true);})()`);
    await sleep(700);
    await evalJs(`applyForm('compact',false)`); await sleep(700);
    await evalJs(`applyForm('pill',false)`); await sleep(700);
    await evalJs(`applyForm('panel',false)`); await sleep(900);
    const back = JSON.parse(await evalJs(`JSON.stringify({w:innerWidth,h:innerHeight,form:document.body.className.includes('form-panel')})`));
    check('形态往返后面板回到固定尺寸 500×840', back.form && back.w === 500 && back.h === 840, JSON.stringify(back));

    // 4. xl 字号:高度被显示器钳制
    await evalJs(`(()=>{const o=CFG.fontScale;CFG.fontScale='xl';applyForm('panel',false,true);CFG.fontScale=o;})()`);
    await sleep(900);
    const xl = JSON.parse(await evalJs(`JSON.stringify({h:innerHeight,screen:screen.height,fit:innerHeight<=screen.height})`));
    check('xl 字号:窗口高度不超过屏幕', xl.fit, JSON.stringify(xl));

    // 还原
    await evalJs(`(()=>{const o=JSON.parse(window.__vCfg);Object.assign(CFG,o);applyTheme();updateLayoutUI();applyForm('panel',false,true);applyChannelsPush=window.__vPush;refresh=window.__vRefresh;})()`);
    await sleep(400);
    await evalJs(`refresh()`); await sleep(600);
    check('现场还原', true);
  } catch (e) {
    console.log('  FAIL  脚本异常 · ' + e.message);
    fails++;
  }
  ws.close();
  console.log(fails ? `✗ ${fails} 项失败` : '✓ 固定布局真机校验通过');
  process.exit(fails ? 1 : 0);
})().catch((e) => { console.error(e.message); process.exit(1); });
