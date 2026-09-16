// 在 Node 里用假 DOM 跑 app.js,看它最终把 body 的 class 设成了什么。
// 比重编译一次 Rust 快得多,而且能直接看到异常。
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const src = fs.readFileSync(path.join(__dirname, '..', 'app', 'src', 'app.js'), 'utf8');

const logs = [];

function makeClassList() {
  const s = new Set();
  return {
    _s: s,
    add: (c) => s.add(c),
    remove: (c) => s.delete(c),
    contains: (c) => s.has(c),
    toggle(c, f) { if (f === undefined) { s.has(c) ? s.delete(c) : s.add(c); } else { f ? s.add(c) : s.delete(c); } },
  };
}

function makeEl(id) {
  const el = {
    id, innerHTML: '', textContent: '', value: '', style: {}, dataset: {},
    classList: makeClassList(),
    addEventListener() {}, appendChild() {}, after() {}, remove() {}, focus() {},
    querySelector: () => makeEl('q'),
    querySelectorAll: () => [],
    closest: () => null,
  };
  return el;
}

const els = {};
const getEl = (id) => (els[id] || (els[id] = makeEl(id)));

const body = makeEl('body');
body.className = 'form-panel';

const documentMock = {
  body,
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
    window: {
      getCurrentWindow: () => ({
        setSize: async () => {},
        setResizable: async () => {},
        setAlwaysOnTop: async () => {},
      }),
      LogicalSize: class LogicalSize { constructor(w, h) { this.w = w; this.h = h; } },
    },
  },
};

const ctx = {
  window: windowMock,
  document: documentMock,
  console: {
    log: (...a) => logs.push(['log', a.join(' ')]),
    error: (...a) => logs.push(['ERROR', a.map(String).join(' ')]),
    warn: (...a) => logs.push(['warn', a.map(String).join(' ')]),
  },
  setTimeout, clearTimeout, Math, Date, JSON, Number, String, Object, Array, Set, Map,
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
}, 400);
