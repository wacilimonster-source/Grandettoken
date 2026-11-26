#!/usr/bin/env node
/**
 * app-*.js 拆分完整性门禁
 *
 * 检查项(见 docs/research/appjs-split-plan-2026-11-22.md §6):
 *   1. index.html 中的 app-*.js 脚本清单与磁盘上的 app-*.js 一一对应,
 *      且 app-core.js 排第一、app-boot.js 排最后;
 *   2. 每个文件可单独通过 new vm.Script 编译(防止切在函数或语句中间);
 *   3. 跨文件顶层声明名字唯一(function / async function / const / let / var / class,含简单解构);
 *   4. 按 index.html 顺序逐文件加载执行时,加载期代码不因跨文件未提升而抛 ReferenceError。
 *
 * 双向验证:`--self-test` 注入 6 类坏样本,必须全部判红。
 */
const fs = require("fs");
const path = require("path");
const vm = require("vm");
const { DEFAULT_SRC, appFileNames, bootstrapFiles } = require("./lib/app-sources");

function extractTopLevelNames(code) {
  // Strip comments and string literals while tracking brace depth so we only inspect depth === 0
  const names = [];
  const lines = code.split(/\r?\n/);
  let depth = 0;
  let inBlockComment = false;

  for (let lineIdx = 0; lineIdx < lines.length; lineIdx++) {
    const rawLine = lines[lineIdx];
    if (depth === 0 && !inBlockComment) {
      const mFn = /^(?:async\s+)?function\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*\(/.exec(rawLine);
      if (mFn) names.push({ name: mFn[1], line: lineIdx + 1 });
      const mCls = /^class\s+([A-Za-z_$][A-Za-z0-9_$]*)\b/.exec(rawLine);
      if (mCls) names.push({ name: mCls[1], line: lineIdx + 1 });
      const mVar = /^(?:const|let|var)\s+(.+)/.exec(rawLine);
      if (mVar) {
        const rest = mVar[1];
        if (rest.startsWith("{") || rest.startsWith("[")) {
          const closeIdx = rest.search(/[}\]]/);
          const inside = closeIdx > 0 ? rest.slice(1, closeIdx) : rest.slice(1);
          for (const part of inside.split(",")) {
            const seg = part.split("=")[0].trim();
            const colonParts = seg.split(":");
            const id = colonParts[colonParts.length - 1].trim().replace(/^\.\.\./, "");
            if (/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(id)) {
              names.push({ name: id, line: lineIdx + 1 });
            }
          }
        } else {
          const mId = /^([A-Za-z_$][A-Za-z0-9_$]*)/.exec(rest);
          if (mId) names.push({ name: mId[1], line: lineIdx + 1 });
        }
      }
    }

    // Advance brace depth across rawLine
    let i = 0;
    const n = rawLine.length;
    while (i < n) {
      if (inBlockComment) {
        const end = rawLine.indexOf("*/", i);
        if (end === -1) break;
        inBlockComment = false;
        i = end + 2;
        continue;
      }
      const c = rawLine[i];
      if (c === "/" && rawLine[i + 1] === "/") break;
      if (c === "/" && rawLine[i + 1] === "*") {
        inBlockComment = true;
        i += 2;
        continue;
      }
      if (c === '"' || c === "'" || c === "`") {
        const q = c;
        i++;
        while (i < n && rawLine[i] !== q) {
          if (rawLine[i] === "\\") i += 2;
          else i++;
        }
        i++;
        continue;
      }
      if (c === "{") depth++;
      else if (c === "}") depth = Math.max(0, depth - 1);
      i++;
    }
  }
  return names;
}

