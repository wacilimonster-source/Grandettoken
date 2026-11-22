#!/usr/bin/env node
/**
 * 文本编码门禁
 *
 * 为什么需要它:2026-11-19 的 PROGRESS.md(v0.3.66 / v0.3.67 两段)被某个工具按 GBK 读、
 * 再按 UTF-8 写回 —— 46 处汉字各丢了末字节,连带吞掉紧跟的空格或换行,几条列表被粘成一行。
 * git 里的首个版本就已经是坏的,直到 2026-11-22 才被人工发现;同一时期还有 fetch.rs 的
 * 一条断言消息、一份产品文档带着 U+FFFD 进了仓库。ci.yml 对 `**.md` / `docs/**` 设了
 * paths-ignore,纯文档提交根本不跑 CI,所以这道门放在独立的 text-encoding.yml 里。
 *
 * 规则(只看 git ls-files,与 scan-secrets.ps1 同口径:只关心真正会推上去的东西):
 *   1. 文本文件必须是合法 UTF-8(允许 BOM);
 *   2. 不得含 U+FFFD 替换字符 —— 它几乎总是「解码失败后被写回」的痕迹。
 *      确需在源码里表达该字符时写转义(`\u{FFFD}` / `\ufffd`),不要写字面量。
 *
 * 双向验证:`--self-test` 构造损坏样本(含与 PROGRESS.md 同形的 GBK 往返损坏),必须判红。
 */
const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const TEXT_EXT = /\.(md|rs|js|mjs|cjs|ts|kt|kts|gradle|html?|css|json|ya?ml|toml|ps1|psm1|py|sh|xml|svg|txt|c|h|S|properties)$/;
const TEXT_NAMES = new Set([".gitignore", ".gitattributes", "LICENSE"]);
const MAX_REPORT = 5;

/** 返回所有非法 UTF-8 序列的起始字节偏移(严格模式:拒绝过长编码、代理区、超出 U+10FFFF)。 */
function invalidUtf8Offsets(buf) {
  const bad = [];
  // 一处损坏只记一次:跳过紧跟在坏起始字节后的续字节,避免「一个坏汉字报两三处」。
  const skipBad = (at) => {
    bad.push(at);
    let j = at + 1;
    for (let k = 0; k < 3 && j < buf.length && (buf[j] & 0xc0) === 0x80; k++) j++;
    return j;
  };
  let i = 0;
  const n = buf.length;
  while (i < n) {
    const b = buf[i];
    if (b < 0x80) { i++; continue; }
    let need;
    if (b >= 0xc2 && b <= 0xdf) need = 1;
    else if (b >= 0xe0 && b <= 0xef) need = 2;
    else if (b >= 0xf0 && b <= 0xf4) need = 3;
    else { i = skipBad(i); continue; }
    let ok = i + need < n;
    for (let k = 1; ok && k <= need; k++) ok = (buf[i + k] & 0xc0) === 0x80;
    if (ok) {
      const b1 = buf[i + 1];
      if ((b === 0xe0 && b1 < 0xa0) || (b === 0xed && b1 > 0x9f) ||
          (b === 0xf0 && b1 < 0x90) || (b === 0xf4 && b1 > 0x8f)) ok = false;
    }
    if (!ok) { i = skipBad(i); continue; }
    i += need + 1;
  }
  return bad;
}

function lineOf(buf, offset) {
  let line = 1;
  for (let i = 0; i < offset; i++) if (buf[i] === 0x0a) line++;
  return line;
}

/** 检查一段字节,返回问题列表 [{kind: "invalid-utf8" | "replacement-char", line}]。 */
function checkBuffer(buf) {
  const problems = [];
  const bad = invalidUtf8Offsets(buf);
  if (bad.length) {
    for (const off of bad) problems.push({ kind: "invalid-utf8", line: lineOf(buf, off) });
    return problems; // 字节都不合法时,再找 U+FFFD 没有意义
  }
  const text = buf.toString("utf8");
  if (text.includes("\uFFFD")) {
    text.split("\n").forEach((l, idx) => {
      if (l.includes("\uFFFD")) problems.push({ kind: "replacement-char", line: idx + 1 });
    });
  }
  return problems;
}

function isTextFile(rel) {
  return TEXT_EXT.test(rel) || TEXT_NAMES.has(path.basename(rel));
}

function listTrackedFiles() {
  const { execFileSync } = require("child_process");
  const out = execFileSync("git", ["ls-files", "-z"], { cwd: ROOT, maxBuffer: 64 * 1024 * 1024 });
  return out.toString("utf8").split("\0").filter(Boolean);
}

