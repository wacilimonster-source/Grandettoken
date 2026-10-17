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

// ── 模型页的桩数据(形状照 Rust 侧的 ClientState / CatalogView,字段名 camelCase)──
const SAMPLE_INJECT = {
  clients: [
    { client: 'trae', short: 'T', label: 'Trae', installed: true, running: false, process: 'Trae CN.exe',
      roots: ['~/.trae'], files: ['~/.trae/state.vscdb'], models: [], selected: null, learnedPrefix: null, note: null },
    { client: 'workbuddy', short: 'WB', label: 'WorkBuddy', installed: true, running: false, process: 'WorkBuddy.exe',
      roots: ['~/.workbuddy'], files: ['~/.workbuddy/models.json'], models: [{ id: 'glm-5.3', source: '4sapi' }],
      selected: null, learnedPrefix: 'custom-local:', note: null },
    { client: 'workbuddy-ai', short: 'WBA', label: 'WorkBuddy AI', installed: false, running: false, process: 'WorkBuddy.exe',
      roots: [], files: [], models: [], selected: null, learnedPrefix: null, note: null },
    { client: 'zcode', short: 'ZC', label: 'ZCode', installed: true, running: false, process: 'ZCode.exe',
      roots: ['~/.zcode/v2'], files: ['~/.zcode/v2/config.json'], models: [], selected: null, learnedPrefix: null, note: null },
    // DSH:装好、**正在运行** —— 走的就是「运行中也能写」那条分支
    { client: 'dsh', short: 'DSH', label: 'DeepSeek Harness', installed: true, running: true, process: 'DeepSeek Harness.exe',
      roots: ['~/.dsh'], files: ['~/.dsh/profiles/desktop/cordis.patch.yml', '~/.dsh/.credentials.yaml'],
      models: [], selected: 'deepseek-v4.1-flash', learnedPrefix: null, note: '边跑边写' },
  ],
  log: [],
};

const SAMPLE_CATALOG = {
  source: '4sapi', sourceName: '4SAPI',
  sources: [
    { id: '4sapi', name: '4SAPI' },
    { id: 'hapi', name: 'Hapi' },
    { id: 'opencode-go', name: 'OpenCode Go' },
  ],
  hasPrice: true, listPublic: false, host: '4sapi.com', baseUrl: 'https://4sapi.com/v1',
  version: '1', fetchedAt: Math.floor(Date.now() / 1000), usableAt: Math.floor(Date.now() / 1000),
  hasKey: true, group: 'default', groups: [], total: 2, chatCount: 2, usableCount: 2,
  rows: [
    { id: 'claude-sonnet-5', description: 'Claude', tags: ['多模态', '1M'], context: '1M', vendor: 'Anthropic',
      quotaType: 0, ratio: 1, completionRatio: 1, cacheRatio: null, modelPrice: null, tiered: false,
      billingExpr: null, groups: [], endpoints: ['openai'], usable: true, chat: true,
      price: { input: 3, output: 15, perCall: null, group: 'default', groupRatio: 1, basisGroup: null, tiered: false } },
    { id: 'glm-5.3', description: 'GLM', tags: ['文本'], context: '200K', vendor: 'Zhipu',
      quotaType: 0, ratio: 1, completionRatio: 1, cacheRatio: null, modelPrice: null, tiered: false,
      billingExpr: null, groups: [], endpoints: ['openai'], usable: true, chat: true,
      price: { input: 1, output: 4, perCall: null, group: 'default', groupRatio: 1, basisGroup: null, tiered: false } },
  ],
  error: null,
};

