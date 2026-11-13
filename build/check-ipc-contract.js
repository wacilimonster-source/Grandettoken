/**
 * IPC 契约门:前端 invoke("xxx") 必须能在 Rust 侧找到定义与注册。
 *
 * 为什么要有这道门:`jsdom-smoke.js` 里的 invoke 桩对未登记命令一律 `return null`,
 * 命令名拼错 / 忘记注册 / 注册项被误删时,前端冒烟照样全绿。2026-11-11 的冗余审计
 * 靠人工做了一次「IPC 三角」比对,但那是**一次性**的,改完代码就失效。
 * 这里把同一比对变成每次提交都能跑的零依赖门禁。
 *
 * 口径:
 *   - 期望值**一律从源码实时提取**,不写死命令表(写死的表本身会腐烂);
 *   - 「调用未定义 / 调用未注册 / 注册无定义」= 失败(exit 1);
 *   - 「注册但前端未调用」= 只报告不失败(可能是后端内部或预留入口,见冗余审计 A2);
 *   - 存在非字面量 invoke 时失败:静态契约覆盖不到动态调用,必须改成字面量或显式复核。
 *
 * 用法:node build/check-ipc-contract.js
 */
const fs = require("fs");
const path = require("path");

const root = path.resolve(__dirname, "..");
const frontPaths = [
  path.join(root, "app/src/app.js"),
  // 重置顾问拆成了独立模块，命令调用必须一起纳入三方比对。
  path.join(root, "app/src/codex-reset.js"),
];
const front = frontPaths.map((p) => fs.readFileSync(p, "utf8")).join("\n");
const rustDir = path.join(root, "app/src-tauri/src");

// ── 1. 前端调用点 ──
const invokeAll = [...front.matchAll(/\binvoke\s*\(/g)];
const calls = new Set();
const dynamic = [];
for (const m of invokeAll) {
  const tail = front.slice(m.index, m.index + 120);
  const lit = /^\s*invoke\s*\(\s*["']([A-Za-z0-9_]+)["']/.exec(tail);
  if (lit) calls.add(lit[1]);
  else dynamic.push(front.slice(m.index, m.index + 60).split("\n")[0]);
}

// ── 2. Rust 命令定义(#Tauri 宏标注的 fn) ──
const defined = new Map(); // name -> file
const registered = new Map(); // name -> raw entry
for (const f of fs.readdirSync(rustDir).filter((n) => n.endsWith(".rs"))) {
  const src = fs.readFileSync(path.join(rustDir, f), "utf8");
  for (const m of src.matchAll(/#\[tauri::command\][\s\S]{0,300}?\b(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)\s*\(/g)) {
    defined.set(m[1], f);
  }
  const gh = /generate_handler!\s*\[([\s\S]*?)\]/.exec(src);
  if (!gh) continue;
  // 按行解析(文件是 CRLF,必须 split /\r?\n/):注册表里每行一个命令,
  // 注释行可能夹在两项之间。曾经直接 split(",") 把
  // "// 说明文字\r\n key_source" 当成一整项丢掉,导致 key_source 被误报为「未注册」——
  // 门禁自己的假阳性比没有门禁更糟,这一步必须逐行剥注释再取标识符。
  for (const ln of gh[1].split(/\r?\n/)) {
    const entry = ln.replace(/\/\/[^\r\n]*/, "").trim().replace(/,$/, "").trim();
    if (!entry) continue;
    const name = entry.split("::").pop().trim();
    if (/^[A-Za-z0-9_]+$/.test(name)) registered.set(name, entry);
  }
}

/** 对给定调用集合做全量判定。抽成函数是为了能拿「哨兵命令」自检判定逻辑本身。 */
function problemsFor(callSet) {
  const p = [];
  // 期望值为空时必须判失败:否则源码格式一变(宏改名、提取规则失效),
  // 门禁会退化成「零调用 = 零问题」的永久绿灯。
  if (!registered.size) p.push("没找到 generate_handler! 注册表 —— 提取规则可能已失效,门禁失去意义");
  if (!defined.size) p.push("没找到任何 #[tauri::command] 定义 —— 提取规则可能已失效,门禁失去意义");
  if (dynamic.length) {
    p.push(`存在 ${dynamic.length} 处非字面量 invoke,静态契约覆盖不到:\n    ` + dynamic.slice(0, 5).join("\n    "));
  }
  for (const c of [...callSet].sort()) {
    if (!defined.has(c)) p.push(`前端调用 ${c}():Rust 侧没有 #[tauri::command] 定义`);
    else if (!registered.has(c)) p.push(`前端调用 ${c}():有定义(${defined.get(c)})但未注册到 generate_handler!`);
  }
  for (const r of [...registered.keys()].sort()) {
    if (!defined.has(r)) p.push(`注册项 ${r}:找不到 #[tauri::command] 定义`);
  }
  return p;
}

// 双向验证:光「当前源码全绿」说明不了门禁是活的。塞一个必然不存在的命令,
// 判定逻辑必须报错;否则说明规则被改坏(例如把缺项降级成警告),整道门禁就没有意义。
// 这条纪律来自 PROGRESS.md 里 scan-secrets.ps1 的教训:单向「变绿了」不算验证。
const sentinelProblems = problemsFor(new Set(["__contract_sentinel__"]));
if (!sentinelProblems.some((p) => p.includes("__contract_sentinel__"))) {
  console.error("IPC 契约自检 FAIL:注入未知命令却没被判为失败 —— 门禁已失效,请修判定逻辑");
  process.exit(2);
}

const problems = problemsFor(calls);
const unused = [...registered.keys()].filter((r) => !calls.has(r)).sort();
console.log(`IPC 契约:前端 ${invokeAll.length} 处 invoke / ${calls.size} 个唯一命令;Rust ${defined.size} 个定义 / ${registered.size} 个注册项`);
console.log(`  调用未定义或未注册: ${[...calls].filter((c) => !defined.has(c) || !registered.has(c)).length}`);
console.log(`  注册但前端未调用(仅提示,不算失败): ${unused.length}${unused.length ? " -> " + unused.join(", ") : ""}`);
if (problems.length) {
  console.error("IPC 契约 FAIL:\n  - " + problems.join("\n  - "));
  process.exit(1);
}
console.log("IPC 契约 PASS");
