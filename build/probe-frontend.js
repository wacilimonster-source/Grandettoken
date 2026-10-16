// 在 Node 里用假 DOM 跑 app.js,看它最终把 body 的 class 设成了什么。
// 比重编译一次 Rust 快得多,而且能直接看到异常。
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const src = fs.readFileSync(path.join(__dirname, '..', 'app', 'src', 'app.js'), 'utf8');

const logs = [];

// 假 DOM 的 classList 必须是真集合:以前对象字面量里第 28 行又把 style/classList 各写了
// 一遍,后一份(contains 恒 false)整体覆盖了上面那份认真实现的,导致探针看到的类名状态
// 永远是空、app.js 里所有 classList.contains(...) 分支都被迫走另一侧 —— 探针报出的
// "故障/通过"两个方向都不可信(报告 G-4)。
function makeClassList(owner) {
  const s = new Set();
  const sync = () => { if (owner) owner.className = Array.from(s).join(' '); };
  return {
    _s: s,
    add: (c) => { s.add(c); sync(); },
    remove: (c) => { s.delete(c); sync(); },
    contains: (c) => s.has(c),
    toggle(c, f) {
      if (f === undefined) { s.has(c) ? s.delete(c) : s.add(c); } else { f ? s.add(c) : s.delete(c); }
      sync();
    },
  };
}

function makeEl(id) {
  const el = {
    id, innerHTML: '', textContent: '', value: '', dataset: {},
    style: { setProperty() {}, removeProperty() {}, getPropertyValue: () => '' },
    addEventListener() {}, appendChild() {}, after() {}, remove() {}, focus() {},
    setAttribute() {}, getAttribute: () => null, matches: () => false,
    getBoundingClientRect: () => ({ width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0 }),
    querySelector: () => makeEl('q'),
    querySelectorAll: () => [],
    closest: () => null,
  };
  const cl = makeClassList(el);
  el.classList = cl;
  // className 与 classList 必须互通(真 DOM 就是这样):以前探针把 className 当普通
  // 字符串,而 app.js 全靠 classList 增删,于是"最终类名"永远读不出来。
  let cls = '';
  Object.defineProperty(el, 'className', {
    get: () => cls,
    set: (v) => {
      cls = String(v || '');
      cl._s.clear();
      cls.split(/\s+/).filter(Boolean).forEach((c) => cl._s.add(c));
    },
  });
  return el;
}

const els = {};
const getEl = (id) => (els[id] || (els[id] = makeEl(id)));

const body = makeEl('body');
body.className = 'form-panel';

const documentMock = {
  body,
  // app.js 的 applyFontScale 往根元素写 --u 自定义属性;缺 documentElement 会让探针
  // 在 init 第一步就抛异常,后面全部断言都白跑。
  documentElement: makeEl('html'),
  getElementById: getEl,
  querySelector: () => null,
  querySelectorAll: () => [],
  createElement: (t) => makeEl(t),
  addEventListener() {},
};

const SAMPLE_CHANNELS = [
  { id: '4sapi', name: '4SAPI', short: '4S', color: '#e8622c', kind: 'amount', unstable: false,
    hasKey: false, valid: false, remaining: null, used: null, total: null, unit: 'CNY',
    windows: [], extra: [], error: null, limited: false, day: null, week: null, month: null,
    estimated: false, stale: false, updatedAt: 0 },
];

const CONFIG = {
  activeIntervalSec: 60, idleIntervalSec: 300, backoffIntervalSec: 900,
  warnPercent: 40, critPercent: 15, notify: true, autostart: false,
  collapseOnBlur: false, form: 'panel', sort: 'percent',
};

const invokeCalls = [];
const invoke = async (cmd, args) => {
  invokeCalls.push(cmd);
  if (cmd === 'get_config') return JSON.parse(JSON.stringify(CONFIG));
  if (cmd === 'get_channels') return JSON.parse(JSON.stringify(SAMPLE_CHANNELS));
  if (cmd === 'get_series') return [];
  return null;
};