const invokeCalls = [];
const invoke = async (cmd, args) => {
  invokeCalls.push(cmd);
  if (cmd === 'get_config') return JSON.parse(JSON.stringify(CONFIG));
  if (cmd === 'get_channels') return JSON.parse(JSON.stringify(SAMPLE_CHANNELS));
  if (cmd === 'get_series') return [];
  if (cmd === 'inject_state') return JSON.parse(JSON.stringify(SAMPLE_INJECT));
  if (cmd === 'get_model_catalog') return JSON.parse(JSON.stringify(SAMPLE_CATALOG));
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

// ── 模型页:真渲染(2026-10-17 补)──
// 为什么补:v0.1.38 里 `mdlRowHtml` 少了一行 `const on = done(id, r.id);`,而行渲染还在用 `on`,
// 于是**每渲染一行抛一次 ReferenceError**,被 renderModels 的 try/catch 兜成界面上的
// 「模型页渲染出错:on is not defined」。老探针只加载页面、不驱动模型页(inject_state 返回 null),
// 所以它照样报"通过" —— 这条就是那个缺口。
// 现在:塞进 5 个客户端(DSH 标成"正在运行")与一份价目表,真跑 renderModels / renderModelSheet,
// 再用**界面自己会显示的那句话**做强断言。
function probeModels() {
  const dump = vm.runInContext(
    `(() => {
       MDL.clients = ${JSON.stringify(SAMPLE_INJECT.clients)};
       MDL.sources = ${JSON.stringify(SAMPLE_CATALOG.sources)};
       MDL.log = [];
       MDL.views['4sapi'] = ${JSON.stringify(SAMPLE_CATALOG)};
       MDL.src = '4sapi';
       MDL.q = '';
       renderModels();
       const meta = document.getElementById('mdlMeta').innerHTML || '';
       const list = document.getElementById('mdlList').innerHTML || '';
       // 客户端详情弹层(DSH 那行文案走的是独立分支)
       MDL.sheet = { kind: 'client', client: 'dsh' };
       renderModelSheet();
       const clientSheet = document.getElementById('mdlOvIn').innerHTML || '';
       // 写入预告弹层(明文密钥提示 + DSH 专属说明)
       MDL.sheet = { kind: 'write', client: 'dsh', model: 'claude-sonnet-5', sourceName: '4SAPI',
                     previews: [{ client: 'dsh', label: 'DeepSeek Harness · desktop',
                                  files: ['~/.dsh/profiles/desktop/cordis.patch.yml', '~/.dsh/.credentials.yaml'],
                                  action: '在 llm-pi-ai 里新增 route「4sapi」与这条模型', changed: true,
                                  writesSecret: true, writesSelection: false, configured: false,
                                  removal: null, blocked: null, notes: ['落点:…'] }] };
       renderModelSheet();
       const writeSheet = document.getElementById('mdlOvIn').innerHTML || '';
       // 已配置(带"取消写入")+ 被挡(运行中)两个分支 —— v0.1.38 改过这里的文案
       MDL.sheet = { kind: 'write', client: 'dsh', model: 'claude-sonnet-5', sourceName: '4SAPI',
                     previews: [{ client: 'dsh', label: 'DeepSeek Harness · desktop',
                                  files: ['~/.dsh/.credentials.yaml'],
                                  action: '已配置,内容和这次要写的一致,无需改动', changed: false,
                                  writesSecret: true, writesSelection: false, configured: true,
                                  removal: { what: 'route「4sapi」下的「claude-sonnet-5」', clearsSelection: false, count: 1 },
                                  blocked: null, notes: [] }] };
       renderModelSheet();
       const removalSheet = document.getElementById('mdlOvIn').innerHTML || '';
       MDL.sheet = { kind: 'write', client: 'trae', model: 'claude-sonnet-5', sourceName: '4SAPI',
                     previews: [{ client: 'trae', label: 'Trae CN', files: ['x.vscdb'],
                                  action: '新增', changed: true, writesSecret: true, writesSelection: false,
                                  configured: false, removal: null, blocked: 'Trae CN 正在运行 —— 先退出它', notes: [] }] };
       renderModelSheet();
       const blockedSheet = document.getElementById('mdlOvIn').innerHTML || '';
       MDL.sheet = null;
       return JSON.stringify({ meta, list, clientSheet, writeSheet, removalSheet, blockedSheet });
     })()`,
    ctx
  );
  const { meta, list, clientSheet, writeSheet, removalSheet, blockedSheet } = JSON.parse(dump);
  if (/渲染出错/.test(meta) || /渲染出错/.test(list)) {
    return '模型页显示了「渲染出错」:' + (meta + list).replace(/<[^>]+>/g, ' ').trim().slice(0, 200);
  }
  const rowCount = (list.match(/class="mrow/g) || []).length;
  // 只数按钮自己:`class="cdot` 会把容器 `class="cdots"` 也数进去(踩过)
  const dots = (list.match(/data-act="mdlcli"/g) || []).length;
  if (rowCount !== 2) {
    return `该渲染 2 行,实得 ${rowCount}｜${list.replace(/</g, '‹').slice(0, 500)}`;
  }
  if (dots !== rowCount * 5) {
    return `每行该 5 个客户端按钮(${rowCount} 行 → ${rowCount * 5} 个),实得 ${dots}｜` +
      list.replace(/</g, '‹').slice(0, 700);
  }
  if (!/data-c="dsh"/.test(list)) return '模型行里没有 DSH 按钮';
  if (!/运行中也能写/.test(list)) return 'DSH 按钮的提示里没有「运行中也能写」';
  if (!/不用退出/.test(clientSheet)) return 'DSH 客户端详情里没有「不用退出」:' + clientSheet.slice(0, 160);
  if (!/credentials\.yaml/.test(writeSheet)) return 'DSH 的写入预告里没写清凭据文件落点';
  // DSH 的「选中项不写」由它自己的说明负责,通用的那句「挂件会记住口径」不该再出现
  if (/挂件会记住口径/.test(writeSheet) || /挂件会记住口径/.test(removalSheet)) {
    return 'DSH 的预告里混进了通用文案「挂件会记住口径」(它是只读不写的)';
  }
  if (!/取消写入/.test(removalSheet)) return '已配置时没给「取消写入」入口';
  if (!/正在运行|重查/.test(blockedSheet)) return '被挡(目标在跑)时没给出重查引导';
  console.log('模型页渲染 ok:' + dots + ' 个客户端按钮(含 DSH)、详情 / 预告 / 已配置 / 被挡四个弹层都出得来');
  return '';
}

process.on('unhandledRejection', (e) => logs.push(['UNHANDLED', String(e && e.stack || e)]));

try {
  vm.runInContext(src, ctx, { filename: 'app.js' });
} catch (e) {
  logs.push(['THROWN', String(e && e.stack || e)]);
}

// app.js 末尾是 async IIFE,等它跑完
setTimeout(() => {
  // 模型页:真渲染一遍(见上面 probeModels 的说明)
  let modelFail = '';
  try {
    modelFail = probeModels();
  } catch (e) {
    modelFail = 'probeModels 自己抛异常: ' + String((e && e.stack) || e);
  }
  console.log('=== invoke calls (in order) ===');
  console.log(invokeCalls.length ? invokeCalls.join(', ') : '(none)');
  console.log('');
  console.log('=== final body.className ===');
  console.log(JSON.stringify(body.className));
  console.log('');
  console.log('=== 模型页 ===');
  console.log(modelFail ? 'FAIL: ' + modelFail : 'ok');
  console.log('');
  console.log('=== console output ===');
  if (!logs.length) console.log('(none)');
  logs.forEach(([lv, msg]) => console.log('[' + lv + '] ' + msg));
  // app.js 会注册 setInterval(贴边折叠等),不显式退出进程会一直挂着。
  // 退出码是这个探针唯一的机器可读信号,所以 app.js 自己打出的 [ERROR] 也算失败:
  // 以前的判断只看 THROWN/UNHANDLED,一次"渲染抛错被 catch 住,只 console.error"
  // 的运行会以 0 退出,看起来是通过。
  // 模型页那条同理:它正是"渲染抛错被 catch 住"的那类故障,必须单独判。
  process.exit(
    modelFail || logs.some(([lv]) => lv === 'THROWN' || lv === 'UNHANDLED' || lv === 'ERROR') ? 1 : 0
  );
}, 400);
