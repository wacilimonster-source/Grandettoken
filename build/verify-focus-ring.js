// 底栏入口的键盘可达性回归(2026-10-20 设置入口收敛后新增)。
//
// 为什么单独一支:收敛之后底栏「设置与管理」成了**唯一**的全局设置入口,
// 而它原来是 <span>(不可 Tab、不可回车)。所以这条路径必须有断言守着,
// 一旦有人把它改回 span,或把 :focus-visible 规则删掉,这里会直接红。
//
// 验三件事:
//   ① 键盘 Shift+Tab 到它 → :focus-visible 出焦点环;Enter / 空格都能进管理页
//   ② 鼠标点它 → 不出焦点环(焦点环只属于键盘,鼠标点击不该闪一下)
//   ③ 焦点往返:进去落在页头「← 返回」,返回后回到它
//
// 用法:
//   powershell -File build/launch-debug.ps1      # 起应用并开 CDP
//   node build/verify-focus-ring.js [port]
//
// 只读:不写 CFG、不落盘,弹层与编辑态由 closeManage() 自己清理。
const PORT = Number(process.argv[2] || process.env.CDP_PORT || 9222);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  const targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  const pages = targets.filter((x) => x.type === 'page');
  // 与 verify-form.js / shot-panel.js 同一条规矩:只挂应用自己的页面,
  // 9222 被别的 Chromium 占用时不要静默连到无关目标上跑。
  const t = pages.find((x) => /tauri|grandettoken|tokenscope/i.test(`${x.title || ''} ${x.url || ''}`));
  if (!t) {
    console.error(`no app page on port ${PORT}`);
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
  const send = (method, params = {}) => new Promise((res, rej) => {
    const i = ++id;
    pending.set(i, (m) => (m.error ? rej(new Error(`${method}: ${m.error.message}`)) : res(m.result)));
    ws.send(JSON.stringify({ id: i, method, params }));
  });
  const ev = (expression, awaitPromise = false) =>
    send("Runtime.evaluate", { expression, awaitPromise, returnByValue: true }).then((r) => r.result.value);

  const state = () => ev(`JSON.stringify({
    manage: document.body.classList.contains('view-manage'),
    active: document.activeElement.id || document.activeElement.tagName,
    ring: document.activeElement.matches(':focus-visible'),
    outline: getComputedStyle(document.activeElement).outlineWidth,
  })`).then(JSON.parse);

  const key = async (code, vk, modifiers = 0, text) => {
    await send("Input.dispatchKeyEvent", { type: "keyDown", windowsVirtualKeyCode: vk, key: code, code, modifiers, ...(text ? { text, unmodifiedText: text } : {}) });
    await send("Input.dispatchKeyEvent", { type: "keyUp", windowsVirtualKeyCode: vk, key: code, code, modifiers });
    await sleep(220);
  };
  const click = async (sel) => {
    const p = JSON.parse(await ev(`JSON.stringify((() => { const r = document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })())`));
    await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: p.x, y: p.y, button: "none", clickCount: 0 });
    await send("Input.dispatchMouseEvent", { type: "mousePressed", x: p.x, y: p.y, button: "left", clickCount: 1 });
    await send("Input.dispatchMouseEvent", { type: "mouseReleased", x: p.x, y: p.y, button: "left", clickCount: 1 });
    await sleep(320);
  };

  let fails = 0;
  const check = (name, ok, detail) => {
    console.log(`  ${ok ? "PASS" : "FAIL"}  ${name}${detail ? "  · " + detail : ""}`);
    if (!ok) fails++;
  };

  try {
    console.log("=== 底栏入口键盘可达性 ===");
    await ev(`closeManage()`); await sleep(200);

    // ① 键盘:Shift+Tab 从「退出」退到「设置与管理」
    await ev(`document.getElementById('lkQuit').focus()`); await sleep(80);
    await key("Tab", 9, 8); // 8 = Shift
    const kbd = await state();
    check("Shift+Tab 落在 #lkKeys", kbd.active === "lkKeys", kbd.active);
    check("键盘聚焦出焦点环(:focus-visible)", kbd.ring === true, "outline=" + kbd.outline);

    // Enter 走完整按键管线(text 才会触发 button 的默认 click)
    await key("Enter", 13, 0, "\r");
    const enter = await state();
    check("Enter 能进管理页", enter.manage === true);
    check("进页后焦点交给页头返回", enter.active === "btnBack", enter.active);

    // 返回:Esc 走的是 closeManage(),焦点应还回底栏入口
    await key("Escape", 27);
    const back = await state();
    check("Esc 返回后面板可见", back.manage === false);
    check("焦点还给底栏入口", back.active === "lkKeys", back.active);

    // ② 空格同样能激活
    await key(" ", 32, 0, " ");
    const space = await state();
    check("空格能进管理页", space.manage === true);
    await key("Escape", 27);

    // ③ 鼠标路径不该出焦点环 —— 焦点环是键盘的反馈,鼠标点一下闪环是噪音
    await ev(`document.activeElement.blur && document.activeElement.blur()`); await sleep(120);
    await click("#lkKeys");
    const mouseIn = await state();
    check("鼠标点入口能进管理页", mouseIn.manage === true);
    check("鼠标进入时不出焦点环", mouseIn.ring === false, "active=" + mouseIn.active);
    await click("#btnBack");
    const mouseOut = await state();
    check("鼠标点返回能退出", mouseOut.manage === false);
    check("鼠标返回时不出焦点环", mouseOut.ring === false, "active=" + mouseOut.active);

    console.log(fails ? `\n${fails} FAILED` : "\nALL PASS");
  } catch (e) {
    console.error("verify failed:", e.message || e);
    fails++;
  } finally {
    try { await ev(`closeManage()`); } catch {}
    try { ws.close(); } catch {}
    process.exit(fails ? 1 : 0);
  }
})().catch((e) => { console.error("verify failed:", e.message || e); process.exit(1); });