function makeLoadSandbox() {
  const makeEl = () => {
    const el = {
      innerHTML: "",
      textContent: "",
      value: "",
      dataset: {},
      hidden: false,
      disabled: false,
      style: { setProperty() {}, removeProperty() {}, getPropertyValue: () => "" },
      classList: { add() {}, remove() {}, toggle() {}, contains: () => false },
      addEventListener() {},
      removeEventListener() {},
      appendChild() {},
      replaceChildren() {},
      setAttribute() {},
      getAttribute: () => null,
      matches: () => false,
      closest: () => null,
      querySelector: () => makeEl(),
      querySelectorAll: () => [],
      getBoundingClientRect: () => ({ width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0 }),
      getContext: () =>
        new Proxy(
          { canvas: { width: 64, height: 64 }, measureText: () => ({ width: 10 }) },
          { get: (t, k) => (k in t ? t[k] : () => {}) }
        ),
      toDataURL: () => "data:image/png;base64,",
    };
    return el;
  };
  const doc = {
    body: makeEl(),
    documentElement: makeEl(),
    head: makeEl(),
    getElementById: () => makeEl(),
    querySelector: () => makeEl(),
    querySelectorAll: () => [],
    createElement: () => makeEl(),
    addEventListener() {},
  };
  const win = {
    document: doc,
    addEventListener() {},
    matchMedia: () => ({ matches: false, addEventListener() {} }),
    getComputedStyle: () => ({
      getPropertyValue: () => "1",
      width: "0px",
      height: "0px",
      fontSize: "14px",
    }),
    requestAnimationFrame: () => 0,
    setTimeout: () => 0,
    clearTimeout() {},
    setInterval: () => 0,
    clearInterval() {},
    localStorage: { getItem: () => null, setItem() {}, removeItem() {} },
    Notification: class { static requestPermission() {} },
    Image: class { constructor() { this.src = ""; } },
    __TAURI__: {
      core: { invoke: () => new Promise(() => {}) },
      event: { listen: () => new Promise(() => {}), emit: () => new Promise(() => {}) },
      dpi: {
        LogicalSize: class { constructor(w, h) { this.width = w; this.height = h; } },
        PhysicalPosition: class { constructor(x, y) { this.x = x; this.y = y; } },
      },
      window: {
        currentMonitor: async () => null,
        getCurrentWindow: () => new Proxy({}, { get: () => async () => () => {} }),
      },
    },
  };
  const ctx = Object.assign({}, win, {
    window: win,
    globalThis: null,
    console: { log() {}, warn() {}, error() {} },
    Math,
    Date,
    JSON,
    Number,
    String,
    Boolean,
    Object,
    Array,
    Set,
    Map,
    WeakMap,
    RegExp,
    Error,
    Promise,
     Symbol,
    Uint8ClampedArray,
    parseInt,
    parseFloat,
    isNaN,
    isFinite,
    encodeURIComponent,
    decodeURIComponent,
  });
  ctx.globalThis = ctx;
  ctx.window = ctx;
  return vm.createContext(ctx);
}

function checkSplit({ htmlNames, diskNames, files, bootstrap = bootstrapFiles() }) {
  const errors = [];

  // 1. 清单一致与首尾约束
  if (htmlNames[0] !== "app-core.js") {
    errors.push(`index.html 第一个 app 脚本必须是 app-core.js，实际为 ${htmlNames[0] || "<空>"}`);
  }
  if (htmlNames[htmlNames.length - 1] !== "app-boot.js") {
    errors.push(`index.html 最后一个 app 脚本必须是 app-boot.js，实际为 ${htmlNames[htmlNames.length - 1] || "<空>"}`);
  }
  const htmlSet = new Set(htmlNames);
  const diskSet = new Set(diskNames);
  for (const name of htmlNames) {
    if (!diskSet.has(name)) errors.push(`index.html 引用了磁盘上不存在的脚本: ${name}`);
  }
  for (const name of diskNames) {
    if (!htmlSet.has(name)) errors.push(`磁盘上存在未写入 index.html 的孤儿脚本: ${name}`);
  }
  if (htmlNames.length !== htmlSet.size) {
    errors.push("index.html 中存在重复的 app-*.js 脚本标签");
  }

  // 2. 单文件独立编译
  const compiled = [];
  for (const f of [...bootstrap, ...files]) {
    try {
      compiled.push({ name: f.name, script: new vm.Script(f.code, { filename: f.name }) });
    } catch (e) {
      errors.push(`${f.name} 无法单独编译: ${e.message}`);
    }
  }

  // 3. 跨文件顶层声明唯一
  const seen = new Map();
  for (const f of [...bootstrap, ...files]) {
    for (const item of extractTopLevelNames(f.code)) {
      const prev = seen.get(item.name);
      if (prev) {
        errors.push(`跨文件顶层名字冲突 '${item.name}': ${prev.file}:L${prev.line} 与 ${f.name}:L${item.line}`);
      } else {
        seen.set(item.name, { file: f.name, line: item.line });
      }
    }
  }

  // 4. 逐文件按序加载执行(拦截加载期前向调用后面文件的函数)
  if (compiled.length === bootstrap.length + files.length && errors.length === 0) {
    const ctx = makeLoadSandbox();
    for (const c of compiled) {
      try {
        c.script.runInContext(ctx);
      } catch (e) {
        errors.push(`${c.name} 加载期执行抛错: ${e.message}`);
      }
    }
  }

  return { ok: errors.length === 0, errors, topLevelCount: seen.size };
}

