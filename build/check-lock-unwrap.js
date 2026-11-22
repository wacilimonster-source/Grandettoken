#!/usr/bin/env node
/**
 * Rust 生产代码锁中毒棘轮门禁 (T-C1)
 *
 * 规则:
 *   `app/core/src` 与 `app/src-tauri/src` 的生产代码(排除 `*_tests*.rs`、`tests.rs`
 *   以及 `#[cfg(test)]` 修饰的模块/函数项)中,禁止出现:
 *     - `.lock().unwrap()` / `.lock().expect(...)`
 *     - `.read().unwrap()` / `.read().expect(...)`
 *     - `.write().unwrap()` / `.write().expect(...)`
 *   常驻进程应统一走 `crate::lockx::lock(&m)` 或 `lock_rw(&m)` 做中毒恢复并记诊断日志。
 *
 * 用法:
 *   node build/check-lock-unwrap.js --self-test
 *   node build/check-lock-unwrap.js
 */
const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const SCAN_DIRS = [
  path.join(ROOT, "app", "core", "src"),
  path.join(ROOT, "app", "src-tauri", "src"),
];

function isTestFileName(filePath) {
  const base = path.basename(filePath);
  return base === "tests.rs" || /_tests(_[a-z0-9_]+)?\.rs$/i.test(base);
}

function walkRustFiles(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...walkRustFiles(full));
    } else if (entry.isFile() && entry.name.endsWith(".rs") && !isTestFileName(full)) {
      out.push(full);
    }
  }
  return out.sort();
}

/**
 * 将 Rust 源码中的注释与字符串字面量替换为等长空白(保留换行),
 * 确保注释/字符串中的括号或 `.lock().unwrap()` 字样不会干扰语法扫描。
 */
function maskRustCommentsAndStrings(src) {
  const n = src.length;
  const out = new Array(n);
  const blank = (c) => (c === "\n" ? "\n" : " ");
  let i = 0;

  while (i < n) {
    const ch = src[i];
    const next = src[i + 1];

    // 行注释
    if (ch === "/" && next === "/") {
      while (i < n && src[i] !== "\n") {
        out[i] = " ";
        i++;
      }
      continue;
    }

    // 块注释(支持 Rust 嵌套块注释)
    if (ch === "/" && next === "*") {
      let depth = 1;
      out[i] = " ";
      out[i + 1] = " ";
      i += 2;
      while (i < n && depth > 0) {
        if (src[i] === "/" && src[i + 1] === "*") {
          depth++;
          out[i] = " ";
          out[i + 1] = " ";
          i += 2;
        } else if (src[i] === "*" && src[i + 1] === "/") {
          depth--;
          out[i] = " ";
          out[i + 1] = " ";
          i += 2;
        } else {
          out[i] = blank(src[i]);
          i++;
        }
      }
      continue;
    }

    // 原生字符串 r"..." / r#"..."# / br"..." / br#"..."#
    if (
      (ch === "r" && (next === '"' || next === "#")) ||
      (ch === "b" && next === "r" && (src[i + 2] === '"' || src[i + 2] === "#"))
    ) {
      let p = ch === "b" ? i + 2 : i + 1;
      let hashes = 0;
      while (p < n && src[p] === "#") {
        hashes++;
        p++;
      }
      if (p < n && src[p] === '"') {
        while (i <= p) {
          out[i] = " ";
          i++;
        }
        while (i < n) {
          if (src[i] === '"') {
            let ok = true;
            for (let h = 1; h <= hashes; h++) {
              if (src[i + h] !== "#") {
                ok = false;
                break;
              }
            }
            if (ok) {
              for (let h = 0; h <= hashes; h++) {
                out[i + h] = " ";
              }
              i += hashes + 1;
              break;
            }
          }
          out[i] = blank(src[i]);
          i++;
        }
        continue;
      }
    }

    // 普通字符串 "..."
    if (ch === '"') {
      out[i] = " ";
      i++;
      while (i < n) {
        if (src[i] === "\\") {
          out[i] = " ";
          if (i + 1 < n) out[i + 1] = blank(src[i + 1]);
          i += 2;
          continue;
        }
        const done = src[i] === '"';
        out[i] = blank(src[i]);
        i++;
        if (done) break;
      }
      continue;
    }

    // 字符字面量 'x' / '\n'(注意区分生命周期 'a)
    if (ch === "'") {
      if (next === "\\" && i + 3 < n && src[i + 3] === "'") {
        out[i] = out[i + 1] = out[i + 2] = out[i + 3] = " ";
        i += 4;
        continue;
      }
      if (i + 2 < n && src[i + 2] === "'" && next !== "\n") {
        out[i] = out[i + 1] = out[i + 2] = " ";
        i += 3;
        continue;
      }
    }

    out[i] = ch;
    i++;
  }
  return out.join("");
}