const windowMock = {
  __TAURI__: {
    core: { invoke },
    event: { listen: async () => {}, emit: async () => {} },
    // 与 app.js 现在用的 API 对齐:Tauri 2 把尺寸类型放在 dpi 命名空间,
    // 窗口对象上还有 setMinSize/setMaxSize/outerPosition/scaleFactor/onMoved/
    // isAlwaysOnTop 等。写错命名空间或漏方法会让这个探针报出假故障。
    window: {
      getCurrentWindow: () => ({
        setSize: async () => {},
        setMinSize: async () => {},
        setMaxSize: async () => {},
        setResizable: async () => {},
        setAlwaysOnTop: async () => {},
        setPosition: async () => {},
        outerPosition: async () => ({ x: 0, y: 0 }),
        outerSize: async () => ({ width: 400, height: 580 }),
        scaleFactor: async () => 1,
        isAlwaysOnTop: async () => false,
        onMoved: async () => () => {},
      }),
      currentMonitor: async () => ({
        position: { x: 0, y: 0 },
        size: { width: 1920, height: 1080 },
        scaleFactor: 1,
      }),
    },
    dpi: {
      LogicalSize: class LogicalSize { constructor(w, h) { this.w = w; this.h = h; } },
      PhysicalPosition: class PhysicalPosition { constructor(x, y) { this.x = x; this.y = y; } },
    },
  },
};

const ctx = {
  window: windowMock,
  document: documentMock,
  console: {
    log: (...a) => logs.push(['log', a.join(' ')]),
    // 级别名 'ERROR' 是文件末尾退出码判断里认的那个字符串,改这里必须一起改
    error: (...a) => logs.push(['ERROR', a.map(String).join(' ')]),
    warn: (...a) => logs.push(['warn', a.map(String).join(' ')]),
  },
  setTimeout, clearTimeout, setInterval, clearInterval,
  // 真浏览器自带的两个:缺了会让 fitPill/renderPillFace 在渲染途中抛异常,
  // 探针就只能报一个"假故障"。
  getComputedStyle: () => ({
    getPropertyValue: (p) => (p === '--u' ? '1' : ''),
    width: '0px',
    height: '0px',
    fontSize: '14px',
  }),
  requestAnimationFrame: (f) => setTimeout(() => f(0), 0),
  Math, Date, JSON, Number, String, Object, Array, Set, Map,
  parseInt, parseFloat, isNaN, Promise, Error,
};
ctx.globalThis = ctx;

vm.createContext(ctx);

process.on('unhandledRejection', (e) => logs.push(['UNHANDLED', String(e && e.stack || e)]));

try {
  vm.runInContext(src, ctx, { filename: 'app.js' });
} catch (e) {
  logs.push(['THROWN', String(e && e.stack || e)]);
}

// app.js 末尾是 async IIFE,等它跑完
setTimeout(() => {
  console.log('=== invoke calls (in order) ===');
  console.log(invokeCalls.length ? invokeCalls.join(', ') : '(none)');
  console.log('');
  console.log('=== final body.className ===');
  console.log(JSON.stringify(body.className));
  console.log('');
  console.log('=== console output ===');
  if (!logs.length) console.log('(none)');
  logs.forEach(([lv, msg]) => console.log('[' + lv + '] ' + msg));
  // app.js 会注册 setInterval(贴边折叠等),不显式退出进程会一直挂着。
  // 退出码是这个探针唯一的机器可读信号,所以 app.js 自己打出的 [ERROR] 也算失败:
  // 以前的判断只看 THROWN/UNHANDLED,一次"渲染抛错被 catch 住,只 console.error"
  // 的运行会以 0 退出,看起来是通过。
  process.exit(logs.some(([lv]) => lv === 'THROWN' || lv === 'UNHANDLED' || lv === 'ERROR') ? 1 : 0);
}, 400);
