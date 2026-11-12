#!/usr/bin/env node
/**
 * 静默失败门禁
 *
 * 背景:本项目前端是 9000+ 行的单文件,历史上 14 处空 catch 里有 5 处是 IPC **写操作** ——
 * 保存失败时界面回到"看起来没事"的状态,重启才发现设置没生效(`AGENTS.md` 2.1.6)。
 * 光靠"下次注意"从来没有生效过,所以把规则做成机器可判定:
 *
 *   **空 catch 必须显式认领** —— 块内(或 catch 同一行/上一行)要有 `silent: <理由>`,
 *   理由至少 MIN_REASON 个字符。不写就是失败,写不出理由说明它不该被吞。
 *
 * 判定口径(与 docs/report 的"定义②恒假死分支"同源):
 *   - 空 catch:去掉注释与空白后块内没有任何语句,或整个 catch 就是 `.catch(() => {})`
 *   - 有代码的 catch(回滚 / 降级 / 重试)不在此门禁范围,它本身已经是处理
 *
 * 双向验证:`node build/check-silent-failures.js --self-test` 会临时注入一个未标记的
 * 空 catch 与一个标记过短的空 catch,两者都必须被判红 —— 只变绿的门禁等于没有门禁。
 */
const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const SRC_DIR = path.join(ROOT, "app", "src");
const MIN_REASON = 6;

/** 把注释与字符串内容替换成等长空白(保留换行,行号不变)。
 *  目的有两个:
 *   1. 注释里写的示例代码(`// 以前这里 .catch(() => {})`)不能被当成真代码;
 *   2. 字符串里的 `}`、`{`、`//`(URL)不能干扰括号配对。
 *  必须按字符走状态机 —— 用正则先删注释会误伤 `"https://…"` 这类字面量。 */
function maskSource(src) {
  const out = [];
  const n = src.length;
  const blank = (c) => (c === "\n" ? "\n" : " ");
  let i = 0;
  while (i < n) {
    const ch = src[i];
    const next = src[i + 1];
    if (ch === "/" && next === "/") {
      while (i < n && src[i] !== "\n") { out.push(" "); i++; }
      continue;
    }
    if (ch === "/" && next === "*") {
      out.push(" ", " ");
      i += 2;
      while (i < n && !(src[i] === "*" && src[i + 1] === "/")) { out.push(blank(src[i])); i++; }
      if (i < n) { out.push(" ", " "); i += 2; }
      continue;
    }
    if (ch === '"' || ch === "'" || ch === "`") {
      const q = ch;
      out.push(" ");
      i++;
      while (i < n) {
        if (src[i] === "\\") {
          out.push(" ");
          if (i + 1 < n) out.push(blank(src[i + 1]));
          i += 2;
          continue;
        }
        const done = src[i] === q;
        out.push(blank(src[i]));
        i++;
        if (done) break;
      }
      continue;
    }
    out.push(ch);
    i++;
  }
  return out.join("");
}

/** 逐字符扫描,返回 {line, body, maskedBody, header} 列表。
 *  不用正则整体匹配块体:块内可能嵌套 try/catch,`[^}]*` 会在第一个 `}` 提前收尾。
 *  注意:JS 的正则匹配结果**没有** `.end`(那是 .NET 的 API),偏移只能用
 *  `index + match[0].length`;第一版写成 `m.end - 1` 得到 NaN,导致所有 catch
 *  都被判成空块 —— 门禁自己先假红了一遍,见 --self-test 的来历。 */
const blockStart = (m) => m.index + m[0].length - 1;