/**
 * 仅抹除 `#[cfg(test)]` 紧接着修饰的那一个语法项(以 `;` 结尾的声明或 `{ ... }` 块),
 * 保留该项之后的生产代码(例如 `fetch.rs` 中间有 `#[cfg(test)] fn fetch_custom_all`,
 * 后面仍有 `impl Fetcher` 生产代码)。
 */
function stripCfgTestItems(masked) {
  const out = masked.split("");
  const re = /#\[\s*cfg\s*\(\s*test\s*\)\s*\]/g;
  let m;
  while ((m = re.exec(masked)) !== null) {
    let i = m.index + m[0].length;
    while (i < masked.length && masked[i] !== ";" && masked[i] !== "{") {
      i++;
    }
    let end = i;
    if (masked[i] === ";") {
      end = i + 1;
    } else if (masked[i] === "{") {
      let depth = 0;
      for (; i < masked.length; i++) {
        if (masked[i] === "{") depth++;
        else if (masked[i] === "}") {
          depth--;
          if (depth === 0) {
            end = i + 1;
            break;
          }
        }
      }
    }
    for (let k = m.index; k < end; k++) {
      if (out[k] !== "\n") out[k] = " ";
    }
    re.lastIndex = end;
  }
  return out.join("");
}

function scanRustSource(src) {
  const masked = maskRustCommentsAndStrings(src);
  const prod = stripCfgTestItems(masked);
  const origLines = src.split("\n");
  const hits = [];
  const re = /\.(lock|read|write)\s*\(\s*\)\s*\.\s*(unwrap|expect)\s*\(/g;
  let m;
  while ((m = re.exec(prod)) !== null) {
    const line = prod.slice(0, m.index).split("\n").length;
    hits.push({
      line,
      match: m[0],
      code: (origLines[line - 1] || "").trim(),
    });
  }
  return hits;
}

function selfTest() {
  const cases = [
    {
      name: "生产代码 .lock().unwrap() 判红",
      src: "fn f(m: &Mutex<i32>) { let _g = m.lock().unwrap(); }\n",
      wantHit: true,
    },
    {
      name: "生产代码 .read().unwrap() 与 .write().expect() 判红",
      src: "fn f(m: &RwLock<i32>) { let _r = m.read().unwrap(); let _w = m.write().expect(\"bad\"); }\n",
      wantHit: true,
    },
    {
      name: "#[cfg(test)] 辅助函数之后的生产代码违规仍被判红",
      src: "#[cfg(test)]\nfn helper(m: &Mutex<i32>) { let _ = m.lock().unwrap(); }\nfn prod(m: &Mutex<i32>) { let _ = m.lock().unwrap(); }\n",
      wantHit: true,
    },
    {
      name: "#[cfg(test)] mod tests 内部不判红",
      src: "fn prod(m: &Mutex<i32>) { let _ = crate::lockx::lock(m); }\n#[cfg(test)]\nmod tests {\n  fn t(m: &Mutex<i32>) { let _ = m.lock().unwrap(); }\n}\n",
      wantHit: false,
    },
    {
      name: "注释与字符串里的 .lock().unwrap() 不误报",
      src: "// 禁止 m.lock().unwrap()\nfn prod() { let _s = \"m.lock().unwrap()\"; }\n",
      wantHit: false,
    },
  ];

  let failed = 0;
  for (const c of cases) {
    const got = scanRustSource(c.src).length > 0;
    if (got !== c.wantHit) {
      console.error(`  ✗ ${c.name}: 期望${c.wantHit ? "红" : "绿"},实际${got ? "红" : "绿"}`);
      failed++;
    } else {
      console.log(`  ✓ ${c.name}`);
    }
  }
  return failed === 0;
}

function main() {
  if (process.argv.includes("--self-test")) {
    console.log("Rust 锁中毒棘轮门禁 自检:");
    process.exit(selfTest() ? 0 : 1);
  }

  const files = SCAN_DIRS.flatMap((d) => walkRustFiles(d));
  let total = 0;
  for (const f of files) {
    const src = fs.readFileSync(f, "utf8");
    const hits = scanRustSource(src);
    if (!hits.length) continue;
    total += hits.length;
    const rel = path.relative(ROOT, f).replace(/\\/g, "/");
    for (const h of hits) {
      console.error(`${rel}:${h.line}  违规锁调用 ${h.match}`);
      console.error(`    ${h.code}`);
    }
  }

  if (total > 0) {
    console.error(`\nRust 锁中毒棘轮门禁 FAIL: 发现 ${total} 处未做中毒恢复的锁 unwrap/expect。`);
    console.error("请改用 crate::lockx::lock(&m) 或 lock_rw(&m)。");
    process.exit(1);
  }

  console.log(`Rust 锁中毒棘轮门禁 PASS: ${files.length} 个生产源码文件,0 处锁 unwrap/expect`);
}

main();
