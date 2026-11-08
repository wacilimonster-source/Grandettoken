// 验证「设置入口收敛为 1 个」:CDP 连真机,断言 DOM/焦点/三档字号无溢出。
// 只读 + 只改 documentElement 上的 --u 临时值(不写 CFG、不落盘),结束时还原。
const PORT = Number(process.argv[2] || process.env.CDP_PORT || 9222);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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

  let fails = 0;
  const check = (name, ok, detail) => {
    console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  · ' + detail : ''}`);
    if (!ok) fails++;
  };

  try {
    console.log('=== 设置入口收敛 ===');

    // ① 顶部「设置」已删;2026-11-03 起顶栏固定四枚入口,不再有 ⋯ 菜单
    const top = JSON.parse(await evalJs(`JSON.stringify({
      btnS: !!document.getElementById('btnS'),
      btnMenu: !!document.getElementById('btnMenu'),
      hmenu: !!document.getElementById('hmenu'),
      btnC: !!document.getElementById('btnC'),
      buttons: ['btnR','btnP','btnH','btnPill'].map(id => {
        const b = document.getElementById(id);
        return b && { id, hidden: b.hidden, text: b.textContent.trim() };
      }),
      chip: !!document.getElementById('updChip'),
      // 2026-11-03 固定布局后,lkReset 已按设计移除,底栏常驻的是「单列/双列」布局切换
      layout: (() => { const b = document.getElementById('lkLayout');
        return b && { hidden: b.hidden, display: getComputedStyle(b).display, text: b.textContent.trim() }; })(),
      resetGone: !document.getElementById('lkReset'),
    })`));
    check('标题栏无 #btnS', top.btnS === false);
    check('顶栏固定刷新/置顶/隐藏/胶囊四枚',
      top.buttons.every((b) => b && !b.hidden),
      JSON.stringify(top.buttons));
    check('顶栏不再有 ⋯ 菜单与「紧凑」按钮',
      !top.btnMenu && !top.hmenu && !top.btnC,
      JSON.stringify({ menu: top.btnMenu, hmenu: top.hmenu, compact: top.btnC }));
    check('底栏「单列/双列」布局切换常驻(旧 lkReset 已移除)',
      !!top.layout && !top.layout.hidden && top.layout.display !== 'none' && /单列|双列/.test(top.layout.text) &&
      top.resetGone,
      JSON.stringify(top.layout));

    // ② 底栏三枚是 <button> 且键盘可达(tabIndex 0、非 disabled)
    const foot = JSON.parse(await evalJs(`JSON.stringify({
      tags: [...document.querySelectorAll('.foot > *')].map(e => e.tagName + (e.id ? '#' + e.id : '')),
      keys: (() => { const k = document.getElementById('lkKeys'); return k && { tag: k.tagName, tab: k.tabIndex, dis: k.disabled, text: k.textContent.trim() }; })(),
      quit: (() => { const k = document.getElementById('lkQuit'); return k && { tag: k.tagName, tab: k.tabIndex, dis: k.disabled }; })(),
      upd: (() => { const k = document.getElementById('lkUpd'); return k && { tag: k.tagName, tab: k.tabIndex }; })(),
      keysStyle: (() => { const k = document.getElementById('lkKeys'); const s = getComputedStyle(k);
        return { bg: s.backgroundColor, border: s.borderTopWidth, font: s.fontSize, color: s.color }; })(),
    })`));
    check('#lkKeys 是 BUTTON', foot.keys.tag === 'BUTTON', foot.keys.tag);
    check('#lkQuit 是 BUTTON', foot.quit.tag === 'BUTTON', foot.quit.tag);
    check('#lkUpd 是 BUTTON', foot.upd.tag === 'BUTTON', foot.upd.tag);
    check('底栏入口可 Tab(不是 -1 / 不是 disabled)', foot.keys.tab === 0 && foot.keys.dis === false, 'tab=' + foot.keys.tab);
    check('按钮外观已归零(无底色/无边框)', foot.keysStyle.bg === 'rgba(0, 0, 0, 0)' && foot.keysStyle.border === '0px',
      `${foot.keysStyle.bg} / ${foot.keysStyle.border}`);

    // ③ 焦点往返:点底栏入口 → 焦点在页头「← 返回」;关掉 → 焦点回底栏入口
    const focusRound = JSON.parse(await evalJs(`(async () => {
      // 前置状态不假设:手动开着设置页时,再点常驻入口是"不重置"的 no-op
      if (document.body.classList.contains('view-manage')) closeManage(false);
      await new Promise(r => setTimeout(r, 120));
      const k = document.getElementById('lkKeys');
      k.focus();
      const before = document.activeElement.id;
      k.click();
      await new Promise(r => setTimeout(r, 260));
      const inManage = document.body.classList.contains('view-manage');
      const inFocus = document.activeElement.id;
      // 入口常驻(2026-11-08):管理页里也固定显示,不再进页就藏
      const keysHidden = getComputedStyle(k).display === 'none';
      document.getElementById('btnBack').click();
      await new Promise(r => setTimeout(r, 260));
      const outManage = !document.body.classList.contains('view-manage');
      const outFocus = document.activeElement.id;
      return JSON.stringify({ before, inManage, inFocus, keysHidden, outManage, outFocus });
    })()`, true));
    check('点底栏入口进管理页', focusRound.inManage === true);
    check('入口常驻:管理页内也固定显示', focusRound.keysHidden === false);
    check('焦点交给页头返回', focusRound.inFocus === 'btnBack', focusRound.inFocus);
    check('返回后焦点还给底栏入口', focusRound.outManage === true && focusRound.outFocus === 'lkKeys', focusRound.outFocus);

    // ④ 三档字号 × 更新角标在场/不在场:标题栏与底栏都不得横向溢出
    const geom = JSON.parse(await evalJs(`(() => {
      const root = document.documentElement;
      const tbar = document.querySelector('.tbar');
      const tt = tbar.querySelector('.tt');
      const chip = document.getElementById('updChip');
      const foot = document.querySelector('.foot');
      const lkUpd = document.getElementById('lkUpd');
      const prevU = root.style.getPropertyValue('--u');
      const prevChip = chip.style.display, prevChipTxt = chip.textContent;
      const prevUpd = lkUpd.style.display, prevUpdTxt = lkUpd.textContent;
      const out = [];
      for (const u of [1, 1.15, 1.3]) {
        root.style.setProperty('--u', String(u));
        for (const withChip of [false, true]) {
          for (const withUpd of [false, true]) {
            chip.style.display = withChip ? '' : 'none';
            chip.textContent = '新版本';          // renderUpdChip() 里最宽的三字档
            lkUpd.style.display = withUpd ? '' : 'none';
            lkUpd.textContent = '有新版本 0.1.43';  // showCard() 的原样文案
            out.push({
              u, withChip, withUpd,
              tbarOver: Math.round(tbar.scrollWidth - tbar.clientWidth),
              titleAvail: Math.round(tt.getBoundingClientRect().width * 10) / 10,
              titleTrunc: tt.scrollWidth > tt.clientWidth + 1,
              footOver: Math.round(foot.scrollWidth - foot.clientWidth),
              footSlack: Math.round(foot.querySelector('.spacer').getBoundingClientRect().width),
            });
          }
        }
      }
      root.style.setProperty('--u', prevU || '1');
      chip.style.display = prevChip; chip.textContent = prevChipTxt;
      lkUpd.style.display = prevUpd; lkUpd.textContent = prevUpdTxt;
      return JSON.stringify(out);
    })()`));
    const worst = geom.filter((g) => g.tbarOver > 0 || g.footOver > 0);
    check('6 种组合下标题栏零溢出', worst.filter((g) => g.tbarOver > 0).length === 0,
      geom.map((g) => `${g.u}${g.withChip ? '+chip' : ''}:${g.tbarOver}`).join(' '));
    check('6 种组合下底栏零溢出', worst.filter((g) => g.footOver > 0).length === 0,
      geom.map((g) => `${g.u}${g.withUpd ? '+upd' : ''}:${g.footOver}`).join(' '));
    const std = geom.find((g) => g.u === 1 && g.withChip && !g.withUpd);
    check('标准字号 + 角标:标题不被截断', std.titleTrunc === false, 'avail=' + std.titleAvail);
    const slack = geom.map((g) => g.footSlack);
    console.log('  底栏弹性空隙(px):', geom.map((g) => `${g.u}${g.withUpd ? '+upd' : ''}=${g.footSlack}`).join(' '));

    // ⑤ 管理页里入口常驻:再点不重置(不把用户正在读的子页抖回索引页)
    const reentry = await evalJs(`(async () => {
      openManage();
      await new Promise(r => setTimeout(r, 200));
      const d = getComputedStyle(document.getElementById('lkKeys')).display;
      // 摆到子页(渠道),点常驻入口后必须仍停在原地
      showTab('ch');
      await new Promise(r => setTimeout(r, 100));
      document.getElementById('lkKeys').click();
      await new Promise(r => setTimeout(r, 100));
      const stayed = document.querySelector('#mTabs [data-p="ch"]').classList.contains('on')
        && !document.body.classList.contains('manage-index');
      closeManage();
      await new Promise(r => setTimeout(r, 200));
      return JSON.stringify({ d, stayed });
    })()`, true);
    const re = JSON.parse(reentry);
    check('管理页内入口常驻显示', re.d !== 'none', re.d);
    check('页内再点入口不重置子页', re.stayed === true, JSON.stringify(re));

    // ⑥ 设置页内部结构未被波及(2026-11-08 起多一个「本地模型网关」分区)
    const tabs = JSON.parse(await evalJs(`JSON.stringify([...document.querySelectorAll('#mTabs button')].map(b => b.dataset.p))`));
    check('设置页 9 个页签原样(含网关/签到/同步)',
      tabs.length === 9 && tabs.join('/') === 'ch/ck/mdl/disp/run/vault/gateway/about/sync', tabs.join('/'));

    console.log(fails ? `\n${fails} FAILED` : '\nALL PASS');
  } catch (e) {
    console.error('verify failed:', e.message || e);
    fails++;
  } finally {
    try { ws.close(); } catch {}
    process.exit(fails ? 1 : 0);
  }
})().catch((e) => { console.error('verify failed:', e.message || e); process.exit(1); });
