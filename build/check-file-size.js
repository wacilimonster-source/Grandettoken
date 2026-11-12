#!/usr/bin/env node
/**
 * 文件规模护栏
 *
 * 为什么需要它:`app/src/app.js` 9176 行、`inject.rs` 7248 行、`src-tauri/lib.rs` 6432 行、
 * `fetch.rs` 5191 行。这四个文件已经是"改一处不知道会影响哪里"的状态,而每一轮
 * 自动化改动都会顺手再往里加几十行 —— 加的时候都合理,累积起来就是不可维护。
 * `AGENTS.md` 2.3 写了「优先拆新模块」,但写了三版都没生效,因为没有机器判定。
 *
 * 规则(棘轮):
 *   - 预算表里的文件不得超过上限;
 *   - **上限只许下调**。清理掉代码后应同步下调,把空间收回去,不许因为"这次要加功能"上调;
 *   - 表外的文件一旦超过 SOFT_LIMIT 也失败,提示先拆模块再加功能。
 *
 * 双向验证:`--self-test` 会构造一个超限文件,必须判红。
 */
const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const SOFT_LIMIT = 1500;

// 上限 = 当前行数 + 约 1% 的应急余量(允许小修补,不允许塞新功能)。
// 清理后请把上限一起调下来 —— 这是唯一能防止"删完再长回去"的机制。
const BUDGETS = {
  "app/src/app.js": 9260,
  "app/core/src/inject.rs": 7320,
  "app/src-tauri/src/lib.rs": 6500,
  "app/core/src/fetch.rs": 5250,
  "app/core/src/store.rs": 3000,
  "app/core/src/workbuddy.rs": 2420,
  "app/core/src/checkin.rs": 2200,
  "app/core/src/providers.rs": 2060,
  "app/core/src/diaglog.rs": 1740,
  "app/src/index.html": 700,
  "app/src/styles.css": 1860,
};

// 不参与判定的路径:构建产物、依赖、测试夹具
const SKIP_DIRS = new Set(["node_modules", "target", "build", "dist", ".git", "docs", "android"]);

function countLines(file) {
  const s = fs.readFileSync(file, "utf8");
  // 与 wc -l 对齐:数换行符,不把文件末尾的空行算成一行
  return s.split("\n").length - (s.endsWith("\n") ? 1 : 0);
}

function walk(dir, out) {
  for (const name of fs.readdirSync(dir)) {
    const p = path.join(dir, name);
    const st = fs.statSync(p);
    if (st.isDirectory()) {
      if (SKIP_DIRS.has(name)) continue;
      walk(p, out);
    } else if (/\.(js|rs|html|css)$/.test(name)) {
      out.push(p);
    }
  }
  return out;
}

function selfTest() {
  const tmp = path.join(ROOT, "build", "__size_probe__.js");
  const cases = [
    { name: "超预算文件被判红", lines: 9261, budget: 9260, wantBad: true },
    { name: "正好等于上限不判红", lines: 9260, budget: 9260, wantBad: false },
    { name: "低于上限不判红", lines: 100, budget: 9260, wantBad: false },
  ];
  let fail = 0;
  for (const c of cases) {
    fs.writeFileSync(tmp, "// x\n".repeat(c.lines), "utf8");
    const got = countLines(tmp) > c.budget;
    if (got !== c.wantBad) {
      console.error(`  ✗ 自检失败:${c.name}`);
      fail++;
    } else {
      console.log(`  ✓ ${c.name}`);
    }
  }
  try { fs.unlinkSync(tmp); } catch {}
  return fail === 0;
}

function main() {
  if (process.argv.includes("--self-test")) {
    console.log("文件规模护栏 自检:");
    process.exit(selfTest() ? 0 : 1);
  }

  const bad = [];
  const notes = [];
  for (const [rel, limit] of Object.entries(BUDGETS)) {
    const abs = path.join(ROOT, rel);
    if (!fs.existsSync(abs)) {
      bad.push(`${rel}  预算表里的文件不存在 —— 删了文件就请一起删掉这条预算`);
      continue;
    }
    const n = countLines(abs);
    const pct = ((n / limit) * 100).toFixed(1);
    if (n > limit) {
      bad.push(`${rel}  ${n} 行 > 上限 ${limit}（${pct}%）`);
    } else {
      notes.push(`${rel}  ${n}/${limit}（${pct}%）`);
    }
  }

  // 表外的文件:超过软上限就要先拆
  const known = new Set(Object.keys(BUDGETS).map((p) => path.join(ROOT, p)));
  const extra = [];
  for (const dir of ["app/src", "app/core/src", "app/src-tauri/src"]) {
    const abs = path.join(ROOT, dir);
    if (!fs.existsSync(abs)) continue;
    for (const f of walk(abs, [])) {
      if (known.has(f)) continue;
      const n = countLines(f);
      if (n > SOFT_LIMIT) extra.push(`${path.relative(ROOT, f)}  ${n} 行 > 软上限 ${SOFT_LIMIT}`);
    }
  }

  if (bad.length || extra.length) {
    for (const b of bad) console.error("✗ " + b);
    for (const e of extra) console.error("✗ " + e);
    console.error("\n文件规模护栏 FAIL。规则见 AGENTS.md 2.3:");
    console.error("  1. 新增功能拆到新模块,不要继续往已超标的文件里堆;");
    console.error("  2. 清理后请把 BUDGETS 的上限**下调**,把空间收回去;");
    console.error("  3. 上调上限需要在本文件注释里写明理由(会被 review 看到)。");
    process.exit(1);
  }

  console.log("文件规模护栏 PASS:");
  for (const n of notes) console.log("  " + n);
}

main();