function loadCurrentState(srcDir = DEFAULT_SRC) {
  const htmlNames = appFileNames(srcDir);
  const diskNames = fs
    .readdirSync(srcDir)
    .filter((n) => /^app(?:-.+)?\.js$/.test(n))
    .sort();
  const files = htmlNames
    .filter((n) => fs.existsSync(path.join(srcDir, n)))
    .map((n) => ({ name: n, code: fs.readFileSync(path.join(srcDir, n), "utf8") }));
  return { htmlNames, diskNames, files };
}

function selfTest() {
  console.log("app-*.js 拆分完整性门禁 自检:");
  const base = loadCurrentState();
  if (!checkSplit(base).ok) throw new Error("正常样本必须先通过: " + checkSplit(base).errors.join(" | "));
  const cloneFiles = () => base.files.map((f) => ({ ...f }));
  const cases = [
    {
      name: "少一个脚本标签被判红",
      run: () =>
        checkSplit({
          htmlNames: base.htmlNames.slice(0, -1),
          diskNames: base.diskNames,
          files: cloneFiles().slice(0, -1),
        }),
    },
    {
      name: "多一个孤儿文件被判红",
      run: () =>
        checkSplit({
          htmlNames: base.htmlNames,
          diskNames: [...base.diskNames, "app-orphan.js"],
          files: cloneFiles(),
        }),
    },
    {
      name: "app-boot.js 不在最后被判红",
      run: () => {
        const names = [...base.htmlNames];
        const last = names.pop();
        names.splice(1, 0, last);
        return checkSplit({ htmlNames: names, diskNames: base.diskNames, files: cloneFiles() });
      },
    },
    {
      name: "函数中间截断被判红",
      run: () => {
        const files = cloneFiles();
        files[0].code += "\nfunction broken( {\n";
        return checkSplit({ htmlNames: base.htmlNames, diskNames: base.diskNames, files });
      },
    },
    {
      name: "跨文件顶层重名被判红",
      run: () => {
        const files = cloneFiles();
        files[1].code = "let T = null;\n" + files[1].code;
        return checkSplit({ htmlNames: base.htmlNames, diskNames: base.diskNames, files });
      },
    },
    {
      name: "加载期前向调用后面文件函数被判红",
      run: () => {
        const files = cloneFiles();
        files[0].code += "\n__forward_declared_in_later_file__();\n";
        files[1].code += "\nfunction __forward_declared_in_later_file__() {}\n";
        return checkSplit({ htmlNames: base.htmlNames, diskNames: base.diskNames, files });
      },
    },
  ];

  let fail = 0;
  for (const c of cases) {
    const res = c.run();
    if (res.ok) {
      console.error(`  ✗ 自检失败(应判红却通过): ${c.name}`);
      fail++;
    } else {
      console.log(`  ✓ ${c.name}`);
    }
  }
  return fail === 0;
}

function main() {
  if (process.argv.includes("--self-test")) {
    process.exit(selfTest() ? 0 : 1);
  }
  const state = loadCurrentState();
  const res = checkSplit(state);
  if (!res.ok) {
    for (const e of res.errors) console.error("✗ " + e);
    console.error("\napp-*.js 拆分完整性门禁 FAIL");
    process.exit(1);
  }
  console.log(
    `app-*.js 拆分完整性门禁 PASS:${state.files.length} 个脚本标签与磁盘一致,单文件独立编译通过,${res.topLevelCount} 个顶层声明无重名,逐文件加载期零异常`
  );
}

main();