function findCatches(src, masked) {
  const out = [];
  const collect = (m) => {
    const start = blockStart(m);
    let depth = 0;
    let i = start;
    for (; i < masked.length; i++) {
      if (masked[i] === "{") depth++;
      else if (masked[i] === "}") {
        depth--;
        if (depth === 0) break;
      }
    }
    const line = src.slice(0, m.index).split("\n").length;
    out.push({
      line,
      start,
      end: i,
      maskedBody: masked.slice(start + 1, i),
      body: src.slice(start + 1, i),
      header: m[0],
    });
  };
  const re = /\bcatch\s*(\([^)]*\))?\s*\{/g;
  let m;
  while ((m = re.exec(masked))) collect(m);
  // `.catch(() => {})` 是 Promise 链上的 catch,没有 `catch {` 关键字形态
  const re2 = /\.catch\s*\(\s*(?:\(\s*\w*\s*\)|\w+)\s*=>\s*\{/g;
  while ((m = re2.exec(masked))) collect(m);
  return out.sort((a, b) => a.line - b.line);
}

function checkFile(file) {
  const src = fs.readFileSync(file, "utf8");
  const masked = maskSource(src);
  const lines = src.split("\n");
  const bad = [];
  for (const c of findCatches(src, masked)) {
    // 只认"块里一条语句都没有"的 catch;有回滚 / 降级 / 重试的不在本门禁范围
    if (c.maskedBody.trim() !== "") continue;
    // 认领标记:块内 / catch 同一行 / 上一行(用未脱敏原文,标记本身就写在注释里)
    const window = [
      c.body,
      lines[c.line - 1] || "",
      lines[c.line - 2] || "",
    ].join("\n");
    const tag = window.match(/silent:\s*([^\n*]*)/);
    if (!tag) {
      bad.push({ line: c.line, why: "空 catch 未标注 `silent: <理由>`", code: (lines[c.line - 1] || "").trim() });
      continue;
    }
    const reason = tag[1].replace(/\*\/.*$/, "").trim();
    if (reason.length < MIN_REASON) {
      bad.push({ line: c.line, why: `silent 理由过短(${reason.length} < ${MIN_REASON} 字)`, code: (lines[c.line - 1] || "").trim() });
    }
  }
  return bad;
}

function selfTest() {
  const tmp = path.join(SRC_DIR, "__silent_probe__.js");
  const cases = [
    { name: "未标记的空 catch", src: "function f(){ try{ g(); }catch(e){ } }\n", wantBad: true },
    { name: "理由过短", src: "function f(){ try{ g(); }catch(e){ /* silent: 无 */ } }\n", wantBad: true },
    { name: "已认领", src: "function f(){ try{ g(); }catch(e){ /* silent: 纯视觉刷新失败无需提示 */ } }\n", wantBad: false },
    { name: "有处理逻辑", src: "function f(){ try{ g(); }catch(e){ showToast(e); } }\n", wantBad: false },
    { name: "嵌套 try 不被误判", src: "function f(){ try{ g(); }catch(e){ try{ h(); }catch(_){ /* silent: 内层清理失败可忽略 */ } } }\n", wantBad: false },
  ];
  let fail = 0;
  for (const c of cases) {
    fs.writeFileSync(tmp, c.src, "utf8");
    const bad = checkFile(tmp);
    const got = bad.length > 0;
    if (got !== c.wantBad) {
      console.error(`  ✗ 自检失败:${c.name} —— 期望${c.wantBad ? "红" : "绿"},实际${got ? "红" : "绿"}`);
      fail++;
    } else {
      console.log(`  ✓ ${c.name}`);
    }
  }
  try { fs.unlinkSync(tmp); } catch {}
  // 仓库里不能留下探针文件
  return fail === 0;
}

function main() {
  if (process.argv.includes("--self-test")) {
    console.log("静默失败门禁 自检:");
    process.exit(selfTest() ? 0 : 1);
  }
  const files = fs.readdirSync(SRC_DIR).filter((f) => f.endsWith(".js")).map((f) => path.join(SRC_DIR, f));
  let total = 0;
  for (const f of files) {
    const bad = checkFile(f);
    if (!bad.length) continue;
    total += bad.length;
    for (const b of bad) {
      console.error(`${path.relative(ROOT, f)}:${b.line}  ${b.why}`);
      console.error(`    ${b.code}`);
    }
  }
  if (total) {
    console.error(`\n静默失败门禁 FAIL:${total} 处空 catch 未认领。`);
    console.error("修法二选一:①按 saveCfg() 范式回滚状态 + showToast 告知用户;");
    console.error("            ②确实是可忽略的降级路径,补 `/* silent: <理由> */`。");
    process.exit(1);
  }
  const marked = files.reduce((n, f) => {
    const s = fs.readFileSync(f, "utf8");
    return n + findCatches(s, maskSource(s)).length;
  }, 0);
  console.log(`静默失败门禁 PASS:${files.length} 个前端脚本,${marked} 处 catch 全部已处理或已认领`);
}

main();