function summarize(problems) {
  const byKind = {};
  for (const p of problems) (byKind[p.kind] = byKind[p.kind] || []).push(p.line);
  return Object.entries(byKind).map(([kind, lines]) => {
    const uniq = [...new Set(lines)];
    const shown = uniq.slice(0, MAX_REPORT).join(", ");
    const more = uniq.length > MAX_REPORT ? ` 等 ${uniq.length} 行` : "";
    const label = kind === "invalid-utf8" ? "非法 UTF-8 字节" : "U+FFFD 替换字符";
    return `${label} ${lines.length} 处,行 ${shown}${more}`;
  }).join(";");
}

function selfTest() {
  const os = require("os");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "enc-probe-"));
  const zh = (s) => Buffer.from(s, "utf8");
  const cases = [
    { name: "合法中文 + BOM 不判红", buf: Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), zh("# 进展\n与桌面发版 run\n")]), want: null },
    { name: "纯 ASCII 不判红", buf: zh("hello\nworld\n"), want: null },
    {
      // 与 PROGRESS.md 同形:「版」(e7 89 88) 的末字节连同后面的空格一起变成了 '?'
      name: "GBK 往返损坏被判红(PROGRESS.md 同形)",
      buf: Buffer.concat([zh("main CI 与桌面发"), Buffer.from([0xe7, 0x89, 0x3f]), zh("run `44`\n")]),
      want: { kind: "invalid-utf8", line: 1 },
    },
    {
      name: "第二行损坏报对行号",
      buf: Buffer.concat([zh("第一行正常\n- 签"), Buffer.from([0xe5, 0x90, 0x3f]), zh("`.sig`\n")]),
      want: { kind: "invalid-utf8", line: 2 },
    },
    { name: "U+FFFD 字面量被判红", buf: zh("正常\n\"{code} \uFFFD\uFFFD\uFFFD服务端\"\n"), want: { kind: "replacement-char", line: 2 } },
    { name: "文件末尾截断被判红", buf: Buffer.concat([zh("尾部"), Buffer.from([0xe4, 0xb8])]), want: { kind: "invalid-utf8", line: 1 } },
    { name: "代理区编码被判红", buf: Buffer.from([0x61, 0xed, 0xa0, 0x80, 0x0a]), want: { kind: "invalid-utf8", line: 1 } },
    { name: "过长编码被判红", buf: Buffer.from([0x2f, 0xc0, 0xaf, 0x0a]), want: { kind: "invalid-utf8", line: 1 } },
  ];
  let fail = 0;
  try {
    for (const c of cases) {
      const f = path.join(dir, "probe.md");
      fs.writeFileSync(f, c.buf);
      const got = checkBuffer(fs.readFileSync(f));
      const ok = c.want === null
        ? got.length === 0
        : got.length > 0 && got[0].kind === c.want.kind && got[0].line === c.want.line;
      if (ok) console.log(`  ✓ ${c.name}`);
      else { console.error(`  ✗ 自检失败:${c.name} —— 实际 ${JSON.stringify(got)}`); fail++; }
    }
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
  return fail === 0;
}

function main() {
  if (process.argv.includes("--self-test")) {
    console.log("文本编码门禁 自检:");
    process.exit(selfTest() ? 0 : 1);
  }

  let files;
  try {
    files = listTrackedFiles();
  } catch (e) {
    // 拿不到文件清单不能当作「没有问题」
    console.error(`✗ 无法执行 git ls-files:${e.message}`);
    process.exit(2);
  }

  const bad = [];
  let scanned = 0;
  for (const rel of files) {
    if (!isTextFile(rel)) continue;
    const abs = path.join(ROOT, rel);
    if (!fs.existsSync(abs)) continue; // 工作区已删除、尚未提交的文件
    scanned++;
    const problems = checkBuffer(fs.readFileSync(abs));
    if (problems.length) bad.push(`${rel}  ${summarize(problems)}`);
  }

  if (bad.length) {
    for (const b of bad) console.error("✗ " + b);
    console.error(`\n文本编码门禁 FAIL(${bad.length} 个文件 / 共扫描 ${scanned} 个)。`);
    console.error("  1. 多半是某个工具按 GBK/ANSI 读、再按 UTF-8 写回;先查清是哪个工具,别只修文件;");
    console.error("  2. 用 git 历史找回原文;首版就坏的,按残留字节与上下文复原,并在提交说明里写明依据;");
    console.error("  3. 源码确需 U+FFFD 时写转义 \\u{FFFD},不要写字面量。");
    process.exit(1);
  }
  console.log(`文本编码门禁 PASS:${scanned} 个受版本控制的文本文件均为合法 UTF-8,且无 U+FFFD。`);
}

if (require.main === module) main();
module.exports = { invalidUtf8Offsets, checkBuffer, isTextFile };
