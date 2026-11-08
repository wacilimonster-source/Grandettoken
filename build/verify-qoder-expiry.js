// 验证「Qoder 逐包到期」:CDP 连真机。断言:
// ① 渠道卡有「登录网页版」——2026-11-08 起国际版与国内版都有,别的渠道没有;
//    且入口常驻折叠体**外**的 .ccquick(原来藏进 .ccbody,用户找不到);
// ② 点它开 qoder-login 窗;
// ③ 无会话时明细卡的「逐笔到期明细」引导行如实出现;④ 有会话时 expiring 真数据(若用户已登录)。
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
    console.log('=== Qoder 逐包到期 ===');
    await evalJs(`(async () => { await loadCheckin(); })()`, true);
    await evalJs(`openManage()`);
    await sleep(300);
    const card = JSON.parse(await evalJs(`JSON.stringify({
      q: !!document.querySelector('#chList .ccard[data-id="qoder"]'),
      qcn: !!document.querySelector('#chList .ccard[data-id="qoder-cn"]'),
      btn: !!document.querySelector('#chList .ccard[data-id="qoder"] [data-act="qoderweblogin"]'),
      btnCn: !!document.querySelector('#chList .ccard[data-id="qoder-cn"] [data-act="qoderweblogin"]'),
      // 入口必须常驻在 .ccbody 之外(折叠体默认 display:none,藏进去等于没有)
      outside: !!document.querySelector('#chList .ccard[data-id="qoder"] .ccquick [data-act="qoderweblogin"]'),
      other: !!document.querySelector('#chList .ccard:not([data-id="qoder"]):not([data-id="qoder-cn"]) [data-act="qoderweblogin"]'),
      why: !!document.querySelector('#chList [data-why="qoderweb"][data-for]'),
    })`));
    check('qoder 渠道卡在场且有「登录网页版」', card.q && card.btn);
    check('qoder-cn 渠道卡也有「登录网页版」', card.qcn && card.btnCn);
    check('入口在折叠体外的 .ccquick 常驻条里', card.outside);
    check('别的渠道没有该入口', !card.other);
    check('「?」说明已水合', card.why);

    // 点按钮:开 qoder-login 窗(真窗口;无头环境里窗口创建仍会成功)
    await evalJs(`document.querySelector('#chList .ccard[data-id="qoder"] [data-act="qoderweblogin"]').click()`);
    await sleep(800);
    const win = JSON.parse(await evalJs(`JSON.stringify({
      logging: QODER_LOGGING,
      toast: (document.querySelector('.toast') || {}).textContent || '',
    })`));
    check('点登录后进入登录中状态', win.logging === true, JSON.stringify(win));

    // 无会话 / 会话无效时的降级文案(取数一轮后看 extra)。
    // 引导行只在**桌面凭据有效**的 Qoder 区上写(fetch.rs:valid 才进逐包链),
    // 本机哪个区有效就断言哪个区 —— 两个区都是合法现场(2026-11-08 起国内版同链)。
    const extra = JSON.parse(await evalJs(`(async () => {
      const vs = await invoke("get_channels");
      const qs = vs.filter((c) => c.id === "qoder" || c.id === "qoder-cn");
      const q = qs.find((c) => c.valid) || qs[0] || {};
      return JSON.stringify({ which: q.id, valid: q.valid,
        extra: q.extra || [], expiryComplete: q.expiryComplete, expiring: (q.expiring || []).length });
    })()`, true));
    const line = (extra.extra.find((x) => x[0] === "逐笔到期明细") || [])[1] || "(无该行)";
    const honest = /需网页登录|未登录|接口未提供|按平台返回|结构还没识别/.test(line);
    // 桌面凭据都无效的区不进逐包链,行缺席也是如实 —— 只在有效区上要求行存在
    const check1 = extra.valid ? honest || extra.expiryComplete === true : line === "(无该行)" || honest;
    check(`「逐笔到期明细」行如实(${extra.which} · 当前:${line})`, check1, line);
    console.log(`  info  区=${extra.which} valid=${extra.valid} expiring=${extra.expiring} 条 · expiryComplete=${extra.expiryComplete}`);
  } catch (e) {
    console.log('  FAIL  脚本异常 · ' + e.message);
    fails++;
  }
  ws.close();
  console.log(fails ? `✗ ${fails} 项失败` : '✓ Qoder 逐包到期真机校验通过');
  process.exit(fails ? 1 : 0);
})().catch((e) => { console.error(e.message); process.exit(1); });
