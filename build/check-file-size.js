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
//
// 2026-11-13 上调(当天两轮,理由同一条:新增渠道的**接线点拆不出去**,
// 逻辑本体都在新模块里 —— minimax.rs / doubao.rs):
//   - checkin.rs 2200 → 2260:第 5 个签到渠道 MiniMax(表项 / Cred / apply_cred / 分发);
//   - checkin.rs 2260 → 2290、fetch.rs 5250 → 5280、providers.rs 2060 → 2095:
//     第 6 个渠道豆包(WebSession 表项 + 逐动作 body + Cookie 分支),
//     渠道表与统一取数/签到管线是这些文件的本职,不许再从这里长功能以外的行。
const BUDGETS = {
  "app/core/src/inject.rs": 2276, // T-A5 生产代码按客户端拆入 inject_{io,workbuddy,zcode,trae,dsh}.rs。
  "app/core/src/store.rs": 1952, // T-A1 测试模块外移至 store_tests.rs。
  "app/core/src/workbuddy.rs": 1776, // T-A1 测试模块外移至 workbuddy_tests.rs。
  "app/core/src/checkin.rs": 1518, // T-A1 测试模块外移至 checkin_tests.rs。
  "app/core/src/providers.rs": 1505, // T-A1 测试模块外移至 providers_tests.rs。
  "app/core/src/diaglog.rs": 1740,
  "app/src/index.html": 698,
  "app/src/styles.css": 1737,
};

// 分组预算:拆分后单文件受 SOFT_LIMIT 约束,同时整组总行数仍受原上限约束(只许下调)。
const GROUP_BUDGETS = [
  {
    label: "app/src/app*.js",
    dir: "app/src",
    pattern: /^app(?:-.+)?\.js$/,
    limit: 9007,
  },
];

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
  const tmp2 = path.join(ROOT, "build", "__size_probe_2__.js");
  const limit = GROUP_BUDGETS[0].limit;
  const cases = [
    { name: "超预算文件被判红", lines: limit + 1, budget: limit, wantBad: true },
    { name: "正好等于上限不判红", lines: limit, budget: limit, wantBad: false },
    { name: "低于上限不判红", lines: 100, budget: limit, wantBad: false },
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
  const part1 = Math.floor(limit / 2) + 1;
  const part2 = limit - part1 + 1;
  fs.writeFileSync(tmp, "// x\n".repeat(part1), "utf8");
  fs.writeFileSync(tmp2, "// x\n".repeat(part2), "utf8");
  const groupSum = countLines(tmp) + countLines(tmp2);
  if (!(groupSum > limit)) {
    console.error("  ✗ 自检失败:分组预算合计超标被判红");
    fail++;
  } else {
    console.log("  ✓ 分组预算合计超标被判红");
  }
  try { fs.unlinkSync(tmp); } catch {}
  try { fs.unlinkSync(tmp2); } catch {}
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

  for (const g of GROUP_BUDGETS) {
    const absDir = path.join(ROOT, g.dir);
    if (!fs.existsSync(absDir)) {
      bad.push(`${g.label}  分组目录不存在`);
      continue;
    }
    const matched = fs.readdirSync(absDir).filter((name) => g.pattern.test(name)).sort();
    if (!matched.length) {
      bad.push(`${g.label}  未匹配到任何文件`);
      continue;
    }
    const sum = matched.reduce((acc, name) => acc + countLines(path.join(absDir, name)), 0);
    const pct = ((sum / g.limit) * 100).toFixed(1);
    if (sum > g.limit) {
      bad.push(`${g.label}（${matched.length} 个文件合计）  ${sum} 行 > 上限 ${g.limit}（${pct}%）`);
    } else {
      notes.push(`${g.label}（${matched.length} 个文件合计）  ${sum}/${g.limit}（${pct}%）`);
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
