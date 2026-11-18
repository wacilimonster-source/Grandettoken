// 验证「固定窗口布局」(2026-11-03 取消自由拖动缩放后的替代闸门)。CDP 连真机。
//
// 用法: launch-debug.ps1(或带 CDP 启动)后 node build/verify-resize.js [port]
// 覆盖:
//   · 拖动缩放入口已移除(rzFrame / rzGrip 不存在)
//   · 单列 / 双列 / 缩略固定尺寸与 data-w 断点、关键区域不越界
//   · 形态往返(紧凑条 / 胶囊 → 面板)后固定尺寸保持
//   · 大 / 特大字号下普通面板、贴边展开与解除贴边均避开任务栏
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
    await evalJs(`(async()=>{
      if(DOCK.on) throw new Error('请先解除贴边');
      window.__vState={cfg:JSON.stringify(CFG),pos:await appWindow.outerPosition(),
        form:currentForm(),push:applyChannelsPush,refresh};
      applyChannelsPush=()=>{};refresh=async()=>{};
      CFG.panelLayout='single';CFG.fontScale='md';CFG.theme='dark';applyTheme();applyFontScale();
      await applyForm('panel',false,true);
    })()`, true);
    await sleep(900);

    // 1. 拖动缩放入口已移除
    const gone = JSON.parse(await evalJs(`JSON.stringify({frame:!document.getElementById('rzFrame'),grip:!document.getElementById('rzGrip')})`));
    check('拖动缩放入口已移除(rzFrame/rzGrip)', gone.frame && gone.grip, JSON.stringify(gone));

    // 2. 两档固定尺寸(期望值按当前字号档位计算:md=1.0 / lg=1.15 / xl=1.3)
    for (const [layout, wantWcls] of [['single', 'sm'], ['double', 'lg'], ['thumbnail', 'sm']]) {
      await evalJs(`(async()=>{CFG.panelLayout='${layout}';updateLayoutUI();render();await applyForm('panel',false,true);})()`, true);
      await sleep(900);
      const [wantW, wantH] = await evalJs(`(async()=>{
        const nominal=formSize('panel');
        const mon=await T.window.currentMonitor(), sf=await appWindow.scaleFactor();
        return [Math.min(nominal[0],Math.floor(mon.workArea.size.width/sf)),
          Math.min(nominal[1],Math.floor(mon.workArea.size.height/sf))];
      })()`, true);
      const s = JSON.parse(await evalJs(`(()=>{const btn=document.querySelector('.tbar').getBoundingClientRect();
        const foot=document.querySelector('.foot').getBoundingClientRect();
        return JSON.stringify({w:innerWidth,h:innerHeight,dw:document.body.dataset.w,
          hScroll:document.documentElement.scrollWidth>innerWidth,
          inWin:btn.right<=innerWidth+1&&foot.bottom<=innerHeight+1,
          label:document.getElementById('lkLayout').textContent})})()`));
      // ±1px:Windows 下 setSize 走物理像素,DPR 非整数(1.25)时逻辑值会差 1
      check(`${layout}:窗口 ${wantW}×${wantH}`, Math.abs(s.w - wantW) <= 1 && Math.abs(s.h - wantH) <= 1, JSON.stringify(s));
      check(`${layout}:有效宽度断点 ${wantWcls}`, s.dw === wantWcls, s.dw);
      check(`${layout}:无横向越界`, !s.hScroll);
      check(`${layout}:标题栏 / 页脚在窗内`, s.inWin);
    }

    // 3. 形态往返后固定尺寸保持
    await evalJs(`(async()=>{CFG.panelLayout='single';CFG.compactEnabled=true;updateLayoutUI();await applyForm('panel',false,true);})()`, true);
    await sleep(700);
    await evalJs(`applyForm('compact',false)`, true); await sleep(700);
    check('切到紧凑条', await evalJs(`document.body.classList.contains('form-compact')`));
    await evalJs(`applyForm('pill',false)`, true); await sleep(700);
    check('切到胶囊', await evalJs(`document.body.classList.contains('form-pill')`));
    await evalJs(`applyForm('panel',false)`, true); await sleep(900);
    const back = await evalJs(`(async()=>({w:innerWidth,h:innerHeight,form:document.body.classList.contains('form-panel'),want:formSize('panel',await monitorInfo())}))()`, true);
    check('形态往返后面板回到固定尺寸', back.form && Math.abs(back.w - back.want[0]) <= 1 && Math.abs(back.h - back.want[1]) <= 1, JSON.stringify(back));

    // 4. 从接近任务栏的位置重排。独立用 Tauri 原始工作区与窗口实测边界核对。
    const verifyWorkArea = async (label, docked = false) => {
      await sleep(350);
      const s = await evalJs(`(async()=>{
        const mon=await T.window.currentMonitor(), sf=await appWindow.scaleFactor();
        const pos=await appWindow.outerPosition(), size=await appWindow.outerSize();
        const area=mon.workArea, footer=document.querySelector('.foot').getBoundingClientRect();
        const top=document.querySelector('.tbar').getBoundingClientRect().top;
        const settings=document.querySelector('#lkKeys').getBoundingClientRect();
        const list=document.querySelector('#list'), scrollTop=list.scrollTop;
        list.scrollTop=list.scrollHeight;
        const last=Array.from(list.querySelectorAll('.row')).at(-1)?.getBoundingClientRect();
        const scrollOk=list.scrollHeight<=list.clientHeight ||
          (list.scrollTop>0 && last && last.bottom<=list.getBoundingClientRect().bottom+1);
        list.scrollTop=scrollTop;
        return {pos,size,area,sf,full:mon.size,viewport:[innerWidth,innerHeight],
          font:CFG.fontScale,fontUnit:fsU(),listHeight:list.clientHeight,listScroll:list.scrollHeight,
          resizable:await appWindow.isResizable(),scrollOk,
          inView:top>=0&&footer.height>0&&footer.bottom<=innerHeight&&settings.height>0&&settings.bottom<=innerHeight};
      })()`, true);
      const margin = docked ? Math.round(10 * s.sf) : 0;
      const { position, size } = s.area;
      const fits = s.pos.x >= position.x - margin && s.pos.x + s.size.width <= position.x + size.width + margin &&
        s.pos.y >= position.y && s.pos.y + s.size.height <= position.y + size.height;
      check(label, fits && s.inView && s.scrollOk && !s.resizable);
      console.log('    ' + JSON.stringify(s));
    };
    await evalJs(`(async()=>{const {position,size}= (await T.window.currentMonitor()).workArea;
      markSelfMove(FORM_SELF_MOVE_MS);
      await appWindow.setPosition(new T.dpi.PhysicalPosition(position.x+40,position.y+Math.round(size.height/3)));
    })()`, true);
    for (const layout of ['single', 'double', 'thumbnail']) {
      await evalJs(`(async()=>{CFG.fontScale='lg';CFG.panelLayout='${layout}';applyFontScale();render();
        await applyForm('panel',false,true);})()`, true);
      await verifyWorkArea(`大字号 ${layout} 工作区、底栏与列表`);
    }
    await evalJs(`(async()=>{CFG.fontScale='xl';applyFontScale();await applyForm('panel',false,true);})()`, true);
    await verifyWorkArea('特大字号 工作区、底栏与列表');
    for (const side of ['right', 'left']) {
      await evalJs(`(async()=>{const info=await monitorInfo();
        await dockEnter('${side}',{y:info.bottom-100});await dockLayout(true);})()`, true);
      check(`贴边 ${side} 已展开`, await evalJs(`document.body.classList.contains('dock-open')`));
      await verifyWorkArea(`贴边 ${side} 展开避开任务栏`, true);
      await evalJs(`dockExit(false)`, true);
      await verifyWorkArea(`贴边 ${side} 解除后避开任务栏`);
    }

  } catch (e) {
    console.log('  FAIL  脚本异常 · ' + e.message);
    fails++;
  } finally {
    try {
      await evalJs(`(async()=>{
        const state=window.__vState;
        if(!state) return;
        try{
          if(DOCK.on) await dockExit(true);
          markSelfMove(FORM_SELF_MOVE_MS);
          await appWindow.setPosition(new T.dpi.PhysicalPosition(state.pos.x,state.pos.y));
          Object.assign(CFG,JSON.parse(state.cfg));applyTheme();applyFontScale();
          await applyForm(state.form,false,true);
        }finally{
          applyChannelsPush=state.push;refresh=state.refresh;delete window.__vState;
        }
        await refresh();
      })()`, true);
      check('现场还原', true);
    } catch (e) {
      check('现场还原', false, e.message);
    }
  }
  ws.close();
  console.log(fails ? `✗ ${fails} 项失败` : '✓ 固定布局真机校验通过');
  process.exit(fails ? 1 : 0);
})().catch((e) => { console.error(e.message); process.exit(1); });
