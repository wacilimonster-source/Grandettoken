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

    // ① 顶部「设置」已删,标题栏只剩文字按钮(刷新是 v0.1.59 的渠道体检加的)
    const top = JSON.parse(await evalJs(`JSON.stringify({
      btnS: !!document.getElementById('btnS'),
      topButtons: [...document.querySelectorAll('.tbar .tb')].map(b => b.textContent.trim()),
      chip: !!document.getElementById('updChip'),
    })`));
    check('标题栏无 #btnS', top.btnS === false);
    check('标题栏文字按钮原样(刷新/置顶/紧凑/胶囊/隐藏)',
      top.topButtons.join('/') === '刷新/置顶/紧凑/胶囊/隐藏', top.topButtons.join('/'));

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
      const k = document.getElementById('lkKeys');
      k.focus();
      const before = document.activeElement.id;
      k.click();
      await new Promise(r => setTimeout(r, 260));
      const inManage = document.body.classList.contains('view-manage');
      const inFocus = document.activeElement.id;
      const keysHidden = getComputedStyle(k).display === 'none';
      document.getElementById('btnBack').click();
      await new Promise(r => setTimeout(r, 260));
      const outManage = !document.body.classList.contains('view-manage');
      const outFocus = document.activeElement.id;
      return JSON.stringify({ before, inManage, inFocus, keysHidden, outManage, outFocus });
    })()`, true));
    check('点底栏入口进管理页', focusRound.inManage === true);
    check('进管理页后入口按设计隐藏', focusRound.keysHidden === true);
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

    // ⑤ 管理页里底栏入口确实隐藏(不出现"进去以后还能点同一个入口"的二义)
    const hidden = await evalJs(`(async () => {
      openManage();
      await new Promise(r => setTimeout(r, 200));
      const d = getComputedStyle(document.getElementById('lkKeys')).display;
      closeManage();
      await new Promise(r => setTimeout(r, 200));
      return d;
    })()`, true);
    check('管理页内 #lkKeys 隐藏', hidden === 'none', hidden);

    // ⑥ 设置页内部结构未被波及
    const tabs = JSON.parse(await evalJs(`JSON.stringify([...document.querySelectorAll('#mTabs button')].map(b => b.dataset.p))`));
    check('设置页 7 个页签原样(含签到)',
      tabs.length === 7 && tabs.join('/') === 'ch/ck/mdl/disp/run/vault/about', tabs.join('/'));

    console.log(fails ? `\n${fails} FAILED` : '\nALL PASS');
  } catch (e) {
    console.error('verify failed:', e.message || e);
    fails++;
  } finally {
    try { ws.close(); } catch {}
    process.exit(fails ? 1 : 0);
  }
})().catch((e) => { console.error('verify failed:', e.message || e); process.exit(1); });
